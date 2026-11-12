//! Application state machine and eframe integration.
//!
//! Per-frame order is fixed and must not be reordered:
//! 1. `tick`: drain PTY -> feed screen -> snapshot -> guard.observe;
//!    if the guard flips to Passthrough, close the overlay and discard staging;
//! 2. `ui::draw`: paint the grid and (only when allowed) the overlay;
//! 3. input: guard.decision chooses `PassToPty` vs smart handling;
//! 4. confirm: unsafe/inexact commands go through `ConfirmGate`, then PTY write.

use crate::config::{Config, LlmProviderKind, PromptIcon};
use crate::contracts::{
    Candidate, CandidateSource, CwdContext, EnvContext, InterceptDecision, KeyInput, ProjectMarker,
    ResolveError, ResolveRequest, Trigger, UiIntent,
};
use crate::input::placeholder::Template;
use crate::input::safety::{self, ConfirmGate, SafetyPolicy, StageOutcome};
use crate::pty::guard::GuardConfig;
use crate::pty::markers::MarkerParser;
use crate::pty::portable::PortablePty;
use crate::pty::vt::Vt100Screen;
use crate::pty::{GridSnapshot, PassthroughGuard, PtyBackend, ScreenModel, SpawnSpec};
use crate::resolver::ResolverChain;
use crate::telemetry::{CommandTracker, PortScanner};
use crate::theme::ThemePalette;
use crate::ui::overlay::{OverlayEvent, OverlayState};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const THINKING_VERBS: &[&str] = &[
    "Ruminating",
    "Honking",
    "Pondering",
    "Simmering",
    "Brewing",
    "Conjuring",
    "Noodling",
    "Mulling",
    "Percolating",
    "Tinkering",
    "Cogitating",
    "Spelunking",
    "Perusing",
    "Marinating",
    "Contemplating",
    "Wrangling",
    "Sifting",
    "Combing",
    "Foraging",
    "Deliberating",
    "Crunching",
    "Decoding",
    "Synthesizing",
    "Unraveling",
    "Daydreaming",
    "Chewing",
    "Polishing",
    "Sketching",
    "Untangling",
    "Calculating",
    "Scanning",
    "Whirring",
    "Distilling",
    "Stewing",
    "Indexing",
    "Assembling",
    "Fermenting",
    "Sprouting",
    "Musing",
    "Puzzling",
    "Brainstorming",
    "Weaving",
];

type AsyncResolveMsg = (
    ResolverChain,
    Result<Vec<Candidate>, ResolveError>,
    CwdContext,
    String,
);

struct PendingResolve {
    rx: mpsc::Receiver<AsyncResolveMsg>,
    started_at: Instant,
    verb: &'static str,
    cancelled: bool,
}

fn random_u64() -> u64 {
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut buf = [0u8; 8];
        if f.read_exact(&mut buf).is_ok() {
            return u64::from_le_bytes(buf);
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15);
    let mut x = nanos ^ ((std::process::id() as u64) << 16);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn shuffled_verb_bag(avoid_first: Option<&'static str>) -> Vec<&'static str> {
    let mut bag: Vec<&'static str> = THINKING_VERBS.to_vec();
    let mut seed = random_u64();
    for i in (1..bag.len()).rev() {
        seed = seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;
        let j = (z as usize) % (i + 1);
        bag.swap(i, j);
    }
    if let Some(prev) = avoid_first {
        if bag.last().copied() == Some(prev) && bag.len() > 1 {
            let len = bag.len();
            bag.swap(0, len - 1);
        }
    }
    bag
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub percent: Option<u8>,
    pub detail: Option<String>,
    pub updated_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProgressEvent {
    Clear,
    Update {
        percent: Option<u8>,
        detail: Option<String>,
    },
}

fn parse_percent_token(line: &str) -> Option<u8> {
    for tok in line.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '|')) {
        let Some(num_str) = tok.strip_suffix('%') else {
            continue;
        };
        if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        if let Ok(val) = num_str.parse::<f32>() {
            if (0.0..=100.0).contains(&val) {
                return Some(val.round() as u8);
            }
        }
    }
    None
}

fn extract_transfer_detail(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ','))
        .filter(|t| !t.is_empty())
        .collect();
    for (i, tok) in tokens.iter().enumerate() {
        let low = tok.to_ascii_lowercase();
        if low.ends_with("/s") && tok.chars().any(|c| c.is_ascii_digit()) {
            return Some((*tok).to_string());
        }
        if matches!(
            low.as_str(),
            "b/s" | "kb/s" | "kib/s" | "mb/s" | "mib/s" | "gb/s" | "gib/s"
        ) && i > 0
            && tokens[i - 1].chars().any(|c| c.is_ascii_digit())
        {
            return Some(format!("{}{}", tokens[i - 1], tok));
        }
    }
    for tok in &tokens {
        let low = tok.to_ascii_lowercase();
        if tok.contains('/')
            && tok.chars().any(|c| c.is_ascii_digit())
            && ["k", "m", "g", "b"].iter().any(|u| low.contains(u))
        {
            return Some((*tok).to_string());
        }
    }
    None
}

fn parse_osc9_progress(bytes: &[u8]) -> Option<ProgressEvent> {
    let text = String::from_utf8_lossy(bytes);
    let mut last = None;
    let mut rest = text.as_ref();
    while let Some(pos) = rest.find("\x1b]9;4;") {
        let after = &rest[pos + 6..];
        let end_bel = after.find('\x07');
        let end_st = after.find("\x1b\\");
        let end = match (end_bel, end_st) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        let Some(end_idx) = end else { break };
        let payload = &after[..end_idx];
        let mut parts = payload.split(';');
        if let Some(state_str) = parts.next() {
            match state_str.trim() {
                "0" => last = Some(ProgressEvent::Clear),
                "3" => {
                    last = Some(ProgressEvent::Update {
                        percent: None,
                        detail: Some("working".into()),
                    })
                }
                "1" | "2" | "4" => {
                    let pct = parts
                        .next()
                        .and_then(|s| s.trim().parse::<u16>().ok())
                        .map(|n| n.min(100) as u8);
                    last = Some(ProgressEvent::Update {
                        percent: pct,
                        detail: None,
                    });
                }
                _ => {}
            }
        }
        rest = &after[end_idx + 1..];
    }
    last
}

/// Build channel: cargo/rustc/make output has no percentage, only stage lines.
/// Kept apart from the transfer heuristics so a build stage does not have to
/// look like a download. No `[===>] N/M` parsing here on purpose.
fn parse_build_line(line: &str) -> Option<ProgressEvent> {
    let trimmed = line.trim();
    let low = trimmed.to_ascii_lowercase();
    if low.starts_with("finished ") || low.starts_with("running `") {
        return Some(ProgressEvent::Clear);
    }
    let kind = [
        "compiling",
        "checking",
        "building",
        "linking",
        "documenting",
    ]
    .into_iter()
    .find(|k| low.starts_with(k))?;
    let stage = &trimmed[..kind.len()];
    let detail = match trimmed[kind.len()..].split_whitespace().next() {
        Some(name) => format!("{stage} {name}"),
        None => stage.to_string(),
    };
    Some(ProgressEvent::Update {
        percent: parse_percent_token(trimmed),
        detail: Some(detail),
    })
}

fn parse_pty_progress(bytes: &[u8]) -> Option<ProgressEvent> {
    if let Some(ev) = parse_osc9_progress(bytes) {
        return Some(ev);
    }
    let clean = crate::telemetry::strip_ansi_sequences(bytes);
    for line in clean.split(['\r', '\n']).rev() {
        let trimmed = line.trim();
        if trimmed.len() < 4 || crate::telemetry::looks_like_encoded_blob(trimmed) {
            continue;
        }
        if let Some(ev) = parse_build_line(trimmed) {
            return Some(ev);
        }
        let low = trimmed.to_ascii_lowercase();
        let has_bar = trimmed.contains("[===")
            || trimmed.contains("[###")
            || trimmed.contains("━━")
            || trimmed.contains("███");
        let has_rate = low.contains("/s")
            && (low.contains("b/s")
                || low.contains("k/s")
                || low.contains("m/s")
                || low.contains("g/s"));
        let has_kw = [
            "downloading",
            "fetching",
            "receiving objects",
            "resolving deltas",
            "unpacking",
            "retrieving",
            "eta ",
        ]
        .iter()
        .any(|k| low.contains(k));
        let has_ratio = low.contains("mb/")
            || low.contains("mib/")
            || low.contains("kb/")
            || low.contains("kib/")
            || low.contains("gb/")
            || low.contains("gib/");

        if !(has_bar || has_rate || has_kw || has_ratio) {
            continue;
        }
        let detail = extract_transfer_detail(trimmed);
        if let Some(pct) = parse_percent_token(trimmed) {
            return Some(ProgressEvent::Update {
                percent: Some(pct),
                detail,
            });
        }
        if has_rate || has_kw {
            return Some(ProgressEvent::Update {
                percent: None,
                detail,
            });
        }
    }
    None
}

fn decode_percent_path(s: &str) -> PathBuf {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

fn parse_osc7_cwd(bytes: &[u8]) -> Option<PathBuf> {
    let text = String::from_utf8_lossy(bytes);
    let idx = text.rfind("\x1b]7;")?;
    let after = &text[idx + 4..];
    let end = match (after.find('\x07'), after.find("\x1b\\")) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return None,
    };
    let uri = after[..end].trim();
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let path = decode_percent_path(&rest[slash..]);
    path.is_dir().then_some(path)
}

fn git_branch_in(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    for _ in 0..6 {
        let cur = dir?;
        let dot_git = cur.join(".git");
        let head_path = if dot_git.is_dir() {
            Some(dot_git.join("HEAD"))
        } else if dot_git.is_file() {
            std::fs::read_to_string(&dot_git).ok().and_then(|s| {
                s.strip_prefix("gitdir:")
                    .map(|p| cur.join(p.trim()).join("HEAD"))
            })
        } else {
            None
        };
        if let Some(head) = head_path {
            if let Ok(content) = std::fs::read_to_string(head) {
                let line = content.trim();
                if let Some(r) = line.strip_prefix("ref:") {
                    let r = r.trim();
                    return Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string());
                }
                if line.len() >= 7 {
                    return Some(line[..7].to_string());
                }
            }
            return None;
        }
        dir = cur.parent();
    }
    None
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    pub working_dir: Option<PathBuf>,
    pub command: Vec<String>,
    pub startup_warning: Option<String>,
}

pub struct LexicueApp {
    pub cfg: Config,
    pub config_path: Option<PathBuf>,
    pub theme: ThemePalette,
    pub pty: Box<dyn PtyBackend>,
    pub screen: Box<dyn ScreenModel>,
    pub guard: Option<PassthroughGuard>,
    pub snapshot: GridSnapshot,
    /// Last rendered cell pixel size; drives child-visible pixel metrics.
    cell_px: (u32, u32),
    pub resolver: Option<ResolverChain>,
    pending_resolve: Option<PendingResolve>,
    pub overlay: OverlayState,
    pub live_hints: Vec<Candidate>,
    pub live_hint_selected: Option<usize>,
    pub settings_open: bool,
    pub top_island_open: bool,
    pub top_island_pinned: bool,
    pub transfer_progress: Option<TransferProgress>,
    cached_branch: Option<(PathBuf, Option<String>, Instant)>,
    pub cached_markers: Option<(PathBuf, Vec<ProjectMarker>, Instant)>,
    pub ports: PortScanner,
    pub env: EnvContext,
    pub command_tracker: CommandTracker,
    marker_parser: MarkerParser,
    pub launcher: crate::contracts::LauncherState,
    pub last_outcome: Option<crate::contracts::CommandOutcome>,
    pub pending_island_actions: Vec<crate::contracts::IslandAction>,
    pub last_command: Option<String>,
    pub status_note: Option<String>,
    pub template: Option<Template>,
    pub gate: Option<ConfirmGate>,
    safety_policy: SafetyPolicy,
    cwd: PathBuf,
    line_buf: String,
    cursor_at_end: bool,
    history_tail: Vec<String>,
    verb_bag: Vec<&'static str>,
    last_verb: Option<&'static str>,
    pub selection_anchor: Option<(u16, usize)>,
    pub selection_end: Option<(u16, usize)>,
    pub last_drag_scroll: Option<Instant>,
    pub toast: Option<(String, Instant)>,
    pending_probe: Option<mpsc::Receiver<Result<u128, ResolveError>>>,
    /// In-flight clipboard helper read; keeps `wl-paste`/`xclip` off the UI thread.
    pending_paste: Option<mpsc::Receiver<Option<String>>>,
    pub probe_status: Option<(bool, String)>,
    last_window_title: String,
    /// shortcut: in-app primary selection buffer, upgrade to X11/Wayland primary selection if arboard is added.
    pub primary_selection: Option<String>,
    pub last_mouse_cell: Option<(u16, u16)>,
    pub search_open: bool,
    pub search_focus_requested: bool,
    pub search_query: String,
    pub search_matches: Vec<(usize, u16, u16)>,
    pub search_index: usize,
    last_bell_count: usize,
    pub bell_flash_until: Option<Instant>,
}

