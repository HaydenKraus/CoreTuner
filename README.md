# CoreTuner

Управление ядрами (CPU affinity) и приоритетами процессов Windows 10/11. Профили, автоприменение, трей.

## Сборка и запуск
```
cargo build --release          # target\release\coretuner.exe (~11 МБ, без консоли, LTO+strip)
cargo run --release
cargo test                     # 182 теста (для storage-тестов при недоступном %TEMP% задайте CT_TEST_TMP=<каталог>)
cargo clippy --all-targets -- -D warnings
```
Нужны Rust stable (MSVC toolchain). Иконка и манифест встраиваются через `winres` (`assets/icon.ico`, `coretuner.manifest`).

Установщик (Inno Setup 6): собрать `target\release\coretuner.exe`, затем
```
"%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\coretuner.iss   # dist\CoreTuner-1.0.1-setup.exe
```
Установка для текущего пользователя в `%LOCALAPPDATA%\Programs\CoreTuner`, без UAC.
Флаги: `--minimized` (используется автозапуском; учитывает «Запускать свёрнутым»).
Переменные: `CORETUNER_CONFIG_DIR` — другой каталог конфига (портативный режим), `CORETUNER_LOG=info|debug` — лог в stderr.

## Архитектура (SPEC.md — контракты модулей)
| Модуль | Назначение |
|---|---|
| `cpu/topology` | `GetLogicalProcessorInformationEx`: физические ядра, SMT-потоки, EfficiencyClass (P/E), группы >64 |
| `cpu/affinity`, `cpu/priority` | `SetProcessAffinityMask` (1 группа) / CPU Sets (>64), `SetPriorityClass`, IO- и memory-приоритет |
| `process/list` | список процессов через ToolHelp32 + WinAPI (имя, путь, RAM, CPU%, приоритет, маска, окно) |
| `process/monitor` | фоновый поток: новые процессы > профиль, триггеры «при запуске/закрытии», повторное применение |
| `profiles/*` | модель, атомарная запись `%APPDATA%\CoreTuner\config.json`, миграции, импорт/экспорт, сопоставление |
| `ui/*`, `app.rs`, `tray.rs`, `i18n.rs` | egui-интерфейс (RU/EN), трей (tray-icon + muda), строки в таблицах |
| `autostart.rs`, `single.rs` | реестр HKCU\...\Run, именованный mutex + событие «показать окно» |

## Ограничения
- Системные/защищённые процессы (`csrss`, `lsass`, `dwm` и др.) и сам CoreTuner не изменяются; процессы повышенных прав требуют запуска от администратора (кнопка «Перезапустить от администратора»).
- Realtime и High-IO приоритеты требуют прав; антивирусы/EDR могут блокировать `SetProcessAffinityMask` для защищённых процессов.
- Системы >64 потоков: используется CPU Sets API (мягкое ограничение планировщика) — проверено только на синтетических топологиях.
- P/E-ядра определяются по `EfficiencyClass`; на реальном гибридном CPU не тестировалось (проверено на синтетической топологии и визуально).
- Системное уведомление при автоприменении заменено плашкой в окне и журналом событий (tray-icon не поддерживает toast).
