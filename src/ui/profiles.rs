//! Вкладка «Профили»: слева список профилей, справа редактор выбранного.
use crate::app::{CoreTunerApp, Dialog};
use crate::i18n::{t, tf};
use crate::model::{new_id, AppBinding, IoPriority, MemPriority, PriorityClass, Profile};
use crate::ui;
use egui::{CornerRadius, RichText, Sense, Stroke, StrokeKind, Theme};
use std::collections::HashSet;

/// Строки, дополняющие общую таблицу `i18n` (ключи уникальны, RU+EN).
pub const STR: &[(&str, &str, &str)] = &[
    ("profiles.summary_all", "маска: все {total}", "affinity: all {total}"),
    ("profiles.summary_count", "маска: {n} из {total}", "affinity: {n} of {total}"),
    ("profiles.summary_keep", "маска: не менять", "affinity: keep"),
    ("profiles.prio_keep", "приоритет: не менять", "priority: keep"),
    ("profiles.make_active", "Сделать активным", "Make active"),
    ("profiles.already_active", "уже активен", "already active"),
    ("profiles.dup_suffix", "(копия)", "(copy)"),
];

/// Состояние вкладки: выбранный профиль, черновик названия, подсветка пустой маски.
#[derive(Default)]
pub struct State {
    pub selected: Option<String>,
    pub name_draft: String,
    name_id: Option<String>,
    empty_mask_id: Option<String>,
}

/// Уникальное имя: «Имя», «Имя (2)», «Имя (3)»…
pub fn unique_name(base: &str, existing: &[String]) -> String {
    let taken: HashSet<&str> = existing.iter().map(|s| s.as_str()).collect();
    if !taken.contains(base) {
        return base.to_string();
    }
    let mut n = 2usize;
    loop {
        let cand = format!("{base} ({n})");
        if !taken.contains(cand.as_str()) {
            return cand;
        }
        n = n.saturating_add(1);
    }
}

/// Новый профиль: имя уникально, маска — все ядра, приоритет Normal.
pub fn make_new_profile(app: &CoreTunerApp) -> Profile {
    let existing: Vec<String> = app.cfg.profiles.iter().map(|p| p.name.clone()).collect();
    let base = t(app.lang(), "profiles.new_name");
    let mut p = Profile::new(new_id("prof"), unique_name(base, &existing));
    p.affinity = Some(app.topo.all_set());
    p.priority = Some(PriorityClass::Normal);
    p
}

/// Копия профиля: новый id, имя «<имя> (копия)».
pub fn duplicate_profile(src: &Profile, existing: &[String]) -> Profile {
    let suffix = "(копия)";
    let mut p = src.clone();
    p.id = new_id("prof");
    p.name = unique_name(&format!("{} {suffix}", src.name), existing);
    p
}

/// Сводка профиля для списка: маска и приоритет.
pub fn profile_summary(app: &CoreTunerApp, p: &Profile) -> String {
    summary_text(app.lang(), p, app.topo.total())
}

/// Чистый расчёт сводки: «маска … · приоритет …».
pub fn summary_text(lang: crate::config::Lang, p: &Profile, total: usize) -> String {
    let mask = match &p.affinity {
        Some(s) if !s.is_empty() => {
            if total > 0 && s.len() >= total {
                tf(lang, "profiles.summary_all", &[("total", &total.to_string())])
            } else {
                tf(
                    lang,
                    "profiles.summary_count",
                    &[("n", &s.len().to_string()), ("total", &total.to_string())],
                )
            }
        }
        _ => t(lang, "profiles.summary_keep").to_string(),
    };
    let prio = match p.priority {
        Some(pr) => t(lang, pr.key()).to_string(),
        None => t(lang, "profiles.prio_keep").to_string(),
    };
    format!("{mask} · {prio}")
}

/// Переключение физического ядра. Возвращает `false`, если действие запрещено:
/// снятие последних включённых потоков оставило бы маску пустой.
pub fn toggle_core(set: &mut crate::model::CpuSet, threads: &[usize], core_on: bool) -> bool {
    if core_on {
        if set.len() <= threads.iter().filter(|i| set.contains(**i)).count() {
            return false;
        }
        for i in threads {
            set.remove(*i);
        }
    } else {
        for i in threads {
            set.insert(*i);
        }
    }
    true
}

