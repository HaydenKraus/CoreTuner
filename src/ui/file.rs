//! Вкладка «По файлу запуска»: привязка профилей к исполняемому файлу.
//!
//! Сверху — форма: зона drag-and-drop, выбор .exe, поле ручного ввода имени, режим
//! сопоставления (имя/полный путь) и профили привязки. Ниже — карточки существующих привязок
//! с активным профилем, переключателем и редактором триггеров («когда запустится X — включить
//! профиль A, когда закроется — профиль B»).
//!
//! Чистая логика (нормализация цели, валидация формы, создание/обновление привязки) вынесена в
//! свободные функции и покрыта тестами — UI вызывает их и лишь применяет результат.

use crate::app::CoreTunerApp;
use crate::config::Config;
use crate::i18n::{t, tf};
use crate::model::{new_id, AppBinding, MatchMode, Trigger};
use egui::{
    Color32, ComboBox, CornerRadius, Frame, Margin, RichText, Sense, Stroke, TextEdit, Ui, Vec2,
};

/// Строки вкладки, которых нет в общей таблице i18n.
pub const STR: &[(&str, &str, &str)] = &[
    ("file.manual_hint", "например, game.exe", "e.g. game.exe"),
    ("file.dropped", "Выбран файл: {name}", "File selected: {name}"),
    ("file.need_target_full", "Укажите путь к .exe или имя файла", "Provide an .exe path or file name"),
    ("file.need_profile_full", "Отметьте хотя бы один профиль для привязки", "Select at least one profile to bind"),
    ("file.default_active", "По умолчанию", "Default"),
    ("file.edit", "Редактировать", "Edit"),
    ("file.editing", "Редактирование", "Editing"),
    ("file.trigger_add", "Добавить триггер", "Add trigger"),
    ("file.trigger_watch", "Приложение-наблюдатель", "Watched application"),
    ("file.trigger_on_start", "Когда запустится", "When it starts"),
    ("file.trigger_on_exit", "Когда закроется", "When it closes"),
    ("file.trigger_need_watch", "Заполните приложение-наблюдатель в триггере", "Fill in the watched application of the trigger"),
];

/// Ширина ComboBox активного профиля в карточке.
const ACTIVE_W: f32 = 190.0;
/// Ширина поля ввода в триггере.
const WATCH_W: f32 = 200.0;

/// Состояние вкладки: черновик формы и раскрытые редакторы триггеров.
#[derive(Default)]
pub struct State {
    target: String,
    mode: MatchMode,
    profile_ids: Vec<String>,
    active_profile: Option<String>,
    triggers: Vec<Trigger>,
    /// id привязки, загруженной в форму (`None` — создаётся новая).
    edit_id: Option<String>,
    /// Развёрнутые редакторы триггеров (в свёрнутом виде — только сводка).
    open: Vec<usize>,
}

impl State {
    /// Принять путь, выбранный в диалоге или перетащенный в окно.
    /// Путь задаёт полную цель и включает режим сопоставления по пути.
    pub fn set_target(&mut self, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        self.target = path.to_string();
        if path.contains('\\') || path.contains('/') {
            self.mode = MatchMode::Path;
        }
    }

    /// Загрузить привязку в форму для редактирования.
    fn load(&mut self, b: &AppBinding) {
        self.edit_id = Some(b.id.clone());
        self.target = b.target.clone();
        self.mode = b.mode;
        self.profile_ids = b.profile_ids.clone();
        self.active_profile = b.active_profile.clone().filter(|id| self.profile_ids.contains(id));
        self.triggers = b.triggers.clone();
        self.open.clear();
    }

    /// Очистить форму и выйти из режима редактирования.
    fn reset(&mut self) {
        let keep_mode = self.mode;
        *self = State { mode: keep_mode, ..State::default() };
    }
}

/// Результат применения формы к конфигу.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpsertResult {
    /// Привязка создана.
    Created,
    /// Существующая привязка обновлена.
    Updated,
    /// Форма заполнена неверно.
    Invalid(FormError),
}

/// Что именно не так в форме.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormError {
    /// Пустая цель.
    EmptyTarget,
    /// Ни один профиль не выбран.
    NoProfile,
    /// У триггера не заполнено наблюдаемое приложение.
    TriggerWatch,
}

impl FormError {
    /// Ключ локализации сообщения об ошибке.
    pub fn key(self) -> &'static str {
        match self {
            FormError::EmptyTarget => "file.need_target_full",
            FormError::NoProfile => "file.need_profile_full",
            FormError::TriggerWatch => "file.trigger_need_watch",
        }
    }
}

/// Черновик привязки из формы: единственное, что нужно для [`upsert_binding`].
#[derive(Clone, Debug, Default)]
pub struct Form {
    /// id редактируемой привязки; `None` — будет создана новая.
    pub id: Option<String>,
    pub target: String,
    pub mode: MatchMode,
    pub profile_ids: Vec<String>,
    pub active_profile: Option<String>,
    pub triggers: Vec<Trigger>,
}

