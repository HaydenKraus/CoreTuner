//! Значок в системном трее: меню, события и видимость.
//!
//! События меню (`muda::MenuEvent`) и кликов по значку (`tray_icon::TrayIconEvent`) приходят
//! не в UI-поток, а в поток оболочки/окна трея. Скрытое окно eframe не получает кадров, поэтому
//! обработчики событий пересылают их в собственный crossbeam-канал и будят окно через
//! `egui::Context::request_repaint`; для показа окна дополнительно дёргается WinAPI напрямую.
//!
//! Обработчики у крейтов статические (`set_event_handler` кладёт их в `OnceLock`), поэтому
//! `Tray::new` осмысленно вызывать один раз за процесс — ровно так делает `app.rs`.
//!
//! Меню пересобирается только при изменении списка профилей, языка или паузы.

use crate::config::Lang;
use crate::i18n::t;
use crossbeam_channel::{Receiver, Sender};
use std::sync::OnceLock;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, TrayIconId};

/// Идентификатор значка (стабильный, чтобы пересборка не плодила копии).
const ICON_ID: &str = "coretuner";
/// Заголовок главного окна — по нему окно ищется в WinAPI.
const WINDOW_TITLE: &str = "CoreTuner";
const ID_OPEN: &str = "ct.open";
const ID_PAUSE: &str = "ct.pause";
const ID_EXIT: &str = "ct.exit";
const ID_PROFILES: &str = "ct.profiles";
const ID_PROFILE_PREFIX: &str = "ct.profile:";
const ID_NO_PROFILES: &str = "ct.profiles.none";
/// Размер программной иконки трея, px.
pub const TRAY_ICON_SIZE: u32 = 32;

/// Действие, выбранное пользователем в трее.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    /// Переключить активный профиль на указанный id.
    SwitchProfile(String),
    TogglePause,
    Exit,
}

/// Параметры, по которым собрано меню: профили, язык, пауза.
type MenuKey = (Vec<(String, String)>, Lang, bool);

/// Значок в трее. Если создание не удалось, `Tray` остаётся в состоянии «недоступен»:
/// все методы безопасны, а `poll()` возвращает пустой список.
pub struct Tray {
    icon: Option<TrayIcon>,
    /// Параметры текущего меню (`None` — меню ещё не собиралось).
    menu: Option<MenuKey>,
    visible: bool,
    events: Receiver<TrayEvent>,
}

impl Tray {
    /// Создать значок. `visible` — показывать ли его сразу.
    pub fn new(
        profiles: &[(String, String)],
        lang: Lang,
        paused: bool,
        visible: bool,
        ctx: egui::Context,
    ) -> anyhow::Result<Tray> {
        let (tx, rx) = crossbeam_channel::unbounded::<TrayEvent>();
        install_handlers(SenderSlot::new(tx), ctx.clone());

        // `TrayIconBuilder::with_id` в tray-icon 0.26 принимает два аргумента, поэтому
        // id задаётся отдельным шагом после `new()`.
        let icon = TrayIconBuilder::new()
            .with_id(TrayIconId::new(ICON_ID))
            .with_menu(Box::new(build_menu(profiles, lang, paused)?))
            .with_tooltip(t(lang, "tray.tooltip"))
            .with_icon(icon()?)
            .with_menu_on_left_click(false)
            .build()?;
        icon.set_visible(visible)?;

        Ok(Tray {
            icon: Some(icon),
            menu: Some((profiles.to_vec(), lang, paused)),
            visible,
            events: rx,
        })
    }

    /// Значок недоступен (оболочка не запущена, нет иконки) — приложение работает без трея.
    pub fn unavailable() -> Tray {
        Tray { icon: None, menu: None, visible: false, events: crossbeam_channel::never() }
    }

    pub fn is_available(&self) -> bool {
        self.icon.is_some()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Показать/скрыть значок.
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if let Some(icon) = &self.icon {
            if let Err(e) = icon.set_visible(visible) {
                tracing::warn!("трей: set_visible({visible}): {e}");
            }
        }
    }

    /// Пересобрать меню, если изменились профили, язык или пауза.
    ///
    /// `true` — меню пересобрано; `visible` меню не касается (её меняет `set_visible`).
    pub fn rebuild_menu(
        &mut self,
        profiles: &[(String, String)],
        lang: Lang,
        paused: bool,
        _visible: bool,
    ) -> bool {
        let key = (profiles.to_vec(), lang, paused);
        if self.menu.as_ref() == Some(&key) {
            return false;
        }
        self.menu = Some(key);
        let Some(icon) = &self.icon else {
            return true;
        };
        match build_menu(profiles, lang, paused) {
            Ok(menu) => icon.set_menu(Some(Box::new(menu))),
            Err(e) => tracing::warn!("трей: меню не пересобрано: {e:#}"),
        }
        true
    }

