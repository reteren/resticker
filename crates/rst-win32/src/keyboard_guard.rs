//! Клавиатурный страж тайлинга: `WH_KEYBOARD_LL` (M9, docs/TILING_DESIGN.md §T3).
//!
//! Зачем не `RegisterHotKey`. Тайлингу нужно 30–60 биндов, среди них стрелки
//! и одиночные клавиши в модальных режимах. `RegisterHotKey` не умеет ни
//! того, ни другого: комбинации с Win-стрелками система забирает себе, а
//! голую клавишу он не регистрирует вовсе (см. отказ в
//! `hotkey.rs::to_win32` и разбор в docs/research/tiling/R4_KEYBINDS.md §1).
//!
//! ## Бюджет колбэка — 1 мс, и это измерено на машине пользователя
//!
//! `HKCU\Control Panel\Desktop\LowLevelHooksTimeout` = 1 мс (window_pin.rs:863).
//! Превышение = Windows МОЛЧА снимает хук: ни ошибки, ни уведомления,
//! клавиатура просто перестаёт слушаться тайлинга до перезапуска. Отсюда
//! правила, которым подчинён [`hook_proc`]:
//!
//! * никаких синхронных вызовов в чужие процессы (именно на этом строился
//!   мышиный страж с его отдельным потоком-пробником);
//! * никаких аллокаций;
//! * блокировка только `try_read` — таблицу в этот момент может переписывать
//!   координатор, и ждать нельзя. Не получилось прочитать — пропускаем
//!   нажатие дальше (**fail-open**, тот же принцип, что у interact-lock:
//!   сомнение трактуется в пользу приложения, а не в пользу нашего замка);
//! * состояние модификаторов считается нами самими, а не спрашивается у
//!   системы на каждом нажатии.
//!
//! ## Почему `thread_local` для отправителя
//!
//! Колбэк низкоуровневого хука вызывается на ТОМ ЖЕ потоке, который его
//! поставил (поэтому потоку и нужен цикл сообщений). Значит отправитель
//! канала может жить в `thread_local` этого потока: ни мьютекса, ни
//! атомиков, ни вопроса о `Sync` для `mpsc::Sender`.
//!
//! ## Чего этот хук не поймает
//!
//! Ctrl+Alt+Del, экран блокировки и окна процессов с более высоким уровнем
//! целостности (UIPI) — тот же класс ограничений, что и у мышиного стража
//! (window_pin.rs:855). Это не чинится из пользовательского процесса.

use std::cell::{Cell, RefCell};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS,
    KEYBDINPUT, KEYEVENTF_KEYUP, LASTINPUTINFO, SendInput, VIRTUAL_KEY, VK_LCONTROL, VK_LMENU,
    VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS,
    LLKHF_INJECTED, MSG, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

use crate::error::Win32Error;

/// Комбинация клавиш в платформенно-нейтральном виде.
///
/// Свой тип, а не `rst_core::tiling::binds::KeyChord`: Win32-слой не должен
/// возвращать наверх типы предметной области тайлинга — иначе завтра он
/// будет знать и про воркспейсы. Координатор переводит одно в другое двумя
/// строками.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Chord {
    /// Виртуальный код клавиши Windows.
    pub vk: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

/// Модификатор, чьё отпускание координатор просит ему сообщать.
///
/// Нужен ровно одному сценарию — своему переключателю окон: он живёт, пока
/// зажат Alt, и закрывается на его отпускании. Обычные бинды срабатывают на
/// НАЖАТИИ, и без этого события «отпустил Alt — переключился» выразить
/// нечем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchedModifier {
    Ctrl,
    Alt,
    Shift,
    Win,
}

impl WatchedModifier {
    /// Биты левой и правой клавиш этого модификатора.
    fn bits(self) -> u8 {
        match self {
            Self::Ctrl => M_LCTRL | M_RCTRL,
            Self::Alt => M_LALT | M_RALT,
            Self::Shift => M_LSHIFT | M_RSHIFT,
            Self::Win => M_LWIN | M_RWIN,
        }
    }
}

