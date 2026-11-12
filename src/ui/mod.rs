//! egui rendering layer. Only this module and `app.rs` may name egui types.

pub mod grid;
pub mod overlay;

use crate::app::LexicueApp;
use crate::config::{LlmProviderKind, PromptIcon, UiLanguage};
use crate::contracts::KeyInput;
use crate::theme::{Rgb, THEME_PRESETS};
use eframe::egui;

struct UiText {
    settings: &'static str,
    cwd_tooltip: &'static str,
    branch_tooltip: &'static str,
    pin_tooltip: &'static str,
    unpin_tooltip: &'static str,
    collapse_tooltip: &'static str,
    btn_interrupt: &'static str,
    btn_interrupt_tip: &'static str,
    btn_clear: &'static str,
    btn_clear_tip: &'static str,
    btn_copy: &'static str,
    btn_copy_tip: &'static str,
    btn_paste: &'static str,
    btn_paste_tip: &'static str,
    toast_copied_path: &'static str,
    toast_copied_branch: &'static str,
    toast_copied: &'static str,
    toast_nothing_to_copy: &'static str,
    toast_opened_folder: &'static str,
    toast_open_folder_err: &'static str,
    ports_label: &'static str,
    tab_appearance: &'static str,
    tab_assistant: &'static str,
    tab_shortcuts: &'static str,
    close_esc: &'static str,
    theme_presets: &'static str,
    reset_theme: &'static str,
    fine_tune_colors: &'static str,
    typography_and_lang: &'static str,
    language_label: &'static str,
    font_size: &'static str,
    scrollback_lines: &'static str,
    prompt_icon: &'static str,
    color_bg: &'static str,
    color_fg: &'static str,
    color_accent: &'static str,
    color_overlay: &'static str,
    color_selection: &'static str,
    color_warning: &'static str,
    color_danger: &'static str,
    suggestions_hdr: &'static str,
    max_inline_hints: &'static str,
    history_weight: &'static str,
    shortcuts_hdr: &'static str,
    sc_up_down: &'static str,
    sc_alt_up_down: &'static str,
    sc_right_click: &'static str,
    sc_qq_text: &'static str,
    sc_alt_f: &'static str,
    sc_copy: &'static str,
    sc_scroll: &'static str,
    sc_ctrl_click: &'static str,
    assistant_safety_hdr: &'static str,
    llm_fallback: &'static str,
    enabled: &'static str,
    disabled: &'static str,
    provider: &'static str,
    endpoint: &'static str,
    model: &'static str,
    api_key: &'static str,
    show: &'static str,
    hide: &'static str,
    test_btn: &'static str,
    prompt_file: &'static str,
    instructions: &'static str,
    instructions_hint: &'static str,
    timeout: &'static str,
    confirm_dangerous: &'static str,
    required: &'static str,
    bypassed: &'static str,
    clear_cache: &'static str,
    reset: &'static str,
    save: &'static str,
    close: &'static str,
    saved: &'static str,
    unsaved_changes: &'static str,
    #[allow(dead_code)] // Ключ есть в обеих локалях, подпись пока не выводится.
    default_config: &'static str,
    launcher_placeholder: &'static str,
    launcher_hints: &'static str,
}

const EN_TEXT: UiText = UiText {
    settings: "Settings",
    cwd_tooltip: "Click: open folder · Right-click: copy path",
    branch_tooltip: "Copy git branch",
    pin_tooltip: "Pin top bar",
    unpin_tooltip: "Unpin top bar",
    collapse_tooltip: "Collapse",
    btn_interrupt: "Interrupt",
    btn_interrupt_tip: "Send Ctrl+C interrupt",
    btn_clear: "Clear",
    btn_clear_tip: "Clear screen (Ctrl+L)",
    btn_copy: "Copy",
    btn_copy_tip: "Copy selection or last output",
    btn_paste: "Paste",
    btn_paste_tip: "Paste from clipboard",
    toast_copied_path: "Copied path",
    toast_copied_branch: "Copied branch",
    toast_copied: "Copied",
    toast_nothing_to_copy: "Nothing to copy",
    toast_opened_folder: "Opened folder",
    toast_open_folder_err: "Failed to open folder",
    ports_label: "PORTS",
    tab_appearance: "Appearance",
    tab_assistant: "Assistant",
    tab_shortcuts: "Shortcuts",
    close_esc: "Close (Esc)",
    theme_presets: "THEME PRESETS",
    reset_theme: "Reset theme",
    fine_tune_colors: "FINE-TUNE COLORS",
    typography_and_lang: "TYPOGRAPHY, BUFFER & LANGUAGE",
    language_label: "Language",
    font_size: "Font size",
    scrollback_lines: "Scrollback lines",
    prompt_icon: "Prompt icon",
    color_bg: "Background",
    color_fg: "Foreground",
    color_accent: "Accent",
    color_overlay: "Overlay",
    color_selection: "Selection",
    color_warning: "Warning",
    color_danger: "Danger",
    suggestions_hdr: "SUGGESTIONS",
    max_inline_hints: "Max inline hints",
    history_weight: "History priority (boost past commands)",
    shortcuts_hdr: "SHORTCUTS",
    sc_up_down: "Shell history, or step into hints",
    sc_alt_up_down: "Navigate hints without shell history",
    sc_right_click: "Fill selected hint into command line",
    sc_qq_text: "Ask AI / local command lookup",
    sc_alt_f: "Ask AI to fix last failed command",
    sc_copy: "Copy selected text",
    sc_scroll: "Scroll history · Esc returns",
    sc_ctrl_click: "Open URL or file path under cursor",
    assistant_safety_hdr: "ASSISTANT & SAFETY",
    llm_fallback: "LLM fallback",
    enabled: "Enabled",
    disabled: "Disabled",
    provider: "Provider",
    endpoint: "Endpoint",
    model: "Model",
    api_key: "API key",
    show: "Show",
    hide: "Hide",
    test_btn: "Test",
    prompt_file: "Prompt file",
    instructions: "Instructions",
    instructions_hint: "Built-in shell rules active (write here or set Prompt file to override)",
    timeout: "Timeout",
    confirm_dangerous: "Confirm dangerous",
    required: "Required",
    bypassed: "Bypassed",
    clear_cache: "Clear cache",
    reset: "Reset",
    save: "Save",
    close: "Close",
    saved: "Saved",
    unsaved_changes: "Changes not applied (press Save)",
    default_config: "default config",
    launcher_placeholder: "Type command or action…",
    launcher_hints: "Esc: close · Enter: fill · Ctrl+Enter: run",
};

const RU_TEXT: UiText = UiText {
    settings: "Настройки",
    cwd_tooltip: "ЛКМ: открыть папку · ПКМ: копировать путь",
    branch_tooltip: "Копировать ветку git",
    pin_tooltip: "Закрепить панель",
    unpin_tooltip: "Открепить панель",
    collapse_tooltip: "Свернуть",
    btn_interrupt: "Прервать",
    btn_interrupt_tip: "Отправить Ctrl+C",
    btn_clear: "Очистить",
    btn_clear_tip: "Очистить экран (Ctrl+L)",
    btn_copy: "Копировать",
    btn_copy_tip: "Копировать выделение или вывод команды",
    btn_paste: "Вставить",
    btn_paste_tip: "Вставить из буфера обмена",
    toast_copied_path: "Путь скопирован",
    toast_copied_branch: "Ветка скопирована",
    toast_copied: "Скопировано",
    toast_nothing_to_copy: "Нечего копировать",
    toast_opened_folder: "Папка открыта",
    toast_open_folder_err: "Не удалось открыть папку",
    ports_label: "ПОРТЫ",
    tab_appearance: "Внешний вид",
    tab_assistant: "Ассистент",
    tab_shortcuts: "Горячие клавиши",
    close_esc: "Закрыть (Esc)",
    theme_presets: "ТЕМЫ",
    reset_theme: "Сбросить тему",
    fine_tune_colors: "ЦВЕТА",
    typography_and_lang: "ШРИФТ, БУФЕР И ЯЗЫК",
    language_label: "Язык",
    font_size: "Размер шрифта",
    scrollback_lines: "Строк истории",
    prompt_icon: "Иконка промпта",
    color_bg: "Фон",
    color_fg: "Текст",
    color_accent: "Акцент",
    color_overlay: "Панель",
    color_selection: "Выделение",
    color_warning: "Предупреждение",
    color_danger: "Опасность",
    suggestions_hdr: "ПОДСКАЗКИ",
    max_inline_hints: "Макс. подсказок",
    history_weight: "Приоритет истории (чаще в подсказках)",
    shortcuts_hdr: "ГОРЯЧИЕ КЛАВИШИ",
    sc_up_down: "История shell или переход к подсказкам",
    sc_alt_up_down: "Навигация по подсказкам без истории",
    sc_right_click: "Вставить выбранную подсказку",
    sc_qq_text: "Спросить ИИ / локальный поиск команды",
    sc_alt_f: "Исправить последнюю ошибку через ИИ",
    sc_copy: "Копировать выделенный текст",
    sc_scroll: "Прокрутка истории · Esc — назад",
    sc_ctrl_click: "Открыть ссылку или путь под курсором",
    assistant_safety_hdr: "АССИСТЕНТ И БЕЗОПАСНОСТЬ",
    llm_fallback: "ИИ-подбор (LLM)",
    enabled: "Вкл.",
    disabled: "Выкл.",
    provider: "Провайдер",
    endpoint: "Адрес (Endpoint)",
    model: "Модель",
    api_key: "API-ключ",
    show: "Показать",
    hide: "Скрыть",
    test_btn: "Тест",
    prompt_file: "Файл промпта",
    instructions: "Инструкции",
    instructions_hint: "Активны встроенные правила (введите свои или укажите файл)",
    timeout: "Таймаут",
    confirm_dangerous: "Подтверждать опасные",
    required: "Требуется",
    bypassed: "Откл.",
    clear_cache: "Очистить кэш",
    reset: "Сбросить",
    save: "Сохранить",
    close: "Закрыть",
    saved: "Сохранено",
    unsaved_changes: "Изменения не применены (нажмите Сохранить)",
    default_config: "конфиг по умолчанию",
    launcher_placeholder: "Введите команду или действие…",
    launcher_hints: "Esc: закрыть · Enter: вставить · Ctrl+Enter: запуск",
};

