//! Вкладка «Активные процессы»: поиск, сортировка, таблица с виртуализацией и
//! панель выбранного процесса.
//!
//! Чистая логика (фильтр, сортировка, форматирование, ключ снимка) вынесена в отдельные
//! функции и покрыта тестами. Список процессов не клонируется: UI работает по индексам,
//! а порядок отображения кэшируется и пересчитывается только при смене входных данных
//! (или раз в [`REFRESH`] — на случай незамеченного обновления снимка).

use crate::app::{affinity_label, fmt_bytes, priority_label, status_color, CoreTunerApp, SortColumn};
use crate::i18n::{t, tf};
use crate::process::list::ProcInfo;
use egui::{Color32, CornerRadius, Rect, RichText, Sense, Ui, Vec2};
use std::time::{Duration, Instant};

/// Строки вкладки, которых нет в общей таблице i18n.
pub const STR: &[(&str, &str, &str)] = &[
    ("proc.search_placeholder", "Фильтр по имени или PID: {q}", "Filter by name or PID: {q}"),
    ("proc.window", "окно", "window"),
    ("proc.path", "Путь", "Path"),
    ("proc.mask", "Маска", "Mask"),
    ("proc.status", "Статус", "Status"),
];

/// Высота строки таблицы (без учёта `item_spacing.y`).
const ROW_H: f32 = 22.0;
/// Сторона иконки в строке.
const ICON: f32 = 18.0;
/// Ширина, начиная с которой панель выбранного процесса уходит вправо.
const WIDE: f32 = 900.0;
/// Доля ширины/высоты, отданная списку.
const LIST_FRACTION: f32 = 0.55;
/// Минимальная ширина списка.
const LIST_MIN: f32 = 380.0;
/// Минимальная ширина панели выбранного процесса.
const PANEL_MIN: f32 = 260.0;
/// Период принудительного пересчёта порядка отображения.
const REFRESH: Duration = Duration::from_millis(500);

/// Состояние вкладки: кэш порядка отображения и ключ, по которому он посчитан.
#[derive(Default)]
pub struct State {
    order: Vec<usize>,
    key: Option<SnapshotKey>,
    checked: Option<Instant>,
    /// Процесс, к которому список уже прокручен.
    scrolled_to: Option<u32>,
}

/// Опознавательный ключ снимка процессов и параметров отображения.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotKey {
    len: usize,
    first_pid: u32,
    last_pid: u32,
    search: String,
    only_window: bool,
    sort: SortColumn,
    desc: bool,
}

impl SnapshotKey {
    /// Ключ строится по длине списка и крайним PID — этого достаточно, чтобы заметить
    /// подмену снимка монитором; полное сравнение делается раз в [`REFRESH`].
    pub fn new(
        procs: &[ProcInfo],
        search: &str,
        only_window: bool,
        sort: SortColumn,
        desc: bool,
    ) -> Self {
        SnapshotKey {
            len: procs.len(),
            first_pid: procs.first().map_or(0, |p| p.pid),
            last_pid: procs.last().map_or(0, |p| p.pid),
            search: search.trim().to_lowercase(),
            only_window,
            sort,
            desc,
        }
    }
}

/// Ранг приоритета для сортировки: `None` — в конец (в любом направлении).
fn prio_rank(p: Option<&crate::model::PriorityClass>) -> u8 {
    use crate::model::PriorityClass as P;
    match p {
        None => 0,
        Some(P::Idle) => 1,
        Some(P::BelowNormal) => 2,
        Some(P::Normal) => 3,
        Some(P::AboveNormal) => 4,
        Some(P::High) => 5,
        Some(P::Realtime) => 6,
    }
}

/// Подходит ли процесс под строку поиска (подстрока имени без учёта регистра или PID).
pub fn matches_search(p: &ProcInfo, search: &str) -> bool {
    let q = search.trim();
    if q.is_empty() {
        return true;
    }
    // Для чисто цифрового запроса — поиск по PID, иначе по имени: «12» не должно
    // находить «chrome.exe» из-за совпадения с «12».
    if q.len() >= 5 && q.bytes().all(|b| b.is_ascii_digit()) {
        return q.parse::<u32>().map(|pid| p.pid == pid).unwrap_or(false);
    }
    let ql = q.to_lowercase();
    p.name.to_lowercase().contains(&ql) || p.pid.to_string().contains(&ql)
}

