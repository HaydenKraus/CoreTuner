//! Системный слой: топология CPU, affinity, приоритеты и утилиты WinAPI.
//!
//! Все функции модуля безопасны для вызова: ошибки WinAPI превращаются в [`SysError`],
//! паники в рабочем коде отсутствуют.
use std::fmt;

pub mod affinity;
pub mod priority;
pub mod topology;

/// Ошибка системного вызова WinAPI, приведённая к прикладным категориям.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum SysError {
    #[error("нет доступа")]
    AccessDenied,
    #[error("процесс не найден")]
    NotFound,
    #[error("некорректные данные")]
    Invalid,
    #[error("{0}")]
    Other(String),
}

impl SysError {
    /// Преобразовать `GetLastError()` в [`SysError`].
    pub fn from_last_error(code: u32) -> Self {
        use windows::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND,
            ERROR_PARTIAL_COPY,
        };
        match code {
            c if c == ERROR_ACCESS_DENIED.0 || c == ERROR_PARTIAL_COPY.0 => SysError::AccessDenied,
            c if c == ERROR_INVALID_HANDLE.0 || c == ERROR_NOT_FOUND.0 => SysError::NotFound,
            c if c == ERROR_INVALID_PARAMETER.0 => SysError::Invalid,
            other => SysError::Other(format!("WinAPI error {other}")),
        }
    }

    /// Преобразовать ошибку `windows`-крейта в [`SysError`].
    pub fn from_win_error(e: &windows::core::Error) -> Self {
        Self::from_last_error(win32_code(e))
    }

    /// Преобразовать NTSTATUS из ntdll в [`SysError`].
    ///
    /// Коды статусов NT: 0xC0000022 — отказ доступа, 0xC000000D — неверный параметр,
    /// 0xC0000034/0xC000010A — объект не найден.
    pub fn from_ntstatus(status: i32) -> Self {
        match status as u32 {
            0xC000_0022 => SysError::AccessDenied, // STATUS_ACCESS_DENIED
            0xC000_000D => SysError::Invalid,      // STATUS_INVALID_PARAMETER
            0xC000_0034 => SysError::NotFound,     // STATUS_OBJECT_NAME_NOT_FOUND
            0xC000_010A => SysError::NotFound,     // STATUS_PROCESS_IS_TERMINATING
            0xC000_0005 => SysError::NotFound,     // STATUS_INVALID_HANDLE
            other => SysError::Other(format!("NTSTATUS 0x{other:08X}")),
        }
    }
}

/// Win32-код из HRESULT вида `0x8007xxxx` (FACILITY_WIN32); прочие значения возвращаются как есть.
pub(crate) fn win32_from_hresult(hr: i32) -> u32 {
    let code = hr as u32;
    if code >> 16 == 0x8007 {
        code & 0xFFFF
    } else {
        code
    }
}

/// Win32-код ошибки `windows`-крейта (его `code()` — HRESULT, а не голый Win32-код).
pub(crate) fn win32_code(e: &windows::core::Error) -> u32 {
    win32_from_hresult(e.code().0)
}

impl From<windows::core::Error> for SysError {
    fn from(e: windows::core::Error) -> Self {
        Self::from_win_error(&e)
    }
}

/// Имена процессов, менять настройки которых нельзя или опасно
/// (критические системные процессы, а также сам CoreTuner).
const PROTECTED_NAMES: &[&str] = &[
    "system",
    "system idle process",
    "registry",
    "memory compression",
    "memcompression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "winlogon.exe",
    "dwm.exe",
    "fontdrvhost.exe",
    "sihost.exe",
    "ctfmon.exe",
    "audiodg.exe",
    "wudfhost.exe",
    "searchindexer.exe",
    "securityhealthservice.exe",
    "msmpeng.exe",
    "nissrv.exe",
    "coretuner.exe",
];

/// Является ли процесс системным/защищённым (сравнение без учёта регистра).
pub fn is_protected_name(exe_name: &str) -> bool {
    let name = exe_name.trim();
    if name.is_empty() {
        return true;
    }
    // Сравниваем и полное имя, и имя без расширения — «System» приходит без «.exe».
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    PROTECTED_NAMES
        .iter()
        .any(|p| *p == lower || p.strip_suffix(".exe") == Some(stem) && p.ends_with(".exe"))
}

