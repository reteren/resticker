//! Проверка комбинаций хоткеев на «системно рискованные» — те, что Windows
//! зарезервировала за собой или обрабатывает раньше слоя глобальных хоткеев
//! (docs/research/hotkeys/R_RESERVED_COMBOS.md, H2).
//!
//! Зачем отдельный модуль, а не функция в `hotkey.rs`: hotkey.rs занимается
//! регистрацией и разбором `WM_HOTKEY`, а здесь — ПРЕДУПРЕЖДЕНИЕМ пользователя
//! до регистрации. Ответственности разные, `HotkeyCombo` общий.
//!
//! Ключевая идея: успешная регистрация не гарантирует доставку нажатия.
//! Переключатель языка/раскладки Windows (Alt+Shift/Ctrl+Shift) живёт в слое
//! ввода НИЖЕ хоткеев и на многих машинах съедает нажатие — `RegisterHotKey`
//! при этом возвращает успех, и хоткей «молча не работает» (именно так упал
//! Alt+Shift+S у пользователя). Настройка переключателя у каждого пользователя
//! своя, поэтому предупреждение читает РЕАЛЬНУЮ настройку из реестра и не
//! гадает.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::PCWSTR;

use crate::hotkey::HotkeyCombo;

/// Виртуальный код F12 (VK_F12): зарезервирован отладчиком (RegisterHotKey, docs).
const VK_F12: u32 = 0x7B;

/// Ключ реестра с настройкой переключателя раскладки (R_RESERVED_COMBOS.md §2.2).
const TOGGLE_KEY: &str = "Keyboard Layout\\Toggle";

/// Объяснение для Alt+Shift: настройка по умолчанию Windows (и «1» в реестре).
///
/// Формулировка намеренно без абсолютных гарантий: на части сборок Windows
/// зарегистрированный хоткей выигрывает у переключателя (измерено, Win11 26200),
/// на других — переключатель съедает нажатие (случай пользователя из H2).
/// Обещать «не сработает» — врать; «может не дойти» — честно.
const TOGGLE_ALT_SHIFT: &str = "Alt+Shift назначен в Windows переключателем языка/раскладки \
    клавиатуры (настройка по умолчанию либо HKCU\\Keyboard Layout\\Toggle). На многих машинах \
    Windows переключает раскладку раньше, чем нажатие доходит до приложения: хоткей \
    зарегистрируется, но молча не сработает. Выберите другую комбинацию или отключите \
    переключатель: Параметры → Время и язык → Ввод → Дополнительные параметры клавиатуры → \
    Сочетания клавиш ввода языка.";

/// Объяснение для Ctrl+Shift: значение «2» в реестре переключателя.
const TOGGLE_CTRL_SHIFT: &str = "Ctrl+Shift назначен в Windows переключателем языка/раскладки \
    клавиатуры (HKCU\\Keyboard Layout\\Toggle). На многих машинах Windows переключает раскладку \
    раньше, чем нажатие доходит до приложения: хоткей зарегистрируется, но молча не сработает. \
    Выберите другую комбинацию или отключите переключатель: Параметры → Время и язык → Ввод → \
    Дополнительные параметры клавиатуры → Сочетания клавиш ввода языка.";

/// Объяснение для Win-комбинаций: зарезервированы ОС (docs RegisterHotKey, MOD_WIN).
const WIN_MODIFIER: &str = "Сочетания с клавишей Win зарезервированы за операционной системой: \
    большинство Win-комбинаций (Win+D, Win+E, Win+R, Win+L, Win+S, Win+Shift+S и другие) не \
    регистрируются — система отвечает «уже зарегистрировано», а часть системных обрабатывается \
    раньше слоя хоткеев. Выберите комбинацию без Win.";

/// Объяснение для F12: зарезервирован отладчиком (docs RegisterHotKey).
const F12_RESERVED: &str = "F12 зарезервирован Windows для отладчика (документация \
    RegisterHotKey): на машинах с активным отладчиком нажатие не дойдёт до приложения. \
    Выберите другую клавишу.";

