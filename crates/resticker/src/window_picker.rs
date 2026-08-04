//! Чистая логика панели выбора окон («Слои видимости», SPEC.md §4.2;
//! docs/M4_WINDOW_PICKER_DESIGN.md §2-4): только данные и решения, без
//! Panel/Checkbox/рендера — билдер и роутинг событий живут отдельными
//! срезами (дизайн §7.1, шаги 5-7). Зависимостей на rst-render здесь нет.
//!
//! Главный инвариант (§2.1): состояние «выбран ли чекбокс» определяется тем
//! же предикатом [`rst_core::occluders::rule_matches`], которым маска решает
//! про окклюдера, — пересборка панели из `Sticker.visibility` никогда не
//! разойдётся с фактическим поведением маски.
//!
//! Модуль пока никем не вызывается: сшивка (билдер панели, кнопка тулбара,
//! `EditState`) — отдельные шаги плана §7.1 (5-7), придут следующими
//! срезами. `dead_code` снят до сшивки, чтобы `clippy -D warnings` оставался
//! зелёным; после подключения атрибут удалить.
#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::HashMap;

use rst_core::model::{OverlapRule, VisibilityMode, VisibilityRule};
use rst_core::occluders::{OccluderCandidate, rule_matches};
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
}
