//! Состояние приложения, вкладки, монитор, трей, сохранение конфига и фоновые задачи.
//!
//! Тяжёлые операции (диалоги rfd, ShellExecute, чтение файлов) идут в фоновых потоках,
//! результат возвращается через каналы и опрашивается в `update` (без блокировки UI).

use crate::config::{Config, Lang};
use crate::cpu::topology::Topology;
use crate::i18n::{t, tf};
use crate::model::{AppBinding, CpuSet, MatchMode, PriorityClass, Profile};
use crate::process::list::ProcInfo;
use crate::process::monitor::{MonitorCmd, MonitorEvent};
use crate::profiles::storage;
use crate::tray::{Tray, TrayAction};
use crate::ui;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui;
use egui::{Color32, ColorImage, TextureHandle};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Активная вкладка.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Processes,
    ByFile,
    Profiles,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Processes, Tab::ByFile, Tab::Profiles, Tab::Settings];

    pub fn key(self) -> &'static str {
        match self {
            Tab::Processes => "tab.processes",
            Tab::ByFile => "tab.by_file",
            Tab::Profiles => "tab.profiles",
            Tab::Settings => "tab.settings",
        }
    }
}

/// Колонка сортировки списка процессов.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Pid,
    Cpu,
    Ram,
    Priority,
}

impl SortColumn {
    pub fn key(self) -> &'static str {
        match self {
            SortColumn::Name => "proc.col.name",
            SortColumn::Pid => "proc.col.pid",
            SortColumn::Cpu => "proc.col.cpu",
            SortColumn::Ram => "proc.col.ram",
            SortColumn::Priority => "proc.col.priority",
        }
    }
}

/// Запись журнала событий.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    Applied,
    Failed,
    ProfileSwitched,
    Info,
}

/// Одна запись журнала «События».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRecord {
    pub kind: EventKind,
    pub pid: u32,
    pub name: String,
    pub profile: String,
    pub reason: String,
    pub time: String,
}

/// Действие, требующее ответа пользователя (модальное окно).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    Realtime { pid: u32, name: String },
    SaveProfile { pid: u32, name: String, target: String, mask: CpuSet, prio: PriorityClass },
    BindToApp { pid: u32, name: String, target: String, mask: CpuSet, prio: PriorityClass },
    DeleteProfile { id: String, name: String },
    AccessDenied { pid: u32, name: String },
}

/// Результат фоновой задачи, возвращаемый в UI-поток.
#[derive(Clone, Debug)]
pub enum TaskResult {
    PickedFile(Option<String>),
    ExportPath(Option<std::path::PathBuf>),
    ImportPath(Option<std::path::PathBuf>),
    RestartAdmin(Result<(), String>),
    Icon { path: String, image: Option<ColorImage> },
}

/// Всплывающая плашка внутри окна.
#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// Ограничение журнала событий.
const MAX_EVENTS: usize = 100;
/// Время жизни тоста.
const TOAST_LIFETIME: Duration = Duration::from_secs(6);
/// Репейнт скрытого окна (опрос трея).
const HIDDEN_REPAINT: Duration = Duration::from_millis(250);
/// Репейнт видимого окна.
const VISIBLE_REPAINT: Duration = Duration::from_millis(500);
/// Дебаунс сохранения конфига.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(1200);
/// Сколько иконок загружать за кадр.
const ICONS_PER_FRAME: usize = 4;
/// Сколько иконок держать в кэше.
const ICON_CACHE_MAX: usize = 160;

/// Форматирование объёма памяти.
pub fn fmt_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let v = b as f64;
    if v >= GB {
        format!("{:.1} ГБ", v / GB)
    } else if v >= MB {
        format!("{:.0} МБ", v / MB)
    } else if v >= KB {
        format!("{:.0} КБ", v / KB)
    } else {
        format!("{b} Б")
    }
}

/// Описание текущей маски процесса для таблицы.
pub fn affinity_label(aff: Option<&CpuSet>, total: usize, lang: Lang) -> String {
    match aff {
        None => t(lang, "proc.affinity_none").to_string(),
        Some(s) if s.is_empty() => t(lang, "proc.affinity_none").to_string(),
        Some(s) if total > 0 && s.len() >= total => tf(lang, "proc.affinity_all", &[("n", &total.to_string())]),
        Some(s) => tf(lang, "proc.affinity_count", &[("n", &s.len().to_string()), ("total", &total.to_string())]),
    }
}

