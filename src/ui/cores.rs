//! Панель «Ядра и приоритет» для выбранного процесса.
//!
//! Сетка физических ядер с вложенными плитками потоков (виртуальные SMT-потоки —
//! пунктирная рамка и значок «V»), полосы загрузки, легенда, быстрые наборы ядер
//! и применение маски и приоритетов к процессу.
//!
//! Чистая логика (наборы ядер, переключение, раскладка, подписи) вынесена в функции
//! без `Ui`-контекста и покрыта тестами. Действия пользователя собираются в [`Action`]
//! и обрабатываются через состояние панели, а всё, что требует [`CoreTunerApp`]
//! (диалоги, тосты, журнал), выполняется в `commit`.

use crate::app::{CoreTunerApp, Dialog, EventKind};
use crate::config::Lang;
use crate::cpu::topology::Topology;
use crate::cpu::{affinity, priority, SysError};
use crate::i18n::{t, tf};
use crate::model::{CpuSet, IoPriority, MatchMode, MemPriority, PriorityClass};
use crate::process::list::ProcInfo;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Theme, Ui, Vec2};
use std::time::{Duration, Instant};

/// Строки панели, которых нет в общей таблице i18n.
pub const STR: &[(&str, &str, &str)] = &[
    (
        "cores.stale_hint",
        "Показаны данные другого процесса — идёт обновление…",
        "Showing another process — updating…",
    ),
    ("cores.prio_unavailable", "недоступно", "unavailable"),
    ("cores.whole_core", "Заголовок — целое ядро", "Header toggles whole core"),
    ("cores.thread_load_tip", " · загрузка {cpu} %", " · load {cpu} %"),
];

/// Период перечитывания настроек процесса из ОС, пока пользователь не редактирует.
const RELOAD: Duration = Duration::from_secs(2);
/// Ширина плитки физического ядра.
const CORE_W: f32 = 148.0;
/// Зазор между плитками по горизонтали.
const GAP_X: f32 = 8.0;
/// Высота кликабельного заголовка ядра.
const HEADER_H: f32 = 18.0;
/// Высота строки потока внутри плитки ядра.
const THREAD_ROW: f32 = 30.0;
/// Ширина полосы загрузки потока.
const LOAD_W: f32 = 40.0;
/// Высота полосы загрузки потока.
const LOAD_H: f32 = 5.0;
/// Минимальная ширина, на которую рассчитана раскладка.
const MIN_PANEL_W: f32 = 240.0;

// ------------------------------------------------------------- чистая логика

/// Быстрый набор ядер.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    All,
    Physical,
    Virtual,
    Invert,
}

impl Preset {
    /// Все кнопки в порядке отображения.
    pub const ALL: [Preset; 4] = [Preset::All, Preset::Physical, Preset::Virtual, Preset::Invert];

    /// Ключ локализации подписи кнопки.
    pub fn key(self) -> &'static str {
        match self {
            Preset::All => "cores.btn_all",
            Preset::Physical => "cores.btn_physical",
            Preset::Virtual => "cores.btn_virtual",
            Preset::Invert => "cores.btn_invert",
        }
    }
}

/// Строка легенды.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegendRow {
    On,
    Off,
    Physical,
    Virtual,
    Load,
}

impl LegendRow {
    /// Все строки легенды по порядку.
    pub const ALL: [LegendRow; 5] =
        [LegendRow::On, LegendRow::Off, LegendRow::Physical, LegendRow::Virtual, LegendRow::Load];

    /// Ключ локализации подписи.
    pub fn key(self) -> &'static str {
        match self {
            LegendRow::On => "cores.legend_on",
            LegendRow::Off => "cores.legend_off",
            LegendRow::Physical => "cores.legend_core",
            LegendRow::Virtual => "cores.legend_virtual",
            LegendRow::Load => "cores.legend_load",
        }
    }
}

/// Поток внутри физического ядра.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadTile {
    /// Глобальный индекс логического CPU.
    pub index: usize,
    /// Виртуальный (SMT) поток: пунктирная рамка и значок «V».
    pub virtual_thread: bool,
}

/// Сгруппировать потоки по физическим ядрам.
///
/// Неизвестные индексы и ядра без потоков отбрасываются, чтобы отрисовка не падала.
pub fn thread_groups(topo: &Topology) -> Vec<Vec<ThreadTile>> {
    topo.cores
        .iter()
        .map(|core| {
            core.threads
                .iter()
                .filter_map(|idx| {
                    let cpu = topo.cpus.get(*idx)?;
                    Some(ThreadTile { index: cpu.index, virtual_thread: cpu.smt_index > 0 })
                })
                .collect::<Vec<_>>()
        })
        .filter(|group: &Vec<ThreadTile>| !group.is_empty())
        .collect()
}

/// Переключить один поток.
///
/// Последний включённый поток не снимается: пустая маска запрещена.
pub fn toggle_thread(set: &CpuSet, index: usize) -> CpuSet {
    if set.contains(index) && set.len() <= 1 {
        return set.clone();
    }
    let mut next = set.clone();
    next.toggle(index);
    next
}

/// Переключить целое ядро.
///
/// Включены все потоки — выключаем все, иначе включаем все. Пустая маска не допускается.
pub fn toggle_core(topo: &Topology, set: &CpuSet, core: usize) -> CpuSet {
    let Some(threads) = topo.cores.get(core).map(|c| c.threads.clone()) else { return set.clone() };
    if threads.is_empty() {
        return set.clone();
    }
    let on = !threads.iter().all(|i| set.contains(*i));
    let mut next = set.clone();
    for idx in &threads {
        next.set(*idx, on);
    }
    if next.is_empty() {
        set.clone()
    } else {
        next
    }
}

/// Переключить первый (основной) поток ядра — режим клика по заголовку по умолчанию.
pub fn toggle_core_first_thread(topo: &Topology, set: &CpuSet, core: usize) -> CpuSet {
    match topo.cores.get(core).and_then(|c| c.threads.first().copied()) {
        Some(first) => toggle_thread(set, first),
        None => set.clone(),
    }
}

/// Применить быстрый набор.
///
/// Пустой результат (нет виртуальных потоков, инверсия полной маски) не применяется:
/// возвращается исходная маска.
pub fn apply_preset(topo: &Topology, current: &CpuSet, preset: Preset) -> CpuSet {
    let candidate = match preset {
        Preset::All => topo.all_set(),
        Preset::Physical => topo.physical_set(),
        Preset::Virtual => topo.virtual_set(),
        Preset::Invert => current.inverted(topo.total()),
    };
    if candidate.is_empty() {
        current.clone()
    } else {
        candidate
    }
}

/// Действие, выбранное пользователем в панели.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Переключить поток.
    ToggleThread(usize),
    /// Переключить ядро: целиком или только первый поток — по режиму состояния.
    ToggleCore(usize),
    /// Применить быстрый набор.
    Preset(Preset),
    /// Сменить класс приоритета.
    SetPriority(PriorityClass),
    /// Повысить приоритет на шаг.
    Raise,
    /// Понизить приоритет на шаг.
    Lower,
    /// Сменить приоритет ввода-вывода.
    SetIo(IoPriority),
    /// Сменить приоритет памяти.
    SetMem(MemPriority),
    /// Применить маску и приоритеты к процессу.
    Apply,
    /// Открыть диалог сохранения профиля.
    SaveProfile,
    /// Открыть диалог привязки к приложению.
    BindToApp,
}

/// Отложенный запрос, выполняемый только через [`CoreTunerApp`].
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    /// Подтверждение приоритета Realtime.
    Realtime { pid: u32, name: String },
    /// Окно «нет доступа».
    AccessDenied { pid: u32, name: String },
}

/// Итог обработки действий: тосты, журнал, применение, диалоги.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// Тосты: текст и признак ошибки.
    pub toasts: Vec<(String, bool)>,
    /// Запись журнала: тип, PID, имя, профиль, причина.
    pub log: Option<(EventKind, u32, String, String, String)>,
    /// Нужно применить настройки к процессу.
    pub apply: bool,
    /// Открыть диалог профиля: `true` — привязка к приложению, `false` — сохранение.
    pub dialog: Option<bool>,
    /// Запросы к [`CoreTunerApp`].
    pub pending: Vec<Pending>,
}