    /// Забрать накопившиеся события трея. Повторы внутри одного опроса схлопываются.
    pub fn poll(&mut self) -> Vec<TrayAction> {
        let mut out: Vec<TrayAction> = Vec::new();
        let mut open = false;
        for ev in self.events.try_iter() {
            match ev {
                TrayEvent::Open => open = true,
                TrayEvent::Menu(id) => {
                    if let Some(action) = action_for(&id) {
                        push_unique(&mut out, action);
                    }
                }
            }
        }
        // Одиночный и двойной клик приходят парой — окно открываем один раз.
        if open {
            out.insert(0, TrayAction::Open);
        }
        out
    }

    /// Всплывающее уведомление: нативное без PowerShell недоступно, уведомление показывает
    /// сам UI (тост в окне и журнал событий). Метод оставлен no-op, чтобы вызывающий код
    /// не ветвился; до подключения в UI помечен как неиспользуемый.
    #[allow(dead_code)]
    pub fn notify(&self, _title: &str, _body: &str) {}
}

/// Событие трея в собственном канале: id пункта меню либо «показать окно».
#[derive(Clone, Debug)]
enum TrayEvent {
    Open,
    Menu(MenuId),
}

/// Отправитель событий для статических обработчиков (устанавливается один раз за процесс).
struct SenderSlot(OnceLock<Sender<TrayEvent>>);

impl SenderSlot {
    fn new(tx: Sender<TrayEvent>) -> Self {
        let slot = SenderSlot(OnceLock::new());
        let _ = slot.0.set(tx);
        slot
    }

    fn send(&self, ev: TrayEvent) {
        if let Some(tx) = self.0.get() {
            let _ = tx.send(ev);
        }
    }
}

static EVENT_TX: OnceLock<SenderSlot> = OnceLock::new();

/// Поставить статические обработчики событий трея и меню (один раз за процесс).
fn install_handlers(tx: SenderSlot, ctx: egui::Context) {
    let slot = EVENT_TX.get_or_init(|| tx);
    let ctx_icon = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
        if opens_window(&ev) {
            slot.send(TrayEvent::Open);
            // Показываем окно сразу: кадр eframe придёт только после пробуждения.
            show_native_window();
            ctx_icon.request_repaint();
        }
    }));
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        slot.send(TrayEvent::Menu(ev.id));
        ctx.request_repaint();
    }));
}

fn push_unique(v: &mut Vec<TrayAction>, a: TrayAction) {
    if !v.contains(&a) {
        v.push(a);
    }
}

/// Разбор id пункта меню в действие.
fn action_for(id: &MenuId) -> Option<TrayAction> {
    let s = id.0.as_str();
    match s {
        ID_OPEN => Some(TrayAction::Open),
        ID_PAUSE => Some(TrayAction::TogglePause),
        ID_EXIT => Some(TrayAction::Exit),
        _ => s.strip_prefix(ID_PROFILE_PREFIX).map(|p| TrayAction::SwitchProfile(p.to_string())),
    }
}

/// Открывать окно: одиночный клик левой кнопкой (WM_LBUTTONUP) и двойной клик.
fn opens_window(ev: &TrayIconEvent) -> bool {
    match ev {
        TrayIconEvent::Click { button, button_state, .. } => {
            *button == MouseButton::Left && *button_state == MouseButtonState::Up
        }
        TrayIconEvent::DoubleClick { button, .. } => *button == MouseButton::Left,
        _ => false,
    }
}

/// Показать и поднять главное окно напрямую через WinAPI.
///
/// Скрытое окно eframe не обрабатывает `ViewportCommand` до следующего кадра, поэтому
/// показываем его синхронно из обработчика события трея.
pub fn show_native_window() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    // SAFETY: вызовы WinAPI без разыменования указателей; при отсутствии окна FindWindowW
    // возвращает ошибку, которую обрабатывает `ok()`. Макрос `w!` принимает только литерал.
    unsafe {
        match FindWindowW(None, w!("CoreTuner")).ok() {
            Some(hwnd) => {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            None => tracing::debug!("трей: окно «{WINDOW_TITLE}» не найдено"),
        }
    }
}

/// Собрать меню: «Открыть», «Профили», пауза, разделитель, «Выход».
fn build_menu(profiles: &[(String, String)], lang: Lang, paused: bool) -> anyhow::Result<Menu> {
    let menu = Menu::new();
    menu.append(&MenuItem::with_id(ID_OPEN, t(lang, "tray.open"), true, None))?;

    let submenu = Submenu::with_id(ID_PROFILES, t(lang, "tray.profiles"), true);
    if profiles.is_empty() {
        submenu.append(&MenuItem::with_id(ID_NO_PROFILES, t(lang, "tray.no_profiles"), false, None))?;
    } else {
        for (id, name) in profiles {
            let item = CheckMenuItem::with_id(profile_menu_id(id), name, true, false, None);
            submenu.append(&item)?;
        }
    }
    menu.append(&submenu)?;

    menu.append(&CheckMenuItem::with_id(ID_PAUSE, t(lang, "tray.pause"), true, paused, None))?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(ID_EXIT, t(lang, "tray.exit"), true, None))?;
    Ok(menu)
}

