//! Запасной путь для глобальных хоткеев, которые не отдала `RegisterHotKey`:
//! низкоуровневый клавиатурный хук (`WH_KEYBOARD_LL`).
//!
//! Зачем он вообще есть. `RegisterHotKey` — механизм «кто первый встал, того
//! и тапки»: комбинацию получает то приложение, которое зарегистрировало её
//! раньше, остальным система отказывает и второго шанса не даёт. У
//! пользователя `Ctrl+Alt+M` держит Discord (замер по журналу 2026-09-01:
//! `хоткей уже занят другим приложением name=MuteAll combo=Ctrl+Alt+M`), и
//! никакой перерегистрацией это не обойти — Discord не отпускает комбинацию,
//! пока работает.
//!
//! Низкоуровневый хук стоит ВЫШЕ по цепочке: система зовёт его до того, как
//! нажатие дойдёт до окон, и он может это нажатие проглотить. Поэтому
//! комбинация достаётся нам, а не тому, кто успел зарегистрировать её первым.
//!
//! Цена, из-за которой хук ставится ТОЛЬКО как запасной путь и только под
//! реально нужные комбинации: колбэк вызывается на КАЖДОЕ нажатие клавиши во
//! всей системе. Это прямо противоречит требованию «в покое программа не
//! просыпается вообще» (SPEC.md, раздел 13), поэтому пока `RegisterHotKey`
//! справляется — хука нет вовсе, а как только последняя занятая комбинация
//! освободилась, он снимается.
//!
//! Поток. Хук привязан к потоку, который его поставил: система вызывает
//! колбэк на нём же, и очередь сообщений этого потока обязана крутиться
//! (иначе Windows молча выкинет хук по таймауту `LowLevelHooksTimeout`).
//! Оба условия выполняет поток оверлей-окна, владеющего хоткеями, — весь
//! модуль работает только на нём, состояние живёт в `thread_local`.

use std::cell::RefCell;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageExtraInfo, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, PostThreadMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use crate::hotkey::HotkeyCombo;

thread_local! {
    /// Состояние хука этого потока. `thread_local`, а не глобаль: хук
    /// принадлежит потоку, и разделять его между окнами нельзя — снял бы
    /// чужой.
    static HOOK: RefCell<HookState> = const { RefCell::new(HookState::new()) };
}

struct HookState {
    hook: Option<HHOOK>,
    /// Комбинации, которые ловит хук, вместе с id — тем же, что пришёл бы в
    /// `WM_HOTKEY` от `RegisterHotKey`. Совпадение id — не совпадение, а
    /// намеренное равенство: вызывающий код разбирает оба источника одним
    /// кодом и не должен знать, откуда пришло нажатие.
    combos: Vec<(i32, HotkeyCombo)>,
}

impl HookState {
    const fn new() -> Self {
        Self {
            hook: None,
            combos: Vec::new(),
        }
    }
}

/// Задать полный набор комбинаций, которые ловит хук этого потока.
///
/// Набор ЗАМЕНЯЕТСЯ целиком, а не дополняется: он собирается заново на
/// каждую перерегистрацию хоткеев, и «добавить» означало бы копить в нём
/// комбинации, которые уже освободились. Пустой набор снимает хук.
///
/// Вызывать только с потока окна-владельца хоткеев.
pub fn set_fallback_combos(combos: Vec<(i32, HotkeyCombo)>) {
    HOOK.with(|cell| {
        let mut state = cell.borrow_mut();
        state.combos = combos;
        if state.combos.is_empty() {
            unhook(&mut state);
            return;
        }
        if state.hook.is_some() {
            return;
        }
        // SAFETY: WH_KEYBOARD_LL не требует модуля (hmod=None) и ставится на
        // текущий поток; колбэк — `extern "system"` нужной сигнатуры.
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0) } {
            Ok(hook) => {
                state.hook = Some(hook);
                tracing::info!(
                    combos = state.combos.len(),
                    "низкоуровневый клавиатурный хук поставлен — перехватываем занятые хоткеи"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "не удалось поставить клавиатурный хук — занятые хоткеи останутся недоступными");
            }
        }
    });
}