/// Главное состояние приложения.
pub struct CoreTunerApp {
    pub cfg: Config,
    pub topo: Arc<Topology>,
    pub tab: Tab,
    pub procs: Vec<ProcInfo>,
    pub cpu_usage: Vec<f32>,
    pub selected: Option<u32>,
    pub search: String,
    pub only_with_window: bool,
    pub sort: SortColumn,
    pub sort_desc: bool,
    pub events: Vec<EventRecord>,
    pub toasts: Vec<Toast>,
    pub dialog: Option<Dialog>,
    pub error: Option<String>,
    pub ui: ui::UiState,
    /// Окно сейчас видно (не скрыто в трей).
    pub window_visible: bool,
    pub dirty: bool,
    pub icons: HashMap<String, TextureHandle>,
    pub icon_missing: HashSet<String>,
    pub icon_tried: HashSet<String>,
    pub frame_index: u64,
    pub elevated: bool,
    pub config_dir: String,
    /// Канал «показать окно» от второго экземпляра.
    pub show_rx: Receiver<()>,
    /// Канал результатов фоновых задач.
    pub task_rx: Receiver<TaskResult>,
    pub task_tx: Sender<TaskResult>,
    pub tray: Tray,
    pub cmd_tx: Sender<MonitorCmd>,
    pub ev_rx: Receiver<MonitorEvent>,
    pub monitor_alive: bool,
    pub last_save: Instant,
    pub last_event: Option<Instant>,
    pub profiles_snapshot: Vec<(String, String)>,
}