impl Outcome {
    /// Добавить тост.
    fn toast(&mut self, text: String, error: bool) {
        self.toasts.push((text, error));
    }

    /// Записать событие журнала.
    fn event(&mut self, kind: EventKind, pid: u32, name: &str, reason: impl Into<String>) {
        self.log = Some((kind, pid, name.to_string(), String::new(), reason.into()));
    }
}

/// Состояние панели «Ядра и приоритет».
#[derive(Clone, Debug)]
pub struct State {
    /// PID, к которому относятся загруженные из ОС значения.
    pub pid: Option<u32>,
    /// Значения из ОС успешно загружены.
    pub loaded: bool,
    /// Редактируемая маска ядер.
    pub mask: CpuSet,
    /// Класс приоритета.
    pub prio: PriorityClass,
    /// Приоритет ввода-вывода.
    pub io: IoPriority,
    /// Приоритет памяти.
    pub mem: MemPriority,
    /// Приоритет ввода-вывода недоступен: показывается серым.
    pub io_unavailable: bool,
    /// Приоритет памяти недоступен.
    pub mem_unavailable: bool,
    /// Применять изменения сразу при клике по ядру или потоку.
    pub apply_instantly: bool,
    /// Клик по заголовку переключает целое ядро, иначе — только первый поток.
    pub core_click_whole: bool,
    /// Предупреждение о приоритете Realtime уже выдано для текущего PID.
    pub realtime_warned: bool,
    /// Баннер ошибки.
    pub error: Option<String>,
    /// Предупреждение о доступе.
    pub warning: Option<String>,
    /// Процесс недоступен: уместно приглашение перезапуска от администратора.
    pub access_denied: bool,
    /// Имя процесса для записей журнала.
    pub proc_name: String,
    /// Данные панели относятся к другому процессу.
    pub stale: bool,
    /// Время последнего чтения из ОС.
    pub last_load: Instant,
}

impl Default for State {
    fn default() -> Self {
        State {
            pid: None,
            loaded: false,
            mask: CpuSet::new(),
            prio: PriorityClass::default(),
            io: IoPriority::Normal,
            mem: MemPriority::Normal,
            io_unavailable: false,
            mem_unavailable: false,
            apply_instantly: true,
            core_click_whole: false,
            realtime_warned: false,
            error: None,
            warning: None,
            access_denied: false,
            proc_name: String::new(),
            stale: false,
            last_load: Instant::now(),
        }
    }
}

impl State {
    /// Пора перечитать настройки процесса из ОС.
    pub fn needs_load(&self, pid: u32) -> bool {
        self.pid != Some(pid) || !self.loaded || self.last_load.elapsed() >= RELOAD
    }

    /// Принять значения, прочитанные из ОС, и снять разовые пометки.
    pub fn adopt(&mut self, pid: u32, mask: CpuSet, prio: PriorityClass, io: IoPriority, mem: MemPriority) {
        self.pid = Some(pid);
        self.loaded = true;
        self.mask = mask;
        self.prio = prio;
        self.io = io;
        self.mem = mem;
        self.error = None;
        self.warning = None;
        self.access_denied = false;
        self.stale = false;
        self.realtime_warned = false;
        self.last_load = Instant::now();
    }

    /// Маска для отправки в ОС: индексы в пределах топологии, непустая.
    pub fn effective_mask(&self, total: usize) -> Option<CpuSet> {
        let mask = self.mask.clamped(total);
        if mask.is_empty() {
            None
        } else {
            Some(mask)
        }
    }

    /// Вернуть набор к первым потокам ядер после неудачного применения.
    ///
    /// В терминах «целых ядер» действует именно такая маска, поэтому панель остаётся
    /// согласованной с состоянием ОС.
    pub fn mark_apply_failed(&mut self, topo: &Topology) {
        let mut keep = CpuSet::new();
        for core in &topo.cores {
            if let Some(first) = core.threads.first() {
                if self.mask.contains(*first) {
                    keep.insert(*first);
                }
            }
        }
        if !keep.is_empty() {
            self.mask = keep;
        }
    }

    /// Обработать одно действие пользователя.
    pub fn apply_action(
        &mut self,
        action: &Action,
        topo: &Topology,
        lang: Lang,
        pid: u32,
        name: &str,
    ) -> Outcome {
        let total = topo.total();
        let mut out = Outcome::default();
        match action {
            Action::ToggleThread(idx) => self.mask = toggle_thread(&self.mask, *idx),
            Action::ToggleCore(core) => {
                self.mask = if self.core_click_whole {
                    toggle_core(topo, &self.mask, *core)
                } else {
                    toggle_core_first_thread(topo, &self.mask, *core)
                };
            }
            Action::Preset(preset) => {
                self.mask = apply_preset(topo, &self.mask, *preset);
                let text = t(lang, preset.key()).to_string();
                out.toast(text.clone(), false);
                out.event(EventKind::Info, pid, name, text);
            }
            Action::SetPriority(p) => {
                // Realtime применяем только после подтверждения пользователем.
                if *p == PriorityClass::Realtime && !self.realtime_warned {
                    self.realtime_warned = true;
                    out.pending.push(Pending::Realtime { pid, name: name.to_string() });
                    return out;
                }
                self.prio = *p;
            }
            Action::Raise => self.prio = self.prio.raise(),
            Action::Lower => self.prio = self.prio.lower(),
            Action::SetIo(p) => {
                self.io = *p;
                self.io_unavailable = false;
            }
            Action::SetMem(p) => {
                self.mem = *p;
                self.mem_unavailable = false;
            }
            Action::Apply => {
                if self.effective_mask(total).is_none() {
                    out.toast(t(lang, "cores.empty_mask").to_string(), true);
                } else {
                    out.apply = true;
                }
                return out;
            }
            // Диалоги профиля обрабатываются в `show`: нужен доступ к `CoreTunerApp`.
            Action::SaveProfile | Action::BindToApp => return out,
        }
        // «Применять сразу» относится к любому изменению маски: клик по потоку или ядру
        // и кнопки быстрых наборов.
        if self.apply_instantly
            && matches!(action, Action::ToggleThread(_) | Action::ToggleCore(_) | Action::Preset(_))
            && self.effective_mask(total).is_some()
        {
            out.apply = true;
        }
        out
    }

    /// Записать результат применения маски.
    pub fn apply_result(&mut self, result: Result<(), SysError>, lang: Lang, pid: u32) -> Outcome {
        let mut out = Outcome::default();
        match result {
            Ok(()) => {
                self.error = None;
                let text = tf(lang, "cores.applied", &[("pid", &pid.to_string())]);
                out.toast(text.clone(), false);
                out.event(EventKind::Applied, pid, &self.proc_name, text);
            }
            Err(e) => {
                let text = tf(lang, "err.apply", &[("e", &e.to_string())]);
                self.error = Some(text.clone());
                out.toast(text.clone(), true);
                out.event(EventKind::Failed, pid, &self.proc_name, text);
                if matches!(e, SysError::AccessDenied) {
                    self.access_denied = true;
                    out.pending.push(Pending::AccessDenied { pid, name: self.proc_name.clone() });
                }
            }
        }
        out
    }

    /// Имя профиля по умолчанию для диалогов: имя exe без расширения.
    pub fn dialog_default_name(&self) -> String {
        default_target_name(&self.proc_name)
    }
}

/// Размер плитки потока: первый поток ядра — средний, виртуальный — меньше.
pub fn thread_size(virtual_thread: bool) -> Vec2 {
    if virtual_thread {
        Vec2::new(26.0, 24.0)
    } else {
        Vec2::new(38.0, 28.0)
    }
}

/// Высота плитки физического ядра с учётом числа потоков.
pub fn core_tile_height(topo: &Topology, core: usize) -> f32 {
    let n = topo.cores.get(core).map_or(1, |c| c.threads.len().max(1));
    (16.0 + HEADER_H + 6.0 + n as f32 * THREAD_ROW + 8.0).max(64.0)
}

