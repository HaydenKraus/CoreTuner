//! Чтение и установка привязки процесса к логическим процессорам.
//!
//! В системе с одной группой процессоров используется классический
//! `Get/SetProcessAffinityMask`; при нескольких группах — CPU Sets API
//! (`Get/SetProcessDefaultCpuSets`), потому что маска `usize` покрывает
//! только первые 64 логических CPU.
use crate::cpu::topology::Topology;
use crate::cpu::{HandleGuard, SysError};
use crate::model::CpuSet;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    GetProcessAffinityMask, GetProcessDefaultCpuSets, OpenProcess, SetProcessAffinityMask,
    SetProcessDefaultCpuSets, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SET_LIMITED_INFORMATION,
};

/// Открыть процесс с нужными правами доступа.
fn open(pid: u32, access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS) -> Result<HandleGuard, SysError> {
    // SAFETY: вызов без указателей; владение дескриптором забирает HandleGuard.
    unsafe { OpenProcess(access, false, pid) }
        .map(HandleGuard::new)
        .map_err(|e| SysError::from_win_error(&e))
}

/// Сколько групп процессоров в системе (по топологии).
fn group_count(topo: &Topology) -> usize {
    topo.cpus.iter().map(|c| c.group as usize + 1).max().unwrap_or(1)
}

/// Прочитать текущий набор логических CPU процесса.
///
/// Одна группа: `GetProcessAffinityMask` (маска фильтруется системной маской процесса).
/// Несколько групп: `GetProcessGroupAffinity` + CPU Sets API.
pub fn get_affinity(pid: u32, topo: &Topology) -> Result<CpuSet, SysError> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    get_affinity_handle(handle.raw(), topo)
}

/// Внутренняя реализация: работает с уже открытым дескриптором.
fn get_affinity_handle(handle: HANDLE, topo: &Topology) -> Result<CpuSet, SysError> {
    if topo.total() == 0 {
        return Err(SysError::Invalid);
    }

    if group_count(topo) == 1 {
        let mut process_mask: usize = 0;
        let mut system_mask: usize = 0;
        // SAFETY: оба out-параметра валидны; дескриптор имеет PROCESS_QUERY_LIMITED_INFORMATION.
        unsafe { GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask) }
            .map_err(|e| SysError::from_win_error(&e))?;
        // Фильтруем по системной маске: биты вне неё не должны попадать в набор.
        let mask = (process_mask & system_mask) as u64;
        return Ok(cpus_from_group_mask(topo, 0, mask));
    }

    // Многогруппный случай: сначала пробуем CPU Sets (он единственный даёт точный
    // набор при >64 CPU), при неудаче — маска по группам процесса.
    if let Some(ids) = cpu_set_ids(handle) {
        if !ids.is_empty() {
            let mapped = map_cpu_set_ids(topo, &ids);
            if !mapped.is_empty() {
                return Ok(mapped);
            }
        }
    }

    let groups = process_groups(handle)?;
    let mut set = CpuSet::new();
    for g in groups {
        // GetProcessAffinityMask для многогруппного процесса возвращает маску его
        // основной группы; остальные группы берём как полностью доступные.
        let mask = if g == 0 {
            let mut pm: usize = 0;
            let mut sm: usize = 0;
            // SAFETY: out-параметры валидны.
            match unsafe { GetProcessAffinityMask(handle, &mut pm, &mut sm) } {
                Ok(()) => (pm & sm) as u64,
                Err(_) => group_mask(topo, g),
            }
        } else {
            group_mask(topo, g)
        };
        for cpu in topo.cpus.iter().filter(|c| c.group == g) {
            if mask & (1u64 << cpu.bit) != 0 {
                set.insert(cpu.index);
            }
        }
    }
    Ok(set)
}

/// Установить набор логических CPU для процесса.
///
/// Пустой набор — [`SysError::Invalid`]. Одна группа: `SetProcessAffinityMask`.
/// Несколько групп: `SetProcessDefaultCpuSets`.
pub fn set_affinity(pid: u32, set: &CpuSet, topo: &Topology) -> Result<(), SysError> {
    if set.is_empty() {
        return Err(SysError::Invalid);
    }
    if topo.total() == 0 {
        return Err(SysError::Invalid);
    }

    // Отбрасываем индексы, которых нет в топологии.
    let masks = topo.group_masks(set);
    if masks.is_empty() {
        return Err(SysError::Invalid);
    }

    // Права: SetProcessAffinityMask требует PROCESS_SET_INFORMATION,
    // SetProcessDefaultCpuSets — PROCESS_SET_LIMITED_INFORMATION.
    let multi = group_count(topo) > 1;
    let access = if multi { PROCESS_SET_LIMITED_INFORMATION } else { PROCESS_SET_INFORMATION };
    let handle = open(pid, access)?;

    if !multi {
        let mask = masks.iter().map(|(_, m)| *m).fold(0u64, |a, b| a | b) as usize;
        // SAFETY: маска непустая и соответствует топологии; дескриптор имеет нужные права.
        return unsafe { SetProcessAffinityMask(handle.raw(), mask) }
            .map_err(|e| SysError::from_win_error(&e));
    }

    let ids = cpu_set_ids_for(topo, set);
    if ids.is_empty() {
        return Err(SysError::Invalid);
    }
    // SAFETY: срез id валиден на время вызова.
    let ok = unsafe { SetProcessDefaultCpuSets(handle.raw(), Some(&ids)) };
    if ok.as_bool() {
        Ok(())
    } else {
        Err(last_error())
    }
}