impl CoreTunerApp {
    /// Конструктор по контракту SPEC.md.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        topo: Arc<Topology>,
        start_hidden: bool,
        show_rx: Receiver<()>,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        ui::install_fonts(ctx);
        ui::apply_style(ctx, cfg.settings.theme);
        ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(ui::icon_data(64)))));

        let (cmd_tx, ev_rx, _handle) = crate::process::monitor::spawn(cfg.clone(), Arc::clone(&topo));
        let (task_tx, task_rx) = crossbeam_channel::unbounded::<TaskResult>();

        let profiles = cfg.profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect::<Vec<_>>();
        let tray = match Tray::new(&profiles, cfg.settings.lang, cfg.settings.paused, true, ctx.clone()) {
            Ok(tray) => tray,
            Err(e) => {
                tracing::warn!("трей недоступен: {e:#}");
                Tray::unavailable()
            }
        };

        let mut app = CoreTunerApp {
            cfg,
            topo,
            tab: Tab::default(),
            procs: Vec::new(),
            cpu_usage: Vec::new(),
            selected: None,
            search: String::new(),
            only_with_window: false,
            sort: SortColumn::default(),
            sort_desc: false,
            events: Vec::new(),
            toasts: Vec::new(),
            dialog: None,
            error: None,
            ui: ui::UiState::default(),
            window_visible: !start_hidden,
            dirty: false,
            icons: HashMap::new(),
            icon_missing: HashSet::new(),
            icon_tried: HashSet::new(),
            frame_index: 0,
            elevated: crate::cpu::is_elevated(),
            config_dir: storage::config_dir().display().to_string(),
            show_rx,
            task_rx,
            task_tx,
            tray,
            cmd_tx,
            ev_rx,
            monitor_alive: true,
            last_save: Instant::now(),
            last_event: None,
            profiles_snapshot: Vec::new(),
        };
        app.ui.settings.autostart = crate::autostart::is_enabled();
        app.sync_tray();
        if start_hidden {
            app.tray.set_visible(true);
        } else {
            app.tray.set_visible(app.cfg.settings.always_show_tray);
        }
        app
    }

    pub fn lang(&self) -> Lang {
        self.cfg.settings.lang
    }

    pub fn text(&self, key: &'static str) -> &'static str {
        t(self.lang(), key)
    }

    pub fn fmt(&self, key: &'static str, args: &[(&str, &str)]) -> String {
        tf(self.lang(), key, args)
    }

    /// Отметить конфиг как изменённый (дебаунс сохранения, ConfigChanged монитору).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_save = Instant::now();
    }

    /// Показать всплывающую плашку.
    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast { text: text.into(), error, until: Instant::now() + TOAST_LIFETIME });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    /// Записать событие в журнал (последние 100).
    pub fn log_event(&mut self, kind: EventKind, pid: u32, name: &str, profile: &str, reason: &str) {
        let time = timestamp();
        self.events.push(EventRecord {
            kind,
            pid,
            name: name.to_string(),
            profile: profile.to_string(),
            reason: reason.to_string(),
            time,
        });
        if self.events.len() > MAX_EVENTS {
            let extra = self.events.len() - MAX_EVENTS;
            self.events.drain(0..extra);
        }
        self.last_event = Some(Instant::now());
    }

    pub fn selected_proc(&self) -> Option<&ProcInfo> {
        let pid = self.selected?;
        self.procs.iter().find(|p| p.pid == pid)
    }

    pub fn profile_name(&self, id: &str) -> String {
        self.cfg.profile(id).map(|p| p.name.clone()).unwrap_or_else(|| id.to_string())
    }

    /// Запустить фоновую задачу и вернуть результат в `task_rx`.
    pub fn spawn_task<F>(&self, f: F)
    where
        F: FnOnce() -> TaskResult + Send + 'static,
    {
        let tx = self.task_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }

    /// Запрос выбора .exe в фоновом потоке.
    pub fn pick_exe(&mut self) {
        let title = self.text("file.pick").to_string();
        self.spawn_task(move || {
            let picked = rfd::FileDialog::new()
                .add_filter("exe", &["exe"])
                .set_title(title)
                .pick_file();
            TaskResult::PickedFile(picked.map(|p| p.display().to_string()))
        });
    }

    /// Экспорт профилей в JSON: диалог в фоне, запись — по результату.
    pub fn export_profiles(&mut self) {
        let title = self.text("profiles.export").to_string();
        self.spawn_task(move || {
            let path = rfd::FileDialog::new()
                .add_filter("json", &["json"])
                .set_title(title)
                .set_file_name("coretuner-profiles.json")
                .save_file();
            TaskResult::ExportPath(path)
        });
    }

    /// Импорт профилей из JSON: диалог в фоне, слияние — по результату.
    pub fn import_profiles(&mut self) {
        let title = self.text("profiles.import").to_string();
        self.spawn_task(move || {
            TaskResult::ImportPath(rfd::FileDialog::new().add_filter("json", &["json"]).set_title(title).pick_file())
        });
    }

    /// Перезапуск от администратора (ShellExecute в фоновом потоке).
    pub fn restart_as_admin(&mut self) {
        self.spawn_task(|| TaskResult::RestartAdmin(crate::cpu::restart_as_admin().map_err(|e| format!("{e:#}"))));
    }

    /// Загрузить иконку exe в фоне (не чаще `ICONS_PER_FRAME` за кадр).
    ///
    /// Текстуры создаются в `drain_tasks` по готовности (там есть `ctx`), поэтому
    /// контекст рисования здесь не нужен.
    pub fn load_icons(&mut self) {
        let mut budget = ICONS_PER_FRAME;
        let mut queued: Vec<String> = Vec::new();
        for p in &self.procs {
            if budget == 0 {
                break;
            }
            let Some(path) = &p.exe_path else { continue };
            if self.icons.contains_key(path) || self.icon_tried.contains(path) {
                continue;
            }
            self.icon_tried.insert(path.clone());
            queued.push(path.clone());
            budget -= 1;
        }
        for path in queued {
            let tx = self.task_tx.clone();
            std::thread::spawn(move || {
                let image = crate::process::list::exe_icon(&path);
                let _ = tx.send(TaskResult::Icon { path, image });
            });
        }
        if self.icons.len() > ICON_CACHE_MAX {
            // Держим кэш ограниченным: лишние текстуры (самые старые по вставке)
            // выгружаются, иначе память растёт с каждым встреченным exe.
            let extra = self.icons.len() - ICON_CACHE_MAX;
            let stale: Vec<String> = self.icons.keys().take(extra).cloned().collect();
            for path in stale {
                self.icons.remove(&path);
            }
        }
    }

    pub fn icon(&self, path: &str) -> Option<&TextureHandle> {
        self.icons.get(path)
    }

    /// Синхронизировать меню трея с конфигом.
    pub fn sync_tray(&mut self) {
        let profiles: Vec<(String, String)> = self.cfg.profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
        let lang = self.cfg.settings.lang;
        let paused = self.cfg.settings.paused;
        let visible = self.tray.is_visible();
        if self.tray.rebuild_menu(&profiles, lang, paused, visible) {
            self.profiles_snapshot = profiles;
        }
    }

    /// Управление видимостью значка по настройкам и состоянию окна.
    pub fn update_tray_visibility(&mut self, window_hidden: bool) {
        let should = self.cfg.settings.always_show_tray || window_hidden;
        if self.tray.is_visible() != should {
            self.tray.set_visible(should);
        }
    }

    /// Применить действие трея.
    pub fn handle_tray_action(&mut self, action: TrayAction, ctx: &egui::Context) {
        match action {
            TrayAction::Open => show_window(ctx),
            TrayAction::Exit => {
                self.save_now();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            TrayAction::TogglePause => {
                let paused = !self.cfg.settings.paused;
                self.set_paused(paused);
            }
            TrayAction::SwitchProfile(id) => {
                self.switch_profile_everywhere(&id);
            }
        }
    }

    /// Пауза/возобновление автоприменения (трей и настройки).
    pub fn set_paused(&mut self, paused: bool) {
        self.cfg.settings.paused = paused;
        let _ = self.cmd_tx.send(MonitorCmd::Pause(paused));
        self.sync_tray();
        self.mark_dirty();
        let msg = if paused { self.text("tray.paused") } else { self.text("tray.resumed") };
        self.toast(msg, false);
        self.log_event(EventKind::Info, 0, "CoreTuner", "", msg);
    }

    /// Быстрое переключение активного профиля для всех привязок, где он есть.
    pub fn switch_profile_everywhere(&mut self, id: &str) {
        let mut count = 0;
        for b in &mut self.cfg.bindings {
            if b.profile_ids.iter().any(|p| p == id) {
                b.active_profile = Some(id.to_string());
                count += 1;
            }
        }
        let name = self.profile_name(id);
        self.mark_dirty();
        if count > 0 {
            let msg = self.fmt("tray.switched", &[("name", &name)]);
            self.toast(msg, false);
            self.log_event(EventKind::ProfileSwitched, 0, "CoreTuner", &name, "");
        }
    }

    /// Сохранить конфиг сейчас (без дебаунса).
    pub fn save_now(&mut self) {
        if let Err(e) = storage::save(&self.cfg) {
            tracing::error!("сохранение конфига: {e:#}");
            self.error = Some(tf(self.lang(), "err.save", &[("e", &format!("{e:#}"))]));
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    /// Создать профиль из текущего состояния панели ядер.
    pub fn create_profile(&mut self, name: &str, mask: CpuSet, prio: PriorityClass) -> String {
        let mut p = Profile::new(crate::model::new_id("prof"), name);
        p.affinity = Some(mask);
        p.priority = Some(prio);
        let id = p.id.clone();
        self.cfg.profiles.push(p);
        self.mark_dirty();
        self.sync_tray();
        id
    }

    /// Привязать профиль к приложению (создать или обновить привязку).
    pub fn bind_profile(&mut self, target: &str, mode: MatchMode, profile_id: &str) {
        let existing = self
            .cfg
            .bindings
            .iter_mut()
            .find(|b| b.mode == mode && b.target.eq_ignore_ascii_case(target));
        match existing {
            Some(b) => {
                if !b.profile_ids.iter().any(|p| p == profile_id) {
                    b.profile_ids.push(profile_id.to_string());
                }
                b.active_profile = Some(profile_id.to_string());
                b.enabled = true;
            }
            None => {
                let mut b = AppBinding {
                    id: crate::model::new_id("bind"),
                    target: target.to_string(),
                    mode,
                    enabled: true,
                    profile_ids: vec![profile_id.to_string()],
                    active_profile: Some(profile_id.to_string()),
                    triggers: Vec::new(),
                };
                if b.active_profile.is_none() {
                    b.active_profile = Some(profile_id.to_string());
                }
                self.cfg.bindings.push(b);
            }
        }
        self.mark_dirty();
        self.sync_tray();
    }

    /// Обработать события монитора.
    pub fn drain_monitor(&mut self) {
        let mut changed = false;
        let mut need_icons = false;
        loop {
            match self.ev_rx.try_recv() {
                Ok(ev) => match ev {
                    MonitorEvent::Snapshot(list) => {
                        need_icons = true;
                        self.procs = list;
                        if let Some(pid) = self.selected {
                            if !self.procs.iter().any(|p| p.pid == pid) {
                                self.selected = None;
                            }
                        }
                    }
                    MonitorEvent::CpuUsage(v) => {
                        self.cpu_usage = v;
                    }
                    MonitorEvent::Applied { pid, name, profile } => {
                        self.log_event(EventKind::Applied, pid, &name, &profile, "");
                        if self.cfg.settings.notify_on_apply {
                            let text = tf(self.lang(), "events.toast_applied", &[("profile", &profile), ("name", &name)]);
                            self.toast(text, false);
                        }
                    }
                    MonitorEvent::Failed { pid, name, reason } => {
                        self.log_event(EventKind::Failed, pid, &name, "", &reason);
                        if self.cfg.settings.notify_on_apply {
                            let text = tf(self.lang(), "events.toast_failed", &[("profile", &name), ("name", &name)]);
                            self.toast(text, true);
                        }
                    }
                    MonitorEvent::ProfileSwitched { binding_id, profile_id } => {
                        if let Some(b) = self.cfg.bindings.iter_mut().find(|b| b.id == binding_id) {
                            b.active_profile = profile_id.clone();
                            changed = true;
                        }
                        let name = profile_id.as_deref().map(|id| self.profile_name(id)).unwrap_or_default();
                        self.log_event(EventKind::ProfileSwitched, 0, "CoreTuner", &name, "");
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.monitor_alive = false;
                    break;
                }
            }
        }
        if changed {
            self.mark_dirty();
        }
        let _ = need_icons;
    }

    /// Обработать результаты фоновых задач.
    pub fn drain_tasks(&mut self, ctx: &egui::Context) {
        while let Ok(res) = self.task_rx.try_recv() {
            match res {
                TaskResult::PickedFile(Some(path)) => {
                    self.ui.file.set_target(&path);
                    self.tab = Tab::ByFile;
                }
                TaskResult::PickedFile(None) => {}
                TaskResult::ExportPath(None) | TaskResult::ImportPath(None) => {}
                TaskResult::ExportPath(Some(path)) => match storage::export_profiles(&self.cfg, &path) {
                    Ok(()) => {
                        let msg = self.text("profiles.exported").to_string();
                        self.toast(msg, false);
                    }
                    Err(e) => {
                        let msg = self.fmt("err.export", &[("e", &format!("{e:#}"))]);
                        self.toast(msg, true);
                    }
                },
                TaskResult::ImportPath(Some(path)) => match storage::import_profiles(&mut self.cfg, &path) {
                    Ok(n) => {
                        let msg = self.fmt("profiles.imported", &[("n", &n.to_string())]);
                        self.toast(msg, false);
                        self.mark_dirty();
                        self.sync_tray();
                    }
                    Err(e) => {
                        let msg = self.fmt("err.import", &[("e", &format!("{e:#}"))]);
                        self.toast(msg, true);
                    }
                },
                TaskResult::RestartAdmin(Ok(())) => {
                    self.save_now();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                TaskResult::RestartAdmin(Err(e)) => {
                    let msg = self.fmt("err.restart_admin", &[("e", &e)]);
                    self.toast(msg, true);
                }
                TaskResult::Icon { path, image } => {
                    if let Some(img) = image {
                        let texture = ctx.load_texture(
                            format!("icon:{path}"),
                            img,
                            egui::TextureOptions::LINEAR,
                        );
                        self.icons.insert(path, texture);
                    } else {
                        self.icon_missing.insert(path);
                    }
                }
            }
        }
    }
}

/// Форматированное время для журнала событий (без внешних крейтов, UTC).
fn timestamp() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Показать окно и сфокусировать его.
pub fn show_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// Реальная видимость нативного окна (скрытое в трей окно не видно).
fn native_window_visible() -> Option<bool> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindowVisible};
    // SAFETY: простые вызовы WinAPI без разыменования указателей.
    unsafe {
        let hwnd = FindWindowW(None, w!("CoreTuner")).ok()?;
        Some(IsWindowVisible(hwnd).as_bool())
    }
}

impl eframe::App for CoreTunerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_index = self.frame_index.wrapping_add(1);

        // Сигнал «показать окно» от второго экземпляра.
        let was_visible = self.window_visible;
        if self.show_rx.try_iter().count() > 0 {
            show_window(ctx);
            self.window_visible = true;
        }
        if let Some(v) = native_window_visible() {
            self.window_visible = v;
        }
        // После показа окно может отрисовать кадр, подготовленный ещё скрытым:
        // принудительный кадр сразу, иначе текст/контролы остаются тусклыми до таймера.
        if self.window_visible && !was_visible {
            ctx.request_repaint();
        }

        self.drain_monitor();
        self.drain_tasks(ctx);

        for a in self.tray.poll() {
            self.handle_tray_action(a, ctx);
        }

        // Закрытие окна кнопкой ×.
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.cfg.settings.minimize_to_tray_on_close && self.tray.is_available() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.window_visible = false;
            } else {
                self.save_now();
                let _ = self.cmd_tx.send(MonitorCmd::Stop);
            }
        }
        // Значок трея: всегда, либо пока окно скрыто.
        self.update_tray_visibility(!self.window_visible);

        if self.dirty && self.last_save.elapsed() >= SAVE_DEBOUNCE {
            let _ = self.cmd_tx.send(MonitorCmd::ConfigChanged(self.cfg.clone()));
            self.save_now();
        }

        if self.window_visible {
            self.load_icons();
        }
        if self.ui.settings.theme_dirty {
            ui::apply_style(ctx, self.cfg.settings.theme);
            self.ui.settings.theme_dirty = false;
            ctx.request_repaint();
        }
        if self.ui.settings.lang_dirty {
            self.sync_tray();
            self.ui.settings.lang_dirty = false;
        }
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);

        // Видимое окно обновляется чаще; скрытое — только для опроса трея.
        ctx.request_repaint_after(if self.window_visible { VISIBLE_REPAINT } else { HIDDEN_REPAINT });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.window_visible {
            return;
        }
        ui::draw(self, ui);
        let ctx = ui.ctx().clone();
        ui::draw_dialogs(self, &ctx);
    }

    fn on_exit(&mut self) {
        self.save_now();
        let _ = self.cmd_tx.send(MonitorCmd::Stop);
        self.tray.set_visible(false);
    }
}