/// Снять хук этого потока и забыть набор (уничтожение окна).
pub fn clear() {
    HOOK.with(|cell| {
        let mut state = cell.borrow_mut();
        state.combos.clear();
        unhook(&mut state);
    });
}

fn unhook(state: &mut HookState) {
    if let Some(hook) = state.hook.take() {
        // SAFETY: хэндл получен из успешного SetWindowsHookExW на этом же
        // потоке и снимается ровно один раз (`take`).
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
        tracing::info!("низкоуровневый клавиатурный хук снят");
    }
}

/// Держится ли клавиша прямо сейчас (старший бит `GetAsyncKeyState`).
fn key_down(vk: u16) -> bool {
    // SAFETY: чтение состояния клавиатуры, безопасно для любого VK.
    (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16 & 0x8000) != 0
}

/// Какие модификаторы зажаты в момент нажатия.
///
/// Отдельным типом, а не четырьмя `bool` подряд в аргументах: сравнение
/// точное, а перепутанные местами `alt` и `shift` в вызове компилятор бы не
/// поймал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeldModifiers {
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
}

impl HeldModifiers {
    /// Снимок ФИЗИЧЕСКОГО состояния клавиатуры.
    fn current() -> Self {
        Self {
            ctrl: key_down(VK_CONTROL.0),
            alt: key_down(VK_MENU.0),
            shift: key_down(VK_SHIFT.0),
            win: key_down(VK_LWIN.0) || key_down(VK_RWIN.0),
        }
    }
}

/// Совпадает ли нажатие `vk` при текущих модификаторах с одной из
/// комбинаций набора; возвращает её id.
fn match_combo(combos: &[(i32, HotkeyCombo)], vk: u32) -> Option<i32> {
    match_held(combos, vk, HeldModifiers::current())
}

/// Чистое ядро сравнения: те же правила, но состояние клавиатуры приходит
/// аргументом.
///
/// Модификаторы сравниваются ТОЧНО, как это делает `RegisterHotKey`: у
/// комбинации без модификаторов ни один не должен быть зажат, иначе
/// `Ctrl+Space` срабатывал бы как голый `Space`. Без этого медиа-клавиши
/// (пробел, PgUp, PgDn) отбирали бы у системы половину сочетаний.
///
/// Разделение появилось из-за плавающего теста: он утверждал «ни один
/// модификатор не зажат», читая живую клавиатуру, и падал, стоило человеку
/// печатать в момент прогона. Состояние, которое тест не контролирует,
/// проверять нельзя — теперь оно задаётся явно.
fn match_held(combos: &[(i32, HotkeyCombo)], vk: u32, held: HeldModifiers) -> Option<i32> {
    combos
        .iter()
        .find(|(_, c)| {
            c.vk == vk
                && c.ctrl == held.ctrl
                && c.alt == held.alt
                && c.shift == held.shift
                && c.win == held.win
        })
        .map(|(id, _)| *id)
}