fn command_in_path(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.contains('/') {
        return Path::new(token).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(token).is_file()))
        .unwrap_or(false)
}

/// System font files that add Unicode / Nerd Font / emoji coverage.
/// Reads up to 48 MiB of the highest-priority `.ttf`/`.otf` files found in the
/// standard per-user and system font roots (recursively, missing roots ignored).
fn load_system_fallback_fonts() -> Vec<(String, Vec<u8>)> {
    const MAX_FONTS: usize = 16;
    const MAX_TOTAL_BYTES: u64 = 48 * 1024 * 1024;
    const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

    let mut roots: Vec<PathBuf> = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/share/fonts"));
        roots.push(home.join(".fonts"));
    }

    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 5 || out.len() >= 4096 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                walk(&path, depth + 1, out);
            } else if ft.is_file() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase);
                if matches!(ext.as_deref(), Some("ttf" | "otf")) {
                    out.push(path);
                }
            }
        }
    }

    fn score(name: &str) -> u8 {
        let n: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        let base = if n.contains("nerdfont") || (n.contains("nerd") && n.contains("symbols")) {
            0
        } else if n.contains("notosanssymbols") || n.contains("symbols2") {
            1
        } else if n.contains("emoji") {
            2
        } else if n.contains("notosansmono") || n.contains("notomono") {
            3
        } else if n.contains("dejavusansmono") || n.contains("liberationmono") {
            4
        } else if n.contains("dejavusans") {
            5
        } else if n.contains("notosans") {
            6
        } else if n.contains("unifont") || n.contains("freeserif") || n.contains("freesans") {
            7
        } else if n.contains("jetbrainsmono")
            || n.contains("firacode")
            || n.contains("cascadiamono")
            || n.contains("sourcecodepro")
        {
            8
        } else if n.contains("material") || n.contains("awesome") || n.contains("symbols") {
            9
        } else if n.contains("mono") || n.contains("sans") {
            20
        } else {
            return 255;
        };
        // Prefer the Regular face; bold/italic/condensed variants add no coverage.
        let styled = [
            "bold",
            "italic",
            "oblique",
            "light",
            "thin",
            "black",
            "semibold",
            "extrabold",
            "condensed",
        ]
        .iter()
        .any(|w| n.contains(w));
        base + if styled { 40 } else { 0 }
    }

    let mut paths = Vec::new();
    for root in &roots {
        walk(root, 0, &mut paths);
    }
    paths.sort();
    paths.dedup();
    let mut ranked: Vec<(u8, String, PathBuf)> = paths
        .into_iter()
        .filter_map(|path| {
            let file_name = path.file_name()?.to_str()?.to_string();
            let s = score(&file_name);
            (s < 255).then_some((s, file_name, path))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut loaded = Vec::new();
    let mut total = 0u64;
    for (_, _, path) in ranked {
        if loaded.len() >= MAX_FONTS || total >= MAX_TOTAL_BYTES {
            break;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() == 0 || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // SFNT magic: TrueType, OpenType/CFF, Apple TrueType, or a collection (skip TTC).
        let is_sfnt = bytes.starts_with(b"\x00\x01\x00\x00")
            || bytes.starts_with(b"OTTO")
            || bytes.starts_with(b"true")
            || bytes.starts_with(b"typ1");
        if !is_sfnt {
            continue;
        }
        total += bytes.len() as u64;
        loaded.push((format!("system_fallback_{}", loaded.len()), bytes));
    }
    loaded
}

fn detect_markers(cwd: &Path, marker_files: &[String]) -> Vec<ProjectMarker> {
    let mut markers = Vec::new();
    for name in marker_files {
        if !cwd.join(name).exists() {
            continue;
        }
        let marker = match name.as_str() {
            "Cargo.toml" => ProjectMarker::Cargo,
            "package.json" => ProjectMarker::Node,
            "go.mod" => ProjectMarker::Go,
            "pyproject.toml" => ProjectMarker::Python,
            "Makefile" => ProjectMarker::Make,
            "Dockerfile" => ProjectMarker::Docker,
            ".git" => ProjectMarker::Git,
            _ => ProjectMarker::Unknown,
        };
        if !markers.contains(&marker) {
            markers.push(marker);
        }
    }
    if cwd.join(".git").exists() && !markers.contains(&ProjectMarker::Git) {
        markers.push(ProjectMarker::Git);
    }
    markers
}

impl LexicueApp {
    fn install_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "golos_text".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/GolosText.ttf")),
        );
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "golos_text".to_owned());
        }

        // GolosText has no Nerd Font PUA / emoji / wide Unicode coverage. Append
        // system fonts as fallbacks so `󰄾`, arrows, box drawing and CJK render
        // as glyphs instead of `□`. Missing directories are simply skipped.
        for (name, bytes) in load_system_fallback_fonts() {
            if fonts
                .font_data
                .insert(name.clone(), egui::FontData::from_owned(bytes))
                .is_none()
            {
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push(name.clone());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push(name);
            }
        }
        ctx.set_fonts(fonts);
    }

    /// Build palette, spawn `$SHELL` (or explicit command) in `cwd`, perform the first PTY drain.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        config_path: Option<PathBuf>,
        launch: LaunchOptions,
    ) -> Result<Self, AppError> {
        Self::install_fonts(&cc.egui_ctx);
        let theme =
            ThemePalette::from_section(&cfg.theme).map_err(|e| AppError::Startup(e.to_string()))?;

        let cols = 100u16;
        let rows = 36u16;
        let cwd = match launch.working_dir {
            Some(dir) if dir.is_dir() => dir,
            Some(dir) => {
                return Err(AppError::Startup(format!(
                    "working directory does not exist: {}",
                    dir.display()
                )))
            }
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        };
        let (program, args) = if let Some((prog, rest)) = launch.command.split_first() {
            (prog.clone(), rest.to_vec())
        } else {
            (cfg.shell(), Vec::new())
        };

        let mut pty = Box::new(PortablePty::new());
        pty.spawn(SpawnSpec {
            program,
            args,
            cwd: cwd.clone(),
            env: vec![
                // xterm-kitty: fastfetch/shell tools emit real Kitty Graphics
                // images, which src/pty/kitty.rs already decodes.
                ("TERM".into(), "xterm-kitty".into()),
                ("COLORTERM".into(), "truecolor".into()),
                ("TERM_PROGRAM".into(), "Lexicue".into()),
                (
                    "TERM_PROGRAM_VERSION".into(),
                    env!("CARGO_PKG_VERSION").into(),
                ),
            ],
            cols,
            rows,
        })
        .map_err(|e| AppError::Pty(e.to_string()))?;

        let mut screen = Box::new(Vt100Screen::new(cols, rows, cfg.general.scrollback_lines));
        if let Ok(bytes) = pty.drain() {
            if !bytes.is_empty() {
                screen.feed(&bytes);
            }
        }

        let mut guard = PassthroughGuard::new(GuardConfig {
            tui_titles: cfg.guard.tui_titles.clone(),
            password_prompt_markers: cfg.guard.password_prompt_markers.clone(),
        });
        let mode = guard.observe(screen.as_ref(), pty.shell_signals());
        screen.set_mode_hints(mode.raw_mode, mode.echo_off);
        let snapshot = screen.snapshot();

        let resolver = ResolverChain::build(&cfg.resolver, &cfg)
            .map_err(|e| AppError::Startup(e.to_string()))?;
        let safety_policy = SafetyPolicy::from_settings(&cfg.safety);
        let gate = ConfirmGate::new(cfg.safety.confirm_dangerous);

        let initial_bell = snapshot.bell_count;
        let app = Self {
            cfg,
            config_path,
            theme,
            pty,
            screen,
            guard: Some(guard),
            snapshot,
            cell_px: (0, 0),
            resolver: Some(resolver),
            pending_resolve: None,
            overlay: OverlayState::default(),
            live_hints: Vec::new(),
            live_hint_selected: None,
            settings_open: false,
            top_island_open: false,
            top_island_pinned: false,
            transfer_progress: None,
            cached_branch: None,
            cached_markers: None,
            ports: PortScanner::new(),
            env: EnvContext::detect(),
            command_tracker: CommandTracker::new(),
            marker_parser: MarkerParser::new(),
            launcher: crate::contracts::LauncherState::default(),
            last_outcome: None,
            pending_island_actions: Vec::new(),
            last_command: None,
            status_note: launch.startup_warning,
            template: None,
            gate: Some(gate),
            safety_policy,
            cwd,
            line_buf: String::new(),
            cursor_at_end: true,
            history_tail: Vec::new(),
            verb_bag: shuffled_verb_bag(None),
            last_verb: None,
            selection_anchor: None,
            selection_end: None,
            last_drag_scroll: None,
            toast: None,
            pending_probe: None,
            pending_paste: None,
            probe_status: None,
            last_window_title: "Lexicue".to_owned(),
            primary_selection: None,
            last_mouse_cell: None,
            search_open: false,
            search_focus_requested: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_index: 0,
            last_bell_count: initial_bell,
            bell_flash_until: None,
        };
        Ok(app)
    }

    /// Non-blocking pump; called once per frame before drawing.
    pub fn tick(&mut self) {
        if let Some((_, at)) = &self.toast {
            if at.elapsed() >= Duration::from_secs(3) {
                self.toast = None;
            }
        }
        if let Some(tp) = &self.transfer_progress {
            let max_age = if tp.percent == Some(100) {
                Duration::from_millis(1600)
            } else {
                Duration::from_secs(4)
            };
            if tp.updated_at.elapsed() >= max_age {
                self.transfer_progress = None;
            }
        }
        if let Some(rx) = self.pending_paste.take() {
            match rx.try_recv() {
                Ok(Some(text)) => self.apply_paste_text(text),
                Ok(None) => {
                    let msg = match self.cfg.general.language {
                        crate::config::UiLanguage::Ru => "Буфер обмена пуст или недоступен",
                        crate::config::UiLanguage::En => "Clipboard empty or unavailable",
                    };
                    self.set_toast(msg);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_paste = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if let Some(rx) = self.pending_probe.take() {
            match rx.try_recv() {
                Ok(Ok(ms)) => {
                    self.probe_status = Some((true, format!("Connected ({ms} ms)")));
                }
                Ok(Err(err)) => {
                    self.probe_status = Some((false, err.to_string()));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_probe = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.probe_status = Some((false, "Probe cancelled".into()));
                }
            }
        }
        if let Some(pending) = self.pending_resolve.take() {
            match pending.rx.try_recv() {
                Ok((resolver, res, ctx, raw_input)) => {
                    self.resolver = Some(resolver);
                    if !pending.cancelled {
                        self.finish_candidates(res, &ctx, &raw_input);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_resolve = Some(pending);
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        let now = Instant::now();
        if let Ok(bytes) = self.pty.drain() {
            if !bytes.is_empty() {
                // 2. Feed marker parser
                let markers = self.marker_parser.feed(&bytes);
                for marker in markers {
                    if let Some(done) = self.command_tracker.on_marker(marker, now) {
                        self.finish_command(done);
                    }
                }

                // 3. parse existing OSC 7 / OSC 9;4 progress (unchanged)
                if let Some(new_cwd) = parse_osc7_cwd(&bytes) {
                    if new_cwd != self.cwd {
                        self.cwd = new_cwd;
                        self.cached_branch = None;
                        self.cached_markers = None;
                    }
                }
                match parse_pty_progress(&bytes) {
                    Some(ProgressEvent::Clear) => self.transfer_progress = None,
                    Some(ProgressEvent::Update { percent, detail }) => {
                        let prev_detail = self
                            .transfer_progress
                            .as_ref()
                            .and_then(|p| p.detail.clone());
                        self.transfer_progress = Some(TransferProgress {
                            percent,
                            detail: detail.or(prev_detail),
                            updated_at: Instant::now(),
                        });
                    }
                    None => {}
                }

                // 4. screen.feed(&bytes); refresh_snapshot
                self.screen.feed(&bytes);
                for reply in self.screen.take_replies() {
                    let _ = self.pty.write(&reply);
                }
                self.refresh_snapshot();

                // 6. command_tracker.on_output
                self.command_tracker.on_output(&bytes, now);

                // 7. ports.observe
                self.ports.observe(&bytes, now);
            }
        }

        // 5. shell_signals() ONCE -> guard.observe(...) -> set_mode_hints
        let signals = self.pty.shell_signals();
        self.observe_guard(signals, now);

        // 7b. ports.revalidate (throttled)
        self.ports.revalidate(
            now,
            Duration::from_secs(self.cfg.island.port_revalidate_secs),
            Duration::from_secs(self.cfg.island.port_ttl_secs),
        );

        // Refresh cwd markers / git context debounced in tick
        self.refresh_cwd_context();
    }

    fn observe_guard(&mut self, signals: crate::pty::guard::ShellSignals, now: Instant) {
        let Some(guard) = self.guard.as_mut() else {
            return;
        };
        let was_alt = guard.mode().alternate_screen;
        let now_alt = self.screen.terminal_mode().alternate_screen;
        if was_alt && !now_alt {
            guard.reset();
        }
        let mode = guard.observe(self.screen.as_ref(), signals);
        let passthrough = guard.decision() == InterceptDecision::Passthrough;
        self.screen.set_mode_hints(mode.raw_mode, mode.echo_off);

        // 6b. on_foreground when guard == Intercept
        if !passthrough {
            if let Some(busy) = signals.foreground_busy() {
                if let Some(done) = self.command_tracker.on_foreground(busy, now) {
                    self.finish_command(done);
                }
            }
        }

        // 8. env.note_mode
        self.env.note_mode(mode.echo_off);

        if passthrough {
            self.cancel_pending_resolve();
            self.clear_live_hints();
            if self.overlay.open {
                self.overlay.open = false;
                self.overlay.candidates.clear();
                self.overlay.error = None;
                self.template = None;
            }
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.guard
            .as_ref()
            .is_some_and(|g| g.decision() == InterceptDecision::Passthrough)
    }

    pub fn command_start_col(&self) -> u16 {
        let typed_len = self.line_buf.chars().count() as u16;
        self.snapshot.cursor.0.saturating_sub(typed_len)
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn git_branch(&mut self) -> Option<&str> {
        let stale = match &self.cached_branch {
            Some((path, _, at)) => path != &self.cwd || at.elapsed() >= Duration::from_secs(2),
            None => true,
        };
        if stale {
            let branch = git_branch_in(&self.cwd);
            self.cached_branch = Some((self.cwd.clone(), branch, Instant::now()));
        }
        self.cached_branch
            .as_ref()
            .and_then(|(_, b, _)| b.as_deref())
    }

    pub fn active_transfer(&self) -> Option<&TransferProgress> {
        let tp = self.transfer_progress.as_ref()?;
        let max_age = if tp.percent == Some(100) {
            Duration::from_millis(1600)
        } else {
            Duration::from_secs(4)
        };
        (tp.updated_at.elapsed() < max_age).then_some(tp)
    }

    pub fn write_pty_bytes(&mut self, bytes: &[u8]) {
        if self.pty.write(bytes).is_ok() && bytes == [0x03] {
            self.transfer_progress = None;
        }
    }

    pub fn refresh_cwd_context(&mut self) {
        let now = Instant::now();
        let need_marker_refresh = match &self.cached_markers {
            Some((cwd, _, at)) => {
                cwd != &self.cwd || now.duration_since(*at) > Duration::from_secs(2)
            }
            None => true,
        };
        if need_marker_refresh {
            let markers = detect_markers(&self.cwd, &self.cfg.resolver.context_markers);
            self.cached_markers = Some((self.cwd.clone(), markers, now));
        }

        let need_branch_refresh = match &self.cached_branch {
            Some((cwd, _, at)) => {
                cwd != &self.cwd || now.duration_since(*at) > Duration::from_secs(2)
            }
            None => true,
        };
        if need_branch_refresh {
            let branch = git_branch_in(&self.cwd);
            self.cached_branch = Some((self.cwd.clone(), branch, now));
        }
    }

    pub fn submit_command(&mut self, command: &str, bytes: Vec<u8>) -> UiIntent {
        let anchor = self.screen.mark_output();
        let now = Instant::now();
        if let Some(done) = self.command_tracker.begin(command.to_string(), anchor, now) {
            self.finish_command(done);
        }
        self.last_outcome = None;
        self.push_history(command);
        self.last_command = Some(command.to_string());
        self.env.note_command(command);
        self.line_buf.clear();
        self.cursor_at_end = true;
        self.clear_live_hints();
        UiIntent::PassToPty(bytes)
    }

    pub fn finish_command(&mut self, done: crate::telemetry::CommandCompletion) {
        let duration = done.started_at.elapsed();
        let output = self
            .screen
            .capture_since(done.anchor, crate::contracts::COMMAND_OUTPUT_MAX_BYTES);
        self.last_outcome = Some(crate::contracts::CommandOutcome {
            command: done.command,
            exit_code: done.exit_code,
            duration,
            output,
            completed_at: Instant::now(),
        });
        self.env.sudo = false;
    }

    pub fn active_ports(&self) -> Vec<crate::contracts::PortHint> {
        let ttl = Duration::from_secs(self.cfg.island.port_ttl_secs);
        let mut ports = self.ports.active(Instant::now(), ttl);
        ports.truncate(self.cfg.island.max_ports);
        ports
    }

    pub fn env_chips(&self) -> Vec<crate::contracts::EnvChip> {
        self.env.chips()
    }

    pub fn island_tint(&self) -> crate::contracts::RiskLevel {
        self.env.tint()
    }

    pub fn last_output(&self) -> Option<&str> {
        self.last_outcome.as_ref().map(|o| o.output.as_str())
    }

    #[allow(dead_code)] // Shortcut: API островка, UI ещё не читает.
    pub fn active_markers(&self) -> &[crate::contracts::ProjectMarker] {
        match &self.cached_markers {
            Some((_, markers, _)) => markers.as_slice(),
            None => &[],
        }
    }

    #[allow(dead_code)]
    pub fn snippets(&self) -> Vec<crate::contracts::SnippetAction> {
        crate::telemetry::snippets_for(self.active_markers(), &self.cfg.island)
    }

    pub fn apply_island_action(
        &mut self,
        action: crate::contracts::IslandAction,
        ctx: &egui::Context,
    ) {
        match action {
            crate::contracts::IslandAction::OpenPort(port) => {
                let url = format!("http://localhost:{port}");
                match std::process::Command::new("xdg-open").arg(&url).spawn() {
                    Ok(_) => self.set_toast(format!("Opened {url}")),
                    Err(_) => self.set_toast(format!("Failed to open {url} (missing xdg-open)")),
                }
            }
            crate::contracts::IslandAction::OpenLink(url) => {
                match std::process::Command::new("xdg-open").arg(&url).spawn() {
                    Ok(_) => {
                        self.set_toast(format!("Opened {}", crate::ui::truncate_text(&url, 32)))
                    }
                    Err(_) => self.set_toast("Failed to open link (missing xdg-open)"),
                }
            }
            crate::contracts::IslandAction::CopyLastOutput => {
                if let Some(outcome) = &self.last_outcome {
                    if !outcome.output.is_empty() {
                        ctx.copy_text(outcome.output.clone());
                        self.set_toast("Copied output");
                    } else {
                        self.set_toast("No output to copy");
                    }
                } else {
                    self.set_toast("No command output yet");
                }
            }
            crate::contracts::IslandAction::FixLastError => {
                self.start_fix_for_last();
            }
            crate::contracts::IslandAction::FillSnippet(cmd) => {
                self.line_buf = cmd.clone();
                self.cursor_at_end = true;
                let mut bytes = Vec::with_capacity(cmd.len() + 1);
                bytes.push(0x15);
                bytes.extend_from_slice(cmd.as_bytes());
                let _ = self.pty.write(&bytes);
            }
            crate::contracts::IslandAction::RunSnippet(cmd) => {
                let req = self.build_request("", Trigger::AfterMiss);
                let analysis = safety::analyze(&cmd, &self.safety_policy, &req.context);
                if analysis.level == crate::contracts::RiskLevel::Safe {
                    let mut bytes = Vec::with_capacity(cmd.len() + 2);
                    bytes.push(0x15);
                    bytes.extend_from_slice(cmd.as_bytes());
                    bytes.push(b'\r');
                    let _ = self.submit_command(&cmd, bytes);
                } else {
                    self.line_buf = cmd.clone();
                    self.cursor_at_end = true;
                    self.set_toast("Review, then press Enter");
                    let mut bytes = Vec::with_capacity(cmd.len() + 1);
                    bytes.push(0x15);
                    bytes.extend_from_slice(cmd.as_bytes());
                    let _ = self.pty.write(&bytes);
                }
            }
            crate::contracts::IslandAction::ReloadConfig => {
                self.reload_config();
            }
        }
    }

    pub fn command_badge(&self) -> Option<crate::contracts::CommandBadge> {
        let now = Instant::now();
        let threshold = Duration::from_millis(self.cfg.island.timer_threshold_ms);
        let ttl = Duration::from_millis(self.cfg.island.status_ttl_ms);

        if let Some(badge) = self.command_tracker.badge(now, threshold) {
            return Some(badge);
        }

        if let Some(outcome) = &self.last_outcome {
            if now.duration_since(outcome.completed_at) < ttl {
                let fix_available = outcome.exit_code != Some(0);
                if outcome.exit_code == Some(0) {
                    return Some(crate::contracts::CommandBadge::Succeeded {
                        duration: outcome.duration,
                    });
                } else {
                    return Some(crate::contracts::CommandBadge::Failed {
                        duration: outcome.duration,
                        exit_code: outcome.exit_code,
                        fix_available,
                    });
                }
            }
        }
        None
    }

    pub fn start_fix_for_last(&mut self) {
        if self.ai_configured() {
            self.start_error_fix();
        } else {
            self.set_toast("AI is off: set API key in Settings");
        }
    }

    pub fn open_launcher(&mut self) {
        if self.is_passthrough() {
            return;
        }
        self.launcher.open = true;
        self.launcher.query.clear();
        self.rebuild_launcher();
    }

    pub fn close_launcher(&mut self) {
        self.launcher.open = false;
        self.launcher.query.clear();
    }

    pub fn rebuild_launcher(&mut self) {
        let q = self.launcher.query.trim().to_lowercase();
        let mut items = Vec::new();

        // 1. History items matching query, deduplicated
        let mut seen = std::collections::HashSet::new();
        for cmd in self.history_tail.iter().rev() {
            let trimmed = cmd.trim();
            if trimmed.is_empty() || !seen.insert(trimmed) {
                continue;
            }
            if q.is_empty() || trimmed.to_lowercase().contains(&q) {
                items.push(crate::contracts::LauncherItem {
                    label: trimmed.to_string(),
                    detail: "History".into(),
                    action: crate::contracts::LauncherAction::FillCommand(trimmed.to_string()),
                });
                if items.len() >= 6 {
                    break;
                }
            }
        }

        // 2. Action items
        let actions = [
            (
                "Settings",
                "Configure LLM, themes, keys",
                crate::contracts::LauncherAction::OpenSettings,
            ),
            (
                "Ask AI",
                "Explain, suggest or debug with LLM",
                crate::contracts::LauncherAction::AskAi,
            ),
            (
                "Reload Config",
                "Reload configuration from disk",
                crate::contracts::LauncherAction::ReloadConfig,
            ),
        ];

        for (lbl, det, act) in actions {
            if q.is_empty() || lbl.to_lowercase().contains(&q) || det.to_lowercase().contains(&q) {
                items.push(crate::contracts::LauncherItem {
                    label: lbl.into(),
                    detail: det.into(),
                    action: act,
                });
            }
        }

        self.launcher.items = items;
        if self.launcher.selected >= self.launcher.items.len() {
            self.launcher.selected = 0;
        }
    }

    pub fn on_launcher_key(&mut self, key: KeyInput) -> UiIntent {
        match key {
            KeyInput::Escape | KeyInput::Ctrl('p') => {
                self.close_launcher();
                UiIntent::Noop
            }
            KeyInput::ArrowUp => {
                if !self.launcher.items.is_empty() {
                    if self.launcher.selected == 0 {
                        self.launcher.selected = self.launcher.items.len() - 1;
                    } else {
                        self.launcher.selected -= 1;
                    }
                }
                UiIntent::Noop
            }
            KeyInput::ArrowDown => {
                if !self.launcher.items.is_empty() {
                    self.launcher.selected =
                        (self.launcher.selected + 1) % self.launcher.items.len();
                }
                UiIntent::Noop
            }
            KeyInput::Backspace => {
                self.launcher.query.pop();
                self.rebuild_launcher();
                UiIntent::Noop
            }
            KeyInput::Char(ch) => {
                self.launcher.query.push(ch);
                self.rebuild_launcher();
                UiIntent::Noop
            }
            KeyInput::Enter => {
                if let Some(item) = self.launcher.items.get(self.launcher.selected).cloned() {
                    self.close_launcher();
                    self.execute_launcher_action(item.action, false)
                } else {
                    self.close_launcher();
                    UiIntent::Noop
                }
            }
            KeyInput::CtrlEnter => {
                if let Some(item) = self.launcher.items.get(self.launcher.selected).cloned() {
                    self.close_launcher();
                    self.execute_launcher_action(item.action, true)
                } else {
                    self.close_launcher();
                    UiIntent::Noop
                }
            }
            _ => UiIntent::Noop,
        }
    }

    pub fn execute_launcher_action(
        &mut self,
        action: crate::contracts::LauncherAction,
        force_run: bool,
    ) -> UiIntent {
        match action {
            crate::contracts::LauncherAction::FillCommand(cmd) => {
                if force_run {
                    let analysis = safety::analyze(
                        &cmd,
                        &self.safety_policy,
                        &self.build_request("", Trigger::AfterMiss).context,
                    );
                    if analysis.level == crate::contracts::RiskLevel::Safe {
                        let mut bytes = Vec::with_capacity(cmd.len() + 2);
                        bytes.push(0x15);
                        bytes.extend_from_slice(cmd.as_bytes());
                        bytes.push(b'\r');
                        return self.submit_command(&cmd, bytes);
                    } else {
                        self.set_toast("Review, then press Enter");
                    }
                }
                self.line_buf = cmd.clone();
                self.cursor_at_end = true;
                let mut bytes = Vec::with_capacity(cmd.len() + 1);
                bytes.push(0x15);
                bytes.extend_from_slice(cmd.as_bytes());
                UiIntent::PassToPty(bytes)
            }
            crate::contracts::LauncherAction::RunCommand(cmd) => {
                let analysis = safety::analyze(
                    &cmd,
                    &self.safety_policy,
                    &self.build_request("", Trigger::AfterMiss).context,
                );
                if analysis.level == crate::contracts::RiskLevel::Safe {
                    let mut bytes = Vec::with_capacity(cmd.len() + 2);
                    bytes.push(0x15);
                    bytes.extend_from_slice(cmd.as_bytes());
                    bytes.push(b'\r');
                    self.submit_command(&cmd, bytes)
                } else {
                    self.line_buf = cmd.clone();
                    self.cursor_at_end = true;
                    self.set_toast("Review, then press Enter");
                    let mut bytes = Vec::with_capacity(cmd.len() + 1);
                    bytes.push(0x15);
                    bytes.extend_from_slice(cmd.as_bytes());
                    UiIntent::PassToPty(bytes)
                }
            }
            crate::contracts::LauncherAction::ToggleTheme => {
                self.toggle_theme_preset();
                UiIntent::Noop
            }
            crate::contracts::LauncherAction::AskAi => {
                self.open_candidates("??", Trigger::Explicit);
                UiIntent::Noop
            }
            crate::contracts::LauncherAction::ReloadConfig => {
                self.reload_config();
                UiIntent::Noop
            }
            crate::contracts::LauncherAction::OpenSettings => {
                self.settings_open = true;
                UiIntent::Noop
            }
        }
    }

    pub fn reload_config(&mut self) {
        match Config::load(self.config_path.as_deref()) {
            Ok(cfg) => {
                self.cfg = cfg;
                self.rebuild_resolver();
                self.set_toast("Config reloaded");
            }
            Err(e) => {
                self.set_toast(format!("Config reload failed: {e}"));
            }
        }
    }

    pub fn toggle_theme_preset(&mut self) {
        if self.theme.is_light() {
            if let Some(preset) = crate::theme::THEME_PRESETS.first() {
                self.apply_theme_preset(preset);
            }
        } else {
            if let Some(preset) = crate::theme::THEME_PRESETS.get(1) {
                self.apply_theme_preset(preset);
            }
        }
    }

    /// Update PTY and screen dimensions when the viewport cell capacity or the
    /// rendered cell pixel size changes. The pixel size must reach the child so
    /// kitty-protocol images (fastfetch) are positioned for the real grid.
    pub fn resize_if_needed(&mut self, cols: u16, rows: u16, cell: egui::Vec2) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let cell_px = (
            cell.x.max(1.0).round() as u32,
            cell.y.max(1.0).round() as u32,
        );
        if cols != self.snapshot.cols || rows != self.snapshot.rows || cell_px != self.cell_px {
            self.cell_px = cell_px;
            self.pty.set_cell_pixels(cell_px.0, cell_px.1);
            self.screen.set_cell_pixels(cell_px.0, cell_px.1);
            let _ = self.pty.resize(cols, rows);
            self.screen.resize(cols, rows);
            self.refresh_snapshot();
        }
    }

    fn key_to_bytes(&self, key: KeyInput) -> Vec<u8> {
        match key {
            KeyInput::Char(ch) => {
                let mut buf = [0u8; 4];
                ch.encode_utf8(&mut buf).as_bytes().to_vec()
            }
            KeyInput::Paste(text) => {
                if self.screen.terminal_mode().bracketed_paste {
                    let mut out = Vec::with_capacity(text.len() + 12);
                    out.extend_from_slice(b"\x1b[200~");
                    out.extend_from_slice(text.as_bytes());
                    out.extend_from_slice(b"\x1b[201~");
                    out
                } else {
                    text.into_bytes()
                }
            }
            KeyInput::Enter => vec![b'\r'],
            KeyInput::CtrlEnter => vec![b'\r'],
            KeyInput::Tab => vec![b'\t'],
            KeyInput::Backslash => vec![b'\\'],
            KeyInput::Escape => vec![0x1b],
            KeyInput::Backspace => vec![0x7f],
            KeyInput::ArrowUp => b"\x1b[A".to_vec(),
            KeyInput::ArrowDown => b"\x1b[B".to_vec(),
            KeyInput::ArrowRight => b"\x1b[C".to_vec(),
            KeyInput::ArrowLeft => b"\x1b[D".to_vec(),
            KeyInput::PageUp | KeyInput::ShiftPageUp => b"\x1b[5~".to_vec(),
            KeyInput::PageDown | KeyInput::ShiftPageDown => b"\x1b[6~".to_vec(),
            KeyInput::AltArrowUp => b"\x1b[1;3A".to_vec(),
            KeyInput::AltArrowDown => b"\x1b[1;3B".to_vec(),
            KeyInput::AltChar(ch) => {
                let mut out = vec![0x1b];
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                out
            }
            KeyInput::Ctrl(ch) | KeyInput::CtrlShift(ch) => {
                let lower = ch.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    vec![(lower as u8) - b'a' + 1]
                } else {
                    match lower {
                        '[' => vec![0x1b],
                        '\\' => vec![0x1c],
                        ']' => vec![0x1d],
                        _ => Vec::new(),
                    }
                }
            }
        }
    }

    fn push_history(&mut self, cmd: &str) {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return;
        }
        self.history_tail.push(trimmed.to_string());
        let cap = self.cfg.general.history_limit.max(1);
        if self.history_tail.len() > cap {
            let excess = self.history_tail.len() - cap;
            self.history_tail.drain(0..excess);
        }
    }

    fn build_request(&self, raw_input: &str, trigger: Trigger) -> ResolveRequest {
        let markers = detect_markers(&self.cwd, &self.cfg.resolver.context_markers);
        ResolveRequest {
            raw_input: raw_input.to_string(),
            context: CwdContext {
                cwd: self.cwd.clone(),
                markers,
                shell: self.cfg.shell(),
                os: std::env::consts::OS.to_string(),
            },
            history_tail: self.history_tail.clone(),
            trigger,
            limit: self.cfg.resolver.max_candidates,
        }
    }

    fn clear_live_hints(&mut self) {
        self.live_hints.clear();
        self.live_hint_selected = None;
    }

    pub fn refresh_live_hints(&mut self) {
        if self.is_passthrough() || self.overlay.open {
            self.clear_live_hints();
            return;
        }
        let query = self.line_buf.trim();
        let effective = query
            .strip_prefix(&self.cfg.llm.explicit_trigger)
            .unwrap_or(query)
            .trim();
        if effective.len() < 2 {
            self.clear_live_hints();
            return;
        }
        let req = self.build_request(query, Trigger::AfterMiss);
        let Some(resolver) = self.resolver.as_mut() else {
            self.clear_live_hints();
            return;
        };
        let mut hints = resolver.resolve_live(&req);
        for cand in &mut hints {
            cand.safety = safety::analyze(&cand.rendered, &self.safety_policy, &req.context);
        }
        self.live_hints = hints;
        self.live_hint_selected = None;
    }

    pub fn ghost_suffix(&self) -> Option<&str> {
        if self.is_passthrough()
            || self.overlay.open
            || !self.cursor_at_end
            || self.line_buf.trim().is_empty()
        {
            return None;
        }
        let typed = self.line_buf.as_str();
        for cand in &self.live_hints {
            if !cand.rendered.contains('\n')
                && cand.rendered.starts_with(typed)
                && cand.rendered.len() > typed.len()
            {
                return Some(&cand.rendered[typed.len()..]);
            }
        }
        None
    }

    /// Fill the selected live hint into the command line without executing.
    /// Opens the minimal slot prompt only if the command still has `{{slot}}` placeholders.
    pub fn fill_live_hint(&mut self, index: usize) {
        let Some(cand) = self.live_hints.get(index).cloned() else {
            return;
        };
        if cand.rendered.contains('\n') || cand.template.contains("{{") {
            self.promote_live_hint(index);
            return;
        }
        let mut bytes = Vec::with_capacity(cand.rendered.len() + 1);
        bytes.push(0x15);
        bytes.extend_from_slice(cand.rendered.as_bytes());
        if self.pty.write(&bytes).is_ok() {
            self.line_buf = cand.rendered;
            self.cursor_at_end = true;
            self.refresh_live_hints();
        }
    }

    pub fn promote_live_hint(&mut self, index: usize) {
        let Some(cand) = self.live_hints.get(index).cloned() else {
            return;
        };
        self.clear_live_hints();
        if let Some(gate) = self.gate.as_mut() {
            gate.cancel();
        }
        self.overlay = OverlayState::with_candidates(vec![cand], None);
        let ctx = self.build_request("", Trigger::AfterMiss).context;
        self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
    }

    pub fn is_loading(&self) -> bool {
        self.pending_resolve.as_ref().is_some_and(|p| !p.cancelled)
    }

    pub fn loading_text(&self) -> Option<String> {
        let pending = self.pending_resolve.as_ref().filter(|p| !p.cancelled)?;
        let idx = (pending.started_at.elapsed().as_millis() / 85) as usize % BRAILLE_SPINNER.len();
        Some(format!("{} {}...", BRAILLE_SPINNER[idx], pending.verb))
    }

    fn cancel_pending_resolve(&mut self) {
        if let Some(pending) = self.pending_resolve.as_mut() {
            pending.cancelled = true;
        }
    }

    /// Heuristic: natural-language prompts (non-ASCII, trailing `?`, or multi-word prose) should route to AI.
    pub fn ai_configured(&self) -> bool {
        self.cfg.llm.enabled
            || !self.cfg.llm.api_key.trim().is_empty()
            || (!matches!(
                self.cfg.llm.provider,
                crate::config::LlmProviderKind::Ollama
            ) && std::env::var(&self.cfg.llm.api_key_env).is_ok_and(|v| !v.trim().is_empty()))
    }

    pub fn looks_like_ai_message(&self, line: &str) -> bool {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.starts_with(&self.cfg.llm.explicit_trigger) {
            return true;
        }
        let words = trimmed.split_whitespace().count();
        let has_non_ascii = !trimmed.is_ascii();
        let ends_question = trimmed.ends_with('?');
        let looks_like_shell_syntax = trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("~/")
            || trimmed.contains('=')
            || trimmed.contains('|')
            || trimmed.contains('>');
        has_non_ascii || ends_question || (words >= 2 && !looks_like_shell_syntax)
    }

    /// Number of visual terminal rows the current command line occupies.
    /// Used to anchor hints/spinners below the command instead of on top of it.
    pub fn command_line_count(&self) -> u16 {
        let typed = self.line_buf.chars().count();
        let cols = usize::from(self.snapshot.cols.max(1));
        let explicit = self.line_buf.matches('\n').count();
        let soft_wraps = typed / cols;
        (explicit + soft_wraps + 1)
            .min(usize::from(self.snapshot.rows.max(1)))
            .max(1) as u16
    }

    /// True when Enter should go to the LLM resolver instead of the raw shell
    /// line. Explicit `??` and prose (non-ASCII / question / unknown `$0`) are
    /// intercepted; real shell syntax and known executables still run locally.
    fn should_route_to_ai(&self, line: &str) -> bool {
        if line.trim().is_empty() {
            return false;
        }
        if line.starts_with(&self.cfg.llm.explicit_trigger) {
            return true;
        }
        if !self.ai_configured() || !self.looks_like_ai_message(line) {
            return false;
        }
        if !line.is_ascii() || line.trim_end().ends_with('?') {
            return true;
        }
        if line.starts_with('/')
            || line.starts_with("./")
            || line.starts_with("~/")
            || line.contains('=')
            || line.contains('|')
            || line.contains('>')
        {
            return false;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        let known = self
            .live_hints
            .iter()
            .any(|c| c.rendered.split_whitespace().next() == Some(first))
            || command_in_path(first);
        !known
    }

    fn finish_candidates(
        &mut self,
        res: Result<Vec<Candidate>, ResolveError>,
        ctx: &CwdContext,
        raw_input: &str,
    ) {
        match res {
            Ok(mut cands) => {
                for cand in &mut cands {
                    cand.safety = safety::analyze(&cand.rendered, &self.safety_policy, ctx);
                }
                if !cands.is_empty() {
                    // Keep candidates strictly inline below the command row (no popup window).
                    self.live_hints = cands;
                    self.live_hint_selected = None;
                    self.overlay.open = false;
                    self.overlay.candidates.clear();
                    self.overlay.error = None;
                } else {
                    self.clear_live_hints();
                    self.overlay = OverlayState::with_candidates(
                        Vec::new(),
                        Some(format!("No matching command for `{}`", raw_input.trim())),
                    );
                }
            }
            Err(err) => {
                self.clear_live_hints();
                self.overlay = OverlayState::with_candidates(Vec::new(), Some(err.to_string()));
            }
        }
    }

    pub fn open_candidates(&mut self, raw_input: &str, trigger: Trigger) {
        let req = self.build_request(raw_input, trigger);
        let Some(resolver) = self.resolver.as_mut() else {
            return;
        };
        if let Some(gate) = self.gate.as_mut() {
            gate.cancel();
        }
        let res = resolver.resolve(&req);
        self.finish_candidates(res, &req.context, raw_input);
    }

    fn next_thinking_verb(&mut self) -> &'static str {
        if self.verb_bag.is_empty() {
            self.verb_bag = shuffled_verb_bag(self.last_verb);
        }
        let verb = self.verb_bag.pop().unwrap_or(THINKING_VERBS[0]);
        self.last_verb = Some(verb);
        verb
    }

    pub fn start_async_candidates(&mut self, raw_input: &str, trigger: Trigger) {
        if self.is_loading() {
            return;
        }
        let Some(mut resolver) = self.resolver.take() else {
            return;
        };
        if let Some(gate) = self.gate.as_mut() {
            gate.cancel();
        }
        self.clear_live_hints();
        let req = self.build_request(raw_input, trigger);
        let raw_owned = raw_input.to_string();
        let ctx_clone = req.context.clone();
        let (tx, rx) = mpsc::channel();
        let verb = self.next_thinking_verb();
        self.pending_resolve = Some(PendingResolve {
            rx,
            started_at: Instant::now(),
            verb,
            cancelled: false,
        });
        std::thread::spawn(move || {
            let res = resolver.resolve(&req);
            let _ = tx.send((resolver, res, ctx_clone, raw_owned));
        });
    }

    #[allow(dead_code)]
    pub fn handle_overlay_event(&mut self, ev: OverlayEvent) {
        match ev {
            OverlayEvent::Confirm => {
                if let Some(bytes) = self.accept_selected() {
                    let _ = self.pty.write(&bytes);
                }
            }
            OverlayEvent::Cancel => {
                if let Some(gate) = self.gate.as_mut() {
                    gate.cancel();
                }
                self.overlay.handle(OverlayEvent::Cancel);
            }
            other => {
                self.overlay.handle(other);
                let ctx = self.build_request("", Trigger::AfterMiss).context;
                self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
            }
        }
    }

    /// Guard-aware key routing. In Passthrough every key becomes raw bytes.
    pub fn on_key(&mut self, key: KeyInput) -> UiIntent {
        if matches!(key, KeyInput::CtrlShift('c')) {
            return UiIntent::CopySelection;
        }
        if matches!(key, KeyInput::ShiftPageUp) {
            return UiIntent::ScrollbackDelta(
                (self.snapshot.rows.saturating_sub(2) as isize).max(1),
            );
        }
        if matches!(key, KeyInput::ShiftPageDown) {
            return UiIntent::ScrollbackDelta(
                -((self.snapshot.rows.saturating_sub(2) as isize).max(1)),
            );
        }
        if self.launcher.open {
            return self.on_launcher_key(key);
        }
        if self.is_passthrough() {
            self.launcher.open = false;
            self.cancel_pending_resolve();
            self.line_buf.clear();
            self.clear_live_hints();
            self.cursor_at_end = true;
            if let Some(gate) = self.gate.as_mut() {
                gate.cancel();
            }
            let bytes = self.key_to_bytes(key);
            return if bytes.is_empty() {
                UiIntent::Noop
            } else {
                UiIntent::PassToPty(bytes)
            };
        }

        if matches!(key, KeyInput::Ctrl('p')) && self.line_buf.is_empty() {
            self.open_launcher();
            return UiIntent::Noop;
        }

        if matches!(key, KeyInput::AltChar('f')) {
            if self
                .last_outcome
                .as_ref()
                .is_some_and(|o| o.exit_code != Some(0))
            {
                return UiIntent::FixLastError;
            } else {
                return UiIntent::PassToPty(self.key_to_bytes(KeyInput::AltChar('f')));
            }
        }
        self.reset_scrollback();
        if matches!(
            key,
            KeyInput::Char(_) | KeyInput::Backspace | KeyInput::Enter
        ) {
            self.clear_selection();
        }

        if self.overlay.open {
            return match key {
                KeyInput::ArrowUp => {
                    self.overlay.error = None;
                    if let Some(gate) = self.gate.as_mut() {
                        gate.cancel();
                    }
                    self.overlay.handle(OverlayEvent::MoveUp);
                    let ctx = self.build_request("", Trigger::AfterMiss).context;
                    self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
                    UiIntent::Noop
                }
                KeyInput::ArrowDown => {
                    self.overlay.error = None;
                    if let Some(gate) = self.gate.as_mut() {
                        gate.cancel();
                    }
                    self.overlay.handle(OverlayEvent::MoveDown);
                    let ctx = self.build_request("", Trigger::AfterMiss).context;
                    self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
                    UiIntent::Noop
                }
                KeyInput::Escape => {
                    if let Some(gate) = self.gate.as_mut() {
                        gate.cancel();
                    }
                    self.overlay.handle(OverlayEvent::Cancel);
                    UiIntent::CloseCandidates
                }
                KeyInput::Enter => UiIntent::ConfirmCandidate,
                KeyInput::Tab => {
                    self.overlay.error = None;
                    self.overlay.handle(OverlayEvent::NextSlot);
                    UiIntent::FocusNextSlot
                }
                KeyInput::Backspace => {
                    self.overlay.error = None;
                    if let Some(gate) = self.gate.as_mut() {
                        gate.cancel();
                    }
                    self.overlay.handle(OverlayEvent::Backspace);
                    let ctx = self.build_request("", Trigger::AfterMiss).context;
                    self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
                    UiIntent::Noop
                }
                KeyInput::Char(ch) => {
                    self.overlay.error = None;
                    if let Some(gate) = self.gate.as_mut() {
                        gate.cancel();
                    }
                    self.overlay.handle(OverlayEvent::Char(ch));
                    let ctx = self.build_request("", Trigger::AfterMiss).context;
                    self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
                    UiIntent::Noop
                }
                KeyInput::Ctrl('c') => {
                    if let Some(gate) = self.gate.as_mut() {
                        gate.cancel();
                    }
                    self.overlay.handle(OverlayEvent::Cancel);
                    self.line_buf.clear();
                    UiIntent::PassToPty(vec![0x03])
                }
                _ => UiIntent::Noop,
            };
        }

        match key {
            KeyInput::Enter => {
                self.cursor_at_end = true;
                let line = self.line_buf.trim().to_string();
                if line.is_empty() {
                    self.clear_live_hints();
                    return UiIntent::PassToPty(vec![b'\r']);
                }
                // Route explicit/prose input to the LLM before an inline history
                // hint can swallow Enter and leak raw text into zsh.
                if self.should_route_to_ai(&line) {
                    return UiIntent::OpenCandidates;
                }
                if let Some(idx) = self.live_hint_selected {
                    if idx < self.live_hints.len() {
                        self.promote_live_hint(idx);
                        return UiIntent::ConfirmCandidate;
                    }
                }
                let req = self.build_request(&line, Trigger::AfterMiss);
                if let Some(mut hit) = self.resolver.as_mut().and_then(|r| r.exact_hit(&req)) {
                    self.clear_live_hints();
                    hit.safety = safety::analyze(&hit.rendered, &self.safety_policy, &req.context);
                    let outcome = if let Some(gate) = self.gate.as_mut() {
                        gate.stage(&hit)
                    } else {
                        StageOutcome::Executable
                    };

                    if outcome == StageOutcome::Executable {
                        if let Some(gate) = self.gate.as_mut() {
                            let _ = gate.confirm();
                        }
                        if hit.source == CandidateSource::Alias && hit.rendered != line {
                            let mut bytes = Vec::with_capacity(hit.rendered.len() + 2);
                            bytes.push(0x15);
                            bytes.extend_from_slice(hit.rendered.as_bytes());
                            bytes.push(b'\r');
                            return self.submit_command(&hit.rendered, bytes);
                        }
                        return self.submit_command(&line, vec![b'\r']);
                    }

                    // Non-safe exact commands (sudo, rm -rf, etc.) must go through the confirm gate.
                    self.overlay = OverlayState::with_candidates(vec![hit], None);
                    return UiIntent::Noop;
                }
                // No AI/Alias match: this is a plain shell line, so run it as typed.
                self.submit_command(&line, vec![b'\r'])
            }
            KeyInput::CtrlEnter => {
                // Interrupt: drop hints/overlay, clear the line and stop the running command.
                self.cancel_pending_resolve();
                if let Some(gate) = self.gate.as_mut() {
                    gate.cancel();
                }
                self.overlay.handle(OverlayEvent::Cancel);
                self.clear_live_hints();
                self.line_buf.clear();
                self.cursor_at_end = true;
                UiIntent::PassToPty(vec![0x03])
            }
            KeyInput::Char(ch) => {
                self.cancel_pending_resolve();
                self.line_buf.push(ch);
                self.cursor_at_end = true;
                self.refresh_live_hints();
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::Char(ch)))
            }
            KeyInput::Backslash => {
                self.cancel_pending_resolve();
                self.line_buf.push('\\');
                self.cursor_at_end = true;
                self.refresh_live_hints();
                UiIntent::PassToPty(vec![b'\\'])
            }
            KeyInput::Backspace => {
                self.cancel_pending_resolve();
                self.line_buf.pop();
                self.cursor_at_end = true;
                self.refresh_live_hints();
                UiIntent::PassToPty(vec![0x7f])
            }
            KeyInput::ArrowLeft => {
                self.cursor_at_end = false;
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::ArrowLeft))
            }
            KeyInput::ArrowRight => {
                if let Some(idx) = self.live_hint_selected {
                    if let Some(cand) = self.live_hints.get(idx).cloned() {
                        if cand.rendered.contains('\n') {
                            self.promote_live_hint(idx);
                            return UiIntent::Noop;
                        }
                        let bytes =
                            if self.cursor_at_end && cand.rendered.starts_with(&self.line_buf) {
                                cand.rendered.as_bytes()[self.line_buf.len()..].to_vec()
                            } else {
                                let mut buf = Vec::with_capacity(cand.rendered.len() + 1);
                                buf.push(0x15);
                                buf.extend_from_slice(cand.rendered.as_bytes());
                                buf
                            };
                        self.line_buf = cand.rendered;
                        self.cursor_at_end = true;
                        self.refresh_live_hints();
                        return UiIntent::PassToPty(bytes);
                    }
                }
                if self.cursor_at_end {
                    if let Some(suffix) = self.ghost_suffix().map(ToOwned::to_owned) {
                        self.line_buf.push_str(&suffix);
                        self.refresh_live_hints();
                        return UiIntent::PassToPty(suffix.into_bytes());
                    }
                }
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::ArrowRight))
            }
            KeyInput::ArrowDown => {
                if !self.live_hints.is_empty()
                    && (!self.line_buf.trim().is_empty() || self.live_hint_selected.is_some())
                {
                    let next = match self.live_hint_selected {
                        None => 0,
                        Some(i) => (i + 1).min(self.live_hints.len() - 1),
                    };
                    self.live_hint_selected = Some(next);
                    return UiIntent::Noop;
                }
                self.clear_live_hints();
                self.line_buf.clear();
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::ArrowDown))
            }
            KeyInput::ArrowUp => {
                if let Some(idx) = self.live_hint_selected {
                    self.live_hint_selected = if idx == 0 { None } else { Some(idx - 1) };
                    return UiIntent::Noop;
                }
                self.clear_live_hints();
                self.line_buf.clear();
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::ArrowUp))
            }
            KeyInput::AltArrowDown => {
                if !self.live_hints.is_empty() {
                    let next = match self.live_hint_selected {
                        None => 0,
                        Some(i) => (i + 1).min(self.live_hints.len() - 1),
                    };
                    self.live_hint_selected = Some(next);
                    return UiIntent::Noop;
                }
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::AltArrowDown))
            }
            KeyInput::AltArrowUp => {
                if !self.live_hints.is_empty() {
                    self.live_hint_selected = match self.live_hint_selected {
                        None => Some(self.live_hints.len() - 1),
                        Some(0) => None,
                        Some(i) => Some(i - 1),
                    };
                    return UiIntent::Noop;
                }
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::AltArrowUp))
            }
            KeyInput::Escape => {
                if self.is_loading() {
                    self.cancel_pending_resolve();
                    return UiIntent::Noop;
                }
                if self.live_hint_selected.is_some() {
                    self.live_hint_selected = None;
                    return UiIntent::Noop;
                }
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::Escape))
            }
            KeyInput::Tab => {
                self.clear_live_hints();
                UiIntent::PassToPty(self.key_to_bytes(KeyInput::Tab))
            }
            KeyInput::Ctrl('w') => {
                self.cancel_pending_resolve();
                while self
                    .line_buf
                    .chars()
                    .last()
                    .is_some_and(char::is_whitespace)
                {
                    self.line_buf.pop();
                }
                while self
                    .line_buf
                    .chars()
                    .last()
                    .is_some_and(|c| !c.is_whitespace())
                {
                    self.line_buf.pop();
                }
                self.cursor_at_end = true;
                self.refresh_live_hints();
                UiIntent::PassToPty(self.key_to_bytes(key))
            }
            KeyInput::Ctrl('c') | KeyInput::Ctrl('u') | KeyInput::Ctrl('d') => {
                self.cancel_pending_resolve();
                self.line_buf.clear();
                self.clear_live_hints();
                self.cursor_at_end = true;
                UiIntent::PassToPty(self.key_to_bytes(key))
            }
            KeyInput::Paste(ref text) => {
                let bracketed = self.screen.terminal_mode().bracketed_paste;
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                self.cancel_pending_resolve();
                self.line_buf.push_str(&normalized);
                self.cursor_at_end = true;
                if normalized.contains('\n') {
                    self.clear_live_hints();
                    // Without bracketed paste each newline executes immediately;
                    // the shell buffer is gone, so do not keep a stale mirror.
                    if !bracketed {
                        self.line_buf.clear();
                    }
                } else {
                    self.refresh_live_hints();
                }
                UiIntent::PassToPty(self.key_to_bytes(key))
            }
            other => {
                let bytes = self.key_to_bytes(other);
                if bytes.is_empty() {
                    UiIntent::Noop
                } else {
                    UiIntent::PassToPty(bytes)
                }
            }
        }
    }

    /// Confirm the overlay selection; returns bytes to write to the PTY, if any.
    /// Also persists an accepted LLM/cached candidate to history and cache.
    pub fn accept_selected(&mut self) -> Option<Vec<u8>> {
        let ctx = self.build_request("", Trigger::AfterMiss).context;
        self.overlay.reanalyze_selected(&self.safety_policy, &ctx);
        let cand = self.overlay.selected_candidate()?.clone();

        let cmd = if let Some(gate) = self.gate.as_mut() {
            let already_armed = gate.pending().is_some_and(|p| p.rendered == cand.rendered)
                && gate.requires_second_confirm();
            let accepted = if already_armed {
                gate.force()
            } else {
                let _ = gate.stage(&cand);
                gate.confirm()
            };
            match accepted {
                Some(pending) => pending.rendered,
                None => {
                    self.overlay.error =
                        Some("DANGEROUS command: press Enter again to confirm".into());
                    return None;
                }
            }
        } else {
            cand.rendered.clone()
        };

        self.overlay.open = false;
        self.overlay.candidates.clear();
        self.overlay.error = None;
        self.template = None;

        let mut bytes = Vec::with_capacity(cmd.len() + 2);
        bytes.push(0x15);
        bytes.extend_from_slice(cmd.as_bytes());
        bytes.push(b'\r');

        let intent = self.submit_command(&cmd, bytes);
        if let UiIntent::PassToPty(p_bytes) = intent {
            Some(p_bytes)
        } else {
            None
        }
    }

    /// Recompute `GridSnapshot` from the screen and request a repaint.
    pub fn refresh_snapshot(&mut self) {
        self.snapshot = self.screen.snapshot();
        if self.snapshot.bell_count > self.last_bell_count {
            self.last_bell_count = self.snapshot.bell_count;
            self.bell_flash_until = Some(Instant::now() + Duration::from_millis(160));
        }
    }

    pub fn terminal_state_label(&self) -> Option<String> {
        if self.is_passthrough() {
            if let Some(title) = self
                .snapshot
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(title.to_string());
            }
            return Some("passthrough".to_string());
        }
        if let Some(title) = self
            .snapshot
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(title.to_string());
        }
        self.last_command
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|cmd| format!("last: {cmd}"))
    }

    #[allow(dead_code)]
    pub fn minimize_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    pub fn adjust_font_size(&mut self, delta: f32) {
        let next = (self.cfg.theme.font_size + delta).clamp(10.0, 26.0);
        self.cfg.theme.font_size = next;
        self.theme.font_size = next;
        self.status_note = None;
    }

    pub fn apply_theme_from_cfg(&mut self) {
        match ThemePalette::from_section(&self.cfg.theme) {
            Ok(palette) => {
                self.theme = palette;
                self.status_note = None;
            }
            Err(err) => {
                self.status_note = Some(format!("Invalid theme color: {err}"));
            }
        }
    }

    #[allow(dead_code)]
    pub fn set_theme_background(&mut self, hex: &str) {
        self.cfg.theme.background = hex.to_string();
        if let Ok(bg) = crate::theme::Rgb::from_hex(hex) {
            let lum = (bg.r as u32 * 299 + bg.g as u32 * 587 + bg.b as u32 * 114) / 1000;
            if lum >= 128 {
                self.cfg.theme.foreground = "#252527".to_string();
                self.cfg.theme.accent = "#444448".to_string();
                self.cfg.theme.selection = "#dcdcd9".to_string();
                self.cfg.theme.overlay_bg = "#eaeae7".to_string();
            } else {
                self.cfg.theme.foreground = "#e4e4e4".to_string();
                self.cfg.theme.accent = "#d4d4d4".to_string();
                self.cfg.theme.selection = "#363639".to_string();
                self.cfg.theme.overlay_bg = "#2c2c2f".to_string();
            }
        }
        self.apply_theme_from_cfg();
    }

    pub fn apply_theme_preset(&mut self, preset: &crate::theme::ThemePreset) {
        self.cfg.theme.background = preset.background.to_string();
        self.cfg.theme.foreground = preset.foreground.to_string();
        self.cfg.theme.accent = preset.accent.to_string();
        self.cfg.theme.warning = preset.warning.to_string();
        self.cfg.theme.danger = preset.danger.to_string();
        self.cfg.theme.selection = preset.selection.to_string();
        self.cfg.theme.overlay_bg = preset.overlay_bg.to_string();
        self.apply_theme_from_cfg();
    }

    pub fn reset_theme_defaults(&mut self) {
        self.cfg.theme = Config::default().theme;
        self.apply_theme_from_cfg();
        self.set_toast("Theme reset to Key Gray");
    }

    pub fn set_prompt_icon(&mut self, icon: PromptIcon) {
        self.cfg.theme.prompt_icon = icon;
        self.apply_theme_from_cfg();
    }

    pub fn set_language(&mut self, lang: crate::config::UiLanguage) {
        self.cfg.general.language = lang;
        self.save_config();
    }

    pub fn paste_from_clipboard(&mut self) {
        if self.pending_paste.is_some() {
            return;
        }
        // Clipboard helpers can block indefinitely, so read them off the UI thread.
        let (tx, rx) = mpsc::channel();
        self.pending_paste = Some(rx);
        std::thread::spawn(move || {
            let cmds: [(&str, &[&str]); 3] = [
                ("wl-paste", &["-n"]),
                ("xclip", &["-o", "-selection", "clipboard"]),
                ("xsel", &["-ob"]),
            ];
            let text = cmds.iter().find_map(|&(prog, args)| {
                let out = std::process::Command::new(prog)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .ok()?;
                if !out.status.success() {
                    return None;
                }
                String::from_utf8(out.stdout).ok().filter(|t| !t.is_empty())
            });
            let _ = tx.send(text);
        });
    }

    fn apply_paste_text(&mut self, text: String) {
        if let UiIntent::PassToPty(bytes) = self.on_key(KeyInput::Paste(text)) {
            self.write_pty_bytes(&bytes);
        }
    }

    pub fn rebuild_resolver(&mut self) {
        match ResolverChain::build(&self.cfg.resolver, &self.cfg) {
            Ok(resolver) => {
                self.resolver = Some(resolver);
                self.status_note =
                    crate::resolver::llm::LlmResolver::effective_instructions(&self.cfg.llm).1;
            }
            Err(err) => {
                self.status_note = Some(err.to_string());
            }
        }
    }

    pub fn toggle_llm(&mut self) {
        self.cfg.llm.enabled = !self.cfg.llm.enabled;
        self.rebuild_resolver();
    }

    pub fn set_llm_provider(&mut self, provider: LlmProviderKind) {
        let (endpoint, model, key_env) = provider.defaults();
        self.cfg.llm.provider = provider;
        self.cfg.llm.endpoint = endpoint.to_string();
        self.cfg.llm.model = model.to_string();
        if !key_env.is_empty() {
            self.cfg.llm.api_key_env = key_env.to_string();
        }
        self.rebuild_resolver();
    }

    pub fn toggle_confirm_dangerous(&mut self) {
        self.cfg.safety.confirm_dangerous = !self.cfg.safety.confirm_dangerous;
        self.gate = Some(ConfirmGate::new(self.cfg.safety.confirm_dangerous));
        self.status_note = None;
    }

    pub fn reset_defaults(&mut self) {
        self.cfg = Config::default();
        self.apply_theme_from_cfg();
        self.toggle_confirm_dangerous_sync();
        self.rebuild_resolver();
        self.status_note = Some("Defaults restored (unsaved)".into());
    }

    fn toggle_confirm_dangerous_sync(&mut self) {
        self.gate = Some(ConfirmGate::new(self.cfg.safety.confirm_dangerous));
    }

    pub fn save_config(&mut self) {
        if !self.cfg.llm.api_key.trim().is_empty() {
            self.cfg.llm.enabled = true;
        }
        self.rebuild_resolver();
        match self.cfg.save(self.config_path.as_deref()) {
            Ok(()) => self.status_note = Some("Saved".into()),
            Err(err) => self.status_note = Some(format!("Save failed: {err}")),
        }
    }

    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn active_toast(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(3))
            .map(|(m, _)| m.as_str())
    }

    pub fn view_row_to_dist(&self, view_row: u16) -> usize {
        self.snapshot.scrollback_offset
            + usize::from(
                self.snapshot
                    .rows
                    .saturating_sub(1)
                    .saturating_sub(view_row),
            )
    }

    pub fn dist_to_view_row(&self, dist: usize) -> isize {
        (self.snapshot.rows as isize - 1) + self.snapshot.scrollback_offset as isize - dist as isize
    }

    pub fn active_selection(&self) -> Option<((u16, isize), (u16, isize))> {
        let a = self.selection_anchor?;
        let b = self.selection_end?;
        if a == b {
            None
        } else {
            Some((
                (a.0, self.dist_to_view_row(a.1)),
                (b.0, self.dist_to_view_row(b.1)),
            ))
        }
    }

    pub fn extract_logical_selection(&self) -> String {
        let (Some(a), Some(b)) = (self.selection_anchor, self.selection_end) else {
            return String::new();
        };
        if a == b {
            return String::new();
        }
        let (top, bot) = if a.1 > b.1 || (a.1 == b.1 && a.0 <= b.0) {
            (a, b)
        } else {
            (b, a)
        };
        let mut lines = Vec::new();
        for dist in (bot.1..=top.1).rev() {
            let row_str = self.screen.row_text(dist).unwrap_or_else(|| {
                let vr = self.dist_to_view_row(dist);
                if vr >= 0 && (vr as u16) < self.snapshot.rows {
                    let r = vr as u16;
                    (0..self.snapshot.cols)
                        .filter_map(|c| self.snapshot.cell(c, r))
                        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                        .collect()
                } else {
                    String::new()
                }
            });
            let chars: Vec<char> = row_str.chars().collect();
            let from_c = if dist == top.1 { usize::from(top.0) } else { 0 };
            let to_c = if dist == bot.1 {
                usize::from(bot.0).min(chars.len().saturating_sub(1))
            } else {
                chars.len().saturating_sub(1)
            };
            if from_c <= to_c && from_c < chars.len() {
                let slice: String = chars[from_c..=to_c].iter().collect();
                lines.push(slice.trim_end().to_string());
            } else {
                lines.push(String::new());
            }
        }
        lines.join("\n")
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_end = None;
    }

    pub fn scroll_by(&mut self, delta: isize) {
        if self.snapshot.alternate_screen {
            return;
        }
        let cur = self.screen.scrollback() as isize;
        let next = (cur + delta).max(0) as usize;
        if next != cur as usize {
            self.screen.set_scrollback(next);
            self.refresh_snapshot();
        }
    }

    pub fn reset_scrollback(&mut self) {
        if self.screen.scrollback() > 0 {
            self.screen.set_scrollback(0);
            self.refresh_snapshot();
        }
    }

    /// Recompute case-insensitive scrollback + screen matches for `self.search_query`.
    pub fn update_search_matches(&mut self) {
        self.search_matches.clear();
        self.search_index = 0;
        let query = self.search_query.trim();
        if query.is_empty() {
            return;
        }
        let needle = query.to_lowercase();
        let needle_chars = needle.chars().count();
        let total_rows = self.snapshot.scrollback_total + usize::from(self.snapshot.rows);
        for dist in 0..total_rows {
            let Some(row) = self.screen.row_text(dist) else {
                continue;
            };
            let lower = row.to_lowercase();
            let mut byte_pos = 0;
            while let Some(rel) = lower[byte_pos..].find(&needle) {
                let start_byte = byte_pos + rel;
                let col_start = lower[..start_byte].chars().count() as u16;
                let col_end = col_start.saturating_add(needle_chars as u16);
                self.search_matches.push((dist, col_start, col_end));
                byte_pos = start_byte + needle.len().max(1);
                if self.search_matches.len() >= 1000 {
                    return;
                }
            }
        }
        if !self.search_matches.is_empty() {
            self.jump_to_search_match(0);
        }
    }

    pub fn step_search_match(&mut self, forward: bool) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len();
        let next = if forward {
            (self.search_index + 1) % len
        } else {
            (self.search_index + len - 1) % len
        };
        self.jump_to_search_match(next);
    }

    pub fn jump_to_search_match(&mut self, idx: usize) {
        if idx >= self.search_matches.len() {
            return;
        }
        self.search_index = idx;
        let (dist, _, _) = self.search_matches[idx];
        if !self.snapshot.alternate_screen {
            let rows = usize::from(self.snapshot.rows.max(1));
            let target_offset = (dist + rows / 2).saturating_sub(rows.saturating_sub(1));
            self.screen.set_scrollback(target_offset);
            self.refresh_snapshot();
        }
    }

    pub fn visible_search_hits(&self) -> Vec<(u16, u16, u16, bool)> {
        if !self.search_open || self.search_matches.is_empty() {
            return Vec::new();
        }
        let rows = self.snapshot.rows as isize;
        self.search_matches
            .iter()
            .enumerate()
            .filter_map(|(idx, &(dist, c0, c1))| {
                let vr = self.dist_to_view_row(dist);
                if vr >= 0 && vr < rows {
                    Some((vr as u16, c0, c1, idx == self.search_index))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn screen_tail_text(&self, max_lines: usize) -> String {
        let mut lines = Vec::new();
        for row in 0..self.snapshot.rows {
            let mut line = String::new();
            for col in 0..self.snapshot.cols {
                let ch = self.snapshot.cell(col, row).map(|c| c.ch).unwrap_or(' ');
                line.push(if ch == '\0' { ' ' } else { ch });
            }
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                lines.push(trimmed.to_string());
            }
        }
        if lines.len() > max_lines {
            lines.drain(0..lines.len() - max_lines);
        }
        lines.join("\n")
    }

    pub fn start_error_fix(&mut self) {
        if !self.ai_configured() {
            self.set_toast("AI is off: enable LLM or set API key in Settings");
            return;
        }
        let tail = self.screen_tail_text(18);
        if tail.trim().is_empty() {
            self.set_toast("No terminal output to analyze");
            return;
        }
        let masked = crate::resolver::llm::LlmResolver::mask(&tail).text;
        let query =
            format!("?? Fix the last failed command based on this terminal output:\n{masked}");
        self.start_async_candidates(&query, Trigger::Explicit);
    }

    pub fn clear_ai_cache(&mut self) {
        let count = self.resolver.as_mut().map_or(0, |r| r.clear_cache());
        self.set_toast(format!("Cleared {count} cached entries"));
        self.status_note = Some(format!("Cleared {count} cached entries"));
    }

    pub fn start_provider_probe(&mut self) {
        if self.pending_probe.is_some() {
            return;
        }
        let settings = self.cfg.llm.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_probe = Some(rx);
        self.probe_status = Some((true, "Testing...".into()));
        std::thread::spawn(move || {
            let res = crate::resolver::llm::LlmResolver::test_connection(settings);
            let _ = tx.send(res);
        });
    }
}

impl Drop for LexicueApp {
    fn drop(&mut self) {
        let _ = self.pty.kill();
    }
}

impl eframe::App for LexicueApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick();
        let next_title = match self.snapshot.title.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => format!("{t} — Lexicue"),
            _ => "Lexicue".to_owned(),
        };
        if next_title != self.last_window_title {
            self.last_window_title = next_title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(next_title));
        }
        crate::ui::draw(self, ctx);

        let pending_actions = std::mem::take(&mut self.pending_island_actions);
        for action in pending_actions {
            self.apply_island_action(action, ctx);
        }

        let events = ctx.input(|i| i.events.clone());
        let mut wrote = false;
        for key in crate::ui::translate_input(&events) {
            if self.settings_open {
                if key == KeyInput::Escape {
                    self.settings_open = false;
                }
                continue;
            }
            if key == KeyInput::CtrlShift('f') {
                self.search_open = true;
                self.search_focus_requested = true;
                self.update_search_matches();
                continue;
            }
            if self.search_open {
                if key == KeyInput::Escape {
                    self.search_open = false;
                }
                continue;
            }
            if !self.top_island_pinned && self.top_island_open && key == KeyInput::Escape {
                self.top_island_open = false;
                continue;
            }
            match self.on_key(key) {
                UiIntent::PassToPty(bytes) => {
                    if self.pty.write(&bytes).is_ok() {
                        wrote = true;
                    }
                }
                UiIntent::OpenCandidates => {
                    let line = self.line_buf.trim().to_string();
                    let explicit = line.starts_with(&self.cfg.llm.explicit_trigger);
                    let nl = self.looks_like_ai_message(&line);
                    if !explicit && nl && !self.ai_configured() {
                        self.clear_live_hints();
                        self.overlay = OverlayState::with_candidates(
                            Vec::new(),
                            Some(
                                "AI is off: enable LLM or set API key in Settings (or use ??)"
                                    .into(),
                            ),
                        );
                    } else {
                        let trigger = if explicit || (self.ai_configured() && nl) {
                            Trigger::Explicit
                        } else {
                            Trigger::AfterMiss
                        };
                        self.start_async_candidates(&line, trigger);
                        wrote = true;
                    }
                }
                UiIntent::ConfirmCandidate => {
                    if let Some(bytes) = self.accept_selected() {
                        if self.pty.write(&bytes).is_ok() {
                            wrote = true;
                        }
                    }
                }
                UiIntent::CopySelection => {
                    let text = self.extract_logical_selection();
                    if !text.is_empty() {
                        ctx.copy_text(text);
                        self.set_toast("Copied");
                    }
                }
                UiIntent::FixLastError => {
                    self.start_fix_for_last();
                    wrote = true;
                }
                UiIntent::ScrollbackDelta(delta) => {
                    self.scroll_by(delta);
                }
                UiIntent::OpenLauncher => {
                    self.top_island_open = true;
                }
                UiIntent::CopyLastOutput => {}
                UiIntent::CloseCandidates | UiIntent::FocusNextSlot | UiIntent::Noop => {}
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) || !self.pty.is_alive() {
            let _ = self.pty.kill();
            if !self.pty.is_alive() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if wrote || self.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(80));
        } else {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("startup failed: {0}")]
    Startup(String),
    #[error("pty failed: {0}")]
    Pty(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aliases::AliasTable;
    use crate::config::ResolverSettings;
    use crate::pty::PtyError;
    use crate::resolver::exact::ExactResolver;
    use crate::resolver::local::LocalResolver;

    struct NullPty;
    impl PtyBackend for NullPty {
        fn spawn(&mut self, _spec: SpawnSpec) -> Result<(), PtyError> {
            Ok(())
        }
        fn write(&mut self, _bytes: &[u8]) -> Result<(), PtyError> {
            Ok(())
        }
        fn resize(&mut self, _cols: u16, _rows: u16) -> Result<(), PtyError> {
            Ok(())
        }
        fn drain(&mut self) -> Result<Vec<u8>, PtyError> {
            Ok(Vec::new())
        }
        fn is_alive(&mut self) -> bool {
            true
        }
        fn kill(&mut self) -> Result<(), PtyError> {
            Ok(())
        }
    }

    fn test_app_with_history(history: &[&str]) -> LexicueApp {
        let cfg = Config::default();
        let theme = ThemePalette::dark();
        let screen = Box::new(Vt100Screen::new(80, 24, 100));
        let snapshot = screen.snapshot();
        let guard = PassthroughGuard::new(GuardConfig::default());

        let mut local = LocalResolver::new(ResolverSettings {
            aliases_rc: Vec::new(),
            user_aliases: PathBuf::from("/tmp/nonexistent_aliases.toml"),
            dictionary_dirs: Vec::new(),
            context_markers: Vec::new(),
            max_candidates: 8,
            history_bonus: 1.5,
        })
        .unwrap();
        let hist: Vec<String> = history.iter().map(|s| (*s).to_string()).collect();
        local.rebuild(&hist).unwrap();

        let exact = ExactResolver::new(AliasTable::default(), Vec::new());
        let resolver = ResolverChain::from_stages(vec![Box::new(exact), Box::new(local)], 8);

        LexicueApp {
            cfg,
            config_path: None,
            theme,
            pty: Box::new(NullPty),
            screen,
            guard: Some(guard),
            snapshot,
            cell_px: (0, 0),
            resolver: Some(resolver),
            pending_resolve: None,
            overlay: OverlayState::default(),
            live_hints: Vec::new(),
            live_hint_selected: None,
            settings_open: false,
            top_island_open: false,
            top_island_pinned: false,
            transfer_progress: None,
            cached_branch: None,
            cached_markers: None,
            ports: PortScanner::new(),
            env: EnvContext::detect(),
            command_tracker: CommandTracker::new(),
            marker_parser: MarkerParser::new(),
            launcher: crate::contracts::LauncherState::default(),
            last_outcome: None,
            pending_island_actions: Vec::new(),
            last_command: None,
            status_note: None,
            template: None,
            gate: Some(ConfirmGate::new(true)),
            safety_policy: SafetyPolicy::default(),
            cwd: PathBuf::from("/tmp"),
            line_buf: String::new(),
            cursor_at_end: true,
            history_tail: hist,
            verb_bag: shuffled_verb_bag(None),
            last_verb: None,
            selection_anchor: None,
            selection_end: None,
            last_drag_scroll: None,
            toast: None,
            pending_probe: None,
            pending_paste: None,
            probe_status: None,
            last_window_title: "Lexicue".to_owned(),
            primary_selection: None,
            last_mouse_cell: None,
            search_open: false,
            search_focus_requested: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_index: 0,
            last_bell_count: 0,
            bell_flash_until: None,
        }
    }

    #[test]
    fn live_hints_and_ghost_suffix_update_while_typing_and_accept_with_right_arrow() {
        let mut app = test_app_with_history(&["git status -sb"]);

        for ch in "git st".chars() {
            let intent = app.on_key(KeyInput::Char(ch));
            assert!(matches!(intent, UiIntent::PassToPty(_)));
        }

        assert!(!app.live_hints.is_empty());
        assert_eq!(app.ghost_suffix(), Some("atus -sb"));

        // ArrowRight at end of line accepts the ghost suffix into the PTY & line buffer.
        assert_eq!(
            app.on_key(KeyInput::ArrowRight),
            UiIntent::PassToPty(b"atus -sb".to_vec())
        );
        assert_eq!(app.line_buf, "git status -sb");
        assert_eq!(app.ghost_suffix(), None);
    }

    #[test]
    fn backspace_updates_live_hints_and_passthrough_clears_them() {
        let mut app = test_app_with_history(&["cargo test --workspace"]);
        for ch in "cargo te".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        assert_eq!(app.ghost_suffix(), Some("st --workspace"));

        app.on_key(KeyInput::Backspace);
        assert_eq!(app.line_buf, "cargo t");
        assert_eq!(app.ghost_suffix(), Some("est --workspace"));

        // Enter alternate screen (TUI) -> guard switches to Passthrough and clears hints.
        app.screen.feed(b"\x1b[?1049h");
        app.observe_guard(crate::pty::guard::ShellSignals::default(), Instant::now());
        assert!(app.is_passthrough());
        assert!(app.live_hints.is_empty());
        assert_eq!(app.ghost_suffix(), None);
    }

    #[test]
    fn arrow_navigation_respects_shell_history_and_selects_hints_without_conflict() {
        let mut app = test_app_with_history(&["git status -sb", "git stash list"]);

        // Empty prompt: ArrowUp/ArrowDown go straight to PTY shell history.
        assert_eq!(
            app.on_key(KeyInput::ArrowUp),
            UiIntent::PassToPty(b"\x1b[A".to_vec())
        );
        assert_eq!(app.live_hint_selected, None);

        // Type partial command to trigger multiple live hints.
        for ch in "git st".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        assert!(app.live_hints.len() >= 2);
        assert_eq!(app.live_hint_selected, None);

        // Plain ArrowUp while unselected still triggers shell history and clears hints.
        assert_eq!(
            app.on_key(KeyInput::ArrowUp),
            UiIntent::PassToPty(b"\x1b[A".to_vec())
        );
        assert!(app.live_hints.is_empty());

        // Type again, then use ArrowDown (or AltArrowDown) to navigate inline hints.
        for ch in "git st".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        assert_eq!(app.on_key(KeyInput::ArrowDown), UiIntent::Noop);
        assert_eq!(app.live_hint_selected, Some(0));

        assert_eq!(app.on_key(KeyInput::AltArrowDown), UiIntent::Noop);
        assert_eq!(app.live_hint_selected, Some(1));

        // ArrowUp navigates back up and returns to prompt (None) from index 0.
        assert_eq!(app.on_key(KeyInput::ArrowUp), UiIntent::Noop);
        assert_eq!(app.live_hint_selected, Some(0));
        assert_eq!(app.on_key(KeyInput::ArrowUp), UiIntent::Noop);
        assert_eq!(app.live_hint_selected, None);
    }

    #[test]
    fn non_exact_enter_and_open_candidates_stay_inline_without_duplicate_popup() {
        let mut app = test_app_with_history(&["git status -sb"]);
        for ch in "git st".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        // Enter runs the typed line even while inline hints are visible.
        assert_eq!(
            app.on_key(KeyInput::Enter),
            UiIntent::PassToPty(vec![b'\r'])
        );
        assert!(app.live_hints.is_empty());
        assert!(!app.overlay.open);

        // Explicit open_candidates also keeps results inline without opening a second popup window.
        app.open_candidates("git st", Trigger::Explicit);
        assert!(!app.live_hints.is_empty());
        assert_eq!(app.live_hint_selected, None);
        assert!(!app.overlay.open);
    }

    #[test]
    fn natural_language_routes_to_ai_when_enabled_and_spinner_renders_braille() {
        let mut app = test_app_with_history(&["ss -tulpn"]);
        app.cfg.llm.enabled = true;

        assert!(app.looks_like_ai_message("найди большие файлы"));
        assert!(app.looks_like_ai_message("show open ports?"));
        assert!(app.looks_like_ai_message("list running containers"));
        assert!(!app.looks_like_ai_message("./build.sh"));
        assert!(!app.looks_like_ai_message("FOO=bar"));

        // Non-ASCII natural-language prompt triggers AI candidate flow cleanly.
        for ch in "покажи порты".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        assert_eq!(app.on_key(KeyInput::Enter), UiIntent::OpenCandidates);

        // Braille spinner + random Claude-style -ing verb.
        let mut seen = std::collections::HashSet::new();
        let mut prev = None;
        for _ in 0..THINKING_VERBS.len() {
            let v = app.next_thinking_verb();
            assert_ne!(Some(v), prev);
            prev = Some(v);
            seen.insert(v);
        }
        assert!(THINKING_VERBS.len() >= 30);
        assert_eq!(seen.len(), THINKING_VERBS.len());

        let (_tx, rx) = mpsc::channel();
        app.pending_resolve = Some(PendingResolve {
            rx,
            started_at: Instant::now(),
            verb: "Ruminating",
            cancelled: false,
        });
        let spinner = app.loading_text().unwrap();
        assert!(spinner.contains("Ruminating..."));
        assert!(BRAILLE_SPINNER.iter().any(|c| spinner.starts_with(*c)));
    }

    #[test]
    fn paste_mirrors_line_buf_and_ai_enter_preempts_stale_hints() {
        let mut app = test_app_with_history(&["git stash list"]);
        app.screen.feed(b"\x1b[?2004h");
        let intent = app.on_key(KeyInput::Paste("первая строка\nвторая строка".into()));
        assert!(matches!(intent, UiIntent::PassToPty(_)));
        assert_eq!(app.line_buf, "первая строка\nвторая строка");
        assert_eq!(app.command_line_count(), 2);

        // A previously selected inline hint must not steal Enter from the LLM.
        app.live_hints = vec![Candidate {
            id: 1,
            template: "git stash list".into(),
            rendered: "git stash list".into(),
            source: CandidateSource::History,
            score: 1.0,
            safety: Default::default(),
            note: None,
        }];
        app.live_hint_selected = Some(0);
        app.cfg.llm.enabled = true;
        assert_eq!(app.on_key(KeyInput::Enter), UiIntent::OpenCandidates);
    }

    #[test]
    fn api_key_enables_ai_routing_and_ctrl_w_updates_line_buf() {
        let mut app = test_app_with_history(&["systemctl restart nginx"]);
        assert!(!app.ai_configured());
        app.cfg.llm.api_key = "sk-test-123".into();
        assert!(app.ai_configured());

        for ch in "покажи файлы".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        assert_eq!(app.on_key(KeyInput::Enter), UiIntent::OpenCandidates);

        app.line_buf = "systemctl restart incy".into();
        app.on_key(KeyInput::Ctrl('w'));
        assert_eq!(app.line_buf, "systemctl restart ");
        app.on_key(KeyInput::ArrowUp);
        assert!(app.line_buf.is_empty());
    }

    #[test]
    fn parses_osc9_and_curl_wget_progress_and_osc7_cwd() {
        assert_eq!(
            parse_pty_progress(b"\x1b]9;4;1;64\x07"),
            Some(ProgressEvent::Update {
                percent: Some(64),
                detail: None
            })
        );
        assert_eq!(
            parse_pty_progress(b"\x1b]9;4;0\x07"),
            Some(ProgressEvent::Clear)
        );
        assert_eq!(
            parse_pty_progress(b"\rFetching package [======>   ]  42% 3.4MB/s ETA 00:05"),
            Some(ProgressEvent::Update {
                percent: Some(42),
                detail: Some("3.4MB/s".into()),
            })
        );
        assert_eq!(
            parse_pty_progress(b"Downloading release tarball 1.8 MiB/s"),
            Some(ProgressEvent::Update {
                percent: None,
                detail: Some("1.8MiB/s".into()),
            })
        );
        assert_eq!(
            parse_pty_progress(b"git diff shows 50% unchanged lines"),
            None
        );
        // Build channel: cargo stage lines drive the island bar without a percent.
        assert_eq!(
            parse_pty_progress(b"   Compiling serde v1.0.219\n"),
            Some(ProgressEvent::Update {
                percent: None,
                detail: Some("Compiling serde".into()),
            })
        );
        assert_eq!(
            parse_pty_progress(b"    Finished `dev` profile [unoptimized] target(s) in 1.2s\n"),
            Some(ProgressEvent::Clear)
        );
        // Kitty Graphics APC chunks must be invisible to the progress parser.
        assert_eq!(
            parse_pty_progress(b"\x1b_Gm=1;eJzL21zLzM0TUIvcw==\x1b\\"),
            None
        );
        assert_eq!(
            parse_pty_progress(b"\x1b_Ga=T,f=100,m=1;eJzL21zLzM0TUIvcw==\x1b\\"),
            None
        );
        // A chunk split between PTY reads has no ESC prefix left; the encoded
        // blob heuristic still rejects it.
        assert_eq!(parse_pty_progress(b"Gm=1;eJzL21zLzM0TUIvcw=="), None);
        // Real progress after an APC sequence is still detected.
        assert_eq!(
            parse_pty_progress(b"\x1b_Gm=1;AAAA\x1b\\\rDownloading 42% 3.4MB/s"),
            Some(ProgressEvent::Update {
                percent: Some(42),
                detail: Some("3.4MB/s".into()),
            })
        );
        assert_eq!(
            parse_osc7_cwd(b"\x1b]7;file://localhost/tmp\x07"),
            Some(PathBuf::from("/tmp"))
        );
    }

    #[test]
    fn command_timer_and_outcome_badge_lifecycle() {
        let mut app = test_app_with_history(&[]);
        assert!(app.command_badge().is_none());

        // Submit command
        let intent = app.submit_command("sleep 3", vec![b'\r']);
        assert!(matches!(intent, UiIntent::PassToPty(_)));
        assert_eq!(app.last_command.as_deref(), Some("sleep 3"));

        // Finish command with success
        app.finish_command(crate::telemetry::CommandCompletion {
            command: "sleep 3".into(),
            started_at: Instant::now() - Duration::from_secs(3),
            anchor: crate::pty::OutputAnchor { abs_row: 1 },
            exit_code: Some(0),
            interrupted: false,
        });

        let badge = app.command_badge();
        assert!(matches!(
            badge,
            Some(crate::contracts::CommandBadge::Succeeded { .. })
        ));

        // Finish command with failure
        app.finish_command(crate::telemetry::CommandCompletion {
            command: "cargo build".into(),
            started_at: Instant::now() - Duration::from_secs(4),
            anchor: crate::pty::OutputAnchor { abs_row: 2 },
            exit_code: Some(101),
            interrupted: false,
        });

        let badge = app.command_badge();
        assert!(matches!(
            badge,
            Some(crate::contracts::CommandBadge::Failed {
                exit_code: Some(101),
                fix_available: true,
                ..
            })
        ));
    }

    #[test]
    fn mini_launcher_open_filter_fill_and_run() {
        let mut app = test_app_with_history(&["cargo check", "npm test"]);
        assert!(!app.launcher.open);

        // Open via Ctrl+P at empty prompt
        assert_eq!(app.on_key(KeyInput::Ctrl('p')), UiIntent::Noop);
        assert!(app.launcher.open);
        assert!(!app.launcher.items.is_empty());

        // Type query 'cargo'
        for ch in "cargo".chars() {
            app.on_key(KeyInput::Char(ch));
        }
        assert_eq!(app.launcher.query, "cargo");
        assert!(app
            .launcher
            .items
            .iter()
            .any(|i| i.label.contains("cargo check")));

        // Enter fills command into line buffer
        let fill_intent = app.on_launcher_key(KeyInput::Enter);
        assert!(!app.launcher.open);
        assert_eq!(app.line_buf, "cargo check");
        assert!(matches!(fill_intent, UiIntent::PassToPty(_)));

        // Reopen and Ctrl+Enter runs safe command
        app.line_buf.clear();
        app.open_launcher();
        assert!(app.launcher.open);
        let run_intent = app.on_launcher_key(KeyInput::CtrlEnter);
        assert!(!app.launcher.open);
        assert!(matches!(run_intent, UiIntent::PassToPty(_)));
    }

    #[test]
    fn system_fallback_fonts_parse_and_cover_nerd_glyphs() {
        let loaded = load_system_fallback_fonts();
        assert!(loaded.len() <= 16);
        let ctx = egui::Context::default();
        LexicueApp::install_fonts(&ctx);
        // Force the font atlas to parse every loaded face and lay out a PUA glyph.
        let _ = ctx.run(Default::default(), |_| {});
        let galley = ctx.fonts(|f| {
            f.layout_no_wrap(
                "󰄾 → █".to_owned(),
                egui::FontId::monospace(14.0),
                egui::Color32::WHITE,
            )
        });
        assert!(galley.size().x > 0.0);
    }

    #[test]
    fn ports_detection_and_island_action_queue() {
        let mut app = test_app_with_history(&[]);
        assert!(app.active_ports().is_empty());

        let now = Instant::now();
        app.ports
            .observe(b"Server listening on http://localhost:8080\n", now);
        let ports = app.active_ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 8080);
    }

    #[test]
    fn env_context_and_chips_reflection() {
        let mut app = test_app_with_history(&[]);
        app.env.root = true;
        assert_eq!(app.island_tint(), crate::contracts::RiskLevel::Dangerous);
        let chips = app.env_chips();
        assert!(chips.iter().any(|c| c.label == "root"));
    }

    #[test]
    fn snippets_and_island_fill_and_run() {
        let mut app = test_app_with_history(&[]);
        app.cached_markers = Some((
            app.cwd.clone(),
            vec![crate::contracts::ProjectMarker::Cargo],
            Instant::now(),
        ));
        let snips = app.snippets();
        assert!(!snips.is_empty());
        assert_eq!(snips[0].command, "cargo test");

        // Action run check
        app.apply_island_action(
            crate::contracts::IslandAction::FillSnippet("cargo check".into()),
            &egui::Context::default(),
        );
        assert_eq!(app.line_buf, "cargo check");
    }
}
