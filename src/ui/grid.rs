//! Monospace viewport renderer.
//!
//! Draws `GridSnapshot` as a fixed cell grid; the font is egui's built-in
//! monospace (no custom glyph atlas in v1). Scrollback above the viewport is not
//! rendered in v1; `pty` keeps the lines for v2.

use crate::config::PromptIcon;
use crate::contracts::{Candidate, RiskLevel};
use crate::input::safety;
use crate::pty::{Color, GridSnapshot};
use crate::theme::ThemePalette;
use crate::ui::overlay::OverlayState;
use eframe::egui;

fn ansi_256_color(idx: u8) -> egui::Color32 {
    const ANSI_16: [[u8; 3]; 16] = [
        [0x15, 0x16, 0x1e],
        [0xf7, 0x76, 0x8e],
        [0x9e, 0xce, 0x6a],
        [0xe0, 0xaf, 0x68],
        [0x7a, 0xa2, 0xf7],
        [0xbb, 0x9a, 0xf7],
        [0x7d, 0xcf, 0xff],
        [0xa9, 0xb1, 0xd6],
        [0x41, 0x48, 0x68],
        [0xf7, 0x76, 0x8e],
        [0x9e, 0xce, 0x6a],
        [0xe0, 0xaf, 0x68],
        [0x7a, 0xa2, 0xf7],
        [0xbb, 0x9a, 0xf7],
        [0x7d, 0xcf, 0xff],
        [0xc0, 0xca, 0xf5],
    ];
    if idx < 16 {
        let [r, g, b] = ANSI_16[idx as usize];
        return egui::Color32::from_rgb(r, g, b);
    }
    if idx < 232 {
        let n = idx - 16;
        let r = n / 36;
        let g = (n % 36) / 6;
        let b = n % 6;
        let to_u8 = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        return egui::Color32::from_rgb(to_u8(r), to_u8(g), to_u8(b));
    }
    let gray = 8 + (idx - 232) * 10;
    egui::Color32::from_rgb(gray, gray, gray)
}

fn resolve_color(color: Color, default: egui::Color32) -> egui::Color32 {
    match color {
        Color::Default => default,
        Color::Indexed(idx) => ansi_256_color(idx),
        Color::Rgb(r, g, b) => egui::Color32::from_rgb(r, g, b),
    }
}

/// Cell pixel dimensions derived from `ui`'s monospace font metrics.
pub fn cell_size(ui: &egui::Ui, theme: &ThemePalette) -> egui::Vec2 {
    let font_id = egui::FontId::monospace(theme.font_size);
    ui.fonts(|f| {
        let w = f.glyph_width(&font_id, 'M').max(1.0);
        let h = (f.row_height(&font_id) * 1.18).ceil().max(1.0);
        egui::vec2(w, h)
    })
}

/// Nerd Font / symbol glyphs often advance wider than one cell (`pw` > cell
/// width) or fall back to a proportional font. Swap in a narrow, ink-similar
/// alternate (`[`, `*`, `~`, `o`) when available so later cells in the same
/// PDF/icon run do not step right and garble the row.
fn alt_glyph(ui: &egui::Ui, font_id: &egui::FontId, ch: char, cell_w: f32) -> Option<char> {
    if let Some(alt) = nerd_alt(ch) {
        return Some(alt);
    }
    if ch.is_ascii() {
        return None;
    }
    if ui.fonts(|f| f.glyph_width(font_id, ch)) <= cell_w * 1.04 {
        return None;
    }
    ['[', '*', '~', 'o']
        .into_iter()
        .find(|c| ui.fonts(|f| f.glyph_width(font_id, *c)) <= cell_w * 1.04)
}

/// Hand-picked narrow stand-ins for the icons fastfetch/neofetch emit.
fn nerd_alt(ch: char) -> Option<char> {
    Some(match u32::from(ch) {
        0xf17c => 'w',  // Linux
        0xf313 => ']',  // Arch/EndeavourOS
        0xf607 => '♥',  // EndeavourOS
        0xf0ad => '*',  // CJK-style wrench
        0xf0e7 => '*',  // speed/uptime
        0xf17a | 0xf489 | 0xf108 => '>', // terminal / window
        0xf114 => '$',  // shell
        0xf07c => '|',  // log
        0xf0c9 => '≡',  // list
        0xf538 => 'V',  // version
        _ => return None,
    })
}

/// Number of `(cols, rows)` that fit inside `avail` pixels for `cell`.
pub fn grid_dimensions(avail: egui::Vec2, cell: egui::Vec2) -> (u16, u16) {
    let cols = (avail.x / cell.x.max(1.0)).floor().max(1.0) as u16;
    let rows = (avail.y / cell.y.max(1.0)).floor().max(1.0) as u16;
    (cols, rows)
}

pub type CellCoord = (u16, u16);

#[derive(Clone, Copy, Debug)]
// Shortcut: геометрия кадра снимается целиком, читаются пока только hit-поля.
#[allow(dead_code)]
pub struct GridMetrics {
    pub viewport: egui::Rect,
    pub cursor_rect: egui::Rect,
    pub cell_size: egui::Vec2,
    pub clicked_hint: Option<usize>,
}

