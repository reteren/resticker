//! Чистая логика панели выбора окон («Слои видимости», SPEC.md §4.2;
//! docs/M4_WINDOW_PICKER_DESIGN.md §2-4): предикаты и решения без рендера +
//! билдер панели (дизайн §7.1, шаг 5). Роутинг событий и открытие — шаги 6-7,
//! отдельными срезами. rst-render нужен только билдеру (Panel/Checkbox/Button/
//! Primitive); предикаты §2-4 остаются чистыми.
//!
//! Главный инвариант (§2.1): состояние «выбран ли чекбокс» определяется тем
//! же предикатом [`rst_core::occluders::rule_matches`], которым маска решает
//! про окклюдера, — пересборка панели из `Sticker.visibility` никогда не
//! разойдётся с фактическим поведением маски.
//!
//! Модуль пока никем не вызывается: сшивка (кнопка тулбара, `EditState`) —
//! отдельные шаги плана §7.1 (6-7), придут следующими срезами. `dead_code`
//! снят до сшивки, чтобы `clippy -D warnings` оставался зелёным; после
//! подключения атрибут удалить.
#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::HashMap;

use rst_core::model::{OverlapRule, VisibilityMode, VisibilityRule};
use rst_core::occluders::{OccluderCandidate, rule_matches};
use rst_render::{
    Box2D, Button, ButtonContent, Checkbox, Panel, Primitive, Widget, WidgetId, text_size, theme,
};
use rst_win32::window_enum::WindowInfo;

/// Одна группа процессов (дизайн §3): ключ — короткое имя `exe_path`,
/// не pid (переживает рестарты; «все будущие окна этого процесса», SPEC 4.2).
/// `process_name: None` — группа «процесс неизвестен» (окна без `exe_path`):
/// без процесса-чекбокса, только строки окон с title-правилами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessGroup {
    /// Короткое имя exe (первая встреченная орфография) или `None` для
    /// группы «процесс неизвестен».
    pub process_name: Option<String>,
    /// Окна группы, отсортированные по `z_order` (как в Alt+Tab).
    pub windows: Vec<WindowInfo>,
}

/// Окно выразимо правилом (не protected process)? Пустые `exe_path` И
/// `title` одновременно модель не выражает ничем (дизайн §2.3) — строка
/// такого окна рисуется disabled и не подлежит выбору.
pub fn window_can_express_rule(window: &WindowInfo) -> bool {
    !(window.exe_path.as_os_str().is_empty() && window.title.is_empty())
}

/// Показывать ли окно «выбранным» в панели (дизайн §2.1): режим
/// `OverlapAllowlist` и хоть одно правило матчит окно тем же предикатом,
/// что маска использует для решения про окклюдера. Protected process —
/// безусловно не выбран. В других режимах (`Always`/`Desktop`/
/// `NeverOverlap`) чекбоксы сняты (дизайн §2.3: `Always` показывает всё
/// снятым).
pub fn window_is_checked(visibility: &VisibilityRule, window: &WindowInfo) -> bool {
    if !window_can_express_rule(window) || visibility.mode != VisibilityMode::OverlapAllowlist {
        return false;
    }
    visibility
        .rules
        .iter()
        .any(|r| rule_matches(r, &candidate(window)))
}

/// Отмечен ли чекбокс процесса (дизайн §2.1): есть правило, которое матчит
/// процесс целиком (обычно — `process_name`-правило на имя exe; title-правило
/// конкретного окна процесс не отмечает). У группы «процесс неизвестен»
/// чекбокса нет — всегда `false`.
pub fn process_is_checked(visibility: &VisibilityRule, group: &ProcessGroup) -> bool {
    if group.process_name.is_none() || visibility.mode != VisibilityMode::OverlapAllowlist {
        return false;
    }
    let Some(exe_path) = group
        .windows
        .iter()
        .find(|w| !w.exe_path.as_os_str().is_empty())
        .map(|w| w.exe_path.to_string_lossy().into_owned())
    else {
        return false;
    };
    let candidate = OccluderCandidate {
        exe_path: Some(exe_path),
        title: String::new(),
        class: String::new(),
    };
    visibility.rules.iter().any(|r| rule_matches(r, &candidate))
}

