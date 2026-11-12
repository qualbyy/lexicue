//! Adapter over `vt100`. Converts the crate's buffer into lexicue's `GridSnapshot`
//! and exposes only the mode flags the guard needs.

use std::collections::VecDeque;

use super::{Attrs, Cell, Color, GridSnapshot, ScreenModel};
use crate::contracts::TerminalMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscState {
    Normal,
    Esc,
    Csi(u16),
    Osc,
    OscEsc,
    Apc,
    ApcEsc,
}

pub struct Vt100Screen {
    parser: vt100::Parser,
    title: String,
    /// Set by the guard when termios reports a non-canonical/raw foreground mode.
    raw_mode: bool,
    /// Set by the guard when ECHO is cleared (password prompt).
    echo_off: bool,
    /// Maintained outside `vt100` because `vt100 0.15.2` subtracts `rows_len - scrollback_offset`
    /// without saturation in `visible_rows()`, panicking whenever scrolling > 1 screenful.
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_max: usize,
    scrollback_offset: usize,
    history_rows: u64,
    esc_state: EscState,
    apc_buf: Vec<u8>,
    kitty: super::kitty::KittyState,
    /// Real rendered cell size, updated from the UI; `9x18` until the first frame.
    cell_w: u32,
    cell_h: u32,
}

impl Vt100Screen {
    /// `scrollback` = lines retained above the viewport.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), 0),
            title: String::new(),
            raw_mode: false,
            echo_off: false,
            scrollback: VecDeque::new(),
            scrollback_max: scrollback,
            scrollback_offset: 0,
            history_rows: 0,
            esc_state: EscState::Normal,
            apc_buf: Vec::new(),
            kitty: super::kitty::KittyState::default(),
            cell_w: 9,
            cell_h: 18,
        }
    }

    /// OSC 0/2 window title updates are forwarded here by the parser callback.
    #[allow(dead_code)] // Shortcut: заголовок окна берётся из cwd/имени профиля.
    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }

    /// The guard owns these two flags; the screen only reports them.
    pub fn set_mode_hints(&mut self, raw_mode: bool, echo_off: bool) {
        self.raw_mode = raw_mode;
        self.echo_off = echo_off;
    }

    fn map_color(color: vt100::Color) -> Color {
        match color {
            vt100::Color::Default => Color::Default,
            vt100::Color::Idx(idx) => Color::Indexed(idx),
            vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }

    fn to_cell(input: &vt100::Cell) -> Cell {
        let ch = input.contents().chars().next().unwrap_or(' ');
        Cell {
            ch,
            fg: Self::map_color(input.fgcolor()),
            bg: Self::map_color(input.bgcolor()),
            attrs: Attrs {
                bold: input.bold(),
                dim: false,
                italic: input.italic(),
                underline: input.underline(),
                inverse: input.inverse(),
            },
        }
    }

    fn capture_row(&self, row: u16) -> Vec<Cell> {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        (0..cols)
            .map(|col| screen.cell(row, col).map(Self::to_cell).unwrap_or_default())
            .collect()
    }

    fn push_scrollback_row(&mut self, row: Vec<Cell>) {
        self.history_rows = self.history_rows.saturating_add(1);
        let max_dist = self.scrollback_max + usize::from(self.parser.screen().size().0);
        self.kitty.on_scroll_up(max_dist);
        if self.scrollback_max == 0 {
            return;
        }
        self.scrollback.push_back(row);
        while self.scrollback.len() > self.scrollback_max {
            self.scrollback.pop_front();
        }
        if self.scrollback_offset > 0 {
            self.scrollback_offset = self.scrollback.len().min(self.scrollback_offset + 1);
        }
    }

    fn capture_top_rows(&mut self, count: u16) {
        if self.scrollback_max == 0 || self.parser.screen().alternate_screen() {
            return;
        }
        let rows = self.parser.screen().size().0;
        for row in 0..count.min(rows) {
            let captured = self.capture_row(row);
            self.push_scrollback_row(captured);
        }
    }

    /// Kitty `C=0` default: after printing an image the cursor moves right by the
    /// placement width and down by its height. Scrolling keeps images anchored.
    fn apply_kitty_advance(&mut self) {
        let Some((adv_cols, adv_rows)) = self.kitty.pending_advance.take() else {
            return;
        };
        let (rows, cols) = self.parser.screen().size();
        let (_, cur_col) = self.parser.screen().cursor_position();
        let target_col = cur_col.saturating_add(adv_cols).min(cols.saturating_sub(1));
        if target_col > cur_col {
            self.parser
                .process(format!("\x1b[{}C", target_col - cur_col).as_bytes());
        }
        let alt = self.parser.screen().alternate_screen();
        for _ in 0..adv_rows {
            let (row, _) = self.parser.screen().cursor_position();
            if row + 1 >= rows {
                if !alt {
                    self.capture_top_rows(1);
                }
                self.parser.process(b"\n");
            } else {
                self.parser.process(b"\x1b[B");
            }
        }
    }
}

