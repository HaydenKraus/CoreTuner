//! Топология процессоров: физические ядра, их потоки, группы и гибридность (P/E-ядра).
use crate::model::CpuSet;
use crate::cpu::SysError;

/// Один логический процессор (поток), пронумерованный глобально.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalCpu {
    /// Глобальный индекс 0..N (сквозная нумерация по группам).
    pub index: usize,
    /// Номер группы процессоров (обычно 0; >0 при >64 логических CPU).
    pub group: u16,
    /// Бит внутри группы, 0..=63.
    pub bit: u8,
    /// Индекс физического ядра в `Topology::cores`.
    pub core: usize,
    /// 0 — первый поток ядра, >0 — виртуальный (SMT) поток.
    pub smt_index: usize,
}

/// Физическое ядро и его логические потоки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalCore {
    /// Индекс ядра в `Topology::cores`.
    pub index: usize,
    /// Группа процессоров, в которой находится ядро.
    pub group: u16,
    /// Класс эффективности (0 — E-ядра, больше — производительнее; у не-гибридных 0).
    pub efficiency_class: u8,
    /// Глобальные индексы `LogicalCpu`, принадлежащих ядру.
    pub threads: Vec<usize>,
}

/// Сырое описание ядра для [`Topology::from_raw`] (используется в тестах).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawCore {
    pub group: u16,
    pub efficiency_class: u8,
    /// Битовая маска потоков ядра внутри группы.
    pub mask: u64,
}

/// Полная топология системы.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    pub cores: Vec<PhysicalCore>,
    pub cpus: Vec<LogicalCpu>,
    /// Есть ли ядра с разными классами эффективности (Intel P/E, ARM big.LITTLE).
    pub is_hybrid: bool,
}

impl Topology {
    /// Определить топологию через `GetLogicalProcessorInformationEx(RelationProcessorCore)`.
    ///
    /// При недоступности API (или пустом результате) — запасной вариант: N одиночных
    /// однопоточных ядер по данным `GetSystemInfo`.
    pub fn detect() -> anyhow::Result<Topology> {
        match detect_via_api() {
            Ok(raws) if !raws.is_empty() => Ok(Self::from_raw(&raws)),
            Ok(_) => Ok(Self::fallback()),
            Err(_) => Ok(Self::fallback()),
        }
    }

    /// Построить топологию из сырых описаний ядер (чистая функция, пригодна для тестов).
    ///
    /// Порядок: ядра сортируются по (группа, минимальный бит маски), потоки внутри ядра —
    /// по возрастанию бита. Нулевые маски игнорируются.
    pub fn from_raw(entries: &[RawCore]) -> Topology {
        let mut sorted: Vec<RawCore> = entries.iter().copied().filter(|e| e.mask != 0).collect();
        sorted.sort_by_key(|e| (e.group, e.mask.trailing_zeros()));

        let mut cores: Vec<PhysicalCore> = Vec::with_capacity(sorted.len());
        let mut cpus: Vec<LogicalCpu> = Vec::new();
        let mut classes: Vec<u8> = Vec::new();

        for raw in sorted {
            let core_index = cores.len();
            let mut threads: Vec<usize> = Vec::new();
            for bit in 0..64u8 {
                if raw.mask & (1u64 << bit) == 0 {
                    continue;
                }
                let index = cpus.len();
                cpus.push(LogicalCpu {
                    index,
                    group: raw.group,
                    bit,
                    core: core_index,
                    smt_index: threads.len(),
                });
                threads.push(index);
            }
            classes.push(raw.efficiency_class);
            cores.push(PhysicalCore {
                index: core_index,
                group: raw.group,
                efficiency_class: raw.efficiency_class,
                threads,
            });
        }

        let first = classes.first().copied();
        let is_hybrid = first.is_some_and(|f| classes.iter().any(|&c| c != f));

        Topology { cores, cpus, is_hybrid }
    }

    /// Запасная топология: все логические CPU — отдельные физические ядра.
    fn fallback() -> Topology {
        use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
        let mut info = SYSTEM_INFO::default();
        // SAFETY: GetSystemInfo заполняет переданную структуру.
        unsafe { GetSystemInfo(&mut info) };
        let count = info.dwNumberOfProcessors.max(1) as usize;
        let raws: Vec<RawCore> = (0..count)
            .map(|i| RawCore {
                group: (i / 64) as u16,
                efficiency_class: 0,
                mask: 1u64 << (i % 64),
            })
            .collect();
        Self::from_raw(&raws)
    }

