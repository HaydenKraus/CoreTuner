//! Перечисление процессов, загрузка логических CPU и иконки исполняемых файлов.
use crate::cpu::affinity;
use crate::cpu::priority;
use crate::cpu::topology::Topology;
use crate::cpu::{is_protected_name, HandleGuard};
use crate::model::{CpuSet, PriorityClass};
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{FILETIME, HANDLE, HWND, LPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

/// Сведения о процессе для UI и логики применения профилей.
#[derive(Clone, Debug, Default)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub exe_path: Option<String>,
    /// Загрузка CPU в процентах от одного логического CPU (0..100 на поток).
    pub cpu_percent: f32,
    pub ram_bytes: u64,
    pub priority: Option<PriorityClass>,
    pub affinity: Option<CpuSet>,
    pub has_window: bool,
    /// Доступны ли операции над процессом (права/защита).
    pub accessible: bool,
    pub protected: bool,
    /// Время запуска в секундах от UNIX EPOCH (из creation FILETIME).
    pub start_time: u64,
}

/// Перечислитель процессов: владеет замерами CPU между вызовами.
pub struct ProcessLister {
    /// Число логических CPU для нормализации `cpu_percent`.
    cpu_count: usize,
    /// Предыдущие замеры CPU: pid → (время запуска, время процесса kernel+user в 100 нс, момент).
    prev_cpu: HashMap<u32, (u64, u64, Instant)>,
    /// Кэш пути к exe: (pid, start_time) → путь (QueryFullProcessImageNameW дорогой).
    path_cache: HashMap<(u32, u64), String>,
    /// Длительность тика предыдущего замера CPU в 100 нс.
    prev_tick: u64,
}

impl Default for ProcessLister {
    fn default() -> Self {
        Self::new()
    }
}

/// Сырые сведения о процессе из ToolHelp + WinAPI.
struct RawProc {
    pid: u32,
    name: String,
    start_time: u64,
    /// kernel+user время процесса в 100 нс.
    cpu_100ns: Option<u64>,
    ram_bytes: u64,
    /// Открыт ли процесс для запросов (один `OpenProcess` за refresh).
    handle: Option<HandleGuard>,
}

impl ProcessLister {
    pub fn new() -> Self {
        Self {
            cpu_count: 1,
            prev_cpu: HashMap::new(),
            path_cache: HashMap::new(),
            prev_tick: 0,
        }
    }

    /// Обновить данные и вернуть список процессов.
    ///
    /// `cpu_percent` — загрузка в процентах от одного логического CPU: дельта
    /// (kernel+user) времени процесса между вызовами делится на реально прошедшее
    /// время и на число логических CPU.
    pub fn refresh(&mut self, topo: &Topology) -> Vec<ProcInfo> {
        self.cpu_count = topo.total().max(1);
        let windowed = windowed_pids();
        let now_instant = Instant::now();
        let now_tick = unix_time_100ns();
        let elapsed_100ns = now_tick.saturating_sub(self.prev_tick);

        let mut raws = snapshot_processes();
        if raws.is_empty() {
            return Vec::new();
        }

        let mut prev_cpu = HashMap::with_capacity(raws.len());
        let mut path_cache = HashMap::with_capacity(raws.len());
        let mut out: Vec<ProcInfo> = Vec::with_capacity(raws.len());

        for raw in raws.drain(..) {
            let RawProc { pid, name, start_time, cpu_100ns, ram_bytes, handle } = raw;
            let key = (pid, start_time);

            let exe_path = match self.path_cache.get(&key) {
                Some(p) => Some(p.clone()),
                None => match handle.as_ref().map(|h| exe_path_from(h.raw())) {
                    Some(Some(path)) => {
                        path_cache.insert(key, path.clone());
                        Some(path)
                    }
                    // Дескриптор не открыт: путь попробуем отдельным запросом.
                    None => query_exe_path(pid),
                    // Процесс открыт, но путь не читается — запоминать нечего.
                    Some(None) => None,
                },
            };

            // Дельту считаем только для того же образа процесса: при переиспользовании pid
            // время запуска меняется, и разница времён была бы бессмысленной.
            let cpu_percent = match (cpu_100ns, self.prev_cpu.get(&pid)) {
                (Some(cur), Some(&(prev_start, prev, _))) if prev_start == start_time => {
                    cpu_delta_percent(prev, cur, elapsed_100ns, self.cpu_count)
                }
                _ => 0.0,
            };
            if let Some(cur) = cpu_100ns {
                prev_cpu.insert(pid, (start_time, cur, now_instant));
            }

            let protected = is_protected_name(&name) || pid == std::process::id() || pid == 0;
            // Права запрашиваем только для открытого дескриптора и незащищённых процессов.
            let (priority_val, affinity_val) = match handle.filter(|_| !protected) {
                Some(_) => {
                    let prio = priority::get_priority(pid).ok();
                    let aff = affinity::get_affinity(pid, topo).ok();
                    (prio, aff)
                }
                None => (None, None),
            };
            let accessible = !protected && (priority_val.is_some() || affinity_val.is_some());

            out.push(ProcInfo {
                pid,
                name,
                exe_path,
                cpu_percent,
                ram_bytes,
                priority: priority_val,
                affinity: affinity_val,
                has_window: windowed.contains(&pid),
                accessible,
                protected,
                start_time,
            });
        }

        self.prev_cpu = prev_cpu;
        self.path_cache = path_cache;
        self.prev_tick = now_tick;
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)));
        out
    }
}