pub fn normalized_selection(a: CellCoord, b: CellCoord) -> (CellCoord, CellCoord) {
    if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

pub fn row_selection_rect(
    snapshot: &GridSnapshot,
    origin: egui::Pos2,
    cell_size: egui::Vec2,
    row: u16,
    range: Option<((u16, isize), (u16, isize))>,
) -> Option<egui::Rect> {
    let ((c_a, r_a), (c_b, r_b)) = range?;
    let (top, bot) = if (r_a, c_a) <= (r_b, c_b) {
        ((c_a, r_a), (c_b, r_b))
    } else {
        ((c_b, r_b), (c_a, r_a))
    };
    if top == bot {
        return None;
    }
    let r = row as isize;
    if r < top.1 || r > bot.1 {
        return None;
    }
    let last_non_space = (0..snapshot.cols).rev().find(|&c| {
        snapshot
            .cell(c, row)
            .is_some_and(|cell| cell.ch != ' ' && cell.ch != '\0')
    });
    let span = if top.1 == bot.1 {
        last_non_space.and_then(|m| (top.0 <= m).then_some((top.0, bot.0.min(m))))
    } else if r == top.1 {
        last_non_space.and_then(|m| (top.0 <= m).then_some((top.0, m)))
    } else if r == bot.1 {
        last_non_space.map(|m| (0, bot.0.min(m)))
    } else {
        last_non_space.map(|m| (0, m))
    };
    let y0 = origin.y + row as f32 * cell_size.y + 0.5;
    let y1 = origin.y + (row + 1) as f32 * cell_size.y - 0.5;
    match span {
        Some((c0, c1)) if c0 <= c1 => Some(egui::Rect::from_min_max(
            egui::pos2(origin.x + c0 as f32 * cell_size.x, y0),
            egui::pos2(origin.x + (c1 + 1) as f32 * cell_size.x, y1),
        )),
        _ if top.1 < bot.1 => Some(egui::Rect::from_min_max(
            egui::pos2(origin.x, y0),
            egui::pos2(origin.x + (cell_size.x * 0.45).max(3.0), y1),
        )),
        _ => None,
    }
}

fn resolve_prompt_icon(icon: PromptIcon, row: u16, scrollback_offset: usize) -> PromptIcon {
    match icon {
        PromptIcon::Chaos => {
            const POOL: [PromptIcon; 6] = [
                PromptIcon::Spark,
                PromptIcon::Bolt,
                PromptIcon::Cat,
                PromptIcon::Ghost,
                PromptIcon::Rocket,
                PromptIcon::Lambda,
            ];
            POOL[(usize::from(row) + scrollback_offset) % POOL.len()]
        }
        other => other,
    }
}

fn draw_prompt_icon(
    painter: &egui::Painter,
    cell_r: egui::Rect,
    _font_id: &egui::FontId,
    fg: egui::Color32,
    bg: egui::Color32,
    icon: PromptIcon,
) {
    let c = cell_r.center();
    let s = cell_r.width().min(cell_r.height());
    match icon {
        PromptIcon::Arrow | PromptIcon::Chaos => {}
        PromptIcon::Lambda => {
            let stroke = egui::Stroke::new((s * 0.13).max(1.2), fg);
            let top = egui::pos2(c.x - s * 0.24, c.y - s * 0.36);
            let bot_r = egui::pos2(c.x + s * 0.28, c.y + s * 0.36);
            let mid = egui::pos2(c.x + s * 0.01, c.y - s * 0.02);
            let bot_l = egui::pos2(c.x - s * 0.26, c.y + s * 0.36);
            painter.line_segment([top, bot_r], stroke);
            painter.line_segment([mid, bot_l], stroke);
        }
        PromptIcon::Spark => {
            let rx = s * 0.44;
            let ry = s * 0.46;
            let mid = s * 0.14;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x, c.y - ry),
                    egui::pos2(c.x + mid, c.y),
                    egui::pos2(c.x, c.y + ry),
                    egui::pos2(c.x - mid, c.y),
                ],
                fg,
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - rx, c.y),
                    egui::pos2(c.x, c.y - mid),
                    egui::pos2(c.x + rx, c.y),
                    egui::pos2(c.x, c.y + mid),
                ],
                fg,
                egui::Stroke::NONE,
            ));
        }
        PromptIcon::Bolt => {
            let w = s * 0.38;
            let h = s * 0.46;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x + w * 0.35, c.y - h),
                    egui::pos2(c.x - w * 0.9, c.y + h * 0.12),
                    egui::pos2(c.x + w * 0.15, c.y + h * 0.12),
                ],
                fg,
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - w * 0.15, c.y - h * 0.12),
                    egui::pos2(c.x + w * 0.9, c.y - h * 0.12),
                    egui::pos2(c.x - w * 0.35, c.y + h),
                ],
                fg,
                egui::Stroke::NONE,
            ));
        }
        PromptIcon::Cat => {
            let r = s * 0.36;
            let head_c = egui::pos2(c.x, c.y + r * 0.25);
            painter.circle_filled(head_c, r * 0.88, fg);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(head_c.x - r * 0.85, head_c.y - r * 0.2),
                    egui::pos2(head_c.x - r * 0.82, head_c.y - r * 1.25),
                    egui::pos2(head_c.x - r * 0.15, head_c.y - r * 0.72),
                ],
                fg,
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(head_c.x + r * 0.15, head_c.y - r * 0.72),
                    egui::pos2(head_c.x + r * 0.82, head_c.y - r * 1.25),
                    egui::pos2(head_c.x + r * 0.85, head_c.y - r * 0.2),
                ],
                fg,
                egui::Stroke::NONE,
            ));
            let eye_r = (r * 0.19).max(1.0);
            painter.circle_filled(
                egui::pos2(head_c.x - r * 0.34, head_c.y - r * 0.08),
                eye_r,
                bg,
            );
            painter.circle_filled(
                egui::pos2(head_c.x + r * 0.34, head_c.y - r * 0.08),
                eye_r,
                bg,
            );
        }
        PromptIcon::Ghost => {
            let r = s * 0.38;
            let dome = egui::pos2(c.x, c.y - r * 0.15);
            painter.circle_filled(dome, r * 0.85, fg);
            let body = egui::Rect::from_min_max(
                egui::pos2(dome.x - r * 0.85, dome.y),
                egui::pos2(dome.x + r * 0.85, dome.y + r * 0.95),
            );
            painter.rect_filled(body, 0.0, fg);
            let eye_r = (r * 0.2).max(1.0);
            painter.circle_filled(egui::pos2(dome.x - r * 0.32, dome.y - r * 0.05), eye_r, bg);
            painter.circle_filled(egui::pos2(dome.x + r * 0.32, dome.y - r * 0.05), eye_r, bg);
        }
        PromptIcon::Rocket => {
            let w = s * 0.28;
            let h = s * 0.46;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x, c.y - h),
                    egui::pos2(c.x + w, c.y - h * 0.15),
                    egui::pos2(c.x + w * 0.75, c.y + h * 0.55),
                    egui::pos2(c.x - w * 0.75, c.y + h * 0.55),
                    egui::pos2(c.x - w, c.y - h * 0.15),
                ],
                fg,
                egui::Stroke::NONE,
            ));
            painter.circle_filled(egui::pos2(c.x, c.y - h * 0.2), (w * 0.38).max(1.0), bg);
            painter.circle_filled(egui::pos2(c.x, c.y + h * 0.82), (w * 0.36).max(1.1), fg);
        }
    }
}

