//! Stable cross-module contracts for termby.
//!
//! Boundary rule: third-party types (`portable-pty`, `vt100`, `eframe`, `ureq`)
//! must not appear outside their adapter modules. Everything below is owned by
//! termby and may be relied on by every module and by v2 extensions.
//!
//! v2 seam: new hint sources implement [`CommandResolver`]; theming only reads
//! [`crate::theme::ThemePalette`]. No other extension points are planned.

// Shortcut: контракты держим целиком как стабильный seam для v2; часть
// действий островка/бейджей ещё не разведена по UI — не удаляем до релиза v2.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const COMMAND_BADGE_THRESHOLD: Duration = Duration::from_secs(2);
pub const COMMAND_BADGE_TTL: Duration = Duration::from_secs(4);
pub const COMMAND_OUTPUT_MAX_BYTES: usize = 64 * 1024;
pub const PORT_SCAN_MAX_BYTES: usize = 8 * 1024;
pub const PORT_TTL: Duration = Duration::from_secs(45);
pub const PORT_REVALIDATE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandBadge {
    Running {
        elapsed: Duration,
    },
    Succeeded {
        duration: Duration,
    },
    Failed {
        duration: Duration,
        exit_code: Option<i32>,
        fix_available: bool,
    },
}

#[derive(Clone, Debug)]
pub struct CommandOutcome {
    pub command: String,
    pub exit_code: Option<i32>,
    pub duration: Duration,
    pub output: String, // <= COMMAND_OUTPUT_MAX_BYTES, tail-biased
    pub completed_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnvBadge {
    Ssh,
    Root,
    Sudo,
    Container,
    Venv,
    Password,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvChip {
    pub kind: EnvBadge,
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct EnvContext {
    pub ssh_host: Option<String>,
    pub container: Option<String>, // docker | podman | lxc | k8s
    pub venv: Option<String>,      // basename only, never full path
    pub root: bool,
    pub sudo: bool,
    pub password_prompt: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortHint {
    pub port: u16,
    pub listening: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherAction {
    FillCommand(String),
    RunCommand(String),
    ToggleTheme,
    AskAi,
    ReloadConfig,
    OpenSettings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherItem {
    pub label: String,
    pub detail: String,
    pub action: LauncherAction,
}

#[derive(Clone, Debug, Default)]
pub struct LauncherState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub items: Vec<LauncherItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IslandAction {
    OpenPort(u16),
    OpenLink(String),
    CopyLastOutput,
    FixLastError,
    FillSnippet(String),
    RunSnippet(String),
    ReloadConfig,
}

/// Row identity inside the candidate overlay.
pub type CmdId = u64;

/// Hard cap for the extra resolver context (cwd + markers), in bytes.
/// The screen buffer is never part of this budget.
pub const CONTEXT_BUDGET_BYTES: usize = 256;

/// Project marker discovered from the current directory listing only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMarker {
    Cargo,
    Node,
    Git,
    Python,
    Go,
    Make,
    Docker,
    Unknown,
}

/// The complete context a resolver may receive. Never contains screen contents.
#[derive(Clone, Debug, Default)]
pub struct CwdContext {
    pub cwd: PathBuf,
    pub markers: Vec<ProjectMarker>,
    pub shell: String,
    pub os: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetAction {
    pub marker: ProjectMarker,
    pub label: String,
    pub command: String,
}

/// Why resolution was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// User typed the explicit marker (default `??`).
    Explicit,
    /// Layer-1 (exact / alias / history) produced no candidate.
    AfterMiss,
}

/// Candidate provenance, ordered from most to least trusted.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    Exact,
    Alias,
    Builtin,
    History,
    Dictionary,
    /// Local cache hit; equivalent to `$0` cost.
    Cache,
    Llm,
}

impl CandidateSource {
    /// Only exact layer-1 hits may ever run without an overlay confirm.
    pub fn may_execute_directly(self) -> bool {
        matches!(
            self,
            CandidateSource::Exact | CandidateSource::Builtin | CandidateSource::Alias
        )
    }
}

/// Risk classification produced by [`crate::input::safety`].
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    #[default]
    Safe,
    Warn,
    Dangerous,
}

/// Machine-readable reason attached to a [`SafetyReport`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskReason {
    DestructiveVerb { verb: String, target: String },
    PrivilegeEscalation,
    SystemPathWrite { path: String },
    RawDeviceWrite { path: String },
    RecursiveDelete,
    UnbalancedQuotes,
    EmptyPlaceholder,
}

/// Deterministic safety verdict for a rendered command line.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SafetyReport {
    pub level: RiskLevel,
    pub reasons: Vec<RiskReason>,
}

/// One row of the candidate overlay. The overlay always shows `rendered`.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: CmdId,
    /// May contain `{{arg}}` placeholders.
    pub template: String,
    /// Template with current placeholder values substituted; what Enter executes.
    pub rendered: String,
    pub source: CandidateSource,
    pub score: f32,
    pub safety: SafetyReport,
    /// One short line shown under the command (never free-form model chatter).
    pub note: Option<String>,
}