/// Индексы процессов в порядке отображения (фильтр + стабильная сортировка).
pub fn filter_and_sort(
    procs: &[ProcInfo],
    search: &str,
    only_window: bool,
    sort: SortColumn,
    desc: bool,
) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..procs.len())
        .filter(|&i| {
            let p = &procs[i];
            (!only_window || p.has_window) && matches_search(p, search)
        })
        .collect();
    idx.sort_by(|&a, &b| {
        let (pa, pb) = (&procs[a], &procs[b]);
        // Процессы без известного приоритета всегда в конце — независимо от направления,
        // поэтому проверка стоит до разворота для сортировки по убыванию.
        if sort == SortColumn::Priority {
            match (pa.priority.is_some(), pb.priority.is_some()) {
                (false, true) => return std::cmp::Ordering::Greater,
                (true, false) => return std::cmp::Ordering::Less,
                _ => {}
            }
        }
        let ord = match sort {
            SortColumn::Name => pa
                .name
                .to_lowercase()
                .cmp(&pb.name.to_lowercase())
                .then(pa.pid.cmp(&pb.pid)),
            SortColumn::Pid => pa.pid.cmp(&pb.pid),
            SortColumn::Cpu => pa
                .cpu_percent
                .total_cmp(&pb.cpu_percent)
                .then(pa.pid.cmp(&pb.pid)),
            SortColumn::Ram => pa.ram_bytes.cmp(&pb.ram_bytes).then(pa.pid.cmp(&pb.pid)),
            SortColumn::Priority => prio_rank(pa.priority.as_ref())
                .cmp(&prio_rank(pb.priority.as_ref()))
                .then(pa.pid.cmp(&pb.pid)),
        };
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
    idx
}

/// Поиск позиции процесса по PID (для прокрутки к выбранной строке).
fn find_row(procs: &[ProcInfo], order: &[usize], pid: u32) -> Option<usize> {
    order.iter().position(|&i| procs.get(i).is_some_and(|p| p.pid == pid))
}

/// Пересчитать порядок отображения, если изменился снимок или параметры.
fn refresh_order(app: &CoreTunerApp, st: &mut State, force: bool) {
    let key = SnapshotKey::new(&app.procs, &app.search, app.only_with_window, app.sort, app.sort_desc);
    let stale = force || st.key.as_ref() != Some(&key);
    if !stale {
        return;
    }
    st.order = filter_and_sort(&app.procs, &app.search, app.only_with_window, app.sort, app.sort_desc);
    st.key = Some(key);
}

/// Заголовок колонки: клик переключает сортировку, стрелка показывает направление.
fn header(ui: &mut Ui, app: &mut CoreTunerApp, width: f32, col: SortColumn) {
    let active = app.sort == col;
    let mut text = t(app.lang(), col.key()).to_string();
    if active {
        text.push(if app.sort_desc { '▼' } else { '▲' });
    }
    let rich = RichText::new(text).strong().size(12.0);
    let rich = if active { rich.color(ui.visuals().selection.bg_fill) } else { rich };
    let resp = ui.add_sized([width, ROW_H], egui::Button::new(rich).frame(false));
    if resp.clicked() {
        if active {
            app.sort_desc = !app.sort_desc;
        } else {
            app.sort = col;
            app.sort_desc = matches!(col, SortColumn::Cpu | SortColumn::Ram);
        }
    }
}

/// Иконка процесса 18×18 из кэша `app.icons`, иначе — пустое место.
fn process_icon(ui: &mut Ui, app: &CoreTunerApp, p: &ProcInfo) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(ICON), Sense::hover());
    let Some(tex) = p.exe_path.as_deref().and_then(|path| app.icon(path)) else {
        return;
    };
    egui::Image::new((tex.id(), Vec2::splat(ICON))).paint_at(ui, rect);
}

/// Цвет статуса строки.
fn row_status_color(app: &CoreTunerApp, p: &ProcInfo) -> Color32 {
    status_color(p, !app.cfg.settings.theme.eq(&crate::config::ThemeMode::Light))
}

/// Текст статуса процесса. Используются строки, уже имеющиеся в общей таблице i18n.
fn status_text(lang: crate::config::Lang, p: &ProcInfo) -> String {
    if p.protected {
        t(lang, "proc.protected").to_string()
    } else if !p.accessible {
        t(lang, "proc.no_access").to_string()
    } else if p.has_window {
        t(lang, "proc.window").to_string()
    } else {
        t(lang, "proc.accessible").to_string()
    }
}