/// Сколько колонок плиток ядер помещается при заданной ширине (не меньше одной).
pub fn grid_columns(width: f32, tile_w: f32, gap: f32) -> usize {
    let step = (tile_w + gap).max(1.0);
    ((width.max(0.0) + gap) / step).floor().max(1.0) as usize
}

/// Раскладка плиток: пары «индекс ядра — номер колонки» и высота каждой строки.
pub fn layout(heights: &[f32], width: f32, tile_w: f32, gap: f32) -> (Vec<(usize, usize)>, Vec<f32>) {
    let cols = grid_columns(width, tile_w, gap);
    let mut places = Vec::with_capacity(heights.len());
    let mut rows: Vec<f32> = Vec::new();
    for (i, h) in heights.iter().enumerate() {
        let row = i / cols;
        if rows.len() <= row {
            rows.push(0.0);
        }
        if let Some(slot) = rows.get_mut(row) {
            *slot = slot.max(*h);
        }
        places.push((i, i % cols));
    }
    (places, rows)
}

/// Отрезки пунктирной рамки по периметру прямоугольника.
pub fn dashed_segments(rect: Rect, dash: f32, gap: f32) -> Vec<[Pos2; 2]> {
    let mut out = Vec::new();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return out;
    }
    let dash = dash.max(0.5);
    let step = (dash + gap.max(0.0)).max(1.0);
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let total = (dx * dx + dy * dy).sqrt();
        if total <= 0.0 {
            continue;
        }
        let mut d = 0.0f32;
        while d < total {
            let d2 = (d + dash).min(total);
            out.push([
                Pos2::new(a.x + dx * d / total, a.y + dy * d / total),
                Pos2::new(a.x + dx * d2 / total, a.y + dy * d2 / total),
            ]);
            d += step;
        }
    }
    out
}

/// Подпись ядра: «Core N» с меткой P/E на гибридных системах.
pub fn core_caption(lang: Lang, topo: &Topology, core: usize) -> String {
    let title = tf(lang, "cores.core", &[("n", &(core + 1).to_string())]);
    match topo.core_label(core) {
        "P" => format!("{title} · {}", t(lang, "cores.core_p")),
        "E" => format!("{title} · {}", t(lang, "cores.core_e")),
        _ => title,
    }
}

/// Подпись потока: «Thread N» или «Thread N (virtual)», нумерация от единицы.
pub fn thread_caption(lang: Lang, index: usize, virtual_thread: bool) -> String {
    let n = (index + 1).to_string();
    if virtual_thread {
        tf(lang, "cores.thread_virtual", &[("n", &n)])
    } else {
        tf(lang, "cores.thread", &[("n", &n)])
    }
}

/// Тултип потока: логический процессор, группа, бит и текущая загрузка.
pub fn thread_tip(lang: Lang, index: usize, group: u16, bit: u8, load: f32) -> String {
    let base = tf(
        lang,
        "cores.thread_tip",
        &[("n", &(index + 1).to_string()), ("g", &group.to_string()), ("b", &bit.to_string())],
    );
    base + &tf(lang, "cores.thread_load_tip", &[("cpu", &format!("{load:.0}"))])
}

/// Сводка маски: «Включено N из M».
pub fn mask_summary(lang: Lang, mask: &CpuSet, total: usize) -> String {
    tf(lang, "cores.mask_summary", &[("on", &mask.len().to_string()), ("total", &total.to_string())])
}

/// Имя по умолчанию для диалогов: имя exe без расширения.
pub fn default_target_name(name: &str) -> String {
    let base = name.trim();
    let stem = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .or_else(|| base.strip_suffix(".Exe"))
        .unwrap_or(base);
    if stem.is_empty() {
        base.to_string()
    } else {
        stem.to_string()
    }
}

/// Ключ локализации приоритета ввода-вывода.
pub fn io_key(p: IoPriority) -> &'static str {
    match p {
        IoPriority::VeryLow => "io.very_low",
        IoPriority::Low => "io.low",
        IoPriority::Normal => "io.normal",
        IoPriority::High => "io.high",
    }
}

/// Ключ локализации приоритета памяти.
pub fn mem_key(p: MemPriority) -> &'static str {
    match p {
        MemPriority::VeryLow => "mem.very_low",
        MemPriority::Low => "mem.low",
        MemPriority::Medium => "mem.medium",
        MemPriority::BelowNormal => "mem.below_normal",
        MemPriority::Normal => "mem.normal",
    }
}

/// Цвета панели, зависящие от темы.
struct Palette {
    on: Color32,
    off: Color32,
    stroke_on: Stroke,
    stroke_off: Stroke,
    text_on: Color32,
    text_off: Color32,
    badge_p: Color32,
    badge_e: Color32,
    header_bg: Color32,
    tile_bg: Color32,
    track_bg: Color32,
}

impl Palette {
    /// Собрать палитру для темы.
    fn new(theme: Theme) -> Self {
        let (badge_p, badge_e) = match theme {
            Theme::Dark => (Color32::from_rgb(0x5C, 0x9E, 0xFF), Color32::from_rgb(0xE0, 0x9B, 0x2B)),
            Theme::Light => (Color32::from_rgb(0x2E, 0x6F, 0xD6), Color32::from_rgb(0xB8, 0x76, 0x12)),
        };
        Palette {
            on: crate::ui::on_color(theme),
            off: crate::ui::off_color(theme),
            stroke_on: crate::ui::tile_stroke(theme, true),
            stroke_off: crate::ui::tile_stroke(theme, false),
            text_on: crate::ui::tile_text_color(theme, true),
            text_off: crate::ui::tile_text_color(theme, false),
            badge_p,
            badge_e,
            header_bg: shade(theme, crate::ui::off_color(theme), 0.82),
            tile_bg: shade(theme, crate::ui::off_color(theme), 0.94),
            track_bg: shade(theme, crate::ui::off_color(theme), 0.70),
        }
    }

    /// Цвет заливки плитки.
    fn fill(&self, on: bool) -> Color32 {
        if on {
            self.on
        } else {
            self.off
        }
    }

    /// Цвет текста на плитке.
    fn text(&self, on: bool) -> Color32 {
        if on {
            self.text_on
        } else {
            self.text_off
        }
    }

    /// Рамка плитки.
    fn stroke(&self, on: bool) -> Stroke {
        if on {
            self.stroke_on
        } else {
            self.stroke_off
        }
    }

    /// Цвет метки P/E.
    fn badge(&self, p_core: bool) -> Color32 {
        if p_core {
            self.badge_p
        } else {
            self.badge_e
        }
    }
}

/// Смешать цвет с фоном: `f < 1` — темнее на тёмной теме и светлее на светлой.
fn shade(theme: Theme, color: Color32, f: f32) -> Color32 {
    let bg = match theme {
        Theme::Dark => Color32::from_rgb(24, 26, 30),
        Theme::Light => Color32::from_rgb(246, 247, 249),
    };
    let k = match theme {
        Theme::Dark => f,
        Theme::Light => 2.0 - f,
    };
    let mix = |a: u8, b: u8| ((b as f32 + (a as f32 - b as f32) * k).clamp(0.0, 255.0)).round() as u8;
    Color32::from_rgb(mix(color.r(), bg.r()), mix(color.g(), bg.g()), mix(color.b(), bg.b()))
}

/// Контекст отрисовки сетки ядер.
struct GridCtx<'a> {
    topo: &'a Topology,
    groups: &'a [Vec<ThreadTile>],
    mask: &'a CpuSet,
    loads: &'a [f32],
    lang: Lang,
    palette: Palette,
    actions: Vec<Action>,
}

impl GridCtx<'_> {
    /// Загрузка потока в процентах (0..100).
    fn load(&self, index: usize) -> f32 {
        self.loads.get(index).copied().unwrap_or(0.0).clamp(0.0, 100.0)
    }
}

// ------------------------------------------------------------- отрисовка