/// Загрузка CPU в процентах от одного логического CPU по дельте времени процесса.
///
/// `prev_delta` и `cur_delta` — время процесса (100 нс), `elapsed` — реальное время
/// между замерами (100 нс), `cpu_count` — число логических CPU.
fn cpu_delta_percent(prev: u64, cur: u64, elapsed: u64, cpu_count: usize) -> f32 {
    if elapsed == 0 || cpu_count == 0 || cur < prev {
        return 0.0;
    }
    let used = (cur - prev) as f64;
    let ratio = used / elapsed as f64 / cpu_count as f64 * 100.0;
    ratio.clamp(0.0, 100.0) as f32
}

/// Загрузка каждого логического CPU (по глобальному индексу), 0..100.
///
/// Источник — `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`:
/// загрузка считается по дельте времен между вызовами.
pub struct CpuMonitor {
    /// Предыдущие снимки по логическому CPU: время простоя и общее время (100 нс).
    prev: Vec<(u64, u64)>,
    /// Число логических CPU (длина результата; при недоступности — из топологии).
    total: usize,
}

impl Default for CpuMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuMonitor {
    pub fn new() -> Self {
        let mut mon = Self { prev: Vec::new(), total: 0 };
        mon.total = mon.sample().len();
        mon
    }

    /// Задать эталонное число логических CPU (из топологии).
    ///
    /// Вызывать при расхождении `Topology::total()` с числом CPU, видимым системой.
    pub fn set_total(&mut self, total: usize) {
        self.total = total;
    }

    /// Обновить и вернуть загрузку логических CPU по глобальному индексу.
    ///
    /// Длина результата — число логических CPU (недоступные — 0.0).
    pub fn refresh(&mut self) -> Vec<f32> {
        let sample = self.sample();
        let len = self.total.max(sample.len());
        let mut usage = vec![0.0f32; len];
        for (i, &(idle, kernel, user)) in sample.iter().enumerate() {
            let usage_i = match self.prev.get(i) {
                Some(&(prev_idle, prev_busy)) => {
                    let busy = (kernel + user).saturating_sub(prev_busy);
                    let total = busy + idle.saturating_sub(prev_idle);
                    if total > 0 { (busy as f64 / total as f64 * 100.0) as f32 } else { 0.0 }
                }
                None => 0.0,
            };
            usage[i] = usage_i.clamp(0.0, 100.0);
        }
        self.prev = sample.iter().map(|&(idle, kernel, user)| (idle, kernel + user)).collect();
        usage
    }