/// Запущен ли процесс с повышенными правами (токен текущего процесса, TokenElevation).
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();
    // SAFETY: OpenProcessToken получает псевдо-хэндл текущего процесса и валидный out-параметр.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
    }
    let _guard = HandleGuard::new(token);

    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    // SAFETY: буфер и его размер соответствуют запрашиваемому классу информации.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            size,
            &mut returned,
        )
    };
    ok.is_ok() && elevation.TokenIsElevated != 0
}

/// Перезапустить приложение с запросом повышения прав (UAC).
///
/// При успехе вызывающий код обязан завершить текущий процесс: новый экземпляр
/// уже получит одиночный mutex только после этого.
pub fn restart_as_admin() -> anyhow::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("не удалось определить путь к exe: {e}"))?;
    let exe_w = path_to_wide(&exe);
    let verb = to_wide("runas");
    // Флаг сообщает новому экземпляру, что старый завершается: нужно подождать его mutex.
    let args = to_wide("--restarted");

    // SAFETY: передаются корректные NUL-терминированные строки; рабочий каталог не задан.
    let ret = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(exe_w.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW возвращает HINSTANCE-подобный код: <= 32 означает ошибку.
    let code = ret.0 as usize;
    if code <= 32 {
        return Err(anyhow::anyhow!(
            "не удалось перезапустить с правами администратора (код {code})"
        ));
    }
    Ok(())
}

/// RAII-обёртка над `HANDLE`: закрывает дескриптор при выходе из области видимости.
pub(crate) struct HandleGuard(windows::Win32::Foundation::HANDLE);

impl HandleGuard {
    /// Принять владение дескриптором (уже проверенным на валидность).
    pub(crate) fn new(handle: windows::Win32::Foundation::HANDLE) -> Self {
        Self(handle)
    }

    pub(crate) fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        self.0
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        if !self.0.is_invalid() {
            // SAFETY: дескриптор получен от WinAPI и ещё не закрыт.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

impl fmt::Debug for HandleGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HandleGuard").field(&self.0 .0).finish()
    }
}

/// Строка Rust в NUL-терминированный UTF-16 буфер.
pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Путь в NUL-терминированный UTF-16 буфер.
pub(crate) fn path_to_wide(p: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_critical_processes() {
        assert!(is_protected_name("System"));
        assert!(is_protected_name("smss.exe"));
        assert!(is_protected_name("CSRSS.EXE"));
        assert!(is_protected_name("lsass.exe"));
        assert!(is_protected_name("winlogon.exe"));
        assert!(is_protected_name("services.exe"));
        assert!(is_protected_name("dwm.exe"));
        assert!(is_protected_name("CoreTuner.exe"));
        assert!(is_protected_name("  csrss.exe  "));
        assert!(is_protected_name(""));
    }

    #[test]
    fn normal_processes_allowed() {
        assert!(!is_protected_name("notepad.exe"));
        assert!(!is_protected_name("chrome.exe"));
        assert!(!is_protected_name("explorer.exe"));
        assert!(!is_protected_name("game.exe"));
    }

    #[test]
    fn error_mapping() {
        use windows::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND,
        };
        assert_eq!(SysError::from_last_error(ERROR_ACCESS_DENIED.0), SysError::AccessDenied);
        assert_eq!(SysError::from_last_error(ERROR_INVALID_HANDLE.0), SysError::NotFound);
        assert_eq!(SysError::from_last_error(ERROR_NOT_FOUND.0), SysError::NotFound);
        assert_eq!(SysError::from_last_error(ERROR_INVALID_PARAMETER.0), SysError::Invalid);
        assert!(matches!(SysError::from_last_error(12345), SysError::Other(_)));
    }

    #[test]
    fn hresult_is_unwrapped_to_win32() {
        // HRESULT_FROM_WIN32(5) = 0x80070005, HRESULT_FROM_WIN32(122) = 0x8007007A.
        assert_eq!(win32_from_hresult(0x8007_0005_u32 as i32), 5);
        assert_eq!(win32_from_hresult(0x8007_007A_u32 as i32), 122);
        // Не-Win32 HRESULT и голые коды не меняются.
        assert_eq!(win32_from_hresult(0x8000_4005_u32 as i32), 0x8000_4005);
        assert_eq!(win32_from_hresult(5), 5);
        let hr_access = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005_u32 as i32));
        assert_eq!(SysError::from_win_error(&hr_access), SysError::AccessDenied);
    }

    #[test]
    fn wide_is_nul_terminated() {
        let w = to_wide("ab");
        assert_eq!(w, vec![0x61, 0x62, 0x00]);
    }
}