/// Нарисовать панель «Ядра и приоритет» для выбранного процесса.
pub fn show(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    let topo = std::sync::Arc::clone(&app.topo);

    header(ui, app, lang);
    ui.separator();

    let Some(proc) = app.selected_proc().cloned() else {
        ui.label(t(lang, "common.not_selected"));
        return;
    };
    let pid = proc.pid;
    st.proc_name = proc.name.clone();

    // Данные панели от другого процесса: до перечитывания помечаем это в интерфейсе.
    st.stale = st.pid.is_some() && st.pid != Some(pid);

    // Загрузка из ОС: при смене процесса, при первой отрисовке и раз в ~2 с,
    // пока пользователь не редактирует (после его правки ждём применения).
    if st.needs_load(pid) {
        load_from_os(app, st, &proc, &topo, lang);
    }

    if topo.cores.is_empty() {
        ui.label(t(lang, "cores.no_topology"));
        return;
    }

    if draw_notes(ui, app, st, &proc, lang) {
        app.restart_as_admin();
    }

    let can_edit = !proc.protected && proc.accessible;
    let mut actions: Vec<Action> = Vec::new();

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(mask_summary(lang, &st.mask, topo.total())).strong());
        ui.separator();
        ui.checkbox(&mut st.apply_instantly, t(lang, "cores.apply_now"));
        let ready = can_edit && st.effective_mask(topo.total()).is_some();
        let tip = tf(lang, "cores.apply_to", &[("pid", &pid.to_string())]);
        if ui
            .add_enabled(ready, egui::Button::new(t(lang, "cores.apply")))
            .on_hover_text(tip)
            .clicked()
        {
            actions.push(Action::Apply);
        }
    });

    // Для защищённых и недоступных процессов вся работа с ядрами и приоритетами выключена.
    ui.add_enabled_ui(can_edit, |ui| {
        ui.horizontal_wrapped(|ui| {
            for preset in Preset::ALL {
                if ui.button(t(lang, preset.key())).clicked() {
                    actions.push(Action::Preset(preset));
                }
            }
            ui.checkbox(&mut st.core_click_whole, t(lang, "cores.whole_core"));
        });

        draw_grid(ui, &topo, st, &app.cpu_usage, lang, &mut actions);

        ui.horizontal_wrapped(|ui| {
            ui.label(t(lang, "cores.priority"));
            let mut value = st.prio;
            egui::ComboBox::from_id_salt("cores_prio")
                .selected_text(t(lang, value.key()))
                .show_ui(ui, |ui| {
                    for p in PriorityClass::ALL {
                        ui.selectable_value(&mut value, p, t(lang, p.key()));
                    }
                });
            if value != st.prio {
                actions.push(Action::SetPriority(value));
            }
            if ui.button(t(lang, "cores.raise")).clicked() {
                actions.push(Action::Raise);
            }
            if ui.button(t(lang, "cores.lower")).clicked() {
                actions.push(Action::Lower);
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(t(lang, "cores.io_priority"));
            if st.io_unavailable {
                ui.label(egui::RichText::new(t(lang, "cores.prio_unavailable")).weak());
            } else {
                let mut io = st.io;
                egui::ComboBox::from_id_salt("cores_io")
                    .selected_text(t(lang, io_key(io)))
                    .show_ui(ui, |ui| {
                        for p in
                            [IoPriority::VeryLow, IoPriority::Low, IoPriority::Normal, IoPriority::High]
                        {
                            ui.selectable_value(&mut io, p, t(lang, io_key(p)));
                        }
                    });
                if io != st.io {
                    actions.push(Action::SetIo(io));
                }
            }

            ui.separator();
            ui.label(t(lang, "cores.mem_priority"));
            if st.mem_unavailable {
                ui.label(egui::RichText::new(t(lang, "cores.prio_unavailable")).weak());
            } else {
                let mut mem = st.mem;
                egui::ComboBox::from_id_salt("cores_mem")
                    .selected_text(t(lang, mem_key(mem)))
                    .show_ui(ui, |ui| {
                        for p in [
                            MemPriority::VeryLow,
                            MemPriority::Low,
                            MemPriority::Medium,
                            MemPriority::BelowNormal,
                            MemPriority::Normal,
                        ] {
                            ui.selectable_value(&mut mem, p, t(lang, mem_key(p)));
                        }
                    });
                if mem != st.mem {
                    actions.push(Action::SetMem(mem));
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui.button(t(lang, "cores.save_as_profile")).clicked() {
                actions.push(Action::SaveProfile);
            }
            if ui.button(t(lang, "cores.bind_to_app")).clicked() {
                actions.push(Action::BindToApp);
            }
        });
    });

    draw_legend(ui, lang);

    let mut outcome = Outcome::default();
    for action in &actions {
        if matches!(action, Action::SaveProfile | Action::BindToApp) {
            if st.effective_mask(topo.total()).is_some() {
                outcome.dialog = Some(*action == Action::BindToApp);
            } else {
                outcome.toasts.push((t(lang, "cores.empty_mask").to_string(), true));
            }
            continue;
        }
        let part = st.apply_action(action, &topo, lang, pid, &proc.name);
        merge(&mut outcome, part);
    }
    commit(app, st, &topo, &proc, outcome);
}

/// Заголовок панели: название, иконка процесса, имя и PID.
fn header(ui: &mut Ui, app: &CoreTunerApp, lang: Lang) {
    ui.horizontal(|ui| {
        ui.heading(t(lang, "cores.title"));
        ui.separator();
        let Some(proc) = app.selected_proc() else { return };
        let (name, pid, path) = (proc.name.clone(), proc.pid, proc.exe_path.clone());
        if let Some(texture) = path.as_deref().and_then(|p| app.icon(p)) {
            ui.add(egui::Image::new(texture).fit_to_exact_size(Vec2::splat(20.0)).corner_radius(4));
        }
        ui.label(format!("{name} · PID {pid}"));
    });
}

/// Пояснения и предупреждения о доступе к процессу.
///
/// Возвращает `true`, если пользователь попросил перезапуск от администратора.
fn draw_notes(ui: &mut Ui, app: &CoreTunerApp, st: &mut State, proc: &ProcInfo, lang: Lang) -> bool {
    if proc.protected {
        ui.label(egui::RichText::new(t(lang, "cores.protected_note")).color(ui.visuals().warn_fg_color));
    } else if !proc.accessible {
        ui.label(egui::RichText::new(t(lang, "cores.no_access_note")).color(ui.visuals().error_fg_color));
    }
    if st.stale {
        ui.label(egui::RichText::new(t(lang, "cores.stale_hint")).weak());
    }
    let mut restart = false;
    if st.access_denied {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(t(lang, "cores.access_denied")).color(ui.visuals().error_fg_color));
            if app.elevated {
                ui.label(t(lang, "cores.already_admin"));
            } else if ui.button(t(lang, "cores.restart_admin")).clicked() {
                restart = true;
            }
        });
    }
    if let Some(err) = st.error.clone() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(err).color(ui.visuals().error_fg_color));
            if ui.small_button(t(lang, "common.dismiss")).clicked() {
                st.error = None;
            }
        });
    }
    if let Some(warn) = st.warning.clone() {
        ui.label(egui::RichText::new(warn).color(ui.visuals().warn_fg_color));
    }
    restart
}

/// Прочитать текущие маску и приоритеты процесса из ОС.
fn load_from_os(app: &mut CoreTunerApp, st: &mut State, proc: &ProcInfo, topo: &Topology, lang: Lang) {
    let pid = proc.pid;
    let denied = |e: &SysError| matches!(e, SysError::AccessDenied | SysError::NotFound);

    let prio = priority::get_priority(pid);
    let affinity_result = affinity::get_affinity(pid, topo);
    let io = priority::get_io_priority(pid);
    let mem = priority::get_memory_priority(pid);

    for (what, err) in [
        ("приоритет", prio.as_ref().err()),
        ("маска", affinity_result.as_ref().err()),
        ("IoPriority", io.as_ref().err()),
        ("MemoryPriority", mem.as_ref().err()),
    ] {
        if let Some(e) = err {
            tracing::debug!("{what} PID {pid}: {e}");
        }
    }

    let prio_denied = prio.as_ref().err().is_some_and(denied);
    let affinity_denied = affinity_result.as_ref().err().is_some_and(denied);
    let mask = match &affinity_result {
        Ok(set) if !set.is_empty() => set.clone(),
        // Маску не прочитали: показываем полный набор — это и есть поведение «все ядра».
        _ => topo.all_set(),
    };

    let io_unavailable = io.as_ref().err().is_some_and(supported_api_failure);
    let mem_unavailable = mem.as_ref().err().is_some_and(supported_api_failure);

    st.adopt(
        pid,
        mask,
        prio.unwrap_or_default(),
        io.unwrap_or(IoPriority::Normal),
        mem.unwrap_or(MemPriority::Normal),
    );
    st.io_unavailable = io_unavailable;
    st.mem_unavailable = mem_unavailable;

    if prio_denied || affinity_denied {
        st.warning = Some(t(lang, "cores.access_denied").to_string());
        st.access_denied = !app.elevated;
    }
}