    /// Сырые счётчики по логическим CPU: (IdleTime, KernelTime, UserTime) в 100 нс.
    fn sample(&self) -> Vec<(u64, u64, u64)> {
        query_processor_times().unwrap_or_default()
    }
}

/// Времена простоя/ядра/пользователя по каждому логическому CPU через ntdll.
///
/// `GetProcAddress` — чтобы не зависеть от feature-набора крейта `windows`
/// и линковки ntdll: при недоступности функции возвращается `None`.
fn query_processor_times() -> Option<Vec<(u64, u64, u64)>> {
    /// SystemProcessorPerformanceInformation.
    const CLASS: u32 = 8;

    /// Эквивалент `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ProcessorPerformance {
        idle: i64,
        kernel: i64,
        user: i64,
        dpc: i64,
        interrupt: i64,
        interrupt_count: u32,
        _pad: u32,
    }

    type NtQuery = unsafe extern "system" fn(u32, *mut core::ffi::c_void, u32, *mut u32) -> i32;

    let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("ntdll.dll")) }.ok()?;
    let name = windows::core::s!("NtQuerySystemInformation");
    let proc = unsafe { windows::Win32::System::LibraryLoader::GetProcAddress(module, name) }?;
    // GetProcAddress возвращает FARPROC; адрес всегда ненулевой (проверено выше).
    // SAFETY: по имени запрошена функция ntdll с совместимой сигнатурой NtQuerySystemInformation.
    let query: NtQuery = unsafe { std::mem::transmute(proc) };

    // Пробный вызов с нулевым буфером: ожидается STATUS_INFO_LENGTH_MISMATCH (0xC0000004).
    let mut len: u32 = 0;
    let status = unsafe { query(CLASS, std::ptr::null_mut(), 0, &mut len) };
    if status >= 0 {
        return None;
    }
    // Запрашиваем с запасом: между пробным и реальным вызовом набор CPU не меняется,
    // но размер структуры может отличаться от ожидаемого — ориентируемся на `len`.
    let count = (len as usize / std::mem::size_of::<ProcessorPerformance>()).max(1) + 8;
    let mut buf = vec![ProcessorPerformance::default(); count];
    let bytes = (buf.len() * std::mem::size_of::<ProcessorPerformance>()) as u32;
    // SAFETY: буфер достаточного размера и выровнен под структуру; len — реально записанное.
    let status = unsafe { query(CLASS, buf.as_mut_ptr() as *mut core::ffi::c_void, bytes, &mut len) };
    if status < 0 {
        return None;
    }
    let got = ((len as usize) / std::mem::size_of::<ProcessorPerformance>()).min(buf.len());
    buf.truncate(got.max(1).min(count));
    Some(buf.iter().map(|p| (p.idle.max(0) as u64, p.kernel.max(0) as u64, p.user.max(0) as u64)).collect())
}

/// Снимок процессов через ToolHelp: все процессы, видимые системе.
fn snapshot_processes() -> Vec<RawProc> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    // SAFETY: вызов без указателей; снапшот закрывается guard'ом.
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let _guard = HandleGuard::new(snap);

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut out = Vec::new();
    // SAFETY: entry инициализирован, dwSize задан.
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
    while ok {
        let pid = entry.th32ProcessID;
        // PID 0 — «System Idle Process»: дескриптор не открыть, метрик нет.
        if pid != 0 {
            let name = String::from_utf16_lossy(&entry.szExeFile)
                .trim_end_matches('\0')
                .to_string();
            // SYSTEM (pid 4) защищён, а свой процесс открываем: без дескриптора нет памяти и CPU.
            let handle = if pid == 4 { None } else { open_process(pid) };
            // Один дескриптор на процесс за весь refresh: времена, память, потом путь.
            let (cpu_100ns, ram_bytes, start_time) = match handle.as_ref() {
                Some(h) => (process_cpu_time(h.raw()), process_working_set(h.raw()), process_start_time(h.raw())),
                None => (None, 0, 0),
            };
            out.push(RawProc { pid, name, start_time, cpu_100ns, ram_bytes, handle });
        }
        // SAFETY: дескриптор снапшота валиден на протяжении обхода.
        ok = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
    }
    out
}

