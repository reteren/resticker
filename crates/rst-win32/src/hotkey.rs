//! Глобальный хоткей входа/выхода из режима редактирования через
//! `RegisterHotKey` (ARCHITECTURE.md, раздел 5.1; ADR-009).
//!
//! Регистрация привязана к потоку: [`RegisteredHotkey`] обязан создаваться
//! и уничтожаться на потоке с циклом сообщений (оверлей-поток, ADR-013) —
//! тип намеренно `!Send`. Низкоуровневый хук `WH_KEYBOARD_LL` не используется
//! даже как fallback (ADR-009).

use std::marker::PhantomData;

use rst_core::model::Hotkeys;
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

/// База id цифровых хоткеев групп: группе с номером `n` (1..=9) отвечает
/// id `GROUP_OPEN_HOTKEY_ID_BASE + n - 1`. Единая точка для регистрации
/// ([`group_open_combos`]) и для разбора `WM_HOTKEY` в оверлее: рассинхрон
/// двух мест означал бы «хоткей открывает не ту группу».
///
/// Начало диапазона 8 — после глобальных хоткеев (1..=4) и медиа-хоткеев
/// (5..=7) из `overlay.rs`; девятка групп занимает 8..=16, дальше —
/// свободные id для одиночных хоткеев групп (меню, удаление).
pub const GROUP_OPEN_HOTKEY_ID_BASE: i32 = 8;

/// Хоткей меню редактирования групп. Сразу за девяткой открытия (8..=16) —
/// см. [`GROUP_OPEN_HOTKEY_ID_BASE`].
pub const GROUP_MENU_HOTKEY_ID: i32 = 17;

/// Хоткей удаления открытой группы.
pub const GROUP_DELETE_HOTKEY_ID: i32 = 18;

/// Хоткей «открепить все закреплённые окна».
///
/// К группам отношения не имеет, но регистрируется тем же пакетом: пакетная
/// регистрация изолирует конфликты, а одиночная роняет весь хоткей при
/// первой же занятой комбинации.
pub const UNPIN_ALL_HOTKEY_ID: i32 = 19;

/// Все хоткеи групп одним списком: девятка открытия плюс меню и удаление.
///
/// Собирается здесь, а не в оверлее, по той же причине, что и
/// [`group_open_combos`]: разбор `WM_HOTKEY` и регистрация обязаны знать об
/// одном и том же наборе id, и второе место, где этот набор перечислен, рано
/// или поздно разойдётся с первым.
///
/// Не назначенные и не разбирающиеся комбинации пропускаются молча — как у
/// остальных хоткеев программы.
pub fn group_hotkey_combos(hotkeys: &Hotkeys) -> Vec<(i32, HotkeyCombo)> {
    let mut combos = group_open_combos(hotkeys);
    for (id, raw) in [
        (GROUP_MENU_HOTKEY_ID, hotkeys.edit_groups_menu.as_deref()),
        (GROUP_DELETE_HOTKEY_ID, hotkeys.delete_open_group.as_deref()),
        (UNPIN_ALL_HOTKEY_ID, hotkeys.unpin_all.as_deref()),
    ] {
        if let Some(combo) = raw.and_then(|s| HotkeyCombo::parse(s).ok()) {
            combos.push((id, combo));
        }
    }
    combos
}

/// Один неудавшийся хоткей из пакетной регистрации (T5): комбинация занята
/// другим приложением, регистрация этого хоткея не состоялась — остальные
/// при этом зарегистрировались как ни в чём не бывало.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyRegistrationConflict {
    /// Id, под которым хоткей собирались регистрировать — по нему
    /// вызывающий понимает, какой именно хоткей не удался.
    pub id: i32,
    /// Каноничный вид комбинации из конфига — для предупреждения
    /// пользователю (то же, что в [`Win32Error::HotkeyConflict`]).
    pub combo: String,
}

/// Пакет зарегистрированных хоткеев: живут до `Drop` набора, как отдельные
/// [`RegisteredHotkey`] — на том же потоке с циклом сообщений (тип `!Send`).
///
/// Появился для цифровых хоткеев групп (T5): регистрировать девять
/// комбинаций по одной и на каждый сбой прерывать остальные — значит
/// превратить один занятый `Ctrl+Shift+1` (а он занят во многих
/// приложениях) в недоступность всех девяти. Пакет записывает конфликт
/// и идёт дальше; запуск при этом не роняется.
#[derive(Debug)]
pub struct RegisteredHotkeySet {
    hotkeys: Vec<RegisteredHotkey>,
    conflicts: Vec<HotkeyRegistrationConflict>,
}