/// Переключение одного логического процессора; `false` — если это последний включённый.
pub fn toggle_cpu(set: &mut crate::model::CpuSet, index: usize) -> bool {
    if set.contains(index) && set.len() <= 1 {
        return false;
    }
    set.toggle(index);
    true
}

/// Привязки, где профиль доступен или активен.
pub fn bindings_for<'a>(app: &'a CoreTunerApp, id: &str) -> Vec<&'a AppBinding> {
    app.cfg
        .bindings
        .iter()
        .filter(|b| b.profile_ids.iter().any(|x| x == id) || b.active_profile.as_deref() == Some(id))
        .collect()
}

fn index_of(app: &CoreTunerApp, id: &str) -> Option<usize> {
    app.cfg.profiles.iter().position(|p| p.id == id)
}

fn select(st: &mut State, id: &str, name: &str) {
    st.selected = Some(id.to_string());
    st.name_draft = name.to_string();
    st.name_id = Some(id.to_string());
}

pub fn show(ui: &mut egui::Ui, app: &mut CoreTunerApp, st: &mut State) {
    // Выбранный профиль мог исчезнуть (удаление, импорт).
    if let Some(id) = st.selected.clone() {
        if index_of(app, &id).is_none() {
            st.selected = None;
            st.name_draft.clear();
            st.name_id = None;
        }
    }
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(300.0, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| list_panel(ui, app, st),
        );
        ui.separator();
        let w = ui.available_width();
        let h = ui.available_height();
        ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("profiles_editor").show(ui, |ui| {
                editor(ui, app, st);
            });
        });
    });
}

/// Левая колонка: список и кнопки управления профилями.
fn list_panel(ui: &mut egui::Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    ui.label(RichText::new(t(lang, "profiles.list")).strong());
    let entries: Vec<(String, String, String)> = app
        .cfg
        .profiles
        .iter()
        .map(|p| (p.id.clone(), p.name.clone(), profile_summary(app, p)))
        .collect();

    let mut pick: Option<(String, String)> = None;
    let mut new_clicked = false;
    let mut dup_clicked = false;
    let mut del_clicked = false;
    let mut import_clicked = false;
    let mut export_clicked = false;

    let scroll_h = (ui.available_height() - 130.0).max(80.0);
    egui::ScrollArea::vertical()
        .id_salt("profiles_list")
        .max_height(scroll_h)
        .show(ui, |ui| {
            if entries.is_empty() {
                ui.weak(t(lang, "profiles.empty"));
            }
            for (id, name, summary) in &entries {
                let selected = st.selected.as_deref() == Some(id.as_str());
                ui.push_id(id, |ui| {
                    if ui.selectable_label(selected, RichText::new(name).strong()).clicked() {
                        pick = Some((id.clone(), name.clone()));
                    }
                    ui.weak(RichText::new(summary).small());
                });
                ui.add_space(2.0);
            }
        });

    ui.add_space(4.0);
    ui.separator();
    let has_sel = st.selected.as_deref().is_some_and(|id| index_of(app, id).is_some());
    ui.horizontal_wrapped(|ui| {
        new_clicked = ui.button(t(lang, "profiles.new")).clicked();
        dup_clicked = ui
            .add_enabled(has_sel, egui::Button::new(t(lang, "profiles.duplicate")))
            .clicked();
        del_clicked = ui
            .add_enabled(has_sel, egui::Button::new(t(lang, "profiles.delete")))
            .clicked();
    });
    ui.horizontal_wrapped(|ui| {
        import_clicked = ui.button(t(lang, "profiles.import")).clicked();
        export_clicked = ui.button(t(lang, "profiles.export")).clicked();
    });

    if let Some((id, name)) = pick {
        select(st, &id, &name);
    }
    if new_clicked {
        let p = make_new_profile(app);
        let (id, name) = (p.id.clone(), p.name.clone());
        app.cfg.profiles.push(p);
        select(st, &id, &name);
        app.mark_dirty();
        app.sync_tray();
        app.toast(t(lang, "profiles.created").to_string(), false);
    }
    if dup_clicked {
        if let Some(id) = st.selected.clone() {
            if let Some(idx) = index_of(app, &id) {
                let existing: Vec<String> = app.cfg.profiles.iter().map(|p| p.name.clone()).collect();
                let copy = duplicate_profile(&app.cfg.profiles[idx], &existing);
                let (cid, cname) = (copy.id.clone(), copy.name.clone());
                app.cfg.profiles.push(copy);
                select(st, &cid, &cname);
                app.mark_dirty();
                app.sync_tray();
                app.toast(t(lang, "profiles.duplicated").to_string(), false);
            }
        }
    }
    if del_clicked {
        if let Some(idx) = st.selected.as_deref().and_then(|id| index_of(app, id)) {
            let id = app.cfg.profiles[idx].id.clone();
            let name = app.cfg.profiles[idx].name.clone();
            app.dialog = Some(Dialog::DeleteProfile { id, name });
        }
    }
    if import_clicked {
        app.import_profiles();
    }
    if export_clicked {
        app.export_profiles();
    }
}