/// Открыть процесс с минимальными правами (`PROCESS_QUERY_LIMITED_INFORMATION`).
fn open_process(pid: u32) -> Option<HandleGuard> {
    // SAFETY: вызов без указателей; владение дескриптором забирает guard.
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok().map(HandleGuard::new)
}

/// Полное имя exe по уже открытому дескриптору (`QueryFullProcessImageNameW`).
fn exe_path_from(handle: HANDLE) -> Option<String> {
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    use windows::core::PWSTR;

    // Длинные пути (\\?\...) не укладываются в MAX_PATH: буфер с запасом.
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as u32;
    // SAFETY: буфер и его размер валидны; PWSTR указывает на буфер.
    unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size)
    }
    .ok()?;
    buf.truncate(size as usize);
    Some(String::from_utf16_lossy(&buf))
}

/// Время процесса (kernel+user) в 100 нс через `GetProcessTimes`.
fn process_cpu_time(handle: HANDLE) -> Option<u64> {
    let (_, _, kernel, user) = process_times(handle)?;
    Some(filetime_to_u64(kernel).saturating_add(filetime_to_u64(user)))
}

/// Время создания процесса в секундах от UNIX EPOCH.
fn process_start_time(handle: HANDLE) -> u64 {
    process_times(handle).map_or(0, |(creation, _, _, _)| filetime_unix_seconds(creation))
}

/// `GetProcessTimes`: (creation, exit, kernel, user).
fn process_times(handle: HANDLE) -> Option<(FILETIME, FILETIME, FILETIME, FILETIME)> {
    use windows::Win32::System::Threading::GetProcessTimes;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: out-параметры валидны, дескриптор открыт с правом запроса.
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
    Some((creation, exit, kernel, user))
}

/// FILETIME → u64 (100 нс).
fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// FILETIME → секунды от UNIX EPOCH.
fn filetime_unix_seconds(ft: FILETIME) -> u64 {
    /// FILETIME эпохи UNIX (1970-01-01T00:00:00Z) — FILETIME уже UTC, пересчёт не нужен.
    const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;
    filetime_to_u64(ft).saturating_sub(UNIX_EPOCH_FILETIME) / 10_000_000
}

/// Рабочий набор процесса в байтах (`K32GetProcessMemoryInfo`).
fn process_working_set(handle: HANDLE) -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    let size = counters.cb;
    // SAFETY: структура и её размер валидны; дескриптор открыт с правом запроса.
    let ok = unsafe { GetProcessMemoryInfo(handle, &mut counters, size) };
    if ok.is_ok() {
        counters.WorkingSetSize as u64
    } else {
        0
    }
}

/// Текущее время в 100 нс от UNIX EPOCH (монотонно для дельт за короткий интервал).
fn unix_time_100ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64 / 100)
}

/// PID процессов, имеющих видимое окно верхнего уровня (без owner-окон).
fn windowed_pids() -> std::collections::HashSet<u32> {
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
    };

    /// Контекст для колбэка: множество PID.
    struct Ctx {
        pids: std::collections::HashSet<u32>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: lparam — указатель на Ctx, переданный из windowed_pids и живой на время вызова.
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        // SAFETY: hwnd валиден (передан системой).
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        if visible {
            // Окна с владельцем — служебные (диалоги, тултипы): не считаем их «окном приложения».
            // SAFETY: hwnd валиден.
            let has_owner = unsafe { GetWindow(hwnd, GW_OWNER) };
            if has_owner.is_err() {
                let mut pid = 0u32;
                // SAFETY: out-параметр валиден.
                unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
                if pid != 0 {
                    ctx.pids.insert(pid);
                }
            }
        }
        // Продолжаем перечисление.
        windows::core::BOOL(1)
    }

    let mut ctx = Ctx { pids: std::collections::HashSet::new() };
    // SAFETY: указатель на ctx живёт до конца вызова EnumWindows.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.pids
}

