//! PTY Passthrough Guard (critical invariant).
//!
//! Interception (overlay, alias expansion, `??`) is allowed only while the
//! foreground program behaves like a line-oriented shell. Inside TUIs or while a
//! password is being typed every byte must reach the PTY unmodified.
//!
//! Detection sources, strongest first:
//! 1. `termios` on the PTY master (ECHO / ICANON bits) — authoritative when readable;
//! 2. alternate-screen flag from the screen model;
//! 3. window title against `[guard].tui_titles` (`vim`, `htop`, `less`, `ssh`, ...);
//! 4. password-prompt markers on the last visible line (heuristic only);
//! 5. process-name blocklist is intentionally NOT used in v1.
//!
//! Rule: any positive signal yields [`InterceptDecision::Passthrough`]. There is
//! no override key. The overlay is force-closed on transition into passthrough.

use crate::contracts::{InterceptDecision, TerminalMode};
use crate::pty::ScreenModel;

/// Termios facts collected from the PTY fd; `None` when the fd is not inspectable.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellSignals {
    pub echo: Option<bool>,
    pub raw: Option<bool>,
    pub foreground_pgid: Option<i32>,
    pub shell_pgid: Option<i32>,
}

impl ShellSignals {
    /// None when the platform cannot report process groups.
    pub fn foreground_busy(&self) -> Option<bool> {
        Some(self.foreground_pgid? != self.shell_pgid?)
    }
}

#[derive(Clone, Debug)]
pub struct GuardConfig {
    pub tui_titles: Vec<String>,
    pub password_prompt_markers: Vec<String>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            tui_titles: [
                "vim", "nvim", "vi", "htop", "top", "less", "more", "man", "ssh", "tmux", "fzf",
                "lazygit",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            password_prompt_markers: ["password:", "passphrase:", "[sudo]", "Enter passphrase"]
                .into_iter()
                .map(String::from)
                .collect(),
        }
    }
}

pub struct PassthroughGuard {
    mode: TerminalMode,
    config: GuardConfig,
    decision: InterceptDecision,
    /// Latches until the alternate screen is left, to avoid flapping mid-TUI.
    latched: bool,
}

