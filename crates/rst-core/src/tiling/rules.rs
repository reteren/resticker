//! Правила окон: float / ignore / workspace N / tile по exe, классу, заголовку
//! (docs/TILING_DESIGN.md §Р2). Платформенно-чисто: факты об окне заполняет
//! координатор из `rst_win32::window_enum::WindowInfo` — сюда приходят уже
//! переведённые строки.
//!
//! Зачем это вообще: тайлинг по умолчанию берёт под управление ВСЁ, поэтому
//! без правил инсталляторы и диалоги попадут в сетку — это первое, что
//! взбесит пользователя. Правила — способ сказать «это окно не трогай»
//! (`Ignore`), «это всегда плавающее» (`Float`) или «это всегда на воркспейс
//! N» (`Workspace`), пока пользователь не поправил руками.
//!
//! Отличие от [`crate::occluders`]: там правило матчит по ИЛИ (процесс ИЛИ
//! заголовок — окно уже окклюдер, любое совпадение снимает его с роли),
//! здесь — по И: все заданные поля правила должны совпасть одновременно.
//! Это осознанное различие, а не баг: тайлинг-правило «firefox.exe на
//! воркспейсе 3» с семантикой ИЛИ превратилось бы в «любой Firefox ИЛИ
//! любое окно с заголовком X летит на воркспейс 3» — слишком грубо.

use serde::{Deserialize, Serialize};

use crate::occluders::wildcard_match;

/// Факты об окне, по которым матчатся правила.
///
/// `exe_path` — `None`, когда путь получить не удалось (защищённый процесс):
/// тогда правило, заданное по exe, такому окну НЕ соответствует —
/// консервативно, окно остаётся под управлением WM по умолчанию.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowFacts {
    pub exe_path: Option<String>,
    pub title: String,
    pub class: String,
}

/// Условие правила. Пустое (все поля `None`) не матчит ничего.
///
/// Поля — это И, а не ИЛИ (см. докмодуль). `#[serde(default)]` на полях
/// позволяет писать в config.json только то, что нужно:
/// `{"exe": "firefox.exe"}` — без заглушек для class/title.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RuleMatch {
    #[serde(default)]
    pub exe: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Что сделать с окном, если правило сматчилось.
///
/// `Float` и `Tile` меняют только флаг «тайлится» и не трогают воркспейс,
/// назначенный более ранним правилом: плавающее окно всё равно принадлежит
/// воркспейсу (поведение i3), просто не участвует в раскладке. `Ignore`
/// выкидывает окно из-под WM целиком — поэтому оно терминальное и
/// аннулирует воркспейс: привязывать окно к воркспейсу, где его никто не
/// раскладывает, бессмысленно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Float,
    Ignore,
    Workspace(u8),
    Tile,
}

/// Одно правило: условие + действие.
///
/// `#[serde(default)]` на matcher — правило в config.json может задать
/// только действие: `{"action": "float"}` валидно и матчит (правильно —
/// ничего).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRule {
    #[serde(default)]
    pub matcher: RuleMatch,
    pub action: RuleAction,
}

/// Итог применения набора правил к окну.
///
/// Поля накапливаются независимо: `tiled` задают `Float`/`Tile`,
/// `workspace` — `Workspace(n)`, и последнее совпавшее правило выигрывает
/// по СВОЕМУ полю. `Ignore` — терминальное: после него остальные правила
/// не смотрятся (см. [`RuleAction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleOutcome {
    /// Окно участвует в тайлинге (а не плавает).
    pub tiled: bool,
    /// Окно полностью исключено из-под управления WM.
    pub ignored: bool,
    /// Воркспейс, на котором окно должно появиться.
    pub workspace: Option<u8>,
}

impl Default for RuleOutcome {
    /// Пустой набор правил: тайлинг по умолчанию, ничего не игнорируется,
    /// воркспейс решает политика вставки.
    fn default() -> Self {
        Self {
            tiled: true,
            ignored: false,
            workspace: None,
        }
    }
}