/// Предупреждение о рискованной комбинации — готово к показу пользователю.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyWarning {
    /// Каноничный вид комбинации («Alt+Shift+S») — заголовок предупреждения.
    pub combo: String,
    /// Объяснение на русском: почему комбинация рискованна и что делать.
    pub explanation: &'static str,
}

/// Реальная настройка переключателя языка/раскладки Windows (реестр
/// `HKCU\Keyboard Layout\Toggle`): какие пары модификаторов назначены
/// переключателем. Читается из реестра, а не предполагается — у разных
/// пользователей она разная (R_RESERVED_COMBOS.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutToggleConfig {
    /// Alt+Shift назначен переключателем (значение «1» или отсутствие настройки).
    pub alt_shift: bool,
    /// Ctrl+Shift назначен переключателем (значение «2»).
    pub ctrl_shift: bool,
}

impl LayoutToggleConfig {
    /// Настройки по умолчанию, когда ключа в реестре нет вовсе: язык
    /// переключается Alt+Shift — это подтверждено измерением (голый Shift+Alt
    /// переключил раскладку на машине без ключа Toggle) и диалогом Windows
    /// «Сочетания клавиш ввода языка». Ctrl+Shift по умолчанию не
    /// предупреждаем: его исторический дефолт — раскладка внутри языка,
    /// и при одной раскладке на язык он неактивен; гадать здесь — врать.
    fn defaults() -> Self {
        Self {
            alt_shift: true,
            ctrl_shift: false,
        }
    }
}

/// Проверить комбинацию на риск молчаливого отказа: возвращает предупреждение,
/// если комбинация системно рискованна для ЭТОГО пользователя.
///
/// Читает реальную настройку переключателя раскладки из реестра (не гадает).
/// Вызывается из UI настроек до регистрации хоткея.
pub fn check_risky_combo(combo: HotkeyCombo) -> Option<HotkeyWarning> {
    check_risky_combo_with_toggle(combo, read_layout_toggle())
}

/// Чистая версия проверки: настройка переключателя передаётся явно.
///
/// Отделена от чтения реестра ради тестируемости: все ветки логики проверяются
/// без обращения к реальному реестру. Приоритет предупреждений: переключатель
/// раскладки (самый вероятный «тихий» отказ) → Win → F12.
pub fn check_risky_combo_with_toggle(
    combo: HotkeyCombo,
    toggle: LayoutToggleConfig,
) -> Option<HotkeyWarning> {
    if toggle.alt_shift && combo.alt && combo.shift {
        return Some(HotkeyWarning {
            combo: combo.display_string(),
            explanation: TOGGLE_ALT_SHIFT,
        });
    }
    if toggle.ctrl_shift && combo.ctrl && combo.shift {
        return Some(HotkeyWarning {
            combo: combo.display_string(),
            explanation: TOGGLE_CTRL_SHIFT,
        });
    }
    if combo.win {
        return Some(HotkeyWarning {
            combo: combo.display_string(),
            explanation: WIN_MODIFIER,
        });
    }
    if combo.vk == VK_F12 {
        return Some(HotkeyWarning {
            combo: combo.display_string(),
            explanation: F12_RESERVED,
        });
    }
    None
}

/// Прочитать настройку переключателя раскладки из реестра пользователя.
///
/// Ключ `HKCU\Keyboard Layout\Toggle`, значения `Hotkey`, `Language Hotkey`,
/// `Layout Hotkey` (REG_SZ): «1» — Alt+Shift, «2» — Ctrl+Shift, «3» — выключено.
/// Ключа/значения нет — Windows использует настройку по умолчанию, Alt+Shift
/// (измерено). Чтение никогда не падает: сбой реестра — повод предупредить по
/// умолчанию, а не молчать (тихий отказ хоткея дороже лишнего предупреждения).
pub fn read_layout_toggle() -> LayoutToggleConfig {
    read_layout_toggle_from_root(HKEY_CURRENT_USER, TOGGLE_KEY)
}

