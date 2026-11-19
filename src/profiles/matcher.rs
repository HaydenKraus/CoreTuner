//! Сопоставление процессов с привязками и вычисление переключений по триггерам.
use crate::config::Config;
use crate::model::{AppBinding, MatchMode, Profile, Trigger};
use std::collections::HashMap;

/// Минимальное представление процесса для сопоставления (не тянет sysinfo).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcView {
    pub pid: u32,
    pub name: String,
    pub exe_path: Option<String>,
}

impl ProcView {
    pub fn new(pid: u32, name: impl Into<String>, exe_path: Option<String>) -> Self {
        ProcView { pid, name: name.into(), exe_path }
    }

    /// Представление процесса только по имени (pid 0, без пути).
    #[cfg(test)]
    pub fn by_name(name: impl Into<String>) -> Self {
        ProcView { pid: 0, name: name.into(), exe_path: None }
    }
}

/// Нормализация пути: слэши к `\`, нижний регистр, снятие `\\?\`.
fn normalize_path(p: &str) -> String {
    let p = p.trim();
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    p.replace('/', "\\").to_lowercase()
}

fn normalize_name(n: &str) -> String {
    n.trim().to_lowercase()
}

/// Совпадает ли привязка с процессом.
pub fn binding_matches(b: &AppBinding, p: &ProcView) -> bool {
    if !b.enabled {
        return false;
    }
    match b.mode {
        MatchMode::Name => normalize_name(&b.target) == normalize_name(&p.name),
        MatchMode::Path => match &p.exe_path {
            Some(path) => normalize_path(&b.target) == normalize_path(path),
            None => false,
        },
    }
}

/// Совпадает ли `watch` триггера с процессом (watch_mode учитывается так же).
pub fn trigger_matches(t: &Trigger, p: &ProcView) -> bool {
    match t.watch_mode {
        MatchMode::Name => normalize_name(&t.watch) == normalize_name(&p.name),
        MatchMode::Path => match &p.exe_path {
            Some(path) => normalize_path(&t.watch) == normalize_path(path),
            None => false,
        },
    }
}

/// Подходящие привязки для процесса. Path-привязка приоритетнее Name при прочих равных.
pub fn matching_bindings<'a>(cfg: &'a Config, p: &ProcView) -> Vec<&'a AppBinding> {
    let mut v: Vec<&AppBinding> = cfg.bindings.iter().filter(|b| binding_matches(b, p)).collect();
    v.sort_by_key(|b| match b.mode {
        MatchMode::Path => 0u8,
        MatchMode::Name => 1u8,
    });
    v
}

/// Профиль, назначенный привязке: активный, если он включён и входит в `profile_ids`.
pub fn binding_active_profile<'a>(cfg: &'a Config, b: &AppBinding) -> Option<&'a Profile> {
    if !b.enabled {
        return None;
    }
    let id = b.active_profile.as_deref()?;
    if !b.profile_ids.iter().any(|x| x == id) {
        return None;
    }
    cfg.profile(id)
}

/// Профиль процесса: сначала активный профиль подходящей привязки
/// (Path-привязка приоритетнее Name), затем переопределения по триггерам.
///
/// Используется только тестами: монитор выбирает профиль через
/// [`matching_bindings`] + [`binding_active_profile`] и отслеживает переходы
/// триггеров через [`evaluate_triggers_stateful`].
#[cfg(test)]
pub fn resolve_profile<'a>(cfg: &'a Config, p: &ProcView) -> Option<&'a Profile> {
    for b in matching_bindings(cfg, p) {
        if let Some(pr) = binding_active_profile(cfg, b) {
            return Some(pr);
        }
    }
    for b in cfg.bindings.iter().filter(|b| b.enabled) {
        for t in &b.triggers {
            if t.on_start.is_none() && t.on_exit.is_none() {
                continue;
            }
            if trigger_matches(t, p) {
                // Триггер сработал по этому же процессу: считаем его активным сейчас,
                // поэтому применяем on_start (в покое значение вернёт evaluate_triggers).
                if let Some(pr) = t.on_start.as_deref().and_then(|id| cfg.profile(id)) {
                    return Some(pr);
                }
            }
        }
    }
    None
}

/// Состояние предыдущего тика по триггерам, чтобы ловить переходы запущен/остановлен.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriggerState {
    running: HashMap<(String, String), bool>,
}

impl TriggerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Наблюдается ли `watch` привязки прямо сейчас (только для тестов).
    #[cfg(test)]
    pub fn is_running(&self, binding_id: &str, watch: &str) -> bool {
        self.running.get(&(binding_id.to_string(), watch.to_string())).copied().unwrap_or(false)
    }

    /// Забыть состояние привязки (только для тестов).
    #[cfg(test)]
    pub fn clear_binding(&mut self, binding_id: &str) {
        self.running.retain(|(b, _), _| b != binding_id);
    }
}

