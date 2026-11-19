//! Приоритеты процесса: класс приоритета, приоритет ввода-вывода и памяти.
use crate::cpu::{HandleGuard, SysError};
use crate::model::{IoPriority, MemPriority, PriorityClass};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    GetPriorityClass, OpenProcess, SetPriorityClass, SetProcessInformation, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, MEMORY_PRIORITY_INFORMATION,
    NORMAL_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    REALTIME_PRIORITY_CLASS,
};

/// Информационный класс `ProcessIoPriority` (ntdll, PROCESSINFOCLASS = 33).
const PROCESS_IO_PRIORITY_CLASS: u32 = 33;

/// Открыть процесс с нужными правами.
fn open(pid: u32, access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS) -> Result<HandleGuard, SysError> {
    // SAFETY: вызов без указателей; владение дескриптором забирает HandleGuard.
    unsafe { OpenProcess(access, false, pid) }
        .map(HandleGuard::new)
        .map_err(|e| SysError::from_win_error(&e))
}

/// Соответствие `PriorityClass` константе WinAPI.
fn class_flag(p: PriorityClass) -> windows::Win32::System::Threading::PROCESS_CREATION_FLAGS {
    match p {
        PriorityClass::Idle => IDLE_PRIORITY_CLASS,
        PriorityClass::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        PriorityClass::Normal => NORMAL_PRIORITY_CLASS,
        PriorityClass::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        PriorityClass::High => HIGH_PRIORITY_CLASS,
        PriorityClass::Realtime => REALTIME_PRIORITY_CLASS,
    }
}

/// Обратное преобразование флага WinAPI в `PriorityClass`.
fn class_from_flag(flag: u32) -> PriorityClass {
    match flag {
        f if f == IDLE_PRIORITY_CLASS.0 => PriorityClass::Idle,
        f if f == BELOW_NORMAL_PRIORITY_CLASS.0 => PriorityClass::BelowNormal,
        f if f == ABOVE_NORMAL_PRIORITY_CLASS.0 => PriorityClass::AboveNormal,
        f if f == HIGH_PRIORITY_CLASS.0 => PriorityClass::High,
        f if f == REALTIME_PRIORITY_CLASS.0 => PriorityClass::Realtime,
        // NORMAL_PRIORITY_CLASS, а также «idle с флагом» и прочие варианты.
        _ => PriorityClass::Normal,
    }
}

/// Прочитать класс приоритета процесса.
pub fn get_priority(pid: u32) -> Result<PriorityClass, SysError> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    // SAFETY: дескриптор имеет PROCESS_QUERY_LIMITED_INFORMATION.
    let flag = unsafe { GetPriorityClass(handle.raw()) };
    if flag == 0 {
        // SAFETY: GetLastError безопасен.
        return Err(SysError::from_last_error(unsafe { windows::Win32::Foundation::GetLastError().0 }));
    }
    Ok(class_from_flag(flag))
}

/// Установить класс приоритета процесса.
pub fn set_priority(pid: u32, priority: PriorityClass) -> Result<(), SysError> {
    let handle = open(pid, PROCESS_SET_INFORMATION)?;
    // SAFETY: дескриптор имеет PROCESS_SET_INFORMATION.
    unsafe { SetPriorityClass(handle.raw(), class_flag(priority)) }
        .map_err(|e| SysError::from_win_error(&e))
}

/// Значение приоритета ввода-вывода для NT (0..=3).
fn io_value(p: IoPriority) -> u32 {
    match p {
        IoPriority::VeryLow => 0,
        IoPriority::Low => 1,
        IoPriority::Normal => 2,
        IoPriority::High => 3,
    }
}

fn io_from_value(v: u32) -> IoPriority {
    match v {
        0 => IoPriority::VeryLow,
        1 => IoPriority::Low,
        3 => IoPriority::High,
        _ => IoPriority::Normal,
    }
}

/// Указатель на `NtQueryInformationProcess` из ntdll.
type NtQueryInformationProcessFn =
    unsafe extern "system" fn(HANDLE, u32, *mut core::ffi::c_void, u32, *mut u32) -> i32;