/// `GetLastError()` в виде [`SysError`].
fn last_error() -> SysError {
    // SAFETY: GetLastError безопасен и не требует аргументов.
    SysError::from_last_error(unsafe { windows::Win32::Foundation::GetLastError().0 })
}

/// CPU Set ID для каждого глобального индекса логического CPU.
///
/// На практике CPU Set ID совпадает с глобальным номером логического процессора,
/// но берём его из системы, чтобы не полагаться на соглашение.
fn cpu_set_ids_for(topo: &Topology, set: &CpuSet) -> Vec<u32> {
    let known = system_cpu_set_ids();
    let by_index: std::collections::HashMap<usize, u32> =
        known.iter().map(|(idx, id)| (*idx, *id)).collect();
    set.iter()
        .filter(|i| *i < topo.total())
        .map(|i| by_index.get(&i).copied().unwrap_or(i as u32))
        .collect()
}

/// Все CPU Set ID системы, упорядоченные по глобальному индексу логического CPU.
///
/// Пустой результат означает, что CPU Sets API недоступен (старая Windows).
fn system_cpu_set_ids() -> Vec<(usize, u32)> {
    use windows::Win32::System::SystemInformation::{
        GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    };

    let mut len: u32 = 0;
    // Первый вызов — размер буфера.
    // SAFETY: нулевой буфер допустим и возвращает требуемый размер.
    let _ = unsafe { GetSystemCpuSetInformation(None, 0, &mut len, None, None) };
    if len == 0 {
        return Vec::new();
    }

    let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
    let mut buffer: Vec<u64> = vec![0u64; words];
    let ptr = buffer.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION;
    // SAFETY: буфер достаточного размера и выровнен.
    let ok = unsafe { GetSystemCpuSetInformation(Some(ptr), len, &mut len, None, None) };
    if !ok.as_bool() {
        return Vec::new();
    }

    // SAFETY: буфер заполнен системой; разбор ограничен `len` и полем `Size`.
    let mut out = unsafe { parse_cpu_set_entries(buffer.as_ptr() as *const u8, len as usize) };
    out.sort_by_key(|(idx, _)| *idx);
    out
}

/// Разбор буфера переменной длины из `GetSystemCpuSetInformation`.
///
/// # Safety
/// `base` должен указывать на буфер длиной не менее `len` байт, заполненный WinAPI.
unsafe fn parse_cpu_set_entries(base: *const u8, len: usize) -> Vec<(usize, u32)> {
    use windows::Win32::System::SystemInformation::{
        CpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    };

    const HEADER: usize = std::mem::size_of::<SYSTEM_CPU_SET_INFORMATION>();
    let mut out = Vec::new();
    let mut offset = 0usize;

    while offset + HEADER <= len {
        // SAFETY: заголовок целиком помещается в буфер (проверено условием цикла).
        let entry = unsafe { &*(base.add(offset) as *const SYSTEM_CPU_SET_INFORMATION) };
        let size = entry.Size as usize;
        if size < HEADER || offset + size > len {
            break;
        }
        if entry.Type == CpuSetInformation {
            // SAFETY: Type == CpuSetInformation гарантирует вариант `CpuSet`.
            let cs = unsafe { entry.Anonymous.CpuSet };
            let index = cs.Group as usize * 64 + cs.LogicalProcessorIndex as usize;
            out.push((index, cs.Id));
        }
        offset += size;
    }
    out
}

/// Прочитать CPU Set ID процесса (None — API недоступен или набор не задан явно).
fn cpu_set_ids(handle: HANDLE) -> Option<Vec<u32>> {
    let mut required: u32 = 0;
    // Первый вызов — узнать число элементов.
    // SAFETY: пустой срез и out-параметр допустимы.
    let _ = unsafe { GetProcessDefaultCpuSets(handle, None, &mut required) };
    if required == 0 {
        // Набор не задан явно (процесс использует все CPU) — считаем недоступным.
        return None;
    }
    let mut ids = vec![0u32; required as usize];
    // SAFETY: буфер достаточного размера, соответствующий required.
    let ok = unsafe { GetProcessDefaultCpuSets(handle, Some(&mut ids), &mut required) };
    if ok.as_bool() {
        ids.truncate(required as usize);
        Some(ids)
    } else {
        None
    }
}

