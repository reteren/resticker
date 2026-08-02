//! Глобальный хоткей входа/выхода из режима редактирования через
//! `RegisterHotKey` (ARCHITECTURE.md, раздел 5.1; ADR-009).
//!
//! Регистрация привязана к потоку: [`RegisteredHotkey`] обязан создаваться
//! и уничтожаться на потоке с циклом сообщений (оверлей-поток, ADR-013) —
//! тип намеренно `!Send`. Низкоуровневый хук `WH_KEYBOARD_LL` не используется
//! даже как fallback (ADR-009).

use std::marker::PhantomData;

use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::core::HRESULT;

use crate::error::Win32Error;

/// Комбинация клавиш глобального хоткея (конфиг: строки вида `"Ctrl+Alt+S"`,
/// CONFIG.md, «hotkeys»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyCombo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Виртуальный код основной клавиши (`VK_*`).
    pub vk: u32,
}

impl HotkeyCombo {
    /// Разобрать строку формата `"Ctrl+Alt+S"`. Регистр и пробелы вокруг
    /// токенов игнорируются. Поддерживаются модификаторы `Ctrl`/`Alt`/`Shift`/
    /// `Win` и основная клавиша: латинская буква, цифра или `F1`–`F24`
    /// (раскладко-зависимые клавиши требуют `VkKeyScan` — сознательно
    /// не поддерживаются в 1.0).
    pub fn parse(s: &str) -> Result<Self, Win32Error> {
        let mut combo = Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            vk: 0,
        };
        let mut key_seen = false;

        for token in s.split('+') {
            let token = token.trim();
            if token.is_empty() {
                return Err(Win32Error::InvalidHotkey(s.to_string()));
            }
            if token.eq_ignore_ascii_case("ctrl") || token.eq_ignore_ascii_case("control") {
                if combo.ctrl {
                    return Err(Win32Error::InvalidHotkey(s.to_string()));
                }
                combo.ctrl = true;
            } else if token.eq_ignore_ascii_case("alt") {
                if combo.alt {
                    return Err(Win32Error::InvalidHotkey(s.to_string()));
                }
                combo.alt = true;
            } else if token.eq_ignore_ascii_case("shift") {
                if combo.shift {
                    return Err(Win32Error::InvalidHotkey(s.to_string()));
                }
                combo.shift = true;
            } else if token.eq_ignore_ascii_case("win") {
                if combo.win {
                    return Err(Win32Error::InvalidHotkey(s.to_string()));
                }
                combo.win = true;
            } else {
                if key_seen {
                    return Err(Win32Error::InvalidHotkey(s.to_string()));
                }
                combo.vk = parse_key(token).ok_or_else(|| Win32Error::InvalidHotkey(s.into()))?;
                key_seen = true;
            }
        }

        if !key_seen {
            return Err(Win32Error::InvalidHotkey(s.to_string()));
        }
        // Хоткей без модификаторов перехватывал бы обычный ввод — запрещаем.
        if !(combo.ctrl || combo.alt || combo.shift || combo.win) {
            return Err(Win32Error::InvalidHotkey(s.to_string()));
        }
        Ok(combo)
    }
}
impl HotkeyCombo {
    /// Каноничная строка вида `"Ctrl+Alt+S"` — для сообщений об ошибках
    /// и отображения в настройках.
    pub fn display_string(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push("Win".into());
        }
        parts.push(key_name(self.vk));
        parts.join("+")
    }

    /// Модификаторы и клавиша для `RegisterHotKey`. `MOD_NOREPEAT` — всегда:
    /// режим редактирования — переключатель (SPEC 3.1), и автоповтор зажатой
    /// клавиши не должен дёргать его туда-сюда.
    fn to_win32(self) -> (HOT_KEY_MODIFIERS, u32) {
        let mut mods = MOD_NOREPEAT;
        if self.ctrl {
            mods |= MOD_CONTROL;
        }
        if self.alt {
            mods |= MOD_ALT;
        }
        if self.shift {
            mods |= MOD_SHIFT;
        }
        if self.win {
            mods |= MOD_WIN;
        }
        (mods, self.vk)
    }
}

/// Виртуальный код клавиши по имени токена: `A`–`Z`, `0`–`9`, `F1`–`F24`.
fn parse_key(token: &str) -> Option<u32> {
    let upper = token.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            // VK_F1 = 0x70, VK_F24 = 0x87.
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    let mut chars = upper.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c as u32),
        _ => None,
    }
}

/// Имя клавиши по виртуальному коду (обратно к [`parse_key`]).
fn key_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => format!("VK{vk:#04X}"),
    }
}

/// Зарегистрированный глобальный хоткей. `Drop` снимает регистрацию.
///
/// Создание и `Drop` — строго на одном и том же потоке с циклом сообщений:
/// хоткей принадлежит потоку, и с чужого потока `UnregisterHotKey` его не
/// снимет (тип поэтому `!Send`).
#[derive(Debug)]
pub struct RegisteredHotkey {
    id: i32,
    _not_send: PhantomData<*const ()>,
}

