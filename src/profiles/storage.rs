//! Чтение/запись config.json: каталог в %APPDATA%\CoreTuner, атомарная запись, миграции,
//! импорт/экспорт профилей.
use crate::config::{Config, CONFIG_VERSION};
use crate::model::{new_id, AppBinding, Profile};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Каталог конфига: %APPDATA%\CoreTuner (создаётся при необходимости).
/// Переменная окружения `CORETUNER_CONFIG_DIR` задаёт другой каталог (портативный режим, отладка).
pub fn config_dir() -> PathBuf {
    let dir = match std::env::var_os("CORETUNER_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("CoreTuner"),
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> Config {
    load_from(&config_path())
}

/// Чтение конфига с миграцией и валидацией; повреждённый файл уезжает в `config.json.corrupt-<ts>`.
pub fn load_from(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            tracing::warn!("не удалось прочитать {}: {e}", path.display());
            return Config::default();
        }
    };
    match parse_config(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!("конфиг повреждён ({e}); сохраняю копию и беру настройки по умолчанию");
            match quarantine(path) {
                Ok(dst) => tracing::warn!("копия повреждённого конфига: {}", dst.display()),
                Err(qe) => tracing::warn!("не удалось сохранить копию повреждённого конфига: {qe}"),
            }
            Config::default()
        }
    }
}

fn parse_config(text: &str) -> Result<Config> {
    // Блокнот и PowerShell 5 добавляют BOM — терпим его.
    let text = text.trim_start_matches('\u{feff}');
    let value: Value = serde_json::from_str(text).context("некорректный JSON")?;
    let cfg = migrate(value, CONFIG_VERSION)?;
    let cfg = validate(cfg);
    Ok(cfg)
}

fn quarantine(path: &Path) -> Result<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut dst = path.to_path_buf();
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "config.json".into());
    dst.set_file_name(format!("{name}.corrupt-{ts}"));
    std::fs::copy(path, &dst).with_context(|| format!("копирование в {}", dst.display()))?;
    Ok(dst)
}

/// v0 (без поля `version`) → v1. Неизвестные будущие версии принимаем как есть.
pub fn migrate(mut value: Value, target: u32) -> Result<Config> {
    let obj = value.as_object_mut().ok_or_else(|| anyhow!("корень конфига — не объект"))?;
    let from = match obj.get("version").and_then(Value::as_u64) {
        Some(v) if v > u64::from(target) => {
            tracing::warn!("конфиг версии {v} новее ожидаемой {target}; читаю как есть");
            v as u32
        }
        Some(v) => v as u32,
        None => 0,
    };
    if from == 0 {
        obj.insert("version".into(), Value::from(target));
    }
    // Общие миграции отсутствуют — v0 совпадает с v1 по структуре.
    serde_json::from_value(value).context("структура конфига не соответствует модели")
}

/// Обрезка пустых масок, несуществующих профилей в привязках, дубликатов id.
pub fn validate(mut cfg: Config) -> Config {
    let mut seen_profile: HashSet<String> = HashSet::new();
    cfg.profiles.retain(|p| {
        if p.id.is_empty() {
            return false;
        }
        if let Some(set) = &p.affinity {
            if set.is_empty() {
                return false;
            }
        }
        seen_profile.insert(p.id.clone())
    });
    let mut seen_binding: HashSet<String> = HashSet::new();
    cfg.bindings.retain(|b| !b.id.is_empty() && !b.target.trim().is_empty() && seen_binding.insert(b.id.clone()));
    for b in &mut cfg.bindings {
        b.profile_ids.retain(|id| seen_profile.contains(id));
        let mut uniq: HashSet<String> = HashSet::new();
        b.profile_ids.retain(|id| uniq.insert(id.clone()));
        if !b.active_profile.as_ref().is_some_and(|id| b.profile_ids.contains(id)) {
            b.active_profile = None;
        }
        for t in &mut b.triggers {
            if !t.on_start.as_ref().is_some_and(|id| seen_profile.contains(id)) {
                t.on_start = None;
            }
            if !t.on_exit.as_ref().is_some_and(|id| seen_profile.contains(id)) {
                t.on_exit = None;
            }
        }
    }
    // Профиль из `affinity` с пустым набором превращаем в «не менять» — уже отсеян выше.
    cfg.settings.clamp();
    cfg.version = CONFIG_VERSION;
    cfg
}

pub fn save(cfg: &Config) -> Result<()> {
    save_to(&config_path(), cfg)
}

/// Атомарная запись: tmp-файл рядом с целью + fsync + переименование поверх.
pub fn save_to(path: &Path, cfg: &Config) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("каталог {}", dir.display()))?;
    }
    let tmp = tmp_sibling(path);
    let data = serde_json::to_vec_pretty(cfg).context("сериализация конфига")?;
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("создание {}", tmp.display()))?;
        std::io::Write::write_all(&mut f, &data)?;
        f.sync_all()?;
    }
    if let Err(e) = rename_over(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("переименование во {}", path.display()));
    }
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "config.json".into());
    let pid = std::process::id();
    path.with_file_name(format!(".{name}.{pid}.tmp"))
}