/// Правая колонка: редактор профиля.
fn editor(ui: &mut egui::Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    let Some(id) = st.selected.clone() else {
        ui.weak(t(lang, "profiles.select"));
        return;
    };
    let Some(idx) = index_of(app, &id) else {
        ui.weak(t(lang, "profiles.select"));
        return;
    };
    if st.name_id.as_deref() != Some(id.as_str()) {
        st.name_id = Some(id.clone());
        st.name_draft = app.cfg.profiles[idx].name.clone();
    }
    let total = app.topo.total();
    let theme = ui.ctx().theme();

    // --- Название ---
    ui.horizontal(|ui| {
        ui.label(t(lang, "profiles.name"));
        let resp = ui.add(
            egui::TextEdit::singleline(&mut st.name_draft)
                .desired_width(280.0)
                .hint_text(t(lang, "profiles.name")),
        );
        if resp.changed() {
            let trimmed = st.name_draft.trim().to_string();
            if !trimmed.is_empty() {
                if let Some(p) = app.cfg.profiles.get_mut(idx) {
                    if p.name != trimmed {
                        p.name = trimmed;
                        app.mark_dirty();
                        app.sync_tray();
                    }
                }
            }
        }
        if st.name_draft.trim().is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, t(lang, "profiles.name_required"));
        }
    });

    ui.add_space(6.0);
    ui.separator();

    // --- Маска ядер ---
    let mut mask_on = app.cfg.profiles[idx].affinity.is_some();
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "profiles.affinity")).strong());
        if ui.checkbox(&mut mask_on, t(lang, "common.keep")).changed() {
            if mask_on {
                let cur = app.cfg.profiles[idx].affinity.clone();
                let set = match cur {
                    Some(s) if !s.is_empty() => s,
                    _ => app.topo.all_set(),
                };
                app.cfg.profiles[idx].affinity = Some(set);
                st.empty_mask_id = None;
            } else {
                app.cfg.profiles[idx].affinity = None;
                st.empty_mask_id = None;
            }
            app.mark_dirty();
        }
    });

    if mask_on {
        let mut set = app.cfg.profiles[idx]
            .affinity
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| app.topo.all_set());
        let mut changed = false;
        let mut blocked = false;

        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("profiles_mask")
                .max_height(280.0)
                .show(ui, |ui| {
                    for core in &app.topo.cores {
                        ui.push_id(core.index, |ui| {
                            ui.horizontal(|ui| {
                                let core_on = core.threads.iter().any(|i| set.contains(*i));
                                let label = tf(lang, "cores.core", &[("n", &core.index.to_string())]);
                                let r = tile(
                                    ui,
                                    &label,
                                    core_on,
                                    false,
                                    theme,
                                    egui::vec2(84.0, 48.0),
                                    t(lang, "cores.legend_core"),
                                );
                                if r.clicked() {
                                    if !toggle_core(&mut set, &core.threads, core_on) {
                                        blocked = true;
                                    } else {
                                        changed = true;
                                    }
                                }
                                for i in core.threads.iter().skip(1) {
                                    let on = set.contains(*i);
                                    let label = tf(lang, "cores.thread", &[("n", &i.to_string())]);
                                    let tip = tf(
                                        lang,
                                        "cores.thread_virtual",
                                        &[("n", &i.to_string())],
                                    );
                                    let r = tile(
                                        ui,
                                        &format!("{label} {v}", v = t(lang, "cores.virtual_badge")),
                                        on,
                                        true,
                                        theme,
                                        egui::vec2(66.0, 34.0),
                                        &tip,
                                    );
                                    if r.clicked() {
                                        if !toggle_cpu(&mut set, *i) {
                                            blocked = true;
                                        } else {
                                            changed = true;
                                        }
                                    }
                                }
                            });
                        });
                    }
                });

            ui.horizontal_wrapped(|ui| {
                if ui.button(t(lang, "cores.btn_all")).clicked() {
                    set = app.topo.all_set();
                    changed = true;
                }
                if ui.button(t(lang, "cores.btn_physical")).clicked() {
                    set = app.topo.physical_set();
                    changed = true;
                }
                if ui.button(t(lang, "cores.btn_virtual")).clicked() {
                    set = app.topo.virtual_set();
                    changed = true;
                }
                if ui.button(t(lang, "cores.btn_invert")).clicked() {
                    set = set.inverted(total);
                    changed = true;
                }
            });
            ui.label(ui::mask_summary(lang, &set, total));
        });

        if changed && !set.is_empty() {
            app.cfg.profiles[idx].affinity = Some(set);
            st.empty_mask_id = None;
            app.mark_dirty();
        }
        if blocked {
            st.empty_mask_id = Some(id.clone());
        }
        if st.empty_mask_id.as_deref() == Some(id.as_str()) {
            ui.colored_label(ui.visuals().error_fg_color, t(lang, "profiles.affinity_empty"));
        }
    }

    ui.add_space(6.0);
    ui.separator();

    // --- Приоритет процесса ---
    let mut prio_on = app.cfg.profiles[idx].priority.is_some();
    let mut cur_prio = app.cfg.profiles[idx].priority.unwrap_or(PriorityClass::Normal);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "profiles.priority")).strong());
        if ui.checkbox(&mut prio_on, t(lang, "common.keep")).changed() {
            app.cfg.profiles[idx].priority = if prio_on { Some(cur_prio) } else { None };
            app.mark_dirty();
        }
        if prio_on {
            let before = cur_prio;
            egui::ComboBox::from_id_salt("profiles_prio")
                .selected_text(t(lang, cur_prio.key()))
                .show_ui(ui, |ui| {
                    for p in PriorityClass::ALL {
                        ui.selectable_value(&mut cur_prio, p, t(lang, p.key()));
                    }
                });
            if cur_prio != before {
                app.cfg.profiles[idx].priority = Some(cur_prio);
                app.mark_dirty();
            }
        }
    });

    // --- Приоритет ввода-вывода ---
    let mut io_on = app.cfg.profiles[idx].io_priority.is_some();
    let mut cur_io = app.cfg.profiles[idx].io_priority.unwrap_or(IoPriority::Normal);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "profiles.io_priority")).strong());
        if ui.checkbox(&mut io_on, t(lang, "common.keep")).changed() {
            app.cfg.profiles[idx].io_priority = if io_on { Some(cur_io) } else { None };
            app.mark_dirty();
        }
        if io_on {
            let before = cur_io;
            egui::ComboBox::from_id_salt("profiles_io")
                .selected_text(t(lang, io_key(cur_io)))
                .show_ui(ui, |ui| {
                    for v in IO_VALUES {
                        ui.selectable_value(&mut cur_io, v, t(lang, io_key(v)));
                    }
                });
            if cur_io != before {
                app.cfg.profiles[idx].io_priority = Some(cur_io);
                app.mark_dirty();
            }
        }
    });

    // --- Приоритет памяти ---
    let mut mem_on = app.cfg.profiles[idx].mem_priority.is_some();
    let mut cur_mem = app.cfg.profiles[idx].mem_priority.unwrap_or(MemPriority::Normal);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "profiles.mem_priority")).strong());
        if ui.checkbox(&mut mem_on, t(lang, "common.keep")).changed() {
            app.cfg.profiles[idx].mem_priority = if mem_on { Some(cur_mem) } else { None };
            app.mark_dirty();
        }
        if mem_on {
            let before = cur_mem;
            egui::ComboBox::from_id_salt("profiles_mem")
                .selected_text(t(lang, mem_key(cur_mem)))
                .show_ui(ui, |ui| {
                    for v in MEM_VALUES {
                        ui.selectable_value(&mut cur_mem, v, t(lang, mem_key(v)));
                    }
                });
            if cur_mem != before {
                app.cfg.profiles[idx].mem_priority = Some(cur_mem);
                app.mark_dirty();
            }
        }
    });

    ui.add_space(6.0);
    ui.separator();

    // --- Приложения с этим профилем ---
    ui.label(RichText::new(t(lang, "profiles.apps")).strong());
    let bound: Vec<(String, String, bool)> = bindings_for(app, &id)
        .into_iter()
        .map(|b| {
            (
                b.id.clone(),
                b.display_name(),
                b.active_profile.as_deref() == Some(id.as_str()),
            )
        })
        .collect();
    if bound.is_empty() {
        ui.weak(t(lang, "profiles.no_apps"));
    } else {
        let mut activate: Option<String> = None;
        for (bid, bname, is_active) in &bound {
            ui.horizontal(|ui| {
                ui.label(bname);
                if *is_active {
                    ui.weak(t(lang, "profiles.already_active"));
                } else if ui.button(t(lang, "profiles.make_active")).clicked() {
                    activate = Some(bid.clone());
                }
            });
        }
        if let Some(bid) = activate {
            if let Some(b) = app.cfg.bindings.iter_mut().find(|b| b.id == bid) {
                b.active_profile = Some(id.clone());
                app.mark_dirty();
                app.sync_tray();
            }
        }
    }
}