/// Указатель на `NtSetInformationProcess` из ntdll.
type NtSetInformationProcessFn =
    unsafe extern "system" fn(HANDLE, u32, *const core::ffi::c_void, u32) -> i32;

/// Загрузить функцию ntdll по имени.
///
/// Безопасность: возвращаемый указатель валиден, пока загружена ntdll (она загружена всегда).
unsafe fn ntdll_proc<T>(name: &[u8]) -> Result<T, SysError> {
    use windows::core::PCSTR;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::Foundation::HMODULE;

    // SAFETY: имя модуля — валидная NUL-строка "ntdll.dll".
    let module: HMODULE = unsafe { GetModuleHandleW(windows::core::w!("ntdll.dll")) }
        .map_err(|e| SysError::from_win_error(&e))?;
    // SAFETY: имя функции — валидная NUL-строка; результат проверяется на null.
    let addr = unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) };
    match addr {
        Some(f) if (f as usize) != 0 => {
            // SAFETY: сигнатура соответствует документированным экспортам ntdll.
            Ok(unsafe { std::mem::transmute_copy::<_, T>(&f) })
        }
        _ => Err(SysError::Other("функция ntdll недоступна".into())),
    }
}

/// Прочитать приоритет ввода-вывода процесса (`NtQueryInformationProcess`, ProcessIoPriority).
pub fn get_io_priority(pid: u32) -> Result<IoPriority, SysError> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    // SAFETY: загрузка экспорта ntdll по известному имени.
    let func: NtQueryInformationProcessFn = unsafe { ntdll_proc(b"NtQueryInformationProcess\0")? };

    let mut value: u32 = 0;
    let mut returned: u32 = 0;
    // SAFETY: буфер и его размер соответствуют u32; дескриптор валиден.
    let status = unsafe {
        func(
            handle.raw(),
            PROCESS_IO_PRIORITY_CLASS,
            &mut value as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    };
    if status < 0 {
        return Err(SysError::from_ntstatus(status));
    }
    Ok(io_from_value(value))
}

