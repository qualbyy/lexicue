//! PTY + screen adapters. This is the version firewall for `portable-pty`/`vt100`:
//! the rest of termby only sees the traits and plain types defined here.

use crate::contracts::TerminalMode;
use std::path::PathBuf;

pub mod guard;
pub mod kitty;
pub mod markers;
pub mod portable;
pub mod vt;

pub use guard::PassthroughGuard;
pub use markers::PromptMarker;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OutputAnchor {
    pub abs_row: u64,
}

/// Everything needed to start the login shell inside the PTY.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// Terminal cell color in termby terms, not in vt100 terms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

/// Terminal mouse reporting mode requested by the running application.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// Wire encoding for mouse reporting events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

/// Encode a mouse press/release/motion/wheel event for the PTY.
/// `col` and `row` are 0-based grid coordinates.
pub fn encode_mouse_event(
    encoding: MouseEncoding,
    button: u8,
    col: u16,
    row: u16,
    pressed: bool,
) -> Vec<u8> {
    let x = col.saturating_add(1);
    let y = row.saturating_add(1);
    match encoding {
        MouseEncoding::Sgr => {
            let suffix = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{button};{x};{y}{suffix}").into_bytes()
        }
        MouseEncoding::Default => {
            if x > 223 || y > 223 {
                return Vec::new();
            }
            let cb = if pressed { button } else { 3 };
            vec![0x1b, b'[', b'M', 32 + cb, 32 + x as u8, 32 + y as u8]
        }
        MouseEncoding::Utf8 => {
            let cb = if pressed { button } else { 3 };
            let mut out = vec![0x1b, b'[', b'M', 32 + cb];
            let mut buf = [0u8; 4];
            if let Some(cx) = char::from_u32(32 + u32::from(x)) {
                out.extend_from_slice(cx.encode_utf8(&mut buf).as_bytes());
            }
            if let Some(cy) = char::from_u32(32 + u32::from(y)) {
                out.extend_from_slice(cy.encode_utf8(&mut buf).as_bytes());
            }
            out
        }
    }
}

/// Decoded inline image placed on the terminal grid via Kitty Graphics Protocol.
#[derive(Clone, Debug)]
pub struct KittyImage {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub col: u16,
    pub dist_from_bottom: usize,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

/// Immutable frame handed to the renderer. Grid is row-major, `cols * rows` cells.
#[derive(Clone, Debug, Default)]
pub struct GridSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    /// `(col, row)`, 0-based.
    pub cursor: (u16, u16),
    pub title: Option<String>,
    pub alternate_screen: bool,
    pub scrollback_offset: usize,
    pub scrollback_total: usize,
    pub mouse_reporting: bool,
    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
    pub bell_count: usize,
    pub images: Vec<KittyImage>,
}

impl GridSnapshot {
    /// `None` when `(col, row)` is outside the grid.
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let idx = (row as usize) * (self.cols as usize) + (col as usize);
        self.cells.get(idx)
    }
}

/// Byte source/sink for the child process.
pub trait PtyBackend: Send {
    fn spawn(&mut self, spec: SpawnSpec) -> Result<(), PtyError>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError>;
    fn resize(&mut self, cols: u16, rows: u16) -> Result<(), PtyError>;
    /// Report the real rendered cell size so child-visible pixel metrics
    /// (`winsize` pixels, `CSI 14t`/`16t` replies) match what egui paints.
    fn set_cell_pixels(&mut self, _width: u32, _height: u32) {}
    /// Non-blocking; returns whatever bytes are ready, empty when idle.
    fn drain(&mut self) -> Result<Vec<u8>, PtyError>;
    fn is_alive(&mut self) -> bool;
    fn kill(&mut self) -> Result<(), PtyError>;
    /// Non-blocking termios probe; returns default when unavailable.
    fn shell_signals(&self) -> guard::ShellSignals {
        guard::ShellSignals::default()
    }
}

/// Terminal emulator state. Feed bytes, ask for a snapshot, never query vt100 directly.
pub trait ScreenModel: Send {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn snapshot(&self) -> GridSnapshot;
    fn title(&self) -> Option<&str>;
    /// What the emulator currently believes about alternate screen / raw-ish modes.
    fn terminal_mode(&self) -> TerminalMode;
    fn set_mode_hints(&mut self, _raw_mode: bool, _echo_off: bool) {}
    /// Real rendered cell pixel size; drives `CSI 14t`/`16t` replies so image
    /// placement math (fastfetch/kitty) matches the drawn grid.
    fn set_cell_pixels(&mut self, _width: u32, _height: u32) {}
    fn set_scrollback(&mut self, _rows: usize) {}
    fn scrollback(&self) -> usize {
        0
    }
    /// Plain trimmed text of a row indexed by `dist_from_bottom` (`0` = bottom live row).
    fn row_text(&self, _dist_from_bottom: usize) -> Option<String> {
        None
    }
    fn mark_output(&self) -> OutputAnchor {
        OutputAnchor::default()
    }
    fn capture_since(&self, _anchor: OutputAnchor, _max_bytes: usize) -> String {
        String::new()
    }
    fn take_replies(&mut self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("pty io error: {0}")]
    Io(String),
    #[error("pty is closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_sgr_and_x10_mouse_events() {
        assert_eq!(
            encode_mouse_event(MouseEncoding::Sgr, 0, 9, 4, true),
            b"\x1b[<0;10;5M"
        );
        assert_eq!(
            encode_mouse_event(MouseEncoding::Sgr, 0, 9, 4, false),
            b"\x1b[<0;10;5m"
        );
        assert_eq!(
            encode_mouse_event(MouseEncoding::Sgr, 64, 0, 0, true),
            b"\x1b[<64;1;1M"
        );
        assert_eq!(
            encode_mouse_event(MouseEncoding::Default, 0, 0, 0, true),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            encode_mouse_event(MouseEncoding::Default, 0, 0, 0, false),
            vec![0x1b, b'[', b'M', 35, 33, 33]
        );
    }
}