impl RegisteredHotkeySet {
    /// Зарегистрировать все комбинации на текущем потоке. Конфликт одного
    /// хоткея не мешает остальным: он попадает в [`Self::conflicts`],
    /// остальные регистрируются как обычно.
    ///
    /// Прочие ошибки — не конфликт (например, id вне диапазона) —
    /// возвращаются как `Err` целиком: это программистская ошибка, а не
    /// занятая пользователем комбинация, и прятать её в списке конфликтов
    /// значило бы молча потерять хоткей.
    pub fn register_all(
        combos: impl IntoIterator<Item = (i32, HotkeyCombo)>,
    ) -> Result<Self, Win32Error> {
        let mut set = Self {
            hotkeys: Vec::new(),
            conflicts: Vec::new(),
        };
        for (id, combo) in combos {
            match RegisteredHotkey::register(id, combo) {
                Ok(hotkey) => set.hotkeys.push(hotkey),
                Err(Win32Error::HotkeyConflict(combo_string)) => {
                    set.conflicts.push(HotkeyRegistrationConflict {
                        id,
                        combo: combo_string,
                    });
                }
                Err(other) => return Err(other),
            }
        }
        Ok(set)
    }

    /// Конфликты регистрации: комбинация уже занята другим приложением.
    pub fn conflicts(&self) -> &[HotkeyRegistrationConflict] {
        &self.conflicts
    }

    /// Сколько хоткеев из пакета зарегистрировалось успешно.
    pub fn registered(&self) -> usize {
        self.hotkeys.len()
    }

    /// Идентификаторы успешно зарегистрированных хоткеев.
    ///
    /// Нужны разбору `WM_HOTKEY`: у процесса столько окон оверлея, сколько
    /// мониторов, а хоткеи регистрирует ровно одно — остальные обязаны
    /// чужие сообщения игнорировать.
    pub fn registered_ids(&self) -> std::collections::HashSet<i32> {
        self.hotkeys.iter().map(|h| h.id()).collect()
    }
}