/// Признак «API недоступен»: показываем серым, без падения и без тревоги.
/// Исчезнувший процесс (`NotFound`) недоступностью API не считается.
fn supported_api_failure(e: &SysError) -> bool {
    !matches!(e, SysError::NotFound)
}

/// Склеить результат обработки действия с накопленным.
fn merge(total: &mut Outcome, part: Outcome) {
    if part.log.is_some() {
        total.log = part.log;
    }
    total.toasts.extend(part.toasts);
    total.pending.extend(part.pending);
    total.apply |= part.apply;
    if part.dialog.is_some() {
        total.dialog = part.dialog;
    }
}

/// Выполнить отложенные запросы, применение и запись в журнал.
fn commit(app: &mut CoreTunerApp, st: &mut State, topo: &Topology, proc: &ProcInfo, outcome: Outcome) {
    for (text, error) in &outcome.toasts {
        app.toast(text.clone(), *error);
    }
    if let Some((kind, pid, name, profile, reason)) = outcome.log.clone() {
        app.log_event(kind, pid, &name, &profile, &reason);
    }
    for request in &outcome.pending {
        match request {
            Pending::Realtime { pid, name } => {
                app.ui.dialog_text = default_target_name(name);
                app.ui.dialog_mode = MatchMode::default();
                app.dialog = Some(Dialog::Realtime { pid: *pid, name: name.clone() });
            }
            Pending::AccessDenied { pid, name } => {
                if !app.elevated {
                    app.dialog = Some(Dialog::AccessDenied { pid: *pid, name: name.clone() });
                }
            }
        }
    }
    if outcome.apply {
        apply_to_process(app, st, topo, proc);
    }
    // Диалоги профиля: текст по умолчанию — имя exe без расширения.
    if let Some(bind) = outcome.dialog {
        app.ui.dialog_text = st.dialog_default_name();
        app.ui.dialog_mode = MatchMode::default();
        let mask = st.effective_mask(topo.total()).unwrap_or_default();
        let name = proc.name.clone();
        let target = proc.exe_path.clone().unwrap_or_else(|| name.clone());
        let pid = proc.pid;
        app.dialog = Some(if bind {
            Dialog::BindToApp { pid, name, target, mask, prio: st.prio }
        } else {
            Dialog::SaveProfile { pid, name, target, mask, prio: st.prio }
        });
    }
}

/// Применить маску и приоритеты к процессу.
fn apply_to_process(app: &mut CoreTunerApp, st: &mut State, topo: &Topology, proc: &ProcInfo) {
    let lang = app.lang();
    let Some(mask) = st.effective_mask(topo.total()) else {
        app.toast(t(lang, "cores.empty_mask"), true);
        return;
    };
    let pid = proc.pid;
    st.proc_name = proc.name.clone();

    let result = affinity::set_affinity(pid, &mask, topo);
    let failed = result.is_err();
    let outcome = st.apply_result(result, lang, pid);
    if failed {
        st.mark_apply_failed(topo);
    }
    for (text, error) in &outcome.toasts {
        app.toast(text.clone(), *error);
    }
    if let Some((kind, log_pid, name, profile, reason)) = outcome.log.clone() {
        app.log_event(kind, log_pid, &name, &profile, &reason);
    }
    for request in &outcome.pending {
        if let Pending::AccessDenied { pid, name } = request {
            if !app.elevated {
                app.dialog = Some(Dialog::AccessDenied { pid: *pid, name: name.clone() });
            }
        }
    }
    if failed {
        return;
    }

    if let Err(e) = priority::set_priority(pid, st.prio) {
        let msg = tf(lang, "err.apply", &[("e", &e.to_string())]);
        st.error = Some(msg.clone());
        app.toast(msg, true);
        return;
    }
    // Ввод-вывод и память необязательны: их отказ не отменяет применённую маску.
    if let Err(e) = priority::set_io_priority(pid, st.io) {
        st.io_unavailable = true;
        tracing::debug!("set_io_priority PID {pid}: {e}");
    }
    if let Err(e) = priority::set_memory_priority(pid, st.mem) {
        st.mem_unavailable = true;
        tracing::debug!("set_memory_priority PID {pid}: {e}");
    }
    let text = tf(lang, "cores.applied", &[("pid", &pid.to_string())]);
    app.toast(text.clone(), false);
    app.log_event(EventKind::Applied, pid, &proc.name, "", &text);
}

/// Сетка ядер: крупные плитки физических ядер с вложенными плитками потоков.
fn draw_grid(ui: &mut Ui, topo: &Topology, st: &State, loads: &[f32], lang: Lang, actions: &mut Vec<Action>) {
    let groups = thread_groups(topo);
    if groups.is_empty() {
        ui.label(t(lang, "cores.no_topology"));
        return;
    }
    let mut ctx = GridCtx {
        topo,
        groups: &groups,
        mask: &st.mask,
        loads,
        lang,
        palette: Palette::new(ui.ctx().theme()),
        actions: Vec::new(),
    };

    let heights: Vec<f32> = topo.cores.iter().map(|c| core_tile_height(topo, c.index)).collect();
    let width = ui.available_width().max(MIN_PANEL_W);
    let cols = grid_columns(width, CORE_W, GAP_X);
    let (_, rows) = layout(&heights, width, CORE_W, GAP_X);
    // Плитки растягиваются на доступную ширину: сетка адаптируется без горизонтальной прокрутки.
    let tile_w = ((width - GAP_X * (cols as f32 - 1.0)) / cols as f32).max(CORE_W);

    for (row, height) in rows.iter().enumerate() {
        let from = row * cols;
        let to = (from + cols).min(ctx.groups.len());
        if from >= to {
            break;
        }
        ui.horizontal(|ui| {
            for core in from..to {
                paint_core(ui, &mut ctx, core, *height, tile_w);
                ui.add_space(GAP_X);
            }
        });
        ui.add_space(GAP_X);
    }

    actions.append(&mut ctx.actions);
}

/// Нарисовать плитку физического ядра со вложенными плитками потоков.
fn paint_core(ui: &mut Ui, ctx: &mut GridCtx<'_>, core: usize, height: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter().clone();
    let pal = &ctx.palette;

    // Фон и внешняя сплошная рамка — признак физического ядра.
    painter.rect_filled(rect, 6.0, pal.tile_bg);
    painter.rect_stroke(rect, 6.0, pal.stroke_off, StrokeKind::Inside);

    // Заголовок ядра: клик переключает ядро целиком (или первый поток — по режиму).
    let header = Rect::from_min_size(rect.min + Vec2::new(6.0, 6.0), Vec2::new(rect.width() - 12.0, HEADER_H));
    let header_resp = ui.interact(header, ui.scope_id().with(("cores_header", core)), Sense::click());
    let header_bg = if header_resp.hovered() { pal.header_bg.gamma_multiply(1.3) } else { pal.header_bg };
    painter.rect_filled(header, 3.0, header_bg);
    painter.rect_stroke(header, 3.0, Stroke::new(1.0, pal.stroke_off.color), StrokeKind::Inside);

    let title = core_caption(ctx.lang, ctx.topo, core);
    let title_rect = painter.text(
        header.left_center() + Vec2::new(6.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(11.0),
        pal.text_off,
    );

    // Метка P/E на гибридных системах.
    let label = ctx.topo.core_label(core);
    if !label.is_empty() {
        let badge = Rect::from_min_size(
            Pos2::new(header.right() - 20.0, header.center().y - 7.0),
            Vec2::new(16.0, 14.0),
        );
        painter.rect_filled(badge, 3.0, pal.badge(ctx.topo.is_p_core(core)));
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(9.0),
            Color32::BLACK,
        );
    }
    let _ = title_rect;

    if header_resp.clicked() {
        ctx.actions.push(Action::ToggleCore(core));
    }

    let Some(group) = ctx.groups.get(core) else { return };
    let mut y = header.bottom() + 6.0;
    for tile in group {
        let size = thread_size(tile.virtual_thread);
        let tile_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 6.0, y + (THREAD_ROW - size.y) * 0.5),
            Vec2::new((rect.width() - 12.0).max(size.x), size.y),
        );
        let resp = ui.interact(tile_rect, ui.scope_id().with(("cores_thread", tile.index)), Sense::click());
        paint_thread(ui, ctx, *tile, tile_rect, resp.hovered());
        if resp.clicked() {
            ctx.actions.push(Action::ToggleThread(tile.index));
        }
        if resp.hovered() {
            let (group_no, bit) = ctx.topo.cpus.get(tile.index).map_or((0, 0), |cpu| (cpu.group, cpu.bit));
            let tip = thread_tip(ctx.lang, tile.index, group_no, bit, ctx.load(tile.index));
            resp.on_hover_text(tip);
        }
        y += THREAD_ROW;
    }
}

