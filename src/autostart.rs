//! Автозапуск через ключ HKCU\Software\Microsoft\Windows\CurrentVersion\Run.
use anyhow::{anyhow, Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use winreg::enums::{KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "CoreTuner";

/// Командная строка для значения в реестре: `"<exe>" --minimized`.
/// Пустая команда означает «автозапуск выключен».
fn command_line(exe: &Path, minimized: bool) -> Result<String> {
    let exe = exe.to_str().ok_or_else(|| anyhow!("путь к exe не является Unicode: {}", exe.display()))?;
    if exe.contains('"') {
        return Err(anyhow!("путь к exe содержит кавычку, автозапуск невозможен"));
    }
    Ok(if minimized { format!("\"{exe}\" --minimized") } else { format!("\"{exe}\"") })
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("не удалось определить путь к exe")
}

/// Разбор значения реестра обратно в (путь, minimized). Нужен для тестов и диагностики.
pub fn parse_command(cmd: &str) -> Option<(String, bool)> {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        let end = rest.find('"')?;
        let exe = &rest[..end];
        let args = &rest[end + 1..];
        return Some((exe.to_string(), args.split_whitespace().any(|a| a == "--minimized")));
    }
    let mut it = cmd.split_whitespace();
    let exe = it.next()?.to_string();
    Some((exe, it.any(|a| a == "--minimized")))
}

/// Автозапуск включён и указывает на текущий exe.
pub fn is_enabled() -> bool {
    let Ok(exe) = current_exe() else { return false };
    let Some(parsed) = read_value().as_deref().and_then(parse_command) else { return false };
    paths_equal(Path::new(&parsed.0), &exe)
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        p.as_os_str().to_string_lossy().replace('/', "\\").to_lowercase()
    };
    norm(a) == norm(b)
}

fn read_value() -> Option<String> {
    let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let key = hkcu.open_subkey_with_flags(RUN_KEY, KEY_READ).ok()?;
    key.get_value::<String, _>(VALUE_NAME).ok()
}

pub fn set_enabled(on: bool, minimized: bool) -> Result<()> {
    let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        .context("не удалось открыть ключ Run в реестре")?;
    if on {
        let cmd = command_line(&current_exe()?, minimized)?;
        key.set_value(VALUE_NAME, &cmd)?;
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("не удалось удалить значение автозапуска"),
        }
    }
    Ok(())
}

/// Проверка значения реестра как OsStr — используется только для отладки.
#[allow(dead_code)]
fn value_is_ours(name: &OsStr) -> bool {
    name.to_string_lossy() == VALUE_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_path_with_spaces() {
        let c = command_line(Path::new(r"C:\Program Files\CoreTuner\coretuner.exe"), true).unwrap();
        assert_eq!(c, "\"C:\\Program Files\\CoreTuner\\coretuner.exe\" --minimized");
    }

    #[test]
    fn command_line_without_minimized() {
        let c = command_line(Path::new(r"C:\ct\coretuner.exe"), false).unwrap();
        assert_eq!(c, "\"C:\\ct\\coretuner.exe\"");
        assert!(!c.contains("--minimized"));
    }

    #[test]
    fn command_line_roundtrip() {
        let p = Path::new(r"C:\Program Files\CoreTuner\coretuner.exe");
        let (exe, min) = parse_command(&command_line(p, true).unwrap()).unwrap();
        assert_eq!(exe, p.to_str().unwrap());
        assert!(min);
    }

    #[test]
    fn parse_unquoted() {
        assert_eq!(parse_command("coretuner.exe --minimized").unwrap(), ("coretuner.exe".to_string(), true));
        assert_eq!(parse_command("coretuner.exe").unwrap(), ("coretuner.exe".to_string(), false));
        assert!(parse_command("   ").is_none());
    }
}