/// Установить приоритет ввода-вывода процесса.
///
/// Значение `High` (3) обычно требует повышенных прав.
pub fn set_io_priority(pid: u32, priority: IoPriority) -> Result<(), SysError> {
    let handle = open(pid, PROCESS_SET_INFORMATION)?;
    // SAFETY: загрузка экспорта ntdll по известному имени.
    let func: NtSetInformationProcessFn = unsafe { ntdll_proc(b"NtSetInformationProcess\0")? };

    let value = io_value(priority);
    // SAFETY: передаётся адрес локальной переменной нужного размера.
    let status = unsafe {
        func(
            handle.raw(),
            PROCESS_IO_PRIORITY_CLASS,
            &value as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if status < 0 {
        return Err(SysError::from_ntstatus(status));
    }
    Ok(())
}

/// Значение `MEMORY_PRIORITY` для WinAPI (1..=5).
fn mem_value(p: MemPriority) -> windows::Win32::System::Threading::MEMORY_PRIORITY {
    use windows::Win32::System::Threading::{
        MEMORY_PRIORITY_BELOW_NORMAL, MEMORY_PRIORITY_LOW, MEMORY_PRIORITY_MEDIUM, MEMORY_PRIORITY_NORMAL,
        MEMORY_PRIORITY_VERY_LOW,
    };
    match p {
        MemPriority::VeryLow => MEMORY_PRIORITY_VERY_LOW,
        MemPriority::Low => MEMORY_PRIORITY_LOW,
        MemPriority::Medium => MEMORY_PRIORITY_MEDIUM,
        MemPriority::BelowNormal => MEMORY_PRIORITY_BELOW_NORMAL,
        MemPriority::Normal => MEMORY_PRIORITY_NORMAL,
    }
}

/// Установить приоритет памяти процесса (`SetProcessInformation`, `ProcessMemoryPriority`).
pub fn set_memory_priority(pid: u32, priority: MemPriority) -> Result<(), SysError> {
    use windows::Win32::System::Threading::ProcessMemoryPriority;

    let handle = open(pid, PROCESS_SET_INFORMATION)?;
    let info = MEMORY_PRIORITY_INFORMATION { MemoryPriority: mem_value(priority) };
    // SAFETY: структура и её размер соответствуют классу ProcessMemoryPriority.
    unsafe {
        SetProcessInformation(
            handle.raw(),
            ProcessMemoryPriority,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    }
    .map_err(|e| SysError::from_win_error(&e))
}

/// Прочитать приоритет памяти процесса (если API доступен).
pub fn get_memory_priority(pid: u32) -> Result<MemPriority, SysError> {
    use windows::Win32::System::Threading::{GetProcessInformation, ProcessMemoryPriority};

    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut info = MEMORY_PRIORITY_INFORMATION::default();
    // SAFETY: буфер и его размер соответствуют классу ProcessMemoryPriority.
    unsafe {
        GetProcessInformation(
            handle.raw(),
            ProcessMemoryPriority,
            &mut info as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    }
    .map_err(|e| SysError::from_win_error(&e))?;
    Ok(mem_from_value(info.MemoryPriority))
}

/// Обратное преобразование `MEMORY_PRIORITY` в `MemPriority`.
fn mem_from_value(v: windows::Win32::System::Threading::MEMORY_PRIORITY) -> MemPriority {
    use windows::Win32::System::Threading::{
        MEMORY_PRIORITY_BELOW_NORMAL, MEMORY_PRIORITY_LOW, MEMORY_PRIORITY_MEDIUM, MEMORY_PRIORITY_VERY_LOW,
    };
    match v.0 {
        x if x == MEMORY_PRIORITY_VERY_LOW.0 => MemPriority::VeryLow,
        x if x == MEMORY_PRIORITY_LOW.0 => MemPriority::Low,
        x if x == MEMORY_PRIORITY_MEDIUM.0 => MemPriority::Medium,
        x if x == MEMORY_PRIORITY_BELOW_NORMAL.0 => MemPriority::BelowNormal,
        _ => MemPriority::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_flag_roundtrip() {
        for p in PriorityClass::ALL {
            assert_eq!(class_from_flag(class_flag(p).0), p, "класс {p:?} должен сохраняться");
        }
    }

    #[test]
    fn io_value_roundtrip() {
        for p in [IoPriority::VeryLow, IoPriority::Low, IoPriority::Normal, IoPriority::High] {
            assert_eq!(io_from_value(io_value(p)), p);
        }
    }

    #[test]
    fn mem_values_match_spec() {
        assert_eq!(mem_value(MemPriority::VeryLow).0, 1);
        assert_eq!(mem_value(MemPriority::Low).0, 2);
        assert_eq!(mem_value(MemPriority::Medium).0, 3);
        assert_eq!(mem_value(MemPriority::BelowNormal).0, 4);
        assert_eq!(mem_value(MemPriority::Normal).0, 5);
        for p in [
            MemPriority::VeryLow,
            MemPriority::Low,
            MemPriority::Medium,
            MemPriority::BelowNormal,
            MemPriority::Normal,
        ] {
            assert_eq!(mem_from_value(mem_value(p)), p, "mem {p:?} должен сохраняться");
        }
    }

    #[test]
    fn unknown_class_falls_back_to_normal() {
        assert_eq!(class_from_flag(0), PriorityClass::Normal);
        assert_eq!(class_from_flag(0x1234), PriorityClass::Normal);
    }

    #[test]
    fn live_read_own_priorities() {
        let pid = std::process::id();
        let prio = get_priority(pid).expect("приоритет своего процесса читается");
        assert!(PriorityClass::ALL.contains(&prio));
        // Приоритет памяти доступен с Windows 8; в тесте допускаем любую ошибку API.
        let _ = get_memory_priority(pid);
        let _ = get_io_priority(pid);
    }

    #[test]
    fn missing_process_is_error_not_panic() {
        // Заведомо несуществующий PID.
        let pid = 0xFFFF_FFF0;
        assert!(matches!(
            get_priority(pid),
            Err(SysError::NotFound) | Err(SysError::AccessDenied) | Err(SysError::Other(_))
        ));
        assert!(set_priority(pid, PriorityClass::Normal).is_err());
        assert!(set_io_priority(pid, IoPriority::Normal).is_err());
        assert!(set_memory_priority(pid, MemPriority::Normal).is_err());
    }
}