fn ui_text(lang: UiLanguage) -> &'static UiText {
    match lang {
        UiLanguage::En => &EN_TEXT,
        UiLanguage::Ru => &RU_TEXT,
    }
}

/// Root painter: applies theme visuals, draws the terminal grid, then the overlay.
/// Overlay is skipped entirely while [`crate::pty::guard::PassthroughGuard`] says
/// `Passthrough`.
fn format_cwd(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            let s = rel.display().to_string();
            return if s.is_empty() {
                "~".to_string()
            } else {
                format!("~/{s}")
            };
        }
    }
    path.display().to_string()
}

#[derive(Clone, Copy)]
enum VectorIcon {
    CloseCross,
    Pin,
    ChevronUp,
}

fn flat_button(
    ui: &mut egui::Ui,
    label: &str,
    fg: egui::Color32,
    hover_fill: egui::Color32,
    hairline: egui::Color32,
) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        w.inactive.bg_fill = egui::Color32::TRANSPARENT;
        w.inactive.bg_stroke = egui::Stroke::NONE;
        w.hovered.weak_bg_fill = hover_fill;
        w.hovered.bg_fill = hover_fill;
        w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, hairline);
        w.active.weak_bg_fill = hover_fill;
        w.active.bg_fill = hover_fill;
        w.active.bg_stroke = egui::Stroke::new(1.0_f32, hairline);
        ui.add(
            egui::Button::new(egui::RichText::new(label).size(12.5).color(fg))
                .rounding(egui::Rounding::same(6.0))
                .min_size(egui::vec2(28.0, 24.0)),
        )
    })
    .inner
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: VectorIcon,
    fg: egui::Color32,
    surface: egui::Color32,
    hover_fill: egui::Color32,
    hairline: egui::Color32,
    bordered: bool,
) -> egui::Response {
    let size = egui::vec2(26.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let bg = if resp.hovered() || resp.has_focus() {
        hover_fill
    } else if bordered {
        surface
    } else {
        egui::Color32::TRANSPARENT
    };
    let stroke = if bordered || resp.hovered() || resp.has_focus() {
        egui::Stroke::new(1.0_f32, hairline)
    } else {
        egui::Stroke::NONE
    };
    ui.painter()
        .rect(rect, egui::Rounding::same(6.0), bg, stroke);

    let c = rect.center();
    let icon_stroke = egui::Stroke::new(1.6_f32, fg);
    match icon {
        VectorIcon::CloseCross => {
            let d = 4.2;
            ui.painter().line_segment(
                [egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)],
                icon_stroke,
            );
            ui.painter().line_segment(
                [egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)],
                icon_stroke,
            );
        }
        VectorIcon::Pin => {
            ui.painter().line_segment(
                [
                    egui::pos2(c.x - 3.0, c.y - 4.0),
                    egui::pos2(c.x + 3.0, c.y - 4.0),
                ],
                icon_stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(c.x - 2.0, c.y - 4.0),
                    egui::pos2(c.x - 2.5, c.y + 1.0),
                ],
                icon_stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(c.x + 2.0, c.y - 4.0),
                    egui::pos2(c.x + 2.5, c.y + 1.0),
                ],
                icon_stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(c.x - 4.5, c.y + 1.0),
                    egui::pos2(c.x + 4.5, c.y + 1.0),
                ],
                icon_stroke,
            );
            ui.painter().line_segment(
                [egui::pos2(c.x, c.y + 1.0), egui::pos2(c.x, c.y + 5.0)],
                icon_stroke,
            );
        }
        VectorIcon::ChevronUp => {
            ui.painter().line_segment(
                [egui::pos2(c.x - 4.5, c.y + 2.0), egui::pos2(c.x, c.y - 2.5)],
                icon_stroke,
            );
            ui.painter().line_segment(
                [egui::pos2(c.x, c.y - 2.5), egui::pos2(c.x + 4.5, c.y + 2.0)],
                icon_stroke,
            );
        }
    }
    resp
}

fn choice_pill(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    fg: egui::Color32,
    accent: egui::Color32,
    surface: egui::Color32,
    selection: egui::Color32,
    hairline: egui::Color32,
) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).size(12.0).color(if active {
            accent
        } else {
            fg
        }))
        .fill(if active { selection } else { surface })
        .stroke(egui::Stroke::new(
            1.0_f32,
            if active { accent } else { hairline },
        ))
        .rounding(egui::Rounding::same(6.0))
        .min_size(egui::vec2(34.0, 25.0)),
    )
}

fn settings_card(
    ui: &mut egui::Ui,
    surface: egui::Color32,
    hairline: egui::Color32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::none()
        .fill(surface)
        .stroke(egui::Stroke::new(1.0_f32, hairline))
        .rounding(egui::Rounding::same(9.0))
        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 9.0);
            add_contents(ui);
        });
}

fn keycap_pill(
    ui: &mut egui::Ui,
    keys: &str,
    overlay_bg: egui::Color32,
    hairline: egui::Color32,
    accent: egui::Color32,
) {
    egui::Frame::none()
        .fill(overlay_bg)
        .stroke(egui::Stroke::new(1.0_f32, hairline))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::symmetric(7.0, 2.5))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(keys).size(11.0).color(accent).strong());
        });
}

/// Live sRGB swatch + `#rrggbb` input for fine-tuning one theme color role.
fn color_role_row(
    ui: &mut egui::Ui,
    label: &str,
    hex: &mut String,
    fallback: Rgb,
    fg: egui::Color32,
    muted: egui::Color32,
) -> bool {
    let mut changed = false;
    let parsed = Rgb::from_hex(hex).unwrap_or(fallback);
    let mut srgb = [parsed.r, parsed.g, parsed.b];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if ui.color_edit_button_srgb(&mut srgb).changed() {
            *hex = Rgb {
                r: srgb[0],
                g: srgb[1],
                b: srgb[2],
            }
            .to_hex();
            changed = true;
        }
        ui.label(egui::RichText::new(label).size(12.5).color(fg));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(hex)
                    .hint_text(egui::RichText::new(fallback.to_hex()).color(muted))
                    .desired_width(68.0),
            );
            if edit.changed() && Rgb::from_hex(hex).is_ok() {
                changed = true;
            }
        });
    });
    changed
}