/// Колбэк низкоуровневого хука.
///
/// Делает РОВНО две вещи: сверяет нажатие с набором и, если совпало, кладёт
/// в очередь своего потока то же самое `WM_HOTKEY`, которое прислала бы
/// система. Никакой работы здесь быть не должно: пока колбэк не вернулся,
/// нажатие не доходит ни до одного окна системы, а превысив
/// `LowLevelHooksTimeout`, хук будет молча выкинут Windows.
///
/// Возврат `1` проглатывает нажатие. Это осознанно: комбинацию у нас отнял
/// чужой обработчик (Discord), и оставить ему ещё и это нажатие значило бы
/// выполнить два действия на одно нажатие.
unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN)
    {
        // SAFETY: при HC_ACTION система гарантирует, что lparam указывает на
        // валидную KBDLLHOOKSTRUCT, живущую на время вызова.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let hit = HOOK.with(|cell| {
            cell.try_borrow()
                .ok()
                .and_then(|state| match_combo(&state.combos, info.vkCode))
        });
        if let Some(id) = hit {
            // SAFETY: сообщение кладётся в очередь СВОЕГО потока — того, что
            // поставил хук (система зовёт колбэк именно на нём). `hwnd`
            // такого сообщения — NULL, ровно как у настоящего WM_HOTKEY от
            // хоткея потока, поэтому разбирается оно тем же кодом.
            unsafe {
                let _ = PostThreadMessageW(
                    windows::Win32::System::Threading::GetCurrentThreadId(),
                    WM_HOTKEY,
                    WPARAM(id as usize),
                    LPARAM(GetMessageExtraInfo().0),
                );
            }
            return LRESULT(1);
        }
    }
    // SAFETY: обязательная передача по цепочке; хэндл не нужен (None) —
    // система сама находит следующий хук.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32) -> HotkeyCombo {
        HotkeyCombo {
            ctrl,
            alt,
            shift,
            win,
            vk,
        }
    }

    /// Зажатые модификаторы для чистого сравнения.
    fn held(ctrl: bool, alt: bool, shift: bool, win: bool) -> HeldModifiers {
        HeldModifiers {
            ctrl,
            alt,
            shift,
            win,
        }
    }

    const NOTHING_HELD: HeldModifiers = HeldModifiers {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
    };

    #[test]
    fn empty_set_never_matches() {
        assert_eq!(match_held(&[], 'M' as u32, NOTHING_HELD), None);
    }

    #[test]
    fn a_combo_with_modifiers_needs_them_all() {
        // Точное сравнение: комбинация ловится ровно при своих модификаторах
        // и ни при каких других. Раньше проверялась только отрицательная
        // половина — положительную было не на чем показать, потому что
        // состояние читалось с живой клавиатуры.
        let set = [(3, combo(true, true, false, false, 'M' as u32))];
        assert_eq!(
            match_held(&set, 'M' as u32, held(true, true, false, false)),
            Some(3)
        );
        assert_eq!(match_held(&set, 'M' as u32, NOTHING_HELD), None);
        // Лишний зажатый модификатор — уже другая комбинация.
        assert_eq!(
            match_held(&set, 'M' as u32, held(true, true, true, false)),
            None
        );
        // Не хватает одного — тоже мимо.
        assert_eq!(
            match_held(&set, 'M' as u32, held(true, false, false, false)),
            None
        );
    }

    #[test]
    fn a_bare_key_matches_only_with_no_modifier_held() {
        // Обратная сторона того же правила: голая клавиша срабатывает именно
        // потому, что ни один модификатор не зажат — иначе `Ctrl+PgUp`
        // сработал бы как голый `PgUp`.
        let set = [(5, combo(false, false, false, false, 0x21))]; // VK_PRIOR
        assert_eq!(match_held(&set, 0x21, NOTHING_HELD), Some(5));
        assert_eq!(
            match_held(&set, 0x21, held(true, false, false, false)),
            None
        );
        assert_eq!(
            match_held(&set, 0x21, held(false, false, false, true)),
            None
        );
        // Чужая клавиша тем же набором не ловится.
        assert_eq!(match_held(&set, 0x22, NOTHING_HELD), None);
    }

    #[test]
    fn setting_an_empty_set_is_a_no_op_and_leaves_no_hook() {
        // Ключевое свойство: пока перехватывать нечего, хука в системе нет
        // вовсе (SPEC.md раздел 13 — в покое не просыпаемся).
        set_fallback_combos(Vec::new());
        HOOK.with(|cell| assert!(cell.borrow().hook.is_none()));
    }

    #[test]
    fn a_non_empty_set_installs_the_hook_and_clear_removes_it() {
        set_fallback_combos(vec![(3, combo(true, true, false, false, 'M' as u32))]);
        HOOK.with(|cell| {
            let state = cell.borrow();
            assert_eq!(state.combos.len(), 1);
            assert!(state.hook.is_some(), "хук должен быть поставлен");
        });
        clear();
        HOOK.with(|cell| {
            let state = cell.borrow();
            assert!(state.combos.is_empty());
            assert!(state.hook.is_none(), "хук должен быть снят");
        });
    }
}