impl State {
    /// Снимок формы для чистой функции [`upsert_binding`].
    pub fn form(&self) -> Form {
        Form {
            id: self.edit_id.clone(),
            target: self.target.clone(),
            mode: self.mode,
            profile_ids: self.profile_ids.clone(),
            active_profile: self.active_profile.clone(),
            triggers: self.triggers.clone(),
        }
    }
}

/// Цель привязки в виде, в котором её сравнивает монитор (`matcher.rs`):
/// без префикса `\\?\`, с обратными слэшами и в нижнем регистре.
/// В режиме [`MatchMode::Name`] — только имя файла.
pub fn normalize_target(path: &str, mode: MatchMode) -> String {
    let mut s = path.trim().trim_matches('"').trim();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest;
    }
    s = s.trim_end_matches(['\\', '/']);
    let name = match mode {
        MatchMode::Path => s,
        MatchMode::Name => s.rsplit(['\\', '/']).next().unwrap_or(s),
    };
    name.trim().replace('/', "\\").to_lowercase()
}

/// Индекс привязки с той же целью и режимом (без учёта регистра), исключая `skip_id`.
fn find_duplicate(cfg: &Config, mode: MatchMode, target: &str, skip_id: Option<&str>) -> Option<usize> {
    let key = normalize_target(target, mode);
    cfg.bindings.iter().position(|b| {
        b.mode == mode
            && Some(b.id.as_str()) != skip_id
            && normalize_target(&b.target, b.mode) == key
    })
}

/// Проверка формы: цель не пуста, профиль выбран, у заполняемого триггера указано
/// наблюдаемое приложение. Полностью пустые строки триггеров не мешают сохранению —
/// [`upsert_binding`] просто их отбрасывает.
pub fn validate_form(form: &Form) -> Result<(), FormError> {
    if normalize_target(&form.target, form.mode).is_empty() {
        return Err(FormError::EmptyTarget);
    }
    if form.profile_ids.is_empty() {
        return Err(FormError::NoProfile);
    }
    // Пустой триггер целиком (наблюдатель и оба профиля) — заготовка строки, а не ошибка.
    // Ошибка только если у триггера выбран профиль, но не указано наблюдаемое приложение.
    if form
        .triggers
        .iter()
        .any(|t| (t.on_start.is_some() || t.on_exit.is_some()) && t.watch.trim().is_empty())
    {
        return Err(FormError::TriggerWatch);
    }
    Ok(())
}

/// Создать или обновить привязку. Дубликат (тот же режим и цель без учёта регистра)
/// обновляется вместо создания второй записи; редактируемая привязка дубликатом не считается
/// и меняет цель/режим на месте. Возвращает, что именно произошло с конфигом.
pub fn upsert_binding(cfg: &mut Config, form: &Form) -> UpsertResult {
    if let Err(e) = validate_form(form) {
        return UpsertResult::Invalid(e);
    }
    let target = form.target.trim().to_string();
    let profile_ids: Vec<String> = {
        let mut ids = form.profile_ids.clone();
        ids.dedup();
        ids
    };
    if profile_ids.is_empty() {
        return UpsertResult::Invalid(FormError::NoProfile);
    }
    // Активный профиль обязан входить в список профилей привязки.
    let active = form.active_profile.clone().filter(|id| profile_ids.contains(id));
    let triggers: Vec<Trigger> = form
        .triggers
        .iter()
        .filter(|t| !t.watch.trim().is_empty())
        .cloned()
        .collect();

    if let Some(id) = form.id.as_deref() {
        if let Some(b) = cfg.bindings.iter_mut().find(|b| b.id == id) {
            b.target = target;
            b.mode = form.mode;
            b.profile_ids = profile_ids;
            b.active_profile = active;
            b.triggers = triggers;
            return UpsertResult::Updated;
        }
    }

    match find_duplicate(cfg, form.mode, &target, form.id.as_deref()) {
        Some(i) => {
            let b = &mut cfg.bindings[i];
            b.profile_ids = profile_ids;
            b.active_profile = active;
            b.triggers = triggers;
            UpsertResult::Updated
        }
        None => {
            cfg.bindings.push(AppBinding {
                id: new_id("bind"),
                target,
                mode: form.mode,
                enabled: true,
                profile_ids,
                active_profile: active,
                triggers,
            });
            UpsertResult::Created
        }
    }
}

/// Профили, которых уже нет в конфиге (удалены на вкладке «Профили»).
fn missing_profiles(app: &CoreTunerApp, st: &State) -> Vec<String> {
    st.profile_ids.iter().filter(|id| app.cfg.profile(id).is_none()).cloned().collect()
}

/// Первое перетащенное значение, похожее на .exe (расширение без учёта регистра).
fn dropped_exe(ui: &Ui) -> Option<String> {
    ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().display().to_string())
            .find(|p| p.to_lowercase().ends_with(".exe"))
    })
}

/// Курсор над зоной перетаскивания.
fn hovering_files(ui: &Ui) -> bool {
    ui.ctx().input(|i| !i.raw.hovered_files.is_empty())
}

