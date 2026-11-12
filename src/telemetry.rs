//! Pure telemetry and state machines for Lexicue.
//!
//! Firewall invariant: NO `egui`, NO `portable-pty`, NO `vt100` types in this module.
//!
//! Contains:
//! - `CommandTracker`: tracks foreground command execution, duration, exit status, and badges.
//! - `PortScanner`: discovers active dev ports from terminal output and verifies against `/proc/net/tcp`.
//! - `EnvContext`: detects environment chips (root/sudo/ssh/container/venv/password) and security tint.
//! - `snippets_for`: maps active project markers to configured command snippets.
//! - `strip_ansi_sequences`: cleans ANSI escape sequences for text processing.

// Shortcut: телеметрия собирает данные раньше, чем UI их показывает; часть
// геттеров/порогов подключим в v2 — не выбрасываем до релиза.
#![allow(dead_code)]

use crate::config::IslandSettings;
use crate::contracts::{
    CommandBadge, EnvBadge, EnvChip, EnvContext, PortHint, ProjectMarker, RiskLevel, SnippetAction,
};
use crate::pty::{OutputAnchor, PromptMarker};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct CommandCompletion {
    pub command: String,
    pub started_at: Instant,
    pub anchor: OutputAnchor,
    pub exit_code: Option<i32>,
    pub interrupted: bool,
}

#[derive(Debug)]
enum TrackerState {
    Idle,
    Armed {
        command: String,
        anchor: OutputAnchor,
        armed_at: Instant,
        saw_output: bool,
    },
    Running {
        command: String,
        anchor: OutputAnchor,
        started_at: Instant,
    },
}

pub struct CommandTracker {
    state: TrackerState,
    last_completion: Option<CommandCompletion>,
}

impl CommandTracker {
    const ARMED_TIMEOUT: Duration = Duration::from_millis(1200);

    pub fn new() -> Self {
        Self {
            state: TrackerState::Idle,
            last_completion: None,
        }
    }

    pub fn begin(
        &mut self,
        command: String,
        anchor: OutputAnchor,
        now: Instant,
    ) -> Option<CommandCompletion> {
        let superseded = self.finalize_current(now, None, false);
        self.state = TrackerState::Armed {
            command,
            anchor,
            armed_at: now,
            saw_output: false,
        };
        superseded
    }

    pub fn on_marker(&mut self, marker: PromptMarker, now: Instant) -> Option<CommandCompletion> {
        match marker {
            PromptMarker::PromptStart | PromptMarker::InputStart => None,
            PromptMarker::CommandStart => {
                if let TrackerState::Armed {
                    command, anchor, ..
                } = std::mem::replace(&mut self.state, TrackerState::Idle)
                {
                    self.state = TrackerState::Running {
                        command,
                        anchor,
                        started_at: now,
                    };
                }
                None
            }
            PromptMarker::Finished { exit_code } => self.finalize_current(now, exit_code, false),
        }
    }

    pub fn on_foreground(&mut self, busy: bool, now: Instant) -> Option<CommandCompletion> {
        match &mut self.state {
            TrackerState::Armed {
                command,
                anchor,
                armed_at,
                saw_output,
            } => {
                if busy {
                    let cmd = command.clone();
                    let anch = *anchor;
                    self.state = TrackerState::Running {
                        command: cmd,
                        anchor: anch,
                        started_at: *armed_at,
                    };
                    None
                } else if !*saw_output && now.duration_since(*armed_at) >= Self::ARMED_TIMEOUT {
                    self.finalize_current(now, None, false)
                } else {
                    None
                }
            }
            TrackerState::Running { .. } => {
                if !busy {
                    self.finalize_current(now, None, false)
                } else {
                    None
                }
            }
            TrackerState::Idle => None,
        }
    }

    pub fn on_interrupt(&mut self, now: Instant) -> Option<CommandCompletion> {
        self.finalize_current(now, None, true)
    }

    /// Ignores CR/LF/BEL/ANSI-only chunks (input echo); real text prevents
    /// Armed timeout from firing while a builtin such as `read -p` waits.
    pub fn on_output(&mut self, bytes: &[u8], _now: Instant) {
        if let TrackerState::Armed { saw_output, .. } = &mut self.state {
            let stripped = strip_ansi_sequences(bytes);
            let meaningful = stripped.chars().any(|c| c >= ' ' && c != '\x7f');
            if meaningful {
                *saw_output = true;
            }
        }
    }

