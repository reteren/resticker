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
/// Заголовок подменю отступа закреплённого окна в снап-зоне Windows.
/// «Snap gap», а не «Snap shrink»: пользователь выбирает величину ЗАЗОРА
/// вокруг окна — это то, что он видит на экране, а «ужатие» описывает
/// внутренний механизм.
/// Пункт трея «режим редактирования».
///
/// Дублирует хоткей: если его перехватывает чужая программа, войти в режим
/// иначе нельзя, а без режима недоступны и стикеры, и менеджер групп.
pub fn tray_edit_mode() -> &'static str {
    "Edit mode"
}

pub fn tray_snap_gap_submenu() -> &'static str {
    "Snap gap"
}

/// Пункт, открывающий панель с полем ввода зазора. Многоточие — обычное
/// соглашение: пункт не выполняет действие, а открывает что-то ещё.
pub fn tray_snap_gap_set() -> &'static str {
    "Set value…"
}

/// Пункт-галочка «применять отступ и к обычным окнам» в подменю отступа.
pub fn tray_snap_gap_all_windows() -> &'static str {
    "All windows, not just pinned"
}

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

/// Заголовок окна живого куска в Alt+Tab и на панели задач.
///
/// С пометкой «piece», а не голым именем приложения: иначе в Alt+Tab стояли
/// бы два одинаковых «Калькулятора» — само приложение и его кусок, и выбрать
/// нужный можно было бы только наугад.
///
/// `tag` — короткая метка куска, первые символы его идентификатора. Она
/// делает заголовок УНИКАЛЬНЫМ и неизменным, и это не украшение: группы окон
/// опознают члена по exe + заголовку + классу (`rst_core::group_match`), а у
/// двух кусков одного приложения exe и класс совпадают. Без метки группа не
/// могла бы отличить один кусок от другого — и после перезапуска ставила бы
/// на место первого второй (разбор P2, 2026-09-13). Метка берётся из
/// идентификатора стикера, поэтому переживает перезапуск ровно так же, как
/// сам кусок.
pub fn crop_window_title(app: &str, tag: &str) -> String {
    let tag = tag.trim();
    let base = if app.trim().is_empty() {
        "Window piece".to_string()
    } else {
        format!("{app} — piece")
    };
    if tag.is_empty() {
        base
    } else {
        format!("{base} {tag}")
    }
}

/// Тост отказа при отделении куска окна (запрос пользователя 2026-09-10).
///
/// Каждый текст говорит, ЧТО сделать иначе: выделение, которое «просто
/// ничего не дало», читается как поломка программы, а не как промах рукой.
pub fn window_crop_refusal(refusal: rst_core::window_crop::CropError) -> String {
    use rst_core::window_crop::CropError;
    match refusal {
        CropError::DragOutsideWindow => {
            "No piece taken: the drag has to start inside a window. Point at one, then drag."
                .to_string()
        }
        CropError::DragTooSmall => {
            "No piece taken: that selection is too small to show. Drag a bigger rectangle."
                .to_string()
        }
        CropError::DegenerateWindow => {
            "No piece taken: that window has no visible area to copy from.".to_string()
        }
    }
}

/// Название конфликтующего хоткея в тексте тоста — какое действие сейчас
/// недоступно.
/// Текст баннера, когда митоз окна не состоялся
/// (docs/M9_WINDOW_MITOSIS_DESIGN.md §3). `app` — имя файла exe, если оно
/// известно: назвать приложение поимённо стоит дороже, чем «это окно».
///
/// Каждая формулировка называет ПРИЧИНУ и, где это уместно, число, которое
/// её вызвало: «не получилось» без причины оставляет пользователя гадать,
/// сломана программа или он выбрал не то окно. Мегабайты, а не байты, —
/// потолок в настройках задан в мегабайтах, и два разных порядка величины
/// в одном сообщении читались бы как ошибка.
pub fn mitosis_refusal(refusal: rst_core::mitosis::MitosisRefusal, app: Option<&str>) -> String {
    use rst_core::mitosis::MitosisRefusal;
    const MB: u64 = 1024 * 1024;
    let name = app.unwrap_or("this app");
    match refusal {
        MitosisRefusal::TooHeavy { bytes, limit_bytes } => format!(
            "Mitosis refused: {name} holds {} MB, over the {} MB limit. Splitting it would launch a second copy.",
            bytes / MB,
            limit_bytes / MB
        ),
        MitosisRefusal::NotEnoughMemory {
            bytes,
            available_bytes,
        } => format!(
            "Mitosis refused: a second copy needs about {} MB, and only {} MB of RAM is free.",
            bytes / MB,
            available_bytes / MB
        ),
        MitosisRefusal::TooSmall { half_px, min_px } => format!(
            "Mitosis refused: a {half_px} px half is below the {min_px} px minimum. Cut closer to the middle, or pick a bigger window."
        ),
        MitosisRefusal::NoExePath => {
            "Mitosis refused: this window's program cannot be identified, so a second copy cannot be started.".to_string()
        }
        MitosisRefusal::SpawnFailed => {
            "Mitosis failed: the second copy could not be started. The window was restored.".to_string()
        }
        MitosisRefusal::NoSecondWindow => format!(
            "Mitosis failed: {name} did not open a second window. The window was restored."
        ),
        MitosisRefusal::SingleInstanceApp => format!(
            "Mitosis is off for {name}: it only ever opens one window. Remove it from mitosis_single_instance_apps in config.json to try again."
        ),
    }
}