const IO_VALUES: [IoPriority; 4] = [
    IoPriority::VeryLow,
    IoPriority::Low,
    IoPriority::Normal,
    IoPriority::High,
];

const MEM_VALUES: [MemPriority; 5] = [
    MemPriority::VeryLow,
    MemPriority::Low,
    MemPriority::Medium,
    MemPriority::BelowNormal,
    MemPriority::Normal,
];

pub fn io_key(v: IoPriority) -> &'static str {
    match v {
        IoPriority::VeryLow => "io.very_low",
        IoPriority::Low => "io.low",
        IoPriority::Normal => "io.normal",
        IoPriority::High => "io.high",
    }
}

pub fn mem_key(v: MemPriority) -> &'static str {
    match v {
        MemPriority::VeryLow => "mem.very_low",
        MemPriority::Low => "mem.low",
        MemPriority::Medium => "mem.medium",
        MemPriority::BelowNormal => "mem.below_normal",
        MemPriority::Normal => "mem.normal",
    }
}

/// Плитка ядра/потока: сплошная рамка у физического ядра, пунктирная у виртуального потока.
fn tile(
    ui: &mut egui::Ui,
    label: &str,
    on: bool,
    dashed: bool,
    theme: Theme,
    size: egui::Vec2,
    tip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(6);
        let fill = if on { ui::on_color(theme) } else { ui::off_color(theme) };
        let stroke = ui::tile_stroke(theme, on);
        let text_color = ui::tile_text_color(theme, on);
        let painter = ui.painter();
        painter.rect_filled(rect, radius, fill);
        painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
        if dashed {
            draw_dashed(painter, rect, stroke);
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            text_color,
        );
    }
    resp.on_hover_text(tip)
}