/// Зона drag-and-drop: подсказка, кнопка выбора файла и поле ручного ввода.
fn drop_zone(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State, hovered: bool) {
    let lang = app.lang();
    let frame = Frame::new()
        .inner_margin(Margin::same(10))
        .corner_radius(CornerRadius::same(8))
        .fill(drop_fill(ui, hovered))
        .stroke(Stroke::new(if hovered { 2.0 } else { 1.0 }, drop_stroke(ui, hovered)));

    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(t(lang, "file.drop_hint")).strong());
            ui.label(RichText::new(t(lang, "file.hint")).weak().size(12.0));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let total = ui.available_width();
                let pick_w = 150.0;
                let button = egui::Button::new(t(lang, "file.pick")).min_size(Vec2::new(pick_w, 26.0));
                let name_w = (total - pick_w - ui.spacing().item_spacing.x - 60.0).max(120.0);
                ui.add_space(((total - pick_w - name_w - 60.0) / 2.0).max(0.0));
                if ui.add(button).clicked() {
                    app.pick_exe();
                }
                ui.add(
                    TextEdit::singleline(&mut st.target)
                        .hint_text(t(lang, "file.manual_hint"))
                        .desired_width(name_w),
                );
                ui.label(RichText::new(t(lang, "file.manual_name")).weak().size(11.0));
            });
        });
    });
    if inner.response.interact(Sense::hover()).hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
    }
}

/// Заливка зоны: подсветка при перетаскивании файла в окно.
fn drop_fill(ui: &Ui, hovered: bool) -> Color32 {
    let v = ui.visuals();
    if hovered {
        v.selection.bg_fill.gamma_multiply(0.30)
    } else {
        v.widgets.inactive.weak_bg_fill
    }
}

/// Рамка зоны: сплошная обычная, акцентная при перетаскивании.
fn drop_stroke(ui: &Ui, hovered: bool) -> Color32 {
    let v = ui.visuals();
    if hovered {
        v.selection.bg_fill
    } else {
        v.widgets.inactive.bg_stroke.color
    }
}

/// Выбор режима сопоставления с подсказкой.
fn mode_row(ui: &mut Ui, app: &CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "file.mode")).strong());
        ui.radio_value(&mut st.mode, MatchMode::Name, t(lang, "file.mode.name"))
            .on_hover_text(t(lang, "file.mode.name_hint"));
        ui.radio_value(&mut st.mode, MatchMode::Path, t(lang, "file.mode.path"))
            .on_hover_text(t(lang, "file.mode.path_hint"));
    });
}

/// Список профилей для привязки одной строкой: чекбоксы и выбор активного.
fn profiles_row(ui: &mut Ui, app: &CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t(lang, "file.choose_profile")).strong());
        if app.cfg.profiles.is_empty() {
            ui.label(RichText::new(t(lang, "file.need_profiles")).color(ui.visuals().warn_fg_color));
            return;
        }
        let mut marked: Vec<bool> = st
            .profile_ids
            .iter()
            .map(|id| app.cfg.profiles.iter().any(|p| &p.id == id))
            .collect();
        for (i, p) in app.cfg.profiles.iter().enumerate() {
            if marked.get(i).copied().unwrap_or(false) {
                continue;
            }
            let mut on = false;
            if ui.checkbox(&mut on, &p.name).changed() && on {
                st.profile_ids.push(p.id.clone());
                if st.active_profile.is_none() {
                    st.active_profile = Some(p.id.clone());
                }
            }
        }
        for (i, p) in app.cfg.profiles.iter().enumerate() {
            if !marked.get(i).copied().unwrap_or(false) {
                continue;
            }
            let mut on = true;
            if ui.checkbox(&mut on, &p.name).changed() && !on {
                st.profile_ids.retain(|id| id != &p.id);
                if st.active_profile.as_deref() == Some(p.id.as_str()) {
                    st.active_profile = None;
                }
            }
        }
        marked.clear();
    });
    if !st.profile_ids.is_empty() {
        let missing = missing_profiles(app, st);
        if !missing.is_empty() {
            ui.label(
                RichText::new(missing.join(", "))
                    .color(ui.visuals().error_fg_color)
                    .size(11.0),
            );
        }
    }
}

/// Редактор одного триггера: наблюдатель, режим и профили на запуск/закрытие.
fn trigger_editor(ui: &mut Ui, app: &CoreTunerApp, trigger: &mut Trigger) {
    let lang = app.lang();
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "file.trigger_watch")).weak().size(12.0));
        ui.add(
            TextEdit::singleline(&mut trigger.watch)
                .hint_text(t(lang, "file.manual_hint"))
                .desired_width(WATCH_W),
        );
        ui.radio_value(&mut trigger.watch_mode, MatchMode::Name, t(lang, "file.mode.name"))
            .on_hover_text(t(lang, "file.mode.name_hint"));
        ui.radio_value(&mut trigger.watch_mode, MatchMode::Path, t(lang, "file.mode.path"))
            .on_hover_text(t(lang, "file.mode.path_hint"));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t(lang, "file.trigger_on_start")).weak().size(12.0));
        profile_combo(ui, app, ui.scope_id().with("trig_start"), &mut trigger.on_start);
        ui.add_space(8.0);
        ui.label(RichText::new(t(lang, "file.trigger_on_exit")).weak().size(12.0));
        profile_combo(ui, app, ui.scope_id().with("trig_exit"), &mut trigger.on_exit);
    });
}

