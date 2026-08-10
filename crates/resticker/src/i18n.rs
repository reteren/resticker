//! Локализация небольшого набора нативных пользовательских строк — пункты
//! меню трея и тексты баллон-уведомлений (ROADMAP.md M8, «локализация:
//! русский и английский»). Основная поверхность локализации — окно настроек
//! Tauri (`crates/resticker/ui/i18n.js`, ru/en словарь); здесь только то,
//! что рисуется МИМО этого окна, где словарь на JS не помогает.
//!
//! Язык — `Settings.language` (`"ru"`/что угодно ещё трактуется как `"ru"`;
//! `#[serde(default)]` даёт `"ru"` старым config.json без этого поля).
//! Тред меню трея (см. `main.rs::build_tray_menu`) не отслеживает язык живьём
//! — как и хоткеи (UI-подсказка «применяются после перезапуска»), смена
//! языка подхватывается тут при следующем запуске resticker, а не мгновенно.
//!
//! Тексты `Win32Error` (например `PinAccessDenied`, ROADMAP.md «понятные
//! тексты ошибок») сюда намеренно не включены: это готовые многострочные
//! предложения из `thiserror`, без параметра языка — переводить их означало
//! бы дублировать формулировки в двух местах не по одному источнику
//! истины. Оставлены на русском, известный небольшой пробел.

/// `true`, если `lang` — английский; всё остальное (включая пустую строку,
/// незнакомые значения) трактуется как русский — тот же принцип, что
/// `Settings::language` по умолчанию `"ru"`.
fn is_en(lang: &str) -> bool {
    lang == "en"
}

/// Пункт меню трея «Открыть настройки».
pub fn tray_open_settings(lang: &str) -> &'static str {
    if is_en(lang) {
        "Open settings"
    } else {
        "Открыть настройки"
    }
}

/// Пункт меню трея «Показать/скрыть все стикеры».
pub fn tray_toggle_visible(lang: &str) -> &'static str {
    if is_en(lang) {
        "Show/hide all stickers"
    } else {
        "Показать/скрыть все стикеры"
    }
}

/// Заголовок подменю «Пресеты» в трее.
pub fn tray_presets_submenu(lang: &str) -> &'static str {
    if is_en(lang) {
        "Presets"
    } else {
        "Пресеты"
    }
}

/// Пункт меню трея «Выход».
pub fn tray_exit(lang: &str) -> &'static str {
    if is_en(lang) { "Exit" } else { "Выход" }
}

/// Тост первого запуска (ROADMAP.md M8, онбординг): заголовок + тело с
/// подставленным хоткеем.
pub fn onboarding_notification(lang: &str, hotkey: &str) -> (String, String) {
    if is_en(lang) {
        (
            "resticker is running".to_string(),
            format!("Press {hotkey} to enter edit mode and add your first sticker."),
        )
    } else {
        (
            "resticker запущен".to_string(),
            format!(
                "Нажмите {hotkey}, чтобы войти в режим редактирования и добавить первый стикер."
            ),
        )
    }
}

/// Название конфликтующего хоткея в тексте тоста — на нужном языке и в
/// нужном падеже/форме («… недоступен»/«… недоступно», см. вызов ниже).
fn hotkey_action_label(lang: &str, name: rst_win32::overlay::HotkeyName) -> &'static str {
    use rst_win32::overlay::HotkeyName;
    if is_en(lang) {
        match name {
            HotkeyName::EditMode => "entering edit mode",
            HotkeyName::ToggleAllStickers => "showing/hiding all stickers",
            HotkeyName::MuteAll => "muting all stickers",
        }
    } else {
        match name {
            HotkeyName::EditMode => "вход в режим редактирования",
            HotkeyName::ToggleAllStickers => "показ/скрытие всех стикеров",
            HotkeyName::MuteAll => "заглушение всех стикеров",
        }
    }
}

