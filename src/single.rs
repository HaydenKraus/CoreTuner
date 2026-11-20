//! Защита от второго экземпляра: именованный mutex + сигнал «показать окно» второму запуску.
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenProcess, QueryFullProcessImageNameW, SetEvent,
    WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindow, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
};

const MUTEX_NAME: &str = r"Local\CoreTuner_SingleInstance";
const EVENT_NAME: &str = r"Local\CoreTuner_Show";
const WINDOW_TITLE: &str = "CoreTuner";

/// Владение единственным экземпляром; закрывает mutex при разрушении.
pub struct Guard {
    handle: HANDLE,
}

// Дескриптор ядра можно передавать между потоками.
unsafe impl Send for Guard {}
unsafe impl Sync for Guard {}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub enum Instance {
    First(Guard),
    Already,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Захватить единственный экземпляр. Первый получает `Guard`, последующие — `Already`.
pub fn acquire() -> Instance {
    let name = wide(MUTEX_NAME);
    match unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) } {
        Ok(handle) => {
            let already = std::io::Error::last_os_error().raw_os_error()
                == Some(windows::Win32::Foundation::ERROR_ALREADY_EXISTS.0 as i32);
            if already {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                Instance::Already
            } else {
                Instance::First(Guard { handle })
            }
        }
        Err(e) => {
            tracing::warn!("не удалось создать mutex единственного экземпляра: {e}");
            // Не блокируем запуск, если система не дала создать объект.
            Instance::First(Guard { handle: HANDLE::default() })
        }
    }
}

/// Как [`acquire`], но при занятом mutex ждёт до `timeout`: нужен после перезапуска
/// с правами администратора, когда прежний экземпляр ещё завершается.
pub fn acquire_wait(timeout: std::time::Duration) -> Instance {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match acquire() {
            Instance::Already if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

/// Сообщить уже запущенному экземпляру, что нужно показать окно.
/// Сначала именованное событие, при неудаче — поиск окна по заголовку.
pub fn signal_existing() {
    if signal_by_event() {
        return;
    }
    signal_by_window();
}

fn signal_by_event() -> bool {
    let name = wide(EVENT_NAME);
    let handle = match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) } {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!("событие {EVENT_NAME} не открылось: {e}");
            return false;
        }
    };
    let ok = unsafe { SetEvent(handle) }.is_ok();
    unsafe {
        let _ = CloseHandle(handle);
    }
    ok
}

/// HWND главного окна этого процесса (0 — ещё не создано). Заполняется при создании окна.
static OWN_HWND: AtomicIsize = AtomicIsize::new(0);

/// Запомнить дескриптор главного окна (берётся у eframe при создании окна).
pub fn set_own_window(hwnd: isize) {
    OWN_HWND.store(hwnd, Ordering::Relaxed);
}

/// Параметры перебора окон верхнего уровня.
struct WindowSearch {
    /// Имя exe процесса-владельца окна (окно другого экземпляра).
    exe_name: String,
    found: Option<HWND>,
}

/// Имя exe процесса (без пути) или `None`, если процесс недоступен.
fn process_exe_name(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess с валидным pid; дескриптор закрывает HandleGuard.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let guard = crate::cpu::HandleGuard::new(handle);
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    // SAFETY: буфер и его длина в символах корректны.
    unsafe { QueryFullProcessImageNameW(guard.raw(), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    let path = String::from_utf16_lossy(buf.get(..len as usize)?);
    path.rsplit('\\').next().map(str::to_string)
}

unsafe extern "system" fn enum_windows_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam — указатель на WindowSearch, живущий на стеке вызывающего EnumWindows.
    let search = unsafe { &mut *(lparam.0 as *mut WindowSearch) };
    let mut pid = 0u32;
    // SAFETY: валидный hwnd из EnumWindows и out-параметр.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32)) };
    // Свои окна пропускаем: GetWindowText для окна своего процесса шлёт ему WM_GETTEXT,
    // а перебор нужен только для окна ДРУГОГО экземпляра.
    if pid == std::process::id() {
        return BOOL(1);
    }
    let mut title = [0u16; 64];
    // SAFETY: буфер доступен на запись.
    let n = unsafe { GetWindowTextW(hwnd, &mut title) };
    if n <= 0 || String::from_utf16_lossy(title.get(..n as usize).unwrap_or(&[])) != WINDOW_TITLE {
        return BOOL(1);
    }
    if process_exe_name(pid).is_some_and(|n| n.eq_ignore_ascii_case(&search.exe_name)) {
        search.found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}

fn find_window(exe_name: String) -> Option<HWND> {
    let mut search = WindowSearch { exe_name, found: None };
    // SAFETY: колбэк вызывается синхронно, `search` живёт до возврата из EnumWindows.
    // Прерывание перебора колбэком возвращается как ошибка — это не сбой.
    let _ = unsafe { EnumWindows(Some(enum_windows_cb), LPARAM(&mut search as *mut WindowSearch as isize)) };
    search.found
}

/// Главное окно текущего процесса (сохранённый HWND), а не любое окно с таким названием.
pub fn find_own_window() -> Option<HWND> {
    let raw = OWN_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return None;
    }
    let hwnd = HWND(raw as *mut core::ffi::c_void);
    // SAFETY: IsWindow безопасно вызывать с любым значением дескриптора.
    unsafe { IsWindow(Some(hwnd)) }.as_bool().then_some(hwnd)
}

fn signal_by_window() -> bool {
    let Some(exe_name) = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())) else {
        return false;
    };
    let hwnd = match find_window(exe_name) {
        Some(h) => h,
        None => {
            tracing::debug!("окно \"{WINDOW_TITLE}\" не найдено");
            return false;
        }
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

/// Поток, ожидающий событие «показать окно». Вызывает `tx` при каждом сигнале.
pub fn spawn_show_listener(tx: impl Fn() + Send + 'static) {
    let name = wide(EVENT_NAME);
    let handle = match unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("не удалось создать событие {EVENT_NAME}: {e}");
            return;
        }
    };
    // Событие живёт столько же, сколько процесс: дескриптор не закрываем.
    let raw = handle.0 as usize;

    std::thread::Builder::new()
        .name("ct-show-listener".into())
        .spawn(move || {
            let handle = HANDLE(raw as *mut core::ffi::c_void);
            loop {
                let r = unsafe { WaitForSingleObject(handle, INFINITE) };
                if r == WAIT_OBJECT_0 {
                    tx();
                } else {
                    tracing::debug!("ожидание события завершилось с кодом {r:?}");
                    break;
                }
            }
        })
        .unwrap_or_else(|e| {
            tracing::warn!("не удалось запустить слушатель показа окна: {e}");
            std::thread::spawn(|| {})
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_null_terminated() {
        let w = wide("abc");
        assert_eq!(w, vec![97, 98, 99, 0]);
    }

    #[test]
    fn acquire_second_instance_is_already() {
        // Первый захват в этом процессе должен удаться, повторный — вернуть Already.
        let first = acquire();
        let is_first = matches!(first, Instance::First(_));
        assert!(is_first, "первый захват mutex не удался");
        assert!(matches!(acquire(), Instance::Already), "второй захват должен вернуть Already");
        drop(first);
        // После освобождения mutex снова свободен.
        assert!(matches!(acquire(), Instance::First(_)));
    }
}