/// ComboBox профиля: «не менять» (`None`) плюс список профилей конфига.
fn profile_combo(ui: &mut Ui, app: &CoreTunerApp, salt: egui::Id, value: &mut Option<String>) {
    let lang = app.lang();
    let selected = match value.as_deref() {
        Some(id) => app.profile_name(id),
        None => t(lang, "common.none").to_string(),
    };
    ComboBox::from_id_salt(salt)
        .selected_text(selected)
        .width(ACTIVE_W)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(value.is_none(), RichText::new(t(lang, "common.keep")).weak())
                .clicked()
            {
                *value = None;
            }
            for p in &app.cfg.profiles {
                let on = value.as_deref() == Some(p.id.as_str());
                if ui.selectable_label(on, &p.name).clicked() {
                    *value = Some(p.id.clone());
                }
            }
        });
}

/// Редактор триггеров формы: сводка по каждому, кнопки добавления и удаления.
fn triggers_section(ui: &mut Ui, app: &CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "file.triggers")).strong())
            .on_hover_text(t(lang, "file.triggers"));
        ui.label(
            RichText::new(tf(lang, "file.triggers_count", &[("n", &st.triggers.len().to_string())]))
                .weak()
                .size(11.0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t(lang, "file.trigger_add")).clicked() {
                st.triggers.push(Trigger {
                    watch: String::new(),
                    watch_mode: MatchMode::Name,
                    on_start: None,
                    on_exit: None,
                });
                st.open.push(st.triggers.len() - 1);
            }
        });
    });
    if st.triggers.is_empty() {
        return;
    }
    ui.label(RichText::new(trigger_summary(lang, &st.triggers, &|id| app.profile_name(id))).weak().size(11.0));

    let mut remove: Option<usize> = None;
    egui::ScrollArea::vertical()
        .id_salt("ct_file_triggers")
        .max_height(170.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for i in 0..st.triggers.len() {
                ui.push_id(("ct_trigger", i), |ui| {
                    let open = st.open.contains(&i);
                    let head = {
                        let tg = &st.triggers[i];
                        let watch = if tg.watch.trim().is_empty() {
                            t(lang, "common.none").to_string()
                        } else {
                            tg.watch.clone()
                        };
                        let mode = t(lang, mode_key(tg.watch_mode));
                        format!("{watch}  ·  {mode}")
                    };
                    ui.horizontal(|ui| {
                        let arrow = if open { "⏷" } else { "⏵" };
                        if ui.small_button(arrow).clicked() {
                            if open {
                                st.open.retain(|&x| x != i);
                            } else {
                                st.open.push(i);
                            }
                        }
                        ui.label(RichText::new(head).size(12.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(t(lang, "common.delete")).clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                    if open {
                        ui.indent(("ct_trigger_body", i), |ui| {
                            if let Some(tg) = st.triggers.get_mut(i) {
                                trigger_editor(ui, app, tg);
                            }
                        });
                    }
                });
            }
        });
    if let Some(i) = remove {
        if i < st.triggers.len() {
            st.triggers.remove(i);
        }
        st.open.retain(|&x| x != i);
        for x in &mut st.open {
            if *x > i {
                *x -= 1;
            }
        }
    }
}

/// Сводка триггеров: «имя: A → B» по каждой записи. `name_of` переводит id профиля в имя.
fn trigger_summary(
    lang: crate::config::Lang,
    triggers: &[Trigger],
    name_of: &dyn Fn(&str) -> String,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for tg in triggers {
        if tg.watch.trim().is_empty() {
            continue;
        }
        let side = |id: &Option<String>| match id.as_deref() {
            Some(id) => name_of(id),
            None => t(lang, "common.keep").to_string(),
        };
        parts.push(format!("{}: {} → {}", tg.watch.trim(), side(&tg.on_start), side(&tg.on_exit)));
    }
    parts.join("; ")
}

/// Ключ локализации названия режима сопоставления.
fn mode_key(mode: MatchMode) -> &'static str {
    match mode {
        MatchMode::Name => "file.mode.name",
        MatchMode::Path => "file.mode.path",
    }
}

/// Форма целиком: зона перетаскивания, режим, профили, триггеры и кнопка сохранения.
fn form_ui(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    ui.label(RichText::new(t(app.lang(), "file.title")).strong());
    ui.label(RichText::new(t(app.lang(), "file.hint")).weak().size(12.0));
    ui.add_space(4.0);
    let hovered = hovering_files(ui);
    drop_zone(ui, app, st, hovered);
    ui.add_space(4.0);
    mode_row(ui, app, st);
    profiles_row(ui, app, st);
    triggers_section(ui, app, st);

    ui.separator();
    ui.horizontal(|ui| {
        let lang = app.lang();
        let valid = missing_profiles(app, st).is_empty();
        let label = if st.edit_id.is_some() {
            t(lang, "file.update_binding")
        } else {
            t(lang, "file.create_binding")
        };
        let create = ui.add_enabled(valid, egui::Button::new(RichText::new(label).strong()));
        if create.clicked() {
            commit(app, st);
        }
        if st.edit_id.is_some() && ui.button(t(lang, "common.cancel")).clicked() {
            st.reset();
        }
        let forgotten = missing_profiles(app, st);
        if !forgotten.is_empty() {
            ui.label(RichText::new(t(lang, "file.need_profiles")).color(ui.visuals().warn_fg_color));
        }
    });
}

/// Применить форму к конфигу и сообщить результат пользователю.
fn commit(app: &mut CoreTunerApp, st: &mut State) {
    let form = st.form();
    match upsert_binding(&mut app.cfg, &form) {
        UpsertResult::Created => {
            app.mark_dirty();
            app.sync_tray();
            let msg = app.text("file.binding_added").to_string();
            app.toast(msg, false);
            st.reset();
        }
        UpsertResult::Updated => {
            app.mark_dirty();
            app.sync_tray();
            let msg = app.text("file.binding_updated").to_string();
            app.toast(msg, false);
            st.reset();
        }
        UpsertResult::Invalid(e) => {
            let msg = app.text(e.key()).to_string();
            app.toast(msg, true);
        }
    }
}

/// Переключатель «включена» рядом с именем привязки.
fn card_enabled(ui: &mut Ui, binding: &mut AppBinding) -> bool {
    let mut on = binding.enabled;
    let changed = ui.checkbox(&mut on, "").changed();
    binding.enabled = on;
    changed
}

/// ComboBox активного профиля привязки (среди её профилей, с вариантом «не назначен»).
/// `profiles` — уже разрешённые пары (id, имя) из `binding.profile_ids`.
fn card_active(
    ui: &mut Ui,
    lang: crate::config::Lang,
    binding: &mut AppBinding,
    profiles: &[(String, String)],
) -> bool {
    let selected = match binding.active_profile.as_deref() {
        Some(id) => profiles
            .iter()
            .find(|(pid, _)| pid == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| id.to_string()),
        None => t(lang, "file.none_profile").to_string(),
    };
    let mut changed = false;
    ComboBox::from_id_salt(("ct_bind_active", binding.id.as_str()))
        .selected_text(selected)
        .width(ACTIVE_W)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(binding.active_profile.is_none(), RichText::new(t(lang, "common.none")).weak())
                .clicked()
            {
                binding.active_profile = None;
                changed = true;
            }
            for (id, name) in profiles {
                let on = binding.active_profile.as_deref() == Some(id.as_str());
                if ui.selectable_label(on, name).clicked() {
                    binding.active_profile = Some(id.clone());
                    changed = true;
                }
            }
        });
    changed
}

