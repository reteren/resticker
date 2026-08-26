//! Логика панели выбора окон («Слои видимости», SPEC.md §4.2;
//! docs/M4_WINDOW_PICKER_DESIGN.md §1-4): предикаты, решения и билдер панели.
//! Подключена в `overlay_manager.rs` (план §7.1, шаг 7): `EditState.window_picker`,
//! кнопка `TB_LAYERS` в тулбаре, роутинг указателя/клавиатуры, отрисовка.
//!
//! Главный инвариант (§2.1): состояние «выбран ли чекбокс» определяется тем
//! же предикатом [`rst_core::occluders::rule_matches`], которым маска решает
//! про окклюдера, — пересборка панели из `Sticker.visibility` никогда не
//! разойдётся с фактическим поведением маски.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use rst_core::model::{OverlapRule, VisibilityMode, VisibilityRule};
use rst_core::occluders::{OccluderCandidate, rule_matches};
use rst_render::{
    Box2D, Button, ButtonContent, Checkbox, Panel, Primitive, ScrollBar, Widget, WidgetId,
    text_size, theme,
};
use rst_win32::window_enum::{WindowIcon, WindowInfo};

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
    if !window_can_express_rule(window) {
        return false;
    }
    if visibility.mode == VisibilityMode::Always {
        // «Всегда поверх всего» — это и есть «выбраны все окна»: элемент
        // остаётся видимым над любым из них. Раньше панель в этом режиме
        // показывала ВСЕ галочки снятыми, хотя элемент виден везде — прямо
        // противоположную картину (репорт пользователя 2026-08-22: «сделай
        // так, чтобы по умолчанию были выбраны все окна»).
        return true;
    }
    if visibility.mode != VisibilityMode::OverlapAllowlist {
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
    if group.process_name.is_none() {
        return false;
    }
    if visibility.mode == VisibilityMode::Always {
        return true; // см. `window_is_checked`
    }
    if visibility.mode != VisibilityMode::OverlapAllowlist {
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
    snapshot: &[WindowInfo],
) -> Option<VisibilityRule> {
    let name = group.process_name.as_deref()?;
    if visibility.mode == VisibilityMode::Always {
        // Снятие галочки из состояния «выбраны все»: материализуем список
        // из всех остальных процессов снимка. Без этого шага снятие было бы
        //но-опом — в режиме `Always` списка правил ещё не существует.
        let mut rules = Vec::new();
        for other in group_by_process(snapshot) {
            let Some(other_name) = other.process_name else {
                continue;
            };
            if other_name.eq_ignore_ascii_case(name) {
                continue;
            }
            rules.push(OverlapRule {
                process_name: Some(other_name),
                title_pattern: None,
            });
        }
        return Some(VisibilityRule {
            mode: VisibilityMode::OverlapAllowlist,
            rules,
        });
    }
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
        // «Снять все» — пустой allow-list. Режим обязательно
        // `OverlapAllowlist`: в `Always` пустой список правил означал бы
        // «выбраны все» (см. `window_is_checked`), то есть кнопка не делала
        // бы ничего.
        return VisibilityRule {
            mode: VisibilityMode::OverlapAllowlist,
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

/// Пресет «только рабочий стол» (ROADMAP.md M4): ровно
/// `{mode: OverlapAllowlist, rules: []}` — пустой allow-list семантически
/// тождествен `Desktop` (occluders.rs, тест
/// `allowlist_empty_rules_is_desktop_equivalent`).
///
/// Не новая бизнес-логика — переиспользует существующий путь «Снять все»:
/// [`toggle_select_all`] с пустым снимком заведомо идёт в ветку «всё
/// выбрано» (пустое «все» истинно по определению) и чистит правила — тот же
/// результат, что у кнопки «Снять все». Единственная правка — режим
/// безусловно `OverlapAllowlist`: переключатель «Снять все» сохраняет
/// текущий режим, а в `Always` при пустом снимке вернул бы `{Always, []}` —
/// обратная семантика (Always — «всегда виден», не «только рабочий стол»;
/// дизайн §2.3).
pub fn apply_desktop_only_preset(visibility: &VisibilityRule) -> VisibilityRule {
    let cleared = toggle_select_all(visibility, &[]);
    VisibilityRule {
        mode: VisibilityMode::OverlapAllowlist,
        rules: cleared.rules,
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
/// Кнопка-пресет «Только рабочий стол» в шапке (ROADMAP.md M4): выставляет
/// ровно `{mode: OverlapAllowlist, rules: []}` — то же правило, что даёт
/// «Снять все», но с постоянной узнаваемой подписью (дизайн §2.3: пустой
/// allow-list семантически тождествен `Desktop`).
pub const PICKER_BTN_DESKTOP_ONLY: WidgetId = 202;
/// Полоса скролла списка (живой репорт пользователя: длинный список окон
/// обрезался без видимого намёка, что его можно листать колесом мыши —
/// `overlay_manager::handle_input`, `InputEvent::MouseWheel`).
const PICKER_SCROLLBAR_ID: WidgetId = 203;

/// Подпись кнопки-пресета «Только рабочий стол» — постоянна, не зависит от
/// состояния списка правил (в отличие от подписи переключателя «Выбрать
/// все»/«Снять все»).
const DESKTOP_ONLY_LABEL: &str = "Desktop only";

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
/// Высота шапки (кнопки «Выбрать все»/«Снять все» и «Только рабочий
/// стол»), DIP.
pub const PICKER_HEADER_H: f64 = 40.0;
/// Высота строки списка, DIP (равна `theme::BUTTON_SIZE` — колонка иконок
/// проектировалась квадратом этого размера, дизайн §6).
pub const PICKER_ROW_H: f64 = theme::BUTTON_SIZE;
/// Внутренний отступ панели, DIP.
pub const PICKER_PAD: f64 = 6.0;
/// Отступ строк окон от левого края строк процесса, DIP.
pub const PICKER_WINDOW_INDENT: f64 = 24.0;
/// Сторона слота иконки, DIP (дизайн §6: реальный растр окна
/// растягивается в этот квадрат; без иконки — плейсхолдер того же размера).
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
/// Шапка (переключатель «Выбрать все»/«Снять все» + кнопка-пресет «Только
/// рабочий стол», дизайн §4, ROADMAP.md M4) видима всегда; скролл сдвигает
/// только список под ней. Надпись переключателя отражает предстоящее действие
/// («Выбрать все»/«Снять все»), как `BTN_TOGGLE_ALL` в cursor_panel.rs;
/// подпись пресета постоянна. Чекбокс процесса кликабелен; чекбоксы окон
/// всегда disabled — окна только отображение, выбор лишь на уровне процесса
/// (дизайн §7.6).
pub fn build_picker_panel(
    visibility: &VisibilityRule,
    snapshot: &[WindowInfo],
    scroll: usize,
    frame: Box2D,
    desktop_preset: bool,
) -> PickerPanel {
    let mut panel = Panel::new(PICKER_PANEL_ID, frame);
    let left = frame.cx - frame.w / 2.0 + PICKER_PAD;
    let top = frame.cy - frame.h / 2.0;

    // Шапка: «Выбрать все» — переключатель (SPEC §4.2); надпись — предстоящее
    // действие («выбрать» или «снять»), как BTN_TOGGLE_ALL в cursor_panel.rs.
    // Рядом — кнопка-пресет «Только рабочий стол» (ROADMAP.md M4): то же
    // правило, что у «Снять все» ({mode: OverlapAllowlist, rules: []}),
    // постоянная подпись вместо чтения состояния списка.
    let all_checked = all_windows_checked(visibility, snapshot);
    let toggle_label = if all_checked {
        "Clear all"
    } else {
        "Select all"
    };
    let header_y = top + PICKER_PAD + PICKER_HEADER_H / 2.0;
    let mut btn_cx = left;
    // Пресет «только рабочий стол» осмыслен лишь для стикера: у панели,
    // открытой для ЗАКРЕПЛЁННОГО окна, тот же набор правил читается наоборот
    // («показывать только на этих окнах»), и кнопка с таким названием врала
    // бы. Остальная панель — та же самая, как и просил пользователь
    // (2026-08-22: «сделай редактор выбора окон точь в точь как у стикеров»).
    let header_buttons: &[(WidgetId, &str)] = if desktop_preset {
        &[
            (PICKER_BTN_SELECT_ALL, toggle_label),
            (PICKER_BTN_DESKTOP_ONLY, DESKTOP_ONLY_LABEL),
        ]
    } else {
        &[(PICKER_BTN_SELECT_ALL, toggle_label)]
    };
    for &(id, text) in header_buttons {
        let (tw, _) = text_size(text);
        let btn_w = tw + 2.0 * theme::BUTTON_PAD + 8.0;
        panel.add_widget(Button::new(
            id,
            Box2D {
                cx: btn_cx + btn_w / 2.0,
                cy: header_y,
                w: btn_w,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label(text.to_string()),
        ));
        btn_cx += btn_w + PICKER_GAP;
    }

    let groups = group_by_process(snapshot);
    let list_top = top + PICKER_PAD + PICKER_HEADER_H;
    let icon_cx = left + PICKER_ICON_SIZE / 2.0;
    let process_cb_cx = icon_cx + PICKER_ICON_SIZE / 2.0 + PICKER_GAP + theme::CHECKBOX_SIZE / 2.0;
    let process_label_left = process_cb_cx + theme::CHECKBOX_SIZE / 2.0 + PICKER_GAP;
    let window_cb_cx = process_cb_cx + PICKER_WINDOW_INDENT;
    let window_label_left = window_cb_cx + theme::CHECKBOX_SIZE / 2.0 + PICKER_GAP;
    // Правый край панели — независимое ревью нашло, что панель была первым
    // местом в конвейере примитивов, где рисуется текст произвольной длины
    // без клиппинга: длинный заголовок окна/имя процесса иначе просто рисуют
    // за рамкой. Обрезаем по ширине с многоточием (`truncate_to_width`).
    // Полоса скролла (ниже) всегда откусывает свою колонку от правого края —
    // ширина текста не скачет в зависимости от того, нужен ли сейчас скролл.
    let right_edge = frame.cx + frame.w / 2.0 - PICKER_PAD - theme::SCROLLBAR_WIDTH - PICKER_GAP;
    let process_label_max_w = right_edge - process_label_left;
    let window_label_max_w = right_edge - window_label_left;

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
                None => format!("Unknown process ({})", group.windows.len()),
            };
            let text = truncate_to_width(&text, process_label_max_w);
            panel.add_widget(RowLabel::new(
                PICKER_ROW_PROCESS_BASE + g as WidgetId + LABEL_FLAG,
                text_rect(process_label_left, cy, &text),
                icon_rect(icon_cx, cy),
                text,
                process_icon(group),
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
                let title = truncate_to_width(&window.title, window_label_max_w);
                let window_icon = window
                    .icon
                    .clone()
                    .map(|icon| (icon_key(&window.exe_path), icon));
                panel.add_widget(RowLabel::new(
                    id + LABEL_FLAG,
                    text_rect(window_label_left, cy, &title),
                    icon_rect(icon_cx, cy),
                    title,
                    window_icon,
                ));
                built += 1;
            }
            row += 1;
        }
    }

    // Полоса скролла — только когда реально есть что листать (живой репорт
    // пользователя: список был длиннее видимой части панели, но ничего не
    // намекало, что можно листать колесом мыши — `PICKER_SCROLLBAR_ID`,
    // `overlay_manager::handle_input`, `InputEvent::MouseWheel`).
    if row > PICKER_VISIBLE_ROWS {
        let list_h = PICKER_VISIBLE_ROWS as f64 * PICKER_ROW_H;
        panel.add_widget(ScrollBar::new(
            PICKER_SCROLLBAR_ID,
            Box2D {
                cx: right_edge + PICKER_GAP + theme::SCROLLBAR_WIDTH / 2.0,
                cy: list_top + list_h / 2.0,
                w: theme::SCROLLBAR_WIDTH,
                h: list_h,
                rotation: 0.0,
            },
            PICKER_VISIBLE_ROWS,
            row,
            scroll,
        ));
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

/// Обрезать `text` многоточием, чтобы уместиться в `max_w` DIP (независимое
/// ревью нашло: конвейер примитивов не клипует текст, а панель — первое
/// место, где рисуется текст произвольной длины — длинный заголовок окна
/// или имя процесса иначе просто рисовался бы за правым краем панели).
/// Считает по символам (`char`), не байтам — заголовки часто кириллические,
/// обрезка по байтам могла бы разрезать символ пополам.
pub(crate) fn truncate_to_width(text: &str, max_w: f64) -> String {
    if text_size(text).0 <= max_w {
        return text.to_string();
    }
    const ELLIPSIS: &str = "...";
    let chars: Vec<char> = text.chars().collect();
    for len in (0..chars.len()).rev() {
        let candidate: String = chars[..len].iter().collect::<String>() + ELLIPSIS;
        if text_size(&candidate).0 <= max_w {
            return candidate;
        }
    }
    ELLIPSIS.to_string()
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

/// Слот иконки строки: квадрат в колонке иконок (реальный растр
/// [`Primitive::Rgba`] или плейсхолдер-`Fill` — решает `RowLabel::draw`).
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

/// Надпись строки списка + слот иконки (дизайн §6; иконки — срез M4,
/// `WindowInfo.icon`): один виджет на строку, эмитит квадрат колонки
/// иконок (реальный растр [`Primitive::Rgba`] при `icon: Some`, иначе
/// плейсхолдер-`Fill`) и текст (заголовок окна / имя процесса).
/// В toolbar.rs/cursor_panel.rs текстовых виджетов нет (подписи живут
/// внутри кнопок) — поэтому мини-виджет приватный здесь. Хит-теста нет:
/// строка не потребляет клики, интерактивны только чекбоксы.
struct RowLabel {
    id: WidgetId,
    text_rect: Box2D,
    icon_rect: Box2D,
    text: String,
    /// Иконка строки: key кэша текстур (`Primitive::Rgba`, окна одного
    /// exe делят key) + сам растр. `None` — иконка не извлеклась
    /// (панель рисует плейсхолдер, дизайн §6).
    icon: Option<(u64, WindowIcon)>,
}

impl RowLabel {
    fn new(
        id: WidgetId,
        text_rect: Box2D,
        icon_rect: Box2D,
        text: String,
        icon: Option<(u64, WindowIcon)>,
    ) -> Self {
        Self {
            id,
            text_rect,
            icon_rect,
            text,
            icon,
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
        match &self.icon {
            Some((key, icon)) => out.push(Primitive::Rgba {
                rect: self.icon_rect,
                key: *key,
                width: icon.width,
                height: icon.height,
                rgba: icon.rgba.clone(),
                opacity: 1.0,
            }),
            None => out.push(Primitive::Fill {
                rect: self.icon_rect,
                color: theme::BUTTON_BG,
                opacity: 1.0,
            }),
        }
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

/// Стабильный key иконки для кэша текстур (`Primitive::Rgba`): хэш полного
/// пути exe — окна одного процесса делят одну GPU-текстуру (дедупликация
/// на уровне кэша, как и на уровне извлечения иконок в rst-win32).
/// `DefaultHasher` детерминирован в рамках процесса — кэш текстур живёт
/// в нём же, рассинхрон ключей невозможен.
fn icon_key(exe_path: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    exe_path.hash(&mut hasher);
    hasher.finish()
}

/// Иконка строки процесса: общая для всех окон группы (один exe) — берём
/// первое окно. `None` — у группы нет иконки (не извлеклась).
fn process_icon(group: &ProcessGroup) -> Option<(u64, WindowIcon)> {
    let window = group.windows.first()?;
    let icon = window.icon.clone()?;
    Some((icon_key(&window.exe_path), icon))
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

    /// Окно с иконкой `size×size` (пустой растр — важен только факт наличия
    /// и его размер/ключ).
    fn window_with_icon(
        hwnd: usize,
        exe: &str,
        title: &str,
        pid: u32,
        z: u32,
        size: u32,
    ) -> WindowInfo {
        let mut w = window(hwnd, exe, title, pid, z);
        w.icon = Some(WindowIcon {
            width: size,
            height: size,
            rgba: vec![0u8; (size * size * 4) as usize],
        });
        w
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

    /// `Always` — это «выбраны все окна» (запрос пользователя 2026-08-22:
    /// по умолчанию должны стоять все галочки, а не ни одной, — элемент в
    /// этом режиме и правда виден над любым окном). Остальные
    /// не-allowlist режимы по-прежнему показывают всё снятым.
    #[test]
    fn always_is_everything_checked_other_modes_nothing() {
        let w = window(1, r"C:\Apps\app.exe", "t", 1, 1);
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![w.clone()],
        };
        let always = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
        };
        assert!(window_is_checked(&always, &w), "Always — все окна выбраны");
        assert!(process_is_checked(&always, &group), "и все процессы");

        for mode in [VisibilityMode::Desktop, VisibilityMode::NeverOverlap] {
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
        let on = toggle_process_group(&v, &group, &group.windows).unwrap();
        assert_eq!(on.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(on.rules, vec![rule(Some("chrome.exe"), None)]);
        for w in &group.windows {
            assert!(window_is_checked(&on, w));
        }
        assert!(process_is_checked(&on, &group));

        let off = toggle_process_group(&on, &group, &group.windows).unwrap();
        assert!(off.rules.is_empty(), "round-trip снял правило");
        for w in &group.windows {
            assert!(!window_is_checked(&off, w));
        }
    }

    /// Снятие галочки из состояния «выбраны все» (`Always`) материализует
    /// список из ВСЕХ ОСТАЛЬНЫХ процессов снимка — иначе клик был бы
    /// но-опом: в `Always` списка правил ещё не существует.
    #[test]
    fn toggle_process_from_always_materializes_the_rest() {
        let snapshot = [
            window(1, r"C:\Apps\app.exe", "t", 1, 1),
            window(2, r"C:\Apps\other.exe", "t2", 2, 2),
        ];
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![snapshot[0].clone()],
        };
        let v = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
        };
        let off = toggle_process_group(&v, &group, &snapshot).unwrap();
        assert_eq!(off.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(
            off.rules,
            vec![rule(Some("other.exe"), None)],
            "снятый процесс выпадает, остальные остаются выбранными"
        );
        assert!(!process_is_checked(&off, &group), "галочка снята");
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
        let on = toggle_process_group(&v, &group, &group.windows).unwrap();
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
        let on = toggle_process_group(&v, &group, &group.windows).unwrap();
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
        let off = toggle_process_group(&v, &group, &group.windows).unwrap();
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
        let off = toggle_process_group(&v, &group, &group.windows).unwrap();
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
        let off = toggle_process_group(&v, &group, &group.windows).unwrap();
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
        let off = toggle_process_group(&v, &group, &group.windows).unwrap();
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
            toggle_process_group(&v, &group, &group.windows),
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

    /// В `Always` уже выбрано всё, поэтому переключатель работает как
    /// «Снять все» и обязан сменить режим: пустой список правил в `Always`
    /// снова означал бы «выбраны все», то есть кнопка ничего не делала бы.
    #[test]
    fn select_all_from_always_clears_to_allowlist() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let v = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
        };
        let cleared = toggle_select_all(&v, &snapshot);
        assert_eq!(cleared.mode, VisibilityMode::OverlapAllowlist);
        assert!(cleared.rules.is_empty());
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

    // --- пресет «только рабочий стол» (ROADMAP.md M4) ---

    #[test]
    fn desktop_only_preset_clears_rules_and_pins_allowlist() {
        let snapshot = [
            window(1, r"C:\Apps\chrome.exe", "Chrome", 100, 1),
            window(2, "", "Настройки", 200, 2),
        ];
        let v = allowlist(vec![
            rule(Some("chrome.exe"), None),
            rule(None, Some("Настройки")),
        ]);
        let preset = apply_desktop_only_preset(&v);
        assert_eq!(preset.mode, VisibilityMode::OverlapAllowlist);
        assert!(preset.rules.is_empty());
        assert!(
            snapshot.iter().all(|w| !window_is_checked(&preset, w)),
            "после пресета ничего не выбрано"
        );
    }

    #[test]
    fn desktop_only_preset_equals_select_all_clear_branch() {
        // «Снять все» (клик по переключателю в состоянии «всё выбрано»)
        // даёт то же правило — пресет просто делает его доступным одной
        // кнопкой с постоянной подписью.
        let snapshot = [
            window(1, r"C:\Apps\chrome.exe", "Chrome", 100, 1),
            window(2, "", "Настройки", 200, 2),
        ];
        let all = toggle_select_all(&allowlist(vec![]), &snapshot);
        let cleared = toggle_select_all(&all, &snapshot);
        let preset = apply_desktop_only_preset(&all);
        assert_eq!(preset, cleared, "пресет == результат «Снять все»");
        assert_eq!(preset.mode, VisibilityMode::OverlapAllowlist);
        assert!(preset.rules.is_empty());
    }

    #[test]
    fn desktop_only_preset_from_always_is_allowlist_not_always() {
        // Крайний случай: «Снять все» в `Always` при пустом снимке сохранило
        // бы {Always, []} (Always — «всегда виден», обратная семантика);
        // пресет обязан дать ровно {OverlapAllowlist, []} (дизайн §2.3).
        let v = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
        };
        let preset = apply_desktop_only_preset(&v);
        assert_eq!(preset.mode, VisibilityMode::OverlapAllowlist);
        assert!(preset.rules.is_empty());
    }

    #[test]
    fn desktop_only_preset_from_any_mode_is_allowlist() {
        for mode in [
            VisibilityMode::Always,
            VisibilityMode::Desktop,
            VisibilityMode::NeverOverlap,
            VisibilityMode::OverlapAllowlist,
        ] {
            let v = VisibilityRule {
                mode,
                rules: vec![rule(Some("stale.exe"), None)],
            };
            let preset = apply_desktop_only_preset(&v);
            assert_eq!(preset.mode, VisibilityMode::OverlapAllowlist, "{mode:?}");
            assert!(preset.rules.is_empty(), "{mode:?}");
        }
    }

    #[test]
    fn desktop_only_preset_is_idempotent() {
        let v = allowlist(vec![rule(Some("chrome.exe"), None)]);
        let once = apply_desktop_only_preset(&v);
        let twice = apply_desktop_only_preset(&once);
        assert_eq!(once, twice);
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

    /// Все `Primitive::Rgba` панели в порядке отрисовки.
    fn rgba_icons(panel: &Panel) -> Vec<(u64, u32, u32)> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter_map(|p| match p {
                Primitive::Rgba {
                    key, width, height, ..
                } => Some((key, width, height)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_snapshot_builds_header_only() {
        let p = build_picker_panel(&allowlist(vec![]), &[], 0, picker_frame(), true);
        assert_eq!(p.total_rows, 0);
        assert!(p.panel.widget::<Button>(PICKER_BTN_SELECT_ALL).is_some());
        assert!(p.panel.widget::<Button>(PICKER_BTN_DESKTOP_ONLY).is_some());
        // Пустой снимок: «всё выбрано» (пустое «все» истинно) — кнопка
        // предлагает снять; рядом — пресет «Только рабочий стол».
        assert_eq!(
            picker_texts(&p.panel),
            vec!["Clear all".to_string(), DESKTOP_ONLY_LABEL.to_string()]
        );
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
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
            "Unknown process (1)",
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
            true,
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
        let off = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
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
            true,
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 2, picker_frame(), true);
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 6, picker_frame(), true);
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .is_none()
        );
        assert!(p.panel.widget::<Button>(PICKER_BTN_SELECT_ALL).is_some());
        // Скролл в хвост: видна только строка c.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 4, picker_frame(), true);
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
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
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
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

    /// Регрессия на живой репорт пользователя: список длиннее видимой части
    /// панели ничем не намекал, что его можно листать — полоса скролла
    /// теперь обязана появиться, когда есть что скроллить, и отсутствовать,
    /// когда весь список и так помещается.
    #[test]
    fn scrollbar_appears_only_when_list_overflows_visible_rows() {
        let short: Vec<WindowInfo> = (0..3u32)
            .map(|i| window(i as usize, r"C:\Apps\app.exe", &format!("t{i}"), 1, i))
            .collect();
        let short_panel = build_picker_panel(&allowlist(vec![]), &short, 0, picker_frame(), true);
        assert!(
            short_panel.total_rows <= PICKER_VISIBLE_ROWS,
            "фикстура должна умещаться без скролла"
        );
        assert!(
            short_panel
                .panel
                .widget::<ScrollBar>(PICKER_SCROLLBAR_ID)
                .is_none(),
            "нечего листать — полосы скролла быть не должно"
        );

        let mut long = Vec::new();
        for i in 0..12u32 {
            long.push(window(
                i as usize,
                r"C:\Apps\app.exe",
                &format!("t{i}"),
                1,
                i,
            ));
        }
        let long_panel = build_picker_panel(&allowlist(vec![]), &long, 0, picker_frame(), true);
        assert!(long_panel.total_rows > PICKER_VISIBLE_ROWS);
        assert!(
            long_panel
                .panel
                .widget::<ScrollBar>(PICKER_SCROLLBAR_ID)
                .is_some(),
            "список длиннее видимой части — полоса скролла обязана появиться"
        );
    }

    #[test]
    fn select_all_button_label_reflects_state() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let partial = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        assert!(picker_texts(&partial.panel).contains(&"Select all".to_string()));
        let all = build_picker_panel(
            &allowlist(vec![rule(Some("app.exe"), None)]),
            &snapshot,
            0,
            picker_frame(),
            true,
        );
        assert!(picker_texts(&all.panel).contains(&"Clear all".to_string()));
    }

    #[test]
    fn desktop_only_button_present_in_both_toggle_states() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let partial = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        assert!(
            partial
                .panel
                .widget::<Button>(PICKER_BTN_DESKTOP_ONLY)
                .is_some()
        );
        let texts = picker_texts(&partial.panel);
        assert!(texts.contains(&"Select all".to_string()));
        assert!(texts.contains(&DESKTOP_ONLY_LABEL.to_string()));

        let all = build_picker_panel(
            &allowlist(vec![rule(Some("app.exe"), None)]),
            &snapshot,
            0,
            picker_frame(),
            true,
        );
        assert!(
            all.panel
                .widget::<Button>(PICKER_BTN_DESKTOP_ONLY)
                .is_some()
        );
        let texts = picker_texts(&all.panel);
        assert!(texts.contains(&"Clear all".to_string()));
        assert!(texts.contains(&DESKTOP_ONLY_LABEL.to_string()));
    }

    #[test]
    fn desktop_only_button_sits_next_to_toggle_all_within_panel() {
        let p = build_picker_panel(&allowlist(vec![]), &[], 0, picker_frame(), true);
        let frame = p.panel.frame();
        let toggle = p
            .panel
            .widget::<Button>(PICKER_BTN_SELECT_ALL)
            .unwrap()
            .bounds();
        let desktop = p
            .panel
            .widget::<Button>(PICKER_BTN_DESKTOP_ONLY)
            .unwrap()
            .bounds();
        assert!(
            desktop.cx > toggle.cx + toggle.w / 2.0,
            "пресет справа от переключателя, а не поверх"
        );
        assert!(
            desktop.cx - desktop.w / 2.0 >= frame.cx - frame.w / 2.0,
            "кнопка не левее рамки панели"
        );
        assert!(
            desktop.cx + desktop.w / 2.0 <= frame.cx + frame.w / 2.0,
            "кнопка не правее рамки панели"
        );
        assert!(
            (desktop.cy - toggle.cy).abs() < 1e-9,
            "обе кнопки в одном ряду шапки"
        );
    }

    // --- обрезка длинного текста (независимое ревью, конвейер не клипует) ---

    #[test]
    fn truncate_to_width_keeps_short_text_intact() {
        assert_eq!(truncate_to_width("chrome.exe", 500.0), "chrome.exe");
    }

    #[test]
    fn truncate_to_width_shortens_with_ellipsis() {
        let long = "Очень длинный заголовок окна, который явно не влезет";
        let truncated = truncate_to_width(long, 60.0);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() < long.len());
        assert!(text_size(&truncated).0 <= 60.0);
    }

    #[test]
    fn truncate_to_width_handles_cyrillic_by_char_not_byte() {
        // Кириллица — многобайтовые символы в UTF-8; обрезка по байтам могла
        // бы разрезать символ пополам и дать невалидную строку/панику.
        let long = "жжжжжжжжжжжжжжжжжжжжжжжжжжжжжж";
        let truncated = truncate_to_width(long, 20.0);
        assert!(truncated.ends_with("..."));
        assert!(truncated.chars().count() < long.chars().count() + 3);
    }

    #[test]
    fn build_picker_panel_truncates_long_window_title() {
        let long_title = "Ж".repeat(200);
        let snapshot = [window(1, r"C:\Apps\app.exe", &long_title, 1, 1)];
        let picker = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        let texts = picker_texts(&picker.panel);
        assert!(
            texts
                .iter()
                .any(|t| t.ends_with("...") && t.len() < long_title.len()),
            "длинный заголовок должен быть обрезан с многоточием: {texts:?}"
        );
    }

    // --- иконки строк (M4 §6) ---

    #[test]
    fn row_with_icon_emits_rgba_primitive_not_fill() {
        let snapshot = [window_with_icon(
            1,
            r"C:\Apps\chrome.exe",
            "Chrome",
            100,
            1,
            16,
        )];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        // Строки: chrome-процесс + chrome-окно — обе с иконками: два Rgba,
        // плейсхолдеров (Fill) в колонке иконок нет вовсе.
        let icons = rgba_icons(&p.panel);
        assert_eq!(icons.len(), 2, "процесс и окно несут иконки: {icons:?}");
        assert!(icons.iter().all(|&(_, w, h)| w == 16 && h == 16));
        assert_eq!(icon_slot_count(&p.panel), 0, "плейсхолдеров не осталось");
    }

    #[test]
    fn rows_of_same_process_share_icon_key() {
        let snapshot = [
            window_with_icon(1, r"C:\Apps\chrome.exe", "Chrome A", 100, 1, 16),
            window_with_icon(2, r"C:\Apps\chrome.exe", "Chrome B", 200, 2, 16),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        let icons = rgba_icons(&p.panel);
        // Процесс + два окна: три Rgba с одним ключом — одна GPU-текстура
        // на весь процесс (дедупликация кэша текстур).
        assert_eq!(icons.len(), 3);
        let keys: Vec<u64> = icons.iter().map(|&(k, _, _)| k).collect();
        assert!(
            keys.iter().all(|&k| k == keys[0]),
            "все строки одного exe делят key: {keys:?}"
        );
    }

    #[test]
    fn distinct_processes_get_distinct_icon_keys() {
        let snapshot = [
            window_with_icon(1, r"C:\Apps\chrome.exe", "Chrome", 100, 1, 16),
            window_with_icon(2, r"C:\Apps\firefox.exe", "Firefox", 200, 2, 16),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        let icons = rgba_icons(&p.panel);
        let keys: Vec<u64> = icons.iter().map(|&(k, _, _)| k).collect();
        assert_eq!(keys[0], keys[1], "chrome: процесс и окно");
        assert_eq!(keys[2], keys[3], "firefox: процесс и окно");
        assert_ne!(keys[0], keys[2], "разные exe — разные key");
    }

    #[test]
    fn window_without_icon_keeps_fill_placeholder() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "No icon", 1, 1)];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        assert_eq!(icon_slot_count(&p.panel), 2, "процесс и окно: плейсхолдеры");
        assert!(rgba_icons(&p.panel).is_empty());
    }

    #[test]
    fn icon_key_is_stable_for_same_path() {
        let a = icon_key(Path::new(r"C:\Apps\chrome.exe"));
        let b = icon_key(Path::new(r"C:\Apps\chrome.exe"));
        assert_eq!(a, b, "тот же путь — тот же key (кэш текстур стабилен)");
        let c = icon_key(Path::new(r"C:\Apps\firefox.exe"));
        assert_ne!(a, c, "разные пути — разные key");
    }
}
