//! Вкладка «Настройки»: общие параметры, трей и окно, монитор, «О приложении» и журнал событий.
//!
//! Все изменения сразу помечают конфиг как изменённый (`CoreTunerApp::mark_dirty`) — сохранение
//! и отправка `MonitorCmd::ConfigChanged` выполняются дебаунсом в `app.rs`.
use crate::app::{CoreTunerApp, EventRecord};
use crate::config::{Lang, ThemeMode};
use crate::i18n::{t, tf};
use crate::process::monitor::MonitorCmd;
use crate::ui::{event_color, event_glyph, event_text};
use egui::{RichText, Ui};

/// Дополнительные строки экрана (ключи уникальны среди всех таблиц i18n).
pub const STR: &[(&str, &str, &str)] = &[
    ("settings.ms_suffix", " мс", " ms"),
    ("settings.version", "Версия", "Version"),
    ("settings.restart_admin_hint", "Приложение закроется и запросит повышение прав UAC.",
     "The app will close and ask for UAC elevation."),
    ("settings.open_folder_fail", "Не удалось открыть папку: {e}", "Failed to open the folder: {e}"),
    ("settings.poll_now", "Сейчас: {n} {unit}", "Now: {n} {unit}"),
    ("settings.poll_range", "Допустимо от {min} до {max} мс.", "Allowed range: {min} to {max} ms."),
    ("settings.events_shown", "Показаны последние {n} из {total}", "Showing the last {n} of {total}"),
    ("settings.apply_now", "Применить сейчас", "Apply now"),
    ("settings.apply_now_done", "Профили применяются заново", "Profiles are being re-applied"),
];

/// Состояние экрана «Настройки».
#[derive(Default)]
pub struct State {
    /// Отображаемое состояние автозапуска (истина — запись реестра есть).
    pub autostart: bool,
    /// Нужно переприменить тему.
    pub theme_dirty: bool,
    /// Нужно перестроить меню трея.
    pub lang_dirty: bool,
    /// Значение автозапуска уже прочитано из реестра (`app.rs` заполняет его в конструкторе).
    pub autostart_synced: bool,
}

/// Нижняя граница интервала опроса, мс.
pub const POLL_MIN: u64 = 500;
/// Верхняя граница интервала опроса, мс.
pub const POLL_MAX: u64 = 10_000;
/// Шаг слайдера интервала опроса, мс.
const POLL_STEP: u64 = 100;
/// Сколько последних событий показывать в журнале.
const EVENT_ROWS: usize = 100;

/// Интервал опроса, приведённый к допустимому диапазону.
pub fn clamp_interval_ms(ms: u64) -> u64 {
    ms.clamp(POLL_MIN, POLL_MAX)
}

/// Интервал с шагом: значение всегда кратно [`POLL_STEP`] и лежит в допустимых границах.
/// UI использует `Slider::step_by`, поэтому функция нужна только тестам.
#[cfg(test)]
pub fn snap_interval_ms(ms: u64) -> u64 {
    clamp_interval_ms(ms).div_ceil(POLL_STEP) * POLL_STEP
}

/// Подпись значения интервала в интерфейсе (используется только тестами).
#[cfg(test)]
pub fn interval_label(lang: Lang, ms: u64) -> String {
    let unit = t(lang, "settings.ms_suffix").trim();
    tf(lang, "settings.poll_now", &[("n", &ms.to_string()), ("unit", unit)])
}

/// Подпись темы.
pub fn theme_label(lang: Lang, m: ThemeMode) -> &'static str {
    match m {
        ThemeMode::Light => t(lang, "settings.theme.light"),
        ThemeMode::Dark => t(lang, "settings.theme.dark"),
        ThemeMode::System => t(lang, "settings.theme.system"),
    }
}

/// Подпись языка.
pub fn lang_label(lang: Lang, m: Lang) -> &'static str {
    match m {
        Lang::Ru => t(lang, "settings.lang.ru"),
        Lang::En => t(lang, "settings.lang.en"),
    }
}

/// Подпись состояния прав администратора.
pub fn admin_label(lang: Lang, elevated: bool) -> &'static str {
    if elevated {
        t(lang, "settings.admin_yes")
    } else {
        t(lang, "settings.admin_no")
    }
}

/// Множество имён подстановок `{name}` в шаблоне (используется только тестами).
#[cfg(test)]
pub fn placeholders(template: &str) -> Vec<&str> {
    template
        .split('{')
        .skip(1)
        .filter_map(|rest| rest.split('}').next())
        .collect()
}

/// Сводка журнала: сколько записей показано из всего списка.
pub fn events_summary(lang: Lang, shown: usize, total: usize) -> String {
    tf(lang, "settings.events_shown", &[("n", &shown.to_string()), ("total", &total.to_string())])
}