/// Карточка одной привязки. Возвращает действие, выбранное пользователем.
/// Правки полей карточки пишутся прямо в `cfg`; [`bindings_list`] сразу отмечает конфиг изменённым.
fn card(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State, index: usize) -> CardAction {
    let lang = app.lang();
    let mut action = CardAction::None;
    let total = app.cfg.bindings.len();
    // Имена профилей разрешаются заранее: изменять привязку и читать `app` одновременно нельзя.
    let assigned: Vec<(String, String)> = app
        .cfg
        .bindings
        .get(index)
        .map(|b| b.profile_ids.iter().map(|id| (id.clone(), app.profile_name(id))).collect())
        .unwrap_or_default();

    Frame::group(ui.style())
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let name = app
                .cfg
                .bindings
                .get(index)
                .map(|b| b.display_name())
                .unwrap_or_default();
            let mut chosen = CardAction::None;
            ui.horizontal(|ui| {
                if let Some(b) = app.cfg.bindings.get_mut(index) {
                    card_enabled(ui, b);
                }
                ui.label(RichText::new(name).strong());
                let mode = app.cfg.bindings.get(index).map(|b| b.mode).unwrap_or_default();
                ui.label(RichText::new(t(lang, mode_key(mode))).weak().size(11.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let remove = egui::Button::new(t(lang, "file.remove_binding"))
                        .fill(ui.visuals().error_fg_color.gamma_multiply(0.25));
                    if ui.add(remove).clicked() {
                        chosen = CardAction::Delete;
                    }
                    if ui.button(t(lang, "file.edit")).clicked() {
                        chosen = CardAction::Edit;
                    }
                    if ui.add_enabled(index + 1 < total, egui::Button::new("⏷")).clicked() {
                        chosen = CardAction::MoveDown(true);
                    }
                    if ui.add_enabled(index > 0, egui::Button::new("⏶")).clicked() {
                        chosen = CardAction::MoveUp(true);
                    }
                });
            });
            if chosen != CardAction::None {
                action = chosen;
            }
            if let Some(b) = app.cfg.bindings.get(index) {
                ui.label(RichText::new(&b.target).monospace().size(11.0));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{}:", t(lang, "file.active"))).weak().size(12.0));
                if let Some(b) = app.cfg.bindings.get_mut(index) {
                    card_active(ui, lang, b, &assigned);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{}:", t(lang, "file.choose_profile"))).weak().size(12.0));
                let mut removal: Option<String> = None;
                for (id, pname) in &assigned {
                    let mut on = true;
                    if ui.checkbox(&mut on, pname).changed() && !on {
                        removal = Some(id.clone());
                    }
                }
                if let Some(id) = removal {
                    if let Some(b) = app.cfg.bindings.get_mut(index) {
                        b.profile_ids.retain(|x| x != &id);
                        if b.active_profile.as_deref() == Some(id.as_str()) {
                            b.active_profile = None;
                        }
                    }
                }
                let mut add: Option<String> = None;
                ComboBox::from_id_salt(("ct_bind_add", index))
                    .selected_text(t(lang, "common.add"))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        let available: Vec<(String, String)> = app
                            .cfg
                            .profiles
                            .iter()
                            .filter(|p| {
                                !app.cfg
                                    .bindings
                                    .get(index)
                                    .is_some_and(|b| b.profile_ids.iter().any(|x| x == &p.id))
                            })
                            .map(|p| (p.id.clone(), p.name.clone()))
                            .collect();
                        for (id, pname) in available {
                            if ui.selectable_label(false, pname).clicked() {
                                add = Some(id);
                            }
                        }
                    });
                if let Some(id) = add {
                    if let Some(b) = app.cfg.bindings.get_mut(index) {
                        b.profile_ids.push(id);
                    }
                }
            });

            let triggers = app.cfg.bindings.get(index).map(|b| b.triggers.clone()).unwrap_or_default();
            let count = tf(lang, "file.triggers_count", &[("n", &triggers.len().to_string())]);
            ui.label(RichText::new(format!("{}: {count}", t(lang, "file.triggers"))).weak().size(11.0))
                .on_hover_text(t(lang, "file.triggers"));
            if !triggers.is_empty() {
                let summary = trigger_summary(lang, &triggers, &|id| app.profile_name(id));
                ui.label(RichText::new(summary).weak().size(11.0));
            }
            if st.edit_id.as_deref() == app.cfg.bindings.get(index).map(|b| b.id.as_str()) {
                ui.label(
                    RichText::new(t(lang, "file.editing"))
                        .color(ui.visuals().selection.bg_fill)
                        .size(11.0),
                );
            }
        });

    action
}