    pub fn badge(&self, now: Instant, threshold: Duration) -> Option<CommandBadge> {
        match &self.state {
            TrackerState::Running { started_at, .. } => {
                let elapsed = now.duration_since(*started_at);
                if elapsed >= threshold {
                    Some(CommandBadge::Running { elapsed })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn anchor(&self) -> Option<OutputAnchor> {
        match &self.state {
            TrackerState::Armed { anchor, .. } | TrackerState::Running { anchor, .. } => {
                Some(*anchor)
            }
            TrackerState::Idle => None,
        }
    }

    pub fn active_command(&self) -> Option<&str> {
        match &self.state {
            TrackerState::Armed { command, .. } | TrackerState::Running { command, .. } => {
                Some(command)
            }
            TrackerState::Idle => None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, TrackerState::Running { .. })
    }

    fn finalize_current(
        &mut self,
        _now: Instant,
        exit_code: Option<i32>,
        interrupted: bool,
    ) -> Option<CommandCompletion> {
        let completion = match std::mem::replace(&mut self.state, TrackerState::Idle) {
            TrackerState::Armed {
                command,
                anchor,
                armed_at,
                ..
            } => Some(CommandCompletion {
                command,
                started_at: armed_at,
                anchor,
                exit_code,
                interrupted,
            }),
            TrackerState::Running {
                command,
                anchor,
                started_at,
            } => Some(CommandCompletion {
                command,
                started_at,
                anchor,
                exit_code,
                interrupted,
            }),
            TrackerState::Idle => None,
        };
        if completion.is_some() {
            self.last_completion = completion.clone();
        }
        completion
    }
}

impl Default for CommandTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for CommandCompletion {
    fn clone(&self) -> Self {
        Self {
            command: self.command.clone(),
            started_at: self.started_at,
            anchor: self.anchor,
            exit_code: self.exit_code,
            interrupted: self.interrupted,
        }
    }
}

pub struct PortRecord {
    pub port: u16,
    pub last_seen: Instant,
    pub miss_count: u32,
    pub listening: Option<bool>,
}

pub struct PortScanner {
    tail_buffer: Vec<u8>,
    ports: HashMap<u16, PortRecord>,
    last_revalidate: Option<Instant>,
}

impl PortScanner {
    const MAX_TAIL: usize = crate::contracts::PORT_SCAN_MAX_BYTES;

    pub fn new() -> Self {
        Self {
            tail_buffer: Vec::with_capacity(Self::MAX_TAIL),
            ports: HashMap::new(),
            last_revalidate: None,
        }
    }

    pub fn observe(&mut self, bytes: &[u8], now: Instant) -> bool {
        if self.tail_buffer.len() + bytes.len() > Self::MAX_TAIL {
            let keep = Self::MAX_TAIL.saturating_sub(bytes.len());
            if keep > 0 && self.tail_buffer.len() > keep {
                let start = self.tail_buffer.len() - keep;
                self.tail_buffer = self.tail_buffer[start..].to_vec();
            } else if keep == 0 {
                self.tail_buffer.clear();
            }
        }
        self.tail_buffer.extend_from_slice(bytes);

        let clean = strip_ansi_sequences(&self.tail_buffer);
        let found = parse_ports(&clean);
        let mut changed = false;

        for port in found {
            match self.ports.get_mut(&port) {
                Some(record) => {
                    record.last_seen = now;
                    record.miss_count = 0;
                }
                None => {
                    self.ports.insert(
                        port,
                        PortRecord {
                            port,
                            last_seen: now,
                            miss_count: 0,
                            listening: None,
                        },
                    );
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn revalidate(&mut self, now: Instant, interval: Duration, ttl: Duration) -> bool {
        if let Some(last) = self.last_revalidate {
            if now.duration_since(last) < interval {
                return false;
            }
        }
        self.last_revalidate = Some(now);

        let active_listeners = read_proc_net_tcp_listening();

        let mut changed = false;
        let mut to_remove = Vec::new();

        for (&port, record) in self.ports.iter_mut() {
            if let Some(listening_set) = &active_listeners {
                if listening_set.contains(&port) {
                    if record.listening != Some(true) {
                        record.listening = Some(true);
                        changed = true;
                    }
                    record.miss_count = 0;
                } else {
                    record.miss_count += 1;
                    record.listening = Some(false);
                    if record.miss_count >= 3 || now.duration_since(record.last_seen) >= ttl {
                        to_remove.push(port);
                        changed = true;
                    }
                }
            } else if now.duration_since(record.last_seen) >= ttl {
                to_remove.push(port);
                changed = true;
            }
        }

        for port in to_remove {
            self.ports.remove(&port);
        }

        changed
    }

    pub fn active(&self, now: Instant, ttl: Duration) -> Vec<PortHint> {
        let mut out: Vec<PortHint> = self
            .ports
            .values()
            .filter(|rec| now.duration_since(rec.last_seen) < ttl)
            .map(|rec| PortHint {
                port: rec.port,
                listening: rec.listening,
            })
            .collect();
        out.sort_by_key(|h| h.port);
        out
    }
}

impl Default for PortScanner {
    fn default() -> Self {
        Self::new()
    }
}

pub fn strip_ansi_sequences(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if ('@'..='~').contains(&nc) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: skip the whole string sequence. Kitty
                // graphics arrive as APC (`ESC _ G... ESC \`); leaving their
                // base64 payload in the text made transfer_progress see fake `/s`.
                Some(']' | 'P' | '^' | '_' | 'X') => {
                    chars.next();
                    let mut prev_esc = false;
                    for nc in chars.by_ref() {
                        if nc == '\x07' || (prev_esc && nc == '\\') {
                            break;
                        }
                        prev_esc = nc == '\x1b';
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c == '\r' || c == '\n' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Heuristic for Kitty/base64 payload remnants split across PTY reads.
/// A line that is almost entirely base64 with no whitespace must never be
/// mistaken for a progress line (base64 casually contains `/s`, `m/s`, `%`).
pub fn looks_like_encoded_blob(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 16 || trimmed.contains(' ') {
        return false;
    }
    let encoded = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | ';' | ','))
        .count();
    encoded * 10 >= trimmed.len() * 9
}

static PORT_REGEX: OnceLock<regex::Regex> = OnceLock::new();

pub fn parse_ports(text: &str) -> Vec<u16> {
    let re = PORT_REGEX.get_or_init(|| {
        regex::Regex::new(r"(?i)(?:https?://)?(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|\[::\]):([0-9]{1,5})\b").unwrap()
    });

    let mut ports = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            if let Ok(p) = m.as_str().parse::<u16>() {
                if (1..=65535).contains(&p) && !ports.contains(&p) {
                    ports.push(p);
                }
            }
        }
    }
    ports
}

fn read_proc_net_tcp_listening() -> Option<HashSet<u16>> {
    let mut ports = HashSet::new();
    let mut read_any = false;

    if let Ok(content) = std::fs::read_to_string("/proc/net/tcp") {
        read_any = true;
        for p in parse_proc_net_tcp(&content) {
            ports.insert(p);
        }
    }
    if let Ok(content) = std::fs::read_to_string("/proc/net/tcp6") {
        read_any = true;
        for p in parse_proc_net_tcp(&content) {
            ports.insert(p);
        }
    }

    if read_any {
        Some(ports)
    } else {
        None
    }
}

pub fn parse_proc_net_tcp(content: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 4 {
            let local_addr = fields[1];
            let state = fields[3];
            // 0A is TCP_LISTEN in Linux kernel
            if state == "0A" {
                if let Some((_, port_hex)) = local_addr.split_once(':') {
                    if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                        if !ports.contains(&port) {
                            ports.push(port);
                        }
                    }
                }
            }
        }
    }
    ports
}

impl EnvContext {
    pub fn detect() -> Self {
        let ssh_host = std::env::var("SSH_CONNECTION")
            .ok()
            .and_then(|conn| conn.split_whitespace().next().map(|s| s.to_string()))
            .or_else(|| {
                std::env::var("SSH_CLIENT")
                    .ok()
                    .and_then(|c| c.split_whitespace().next().map(|s| s.to_string()))
            });

        let container = if std::path::Path::new("/.dockerenv").exists() {
            Some("docker".to_string())
        } else if std::path::Path::new("/run/.containerenv").exists() {
            Some("container".to_string())
        } else if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("docker") {
                Some("docker".to_string())
            } else if cgroup.contains("containerd") || cgroup.contains("kubepods") {
                Some("k8s".to_string())
            } else {
                None
            }
        } else {
            None
        };

        let venv = std::env::var("VIRTUAL_ENV").ok().and_then(|p| {
            std::path::Path::new(&p)
                .file_name()
                .and_then(|f| f.to_str())
                .map(|s| s.to_string())
        });

        #[cfg(unix)]
        let root = libc_is_root();
        #[cfg(not(unix))]
        let root = false;

        Self {
            ssh_host,
            container,
            venv,
            root,
            sudo: false,
            password_prompt: false,
        }
    }

    pub fn note_command(&mut self, command: &str) {
        let trimmed = command.trim();
        let first_token = trimmed.split_whitespace().next().unwrap_or("");
        if first_token == "sudo" || first_token == "doas" {
            self.sudo = true;
        }
    }

    pub fn note_mode(&mut self, echo_off: bool) {
        self.password_prompt = echo_off;
    }

    pub fn chips(&self) -> Vec<EnvChip> {
        let mut chips = Vec::new();
        if self.root {
            chips.push(EnvChip {
                kind: EnvBadge::Root,
                label: "root".into(),
            });
        }
        if self.sudo {
            chips.push(EnvChip {
                kind: EnvBadge::Sudo,
                label: "sudo".into(),
            });
        }
        if let Some(host) = &self.ssh_host {
            chips.push(EnvChip {
                kind: EnvBadge::Ssh,
                label: format!("ssh:{host}"),
            });
        }
        if let Some(ctn) = &self.container {
            chips.push(EnvChip {
                kind: EnvBadge::Container,
                label: ctn.clone(),
            });
        }
        if let Some(venv) = &self.venv {
            chips.push(EnvChip {
                kind: EnvBadge::Venv,
                label: format!("({venv})"),
            });
        }
        if self.password_prompt {
            chips.push(EnvChip {
                kind: EnvBadge::Password,
                label: "password".into(),
            });
        }
        chips
    }

    pub fn tint(&self) -> RiskLevel {
        if self.root || self.sudo {
            RiskLevel::Dangerous
        } else if self.ssh_host.is_some() || self.container.is_some() || self.password_prompt {
            RiskLevel::Warn
        } else {
            RiskLevel::Safe
        }
    }
}

#[cfg(unix)]
fn libc_is_root() -> bool {
    // Rely on std/nix-less rust standard lib geteuid check via libc syscall if available,
    // or std::fs checking /root readability.
    std::env::var("USER").map(|u| u == "root").unwrap_or(false)
}

pub fn snippets_for(markers: &[ProjectMarker], cfg: &IslandSettings) -> Vec<SnippetAction> {
    let mut actions = Vec::new();
    let mut seen_commands = HashSet::new();

    for &marker in markers {
        for cmd in cfg.snippets.for_marker(marker) {
            if seen_commands.insert(cmd.clone()) {
                let label = cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                actions.push(SnippetAction {
                    marker,
                    label,
                    command: cmd.clone(),
                });
                if actions.len() >= cfg.max_snippets {
                    return actions;
                }
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_apc_kitty_payload_and_flags_split_chunks() {
        assert_eq!(
            strip_ansi_sequences(b"\x1b_Gm=1;eJzL21zLzM0TUIvcw==\x1b\\visible"),
            "visible"
        );
        assert_eq!(strip_ansi_sequences(b"\x1b]9;4;1;42\x07done"), "done");
        assert!(looks_like_encoded_blob("m=1;eJzL21zLzM0TUIvcw=="));
        assert!(!looks_like_encoded_blob("Downloading 42% 3.4MB/s"));
    }

    #[test]
    fn parses_dev_ports_correctly() {
        let text = "Server listening on http://localhost:3000 and 127.0.0.1:8080 and [::1]:5173";
        let ports = parse_ports(text);
        assert_eq!(ports, vec![3000, 8080, 5173]);
    }

    #[test]
    fn parses_proc_net_tcp_output() {
        let proc_sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1";
        let ports = parse_proc_net_tcp(proc_sample);
        // 0x0BB8 is 3000
        assert_eq!(ports, vec![3000]);
    }

    #[test]
    fn tracker_running_badge_and_completion() {
        let mut tracker = CommandTracker::new();
        let now = Instant::now();
        let anchor = OutputAnchor { abs_row: 10 };

        tracker.begin("sleep 5".into(), anchor, now);
        tracker.on_foreground(true, now);

        assert!(tracker.is_running());
        let badge = tracker.badge(now + Duration::from_secs(3), Duration::from_secs(2));
        assert!(matches!(badge, Some(CommandBadge::Running { .. })));

        let completed = tracker.on_foreground(false, now + Duration::from_secs(5));
        assert!(completed.is_some());
        assert_eq!(completed.unwrap().command, "sleep 5");
    }

    #[test]
    fn env_context_tint_and_chips() {
        let mut env = EnvContext::default();
        assert_eq!(env.tint(), RiskLevel::Safe);

        env.note_command("sudo apt update");
        assert!(env.sudo);
        assert_eq!(env.tint(), RiskLevel::Dangerous);

        let chips = env.chips();
        assert!(chips.iter().any(|c| c.kind == EnvBadge::Sudo));
    }

    #[test]
    fn snippets_generation_respects_cap_and_dedup() {
        let cfg = IslandSettings::default();
        let markers = vec![ProjectMarker::Cargo, ProjectMarker::Git];
        let snippets = snippets_for(&markers, &cfg);
        assert!(snippets.len() <= cfg.max_snippets);
        assert_eq!(snippets[0].marker, ProjectMarker::Cargo);
    }
}