fn last_non_empty_line(screen: &dyn ScreenModel) -> String {
    let snap = screen.snapshot();
    for row in (0..snap.rows).rev() {
        let mut line = String::with_capacity(snap.cols as usize);
        for col in 0..snap.cols {
            let ch = snap.cell(col, row).map(|c| c.ch).unwrap_or(' ');
            line.push(if ch == '\0' { ' ' } else { ch });
        }
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    String::new()
}

impl PassthroughGuard {
    pub fn new(config: GuardConfig) -> Self {
        Self {
            mode: TerminalMode::default(),
            config,
            decision: InterceptDecision::Intercept,
            latched: false,
        }
    }

    /// Recompute the mode + decision. Returns the mode the app must store in the screen.
    pub fn observe(&mut self, screen: &dyn ScreenModel, signals: ShellSignals) -> TerminalMode {
        let base = screen.terminal_mode();
        let last_line = last_non_empty_line(screen);

        let echo_off = match signals.echo {
            Some(echo) => !echo,
            None => {
                base.echo_off
                    || looks_like_password_prompt(&last_line, &self.config.password_prompt_markers)
            }
        };
        let raw_mode = signals.raw.unwrap_or(base.raw_mode);
        let alt_screen = base.alternate_screen;
        if alt_screen {
            self.latched = true;
        }
        let tui_title = screen
            .title()
            .is_some_and(|title| is_tui_title(title, &self.config.tui_titles));

        self.mode = TerminalMode {
            alternate_screen: alt_screen,
            raw_mode,
            echo_off,
            bracketed_paste: base.bracketed_paste,
            mouse_reporting: base.mouse_reporting,
        };

        self.decision = if self.latched || alt_screen || raw_mode || echo_off || tui_title {
            InterceptDecision::Passthrough
        } else {
            InterceptDecision::Intercept
        };

        self.mode
    }

    pub fn decision(&self) -> InterceptDecision {
        self.decision
    }

    pub fn mode(&self) -> TerminalMode {
        self.mode
    }

    /// Clear the latch after the child returns to the primary screen.
    pub fn reset(&mut self) {
        self.latched = false;
        self.mode = TerminalMode::default();
        self.decision = InterceptDecision::Intercept;
    }
}

/// Case-insensitive substring match of the terminal title against the TUI list.
pub fn is_tui_title(title: &str, blocklist: &[String]) -> bool {
    let lower = title.to_lowercase();
    blocklist
        .iter()
        .any(|pat| !pat.is_empty() && lower.contains(&pat.to_lowercase()))
}

/// Last-non-empty-line heuristic used only when termios is unavailable.
pub fn looks_like_password_prompt(last_line: &str, markers: &[String]) -> bool {
    let lower = last_line.to_lowercase();
    markers
        .iter()
        .any(|marker| !marker.is_empty() && lower.contains(&marker.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{Attrs, Cell, Color, GridSnapshot};

    struct MockScreen {
        title: Option<String>,
        mode: TerminalMode,
        line: String,
    }

    impl MockScreen {
        fn new(title: Option<&str>, mode: TerminalMode, line: &str) -> Self {
            Self {
                title: title.map(ToOwned::to_owned),
                mode,
                line: line.to_string(),
            }
        }
    }

    impl ScreenModel for MockScreen {
        fn feed(&mut self, _bytes: &[u8]) {}
        fn resize(&mut self, _cols: u16, _rows: u16) {}
        fn snapshot(&self) -> GridSnapshot {
            let cols = self.line.chars().count().max(1) as u16;
            let mut cells: Vec<Cell> = self
                .line
                .chars()
                .map(|ch| Cell {
                    ch,
                    fg: Color::Default,
                    bg: Color::Default,
                    attrs: Attrs::default(),
                })
                .collect();
            if cells.is_empty() {
                cells.push(Cell::default());
            }
            GridSnapshot {
                cols,
                rows: 1,
                cells,
                cursor: (0, 0),
                title: self.title.clone(),
                alternate_screen: self.mode.alternate_screen,
                scrollback_offset: 0,
                scrollback_total: 0,
                mouse_reporting: false,
                ..Default::default()
            }
        }
        fn title(&self) -> Option<&str> {
            self.title.as_deref()
        }
        fn terminal_mode(&self) -> TerminalMode {
            self.mode
        }
    }

    #[test]
    fn detects_tui_titles_case_insensitively() {
        let cfg = GuardConfig::default();
        assert!(is_tui_title("VIM - Cargo.toml", &cfg.tui_titles));
        assert!(is_tui_title("htop", &cfg.tui_titles));
        assert!(is_tui_title("less README.md", &cfg.tui_titles));
        assert!(is_tui_title("ssh prod-box", &cfg.tui_titles));
        assert!(!is_tui_title("zsh - ~/projects/lexicue", &cfg.tui_titles));
    }

    #[test]
    fn detects_password_prompts_from_last_line() {
        let cfg = GuardConfig::default();
        assert!(looks_like_password_prompt(
            "[sudo] password for qualbyy: ",
            &cfg.password_prompt_markers
        ));
        assert!(looks_like_password_prompt(
            "Enter passphrase for key '/home/u/.ssh/id_ed25519':",
            &cfg.password_prompt_markers
        ));
        assert!(!looks_like_password_prompt(
            "qualbyy@host ~/projects $ ",
            &cfg.password_prompt_markers
        ));
    }

    #[test]
    fn echo_off_forces_passthrough_without_latching() {
        let mut guard = PassthroughGuard::new(GuardConfig::default());
        let prompt_screen = MockScreen::new(
            Some("zsh"),
            TerminalMode::default(),
            "[sudo] password for qualbyy:",
        );
        let mode = guard.observe(
            &prompt_screen,
            ShellSignals {
                echo: Some(false),
                raw: None,
                foreground_pgid: None,
                shell_pgid: None,
            },
        );
        assert!(mode.echo_off);
        assert_eq!(guard.decision(), InterceptDecision::Passthrough);

        let normal_screen = MockScreen::new(Some("zsh"), TerminalMode::default(), "$ ");
        guard.observe(
            &normal_screen,
            ShellSignals {
                echo: Some(true),
                raw: Some(false),
                foreground_pgid: None,
                shell_pgid: None,
            },
        );
        assert_eq!(guard.decision(), InterceptDecision::Intercept);
    }

    #[test]
    fn alternate_screen_latches_until_reset() {
        let mut guard = PassthroughGuard::new(GuardConfig::default());
        let alt_screen = MockScreen::new(
            Some("vim"),
            TerminalMode {
                alternate_screen: true,
                ..Default::default()
            },
            "",
        );
        guard.observe(&alt_screen, ShellSignals::default());
        assert_eq!(guard.decision(), InterceptDecision::Passthrough);

        // Returning to primary screen remains latched until reset() is called.
        let primary = MockScreen::new(Some("zsh"), TerminalMode::default(), "$ ");
        guard.observe(&primary, ShellSignals::default());
        assert_eq!(guard.decision(), InterceptDecision::Passthrough);

        guard.reset();
        assert_eq!(guard.decision(), InterceptDecision::Intercept);
        guard.observe(&primary, ShellSignals::default());
        assert_eq!(guard.decision(), InterceptDecision::Intercept);
    }
}
