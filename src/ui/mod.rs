//! UI-слой: каркас окна, диалоги, стиль и общие помощники.
//!
//! Каждый экран живёт в своём файле и экспортирует `State` (Default), `STR` (таблица строк)
//! и `show(ui, app, &mut State)`. Состояния хранятся в [`UiState`]; чтобы не держать заимствование
//! всего `CoreTunerApp`, `draw` на время вызова забирает состояние экрана через `mem::take`.
pub mod cores;
pub mod file;
pub mod processes;
pub mod profiles;
pub mod settings;

use crate::app::{CoreTunerApp, Dialog, EventKind, EventRecord, Tab};
use crate::config::{Lang, ThemeMode};
use crate::i18n::{fill, t, tf};use crate::model::{CpuSet, MatchMode, PriorityClass};
use egui::{Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, RichText, Stroke, Theme, ThemePreference};
use std::sync::Arc;

/// Строки каркаса и диалогов.
pub const STR: &[(&str, &str, &str)] = &[
    ("dialog.title", "Подтверждение", "Confirm"),
    ("dialog.realtime", "Приоритет Realtime может «заморозить» систему: процесс {name} (PID {pid}) вытеснит почти всё остальное. Продолжить?", "Realtime priority can freeze the system: {name} (PID {pid}) will pre-empt almost everything else. Continue?"),
    ("dialog.realtime_apply", "Всё равно применить", "Apply anyway"),
    ("dialog.save_profile", "Сохранить текущие настройки процесса {name} как профиль:", "Save the current settings of {name} as a profile:"),
    ("dialog.bind_app", "Создать профиль из настроек {name} и привязать его к приложению:", "Create a profile from the settings of {name} and bind it to the app:"),
    ("dialog.delete_profile", "Удалить профиль «{name}»?", "Delete profile \"{name}\"?"),
    ("dialog.access_denied", "Нет доступа к процессу {name}. Для изменения нужны права администратора.", "Access to {name} is denied. Administrator rights are required."),
    ("dialog.restart_admin", "Перезапустить от администратора", "Restart as administrator"),
    ("dialog.create", "Создать", "Create"),
    ("dialog.create_bind", "Создать и привязать", "Create and bind"),
    ("dialog.bind_by", "Привязка", "Bind by"),
];

/// Состояния всех экранов.
#[derive(Default)]
pub struct UiState {
    pub processes: processes::State,
    pub file: file::State,
    pub profiles: profiles::State,
    pub cores: cores::State,
    pub settings: settings::State,
    /// Текстовое поле открытого диалога.
    pub dialog_text: String,
    pub dialog_mode: MatchMode,
}

/// Верхняя панель: название, вкладки, статусы.
fn top_bar(ui: &mut egui::Ui, app: &mut CoreTunerApp) {
    let lang = app.lang();
    ui.horizontal(|ui| {
        ui.heading(RichText::new("CoreTuner").strong());
        ui.separator();
        for tab in Tab::ALL {
            if ui.selectable_label(app.tab == tab, t(lang, tab.key())).clicked() {
                app.tab = tab;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if app.cfg.settings.paused {
                ui.label(RichText::new(t(lang, "settings.paused")).color(ui.visuals().warn_fg_color));
            }
            if !app.elevated {
                ui.label(RichText::new(format!("{}: {}", t(lang, "settings.admin"), t(lang, "settings.admin_no"))).weak());
            }
            if !app.monitor_alive {
                ui.label(RichText::new(t(lang, "err.monitor")).color(ui.visuals().error_fg_color));
            }
        });
    });
    ui.separator();
}

/// Полный кадр: верхняя панель, текущая вкладка, баннер ошибки, тосты.
pub fn draw(app: &mut CoreTunerApp, ui: &mut egui::Ui) {
    egui::Frame::NONE.inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        top_bar(ui, app);
        if let Some(err) = app.error.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, err);
                if ui.small_button(t(app.lang(), "common.dismiss")).clicked() {
                    app.error = None;
                }
            });
        }
        match app.tab {
            Tab::Processes => {
                let mut st = std::mem::take(&mut app.ui.processes);
                processes::show(ui, app, &mut st);
                app.ui.processes = st;
            }
            Tab::ByFile => {
                let mut st = std::mem::take(&mut app.ui.file);
                file::show(ui, app, &mut st);
                app.ui.file = st;
            }
            Tab::Profiles => {
                let mut st = std::mem::take(&mut app.ui.profiles);
                profiles::show(ui, app, &mut st);
                app.ui.profiles = st;
            }
            Tab::Settings => {
                let mut st = std::mem::take(&mut app.ui.settings);
                settings::show(ui, app, &mut st);
                app.ui.settings = st;
            }
        }
    });
    draw_toasts(ui.ctx(), app);
}