/// Всплывающая подсказка строки: путь exe и статус.
fn row_tooltip(lang: crate::config::Lang, p: &ProcInfo) -> String {
    let mut s = format!("{}  (PID {})", p.name, p.pid);
    s.push('\n');
    match p.exe_path.as_deref() {
        Some(path) => s.push_str(path),
        None => s.push_str(t(lang, "proc.unknown_path")),
    }
    s.push('\n');
    s.push_str(&status_text(lang, p));
    s
}

/// Одна строка таблицы фиксированной высоты. Возвращает `true`, если по ней кликнули.
fn row(ui: &mut Ui, app: &CoreTunerApp, p: &ProcInfo, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    let visuals = ui.visuals();
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(3), visuals.selection.bg_fill.gamma_multiply(0.45));
    } else if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(3),
            visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.35),
        );
    }

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(4.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let lang = app.lang();
    let total = app.topo.total();

    process_icon(&mut child, app, p);
    child.add_space(4.0);
    let name = if p.name.is_empty() { t(lang, "common.unknown") } else { p.name.as_str() };
    let mut name_text = RichText::new(name).size(13.0);
    if !p.accessible {
        name_text = name_text.weak();
    }
    child.add_sized([150.0, ROW_H], egui::Label::new(name_text).truncate());
    child.add_sized(
        [54.0, ROW_H],
        egui::Label::new(RichText::new(p.pid.to_string()).monospace().size(12.0)),
    );
    let cpu = RichText::new(format!("{:.1}", p.cpu_percent)).monospace().size(12.0);
    child.add_sized([52.0, ROW_H], egui::Label::new(cpu));
    child.add_sized([74.0, ROW_H], egui::Label::new(RichText::new(fmt_bytes(p.ram_bytes, lang)).size(12.0)));
    child.add_sized(
        [104.0, ROW_H],
        egui::Label::new(RichText::new(priority_label(p.priority, lang)).size(12.0)).truncate(),
    );
    child.add_sized(
        [96.0, ROW_H],
        egui::Label::new(RichText::new(affinity_label(p.affinity.as_ref(), total, lang)).size(12.0))
            .truncate(),
    );
    child.add_sized(
        [88.0, ROW_H],
        egui::Label::new(RichText::new(status_text(lang, p)).color(row_status_color(app, p)).size(12.0))
            .truncate(),
    );

    let resp = resp.on_hover_text(row_tooltip(lang, p));
    resp.clicked()
}

/// Таблица со строкой заголовков и виртуализированным списком.
///
/// Возвращает PID строки, по которой кликнули (повторный клик выбор не снимает).
fn table(ui: &mut Ui, app: &mut CoreTunerApp, st: &State, scroll_to: Option<u32>) -> Option<u32> {
    ui.horizontal(|ui| {
        ui.add_space(4.0 + ICON + 4.0);
        header(ui, app, 150.0, SortColumn::Name);
        header(ui, app, 54.0, SortColumn::Pid);
        header(ui, app, 52.0, SortColumn::Cpu);
        header(ui, app, 74.0, SortColumn::Ram);
        header(ui, app, 104.0, SortColumn::Priority);
        ui.add_sized(
            [96.0, ROW_H],
            egui::Label::new(RichText::new(t(app.lang(), "proc.mask")).strong().size(12.0)),
        );
        ui.add_sized(
            [88.0, ROW_H],
            egui::Label::new(RichText::new(t(app.lang(), "proc.status")).strong().size(12.0)),
        );
    });
    ui.separator();

    if app.procs.is_empty() {
        ui.add_space(16.0);
        ui.label(RichText::new(t(app.lang(), "proc.loading")).weak());
        return None;
    }
    if st.order.is_empty() {
        ui.add_space(16.0);
        ui.label(RichText::new(t(app.lang(), "proc.empty")).weak());
        return None;
    }

    let target_row = scroll_to.and_then(|pid| find_row(&app.procs, &st.order, pid));
    let mut clicked: Option<u32> = None;
    let selected = app.selected;
    egui::ScrollArea::vertical()
        .id_salt("ct_proc_list")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, st.order.len(), |ui, range| {
            if let Some(target) = target_row {
                if range.start == target {
                    ui.scroll_to_rect(Rect::from_min_size(ui.max_rect().min, Vec2::new(1.0, ROW_H)), Some(egui::Align::Center));
                }
            }
            for i in range {
                let Some(&pi) = st.order.get(i) else { continue };
                let Some(p) = app.procs.get(pi) else { continue };
                if row(ui, app, p, selected == Some(p.pid)) {
                    clicked = Some(p.pid);
                }
            }
        });
    clicked
}