/// Форматирование приоритета для таблицы.
pub fn priority_label(p: Option<PriorityClass>, lang: Lang) -> String {
    match p {
        Some(p) => t(lang, p.key()).to_string(),
        None => t(lang, "common.none").to_string(),
    }
}

/// Цвет статуса процесса.
pub fn status_color(p: &ProcInfo, dark: bool) -> Color32 {
    if p.protected {
        Color32::from_rgb(0xC0, 0x8A, 0x2E)
    } else if !p.accessible {
        Color32::from_rgb(0xC0, 0x4A, 0x4A)
    } else if dark {
        Color32::from_rgb(0x8A, 0x9A, 0x8A)
    } else {
        Color32::from_rgb(0x60, 0x6A, 0x60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CpuSet;

    #[test]
    fn tab_keys_unique() {
        let mut set = HashSet::new();
        for t in Tab::ALL {
            assert!(set.insert(t.key()), "повтор ключа вкладки");
        }
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(512), "512 Б");
        assert_eq!(fmt_bytes(2048), "2 КБ");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5 МБ");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 ГБ");
    }

    #[test]
    fn affinity_label_variants() {
        assert_eq!(affinity_label(None, 8, Lang::En), "not set");
        assert_eq!(affinity_label(Some(&CpuSet::new()), 8, Lang::En), "not set");
        assert_eq!(affinity_label(Some(&CpuSet::from_indices([0, 1, 2])), 8, Lang::En), "3 of 8");
        assert_eq!(affinity_label(Some(&CpuSet::from_indices([0, 1, 2])), 3, Lang::En), "all (3)");
    }

    #[test]
    fn timestamp_shape() {
        let ts = timestamp();
        assert_eq!(ts.len(), 8);
        assert!(ts.chars().all(|c| c.is_ascii_digit() || c == ':'));
    }

    #[test]
    fn priority_label_falls_back() {
        assert_eq!(priority_label(None, Lang::En), "—");
        assert_eq!(priority_label(Some(PriorityClass::High), Lang::En), "High");
    }
}