/// Нарисовать плитку потока: заливка, рамка, подпись, значок «V» и полоса загрузки.
fn paint_thread(ui: &Ui, ctx: &GridCtx<'_>, tile: ThreadTile, rect: Rect, hovered: bool) {
    let pal = &ctx.palette;
    let painter = ui.painter();
    let on = ctx.mask.contains(tile.index);
    let mut fill = pal.fill(on);
    if hovered {
        fill = fill.gamma_multiply(1.15);
    }
    painter.rect_filled(rect, 3.0, fill);

    if tile.virtual_thread {
        // Виртуальный поток: пунктирная рамка отрезками и значок «V».
        for [a, b] in dashed_segments(rect, 3.0, 2.5) {
            painter.line_segment([a, b], pal.stroke(on));
        }
        let badge = Rect::from_min_size(rect.left_top() + Vec2::new(2.0, 2.0), Vec2::splat(9.0));
        painter.rect_filled(badge, 2.0, pal.badge_e);
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            t(ctx.lang, "cores.virtual_badge"),
            FontId::proportional(8.0),
            Color32::BLACK,
        );
    } else {
        // Первый поток ядра: средняя плитка со сплошной рамкой.
        painter.rect_stroke(rect, 3.0, pal.stroke(on), StrokeKind::Inside);
    }

    let text_x = if tile.virtual_thread { rect.left() + 14.0 } else { rect.left() + 4.0 };
    painter.text(
        Pos2::new(text_x, rect.center().y - 3.0),
        Align2::LEFT_CENTER,
        thread_caption(ctx.lang, tile.index, tile.virtual_thread),
        FontId::proportional(10.0),
        pal.text(on),
    );

    paint_load(painter, rect, ctx.load(tile.index), pal);
}

/// Тонкая полоса загрузки потока внизу плитки.
fn paint_load(painter: &egui::Painter, rect: Rect, load: f32, pal: &Palette) {
    let bar = Rect::from_min_size(
        Pos2::new(rect.right() - LOAD_W.min(rect.width() * 0.35) - 4.0, rect.bottom() - LOAD_H - 3.0),
        Vec2::new(LOAD_W.min(rect.width() * 0.35).max(8.0), LOAD_H),
    );
    painter.rect_filled(bar, 2.0, pal.track_bg);
    let value = (load / 100.0).clamp(0.0, 1.0);
    if value > 0.0 {
        let mut filled = bar;
        filled.set_width((bar.width() * value).max(1.0));
        painter.rect_filled(filled, 2.0, pal.badge_p);
    }
}

/// Легенда панели.
fn draw_legend(ui: &mut Ui, lang: Lang) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        for row in LegendRow::ALL {
            legend_swatch(ui, row, lang);
            ui.label(t(lang, row.key()));
        }
    });
}