/// Чтение из заданного корня и подключа: `root` + `subkey` открываются
/// вместе — так тесты могут читать временные ключи, не трогая настоящий.
fn read_layout_toggle_from_root(root: HKEY, subkey: &str) -> LayoutToggleConfig {
    let key = match ToggleKey::open(root, subkey) {
        Ok(key) => key,
        Err(code) => {
            // Ключа нет или его нельзя прочитать — Windows живёт с настройкой
            // по умолчанию; предупреждаем соответственно.
            if code != ERROR_FILE_NOT_FOUND.0 {
                tracing::warn!(
                    code,
                    subkey,
                    "не удалось прочитать ключ переключателя раскладки"
                );
            }
            return LayoutToggleConfig::defaults();
        }
    };
    let mut cfg = LayoutToggleConfig {
        alt_shift: false,
        ctrl_shift: false,
    };
    for (name, missing_default) in [
        ("Hotkey", true),
        ("Language Hotkey", true),
        ("Layout Hotkey", false),
    ] {
        match read_value(key.0, name) {
            // Явное значение — источник истины: «1»/«2» назначают переключатель,
            // «3» и всё неизвестное — выключено.
            Ok(Some(v)) => match v.trim() {
                "1" => cfg.alt_shift = true,
                "2" => cfg.ctrl_shift = true,
                _ => {}
            },
            // Значение отсутствует — Windows берёт свою настройку по умолчанию.
            // Для Hotkey и Language Hotkey дефолт измерен и документирован
            // (Alt+Shift); для Layout Hotkey дефолт зависит от числа раскладок
            // в языке — не гадаем.
            Ok(None) => {
                if missing_default {
                    cfg.alt_shift = true;
                }
            }
            Err(code) => {
                tracing::warn!(
                    code,
                    name,
                    "не удалось прочитать значение переключателя раскладки"
                );
            }
        }
    }
    cfg
}

/// Значение ключа как строка: `None` — значения нет (ERROR_FILE_NOT_FOUND).
fn read_value(hkey: HKEY, name: &str) -> Result<Option<String>, u32> {
    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut data = [0u16; 32];
    let mut size = (data.len() * 2) as u32;
    let mut value_type = windows::Win32::System::Registry::REG_VALUE_TYPE::default();
    // SAFETY: data — буфер достаточного размера; size и value_type обновляет
    // система; name_wide — валидная nul-terminated wide-строка, живущая до
    // конца вызова.
    let ret = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut value_type),
            Some(data.as_mut_ptr().cast::<u8>()),
            Some(&mut size),
        )
    };
    if ret.0 == ERROR_FILE_NOT_FOUND.0 {
        return Ok(None);
    }
    if ret != windows::Win32::Foundation::ERROR_SUCCESS {
        return Err(ret.0);
    }
    // Значения переключателя — REG_SZ; всё иное (например, REG_DWORD) система
    // в этот ключ не пишет, и трактовать чужие типы как настройку нельзя.
    if value_type != REG_SZ {
        return Ok(None);
    }
    let len = size as usize / 2;
    let s: String = data[..len.min(data.len())]
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect();
    Ok(Some(s))
}

/// Обёртка над `HKEY`, закрывающая ключ в `Drop` (CONTRIBUTING.md, «Правила unsafe»).
struct ToggleKey(HKEY);

impl ToggleKey {
    fn open(root: HKEY, subkey: &str) -> Result<Self, u32> {
        let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey = HKEY::default();
        // SAFETY: subkey_wide — валидная nul-terminated wide-строка, hkey
        // получает владение действительным HKEY при успехе (ERROR_SUCCESS).
        let ret = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(subkey_wide.as_ptr()),
                Some(0),
                KEY_QUERY_VALUE,
                &mut hkey,
            )
        };
        if ret != windows::Win32::Foundation::ERROR_SUCCESS {
            return Err(ret.0);
        }
        Ok(Self(hkey))
    }
}