/// Что пришло от клавиатурного стража.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    /// Сработала комбинация из набора перехвата.
    Chord(Chord),
    /// Отпущен модификатор, за которым просили следить, — причём ПОЛНОСТЬЮ:
    /// если зажаты оба Alt и отпустили один, события не будет.
    ModifierUp(WatchedModifier),
}

/// Клавиша, которой в раскладке нет, — ею «гасится» меню Пуск.
///
/// Проглотив Win+что-то, мы не даём комбинации дойти до приложения, но сам
/// Win остаётся нажатым, и его отпускание система прочитает как «открыть
/// меню Пуск». Общепринятое лечение — послать между ними нажатие клавиши,
/// которая ничего не значит: тогда Win перестаёт быть «одиноким».
/// `VK_F24` (0x87): клавиша, которой нет ни на одной физической клавиатуре и
/// которую не занимает ни одна раскладка. `0xE8` из OEM-диапазона был бы
/// хуже - его производители клавиатур используют под свои клавиши
/// (замечание ревью, находка 8).
const MASK_VK: u16 = 0x87;

/// Сообщение потоку-помпе: переустановить хук (просьба сторожа).
const WM_APP_REINSTALL: u32 = WM_APP + 1;

/// Когда колбэк последний раз отработал (мс `GetTickCount`); 0 — ни разу.
static LAST_CALLBACK_MS: AtomicU64 = AtomicU64::new(0);

/// Идентификатор потока-помпы: сторожу некуда слать просьбу без него.
static PUMP_TID: AtomicU64 = AtomicU64::new(0);

/// Сторож просят остановиться (страж уходит).
static WATCHDOG_STOP: AtomicBool = AtomicBool::new(false);

/// Столько системного ввода без единого нашего колбэка считаем «хук сняли».
///
/// Ввод бывает и мышиный — тогда молчание клавиатурного хука законно, и
/// сторож сработает вхолостую. Это осознанный размен: ложная переустановка
/// стоит двух системных вызовов, а пропущенная смерть хука стоит
/// пользователю всей тайлинговой клавиатуры до перезапуска программы.
const SILENCE_MS: u64 = 3_000;

/// Не переустанавливать чаще этого интервала.
const REINSTALL_COOLDOWN_MS: u64 = 5_000;

/// Как часто сторож просыпается.
const WATCHDOG_PERIOD_MS: u64 = 1_000;

/// За каким модификатором следим: биты из [`WatchedModifier::bits`];
/// 0 — ни за каким.
static WATCHED_MODIFIER: AtomicU64 = AtomicU64::new(0);

/// Таблица комбинаций, которые надо глотать.
///
/// Плоский список, а не правила: колбэк обязан решать за микросекунды, и
/// разбирать в нём режимы и приоритеты нельзя. Кто именно сработал —
/// разбирается уже в координаторе (`rst_core::tiling::binds`).
static SWALLOW: RwLock<Vec<Chord>> = RwLock::new(Vec::new());