/// Id пункта меню для профиля.
fn profile_menu_id(profile_id: &str) -> String {
    format!("{ID_PROFILE_PREFIX}{profile_id}")
}

/// Иконка трея: 32×32 RGBA, генерируется программно.
fn icon() -> anyhow::Result<Icon> {
    let rgba = crate::ui::icon_rgba(TRAY_ICON_SIZE);
    let expected = (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize;
    anyhow::ensure!(
        rgba.len() == expected,
        "трей: иконка {TRAY_ICON_SIZE}×{TRAY_ICON_SIZE} имеет {} байт вместо {expected}",
        rgba.len()
    );
    Ok(Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu_event(id: &str) -> TrayEvent {
        TrayEvent::Menu(MenuId::new(id))
    }

    /// Разбор id пункта меню в действие — чистая функция, проверяется без трея.
    #[test]
    fn action_ids_parse() {
        assert_eq!(action_for(&MenuId::new(ID_OPEN)), Some(TrayAction::Open));
        assert_eq!(action_for(&MenuId::new(ID_PAUSE)), Some(TrayAction::TogglePause));
        assert_eq!(action_for(&MenuId::new(ID_EXIT)), Some(TrayAction::Exit));
        assert_eq!(action_for(&MenuId::new("ct.profile:abc")), Some(TrayAction::SwitchProfile("abc".into())));
        assert_eq!(action_for(&MenuId::new(ID_NO_PROFILES)), None);
        assert_eq!(action_for(&MenuId::new(ID_PROFILES)), None);
        assert_eq!(action_for(&MenuId::new("ct.unknown")), None);
    }

    #[test]
    fn profile_ids_round_trip() {
        assert_eq!(profile_menu_id("game"), "ct.profile:game");
        assert_eq!(action_for(&MenuId::new(profile_menu_id("game"))), Some(TrayAction::SwitchProfile("game".into())));
        assert_eq!(profile_menu_id(""), "ct.profile:");
        // Id профиля с двоеточием не ломает разбор: префикс срезается один раз.
        assert_eq!(action_for(&MenuId::new(profile_menu_id("a:b"))), Some(TrayAction::SwitchProfile("a:b".into())));
    }

    #[test]
    fn left_click_opens_right_does_not() {
        let click = |button| TrayIconEvent::Click {
            id: TrayIconId::new(ICON_ID),
            position: tray_icon::dpi::PhysicalPosition::new(0.0, 0.0),
            rect: tray_icon::Rect::default(),
            button,
            button_state: MouseButtonState::Up,
        };
        assert!(opens_window(&click(MouseButton::Left)));
        assert!(!opens_window(&click(MouseButton::Right)));
        assert!(opens_window(&TrayIconEvent::DoubleClick {
            id: TrayIconId::new(ICON_ID),
            position: tray_icon::dpi::PhysicalPosition::new(0.0, 0.0),
            rect: tray_icon::Rect::default(),
            button: MouseButton::Left,
        }));
        assert!(!opens_window(&TrayIconEvent::Enter {
            id: TrayIconId::new(ICON_ID),
            position: tray_icon::dpi::PhysicalPosition::new(0.0, 0.0),
            rect: tray_icon::Rect::default(),
        }));
    }

    #[test]
    fn poll_deduplicates() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut tray = Tray { icon: None, menu: None, visible: true, events: rx };
        for id in [ID_PAUSE, ID_PAUSE, "ct.profile:a", "ct.profile:a"] {
            let _ = tx.send(menu_event(id));
        }
        let _ = tx.send(TrayEvent::Open);
        let _ = tx.send(TrayEvent::Open);
        assert_eq!(
            tray.poll(),
            vec![
                TrayAction::Open,
                TrayAction::TogglePause,
                TrayAction::SwitchProfile("a".into()),
            ]
        );
        // Второй опрос пуст — события забраны.
        assert!(tray.poll().is_empty());
    }

    #[test]
    fn unavailable_is_safe() {
        let mut tray = Tray::unavailable();
        assert!(!tray.is_available());
        assert!(!tray.is_visible());
        assert!(tray.poll().is_empty());
        assert!(tray.rebuild_menu(&[("a".into(), "Игра".into())], Lang::Ru, false, true));
        assert!(!tray.rebuild_menu(&[("a".into(), "Игра".into())], Lang::Ru, false, true));
        assert!(tray.rebuild_menu(&[("a".into(), "Игра".into())], Lang::En, false, true));
        assert!(tray.rebuild_menu(&[("a".into(), "Игра".into())], Lang::En, true, true));
        assert!(!tray.rebuild_menu(&[("a".into(), "Игра".into())], Lang::En, true, false));
        assert!(!tray.is_available());
        tray.set_visible(true);
        assert!(tray.is_visible());
        tray.notify("t", "b");
    }

    #[test]
    fn icon_is_32px_rgba() {
        let rgba = crate::ui::icon_rgba(TRAY_ICON_SIZE);
        assert_eq!(rgba.len(), (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize);
    }
}