/// Input to the resolver chain. `raw_input` has already been trimmed.
#[derive(Clone, Debug)]
pub struct ResolveRequest {
    pub raw_input: String,
    pub context: CwdContext,
    pub history_tail: Vec<String>,
    pub trigger: Trigger,
    pub limit: usize,
}

/// A resolver stage. Stages are ordered by [`CommandResolver::priority`] (lower first).
/// `&mut self` allows stages to keep indexes and caches without interior mutability.
pub trait CommandResolver: Send + Sync {
    /// Stable identifier used in logs and cache provenance (`"exact"`, `"llm"`, ...).
    fn id(&self) -> &'static str;
    /// Lower runs earlier. L1 = 10, local search = 20, LLM = 90.
    fn priority(&self) -> u8;
    /// Return zero or more candidates. `Err` must never abort the whole chain.
    fn resolve(&mut self, req: &ResolveRequest) -> Result<Vec<Candidate>, ResolveError>;
    /// Optional `$0` live-typing completions (never performs network calls).
    fn resolve_live(&mut self, _req: &ResolveRequest) -> Vec<Candidate> {
        Vec::new()
    }
    /// Clear any cached entries held by this stage; returns count cleared.
    fn clear_cache(&mut self) -> usize {
        0
    }
}

/// Optional observer notified after a candidate is confirmed by the user.
pub trait ResolutionSink: Send + Sync {
    fn record_accepted(&mut self, candidate: &Candidate, context: &CwdContext);
}

/// Recoverable resolver failure. The chain degrades to the next stage on `Err`.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("provider not configured")]
    NotConfigured,
    #[error("request timed out")]
    Timeout,
    #[error("network error: {0}")]
    Network(String),
    #[error("cannot parse provider reply: {0}")]
    Parse(String),
    #[error("resolver unavailable: {0}")]
    Unavailable(String),
}

/// Terminal state that gates smart interception.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalMode {
    /// TUI apps (`vim`, `htop`, `less`) switch to the alternate screen.
    pub alternate_screen: bool,
    /// ICANON/ECHO disabled by the foreground program.
    pub raw_mode: bool,
    /// `ECHO` cleared; treats the next line as sensitive (password prompts).
    pub echo_off: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
}

/// Guard verdict for the next input byte(s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterceptDecision {
    /// Safe to intercept: overlay, `??` trigger, alias expansion.
    Intercept,
    /// Bypass completely: bytes go straight to the PTY, overlay is force-closed.
    Passthrough,
}

/// Normalized keyboard/paste input produced by the UI layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyInput {
    Char(char),
    Paste(String),
    Enter,
    Tab,
    Backslash,
    Escape,
    Backspace,
    ArrowUp,
    ArrowDown,
    AltArrowUp,
    AltArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    ShiftPageUp,
    ShiftPageDown,
    Ctrl(char),
    CtrlShift(char),
    AltChar(char),
    CtrlEnter,
}

/// What the app must do after handling one input event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiIntent {
    /// Write these bytes to the PTY verbatim.
    PassToPty(Vec<u8>),
    /// Start a resolver run for the current input line.
    OpenCandidates,
    /// Close the overlay and discard staged state.
    CloseCandidates,
    /// Advance to the next `{{arg}}` slot.
    FocusNextSlot,
    /// Stage/execute the selected candidate through the confirm gate.
    ConfirmCandidate,
    /// Copy active terminal selection to clipboard.
    CopySelection,
    /// Ask AI to fix the last failed command using visible terminal output.
    FixLastError,
    /// Adjust scrollback offset (positive = scroll up into history, negative = down).
    ScrollbackDelta(isize),
    OpenLauncher,
    CopyLastOutput,
    /// Consumed, nothing to do.
    Noop,
}
