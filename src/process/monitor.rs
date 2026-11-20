//! Фоновый поток мониторинга: опрос процессов, применение профилей, триггеры.
use crate::config::Config;
use crate::cpu::priority::{get_io_priority, get_priority, set_io_priority, set_memory_priority, set_priority};
use crate::cpu::topology::Topology;
use crate::cpu::{affinity, is_protected_name, SysError};
use crate::model::{CpuSet, PriorityClass, Profile};
use crate::process::list::{CpuMonitor, ProcInfo, ProcessLister};
use crate::profiles::matcher::{binding_active_profile, evaluate_triggers_stateful, matching_bindings, ProcView, TriggerState};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub enum MonitorCmd {
    Pause(bool),
    ConfigChanged(Config),
    ReapplyNow,
    SetInterval(u64),
    Stop,
}

#[derive(Clone, Debug)]
pub enum MonitorEvent {
    Applied { pid: u32, name: String, profile: String },
    Failed { pid: u32, name: String, reason: String },
    Snapshot(Vec<ProcInfo>),
    CpuUsage(Vec<f32>),
    ProfileSwitched { binding_id: String, profile_id: Option<String> },
    /// Процессу возвращены исходные affinity/приоритет.
    Reverted { pid: u32, name: String },
}

/// Что известно о процессе, к которому уже применялся профиль.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tracked {
    start_time: u64,
    profile: Option<String>,
}

/// Исходные настройки процесса до первого применения профиля и то, что применили мы.
#[derive(Clone, Debug)]
struct Original {
    /// Защита от повторного использования PID.
    start_time: u64,
    name: String,
    affinity: Option<CpuSet>,
    priority: Option<PriorityClass>,
    /// Значения, прочитанные из системы после нашего применения.
    applied_affinity: Option<CpuSet>,
    applied_priority: Option<PriorityClass>,
}

/// Что вернуть процессу. Поле откатывается, только если сейчас оно равно применённому нами:
/// иначе его уже изменил пользователь или сам процесс, и затирать это нельзя.
fn revert_plan(
    orig: &Original,
    cur_aff: Option<&CpuSet>,
    cur_prio: Option<PriorityClass>,
) -> (Option<CpuSet>, Option<PriorityClass>) {
    let aff = match (&orig.affinity, &orig.applied_affinity) {
        (Some(o), Some(a)) if cur_aff == Some(a) && a != o => Some(o.clone()),
        _ => None,
    };
    let prio = match (orig.priority, orig.applied_priority) {
        (Some(o), Some(a)) if cur_prio == Some(a) && a != o => Some(o),
        _ => None,
    };
    (aff, prio)
}

/// Вернуть исходные настройки и забыть процесс. `None`, если ничего не менялось.
fn revert_process(
    originals: &mut HashMap<u32, Original>,
    pid: u32,
    start_time: u64,
    topo: &Topology,
) -> Option<MonitorEvent> {
    let orig = originals.remove(&pid)?;
    if orig.start_time != start_time {
        return None;
    }
    let cur_aff = affinity::get_affinity(pid, topo).ok();
    let cur_prio = get_priority(pid).ok();
    let (aff, prio) = revert_plan(&orig, cur_aff.as_ref(), cur_prio);
    let mut done = false;
    if let Some(a) = aff {
        done |= affinity::set_affinity(pid, &a, topo).is_ok();
    }
    if let Some(pr) = prio {
        done |= set_priority(pid, pr).is_ok();
    }
    done.then_some(MonitorEvent::Reverted { pid, name: orig.name })
}

/// Запомнить значения, которые реально стоят у процесса после применения профиля.
fn record_applied(originals: &mut HashMap<u32, Original>, pid: u32, profile: &Profile, topo: &Topology) {
    let Some(o) = originals.get_mut(&pid) else { return };
    if profile.affinity.as_ref().is_some_and(|s| !s.clamped(topo.total()).is_empty()) {
        if let Ok(a) = affinity::get_affinity(pid, topo) {
            o.applied_affinity = Some(a);
        }
    }
    if profile.priority.is_some() {
        if let Ok(pr) = get_priority(pid) {
            o.applied_priority = Some(pr);
        }
    }
}