thread_local! {
    /// Отправитель нажатий в координатор. Живёт только на потоке-помпе.
    static TX: RefCell<Option<Sender<KeyEvent>>> = const { RefCell::new(None) };
    /// Состояние модификаторов, посчитанное нами по проходящим событиям.
    static MODS: Cell<u8> = const { Cell::new(0) };
    /// Коды клавиш, чьё НАЖАТИЕ мы проглотили.
    ///
    /// Отпускание такой клавиши надо проглотить тоже, иначе приложение
    /// получит «отпустили то, что не нажимали». Сверять отпускание с
    /// таблицей заново нельзя: модификаторы к этому моменту могли уже
    /// отпустить, и комбинация не совпадёт.
    static SWALLOWED_DOWN: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

// Биты в [`MODS`]. Левый и правый модификаторы считаются раздельно: иначе
// отпускание правого Shift погасило бы всё ещё зажатый левый.
const M_LCTRL: u8 = 1 << 0;
const M_RCTRL: u8 = 1 << 1;
const M_LALT: u8 = 1 << 2;
const M_RALT: u8 = 1 << 3;
const M_LSHIFT: u8 = 1 << 4;
const M_RSHIFT: u8 = 1 << 5;
const M_LWIN: u8 = 1 << 6;
const M_RWIN: u8 = 1 << 7;

/// Живой хук. `Drop` снимает его и останавливает поток-помпу.
pub struct KeyboardGuard {
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Сторож переустановки хука (см. [`watchdog_tick`]).
    watchdog: Option<std::thread::JoinHandle<()>>,
}

impl KeyboardGuard {
    /// Поставить хук и начать слушать клавиатуру.
    ///
    /// `swallow` — комбинации, которые надо перехватывать. Список можно
    /// менять на ходу через [`set_swallow_set`]; хук при этом не
    /// переставляется (переустановка на каждый вход в модальный режим была
    /// бы и медленной, и гоночной).
    pub fn start(swallow: Vec<Chord>) -> Result<(Self, Receiver<KeyEvent>), Win32Error> {
        set_swallow_set(swallow);
        let (tx, rx) = mpsc::channel::<KeyEvent>();
        let (ready_tx, ready_rx) = mpsc::channel::<Option<u32>>();

        let thread = std::thread::Builder::new()
            .name("resticker-keyboard-guard".into())
            .spawn(move || {
                TX.with(|slot| *slot.borrow_mut() = Some(tx));
                MODS.with(|m| m.set(initial_mods()));

                // SAFETY: WH_KEYBOARD_LL — глобальный хук без DLL: hmod = None,
                // thread id = 0. Колбэк будет вызываться на ЭТОМ потоке, у
                // которого ниже есть цикл сообщений (требование хука).
                let hook =
                    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!(error = %e, "клавиатурный хук не поставлен");
                            let _ = ready_tx.send(None);
                            return;
                        }
                    };
                // SAFETY: GetCurrentThreadId не принимает аргументов.
                let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
                PUMP_TID.store(tid as u64, Ordering::Release);
                LAST_CALLBACK_MS.store(tick_ms(), Ordering::Relaxed);
                let _ = ready_tx.send(Some(tid));

                let hook = pump(hook);

                // SAFETY: hook — последний установленный, ещё не снимался.
                let _ = unsafe { UnhookWindowsHookEx(hook) };
                PUMP_TID.store(0, Ordering::Release);
                TX.with(|slot| *slot.borrow_mut() = None);
            })
            .map_err(|e| {
                tracing::warn!(error = %e, "поток клавиатурного стража не создан");
                Win32Error::KeyboardGuardThreadCrashed
            })?;

        let watchdog = {
            WATCHDOG_STOP.store(false, Ordering::Release);
            std::thread::Builder::new()
                .name("resticker-keyboard-watchdog".into())
                .spawn(|| {
                    let mut last_reinstall = 0u64;
                    while !WATCHDOG_STOP.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_PERIOD_MS));
                        if WATCHDOG_STOP.load(Ordering::Acquire) {
                            break;
                        }
                        watchdog_tick(&mut last_reinstall);
                    }
                })
                .ok()
        };

        match ready_rx.recv() {
            Ok(Some(thread_id)) => Ok((
                Self {
                    thread_id,
                    thread: Some(thread),
                    watchdog,
                },
                rx,
            )),
            _ => {
                WATCHDOG_STOP.store(true, Ordering::Release);
                Err(Win32Error::KeyboardHookFailed)
            }
        }
    }
}

impl Drop for KeyboardGuard {
    fn drop(&mut self) {
        // SAFETY: WM_QUIT потоку-помпе — штатный способ завершить его цикл;
        // мёртвый поток просто вернёт ошибку.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        WATCHDOG_STOP.store(true, Ordering::Release);
        if let Some(t) = self.watchdog.take() {
            let _ = t.join();
        }
        set_swallow_set(Vec::new());
        watch_modifier_release(None);
    }
}