/// Тост «хоткей <name> уже занят другим приложением» — `name` определяет,
/// какое именно действие сейчас недоступно (раньше текст всегда говорил
/// «режим редактирования», даже когда конфликтовал `toggle_all`/`mute_all`).
pub fn hotkey_conflict_notification(
    lang: &str,
    name: rst_win32::overlay::HotkeyName,
    combo: &str,
) -> (String, String) {
    let action = hotkey_action_label(lang, name);
    if is_en(lang) {
        (
            "Hotkey already in use".to_string(),
            format!(
                "The {combo} combination is already used by another application — {action} is unavailable."
            ),
        )
    } else {
        // «Не работает» вместо «недоступен/недоступно» — глагол не требует
        // согласования в роде/числе с подставляемым `action`
        // («вход…»/«показ…»/«заглушение…» — три разных рода).
        (
            "Хоткей уже занят".to_string(),
            format!(
                "Комбинация {combo} уже используется другим приложением — {action} не работает."
            ),
        )
    }
}

/// Тост «нет сохранённых пресетов» (клик по кнопке пресетов у курсора,
/// пустой список).
pub fn no_presets_notification(lang: &str) -> (String, String) {
    if is_en(lang) {
        (
            "Presets".to_string(),
            "No saved presets yet — save your layout in settings.".to_string(),
        )
    } else {
        (
            "Пресеты".to_string(),
            "Нет сохранённых пресетов — сохраните расстановку в настройках.".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_empty_language_falls_back_to_russian() {
        for lang in ["", "fr", "ru", "RU", "En"] {
            assert_eq!(tray_exit(lang), "Выход", "lang={lang:?}");
        }
    }

    #[test]
    fn en_gives_english_tray_labels() {
        assert_eq!(tray_open_settings("en"), "Open settings");
        assert_eq!(tray_toggle_visible("en"), "Show/hide all stickers");
        assert_eq!(tray_presets_submenu("en"), "Presets");
        assert_eq!(tray_exit("en"), "Exit");
    }

    #[test]
    fn onboarding_notification_embeds_hotkey_in_both_languages() {
        let (title_ru, body_ru) = onboarding_notification("ru", "Ctrl+Alt+S");
        assert_eq!(title_ru, "resticker запущен");
        assert!(body_ru.contains("Ctrl+Alt+S"));

        let (title_en, body_en) = onboarding_notification("en", "Ctrl+Alt+S");
        assert_eq!(title_en, "resticker is running");
        assert!(body_en.contains("Ctrl+Alt+S"));
        assert_ne!(body_ru, body_en);
    }

    #[test]
    fn hotkey_conflict_notification_embeds_combo_in_both_languages() {
        use rst_win32::overlay::HotkeyName;
        let (_, body_ru) = hotkey_conflict_notification("ru", HotkeyName::EditMode, "Ctrl+Alt+S");
        assert!(body_ru.contains("Ctrl+Alt+S"));
        let (_, body_en) = hotkey_conflict_notification("en", HotkeyName::EditMode, "Ctrl+Alt+S");
        assert!(body_en.contains("Ctrl+Alt+S"));
        assert_ne!(body_ru, body_en);
    }

    #[test]
    fn hotkey_conflict_notification_names_the_right_hotkey() {
        // Регрессия: раньше текст ВСЕГДА говорил «режим редактирования»,
        // даже когда конфликтовал toggle_all/mute_all — пользователь получал
        // неверный тост, будто сломан не тот хоткей.
        use rst_win32::overlay::HotkeyName;
        let (_, edit) = hotkey_conflict_notification("ru", HotkeyName::EditMode, "Ctrl+Alt+M");
        let (_, toggle) =
            hotkey_conflict_notification("ru", HotkeyName::ToggleAllStickers, "Ctrl+Alt+M");
        let (_, mute) = hotkey_conflict_notification("ru", HotkeyName::MuteAll, "Ctrl+Alt+M");
        assert!(edit.contains("режим редактирования"));
        assert!(toggle.contains("показ/скрытие"));
        assert!(mute.contains("заглушение"));
        assert_ne!(edit, toggle);
        assert_ne!(edit, mute);
        assert_ne!(toggle, mute);
    }

    #[test]
    fn no_presets_notification_differs_by_language() {
        let (title_ru, body_ru) = no_presets_notification("ru");
        let (title_en, body_en) = no_presets_notification("en");
        assert_eq!(title_ru, "Пресеты");
        assert_eq!(title_en, "Presets");
        assert_ne!(body_ru, body_en);
    }
}