/// Действие, выбранное в карточке привязки.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum CardAction {
    #[default]
    None,
    Edit,
    Delete,
    MoveUp(bool),
    MoveDown(bool),
}

/// Список существующих привязок.
fn bindings_list(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    let lang = app.lang();
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new(t(lang, "file.bindings")).strong());
        ui.label(
            RichText::new(tf(lang, "file.applications", &[("n", &app.cfg.bindings.len().to_string())]))
                .weak()
                .size(11.0),
        );
    });
    if app.cfg.bindings.is_empty() {
        ui.label(RichText::new(t(lang, "file.no_bindings")).weak());
        return;
    }

    let mut delete: Option<usize> = None;
    let mut move_by: Option<(usize, isize)> = None;
    // Привязки читаются по индексу, поэтому правки полей карточки меняют `cfg` на месте.
    let cfg_before = app.cfg.bindings.clone();
    egui::ScrollArea::vertical()
        .id_salt("ct_file_bindings")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for i in 0..app.cfg.bindings.len() {
                match card(ui, app, st, i) {
                    CardAction::Edit => {
                        if let Some(b) = app.cfg.bindings.get(i) {
                            let b = b.clone();
                            st.load(&b);
                        }
                    }
                    CardAction::Delete => delete = Some(i),
                    CardAction::MoveUp(_) => move_by = Some((i, -1)),
                    CardAction::MoveDown(_) => move_by = Some((i, 1)),
                    CardAction::None => {}
                }
            }
        });
    if app.cfg.bindings != cfg_before {
        app.mark_dirty();
    }

    if let Some((i, delta)) = move_by {
        let j = i as isize + delta;
        if j >= 0 && (j as usize) < app.cfg.bindings.len() {
            app.cfg.bindings.swap(i, j as usize);
            app.mark_dirty();
        }
    }
    if let Some(i) = delete {
        app.cfg.bindings.remove(i);
        app.mark_dirty();
        app.sync_tray();
        let msg = app.text("file.binding_removed").to_string();
        app.toast(msg, false);
    }
}

