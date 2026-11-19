//! Локализация интерфейса: таблица строк RU/EN, все строки UI идут через `t()`.
//!
//! Ключи хранятся в одной статической таблице `[(key, ru, en)]`; `t()` ищет ключ линейным
//! поиском. При отсутствии ключа возвращается сам ключ — это заметно в UI и не паникует.

use crate::config::Lang;

/// Единственная таблица строк. Порядок — по группам разделов.
pub const STRINGS: &[(&str, &str, &str)] = &[
    // --- общее / приложение
    ("app.title", "CoreTuner", "CoreTuner"),
    ("app.subtitle", "Ядра и приоритеты процессов", "CPU cores and process priorities"),
    ("tab.processes", "Процессы", "Processes"),
    ("tab.by_file", "По файлу запуска", "By executable"),
    ("tab.profiles", "Профили", "Profiles"),
    ("tab.settings", "Настройки", "Settings"),
    ("common.ok", "OK", "OK"),
    ("common.cancel", "Отмена", "Cancel"),
    ("common.yes", "Да", "Yes"),
    ("common.no", "Нет", "No"),
    ("common.close", "Закрыть", "Close"),
    ("common.delete", "Удалить", "Delete"),
    ("common.rename", "Переименовать", "Rename"),
    ("common.duplicate", "Дублировать", "Duplicate"),
    ("common.save", "Сохранить", "Save"),
    ("common.add", "Добавить", "Add"),
    ("common.none", "—", "—"),
    ("common.not_selected", "Процесс не выбран", "No process selected"),
    ("common.enabled", "включено", "enabled"),
    ("common.disabled", "выключено", "disabled"),
    ("common.on", "Вкл", "On"),
    ("common.off", "Выкл", "Off"),
    ("common.name", "Имя", "Name"),
    ("common.path", "Путь", "Path"),
    ("common.search", "Поиск", "Search"),
    ("common.refresh", "Обновить", "Refresh"),
    ("common.error", "Ошибка", "Error"),
    ("common.warning", "Предупреждение", "Warning"),
    ("common.unknown", "неизвестно", "unknown"),
    ("common.copied", "Скопировано", "Copied"),
    ("common.clear", "Очистить", "Clear"),
    ("common.apply", "Применить", "Apply"),
    ("common.dismiss", "Скрыть", "Dismiss"),
    ("common.confirm", "Подтвердить", "Confirm"),
    ("common.any", "Любой", "Any"),
    ("common.keep", "Не менять", "Keep"),
    ("common.column", "Колонка", "Column"),
    ("common.ascending", "По возрастанию", "Ascending"),
    ("common.descending", "По убыванию", "Descending"),
    // --- вкладка «Процессы»
    ("proc.search_hint", "Поиск по имени или PID…", "Search by name or PID…"),
    ("proc.only_with_window", "Только с окном", "Only with window"),
    ("proc.col.name", "Имя", "Name"),
    ("proc.col.pid", "PID", "PID"),
    ("proc.col.cpu", "CPU %", "CPU %"),
    ("proc.col.ram", "RAM", "RAM"),
    ("proc.col.priority", "Приоритет", "Priority"),
    ("proc.col.affinity", "Маска ядер", "Affinity"),
    ("proc.col.status", "Статус", "Status"),
    ("proc.count", "Процессов: {n}", "Processes: {n}"),
    ("proc.showing", "Показано {n} из {total}", "Showing {n} of {total}"),
    ("proc.empty", "Процессы не найдены", "No processes found"),
    ("proc.loading", "Ожидание данных монитора…", "Waiting for monitor data…"),
    ("proc.refresh_now", "Обновить сейчас", "Refresh now"),
    ("proc.sort_hint", "Клик по заголовку — сортировка", "Click a header to sort"),
    ("proc.no_access", "нет доступа", "no access"),
    ("proc.protected", "защищён", "protected"),
    ("proc.accessible", "доступен", "accessible"),
    ("proc.no_window", "без окна", "no window"),
    ("proc.self", "это приложение", "this app"),
    ("proc.affinity_all", "все ({n})", "all ({n})"),
    ("proc.affinity_none", "не задана", "not set"),
    ("proc.affinity_count", "{n} из {total}", "{n} of {total}"),
    ("proc.affinity_multi", "{n} ядер в {g} группах", "{n} CPUs in {g} groups"),
    ("proc.unknown_path", "путь недоступен", "path unavailable"),
    // --- вкладка «По файлу запуска»
    ("file.title", "Привязка к файлу запуска", "Bind to an executable"),
    ("file.hint", "Выберите .exe или перетащите файл в окно. Настройки применятся ко всем \
                  будущим запускам, даже если процесс ещё не запущен.",
     "Pick an .exe or drop a file onto the window. Settings will apply to every future launch, \
      even if the process is not running yet."),
    ("file.pick", "Выбрать .exe…", "Choose .exe…"),
    ("file.drop_hint", "Перетащите .exe сюда", "Drop an .exe here"),
    ("file.target", "Цель", "Target"),
    ("file.mode", "Режим сопоставления", "Match mode"),
    ("file.mode.name", "По имени exe", "By executable name"),
    ("file.mode.path", "По полному пути", "By full path"),
    ("file.mode.name_hint", "Совпадение по имени файла без учёта регистра.",
     "Matches the file name, case-insensitive."),
    ("file.mode.path_hint", "Совпадение по полному пути без учёта регистра и слэшей.",
     "Matches the full path, ignoring case and slashes."),
    ("file.choose_profile", "Профиль для привязки", "Profile to bind"),
    ("file.create_binding", "Создать привязку", "Create binding"),
    ("file.update_binding", "Сохранить привязку", "Save binding"),
    ("file.remove_binding", "Удалить привязку", "Remove binding"),
    ("file.bindings", "Существующие привязки", "Existing bindings"),
    ("file.no_bindings", "Привязок пока нет", "No bindings yet"),
    ("file.need_target", "Укажите файл или имя exe", "Provide a file or an executable name"),
    ("file.need_profile", "Выберите хотя бы один профиль", "Select at least one profile"),
    ("file.need_profiles", "Сначала создайте профиль на вкладке «Профили»",
     "Create a profile on the Profiles tab first"),
    ("file.binding_added", "Привязка создана", "Binding created"),
    ("file.binding_updated", "Привязка обновлена", "Binding updated"),
    ("file.binding_removed", "Привязка удалена", "Binding removed"),
    ("file.manual_name", "или введите имя exe вручную", "or type the executable name manually"),
    ("file.active", "Активный профиль", "Active profile"),
    ("file.triggers", "Триггеры", "Triggers"),
    ("file.triggers_count", "триггеров: {n}", "triggers: {n}"),
    ("file.applications", "Приложений: {n}", "Applications: {n}"),
    ("file.none_profile", "Профиль не назначен", "No profile assigned"),
    // --- вкладка «Профили»
    ("profiles.title", "Профили", "Profiles"),
    ("profiles.new", "Новый профиль", "New profile"),
    ("profiles.new_name", "Новый профиль", "New profile"),
    ("profiles.rename", "Переименовать", "Rename"),
    ("profiles.duplicate", "Дублировать", "Duplicate"),
    ("profiles.delete", "Удалить", "Delete"),
    ("profiles.export", "Экспорт JSON…", "Export JSON…"),
    ("profiles.import", "Импорт JSON…", "Import JSON…"),
    ("profiles.list", "Список профилей", "Profile list"),
    ("profiles.empty", "Профилей пока нет. Создайте первый.", "No profiles yet. Create the first one."),
    ("profiles.select", "Выберите профиль слева", "Select a profile on the left"),
    ("profiles.name", "Название", "Name"),
    ("profiles.name_required", "Название не может быть пустым", "Name cannot be empty"),
    ("profiles.affinity", "Маска ядер", "CPU affinity"),
    ("profiles.affinity_keep", "Не менять маску", "Do not change affinity"),
    ("profiles.affinity_empty", "Пустая маска недопустима", "Empty affinity is not allowed"),
    ("profiles.priority", "Приоритет", "Priority"),
    ("profiles.io_priority", "Приоритет ввода-вывода", "I/O priority"),
    ("profiles.mem_priority", "Приоритет памяти", "Memory priority"),
    ("profiles.apps", "Приложения с этим профилем", "Applications using this profile"),
    ("profiles.no_apps", "Профиль не привязан к приложениям", "Not bound to any application"),
    ("profiles.created", "Профиль создан", "Profile created"),
    ("profiles.deleted", "Профиль удалён", "Profile deleted"),
    ("profiles.duplicated", "Профиль дублирован", "Profile duplicated"),
    ("profiles.renamed", "Профиль переименован", "Profile renamed"),
    ("profiles.exported", "Профили экспортированы", "Profiles exported"),
    ("profiles.imported", "Импортировано профилей: {n}", "Imported profiles: {n}"),
    ("profiles.delete_confirm", "Удалить профиль «{name}»? Он будет убран из привязок.",
     "Delete profile “{name}”? It will be removed from bindings."),
    ("profiles.bindings", "Привязки к приложениям", "Application bindings"),
    ("profiles.active_profile", "Активный профиль", "Active profile"),
    ("profiles.active_for", "Активный профиль для", "Active profile for"),
    ("profiles.trigger_rule", "При запуске → {on_start}; при закрытии → {on_exit}",
     "On start → {on_start}; on exit → {on_exit}"),
    ("profiles.enabled", "Включена", "Enabled"),
    ("profiles.used_by", "используется в {n} привязках", "used in {n} bindings"),
    // --- панель ядер и приоритета
    ("cores.title", "Ядра и приоритет", "Cores and priority"),
    ("cores.process", "Процесс", "Process"),
    ("cores.core", "Core {n}", "Core {n}"),
    ("cores.core_p", "P-ядро", "P-core"),
    ("cores.core_e", "E-ядро", "E-core"),
    ("cores.thread", "Thread {n}", "Thread {n}"),
    ("cores.thread_virtual", "Thread {n} (virtual)", "Thread {n} (virtual)"),
    ("cores.thread_tip", "Логический процессор #{n} · группа {g} · бит {b}",
     "Logical CPU #{n} · group {g} · bit {b}"),
    ("cores.virtual_badge", "V", "V"),
    ("cores.legend_on", "включено", "enabled"),
    ("cores.legend_off", "выключено", "disabled"),
    ("cores.legend_core", "физическое ядро", "physical core"),
    ("cores.legend_virtual", "виртуальный поток (SMT)", "virtual thread (SMT)"),
    ("cores.legend_load", "загрузка потока", "thread load"),
    ("cores.btn_all", "Все", "All"),
    ("cores.btn_physical", "Только физические", "Physical only"),
    ("cores.btn_virtual", "Только виртуальные", "Virtual only"),
    ("cores.btn_invert", "Инвертировать", "Invert"),
    ("cores.apply_now", "Применять сразу", "Apply immediately"),
    ("cores.apply", "Применить", "Apply"),
    ("cores.apply_to", "Применить к PID {pid}", "Apply to PID {pid}"),
    ("cores.empty_mask", "Маска пуста — выберите хотя бы одно ядро", "Empty mask — select at least one core"),
    ("cores.applied", "Настройки применены к PID {pid}", "Settings applied to PID {pid}"),
    ("cores.affinity_applied", "Маска ядер применена", "Affinity applied"),
    ("cores.priority_applied", "Приоритет применён", "Priority applied"),
    ("cores.need_selection", "Сначала выберите процесс", "Select a process first"),
    ("cores.no_topology", "Топология не определена", "Topology unavailable"),
    ("cores.priority", "Приоритет", "Priority"),
    ("cores.raise", "Повысить", "Raise"),
    ("cores.lower", "Понизить", "Lower"),
    ("cores.io_priority", "Ввод-вывод", "I/O"),
    ("cores.mem_priority", "Память", "Memory"),
    ("cores.realtime_title", "Включить приоритет Realtime?", "Enable Realtime priority?"),
    ("cores.realtime_warning", "Приоритет Realtime может сделать систему неотзывчивой: \
        процесс получит почти всё время CPU. Продолжить?",
     "Realtime priority can make the system unresponsive: the process gets almost all CPU time. \
      Continue?"),
    ("cores.access_denied", "Нет доступа к процессу. Запустите CoreTuner от администратора.",
     "Access denied for this process. Run CoreTuner as administrator."),
    ("cores.restart_admin", "Перезапустить от администратора", "Restart as administrator"),
    ("cores.already_admin", "Уже запущено от администратора", "Already running as administrator"),
    ("cores.protected_note", "Процесс защищён системой — изменение настроек отключено.",
     "This process is protected by the system — editing is disabled."),
    ("cores.no_access_note", "Процесс недоступен — изменение настроек отключено.",
     "This process is not accessible — editing is disabled."),
    ("cores.current_affinity", "Текущая маска", "Current affinity"),
    ("cores.save_as_profile", "Сохранить как профиль", "Save as profile"),
    ("cores.bind_to_app", "Привязать к приложению", "Bind to application"),
    ("cores.save_profile_title", "Сохранить профиль", "Save profile"),
    ("cores.save_profile_name", "Имя профиля", "Profile name"),
    ("cores.bind_title", "Привязать к приложению", "Bind to application"),
    ("cores.bind_target", "Имя exe", "Executable name"),
    ("cores.bound", "Профиль «{name}» привязан к {target}", "Profile “{name}” bound to {target}"),
    ("cores.saved_profile", "Профиль «{name}» сохранён", "Profile “{name}” saved"),
    ("cores.mask_summary", "Включено {on} из {total}", "{on} of {total} enabled"),
    ("cores.instant_hint", "Изменения применяются сразу при клике по ядру.",
     "Changes apply instantly when you click a core."),
    ("cores.loading", "загрузка…", "loading…"),
    // --- настройки
    ("settings.title", "Настройки", "Settings"),
    ("settings.autostart", "Запускать вместе с Windows", "Start with Windows"),
    ("settings.autostart_hint", "Запись в HKCU\\…\\Run с флагом --minimized.",
     "Adds an HKCU\\…\\Run entry with the --minimized flag."),
    ("settings.minimize_to_tray", "Сворачивать в трей при закрытии", "Minimize to tray on close"),
    ("settings.minimize_to_tray_hint", "Кнопка × скрывает окно, приложение продолжает работать.",
     "The × button hides the window while the app keeps running."),
    ("settings.always_show_tray", "Всегда показывать значок в трее", "Always show tray icon"),
    ("settings.always_show_tray_hint", "Иначе значок виден только когда окно скрыто.",
     "Otherwise the icon is visible only while the window is hidden."),
    ("settings.start_minimized", "Запускать свёрнутым в трей", "Start minimized to tray"),
    ("settings.start_minimized_hint", "Применяется при запуске с флагом --minimized.",
     "Used when launched with the --minimized flag."),
    ("settings.lang", "Язык интерфейса", "Interface language"),
    ("settings.lang.ru", "Русский", "Russian"),
    ("settings.lang.en", "Английский", "English"),
    ("settings.theme", "Тема", "Theme"),
    ("settings.theme.light", "Светлая", "Light"),
    ("settings.theme.dark", "Тёмная", "Dark"),
    ("settings.theme.system", "Системная", "System"),
    ("settings.poll_interval", "Интервал опроса процессов, мс", "Process poll interval, ms"),
    ("settings.poll_interval_hint", "От 500 до 10000 мс. Меньше — точнее, но дороже.",
     "From 500 to 10000 ms. Smaller is more precise but costs more."),
    ("settings.reapply", "Повторно применять настройки, если процесс их сбросил",
     "Re-apply settings if the process resets them"),
    ("settings.notify", "Уведомлять об автоприменении", "Notify on automatic apply"),
    ("settings.paused", "Автоприменение приостановлено", "Auto-apply is paused"),
    ("settings.resume", "Возобновить автоприменение", "Resume auto-apply"),
    ("settings.pause", "Приостановить автоприменение", "Pause auto-apply"),
    ("settings.autostart_ok", "Автозапуск обновлён", "Autostart updated"),
    ("settings.autostart_fail", "Не удалось изменить автозапуск: {e}", "Failed to update autostart: {e}"),
    ("settings.saved", "Настройки сохранены", "Settings saved"),
    ("settings.config_dir", "Папка конфигурации", "Config folder"),
    ("settings.open_folder", "Открыть папку", "Open folder"),
    ("settings.section.general", "Общие", "General"),
    ("settings.section.tray", "Трей и окно", "Tray and window"),
    ("settings.section.engine", "Монитор и автоприменение", "Monitor and auto-apply"),
    ("settings.section.about", "О приложении", "About"),
    ("settings.about_text", "CoreTuner 1.0 — управление ядрами и приоритетами процессов Windows.",
     "CoreTuner 1.0 — CPU affinity and priority manager for Windows."),
    ("settings.admin", "Права администратора", "Administrator rights"),
    ("settings.admin_yes", "есть", "granted"),
    ("settings.admin_no", "нет", "not granted"),
    ("settings.language_restart", "Язык применяется сразу", "Language applies immediately"),
    // --- события
    ("events.title", "События", "Events"),
    ("events.empty", "Событий пока нет", "No events yet"),
    ("events.applied", "Применено: {name} ({pid}) ← {profile}", "Applied: {name} ({pid}) ← {profile}"),
    ("events.failed", "Не удалось: {name} ({pid}) — {reason}", "Failed: {name} ({pid}) — {reason}"),
    ("events.switched", "Профиль переключён: {profile}", "Profile switched: {profile}"),
    ("events.clear", "Очистить журнал", "Clear log"),
    ("events.show", "Журнал событий", "Event log"),
    ("events.count", "событий: {n}", "events: {n}"),
    ("events.toast_applied", "Профиль «{profile}» применён к {name}", "Profile “{profile}” applied to {name}"),
    ("events.toast_failed", "Не удалось применить «{profile}» к {name}", "Failed to apply “{profile}” to {name}"),
    // --- трей
    ("tray.open", "Открыть", "Open"),
    ("tray.profiles", "Профили", "Profiles"),
    ("tray.no_profiles", "Нет профилей", "No profiles"),
    ("tray.pause", "Приостановить автоприменение", "Pause auto-apply"),
    ("tray.exit", "Выход", "Exit"),
    ("tray.tooltip", "CoreTuner — ядра и приоритеты", "CoreTuner — cores and priorities"),
    ("tray.not_available", "Трей недоступен: {e}", "Tray unavailable: {e}"),
    ("tray.switched", "Активный профиль: {name}", "Active profile: {name}"),
    ("tray.paused", "Автоприменение приостановлено", "Auto-apply paused"),
    ("tray.resumed", "Автоприменение возобновлено", "Auto-apply resumed"),
    // --- приоритеты
    ("prio.idle", "Idle", "Idle"),
    ("prio.below", "Below Normal", "Below Normal"),
    ("prio.normal", "Normal", "Normal"),
    ("prio.above", "Above Normal", "Above Normal"),
    ("prio.high", "High", "High"),
    ("prio.realtime", "Realtime", "Realtime"),
    ("io.very_low", "Very Low", "Very Low"),
    ("io.low", "Low", "Low"),
    ("io.normal", "Normal", "Normal"),
    ("io.high", "High", "High"),
    ("mem.very_low", "Very Low", "Very Low"),
    ("mem.low", "Low", "Low"),
    ("mem.medium", "Medium", "Medium"),
    ("mem.below_normal", "Below Normal", "Below Normal"),
    ("mem.normal", "Normal", "Normal"),
    // --- ошибки
    ("err.apply", "Не удалось применить: {e}", "Failed to apply: {e}"),
    ("err.save", "Не удалось сохранить конфиг: {e}", "Failed to save config: {e}"),
    ("err.load", "Не удалось загрузить: {e}", "Failed to load: {e}"),
    ("err.export", "Не удалось экспортировать: {e}", "Export failed: {e}"),
    ("err.import", "Не удалось импортировать: {e}", "Import failed: {e}"),
    ("err.restart_admin", "Не удалось перезапустить: {e}", "Restart failed: {e}"),
    ("err.dialog", "Диалог недоступен: {e}", "Dialog unavailable: {e}"),
    ("err.monitor", "Монитор процессов остановился", "The process monitor has stopped"),
];