/// Как показывать папку конфигурации средствами системы (без блокировки UI).
fn open_folder(path: &str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Отрисовать карточку раздела с заголовком.
fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong().size(15.0));
        ui.add_space(4.0);
        body(ui);
    });
}

/// Флажок с подсказкой под ним. Возвращает `true`, если значение изменилось.
fn check(ui: &mut Ui, value: &mut bool, label: &str, hint: &str) -> bool {
    let changed = ui.checkbox(value, label).changed();
    if !hint.is_empty() {
        ui.label(RichText::new(hint).small().weak());
    }
    ui.add_space(4.0);
    changed
}

/// Вкладка «Настройки».
pub fn show(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    ui.add_space(4.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if !st.autostart_synced {
            st.autostart_synced = true;
            st.autostart = crate::autostart::is_enabled();
        }
        general(ui, app, st);
        ui.add_space(10.0);
        tray_and_window(ui, app, st);
        ui.add_space(10.0);
        engine(ui, app);
        ui.add_space(10.0);
        about(ui, app);
        ui.add_space(10.0);
        events(ui, app);
        ui.add_space(8.0);
    });
}

/// Раздел «Общие»: автозапуск, язык, тема.
fn general(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    section(ui, t(lang, "settings.section.general"), |ui| {
        // Автозапуск: отображаемое состояние живёт в `st`, реестр — источник истины только при ошибке.
        if check(
            ui,
            &mut st.autostart,
            t(lang, "settings.autostart"),
            t(lang, "settings.autostart_hint"),
        ) {
            let on = st.autostart;
            match crate::autostart::set_enabled(on, app.cfg.settings.start_minimized) {
                Ok(()) => {
                    app.cfg.settings.autostart = on;
                    app.mark_dirty();
                    let msg = app.text("settings.autostart_ok").to_string();
                    app.toast(msg, false);
                }
                Err(e) => {
                    st.autostart = !on;
                    app.cfg.settings.autostart = st.autostart;
                    let msg = app.fmt("settings.autostart_fail", &[("e", &format!("{e:#}"))]);
                    app.toast(msg, true);
                }
            }
        }

        ui.horizontal(|ui| {
            ui.label(t(lang, "settings.lang"));
            let mut lang_lang = app.cfg.settings.lang;
            for m in [Lang::Ru, Lang::En] {
                ui.selectable_value(&mut lang_lang, m, lang_label(lang, m));
            }
            if lang_lang != app.cfg.settings.lang {
                app.cfg.settings.lang = lang_lang;
                st.lang_dirty = true;
                app.mark_dirty();
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(t(lang, "settings.theme"));
            let mut theme = app.cfg.settings.theme;
            egui::ComboBox::from_id_salt("ct_settings_theme")
                .selected_text(theme_label(lang, theme))
                .show_ui(ui, |ui| {
                    for m in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System] {
                        ui.selectable_value(&mut theme, m, theme_label(lang, m));
                    }
                });
            if theme != app.cfg.settings.theme {
                app.cfg.settings.theme = theme;
                st.theme_dirty = true;
                app.mark_dirty();
            }
        });
    });
}

/// Раздел «Трей и окно»: четыре обязательных флажка.
fn tray_and_window(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    section(ui, t(lang, "settings.section.tray"), |ui| {
        ui.label(RichText::new(t(lang, "settings.autostart")).small().weak());
        if check(ui, &mut st.autostart, "", t(lang, "settings.autostart_hint")) {
            let on = st.autostart;
            match crate::autostart::set_enabled(on, app.cfg.settings.start_minimized) {
                Ok(()) => {
                    app.cfg.settings.autostart = on;
                    app.mark_dirty();
                }
                Err(e) => {
                    st.autostart = !on;
                    app.cfg.settings.autostart = st.autostart;
                    let msg = app.fmt("settings.autostart_fail", &[("e", &format!("{e:#}"))]);
                    app.toast(msg, true);
                }
            }
        }

        let changed = check(
            ui,
            &mut app.cfg.settings.minimize_to_tray_on_close,
            t(lang, "settings.minimize_to_tray"),
            t(lang, "settings.minimize_to_tray_hint"),
        ) | check(
            ui,
            &mut app.cfg.settings.always_show_tray,
            t(lang, "settings.always_show_tray"),
            t(lang, "settings.always_show_tray_hint"),
        );
        if changed {
            app.mark_dirty();
            app.update_tray_visibility(!app.window_visible);
        }

        if check(
            ui,
            &mut app.cfg.settings.start_minimized,
            t(lang, "settings.start_minimized"),
            t(lang, "settings.start_minimized_hint"),
        ) {
            // При включённом автозапуске командная строка в реестре содержит флаг --minimized.
            if st.autostart {
                match crate::autostart::set_enabled(true, app.cfg.settings.start_minimized) {
                    Ok(()) => app.cfg.settings.autostart = true,
                    Err(e) => {
                        let msg = app.fmt("settings.autostart_fail", &[("e", &format!("{e:#}"))]);
                        app.toast(msg, true);
                    }
                }
            }
            app.mark_dirty();
        }
    });
}