/// Образец плитки в легенде.
fn legend_swatch(ui: &mut Ui, row: LegendRow, lang: Lang) {
    let pal = Palette::new(ui.ctx().theme());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(22.0, 14.0), Sense::hover());
    let painter = ui.painter();
    match row {
        LegendRow::On | LegendRow::Off => {
            let on = row == LegendRow::On;
            painter.rect_filled(rect, 2.0, pal.fill(on));
            painter.rect_stroke(rect, 2.0, pal.stroke(on), StrokeKind::Inside);
        }
        LegendRow::Physical => {
            painter.rect_filled(rect, 2.0, pal.off);
            painter.rect_stroke(rect, 2.0, pal.stroke_off, StrokeKind::Inside);
        }
        LegendRow::Virtual => {
            painter.rect_filled(rect, 2.0, pal.off);
            for [a, b] in dashed_segments(rect, 3.0, 2.5) {
                painter.line_segment([a, b], pal.stroke_off);
            }
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                t(lang, "cores.virtual_badge"),
                FontId::proportional(8.0),
                pal.text_off,
            );
        }
        LegendRow::Load => paint_load(painter, rect, 55.0, &pal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::topology::RawCore;

    fn raw(group: u16, class: u8, mask: u64) -> RawCore {
        RawCore { group, efficiency_class: class, mask }
    }

    /// 4 ядра по 2 потока — 8 логических CPU.
    fn topo_smt() -> Topology {
        let mut entries = Vec::new();
        for core in 0..4u64 {
            entries.push(raw(0, 0, 0b11u64 << (core * 2)));
        }
        Topology::from_raw(&entries)
    }

    /// Гибридная система: 2 P-ядра (класс 1) и 2 E-ядра (класс 0), по 2 потока.
    fn topo_hybrid() -> Topology {
        let mut entries = Vec::new();
        for core in 0..4u64 {
            let class = if core < 2 { 1 } else { 0 };
            entries.push(raw(0, class, 0b11u64 << (core * 2)));
        }
        Topology::from_raw(&entries)
    }

    #[test]
    fn thread_groups_match_cores() {
        let topo = topo_smt();
        let groups = thread_groups(&topo);
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].len(), 2);
        assert!(!groups[0][0].virtual_thread);
        assert!(groups[0][1].virtual_thread);
        assert_eq!(groups[0][0].index, 0);
        assert_eq!(groups[3][1].index, 7);
    }

    #[test]
    fn presets_pick_expected_cpus() {
        let topo = topo_smt();
        let empty = CpuSet::new();
        assert_eq!(apply_preset(&topo, &empty, Preset::All), CpuSet::from_indices(0..8));
        assert_eq!(apply_preset(&topo, &empty, Preset::Physical), CpuSet::from_indices([0, 2, 4, 6]));
        assert_eq!(apply_preset(&topo, &empty, Preset::Virtual), CpuSet::from_indices([1, 3, 5, 7]));

        let partial = CpuSet::from_indices([0, 1]);
        assert_eq!(
            apply_preset(&topo, &partial, Preset::Invert),
            CpuSet::from_indices([2, 3, 4, 5, 6, 7])
        );
    }

    #[test]
    fn invert_of_full_mask_keeps_current() {
        let topo = topo_smt();
        let full = topo.all_set();
        assert_eq!(apply_preset(&topo, &full, Preset::Invert), full, "пустая маска недопустима");
    }

    #[test]
    fn virtual_preset_without_smt_keeps_current() {
        let topo = Topology::from_raw(&[raw(0, 0, 0b1), raw(0, 0, 0b10)]);
        let cur = topo.all_set();
        assert_eq!(apply_preset(&topo, &cur, Preset::Virtual), cur);
    }

    #[test]
    fn toggling_thread_never_empties_mask() {
        let only = CpuSet::from_indices([3]);
        assert_eq!(toggle_thread(&only, 3), only, "последний поток не снимается");
        let two = CpuSet::from_indices([3, 5]);
        assert_eq!(toggle_thread(&two, 3), CpuSet::from_indices([5]));
        assert_eq!(toggle_thread(&two, 1), CpuSet::from_indices([1, 3, 5]));
    }

    #[test]
    fn toggling_core_switches_all_threads() {
        let topo = topo_smt();
        let full = topo.all_set();
        let off = toggle_core(&topo, &full, 1);
        assert_eq!(off, CpuSet::from_indices([0, 1, 4, 5, 6, 7]));
        assert_eq!(toggle_core(&topo, &off, 1), full);
    }

    #[test]
    fn toggling_last_core_is_rejected() {
        let topo = topo_smt();
        let only_core = CpuSet::from_indices([6, 7]);
        // Целое ядро: его выключение оставило бы пустую маску.
        assert_eq!(toggle_core(&topo, &only_core, 3), only_core);
        // Режим первого потока: снимается только индекс 6, индекс 7 сохраняет маску непустой.
        assert_eq!(toggle_core_first_thread(&topo, &only_core, 3), CpuSet::from_indices([7]));
        // Единственный включённый поток снять нельзя.
        let single = CpuSet::from_indices([6]);
        assert_eq!(toggle_core_first_thread(&topo, &single, 3), single);
    }

    #[test]
    fn core_first_thread_mode_touches_single_cpu() {
        let topo = topo_smt();
        let next = toggle_core_first_thread(&topo, &topo.all_set(), 2);
        assert!(!next.contains(4), "снимается только первый поток ядра");
        assert!(next.contains(5), "виртуальный поток остаётся");
    }

    #[test]
    fn unknown_core_toggle_is_noop() {
        let topo = topo_smt();
        let set = topo.all_set();
        assert_eq!(toggle_core(&topo, &set, 99), set);
        assert_eq!(toggle_core_first_thread(&topo, &set, 99), set);
    }

    #[test]
    fn hybrid_labels_mark_p_and_e() {
        let topo = topo_hybrid();
        assert!(topo.is_hybrid);
        let p = core_caption(Lang::En, &topo, 0);
        let e = core_caption(Lang::En, &topo, 3);
        assert!(p.contains(t(Lang::En, "cores.core_p")), "{p}");
        assert!(e.contains(t(Lang::En, "cores.core_e")), "{e}");
        assert!(e.contains('4'), "{e}");
    }

    #[test]
    fn non_hybrid_caption_has_no_badge() {
        let topo = topo_smt();
        let caption = core_caption(Lang::Ru, &topo, 0);
        assert!(!caption.contains(t(Lang::Ru, "cores.core_p")));
        assert!(caption.contains('1'), "{caption}");
    }

    #[test]
    fn thread_caption_is_localized() {
        // По ТЗ подписи плиток одинаковы в обоих языках: «Thread N» / «Thread N (virtual)».
        assert_eq!(thread_caption(Lang::Ru, 0, false), "Thread 1");
        let virt = thread_caption(Lang::En, 5, true);
        assert!(virt.contains("virtual"), "{virt}");
        assert!(virt.contains('6'), "{virt}");
    }

    #[test]
    fn mask_summary_shows_fraction() {
        let s = mask_summary(Lang::En, &CpuSet::from_indices([0, 1, 2]), 8);
        assert!(s.contains('3') && s.contains('8'), "{s}");
    }

    #[test]
    fn thread_tip_fills_every_placeholder() {
        let tip = thread_tip(Lang::Ru, 3, 0, 3, 42.0);
        assert!(!tip.contains('{'), "{tip}");
        assert!(tip.contains('4'));
        assert!(tip.contains("42"), "{tip}");
    }

    #[test]
    fn default_target_name_strips_exe() {
        assert_eq!(default_target_name("game.exe"), "game");
        assert_eq!(default_target_name("GAME.EXE"), "GAME");
        assert_eq!(default_target_name("  notepad.exe "), "notepad");
        assert_eq!(default_target_name("System"), "System");
        assert_eq!(default_target_name(""), "");
    }

    #[test]
    fn grid_columns_adapt_to_width() {
        assert_eq!(grid_columns(MIN_PANEL_W, CORE_W, GAP_X), 1);
        assert_eq!(grid_columns(1000.0, CORE_W, GAP_X), 6);
        assert_eq!(grid_columns(0.0, CORE_W, GAP_X), 1, "минимум одна колонка");
        for width in [200.0f32, 320.0, 640.0, 1280.0, 1920.0] {
            let cols = grid_columns(width, CORE_W, GAP_X);
            let used = cols as f32 * CORE_W + (cols as f32 - 1.0) * GAP_X;
            assert!(used <= width.max(CORE_W), "ширина {width}: {cols} колонок не влезают");
        }
    }

    #[test]
    fn layout_wraps_rows_and_keeps_heights() {
        let topo = topo_smt();
        let heights: Vec<f32> = (0..4).map(|c| core_tile_height(&topo, c)).collect();

        let (places, rows) = layout(&heights, MIN_PANEL_W, CORE_W, GAP_X);
        assert_eq!(places.len(), 4);
        assert_eq!(rows.len(), 4, "при узкой панели по одной плитке в строке");
        assert_eq!(places[0], (0, 0));
        assert_eq!(places[3], (3, 0));

        let (places, rows) = layout(&heights, 1000.0, CORE_W, GAP_X);
        assert_eq!(rows.len(), 1, "шесть колонок — все ядра в одной строке");
        assert_eq!(places[2], (2, 2));
    }

    #[test]
    fn layout_covers_sixty_four_cores() {
        // 64 ядра по 2 потока — крайний случай по числу логических CPU в одной группе.
        let mut entries = Vec::new();
        for core in 0..32u64 {
            entries.push(raw(0, 0, 0b11u64 << (core * 2)));
        }
        let topo = Topology::from_raw(&entries);
        assert_eq!(topo.total(), 64);
        let heights: Vec<f32> = topo.cores.iter().map(|c| core_tile_height(&topo, c.index)).collect();
        for width in [300.0f32, 700.0, 1400.0] {
            let (places, rows) = layout(&heights, width, CORE_W, GAP_X);
            assert_eq!(places.len(), 32);
            assert_eq!(rows.len(), 32usize.div_ceil(grid_columns(width, CORE_W, GAP_X)));
        }
    }

    #[test]
    fn core_tile_height_grows_with_threads() {
        let one = Topology::from_raw(&[raw(0, 0, 0b1)]);
        let many = Topology::from_raw(&[raw(0, 0, 0b1111)]);
        assert!(core_tile_height(&many, 0) > core_tile_height(&one, 0));
        assert!(core_tile_height(&one, 0) >= 64.0);
    }

    #[test]
    fn thread_tiles_differ_in_size() {
        let normal = thread_size(false);
        let virt = thread_size(true);
        assert!(virt.x < normal.x && virt.y < normal.y, "виртуальный поток рисуется меньше");
    }

    #[test]
    fn dashed_segments_stay_inside_rect() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(60.0, 20.0));
        let segs = dashed_segments(rect, 3.0, 2.0);
        assert!(!segs.is_empty());
        for [a, b] in &segs {
            for p in [a, b] {
                assert!(rect.expand(0.5).contains(*p), "{p:?} вне плитки");
            }
        }
    }

    #[test]
    fn dashed_segments_empty_rect_is_empty() {
        assert!(dashed_segments(Rect::from_min_size(Pos2::ZERO, Vec2::ZERO), 3.0, 2.0).is_empty());
    }

    #[test]
    fn state_adopt_clears_transient_flags() {
        let mut st = State {
            error: Some("x".into()),
            warning: Some("y".into()),
            access_denied: true,
            ..State::default()
        };
        st.adopt(42, CpuSet::from_indices([0, 1]), PriorityClass::High, IoPriority::Low, MemPriority::Low);
        assert_eq!(st.pid, Some(42));
        assert!(st.loaded);
        assert_eq!(st.mask, CpuSet::from_indices([0, 1]));
        assert_eq!(st.prio, PriorityClass::High);
        assert!(st.error.is_none() && st.warning.is_none() && !st.access_denied);
    }

    #[test]
    fn state_defaults_apply_instantly() {
        let st = State::default();
        assert!(st.apply_instantly, "по умолчанию применяем сразу");
        assert!(!st.loaded);
    }

    #[test]
    fn needs_load_on_pid_change_and_timeout() {
        let mut st = State::default();
        let topo = topo_smt();
        assert!(st.needs_load(7), "первый вызов требует загрузки");
        st.adopt(7, topo.all_set(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        assert!(!st.needs_load(7));
        assert!(st.needs_load(8), "смена процесса требует загрузки");
        st.last_load = Instant::now() - RELOAD;
        assert!(st.needs_load(7), "по таймауту перечитываем");
    }

    #[test]
    fn effective_mask_drops_foreign_indices() {
        let mut st = State { mask: CpuSet::from_indices([0, 2, 1000]), ..State::default() };
        assert_eq!(st.effective_mask(8), Some(CpuSet::from_indices([0, 2])));
        st.mask = CpuSet::from_indices([1000]);
        assert_eq!(st.effective_mask(8), None, "пустая маска не применяется");
    }

    #[test]
    fn apply_action_toggles_and_applies_instantly() {
        let mut st = State::default();
        let topo = topo_smt();
        st.adopt(5, topo.all_set(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        let out = st.apply_action(&Action::ToggleThread(1), &topo, Lang::Ru, 5, "game.exe");
        assert!(!st.mask.contains(1));
        assert!(out.apply, "при включённом «применять сразу» набор уходит в ОС");
    }

    #[test]
    fn apply_action_respects_instant_switch() {
        let mut st = State::default();
        let topo = topo_smt();
        st.adopt(5, topo.all_set(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        st.apply_instantly = false;
        let out = st.apply_action(&Action::ToggleCore(0), &topo, Lang::Ru, 5, "game.exe");
        assert!(!out.apply);
        assert!(out.toasts.is_empty());
    }

    #[test]
    fn empty_mask_is_not_applied() {
        let mut st = State::default();
        let topo = topo_smt();
        st.adopt(5, CpuSet::from_indices([0]), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        st.apply_instantly = false;
        // Снять последний поток невозможно, так что маска не пустеет.
        let out = st.apply_action(&Action::ToggleThread(0), &topo, Lang::Ru, 5, "game.exe");
        assert!(!out.apply);
        // Даже принудительно пустая маска в ОС не уходит.
        st.mask = CpuSet::new();
        let out = st.apply_action(&Action::Apply, &topo, Lang::Ru, 5, "game.exe");
        assert!(!out.apply);
        assert!(out.toasts.iter().any(|(txt, err)| *err && txt == t(Lang::Ru, "cores.empty_mask")));
    }

    #[test]
    fn preset_action_reports_mask_in_log() {
        let mut st = State::default();
        let topo = topo_smt();
        st.adopt(9, CpuSet::new(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        let out = st.apply_action(&Action::Preset(Preset::Physical), &topo, Lang::Ru, 9, "app.exe");
        assert_eq!(st.mask, CpuSet::from_indices([0, 2, 4, 6]));
        assert!(out.apply);
        let (kind, pid, name, _, _) = out.log.expect("событие записано");
        assert_eq!(kind, EventKind::Info);
        assert_eq!((pid, name.as_str()), (9, "app.exe"));
    }

    #[test]
    fn realtime_priority_asks_for_confirmation_once() {
        let mut st = State::default();
        let topo = topo_smt();
        st.adopt(3, topo.all_set(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        let out = st.apply_action(&Action::SetPriority(PriorityClass::Realtime), &topo, Lang::Ru, 3, "g.exe");
        assert_eq!(st.prio, PriorityClass::Normal, "без подтверждения приоритет не меняется");
        assert_eq!(out.pending, vec![Pending::Realtime { pid: 3, name: "g.exe".into() }]);

        // Повторный выбор после подтверждения применяется без нового вопроса.
        let out = st.apply_action(&Action::SetPriority(PriorityClass::Realtime), &topo, Lang::Ru, 3, "g.exe");
        assert!(out.pending.is_empty());
        assert_eq!(st.prio, PriorityClass::Realtime);
    }

    #[test]
    fn raise_and_lower_step_through_classes() {
        let mut st = State::default();
        let topo = topo_smt();
        st.adopt(3, topo.all_set(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        st.apply_action(&Action::Raise, &topo, Lang::Ru, 3, "g.exe");
        assert_eq!(st.prio, PriorityClass::AboveNormal);
        st.apply_action(&Action::Lower, &topo, Lang::Ru, 3, "g.exe");
        st.apply_action(&Action::Lower, &topo, Lang::Ru, 3, "g.exe");
        assert_eq!(st.prio, PriorityClass::BelowNormal);
    }

    #[test]
    fn apply_result_maps_access_denied_to_dialog_request() {
        let mut st = State { proc_name: "game.exe".into(), ..State::default() };
        let out = st.apply_result(Err(SysError::AccessDenied), Lang::Ru, 77);
        assert!(st.access_denied);
        assert!(st.error.is_some());
        assert_eq!(out.pending, vec![Pending::AccessDenied { pid: 77, name: "game.exe".into() }]);
        assert_eq!(out.log.expect("журнал").0, EventKind::Failed);
    }

    #[test]
    fn mark_apply_failed_returns_first_threads() {
        let topo = topo_smt();
        let mut st = State::default();
        st.adopt(
            1,
            CpuSet::from_indices([1, 2, 3, 5]),
            PriorityClass::Normal,
            IoPriority::Normal,
            MemPriority::Normal,
        );
        st.mark_apply_failed(&topo);
        assert_eq!(st.mask, CpuSet::from_indices([2]), "остаются только первые потоки ядер");
    }

    #[test]
    fn priority_keys_resolve_to_translations() {
        for p in [IoPriority::VeryLow, IoPriority::Low, IoPriority::Normal, IoPriority::High] {
            assert_ne!(t(Lang::Ru, io_key(p)), io_key(p));
            assert_ne!(t(Lang::En, io_key(p)), io_key(p));
        }
        for p in [
            MemPriority::VeryLow,
            MemPriority::Low,
            MemPriority::Medium,
            MemPriority::BelowNormal,
            MemPriority::Normal,
        ] {
            assert_ne!(t(Lang::Ru, mem_key(p)), mem_key(p));
            assert_ne!(t(Lang::En, mem_key(p)), mem_key(p));
        }
    }

    #[test]
    fn legend_rows_are_translated() {
        for row in LegendRow::ALL {
            assert_ne!(t(Lang::Ru, row.key()), row.key(), "нет RU для {}", row.key());
            assert_ne!(t(Lang::En, row.key()), row.key(), "нет EN для {}", row.key());
        }
    }

    #[test]
    fn own_str_table_is_complete() {
        for (key, ru, en) in STR {
            assert!(!ru.trim().is_empty(), "пустой RU для {key}");
            assert!(!en.trim().is_empty(), "пустой EN для {key}");
            assert_ne!(t(Lang::Ru, key), *key, "ключ {key} не найден в таблицах");
            assert_ne!(t(Lang::En, key), *key, "ключ {key} не найден в таблицах");
            let mut ru_ph: Vec<&str> = ru.match_indices('{').map(|(i, _)| &ru[i..]).collect();
            let mut en_ph: Vec<&str> = en.match_indices('{').map(|(i, _)| &en[i..]).collect();
            ru_ph.sort_unstable();
            en_ph.sort_unstable();
            assert_eq!(ru_ph, en_ph, "разные подстановки в {key}");
        }
    }

    #[test]
    fn core_click_whole_switch_uses_all_threads() {
        let topo = topo_smt();
        let mut st = State::default();
        st.adopt(1, topo.all_set(), PriorityClass::Normal, IoPriority::Normal, MemPriority::Normal);
        st.apply_instantly = false;
        st.core_click_whole = true;
        st.apply_action(&Action::ToggleCore(0), &topo, Lang::Ru, 1, "a.exe");
        assert!(!st.mask.contains(0) && !st.mask.contains(1));

        st.core_click_whole = false;
        st.apply_action(&Action::ToggleCore(0), &topo, Lang::Ru, 1, "a.exe");
        assert!(st.mask.contains(0), "в режиме первого потока включается только он");
        assert!(!st.mask.contains(1));
    }
}

