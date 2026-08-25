//! Глобальный хоткей входа/выхода из режима редактирования через
//! `RegisterHotKey` (ARCHITECTURE.md, раздел 5.1; ADR-009).
//!
//! Регистрация привязана к потоку: [`RegisteredHotkey`] обязан создаваться
//! и уничтожаться на потоке с циклом сообщений (оверлей-поток, ADR-013) —
//! тип намеренно `!Send`. Низкоуровневый хук `WH_KEYBOARD_LL` не используется
//! даже как fallback (ADR-009) — НО только для этого хоткея: тайлинговый слой
//! (docs/TILING_DESIGN.md §T3) принесёт собственный `WH_KEYBOARD_LL` для
//! десятков биндов и модальных режимов, которым `RegisterHotKey` в принципе
//! не подходит (docs/research/tiling/R4_KEYBINDS.md §1). Этот файл готовит
//! общий парсер: именованные клавиши для биндов и честный ответ, заберёт ли
//! Windows комбинацию себе ([`HotkeyCombo::is_reserved_by_windows`]).
//!
//! Парсер — не только для `RegisterHotKey`: [`HotkeyCombo::parse_binding`]
//! допускает одиночные клавиши без модификаторов (модальные submap-режимы),
//! поэтому НЕ спрашивай у него «зарегистрируется ли это» — этот вопрос
//! задаётся [`HotkeyCombo::to_win32`], и он падает на голой клавише.

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
    /// Разобрать строку формата `"Ctrl+Alt+S"` для `RegisterHotKey`. Регистр
    /// и пробелы вокруг токенов игнорируются. Поддерживаются модификаторы
    /// `Ctrl`/`Alt`/`Shift`/`Win` и основная клавиша: латинская буква, цифра,
    /// `F1`–`F24` или именованная клавиша ([`NAMED_KEYS`], включая синонимы
    /// `Super`/`Meta`, `Return`, `Esc`, `PgUp`/`PgDn`).
    ///
    /// Комбинация БЕЗ модификатора отвергается: `RegisterHotKey` такую
    /// зарегистрировать не может, а молча проглотить её в конфиге — значит
    /// создать бинд, который никогда не сработает. Для голых клавиш (субмап-
    /// режимы тайлинга) есть [`Self::parse_binding`].
    pub fn parse(s: &str) -> Result<Self, Win32Error> {
        Self::parse_impl(s, false)
    }

    /// Разобрать строку бинда тайлинга — то же, что [`Self::parse`], но
    /// допускает клавишу БЕЗ модификаторов (`"H"`, `"Left"`, `"Escape"`).
    ///
    /// Такие бинды живут только в модальных режимах (submap) на
    /// низкоуровневом хуке `WH_KEYBOARD_LL` (docs/research/tiling/
    /// R4_KEYBINDS.md §2, §4.2): там одиночная клавиша осмысленна и не
    /// перехватывает обычный ввод — режим активен временно и явно.
    /// Проверить, пройдёт ли комбинация через `RegisterHotKey`, — это
    /// отдельный вопрос, у него свой ответ: [`Self::to_win32`].
    pub fn parse_binding(s: &str) -> Result<Self, Win32Error> {
        Self::parse_impl(s, true)
    }

    fn parse_impl(s: &str, allow_naked: bool) -> Result<Self, Win32Error> {
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
            } else if token.eq_ignore_ascii_case("win")
                || token.eq_ignore_ascii_case("super")
                || token.eq_ignore_ascii_case("meta")
            {
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
        // Хоткей без модификаторов перехватывал бы обычный ввод — для
        // `RegisterHotKey` запрещаем (см. `parse`). `parse_binding` —
        // единственный вход для голых клавиш: они осмысленны только в
        // временных модальных режимах на `WH_KEYBOARD_LL`.
        if !allow_naked && !(combo.ctrl || combo.alt || combo.shift || combo.win) {
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
    ///
    /// Комбинация БЕЗ модификатора — ошибка, а не `(0, vk)`: `RegisterHotKey`
    /// такую зарегистрировать не может (она перехватывала бы обычный ввод во
    /// всей ОС), а одиночные клавиши тайлинга будут жить на низкоуровневом
    /// хуке `WH_KEYBOARD_LL` в модальных режимах (docs/TILING_DESIGN.md §T3;
    /// docs/research/tiling/R4_KEYBINDS.md §2). Молча зарегистрировать нельзя,
    /// молча пропустить — тоже: хоткей просто «пропал» бы без объяснений.
    /// Ошибка — единственный честный путь; конфиг увидит её и скажет
    /// пользователю, что голую клавишу нужно вешать не на `RegisterHotKey`.
    fn to_win32(self) -> Result<(HOT_KEY_MODIFIERS, u32), Win32Error> {
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
        if !(self.ctrl || self.alt || self.shift || self.win) {
            return Err(Win32Error::InvalidHotkey(format!(
                "{}: a bare key without modifiers cannot be registered via RegisterHotKey \
                 (naked keys are valid only for the low-level keyboard hook in submap modes)",
                self.display_string()
            )));
        }
        Ok((mods, self.vk))
    }

    /// Комбинация зарезервирована самой Windows: `RegisterHotKey` на неё
    /// провалится с `ERROR_HOTKEY_ALREADY_REGISTERED`, либо система
    /// перехватит её раньше нас. Список — ровно из docs/research/tiling/
    /// R4_KEYBINDS.md §1.1: `Win+L` (блокировка), `Win+Tab` (Task View),
    /// `Win+D/E/R/I/S/A/N`, `Win+1..=9`, `Win+стрелки` (Snap), `Ctrl+Alt+Del`
    /// (SAS — ядро ловит раньше любой очереди сообщений) и `Ctrl+Shift+Esc`
    /// (диспетчер задач).
    ///
    /// Нужно, чтобы настройки честно сказали пользователю «эту комбинацию
    /// назначить нельзя, и почему»: механизм конфликтного тоста уже есть —
    /// `OverlayEvent::HotkeyConflict` рисует баннер
    /// (crates/resticker/src/overlay_manager.rs:3006) через
    /// `i18n::hotkey_conflict_notification` (crates/resticker/src/i18n.rs:64).
    /// Список намеренно ТОЧНЫЙ, не эвристический: предсказать все системные
    /// комбинации нельзя, поэтому про не перечисленное предикат честно
    /// говорит «не знаю» (`false`), а реальный конфликт поймает сам
    /// `RegisterHotKey` при регистрации.
    pub fn is_reserved_by_windows(&self) -> bool {
        // Ctrl+Alt+Del — аппаратный SAS: его не отдаст даже WH_KEYBOARD_LL
        // (R4 §3.1), не то что RegisterHotKey. Delete = 0x2E.
        if self.ctrl && self.alt && !self.shift && !self.win && self.vk == 0x2E {
            return true;
        }
        // Ctrl+Shift+Esc — диспетчер задач (explorer держит приоритет).
        // Escape = 0x1B.
        if self.ctrl && self.shift && !self.alt && !self.win && self.vk == 0x1B {
            return true;
        }
        // Чистые Win+... комбинации оболочки (без дополнительных
        // модификаторов — shell перехватывает именно их).
        if self.win && !self.ctrl && !self.alt && !self.shift {
            // Win+L (0x4C), Win+Tab (0x09), Win+D (0x44), Win+E/R/I/S/A/N,
            // Win+стрелки (0x25..=0x28), Win+1..9 (0x31..=0x39).
            return matches!(
                self.vk,
                0x4C | 0x09 | 0x44 | 0x45 | 0x52 | 0x49 | 0x53 | 0x41 | 0x4E | 0x25..=0x28
                    | 0x31..=0x39
            );
        }
        false
    }
}

/// Виртуальные коды именованных клавиш. Каноническое имя — ПЕРВЫМ в паре,
/// синонимы — следом: [`key_name`] ищет первое вхождение кода, поэтому
/// roundtrip `parse -> display -> parse` возвращает канонический вид
/// (`"PgUp"` в конфиге напечатается как `"PageUp"`).
///
/// Коды не пересекаются с диапазонами [`parse_key`] (A–Z, 0–9, F1–F24),
/// поэтому «Ctrl+F» остаётся клавишей `F`, а не F-префиксом, а «Ctrl+S» —
/// клавишей `S`, а не чем-то ещё.
///
/// VK-коды записаны шестнадцатеричными литералами, как в остальном файле
/// (F1 = 0x70): импорт `VK_*` из `windows`-крейта дал бы `VIRTUAL_KEY`
/// (u16-обёртку) и касты к `u32` на каждом использовании без выигрыша в
/// читаемости.
const NAMED_KEYS: &[(&str, u32)] = &[
    // Навигация (VK_LEFT..VK_DOWN = 0x25..0x28, VK_PRIOR = 0x21, VK_NEXT = 0x22).
    ("Left", 0x25),
    ("Right", 0x27),
    ("Up", 0x26),
    ("Down", 0x28),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PgUp", 0x21),
    ("PageDown", 0x22),
    ("PgDn", 0x22),
    // Правка и ввод.
    ("Enter", 0x0D), // VK_RETURN
    ("Return", 0x0D),
    ("Space", 0x20),  // VK_SPACE
    ("Tab", 0x09),    // VK_TAB
    ("Escape", 0x1B), // VK_ESCAPE
    ("Esc", 0x1B),
    ("Backspace", 0x08), // VK_BACK
    ("Delete", 0x2E),    // VK_DELETE
    ("Insert", 0x2D),    // VK_INSERT
    // Пунктуация (VK_OEM_* — раскладко-зависимые, имена фиксированные).
    ("Minus", 0xBD),        // VK_OEM_MINUS
    ("Equal", 0xBB),        // VK_OEM_PLUS
    ("BracketLeft", 0xDB),  // VK_OEM_4
    ("BracketRight", 0xDD), // VK_OEM_6
    ("Semicolon", 0xBA),    // VK_OEM_1
    ("Quote", 0xDE),        // VK_OEM_7
    ("Backslash", 0xDC),    // VK_OEM_5
    ("Comma", 0xBC),        // VK_OEM_COMMA
    ("Period", 0xBE),       // VK_OEM_PERIOD
    ("Slash", 0xBF),        // VK_OEM_2
    ("Grave", 0xC0),        // VK_OEM_3
    // Цифровой блок — тот же лейаут, что у GlazeWM в конфиге биндов
    // (VK_NUMPAD0..VK_NUMPAD9 = 0x60..0x69).
    ("NumPad0", 0x60),
    ("NumPad1", 0x61),
    ("NumPad2", 0x62),
    ("NumPad3", 0x63),
    ("NumPad4", 0x64),
    ("NumPad5", 0x65),
    ("NumPad6", 0x66),
    ("NumPad7", 0x67),
    ("NumPad8", 0x68),
    ("NumPad9", 0x69),
];

/// Виртуальный код клавиши по имени токена: `A`–`Z`, `0`–`9`, `F1`–`F24`
/// (раскладко-зависимые клавиши требуют `VkKeyScan` — сознательно
/// не поддерживаются) или именованная клавиша из [`NAMED_KEYS`] с
/// синонимами, регистр не важен.
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
    for (name, vk) in NAMED_KEYS {
        if name.eq_ignore_ascii_case(token) {
            return Some(*vk);
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
        // Каноническое имя из таблицы: синонимы (Return, Esc, PgUp...) здесь
        // намеренно не всплывают — display_string печатает один канон.
        _ => NAMED_KEYS
            .iter()
            .find(|(_, code)| *code == vk)
            .map_or_else(|| format!("VK{vk:#04X}"), |(name, _)| (*name).to_string()),
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
        let (mods, vk) = combo.to_win32()?;
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
        let (mods, vk) = combo.to_win32().expect("с модификатором — регистрируется");
        assert_ne!(mods.0 & MOD_NOREPEAT.0, 0);
        assert_ne!(mods.0 & MOD_CONTROL.0, 0);
        assert_eq!(mods.0 & MOD_ALT.0, 0);
        assert_eq!(vk, 'S' as u32);
    }

    // --- T2: именованные клавиши, синонимы, голые бинды, reserved ---

    fn parse_named(key: &str) -> HotkeyCombo {
        HotkeyCombo::parse(&format!("Alt+{key}")).expect("именованная клавиша валидна")
    }

    #[test]
    fn navigation_keys_parse() {
        for (name, vk) in [
            ("Left", 0x25),
            ("Right", 0x27),
            ("Up", 0x26),
            ("Down", 0x28),
        ] {
            assert_eq!(parse_named(name).vk, vk, "{name}");
        }
    }

    #[test]
    fn editing_keys_parse() {
        for (name, vk) in [
            ("Enter", 0x0D),
            ("Space", 0x20),
            ("Tab", 0x09),
            ("Escape", 0x1B),
            ("Backspace", 0x08),
            ("Delete", 0x2E),
            ("Home", 0x24),
            ("End", 0x23),
            ("PageUp", 0x21),
            ("PageDown", 0x22),
            ("Insert", 0x2D),
        ] {
            assert_eq!(parse_named(name).vk, vk, "{name}");
        }
    }

    #[test]
    fn punctuation_keys_parse() {
        for (name, vk) in [
            ("Minus", 0xBD),
            ("Equal", 0xBB),
            ("BracketLeft", 0xDB),
            ("BracketRight", 0xDD),
            ("Semicolon", 0xBA),
            ("Quote", 0xDE),
            ("Backslash", 0xDC),
            ("Comma", 0xBC),
            ("Period", 0xBE),
            ("Slash", 0xBF),
            ("Grave", 0xC0),
        ] {
            assert_eq!(parse_named(name).vk, vk, "{name}");
        }
    }

    #[test]
    fn numpad_keys_parse() {
        for n in 0..=9 {
            let key = format!("NumPad{n}");
            assert_eq!(parse_named(&key).vk, 0x60 + n, "{key}");
        }
    }

    #[test]
    fn modifier_synonyms_are_equivalent() {
        for s in ["Win+S", "Super+S", "Meta+S", "super + s", "META+S"] {
            let combo = HotkeyCombo::parse(s).expect("синоним валиден");
            assert!(
                combo.win && !combo.ctrl && !combo.alt && !combo.shift,
                "{s}"
            );
            assert_eq!(combo.vk, 'S' as u32);
        }
        // Повтор синонима — та же ошибка дубликата, что повтор Win.
        assert!(HotkeyCombo::parse("Super+Meta+S").is_err());
    }

    #[test]
    fn key_synonyms_are_equivalent() {
        for (a, b) in [
            ("Alt+Return", "Alt+Enter"),
            ("Alt+Esc", "Alt+Escape"),
            ("Alt+PgUp", "Alt+PageUp"),
            ("Alt+PgDn", "Alt+PageDown"),
            ("Ctrl+S", "Control+S"),
        ] {
            assert_eq!(
                HotkeyCombo::parse(a).expect("синоним валиден"),
                HotkeyCombo::parse(b).expect("канон валиден"),
                "{a} == {b}"
            );
        }
    }

    #[test]
    fn named_keys_are_case_insensitive() {
        assert_eq!(parse_named("LEFT"), parse_named("Left"));
        assert_eq!(parse_named("numpad5"), parse_named("NumPad5"));
        assert_eq!(parse_named("eScApE"), parse_named("Escape"));
    }

    #[test]
    fn parse_still_rejects_bare_key() {
        for bare in ["S", "Left", "Space", "1", "F5"] {
            assert!(
                matches!(HotkeyCombo::parse(bare), Err(Win32Error::InvalidHotkey(_))),
                "{bare:?}: parse обязан требовать модификатор"
            );
        }
    }

    #[test]
    fn parse_binding_accepts_bare_key() {
        let combo = HotkeyCombo::parse_binding("H").expect("голая клавиша в бинде валидна");
        assert!(!combo.ctrl && !combo.alt && !combo.shift && !combo.win);
        assert_eq!(combo.vk, 'H' as u32);

        // Стрелки и модифицированные комбинации работают в обоих парсерах.
        assert_eq!(HotkeyCombo::parse_binding("Left").unwrap().vk, 0x25);
        assert_eq!(HotkeyCombo::parse_binding("Alt+Left").unwrap().vk, 0x25);
    }

    #[test]
    fn parse_binding_rejects_garbage() {
        for bad in [
            "",
            "+++",
            "Alt+",
            "Alt+===",
            "Ctrl+Left+Right",
            "Alt+NumPad10",
            "Alt+Ё",
        ] {
            assert!(
                matches!(
                    HotkeyCombo::parse_binding(bad),
                    Err(Win32Error::InvalidHotkey(_))
                ),
                "строка {bad:?} должна отвергаться и parse_binding"
            );
        }
    }

    #[test]
    fn to_win32_refuses_bare_key() {
        // RegisterHotKey не может зарегистрировать голую клавишу; тост-механика
        // конфликта: overlay_manager.rs:3006. Ошибка, а не тихий (0, vk).
        let combo = HotkeyCombo::parse_binding("Space").expect("голая клавиша парсится");
        assert!(matches!(
            combo.to_win32(),
            Err(Win32Error::InvalidHotkey(_))
        ));
        // С модификатором — ок.
        assert!(HotkeyCombo::parse("Ctrl+Space").unwrap().to_win32().is_ok());
    }

    #[test]
    fn register_refuses_bare_key() {
        // Публичный путь: регистрация голой клавиши обязана упасть ДО
        // обращения к RegisterHotKey — системный вызов такую не принял бы,
        // а молчаливый пропуск сделал бы бинд «мёртвым».
        let combo = HotkeyCombo::parse_binding("H").expect("голая клавиша парсится");
        assert!(matches!(
            RegisteredHotkey::register(0x4F01, combo),
            Err(Win32Error::InvalidHotkey(_))
        ));
    }

    #[test]
    fn display_prints_canonical_names_and_synonyms_collapse() {
        for name in [
            "Left",
            "Space",
            "Escape",
            "PageUp",
            "NumPad3",
            "Grave",
            "Semicolon",
        ] {
            let combo = parse_named(name);
            assert_eq!(combo.display_string(), format!("Alt+{name}"));
        }
        // Синоним печатается каноном — roundtrip не дрейфует.
        assert_eq!(parse_named("PgUp").display_string(), "Alt+PageUp");
        assert_eq!(parse_named("Esc").display_string(), "Alt+Escape");
        assert_eq!(
            HotkeyCombo::parse("Super+S").unwrap().display_string(),
            "Win+S"
        );
    }

    #[test]
    fn display_round_trip_through_parse_binding() {
        for s in ["H", "Left", "Alt+Right", "Ctrl+Shift+NumPad0", "Win+Space"] {
            let combo = HotkeyCombo::parse_binding(s).expect("биндовый формат валиден");
            let printed = combo.display_string();
            let back =
                HotkeyCombo::parse_binding(&printed).expect("display_string читается обратно");
            assert_eq!(back, combo, "{s:?} -> {printed:?}");
        }
    }

    #[test]
    fn win_shell_combos_are_reserved() {
        for s in [
            "Win+L",
            "Win+Tab",
            "Win+D",
            "Win+E",
            "Win+R",
            "Win+I",
            "Win+S",
            "Win+A",
            "Win+N",
            "Win+1",
            "Win+9",
            "Win+Left",
            "Win+Right",
            "Win+Up",
            "Win+Down",
        ] {
            let combo = HotkeyCombo::parse(s).expect("валидная комбинация");
            assert!(
                combo.is_reserved_by_windows(),
                "{s} должна быть зарезервирована"
            );
        }
    }

    #[test]
    fn secure_attention_combos_are_reserved() {
        assert!(
            HotkeyCombo::parse("Ctrl+Alt+Delete")
                .unwrap()
                .is_reserved_by_windows()
        );
        assert!(
            HotkeyCombo::parse("Ctrl+Shift+Esc")
                .unwrap()
                .is_reserved_by_windows()
        );
    }

    #[test]
    fn ordinary_combos_are_not_reserved() {
        for s in [
            "Alt+F4",
            "Ctrl+Alt+Shift+Delete",
            "Win+0",
            "Ctrl+S",
            "Ctrl+Shift+Tab",
        ] {
            let combo = HotkeyCombo::parse(s).expect("валидная комбинация");
            assert!(
                !combo.is_reserved_by_windows(),
                "{s} не должна считаться зарезервированной"
            );
        }
        // Голая клавиша — не «зарезервирована Windows»: это отдельный вопрос
        // (её не примет RegisterHotKey), и отвечает на него to_win32.
        assert!(
            !HotkeyCombo::parse_binding("H")
                .unwrap()
                .is_reserved_by_windows()
        );
    }

    #[test]
    fn garbage_input_returns_error_not_panic() {
        for bad in [
            "",
            "   ",
            "Alt+===",
            "+++",
            "Ctrl+Ctrl",
            "Alt+NumPad10",
            "Ctrl+Ё",
            "F0",
            "F25",
        ] {
            let _ = HotkeyCombo::parse(bad);
            let _ = HotkeyCombo::parse_binding(bad);
        }
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