/// Просить страж сообщать об отпускании модификатора.
///
/// `None` — перестать следить. Сам модификатор при этом НЕ глотается: Alt
/// нужен приложениям, и отбирать его ради переключателя нельзя.
pub fn watch_modifier_release(watched: Option<WatchedModifier>) {
    let bits = watched.map_or(0, |m| m.bits() as u64);
    WATCHED_MODIFIER.store(bits, Ordering::Relaxed);
}

/// Заменить набор перехватываемых комбинаций.
///
/// Отравленный замок (паника внутри чтения таблицы) не должен обезоруживать
/// тайлинг навсегда — забираем содержимое и продолжаем, как это уже принято
/// в проекте для кэша иконок (window_enum.rs).
pub fn set_swallow_set(chords: Vec<Chord>) {
    let mut guard = SWALLOW.write().unwrap_or_else(|e| e.into_inner());
    *guard = chords;
}

/// Цикл сообщений потока-помпы. Возвращает актуальный хук: сторож мог
/// попросить переустановить его, и снимать в конце надо именно новый.
fn pump(initial: HHOOK) -> HHOOK {
    let mut hook = initial;
    let mut msg = MSG::default();
    // SAFETY: GetMessageW с None-окном читает очередь ЭТОГО потока; выход по
    // WM_QUIT (возврат 0 или -1).
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == WM_APP_REINSTALL {
            hook = reinstall(hook);
            continue;
        }
        // SAFETY: msg заполнена GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    hook
}

/// Снять и поставить хук заново.
///
/// Именно в таком порядке, а не «поставить новый, потом снять старый»: два
/// одновременно живых хука обработали бы одно нажатие ДВАЖДЫ, то есть
/// действие тайлинга выполнилось бы дважды. Цена выбранного порядка —
/// микроскопическое окно, в котором нажатие пройдёт непроглоченным; это
/// заметно меньшее зло.
fn reinstall(old: HHOOK) -> HHOOK {
    // SAFETY: old получен от SetWindowsHookExW и ещё не снимался.
    let _ = unsafe { UnhookWindowsHookEx(old) };
    // SAFETY: те же аргументы, что и при первой установке; вызывается с
    // потока-помпы, у которого есть цикл сообщений.
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
        Ok(new) => {
            LAST_CALLBACK_MS.store(tick_ms(), Ordering::Relaxed);
            tracing::info!("клавиатурный хук был снят системой — переустановлен");
            new
        }
        Err(e) => {
            // Помпу не убиваем: сторож попробует снова по следующему циклу.
            tracing::warn!(error = %e, "переустановить клавиатурный хук не удалось");
            old
        }
    }
}

/// Системное время в миллисекундах.
fn tick_ms() -> u64 {
    // SAFETY: GetTickCount не принимает аргументов и всегда безопасен.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() as u64 }
}

/// Момент последнего ввода в системе (любого — мыши или клавиатуры).
fn last_system_input_ms() -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: структура заполнена, cbSize выставлен — документированный
    // контракт GetLastInputInfo.
    if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        info.dwTime as u64
    } else {
        0
    }
}