/// Иконка исполняемого файла 32×32 как `egui::ColorImage` (best-effort).
///
/// Возвращает `None`, если файл недоступен или иконку получить не удалось.
pub fn exe_icon(path: &str) -> Option<egui::ColorImage> {
    let icon = load_hicon(path)?;
    let image = hicon_to_image(icon);
    // SAFETY: icon получен из Shell/ExtractIcon и ещё не уничтожен.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon);
    }
    image
}

/// Получить HICON крупной иконки файла.
fn load_hicon(path: &str) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::HICON;

    let wide = crate::cpu::to_wide(path);
    let mut info = SHFILEINFOW::default();
    // SAFETY: строка NUL-терминирована, структура валидна, размер передан верно.
    let res = unsafe {
        SHGetFileInfoW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if res != 0 && !info.hIcon.is_invalid() {
        return Some(info.hIcon);
    }

    // Запасной путь: прямое извлечение иконки из файла.
    let mut large = HICON::default();
    // SAFETY: out-параметр валиден; путь корректен.
    let count = unsafe {
        windows::Win32::UI::Shell::ExtractIconExW(
            windows::core::PCWSTR(wide.as_ptr()),
            0,
            Some(&mut large),
            None,
            1,
        )
    };
    if count > 0 && !large.is_invalid() {
        return Some(large);
    }
    None
}

/// Скопировать пиксели HICON в `egui::ColorImage` 32×32 через GetDIBits.
fn hicon_to_image(icon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<egui::ColorImage> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, GetDIBits, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    const SIZE: i32 = 32;

    let mut info = ICONINFO::default();
    // SAFETY: out-параметр валиден, icon валиден.
    if unsafe { GetIconInfo(icon, &mut info) }.is_err() {
        return None;
    }
    // Цветной и маска-битмап нужно освободить в любом случае.
    let _color = BitmapGuard(info.hbmColor);
    let _mask = BitmapGuard(info.hbmMask);
    if info.hbmColor.is_invalid() {
        return None;
    }

    // SAFETY: создаём совместимый DC для чтения битмапа.
    let screen: HDC = unsafe { GetDC(None) };
    if screen.is_invalid() {
        return None;
    }
    // SAFETY: screen валиден.
    let mem: HDC = unsafe { CreateCompatibleDC(Some(screen)) };
    if mem.is_invalid() {
        // SAFETY: screen получен из GetDC.
        unsafe { ReleaseDC(None, screen) };
        return None;
    }
    let _screen_guard = ScreenDcGuard(screen);
    let _mem_guard = MemDcGuard(mem);

    // 32-битный BGRA, top-down (отрицательная высота) — удобно для чтения.
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIZE,
            biHeight: -SIZE,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
    // SAFETY: буфер достаточного размера, BITMAPINFO корректен, hbmColor валиден.
    let lines = unsafe {
        GetDIBits(
            mem,
            info.hbmColor,
            0,
            SIZE as u32,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return None;
    }

    let pixels: Vec<egui::Color32> = buf
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            // Иконки Windows приходят без альфы (0) — тогда считаем пиксель непрозрачным.
            let alpha = if a == 0 { 255 } else { a };
            egui::Color32::from_rgba_unmultiplied(r, g, b, alpha)
        })
        .collect();

    Some(egui::ColorImage {
        size: [SIZE as usize, SIZE as usize],
        pixels,
        source_size: egui::vec2(SIZE as f32, SIZE as f32),
    })
}

/// RAII-обёртка над HBITMAP.
struct BitmapGuard(windows::Win32::Graphics::Gdi::HBITMAP);

impl Drop for BitmapGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: битмап получен из GetIconInfo и ещё не удалён.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.0.into());
            }
        }
    }
}