/// Всплывающие плашки в правом нижнем углу окна.
fn draw_toasts(ctx: &egui::Context, app: &CoreTunerApp) {
    if app.toasts.is_empty() {
        return;
    }
    egui::Area::new(egui::Id::unique("ct_toasts"))        .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0])
        .interactable(false)
        .show(ctx, |ui| {
            for toast in &app.toasts {
                let color = if toast.error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(&toast.text).color(color));
                });
            }
        });
}

/// Применить приоритет Realtime после подтверждения.
fn apply_realtime(app: &mut CoreTunerApp, pid: u32) {
    match crate::cpu::priority::set_priority(pid, PriorityClass::Realtime) {
        Ok(()) => {
            let msg = app.text("cores.priority_applied").to_string();
            app.toast(msg, false);
        }
        Err(e) => {
            let msg = app.fmt("err.apply", &[("e", &e.to_string())]);
            app.toast(msg, true);
        }
    }
}

/// Модальные диалоги поверх окна.
pub fn draw_dialogs(app: &mut CoreTunerApp, ctx: &egui::Context) {
    let Some(dialog) = app.dialog.clone() else { return };
    let lang = app.lang();
    let mut close = false;
    let mut open = true;
    egui::Window::new(t(lang, "dialog.title"))
        .id(egui::Id::unique("ct_dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            match &dialog {
                Dialog::Realtime { pid, name } => {
                    ui.label(fill(t(lang, "dialog.realtime"), &[("name", name), ("pid", &pid.to_string())]));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(t(lang, "dialog.realtime_apply")).color(ui.visuals().warn_fg_color)).clicked() {
                            apply_realtime(app, *pid);
                            close = true;
                        }
                        if ui.button(t(lang, "common.cancel")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::SaveProfile { name, target, mask, prio, .. } | Dialog::BindToApp { name, target, mask, prio, .. } => {
                    let bind = matches!(dialog, Dialog::BindToApp { .. });
                    let key = if bind { "dialog.bind_app" } else { "dialog.save_profile" };
                    ui.label(fill(t(lang, key), &[("name", name)]));
                    ui.add_space(4.0);
                    ui.add(egui::TextEdit::singleline(&mut app.ui.dialog_text).desired_width(f32::INFINITY));
                    let mut do_bind = bind;
                    if bind {
                        ui.horizontal(|ui| {
                            ui.label(t(lang, "dialog.bind_by"));
                            ui.selectable_value(&mut app.ui.dialog_mode, MatchMode::Name, t(lang, "file.mode.name"));
                            ui.selectable_value(&mut app.ui.dialog_mode, MatchMode::Path, t(lang, "file.mode.path"));
                        });
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let name_ok = !app.ui.dialog_text.trim().is_empty() && !mask.is_empty();
                        let label = if bind { "dialog.create_bind" } else { "dialog.create" };
                        if ui.add_enabled(name_ok, egui::Button::new(t(lang, label))).clicked() {
                            let pname = app.ui.dialog_text.trim().to_string();
                            let id = app.create_profile(&pname, mask.clone(), *prio);
                            if do_bind {
                                let mode = app.ui.dialog_mode;
                                let tgt = if mode == MatchMode::Name { name.clone() } else { target.clone() };
                                if !tgt.is_empty() {
                                    app.bind_profile(&tgt, mode, &id);
                                }
                            }
                            do_bind = false;
                            let msg = app.fmt("cores.saved_profile", &[("name", &pname)]);
                            app.toast(msg, false);
                            close = true;
                        }
                        if ui.button(t(lang, "common.cancel")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::DeleteProfile { id, name } => {
                    ui.label(fill(t(lang, "dialog.delete_profile"), &[("name", name)]));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(t(lang, "common.delete")).color(ui.visuals().error_fg_color)).clicked() {
                            app.cfg.profiles.retain(|p| &p.id != id);
                            for b in &mut app.cfg.bindings {
                                b.profile_ids.retain(|p| p != id);
                                if b.active_profile.as_deref() == Some(id.as_str()) {
                                    b.active_profile = None;
                                }
                            }
                            app.mark_dirty();
                            app.sync_tray();
                            close = true;
                        }
                        if ui.button(t(lang, "common.cancel")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::AccessDenied { name, .. } => {
                    ui.label(fill(t(lang, "dialog.access_denied"), &[("name", name)]));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(t(lang, "dialog.restart_admin")).clicked() {
                            app.restart_as_admin();
                            close = true;
                        }
                        if ui.button(t(lang, "common.close")).clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
    if close || !open {
        app.dialog = None;
    }
}

/// Цвет строки журнала по типу события.
pub fn event_color(ui: &egui::Ui, kind: &EventKind) -> Color32 {
    match kind {
        EventKind::Applied => ui.visuals().hyperlink_color,
        EventKind::Failed => ui.visuals().error_fg_color,
        EventKind::ProfileSwitched => ui.visuals().warn_fg_color,
        EventKind::Info => ui.visuals().text_color(),
    }
}

/// Значок события для журнала.
pub fn event_glyph(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Applied => "✓",
        EventKind::Failed => "✕",
        EventKind::ProfileSwitched => "⇄",
        EventKind::Info => "•",
    }
}

/// Текст записи журнала: «имя (PID) — профиль — причина».
pub fn event_text(e: &EventRecord) -> String {
    let mut s = if e.name.is_empty() { format!("PID {}", e.pid) } else { format!("{} (PID {})", e.name, e.pid) };
    if !e.profile.is_empty() {
        s.push_str(" — ");
        s.push_str(&e.profile);
    }
    if !e.reason.is_empty() {
        s.push_str(" — ");
        s.push_str(&e.reason);
    }
    s
}

/// Сводка маски для карточки: «N из M».
pub fn mask_summary(lang: Lang, set: &CpuSet, total: usize) -> String {
    tf(lang, "cores.mask_summary", &[("on", &set.len().to_string()), ("total", &total.to_string())])
}

/// Тёмный/светлый стиль, применяется к обеим темам (egui 0.37 хранит стили отдельно).
pub fn apply_style(ctx: &Context, theme: ThemeMode) {
    let pref = match theme {
        ThemeMode::Light => ThemePreference::Light,
        ThemeMode::Dark => ThemePreference::Dark,
        ThemeMode::System => ThemePreference::System,
    };
    ctx.set_theme(pref);
    for th in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(th, |style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size.y = 24.0;
            style.spacing.slider_width = 120.0;
            style.visuals.panel_fill = panel_fill(th);
            style.visuals.window_corner_radius = CornerRadius::same(8);
            style.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
            style.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
            style.visuals.widgets.active.corner_radius = CornerRadius::same(6);
            style.visuals.selection.bg_fill = accent(th).gamma_multiply(0.55);
        });
    }
}

fn panel_fill(theme: Theme) -> Color32 {
    match theme {
        Theme::Dark => Color32::from_rgb(24, 26, 30),
        Theme::Light => Color32::from_rgb(246, 247, 249),
    }
}

fn accent(theme: Theme) -> Color32 {
    match theme {
        Theme::Dark => Color32::from_rgb(92, 170, 255),
        Theme::Light => Color32::from_rgb(40, 120, 220),
    }
}

/// Подключить шрифт с кириллицей. Встроенные Ubuntu-Light/Hack содержат кириллицу, но
/// системный Segoe UI выглядит привычнее на Windows; если файла нет — остаёмся на встроенных.
pub fn install_fonts(ctx: &Context) {
    let mut defs = FontDefinitions::default();
    let mut installed = false;
    for path in ["C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\tahoma.ttf"] {
        match std::fs::read(path) {
            Ok(bytes) => {
                defs.font_data.insert("coretuner_ui".to_owned(), Arc::new(FontData::from_owned(bytes)));
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    defs.families.entry(family).or_default().insert(0, "coretuner_ui".to_owned());
                }
                installed = true;
                break;
            }
            Err(e) => tracing::debug!("шрифт {path} недоступен: {e}"),
        }
    }
    if !installed {
        tracing::info!("системный шрифт не найден, используются встроенные шрифты egui");
    }
    ctx.set_fonts(defs);
}

/// Иконка приложения в формате `IconData` (RGBA, `size`×`size`).
pub fn icon_data(size: u32) -> egui::IconData {
    egui::IconData { rgba: icon_rgba(size), width: size, height: size }
}

/// Программно нарисованная иконка: скруглённая плитка с сеткой 2×2 «ядер».
///
/// Пиксели считаются аналитически (без крейтов рисования), поэтому результат одинаков
/// для окна и трея и легко проверяется тестом.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let n = size.max(4) as usize;
    let mut px = vec![0u8; n * n * 4];
    let f = n as f32;
    let radius = f * 0.22;
    let inset = f * 0.06;
    let tile = Color32::from_rgb(0x2B, 0x7D, 0x5B);
    let cell = Color32::from_rgb(0xBF, 0xE8, 0xD2);
    let gap = f * 0.06;
    let cell_size = (f - 2.0 * inset - gap) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let i = (y * n + x) * 4;
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let coverage = rounded_rect_coverage(fx, fy, inset, inset, f - inset, f - inset, radius);
            if coverage <= 0.0 {
                continue;
            }
            let mut color = tile;
            // Сетка 2×2: ячейки слегка отделены фоном плитки.
            let cx = fx - inset;
            let cy = fy - inset;
            let inside_cell = |v: f32| -> bool {
                let half = (cell_size - gap) / 2.0;
                let center = cell_size / 2.0;
                (v - center).abs() <= half
            };
            if inside_cell(cx) && inside_cell(cy) {
                color = cell;
            }
            let a = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
            px[i] = color.r();
            px[i + 1] = color.g();
            px[i + 2] = color.b();
            px[i + 3] = a;
        }
    }
    px
}

/// Покрытие пикселя скруглённым прямоугольником [x0,x1]×[y0,y1] с радиусом `r` (0..1).
fn rounded_rect_coverage(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return 0.0;
    }
    // Расстояние до ближайшего углового центра.
    let cx = if x < x0 + r {
        x0 + r
    } else if x > x1 - r {
        x1 - r
    } else {
        return 1.0;
    };
    let cy = if y < y0 + r {
        y0 + r
    } else if y > y1 - r {
        y1 - r
    } else {
        return 1.0;
    };
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    (r + 0.5 - d).clamp(0.0, 1.0)
}