/// Подстановка `{name}`-параметров в уже локализованную строку.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in args {
        let pat = format!("{{{k}}}");
        out = out.replace(&pat, v);
    }
    out
}

/// Все таблицы строк: общая и по одной на каждый экран UI (каждый файл UI владеет своей `STR`).
pub fn tables() -> [&'static [(&'static str, &'static str, &'static str)]; 7] {
    [
        STRINGS,
        crate::ui::STR,
        crate::ui::cores::STR,
        crate::ui::processes::STR,
        crate::ui::file::STR,
        crate::ui::profiles::STR,
        crate::ui::settings::STR,
    ]
}

fn find(key: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    tables().into_iter().flat_map(|t| t.iter()).find(|s| s.0 == key)
}

/// Строка по ключу; при отсутствии ключа возвращается сам ключ.
pub fn t(lang: Lang, key: &'static str) -> &'static str {
    match (find(key), lang) {
        (Some(s), Lang::Ru) => s.1,
        (Some(s), Lang::En) => s.2,
        (None, _) => key,
    }
}

/// Строка по ключу с подстановкой параметров `{name}`.
pub fn tf(lang: Lang, key: &'static str, args: &[(&str, &str)]) -> String {
    fill(t(lang, key), args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Имена подстановок `{name}` в шаблоне, отсортированные.
    ///
    /// Учитываются только имена внутри фигурных скобок: текст после `{` сравнивать
    /// нельзя, он различается между языками — «n из total» и «n of total».
    fn placeholders_of(template: &str) -> Vec<&str> {
        let mut names: Vec<&str> = template
            .split('{')
            .skip(1)
            .filter_map(|rest| rest.split('}').next())
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn no_duplicate_keys() {
        let mut seen = HashSet::new();
        for (k, _, _) in tables().into_iter().flatten() {
            assert!(seen.insert(*k), "дубликат ключа i18n: {k}");
        }
    }

    #[test]
    fn every_key_has_both_languages() {
        for (k, ru, en) in tables().into_iter().flatten() {
            assert!(!ru.trim().is_empty(), "пустой RU для {k}");
            assert!(!en.trim().is_empty(), "пустой EN для {k}");
        }
    }

    #[test]
    fn t_returns_fallback_for_missing_key() {
        assert_eq!(t(Lang::Ru, "no.such.key"), "no.such.key");
        assert_eq!(t(Lang::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn t_returns_language_variant() {
        assert_eq!(t(Lang::Ru, "tab.processes"), "Процессы");
        assert_eq!(t(Lang::En, "tab.processes"), "Processes");
    }

    #[test]
    fn priority_keys_exist() {
        for p in crate::model::PriorityClass::ALL {
            assert_ne!(t(Lang::Ru, p.key()), p.key(), "нет строки для {}", p.key());
            assert_ne!(t(Lang::En, p.key()), p.key(), "нет EN-строки для {}", p.key());
        }
    }

    #[test]
    fn fill_substitutes_all_placeholders() {
        assert_eq!(fill("PID {pid} / {name}", &[("pid", "42"), ("name", "game")]), "PID 42 / game");
        assert_eq!(fill("n={n}", &[("n", "1")]), "n=1");
        assert_eq!(fill("без параметров", &[]), "без параметров");
    }

    #[test]
    fn placeholders_match_between_languages() {
        // Множество имён подстановок в фигурных скобках должно совпадать в RU и EN,
        // иначе подстановка даст «{x}». Сравниваются именно имена внутри `{...}`:
        // хвосты строк после `{` сравнивать нельзя — «{n} из {total}» и «{n} of {total}»
        // различаются не подстановками, а текстом между ними.
        for (k, ru, en) in tables().into_iter().flatten() {
            let a = placeholders_of(ru);
            let b = placeholders_of(en);
            assert_eq!(a, b, "разные подстановки в ключе {k}");
        }
    }

    #[test]
    fn table_size_is_reasonable() {
        assert!(STRINGS.len() > 100, "таблица строк слишком мала: {}", STRINGS.len());
    }
}