/// Хоткеи открытия групп из конфига, готовые к регистрации: комбинации,
/// назначенные группам 1..=9, с id из [`GROUP_OPEN_HOTKEY_ID_BASE`].
/// Неназначенные и не парсящиеся комбинации пропускаются — не парсящаяся
/// строка в конфиге это ошибка настройки, а не пользовательский выбор,
/// и молчаливый пропуск здесь тот же, что у `edit_mode` в координаторе.
pub fn group_open_combos(hotkeys: &Hotkeys) -> Vec<(i32, HotkeyCombo)> {
    (1..=Hotkeys::GROUP_OPEN_SLOTS)
        .filter_map(|n| {
            let combo = HotkeyCombo::parse(hotkeys.open_group(n)?).ok()?;
            Some((GROUP_OPEN_HOTKEY_ID_BASE + n as i32 - 1, combo))
        })
        .collect()
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

    #[test]
    fn parse_accepts_three_modifiers() {
        // Ctrl+Alt+Shift+G — три модификатора. Парсер заводился на двух
        // (Ctrl+Alt+S), но модификаторы независимы, а T5 добавил в конфиг
        // сразу несколько трёхмодификаторных комбинаций — разбор обязан
        // принимать их все.
        let c = HotkeyCombo::parse("Ctrl+Alt+Shift+G").expect("валидная комбинация");
        assert!(c.ctrl && c.alt && c.shift && !c.win);
        assert_eq!(c.vk, 'G' as u32);
        assert_eq!(c.display_string(), "Ctrl+Alt+Shift+G");
    }

    #[test]
    fn parse_accepts_ctrl_shift_digits_one_through_nine() {
        // Цифровые хоткеи групп: Ctrl+Shift+1..9. VK цифр — 0x30..0x39
        // (верхний ряд, раскладко-независимый), '1' = 0x31.
        for n in 1..=9 {
            let s = format!("Ctrl+Shift+{n}");
            let c = HotkeyCombo::parse(&s).expect("валидная комбинация");
            assert!(c.ctrl && c.shift && !c.alt && !c.win);
            assert_eq!(c.vk, '0' as u32 + n);
            assert_eq!(c.display_string(), s);
        }
    }

    #[test]
    fn batch_registration_isolates_conflicts_and_registers_the_rest() {
        // Один занятый хоткей не должен утянуть за собой остальные: в
        // пакете из трёх комбинаций одна конфликтует, две регистрируются.
        let occupied = HotkeyCombo::parse("Ctrl+Alt+Shift+F20").expect("валидная комбинация");
        let _hold = RegisteredHotkey::register(0x5001, occupied).expect("первичная регистрация");

        let free_a = HotkeyCombo::parse("Ctrl+Alt+Shift+F19").expect("валидная комбинация");
        let free_b = HotkeyCombo::parse("Ctrl+Alt+Shift+F18").expect("валидная комбинация");

        let set = RegisteredHotkeySet::register_all([
            (0x5002, occupied),
            (0x5003, free_a),
            (0x5004, free_b),
        ])
        .expect("пакет не падает из-за одного конфликта");

        assert_eq!(set.registered(), 2);
        assert_eq!(
            set.conflicts(),
            &[HotkeyRegistrationConflict {
                id: 0x5002,
                combo: "Ctrl+Alt+Shift+F20".to_string(),
            }]
        );

        // После Drop пакета занятые им комбинации снова доступны.
        drop(set);
        let _re_a = RegisteredHotkey::register(0x5003, free_a).expect("F19 снова свободен");
        let _re_b = RegisteredHotkey::register(0x5004, free_b).expect("F18 снова свободен");
    }

    #[test]
    fn batch_registration_with_all_conflicts_still_succeeds() {
        // Крайний случай цифровых хоткеев: Ctrl+Shift+цифра занята во
        // многих приложениях, и теоретически не зарегистрироваться могут
        // ВСЕ девять сразу. Пакет обязан пережить и это — без ошибки.
        let a = HotkeyCombo::parse("Ctrl+Alt+Shift+F17").expect("валидная комбинация");
        let b = HotkeyCombo::parse("Ctrl+Alt+Shift+F16").expect("валидная комбинация");
        let _hold_a = RegisteredHotkey::register(0x5101, a).expect("первичная регистрация");
        let _hold_b = RegisteredHotkey::register(0x5102, b).expect("первичная регистрация");

        let set = RegisteredHotkeySet::register_all([(0x5103, a), (0x5104, b)])
            .expect("все конфликты — не повод для ошибки");
        assert_eq!(set.registered(), 0);
        assert_eq!(set.conflicts().len(), 2);
    }

    #[test]
    fn batch_registration_propagates_non_conflict_errors() {
        // id вне диапазона 0x0000..=0xBFFF — программистская ошибка: она не
        // должна прятаться в списке конфликтов, пользователь тут ни при чём
        // и предупреждать его нечем.
        let combo = HotkeyCombo::parse("Ctrl+Alt+Shift+F15").expect("валидная комбинация");
        let err = RegisteredHotkeySet::register_all([(0xC000, combo)])
            .expect_err("id вне диапазона обязан вернуть ошибку");
        assert!(matches!(err, Win32Error::InvalidHotkey(_)));
    }

    #[test]
    fn group_open_combos_defaults_cover_all_nine_groups() {
        let combos = group_open_combos(&Hotkeys::default());
        assert_eq!(combos.len(), 9);
        for (i, (id, combo)) in combos.iter().enumerate() {
            assert_eq!(*id, GROUP_OPEN_HOTKEY_ID_BASE + i as i32);
            assert_eq!(combo.display_string(), format!("Ctrl+Shift+{}", i + 1));
        }
    }

    #[test]
    fn group_open_combos_skips_unassigned_and_unparseable() {
        let mut hotkeys = Hotkeys::default();
        hotkeys.open_group_by_number[2] = None; // группа 3: не назначена
        hotkeys.open_group_by_number[4] = Some("не хоткей".to_string()); // не парсится
        hotkeys.open_group_by_number[8] = None; // группа 9: не назначена

        let combos = group_open_combos(&hotkeys);
        let expect = [
            (8, "Ctrl+Shift+1"),
            (9, "Ctrl+Shift+2"),
            (11, "Ctrl+Shift+4"),
            (13, "Ctrl+Shift+6"),
            (14, "Ctrl+Shift+7"),
            (15, "Ctrl+Shift+8"),
        ];
        assert_eq!(combos.len(), expect.len());
        for ((id, combo), (exp_id, exp_combo)) in combos.iter().zip(expect) {
            assert_eq!(*id, exp_id);
            assert_eq!(combo.display_string(), exp_combo);
        }
    }

    #[test]
    fn parse_accepts_alt_shift_s_and_arbitrary_modifier_pairs() {
        // Комбинация Alt+Shift+S — живой баг репорта пользователя 2026-08-26.
        // Проверяем, что парсер HotkeyCombo корректно разбирает Alt+Shift+S,
        // Alt+Shift+F и любые другие сочетания модификаторов и букв.
        let s = HotkeyCombo::parse("Alt+Shift+S").expect("Alt+Shift+S валидна");
        assert!(s.alt && s.shift && !s.ctrl && !s.win);
        assert_eq!(s.vk, 'S' as u32);
        assert_eq!(s.display_string(), "Alt+Shift+S");

        let f = HotkeyCombo::parse("Alt+Shift+F").expect("Alt+Shift+F валидна");
        assert!(f.alt && f.shift && !f.ctrl && !f.win);
        assert_eq!(f.vk, 'F' as u32);
        assert_eq!(f.display_string(), "Alt+Shift+F");
    }
}