/// Точки по периметру плитки — визуально отличают виртуальные потоки.
fn draw_dashed(painter: &egui::Painter, rect: egui::Rect, stroke: Stroke) {
    const DASH: f32 = 4.0;
    const GAP: f32 = 3.0;
    let inner = rect.shrink(2.0);
    let mut x = inner.left();
    while x < inner.right() {
        let x2 = (x + DASH).min(inner.right());
        painter.line_segment([egui::pos2(x, inner.top()), egui::pos2(x2, inner.top())], stroke);
        painter.line_segment([egui::pos2(x, inner.bottom()), egui::pos2(x2, inner.bottom())], stroke);
        x += DASH + GAP;
    }
    let mut y = inner.top();
    while y < inner.bottom() {
        let y2 = (y + DASH).min(inner.bottom());
        painter.line_segment([egui::pos2(inner.left(), y), egui::pos2(inner.left(), y2)], stroke);
        painter.line_segment([egui::pos2(inner.right(), y), egui::pos2(inner.right(), y2)], stroke);
        y += DASH + GAP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unique_name_appends_index() {
        let existing = names(&["Игра", "Игра (2)"]);
        assert_eq!(unique_name("Игра", &existing), "Игра (3)");
        assert_eq!(unique_name("Новая", &existing), "Новая");
    }

    #[test]
    fn duplicate_keeps_settings_but_changes_id_and_name() {
        use crate::model::CpuSet;
        let mut src = Profile::new("p1".into(), "Игра");
        src.affinity = Some(CpuSet::from_indices([0, 1]));
        src.priority = Some(PriorityClass::High);
        src.io_priority = Some(IoPriority::Low);
        let copy = duplicate_profile(&src, &names(&["Игра", "Игра (копия)"]));
        assert_ne!(copy.id, src.id);
        assert_eq!(copy.name, "Игра (копия) (2)");
        assert_eq!(copy.affinity, src.affinity);
        assert_eq!(copy.priority, src.priority);
        assert_eq!(copy.io_priority, src.io_priority);
    }

    #[test]
    fn empty_mask_is_forbidden() {
        use crate::model::CpuSet;
        // Один поток в маске — снять нельзя.
        let mut set = CpuSet::from_indices([3]);
        assert!(!toggle_cpu(&mut set, 3));
        assert_eq!(set.len(), 1);
        // Второй поток снять можно.
        set.insert(7);
        assert!(toggle_cpu(&mut set, 3));
        assert_eq!(set, CpuSet::from_indices([7]));
        // Ядро с двумя потоками: снять оба разом нельзя, если это вся маска.
        let mut s2 = CpuSet::from_indices([0, 1]);
        assert!(!toggle_core(&mut s2, &[0, 1], true));
        assert_eq!(s2.len(), 2);
        // Если есть чужие потоки — ядро снимается.
        s2.insert(9);
        assert!(toggle_core(&mut s2, &[0, 1], true));
        assert_eq!(s2, CpuSet::from_indices([9]));
        // Включение ядра из выключенного состояния работает.
        assert!(toggle_core(&mut s2, &[0, 1], false));
        assert_eq!(s2, CpuSet::from_indices([0, 1, 9]));
    }

    #[test]
    fn summary_reports_keep_count_and_priority() {
        use crate::config::Lang;
        use crate::model::CpuSet;
        let mut p = Profile::new("p".into(), "Игра");
        // Маска не задана — «не менять».
        assert!(summary_text(Lang::Ru, &p, 8).contains("не менять"));
        // Полная маска — «все 8».
        p.affinity = Some(CpuSet::from_indices(0..8));
        let full = summary_text(Lang::Ru, &p, 8);
        assert!(full.contains("все 8"), "{full}");
        // Частичная маска — «3 из 8».
        p.affinity = Some(CpuSet::from_indices([0, 1, 2]));
        let part = summary_text(Lang::Ru, &p, 8);
        assert!(part.contains("3 из 8"), "{part}");
        // Приоритет выводится, когда задан.
        p.priority = Some(PriorityClass::High);
        assert!(summary_text(Lang::Ru, &p, 8).contains("High"));
        // Английская локаль использует EN-строки.
        let en = summary_text(Lang::En, &p, 8);
        assert!(en.contains("3 of 8"), "{en}");
    }
}