/// Список групп процессоров, доступных процессу.
fn process_groups(handle: HANDLE) -> Result<Vec<u16>, SysError> {
    use windows::Win32::System::Threading::GetProcessGroupAffinity;
    let mut count: u16 = 0;
    // Первый вызов: получить число групп.
    // SAFETY: нулевой массив допустим и возвращает требуемое число групп.
    let _ = unsafe { GetProcessGroupAffinity(handle, &mut count, std::ptr::null_mut()) };
    if count == 0 {
        return Err(last_error());
    }
    let mut groups = vec![0u16; count as usize];
    // SAFETY: буфер рассчитан на count элементов.
    let ok = unsafe { GetProcessGroupAffinity(handle, &mut count, groups.as_mut_ptr()) };
    if !ok.as_bool() {
        return Err(last_error());
    }
    groups.truncate(count as usize);
    Ok(groups)
}

/// Все логические CPU указанной группы, как маска.
fn group_mask(topo: &Topology, group: u16) -> u64 {
    topo.cpus
        .iter()
        .filter(|c| c.group == group)
        .fold(0u64, |acc, c| acc | (1u64 << c.bit))
}

/// Преобразовать маску одной группы в набор глобальных индексов.
fn cpus_from_group_mask(topo: &Topology, group: u16, mask: u64) -> CpuSet {
    CpuSet::from_indices(
        topo.cpus
            .iter()
            .filter(|c| c.group == group && mask & (1u64 << c.bit) != 0)
            .map(|c| c.index),
    )
}

/// Преобразовать CPU Set ID в набор глобальных индексов по данным системы.
fn map_cpu_set_ids(topo: &Topology, ids: &[u32]) -> CpuSet {
    let known = system_cpu_set_ids();
    if known.is_empty() {
        // Без системной информации считаем, что ID совпадает с глобальным индексом.
        return CpuSet::from_indices(ids.iter().map(|&i| i as usize).filter(|i| *i < topo.total()));
    }
    let index_of: std::collections::HashMap<u32, usize> =
        known.iter().map(|(idx, id)| (*id, *idx)).collect();
    CpuSet::from_indices(ids.iter().filter_map(|id| index_of.get(id).copied()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::topology::{RawCore, Topology};

    fn topo_two_groups() -> Topology {
        let mut entries = Vec::new();
        for core in 0..32 {
            entries.push(RawCore { group: 0, efficiency_class: 0, mask: 0b11u64 << (core * 2) });
        }
        entries.push(RawCore { group: 1, efficiency_class: 0, mask: 0xFF });
        Topology::from_raw(&entries)
    }

    #[test]
    fn group_mask_helpers() {
        let t = topo_two_groups();
        assert_eq!(group_mask(&t, 0), u64::MAX);
        assert_eq!(group_mask(&t, 1), 0xFF);
        assert_eq!(cpus_from_group_mask(&t, 1, 0b101), CpuSet::from_indices([64, 66]));
        assert_eq!(group_count(&t), 2);
    }

    #[test]
    fn single_group_topology() {
        let t = Topology::from_raw(&[RawCore { group: 0, efficiency_class: 0, mask: 0b1111 }]);
        assert_eq!(group_count(&t), 1);
        assert_eq!(cpus_from_group_mask(&t, 0, 0b0101), CpuSet::from_indices([0, 2]));
    }

    #[test]
    fn set_affinity_rejects_empty() {
        let t = topo_two_groups();
        assert_eq!(set_affinity(0, &CpuSet::new(), &t), Err(SysError::Invalid));
    }

    #[test]
    fn set_affinity_rejects_unknown_indices() {
        let t = topo_two_groups();
        let bogus = CpuSet::from_indices([1000]);
        assert_eq!(set_affinity(0, &bogus, &t), Err(SysError::Invalid));
    }

    #[test]
    fn live_read_own_process_does_not_panic() {
        let t = Topology::detect().expect("topology");
        // Чтение привязки текущего процесса обязано отработать без паники.
        let res = get_affinity(std::process::id(), &t);
        assert!(res.is_ok(), "свой процесс должен читаться: {res:?}");
        let set = res.expect("ok");
        assert!(!set.is_empty(), "у процесса всегда есть хотя бы один CPU");
        assert!(set.iter().all(|i| i < t.total()));
    }
}
