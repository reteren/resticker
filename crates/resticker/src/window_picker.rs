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
use rst_core::ui_motion::{CARD_DURATION_MS, STAGGER_STEP_MS, stagger_delay_ms};
use rst_render::{
    Box2D, Button, ButtonContent, Checkbox, Panel, Primitive, ScrollBar, Widget, WidgetId,
    box_contains, glass_control, text_size, theme,
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
    if visibility.mode == VisibilityMode::OverlapDenylist {
        // «Все, кроме перечисленных» — галочка снята ровно у перечисленных.
        return !visibility
            .rules
            .iter()
            .any(|r| rule_matches(r, &candidate(window)));
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
    if visibility.mode != VisibilityMode::OverlapAllowlist
        && visibility.mode != VisibilityMode::OverlapDenylist
    {
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
    let listed = visibility.rules.iter().any(|r| rule_matches(r, &candidate));
    // В режиме «все, кроме» список читается наоборот: перечисленный процесс —
    // это снятая галочка.
    if visibility.mode == VisibilityMode::OverlapDenylist {
        !listed
    } else {
        listed
    }
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
/// Из состояния «выбраны все» ([`VisibilityMode::Always`]) снятие уходит в
/// зеркальный [`VisibilityMode::OverlapDenylist`] — «все, кроме этого», — и
/// возвращается обратно в `Always`, когда исключений не осталось. Снимок
/// окон для этого не нужен и намеренно НЕ принимается: именно попытка
/// выразить «все остальные» перечислением запущенных процессов и делала
/// правило протухающим (репорт пользователя 2026-09-02).
///
/// `None` — no-op: группа «процесс неизвестен» (записывать в `process_name`
/// нечего, §2.3).
pub fn toggle_process_group(
    visibility: &VisibilityRule,
    group: &ProcessGroup,
) -> Option<VisibilityRule> {
    let name = group.process_name.as_deref()?;
    if visibility.mode == VisibilityMode::Always {
        // Снятие галочки из состояния «выбраны все» — это ИСКЛЮЧЕНИЕ, а не
        // список разрешений: записываем один снятый процесс, а «все
        // остальные» остаются определением, а не перечислением.
        //
        // Раньше здесь материализовался снимок всех прочих процессов, и с
        // этого момента «все» означало «все, которые были запущены в ту
        // секунду»: окно, открытое позже — тем более после перезагрузки
        // компьютера, — в список не попадало, и стикер уходил под него
        // (репорт пользователя 2026-09-02).
        return Some(with_rules(
            visibility,
            VisibilityMode::OverlapDenylist,
            vec![OverlapRule {
                process_name: Some(name.to_string()),
                title_pattern: None,
            }],
        ));
    }
    if visibility.mode == VisibilityMode::OverlapDenylist {
        let mut rules = visibility.rules.clone();
        if process_is_checked(visibility, group) {
            // Снять галочку — добавить процесс в исключения.
            if !has_process_rule(&rules, name) {
                rules.push(OverlapRule {
                    process_name: Some(name.to_string()),
                    title_pattern: None,
                });
            }
        } else {
            retain_without_process(&mut rules, name);
            if rules.is_empty() {
                // Исключений не осталось — это снова честное «все», включая
                // те окна, которых пока нет.
                return Some(with_rules(visibility, VisibilityMode::Always, rules));
            }
        }
        return Some(with_rules(
            visibility,
            VisibilityMode::OverlapDenylist,
            rules,
        ));
    }
    if process_is_checked(visibility, group) {
        let mut rules = visibility.rules.clone();
        retain_without_process(&mut rules, name);
        Some(with_rules(visibility, visibility.mode, rules))
    } else {
        let mut rules = visibility.rules.clone();
        // Защита от дубля при переходе режима: правила могли уже лежать в
        // конфиге при `Always`/`Desktop` (is_checked там всегда false).
        if !has_process_rule(&rules, name) {
            rules.push(OverlapRule {
                process_name: Some(name.to_string()),
                title_pattern: None,
            });
        }
        Some(with_rules(
            visibility,
            VisibilityMode::OverlapAllowlist,
            rules,
        ))
    }
}

/// Переключить отдельное окно внутри группы.  Правило процесса нельзя
/// использовать здесь: оно затронет все окна с тем же exe.  Для точечного
/// выбора используется title-only правило, которое уже поддерживает модель
/// `OverlapRule` и тот же matcher, что и маска окклюзии.
///
/// Если существующее правило описывает весь процесс, оно разворачивается в
/// title-only правила соседних окон.  Это сохраняет независимость чекбоксов
/// даже после клика по строке приложения. Окно без заголовка в многократной
/// группе не имеет представимого стабильного ключа и оставляет панель без
/// действия вместо опасного переключения всего процесса.
pub fn toggle_window(
    visibility: &VisibilityRule,
    group: &ProcessGroup,
    window_index: usize,
) -> Option<VisibilityRule> {
    let window = group.windows.get(window_index)?;
    if !window_can_express_rule(window) || window.title.is_empty() {
        return None;
    }
    let title = window.title.as_str();
    let mut rules = visibility.rules.clone();
    let process_name = group.process_name.as_deref();
    let process_rule = process_name.is_some_and(|name| has_process_rule(&rules, name));
    let exact_title = |rule: &OverlapRule| {
        rule.process_name.is_none()
            && rule
                .title_pattern
                .as_deref()
                .is_some_and(|pattern| pattern.eq_ignore_ascii_case(title))
    };
    let checked = window_is_checked(visibility, window);

    match visibility.mode {
        VisibilityMode::Always => Some(with_rules(
            visibility,
            VisibilityMode::OverlapDenylist,
            vec![OverlapRule {
                process_name: None,
                title_pattern: Some(title.to_string()),
            }],
        )),
        VisibilityMode::OverlapAllowlist => {
            if checked {
                if process_rule {
                    let name = process_name?;
                    retain_without_process(&mut rules, name);
                    // The process rule selected every sibling. Preserve that
                    // state as individual title rules, minus the clicked one.
                    for (index, sibling) in group.windows.iter().enumerate() {
                        if index != window_index
                            && !sibling.title.is_empty()
                            && window_is_checked(visibility, sibling)
                            && !rules.iter().any(|rule| {
                                rule.process_name.is_none()
                                    && rule
                                        .title_pattern
                                        .as_deref()
                                        .is_some_and(|p| p.eq_ignore_ascii_case(&sibling.title))
                            })
                        {
                            rules.push(OverlapRule {
                                process_name: None,
                                title_pattern: Some(sibling.title.clone()),
                            });
                        }
                    }
                } else if rules.iter().any(&exact_title) {
                    rules.retain(|rule| !exact_title(rule));
                } else {
                    // A wildcard/combined hand-written rule cannot express
                    // “all but this one” in the current OR matcher.
                    return None;
                }
            } else if !rules.iter().any(&exact_title) {
                rules.push(OverlapRule {
                    process_name: None,
                    title_pattern: Some(title.to_string()),
                });
            }
            Some(with_rules(
                visibility,
                VisibilityMode::OverlapAllowlist,
                rules,
            ))
        }
        VisibilityMode::OverlapDenylist => {
            if process_rule {
                let name = process_name?;
                retain_without_process(&mut rules, name);
                // The process rule denied every sibling. Keep those denials
                // as title rules, except for the clicked window.
                for (index, sibling) in group.windows.iter().enumerate() {
                    if index != window_index
                        && !sibling.title.is_empty()
                        && !rules.iter().any(|rule| {
                            rule.process_name.is_none()
                                && rule
                                    .title_pattern
                                    .as_deref()
                                    .is_some_and(|p| p.eq_ignore_ascii_case(&sibling.title))
                        })
                    {
                        rules.push(OverlapRule {
                            process_name: None,
                            title_pattern: Some(sibling.title.clone()),
                        });
                    }
                }
            } else if checked {
                if !rules.iter().any(&exact_title) {
                    rules.push(OverlapRule {
                        process_name: None,
                        title_pattern: Some(title.to_string()),
                    });
                }
            } else if rules.iter().any(&exact_title) {
                rules.retain(|rule| !exact_title(rule));
            } else {
                return None;
            }
            if rules.is_empty() {
                Some(with_rules(visibility, VisibilityMode::Always, rules))
            } else {
                Some(with_rules(
                    visibility,
                    VisibilityMode::OverlapDenylist,
                    rules,
                ))
            }
        }
        _ => None,
    }
}

/// Точное состояние независимого признака «Рабочий стол» для первой строки.
pub fn desktop_only_is_checked(visibility: &VisibilityRule) -> bool {
    visibility.desktop
}

/// Переключить только видимость на чистом рабочем столе, не меняя правила
/// окон. Это отдельный признак, поэтому выбранные окна не теряются.
pub fn toggle_desktop_only(visibility: &VisibilityRule) -> VisibilityRule {
    let mut updated = visibility.clone();
    updated.desktop = !updated.desktop;
    updated
}

/// Собрать новое правило, сохранив независимую настройку рабочего стола.
fn with_rules(
    visibility: &VisibilityRule,
    mode: VisibilityMode,
    rules: Vec<OverlapRule>,
) -> VisibilityRule {
    VisibilityRule {
        mode,
        rules,
        desktop: visibility.desktop,
    }
}

/// В списке есть правило ровно на этот процесс (по имени exe, без шаблона
/// заголовка)?
fn has_process_rule(rules: &[OverlapRule], name: &str) -> bool {
    rules.iter().any(|r| is_process_rule(r, name))
}

/// Убрать из списка правило ровно на этот процесс.
fn retain_without_process(rules: &mut Vec<OverlapRule>, name: &str) {
    rules.retain(|r| !is_process_rule(r, name));
}

/// Правило — это «весь процесс `name`»: имя exe без шаблона заголовка.
/// Title-правило конкретного окна процесс целиком не описывает.
fn is_process_rule(rule: &OverlapRule, name: &str) -> bool {
    rule.title_pattern.is_none()
        && rule
            .process_name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
}

/// «Выбрать все» — переключатель (SPEC.md §4.2, дословно): если выбрано не
/// всё — выбрать всё; если всё — снять выделение полностью (дизайн §4).
///
/// «Выбрано всё» = каждое выразимое окно снимка матчится правилом; строки
/// protected process (без чекбокса по определению) в подсчёте не участвуют.
///
/// «Выбрать все» даёт `{Always, [], desktop}` — то самое «список целей: все» из
/// SPEC 4.1, а не перечисление. Раньше здесь материализовался снимок: на
/// каждый запущенный процесс по правилу. Такой список описывал не «все
/// окна», а «окна, запущенные в момент нажатия», и первое же новое окно
/// оказывалось не выбранным — после перезагрузки компьютера это ВСЕ окна
/// сразу, потому что процессы поднимаются заново, а часть приложений
/// добавляется (репорт пользователя 2026-09-02: «перезапустил комп — стикеры
/// отображаются только между некоторыми окнами»).
///
/// Снятие — пустой allow-list: режим обязательно `OverlapAllowlist`, в
/// `Always` пустой список означал бы «выбраны все» (см. [`window_is_checked`]),
/// то есть кнопка не делала бы ничего.
pub fn toggle_select_all(visibility: &VisibilityRule, snapshot: &[WindowInfo]) -> VisibilityRule {
    let all_checked = snapshot
        .iter()
        .all(|w| !window_can_express_rule(w) || window_is_checked(visibility, w));
    let mode = if all_checked {
        VisibilityMode::OverlapAllowlist
    } else {
        VisibilityMode::Always
    };
    VisibilityRule {
        mode,
        rules: Vec::new(),
        desktop: visibility.desktop,
    }
}

/// Развернуть «все, кроме» в список разрешений по снимку окон.
///
/// Нужно ровно одному потребителю — правилам соседства ЗАКРЕПЛЁННОГО окна
/// ([`rst_core::pinned_window::HostFilter`]), где формы «везде, кроме» нет:
/// там список хозяев либо отсутствует целиком (`Anywhere`), либо перечисляет
/// разрешённых (`Only`). Снимок здесь протухает так же, как протухал у
/// «Выбрать все», но правила соседства — рантайм-состояние конкретного
/// `HWND`: они не переживают ни перезапуск программы, ни тем более
/// перезагрузку компьютера, и портиться со временем им негде.
///
/// Все прочие режимы возвращаются как есть.
pub fn denylist_as_allowlist(
    visibility: &VisibilityRule,
    snapshot: &[WindowInfo],
) -> VisibilityRule {
    if visibility.mode != VisibilityMode::OverlapDenylist {
        return visibility.clone();
    }
    let mut rules = Vec::new();
    // The overlay's own process is never a host window.  Real snapshots
    // normally exclude it by PID, but synthetic/test snapshots can carry a
    // different PID while retaining the executable name; materializing an
    // allow-list rule for it would make host filtering depend on the probe.
    let own_process_name = std::env::current_exe().ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    for group in group_by_process(snapshot) {
        match &group.process_name {
            Some(name) => {
                // A title-only deny rule can cover every window in a process
                // group.  Such a group is not an allowed host and must not
                // be reintroduced as a process rule during conversion.
                if group
                    .windows
                    .iter()
                    .all(|window| !window_is_checked(visibility, window))
                {
                    continue;
                }
                if own_process_name
                    .as_deref()
                    .is_some_and(|own| name.eq_ignore_ascii_case(own))
                {
                    continue;
                }
                if !has_process_rule(&visibility.rules, name) {
                    rules.push(OverlapRule {
                        process_name: Some(name.clone()),
                        title_pattern: None,
                    });
                }
            }
            None => {
                for w in &group.windows {
                    if w.title.is_empty() || !window_is_checked(visibility, w) {
                        continue;
                    }
                    rules.push(OverlapRule {
                        process_name: None,
                        title_pattern: Some(w.title.clone()),
                    });
                }
            }
        }
    }
    VisibilityRule {
        mode: VisibilityMode::OverlapAllowlist,
        rules,
        desktop: visibility.desktop,
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
pub const PICKER_ROW_DESKTOP: WidgetId = 0x400;
/// Полоса скролла списка (живой репорт пользователя: длинный список окон
/// обрезался без видимого намёка, что его можно листать колесом мыши —
/// `overlay_manager::handle_input`, `InputEvent::MouseWheel`).
const PICKER_SCROLLBAR_ID: WidgetId = 203;

/// Подпись кнопки-пресета «Только рабочий стол» — постоянна, не зависит от
/// состояния списка правил (в отличие от подписи переключателя «Выбрать
/// все»/«Снять все»).
pub const DESKTOP_ONLY_LABEL: &str = "Рабочий стол";

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
/// Флаг невидимой полосы наведения во всю ширину строки.
///
/// Без неё раскрытие начиналось, только когда курсор пересекал подпись или
/// чекбокс: между ними строка была «мёртвой», и человек, ведя мышь по списку,
/// видел паузу на ровном месте — «навожусь и оно ждёт около 0.7 секунды»
/// (репорт пользователя 2026-09-13). Полоса добавляется ПЕРВОЙ в строке, а
/// `Panel::top_at` берёт последний подходящий виджет — поэтому клики
/// по-прежнему достаются чекбоксу и подписи, а ей — только наведение.
const HOVER_FLAG: WidgetId = 0x4000_0000;

/// Вернуть индекс приложения по наведённой части его строки.
///
/// Наведение считается по ЛЮБОЙ части строки — подписи, чекбоксу, полосе
/// наведения — и по строкам окон этого же приложения: пока курсор внутри
/// секции, она обязана оставаться раскрытой.
pub fn hovered_process_group(id: WidgetId) -> Option<usize> {
    // Флаги — это части одной строки, а не разные строки: снимаем и считаем
    // по «голому» идентификатору.
    let id = if id >= LABEL_FLAG {
        id - LABEL_FLAG
    } else if id >= HOVER_FLAG {
        id - HOVER_FLAG
    } else {
        id
    };
    if (PICKER_ROW_PROCESS_BASE..PICKER_ROW_WINDOW_BASE).contains(&id) {
        return Some((id - PICKER_ROW_PROCESS_BASE) as usize);
    }
    if id >= PICKER_ROW_WINDOW_BASE {
        return Some(((id - PICKER_ROW_WINDOW_BASE) >> 16) as usize);
    }
    None
}

/// Сколько строк списка влезает в панель (виртуализация, дизайн §7.4):
/// билдер строит только строки `[scroll, scroll + PICKER_VISIBLE_ROWS)`.
pub const PICKER_VISIBLE_ROWS: usize = 10;
/// Ширина панели, DIP.
pub const PICKER_WIDTH: f64 = 320.0;
/// Высота шапки (кнопка «Выбрать все»/«Снять все»), DIP.
pub const PICKER_HEADER_H: f64 = 40.0;
/// Высота строки списка, DIP (равна `theme::BUTTON_SIZE` — колонка иконок
/// проектировалась квадратом этого размера, дизайн §6).
pub const PICKER_ROW_H: f64 = theme::BUTTON_SIZE;
/// Внутренний отступ корпуса, DIP (§3 `PAD_PANEL`): панель — большой корпус,
/// поэтому берём токен спецификации, а не локальное число — панели должны
/// дышать одинаково.
pub const PICKER_PAD: f64 = theme::PAD_PANEL;
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

/// Состояние раскрытия одной группы. Дробная фаза нужна для плавного
/// появления дочерних строк и не влияет на правила видимости.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PickerLayout {
    pub expanded_group: Option<usize>,
    pub expansion: f64,
}

/// Задержка перед сворачиванием, выбранная в диапазоне 150–250 мс из
/// брифа: короткий grace period не рвёт accordion, когда курсор проходит от
/// строки приложения к дочернему окну.
pub const ACCORDION_COLLAPSE_DELAY_MS: f64 = 200.0;

/// Фаза дочерней строки с учётом stagger-задержки существующей motion-системы.
pub fn accordion_child_progress(expansion: f64, child_index: usize) -> f64 {
    let timeline = CARD_DURATION_MS + (5.0 * STAGGER_STEP_MS);
    let expansion = expansion.clamp(0.0, 1.0);
    let delay = stagger_delay_ms(child_index).min(5.0 * STAGGER_STEP_MS) / timeline;
    // `ease_out` is intended for complete UI phases but collapses this very
    // short stagger interval to its endpoint on the current backend. Keep a
    // local cubic ease-out so two adjacent children remain observably
    // ordered throughout the reveal.
    let t = ((expansion - delay) / (1.0 - delay)).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Собрать панель выбора окон. `visibility` — текущее правило стикера
/// (состояние чекбоксов живёт в нём, не в панели — дизайн §2.1), `snapshot` —
/// снимок окон, `scroll` — сколько строк списка пропустить сверху
/// (виртуализация §7.4: строятся только видимые строки, `Panel` не трогается),
/// `frame` — рамка панели (полностью определяет вызывающий слой; типовой
/// размер — [`PICKER_WIDTH`]×[`PICKER_HEIGHT`]).
///
/// Шапка с переключателем «Выбрать все»/«Снять все» видима всегда; скролл
/// сдвигает только список под ней. Пресет «Рабочий стол» — первая строка
/// списка, а не отдельная кнопка. Чекбоксы приложений и раскрытых окон
/// кликабельны; невыразимые окна остаются disabled.
///
/// Только для тестов: рабочий код собирает панель через
/// [`build_picker_panel_with_layout`], которому раскладка аккордеона нужна
/// целиком. Обёртка с раскладкой по умолчанию остаётся ради читаемости
/// тестов, где раскладка к делу не относится.
#[cfg(test)]
pub fn build_picker_panel(
    visibility: &VisibilityRule,
    snapshot: &[WindowInfo],
    scroll: usize,
    frame: Box2D,
    desktop_preset: bool,
) -> PickerPanel {
    build_picker_panel_impl(
        visibility,
        snapshot,
        scroll,
        frame,
        desktop_preset,
        PickerLayout::default(),
    )
}

/// Собрать панель с заданной фазой раскрытия группы.
pub fn build_picker_panel_with_layout(
    visibility: &VisibilityRule,
    snapshot: &[WindowInfo],
    scroll: usize,
    frame: Box2D,
    desktop_preset: bool,
    layout: PickerLayout,
) -> PickerPanel {
    build_picker_panel_impl(visibility, snapshot, scroll, frame, desktop_preset, layout)
}

fn build_picker_panel_impl(
    visibility: &VisibilityRule,
    snapshot: &[WindowInfo],
    scroll: usize,
    frame: Box2D,
    desktop_preset: bool,
    layout: PickerLayout,
) -> PickerPanel {
    let mut panel = Panel::new(PICKER_PANEL_ID, frame).with_corner_radius(theme::RADIUS_WINDOW);
    let left = frame.cx - frame.w / 2.0 + PICKER_PAD;
    let top = frame.cy - frame.h / 2.0;

    // Шапка: «Выбрать все» — переключатель (SPEC §4.2); надпись — предстоящее
    // действие («выбрать» или «снять»), как BTN_TOGGLE_ALL в cursor_panel.rs.
    let all_checked = all_windows_checked(visibility, snapshot);
    let toggle_label = if all_checked {
        "Clear all"
    } else {
        "Select all"
    };
    let header_y = top + PICKER_PAD + PICKER_HEADER_H / 2.0;
    // Кнопка в шапке осталась одна: пресет «Рабочий стол» переехал в первую
    // строку списка (репорт пользователя 2026-09-12), и перебор по списку из
    // одного элемента только прятал бы это.
    let (tw, _) = text_size(toggle_label);
    // Ширина кнопки — подпись плюс горизонтальные отступы §3 (`PAD_CTRL_X`).
    // Прежнее магическое 8.0 было локальным падом; у кнопки с подписью
    // отступ задаёт спецификация, а `BUTTON_PAD` (4) — это отступ иконки.
    let btn_w = tw + 2.0 * theme::PAD_CTRL_X;
    panel.add_widget(Button::new(
        PICKER_BTN_SELECT_ALL,
        Box2D {
            cx: left + btn_w / 2.0,
            cy: header_y,
            w: btn_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(toggle_label.to_string()),
    ));

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
    // Прямоугольник строки целиком — для полосы наведения.
    let row_box = |cy: f64| Box2D {
        cx: (left + right_edge) / 2.0,
        cy,
        w: (right_edge - left).max(0.0),
        h: PICKER_ROW_H,
        rotation: 0.0,
    };

    // Список: первая строка — «Рабочий стол». Группа из одного окна занимает
    // одну строку приложения; дочерние окна многоконной группы появляются
    // только у раскрытой группы.
    let mut row = 0usize;
    let mut built = 0usize;
    // Дробная позиция следующей строки в единицах высоты строки. Целые
    // `row`/`built` остаются логическими (скролл, виртуализация, порядок
    // попаданий), а рисуется всё по `slot` — иначе раскрытие секции было бы
    // мгновенным скачком вместо роста.
    let mut slot = 0.0_f64;
    if desktop_preset {
        if row >= scroll && built < PICKER_VISIBLE_ROWS {
            let cy = row_cy_at(list_top, slot);
            slot += 1.0;
            panel.add_widget(Checkbox::standard(
                PICKER_ROW_DESKTOP,
                process_cb_cx,
                cy,
                desktop_only_is_checked(visibility),
            ));
            panel.add_widget(RowLabel::new(
                PICKER_ROW_DESKTOP + LABEL_FLAG,
                text_rect(process_label_left, cy, DESKTOP_ONLY_LABEL),
                icon_rect(icon_cx, cy),
                DESKTOP_ONLY_LABEL.to_string(),
                None,
            ));
            built += 1;
        }
        row += 1;
    }
    for (g, group) in groups.iter().enumerate() {
        if row >= scroll && built < PICKER_VISIBLE_ROWS {
            let cy = row_cy_at(list_top, slot);
            slot += 1.0;
            panel.add_widget(RowHover {
                id: PICKER_ROW_PROCESS_BASE + g as WidgetId + HOVER_FLAG,
                bounds: row_box(cy),
            });
            let row_id = if group.process_name.is_some() {
                PICKER_ROW_PROCESS_BASE + g as WidgetId
            } else if group.windows.len() == 1 {
                PICKER_ROW_WINDOW_BASE + ((g as WidgetId) << 16)
            } else {
                0
            };
            if row_id != 0 {
                let mut cb = Checkbox::standard(
                    row_id,
                    process_cb_cx,
                    cy,
                    if group.process_name.is_some() && group.windows.len() > 1 {
                        process_is_checked(visibility, group)
                    } else {
                        window_is_checked(visibility, &group.windows[0])
                    },
                );
                cb.set_disabled(
                    group.process_name.is_none()
                        && (!window_can_express_rule(&group.windows[0])
                            || group.windows[0].title.is_empty()),
                );
                panel.add_widget(cb);
            }
            let text = match &group.process_name {
                Some(name) if group.windows.len() == 1 => name.clone(),
                Some(name) => format!("{name} ({})", group.windows.len()),
                None if group.windows.len() == 1 => group.windows[0].title.clone(),
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
        if group.windows.len() > 1 && layout.expanded_group == Some(g) {
            for (w, window) in group.windows.iter().enumerate() {
                if row >= scroll && built < PICKER_VISIBLE_ROWS {
                    let child_progress = accordion_child_progress(layout.expansion, w);
                    // Строка занимает ровно `child_progress` высоты строки:
                    // секция растёт, и всё, что ниже, разъезжается вместе с
                    // ней. Сама строка при этом выезжает из-под своего
                    // приложения — поэтому её центр поднят на недостающую
                    // часть высоты, а не просто смещён на четверть.
                    let cy = row_cy_at(list_top, slot) - (1.0 - child_progress) * PICKER_ROW_H;
                    slot += child_progress;
                    // Боковой сдвиг и проявление: без обрезки содержимого это
                    // единственный способ показать, что строка ПРИЕХАЛА.
                    let slide = (1.0 - child_progress) * ACCORDION_SLIDE_DIP;
                    let id = PICKER_ROW_WINDOW_BASE + ((g as WidgetId) << 16) + w as WidgetId;
                    // Курсор внутри секции держит её раскрытой, даже если он
                    // идёт по пустому месту между чекбоксом и подписью.
                    panel.add_widget(RowHover {
                        id: id + HOVER_FLAG,
                        bounds: row_box(cy),
                    });
                    let mut cb = Checkbox::standard(
                        id,
                        window_cb_cx + slide,
                        cy,
                        window_is_checked(visibility, window),
                    );
                    cb.set_disabled(
                        !window_can_express_rule(window)
                            || window.title.is_empty()
                            || child_progress < 0.5,
                    );
                    cb.set_fade(child_progress);
                    panel.add_widget(cb);
                    let title = truncate_to_width(&window.title, window_label_max_w);
                    let window_icon = window
                        .icon
                        .clone()
                        .map(|icon| (icon_key(&window.exe_path), icon));
                    panel.add_widget(
                        RowLabel::new(
                            id + LABEL_FLAG,
                            text_rect(window_label_left + slide, cy, &title),
                            icon_rect(icon_cx + slide, cy),
                            title,
                            window_icon,
                        )
                        .with_fade(child_progress),
                    );
                    built += 1;
                }
                row += 1;
            }
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

/// Центр строки списка по Y; позиция задаётся ДРОБНЫМ числом строк, счёт
/// идёт только по строкам, попавшим в окно скролла.
///
/// Без дробной позиции аккордеон не читается как аккордеон: строки ниже
/// раскрытого приложения должны разъезжаться постепенно, освобождая место, а
/// не прыгать на готовое место в первом же кадре (репорт пользователя
/// 2026-09-13: «0 анимаций… резко открывается»).
fn row_cy_at(list_top: f64, slot: f64) -> f64 {
    list_top + slot * PICKER_ROW_H + PICKER_ROW_H / 2.0
}

/// Насколько строка окна выезжает вбок, пока появляется, DIP.
///
/// Боковой сдвиг вместе с проявлением — то, что делает появление видимым без
/// обрезки содержимого: строка выходит из-под своего приложения, а не
/// возникает на месте. Величина заметная, но не карикатурная.
const ACCORDION_SLIDE_DIP: f64 = 14.0;

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
/// [`Primitive::Rgba`] или плита стекла-плейсхолдер — решает `RowLabel::draw`).
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
/// плита стекла-плейсхолдер) и текст (заголовок окна / имя процесса).
/// В toolbar.rs/cursor_panel.rs текстовых виджетов нет (подписи живут
/// внутри кнопок) — поэтому мини-виджет приватный здесь. Строка участвует
/// только в hover-хиттесте для accordion; действие выполняет чекбокс.
struct RowLabel {
    id: WidgetId,
    text_rect: Box2D,
    icon_rect: Box2D,
    /// Общая непрозрачность 0..1: строка окна не возникает готовой, а
    /// проявляется по мере раскрытия секции (репорт пользователя 2026-09-13:
    /// «0 анимаций»).
    fade: f64,
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
            fade: 1.0,
            text,
            icon,
        }
    }

    /// Проявить строку на `fade` (0 — невидима, 1 — обычный вид).
    fn with_fade(mut self, fade: f64) -> Self {
        self.fade = fade.clamp(0.0, 1.0);
        self
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

    fn hit_test(&self, pos: (f64, f64)) -> bool {
        box_contains(&self.text_rect, pos)
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        if self.fade <= 0.0 {
            return;
        }
        match &self.icon {
            Some((key, icon)) => out.push(Primitive::Rgba {
                rect: self.icon_rect,
                key: *key,
                width: icon.width,
                height: icon.height,
                rgba: icon.rgba.clone(),
                opacity: self.fade,
            }),
            None => glass_control(
                out,
                self.icon_rect,
                theme::RADIUS_TIGHT,
                0.0,
                0.0,
                false,
                self.fade,
            ),
        }
        if !self.text.is_empty() {
            out.push(Primitive::Text {
                rect: self.text_rect,
                text: self.text.clone(),
                color: theme::TEXT,
                // Основной текст UI (§6) — белый `TEXT` при непрозрачности §2.3:
                // приглушение теперь только альфой, отдельных серых цветов нет.
                opacity: theme::TEXT_OPACITY * self.fade,
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

/// Невидимая полоса во всю ширину строки: ловит наведение и больше ничего.
///
/// Ничего не рисует и ничего не делает по клику — она существует только
/// чтобы строка считалась «под курсором» целиком, а не одной лишь подписью.
/// Добавляется в строку ПЕРВОЙ: `Panel::top_at` берёт последний подходящий
/// виджет, поэтому чекбокс и подпись по-прежнему получают клики.
struct RowHover {
    id: WidgetId,
    bounds: Box2D,
}

impl Widget for RowHover {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn hit_test(&self, pos: (f64, f64)) -> bool {
        box_contains(&self.bounds, pos)
    }

    fn draw(&self, _out: &mut Vec<Primitive>) {}

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
    use rst_render::glass::Surface;
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
            ..Default::default()
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
            ..Default::default()
        };
        assert!(window_is_checked(&always, &w), "Always — все окна выбраны");
        assert!(process_is_checked(&always, &group), "и все процессы");

        for mode in [VisibilityMode::Desktop, VisibilityMode::NeverOverlap] {
            let v = VisibilityRule {
                mode,
                rules: vec![rule(Some("app.exe"), None)],
                ..Default::default()
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

    /// Снятие галочки из состояния «выбраны все» (`Always`) записывает
    /// ИСКЛЮЧЕНИЕ, а не список из всех остальных процессов снимка: «все
    /// остальные» обязаны остаться определением, иначе правило описывает
    /// лишь те окна, что были запущены в секунду клика (репорт пользователя
    /// 2026-09-02).
    #[test]
    fn toggle_process_from_always_records_an_exception() {
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
            ..Default::default()
        };
        let off = toggle_process_group(&v, &group).unwrap();
        assert_eq!(off.mode, VisibilityMode::OverlapDenylist);
        assert_eq!(
            off.rules,
            vec![rule(Some("app.exe"), None)],
            "в списке — ровно снятый процесс"
        );
        assert!(!process_is_checked(&off, &group), "галочка снята");
        assert!(
            window_is_checked(&off, &snapshot[1]),
            "все прочие остаются выбранными"
        );
        // И, главное, приложение, которого в снимке не было вовсе.
        let newcomer = window(3, r"C:\Apps\launched-later.exe", "t3", 3, 3);
        assert!(
            window_is_checked(&off, &newcomer),
            "окно, открытое позже, обязано остаться выбранным"
        );
    }

    /// Возврат галочки на место убирает исключение, и правило снова
    /// становится честным «все» — а не списком из двух процессов, которые
    /// оказались запущены.
    #[test]
    fn toggle_process_back_returns_to_always() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let group = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![snapshot[0].clone()],
        };
        let off = toggle_process_group(
            &VisibilityRule {
                mode: VisibilityMode::Always,
                rules: vec![],
                ..Default::default()
            },
            &group,
        )
        .unwrap();
        let on = toggle_process_group(&off, &group).unwrap();
        assert_eq!(on.mode, VisibilityMode::Always);
        assert!(on.rules.is_empty());
    }

    /// Второе исключение ложится рядом с первым, а не заменяет его.
    #[test]
    fn exceptions_accumulate() {
        let a = ProcessGroup {
            process_name: Some("app.exe".to_string()),
            windows: vec![window(1, r"C:\Apps\app.exe", "t", 1, 1)],
        };
        let b = ProcessGroup {
            process_name: Some("other.exe".to_string()),
            windows: vec![window(2, r"C:\Apps\other.exe", "t2", 2, 2)],
        };
        let all = VisibilityRule {
            mode: VisibilityMode::Always,
            rules: vec![],
            ..Default::default()
        };
        let one = toggle_process_group(&all, &a).unwrap();
        let two = toggle_process_group(&one, &b).unwrap();
        assert_eq!(two.mode, VisibilityMode::OverlapDenylist);
        assert_eq!(
            two.rules,
            vec![rule(Some("app.exe"), None), rule(Some("other.exe"), None)]
        );
        // Снятие одного из двух оставляет режим исключений.
        let back = toggle_process_group(&two, &a).unwrap();
        assert_eq!(back.mode, VisibilityMode::OverlapDenylist);
        assert_eq!(back.rules, vec![rule(Some("other.exe"), None)]);
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
            ..Default::default()
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
            ..Default::default()
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

    /// «Выбрать все» — это режим «все» (SPEC 4.1, «список целей: все»), а не
    /// перечисление запущенных процессов.
    #[test]
    fn select_all_means_the_mode_not_a_snapshot() {
        let snapshot = [
            window(1, r"C:\Apps\alpha.exe", "t", 10, 1),
            window(2, r"C:\Apps\zebra.exe", "t", 20, 2),
            window(3, "", "Настройки", 30, 3),
        ];
        let v = allowlist(vec![]);
        let all = toggle_select_all(&v, &snapshot);
        assert_eq!(all.mode, VisibilityMode::Always);
        assert!(all.rules.is_empty(), "перечислять нечего — выбраны все");
        assert!(snapshot.iter().all(|w| window_is_checked(&all, w)));
    }

    /// Регрессия репорта 2026-09-02: после «выбрать все» и перезагрузки
    /// компьютера окна поднимаются заново, часть приложений добавляется —
    /// и все они обязаны остаться выбранными.
    #[test]
    fn select_all_still_covers_windows_that_did_not_exist_yet() {
        let snapshot = [window(1, r"C:\Apps\alpha.exe", "t", 10, 1)];
        let all = toggle_select_all(&allowlist(vec![]), &snapshot);
        // Тот же процесс, но окно пересоздано после перезагрузки — другой
        // HWND, другой заголовок; и совсем новое приложение рядом.
        let after_reboot = [
            window(99, r"C:\Apps\alpha.exe", "другой заголовок", 10, 1),
            window(100, r"C:\Apps\installed-later.exe", "t", 20, 2),
            window(101, "", "Окно защищённого процесса", 30, 3),
        ];
        assert!(
            after_reboot.iter().all(|w| window_is_checked(&all, w)),
            "после перезагрузки выбранными обязаны остаться все окна"
        );
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
        // Выбрано не всё (zebra без правила) — «выбрать всё» заменяет
        // частичный список режимом «все».
        let v = allowlist(vec![rule(Some("alpha.exe"), None)]);
        let all = toggle_select_all(&v, &snapshot);
        assert_eq!(all.mode, VisibilityMode::Always);
        assert!(all.rules.is_empty());
        assert!(snapshot.iter().all(|w| window_is_checked(&all, w)));
    }

    /// Разворачивание «все, кроме» в список разрешений — только для правил
    /// соседства закреплённого окна, у которых такой формы нет.
    #[test]
    fn denylist_expands_to_everything_else_in_the_snapshot() {
        let snapshot = [
            window(1, r"C:\Apps\alpha.exe", "t", 10, 1),
            window(2, r"C:\Apps\zebra.exe", "t", 20, 2),
            window(3, "", "Настройки", 30, 3),
        ];
        let denied = VisibilityRule {
            mode: VisibilityMode::OverlapDenylist,
            rules: vec![rule(Some("alpha.exe"), None)],
            ..Default::default()
        };
        let expanded = denylist_as_allowlist(&denied, &snapshot);
        assert_eq!(expanded.mode, VisibilityMode::OverlapAllowlist);
        assert_eq!(
            expanded.rules,
            vec![rule(Some("zebra.exe"), None), rule(None, Some("Настройки"))],
            "остаются все, кроме исключённого"
        );
        // Прочие режимы функция не трогает.
        let untouched = allowlist(vec![rule(Some("alpha.exe"), None)]);
        assert_eq!(denylist_as_allowlist(&untouched, &snapshot), untouched);
    }

    #[test]
    fn denylist_of_the_last_available_process_becomes_empty_allowlist() {
        let snapshot = [window(1, r"C:\Apps\chrome.exe", "Chrome", 1, 1)];
        let denied = VisibilityRule {
            mode: VisibilityMode::OverlapDenylist,
            rules: vec![rule(Some("chrome.exe"), None)],
            ..Default::default()
        };
        let expanded = denylist_as_allowlist(&denied, &snapshot);
        assert_eq!(expanded.mode, VisibilityMode::OverlapAllowlist);
        assert!(expanded.rules.is_empty());
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
            ..Default::default()
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

    // --- независимый признак «рабочий стол» ---

    #[test]
    fn desktop_row_can_be_toggled_without_losing_two_window_rules() {
        let a = window(1, r"C:\Apps\a.exe", "A", 100, 1);
        let b = window(2, r"C:\Apps\b.exe", "B", 100, 2);
        let original = allowlist(vec![rule(Some("a.exe"), None), rule(Some("b.exe"), None)]);
        assert!(original.desktop);
        assert!(window_is_checked(&original, &a));
        assert!(window_is_checked(&original, &b));

        let desktop_off = toggle_desktop_only(&original);
        assert!(!desktop_off.desktop, "снята только строка рабочего стола");
        assert_eq!(desktop_off.mode, original.mode);
        assert_eq!(desktop_off.rules, original.rules);
        assert!(window_is_checked(&desktop_off, &a));
        assert!(window_is_checked(&desktop_off, &b));

        let desktop_on = toggle_desktop_only(&desktop_off);
        assert_eq!(
            desktop_on, original,
            "повторное включение возвращает состояние"
        );
    }

    #[test]
    fn desktop_only_and_empty_selection_are_distinct_valid_states() {
        let only_desktop = allowlist(vec![]);
        assert!(desktop_only_is_checked(&only_desktop));
        let nothing = toggle_desktop_only(&only_desktop);
        assert!(!nothing.desktop);
        assert!(nothing.rules.is_empty());
        assert_eq!(toggle_desktop_only(&nothing), only_desktop);
    }

    #[test]
    fn select_all_does_not_change_independent_desktop_selection() {
        let snapshot = [
            window(1, r"C:\Apps\a.exe", "A", 100, 1),
            window(2, r"C:\Apps\b.exe", "B", 100, 2),
        ];
        let mut desktop_off = allowlist(vec![]);
        desktop_off.desktop = false;
        let all = toggle_select_all(&desktop_off, &snapshot);
        assert_eq!(all.mode, VisibilityMode::Always);
        assert!(!all.desktop, "шапка не меняет строку рабочего стола");
        assert!(snapshot.iter().all(|w| window_is_checked(&all, w)));
    }

    #[test]
    fn always_mode_remains_visible_even_when_desktop_flag_is_off() {
        let always = VisibilityRule {
            desktop: false,
            ..VisibilityRule::default()
        };
        let w = window(1, r"C:\Apps\app.exe", "A", 100, 1);
        assert!(window_is_checked(&always, &w));
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

    /// Сколько слотов-плейсхолдеров иконок: плиты стекла `Surface::Control`
    /// размера `PICKER_ICON_SIZE` (в покое `glass_control` не ужимает rect —
    /// фильтр по точному квадрату работает).
    fn glass_slot_count(panel: &Panel) -> usize {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter(|p| {
                matches!(p, Primitive::Glass { rect, surface: Surface::Control, .. }
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
        assert_eq!(p.total_rows, 1);
        assert!(p.panel.widget::<Button>(PICKER_BTN_SELECT_ALL).is_some());
        // Пустой снимок: «всё выбрано» (пустое «все» истинно) — кнопка
        // предлагает снять; пресет — первой строкой списка.
        assert_eq!(
            picker_texts(&p.panel),
            vec!["Clear all".to_string(), DESKTOP_ONLY_LABEL.to_string()]
        );
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .is_none()
        );
        assert_eq!(glass_slot_count(&p.panel), 1);
    }

    #[test]
    fn process_group_and_unknown_window_row_structure() {
        let snapshot = [
            window(1, r"C:\Apps\chrome.exe", "Chrome", 100, 1),
            window(2, "", "Настройки", 200, 2),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        // Строки: рабочий стол, chrome-процесс, неизвестное окно.
        assert_eq!(p.total_rows, 3);

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
        let texts = picker_texts(&p.panel);
        for expected in [DESKTOP_ONLY_LABEL, "chrome.exe", "Настройки"] {
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
        assert!(proc_cx > 0.0);
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
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .unwrap()
                .checked()
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
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .unwrap()
                .checked()
        );
    }

    #[test]
    fn hovering_anywhere_on_an_app_row_counts_as_hovering_that_app() {
        // Раскрытие начиналось, только когда курсор пересекал подпись: между
        // чекбоксом и текстом строка была мёртвой, и человек видел паузу на
        // ровном месте («навожусь и оно ждёт около 0.7 секунды», 2026-09-13).
        let snapshot = [
            window(1, r"C:\Appslpha.exe", "первое", 1, 1),
            window(2, r"C:\Appslpha.exe", "второе", 1, 2),
        ];
        let p = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: None,
                expansion: 0.0,
            },
        );
        let row = p
            .panel
            .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
            .expect("строка приложения")
            .bounds();
        // Точка справа от подписи — там, где раньше не было ничего.
        let far_right = (
            picker_frame().cx + picker_frame().w / 2.0
                - PICKER_PAD
                - theme::SCROLLBAR_WIDTH
                - PICKER_GAP
                - 2.0,
            row.cy,
        );
        let mut p = p;
        p.panel
            .pointer_event(rst_render::PointerEvent::Move { pos: far_right });
        let hit = p
            .panel
            .hovered_widget()
            .map(|(id, _)| id)
            .expect("строка ловит наведение");
        assert_eq!(
            hovered_process_group(hit),
            Some(0),
            "наведение на любую часть строки означает наведение на приложение"
        );
    }

    #[test]
    fn expanding_section_pushes_the_rows_below_it_down_gradually() {
        // Суть аккордеона: место под строки окон освобождается ПОСТЕПЕННО, и
        // всё, что ниже, съезжает вместе с ним. Пока строки просто возникали
        // на готовом месте, раскрытие читалось как мгновенный скачок — и
        // пользователь честно назвал это «0 анимаций» (2026-09-13).
        let snapshot = [
            window(1, r"C:\Appslpha.exe", "первое", 1, 1),
            window(2, r"C:\Appslpha.exe", "второе", 1, 2),
            window(3, r"C:\Appseta.exe", "чужое", 2, 3),
        ];
        let below_at = |expansion: f64| -> f64 {
            let p = build_picker_panel_with_layout(
                &allowlist(vec![]),
                &snapshot,
                0,
                picker_frame(),
                true,
                PickerLayout {
                    expanded_group: Some(0),
                    expansion,
                },
            );
            // Строка приложения `beta.exe` идёт ПОСЛЕ раскрываемой секции.
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE + 1)
                .expect("строка второго приложения")
                .bounds()
                .cy
        };
        let closed = below_at(0.0);
        let half = below_at(0.5);
        let open = below_at(1.0);
        assert!(
            half > closed && open > half,
            "строка ниже секции обязана ехать вниз вместе с её ростом:              {closed} → {half} → {open}"
        );
    }

    #[test]
    fn a_child_row_arrives_instead_of_appearing_ready() {
        // Строка окна выезжает из-под своего приложения и проявляется: в
        // начале раскрытия она невидима и смещена, в конце — на месте и
        // непрозрачна. Без этого «анимация» сводилась к сдвигу на четверть
        // строки при полной непрозрачности, то есть была незаметна.
        let snapshot = [
            window(1, r"C:\Appslpha.exe", "первое", 1, 1),
            window(2, r"C:\Appslpha.exe", "второе", 1, 2),
        ];
        let child_at = |expansion: f64| -> (f64, f64) {
            let p = build_picker_panel_with_layout(
                &allowlist(vec![]),
                &snapshot,
                0,
                picker_frame(),
                true,
                PickerLayout {
                    expanded_group: Some(0),
                    expansion,
                },
            );
            let cb = p
                .panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
                .expect("первая строка окна");
            (cb.bounds().cx, cb.bounds().cy)
        };
        let (start_x, start_y) = child_at(0.0);
        let (mid_x, mid_y) = child_at(0.5);
        let (end_x, end_y) = child_at(1.0);
        assert!(
            start_x > end_x && mid_x > end_x,
            "строка приезжает сбоку: {start_x} → {mid_x} → {end_x}"
        );
        assert!(
            start_y < mid_y && mid_y < end_y,
            "и одновременно опускается на своё место: {start_y} → {mid_y} → {end_y}"
        );
    }

    #[test]
    fn expanded_window_checkboxes_are_clickable_when_expressible() {
        let snapshot = [
            window(1, r"C:\Apps\app.exe", "t", 1, 1),
            window(2, r"C:\Apps\app.exe", "", 1, 2),
        ];
        let p = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
        );
        let cb = p.panel.widget::<Checkbox>(PICKER_ROW_WINDOW_BASE).unwrap();
        let b = cb.bounds();
        assert!(
            cb.hit_test((b.cx, b.cy)),
            "expressible window checkbox is clickable"
        );
        let cb = p
            .panel
            .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE + 1)
            .unwrap();
        assert!(
            !cb.hit_test((cb.bounds().cx, cb.bounds().cy)),
            "empty-title window stays disabled"
        );
    }

    #[test]
    fn intermediate_accordion_geometry_remains_hit_testable() {
        let snapshot = [
            window(1, r"C:\Apps\app.exe", "Первое окно", 1, 1),
            window(2, r"C:\Apps\app.exe", "Второе окно", 1, 2),
        ];
        let mid = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 0.7,
            },
        );
        let first = mid
            .panel
            .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
            .expect("первая строка появляется во время раскрытия");
        let second = mid
            .panel
            .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE + 1)
            .expect("вторая строка появляется во время раскрытия");
        assert!(
            first.bounds().cy < second.bounds().cy,
            "stagger сохраняет порядок строк на промежуточной фазе"
        );
        for cb in [first, second] {
            let r = cb.bounds();
            assert!(
                cb.hit_test((r.cx, r.cy)),
                "хит-тест следует за нарисованным прямоугольником"
            );
        }
    }

    #[test]
    fn protected_window_checkbox_disabled_regardless() {
        let snapshot = [window(1, "", "", 1, 1), window(2, "", "Visible", 1, 2)];
        let p = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
        );
        assert_eq!(p.total_rows, 4, "рабочий стол + группа + два окна");
        let cb = p.panel.widget::<Checkbox>(PICKER_ROW_WINDOW_BASE).unwrap();
        let b = cb.bounds();
        assert!(!cb.checked());
        assert!(
            !cb.hit_test((b.cx, b.cy)),
            "protected process: disabled всегда"
        );
        // Даже тотальное правило «*» не отмечает protected-окно (дизайн §2.3).
        let p2 = build_picker_panel_with_layout(
            &allowlist(vec![rule(None, Some("*"))]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
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
        // В свёрнутом состоянии строки: 0 рабочий стол, затем a, b, c.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 2, picker_frame(), true);
        assert_eq!(p.total_rows, 4);
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
        assert_eq!(glass_slot_count(&p.panel), 2, "видны b и c");
        // Скролл за пределы списка: строк нет, шапка на месте.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 4, picker_frame(), true);
        assert!(
            p.panel
                .widget::<Checkbox>(PICKER_ROW_PROCESS_BASE)
                .is_none()
        );
        assert!(p.panel.widget::<Button>(PICKER_BTN_SELECT_ALL).is_some());
        // Скролл в хвост: видна только строка c.
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 3, picker_frame(), true);
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
        assert_eq!(glass_slot_count(&p.panel), 1);
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
        // Рабочий стол + a, b и неизвестное однооконное приложение.
        assert_eq!(p.total_rows, 4);
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
        let p = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
        );
        assert_eq!(p.total_rows, 14);
        assert_eq!(
            glass_slot_count(&p.panel),
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
        let long_panel = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &long,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
        );
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
    fn desktop_row_is_first_and_uses_checkbox() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "t", 1, 1)];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        assert!(p.panel.widget::<Checkbox>(PICKER_ROW_DESKTOP).is_some());
        assert!(p.panel.widget::<Button>(PICKER_ROW_DESKTOP).is_none());
        assert_eq!(p.total_rows, 2);
        assert!(picker_texts(&p.panel).contains(&DESKTOP_ONLY_LABEL.to_string()));
    }

    #[test]
    fn desktop_row_tracks_exact_desktop_state() {
        let on = build_picker_panel(&allowlist(vec![]), &[], 0, picker_frame(), true);
        assert!(
            on.panel
                .widget::<Checkbox>(PICKER_ROW_DESKTOP)
                .unwrap()
                .checked()
        );
        // Снятая галочка задаётся ЯВНО: `..Default::default()` даёт
        // `desktop: true` — то самое значение, которое получают старые
        // конфиги, и строить им состояние «выключено» нельзя.
        let off = build_picker_panel(
            &VisibilityRule {
                mode: VisibilityMode::Always,
                rules: vec![],
                desktop: false,
            },
            &[],
            0,
            picker_frame(),
            true,
        );
        assert!(
            !off.panel
                .widget::<Checkbox>(PICKER_ROW_DESKTOP)
                .unwrap()
                .checked()
        );
    }

    #[test]
    fn individual_window_toggle_does_not_change_sibling() {
        let snapshot = [
            window(1, r"C:\Apps\obsidian.exe", "Vault A", 1, 1),
            window(2, r"C:\Apps\obsidian.exe", "Vault B", 1, 2),
        ];
        let group = &group_by_process(&snapshot)[0];
        let next = toggle_window(&VisibilityRule::default(), group, 0).unwrap();
        assert!(!window_is_checked(&next, &group.windows[0]));
        assert!(window_is_checked(&next, &group.windows[1]));
        let again = toggle_window(&next, group, 0).unwrap();
        assert!(window_is_checked(&again, &group.windows[0]));
        assert!(window_is_checked(&again, &group.windows[1]));
    }

    #[test]
    fn individual_window_rule_survives_visibility_round_trip() {
        let snapshot = [
            window(1, r"C:\Apps\obsidian.exe", "Vault A", 1, 1),
            window(2, r"C:\Apps\obsidian.exe", "Vault B", 1, 2),
        ];
        let group = &group_by_process(&snapshot)[0];
        let next = toggle_window(&VisibilityRule::default(), group, 0).unwrap();
        let encoded = serde_json::to_string(&next).unwrap();
        let reread: VisibilityRule = serde_json::from_str(&encoded).unwrap();
        assert_eq!(reread, next);
        assert!(!window_is_checked(&reread, &group.windows[0]));
        assert!(window_is_checked(&reread, &group.windows[1]));
    }

    #[test]
    fn multi_window_group_collapsed_until_layout_expands_it() {
        let snapshot = [
            window(1, r"C:\Apps\obsidian.exe", "Vault A", 1, 1),
            window(2, r"C:\Apps\obsidian.exe", "Vault B", 1, 2),
        ];
        let collapsed = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        assert_eq!(collapsed.total_rows, 2, "desktop + app row");
        assert!(
            collapsed
                .panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
                .is_none()
        );
        let expanded = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
        );
        assert_eq!(expanded.total_rows, 4);
        assert!(
            expanded
                .panel
                .widget::<Checkbox>(PICKER_ROW_WINDOW_BASE)
                .is_some()
        );
    }

    #[test]
    fn accordion_children_stagger_after_app_row() {
        assert_eq!(accordion_child_progress(0.0, 0), 0.0);
        assert!(accordion_child_progress(0.7, 0) > accordion_child_progress(0.7, 1));
        assert_eq!(accordion_child_progress(1.0, 0), 1.0);
    }

    #[test]
    fn hovered_row_ids_decode_group_without_scroll_offset() {
        assert_eq!(
            hovered_process_group(PICKER_ROW_PROCESS_BASE + 7 + LABEL_FLAG),
            Some(7)
        );
        assert_eq!(
            hovered_process_group(PICKER_ROW_WINDOW_BASE + (3 << 16) + 2),
            Some(3)
        );
        assert_eq!(hovered_process_group(PICKER_ROW_DESKTOP), None);
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
        let snapshot = [
            window(1, r"C:\Apps\app.exe", &long_title, 1, 1),
            window(2, r"C:\Apps\app.exe", "Короткое окно", 1, 2),
        ];
        let picker = build_picker_panel_with_layout(
            &allowlist(vec![]),
            &snapshot,
            0,
            picker_frame(),
            true,
            PickerLayout {
                expanded_group: Some(0),
                expansion: 1.0,
            },
        );
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
    fn row_with_icon_emits_rgba_not_placeholder() {
        let snapshot = [window_with_icon(
            1,
            r"C:\Apps\chrome.exe",
            "Chrome",
            100,
            1,
            16,
        )];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        // Свёрнутая однооконная группа — одна строка приложения с иконкой;
        // desktop-строка использует стеклянный слот.
        let icons = rgba_icons(&p.panel);
        assert_eq!(icons.len(), 1, "строка приложения несёт иконку: {icons:?}");
        assert!(icons.iter().all(|&(_, w, h)| w == 16 && h == 16));
        assert_eq!(glass_slot_count(&p.panel), 1, "слот desktop");
    }

    #[test]
    fn rows_of_same_process_share_icon_key() {
        let snapshot = [
            window_with_icon(1, r"C:\Apps\chrome.exe", "Chrome A", 100, 1, 16),
            window_with_icon(2, r"C:\Apps\chrome.exe", "Chrome B", 200, 2, 16),
        ];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        let icons = rgba_icons(&p.panel);
        // Свёрнутый процесс занимает одну строку и делит один key.
        assert_eq!(icons.len(), 1);
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
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1], "разные exe — разные key");
    }

    #[test]
    fn window_without_icon_keeps_glass_placeholder() {
        let snapshot = [window(1, r"C:\Apps\app.exe", "No icon", 1, 1)];
        let p = build_picker_panel(&allowlist(vec![]), &snapshot, 0, picker_frame(), true);
        assert_eq!(glass_slot_count(&p.panel), 2, "desktop и приложение");
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