/// RAII-обёртка над HDC, полученным из GetDC.
struct ScreenDcGuard(windows::Win32::Graphics::Gdi::HDC);

impl Drop for ScreenDcGuard {
    fn drop(&mut self) {
        // SAFETY: DC получен из GetDC(None) и ещё не освобождён.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(None, self.0);
        }
    }
}

/// RAII-обёртка над HDC, созданным CreateCompatibleDC.
struct MemDcGuard(windows::Win32::Graphics::Gdi::HDC);

impl Drop for MemDcGuard {
    fn drop(&mut self) {
        // SAFETY: DC создан CreateCompatibleDC и ещё не удалён.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteDC(self.0);
        }
    }
}

/// Длинный путь процесса по PID, best-effort (открывает процесс самостоятельно).
///
/// В `refresh` не используется: там дескриптор уже открыт и путь берётся из него
/// (плюс кэшируется по `(pid, start_time)`).
pub fn query_exe_path(pid: u32) -> Option<String> {
    let handle = open_process(pid)?;
    exe_path_from(handle.raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lister_returns_own_process() {
        let topo = Topology::detect().expect("topology");
        let mut lister = ProcessLister::new();
        let list = lister.refresh(&topo);

        assert!(!list.is_empty(), "список процессов не должен быть пустым");
        let me = std::process::id();
        let own = list.iter().find(|p| p.pid == me).expect("свой процесс должен быть в списке");
        assert!(own.protected, "CoreTuner помечает сам себя защищённым");
        assert!(own.protected && own.priority.is_none() && own.affinity.is_none());
        let name = own.name.to_lowercase();
        assert!(
            name.contains("coretuner") || name.contains("ct_") || name.contains("coretuner"),
            "имя процесса похоже на тестовый бинарь: {name}"
        );
        assert!(own.ram_bytes > 0, "рабочий набор своего процесса должен быть > 0");
        assert!(
            own.exe_path.as_deref().is_some_and(|p| p.to_lowercase().ends_with(".exe")),
            "путь к exe должен читаться: {:?}",
            own.exe_path
        );
        assert!((0.0..=100.0).contains(&own.cpu_percent));
        assert!(own.cpu_percent <= 100.0);
        assert!(own.start_time > 0);
        assert!(list.iter().all(|p| p.cpu_percent <= 100.0));
    }

    #[test]
    fn lister_sees_many_processes() {
        let topo = Topology::detect().expect("topology");
        let mut lister = ProcessLister::new();
        let list = lister.refresh(&topo);
        assert!(list.len() >= 5, "ToolHelp должен видеть минимум 5 процессов, видно {}", list.len());
        // PID уникальны.
        let mut pids: Vec<u32> = list.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), list.len());
    }

    #[test]
    fn lister_start_time_is_stable_between_refreshes() {
        let topo = Topology::detect().expect("topology");
        let mut lister = ProcessLister::new();
        let first = lister.refresh(&topo);
        let second = lister.refresh(&topo);
        let me = std::process::id();
        let a = first.iter().find(|p| p.pid == me).map(|p| p.start_time);
        let b = second.iter().find(|p| p.pid == me).map(|p| p.start_time);
        assert!(a.is_some() && a == b, "start_time одного процесса должен быть стабилен: {a:?} vs {b:?}");
    }

    #[test]
    fn cpu_delta_percent_math() {
        // 1 CPU, 1 секунда между замерами (10М тиков по 100 нс), процесс занял 0.5 с.
        assert!((cpu_delta_percent(0, 5_000_000, 10_000_000, 1) - 50.0).abs() < 0.01);
        // Те же данные на 4 логических CPU — загрузка в 4 раза меньше.
        assert!((cpu_delta_percent(0, 5_000_000, 10_000_000, 4) - 12.5).abs() < 0.01);
        // Полная загрузка одного потока одного CPU.
        assert!((cpu_delta_percent(0, 10_000_000, 10_000_000, 1) - 100.0).abs() < 0.01);
        // Переполнение нормализации обрезается до 100.
        assert!((cpu_delta_percent(0, 40_000_000, 10_000_000, 1) - 100.0).abs() < 0.01);
        // Нет времени между замерами, нет CPU, время процесса убыло — ноль.
        assert_eq!(cpu_delta_percent(0, 5_000_000, 0, 4), 0.0);
        assert_eq!(cpu_delta_percent(0, 5_000_000, 10_000_000, 0), 0.0);
        assert_eq!(cpu_delta_percent(5_000_000, 0, 10_000_000, 4), 0.0);
        // Дельта отсутствует.
        assert_eq!(cpu_delta_percent(7, 7, 10_000_000, 4), 0.0);
    }

    #[test]
    fn filetime_unix_seconds_math() {
        const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;
        let one_sec = UNIX_EPOCH_FILETIME + 10_000_000;
        let ft = FILETIME { dwLowDateTime: one_sec as u32, dwHighDateTime: (one_sec >> 32) as u32 };
        assert_eq!(filetime_unix_seconds(ft), 1);
        assert_eq!(filetime_unix_seconds(FILETIME::default()), 0);
    }

    #[test]
    fn lister_finds_system_process() {
        let topo = Topology::detect().expect("topology");
        let mut lister = ProcessLister::new();
        let list = lister.refresh(&topo);
        // PID 4 (System) существует всегда и обязан быть защищённым.
        if let Some(sys) = list.iter().find(|p| p.pid == 4) {
            assert!(sys.protected);
            assert!(sys.priority.is_none());
        }
    }

    #[test]
    fn cpu_monitor_length_matches_total() {
        let topo = Topology::detect().expect("topology");
        let mut mon = CpuMonitor::new();
        mon.set_total(topo.total());
        let usage = mon.refresh();
        assert!(usage.len() >= topo.total());
        assert!(usage.iter().all(|u| (0.0..=100.0).contains(u)));
    }

    #[test]
    fn exe_icon_best_effort_never_panics() {
        // Заведомо несуществующий файл: ожидаем None, без паники.
        assert!(exe_icon(r"C:\__no_such_file__\nope.exe").is_none());
        // Реальный файл (сам тестовый бинарник) — иконка может отсутствовать,
        // но вызов обязан корректно завершиться.
        if let Ok(exe) = std::env::current_exe() {
            let _ = exe_icon(&exe.to_string_lossy());
        }
    }

    #[test]
    fn query_exe_path_of_self() {
        let p = query_exe_path(std::process::id());
        assert!(p.is_some(), "путь к своему исполняемому файлу должен читаться");
        assert!(query_exe_path(0xFFFF_FFF0).is_none());
    }

    #[test]
    fn windowed_pids_are_real() {
        let pids = windowed_pids();
        // В интерактивной сессии окна обычно есть; в CI их может не быть — проверяем
        // только отсутствие невалидных нулевых PID.
        assert!(!pids.contains(&0));
    }

    #[test]
    fn enum_windows_sees_visible_toplevel_windows() {
        // Прямая проверка нижнего слоя: EnumWindows должен видеть видимые окна
        // верхнего уровня. Если сессия неинтерактивна, окон может не быть.
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsWindowVisible};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static TOTAL: AtomicUsize = AtomicUsize::new(0);
        static VISIBLE: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn cb(hwnd: HWND, _l: LPARAM) -> windows::core::BOOL {
            TOTAL.fetch_add(1, Ordering::Relaxed);
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                VISIBLE.fetch_add(1, Ordering::Relaxed);
            }
            windows::core::BOOL(1)
        }
        // SAFETY: колбэк не разыменовывает lparam и живёт до конца вызова.
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(0));
        }
        assert!(TOTAL.load(Ordering::Relaxed) > 0, "EnumWindows обязан перечислять окна");
        // `windowed_pids` возвращает подмножество PID видимых окон.
        let pids = windowed_pids();
        assert!(pids.len() <= VISIBLE.load(Ordering::Relaxed));
    }
}
