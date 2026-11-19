# CoreTuner — контракт модулей (источник истины для всех исполнителей)

Windows-only, Rust 2021, deps в Cargo.toml (eframe/egui 0.37, tray-icon 0.26, muda 0.21, windows 0.62, winreg 0.56,
sysinfo 0.40, serde/serde_json, tracing, anyhow, thiserror, crossbeam-channel, rfd 0.18, image, winres).
Правила: без `unwrap`/`expect` в рабочем коде (в тестах можно), без warnings и clippy-замечаний,
`#![windows_subsystem = "windows"]` в release. Не меняй чужие файлы и сигнатуры ниже. Недостающие мелкие
зависимости не добавляй без необходимости. Если API крейта отличается от ожидаемого — смотри исходники в
`%USERPROFILE%\.cargo\registry\src\*\<crate>-<ver>\`. Код проверяй `cargo check` (интеграцию делает lead,
поэтому допустимо, что чужие модули ещё пусты — тогда проверяй свой модуль заглушками в своей области).

## Файлы и владельцы
- lead: Cargo.toml, build.rs, manifest, src/main.rs, src/model.rs, src/config.rs (готовы до старта исполнителей)
- A (системный слой): src/cpu/{mod,topology,affinity,priority}.rs, src/process/{mod,list}.rs
- B (логика): src/profiles/{mod,storage,matcher}.rs, src/process/monitor.rs, src/autostart.rs, src/single.rs
- C (UI): src/app.rs, src/ui/{mod,cores,processes,profiles,settings}.rs, src/tray.rs, src/i18n.rs

## Общие типы (src/model.rs — уже написан lead'ом, читай его)
`PriorityClass`, `Profile`, `AppBinding`, `Settings`, `Config`, `Lang`, `ThemeMode`, `MatchMode`, `CpuSet` (маска по группам).

## A. cpu / process (WinAPI)
```rust
// cpu/topology.rs
pub struct LogicalCpu { pub index: usize /*глобальный*/, pub group: u16, pub bit: u8 /*0..63 в группе*/, pub core: usize /*индекс физ. ядра*/, pub smt_index: usize /*0=первый поток ядра, >0=виртуальный*/, }
pub struct PhysicalCore { pub index: usize, pub group: u16, pub efficiency_class: u8, pub threads: Vec<usize> /*глобальные index LogicalCpu*/ }
pub struct Topology { pub cores: Vec<PhysicalCore>, pub cpus: Vec<LogicalCpu>, pub is_hybrid: bool /*>1 различных efficiency_class*/ }
impl Topology {
    pub fn detect() -> anyhow::Result<Topology>;      // GetLogicalProcessorInformationEx(RelationProcessorCore); fallback: N одиночных ядер из GetSystemInfo
    pub fn from_raw(entries: &[RawCore]) -> Topology;  // чистая функция для тестов
    pub fn is_p_core(&self, core: usize) -> bool;      // hybrid && class == max class
    pub fn all_set(&self) -> CpuSet; pub fn physical_set(&self) -> CpuSet /*первый поток каждого ядра*/; pub fn virtual_set(&self) -> CpuSet;
}
pub struct RawCore { pub group: u16, pub efficiency_class: u8, pub mask: u64 /*биты потоков в группе*/ }
// cpu/affinity.rs
pub fn get_affinity(pid: u32, topo: &Topology) -> Result<CpuSet, SysError>;  // >64: GetProcessGroupAffinity + GetProcessAffinityMask; для многогруппных — Cpu Sets API (GetProcessDefaultCpuSets) 
pub fn set_affinity(pid: u32, set: &CpuSet, topo: &Topology) -> Result<(), SysError>; // 1 группа: SetProcessAffinityMask; >1 группы: SetProcessDefaultCpuSets (CPU Sets). Пустой набор -> Err(SysError::Invalid)
// cpu/priority.rs
pub fn get_priority(pid) -> Result<PriorityClass, SysError>; pub fn set_priority(pid, PriorityClass) -> Result<(), SysError>;
pub fn get_io_priority / set_io_priority(pid, IoPriority) (NtSetInformationProcess ProcessIoPriority 33 через GetProcAddress из ntdll; 0..4 = VeryLow,Low,Normal,High(нужны права) )
pub fn set_memory_priority(pid, MemPriority 1..5) via SetProcessInformation(ProcessMemoryPriority)
// cpu/mod.rs
#[derive(thiserror::Error)] pub enum SysError { #[error("нет доступа")] AccessDenied, #[error("процесс не найден")] NotFound, #[error("некорректные данные")] Invalid, #[error("{0}")] Other(String) }
pub fn is_protected_name(exe_name: &str) -> bool; // список исключений: system, smss.exe, csrss.exe, wininit.exe, services.exe, lsass.exe, winlogon.exe, svchost.exe?, registry, memcompression, dwm.exe, fontdrvhost.exe, coretuner.exe(self) и т.п.
pub fn is_elevated() -> bool; pub fn restart_as_admin() -> anyhow::Result<()>; // ShellExecuteW "runas" с --minimized не добавлять; после успеха вызывающий завершает процесс
pub fn is_pid_alive_same_image(...) не нужно.
// process/list.rs
pub struct ProcInfo { pub pid: u32, pub name: String, pub exe_path: Option<String>, pub cpu_percent: f32, pub ram_bytes: u64, pub priority: Option<PriorityClass>, pub affinity: Option<CpuSet>, pub has_window: bool, pub accessible: bool, pub protected: bool, pub start_time: u64 }
pub struct ProcessLister { .. } // хранит sysinfo::System; fn new()->Self; fn refresh(&mut self, topo:&Topology)->Vec<ProcInfo> (sysinfo refresh процессов + EnumWindows для has_window (видимые окна верхнего уровня) + OpenProcess query для priority/affinity; ошибки доступа -> accessible=false)
pub fn per_cpu_usage() хранится в `CpuMonitor { new(), refresh(&mut self)->Vec<f32> }` (sysinfo, по глобальному index логического CPU)
pub fn exe_icon(path:&str) -> Option<egui::ColorImage> // SHGetFileInfo/ExtractIconEx → HICON → GetDIBits 32x32 (best-effort)
```
Тесты: Topology::from_raw (SMT, hybrid, >64 потоков в 2 группах), CpuSet-операции — в model.rs, расчёт масок.

## B. profiles / monitor / autostart / single
```rust
// profiles/storage.rs
pub fn config_dir() -> PathBuf;                 // %APPDATA%\CoreTuner (создаёт)
pub fn load() -> Config;                        // читает config.json; при ошибке парса: копия config.json.corrupt-<ts>, вернуть Config::default(); миграция версий (migrate(value)->Result<Config>), validate (обрезка пустых масок, дубликатов id)
pub fn save(cfg:&Config) -> anyhow::Result<()>; // атомарно: .tmp + fsync + rename (MoveFileExW REPLACE_EXISTING)
pub fn export_profiles(cfg:&Config, path:&Path)->anyhow::Result<()>; pub fn import_profiles(cfg:&mut Config, path:&Path)->anyhow::Result<usize>; // слияние, новые id при коллизии
// profiles/matcher.rs
pub fn binding_matches(b:&AppBinding, p:&ProcView)->bool  // ProcView{name,exe_path:Option<String>}: MatchMode::Name — case-insens по имени; Path — нормализованный путь без учёта регистра/слэшей
pub fn resolve_profile<'a>(cfg:&'a Config, p:&ProcView)->Option<&'a Profile> // сначала привязка с активным профилем (binding.active_profile), потом переопределения по триггеру (Trigger::WhenRunning(app) — профиль активируется пока запущено app)
pub fn pick_active_profile(cfg:&Config, running:&[ProcView])->... — см. model.rs doc
// process/monitor.rs
pub enum MonitorCmd { Pause(bool), ConfigChanged(Config), ReapplyNow, SetInterval(u64 /*мс*/), Stop }
pub enum MonitorEvent { Applied{pid:u32,name:String,profile:String}, Failed{pid:u32,name:String,reason:String}, Snapshot(Vec<ProcInfo>) , CpuUsage(Vec<f32>) }
pub fn spawn(cfg:Config, topo:Arc<Topology>) -> (Sender<MonitorCmd>, Receiver<MonitorEvent>, JoinHandle<()>)
// поток: интервал из Settings.poll_interval_ms; каждый тик: refresh ProcessLister + CpuMonitor, отправка Snapshot/CpuUsage (Snapshot — только если UI открыт? всегда, но дёшево),
// новые PID (не виденные ранее / другой start_time) сопоставить с профилями → применить affinity+priority (+io/mem) → Applied; не защищённые; AccessDenied → Failed один раз на PID.
// опция reapply_enforce: сравнивать текущие значения и применять заново. Pause учитывается. Триггеры WhenRunning/OnExit переключают active_profile привязки и шлют MonitorEvent::ProfileSwitched{app,profile}.
// autostart.rs
pub fn is_enabled()->bool; pub fn set_enabled(on:bool, minimized:bool)->anyhow::Result<()>; // HKCU Run "CoreTuner" = "\"<exe>\" --minimized"
// single.rs
pub enum Instance { First(Guard), Already } pub fn acquire()->Instance; // именованный mutex Local\CoreTuner_SingleInstance
pub fn signal_existing(); // найти окно по заголовку/классу и отправить именованное событие "Local\CoreTuner_Show" ; 
pub fn spawn_show_listener(tx: impl Fn()+Send+'static); // поток ждёт на событии, вызывает tx
```
Тесты: matcher (имя/путь/регистр/слэши, приоритет привязок), storage (roundtrip, миграция v0→v1, повреждённый файл, атомарная запись во временную папку, import/export).

## C. UI/tray/i18n
```rust
// i18n.rs
pub fn t(lang: Lang, key: &'static str) -> &'static str   // таблицы RU/EN через match; при отсутствии ключа — сам ключ. Все UI-строки идут через t().
// tray.rs
pub struct Tray; impl Tray { pub fn new(profiles:&[(String /*id*/,String /*name*/)], lang:Lang, visible:bool)->anyhow::Result<Tray>; pub fn rebuild_menu(...); pub fn set_visible(&self,bool); pub fn poll(&self)->Vec<TrayAction>; pub fn notify(&self, title:&str, body:&str) }
pub enum TrayAction { Open, SwitchProfile(String), TogglePause, Exit }  // клик/двойной клик по иконке → Open
// app.rs: struct CoreTunerApp: eframe::App. Владеет Config, Topology, каналами монитора, Tray, выбранным PID. 
// Закрытие ×: если settings.minimize_to_tray_on_close и трей виден → CancelClose + Visible(false). Старт с --minimized → окно скрыто (ViewportBuilder::with_visible(false)), трей принудительно виден.
// Если трей отключён настройкой «всегда показывать», иконка показывается только пока окно свёрнуто в трей.
// Вкладки: Процессы | По файлу запуска | Профили | Настройки. Справа/снизу панель «Ядра и приоритет» для выбранного процесса.
```
UI ядер: физическое ядро — крупная плитка (сплошная рамка, «Core N», метка P/E), внутри мелкие плитки потоков:
первый — обычный поток, остальные (smt_index>0) — пунктирная рамка (рисуется отрезками), «V», «Thread N (virtual)»; зелёный/серый; полоса загрузки; легенда;
кнопки Все / Только физические / Только виртуальные / Инвертировать; применяются кнопкой «Применить» или сразу (чекбокс «Применять сразу»).

---
## Уточнения lead (приоритетнее текста выше)
- `CpuSet` = множество **глобальных индексов** логических CPU (`BTreeSet<usize>`, см. model.rs). Группа/бит берутся из `Topology.cpus[i]`.
  Topology предоставляет: `total()`, `group_masks(&CpuSet)->Vec<(u16,u64)>` (маски по группам), `set_from_group_masks(&[(u16,u64)])->CpuSet`.
- Профиль: `affinity: Option<CpuSet>`, `priority: Option<PriorityClass>`, `io_priority`, `mem_priority` (None = не менять).
- Привязка: `AppBinding{target,mode,enabled,profile_ids,active_profile,triggers}`; `Trigger{watch,watch_mode,on_start,on_exit}` — id профилей.
  Монитор: профиль к процессу = binding.active_profile (если enabled и входит в profile_ids). Триггеры: когда процесс `watch` появляется →
  `binding.active_profile = on_start`, когда исчезли все → `on_exit` (если Some); монитор шлёт `MonitorEvent::ProfileSwitched{binding_id, profile_id: Option<String>}`;
  UI обязан применить это к своему Config и сохранить (монитор своей копии Config тоже обновляет).
  Уже запущенные процессы привязки при смене активного профиля переприменяются.
- MonitorEvent: Applied{pid,name,profile}, Failed{pid,name,reason}, Snapshot(Vec<ProcInfo>), CpuUsage(Vec<f32>), ProfileSwitched{..}.
- Config/Settings/Lang/ThemeMode — в src/config.rs. `Settings` имеет также reapply_enforce, notify_on_apply, paused, poll_interval_ms.
- Конструктор UI: `CoreTunerApp::new(cc:&eframe::CreationContext<'_>, cfg:Config, topo:Arc<Topology>, start_hidden:bool, show_rx: crossbeam_channel::Receiver<()>) -> Self`
  (show_rx: сигнал «показать окно» от второго экземпляра). Топология: `cpu::topology::Topology`.
- Модульные пути: `crate::cpu::{SysError,is_protected_name,is_elevated,restart_as_admin}`, `crate::cpu::{topology,affinity,priority}`,
  `crate::process::{list::{ProcInfo,ProcessLister,CpuMonitor,exe_icon}, monitor}`, `crate::profiles::{storage,matcher}`.
  `process/mod.rs`, `cpu/mod.rs`, `profiles/mod.rs`, `ui/mod.rs` содержат `pub mod ...;` (cpu/mod.rs также SysError и пр.).
- Без `#![allow]` для скрытия warnings; допустим `#[allow(dead_code)]` точечно только если реально нужно.