/// Draws any `PromptIcon` as a crisp vector badge for Settings UI.
pub fn draw_prompt_icon_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    fg: egui::Color32,
    bg: egui::Color32,
    icon: PromptIcon,
) {
    let c = rect.center();
    let s = rect.width().min(rect.height());
    match icon {
        PromptIcon::Arrow => {
            let stroke = egui::Stroke::new(1.7_f32, fg);
            let dx = s * 0.22;
            let dy = s * 0.30;
            painter.line_segment(
                [egui::pos2(c.x - dx, c.y - dy), egui::pos2(c.x + dx, c.y)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x + dx, c.y), egui::pos2(c.x - dx, c.y + dy)],
                stroke,
            );
        }
        PromptIcon::Chaos => {
            let half = s * 0.36;
            let die = egui::Rect::from_center_size(c, egui::vec2(half * 2.0, half * 2.0));
            painter.rect_stroke(
                die,
                egui::Rounding::same(3.0),
                egui::Stroke::new(1.4_f32, fg),
            );
            let r = (s * 0.08).max(1.2);
            painter.circle_filled(egui::pos2(c.x - half * 0.42, c.y - half * 0.42), r, fg);
            painter.circle_filled(c, r, fg);
            painter.circle_filled(egui::pos2(c.x + half * 0.42, c.y + half * 0.42), r, fg);
        }
        other => {
            let dummy_font = egui::FontId::monospace(12.0);
            draw_prompt_icon(painter, rect, &dummy_font, fg, bg, other);
        }
    }
}