/// Баннер в тот момент, когда приложение попало в список одно-оконных
/// ([`mitosis_refusal`], `SingleInstanceApp`) — то есть после ВТОРОГО подряд
/// отказа по одному и тому же exe.
///
/// Отдельный текст, а не тот же самый: пользователю важно понять, что
/// изменилось состояние программы, а не просто повторилась неудача, — иначе
/// он не свяжет будущий мгновенный отказ с этим моментом.
pub fn mitosis_app_disabled(app: &str) -> String {
    format!(
        "{app} did not open a second window twice in a row — mitosis is now off for it. Undo that in config.json (mitosis_single_instance_apps)."
    )
}

fn hotkey_action_label(name: rst_win32::overlay::HotkeyName) -> &'static str {
    use rst_win32::overlay::HotkeyName;
    match name {
        HotkeyName::EditMode => "entering edit mode",
        HotkeyName::ToggleAllStickers => "showing/hiding all stickers",
        HotkeyName::MuteAll => "muting all stickers",
        HotkeyName::PinFocusedWindow => "pinning/unpinning the focused window",
        HotkeyName::WindowMitosis => "window mitosis (split a window in two)",
        HotkeyName::WindowCrop => "tearing off a piece of a window",
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

/// Тост об итоге живой перерегистрации хоткеев (кнопка «Применить» в
/// настройках): часть комбинаций занята другими приложениями и не досталась
/// программе. `more` — сколько ещё комбинаций конфликтует кроме `combo`.
///
/// Одно уведомление на весь набор, а не по одному на комбинацию: хоткеев
/// полтора десятка, и очередь из тостов прочитать невозможно — назвать надо
/// первую занятую и число остальных.
pub fn hotkeys_reloaded_conflicts(combo: &str, more: usize) -> (String, String) {
    let title = "Hotkey not assigned".to_string();
    let body = if more > 0 {
        format!(
            "{combo} and {more} more combinations are already used by other applications — those hotkeys did not take effect."
        )
    } else {
        format!(
            "The {combo} combination is already used by another application — that hotkey did not take effect."
        )
    };
    (title, body)
}

/// Тост следующего запуска после аварийного завершения процесса. При
/// `panic = "abort"` и GUI-подсистеме окно ошибки Windows не появляется, а
/// резидентная программа просто исчезает — пользователю нужно явно назвать
/// причину и путь к журналу.
pub fn panic_notification(
    version: &str,
    thread: &str,
    location: &str,
    message: &str,
    log_path: &str,
) -> (String, String) {
    (
        "resticker closed unexpectedly".to_string(),
        format!(
            "The previous resticker session ended with an error (version {version}). Cause: {message} in {location} on thread {thread}. Details are in the log: {log_path}."
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
        assert_eq!(tray_snap_gap_submenu(), "Snap gap");
        assert_eq!(tray_edit_mode(), "Edit mode");
        assert_eq!(tray_snap_gap_set(), "Set value…");
        assert_eq!(tray_snap_gap_all_windows(), "All windows, not just pinned");
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

    #[test]
    fn panic_notification_names_version_cause_and_log() {
        let (title, body) = panic_notification(
            "0.5.0",
            "overlay-1",
            "crates/resticker/src/main.rs:42:7",
            "device lost",
            "C:\\Users\\me\\AppData\\Local\\resticker\\logs\\resticker.log",
        );
        assert_eq!(title, "resticker closed unexpectedly");
        assert!(body.contains("version 0.5.0"));
        assert!(body.contains("device lost"));
        assert!(body.contains("resticker.log"));
    }
}
