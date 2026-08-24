//! Нативные пользовательские строки, которые рисуются МИМО окна настроек —
//! пункты меню трея и тексты баллон-уведомлений.
//!
//! Язык один — английский (запрос пользователя 2026-08-23: «переведи всю
//! программу на английский»). До этого модуль ветвился по
//! `Settings.language` («ru»/«en»), а оверлей (тулбар, панели выбора окон и
//! пресетов) вообще не переводился и был русским — то есть при любом выборе
//! языка программа получалась смешанной. Одна общая формулировка на весь
//! нативный слой честнее: строки живут в одном месте, и их видно целиком.
//!
//! Возврат второго языка — не переписывание модуля заново, а возврат
//! параметра `lang` и второй ветки в каждой функции (см. историю git до
//! 2026-08-23); словарь окна настроек (`ui/i18n.js`) устроен так же и точно
//! так же сведён к английскому.
//!
//! Тексты `Win32Error` (например `PinAccessDenied`) сюда не входят: это
//! готовые предложения из `thiserror`, они переведены на месте, в
//! `rst-win32/src/error.rs`.

/// Пункт меню трея «Открыть настройки».
pub fn tray_open_settings() -> &'static str {
    "Open settings"
}

/// Пункт меню трея «Показать/скрыть все стикеры».
pub fn tray_toggle_visible() -> &'static str {
    "Show/hide all stickers"
}

/// Заголовок подменю «Пресеты» в трее.
pub fn tray_presets_submenu() -> &'static str {
    "Presets"
}

/// Пункт меню трея «Выход».
pub fn tray_exit() -> &'static str {
    "Exit"
}

/// Тост первого запуска (ROADMAP.md M8, онбординг): заголовок + тело с
/// подставленным хоткеем.
pub fn onboarding_notification(hotkey: &str) -> (String, String) {
    (
        "resticker is running".to_string(),
        format!("Press {hotkey} to enter edit mode and add your first sticker."),
    )
}

/// Название конфликтующего хоткея в тексте тоста — какое действие сейчас
/// недоступно.
fn hotkey_action_label(name: rst_win32::overlay::HotkeyName) -> &'static str {
    use rst_win32::overlay::HotkeyName;
    match name {
        HotkeyName::EditMode => "entering edit mode",
        HotkeyName::ToggleAllStickers => "showing/hiding all stickers",
        HotkeyName::MuteAll => "muting all stickers",
        HotkeyName::PinFocusedWindow => "pinning/unpinning the focused window",
    }
}

/// Тост «хоткей <name> уже занят другим приложением» — `name` определяет,
/// какое именно действие сейчас недоступно (раньше текст всегда говорил
/// «режим редактирования», даже когда конфликтовал `toggle_all`/`mute_all`).
pub fn hotkey_conflict_notification(
    name: rst_win32::overlay::HotkeyName,
    combo: &str,
) -> (String, String) {
    let action = hotkey_action_label(name);
    (
        "Hotkey already in use".to_string(),
        format!(
            "The {combo} combination is already used by another application — {action} is unavailable."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_labels_are_english() {
        assert_eq!(tray_open_settings(), "Open settings");
        assert_eq!(tray_toggle_visible(), "Show/hide all stickers");
        assert_eq!(tray_presets_submenu(), "Presets");
        assert_eq!(tray_exit(), "Exit");
    }

    #[test]
    fn onboarding_notification_embeds_hotkey() {
        let (title, body) = onboarding_notification("Ctrl+Alt+S");
        assert_eq!(title, "resticker is running");
        assert!(body.contains("Ctrl+Alt+S"));
    }

    #[test]
    fn hotkey_conflict_notification_embeds_combo() {
        use rst_win32::overlay::HotkeyName;
        let (_, body) = hotkey_conflict_notification(HotkeyName::EditMode, "Ctrl+Alt+S");
        assert!(body.contains("Ctrl+Alt+S"));
    }

    #[test]
    fn hotkey_conflict_notification_names_the_right_hotkey() {
        // Регрессия: раньше текст ВСЕГДА говорил «режим редактирования»,
        // даже когда конфликтовал toggle_all/mute_all — пользователь получал
        // неверный тост, будто сломан не тот хоткей.
        use rst_win32::overlay::HotkeyName;
        let (_, edit) = hotkey_conflict_notification(HotkeyName::EditMode, "Ctrl+Alt+M");
        let (_, toggle) = hotkey_conflict_notification(HotkeyName::ToggleAllStickers, "Ctrl+Alt+M");
        let (_, mute) = hotkey_conflict_notification(HotkeyName::MuteAll, "Ctrl+Alt+M");
        assert!(edit.contains("edit mode"));
        assert!(toggle.contains("showing/hiding"));
        assert!(mute.contains("muting"));
        assert_ne!(edit, toggle);
        assert_ne!(edit, mute);
        assert_ne!(toggle, mute);
    }
}