/// Применить набор правил к окну, вернуть итоговое решение.
///
/// Правила проходятся по порядку; последнее совпавшее выигрывает по своему
/// полю. `Ignore` останавливает просмотр: окно уже вычеркнуто из-под WM,
/// смотреть дальше не на что.
pub fn evaluate(rules: &[WindowRule], facts: &WindowFacts) -> RuleOutcome {
    let mut outcome = RuleOutcome::default();
    for rule in rules {
        if !rule.matches(facts) {
            continue;
        }
        match rule.action {
            RuleAction::Ignore => {
                return RuleOutcome {
                    tiled: false,
                    ignored: true,
                    workspace: None,
                };
            }
            RuleAction::Float => outcome.tiled = false,
            RuleAction::Tile => outcome.tiled = true,
            RuleAction::Workspace(n) => outcome.workspace = Some(n),
        }
    }
    outcome
}

impl WindowRule {
    /// Матчит ли правило окно: ВСЕ заданные поля одновременно (И).
    ///
    /// Отдельная функция, а не встраивание в `evaluate`, чтобы предикат
    /// можно было переиспользовать (панель правил в UI покажет, какие
    /// правила сработали на выбранном окне, тем же предикатом — иначе UI
    /// и движок могли бы разойтись во мнениях).
    fn matches(&self, facts: &WindowFacts) -> bool {
        // Пустое условие (все поля None) не матчит НИЧЕГО: иначе правило
        // `{"action": "float"}` без матчера плавало бы ВСЕ окна системы.
        if self.matcher.exe.is_none()
            && self.matcher.class.is_none()
            && self.matcher.title.is_none()
        {
            return false;
        }
        if let Some(pattern) = &self.matcher.exe {
            // Путь у окна неизвестен — правило по exe консервативно не матчит
            // (тот же выбор, что в `occluders`).
            let Some(path) = &facts.exe_path else {
                return false;
            };
            let file_name = path.rsplit(['\\', '/']).next().unwrap_or(path);
            if !pattern.eq_ignore_ascii_case(file_name) {
                return false;
            }
        }
        if let Some(pattern) = &self.matcher.class
            && !pattern.eq(&facts.class)
        {
            return false;
        }
        if let Some(pattern) = &self.matcher.title
            && !title_matches(pattern, &facts.title)
        {
            return false;
        }
        true
    }
}