/// Вычисляет переключения активного профиля по триггерам текущего тика (без состояния).
/// Возвращает пары (binding_id, новый active_profile); `None` означает «вернуть профиль привязки к
/// значению по умолчанию» — его выбирает вызывающий (монитор).
/// Для отслеживания переходов запущен/остановлен используйте [`evaluate_triggers_stateful`].
#[cfg(test)]
pub fn evaluate_triggers(cfg: &Config, running: &[ProcView]) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for b in &cfg.bindings {
        if !b.enabled || b.triggers.is_empty() {
            continue;
        }
        for t in &b.triggers {
            let now = running.iter().any(|p| trigger_matches(t, p));
            match t.on_start.clone() {
                Some(id) if now && b.active_profile.as_deref() != Some(id.as_str()) => {
                    out.push((b.id.clone(), Some(id)));
                }
                _ if !now && t.on_exit.is_none() && b.active_profile.is_some() => {
                    out.push((b.id.clone(), None));
                }
                _ => {}
            }
        }
    }
    out
}

/// Тот же расчёт, но с состоянием предыдущего тика: возвращает только реальные переходы.
pub fn evaluate_triggers_stateful(
    cfg: &Config,
    running: &[ProcView],
    state: &mut TriggerState,
) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for b in &cfg.bindings {
        if !b.enabled || b.triggers.is_empty() {
            continue;
        }
        for t in &b.triggers {
            let now = running.iter().any(|p| trigger_matches(t, p));
            let key = (b.id.clone(), t.watch.clone());
            let was = state.running.insert(key, now).unwrap_or(false);
            if now == was {
                continue;
            }
            let new = if now { t.on_start.clone() } else { t.on_exit.clone() };
            match new {
                Some(id) => {
                    if b.active_profile.as_deref() != Some(id.as_str()) {
                        out.push((b.id.clone(), Some(id)));
                    }
                }
                // on_exit не задан (при остановке) — возвращаем профиль привязки к значению по умолчанию.
                None if !now && b.active_profile.is_some() => out.push((b.id.clone(), None)),
                None => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Profile, Trigger};

    fn binding(id: &str, target: &str, mode: MatchMode) -> AppBinding {
        AppBinding {
            id: id.into(),
            target: target.into(),
            mode,
            enabled: true,
            profile_ids: vec![],
            active_profile: None,
            triggers: vec![],
        }
    }

    fn cfg_with(bindings: Vec<AppBinding>, profiles: Vec<Profile>) -> Config {
        Config { bindings, profiles, ..Config::default() }
    }

    #[test]
    fn name_match_is_case_insensitive() {
        let b = binding("b", "Game.EXE", MatchMode::Name);
        assert!(binding_matches(&b, &ProcView::by_name("game.exe")));
        assert!(binding_matches(&b, &ProcView::by_name("GAME.EXE")));
        assert!(!binding_matches(&b, &ProcView::by_name("game2.exe")));
    }

    #[test]
    fn path_match_normalizes_case_and_slashes() {
        let b = binding("b", r"C:/Games/Game.exe", MatchMode::Path);
        let p = ProcView::new(1, "Game.exe", Some(r"c:\games\game.EXE".into()));
        assert!(binding_matches(&b, &p));
        let prefixed = ProcView::new(1, "Game.exe", Some(r"\\?\C:\Games\Game.exe".into()));
        assert!(binding_matches(&b, &prefixed));
    }

    #[test]
    fn path_binding_requires_exe_path() {
        let b = binding("b", r"C:\Games\Game.exe", MatchMode::Path);
        assert!(!binding_matches(&b, &ProcView::new(1, "Game.exe", None)));
    }

    #[test]
    fn disabled_binding_never_matches() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.enabled = false;
        assert!(!binding_matches(&b, &ProcView::by_name("game.exe")));
    }

    #[test]
    fn path_binding_wins_over_name() {
        let name_b = binding("bn", "game.exe", MatchMode::Name);
        let path_b = binding("bp", r"C:\Games\game.exe", MatchMode::Path);
        let cfg = cfg_with(vec![name_b, path_b], vec![]);
        let p = ProcView::new(1, "game.exe", Some(r"C:\Games\game.exe".into()));
        let m = matching_bindings(&cfg, &p);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, "bp");
    }

    #[test]
    fn resolve_profile_uses_active_profile_in_profile_ids() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.profile_ids = vec!["p1".into(), "p2".into()];
        b.active_profile = Some("p2".into());
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A"), Profile::new("p2".into(), "B")]);
        let got = resolve_profile(&cfg, &ProcView::by_name("game.exe")).unwrap();
        assert_eq!(got.id, "p2");
    }

    #[test]
    fn resolve_profile_ignores_active_outside_profile_ids() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.profile_ids = vec!["p1".into()];
        b.active_profile = Some("p2".into());
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A"), Profile::new("p2".into(), "B")]);
        assert!(resolve_profile(&cfg, &ProcView::by_name("game.exe")).is_none());
    }

    #[test]
    fn trigger_fires_on_start_and_exit() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.profile_ids = vec!["p1".into(), "p2".into()];
        b.triggers = vec![Trigger {
            watch: "launcher.exe".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: Some("p2".into()),
        }];
        let cfg = cfg_with(vec![b.clone()], vec![Profile::new("p1".into(), "A"), Profile::new("p2".into(), "B")]);

        let mut st = TriggerState::new();
        // тик 1: launcher не запущен → переходов нет
        assert!(evaluate_triggers_stateful(&cfg, &[], &mut st).is_empty());
        // тик 2: launcher запустился → on_start
        let running = vec![ProcView::by_name("Launcher.exe")];
        assert_eq!(evaluate_triggers_stateful(&cfg, &running, &mut st), vec![("b".to_string(), Some("p1".into()))]);
        // тик 3: без изменений → пусто
        assert!(evaluate_triggers_stateful(&cfg, &running, &mut st).is_empty());
        // тик 4: launcher закрылся → on_exit
        assert_eq!(evaluate_triggers_stateful(&cfg, &[], &mut st), vec![("b".to_string(), Some("p2".into()))]);
        // тик 5: без изменений → пусто
        assert!(evaluate_triggers_stateful(&cfg, &[], &mut st).is_empty());
    }

    #[test]
    fn trigger_without_on_exit_returns_none_to_reset() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.profile_ids = vec!["p1".into()];
        b.active_profile = Some("p1".into());
        b.triggers = vec![Trigger {
            watch: "tool.exe".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: None,
        }];
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A")]);
        let mut st = TriggerState::new();
        let running = vec![ProcView::by_name("tool.exe")];
        // active_profile уже p1 — переключения не шлём
        assert!(evaluate_triggers_stateful(&cfg, &running, &mut st).is_empty());
        // остановка и отсутствие on_exit → сброс к профилю по умолчанию
        assert_eq!(evaluate_triggers_stateful(&cfg, &[], &mut st), vec![("b".to_string(), None)]);
    }

    #[test]
    fn trigger_watch_by_path() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.triggers = vec![Trigger {
            watch: r"C:/Launcher/Launcher.exe".into(),
            watch_mode: MatchMode::Path,
            on_start: Some("p1".into()),
            on_exit: None,
        }];
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A")]);
        let mut st = TriggerState::new();
        let wrong = vec![ProcView::new(1, "Launcher.exe", Some(r"C:\Other\Launcher.exe".into()))];
        assert!(evaluate_triggers_stateful(&cfg, &wrong, &mut st).is_empty());
        let right = vec![ProcView::new(2, "Launcher.exe", Some(r"C:\Launcher\launcher.EXE".into()))];
        assert_eq!(evaluate_triggers_stateful(&cfg, &right, &mut st), vec![("b".to_string(), Some("p1".into()))]);
    }

    #[test]
    fn stateless_evaluate_triggers() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.profile_ids = vec!["p1".into()];
        b.triggers = vec![Trigger {
            watch: "launcher.exe".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: None,
        }];
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A")]);
        let running = vec![ProcView::by_name("launcher.exe")];
        assert_eq!(evaluate_triggers(&cfg, &running), vec![("b".to_string(), Some("p1".into()))]);
        assert!(evaluate_triggers(&cfg, &[]).is_empty());
    }

    #[test]
    fn trigger_ignores_disabled_binding() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.enabled = false;
        b.triggers = vec![Trigger {
            watch: "launcher.exe".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: None,
        }];
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A")]);
        let mut st = TriggerState::new();
        assert!(evaluate_triggers_stateful(&cfg, &[ProcView::by_name("launcher.exe")], &mut st).is_empty());
    }

    #[test]
    fn trigger_state_survives_multiple_watch_entries() {
        let mut b = binding("b", "game.exe", MatchMode::Name);
        b.triggers = vec![
            Trigger { watch: "a.exe".into(), watch_mode: MatchMode::Name, on_start: Some("p1".into()), on_exit: None },
            Trigger { watch: "c.exe".into(), watch_mode: MatchMode::Name, on_start: Some("p2".into()), on_exit: None },
        ];
        let cfg = cfg_with(vec![b], vec![Profile::new("p1".into(), "A"), Profile::new("p2".into(), "B")]);
        let mut st = TriggerState::new();
        let running = vec![ProcView::by_name("aerun.exe"), ProcView::by_name("a.exe")];
        assert_eq!(evaluate_triggers_stateful(&cfg, &running, &mut st), vec![("b".to_string(), Some("p1".into()))]);        assert!(st.is_running("b", "a.exe"));
        assert!(!st.is_running("b", "c.exe"));
        st.clear_binding("b");
        assert!(!st.is_running("b", "a.exe"));
    }
}