/// Сторож: ввод в системе идёт, а наш колбэк молчит — значит хук сняли.
///
/// Windows снимает низкоуровневый хук за превышение `LowLevelHooksTimeout`
/// МОЛЧА: ни ошибки, ни уведомления. Без сторожа тайлинговая клавиатура
/// умирала бы до перезапуска программы (находка ревью 6). Тот же приём, что
/// у мышиного стража (window_pin.rs:1796), с поправкой на то, что «движется
/// ли мышь» для клавиатуры не спросишь — вместо этого берём системное время
/// последнего ввода.
fn watchdog_tick(last_reinstall: &mut u64) {
    let now = tick_ms();
    let input = last_system_input_ms();
    // Ввода не было вовсе — молчание хука законно.
    if input == 0 || now.saturating_sub(input) > SILENCE_MS {
        return;
    }
    let seen = LAST_CALLBACK_MS.load(Ordering::Relaxed);
    if now.saturating_sub(seen) < SILENCE_MS {
        return;
    }
    if *last_reinstall != 0 && now.saturating_sub(*last_reinstall) < REINSTALL_COOLDOWN_MS {
        return;
    }
    let tid = PUMP_TID.load(Ordering::Acquire);
    if tid == 0 {
        return;
    }
    *last_reinstall = now;
    // SAFETY: PostThreadMessageW безопасен; мёртвый tid даёт ошибку, которую
    // игнорируем — помпа как раз завершается.
    unsafe {
        let _ = PostThreadMessageW(tid as u32, WM_APP_REINSTALL, WPARAM(0), LPARAM(0));
    }
}

/// Начальное состояние модификаторов на момент установки хука.
///
/// Без него зажатый в этот момент Alt был бы нам не виден до его отпускания.
fn initial_mods() -> u8 {
    let mut mods = 0u8;
    for (vk, bit) in [
        (VK_LCONTROL, M_LCTRL),
        (VK_RCONTROL, M_RCTRL),
        (VK_LMENU, M_LALT),
        (VK_RMENU, M_RALT),
        (VK_LSHIFT, M_LSHIFT),
        (VK_RSHIFT, M_RSHIFT),
        (VK_LWIN, M_LWIN),
        (VK_RWIN, M_RWIN),
    ] {
        // SAFETY: GetAsyncKeyState безопасен для любого кода клавиши.
        if unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0 {
            mods |= bit;
        }
    }
    mods
}

/// Бит модификатора для кода клавиши; `None` — клавиша не модификатор.
fn modifier_bit(vk: u32) -> Option<u8> {
    match VIRTUAL_KEY(vk as u16) {
        VK_LCONTROL => Some(M_LCTRL),
        VK_RCONTROL => Some(M_RCTRL),
        VK_LMENU => Some(M_LALT),
        VK_RMENU => Some(M_RALT),
        VK_LSHIFT => Some(M_LSHIFT),
        VK_RSHIFT => Some(M_RSHIFT),
        VK_LWIN => Some(M_LWIN),
        VK_RWIN => Some(M_RWIN),
        _ => None,
    }
}

/// Обратное к [`WatchedModifier::bits`].
fn watched_kind(bits: u8) -> Option<WatchedModifier> {
    [
        WatchedModifier::Ctrl,
        WatchedModifier::Alt,
        WatchedModifier::Shift,
        WatchedModifier::Win,
    ]
    .into_iter()
    .find(|kind| kind.bits() == bits)
}

/// Собрать комбинацию из кода клавиши и битов модификаторов.
fn chord_from(vk: u32, mods: u8) -> Chord {
    Chord {
        vk,
        ctrl: mods & (M_LCTRL | M_RCTRL) != 0,
        alt: mods & (M_LALT | M_RALT) != 0,
        shift: mods & (M_LSHIFT | M_RSHIFT) != 0,
        win: mods & (M_LWIN | M_RWIN) != 0,
    }
}

/// Комбинация есть в таблице перехвата?
///
/// `try_read`, а не `read`: координатор мог в этот момент начать запись, и
/// ждать нельзя (бюджет 1 мс). Не прочитали — считаем, что не наша.
fn should_swallow(chord: Chord) -> bool {
    match SWALLOW.try_read() {
        Ok(table) => table.contains(&chord),
        Err(_) => false,
    }
}

