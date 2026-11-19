//! Общие типы данных: набор ядер, приоритеты, профили и привязки к приложениям.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Набор логических процессоров по глобальному индексу (0..N, сквозная нумерация по группам).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CpuSet(pub BTreeSet<usize>);

impl CpuSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn from_indices<I: IntoIterator<Item = usize>>(it: I) -> Self {
        CpuSet(it.into_iter().collect())
    }
    pub fn contains(&self, i: usize) -> bool {
        self.0.contains(&i)
    }
    pub fn insert(&mut self, i: usize) {
        self.0.insert(i);
    }
    pub fn remove(&mut self, i: usize) {
        self.0.remove(&i);
    }
    pub fn set(&mut self, i: usize, on: bool) {
        if on {
            self.insert(i)
        } else {
            self.remove(i)
        }
    }
    pub fn toggle(&mut self, i: usize) {
        if !self.0.remove(&i) {
            self.0.insert(i);
        }
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().copied()
    }
    /// Инверсия относительно `total` процессоров.
    pub fn inverted(&self, total: usize) -> CpuSet {
        CpuSet((0..total).filter(|i| !self.0.contains(i)).collect())
    }
    /// Оставить только индексы < total (защита от профиля с другой машины).
    pub fn clamped(&self, total: usize) -> CpuSet {
        CpuSet(self.0.iter().copied().filter(|&i| i < total).collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum PriorityClass {
    Idle,
    BelowNormal,
    #[default]
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl PriorityClass {
    pub const ALL: [PriorityClass; 6] = [
        PriorityClass::Idle,
        PriorityClass::BelowNormal,
        PriorityClass::Normal,
        PriorityClass::AboveNormal,
        PriorityClass::High,
        PriorityClass::Realtime,
    ];
    pub fn raise(self) -> PriorityClass {
        let i = Self::ALL.iter().position(|&p| p == self).unwrap_or(2);
        Self::ALL[(i + 1).min(5)]
    }
    pub fn lower(self) -> PriorityClass {
        let i = Self::ALL.iter().position(|&p| p == self).unwrap_or(2);
        Self::ALL[i.saturating_sub(1)]
    }
    /// Ключ локализации.
    pub fn key(self) -> &'static str {
        match self {
            PriorityClass::Idle => "prio.idle",
            PriorityClass::BelowNormal => "prio.below",
            PriorityClass::Normal => "prio.normal",
            PriorityClass::AboveNormal => "prio.above",
            PriorityClass::High => "prio.high",
            PriorityClass::Realtime => "prio.realtime",
        }
    }
}

/// Приоритет ввода-вывода (0..=3 как в NT: VeryLow, Low, Normal, High).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoPriority {
    VeryLow,
    Low,
    Normal,
    High,
}

/// Приоритет памяти (1..=5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemPriority {
    VeryLow,
    Low,
    Medium,
    BelowNormal,
    Normal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// None — не менять маску процессора.
    pub affinity: Option<CpuSet>,
    /// None — не менять приоритет.
    pub priority: Option<PriorityClass>,
    #[serde(default)]
    pub io_priority: Option<IoPriority>,
    #[serde(default)]
    pub mem_priority: Option<MemPriority>,
}

impl Profile {
    pub fn new(id: String, name: impl Into<String>) -> Self {
        Profile { id, name: name.into(), affinity: None, priority: None, io_priority: None, mem_priority: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MatchMode {
    /// По имени exe (без учёта регистра).
    #[default]
    Name,
    /// По полному пути exe.
    Path,
}

/// Условие переключения профиля привязки: когда `watch` запускается — активен `on_start`,
/// когда все процессы `watch` закрыты — активен `on_exit`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    pub watch: String,
    #[serde(default)]
    pub watch_mode: MatchMode,
    #[serde(default)]
    pub on_start: Option<String>,
    #[serde(default)]
    pub on_exit: Option<String>,
}

/// Приложение и профили, назначенные ему.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppBinding {
    pub id: String,
    /// Имя exe или полный путь — зависит от `mode`.
    pub target: String,
    #[serde(default)]
    pub mode: MatchMode,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Профили, доступные для этого приложения.
    #[serde(default)]
    pub profile_ids: Vec<String>,
    /// Активный профиль (должен входить в `profile_ids`; иначе игнорируется).
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
}

fn yes() -> bool {
    true
}

impl AppBinding {
    pub fn display_name(&self) -> String {
        match self.mode {
            MatchMode::Name => self.target.clone(),
            MatchMode::Path => std::path::Path::new(&self.target)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.target.clone()),
        }
    }
}

/// Генерация уникального id без внешних крейтов.
pub fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{:x}-{:x}", t, N.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpuset_ops() {
        let s = CpuSet::from_indices([0, 1]);
        assert_eq!(s.inverted(4), CpuSet::from_indices([2, 3]));
        assert_eq!(CpuSet::from_indices([1, 9]).clamped(4), CpuSet::from_indices([1]));
        let mut t = s.clone();
        t.toggle(1);
        t.toggle(5);
        assert_eq!(t, CpuSet::from_indices([0, 5]));
    }
    #[test]
    fn priority_steps() {
        assert_eq!(PriorityClass::Normal.raise(), PriorityClass::AboveNormal);
        assert_eq!(PriorityClass::Realtime.raise(), PriorityClass::Realtime);
        assert_eq!(PriorityClass::Idle.lower(), PriorityClass::Idle);
        assert_eq!(PriorityClass::Normal.lower(), PriorityClass::BelowNormal);
    }
    #[test]
    fn ids_unique() {
        assert_ne!(new_id("p"), new_id("p"));
    }
    #[test]
    fn profile_roundtrip() {
        let mut p = Profile::new("a".into(), "Игра");
        p.affinity = Some(CpuSet::from_indices([0, 2]));
        p.priority = Some(PriorityClass::High);
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&j).unwrap(), p);
    }
}