pub fn spawn(cfg: Config, topo: Arc<Topology>) -> (Sender<MonitorCmd>, Receiver<MonitorEvent>, JoinHandle<()>) {
    let (cmd_tx, cmd_rx) = unbounded::<MonitorCmd>();
    let (evt_tx, evt_rx) = bounded::<MonitorEvent>(256);
    let handle = std::thread::Builder::new()
        .name("ct-monitor".into())
        .spawn(move || run(cfg, topo, cmd_rx, evt_tx))
        .unwrap_or_else(|e| {
            tracing::error!("не удалось запустить поток монитора: {e}");
            std::thread::spawn(|| {})
        });
    (cmd_tx, evt_rx, handle)
}

struct Monitor {
    cfg: Config,
    topo: Arc<Topology>,
    lister: ProcessLister,
    cpu: CpuMonitor,
    interval: Duration,
    paused: bool,
    enforce: bool,
    tracked: HashMap<u32, Tracked>,
    failed: HashMap<u32, String>,
    /// «Потолок» приоритета: (желаемый, фактический) после применения — ОС выдала меньше
    /// (например, Realtime без прав администратора даёт High). Повторно не применяем.
    capped: HashMap<u32, (PriorityClass, PriorityClass)>,
    /// Исходные настройки процессов для отката.
    originals: HashMap<u32, Original>,
    /// Живые процессы последнего тика: pid → время старта.
    alive: HashMap<u32, u64>,
    triggers: TriggerState,
    snapshot_tx: Sender<Vec<ProcInfo>>,
    snapshot_digest: Option<u64>,
    snapshot_at: Option<Instant>,
}