impl RegisteredHotkey {
    /// Зарегистрировать хоткей `id` на текущем потоке. Конфликт (комбинация
    /// уже занята другим приложением) возвращается как
    /// [`Win32Error::HotkeyConflict`] — понятная ошибка, а не паника;
    /// пользователю предлагается выбрать другую комбинацию
    /// (ARCHITECTURE.md, раздел 5.1).
    pub fn register(id: i32, combo: HotkeyCombo) -> Result<Self, Win32Error> {
        // Для хоткея потока (hwnd = None) Win32 требует id из этого диапазона.
        if !(0..=0xBFFF).contains(&id) {
            return Err(Win32Error::InvalidHotkey(format!(
                "id хоткея {id} вне диапазона 0x0000..=0xBFFF"
            )));
        }
        let (mods, vk) = combo.to_win32();
        // SAFETY: hwnd=None — регистрация на текущем потоке; снятие
        // гарантированно тем же потоком в `Drop` (тип `!Send`).
        unsafe { RegisterHotKey(None, id, mods, vk) }.map_err(|e| {
            if e.code() == HRESULT::from_win32(ERROR_HOTKEY_ALREADY_REGISTERED.0) {
                Win32Error::HotkeyConflict(combo.display_string())
            } else {
                Win32Error::Win32(e)
            }
        })?;
        Ok(Self {
            id,
            _not_send: PhantomData,
        })
    }

    /// Идентификатор, приходящий в `wparam` сообщения `WM_HOTKEY`.
    pub fn id(&self) -> i32 {
        self.id
    }
}

impl Drop for RegisteredHotkey {
    fn drop(&mut self) {
        // SAFETY: тот же поток, что регистрировал (тип `!Send`), id — наш.
        // Ошибка игнорируется: при смерти потока снимать уже нечего.
        unsafe {
            let _ = UnregisterHotKey(None, self.id);
        }
    }
}

/// Извлечь id хоткея из `wparam` сообщения `WM_HOTKEY`.
pub fn message_hotkey_id(wparam: WPARAM) -> i32 {
    wparam.0 as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_combos() {
        let c = HotkeyCombo::parse("Ctrl+Alt+S").expect("валидная комбинация");
        assert!(c.ctrl && c.alt && !c.shift && !c.win);
        assert_eq!(c.vk, 'S' as u32);

        // Регистр и пробелы не важны.
        let c = HotkeyCombo::parse("  ctrl + SHIFT + f12 ").expect("валидная комбинация");
        assert!(c.ctrl && c.shift && !c.alt && !c.win);
        assert_eq!(c.vk, 0x70 + 11);

        let c = HotkeyCombo::parse("Win+F24").expect("валидная комбинация");
        assert!(c.win);
        assert_eq!(c.vk, 0x87);

        let c = HotkeyCombo::parse("Alt+9").expect("валидная комбинация");
        assert_eq!(c.vk, '9' as u32);

        // "F" сама по себе — клавиша F, а не префикс F-клавиш.
        let c = HotkeyCombo::parse("Ctrl+F").expect("валидная комбинация");
        assert_eq!(c.vk, 'F' as u32);
    }

    #[test]
    fn parse_rejects_invalid() {
        for bad in [
            "",
            "Ctrl",
            "Ctrl+",
            "Ctrl++S",
            "Ctrl+Ctrl+S",
            "Ctrl+Alt",
            "S",
            "Ctrl+S+D",
            "Ctrl+F25",
            "Ctrl+F0",
            "Ctrl+Space",
            "Ctrl+Й",
        ] {
            assert!(
                matches!(HotkeyCombo::parse(bad), Err(Win32Error::InvalidHotkey(_))),
                "строка {bad:?} должна отвергаться"
            );
        }
    }

    #[test]
    fn display_round_trip() {
        // display_string выводит модификаторы в каноничном порядке Ctrl, Alt, Shift, Win.
        for s in ["Ctrl+Alt+S", "Ctrl+Shift+F12", "Alt+Win+0"] {
            let combo = HotkeyCombo::parse(s).expect("валидная комбинация");
            assert_eq!(combo.display_string(), s);
        }
    }

    #[test]
    fn win32_modifiers_always_include_norepeat() {
        let combo = HotkeyCombo::parse("Ctrl+S").expect("валидная комбинация");
        let (mods, vk) = combo.to_win32();
        assert_ne!(mods.0 & MOD_NOREPEAT.0, 0);
        assert_ne!(mods.0 & MOD_CONTROL.0, 0);
        assert_eq!(mods.0 & MOD_ALT.0, 0);
        assert_eq!(vk, 'S' as u32);
    }

    #[test]
    fn hotkey_id_from_wparam() {
        assert_eq!(message_hotkey_id(WPARAM(42)), 42);
    }

    #[test]
    fn register_conflict_then_reregister_after_drop() {
        // Экзотическая комбинация, чтобы исключить конфликт с реально
        // работающими приложениями на машине разработчика/CI.
        let combo = HotkeyCombo::parse("Ctrl+Alt+Shift+F24").expect("валидная комбинация");
        let first = RegisteredHotkey::register(0x4E01, combo).expect("первичная регистрация");
        assert_eq!(first.id(), 0x4E01);

        // Та же комбинация в этом же процессе — детерминированный конфликт:
        // Windows отвечает ERROR_HOTKEY_ALREADY_REGISTERED.
        let err = match RegisteredHotkey::register(0x4E02, combo) {
            Ok(_) => panic!("повторная регистрация занятой комбинации должна отвергаться"),
            Err(e) => e,
        };
        match err {
            Win32Error::HotkeyConflict(s) => assert_eq!(s, "Ctrl+Alt+Shift+F24"),
            e => panic!("ожидался HotkeyConflict, получено: {e}"),
        }

        // После снятия регистрации комбинация снова доступна.
        drop(first);
        let second = RegisteredHotkey::register(0x4E02, combo)
            .expect("после Drop регистрация должна снова работать");
        drop(second);
    }
}