pub fn truncate_text(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn paint_progress_bar(
    ui: &egui::Ui,
    ctx: &egui::Context,
    track: egui::Rect,
    maybe_pct: Option<u8>,
    hairline: egui::Color32,
    accent: egui::Color32,
) {
    let radius = egui::Rounding::same(track.height() * 0.5);
    ui.painter().rect_filled(track, radius, hairline);
    match maybe_pct {
        Some(p) => {
            let frac = (f32::from(p) / 100.0).clamp(0.0, 1.0);
            let fill_w = (track.width() * frac).max(if p > 0 { 4.5 } else { 0.0 });
            let fill = egui::Rect::from_min_size(track.min, egui::vec2(fill_w, track.height()));
            ui.painter().rect_filled(fill, radius, accent);
        }
        None => {
            let t = (ctx.input(|i| i.time) * 1.4).fract() as f32;
            let seg_w = (track.width() * 0.34).max(14.0);
            let travel = (track.width() - seg_w).max(0.0);
            let fill = egui::Rect::from_min_size(
                egui::pos2(track.left() + travel * t, track.top()),
                egui::vec2(seg_w, track.height()),
            );
            ui.painter().rect_filled(fill, radius, accent);
            ctx.request_repaint_after(std::time::Duration::from_millis(45));
        }
    }
}

fn format_transfer_label(pct: Option<u8>, detail: Option<&str>) -> String {
    match (pct, detail) {
        (Some(p), Some(d)) => format!("{p}% · {}", truncate_text(d, 14)),
        (Some(p), None) => format!("{p}%"),
        (None, Some(d)) => truncate_text(d, 16),
        (None, None) => "Working…".to_string(),
    }
}

pub fn draw(app: &mut LexicueApp, ctx: &egui::Context) {
    let bg = app.theme.background.to_color32();
    let fg = app.theme.foreground.to_color32();
    let accent = app.theme.accent.to_color32();
    let selection = app.theme.selection.to_color32();
    let overlay_bg = app.theme.overlay_bg.to_color32();
    let surface = app.theme.surface();
    let hairline = app.theme.hairline();
    let muted = app.theme.muted_fg(68);
    let hover_gray =
        crate::theme::ThemePalette::blend(app.theme.foreground, app.theme.overlay_bg, 22);
    let warn = app.theme.warning.to_color32();
    let danger = app.theme.danger.to_color32();

    let mut visuals = if app.theme.is_light() {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    visuals.panel_fill = bg;
    visuals.window_fill = overlay_bg;
    visuals.widgets.noninteractive.bg_fill = overlay_bg;
    visuals.widgets.noninteractive.fg_stroke.color = fg;
    visuals.widgets.inactive.bg_fill = surface;
    visuals.widgets.inactive.fg_stroke.color = fg;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, hairline);
    visuals.widgets.hovered.bg_fill = hover_gray;
    visuals.widgets.hovered.fg_stroke.color = fg;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, accent);
    visuals.widgets.active.bg_fill = hover_gray;
    visuals.widgets.active.fg_stroke.color = fg;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.2_f32, accent);
    visuals.selection.bg_fill = selection;
    visuals.window_rounding = egui::Rounding::same(10.0);
    visuals.window_stroke = egui::Stroke::new(1.0_f32, hairline);
    ctx.set_visuals(visuals);

    let passthrough = app.is_passthrough();
    let state_text = app.terminal_state_label().map(|s| truncate_text(&s, 26));
    let git_branch = app.git_branch().map(ToOwned::to_owned);
    let transfer = app.active_transfer().cloned();
    let active_toast = app.active_toast().map(ToOwned::to_owned);

    let loading_text = if passthrough {
        None
    } else {
        app.loading_text()
    };

    let has_env_or_ports = !app.env_chips().is_empty() || !app.active_ports().is_empty();
    let screen_rect = ctx.screen_rect();
    let top_pad = if app.top_island_open && app.top_island_pinned {
        if has_env_or_ports {
            96.0
        } else {
            76.0
        }
    } else {
        14.0
    };
    let frame = egui::Frame::none().fill(bg).inner_margin(egui::Margin {
        left: 14.0,
        right: 14.0,
        top: top_pad,
        bottom: 10.0,
    });
    let mut clicked_live_hint = None;
    let mut clicked_link: Option<String> = None;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let cell = grid::cell_size(ui, &app.theme);
        let avail_rect = ui.available_rect_before_wrap();
        let (cols, rows) = grid::grid_dimensions(avail_rect.size(), cell);
        app.resize_if_needed(cols, rows, cell);

        let mut hovered_link = None;
        let mut pointer_over_island = false;
        if app.top_island_open {
            let island_h = if has_env_or_ports { 88.0 } else { 66.0 };
            let island_w = (screen_rect.width() - 32.0).clamp(320.0, 620.0);
            let island_rect = egui::Rect::from_min_size(
                egui::pos2(
                    screen_rect.center().x - island_w * 0.5,
                    screen_rect.top() + 9.0,
                ),
                egui::vec2(island_w, island_h),
            );
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                if island_rect.contains(pos) {
                    pointer_over_island = true;
                }
            }
        }
        if !app.settings_open
            && (!app.top_island_open || app.top_island_pinned)
            && !pointer_over_island
        {
            let (
                pointer_pos,
                primary_pressed,
                primary_down,
                primary_released,
                primary_clicked,
                middle_clicked,
                middle_down,
                secondary_down,
                ctrl_down,
                scroll_y,
                pointer_events,
            ) = ctx.input(|i| {
                (
                    i.pointer.interact_pos(),
                    i.pointer.primary_pressed(),
                    i.pointer.primary_down(),
                    i.pointer.primary_released(),
                    i.pointer.primary_clicked(),
                    i.pointer.button_clicked(egui::PointerButton::Middle),
                    i.pointer.button_down(egui::PointerButton::Middle),
                    i.pointer.secondary_down(),
                    i.modifiers.ctrl || i.modifiers.command,
                    i.raw_scroll_delta.y,
                    i.events.clone(),
                )
            });

            if !app.snapshot.alternate_screen
                && !app.snapshot.mouse_reporting
                && scroll_y.abs() > 1.0
                && pointer_pos.is_some_and(|p| avail_rect.contains(p))
            {
                let step = (scroll_y / 14.0).round() as isize;
                let delta = if scroll_y > 0.0 {
                    step.max(1)
                } else {
                    step.min(-1)
                };
                app.scroll_by(delta);
            }

            if let Some(pos) = pointer_pos {
                let col = ((pos.x - avail_rect.min.x) / cell.x.max(1.0))
                    .floor()
                    .clamp(0.0, cols.saturating_sub(1) as f32) as u16;
                let row = ((pos.y - avail_rect.min.y) / cell.y.max(1.0))
                    .floor()
                    .clamp(0.0, rows.saturating_sub(1) as f32) as u16;

                if ctrl_down && avail_rect.contains(pos) {
                    if let Some((start_c, end_c, target)) =
                        grid::link_at(&app.snapshot, col, row, app.cwd())
                    {
                        hovered_link = Some((row, start_c, end_c));
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        if primary_clicked {
                            clicked_link = Some(target);
                        }
                    }
                } else if app.snapshot.mouse_reporting {
                    let encoding = app.snapshot.mouse_encoding;
                    let mode = app.snapshot.mouse_mode;
                    for ev in pointer_events {
                        if let egui::Event::PointerButton {
                            pos: ev_pos,
                            button,
                            pressed,
                            ..
                        } = ev
                        {
                            if !avail_rect.contains(ev_pos) {
                                continue;
                            }
                            if !pressed && mode == crate::pty::MouseMode::Press {
                                continue;
                            }
                            let btn = match button {
                                egui::PointerButton::Primary => 0,
                                egui::PointerButton::Middle => 1,
                                egui::PointerButton::Secondary => 2,
                                _ => continue,
                            };
                            let c = ((ev_pos.x - avail_rect.min.x) / cell.x.max(1.0))
                                .floor()
                                .clamp(0.0, cols.saturating_sub(1) as f32)
                                as u16;
                            let r = ((ev_pos.y - avail_rect.min.y) / cell.y.max(1.0))
                                .floor()
                                .clamp(0.0, rows.saturating_sub(1) as f32)
                                as u16;
                            let bytes =
                                crate::pty::encode_mouse_event(encoding, btn, c, r, pressed);
                            if !bytes.is_empty() {
                                app.write_pty_bytes(&bytes);
                            }
                            app.last_mouse_cell = Some((c, r));
                        }
                    }
                    if scroll_y.abs() > 1.0 && avail_rect.contains(pos) {
                        let btn = if scroll_y > 0.0 { 64 } else { 65 };
                        let bytes = crate::pty::encode_mouse_event(encoding, btn, col, row, true);
                        if !bytes.is_empty() {
                            app.write_pty_bytes(&bytes);
                        }
                    }
                    let any_down = primary_down || middle_down || secondary_down;
                    let track_motion = match mode {
                        crate::pty::MouseMode::AnyMotion => true,
                        crate::pty::MouseMode::ButtonMotion => any_down,
                        _ => false,
                    };
                    if track_motion
                        && avail_rect.contains(pos)
                        && app.last_mouse_cell != Some((col, row))
                    {
                        app.last_mouse_cell = Some((col, row));
                        let btn = if primary_down {
                            32
                        } else if middle_down {
                            33
                        } else if secondary_down {
                            34
                        } else {
                            35
                        };
                        let bytes = crate::pty::encode_mouse_event(encoding, btn, col, row, true);
                        if !bytes.is_empty() {
                            app.write_pty_bytes(&bytes);
                        }
                    }
                } else {
                    if middle_clicked && avail_rect.contains(pos) {
                        if let Some(text) = app.primary_selection.clone() {
                            if let crate::contracts::UiIntent::PassToPty(bytes) =
                                app.on_key(crate::contracts::KeyInput::Paste(text))
                            {
                                app.write_pty_bytes(&bytes);
                            }
                        }
                    }
                    if primary_pressed && avail_rect.contains(pos) {
                        let dist = app.view_row_to_dist(row);
                        app.selection_anchor = Some((col, dist));
                        app.selection_end = Some((col, dist));
                    } else if primary_down && app.selection_anchor.is_some() {
                        if !app.snapshot.alternate_screen
                            && (pos.y < avail_rect.top() || pos.y > avail_rect.bottom())
                        {
                            let ready = app.last_drag_scroll.is_none_or(|t| {
                                t.elapsed() >= std::time::Duration::from_millis(42)
                            });
                            if ready {
                                let step = if pos.y < avail_rect.top() {
                                    if avail_rect.top() - pos.y > cell.y * 2.0 {
                                        2
                                    } else {
                                        1
                                    }
                                } else if pos.y - avail_rect.bottom() > cell.y * 2.0 {
                                    -2
                                } else {
                                    -1
                                };
                                app.scroll_by(step);
                                app.last_drag_scroll = Some(std::time::Instant::now());
                            }
                            ctx.request_repaint_after(std::time::Duration::from_millis(32));
                        }
                        let end_row = if pos.y < avail_rect.top() {
                            0
                        } else if pos.y > avail_rect.bottom() {
                            rows.saturating_sub(1)
                        } else {
                            row
                        };
                        app.selection_end = Some((col, app.view_row_to_dist(end_row)));
                    } else if primary_released && app.selection_anchor.is_some() {
                        let sel = app.extract_logical_selection();
                        if !sel.is_empty() {
                            app.primary_selection = Some(sel);
                        }
                    }
                }
            }
        }

        let ghost = app.ghost_suffix().map(ToOwned::to_owned);
        let inline_hints: &[crate::contracts::Candidate] =
            if !passthrough && !app.overlay.open && loading_text.is_none() {
                &app.live_hints
            } else {
                &[]
            };
        let inline_overlay = if !passthrough && app.overlay.open {
            Some(&app.overlay)
        } else {
            None
        };
        let metrics = grid::draw(
            ui,
            &app.snapshot,
            &app.theme,
            ghost.as_deref(),
            inline_hints,
            app.live_hint_selected,
            loading_text.as_deref(),
            inline_overlay,
            app.command_line_count(),
            app.command_start_col(),
            !passthrough && !app.snapshot.alternate_screen && app.snapshot.scrollback_offset == 0,
            app.active_selection(),
            hovered_link,
            &app.visible_search_hits(),
        );
        clicked_live_hint = metrics.clicked_hint;
    });

    if let Some(target) = clicked_link {
        app.pending_island_actions
            .push(crate::contracts::IslandAction::OpenLink(target));
    }

    if app.search_open {
        let search_top = if app.top_island_open { 84.0 } else { 22.0 };
        let is_ru = app.cfg.general.language == crate::config::UiLanguage::Ru;
        egui::Area::new(egui::Id::new("lexicue_search_bar"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, search_top))
            .show(ctx, |ui| {
                let bg = if app.theme.is_light() {
                    egui::Color32::from_rgba_unmultiplied(245, 245, 248, 245)
                } else {
                    egui::Color32::from_rgba_unmultiplied(24, 24, 28, 245)
                };
                let stroke = if app.theme.is_light() {
                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 28)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28)
                };
                egui::Frame::none()
                    .fill(bg)
                    .stroke(egui::Stroke::new(1.0_f32, stroke))
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let hint = if is_ru { "Поиск..." } else { "Search..." };
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut app.search_query)
                                    .desired_width(170.0)
                                    .hint_text(hint),
                            );
                            if app.search_focus_requested {
                                resp.request_focus();
                                app.search_focus_requested = false;
                            }
                            if resp.changed() {
                                app.update_search_matches();
                            }
                            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let back = ui.input(|i| i.modifiers.shift);
                                app.step_search_match(!back);
                                resp.request_focus();
                            }
                            let count_label = if app.search_matches.is_empty() {
                                "0/0".to_owned()
                            } else {
                                format!("{}/{}", app.search_index + 1, app.search_matches.len())
                            };
                            ui.label(
                                egui::RichText::new(count_label)
                                    .size(11.0)
                                    .color(app.theme.foreground.to_color32().gamma_multiply(0.65)),
                            );
                            if ui.small_button("↑").clicked() {
                                app.step_search_match(false);
                            }
                            if ui.small_button("↓").clicked() {
                                app.step_search_match(true);
                            }
                            if ui.small_button("×").clicked() {
                                app.search_open = false;
                            }
                        });
                    });
            });
    }

    // Top Dynamic Island handle line + animated dropdown drawer inside the window.
    let screen_rect = ctx.screen_rect();
    if let Some(until) = app.bell_flash_until {
        let now = std::time::Instant::now();
        if now < until {
            let alpha = ((until - now).as_secs_f32() / 0.16).clamp(0.0, 1.0);
            let flash = app.theme.accent.to_color32().gamma_multiply(0.55 * alpha);
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("lexicue_bell_flash"),
            ))
            .rect_stroke(
                screen_rect.shrink(1.5),
                0.0,
                egui::Stroke::new(3.0_f32, flash),
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(24));
        } else {
            app.bell_flash_until = None;
        }
    }
    let t = ui_text(app.cfg.general.language);
    let anim_t = ctx.animate_bool_with_time(
        egui::Id::new("lexicue_top_island_anim"),
        app.top_island_open,
        0.18,
    );
    let ease = 1.0 - (1.0 - anim_t).powi(3);
    if anim_t > 0.01 && anim_t < 0.99 {
        ctx.request_repaint();
    }

    // Draw inline launcher if open
    if app.launcher.open && !passthrough {
        let launch_w = (screen_rect.width() - 40.0).clamp(320.0, 580.0);
        let launch_h = 28.0;
        let launch_rect = egui::Rect::from_center_size(
            egui::pos2(screen_rect.center().x, screen_rect.top() + 18.0),
            egui::vec2(launch_w, launch_h),
        );
        egui::Area::new(egui::Id::new("lexicue_launcher_bar"))
            .fixed_pos(launch_rect.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(overlay_bg)
                    .stroke(egui::Stroke::new(1.0_f32, accent))
                    .rounding(egui::Rounding::same(8.0))
                    .show(ui, |ui| {
                        ui.set_min_size(launch_rect.size());
                        ui.horizontal_centered(|ui| {
                            ui.add_space(10.0);
                            let query_display = if app.launcher.query.is_empty() {
                                egui::RichText::new(t.launcher_placeholder)
                                    .size(12.0)
                                    .color(muted)
                            } else {
                                egui::RichText::new(&app.launcher.query)
                                    .size(12.0)
                                    .color(fg)
                                    .strong()
                            };
                            ui.label(query_display);

                            if let Some(item) = app.launcher.items.get(app.launcher.selected) {
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new("→").size(11.0).color(muted));
                                ui.label(
                                    egui::RichText::new(truncate_text(&item.label, 22))
                                        .size(12.0)
                                        .color(accent),
                                );
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(t.launcher_hints)
                                            .size(9.5)
                                            .color(muted),
                                    );
                                },
                            );
                        });
                    });
            });
        return;
    }

    if !app.top_island_open && anim_t < 0.05 {
        let cmd_badge = app.command_badge();
        // A live transfer/build wins over the plain "Running" badge; the
        // finished/failed badges still win over a stale progress bar.
        let transfer_first = transfer.is_some()
            && matches!(
                cmd_badge.as_ref(),
                None | Some(crate::contracts::CommandBadge::Running { .. })
            );
        let active_pill: Option<(egui::Color32, String, Option<Option<u8>>)> =
            if let Some(tp) = transfer.as_ref().filter(|_| transfer_first) {
                Some((
                    accent,
                    format_transfer_label(tp.percent, tp.detail.as_deref()),
                    Some(tp.percent),
                ))
            } else if let Some(cmd_badge) = cmd_badge {
                Some(match cmd_badge {
                    crate::contracts::CommandBadge::Running { elapsed } => {
                        (accent, format!("Running {}s", elapsed.as_secs()), None)
                    }
                    crate::contracts::CommandBadge::Succeeded { duration } => {
                        (accent, format!("Done {}s", duration.as_secs()), None)
                    }
                    crate::contracts::CommandBadge::Failed {
                        duration,
                        exit_code,
                        fix_available,
                    } => {
                        let code_str = exit_code
                            .map(|c| format!("exit {c}"))
                            .unwrap_or_else(|| "fail".into());
                        let label = if fix_available {
                            format!("Failed {}s · {} · Fix", duration.as_secs(), code_str)
                        } else {
                            format!("Failed {}s · {}", duration.as_secs(), code_str)
                        };
                        (danger, label, None)
                    }
                })
            } else if let Some(txt) = loading_text.as_deref() {
                let clean = txt.trim_start_matches(|c: char| {
                    ('\u{2800}'..='\u{28ff}').contains(&c) || c == ' '
                });
                Some((accent, clean.to_string(), None))
            } else if let Some(port) = app.active_ports().first() {
                Some((accent, format!(":{}", port.port), None))
            } else if passthrough {
                let app_label = state_text
                    .as_deref()
                    .filter(|s| *s != "passthrough")
                    .map(|s| truncate_text(s, 18))
                    .unwrap_or_else(|| "RAW".to_string());
                Some((warn, app_label, None))
            } else {
                None
            };

        let tag_font = egui::FontId::proportional(10.5);
        let badge_target_w = if let Some((dot_color, ref tag, ref bar_pct)) = active_pill {
            let badge_text_w = ctx.fonts(|f| {
                f.layout_no_wrap(tag.clone(), tag_font.clone(), dot_color)
                    .size()
                    .x
            });
            let progress_w = if bar_pct.is_some() { 52.0 } else { 0.0 };
            (badge_text_w + progress_w + 26.0).clamp(74.0, (screen_rect.width() - 40.0).max(140.0))
        } else {
            54.0
        };
        let hit_w = badge_target_w.max(140.0).min(screen_rect.width());
        let hit_h = 22.0;
        let hit_rect = egui::Rect::from_min_size(
            egui::pos2(screen_rect.center().x - hit_w * 0.5, screen_rect.top()),
            egui::vec2(hit_w, hit_h),
        );
        egui::Area::new(egui::Id::new("lexicue_island_handle"))
            .fixed_pos(hit_rect.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(hit_rect.size(), egui::Sense::click());
                let hover_t = ctx.animate_bool_with_time(
                    egui::Id::new("lexicue_island_handle_hover"),
                    resp.hovered(),
                    0.14,
                );
                let hover_ease = 1.0 - (1.0 - hover_t).powi(2);
                if hover_t > 0.01 && hover_t < 0.99 {
                    ctx.request_repaint();
                }

                let badge_stroke = match app.island_tint() {
                    crate::contracts::RiskLevel::Dangerous => danger,
                    crate::contracts::RiskLevel::Warn => warn,
                    crate::contracts::RiskLevel::Safe => {
                        if hover_ease > 0.1 {
                            accent
                        } else {
                            hairline
                        }
                    }
                };

                let mut fix_clicked = false;
                if let Some((dot_color, tag, bar_pct)) = &active_pill {
                    let badge_w = (badge_target_w + 14.0 * hover_ease)
                        .min((screen_rect.width() - 28.0).max(90.0));
                    let badge_h = 18.0;
                    let badge = egui::Rect::from_center_size(
                        egui::pos2(rect.center().x, screen_rect.top() + 11.0),
                        egui::vec2(badge_w, badge_h),
                    );
                    ui.painter().rect(
                        badge,
                        egui::Rounding::same(9.0),
                        overlay_bg,
                        egui::Stroke::new(1.0_f32, badge_stroke),
                    );
                    ui.painter().circle_filled(
                        egui::pos2(badge.left() + 10.0, badge.center().y),
                        2.5,
                        *dot_color,
                    );

                    if let Some(maybe_pct) = *bar_pct {
                        let bar_rect = egui::Rect::from_min_size(
                            egui::pos2(badge.left() + 17.0, badge.center().y - 2.0),
                            egui::vec2(44.0, 4.0),
                        );
                        paint_progress_bar(ui, ctx, bar_rect, maybe_pct, hairline, accent);
                        ui.painter().text(
                            egui::pos2(bar_rect.right() + 7.0, badge.center().y),
                            egui::Align2::LEFT_CENTER,
                            tag,
                            tag_font,
                            fg,
                        );
                    } else {
                        let text_color = if loading_text.is_some() || active_toast.is_some() {
                            accent
                        } else if passthrough {
                            warn
                        } else {
                            fg
                        };
                        ui.painter().text(
                            egui::pos2(badge.left() + 17.0, badge.center().y),
                            egui::Align2::LEFT_CENTER,
                            tag,
                            tag_font,
                            text_color,
                        );
                    }

                    if resp.clicked() && tag.ends_with("Fix") {
                        let click_pos = ui
                            .input(|i| i.pointer.interact_pos())
                            .unwrap_or(rect.center());
                        if click_pos.x - badge.left() > badge.width() - 32.0 {
                            fix_clicked = true;
                        }
                    }
                } else {
                    let line_w = (badge_target_w + 14.0 * hover_ease)
                        .min((screen_rect.width() - 40.0).max(54.0));
                    let line_h = 3.5 + 0.5 * hover_ease;
                    let handle_line = egui::Rect::from_center_size(
                        egui::pos2(rect.center().x, screen_rect.top() + 7.0),
                        egui::vec2(line_w, line_h),
                    );
                    ui.painter().rect_filled(
                        handle_line,
                        egui::Rounding::same(line_h * 0.5),
                        badge_stroke,
                    );
                }

                if resp.clicked() {
                    if fix_clicked {
                        app.start_fix_for_last();
                    } else {
                        app.top_island_open = true;
                    }
                }
            });
    } else {
        if app.top_island_open && !app.top_island_pinned {
            egui::Area::new(egui::Id::new("lexicue_island_dismiss"))
                .fixed_pos(screen_rect.min)
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(screen_rect, egui::Sense::click());
                    let alpha = (36.0 * ease) as u8;
                    ui.painter().rect_filled(
                        screen_rect,
                        0.0,
                        egui::Color32::from_black_alpha(alpha),
                    );
                    if resp.clicked() {
                        app.top_island_open = false;
                    }
                });
        }

        let target_w = (screen_rect.width() - 32.0).clamp(320.0, 620.0);
        let pill_w = 120.0 + (target_w - 120.0) * ease;
        let pill_y = screen_rect.top() - 48.0 + 57.0 * ease;
        let pill_x = screen_rect.center().x - pill_w * 0.5;
        let mut open_settings = false;
        let mut toggle_pin = false;
        let mut collapse_island = false;
        let mut open_cwd = false;
        let mut copy_cwd = false;
        let mut copy_branch = None;
        let mut send_clear = false;
        let mut send_ctrl_c = false;
        let mut do_copy = false;
        let mut do_paste = false;

        egui::Area::new(egui::Id::new("lexicue_top_island"))
            .fixed_pos(egui::pos2(pill_x, pill_y))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let frame_stroke = match app.island_tint() {
                    crate::contracts::RiskLevel::Dangerous => danger,
                    crate::contracts::RiskLevel::Warn => warn,
                    crate::contracts::RiskLevel::Safe => {
                        if app.top_island_pinned { accent } else { hairline }
                    }
                };
                egui::Frame::none()
                    .fill(overlay_bg)
                    .stroke(egui::Stroke::new(1.0_f32, frame_stroke))
                    .rounding(egui::Rounding::same(16.0))
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_min_width((pill_w - 28.0).max(40.0));
                        ui.set_max_width((pill_w - 28.0).max(40.0));
                        if ease > 0.35 {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 7.0;
                                    let dot_color = if passthrough {
                                        warn
                                    } else {
                                        match app.island_tint() {
                                            crate::contracts::RiskLevel::Dangerous => danger,
                                            crate::contracts::RiskLevel::Warn => warn,
                                            crate::contracts::RiskLevel::Safe => accent,
                                        }
                                    };
                                    let (dot_r, dot_resp) = ui.allocate_exact_size(
                                        egui::vec2(6.0, 12.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().circle_filled(dot_r.center(), 2.8, dot_color);
                                    let status_tip = if app.cfg.general.language == crate::config::UiLanguage::Ru {
                                        "Индикатор режима: обычный / TUI-перехват / опасная команда"
                                    } else {
                                        "Mode indicator: normal / TUI passthrough / dangerous command"
                                    };
                                    dot_resp.on_hover_text(status_tip);

                                    let cwd_resp = flat_button(
                                        ui,
                                        &truncate_text(&format_cwd(app.cwd()), 24),
                                        fg,
                                        hover_gray,
                                        hairline,
                                    )
                                    .on_hover_text(t.cwd_tooltip);
                                    if cwd_resp.clicked() {
                                        open_cwd = true;
                                    }
                                    if cwd_resp.secondary_clicked() {
                                        copy_cwd = true;
                                    }
                                    if let Some(br) = &git_branch {
                                        if choice_pill(
                                            ui,
                                            &format!("git:{}", truncate_text(br, 14)),
                                            false,
                                            muted,
                                            accent,
                                            surface,
                                            selection,
                                            hairline,
                                        )
                                        .on_hover_text(t.branch_tooltip)
                                        .clicked()
                                        {
                                            copy_branch = Some(br.clone());
                                        }
                                    }

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            if icon_button(
                                                ui,
                                                VectorIcon::ChevronUp,
                                                fg,
                                                surface,
                                                hover_gray,
                                                hairline,
                                                false,
                                            )
                                            .on_hover_text(t.collapse_tooltip)
                                            .clicked()
                                            {
                                                collapse_island = true;
                                            }
                                            if icon_button(
                                                ui,
                                                VectorIcon::Pin,
                                                if app.top_island_pinned { accent } else { fg },
                                                if app.top_island_pinned { selection } else { surface },
                                                hover_gray,
                                                if app.top_island_pinned { accent } else { hairline },
                                                app.top_island_pinned,
                                            )
                                            .on_hover_text(if app.top_island_pinned {
                                                t.unpin_tooltip
                                            } else {
                                                t.pin_tooltip
                                            })
                                            .clicked()
                                            {
                                                toggle_pin = true;
                                            }
                                            if flat_button(ui, t.settings, fg, hover_gray, hairline)
                                                .on_hover_text(t.settings)
                                                .clicked()
                                            {
                                                open_settings = true;
                                            }
                                        },
                                    );
                                });

                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if let Some(tp) = &transfer {
                                        let (bar_r, _) = ui.allocate_exact_size(
                                            egui::vec2(88.0, 12.0),
                                            egui::Sense::hover(),
                                        );
                                        let track = egui::Rect::from_center_size(
                                            bar_r.center(),
                                            egui::vec2(88.0, 5.0),
                                        );
                                        paint_progress_bar(
                                            ui,
                                            ctx,
                                            track,
                                            tp.percent,
                                            hairline,
                                            accent,
                                        );
                                        ui.label(
                                            egui::RichText::new(format_transfer_label(
                                                tp.percent,
                                                tp.detail.as_deref(),
                                            ))
                                            .size(11.5)
                                            .color(fg),
                                        );
                                    } else if let Some(txt) = &loading_text {
                                        let clean = txt.trim_start_matches(|c: char| ('\u{2800}'..='\u{28ff}').contains(&c) || c == ' ');
                                        ui.add(egui::Spinner::new().size(11.0).color(accent));
                                        ui.label(egui::RichText::new(clean).size(11.5).color(accent));
                                    } else if let Some(msg) = &active_toast {
                                        ui.label(
                                            egui::RichText::new(format!("OK: {msg}"))
                                                .size(11.5)
                                                .color(accent),
                                        );
                                    } else if let Some(state) = &state_text {
                                        ui.label(
                                            egui::RichText::new(truncate_text(state, 24))
                                                .size(11.5)
                                                .color(if passthrough { warn } else { muted }),
                                        );
                                    } else {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{}×{}",
                                                app.snapshot.cols, app.snapshot.rows
                                            ))
                                            .size(11.5)
                                            .color(muted),
                                        );
                                    }

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.spacing_mut().item_spacing.x = 5.0;
                                            if flat_button(ui, t.btn_interrupt, warn, hover_gray, hairline)
                                                .on_hover_text(t.btn_interrupt_tip)
                                                .clicked()
                                            {
                                                send_ctrl_c = true;
                                            }
                                            if flat_button(ui, t.btn_clear, fg, hover_gray, hairline)
                                                .on_hover_text(t.btn_clear_tip)
                                                .clicked()
                                            {
                                                send_clear = true;
                                            }
                                            if flat_button(ui, t.btn_paste, fg, hover_gray, hairline)
                                                .on_hover_text(t.btn_paste_tip)
                                                .clicked()
                                            {
                                                do_paste = true;
                                            }
                                            if flat_button(ui, t.btn_copy, fg, hover_gray, hairline)
                                                .on_hover_text(t.btn_copy_tip)
                                                .clicked()
                                            {
                                                do_copy = true;
                                            }
                                        },
                                    );
                                });

                                // Context row: environment chips and active ports
                                let env_chips = app.env_chips();
                                let active_ports = app.active_ports();
                                if !env_chips.is_empty() || !active_ports.is_empty() {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        for chip in &env_chips {
                                            let chip_color = match chip.kind {
                                                crate::contracts::EnvBadge::Root | crate::contracts::EnvBadge::Sudo => danger,
                                                crate::contracts::EnvBadge::Ssh | crate::contracts::EnvBadge::Container | crate::contracts::EnvBadge::Password => warn,
                                                crate::contracts::EnvBadge::Venv => muted,
                                            };
                                            choice_pill(ui, &chip.label, false, chip_color, chip_color, surface, selection, hairline);
                                        }
                                        if !active_ports.is_empty() {
                                            ui.label(egui::RichText::new(t.ports_label).size(10.0).color(muted).strong());
                                            for port_hint in active_ports {
                                                let label = format!(":{}", port_hint.port);
                                                let p = port_hint.port;
                                                if choice_pill(ui, &label, false, fg, accent, surface, selection, hairline)
                                                    .on_hover_text(format!("Open http://localhost:{p} in browser"))
                                                    .clicked()
                                                {
                                                    app.pending_island_actions.push(crate::contracts::IslandAction::OpenPort(p));
                                                }
                                            }
                                        }
                                    });
                                }
                            });
                        } else {
                            ui.allocate_exact_size(
                                egui::vec2((pill_w - 28.0).max(20.0), 16.0),
                                egui::Sense::hover(),
                            );
                        }
                    });
            });

        if open_cwd {
            match std::process::Command::new("xdg-open")
                .arg(app.cwd())
                .spawn()
            {
                Ok(_) => app.set_toast(t.toast_opened_folder),
                Err(_) => app.set_toast(t.toast_open_folder_err),
            }
        }
        if copy_cwd {
            ctx.copy_text(app.cwd().display().to_string());
            app.set_toast(t.toast_copied_path);
        }
        if let Some(br) = copy_branch {
            ctx.copy_text(br);
            app.set_toast(t.toast_copied_branch);
        }
        if do_copy {
            let sel = app.extract_logical_selection();
            if !sel.is_empty() {
                ctx.copy_text(sel);
                app.set_toast(t.toast_copied);
            } else if let Some(out) = app.last_output().filter(|s| !s.is_empty()) {
                ctx.copy_text(out.to_string());
                app.set_toast(t.toast_copied);
            } else {
                app.set_toast(t.toast_nothing_to_copy);
            }
        }
        if do_paste {
            app.paste_from_clipboard();
        }
        if send_clear {
            app.reset_scrollback();
            app.write_pty_bytes(b"\x0c");
        }
        if send_ctrl_c {
            app.write_pty_bytes(b"\x03");
        }
        if toggle_pin {
            app.top_island_pinned = !app.top_island_pinned;
            app.top_island_open = true;
        }
        if collapse_island {
            app.top_island_pinned = false;
            app.top_island_open = false;
        }
        if open_settings {
            app.top_island_pinned = false;
            app.top_island_open = false;
            app.settings_open = true;
        }
    }

    if app.settings_open {
        draw_settings_modal(
            app, ctx, fg, accent, muted, surface, overlay_bg, selection, hairline,
        );
    }

    if app.snapshot.scrollback_offset > 0 && !app.settings_open {
        let off = app.snapshot.scrollback_offset;
        egui::Area::new(egui::Id::new("lexicue_scrollback_pill"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -14.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let resp = choice_pill(
                    ui,
                    &format!("↑ -{off} lines · Esc"),
                    false,
                    fg,
                    accent,
                    overlay_bg,
                    selection,
                    hairline,
                );
                if resp.on_hover_text("Return to bottom").clicked() {
                    app.reset_scrollback();
                }
            });
    }

    if app.settings_open {
        if let Some(msg) = active_toast {
            let toast_y = if app.snapshot.scrollback_offset > 0 {
                -46.0
            } else {
                -14.0
            };
            egui::Area::new(egui::Id::new("lexicue_toast_pill"))
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, toast_y))
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(overlay_bg)
                        .stroke(egui::Stroke::new(1.0_f32, hairline))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(msg).size(12.0).color(fg));
                        });
                });
        }
    }

    if let Some(idx) = clicked_live_hint {
        app.clear_selection();
        app.fill_live_hint(idx);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_settings_modal(
    app: &mut LexicueApp,
    ctx: &egui::Context,
    fg: egui::Color32,
    accent: egui::Color32,
    muted: egui::Color32,
    surface: egui::Color32,
    overlay_bg: egui::Color32,
    selection: egui::Color32,
    hairline: egui::Color32,
) {
    let screen_rect = ctx.screen_rect();
    let t = ui_text(app.cfg.general.language);
    egui::Area::new(egui::Id::new("lexicue_settings_backdrop"))
        .fixed_pos(screen_rect.min)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click());
            ui.painter()
                .rect_filled(screen_rect, 0.0, egui::Color32::from_black_alpha(130));
            if resp.clicked() {
                app.rebuild_resolver();
                app.settings_open = false;
            }
        });

    let card_w = 600.0_f32.min((screen_rect.width() - 40.0).max(340.0));
    let card_h = 540.0_f32.min((screen_rect.height() - 48.0).max(300.0));
    let mut close_settings = false;
    let tab_id = egui::Id::new("lexicue_settings_tab");
    let mut active_tab = ctx.data_mut(|d| d.get_temp::<u8>(tab_id).unwrap_or(0));

    egui::Window::new("lexicue_settings_modal")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .fixed_size(egui::vec2(card_w, card_h))
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(overlay_bg)
                .stroke(egui::Stroke::new(1.0_f32, hairline))
                .rounding(egui::Rounding::same(12.0))
                .inner_margin(egui::Margin::same(20.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(t.settings)
                        .size(17.0)
                        .color(accent)
                        .strong(),
                );
                ui.add_space(10.0);
                for (idx, title) in [
                    (0u8, t.tab_appearance),
                    (1, t.tab_assistant),
                    (2, t.tab_shortcuts),
                ] {
                    if choice_pill(
                        ui,
                        title,
                        active_tab == idx,
                        fg,
                        accent,
                        surface,
                        selection,
                        hairline,
                    )
                    .clicked()
                    {
                        active_tab = idx;
                        ctx.data_mut(|d| d.insert_temp(tab_id, idx));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(
                        ui,
                        VectorIcon::CloseCross,
                        fg,
                        surface,
                        selection,
                        hairline,
                        true,
                    )
                    .on_hover_text(t.close_esc)
                    .clicked()
                    {
                        close_settings = true;
                    }
                });
            });
            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .max_height((card_h - 102.0).max(160.0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 12.0);

                    if active_tab == 0 {
                        settings_card(ui, surface, hairline, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(t.theme_presets)
                                        .size(10.5)
                                        .color(muted)
                                        .strong(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if choice_pill(
                                            ui,
                                            t.reset_theme,
                                            false,
                                            muted,
                                            accent,
                                            overlay_bg,
                                            selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            app.reset_theme_defaults();
                                        }
                                    },
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
                                for preset in THEME_PRESETS {
                                    let active = app
                                        .cfg
                                        .theme
                                        .background
                                        .eq_ignore_ascii_case(preset.background)
                                        && app.cfg.theme.accent.eq_ignore_ascii_case(preset.accent);
                                    if choice_pill(
                                        ui,
                                        preset.name,
                                        active,
                                        fg,
                                        accent,
                                        overlay_bg,
                                        selection,
                                        hairline,
                                    )
                                    .clicked()
                                    {
                                        app.apply_theme_preset(preset);
                                    }
                                }
                            });
                        });

                        settings_card(ui, surface, hairline, |ui| {
                            ui.label(
                                egui::RichText::new(t.fine_tune_colors)
                                    .size(10.5)
                                    .color(muted)
                                    .strong(),
                            );
                            let pal = app.theme;
                            let mut dirty = false;
                            egui::Grid::new("lexicue_theme_colors_grid")
                                .num_columns(2)
                                .spacing(egui::vec2(18.0, 8.0))
                                .min_col_width(240.0)
                                .show(ui, |ui| {
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_bg,
                                        &mut app.cfg.theme.background,
                                        pal.background,
                                        fg,
                                        muted,
                                    );
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_fg,
                                        &mut app.cfg.theme.foreground,
                                        pal.foreground,
                                        fg,
                                        muted,
                                    );
                                    ui.end_row();
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_accent,
                                        &mut app.cfg.theme.accent,
                                        pal.accent,
                                        fg,
                                        muted,
                                    );
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_overlay,
                                        &mut app.cfg.theme.overlay_bg,
                                        pal.overlay_bg,
                                        fg,
                                        muted,
                                    );
                                    ui.end_row();
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_selection,
                                        &mut app.cfg.theme.selection,
                                        pal.selection,
                                        fg,
                                        muted,
                                    );
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_warning,
                                        &mut app.cfg.theme.warning,
                                        pal.warning,
                                        fg,
                                        muted,
                                    );
                                    ui.end_row();
                                    dirty |= color_role_row(
                                        ui,
                                        t.color_danger,
                                        &mut app.cfg.theme.danger,
                                        pal.danger,
                                        fg,
                                        muted,
                                    );
                                    ui.end_row();
                                });
                            if dirty {
                                app.apply_theme_from_cfg();
                            }
                        });

                        settings_card(ui, surface, hairline, |ui| {
                            ui.label(
                                egui::RichText::new(t.typography_and_lang)
                                    .size(10.5)
                                    .color(muted)
                                    .strong(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(t.language_label).size(13.0).color(fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for (label, lang) in [
                                            ("Русский", UiLanguage::Ru),
                                            ("English", UiLanguage::En),
                                        ] {
                                            let active = app.cfg.general.language == lang;
                                            if choice_pill(
                                                ui, label, active, fg, accent, overlay_bg,
                                                selection, hairline,
                                            )
                                            .clicked()
                                            {
                                                app.set_language(lang);
                                            }
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.font_size).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if choice_pill(
                                            ui, "+", false, fg, accent, overlay_bg, selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            app.adjust_font_size(1.0);
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{:.0} px",
                                                app.theme.font_size
                                            ))
                                            .size(13.0)
                                            .color(accent)
                                            .strong(),
                                        );
                                        if choice_pill(
                                            ui, "-", false, fg, accent, overlay_bg, selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            app.adjust_font_size(-1.0);
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(t.scrollback_lines).size(13.0).color(fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for count in [10000usize, 5000, 2000, 1000] {
                                            let active = app.cfg.general.scrollback_lines == count;
                                            if choice_pill(
                                                ui,
                                                &count.to_string(),
                                                active,
                                                fg,
                                                accent,
                                                overlay_bg,
                                                selection,
                                                hairline,
                                            )
                                            .clicked()
                                            {
                                                app.cfg.general.scrollback_lines = count;
                                                app.status_note = None;
                                            }
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.prompt_icon).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for (label, kind) in [
                                            ("Chaos", PromptIcon::Chaos),
                                            ("Lambda", PromptIcon::Lambda),
                                            ("Rocket", PromptIcon::Rocket),
                                            ("Ghost", PromptIcon::Ghost),
                                            ("Cat", PromptIcon::Cat),
                                            ("Bolt", PromptIcon::Bolt),
                                            ("Spark", PromptIcon::Spark),
                                            ("Arrow", PromptIcon::Arrow),
                                        ] {
                                            let active = app.cfg.theme.prompt_icon == kind;
                                            let (rect, resp) = ui.allocate_exact_size(
                                                egui::vec2(28.0, 24.0),
                                                egui::Sense::click(),
                                            );
                                            let fill = if active { selection } else { overlay_bg };
                                            let stroke_color = if active || resp.hovered() {
                                                accent
                                            } else {
                                                hairline
                                            };
                                            let icon_fg = if active { accent } else { fg };
                                            ui.painter().rect(
                                                rect,
                                                egui::Rounding::same(6.0),
                                                fill,
                                                egui::Stroke::new(1.0_f32, stroke_color),
                                            );
                                            let icon_r = egui::Rect::from_center_size(
                                                rect.center(),
                                                egui::vec2(14.0, 14.0),
                                            );
                                            grid::draw_prompt_icon_preview(
                                                ui.painter(),
                                                icon_r,
                                                icon_fg,
                                                fill,
                                                kind,
                                            );
                                            if resp.on_hover_text(label).clicked() {
                                                app.set_prompt_icon(kind);
                                            }
                                        }
                                    },
                                );
                            });
                        });
                    } else if active_tab == 2 {
                        settings_card(ui, surface, hairline, |ui| {
                            ui.label(
                                egui::RichText::new(t.suggestions_hdr)
                                    .size(10.5)
                                    .color(muted)
                                    .strong(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(t.max_inline_hints).size(13.0).color(fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for n in [8usize, 5, 3] {
                                            let active = app.cfg.resolver.max_candidates == n;
                                            if choice_pill(
                                                ui,
                                                &n.to_string(),
                                                active,
                                                fg,
                                                accent,
                                                overlay_bg,
                                                selection,
                                                hairline,
                                            )
                                            .clicked()
                                            {
                                                app.cfg.resolver.max_candidates = n;
                                                app.rebuild_resolver();
                                            }
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(t.history_weight).size(13.0).color(fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for w in [2.5_f32, 1.5, 1.0] {
                                            let active =
                                                (app.cfg.resolver.history_bonus - w).abs() < 0.05;
                                            if choice_pill(
                                                ui,
                                                &format!("{w:.1}x"),
                                                active,
                                                fg,
                                                accent,
                                                overlay_bg,
                                                selection,
                                                hairline,
                                            )
                                            .clicked()
                                            {
                                                app.cfg.resolver.history_bonus = w;
                                                app.rebuild_resolver();
                                            }
                                        }
                                    },
                                );
                            });
                        });

                        settings_card(ui, surface, hairline, |ui| {
                            ui.label(
                                egui::RichText::new(t.shortcuts_hdr)
                                    .size(10.5)
                                    .color(muted)
                                    .strong(),
                            );
                            for (keys, desc) in [
                                ("Up / Down", t.sc_up_down),
                                ("Alt + Up / Down", t.sc_alt_up_down),
                                ("Right / Click", t.sc_right_click),
                                ("?? <text>", t.sc_qq_text),
                                ("Alt + F", t.sc_alt_f),
                                ("Ctrl + Shift + C", t.sc_copy),
                                ("Wheel / Shift+PgUp", t.sc_scroll),
                                ("Ctrl + Click", t.sc_ctrl_click),
                            ] {
                                ui.horizontal(|ui| {
                                    keycap_pill(ui, keys, overlay_bg, hairline, accent);
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(desc).size(12.0).color(muted),
                                            );
                                        },
                                    );
                                });
                            }
                        });
                    } else {
                        settings_card(ui, surface, hairline, |ui| {
                            ui.label(
                                egui::RichText::new(t.assistant_safety_hdr)
                                    .size(10.5)
                                    .color(muted)
                                    .strong(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.llm_fallback).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let label = if app.cfg.llm.enabled {
                                            t.enabled
                                        } else {
                                            t.disabled
                                        };
                                        if choice_pill(
                                            ui,
                                            label,
                                            app.cfg.llm.enabled,
                                            fg,
                                            accent,
                                            overlay_bg,
                                            selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            app.toggle_llm();
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.provider).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for (label, kind) in [
                                            ("OpenAI", LlmProviderKind::OpenAi),
                                            ("Anthropic", LlmProviderKind::Anthropic),
                                            ("Gemini", LlmProviderKind::Gemini),
                                            ("DeepSeek", LlmProviderKind::DeepSeek),
                                            ("Ollama", LlmProviderKind::Ollama),
                                        ] {
                                            let active = app.cfg.llm.provider == kind;
                                            if choice_pill(
                                                ui, label, active, fg, accent, overlay_bg,
                                                selection, hairline,
                                            )
                                            .clicked()
                                            {
                                                app.set_llm_provider(kind);
                                            }
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.endpoint).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let (def_ep, def_model, _) =
                                            app.cfg.llm.provider.defaults();
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(&mut app.cfg.llm.endpoint)
                                                .hint_text(
                                                    egui::RichText::new(def_ep)
                                                        .color(app.theme.muted_fg(46)),
                                                )
                                                .desired_width(270.0),
                                        );
                                        if resp.changed() || resp.lost_focus() {
                                            app.rebuild_resolver();
                                        }
                                        let _ = def_model;
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.model).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let (_, def_model, _) = app.cfg.llm.provider.defaults();
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(&mut app.cfg.llm.model)
                                                .hint_text(
                                                    egui::RichText::new(def_model)
                                                        .color(app.theme.muted_fg(46)),
                                                )
                                                .desired_width(270.0),
                                        );
                                        if resp.changed() || resp.lost_focus() {
                                            app.rebuild_resolver();
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.api_key).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if choice_pill(
                                            ui, t.test_btn, false, fg, accent, overlay_bg,
                                            selection, hairline,
                                        )
                                        .clicked()
                                        {
                                            app.start_provider_probe();
                                        }
                                        let show_id =
                                            egui::Id::new("lexicue_settings_show_api_key");
                                        let mut reveal = ui.ctx().data_mut(|d| {
                                            d.get_temp::<bool>(show_id).unwrap_or(false)
                                        });
                                        if choice_pill(
                                            ui,
                                            if reveal { t.hide } else { t.show },
                                            reveal,
                                            fg,
                                            accent,
                                            overlay_bg,
                                            selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            reveal = !reveal;
                                            ui.ctx().data_mut(|d| d.insert_temp(show_id, reveal));
                                        }
                                        let edit =
                                            egui::TextEdit::singleline(&mut app.cfg.llm.api_key)
                                                .password(!reveal)
                                                .hint_text(
                                                    egui::RichText::new(&app.cfg.llm.api_key_env)
                                                        .color(app.theme.muted_fg(46)),
                                                )
                                                .desired_width(168.0);
                                        let resp = ui.add(edit);
                                        if resp.changed() || resp.lost_focus() {
                                            if !app.cfg.llm.api_key.trim().is_empty() {
                                                app.cfg.llm.enabled = true;
                                            }
                                            app.rebuild_resolver();
                                        }
                                    },
                                );
                            });
                            if let Some((ok, msg)) = &app.probe_status {
                                let color = if *ok {
                                    accent
                                } else {
                                    app.theme.danger.to_color32()
                                };
                                ui.label(egui::RichText::new(msg).size(11.5).color(color));
                            }
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.prompt_file).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(
                                                &mut app.cfg.llm.system_prompt_file,
                                            )
                                            .hint_text(
                                                egui::RichText::new("~/.config/lexicue/prompt.txt")
                                                    .color(app.theme.muted_fg(46)),
                                            )
                                            .desired_width(270.0),
                                        );
                                        if resp.changed() || resp.lost_focus() {
                                            app.rebuild_resolver();
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.instructions).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if !app.cfg.llm.system_prompt.is_empty()
                                            && choice_pill(
                                                ui, t.reset, false, muted, accent, overlay_bg,
                                                selection, hairline,
                                            )
                                            .clicked()
                                        {
                                            app.cfg.llm.system_prompt.clear();
                                            app.rebuild_resolver();
                                        }
                                    },
                                );
                            });
                            let instr_resp = ui.add(
                                egui::TextEdit::multiline(&mut app.cfg.llm.system_prompt)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(
                                        egui::RichText::new(t.instructions_hint)
                                            .color(app.theme.muted_fg(46)),
                                    ),
                            );
                            if instr_resp.changed() || instr_resp.lost_focus() {
                                app.rebuild_resolver();
                            }
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t.timeout).size(13.0).color(fg));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        for (label, ms) in [
                                            ("45s", 45000u64),
                                            ("25s", 25000u64),
                                            ("10s", 10000u64),
                                        ] {
                                            let active = app.cfg.llm.timeout_ms == ms;
                                            if choice_pill(
                                                ui, label, active, fg, accent, overlay_bg,
                                                selection, hairline,
                                            )
                                            .clicked()
                                            {
                                                app.cfg.llm.timeout_ms = ms;
                                                app.rebuild_resolver();
                                            }
                                        }
                                    },
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(t.confirm_dangerous)
                                        .size(13.0)
                                        .color(fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let label = if app.cfg.safety.confirm_dangerous {
                                            t.required
                                        } else {
                                            t.bypassed
                                        };
                                        if choice_pill(
                                            ui,
                                            label,
                                            app.cfg.safety.confirm_dangerous,
                                            fg,
                                            accent,
                                            overlay_bg,
                                            selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            app.toggle_confirm_dangerous();
                                        }
                                        if choice_pill(
                                            ui,
                                            t.clear_cache,
                                            false,
                                            muted,
                                            accent,
                                            overlay_bg,
                                            selection,
                                            hairline,
                                        )
                                        .clicked()
                                        {
                                            app.clear_ai_cache();
                                        }
                                    },
                                );
                            });
                        });
                    }
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let note = app.status_note.as_deref().unwrap_or("Unsaved");
                let (note_text, note_color) = if note == "Saved" {
                    (t.saved, accent)
                } else if note == "Unsaved" {
                    (t.unsaved_changes, muted)
                } else {
                    (note, accent)
                };
                ui.label(egui::RichText::new(note_text).size(12.0).color(note_color));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if choice_pill(ui, t.close, false, fg, accent, surface, selection, hairline)
                        .clicked()
                    {
                        close_settings = true;
                    }
                    if choice_pill(ui, t.save, true, fg, accent, surface, selection, hairline)
                        .clicked()
                    {
                        app.save_config();
                    }
                    if choice_pill(
                        ui, t.reset, false, muted, accent, surface, selection, hairline,
                    )
                    .clicked()
                    {
                        app.reset_defaults();
                    }
                });
            });
        });

    if close_settings {
        app.rebuild_resolver();
        app.settings_open = false;
    }
}