/// Раздел «Монитор и автоприменение»: интервал опроса, повторное применение, пауза.
fn engine(ui: &mut Ui, app: &mut CoreTunerApp) {
    let lang = app.lang();
    section(ui, t(lang, "settings.section.engine"), |ui| {
        ui.label(t(lang, "settings.poll_interval"));
        let mut ms = clamp_interval_ms(app.cfg.settings.poll_interval_ms);
        ui.horizontal(|ui| {
            let slider = egui::Slider::new(&mut ms, POLL_MIN..=POLL_MAX).step_by(POLL_STEP as f64);
            if ui.add(slider).changed() {
                set_interval(app, ms);
            }
            let mut exact = app.cfg.settings.poll_interval_ms;
            let drag = egui::DragValue::new(&mut exact)
                .range(POLL_MIN..=POLL_MAX)
                .speed(POLL_STEP as f64)
                .suffix(t(lang, "settings.ms_suffix"));
            if ui.add(drag).changed() {
                set_interval(app, exact);
            }
        });
        ui.label(RichText::new(t(lang, "settings.poll_interval_hint")).small().weak());
        let range = tf(
            lang,
            "settings.poll_range",
            &[("min", &POLL_MIN.to_string()), ("max", &POLL_MAX.to_string())],
        );
        ui.label(RichText::new(range).small().weak());
        ui.add_space(4.0);

        let mut changed = check(ui, &mut app.cfg.settings.reapply_enforce, t(lang, "settings.reapply"), "");
        changed |= check(ui, &mut app.cfg.settings.notify_on_apply, t(lang, "settings.notify"), "");
        if changed {
            app.mark_dirty();
        }

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let paused = app.cfg.settings.paused;
            let label = if paused { t(lang, "settings.resume") } else { t(lang, "settings.pause") };
            if ui.button(label).clicked() {
                app.set_paused(!paused);
            }
            // Повторное применение ко всем подходящим процессам, не дожидаясь тика
            // монитора: сбрасывает учёт уже применённых профилей (Monitor::retrack).
            if ui.button(t(lang, "settings.apply_now")).clicked() {
                let _ = app.cmd_tx.send(MonitorCmd::ReapplyNow);
                let msg = app.text("settings.apply_now_done").to_string();
                app.toast(msg, false);
            }
            if paused {
                ui.label(RichText::new(t(lang, "settings.paused")).color(ui.visuals().warn_fg_color));
            }
        });
    });
}

/// Записать новый интервал опроса в конфиг и сообщить монитору.
fn set_interval(app: &mut CoreTunerApp, ms: u64) {
    let ms = clamp_interval_ms(ms);
    if app.cfg.settings.poll_interval_ms == ms {
        return;
    }
    app.cfg.settings.poll_interval_ms = ms;
    let _ = app.cmd_tx.send(MonitorCmd::SetInterval(ms));
    app.mark_dirty();
}

/// Раздел «О приложении»: версия, права, папка конфигурации.
fn about(ui: &mut Ui, app: &mut CoreTunerApp) {
    let lang = app.lang();
    section(ui, t(lang, "settings.section.about"), |ui| {
        ui.label(t(lang, "settings.about_text"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(t(lang, "settings.version"));
            ui.label(RichText::new(env!("CARGO_PKG_VERSION")).monospace());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(t(lang, "settings.admin"));
            if app.elevated {
                ui.label(RichText::new(admin_label(lang, true)).color(ui.visuals().hyperlink_color));
            } else {
                ui.label(RichText::new(admin_label(lang, false)).color(ui.visuals().warn_fg_color));
                if ui.button(t(lang, "cores.restart_admin")).clicked() {
                    app.restart_as_admin();
                }
            }
        });
        if !app.elevated {
            ui.label(RichText::new(t(lang, "settings.restart_admin_hint")).small().weak());
        }
        ui.add_space(4.0);
        let dir = app.config_dir.clone();
        ui.horizontal(|ui| {
            ui.label(t(lang, "settings.config_dir"));
            ui.label(RichText::new(&dir).monospace());
            if ui.button(t(lang, "settings.open_folder")).clicked() {
                if let Err(e) = open_folder(&dir) {
                    let msg = app.fmt("settings.open_folder_fail", &[("e", &e)]);
                    app.toast(msg, true);
                }
            }
        });
    });
}