/// Вкладка «По файлу запуска».
pub fn show(ui: &mut Ui, app: &mut CoreTunerApp, st: &mut State) {
    // Файл, перетащенный в окно, важнее остальных событий кадра.
    if let Some(path) = dropped_exe(ui) {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path.as_str()).to_string();
        st.set_target(&path);
        let msg = app.fmt("file.dropped", &[("name", &name)]);
        app.toast(msg, false);
    }

    let hovered = hovering_files(ui);
    egui::ScrollArea::vertical()
        .id_salt("ct_file_tab")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            form_ui(ui, app, st);
            ui.add_space(6.0);
            ui.separator();
            if !hovered {
                bindings_list(ui, app, st);
            } else {
                ui.label(RichText::new(t(app.lang(), "file.drop_hint")).weak());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Profile;
    use std::collections::HashSet;

    fn profile(id: &str, name: &str) -> Profile {
        Profile::new(id.to_string(), name)
    }

    fn cfg_with(profiles: &[(&str, &str)], bindings: Vec<AppBinding>) -> Config {
        Config {
            profiles: profiles.iter().map(|(id, n)| profile(id, n)).collect(),
            bindings,
            ..Config::default()
        }
    }

    fn form(target: &str, mode: MatchMode, ids: &[&str]) -> Form {
        Form {
            id: None,
            target: target.to_string(),
            mode,
            profile_ids: ids.iter().map(|s| s.to_string()).collect(),
            active_profile: ids.first().map(|s| s.to_string()),
            triggers: Vec::new(),
        }
    }

    #[test]
    fn normalize_target_path_and_name() {
        assert_eq!(normalize_target(r"  C:\Games\Game.EXE  ", MatchMode::Path), r"c:\games\game.exe");
        assert_eq!(normalize_target("C:/Games/Game.exe", MatchMode::Path), r"c:\games\game.exe");
        assert_eq!(normalize_target(r"\\?\C:\Games\Game.exe", MatchMode::Path), r"c:\games\game.exe");
        assert_eq!(normalize_target(r"C:\Games\Game.exe", MatchMode::Name), "game.exe");
        assert_eq!(normalize_target("Game.exe", MatchMode::Name), "game.exe");
        assert_eq!(normalize_target(r#""C:\Games\Game.exe""#, MatchMode::Name), "game.exe");
        assert_eq!(normalize_target(r"C:\Games\", MatchMode::Path), r"c:\games");
        assert_eq!(normalize_target("   ", MatchMode::Path), "");
    }

    #[test]
    fn upsert_creates_new_binding() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        let r = upsert_binding(&mut cfg, &form("game.exe", MatchMode::Name, &["p1"]));
        assert_eq!(r, UpsertResult::Created);
        assert_eq!(cfg.bindings.len(), 1);
        let b = &cfg.bindings[0];
        assert_eq!(b.target, "game.exe");
        assert_eq!(b.mode, MatchMode::Name);
        assert!(b.enabled);
        assert_eq!(b.profile_ids, vec!["p1".to_string()]);
        assert_eq!(b.active_profile.as_deref(), Some("p1"));
        assert!(b.id.starts_with("bind"));
        // Другая цель — отдельная запись.
        assert_eq!(
            upsert_binding(&mut cfg, &form("other.exe", MatchMode::Name, &["p1"])),
            UpsertResult::Created
        );
        assert_eq!(cfg.bindings.len(), 2);
    }

    #[test]
    fn upsert_updates_duplicate_without_new_entry() {
        let mut cfg = cfg_with(&[("p1", "A"), ("p2", "B")], Vec::new());
        upsert_binding(&mut cfg, &form(r"C:\Games\Game.exe", MatchMode::Path, &["p1"]));
        let r = upsert_binding(&mut cfg, &form(r"c:/games/game.EXE", MatchMode::Path, &["p1", "p2"]));
        assert_eq!(r, UpsertResult::Updated);
        assert_eq!(cfg.bindings.len(), 1, "дубликат по режиму и цели не создаёт запись");
        assert_eq!(cfg.bindings[0].profile_ids, vec!["p1".to_string(), "p2".to_string()]);
    }

    #[test]
    fn upsert_keeps_name_and_path_bindings_apart() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        upsert_binding(&mut cfg, &form("game.exe", MatchMode::Name, &["p1"]));
        upsert_binding(&mut cfg, &form(r"C:\Games\game.exe", MatchMode::Path, &["p1"]));
        assert_eq!(cfg.bindings.len(), 2, "разные режимы — разные привязки");
    }

    #[test]
    fn upsert_editing_target_stays_single_entry() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        let mut f = form("game.exe", MatchMode::Name, &["p1"]);
        assert_eq!(upsert_binding(&mut cfg, &f), UpsertResult::Created);
        let id = cfg.bindings[0].id.clone();
        f.id = Some(id.clone());
        f.target = "renamed.exe".into();
        assert_eq!(upsert_binding(&mut cfg, &f), UpsertResult::Updated);
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0].id, id);
        assert_eq!(cfg.bindings[0].target, "renamed.exe");
    }

    #[test]
    fn upsert_editing_into_existing_target_does_not_duplicate() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        let mut a = form("first.exe", MatchMode::Name, &["p1"]);
        upsert_binding(&mut cfg, &a);
        a.id = Some(cfg.bindings[0].id.clone());
        upsert_binding(&mut cfg, &form("second.exe", MatchMode::Name, &["p1"]));
        let count_before = cfg.bindings.len();
        a.target = "SECOND.EXE".into();
        assert_eq!(upsert_binding(&mut cfg, &a), UpsertResult::Updated);
        assert_eq!(cfg.bindings.len(), count_before, "слияние с существующей целью");
    }

    #[test]
    fn upsert_rejects_empty_target_and_empty_profiles() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        assert_eq!(
            upsert_binding(&mut cfg, &form("   ", MatchMode::Name, &["p1"])),
            UpsertResult::Invalid(FormError::EmptyTarget)
        );
        assert_eq!(
            upsert_binding(&mut cfg, &form("game.exe", MatchMode::Name, &[])),
            UpsertResult::Invalid(FormError::NoProfile)
        );
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn upsert_drops_empty_triggers_and_unknown_active() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        let mut f = form("game.exe", MatchMode::Name, &["p1"]);
        f.active_profile = Some("p1".into());
        f.triggers = vec![
            // Заготовка строки без наблюдателя и профилей — выбрасывается молча.
            Trigger { watch: "  ".into(), watch_mode: MatchMode::Name, on_start: None, on_exit: None },
            Trigger { watch: "launcher.exe".into(), watch_mode: MatchMode::Name, on_start: None, on_exit: Some("p1".into()) },
        ];
        assert_eq!(upsert_binding(&mut cfg, &f), UpsertResult::Created);
        assert_eq!(cfg.bindings[0].triggers.len(), 1);
        assert_eq!(cfg.bindings[0].triggers[0].watch, "launcher.exe");

        let mut g = form("game2.exe", MatchMode::Name, &["p1"]);
        g.active_profile = Some("ghost".into());
        assert_eq!(upsert_binding(&mut cfg, &g), UpsertResult::Created);
        assert_eq!(cfg.bindings[1].active_profile, None, "активный вне profile_ids отбрасывается");
    }

    #[test]
    fn upsert_reports_trigger_without_watch() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        let mut f = form("game.exe", MatchMode::Name, &["p1"]);
        // Наблюдатель не заполнен, но профиль на запуск выбран — это ошибка, а не заготовка.
        f.triggers = vec![Trigger {
            watch: " ".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: None,
        }];
        assert_eq!(upsert_binding(&mut cfg, &f), UpsertResult::Invalid(FormError::TriggerWatch));
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn blank_trigger_row_is_not_an_error() {
        let mut cfg = cfg_with(&[("p1", "A")], Vec::new());
        let mut f = form("game.exe", MatchMode::Name, &["p1"]);
        f.triggers =
            vec![Trigger { watch: "".into(), watch_mode: MatchMode::Name, on_start: None, on_exit: None }];
        assert_eq!(validate_form(&f), Ok(()));
        assert_eq!(upsert_binding(&mut cfg, &f), UpsertResult::Created);
        assert!(cfg.bindings[0].triggers.is_empty(), "пустая заготовка не сохраняется");
    }

    #[test]
    fn validate_accepts_filled_form() {
        let f = form(r"C:\Games\game.exe", MatchMode::Path, &["p1"]);
        assert_eq!(validate_form(&f), Ok(()));
    }

    #[test]
    fn set_target_fills_form_and_switches_mode() {
        let mut st = State::default();
        st.set_target(r"C:\Games\Game.exe");
        assert_eq!(st.target, r"C:\Games\Game.exe");
        assert_eq!(st.mode, MatchMode::Path);
        st.set_target("   ");
        assert_eq!(st.target, r"C:\Games\Game.exe", "пустой путь игнорируется");
        st.mode = MatchMode::Name;
        st.set_target("plain.exe");
        assert_eq!(st.mode, MatchMode::Name, "имя без разделителей не включает режим пути");
    }

    #[test]
    fn form_error_keys_exist_in_tables() {
        for e in [FormError::EmptyTarget, FormError::NoProfile, FormError::TriggerWatch] {
            assert_ne!(
                crate::i18n::t(crate::config::Lang::Ru, e.key()),
                e.key(),
                "нет строки для {}",
                e.key()
            );
        }
    }

    #[test]
    fn own_str_keys_are_unique_and_bilingual() {
        let mut seen = HashSet::new();
        for (k, ru, en) in STR {
            assert!(seen.insert(*k), "дубликат ключа: {k}");
            assert!(!ru.trim().is_empty() && !en.trim().is_empty(), "пустой перевод: {k}");
        }
    }

    #[test]
    fn own_str_keys_are_not_in_common_table() {
        for (k, _, _) in STR {
            assert!(
                !crate::i18n::STRINGS.iter().any(|(ck, _, _)| ck == k),
                "ключ {k} уже есть в общей таблице i18n"
            );
        }
    }

    #[test]
    fn own_str_placeholders_match() {
        for (k, ru, en) in STR {
            let mut a: Vec<&str> = ru.match_indices('{').map(|(i, _)| &ru[i..]).collect();
            let mut b: Vec<&str> = en.match_indices('{').map(|(i, _)| &en[i..]).collect();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "разные плейсхолдеры в ключе {k}");
        }
    }

    #[test]
    fn trigger_summary_lists_watch_and_profiles() {
        let lang = crate::config::Lang::Ru;
        let name_of = |id: &str| match id {
            "p1" => "Game".to_string(),
            "p2" => "Idle".to_string(),
            other => other.to_string(),
        };
        let triggers = vec![Trigger {
            watch: "launcher.exe".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: Some("p2".into()),
        }];
        assert_eq!(
            trigger_summary(lang, &triggers, &name_of),
            "launcher.exe: Game → Idle"
        );
        let no_exit = vec![Trigger {
            watch: " launcher.exe ".into(),
            watch_mode: MatchMode::Name,
            on_start: Some("p1".into()),
            on_exit: None,
        }];
        assert_eq!(trigger_summary(lang, &no_exit, &name_of), "launcher.exe: Game → Не менять");
        let blank = vec![Trigger {
            watch: "  ".into(),
            watch_mode: MatchMode::Name,
            on_start: None,
            on_exit: None,
        }];
        assert_eq!(trigger_summary(lang, &blank, &name_of), "");
        assert_eq!(trigger_summary(lang, &[], &name_of), "");
    }
}