/// Заголовок панели выбранного процесса.
fn panel_header(ui: &mut Ui, app: &CoreTunerApp, sel: Option<&ProcInfo>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(app.lang(), "cores.title")).strong());
    });
    match sel {
        Some(p) => {
            let lang = app.lang();
            ui.horizontal(|ui| {
                if let Some(tex) = p.exe_path.as_deref().and_then(|path| app.icon(path)) {
                    ui.add(egui::Image::new((tex.id(), Vec2::splat(32.0))));
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(&p.name).strong());
                    ui.label(
                        RichText::new(format!(
                            "{}: {}",
                            t(lang, "proc.col.pid"),
                            p.pid
                        ))
                        .weak()
                        .size(12.0),
                    );
                });
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}:", t(lang, "proc.col.cpu"))).weak().size(12.0));
                ui.label(RichText::new(format!("{:.1}%", p.cpu_percent)).monospace().size(12.0));
                ui.separator();
                ui.label(RichText::new(format!("{}:", t(lang, "proc.col.ram"))).weak().size(12.0));
                ui.label(RichText::new(fmt_bytes(p.ram_bytes, lang)).monospace().size(12.0));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{}:", t(lang, "proc.path"))).weak().size(12.0));
                let path = p.exe_path.as_deref().unwrap_or_else(|| t(lang, "proc.unknown_path"));
                ui.label(RichText::new(path).size(12.0));
            });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(status_text(lang, p))
                        .color(row_status_color(app, p))
                        .size(12.0),
                );
            });
        }
        None => {
            ui.label(RichText::new(t(app.lang(), "common.not_selected")).weak());
        }
    }
    ui.separator();
}

/// Панель выбранного процесса с вкладкой «Ядра и приоритет».
fn details(ui: &mut Ui, app: &mut CoreTunerApp) {
    let sel_pid = app.selected;
    let sel = sel_pid.and_then(|pid| {
        let p = app.procs.iter().find(|p| p.pid == pid)?;
        Some(p.clone())
    });
    if sel_pid.is_some() && sel.is_none() {
        app.selected = None;
    }
    panel_header(ui, app, sel.as_ref());
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("ct_proc_cores")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if sel.is_none() {
                ui.label(RichText::new(t(app.lang(), "cores.need_selection")).weak());
                return;
            }
            let mut cores = std::mem::take(&mut app.ui.cores);
            crate::ui::cores::show(ui, app, &mut cores);
            app.ui.cores = cores;
        });
}