fn ctrl_char(key: egui::Key) -> Option<char> {
    use egui::Key;
    Some(match key {
        Key::A => 'a',
        Key::B => 'b',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::I => 'i',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::M => 'm',
        Key::N => 'n',
        Key::O => 'o',
        Key::P => 'p',
        Key::Q => 'q',
        Key::R => 'r',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        Key::OpenBracket => '[',
        Key::Backslash => '\\',
        Key::CloseBracket => ']',
        _ => return None,
    })
}

/// Translate raw egui events into lexicue input. Unknown events are dropped.
/// `Paste` arrives as one event and must stay one unit.
pub fn translate_input(events: &[egui::Event]) -> Vec<KeyInput> {
    use egui::{Event, Key};

    let mut out = Vec::new();
    for event in events {
        match event {
            Event::Copy => {
                out.push(KeyInput::CtrlShift('c'));
            }
            Event::Paste(text) => {
                if !text.is_empty() {
                    out.push(KeyInput::Paste(text.clone()));
                }
            }
            Event::Text(text) => {
                for ch in text.chars().filter(|c| !c.is_control()) {
                    out.push(KeyInput::Char(ch));
                }
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if (modifiers.ctrl || modifiers.command) && modifiers.shift && !modifiers.alt {
                    if *key == Key::C {
                        out.push(KeyInput::CtrlShift('c'));
                    } else if *key == Key::F {
                        out.push(KeyInput::CtrlShift('f'));
                    }
                } else if (modifiers.ctrl || modifiers.command) && !modifiers.alt {
                    if *key == Key::Enter {
                        out.push(KeyInput::CtrlEnter);
                    } else if let Some(ch) = ctrl_char(*key) {
                        out.push(KeyInput::Ctrl(ch));
                    }
                } else if modifiers.alt && !modifiers.ctrl && !modifiers.command {
                    let mapped = match key {
                        Key::ArrowUp => Some(KeyInput::AltArrowUp),
                        Key::ArrowDown => Some(KeyInput::AltArrowDown),
                        Key::F => Some(KeyInput::AltChar('f')),
                        _ => None,
                    };
                    if let Some(k) = mapped {
                        out.push(k);
                    }
                } else if modifiers.shift && !modifiers.ctrl && !modifiers.alt && !modifiers.command
                {
                    let mapped = match key {
                        Key::PageUp => Some(KeyInput::ShiftPageUp),
                        Key::PageDown => Some(KeyInput::ShiftPageDown),
                        _ => None,
                    };
                    if let Some(k) = mapped {
                        out.push(k);
                    }
                } else if !modifiers.ctrl && !modifiers.alt && !modifiers.command {
                    let mapped = match key {
                        Key::Enter => Some(KeyInput::Enter),
                        Key::Tab => Some(KeyInput::Tab),
                        Key::Escape => Some(KeyInput::Escape),
                        Key::Backspace => Some(KeyInput::Backspace),
                        Key::ArrowUp => Some(KeyInput::ArrowUp),
                        Key::ArrowDown => Some(KeyInput::ArrowDown),
                        Key::ArrowLeft => Some(KeyInput::ArrowLeft),
                        Key::ArrowRight => Some(KeyInput::ArrowRight),
                        Key::PageUp => Some(KeyInput::PageUp),
                        Key::PageDown => Some(KeyInput::PageDown),
                        _ => None,
                    };
                    if let Some(k) = mapped {
                        out.push(k);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::translate_input;
    use crate::contracts::KeyInput;
    use eframe::egui::{Event, Key, Modifiers, Pos2};

    fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn translates_text_chars_and_filters_control_chars() {
        let events = vec![Event::Text("ls -la".into()), Event::Text("\n\x03".into())];
        assert_eq!(
            translate_input(&events),
            vec![
                KeyInput::Char('l'),
                KeyInput::Char('s'),
                KeyInput::Char(' '),
                KeyInput::Char('-'),
                KeyInput::Char('l'),
                KeyInput::Char('a'),
            ]
        );
    }

    #[test]
    fn translates_enter_tab_backspace_escape_and_arrows() {
        let events = vec![
            key_event(Key::Enter, true, Modifiers::NONE),
            key_event(Key::Tab, true, Modifiers::NONE),
            key_event(Key::Backspace, true, Modifiers::NONE),
            key_event(Key::Escape, true, Modifiers::NONE),
            key_event(Key::ArrowUp, true, Modifiers::NONE),
            key_event(Key::ArrowDown, true, Modifiers::NONE),
            key_event(Key::ArrowLeft, true, Modifiers::NONE),
            key_event(Key::ArrowRight, true, Modifiers::NONE),
            key_event(Key::Enter, false, Modifiers::NONE),
        ];
        assert_eq!(
            translate_input(&events),
            vec![
                KeyInput::Enter,
                KeyInput::Tab,
                KeyInput::Backspace,
                KeyInput::Escape,
                KeyInput::ArrowUp,
                KeyInput::ArrowDown,
                KeyInput::ArrowLeft,
                KeyInput::ArrowRight,
            ]
        );
    }

    #[test]
    fn translates_ctrl_shortcuts() {
        let events = vec![
            key_event(Key::C, true, Modifiers::CTRL),
            key_event(Key::D, true, Modifiers::CTRL),
            key_event(Key::C, false, Modifiers::CTRL),
        ];
        assert_eq!(
            translate_input(&events),
            vec![KeyInput::Ctrl('c'), KeyInput::Ctrl('d')]
        );
    }

    #[test]
    fn translates_alt_arrow_shortcuts() {
        let events = vec![
            key_event(Key::ArrowUp, true, Modifiers::ALT),
            key_event(Key::ArrowDown, true, Modifiers::ALT),
        ];
        assert_eq!(
            translate_input(&events),
            vec![KeyInput::AltArrowUp, KeyInput::AltArrowDown]
        );
    }

    #[test]
    fn preserves_paste_as_single_unit_and_drops_unknown_events() {
        let events = vec![
            Event::PointerMoved(Pos2::new(10.0, 20.0)),
            Event::Paste("echo hello\npwd".into()),
            Event::Paste(String::new()),
        ];
        assert_eq!(
            translate_input(&events),
            vec![KeyInput::Paste("echo hello\npwd".into())]
        );
    }

    #[test]
    fn translates_qol_scroll_copy_and_fix_shortcuts() {
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        let events = vec![
            Event::Copy,
            key_event(Key::C, true, ctrl_shift),
            key_event(Key::F, true, Modifiers::ALT),
            key_event(Key::PageUp, true, Modifiers::SHIFT),
            key_event(Key::PageDown, true, Modifiers::SHIFT),
            key_event(Key::PageUp, true, Modifiers::NONE),
            key_event(Key::PageDown, true, Modifiers::NONE),
        ];
        assert_eq!(
            translate_input(&events),
            vec![
                KeyInput::CtrlShift('c'),
                KeyInput::CtrlShift('c'),
                KeyInput::AltChar('f'),
                KeyInput::ShiftPageUp,
                KeyInput::ShiftPageDown,
                KeyInput::PageUp,
                KeyInput::PageDown,
            ]
        );
    }
}