impl Drop for ToggleKey {
    fn drop(&mut self) {
        // SAFETY: self.0 всегда действительный открытый ключ (см. `open`).
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCreateKeyExW, RegDeleteKeyW, RegSetValueExW,
    };

    fn combo(s: &str) -> HotkeyCombo {
        HotkeyCombo::parse(s).expect("валидная комбинация в тесте")
    }

    static TEST_KEY_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Уникальное имя временного тестового ключа: параллельные тесты не
    /// должны сталкиваться на одном ключе (cargo test запускает тесты
    /// в потоках).
    fn unique_test_key() -> String {
        let n = TEST_KEY_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("Software\\resticker-hotkey-test-{}-{n}", std::process::id())
    }

    /// Временный ключ реестра под HKCU, удаляемый в `Drop` даже при панике теста.
    struct TestToggleKey {
        name: String,
    }

    impl TestToggleKey {
        fn create(values: &[(&str, &str)]) -> Self {
            let name = unique_test_key();
            let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let mut hkey = HKEY::default();
            // SAFETY: валидная wide-строка; phkresult получает владение ключом
            // при успехе (ERROR_SUCCESS).
            let ret = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(name_wide.as_ptr()),
                    Some(0),
                    PCWSTR(std::ptr::null()),
                    windows::Win32::System::Registry::REG_OPEN_CREATE_OPTIONS(0),
                    KEY_SET_VALUE,
                    None,
                    &mut hkey,
                    None,
                )
            };
            assert_eq!(
                ret,
                windows::Win32::Foundation::ERROR_SUCCESS,
                "создание тестового ключа"
            );
            for (value_name, value) in values {
                let value_name_wide: Vec<u16> = value_name
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let value_wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes: &[u8] = unsafe {
                    std::slice::from_raw_parts(
                        value_wide.as_ptr().cast::<u8>(),
                        value_wide.len() * 2,
                    )
                };
                // SAFETY: hkey действителен; bytes — слайс из живого Vec.
                let ret = unsafe {
                    RegSetValueExW(
                        hkey,
                        PCWSTR(value_name_wide.as_ptr()),
                        Some(0),
                        REG_SZ,
                        Some(bytes),
                    )
                };
                assert_eq!(
                    ret,
                    windows::Win32::Foundation::ERROR_SUCCESS,
                    "запись значения"
                );
            }
            // SAFETY: hkey открыт выше.
            unsafe {
                let _ = RegCloseKey(hkey);
            }
            Self { name }
        }
    }

    impl Drop for TestToggleKey {
        fn drop(&mut self) {
            let name_wide: Vec<u16> = self.name.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: валидная wide-строка пути.
            unsafe {
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(name_wide.as_ptr()));
            }
        }
    }

    fn read_from(test_key: &TestToggleKey) -> LayoutToggleConfig {
        read_layout_toggle_from_root(HKEY_CURRENT_USER, &test_key.name)
    }

    // --- чистая логика проверки ---

    #[test]
    fn alt_shift_combo_warns_when_toggle_is_alt_shift() {
        let toggle = LayoutToggleConfig {
            alt_shift: true,
            ctrl_shift: false,
        };
        let warning = check_risky_combo_with_toggle(combo("Alt+Shift+S"), toggle)
            .expect("Alt+Shift при активном переключателе обязано предупредить");
        assert_eq!(warning.combo, "Alt+Shift+S");
        assert!(warning.explanation.contains("Alt+Shift"));
    }

    #[test]
    fn alt_shift_combo_is_silent_when_toggle_is_off() {
        let toggle = LayoutToggleConfig {
            alt_shift: false,
            ctrl_shift: false,
        };
        assert_eq!(
            check_risky_combo_with_toggle(combo("Alt+Shift+S"), toggle),
            None
        );
    }

    #[test]
    fn ctrl_shift_combo_warns_when_toggle_is_ctrl_shift() {
        let toggle = LayoutToggleConfig {
            alt_shift: false,
            ctrl_shift: true,
        };
        let warning = check_risky_combo_with_toggle(combo("Ctrl+Shift+1"), toggle)
            .expect("Ctrl+Shift при активном переключателе обязано предупредить");
        assert_eq!(warning.combo, "Ctrl+Shift+1");
        assert!(warning.explanation.contains("Ctrl+Shift"));
    }

    #[test]
    fn ctrl_shift_combo_is_silent_when_toggle_is_alt_shift() {
        let toggle = LayoutToggleConfig {
            alt_shift: true,
            ctrl_shift: false,
        };
        assert_eq!(
            check_risky_combo_with_toggle(combo("Ctrl+Shift+1"), toggle),
            None
        );
    }

    #[test]
    fn three_modifier_combo_warns_if_it_contains_toggle_pair() {
        // Ctrl+Alt+Shift+S содержит Alt+Shift — переключатель съедает его
        // точно так же, как и двухмодификаторный вариант.
        let toggle = LayoutToggleConfig {
            alt_shift: true,
            ctrl_shift: false,
        };
        let warning = check_risky_combo_with_toggle(combo("Ctrl+Alt+Shift+S"), toggle)
            .expect("Alt+Shift внутри трёхмодификаторной комбинации обязано предупредить");
        assert!(warning.explanation.contains("Alt+Shift"));
    }

    #[test]
    fn any_win_combo_warns() {
        let toggle = LayoutToggleConfig {
            alt_shift: false,
            ctrl_shift: false,
        };
        for s in ["Win+S", "Win+F2", "Win+1", "Win+Shift+F12"] {
            let warning = check_risky_combo_with_toggle(combo(s), toggle)
                .unwrap_or_else(|| panic!("{s} обязано предупредить о Win"));
            assert!(warning.explanation.contains("Win"));
        }
    }

    #[test]
    fn toggle_warning_has_priority_over_win_warning() {
        // Комбинация с Win И Alt+Shift: объяснять надо самое вероятное —
        // переключатель раскладки, а не общее «зарезервировано ОС».
        let toggle = LayoutToggleConfig {
            alt_shift: true,
            ctrl_shift: false,
        };
        let warning = check_risky_combo_with_toggle(combo("Win+Alt+Shift+S"), toggle)
            .expect("обязано предупредить");
        assert!(warning.explanation.contains("Alt+Shift назначен"));
        assert!(!warning.explanation.contains("клавишей Win"));
    }

    #[test]
    fn f12_combo_warns() {
        let toggle = LayoutToggleConfig {
            alt_shift: false,
            ctrl_shift: false,
        };
        // Голый «F12» парсер не принимает (нужен модификатор) — проверяем
        // только выразимые комбинации с F12.
        for s in ["Ctrl+F12", "Ctrl+Shift+F12"] {
            let warning = check_risky_combo_with_toggle(combo(s), toggle)
                .unwrap_or_else(|| panic!("{s} обязано предупредить о F12"));
            assert!(warning.explanation.contains("F12"));
        }
    }

    #[test]
    fn safe_combos_do_not_warn() {
        let toggle = LayoutToggleConfig {
            alt_shift: false,
            ctrl_shift: false,
        };
        for s in [
            "Ctrl+S",
            "Alt+S",
            "Ctrl+Alt+S",
            "Ctrl+Shift+F11",
            "Shift+F5",
            "Ctrl+F1",
        ] {
            assert_eq!(
                check_risky_combo_with_toggle(combo(s), toggle),
                None,
                "{s} не должно предупреждать"
            );
        }
    }

    // --- чтение реестра ---

    #[test]
    fn missing_registry_key_means_default_alt_shift_toggle() {
        // Ключа нет вовсе — Windows живёт с настройкой по умолчанию: Alt+Shift
        // переключает язык (измерено на стенде: голый Shift+Alt переключил
        // раскладку при отсутствии ключа).
        let name = unique_test_key();
        let cfg = read_layout_toggle_from_root(HKEY_CURRENT_USER, &name);
        assert_eq!(
            cfg,
            LayoutToggleConfig {
                alt_shift: true,
                ctrl_shift: false,
            }
        );
    }

    #[test]
    fn registry_values_1_enable_alt_shift_toggle() {
        let key = TestToggleKey::create(&[
            ("Hotkey", "1"),
            ("Language Hotkey", "1"),
            ("Layout Hotkey", "1"),
        ]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: true,
                ctrl_shift: false,
            }
        );
    }

    #[test]
    fn registry_values_2_enable_ctrl_shift_toggle() {
        let key = TestToggleKey::create(&[
            ("Hotkey", "2"),
            ("Language Hotkey", "2"),
            ("Layout Hotkey", "2"),
        ]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: false,
                ctrl_shift: true,
            }
        );
    }

    #[test]
    fn registry_values_3_disable_both_toggles() {
        let key = TestToggleKey::create(&[
            ("Hotkey", "3"),
            ("Language Hotkey", "3"),
            ("Layout Hotkey", "3"),
        ]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: false,
                ctrl_shift: false,
            }
        );
    }

    #[test]
    fn mixed_registry_values_combine_both_toggles() {
        // Hotkey=1 (Alt+Shift) + Layout Hotkey=2 (Ctrl+Shift): назначены оба
        // переключателя — предупреждать нужно про обе пары модификаторов.
        let key = TestToggleKey::create(&[("Hotkey", "1"), ("Layout Hotkey", "2")]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: true,
                ctrl_shift: true,
            }
        );
    }

    #[test]
    fn registry_value_with_surrounding_spaces_is_parsed() {
        // Windows (и reg-файлы из интернета) умеют писать « 3 » с пробелами —
        // читать надо после trim. Остальные значения выключены явно («3»),
        // чтобы проверялся именно пробел в значении.
        let key = TestToggleKey::create(&[
            ("Hotkey", "3"),
            ("Language Hotkey", " 2 "),
            ("Layout Hotkey", "3"),
        ]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: false,
                ctrl_shift: true,
            }
        );
    }

    #[test]
    fn unknown_registry_value_is_ignored() {
        // Незнакомое значение не назначает переключатель: предупреждать без
        // основания нельзя.
        let key = TestToggleKey::create(&[
            ("Hotkey", "3"),
            ("Language Hotkey", "7"),
            ("Layout Hotkey", "3"),
        ]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: false,
                ctrl_shift: false,
            }
        );
    }

    #[test]
    fn partially_configured_key_still_defaults_missing_values_to_alt_shift() {
        // Ручная правка, когда написано только одно значение, — то же «настройка
        // не записана» для остальных: Windows берёт встроенный дефолт (Alt+Shift,
        // измерен для состояния без ключа). Молчать здесь — значит вернуть
        // пользователю «тихий отказ» из задачи H2.
        let key = TestToggleKey::create(&[("Language Hotkey", "2")]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: true,
                ctrl_shift: true,
            }
        );
    }

    #[test]
    fn present_key_without_values_keeps_measured_defaults() {
        // Ключ существует, но значений в нём нет — Windows снова на настройке
        // по умолчанию для языка (Alt+Shift). Для Layout Hotkey дефолт не
        // измерен — не предупреждаем.
        let key = TestToggleKey::create(&[]);
        assert_eq!(
            read_from(&key),
            LayoutToggleConfig {
                alt_shift: true,
                ctrl_shift: false,
            }
        );
    }
}