/// Журнал событий: новые записи сверху, прокручиваемая область.
fn events(ui: &mut Ui, app: &mut CoreTunerApp) {
    let lang = app.lang();
    section(ui, t(lang, "events.title"), |ui| {
        let total = app.events.len();
        let shown = total.min(EVENT_ROWS);
        ui.horizontal(|ui| {
            ui.label(RichText::new(events_summary(lang, shown, total)).small().weak());
            if ui.add_enabled(total > 0, egui::Button::new(t(lang, "events.clear"))).clicked() {
                app.events.clear();
            }
        });
        ui.add_space(2.0);
        if total == 0 {
            ui.label(RichText::new(t(lang, "events.empty")).weak());
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("ct_events_log")
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for e in app.events.iter().rev().take(EVENT_ROWS) {
                    event_row(ui, e);
                }
            });
    });
}

/// Одна строка журнала: время, значок, текст (цвет зависит от типа события).
fn event_row(ui: &mut Ui, e: &EventRecord) {
    let color = event_color(ui, &e.kind);
    ui.horizontal(|ui| {
        ui.label(RichText::new(e.time.as_str()).monospace().small().weak());
        ui.label(RichText::new(event_glyph(&e.kind)).color(color));
        ui.label(RichText::new(event_text(e)).color(color));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::EventKind;
    use std::collections::HashSet;

    #[test]
    fn keys_are_unique_and_translated() {
        let mut seen = HashSet::new();
        for (k, ru, en) in STR {
            assert!(seen.insert(*k), "дубликат ключа: {k}");
            assert!(!ru.trim().is_empty(), "пустой RU для {k}");
            assert!(!en.trim().is_empty(), "пустой EN для {k}");
            assert_eq!(placeholders(ru), placeholders(en), "разные подстановки в ключе {k}");
        }
    }

    #[test]
    fn all_screen_strings_exist_in_tables() {
        for (k, _, _) in STR {
            assert_ne!(t(Lang::Ru, k), *k, "нет RU-строки для {k}");
            assert_ne!(t(Lang::En, k), *k, "нет EN-строки для {k}");
        }
    }

    #[test]
    fn interval_is_clamped() {
        assert_eq!(clamp_interval_ms(0), POLL_MIN);
        assert_eq!(clamp_interval_ms(499), POLL_MIN);
        assert_eq!(clamp_interval_ms(500), 500);
        assert_eq!(clamp_interval_ms(3_000), 3_000);
        assert_eq!(clamp_interval_ms(10_000), 10_000);
        assert_eq!(clamp_interval_ms(99_999), POLL_MAX);
    }

    #[test]
    fn interval_is_snapped_to_step() {
        assert_eq!(snap_interval_ms(0), POLL_MIN);
        assert_eq!(snap_interval_ms(501), 600);
        assert_eq!(snap_interval_ms(1_500), 1_500);
        assert_eq!(snap_interval_ms(99_999), POLL_MAX);
    }

    #[test]
    fn interval_label_is_localized() {
        assert_eq!(interval_label(Lang::Ru, 1_500), "Сейчас: 1500 мс");
        assert_eq!(interval_label(Lang::En, 1_500), "Now: 1500 ms");
    }

    #[test]
    fn theme_labels_are_localized() {
        for lang in [Lang::Ru, Lang::En] {
            for m in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System] {
                let l = theme_label(lang, m);
                assert!(!l.is_empty());
                assert!(!l.starts_with("settings."), "нет строки для темы: {l}");
            }
        }
    }

    #[test]
    fn lang_and_admin_labels_are_localized() {
        assert_eq!(lang_label(Lang::Ru, Lang::Ru), "Русский");
        assert_eq!(lang_label(Lang::En, Lang::En), "English");
        assert_ne!(admin_label(Lang::Ru, true), admin_label(Lang::Ru, false));
        assert_eq!(admin_label(Lang::Ru, true), t(Lang::Ru, "settings.admin_yes"));
    }

    #[test]
    fn events_summary_counts() {
        assert_eq!(events_summary(Lang::Ru, 3, 7), "Показаны последние 3 из 7");
        assert_eq!(events_summary(Lang::En, 0, 0), "Showing the last 0 of 0");
    }

    #[test]
    fn event_glyphs_are_distinct() {
        let mut seen = HashSet::new();
        for k in [EventKind::Applied, EventKind::Failed, EventKind::ProfileSwitched, EventKind::Info] {
            assert!(seen.insert(event_glyph(&k)), "повтор значка события");
        }
    }
}