/// Заголовок: подстрока без учёта регистра, а при наличии `*` — glob.
///
/// `*` поддержана в начале и/или конце (`Chrome*`, `*Notepad`,
/// `*YouTube*`); реализация — общий двух-указательный glob из
/// [`wildcard_match`], поэтому `*` в середине тоже работает, как бонус,
/// без введения regex. Без `*` — именно ПОДСТРОКА, а не точное равенство:
/// правило `untitled` обязано поймать «Untitled - Notepad», иначе заголовки
/// с суффиксами приложения не матчились бы вовсе.
fn title_matches(pattern: &str, title: &str) -> bool {
    if pattern.contains('*') {
        wildcard_match(pattern, title)
    } else {
        title.to_lowercase().contains(&pattern.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(exe: Option<&str>, title: &str, class: &str) -> WindowFacts {
        WindowFacts {
            exe_path: exe.map(String::from),
            title: title.to_string(),
            class: class.to_string(),
        }
    }

    fn rule(
        exe: Option<&str>,
        class: Option<&str>,
        title: Option<&str>,
        action: RuleAction,
    ) -> WindowRule {
        WindowRule {
            matcher: RuleMatch {
                exe: exe.map(String::from),
                class: class.map(String::from),
                title: title.map(String::from),
            },
            action,
        }
    }

    fn default_facts() -> WindowFacts {
        facts(
            Some(r"C:\Program Files\Mozilla Firefox\firefox.exe"),
            "Example - Mozilla Firefox",
            "MozillaWindowClass",
        )
    }

    #[test]
    fn empty_rules_leave_everything_tiled() {
        assert_eq!(
            evaluate(&[], &default_facts()),
            RuleOutcome {
                tiled: true,
                ignored: false,
                workspace: None,
            }
        );
    }

    #[test]
    fn exe_rule_matches_by_file_name_ignoring_path() {
        let rules = [rule(Some("firefox.exe"), None, None, RuleAction::Float)];
        let out = evaluate(&rules, &default_facts());
        assert!(!out.tiled);
    }

    #[test]
    fn exe_rule_is_case_insensitive() {
        let rules = [rule(Some("FIREFOX.EXE"), None, None, RuleAction::Float)];
        assert!(!evaluate(&rules, &default_facts()).tiled);
    }

    #[test]
    fn exe_rule_does_not_match_window_without_known_path() {
        let rules = [rule(Some("firefox.exe"), None, None, RuleAction::Float)];
        let unknown = facts(None, "Example - Mozilla Firefox", "MozillaWindowClass");
        assert!(
            evaluate(&rules, &unknown).tiled,
            "None-путь = консервативно не матч"
        );
    }

    #[test]
    fn class_rule_matches_exactly() {
        let rules = [rule(
            None,
            Some("MozillaWindowClass"),
            None,
            RuleAction::Float,
        )];
        assert!(!evaluate(&rules, &default_facts()).tiled);
    }

    #[test]
    fn class_rule_is_case_sensitive() {
        // Классы окон Win32 регистрозависимы — правило в другом регистре
        // НЕ матчит, в отличие от exe.
        let rules = [rule(
            None,
            Some("mozillawindowclass"),
            None,
            RuleAction::Float,
        )];
        assert!(evaluate(&rules, &default_facts()).tiled);
    }

    #[test]
    fn title_rule_matches_substring_case_insensitively() {
        let rules = [rule(None, None, Some("mozilla firefox"), RuleAction::Float)];
        assert!(
            !evaluate(&rules, &default_facts()).tiled,
            "подстрока, не точное равенство"
        );
    }

    #[test]
    fn combined_rule_requires_all_fields_to_match() {
        let rules = [rule(
            Some("firefox.exe"),
            Some("MozillaWindowClass"),
            Some("Example"),
            RuleAction::Float,
        )];
        assert!(
            !evaluate(&rules, &default_facts()).tiled,
            "все три поля совпали"
        );

        // Любое НЕсовпадение одного поля рушит правило целиком.
        let wrong_class = facts(
            Some(r"C:\Program Files\Mozilla Firefox\firefox.exe"),
            "Example - Mozilla Firefox",
            "Chrome_WidgetWin_1",
        );
        assert!(evaluate(&rules, &wrong_class).tiled);
        let wrong_title = facts(
            Some(r"C:\Program Files\Mozilla Firefox\firefox.exe"),
            "Settings",
            "MozillaWindowClass",
        );
        assert!(evaluate(&rules, &wrong_title).tiled);
    }

    #[test]
    fn exe_is_case_insensitive_but_class_is_not_in_one_rule() {
        let rules = [rule(
            Some("Firefox.Exe"),
            Some("MozillaWindowClass"),
            None,
            RuleAction::Float,
        )];
        assert!(
            !evaluate(&rules, &default_facts()).tiled,
            "exe в другом регистре ок"
        );
        let rules = [rule(
            Some("Firefox.Exe"),
            Some("mozillawindowclass"),
            None,
            RuleAction::Float,
        )];
        assert!(
            evaluate(&rules, &default_facts()).tiled,
            "класс в другом регистре не ок"
        );
    }

    #[test]
    fn title_wildcard_prefix_matches() {
        let rules = [rule(None, None, Some("Chrome*"), RuleAction::Float)];
        let chrome = facts(Some(r"C:\chrome.exe"), "Chrome - YouTube", "c");
        assert!(!evaluate(&rules, &chrome).tiled);
        let not_chrome = facts(Some(r"C:\notepad.exe"), "Settings - Notepad", "c");
        assert!(evaluate(&rules, &not_chrome).tiled);
    }

    #[test]
    fn title_wildcard_suffix_matches() {
        let rules = [rule(None, None, Some("*Notepad"), RuleAction::Float)];
        let notepad = facts(Some(r"C:\notepad.exe"), "Untitled - Notepad", "c");
        assert!(!evaluate(&rules, &notepad).tiled);
    }

    #[test]
    fn title_wildcard_contains_matches() {
        let rules = [rule(None, None, Some("*YouTube*"), RuleAction::Float)];
        let youtube = facts(
            Some(r"C:\chrome.exe"),
            "My Playlist - YouTube - Chrome",
            "c",
        );
        assert!(!evaluate(&rules, &youtube).tiled);
    }

    #[test]
    fn empty_rule_matches_nothing() {
        let rules = [rule(None, None, None, RuleAction::Ignore)];
        assert!(
            evaluate(&rules, &default_facts()).tiled,
            "все None — матча нет"
        );
        assert!(!evaluate(&rules, &default_facts()).ignored);
    }

    #[test]
    fn ignore_rule_is_terminal_and_wins_over_later_rules() {
        // Первое правило — Ignore по exe; второе по классу тоже сматчилось
        // бы, но Ignore терминальный: смотрим только первое, окно ignored.
        let rules = [
            rule(Some("firefox.exe"), None, None, RuleAction::Ignore),
            rule(None, Some("MozillaWindowClass"), None, RuleAction::Float),
        ];
        let out = evaluate(&rules, &default_facts());
        assert!(out.ignored);
        assert!(!out.tiled);
        assert_eq!(
            out.workspace, None,
            "Ignore аннулирует воркспейс ранних правил"
        );
    }

    #[test]
    fn last_matching_rule_wins_per_field() {
        // По воркспейсу выигрывает ПОСЛЕДНЕЕ совпавшее правило (2, а не 1).
        let rules = [
            rule(Some("firefox.exe"), None, None, RuleAction::Workspace(1)),
            rule(
                None,
                Some("MozillaWindowClass"),
                None,
                RuleAction::Workspace(2),
            ),
        ];
        assert_eq!(evaluate(&rules, &default_facts()).workspace, Some(2));
    }

    #[test]
    fn float_after_workspace_keeps_the_workspace() {
        // Float и Workspace меняют РАЗНЫЕ поля итога: плавающее окно
        // остаётся привязанным к воркспейсу, который назначили раньше.
        let rules = [
            rule(Some("firefox.exe"), None, None, RuleAction::Workspace(3)),
            rule(None, Some("MozillaWindowClass"), None, RuleAction::Float),
        ];
        let out = evaluate(&rules, &default_facts());
        assert!(!out.tiled);
        assert_eq!(out.workspace, Some(3));
    }

    #[test]
    fn tile_rule_restores_tiling_after_float() {
        let rules = [
            rule(Some("firefox.exe"), None, None, RuleAction::Float),
            rule(None, Some("MozillaWindowClass"), None, RuleAction::Tile),
        ];
        assert!(
            evaluate(&rules, &default_facts()).tiled,
            "последнее слово за Tile"
        );
    }

    #[test]
    fn workspace_action_sets_workspace() {
        let rules = [rule(
            None,
            None,
            Some("*Firefox*"),
            RuleAction::Workspace(7),
        )];
        let out = evaluate(&rules, &default_facts());
        assert!(out.tiled, "Workspace не меняет tiled");
        assert_eq!(out.workspace, Some(7));
    }

    #[test]
    fn rules_serialize_roundtrip() {
        let rules = [
            rule(Some("firefox.exe"), None, None, RuleAction::Float),
            rule(
                None,
                Some("MozillaWindowClass"),
                None,
                RuleAction::Workspace(3),
            ),
            rule(None, None, Some("*Notepad"), RuleAction::Ignore),
            rule(None, None, None, RuleAction::Tile),
        ];
        let json = serde_json::to_string(&rules).unwrap();
        let back: Vec<WindowRule> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rules);
    }

    #[test]
    fn missing_matcher_fields_default_to_none() {
        // config.json: только действие — валидно, правило просто ничего
        // не матчит. Это проверяет #[serde(default)] на полях RuleMatch.
        let json = r#"[{"action":"float"}]"#;
        let rules: Vec<WindowRule> = serde_json::from_str(json).unwrap();
        assert!(evaluate(&rules, &default_facts()).tiled);
    }

    #[test]
    fn facts_and_outcome_serialize() {
        let f = default_facts();
        let back: WindowFacts = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
        let out = RuleOutcome::default();
        let back: RuleOutcome =
            serde_json::from_str(&serde_json::to_string(&out).unwrap()).unwrap();
        assert_eq!(back, out);
    }
}
