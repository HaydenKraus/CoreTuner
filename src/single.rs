//! Защита от второго экземпляра: именованный mutex + сигнал «показать окно» второму запуску.
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
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

fn signal_by_window() -> bool {
    let title = wide(WINDOW_TITLE);
    let hwnd = match unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
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
