//! Фоновый поток мониторинга: опрос процессов, применение профилей, триггеры.
use crate::config::Config;
use crate::cpu::priority::{get_io_priority, set_io_priority, set_memory_priority, set_priority};
use crate::cpu::topology::Topology;
use crate::cpu::{affinity, is_protected_name, SysError};
use crate::model::Profile;
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
}

/// Что известно о процессе, к которому уже применялся профиль.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tracked {
    start_time: u64,
    profile: Option<String>,
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
                if evt_snapshot.send(MonitorEvent::Snapshot(procs)).is_err() {
                    break;
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
        triggers: TriggerState::new(),
        snapshot_tx,
        snapshot_digest: None,
        snapshot_at: None,
    };
    loop {
        let wake = match cmd_rx.recv_timeout(m.interval) {
            Ok(MonitorCmd::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Ok(cmd) => {
                if !m.handle_cmd(cmd) {
                    break;
                }
                Instant::now()
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                m.tick(&evt);
                Instant::now()
            }
        };
        // Команды, накопившиеся за тик, разбираем без ожидания.
        let mut stop = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            if !m.handle_cmd(cmd) {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }
        // Пока ждём — держим канал отзывчивым, но не крутим процессор.
        if let Some(left) = m.interval.checked_sub(wake.elapsed()) {
            match cmd_rx.recv_timeout(left) {
                Ok(MonitorCmd::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => {
                    if !m.handle_cmd(cmd) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl Monitor {
    /// Возвращает false, если поток должен завершиться.
    fn handle_cmd(&mut self, cmd: MonitorCmd) -> bool {
        match cmd {
            MonitorCmd::Stop => return false,
            MonitorCmd::Pause(p) => self.paused = p,
            MonitorCmd::SetInterval(ms) => self.interval = Duration::from_millis(ms.clamp(500, 10_000)),
            MonitorCmd::ReapplyNow => {
                self.retrack();
            }
            MonitorCmd::ConfigChanged(cfg) => {
                let interval = cfg.settings.poll_interval_ms.clamp(500, 10_000);
                self.paused = cfg.settings.paused;
                self.enforce = cfg.settings.reapply_enforce;
                self.cfg = cfg;
                self.interval = Duration::from_millis(interval);
                self.retrack();
            }
        }
        true
    }

    /// Помечает все процессы как «не применённые», чтобы на следующем тике применить заново.
    fn retrack(&mut self) {
        for t in self.tracked.values_mut() {
            t.profile = None;
        }
        self.failed.clear();
        self.snapshot_at = None;
    }

    fn tick(&mut self, evt: &Sender<MonitorEvent>) {
        let procs = self.lister.refresh(&self.topo);
        let cpu = self.cpu.refresh();

        let views: Vec<ProcView> =
            procs.iter().map(|p| ProcView::new(p.pid, p.name.clone(), p.exe_path.clone())).collect();

        let updates = {
            let mut next = self.cfg.clone();
            let changes = evaluate_triggers_stateful(&next, &views, &mut self.triggers);
            let mut switches = Vec::new();
            for (binding_id, profile_id) in changes {
                if let Some(b) = next.bindings.iter_mut().find(|b| b.id == binding_id) {
                    if b.active_profile != profile_id {
                        b.active_profile = profile_id.clone();
                        switches.push((binding_id, profile_id));
                    }
                }
            }
            if switches.is_empty() {
                None
            } else {
                self.cfg = next;
                Some(switches)
            }
        };

        if self.paused {
            // Пауза: ничего не применяем и не переключаем. Состояние триггеров обновляем
            // (оно уже обновлено выше), чтобы после снятия паузы не сработали ложные переходы.
            self.send(evt, MonitorEvent::CpuUsage(cpu));
            self.push_snapshot(procs);
            return;
        }

        if let Some(switches) = updates {
            for (binding_id, profile_id) in switches {
                self.send(evt, MonitorEvent::ProfileSwitched { binding_id, profile_id });
            }
        }

        let alive: Vec<u32> = procs.iter().map(|p| p.pid).collect();
        self.tracked.retain(|pid, _| alive.contains(pid));
        self.failed.retain(|pid, _| alive.contains(pid));

        let mut applied = Vec::new();
        for p in &procs {
            if let Some(ev) = self.apply_one(p) {
                applied.push(ev);
            }
        }

        self.send(evt, MonitorEvent::CpuUsage(cpu));
        self.push_snapshot(procs);
        for e in applied {
            self.send(evt, e);
        }
    }

    /// Отправка Snapshot не чаще, чем раз в интервал опроса, и только при изменениях состава.
    fn push_snapshot(&mut self, procs: Vec<ProcInfo>) {
        let digest = snapshot_digest(&procs);
        let due = self
            .snapshot_at
            .map(|at| at.elapsed() >= self.interval)
            .unwrap_or(true);
        if self.snapshot_digest == Some(digest) && !due {
            return;
        }
        self.snapshot_digest = Some(digest);
        self.snapshot_at = Some(Instant::now());
        self.send_snapshot(procs);
    }

    fn send_snapshot(&self, procs: Vec<ProcInfo>) {
        if self.snapshot_tx.send(procs).is_err() {
            tracing::debug!("приёмник снимков закрыт");
        }
    }

    fn send(&self, evt: &Sender<MonitorEvent>, e: MonitorEvent) {
        if evt.send(e).is_err() {
            tracing::debug!("приёмник событий монитора закрыт");
        }
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
        self.tracked.insert(p.pid, Tracked { start_time: p.start_time, profile: profile_id.clone() });

        let profile = profile?;

        match apply_profile(p.pid, profile, &self.topo) {
            Ok(()) => {
                self.failed.remove(&p.pid);
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
        let priority_drift = profile.priority.is_some_and(|want| p.priority != Some(want));
        let io_drift = (profile.io_priority.is_some() || profile.mem_priority.is_some())
            && get_io_priority(p.pid).ok() != profile.io_priority;
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