/// Shortcut: выбор мышью пока не подключён к app; вызывается только из тестов,
/// пока копирование выделения не заведено в UI.
#[allow(dead_code)]
pub fn extract_selected_text(snapshot: &GridSnapshot, a: CellCoord, b: CellCoord) -> String {
    let (start, end) = normalized_selection(a, b);
    if start == end {
        return String::new();
    }
    let mut lines = Vec::new();
    for row in start.1..=end.1.min(snapshot.rows.saturating_sub(1)) {
        let from_c = if row == start.1 { start.0 } else { 0 };
        let to_c = if row == end.1 {
            end.0.min(snapshot.cols.saturating_sub(1))
        } else {
            snapshot.cols.saturating_sub(1)
        };
        let mut line = String::new();
        for col in from_c..=to_c {
            let ch = snapshot.cell(col, row).map(|c| c.ch).unwrap_or(' ');
            line.push(if ch == '\0' { ' ' } else { ch });
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

/// Detects an `http(s)://` URL or existing path token under `(col, row)`.
pub fn link_at(
    snapshot: &GridSnapshot,
    col: u16,
    row: u16,
    cwd: &std::path::Path,
) -> Option<(u16, u16, String)> {
    if row >= snapshot.rows || col >= snapshot.cols {
        return None;
    }
    let is_sep = |ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '`' | '\0');
    let cell_ch = |c: u16| snapshot.cell(c, row).map(|cell| cell.ch).unwrap_or(' ');
    if is_sep(cell_ch(col)) {
        return None;
    }
    let mut start = col;
    while start > 0 && !is_sep(cell_ch(start - 1)) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < snapshot.cols && !is_sep(cell_ch(end + 1)) {
        end += 1;
    }
    let raw: String = (start..=end).map(cell_ch).collect();
    let lead_chars = raw.chars().take_while(|c| matches!(c, '(' | '[')).count() as u16;
    let trimmed = raw
        .trim_start_matches(|c| matches!(c, '(' | '['))
        .trim_end_matches(|c| matches!(c, '.' | ',' | ';' | ':' | ')' | ']'));
    if trimmed.is_empty() {
        return None;
    }
    let start_adj = start + lead_chars;
    let end_adj = start_adj + (trimmed.chars().count() as u16).saturating_sub(1);
    if col < start_adj || col > end_adj {
        return None;
    }

    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Some((start_adj, end_adj, trimmed.to_string()));
    }

    let candidate_path = trimmed
        .rsplit_once(':')
        .and_then(|(head, tail)| {
            if tail.chars().all(|c| c.is_ascii_digit()) {
                Some(head)
            } else {
                None
            }
        })
        .unwrap_or(trimmed);

    if !(candidate_path.contains('/') || candidate_path.starts_with('~')) {
        return None;
    }
    let resolved = if let Some(rest) = candidate_path.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else {
        let p = std::path::Path::new(candidate_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    if resolved.exists() {
        Some((start, end_adj, resolved.to_string_lossy().into_owned()))
    } else {
        None
    }
}

fn lerp_color(a: egui::Color32, b: egui::Color32, a_weight: u16) -> egui::Color32 {
    let b_weight = 100 - a_weight;
    let ch = |x: u8, y: u8| -> u8 { ((x as u16 * a_weight + y as u16 * b_weight) / 100) as u8 };
    egui::Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

fn draw_inline_cells(
    painter: &egui::Painter,
    origin: egui::Pos2,
    cell_size: egui::Vec2,
    font_id: &egui::FontId,
    start_col: u16,
    max_cols: u16,
    target_row: u16,
    text: &str,
    color: egui::Color32,
) -> u16 {
    let mut utf8_buf = [0u8; 4];
    let mut col = start_col;
    for ch in text.chars() {
        if col >= max_cols {
            break;
        }
        if ch != ' ' && ch != '\0' {
            let r = cell_rect(origin, cell_size, col, target_row);
            let s = ch.encode_utf8(&mut utf8_buf);
            painter.text(r.min, egui::Align2::LEFT_TOP, s, font_id.clone(), color);
        }
        col = col.saturating_add(1);
    }
    col
}

/// Rows a `chars`-long line occupies when it wraps past `max_cols` and continues
/// on the next row at `indent`.
fn wrapped_rows(chars: usize, start_col: u16, indent: u16, max_cols: u16) -> u16 {
    let width = max_cols.saturating_sub(indent).max(1) as usize;
    let first = max_cols.saturating_sub(start_col).max(1) as usize;
    if chars <= first {
        1
    } else {
        u16::try_from(1 + (chars - first).div_ceil(width)).unwrap_or(u16::MAX)
    }
}

/// Like [`draw_inline_cells`] but wraps instead of clipping at `max_cols`.
/// Returns the `(row, col)` where the next chunk starts.
#[allow(clippy::too_many_arguments)]
fn draw_inline_wrapped(
    painter: &egui::Painter,
    origin: egui::Pos2,
    cell_size: egui::Vec2,
    font_id: &egui::FontId,
    indent: u16,
    max_cols: u16,
    row_limit: u16,
    mut row: u16,
    mut col: u16,
    text: &str,
    color: egui::Color32,
) -> (u16, u16) {
    let mut utf8_buf = [0u8; 4];
    let indent = indent.min(max_cols.saturating_sub(1));
    for ch in text.chars() {
        if col >= max_cols {
            row = row.saturating_add(1);
            col = indent;
            if row >= row_limit {
                return (row, col);
            }
        }
        if ch != ' ' && ch != '\0' {
            let r = cell_rect(origin, cell_size, col, row);
            let s = ch.encode_utf8(&mut utf8_buf);
            painter.text(r.min, egui::Align2::LEFT_TOP, s, font_id.clone(), color);
        }
        col = col.saturating_add(1);
    }
    (row, col)
}

/// Paints `cols x rows` cells, cursor, inline ghost suffix, loading spinner, and muted gray inline hints.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &mut egui::Ui,
    snapshot: &GridSnapshot,
    theme: &ThemePalette,
    ghost_suffix: Option<&str>,
    live_hints: &[Candidate],
    live_hint_selected: Option<usize>,
    loading_text: Option<&str>,
    inline_overlay: Option<&OverlayState>,
    command_rows: u16,
    cmd_start_col: u16,
    bar_cursor: bool,
    selection_range: Option<((u16, isize), (u16, isize))>,
    hovered_link: Option<(u16, u16, u16)>,
    search_hits: &[(u16, u16, u16, bool)],
) -> GridMetrics {
    let font_id = egui::FontId::monospace(theme.font_size);
    let cell_size = cell_size(ui, theme);

    let avail = ui.available_size();
    let (rect, _response) = ui.allocate_exact_size(avail, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    let default_bg = theme.background.to_color32();
    let default_fg = theme.foreground.to_color32();
    let accent = theme.accent.to_color32();
    let selection = theme.selection.to_color32();
    painter.rect_filled(rect, 0.0, default_bg);

    let origin = rect.min;
    let cursor_row = snapshot.cursor.1;
    let mut utf8_buf = [0u8; 4];

    // Subtle active-line highlight while typing at the shell prompt.
    if bar_cursor && cursor_row < snapshot.rows {
        let line_rect = egui::Rect::from_min_size(
            egui::pos2(origin.x, origin.y + cursor_row as f32 * cell_size.y),
            egui::vec2(rect.width(), cell_size.y),
        );
        let row_tint = lerp_color(selection, default_bg, 10);
        painter.rect_filled(line_rect, 0.0, row_tint);
    }

    let sel_fill = egui::Color32::from_rgba_unmultiplied(
        accent.r(),
        accent.g(),
        accent.b(),
        if theme.is_light() { 44 } else { 36 },
    );

    for row in 0..snapshot.rows {
        for col in 0..snapshot.cols {
            let Some(cell) = snapshot.cell(col, row) else {
                continue;
            };
            let cell_r = cell_rect(origin, cell_size, col, row);
            let mut fg = resolve_color(cell.fg, default_fg);
            let mut bg = resolve_color(cell.bg, default_bg);
            if cell.attrs.dim {
                fg = lerp_color(fg, default_bg, 65);
            }
            if cell.attrs.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }

            let is_cursor = (col, row) == snapshot.cursor;
            if is_cursor && !bar_cursor && snapshot.scrollback_offset == 0 {
                bg = accent;
                fg = default_bg;
            }

            if bg != default_bg {
                painter.rect_filled(cell_r, 0.0, bg);
            }
        }

        if let Some(sel_rect) =
            row_selection_rect(snapshot, origin, cell_size, row, selection_range)
        {
            painter.rect_filled(sel_rect, egui::Rounding::same(2.5), sel_fill);
        }

        for &(hit_row, c0, c1, is_active) in search_hits {
            if hit_row == row && c1 > c0 && c0 < snapshot.cols {
                let end_col = c1.min(snapshot.cols);
                let hit_rect = egui::Rect::from_min_max(
                    egui::pos2(
                        origin.x + f32::from(c0) * cell_size.x,
                        origin.y + f32::from(row) * cell_size.y,
                    ),
                    egui::pos2(
                        origin.x + f32::from(end_col) * cell_size.x,
                        origin.y + f32::from(row + 1) * cell_size.y,
                    ),
                );
                let fill = if is_active {
                    egui::Color32::from_rgba_unmultiplied(245, 158, 11, 110)
                } else {
                    egui::Color32::from_rgba_unmultiplied(245, 158, 11, 48)
                };
                painter.rect_filled(hit_rect, egui::Rounding::same(2.0), fill);
                if is_active {
                    painter.rect_stroke(
                        hit_rect,
                        egui::Rounding::same(2.0),
                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(245, 158, 11)),
                    );
                }
            }
        }

        for col in 0..snapshot.cols {
            let Some(cell) = snapshot.cell(col, row) else {
                continue;
            };
            let cell_r = cell_rect(origin, cell_size, col, row);
            let mut fg = resolve_color(cell.fg, default_fg);
            let mut bg = resolve_color(cell.bg, default_bg);
            if cell.attrs.dim {
                fg = lerp_color(fg, default_bg, 65);
            }
            if cell.attrs.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let is_cursor = (col, row) == snapshot.cursor;
            if is_cursor && !bar_cursor && snapshot.scrollback_offset == 0 {
                bg = accent;
                fg = default_bg;
            }

            if cell.ch != ' ' && cell.ch != '\0' {
                let prompt_replace = !snapshot.alternate_screen
                    && matches!(cell.ch, '❯' | '➜')
                    && theme.prompt_icon != PromptIcon::Arrow;
                if prompt_replace {
                    let resolved =
                        resolve_prompt_icon(theme.prompt_icon, row, snapshot.scrollback_offset);
                    draw_prompt_icon(&painter, cell_r, &font_id, fg, bg, resolved);
                } else {
                    let alt = alt_glyph(ui, &font_id, cell.ch, cell_size.x);
                    let s = alt.unwrap_or(cell.ch).encode_utf8(&mut utf8_buf);
                    painter.text(cell_r.min, egui::Align2::LEFT_TOP, s, font_id.clone(), fg);
                }
            }

            let is_link = hovered_link.is_some_and(|(lr, s, e)| row == lr && col >= s && col <= e);
            if cell.attrs.underline || is_link {
                let y = cell_r.bottom() - 1.0;
                painter.line_segment(
                    [egui::pos2(cell_r.left(), y), egui::pos2(cell_r.right(), y)],
                    egui::Stroke::new(1.0_f32, if is_link { accent } else { fg }),
                );
            }
        }
    }

    let cursor_rect = cell_rect(origin, cell_size, snapshot.cursor.0, snapshot.cursor.1);
    if bar_cursor {
        let bar = egui::Rect::from_min_max(
            egui::pos2(cursor_rect.left(), cursor_rect.top() + 1.0),
            egui::pos2(
                (cursor_rect.left() + 2.0).min(cursor_rect.right()),
                cursor_rect.bottom() - 1.0,
            ),
        );
        painter.rect_filled(bar, 1.0, accent);
    }

    let active_gray = theme.muted_fg(66);
    let primary_gray = theme.muted_fg(52);
    let secondary_gray = theme.muted_fg(38);
    let note_gray = theme.muted_fg(26);

    if !snapshot.images.is_empty() {
        let tex_map_id = egui::Id::new("lexicue_kitty_textures");
        let mut cache: std::collections::HashMap<u64, egui::TextureHandle> = ui
            .ctx()
            .data_mut(|d| {
                d.get_temp::<std::collections::HashMap<u64, egui::TextureHandle>>(tex_map_id)
            })
            .unwrap_or_default();
        let active_ids: std::collections::HashSet<u64> =
            snapshot.images.iter().map(|i| i.id).collect();
        cache.retain(|id, _| active_ids.contains(id));

        for img in &snapshot.images {
            let view_row = (snapshot.rows as isize - 1) + snapshot.scrollback_offset as isize
                - img.dist_from_bottom as isize;
            let (w_px, h_px) = match (img.cols, img.rows) {
                (Some(c), Some(r)) => (f32::from(c) * cell_size.x, f32::from(r) * cell_size.y),
                (Some(c), None) => {
                    let w = f32::from(c) * cell_size.x;
                    let h = w * (img.height.max(1) as f32 / img.width.max(1) as f32);
                    (w, h)
                }
                (None, Some(r)) => {
                    let h = f32::from(r) * cell_size.y;
                    let w = h * (img.width.max(1) as f32 / img.height.max(1) as f32);
                    (w, h)
                }
                (None, None) => {
                    let max_w = rect.width().max(cell_size.x);
                    let scale = (max_w / img.width.max(1) as f32).min(1.0);
                    (img.width as f32 * scale, img.height as f32 * scale)
                }
            };
            let top_y = origin.y + view_row as f32 * cell_size.y;
            let left_x = origin.x + f32::from(img.col) * cell_size.x;
            let img_rect =
                egui::Rect::from_min_size(egui::pos2(left_x, top_y), egui::vec2(w_px, h_px));
            if img_rect.bottom() < rect.top() || img_rect.top() > rect.bottom() {
                continue;
            }
            let tex = cache.entry(img.id).or_insert_with(|| {
                let color_img = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.rgba,
                );
                ui.ctx().load_texture(
                    format!("kitty_img_{}", img.id),
                    color_img,
                    egui::TextureOptions::LINEAR,
                )
            });
            painter.image(
                tex.id(),
                img_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        ui.ctx().data_mut(|d| d.insert_temp(tex_map_id, cache));
    }

    if let Some(ghost) = ghost_suffix {
        let row = snapshot.cursor.1;
        for (offset, ch) in ghost.chars().enumerate() {
            let col = snapshot.cursor.0.saturating_add(offset as u16);
            if col >= snapshot.cols {
                break;
            }
            if ch != ' ' && ch != '\0' {
                let r = cell_rect(origin, cell_size, col, row);
                let s = ch.encode_utf8(&mut utf8_buf);
                painter.text(
                    r.min,
                    egui::Align2::LEFT_TOP,
                    s,
                    font_id.clone(),
                    primary_gray,
                );
            }
        }
    }

    // Inline AI loading spinner, confirmation/slot prompt, or gray suggestions (all inside the terminal grid).
    let mut clicked_hint = None;
    let start_col = cmd_start_col.min(snapshot.cols.saturating_sub(1));
    let clear_bg = default_bg;
    let space_below = snapshot.rows.saturating_sub(cursor_row + 1);
    // First visual row of a multi-line / soft-wrapped command. Hints and the
    // spinner are anchored to it so they never paint over the command itself.
    let first_cmd_row = cursor_row.saturating_sub(command_rows.max(1).saturating_sub(1));

    let clear_cols_rect = |row: u16, from_col: u16, char_len: usize| {
        let to_col = from_col.saturating_add(char_len as u16).min(snapshot.cols);
        let min = cell_rect(origin, cell_size, from_col, row).min;
        let max_x = cell_rect(origin, cell_size, to_col, row)
            .min
            .x
            .min(rect.right());
        egui::Rect::from_min_max(min, egui::pos2(max_x.max(min.x), min.y + cell_size.y))
    };

    if bar_cursor && snapshot.rows > 1 {
        if let Some(spinner) = loading_text {
            let target_row = if space_below >= 1 {
                cursor_row + 1
            } else {
                first_cmd_row.saturating_sub(1)
            };
            if target_row < snapshot.rows {
                let row_rect = clear_cols_rect(target_row, start_col, spinner.chars().count());
                painter.rect_filled(row_rect, 0.0, clear_bg);
                let mut chars = spinner.chars();
                let first = chars.next();
                if let Some(braille) = first.filter(|c| ('\u{2800}'..='\u{28FF}').contains(c)) {
                    let bits = (braille as u32) - 0x2800;
                    let cell = cell_rect(origin, cell_size, start_col, target_row);
                    let radius = (cell_size.x * 0.115).clamp(1.1, 1.9);
                    let dim_dot = lerp_color(primary_gray, default_bg, 78);
                    // Unicode Braille 2x4 bit offsets: left col [0,1,2,6], right col [3,4,5,7].
                    const DOT_BITS: [[u32; 4]; 2] =
                        [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
                    for (cx, col_bits) in DOT_BITS.iter().enumerate() {
                        let px = cell.left() + cell_size.x * if cx == 0 { 0.30 } else { 0.70 };
                        for (ry, bit) in col_bits.iter().enumerate() {
                            let py = cell.top() + cell_size.y * (0.20 + ry as f32 * 0.20);
                            let on = (bits & bit) != 0;
                            painter.circle_filled(
                                egui::pos2(px, py),
                                radius,
                                if on { active_gray } else { dim_dot },
                            );
                        }
                    }
                    draw_inline_cells(
                        &painter,
                        origin,
                        cell_size,
                        &font_id,
                        start_col.saturating_add(1),
                        snapshot.cols,
                        target_row,
                        chars.as_str(),
                        primary_gray,
                    );
                } else {
                    draw_inline_cells(
                        &painter,
                        origin,
                        cell_size,
                        &font_id,
                        start_col,
                        snapshot.cols,
                        target_row,
                        spinner,
                        primary_gray,
                    );
                }
            }
        } else if let Some(ov) = inline_overlay.filter(|o| o.open) {
            let mut rows_to_draw: Vec<(bool, String, egui::Color32)> = Vec::new();
            let risk = ov.risk();
            let risk_color = match risk.level {
                RiskLevel::Safe => active_gray,
                RiskLevel::Warn => theme.warning.to_color32(),
                RiskLevel::Dangerous => theme.danger.to_color32(),
            };
            if let Some(preview) = ov.preview() {
                let tag = match risk.level {
                    RiskLevel::Safe => "",
                    RiskLevel::Warn => "  [WARN]",
                    RiskLevel::Dangerous => "  [DANGER]",
                };
                rows_to_draw.push((true, format!("{preview}{tag}"), risk_color));
            }
            if let Some(tpl) = &ov.template {
                if let Some(slot) = tpl.active_slot() {
                    let val = if slot.value.is_empty() {
                        "_"
                    } else {
                        &slot.value
                    };
                    rows_to_draw.push((
                        false,
                        format!(
                            "slot [{}/{}] {{{{{}}}}} = \"{}\"  (Tab next · Enter run · Esc)",
                            tpl.active + 1,
                            tpl.slots.len(),
                            slot.name,
                            val
                        ),
                        primary_gray,
                    ));
                }
            }
            for reason in safety::explain(&risk).into_iter().take(2) {
                rows_to_draw.push((false, format!("! {reason}"), risk_color));
            }
            if let Some(err) = &ov.error {
                rows_to_draw.push((false, format!("! {err}  (Esc)"), theme.danger.to_color32()));
            } else if rows_to_draw.len() == 1 {
                rows_to_draw.push((false, "Enter confirm · Esc cancel".to_string(), note_gray));
            }

            let max_rows = rows_to_draw.len().min(4) as u16;
            let draw_above = space_below < max_rows && first_cmd_row >= max_rows;
            let visible = if draw_above {
                max_rows
            } else {
                max_rows.min(space_below.max(1))
            } as usize;
            let selected_bg = lerp_color(selection, default_bg, 65);

            for (idx, (marked, line_text, color)) in
                rows_to_draw.into_iter().take(visible).enumerate()
            {
                let target_row = if draw_above {
                    first_cmd_row.saturating_sub(visible as u16) + idx as u16
                } else {
                    cursor_row + 1 + idx as u16
                };
                if target_row >= snapshot.rows {
                    continue;
                }
                let text_col = if marked && start_col < 2 {
                    2
                } else {
                    start_col
                };
                let bg_from = if marked {
                    text_col.saturating_sub(2)
                } else {
                    text_col
                };
                let extra = if marked { 3 } else { 0 };
                let row_rect =
                    clear_cols_rect(target_row, bg_from, line_text.chars().count() + extra);
                painter.rect_filled(row_rect, 0.0, if idx == 0 { selected_bg } else { clear_bg });
                let stripe = egui::Rect::from_min_max(
                    row_rect.left_top(),
                    egui::pos2(row_rect.left() + 3.0, row_rect.bottom()),
                );
                painter.rect_filled(stripe, 0.0, risk_color);
                if marked && text_col >= 2 {
                    draw_inline_cells(
                        &painter,
                        origin,
                        cell_size,
                        &font_id,
                        text_col - 2,
                        snapshot.cols,
                        target_row,
                        "> ",
                        color,
                    );
                }
                draw_inline_cells(
                    &painter,
                    origin,
                    cell_size,
                    &font_id,
                    text_col,
                    snapshot.cols,
                    target_row,
                    &line_text,
                    color,
                );
            }
        } else if !live_hints.is_empty() {
            let shown = live_hints.len().min(8);
            let mut row_counts: Vec<u16> = Vec::with_capacity(shown);
            for cand in live_hints.iter().take(shown) {
                let tag_len = match cand.source {
                    crate::contracts::CandidateSource::Llm => 5,
                    crate::contracts::CandidateSource::Cache => 8,
                    crate::contracts::CandidateSource::History => 7,
                    _ => 0,
                };
                let chars = cand.rendered.chars().count()
                    + cand.note.as_ref().map_or(0, |n| n.chars().count() + 4)
                    + tag_len;
                row_counts.push(wrapped_rows(chars, start_col, start_col, snapshot.cols));
            }
            let total: u16 = row_counts.iter().sum();
            let draw_above = space_below < total && first_cmd_row >= total;
            let base_row = if draw_above {
                first_cmd_row - total
            } else {
                cursor_row.saturating_add(1)
            };
            let budget = if draw_above {
                total
            } else {
                total.min(space_below.max(1))
            };
            let row_limit = base_row.saturating_add(budget).min(snapshot.rows);

            let click_pos = ui.input(|i| {
                if i.pointer.primary_clicked() {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            });
            let selected_bg = lerp_color(selection, default_bg, 65);

            let mut row_offset = 0u16;
            for (idx, cand) in live_hints.iter().take(shown).enumerate() {
                let need = row_counts[idx];
                if row_offset + need > budget {
                    break;
                }
                let target_row = base_row + row_offset;
                row_offset += need;

                let is_selected = live_hint_selected == Some(idx);
                let text_col = if is_selected && start_col < 2 {
                    2
                } else {
                    start_col
                };
                let bg_from = if is_selected {
                    text_col.saturating_sub(2)
                } else {
                    text_col
                };
                let source_tag = match cand.source {
                    crate::contracts::CandidateSource::Llm => Some("[AI]"),
                    crate::contracts::CandidateSource::Cache => Some("[cache]"),
                    crate::contracts::CandidateSource::History => Some("[hist]"),
                    _ => None,
                };
                for r in 0..need {
                    let row_rect =
                        clear_cols_rect(target_row + r, bg_from, usize::from(snapshot.cols));
                    painter.rect_filled(
                        row_rect,
                        0.0,
                        if is_selected { selected_bg } else { clear_bg },
                    );
                    if let Some(pos) = click_pos {
                        if row_rect.contains(pos) {
                            clicked_hint = Some(idx);
                        }
                    }
                }

                let cmd_color = if is_selected {
                    active_gray
                } else if idx == 0 {
                    primary_gray
                } else {
                    secondary_gray
                };
                if is_selected && text_col >= 2 {
                    draw_inline_cells(
                        &painter,
                        origin,
                        cell_size,
                        &font_id,
                        text_col - 2,
                        snapshot.cols,
                        target_row,
                        "> ",
                        cmd_color,
                    );
                }
                let mut pos = draw_inline_wrapped(
                    &painter,
                    origin,
                    cell_size,
                    &font_id,
                    text_col,
                    snapshot.cols,
                    row_limit,
                    target_row,
                    text_col,
                    &cand.rendered,
                    cmd_color,
                );
                if let Some(note) = &cand.note {
                    pos = draw_inline_wrapped(
                        &painter,
                        origin,
                        cell_size,
                        &font_id,
                        text_col,
                        snapshot.cols,
                        row_limit,
                        pos.0,
                        pos.1,
                        &format!("  # {note}"),
                        note_gray,
                    );
                }
                if let Some(tag) = source_tag {
                    let tag_color = match cand.source {
                        crate::contracts::CandidateSource::Llm => theme.accent.to_color32(),
                        _ => note_gray,
                    };
                    draw_inline_wrapped(
                        &painter,
                        origin,
                        cell_size,
                        &font_id,
                        text_col,
                        snapshot.cols,
                        row_limit,
                        pos.0,
                        pos.1,
                        &format!("  {tag}"),
                        tag_color,
                    );
                }
            }
        }
    }

    if snapshot.scrollback_offset > 0 {
        let track_top = rect.top() + 8.0;
        let track_bot = (rect.bottom() - 8.0).max(track_top + 28.0);
        let track_h = track_bot - track_top;
        let total_lines = (snapshot.scrollback_total + usize::from(snapshot.rows)).max(1) as f32;
        let thumb_h =
            (track_h * (f32::from(snapshot.rows) / total_lines)).clamp(22.0, track_h * 0.75);
        let max_off = snapshot.scrollback_total.max(1) as f32;
        let frac_from_top = (1.0 - (snapshot.scrollback_offset as f32 / max_off)).clamp(0.0, 1.0);
        let thumb_top = track_top + (track_h - thumb_h) * frac_from_top;
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 4.0, thumb_top),
            egui::pos2(rect.right() - 1.0, thumb_top + thumb_h),
        );
        painter.rect_filled(bar, 1.5, theme.muted_fg(58));
    }

    GridMetrics {
        viewport: rect,
        cursor_rect,
        cell_size,
        clicked_hint,
    }
}

/// Pixel rect of one cell, used for cursor placement and hit-testing.
pub fn cell_rect(origin: egui::Pos2, cell_size: egui::Vec2, col: u16, row: u16) -> egui::Rect {
    let min = egui::pos2(
        origin.x + col as f32 * cell_size.x,
        origin.y + row as f32 * cell_size.y,
    );
    egui::Rect::from_min_size(min, cell_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{Attrs, Cell, Color};

    fn make_snap(lines: &[&str], cols: u16) -> GridSnapshot {
        let mut cells = vec![
            Cell {
                ch: ' ',
                fg: Color::Default,
                bg: Color::Default,
                attrs: Attrs::default(),
            };
            lines.len() * usize::from(cols)
        ];
        for (r, line) in lines.iter().enumerate() {
            for (c, ch) in line.chars().take(usize::from(cols)).enumerate() {
                cells[r * usize::from(cols) + c].ch = ch;
            }
        }
        GridSnapshot {
            cols,
            rows: lines.len() as u16,
            cells,
            cursor: (0, 0),
            title: None,
            alternate_screen: false,
            scrollback_offset: 0,
            scrollback_total: 0,
            mouse_reporting: false,
            ..Default::default()
        }
    }

    #[test]
    fn extracts_selection_and_detects_urls() {
        let snap = make_snap(&["see (https://example.com/docs).", "next line   "], 36);
        let text = extract_selected_text(&snap, (3, 1), (0, 0));
        assert_eq!(text, "see (https://example.com/docs).\nnext");

        let hit = link_at(&snap, 10, 0, std::path::Path::new("/tmp")).map(|(_, _, u)| u);
        assert_eq!(hit.as_deref(), Some("https://example.com/docs"));
        assert_eq!(link_at(&snap, 1, 0, std::path::Path::new("/tmp")), None);

        // Smooth row span stops at trimmed text end rather than highlighting trailing empty cells.
        let r0 = row_selection_rect(
            &snap,
            egui::Pos2::ZERO,
            egui::vec2(10.0, 20.0),
            1,
            Some(((0, 0), (35, 1))),
        )
        .expect("selected row");
        assert_eq!(r0.left(), 0.0);
        assert_eq!(r0.right(), 90.0);
    }
}