/// Цвет включённой плитки ядра.
pub fn on_color(theme: Theme) -> Color32 {
    match theme {
        Theme::Dark => Color32::from_rgb(0x3E, 0x9E, 0x6B),
        Theme::Light => Color32::from_rgb(0x8B, 0xD6, 0xA8),
    }
}

/// Цвет выключенной плитки ядра.
pub fn off_color(theme: Theme) -> Color32 {
    match theme {
        Theme::Dark => Color32::from_rgb(0x3A, 0x3E, 0x45),
        Theme::Light => Color32::from_rgb(0xD8, 0xDB, 0xE0),
    }
}

/// Цвет текста на плитке.
pub fn tile_text_color(theme: Theme, on: bool) -> Color32 {
    match (theme, on) {
        (Theme::Dark, _) => Color32::from_rgb(0xF2, 0xF4, 0xF7),
        (Theme::Light, true) => Color32::from_rgb(0x10, 0x2A, 0x1C),
        (Theme::Light, false) => Color32::from_rgb(0x33, 0x37, 0x3D),
    }
}

/// Рамка плитки: сплошная для физического ядра, пунктирная для виртуального потока.
pub fn tile_stroke(theme: Theme, on: bool) -> Stroke {
    let base = if on {
        match theme {
            Theme::Dark => Color32::from_rgb(0x6C, 0xC8, 0x94),
            Theme::Light => Color32::from_rgb(0x2E, 0x7D, 0x4F),
        }
    } else {
        match theme {
            Theme::Dark => Color32::from_rgb(0x5A, 0x60, 0x6A),
            Theme::Light => Color32::from_rgb(0xA8, 0xAE, 0xB8),
        }
    };
    Stroke::new(1.5, base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_correct_size_and_alpha() {
        for size in [16u32, 32, 64] {
            let px = icon_rgba(size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            // Углы прозрачные, центр непрозрачный.
            assert_eq!(px[3], 0, "левый верхний угол должен быть прозрачным");
            let mid = ((size / 2) * size + size / 2) as usize * 4;
            assert_eq!(px[mid + 3], 255, "центр должен быть непрозрачным");
        }
    }

    #[test]
    fn icon_border_is_transparent() {
        let size = 32u32;
        let px = icon_rgba(size);
        for x in 0..size {
            let i = (x * 4) as usize; // верхняя строка
            assert_eq!(px[i + 3], 0);
        }
    }

    #[test]
    fn rounded_corner_is_cut() {
        assert_eq!(rounded_rect_coverage(0.5, 0.5, 1.0, 1.0, 30.0, 30.0, 6.0), 0.0);
        assert_eq!(rounded_rect_coverage(15.0, 15.0, 1.0, 1.0, 30.0, 30.0, 6.0), 1.0);
        assert!(rounded_rect_coverage(2.5, 2.5, 1.0, 1.0, 30.0, 30.0, 6.0) < 1.0);
    }

    #[test]
    fn tr_substitutes() {
        let s = tf(Lang::Ru, "proc.count", &[("n", "5")]);
        assert_eq!(s, "Процессов: 5");
    }

    #[test]
    fn colors_differ_between_states() {
        for th in [Theme::Light, Theme::Dark] {
            assert_ne!(on_color(th), off_color(th));
        }
    }
}