/// std::fs::rename на Windows заменяет существующий файл (MoveFileExW + MOVEFILE_REPLACE_EXISTING).
#[cfg(windows)]
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }
    let f = wide(from);
    let t = wide(to);
    unsafe {
        MoveFileExW(PCWSTR(f.as_ptr()), PCWSTR(t.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

#[cfg(not(windows))]
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Экспорт профилей и привязок в отдельный JSON.
pub fn export_profiles(cfg: &Config, path: &Path) -> Result<()> {
    #[derive(serde::Serialize)]
    struct Bundle<'a> {
        version: u32,
        profiles: &'a [Profile],
        bindings: &'a [AppBinding],
    }
    let bundle = Bundle { version: CONFIG_VERSION, profiles: &cfg.profiles, bindings: &cfg.bindings };
    let data = serde_json::to_vec_pretty(&bundle).context("сериализация экспорта")?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).ok();
        }
    }
    std::fs::write(path, data).with_context(|| format!("запись {}", path.display()))?;
    Ok(())
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Bundle {
    profiles: Vec<Profile>,
    bindings: Vec<AppBinding>,
}

/// Импорт: слияние по id, при коллизии — новый id; ссылки профилей в привязках ремапятся.
/// Возвращает число импортированных профилей.
pub fn import_profiles(cfg: &mut Config, path: &Path) -> Result<usize> {
    let text = std::fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let mut bundle: Bundle = serde_json::from_str(&text).context("некорректный файл импорта")?;
    let incoming = validate(Config {
        version: CONFIG_VERSION,
        settings: Default::default(),
        profiles: std::mem::take(&mut bundle.profiles),
        bindings: std::mem::take(&mut bundle.bindings),
    });

    let mut map: Vec<(String, String)> = Vec::new();
    for mut p in incoming.profiles {
        let old = p.id.clone();
        if cfg.profiles.iter().any(|e| e.id == p.id) {
            p.id = new_id("p");
        }
        let new = p.id.clone();
        cfg.profiles.push(p);
        map.push((old, new));
    }
    let remap = |id: &str| -> Option<String> {
        map.iter().find(|(o, _)| o == id).map(|(_, n)| n.clone()).or_else(|| {
            cfg.profiles.iter().any(|p| p.id == id).then(|| id.to_string())
        })
    };

    let mut added_bindings = 0usize;
    for mut b in incoming.bindings {
        b.profile_ids = b.profile_ids.iter().filter_map(|id| remap(id)).collect();
        let mut uniq: HashSet<String> = HashSet::new();
        b.profile_ids.retain(|id| uniq.insert(id.clone()));
        b.active_profile = b.active_profile.as_deref().and_then(remap).filter(|id| b.profile_ids.contains(id));
        for t in &mut b.triggers {
            t.on_start = t.on_start.as_deref().and_then(remap);
            t.on_exit = t.on_exit.as_deref().and_then(remap);
        }
        if b.profile_ids.is_empty() && b.triggers.iter().all(|t| t.on_start.is_none() && t.on_exit.is_none()) {
            continue;
        }
        if cfg.bindings.iter().any(|e| e.id == b.id) {
            b.id = new_id("b");
        }
        cfg.bindings.push(b);
        added_bindings += 1;
    }
    let _ = added_bindings;
    *cfg = validate(std::mem::take(cfg));
    Ok(map.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CpuSet, MatchMode, PriorityClass, Trigger};

    /// Каталог для временных файлов тестов. Переменная `CT_TEST_TMP` задаёт корень
    /// (нужна для песочниц, где системный temp недоступен на запись); иначе — temp.
    fn tmp_root() -> PathBuf {
        let base = std::env::var_os("CT_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let d = base.join("ct_storage_tests");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = tmp_root().join(format!("{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Config {
        let mut p = Profile::new("p1".into(), "Игра");
        p.affinity = Some(CpuSet::from_indices([0, 1, 2]));
        p.priority = Some(PriorityClass::High);
        p.io_priority = Some(crate::model::IoPriority::Low);
        p.mem_priority = Some(crate::model::MemPriority::Normal);
        let b = AppBinding {
            id: "b1".into(),
            target: "game.exe".into(),
            mode: MatchMode::Name,
            enabled: true,
            profile_ids: vec!["p1".into()],
            active_profile: Some("p1".into()),
            triggers: vec![Trigger {
                watch: "launcher.exe".into(),
                watch_mode: MatchMode::Name,
                on_start: Some("p1".into()),
                on_exit: None,
            }],
        };
        let mut c = Config::default();
        c.profiles.push(p);
        c.bindings.push(b);
        c
    }

    #[test]
    fn roundtrip() {
        let dir = tmpdir("rt");
        let file = dir.join("config.json");
        let cfg = sample();
        save_to(&file, &cfg).unwrap();
        assert_eq!(load_from(&file), cfg);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp-файл остался: {leftovers:?}");
    }

    #[test]
    fn atomic_write_replaces_previous() {
        let dir = tmpdir("atomic");
        let file = dir.join("config.json");
        let mut cfg = sample();
        save_to(&file, &cfg).unwrap();
        cfg.profiles[0].name = "Новое имя".into();
        save_to(&file, &cfg).unwrap();
        let back = load_from(&file);
        assert_eq!(back.profiles[0].name, "Новое имя");
        assert_eq!(back.profiles.len(), 1);
    }

    #[test]
    fn missing_file_gives_default() {
        let dir = tmpdir("missing");
        let cfg = load_from(&dir.join("nope.json"));
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn migration_v0_to_v1() {
        // v0 — JSON без поля version.
        let text = r#"{
            "settings": {"poll_interval_ms": 2000, "lang": "En"},
            "profiles": [{"id":"p1","name":"P","affinity":[1,2],"priority":"Normal"}],
            "bindings": [{"id":"b1","target":"a.exe","mode":"Name","enabled":true,
                          "profile_ids":["p1"],"active_profile":"p1","triggers":[]}]
        }"#;
        assert!(!text.contains("\"version\""));
        let cfg = parse_config(text).unwrap();
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.settings.poll_interval_ms, 2000);
        assert_eq!(cfg.settings.lang, crate::config::Lang::En);
        assert_eq!(cfg.profiles.len(), 1);
        assert_eq!(cfg.profiles[0].affinity, Some(CpuSet::from_indices([1, 2])));
        assert_eq!(cfg.bindings[0].active_profile.as_deref(), Some("p1"));
    }

    #[test]
    fn corrupt_file_quarantined_and_defaults() {
        let dir = tmpdir("corrupt");
        let file = dir.join("config.json");
        std::fs::write(&file, b"{ this is not json").unwrap();
        let cfg = load_from(&file);
        assert_eq!(cfg, Config::default());
        let copies: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".corrupt-"))
            .collect();
        assert_eq!(copies.len(), 1, "ожидалась одна копия: {copies:?}");
        // исходный файл не тронут
        assert!(std::fs::metadata(&file).unwrap().len() > 0);
    }

    #[test]
    fn wrong_shape_quarantined() {
        let dir = tmpdir("shape");
        let file = dir.join("config.json");
        std::fs::write(&file, "[1,2,3]").unwrap();
        assert_eq!(load_from(&file), Config::default());
        let n = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".corrupt-")).count();
        assert_eq!(n, 1);
    }

    #[test]
    fn validate_drops_empty_affinity_and_dangling_refs() {
        let mut cfg = sample();
        cfg.profiles[0].affinity = Some(CpuSet::new());
        cfg.bindings[0].profile_ids.push("ghost".into());
        cfg.bindings[0].triggers[0].on_start = Some("ghost".into());
        let v = validate(cfg);
        assert_eq!(v.profiles.len(), 0, "профиль с пустой маской должен быть удалён");
        assert!(v.bindings[0].profile_ids.is_empty());
        assert_eq!(v.bindings[0].active_profile, None);
        assert_eq!(v.bindings[0].triggers[0].on_start, None);
    }

    #[test]
    fn validate_dedups_ids() {
        let mut cfg = sample();
        cfg.profiles.push(cfg.profiles[0].clone());
        cfg.bindings[0].profile_ids.push("p1".into());
        let v = validate(cfg);
        assert_eq!(v.profiles.len(), 1);
        assert_eq!(v.bindings[0].profile_ids.len(), 1);
    }

    #[test]
    fn export_import_merges_and_remaps_on_collision() {
        let dir = tmpdir("expimp");
        let file = dir.join("export.json");
        let src = sample();
        export_profiles(&src, &file).unwrap();

        // Целевой конфиг: другой профиль с тем же id, чтобы сработал ремап.
        let mut dst = Config::default();
        dst.profiles.push(Profile::new("p1".into(), "Существующий"));
        let n = import_profiles(&mut dst, &file).unwrap();
        assert_eq!(n, 1);
        assert_eq!(dst.profiles.len(), 2);
        let imported = &dst.profiles[1];
        assert_ne!(imported.id, "p1");
        assert_eq!(imported.name, "Игра");
        let b = dst.bindings.iter().find(|b| b.target == "game.exe").unwrap();
        assert_eq!(b.profile_ids, vec![imported.id.clone()]);
        assert_eq!(b.active_profile.as_deref(), Some(imported.id.as_str()));
        assert_eq!(b.triggers[0].on_start.as_deref(), Some(imported.id.as_str()));
    }

    #[test]
    fn import_keeps_existing_ids_when_free() {
        let dir = tmpdir("imp2");
        let file = dir.join("export.json");
        export_profiles(&sample(), &file).unwrap();
        let mut dst = Config::default();
        let n = import_profiles(&mut dst, &file).unwrap();
        assert_eq!(n, 1);
        assert_eq!(dst.profiles[0].id, "p1");
        assert_eq!(dst.bindings[0].id, "b1");
    }

    #[test]
    fn import_bad_file_errors() {
        let dir = tmpdir("impbad");
        let file = dir.join("bad.json");
        std::fs::write(&file, "not json at all").unwrap();
        let mut cfg = Config::default();
        assert!(import_profiles(&mut cfg, &file).is_err());
    }
}