/// Сгруппировать снимок окон по процессам (дизайн §3): ключ — короткое имя
/// exe (регистронезависимо: Windows-ФС и `rule_matches` регистронезависимы),
/// окна без `exe_path` — в группу «процесс неизвестен` в конце списка.
/// Группы отсортированы по имени процесса, окна внутри — по `z_order`.
pub fn group_by_process(snapshot: &[WindowInfo]) -> Vec<ProcessGroup> {
    let mut by_key: HashMap<String, (String, Vec<WindowInfo>)> = HashMap::new();
    for window in snapshot {
        let short = short_exe_name(window);
        let key = short.as_deref().map(str::to_lowercase).unwrap_or_default();
        let display = short.unwrap_or_default();
        let entry = by_key.entry(key).or_insert_with(|| (display, Vec::new()));
        entry.1.push(window.clone());
    }
    let mut groups: Vec<ProcessGroup> = by_key
        .into_iter()
        .map(|(key, (display, mut windows))| {
            windows.sort_by_key(|w| w.z_order);
            ProcessGroup {
                process_name: if key.is_empty() { None } else { Some(display) },
                windows,
            }
        })
        .collect();
    groups.sort_by(|a, b| match (&a.process_name, &b.process_name) {
        (Some(a), Some(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
    groups
}

/// Переключить чекбокс процесса (дизайн §2.2): включение добавляет
/// `process_name`-правило (и переводит стикер в `OverlapAllowlist`, если
/// режим был не allow-list — §2.3, «первое изменение панели»); снятие
/// удаляет только «свои» правила панели — `process_name`-only по имени
/// (регистронезависимо), не трогая title-правила, combined-правила и
/// wildcard-паттерны (рукописный конфиг панель не «съедает», §2.2).
/// Режим при снятии не возвращается (§2.3: пустой allow-list — валидный
/// «только рабочий стол»).
///
/// `None` — no-op: группа «процесс неизвестен» (записывать в `process_name`
/// нечего, §2.3).
pub fn toggle_process_group(
    visibility: &VisibilityRule,
    group: &ProcessGroup,
) -> Option<VisibilityRule> {
    let name = group.process_name.as_deref()?;
    if process_is_checked(visibility, group) {
        let mut rules = visibility.rules.clone();
        rules.retain(|r| {
            !(r.title_pattern.is_none()
                && r.process_name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name)))
        });
        Some(VisibilityRule {
            mode: visibility.mode,
            rules,
        })
    } else {
        let mut rules = visibility.rules.clone();
        // Защита от дубля при переходе режима: правила могли уже лежать в
        // конфиге при `Always`/`Desktop` (is_checked там всегда false).
        if !rules.iter().any(|r| {
            r.title_pattern.is_none()
                && r.process_name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
        }) {
            rules.push(OverlapRule {
                process_name: Some(name.to_string()),
                title_pattern: None,
            });
        }
        Some(VisibilityRule {
            mode: VisibilityMode::OverlapAllowlist,
            rules,
        })
    }
}

/// «Выбрать все» — переключатель (SPEC.md §4.2, дословно): если выбрано не
/// всё — выбрать всё; если всё — снять выделение полностью (дизайн §4).
///
/// «Выбрано всё» = каждое выразимое окно снимка матчится правилом; строки
/// protected process (без чекбокса по определению) в подсчёте не участвуют.
/// Полный набор правил заменяет текущий: на каждую непустую группу —
/// `process_name`-правило, на окна группы «процесс неизвестен» — точные
/// title-правила (по одному на окно, только с непустым заголовком);
/// `mode` становится `OverlapAllowlist` (то же «первое изменение», §2.3).
/// Снятие — `rules.clear()` без смены режима.
pub fn toggle_select_all(visibility: &VisibilityRule, snapshot: &[WindowInfo]) -> VisibilityRule {
    let all_checked = snapshot
        .iter()
        .all(|w| !window_can_express_rule(w) || window_is_checked(visibility, w));
    if all_checked {
        return VisibilityRule {
            mode: visibility.mode,
            rules: Vec::new(),
        };
    }
    let mut rules = Vec::new();
    for group in group_by_process(snapshot) {
        match &group.process_name {
            Some(name) => rules.push(OverlapRule {
                process_name: Some(name.clone()),
                title_pattern: None,
            }),
            None => {
                for w in &group.windows {
                    if !w.title.is_empty() {
                        rules.push(OverlapRule {
                            process_name: None,
                            title_pattern: Some(w.title.clone()),
                        });
                    }
                }
            }
        }
    }
    VisibilityRule {
        mode: VisibilityMode::OverlapAllowlist,
        rules,
    }
}

/// [`WindowInfo`] → [`OccluderCandidate`] (rst-core не зависит от rst-win32,
/// перевод — на стороне координатора, как и в `refresh_occlusion`).
fn candidate(window: &WindowInfo) -> OccluderCandidate {
    OccluderCandidate {
        exe_path: if window.exe_path.as_os_str().is_empty() {
            None
        } else {
            Some(window.exe_path.to_string_lossy().into_owned())
        },
        title: window.title.clone(),
        class: window.class.clone(),
    }
}

/// Короткое имя exe (file_name); `None` — пустой `exe_path` (protected
/// process или сбой `OpenProcess`).
fn short_exe_name(window: &WindowInfo) -> Option<String> {
    let name = window.exe_path.file_name()?.to_string_lossy();
    let name = name.into_owned();
    if name.is_empty() { None } else { Some(name) }
}

// ---------------------------------------------------------------------------
// Билдер панели (дизайн §1, §3, §4, §6, §7.3-7.4; план §7.1, шаг 5)
// ---------------------------------------------------------------------------

/// Идентификатор панели выбора окон. Диапазон 200+: тулбар 0-8, панель у
/// курсора 100+.
pub const PICKER_PANEL_ID: WidgetId = 200;
/// Кнопка «Выбрать все» в шапке (SPEC §4.2, дизайн §4).
pub const PICKER_BTN_SELECT_ALL: WidgetId = 201;

// Схема WidgetId строк (число строк динамическое — малых констант, как у
// фиксированного тулбара, недостаточно):
// - чекбокс процесса: `PICKER_ROW_PROCESS_BASE + индекс_группы`
//   (группа < 0x1000, диапазон [0x1000, 0x10000));
// - чекбокс окна:     `PICKER_ROW_WINDOW_BASE + (группа << 16) | окно`
//   (группа < 0x1000, окно < 0x10000, диапазон [0x10000, 0x1001_0000));
// - надписи строк: тот же код + `LABEL_FLAG` (не интерактивны, не
//   декодируются, из диапазонов чекбоксов исключены).
// Кодируются РЕАЛЬНЫЕ индексы (не видимые): с учётом скролла вызывающий слой
// по клику чекбокса декодирует id → индекс группы/окна в
// `group_by_process(snapshot)` и применяет [`toggle_process_group`].
pub const PICKER_ROW_PROCESS_BASE: WidgetId = 0x1000;
pub const PICKER_ROW_WINDOW_BASE: WidgetId = 0x1_0000;
const LABEL_FLAG: WidgetId = 0x8000_0000;

/// Сколько строк списка влезает в панель (виртуализация, дизайн §7.4):
/// билдер строит только строки `[scroll, scroll + PICKER_VISIBLE_ROWS)`.
pub const PICKER_VISIBLE_ROWS: usize = 10;
/// Ширина панели, DIP.
pub const PICKER_WIDTH: f64 = 320.0;
/// Высота шапки с кнопкой «Выбрать все», DIP.
pub const PICKER_HEADER_H: f64 = 40.0;
/// Высота строки списка, DIP (равна `theme::BUTTON_SIZE` — колонка иконок
/// проектировалась квадратом этого размера, дизайн §6).
pub const PICKER_ROW_H: f64 = theme::BUTTON_SIZE;
/// Внутренний отступ панели, DIP.
pub const PICKER_PAD: f64 = 6.0;
/// Отступ строк окон от левого края строк процесса, DIP.
pub const PICKER_WINDOW_INDENT: f64 = 24.0;
/// Сторона слота-плейсхолдера иконки, DIP (дизайн §6: реальные иконки —
/// отдельный срез, здесь только зарезервированное место).
pub const PICKER_ICON_SIZE: f64 = 20.0;
/// Зазор между колонками, DIP.
pub const PICKER_GAP: f64 = 8.0;
/// Полная высота панели: отступы + шапка + видимые строки, DIP.
pub const PICKER_HEIGHT: f64 =
    2.0 * PICKER_PAD + PICKER_HEADER_H + PICKER_VISIBLE_ROWS as f64 * PICKER_ROW_H;

/// Результат [`build_picker_panel`]: панель + число строк списка для клампа
/// скролла вызывающим слоем (скролл валиден в `0..=total_rows - 1`).
pub struct PickerPanel {
    /// Собранная панель (шапка + видимый срез строк).
    pub panel: Panel,
    /// Всего строк списка: строки процессов + строки окон, шапка не в счёте.
    pub total_rows: usize,
}

/// Собрать панель выбора окон. `visibility` — текущее правило стикера
/// (состояние чекбоксов живёт в нём, не в панели — дизайн §2.1), `snapshot` —
/// снимок окон, `scroll` — сколько строк списка пропустить сверху
/// (виртуализация §7.4: строятся только видимые строки, `Panel` не трогается),
/// `frame` — рамка панели (полностью определяет вызывающий слой; типовой
/// размер — [`PICKER_WIDTH`]×[`PICKER_HEIGHT`]).
///
/// Шапка (кнопка «Выбрать все», дизайн §4) видима всегда; скролл сдвигает
/// только список под ней. Надпись кнопки отражает предстоящее действие
/// («Выбрать все»/«Снять все»), как `BTN_TOGGLE_ALL` в cursor_panel.rs.
/// Чекбокс процесса кликабелен; чекбоксы окон всегда disabled — окна только
/// отображение, выбор лишь на уровне процесса (дизайн §7.6).
pub fn build_picker_panel(
    visibility: &VisibilityRule,
    snapshot: &[WindowInfo],
    scroll: usize,
    frame: Box2D,
) -> PickerPanel {
    let mut panel = Panel::new(PICKER_PANEL_ID, frame);
    let left = frame.cx - frame.w / 2.0 + PICKER_PAD;
    let top = frame.cy - frame.h / 2.0;

    // Шапка: «Выбрать все» — переключатель (SPEC §4.2); надпись — предстоящее
    // действие («выбрать» или «снять»), как BTN_TOGGLE_ALL в cursor_panel.rs.
    let all_checked = all_windows_checked(visibility, snapshot);
    let label = if all_checked {
        "Снять все"
    } else {
        "Выбрать все"
    };
    let (tw, _) = text_size(label);
    let btn_w = tw + 2.0 * theme::BUTTON_PAD + 8.0;
    panel.add_widget(Button::new(
        PICKER_BTN_SELECT_ALL,
        Box2D {
            cx: left + btn_w / 2.0,
            cy: top + PICKER_PAD + PICKER_HEADER_H / 2.0,
            w: btn_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(label.to_string()),
    ));

    let groups = group_by_process(snapshot);
    let list_top = top + PICKER_PAD + PICKER_HEADER_H;
    let icon_cx = left + PICKER_ICON_SIZE / 2.0;
    let process_cb_cx = icon_cx + PICKER_ICON_SIZE / 2.0 + PICKER_GAP + theme::CHECKBOX_SIZE / 2.0;
    let process_label_left = process_cb_cx + theme::CHECKBOX_SIZE / 2.0 + PICKER_GAP;
    let window_cb_cx = process_cb_cx + PICKER_WINDOW_INDENT;
    let window_label_left = window_cb_cx + theme::CHECKBOX_SIZE / 2.0 + PICKER_GAP;

    // Список: один проход по всем группам. Реальный индекс строки `row`
    // считаем всегда (total_rows и скролл), виджеты строим только для
    // видимого окна `[scroll, scroll + PICKER_VISIBLE_ROWS)`.
    let mut row = 0usize;
    let mut built = 0usize;
    for (g, group) in groups.iter().enumerate() {
        if row >= scroll && built < PICKER_VISIBLE_ROWS {
            let cy = row_cy(list_top, built);
            if group.process_name.is_some() {
                panel.add_widget(Checkbox::standard(
                    PICKER_ROW_PROCESS_BASE + g as WidgetId,
                    process_cb_cx,
                    cy,
                    process_is_checked(visibility, group),
                ));
            }
            let text = match &group.process_name {
                Some(name) => format!("{name} ({})", group.windows.len()),
                None => format!("Неизвестный процесс ({})", group.windows.len()),
            };
            panel.add_widget(RowLabel::new(
                PICKER_ROW_PROCESS_BASE + g as WidgetId + LABEL_FLAG,
                text_rect(process_label_left, cy, &text),
                icon_rect(icon_cx, cy),
                text,
            ));
            built += 1;
        }
        row += 1;
        for (w, window) in group.windows.iter().enumerate() {
            if row >= scroll && built < PICKER_VISIBLE_ROWS {
                let cy = row_cy(list_top, built);
                let id = PICKER_ROW_WINDOW_BASE + ((g as WidgetId) << 16) + w as WidgetId;
                let mut cb =
                    Checkbox::standard(id, window_cb_cx, cy, window_is_checked(visibility, window));
                cb.set_disabled(true);
                panel.add_widget(cb);
                panel.add_widget(RowLabel::new(
                    id + LABEL_FLAG,
                    text_rect(window_label_left, cy, &window.title),
                    icon_rect(icon_cx, cy),
                    window.title.clone(),
                ));
                built += 1;
            }
            row += 1;
        }
    }

    PickerPanel {
        panel,
        total_rows: row,
    }
}

/// Центр по Y строки с видимым индексом `visible_index` (счёт только строк,
/// попавших в окно скролла: они укладываются сверху вниз под шапкой).
fn row_cy(list_top: f64, visible_index: usize) -> f64 {
    list_top + visible_index as f64 * PICKER_ROW_H + PICKER_ROW_H / 2.0
}

/// Текстовая область надписи: левый край `left`, центр строки `cy`.
fn text_rect(left: f64, cy: f64, text: &str) -> Box2D {
    let (tw, th) = text_size(text);
    Box2D {
        cx: left + tw / 2.0,
        cy,
        w: tw,
        h: th,
        rotation: 0.0,
    }
}

/// Слот-плейсхолдер иконки (дизайн §6): пустой квадрат в колонке иконок.
fn icon_rect(cx: f64, cy: f64) -> Box2D {
    Box2D {
        cx,
        cy,
        w: PICKER_ICON_SIZE,
        h: PICKER_ICON_SIZE,
        rotation: 0.0,
    }
}

/// «Всё выбрано» (дизайн §4): каждое выразимое окно снимка матчится правилом —
/// то же условие, по которому [`toggle_select_all`] решает, выбрать или снять.
fn all_windows_checked(visibility: &VisibilityRule, snapshot: &[WindowInfo]) -> bool {
    snapshot
        .iter()
        .all(|w| !window_can_express_rule(w) || window_is_checked(visibility, w))
}

/// Индекс группы по WidgetId чекбокса процесса (`None` — это не чекбокс
/// процесса). Группа — индекс в `group_by_process(snapshot)`, `None`-группа
/// «процесс неизвестен» чекбокса не имеет (см. [`build_picker_panel`]).
pub fn picker_id_to_process(id: WidgetId) -> Option<usize> {
    (PICKER_ROW_PROCESS_BASE..PICKER_ROW_WINDOW_BASE)
        .contains(&id)
        .then(|| (id - PICKER_ROW_PROCESS_BASE) as usize)
}

/// (Индекс группы, индекс окна в группе) по WidgetId чекбокса окна.
pub fn picker_id_to_window(id: WidgetId) -> Option<(usize, usize)> {
    (PICKER_ROW_WINDOW_BASE..PICKER_ROW_WINDOW_BASE + 0x1000_0000)
        .contains(&id)
        .then(|| {
            let v = id - PICKER_ROW_WINDOW_BASE;
            ((v >> 16) as usize, (v & 0xffff) as usize)
        })
}

/// Надпись строки списка + слот-плейсхолдер иконки (дизайн §6): один виджет
/// на строку, эмитит квадрат колонки иконок и текст (заголовок окна / имя
/// процесса). В toolbar.rs/cursor_panel.rs текстовых виджетов нет (подписи
/// живут внутри кнопок) — поэтому мини-виджет приватный здесь. Хит-теста
/// нет: строка не потребляет клики, интерактивны только чекбоксы.
struct RowLabel {
    id: WidgetId,
    text_rect: Box2D,
    icon_rect: Box2D,
    text: String,
}

impl RowLabel {
    fn new(id: WidgetId, text_rect: Box2D, icon_rect: Box2D, text: String) -> Self {
        Self {
            id,
            text_rect,
            icon_rect,
            text,
        }
    }
}

impl Widget for RowLabel {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.text_rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.text_rect = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Fill {
            rect: self.icon_rect,
            color: theme::BUTTON_BG,
            opacity: 1.0,
        });
        if !self.text.is_empty() {
            out.push(Primitive::Text {
                rect: self.text_rect,
                text: self.text.clone(),
                color: theme::TEXT,
                opacity: 1.0,
            });
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn window(hwnd: usize, exe: &str, title: &str, pid: u32, z: u32) -> WindowInfo {
        WindowInfo {
            hwnd,
            rect: Default::default(),
            pid,
            exe_path: if exe.is_empty() {
                PathBuf::new()
            } else {
                PathBuf::from(exe)
            },
            title: title.to_string(),
            class: String::new(),
            z_order: z,
            iconic: false,
            icon: None,
        }
    }

    fn rule(process: Option<&str>, title: Option<&str>) -> OverlapRule {
        OverlapRule {
            process_name: process.map(String::from),
            title_pattern: title.map(String::from),
        }
    }

    fn allowlist(rules: Vec<OverlapRule>) -> VisibilityRule {
        VisibilityRule {
            mode: VisibilityMode::OverlapAllowlist,
            rules,
        }
    }

    fn names(groups: &[ProcessGroup]) -> Vec<Option<&str>> {
        groups.iter().map(|g| g.process_name.as_deref()).collect()
    }

    // --- группировка (дизайн §3) ---

    #[test]
    fn group_two_pids_same_exe_is_one_group() {
        let snapshot = [
            window(1, r"C:\Apps\chrome.exe", "Chrome 1", 100, 1),
            window(2, r"C:\Apps\chrome.exe", "Chrome 2", 200, 2),
        ];
        let groups = group_by_process(&snapshot);
        assert_eq!(names(&groups), vec![Some("chrome.exe")]);
        assert_eq!(groups[0].windows.len(), 2);
    }

    #[test]
    fn group_case_insensitive_key_one_group() {
        let snapshot = [
            window(1, r"C:\Apps\chrome.exe", "t", 1, 1),
            window(2, r"C:\Apps\CHROME.EXE", "t", 2, 2),
        ];
        let groups = group_by_process(&snapshot);
        assert_eq!(groups.len(), 1, "регистр не плодит группы");
        assert_eq!(groups[0].process_name.as_deref(), Some("chrome.exe"));
    }

    #[test]
    fn group_alphabetical_with_unknown_last() {
        let snapshot = [
            window(1, "", "Без процесса", 10, 5),
            window(2, r"C:\Apps\zebra.exe", "z", 20, 1),
            window(3, r"C:\Apps\alpha.exe", "a", 30, 2),
        ];
        let groups = group_by_process(&snapshot);
        assert_eq!(
            names(&groups),
            vec![Some("alpha.exe"), Some("zebra.exe"), None]
        );
    }

    #[test]
    fn group_windows_ordered_by_z_order() {
        let snapshot = [
            window(1, r"C:\Apps\app.exe", "top", 1, 5),
            window(2, r"C:\Apps\app.exe", "mid", 1, 2),
            window(3, r"C:\Apps\app.exe", "bottom", 1, 9),
        ];
        let groups = group_by_process(&snapshot);
        let titles: Vec<&str> = groups[0].windows.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, vec!["mid", "top", "bottom"]);
    }

    #[test]
    fn group_empty_snapshot() {
        assert!(group_by_process(&[]).is_empty());
    }

    // --- состояние «выбран» (дизайн §2.1) ---

    #[test]
    fn window_checked_by_process_rule() {
        let w = window(1, r"C:\Apps\chrome.exe", "Chrome", 1, 1);
        assert!(window_is_checked(
            &allowlist(vec![rule(Some("chrome.exe"), None)]),
            &w
        ));
    }

    #[test]
    fn window_checked_by_title_rule() {
        let w = window(1, "", "Untitled - Notepad", 1, 1);
        assert!(window_is_checked(
            &allowlist(vec![rule(None, Some("Untitled - Notepad"))]),
            &w
        ));
    }

    #[test]
    fn window_checked_by_full_path_rule() {
        let w = window(1, r"C:\Apps\chrome.exe", "t", 1, 1);
        assert!(window_is_checked(
            &allowlist(vec![rule(Some(r"C:\Apps\chrome.exe"), None)]),
            &w
        ));
    }

    #[test]
    fn window_with_exe_but_empty_title_checked_by_process_rule() {
        let w = window(1, r"C:\Apps\app.exe", "", 1, 1);
        assert!(window_is_checked(
            &allowlist(vec![rule(Some("app.exe"), None)]),
            &w
        ));
    }

    #[test]
    fn window_with_title_but_no_exe_checked_by_title_rule() {
        let w = window(1, "", "Settings", 1, 1);
        assert!(window_is_checked(
            &allowlist(vec![rule(None, Some("Settings"))]),
            &w
        ));
    }

    #[test]
    fn window_unchecked_when_no_rule_matches() {
        let w = window(1, r"C:\Apps\chrome.exe", "t", 1, 1);
        assert!(!window_is_checked(
            &allowlist(vec![rule(Some("firefox.exe"), None)]),
            &w
        ));
        assert!(!window_is_checked(&allowlist(vec![]), &w));
    }

    #[test]
    fn protected_window_never_checked() {
        let w = window(1, "", "", 1, 1);
        assert!(!window_can_express_rule(&w));
        // Даже тотальное правило «*» не матчит protected process —
        // безусловно не выбран (дизайн §2.3).
        assert!(!window_is_checked(
            &allowlist(vec![rule(None, Some("*"))]),
            &w
        ));
    }

    #[test]
    fn non_allowlist_modes_show_nothing_checked() {
        let w = window(1, r"C:\Apps\app.exe", "t", 1, 1);
        for mode in [
            VisibilityMode::Always,
            VisibilityMode::Desktop,
            VisibilityMode::NeverOverlap,
        ] {
            let v = VisibilityRule {
                mode,
                rules: vec![rule(Some("app.exe"), None)],
            };
            assert!(!window_is_checked(&v, &w), "{mode:?} показывает всё снятым");
        }
    }

    #[test]
    fn process_checked_by_process_rule() {
        let group = ProcessGroup {
            process_name: Some("chrome.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\chrome.exe", "t", 1, 1)],
        };
        assert!(process_is_checked(
            &allowlist(vec![rule(Some("chrome.exe"), None)]),
            &group
        ));
    }

    #[test]
    fn process_not_checked_by_window_title_rule() {
        let group = ProcessGroup {
            process_name: Some("chrome.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\chrome.exe", "Settings", 1, 1)],
        };
        // Title-правило отмечает только своё окно, процесс целиком — нет.
        assert!(!process_is_checked(
            &allowlist(vec![rule(None, Some("Settings"))]),
            &group
        ));
    }

    #[test]
    fn process_unknown_group_never_checked() {
        let group = ProcessGroup {
            process_name: None,
            windows: vec![window(1, "", "t", 1, 1)],
        };
        assert!(!process_is_checked(
            &allowlist(vec![rule(Some("t"), None)]),
            &group
        ));
    }

    // --- переключение процесса (дизайн §2.2, §2.3) ---

    #[test]
    fn toggle_process_round_trip() {
        let group = ProcessGroup {
            process_name: Some("chrome.exe".to_string()),
            windows: vec![
                window(1, r"C:\Apps\chrome.exe", "A", 1, 1),
                window(2, r"C:\Apps\chrome.exe", "B", 1, 2),
            ],
        };
        let v = allowlist(vec![]);
        let on = toggle_process_group(&v, &group).unwrap();
        assert_eq!(on.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(on.rules, vec![rule(Some("chrome.exe"), None)]);
        for w in &group.windows {
            assert!(window_is_checked(&on, w));
        }
        assert!(process_is_checked(&on, &group));

        let off = toggle_process_group(&on, &group).unwrap();
        assert!(off.rules.is_empty(), "round-trip снял правило");
        for w in &group.windows {
            assert!(!window_is_checked(&off, w));
        }
    }

    #[test]
    fn toggle_process_switches_always_to_allowlist_on_first_check() {
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\app.exe", "t", 1, 1)],
        };
        let v = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
        };
        let on = toggle_process_group(&v, &group).unwrap();
        assert_eq!(on.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(on.rules, vec![rule(Some("app.exe"), None)]);
    }

    #[test]
    fn toggle_process_desktop_switches_to_allowlist() {
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\app.exe", "t", 1, 1)],
        };
        let v = VisibilityRule {
            mode: VisibilityMode::Desktop,
            rules: vec![],
        };
        let on = toggle_process_group(&v, &group).unwrap();
        assert_eq!(on.mode, VisibilityMode::OverlapAllowlist);
    }

    #[test]
    fn toggle_process_on_does_not_duplicate_existing_rule_on_mode_transition() {
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\app.exe", "t", 1, 1)],
        };
        // Правило уже лежит в конфиге, но режим Desktop — чекбокс снят.
        let v = VisibilityRule {
            mode: VisibilityMode::Desktop,
            rules: vec![rule(Some("app.exe"), None)],
        };
        let on = toggle_process_group(&v, &group).unwrap();
        assert_eq!(on.rules, vec![rule(Some("app.exe"), None)], "без дубля");
        assert_eq!(on.mode, VisibilityMode::OverlapAllowlist);
    }

    #[test]
    fn toggle_process_off_keeps_mode() {
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\app.exe", "t", 1, 1)],
        };
        let v = allowlist(vec![rule(Some("app.exe"), None)]);
        let off = toggle_process_group(&v, &group).unwrap();
        assert_eq!(
            off.mode,
            VisibilityMode::OverlapAllowlist,
            "пустой allow-list — валидный «только рабочий стол», режим не возвращается"
        );
    }

    #[test]
    fn toggle_process_off_keeps_title_and_wildcard_rules() {
        let group = ProcessGroup {
            process_name: Some("chrome.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\chrome.exe", "Settings", 1, 1)],
        };
        let v = allowlist(vec![
            rule(Some("chrome.exe"), None),
            rule(None, Some("Settings")),
            rule(None, Some("*Notepad")),
        ]);
        let off = toggle_process_group(&v, &group).unwrap();
        assert_eq!(
            off.rules,
            vec![rule(None, Some("Settings")), rule(None, Some("*Notepad"))],
            "title- и wildcard-правила панель не «съедает»"
        );
    }

    #[test]
    fn toggle_process_off_removes_case_insensitive_rule() {
        let group = ProcessGroup {
            process_name: Some("chrome.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\chrome.exe", "t", 1, 1)],
        };
        let v = allowlist(vec![rule(Some("CHROME.EXE"), None)]);
        let off = toggle_process_group(&v, &group).unwrap();
        assert!(off.rules.is_empty(), "регистронезависимо");
    }

    #[test]
    fn toggle_process_off_keeps_combined_handwritten_rule() {
        let group = ProcessGroup {
            process_name: Some("chrome.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\chrome.exe", "t", 1, 1)],
        };
        // Combined-правило (process + title) — рукописное, снятие его не трогает.
        let combined = OverlapRule {
            process_name: Some("chrome.exe".to_string()),
            title_pattern: Some("*".to_string()),
        };
        let v = allowlist(vec![combined.clone()]);
        let off = toggle_process_group(&v, &group).unwrap();
        assert_eq!(off.rules, vec![combined]);
    }

    #[test]
    fn toggle_process_unknown_group_is_no_op() {
        let group = ProcessGroup {
            process_name: None,
            windows: vec![window(1, "", "t", 1, 1)],
        };
        let v = allowlist(vec![]);
        assert_eq!(
            toggle_process_group(&v, &group),
            None,
            "no-op без имени процесса"
        );
    }

    // --- «выбрать все» (дизайн §4) ---

    #[test]
    fn select_all_from_empty_builds_full_ruleset() {
        let snapshot = [
            window(1, r"C:\Apps\alpha.exe", "t", 10, 1),
            window(2, r"C:\Apps\zebra.exe", "t", 20, 2),
            window(3, "", "Настройки", 30, 3),
        ];
        let v = allowlist(vec![]);
        let all = toggle_select_all(&v, &snapshot);
        assert_eq!(all.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(
            all.rules,
            vec![
                rule(Some("alpha.exe"), None),
                rule(Some("zebra.exe"), None),
                rule(None, Some("Настройки")),
            ]
        );
        assert!(snapshot.iter().all(|w| window_is_checked(&all, w)));
    }

    #[test]
    fn select_all_from_all_checked_clears_rules() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let v = allowlist(vec![rule(Some("app.exe"), None)]);
        let cleared = toggle_select_all(&v, &snapshot);
        assert!(cleared.rules.is_empty(), "снять выделение полностью");
        assert_eq!(cleared.mode, VisibilityMode::OverlapAllowlist);
    }

    #[test]
    fn select_all_replaces_partial_rules() {
        let snapshot = [
            window(1, r"C:\Apps\alpha.exe", "t", 10, 1),
            window(2, r"C:\Apps\zebra.exe", "t", 20, 2),
        ];
        // Выбрано не всё (zebra без правила) — «выбрать всё» строит полный
        // набор, заменяя частичный.
        let v = allowlist(vec![rule(Some("alpha.exe"), None)]);
        let all = toggle_select_all(&v, &snapshot);
        assert_eq!(
            all.rules,
            vec![rule(Some("alpha.exe"), None), rule(Some("zebra.exe"), None)]
        );
    }

    #[test]
    fn select_all_from_always_sets_allowlist_mode() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let v = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
        };
        let all = toggle_select_all(&v, &snapshot);
        assert_eq!(all.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(all.rules, vec![rule(Some("app.exe"), None)]);
    }

    #[test]
    fn select_all_skips_protected_windows() {
        // Protected process (пусто и exe, и title) не получает правила и не
        // влияет на «выбрать всё»: всё выразимое выбрано → инверсия чистит.
        let snapshot = [window(1, "", "", 1, 1)];
        let v = allowlist(vec![]);
        let all = toggle_select_all(&v, &snapshot);
        assert!(all.rules.is_empty(), "protected-окно нельзя выразить");
        assert_eq!(all.mode, VisibilityMode::OverlapAllowlist);
    }

    #[test]
    fn select_all_round_trip_inverts_twice() {
        let snapshot = [
            window(1, r"C:\Apps\alpha.exe", "t", 10, 1),
            window(2, "", "Настройки", 20, 2),
            window(3, "", "", 30, 3), // protected — вне подсчёта
        ];
        let v = allowlist(vec![]);
        let all = toggle_select_all(&v, &snapshot);
        let cleared = toggle_select_all(&all, &snapshot);
        assert!(cleared.rules.is_empty());
        assert_eq!(cleared.mode, VisibilityMode::OverlapAllowlist);
    }

    #[test]
    fn select_all_with_empty_snapshot_clears() {
        let v = allowlist(vec![rule(Some("stale.exe"), None)]);
        let cleared = toggle_select_all(&v, &[]);
        assert!(cleared.rules.is_empty());
    }

    // --- билдер панели (дизайн §1, §3-4, §6, §7.3-7.4) ---

    fn picker_frame() -> Box2D {
        Box2D {
            cx: 200.0,
            cy: 300.0,
            w: PICKER_WIDTH,
            h: PICKER_HEIGHT,
            rotation: 0.0,
        }
    }

    /// Тексты всех `Primitive::Text` панели в порядке отрисовки.
    fn picker_texts(panel: &Panel) -> Vec<String> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter_map(|p| match p {
                Primitive::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Сколько слотов-плейсхолдеров иконок (квадраты `PICKER_ICON_SIZE`).
    fn icon_slot_count(panel: &Panel) -> usize {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter(|p| {
                matches!(p, Primitive::Fill { rect, .. }
                    if rect.w == PICKER_ICON_SIZE && rect.h == PICKER_ICON_SIZE)
            })
            .count()
    }

    #[test]
    fn empty_snapshot_builds_header_only() {
        let p = build_picker_panel(&allowlist(vec![]), &[], 0, picker_frame());
        assert_eq!(p.total_rows, 0);
        assert!(p.panel.widget::<Button>(PICKER_BTN_SELECT_ALL).is_some());
        // Пустой снимок: «всё выбрано» (пустое «все» истинно) — кнопка
        // предлагает снять.
        assert_eq!(picker_texts(&p.panel), vec!["Снять все"]);
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .is_none()
        );
        assert_eq!(icon_slot_count(&p.panel), 0);
    }

    #[test]
    fn process_group_and_unknown_window_row_structure() {
        let snapshot = [
            window(1, r"C:\Apps\chrome.exe", "Chrome", 100, 1),
            window(2, "", "Настройки", 200, 2),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        // Строки: chrome-процесс, chrome-окно, неизвестный-процесс, окно.
        assert_eq!(p.total_rows, 4);

        let proc = p.panel.widget::<Checkbox>(PICKER_ROW_PROCESS_BASE).unwrap();
        assert!(!proc.checked());
        let b = proc.bounds();
        assert!(proc.hit_test((b.cx, b.cy)), "чекбокс процесса кликабелен");
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE + 1)
                .is_none(),
            "у группы «процесс неизвестен» чекбокса нет (дизайн §3)"
        );
        let w0 = p.panel.widget::<Checkbox>(PICKER_ROW_WINDOW_BASE).unwrap();
        let b = w0.bounds();
        assert!(!w0.hit_test((b.cx, b.cy)), "чекбокс окна всегда disabled");
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE + (1u32 << 16))
                .is_some(),
            "строка окна группы «неизвестный процесс» есть"
        );

        let texts = picker_texts(&p.panel);
        for expected in [
            "chrome.exe (1)",
            "Chrome",
            "Неизвестный процесс (1)",
            "Настройки",
        ] {
            assert!(
                texts.iter().any(|t| t == expected),
                "нет текста {expected:?}"
            );
        }
        // Окно-строки сдвинуты вправо от строки процесса.
        let proc_cx = p
            .panel
            .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
            .unwrap()
            .bounds()
            .cx;
        let win_cx = p
            .panel
            .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
            .unwrap()
            .bounds()
            .cx;
        assert!(win_cx > proc_cx + 1.0, "отступ строк окон");
    }

    #[test]
    fn process_checkbox_reflects_rules() {
        let snapshot = [window(1, r"C:\Apps\chrome.exe", "Chrome", 100, 1)];
        let on = build_picker_panel(
            &allowlist(vec![rule(Some("chrome.exe"), None)]),
            &snapshot,
            0,
            picker_frame(),
        );
        let proc = on
            .panel
            .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
            .unwrap();
        assert!(proc.checked());
        assert!(
            on.panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
                .unwrap()
                .checked(),
            "окно отмечено правилом процесса"
        );
        let off = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        assert!(
            !off.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .unwrap()
                .checked()
        );
        assert!(
            !off.panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
                .unwrap()
                .checked()
        );
    }

    #[test]
    fn window_checkboxes_always_disabled() {
        let snapshot = [
            window(1, r"C:\Apps\app.exe", "t", 1, 1),
            window(2, "", "t2", 2, 2),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        for g in 0..2u32 {
            let cb = p
                .panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE + (g << 16))
                .unwrap();
            let b = cb.bounds();
            assert!(
                !cb.hit_test((b.cx, b.cy)),
                "окно {g}: disabled всегда (дизайн §7.6)"
            );
        }
    }

    #[test]
    fn protected_window_checkbox_disabled_regardless() {
        let snapshot = [window(1, "", "", 1, 1)];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        assert_eq!(
            p.total_rows, 2,
            "строка «неизвестный процесс» + строка окна"
        );
        let cb = p.panel.widget::<Checkbox>(PICKER_ROW_WINDOW_BASE).unwrap();
        let b = cb.bounds();
        assert!(!cb.checked());
        assert!(
            !cb.hit_test((b.cx, b.cy)),
            "protected process: disabled всегда"
        );
        // Даже тотальное правило «*» не отмечает protected-окно (дизайн §2.3).
        let p2 = build_picker_panel(
            &allowlist(vec![rule(None, Some("*"))]),
            &snapshot,
            0,
            picker_frame(),
        );
        assert!(
            !p2.panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
                .unwrap()
                .checked()
        );
    }

    #[test]
    fn scroll_skips_rows_from_top() {
        let snapshot = [
            window(1, r"C:\Apps\a.exe", "A", 1, 1),
            window(2, r"C:\Apps\b.exe", "B", 2, 2),
            window(3, r"C:\Apps\c.exe", "C", 3, 3),
        ];
        // Строки: 0 a, 1 окно a, 2 b, 3 окно b, 4 c, 5 окно c.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 2, picker_frame());
        assert_eq!(p.total_rows, 6);
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .is_none(),
            "строка a пропущена"
        );
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE + 1)
                .is_some(),
            "строка b на месте"
        );
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE + 2)
                .is_some(),
            "строка c на месте"
        );
        assert!(
            p.panel.widget::<Checkbox>(PICKER_ROW_WINDOW_BASE).is_none(),
            "окно a пропущено"
        );
        assert_eq!(icon_slot_count(&p.panel), 4, "видны b, окно b, c, окно c");
        // Скролл за пределы списка: строк нет, шапка на месте.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 6, picker_frame());
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .is_none()
        );
        assert!(p.panel.widget::<Button>(PICKER_BTN_SELECT_ALL).is_some());
        // Скролл в хвост: видна только строка c.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 4, picker_frame());
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE + 2)
                .is_some()
        );
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE + 1)
                .is_none()
        );
        assert_eq!(icon_slot_count(&p.panel), 2);
    }

    #[test]
    fn total_rows_counts_all_groups_and_windows() {
        let snapshot = [
            window(1, r"C:\Apps\a.exe", "A1", 1, 1),
            window(2, r"C:\Apps\a.exe", "A2", 1, 2),
            window(3, r"C:\Apps\b.exe", "B", 2, 3),
            window(4, "", "Неизвестное", 3, 4),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        // a: процесс + 2 окна, b: процесс + 1 окно, неизвестный: процесс + 1 окно.
        assert_eq!(p.total_rows, 3 + 2 + 2);
        let texts = picker_texts(&p.panel);
        assert!(
            texts.iter().any(|t| t == "a.exe (2)"),
            "счётчик окон в заголовке группы"
        );
    }

    #[test]
    fn virtualization_caps_visible_rows() {
        let mut snapshot = Vec::new();
        for i in 0..12u32 {
            snapshot.push(window(
                i as usize,
                r"C:\Apps\app.exe",
                &format!("t{i}"),
                1,
                i,
            ));
        }
        // Один процесс + 12 окон = 13 строк списка.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        assert_eq!(p.total_rows, 13);
        assert_eq!(
            icon_slot_count(&p.panel),
            PICKER_VISIBLE_ROWS,
            "строится только видимое окно"
        );
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE + 10)
                .is_none(),
            "10-е окно за видимым окном не строится"
        );
    }

    #[test]
    fn select_all_button_label_reflects_state() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let partial = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame());
        assert!(picker_texts(&partial.panel).contains(&"Выбрать все".to_string()));
        let all = build_picker_panel(
            &allowlist(vec![rule(Some("app.exe"), None)]),
            &snapshot,
            0,
            picker_frame(),
        );
        assert!(picker_texts(&all.panel).contains(&"Снять все".to_string()));
    }

    #[test]
    fn widget_id_decoders() {
        assert_eq!(picker_id_to_process(PICKER_ROW_PROCESS_BASE + 3), Some(3));
        assert_eq!(
            picker_id_to_process(PICKER_ROW_PROCESS_BASE + 3 + LABEL_FLAG),
            None
        );
        assert_eq!(picker_id_to_process(PICKER_BTN_SELECT_ALL), None);
        assert_eq!(
            picker_id_to_window(PICKER_ROW_WINDOW_BASE + (2u32 << 16) + 5),
            Some((2, 5))
        );
        assert_eq!(picker_id_to_window(PICKER_ROW_WINDOW_BASE), Some((0, 0)));
        assert_eq!(
            picker_id_to_window(PICKER_ROW_WINDOW_BASE + (2u32 << 16) + 5 + LABEL_FLAG),
            None
        );
        assert_eq!(picker_id_to_window(PICKER_BTN_SELECT_ALL), None);
    }
}