    /// Общее число логических процессоров.
    pub fn total(&self) -> usize {
        self.cpus.len()
    }

    /// Число физических ядер.
    #[cfg(test)]
    pub fn core_count(&self) -> usize {
        self.cores.len()
    }

    /// Является ли ядро производительным (P-core): только для гибридных систем,
    /// максимум класса эффективности.
    pub fn is_p_core(&self, core: usize) -> bool {
        if !self.is_hybrid {
            return false;
        }
        let max = self.cores.iter().map(|c| c.efficiency_class).max();
        match (self.cores.get(core), max) {
            (Some(c), Some(m)) => c.efficiency_class == m,
            _ => false,
        }
    }

    /// Все логические процессоры.
    pub fn all_set(&self) -> CpuSet {
        CpuSet::from_indices(0..self.total())
    }

    /// Первый поток каждого физического ядра.
    pub fn physical_set(&self) -> CpuSet {
        CpuSet::from_indices(self.cores.iter().filter_map(|c| c.threads.first().copied()))
    }

    /// Виртуальные (SMT) потоки — все, кроме первого потока своего ядра.
    pub fn virtual_set(&self) -> CpuSet {
        CpuSet::from_indices(self.cpus.iter().filter(|c| c.smt_index > 0).map(|c| c.index))
    }

    /// Разложить набор глобальных индексов в маски по группам процессоров.
    ///
    /// Неизвестные индексы (вне топологии) игнорируются.
    pub fn group_masks(&self, set: &CpuSet) -> Vec<(u16, u64)> {
        let mut acc: Vec<(u16, u64)> = Vec::new();
        for idx in set.iter() {
            let Some(cpu) = self.cpus.get(idx) else { continue };
            match acc.iter_mut().find(|(g, _)| *g == cpu.group) {
                Some((_, mask)) => *mask |= 1u64 << cpu.bit,
                None => acc.push((cpu.group, 1u64 << cpu.bit)),
            }
        }
        acc.sort_by_key(|(g, _)| *g);
        acc
    }

    /// Собрать набор глобальных индексов из масок по группам (используется только тестами).
    #[cfg(test)]
    pub fn set_from_group_masks(&self, masks: &[(u16, u64)]) -> CpuSet {
        let mut set = CpuSet::new();
        for (group, mask) in masks {
            for cpu in self.cpus.iter().filter(|c| c.group == *group) {
                if mask & (1u64 << cpu.bit) != 0 {
                    set.insert(cpu.index);
                }
            }
        }
        set
    }

    /// Человекочитаемая метка ядра для UI: "P" / "E" / пусто.
    pub fn core_label(&self, core: usize) -> &'static str {
        if !self.is_hybrid {
            return "";
        }
        match self.cores.get(core) {
            Some(_) if self.is_p_core(core) => "P",
            Some(_) => "E",
            None => "",
        }
    }
}

/// Чтение RelationProcessorCore через WinAPI.
fn detect_via_api() -> Result<Vec<RawCore>, SysError> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };

    /// ERROR_INSUFFICIENT_BUFFER — ожидаемый отказ первого (пробного) вызова.
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

    let mut len: u32 = 0;
    // Первый вызов — узнать требуемый размер буфера.
    // SAFETY: нулевой буфер с нулевым размером допустим и возвращает требуемый размер.
    let probe = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len) };
    if let Err(e) = probe {
        // ERROR_INSUFFICIENT_BUFFER (122) — ожидаемый «успешный» отказ первого вызова.
        if e.code().0 as u32 != ERROR_INSUFFICIENT_BUFFER || len == 0 {
            return Err(SysError::from_win_error(&e));
        }
    }
    if len == 0 {
        return Err(SysError::Invalid);
    }

    // Буфер выровнен под u64: структуры содержат usize/u64-поля.
    let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
    let mut buffer: Vec<u64> = vec![0u64; words];
    let ptr = buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
    // SAFETY: буфер достаточного размера, выровнен, время жизни перекрывает вызов.
    unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, Some(ptr), &mut len) }
        .map_err(|e| SysError::from_win_error(&e))?;

    // SAFETY: буфер заполнен системой; разбор ограничен `len` и полем `Size` каждой записи.
    let raws = unsafe { parse_processor_entries(buffer.as_ptr() as *const u8, len as usize) };
    Ok(raws)
}