impl ScreenModel for Vt100Screen {
    fn feed(&mut self, bytes: &[u8]) {
        if self.scrollback_max == 0 {
            self.parser.process(bytes);
        } else {
            let mut i = 0;
            while i < bytes.len() {
                match self.esc_state {
                    EscState::Normal => {
                        let (rows, cols) = self.parser.screen().size();
                        let (cur_row, cur_col) = self.parser.screen().cursor_position();
                        let alt = self.parser.screen().alternate_screen();

                        // Fast path: feed contiguous bytes while not on bottom edge or in alt screen.
                        if alt || cur_row + 1 < rows {
                            let rel_stop = bytes[i..]
                                .iter()
                                .position(|&b| matches!(b, b'\n' | 0x0b | 0x0c | b'\x1b'))
                                .unwrap_or(bytes.len() - i);
                            if rel_stop > 0 {
                                self.parser.process(&bytes[i..i + rel_stop]);
                                i += rel_stop;
                                continue;
                            }
                        } else if cur_col + 2 < cols {
                            let max_safe = usize::from(cols - 2 - cur_col);
                            let rel_stop = bytes[i..]
                                .iter()
                                .take(max_safe)
                                .position(|&b| {
                                    matches!(b, b'\n' | 0x0b | 0x0c | b'\x1b') || b >= 0x80
                                })
                                .unwrap_or_else(|| (bytes.len() - i).min(max_safe));
                            if rel_stop > 0 {
                                self.parser.process(&bytes[i..i + rel_stop]);
                                i += rel_stop;
                                continue;
                            }
                        }

                        let b = bytes[i];
                        i += 1;
                        match b {
                            b'\x1b' => {
                                self.esc_state = EscState::Esc;
                                self.parser.process(&[b]);
                            }
                            b'\n' | 0x0b | 0x0c => {
                                if !alt && cur_row + 1 >= rows {
                                    self.capture_top_rows(1);
                                }
                                self.parser.process(&[b]);
                            }
                            _ => {
                                let maybe_wrap = !alt && cur_row + 1 >= rows && cur_col + 2 >= cols;
                                let top = if maybe_wrap {
                                    Some(self.capture_row(0))
                                } else {
                                    None
                                };
                                self.parser.process(&[b]);
                                if let Some(row0) = top {
                                    let (new_row, new_col) = self.parser.screen().cursor_position();
                                    if new_row + 1 >= rows && new_col < cur_col && b >= 0x20 {
                                        self.push_scrollback_row(row0);
                                    }
                                }
                            }
                        }
                    }
                    EscState::Esc => {
                        let b = bytes[i];
                        i += 1;
                        match b {
                            b'[' => self.esc_state = EscState::Csi(0),
                            b']' | b'P' | b'^' => self.esc_state = EscState::Osc,
                            b'_' => {
                                self.apc_buf.clear();
                                self.esc_state = EscState::Apc;
                                self.parser.process(&[b]);
                                continue;
                            }
                            b'D' | b'E' => {
                                let (rows, _) = self.parser.screen().size();
                                let (cur_row, _) = self.parser.screen().cursor_position();
                                if cur_row + 1 >= rows {
                                    self.capture_top_rows(1);
                                }
                                self.esc_state = EscState::Normal;
                            }
                            _ => self.esc_state = EscState::Normal,
                        }
                        self.parser.process(&[b]);
                    }
                    EscState::Csi(n) => {
                        let b = bytes[i];
                        i += 1;
                        match b {
                            b'0'..=b'9' => {
                                self.esc_state = EscState::Csi(
                                    n.saturating_mul(10).saturating_add(u16::from(b - b'0')),
                                );
                            }
                            b';' | b':' | b'?' | b'>' | b'<' | b'=' | b' ' | b'!' | b'"'
                            | b'\'' => {}
                            0x40..=0x7e => {
                                if b == b'S' {
                                    self.capture_top_rows(n.max(1));
                                } else if b == b't' && n == 16 {
                                    self.kitty.replies.push(
                                        format!("\x1b[6;{};{}t", self.cell_h, self.cell_w)
                                            .into_bytes(),
                                    );
                                } else if b == b't' && n == 14 {
                                    let (rows, cols) = self.parser.screen().size();
                                    self.kitty.replies.push(
                                        format!(
                                            "\x1b[4;{};{}t",
                                            u32::from(rows) * self.cell_h,
                                            u32::from(cols) * self.cell_w
                                        )
                                        .into_bytes(),
                                    );
                                } else if b == b'n' && n == 6 {
                                    // DSR: CSI 6n -> cursor position report. External
                                    // wrappers (fastfetch-window) block while waiting for
                                    // this reply; without it they wedge the tty in raw mode.
                                    let (cur_row, cur_col) =
                                        self.parser.screen().cursor_position();
                                    self.kitty.replies.push(
                                        format!("\x1b[{};{}R", cur_row + 1, cur_col + 1)
                                            .into_bytes(),
                                    );
                                } else if b == b'J' && (n == 2 || n == 3) {
                                    self.kitty.images.clear();
                                }
                                self.esc_state = EscState::Normal;
                            }
                            _ => self.esc_state = EscState::Normal,
                        }
                        self.parser.process(&[b]);
                    }
                    EscState::Osc => {
                        let b = bytes[i];
                        i += 1;
                        if b == 0x07 {
                            self.esc_state = EscState::Normal;
                        } else if b == b'\x1b' {
                            self.esc_state = EscState::OscEsc;
                        }
                        self.parser.process(&[b]);
                    }
                    EscState::OscEsc => {
                        let b = bytes[i];
                        i += 1;
                        self.esc_state = if b == b'\\' {
                            EscState::Normal
                        } else {
                            EscState::Osc
                        };
                        self.parser.process(&[b]);
                    }
                    EscState::Apc => {
                        let b = bytes[i];
                        i += 1;
                        if b == 0x07 {
                            self.esc_state = EscState::Normal;
                            self.parser.process(b"\x1b\\");
                            let (rows, cols) = self.parser.screen().size();
                            let (cur_row, cur_col) = self.parser.screen().cursor_position();
                            let apc = std::mem::take(&mut self.apc_buf);
                            self.kitty.handle_apc(&apc, cur_col, cur_row, rows, cols);
                            self.apply_kitty_advance();
                        } else if b == b'\x1b' {
                            self.esc_state = EscState::ApcEsc;
                        } else if self.apc_buf.len() < 8 * 1024 * 1024 {
                            self.apc_buf.push(b);
                        }
                    }
                    EscState::ApcEsc => {
                        let b = bytes[i];
                        i += 1;
                        if b == b'\\' {
                            self.esc_state = EscState::Normal;
                            self.parser.process(b"\x1b\\");
                            let (rows, cols) = self.parser.screen().size();
                            let (cur_row, cur_col) = self.parser.screen().cursor_position();
                            let apc = std::mem::take(&mut self.apc_buf);
                            self.kitty.handle_apc(&apc, cur_col, cur_row, rows, cols);
                            self.apply_kitty_advance();
                        } else {
                            self.esc_state = EscState::Apc;
                            if self.apc_buf.len() + 2 < 8 * 1024 * 1024 {
                                self.apc_buf.push(b'\x1b');
                                self.apc_buf.push(b);
                            }
                        }
                    }
                }
            }
        }
        let screen_title = self.parser.screen().title();
        if !screen_title.is_empty() || !self.title.is_empty() {
            self.title = screen_title.to_string();
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.parser.set_size(rows.max(1), cols.max(1));
    }

    fn snapshot(&self) -> GridSnapshot {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let alt = screen.alternate_screen();
        let hist_len = if alt { 0 } else { self.scrollback.len() };
        let offset = if alt {
            0
        } else {
            self.scrollback_offset.min(hist_len)
        };
        let start = hist_len - offset;
        let mut cells = Vec::with_capacity((rows as usize) * (cols as usize));
        for row_idx in 0..usize::from(rows) {
            let global_idx = start + row_idx;
            if global_idx < hist_len {
                let hist_row = &self.scrollback[global_idx];
                for col in 0..usize::from(cols) {
                    cells.push(hist_row.get(col).cloned().unwrap_or_default());
                }
            } else {
                let live_row = (global_idx - hist_len) as u16;
                for col in 0..cols {
                    let cell = screen
                        .cell(live_row, col)
                        .map(Self::to_cell)
                        .unwrap_or_default();
                    cells.push(cell);
                }
            }
        }
        let (cur_row, cur_col) = screen.cursor_position();
        let title = if self.title.is_empty() {
            let t = screen.title();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        } else {
            Some(self.title.clone())
        };
        let mouse_mode = match screen.mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => super::MouseMode::None,
            vt100::MouseProtocolMode::Press => super::MouseMode::Press,
            vt100::MouseProtocolMode::PressRelease => super::MouseMode::PressRelease,
            vt100::MouseProtocolMode::ButtonMotion => super::MouseMode::ButtonMotion,
            vt100::MouseProtocolMode::AnyMotion => super::MouseMode::AnyMotion,
        };
        let mouse_encoding = match screen.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Default => super::MouseEncoding::Default,
            vt100::MouseProtocolEncoding::Utf8 => super::MouseEncoding::Utf8,
            vt100::MouseProtocolEncoding::Sgr => super::MouseEncoding::Sgr,
        };
        GridSnapshot {
            cols,
            rows,
            cells,
            cursor: (cur_col, cur_row),
            title,
            alternate_screen: alt,
            scrollback_offset: offset,
            scrollback_total: hist_len,
            mouse_reporting: mouse_mode != super::MouseMode::None,
            mouse_mode,
            mouse_encoding,
            bell_count: screen.audible_bell_count() + screen.visual_bell_count(),
            images: self.kitty.images.clone(),
        }
    }

    fn title(&self) -> Option<&str> {
        if !self.title.is_empty() {
            Some(&self.title)
        } else {
            let t = self.parser.screen().title();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
    }

    fn terminal_mode(&self) -> TerminalMode {
        let screen = self.parser.screen();
        TerminalMode {
            alternate_screen: screen.alternate_screen(),
            raw_mode: self.raw_mode,
            echo_off: self.echo_off,
            bracketed_paste: screen.bracketed_paste(),
            mouse_reporting: !matches!(
                screen.mouse_protocol_mode(),
                vt100::MouseProtocolMode::None
            ),
        }
    }

    fn set_mode_hints(&mut self, raw_mode: bool, echo_off: bool) {
        self.raw_mode = raw_mode;
        self.echo_off = echo_off;
    }

    fn set_cell_pixels(&mut self, width: u32, height: u32) {
        self.cell_w = width.max(1);
        self.cell_h = height.max(1);
    }

    fn set_scrollback(&mut self, rows: usize) {
        if self.parser.screen().alternate_screen() {
            self.scrollback_offset = 0;
        } else {
            self.scrollback_offset = rows.min(self.scrollback.len());
        }
    }

    fn scrollback(&self) -> usize {
        if self.parser.screen().alternate_screen() {
            0
        } else {
            self.scrollback_offset.min(self.scrollback.len())
        }
    }

    fn row_text(&self, dist_from_bottom: usize) -> Option<String> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let r = usize::from(rows);
        if dist_from_bottom < r {
            let live_row = (r - 1 - dist_from_bottom) as u16;
            let text: String = (0..cols)
                .map(|c| {
                    screen
                        .cell(live_row, c)
                        .and_then(|cell| cell.contents().chars().next())
                        .unwrap_or(' ')
                })
                .collect();
            return Some(text);
        }
        if screen.alternate_screen() {
            return None;
        }
        let hist_back = dist_from_bottom - r;
        let idx = self.scrollback.len().checked_sub(1 + hist_back)?;
        let row = self.scrollback.get(idx)?;
        Some(
            row.iter()
                .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                .collect(),
        )
    }

    fn mark_output(&self) -> super::OutputAnchor {
        let screen = self.parser.screen();
        let (cur_row, _) = screen.cursor_position();
        super::OutputAnchor {
            abs_row: self
                .history_rows
                .saturating_add(u64::from(cur_row))
                .saturating_add(1),
        }
    }

    fn capture_since(&self, anchor: super::OutputAnchor, max_bytes: usize) -> String {
        let screen = self.parser.screen();
        if screen.alternate_screen() {
            return String::new();
        }

        let (rows, cols) = screen.size();
        let (cur_row, _) = screen.cursor_position();
        let _total_abs_rows = self.history_rows.saturating_add(u64::from(rows));

        let start_abs = anchor.abs_row;
        let end_abs = self.history_rows.saturating_add(u64::from(cur_row));
        if start_abs > end_abs {
            return String::new();
        }

        let mut lines = Vec::new();
        let scrollback_len = self.scrollback.len() as u64;
        let oldest_retained_abs = self.history_rows.saturating_sub(scrollback_len);

        for abs in start_abs..=end_abs {
            if abs < oldest_retained_abs {
                continue;
            }
            if abs < self.history_rows {
                let sb_idx = (abs - oldest_retained_abs) as usize;
                if let Some(cells) = self.scrollback.get(sb_idx) {
                    let line: String = cells
                        .iter()
                        .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                        .collect();
                    lines.push(line.trim_end().to_string());
                }
            } else {
                let live_row = (abs - self.history_rows) as u16;
                if live_row < rows {
                    let line: String = (0..cols)
                        .map(|c| {
                            screen
                                .cell(live_row, c)
                                .and_then(|cell| cell.contents().chars().next())
                                .unwrap_or(' ')
                        })
                        .collect();
                    lines.push(line.trim_end().to_string());
                }
            }
        }

        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }

        let joined = lines.join("\n");
        if joined.len() <= max_bytes {
            return joined;
        }
        let keep_bytes = max_bytes.saturating_sub(6); // "[…]\n" length
        // Slice on a UTF-8 boundary or a multi-byte glyph panics.
        let mut start = joined.len().saturating_sub(keep_bytes);
        while start < joined.len() && !joined.is_char_boundary(start) {
            start += 1;
        }
        format!("[…]\n{}", &joined[start..])
    }

    fn take_replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.kitty.replies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_row(snap: &GridSnapshot, row: u16) -> String {
        (0..snap.cols)
            .filter_map(|col| snap.cell(col, row))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn scrollback_tracks_and_clamps_history() {
        let mut screen = Vt100Screen::new(40, 4, 50);
        for i in 1..=20 {
            screen.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(screen.scrollback(), 0);
        // 5 > rows (4) previously panicked in vt100 0.15.2.
        screen.set_scrollback(5);
        assert_eq!(screen.scrollback(), 5);
        let snap = screen.snapshot();
        assert_eq!(snap.scrollback_offset, 5);
        assert_eq!(snap_row(&snap, 0), "line 13");
        assert_eq!(snap_row(&snap, 3), "line 16");

        // Clamps cleanly at buffer size.
        screen.set_scrollback(999);
        let clamped = screen.scrollback();
        assert!(clamped > 5 && clamped <= 50);
        let top = screen.snapshot();
        assert_eq!(snap_row(&top, 0), "line 1");
        assert!(screen.row_text(20).is_some());

        screen.set_scrollback(0);
        assert_eq!(screen.scrollback(), 0);
    }

    fn b64(data: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    #[test]
    fn kitty_png_apc_becomes_an_image_without_polluting_the_grid() {
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, 1, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 255]).unwrap();
        }
        let mut screen = Vt100Screen::new(20, 6, 10);
        screen.feed(
            format!(
                "before\x1b[3;5H\x1b_Ga=T,f=100,i=9;{}\x1b\\after",
                b64(&png)
            )
            .as_bytes(),
        );
        let snap = screen.snapshot();
        assert_eq!(snap.images.len(), 1);
        assert_eq!(snap.images[0].id, 9);
        assert_eq!(snap.images[0].col, 4);
        assert_eq!(snap.images[0].dist_from_bottom, 3);
        assert_eq!(snap.images[0].width, 1);
        assert_eq!(snap.images[0].height, 1);
        let text: String = snap.cells.iter().map(|c| c.ch).collect();
        assert!(text.contains("before") && text.contains("after"));
        assert!(!text.contains("iVBOR") && !text.contains("G a=T"));
    }

    #[test]
    fn dsr_query_reports_cursor_position() {
        let mut screen = Vt100Screen::new(20, 6, 10);
        screen.feed(b"abc\x1b[3;5H\x1b[6n");
        assert_eq!(screen.take_replies(), vec![b"\x1b[3;5R".to_vec()]);
    }

    #[test]
    fn capture_since_respects_anchor_and_tail_truncation() {
        let mut screen = Vt100Screen::new(40, 4, 100);
        screen.feed(b"prompt$ command");
        let anchor = screen.mark_output();
        screen.feed(b"\r\nhello world output line 1\r\nhello world output line 2");

        let captured = screen.capture_since(anchor, 1024);
        assert!(
            captured.contains("hello world output line 1"),
            "captured was: {captured:?}"
        );
        assert!(captured.contains("hello world output line 2"));
        assert!(!captured.contains("prompt$ command"));

        // Truncation test
        let truncated = screen.capture_since(anchor, 15);
        assert!(truncated.starts_with("[…]\n"));
        assert!(truncated.len() <= 20);
    }
}