/// Погасить «одинокий» Win, чтобы не открылось меню Пуск.
fn mask_windows_key() {
    let mut inputs = [INPUT::default(); 2];
    for (i, flags) in [KEYBD_EVENT_FLAGS(0), KEYEVENTF_KEYUP]
        .into_iter()
        .enumerate()
    {
        inputs[i] = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(MASK_VK),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
    }
    // SAFETY: массив живёт до конца вызова, размер элемента передан верно.
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Колбэк хука. Всё, что здесь есть, должно укладываться в микросекунды.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        // SAFETY: документированный контракт хука для code < 0.
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // Отметка для сторожа: хук жив. Один атомарный store — в бюджет
    // колбэка укладывается с огромным запасом.
    LAST_CALLBACK_MS.store(tick_ms(), Ordering::Relaxed);
    // SAFETY: при code >= 0 lparam — указатель на KBDLLHOOKSTRUCT, живой на
    // время вызова колбэка.
    let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };

    // Своя же маскирующая клавиша и вообще любой синтетический ввод нас не
    // касается: иначе mask_windows_key кормила бы хук собственным событием.
    if kb.flags & LLKHF_INJECTED != KBDLLHOOKSTRUCT_FLAGS(0) {
        // SAFETY: см. выше.
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    let vk = kb.vkCode;
    let msg = wparam.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

    // Модификатор сам по себе никогда не глотается: без него у пользователя
    // отвалятся Alt+Tab, Shift-выделение и всё остальное.
    if let Some(bit) = modifier_bit(vk) {
        let after = MODS.with(|m| {
            let mut mods = m.get();
            if is_down {
                mods |= bit;
            } else if is_up {
                mods &= !bit;
            }
            m.set(mods);
            mods
        });
        // Отпустили тот модификатор, за которым просили следить, и он
        // отпущен ПОЛНОСТЬЮ (вторая клавиша той же пары не зажата).
        let watched = WATCHED_MODIFIER.load(Ordering::Relaxed) as u8;
        if is_up && watched != 0 && bit & watched != 0 && after & watched == 0 {
            if let Some(kind) = watched_kind(watched) {
                TX.with(|slot| {
                    if let Some(tx) = slot.borrow().as_ref() {
                        let _ = tx.send(KeyEvent::ModifierUp(kind));
                    }
                });
            }
        }
        // SAFETY: см. выше.
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    if is_up {
        let was_swallowed = SWALLOWED_DOWN.with(|set| {
            let mut set = set.borrow_mut();
            match set.iter().position(|k| *k == vk) {
                Some(i) => {
                    set.swap_remove(i);
                    true
                }
                None => false,
            }
        });
        if was_swallowed {
            return LRESULT(1);
        }
    } else if is_down {
        let mods = MODS.with(|m| m.get());
        let chord = chord_from(vk, mods);
        if should_swallow(chord) {
            let delivered = TX.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .map(|tx| tx.send(KeyEvent::Chord(chord)).is_ok())
                    .unwrap_or(false)
            });
            // Канал оборвался (координатор ушёл) — глотать нажатие больше
            // некому и незачем: отдаём его приложению, иначе клавиша
            // молча пропала бы для пользователя.
            if delivered {
                SWALLOWED_DOWN.with(|set| {
                    let mut set = set.borrow_mut();
                    // Автоповтор шлёт WM_KEYDOWN без промежуточного KEYUP:
                    // без проверки дубликата список рос бы на каждое
                    // повторение, а лишние записи потом глотали бы чужие
                    // отпускания той же клавиши (находка ревью 4).
                    if !set.contains(&vk) {
                        set.push(vk);
                    }
                });
                if chord.win {
                    mask_windows_key();
                }
                return LRESULT(1);
            }
        }
    }

    // SAFETY: см. выше.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тесты таблицы перехвата делят один процесс-wide [`SWALLOW`], а
    /// `cargo test` гоняет их параллельно — без этого замка они изредка
    /// читали бы таблицу, которую в этот момент переписал сосед. Такое
    /// падение выглядит как «тест иногда падает» и стоит дороже, чем
    /// строчка блокировки.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_table() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn modifier_bits_cover_both_sides() {
        assert_eq!(modifier_bit(VK_LCONTROL.0 as u32), Some(M_LCTRL));
        assert_eq!(modifier_bit(VK_RCONTROL.0 as u32), Some(M_RCTRL));
        assert_eq!(modifier_bit(VK_LWIN.0 as u32), Some(M_LWIN));
        assert!(
            modifier_bit(b'A' as u32).is_none(),
            "буква — не модификатор"
        );
    }

    #[test]
    fn a_chord_reads_left_and_right_modifiers_the_same() {
        // Пользователю всё равно, каким Shift он нажал.
        let left = chord_from(b'H' as u32, M_LSHIFT | M_LALT);
        let right = chord_from(b'H' as u32, M_RSHIFT | M_RALT);
        assert_eq!(left, right);
        assert!(left.shift && left.alt);
        assert!(!left.ctrl && !left.win);
    }

    #[test]
    fn releasing_one_side_keeps_the_other_held() {
        // Оба Shift зажаты, отпустили правый — модификатор всё ещё активен.
        let mods = (M_LSHIFT | M_RSHIFT) & !M_RSHIFT;
        assert!(chord_from(b'A' as u32, mods).shift);
    }

    #[test]
    fn each_modifier_maps_to_both_of_its_keys() {
        assert_eq!(WatchedModifier::Alt.bits(), M_LALT | M_RALT);
        assert_eq!(WatchedModifier::Win.bits(), M_LWIN | M_RWIN);
    }

    #[test]
    fn a_bit_pair_maps_back_to_its_modifier() {
        for kind in [
            WatchedModifier::Ctrl,
            WatchedModifier::Alt,
            WatchedModifier::Shift,
            WatchedModifier::Win,
        ] {
            assert_eq!(watched_kind(kind.bits()), Some(kind));
        }
        assert!(
            watched_kind(0).is_none(),
            "ноль — это «ни за кем не следим»"
        );
        assert!(
            watched_kind(M_LALT).is_none(),
            "половина пары — не модификатор целиком"
        );
    }

    #[test]
    fn releasing_one_of_two_alt_keys_does_not_count_as_released() {
        // Пользователь держит оба Alt и отпускает правый: переключатель
        // закрываться не должен, Alt всё ещё зажат.
        let after = (M_LALT | M_RALT) & !M_RALT;
        assert_ne!(after & WatchedModifier::Alt.bits(), 0);
    }

    #[test]
    fn an_empty_table_swallows_nothing() {
        let _guard = lock_table();
        set_swallow_set(Vec::new());
        assert!(!should_swallow(Chord {
            vk: b'H' as u32,
            alt: true,
            ..Default::default()
        }));
    }

    #[test]
    fn a_registered_chord_is_swallowed_and_a_similar_one_is_not() {
        let _guard = lock_table();
        let alt_h = Chord {
            vk: b'H' as u32,
            alt: true,
            ..Default::default()
        };
        set_swallow_set(vec![alt_h]);
        assert!(should_swallow(alt_h));
        // Тот же код клавиши, но с лишним Shift — другая комбинация.
        assert!(!should_swallow(Chord {
            shift: true,
            ..alt_h
        }));
        // И та же комбинация на другой клавише — тоже другая.
        assert!(!should_swallow(Chord {
            vk: b'J' as u32,
            ..alt_h
        }));
        set_swallow_set(Vec::new());
    }

    #[test]
    fn the_table_can_be_replaced_while_it_is_in_use() {
        let _guard = lock_table();
        let a = Chord {
            vk: b'A' as u32,
            ctrl: true,
            ..Default::default()
        };
        let b = Chord {
            vk: b'B' as u32,
            ctrl: true,
            ..Default::default()
        };
        set_swallow_set(vec![a]);
        assert!(should_swallow(a));
        set_swallow_set(vec![b]);
        assert!(
            !should_swallow(a),
            "старая таблица не должна пережить замену"
        );
        assert!(should_swallow(b));
        set_swallow_set(Vec::new());
    }
}