/// Разбор буфера переменной длины из `GetLogicalProcessorInformationEx`.
///
/// # Safety
/// `base` должен указывать на буфер длиной не менее `len` байт, заполненный WinAPI.
unsafe fn parse_processor_entries(base: *const u8, len: usize) -> Vec<RawCore> {
    use windows::Win32::System::SystemInformation::SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;

    const HEADER: usize = std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
    let mut out = Vec::new();
    let mut offset = 0usize;

    while offset + HEADER <= len {
        // SAFETY: проверено, что заголовок целиком помещается в буфер.
        let entry = unsafe { &*(base.add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX) };
        let size = entry.Size as usize;
        // Защита от зацикливания и выхода за буфер при повреждённых данных.
        if size < HEADER || offset + size > len {
            break;
        }

        // SAFETY: RelationProcessorCore гарантирует вариант `Processor` в объединении.
        let processor = unsafe { entry.Anonymous.Processor };
        let flags = processor.Flags as u32;
        // LTP_PC_SMT: ядро имеет более одного логического процессора.
        let has_smt = flags & windows::Win32::System::SystemServices::LTP_PC_SMT != 0;

        // GroupMask — гибкий массив из GroupCount элементов. Ограничиваем чтение
        // размером записи, чтобы не выйти за её границы при повреждённых данных.
        const GA_SIZE: usize = std::mem::size_of::<windows::Win32::System::SystemInformation::GROUP_AFFINITY>();
        let declared = processor.GroupCount as usize;
        let max_by_size = (size - HEADER) / GA_SIZE.max(1);
        let group_count = declared.min(max_by_size);

        for g in 0..group_count {
            // SAFETY: элемент g целиком помещается в запись (проверено выше).
            let ga = unsafe { std::ptr::read_unaligned(processor.GroupMask.as_ptr().add(g)) };
            let mask = ga.Mask as u64;
            if mask == 0 {
                continue;
            }
            let eff_mask = if has_smt {
                // Несколько потоков на ядро: берём всю маску группы.
                mask
            } else {
                // Без SMT у ядра один поток — только младший бит, чтобы не дублировать ядра.
                mask.isolate_lowest_one()
            };
            out.push(RawCore { group: ga.Group, efficiency_class: processor.EfficiencyClass, mask: eff_mask });
        }
        offset += size;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(group: u16, class: u8, mask: u64) -> RawCore {
        RawCore { group, efficiency_class: class, mask }
    }

    #[test]
    fn single_core_with_smt() {
        let t = Topology::from_raw(&[raw(0, 0, 0b11)]);
        assert_eq!(t.total(), 2);
        assert_eq!(t.core_count(), 1);
        assert!(!t.is_hybrid);
        assert_eq!(t.cpus[0].smt_index, 0);
        assert_eq!(t.cpus[1].smt_index, 1);
        assert_eq!(t.cpus[0].bit, 0);
        assert_eq!(t.cpus[1].bit, 1);
        assert_eq!(t.cpus[0].core, 0);
        assert_eq!(t.cores[0].threads, vec![0, 1]);
    }

    #[test]
    fn two_cores_smt_masks() {
        // Ядро 0: биты 0,1; ядро 1: биты 2,3.
        let t = Topology::from_raw(&[raw(0, 0, 0b1100), raw(0, 0, 0b0011)]);
        assert_eq!(t.total(), 4);
        assert_eq!(t.core_count(), 2);
        // Сортировка по младшему биту: ядро с маской 0b11 идёт первым.
        assert_eq!(t.cores[0].threads, vec![0, 1]);
        assert_eq!(t.cores[1].threads, vec![2, 3]);
        assert_eq!(t.physical_set(), CpuSet::from_indices([0, 2]));
        assert_eq!(t.virtual_set(), CpuSet::from_indices([1, 3]));
        assert_eq!(t.all_set(), CpuSet::from_indices([0, 1, 2, 3]));
    }

    #[test]
    fn hybrid_efficiency_classes() {
        // Класс 1 — P-ядра (биты 0..4), класс 0 — E-ядра (биты 4..8).
        let t = Topology::from_raw(&[
            raw(0, 1, 0b0000_1111),
            raw(0, 0, 0b1111_0000),
        ]);
        assert!(t.is_hybrid);
        assert!(t.is_p_core(0));
        assert!(!t.is_p_core(1));
        assert_eq!(t.core_label(0), "P");
        assert_eq!(t.core_label(1), "E");
    }

    #[test]
    fn non_hybrid_is_never_p_core() {
        let t = Topology::from_raw(&[raw(0, 0, 0b1), raw(0, 0, 0b10)]);
        assert!(!t.is_hybrid);
        assert!(!t.is_p_core(0));
        assert_eq!(t.core_label(0), "");
    }

    #[test]
    fn more_than_64_threads_in_two_groups() {
        // 2 группы: 64 потока в группе 0 (32 ядра по 2 потока) и 8 в группе 1.
        let mut entries = Vec::new();
        for core in 0..32 {
            entries.push(raw(0, 0, 0b11u64 << (core * 2)));
        }
        entries.push(raw(1, 0, 0xFF));
        let t = Topology::from_raw(&entries);

        assert_eq!(t.total(), 72);
        assert_eq!(t.core_count(), 33);
        assert_eq!(t.cpus[0].index, 0);
        assert_eq!(t.cpus[0].group, 0);
        assert_eq!(t.cpus[64].group, 1);
        assert_eq!(t.cpus[64].bit, 0);
        assert_eq!(t.cpus[71].group, 1);
        assert_eq!(t.cpus[71].bit, 7);

        // Глобальные индексы в группе 1 начинаются с 64.
        let first_of_group1 = CpuSet::from_indices([64]);
        assert_eq!(t.group_masks(&first_of_group1), vec![(1, 0b1)]);
        assert_eq!(t.set_from_group_masks(&[(1, 0b101)]), CpuSet::from_indices([64, 66]));
        assert_eq!(t.set_from_group_masks(&[(0, 0b11)]), CpuSet::from_indices([0, 1]));

        // Физические ядра группы 1 — только бит 0 (первый поток ядра).
        let phys_g1: Vec<usize> = t.physical_set().iter().filter(|&i| i >= 64).collect();
        assert_eq!(phys_g1, vec![64]);
    }

    #[test]
    fn group_masks_roundtrip() {
        let entries = vec![
            raw(0, 0, 0b0000_0011), // cpus 0,1
            raw(0, 0, 0b0000_1100), // cpus 2,3
            raw(1, 0, 0b0000_0011), // cpus 4,5
        ];
        let t = Topology::from_raw(&entries);
        let set = CpuSet::from_indices([0, 3, 5]);
        let masks = t.group_masks(&set);
        assert_eq!(masks, vec![(0, 0b1001), (1, 0b10)]);
        assert_eq!(t.set_from_group_masks(&masks), set);
    }

    #[test]
    fn unknown_indices_ignored() {
        let t = Topology::from_raw(&[raw(0, 0, 0b1)]);
        assert!(t.group_masks(&CpuSet::from_indices([5, 100])).is_empty());
        assert_eq!(t.set_from_group_masks(&[(7, 0xFF)]), CpuSet::new());
    }

    #[test]
    fn zero_masks_are_skipped() {
        let t = Topology::from_raw(&[raw(0, 0, 0), raw(0, 0, 0b1)]);
        assert_eq!(t.core_count(), 1);
        assert_eq!(t.total(), 1);
    }

    #[test]
    fn detect_returns_plausible_topology() {
        let t = Topology::detect().expect("detect должен работать");
        assert!(t.total() >= 1, "должен быть хотя бы один логический CPU");
        assert_eq!(t.cores.iter().map(|c| c.threads.len()).sum::<usize>(), t.total());
        assert_eq!(t.all_set().len(), t.total());
        // Глобальные индексы — сквозные 0..total.
        for (i, cpu) in t.cpus.iter().enumerate() {
            assert_eq!(cpu.index, i);
            assert!(cpu.bit < 64);
        }
    }
}