/// Вкладка «Активные процессы».
pub fn show(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    ui.horizontal(|ui| {
        ui.label(t(lang, "common.search"));
        ui.add(
            egui::TextEdit::singleline(&mut app.search)
                .hint_text(t(lang, "proc.search_hint"))
                .desired_width(240.0),
        );
        if !app.search.is_empty() && ui.small_button(t(lang, "common.clear")).clicked() {
            app.search.clear();
        }
        ui.add(egui::Checkbox::new(&mut app.only_with_window, t(lang, "proc.only_with_window")));
        if app.only_with_window {
            ui.label(RichText::new(t(lang, "proc.window")).weak().size(11.0));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(tf(
                    lang,
                    "proc.showing",
                    &[("n", &st.order.len().to_string()), ("total", &app.procs.len().to_string())],
                ))
                .weak()
                .size(12.0),
            );
            ui.label(RichText::new(t(lang, "proc.sort_hint")).weak().size(11.0));
        });
    });
    ui.add_space(2.0);

    let now = Instant::now();
    let force = st.checked.is_none_or(|t0| now.duration_since(t0) >= REFRESH);
    if force {
        st.checked = Some(now);
    }
    refresh_order(app, st, force);

    // Прокрутка к выбранному процессу: один раз на изменение выбора.
    let scroll_to = if st.scrolled_to != app.selected {
        st.scrolled_to = app.selected;
        app.selected
    } else {
        None
    };

    let mut clicked: Option<u32> = None;
    let full = ui.available_size();
    if full.x > WIDE {
        ui.horizontal_top(|ui| {
            let list_w = (full.x * LIST_FRACTION).clamp(LIST_MIN, (full.x - PANEL_MIN).max(LIST_MIN));
            ui.allocate_ui_with_layout(
                Vec2::new(list_w, full.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    clicked = table(ui, app, st, scroll_to);
                },
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), full.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| details(ui, app),
            );
        });
    } else {
        let list_h = (full.y * LIST_FRACTION).max(160.0);
        ui.allocate_ui_with_layout(
            Vec2::new(full.x, list_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                clicked = table(ui, app, st, scroll_to);
            },
        );
        ui.separator();
        ui.allocate_ui_with_layout(
            Vec2::new(full.x, ui.available_height().max(120.0)),
            egui::Layout::top_down(egui::Align::Min),
            |ui| details(ui, app),
        );
    }

    if let Some(pid) = clicked {
        app.selected = Some(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::model::{CpuSet, PriorityClass};

    fn proc(pid: u32, name: &str, cpu: f32, ram: u64, window: bool, prio: Option<PriorityClass>) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.to_string(),
            exe_path: Some(format!(r"C:\Apps\{name}")),
            cpu_percent: cpu,
            ram_bytes: ram,
            priority: prio,
            affinity: Some(CpuSet::from_indices([0, 1])),
            has_window: window,
            accessible: true,
            protected: false,
            start_time: 0,
        }
    }

    fn sample() -> Vec<ProcInfo> {
        vec![
            proc(1, "explorer.exe", 1.0, 100, true, Some(PriorityClass::Normal)),
            proc(2, "chrome.exe", 12.5, 900, true, Some(PriorityClass::AboveNormal)),
            proc(3, "svchost.exe", 0.2, 50, false, None),
            proc(4, "game.exe", 40.0, 4000, true, Some(PriorityClass::High)),
        ]
    }

    #[test]
    fn search_matches_name_case_insensitive_and_pid() {
        let p = sample();
        assert!(matches_search(&p[0], "EXPL"));
        assert!(matches_search(&p[0], "expl"));
        assert!(matches_search(&p[0], "1"));
        assert!(matches_search(&p[0], "  "));
        assert!(matches_search(&p[0], ""));
        assert!(!matches_search(&p[0], "chrome"));
    }

    #[test]
    fn long_numeric_query_is_an_exact_pid() {
        let p = [proc(12345, "chrome.exe", 0.0, 0, false, None)];
        // Длинный цифровой запрос — это PID, а не подстрока имени.
        assert!(matches_search(&p[0], "12345"));
        assert!(!matches_search(&p[0], "54321"));
        // Короткий цифровой запрос ищется и по имени, и как подстрока PID.
        assert!(matches_search(&p[0], "123"));
        assert!(!matches_search(&p[0], "98765"));
    }

    #[test]
    fn filter_only_with_window() {
        let p = sample();
        let r = filter_and_sort(&p, "", true, SortColumn::Name, false);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|&i| p[i].has_window));
    }

    #[test]
    fn filter_returns_indices_of_source() {
        let p = sample();
        let r = filter_and_sort(&p, "game", false, SortColumn::Name, false);
        assert_eq!(r, vec![3]);
    }

    #[test]
    fn sort_by_cpu_both_directions() {
        let p = sample();
        let asc = filter_and_sort(&p, "", false, SortColumn::Cpu, false);
        assert_eq!(asc[0], 2);
        let desc = filter_and_sort(&p, "", false, SortColumn::Cpu, true);
        assert_eq!(desc[0], 3);
    }

    #[test]
    fn sort_by_name_is_case_insensitive() {
        let p = vec![
            proc(1, "b.exe", 0.0, 0, false, None),
            proc(2, "A.exe", 0.0, 0, false, None),
            proc(3, "a.exe", 0.0, 0, false, None),
        ];
        let r = filter_and_sort(&p, "", false, SortColumn::Name, false);
        assert_eq!(r, vec![1, 2, 0], "равные имена — по PID, A/a без учёта регистра");
    }

    #[test]
    fn sort_by_ram_and_pid() {
        let p = sample();
        let r = filter_and_sort(&p, "", false, SortColumn::Ram, true);
        assert_eq!(r[0], 3);
        let r = filter_and_sort(&p, "", false, SortColumn::Pid, false);
        assert_eq!(r, vec![0, 1, 2, 3]);
    }

    #[test]
    fn none_priority_always_last() {
        let p = sample();
        for desc in [false, true] {
            let r = filter_and_sort(&p, "", false, SortColumn::Priority, desc);
            assert_eq!(*r.last().expect("непустой список"), 2, "None-приоритет в конце (desc={desc})");
        }
        let asc = filter_and_sort(&p, "", false, SortColumn::Priority, false);
        assert_eq!(asc[0], 0, "Idle..Normal первым при возрастании");
        let desc = filter_and_sort(&p, "", false, SortColumn::Priority, true);
        assert_eq!(desc[0], 3, "High первым при убывании");
    }

    #[test]
    fn sort_is_stable_within_equal_keys() {
        let p = vec![
            proc(7, "x.exe", 5.0, 10, false, Some(PriorityClass::Normal)),
            proc(3, "y.exe", 5.0, 10, false, Some(PriorityClass::Normal)),
            proc(5, "z.exe", 5.0, 10, false, Some(PriorityClass::Normal)),
        ];
        let r = filter_and_sort(&p, "", false, SortColumn::Cpu, false);
        assert_eq!(r, vec![1, 2, 0], "порядок по PID при равных значениях");
    }

    #[test]
    fn empty_input_gives_empty_order() {
        assert!(filter_and_sort(&[], "", false, SortColumn::Name, false).is_empty());
    }

    #[test]
    fn snapshot_key_ignores_trim_and_case_of_search() {
        let p = sample();
        let a = SnapshotKey::new(&p, "Game ", false, SortColumn::Name, false);
        let b = SnapshotKey::new(&p, "game", false, SortColumn::Name, false);
        assert_eq!(a, b);
    }

    #[test]
    fn snapshot_key_changes_with_inputs() {
        let p = sample();
        let base = SnapshotKey::new(&p, "", false, SortColumn::Name, false);
        assert_ne!(base, SnapshotKey::new(&p, "a", false, SortColumn::Name, false));
        assert_ne!(base, SnapshotKey::new(&p, "", true, SortColumn::Name, false));
        assert_ne!(base, SnapshotKey::new(&p, "", false, SortColumn::Pid, false));
        assert_ne!(base, SnapshotKey::new(&p, "", false, SortColumn::Name, true));
        assert_ne!(base, SnapshotKey::new(&p[..2], "", false, SortColumn::Name, false));
    }

    #[test]
    fn find_row_locates_selected_pid() {
        let p = sample();
        let order = filter_and_sort(&p, "", false, SortColumn::Cpu, true);
        let pid = p[order[2]].pid;
        assert_eq!(find_row(&p, &order, pid), Some(2));
        assert_eq!(find_row(&p, &order, 9999), None);
    }

    #[test]
    fn formatters_used_by_table() {
        assert_eq!(fmt_bytes(2048, crate::config::Lang::Ru), "2 КБ");
        assert_eq!(priority_label(None, Lang::En), "—");
        assert_eq!(priority_label(Some(PriorityClass::High), Lang::En), "High");
        assert_eq!(affinity_label(Some(&CpuSet::from_indices([0, 1])), 16, Lang::Ru), "2 из 16");
    }

    #[test]
    fn status_text_prioritizes_protection() {
        let mut p = proc(1, "a.exe", 0.0, 0, true, None);
        p.protected = true;
        p.accessible = false;
        assert_eq!(status_text(Lang::En, &p), "protected");
        p.protected = false;
        assert_eq!(status_text(Lang::En, &p), "no access");
        p.accessible = true;
        assert_eq!(status_text(Lang::En, &p), "window");
        p.has_window = false;
        assert_eq!(status_text(Lang::En, &p), "accessible");
    }

    #[test]
    fn own_str_keys_are_not_in_common_table() {
        let common = crate::i18n::STRINGS;
        for (k, _, _) in STR {
            assert!(
                !common.iter().any(|(ck, _, _)| ck == k),
                "ключ {k} уже есть в общей таблице i18n"
            );
        }
    }

    #[test]
    fn own_str_keys_are_unique_and_bilingual() {
        let mut seen = std::collections::HashSet::new();
        for (k, ru, en) in STR {
            assert!(seen.insert(*k), "дубликат ключа: {k}");
            assert!(!ru.trim().is_empty() && !en.trim().is_empty(), "пустой перевод: {k}");
        }
    }
}