fn run(cfg: Config, topo: Arc<Topology>, cmd_rx: Receiver<MonitorCmd>, evt: Sender<MonitorEvent>) {
    // Тяжёлые снимки идут по отдельному каналу ёмкостью 1: UI разбирает их в своём темпе,
    // а монитор не копит их и не тормозит тики.
    let (snapshot_tx, snapshot_rx) = bounded::<Vec<ProcInfo>>(1);
    let evt_snapshot = evt.clone();
    let relay = std::thread::Builder::new()
        .name("ct-snapshot".into())
        .spawn(move || {
            while let Ok(procs) = snapshot_rx.recv() {
                match evt_snapshot.try_send(MonitorEvent::Snapshot(procs)) {
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => break,
                    // Канал событий переполнен — снимок устарел, следующий придёт на очередном тике.
                    Ok(()) | Err(crossbeam_channel::TrySendError::Full(_)) => {}
                }
            }
        });
    let _ = relay;

    let mut cpu = CpuMonitor::new();
    cpu.set_total(topo.total());

    let mut m = Monitor {
        interval: Duration::from_millis(cfg.settings.poll_interval_ms.clamp(500, 10_000)),
        paused: cfg.settings.paused,
        enforce: cfg.settings.reapply_enforce,
        cfg,
        topo,
        lister: ProcessLister::new(),
        cpu,
        tracked: HashMap::new(),
        failed: HashMap::new(),
        capped: HashMap::new(),
        originals: HashMap::new(),
        alive: HashMap::new(),
        triggers: TriggerState::new(),
        snapshot_tx,
        snapshot_digest: None,
        snapshot_at: None,
    };
    // Единый дедлайн: период тиков равен интервалу независимо от длительности самого тика.
    let mut next_tick = Instant::now();
    loop {
        if Instant::now() >= next_tick {
            m.tick(&evt);
            next_tick = Instant::now() + m.interval;
        }
        match cmd_rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
            Ok(cmd) => match m.handle_cmd(cmd) {
                Flow::Stop => break,
                Flow::TickNow => next_tick = Instant::now(),
                Flow::Continue => {}
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        // Уменьшенный интервал не должен ждать старого дедлайна.
        next_tick = next_tick.min(Instant::now() + m.interval);
    }
    if m.cfg.settings.revert_on_exit {
        m.revert_all();
    }
}

/// Результат обработки команды.
enum Flow {
    Continue,
    /// Выполнить внеочередной тик.
    TickNow,
    Stop,
}

impl Monitor {
    fn handle_cmd(&mut self, cmd: MonitorCmd) -> Flow {
        match cmd {
            MonitorCmd::Stop => Flow::Stop,
            MonitorCmd::Pause(p) => {
                let resumed = self.paused && !p;
                self.paused = p;
                if resumed {
                    Flow::TickNow
                } else {
                    Flow::Continue
                }
            }
            MonitorCmd::SetInterval(ms) => {
                self.interval = Duration::from_millis(ms.clamp(500, 10_000));
                Flow::Continue
            }
            MonitorCmd::ReapplyNow => {
                self.retrack();
                Flow::TickNow
            }
            MonitorCmd::ConfigChanged(cfg) => {
                // Переприменяем профили только при изменении привязок/профилей/триггеров:
                // смена темы или языка не должна затирать ручные правки.
                let reapply = cfg.bindings != self.cfg.bindings || cfg.profiles != self.cfg.profiles;
                let resumed = self.paused && !cfg.settings.paused;
                self.paused = cfg.settings.paused;
                self.enforce = cfg.settings.reapply_enforce;
                self.interval = Duration::from_millis(cfg.settings.poll_interval_ms.clamp(500, 10_000));
                self.cfg = cfg;
                if reapply {
                    self.retrack();
                }
                if reapply || resumed {
                    Flow::TickNow
                } else {
                    Flow::Continue
                }
            }
        }
    }

    /// Вернуть исходные настройки всем процессам, которые ещё живы (выход из приложения).
    fn revert_all(&mut self) {
        let pids: Vec<u32> = self.originals.keys().copied().collect();
        for pid in pids {
            if let Some(&start) = self.alive.get(&pid) {
                let _ = revert_process(&mut self.originals, pid, start, &self.topo);
            }
        }
    }

    /// Помечает все процессы как «не применённые», чтобы на следующем тике применить заново.
    fn retrack(&mut self) {
        for t in self.tracked.values_mut() {
            t.profile = None;
        }
        self.failed.clear();
        self.capped.clear();
        self.snapshot_at = None;
    }

    fn tick(&mut self, evt: &Sender<MonitorEvent>) {
        let procs = self.lister.refresh(&self.topo);
        let cpu = self.cpu.refresh();

        let views: Vec<ProcView> =
            procs.iter().map(|p| ProcView::new(p.pid, p.name.clone(), p.exe_path.clone())).collect();

        self.alive = procs.iter().map(|p| (p.pid, p.start_time)).collect();

        // Триггеры обновляют active_profile и на паузе: иначе после её снятия переход потеряется,
        // а UI рассинхронизируется. На паузе только не применяем профили к процессам.
        let changes = evaluate_triggers_stateful(&self.cfg, &views, &mut self.triggers);
        for (binding_id, profile_id) in changes {
            if let Some(b) = self.cfg.bindings.iter_mut().find(|b| b.id == binding_id) {
                if b.active_profile != profile_id {
                    b.active_profile = profile_id.clone();
                    self.send(evt, MonitorEvent::ProfileSwitched { binding_id, profile_id });
                }
            }
        }

        if self.paused {
            self.send_lossy(evt, MonitorEvent::CpuUsage(cpu));
            self.push_snapshot(procs);
            return;
        }

        self.tracked.retain(|pid, _| self.alive.contains_key(pid));
        self.failed.retain(|pid, _| self.alive.contains_key(pid));
        self.capped.retain(|pid, _| self.alive.contains_key(pid));
        self.originals.retain(|pid, _| self.alive.contains_key(pid));

        let mut applied = Vec::new();
        for p in &procs {
            if let Some(ev) = self.apply_one(p) {
                applied.push(ev);
            }
        }

        self.send_lossy(evt, MonitorEvent::CpuUsage(cpu));
        self.push_snapshot(procs);
        for e in applied {
            self.send(evt, e);
        }
    }

    /// Отправка Snapshot не чаще, чем раз в интервал опроса, и только при изменениях состава.
    /// Если канал занят, снимок отбрасывается и будет отправлен на следующем тике.
    fn push_snapshot(&mut self, procs: Vec<ProcInfo>) {
        let digest = snapshot_digest(&procs);
        let due = self
            .snapshot_at
            .map(|at| at.elapsed() >= self.interval)
            .unwrap_or(true);
        if self.snapshot_digest == Some(digest) && !due {
            return;
        }
        if self.snapshot_tx.try_send(procs).is_ok() {
            self.snapshot_digest = Some(digest);
            self.snapshot_at = Some(Instant::now());
        }
    }

    /// Важные события (Applied/Failed/ProfileSwitched) не теряем.
    fn send(&self, evt: &Sender<MonitorEvent>, e: MonitorEvent) {
        if evt.send(e).is_err() {
            tracing::debug!("приёмник событий монитора закрыт");
        }
    }

    /// Некритичные события (загрузка CPU) при переполнении канала отбрасываются.
    fn send_lossy(&self, evt: &Sender<MonitorEvent>, e: MonitorEvent) {
        let _ = evt.try_send(e);
    }

    /// Применяет профиль к процессу, если он новый, сменил образ или сбросил настройки.
    fn apply_one(&mut self, p: &ProcInfo) -> Option<MonitorEvent> {
        if p.protected || is_protected_name(&p.name) {
            return None;
        }
        let view = ProcView::new(p.pid, p.name.clone(), p.exe_path.clone());
        let binding = matching_bindings(&self.cfg, &view).into_iter().next();
        let profile = binding.as_ref().and_then(|b| binding_active_profile(&self.cfg, b));

        let tracked = self.tracked.get(&p.pid);
        let same_image = tracked.map(|t| t.start_time == p.start_time).unwrap_or(false);
        let profile_id = profile.map(|pr| pr.id.clone());
        let profile_changed = tracked.map(|t| t.profile != profile_id).unwrap_or(true);

        if same_image && !profile_changed && !self.enforce {
            return None;
        }
        if same_image && !profile_changed && self.enforce && !self.drifted(p, profile) {
            return None;
        }
        if !same_image || profile_changed {
            self.capped.remove(&p.pid);
        }
        self.tracked.insert(p.pid, Tracked { start_time: p.start_time, profile: profile_id.clone() });

        // Профиль перестал действовать (отвязка, on_exit, удаление): возвращаем исходные настройки.
        let Some(profile) = profile else {
            return revert_process(&mut self.originals, p.pid, p.start_time, &self.topo);
        };

        if !self.originals.get(&p.pid).is_some_and(|o| o.start_time == p.start_time) {
            self.originals.insert(
                p.pid,
                Original {
                    start_time: p.start_time,
                    name: p.name.clone(),
                    affinity: p.affinity.clone(),
                    priority: p.priority,
                    applied_affinity: None,
                    applied_priority: None,
                },
            );
        }

        let result = apply_profile(p.pid, profile, &self.topo);
        record_applied(&mut self.originals, p.pid, profile, &self.topo);
        match result {
            Ok(()) => {
                self.failed.remove(&p.pid);
                if let Some(want) = profile.priority {
                    match get_priority(p.pid) {
                        Ok(actual) if actual != want => {
                            self.capped.insert(p.pid, (want, actual));
                        }
                        _ => {
                            self.capped.remove(&p.pid);
                        }
                    }
                }
                tracing::info!("применён профиль {} к {} (PID {})", profile.name, p.name, p.pid);
                Some(MonitorEvent::Applied { pid: p.pid, name: p.name.clone(), profile: profile.id.clone() })
            }
            Err(e) => {
                if self.failed.contains_key(&p.pid) {
                    return None;
                }
                let reason = e.to_string();
                tracing::warn!("не удалось применить профиль к {} (PID {}): {reason}", p.name, p.pid);
                self.failed.insert(p.pid, reason.clone());
                Some(MonitorEvent::Failed { pid: p.pid, name: p.name.clone(), reason })
            }
        }
    }

    /// Отличаются ли текущие значения процесса от желаемых профилем.
    fn drifted(&self, p: &ProcInfo, profile: Option<&Profile>) -> bool {
        let Some(profile) = profile else { return false };
        let affinity_drift = profile.affinity.as_ref().is_some_and(|want| {
            let want = want.clamped(self.topo.total());
            !want.is_empty() && p.affinity.as_ref() != Some(&want)
        });
        // ОС могла выдать меньше запрошенного (Realtime без админа → High): это не дрейф.
        let priority_drift = profile
            .priority
            .is_some_and(|want| priority_drifted(want, p.priority, self.capped.get(&p.pid).copied()));
        // IO сравниваем только если он задан профилем и читается; mem_priority не сравниваем.
        let io_drift = profile
            .io_priority
            .is_some_and(|want| get_io_priority(p.pid).ok().is_some_and(|cur| cur != want));
        affinity_drift || priority_drift || io_drift
    }
}

/// Применение профиля к процессу. Пустая после clamp маска означает «не менять affinity».
/// Первая же ошибка доступа возвращается наверх.
pub fn apply_profile(pid: u32, profile: &Profile, topo: &Topology) -> Result<(), SysError> {
    if let Some(set) = &profile.affinity {
        let set = set.clamped(topo.total());
        if !set.is_empty() {
            affinity::set_affinity(pid, &set, topo)?;
        }
    }
    if let Some(p) = profile.priority {
        set_priority(pid, p)?;
    }
    if let Some(p) = profile.io_priority {
        set_io_priority(pid, p)?;
    }
    if let Some(p) = profile.mem_priority {
        set_memory_priority(pid, p)?;
    }
    Ok(())
}

/// Отличается ли текущий приоритет от желаемого, с учётом известного «потолка» ОС.
fn priority_drifted(want: PriorityClass, cur: Option<PriorityClass>, cap: Option<(PriorityClass, PriorityClass)>) -> bool {
    cur != Some(want) && !cur.is_some_and(|c| cap == Some((want, c)))
}

/// Ключ для отслеживания процесса: (pid, время старта).
#[cfg(test)]
pub fn proc_key(p: &ProcInfo) -> (u32, u64) {
    (p.pid, p.start_time)
}

/// Дешёвая свёртка состава процессов: pid, время старта и потребление памяти.
fn snapshot_digest(procs: &[ProcInfo]) -> u64 {
    let mut h: u64 = 1469598103934665603;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(1099511628211);
    };
    for p in procs {
        mix(p.pid as u64);
        mix(p.start_time);
        mix(p.ram_bytes);
        mix(p.cpu_percent.to_bits() as u64 >> 40);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::topology::RawCore;
    use crate::model::CpuSet;

    fn info(pid: u32, name: &str, start: u64) -> ProcInfo {
        ProcInfo { pid, name: name.into(), start_time: start, ..Default::default() }
    }

    fn orig(aff: &[usize], applied: &[usize], prio: PriorityClass, applied_prio: PriorityClass) -> Original {
        Original {
            start_time: 1,
            name: "a.exe".into(),
            affinity: Some(CpuSet::from_indices(aff.iter().copied())),
            priority: Some(prio),
            applied_affinity: Some(CpuSet::from_indices(applied.iter().copied())),
            applied_priority: Some(applied_prio),
        }
    }

    #[test]
    fn revert_restores_only_what_we_applied() {
        use PriorityClass::{BelowNormal, High, Normal};
        let o = orig(&[0, 1, 2, 3], &[0, 1], Normal, BelowNormal);
        let applied = CpuSet::from_indices([0, 1]);
        // Всё осталось как мы поставили — откатываем оба поля.
        let (a, p) = revert_plan(&o, Some(&applied), Some(BelowNormal));
        assert_eq!(a, Some(CpuSet::from_indices([0, 1, 2, 3])));
        assert_eq!(p, Some(Normal));
        // Пользователь поменял приоритет и маску руками — не трогаем.
        let manual = CpuSet::from_indices([2]);
        assert_eq!(revert_plan(&o, Some(&manual), Some(High)), (None, None));
        // Состояние не прочиталось — не трогаем.
        assert_eq!(revert_plan(&o, None, None), (None, None));
    }

    #[test]
    fn revert_skips_unchanged_fields() {
        use PriorityClass::Normal;
        let o = orig(&[0, 1], &[0, 1], Normal, Normal);
        let cur = CpuSet::from_indices([0, 1]);
        assert_eq!(revert_plan(&o, Some(&cur), Some(Normal)), (None, None));
    }

    #[test]
    fn realtime_capped_to_high_is_not_drift() {
        use PriorityClass::{High, Normal, Realtime};
        assert!(priority_drifted(Realtime, Some(High), None));
        assert!(!priority_drifted(Realtime, Some(High), Some((Realtime, High))));
        // Процесс сам сбросил приоритет — дрейф, даже если потолок известен.
        assert!(priority_drifted(Realtime, Some(Normal), Some((Realtime, High))));
        assert!(!priority_drifted(High, Some(High), None));
        assert!(priority_drifted(High, None, None));
    }

    #[test]
    fn proc_key_uses_start_time() {
        assert_eq!(proc_key(&info(7, "a.exe", 100)), (7, 100));
        assert_ne!(proc_key(&info(7, "a.exe", 200)), (7, 100));
    }

    #[test]
    fn snapshot_digest_detects_new_process() {
        let a = vec![info(1, "a.exe", 10), info(2, "b.exe", 20)];
        let mut b = a.clone();
        assert_eq!(snapshot_digest(&a), snapshot_digest(&b));
        b.push(info(3, "c.exe", 30));
        assert_ne!(snapshot_digest(&a), snapshot_digest(&b));
        // Другой образ процесса с тем же pid — тоже изменение.
        let mut c = a.clone();
        c[0].start_time = 99;
        assert_ne!(snapshot_digest(&a), snapshot_digest(&c));
    }

    #[test]
    fn empty_affinity_is_not_applied() {
        let topo = Topology::from_raw(&[RawCore { group: 0, efficiency_class: 0, mask: 0b1 }]);
        let mut profile = Profile::new("p".into(), "P");
        profile.affinity = Some(CpuSet::new());
        // Пустая маска после clamp не доходит до системы: на pid 0 не будет ошибки.
        assert!(apply_profile(0, &profile, &topo).is_ok());
    }
}

#[cfg(test)]
mod integration {
    use super::*;
    use crate::config::Config;
    use crate::model::{AppBinding, CpuSet, MatchMode, PriorityClass, Profile};
    use std::time::{Duration, Instant};

    /// Сквозной тест: запущенный после монитора `ping.exe` получает маску ядер и приоритет профиля.
    #[test]
    fn applies_profile_to_newly_started_process() {
        let topo = Arc::new(crate::cpu::topology::Topology::detect().expect("topology"));
        if topo.total() < 2 {
            return;
        }
        let mut p = Profile::new("p1".into(), "Test");
        p.affinity = Some(CpuSet::from_indices([0, 2]));
        p.priority = Some(PriorityClass::BelowNormal);
        let mut cfg = Config::default();
        cfg.settings.poll_interval_ms = 500;
        cfg.profiles.push(p);
        cfg.bindings.push(AppBinding {
            id: "b1".into(),
            target: "ping.exe".into(),
            mode: MatchMode::Name,
            enabled: true,
            profile_ids: vec!["p1".into()],
            active_profile: Some("p1".into()),
            triggers: Vec::new(),
        });
        let (tx, rx, handle) = spawn(cfg, Arc::clone(&topo));
        std::thread::sleep(Duration::from_millis(1200));
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping");
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut applied = false;
        let mut events = Vec::new();
        while Instant::now() < deadline && !applied {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                match ev {
                    MonitorEvent::Applied { pid, .. } if pid == child.id() => applied = true,
                    MonitorEvent::Failed { reason, .. } => events.push(reason),
                    _ => {}
                }
            }
        }
        let aff = crate::cpu::affinity::get_affinity(child.id(), &topo).ok();
        let prio = crate::cpu::priority::get_priority(child.id()).ok();
        let _ = child.kill();
        let _ = child.wait();
        let _ = tx.send(MonitorCmd::Stop);
        let _ = handle.join();
        assert!(applied, "Applied не получен; failures={events:?}");
        assert_eq!(aff, Some(CpuSet::from_indices([0, 2])));
        assert_eq!(prio, Some(PriorityClass::BelowNormal));
    }
}



