//! Инкрементальный кэш «реальных» окон на WinEvent-хуках (M4_PREP_NOTES.md
//! §3; docs/M4_WINDOW_TRACKER_DESIGN.md — полное проектирование этого среза).
//! Живёт на собственном потоке со своим циклом сообщений (тот же паттерн,
//! что у [`crate::overlay::OverlayWindow`]/[`crate::tray::TrayIcon`]):
//! `start()` возвращает `(Self, Receiver<WindowEvent>)`, `Drop` останавливает
//! поток. Разовое перечисление — [`crate::window_enum::enumerate`]; здесь —
//! только дифф на WinEvent-хуках между полными перечислениями.
//!
//! Fast path (ADR-005): пока [`WindowTracker::set_mask_needed`] не вызван с
//! `true`, хуки не ставятся вовсе, поток крутит пустой pump практически без
//! накладных расходов.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HWND_MESSAGE, IsWindow, KillTimer, MSG, OBJID_WINDOW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetTimer, SetWindowLongPtrW, TranslateMessage,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_CLOSE, WM_DESTROY, WM_TIMER, WM_WTSSESSION_CHANGE,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_POPUP, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use crate::error::Win32Error;
use crate::window_enum::{self, WindowInfo, WindowRect};

const CLASS_NAME: PCWSTR = w!("resticker_window_tracker");
const WINDOW_TITLE: PCWSTR = w!("resticker_window_tracker_wnd");

/// Дебаунс `EVENT_OBJECT_LOCATIONCHANGE` (M4_PREP_NOTES §3.3): не чаще
/// одного пересчёта за кадр @60 Гц.
const DEBOUNCE_MS: u32 = 16;
const DEBOUNCE_TIMER_ID: usize = 1;

/// Координатор → трекер: включить/выключить хуки (fast path, ADR-005).
/// `bool` в `wParam`.
const WM_APP_SET_MASK_NEEDED: u32 = WM_APP + 1;

/// Свежий снимок кэша окон. Полный список, а не дифф: 25-30 окон, копия
/// дёшева, и координатору не нужна дифф-логика — сравнение «что изменилось»
/// не требуется, читается только текущее состояние (M4_WINDOW_TRACKER_DESIGN.md
/// §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowEvent {
    Changed(Vec<WindowInfo>),
}

/// Поток трекера и его message-only окно. `Drop` шлёт `WM_CLOSE` и `join`'ит
/// поток (тот же паттерн, что `OverlayWindow`/`TrayIcon`).
pub struct WindowTracker {
    hwnd: HWND,
    thread: Option<JoinHandle<()>>,
}

// HWND — просто числовой хэндл; уничтожение окна происходит только в Drop
// (с владением WindowTracker), поэтому Sync безопасен (см. тот же
// комментарий у OverlayWindow/TrayIcon).
unsafe impl Send for WindowTracker {}
unsafe impl Sync for WindowTracker {}

/// `HWND` не `Send` по умолчанию — обёртка только для пересылки готового
/// хэндла через канал готовности.
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

type ReadyResult = Result<SendHwnd, Win32Error>;

impl WindowTracker {
    /// Запустить поток трекера. Хуки на этом этапе НЕ ставятся — трекер
    /// «спит» до первого `set_mask_needed(true)` (fast path, ADR-005).
    pub fn start() -> Result<(Self, Receiver<WindowEvent>), Win32Error> {
        let (event_tx, event_rx) = mpsc::channel::<WindowEvent>();
        let (ready_tx, ready_rx) = mpsc::channel::<ReadyResult>();

        let thread = thread::spawn(move || run_message_loop(event_tx, ready_tx));

        let hwnd = ready_rx
            .recv()
            .map_err(|_| Win32Error::WindowTrackerThreadCrashed)??
            .0;

        Ok((
            Self {
                hwnd,
                thread: Some(thread),
            },
            event_rx,
        ))
    }

    /// Гейт хуков (ADR-005, M4_PREP_NOTES §3.4/§6.4): `true` — поставить
    /// WinEvent-хуки и сразу отдать полное перечисление (координатор не
    /// ждёт первого системного события); `false` — снять хуки, забыть кэш,
    /// эмиссию прекратить. Идемпотентно — повторный вызов с тем же
    /// значением не дублирует хуки/не эмитит лишний раз.
    pub fn set_mask_needed(&self, needed: bool) {
        // SAFETY: self.hwnd — наше живое окно; PostMessage безопасен с
        // любого потока (в т.ч. если окно уже уничтожается — просто вернёт
        // ошибку, которую игнорируем).
        unsafe {
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_APP_SET_MASK_NEEDED,
                WPARAM(needed as usize),
                LPARAM(0),
            );
        }
    }
}

impl Drop for WindowTracker {
    fn drop(&mut self) {
        // Окно и хуки принадлежат потоку трекера — уничтожает свои
        // Win32-ресурсы он сам (тот же паттерн, что OverlayWindow/TrayIcon:
        // WM_CLOSE → DefWindowProc → DestroyWindow → WM_DESTROY на СВОЁМ
        // потоке → снятие хуков/таймера/WTS → PostQuitMessage).
        // SAFETY: self.hwnd — наше окно; PostMessage безопасен и для уже
        // уничтоженного окна.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Отложенные изменения кэша, накопленные между дебаунс-циклами
/// (M4_WINDOW_TRACKER_DESIGN.md §2, §4). `needs_full` перекрывает всё
/// остальное: полное перечисление даёт свежий z-order и не нуждается в
/// применении инкрементальных дельт поверх него.
#[derive(Default)]
struct Pending {
    needs_full: bool,
    destroyed: HashSet<usize>,
    /// `hwnd → iconic`; последняя запись в пределах одного окна дебаунса
    /// побеждает (быстрый minimize+restore схлопывается в no-op).
    minimized: HashMap<usize, bool>,
    location_changed: HashSet<usize>,
}

/// Состояние, живущее на потоке трекера между сообщениями (GWLP_USERDATA —
/// тот же паттерн, что `WndState` у оверлея/трея). Кэш и `pending` читает и
/// пишет только этот поток (сам pump и WinEvent-колбэк, вызываемый на нём
/// же), поэтому мьютексов не требуется.
struct WndState {
    tx: Sender<WindowEvent>,
    cache: Vec<WindowInfo>,
    mask_needed: bool,
    hooks: Vec<HWINEVENTHOOK>,
    timer_running: bool,
    /// Сессия заблокирована (`WTS_SESSION_LOCK`): не копить/не эмитить —
    /// окна за экраном блокировки не валидные оклюдеры, а перечислять их в
    /// этот момент дорого и бессмысленно (M4_PREP_NOTES §9). Разморозка —
    /// полный ребилд на `WTS_SESSION_UNLOCK`.
    frozen: bool,
    pending: Pending,
}

thread_local! {
    /// HWND message-only окна ТЕКУЩЕГО потока — WinEvent-колбэк не получает
    /// его параметром (только hwnd окна-источника события), поэтому не может
    /// достать `GWLP_USERDATA` иначе как зная свой собственный hwnd. Пишется
    /// один раз в начале `run_message_loop`, читается только колбэком на том
    /// же потоке — `Cell` без дополнительной синхронизации достаточно.
    static TRACKER_HWND: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

fn hwnd_from_usize(hwnd: usize) -> HWND {
    HWND(hwnd as *mut core::ffi::c_void)
}

fn run_message_loop(tx: Sender<WindowEvent>, ready_tx: Sender<ReadyResult>) {
    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    TRACKER_HWND.with(|c| c.set(hwnd));

    let state = Box::new(WndState {
        tx,
        cache: Vec::new(),
        mask_needed: false,
        hooks: Vec::new(),
        timer_running: false,
        frozen: false,
        pending: Pending::default(),
    });
    // SAFETY: hwnd только что создано этим потоком; GWLP_USERDATA хранит
    // единственный владеющий указатель, освобождаемый после выхода из pump.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    }

    if ready_tx.send(Ok(SendHwnd(hwnd))).is_err() {
        // Получатель уже отброшен — свернуться, не оставляя окно/хуки.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        reclaim_state(hwnd);
        return;
    }

    let mut msg = MSG::default();
    // SAFETY: стандартный цикл сообщений для окна, созданного этим потоком.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    reclaim_state(hwnd);
}

/// Забрать и уничтожить `Box<WndState>`, оставленный в GWLP_USERDATA
/// (тот же паттерн, что у трея). Снятие хуков/таймера/WTS уже случилось в
/// `WM_DESTROY` до этого — здесь только память.
fn reclaim_state(hwnd: HWND) {
    // SAFETY: указатель либо null, либо был получен из `Box::into_raw` в
    // этом же потоке и ни разу не освобождался (цикл сообщений уже завершён).
    unsafe {
        let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut WndState;
        if !ptr.is_null() {
            drop(Box::from_raw(ptr));
        }
    }
}

fn create_window() -> Result<HWND, Win32Error> {
    // SAFETY: GetModuleHandleW(None) возвращает хэндл текущего модуля.
    let hinstance = unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .map_err(Win32Error::Win32)?;

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: wc заполнена корректно; повторная регистрация класса (второй
    // трекер в процессе, параллельные тесты) — не ошибка, класс уже готов.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        // SAFETY: GetLastError осмысленна сразу после провалившегося вызова
        // на этом же потоке.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(Win32Error::Win32(err.into()));
        }
    }

    // HWND_MESSAGE: окно только для приёма сообщений (таймер дебаунса, WTS),
    // никогда не показывается и не участвует в z-order реальных окон.
    // SAFETY: все аргументы — валидные константы/только что созданный класс.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(Win32Error::Win32)?;

    if hwnd.0.is_null() {
        return Err(Win32Error::WindowTrackerWindowCreateFailed);
    }

    // Заморозка кэша на блокировке сессии (§7) — тот же паттерн, что у
    // оверлея; отказ регистрации не фатален (просто не будет заморозки).
    // SAFETY: hwnd — действительное окно этого потока, живёт до WM_DESTROY.
    if unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }.is_err() {
        tracing::warn!(
            "WTSRegisterSessionNotification не удалась; заморозка на блокировке недоступна"
        );
    }

    Ok(hwnd)
}

/// Установить оба диапазона хуков (M4_WINDOW_TRACKER_DESIGN.md §3): диапазон
/// `DESTROY..LOCATIONCHANGE` захватывает и `SHOW`/`HIDE` (соседние коды) —
/// колбэк фильтрует по точному `event`, лишние коды в диапазоне просто
/// падают в `_ => {}`; `MINIMIZESTART..MINIMIZEEND` — отдельный узкий
/// диапазон (коды не соседствуют с первым). Без `WINEVENT_SKIPOWNPROCESS`
/// намеренно: собственные оверлеи и так никогда не попадают в кэш
/// (`is_real_window` отбраковывает их по `WS_EX_NOACTIVATE`), поэтому их
/// `LOCATIONCHANGE` просто не проходит проверку «hwnd в кэше» в колбэке —
/// накладные расходы на лишний вызов колбэка для горстки редко двигающихся
/// собственных окон ничтожны, а SKIP лишил бы трекер возможности увидеть
/// СОБСТВЕННОЕ тестовое окно в интеграционных тестах этого же модуля
/// (тест обязательно живёт в одном процессе с трекером).
fn install_hooks(state: &mut WndState) {
    if !state.hooks.is_empty() {
        return; // уже установлены — идемпотентно
    }
    let flags = WINEVENT_OUTOFCONTEXT;
    for (min, max) in [
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    ] {
        // SAFETY: win_event_proc — валидный `WINEVENTPROC`; hmodule/idprocess/
        // idthread = 0/None — весь процесс, любой поток (WINEVENT_OUTOFCONTEXT
        // сам доставит колбэк в очередь ЭТОГО потока, ADR-005).
        let hook = unsafe { SetWinEventHook(min, max, None, Some(win_event_proc), 0, 0, flags) };
        if hook.0.is_null() {
            // Половинчатая установка хуже отсутствия: первый диапазон несёт
            // SHOW/HIDE — единственный источник полных перечислений — его
            // потеря при живом втором навсегда «замораживает» кэш молча.
            // Честная деградация — снести уже поставленные и остаться без
            // хуков вовсе (эквивалент fast path, без эмиссии), а не жить в
            // полуработающем состоянии (M4_WINDOW_TRACKER_REVIEW.md, пункт
            // 2.2). Следующий переключающий цикл set_mask_needed(false→true)
            // повторит попытку — `state.hooks.is_empty()` снова пропустит
            // идемпотентный ранний выход выше.
            tracing::warn!(
                min,
                max,
                "SetWinEventHook не удался — снимаю уже установленные диапазоны, кэш окон не будет живым"
            );
            uninstall_hooks(state);
            return;
        }
        state.hooks.push(hook);
    }
}

fn uninstall_hooks(state: &mut WndState) {
    for hook in state.hooks.drain(..) {
        // SAFETY: hook получен из SetWinEventHook на этом же потоке и ещё не
        // снимался.
        unsafe {
            let _ = UnhookWinEvent(hook);
        }
    }
}

/// Обнулить дебаунс-таймер, если он ещё заведён (снос окна, уход в fast
/// path, заморозка сессии).
fn kill_debounce_timer(hwnd: HWND, state: &mut WndState) {
    if state.timer_running {
        // SAFETY: hwnd — наше окно, DEBOUNCE_TIMER_ID — тот же id, что при
        // установке.
        unsafe {
            let _ = KillTimer(Some(hwnd), DEBOUNCE_TIMER_ID);
        }
        state.timer_running = false;
    }
}

/// Удалить `hwnd` из кэша. Возвращает, был ли он там.
fn apply_destroy(cache: &mut Vec<WindowInfo>, hwnd: usize) -> bool {
    if let Some(pos) = cache.iter().position(|w| w.hwnd == hwnd) {
        cache.remove(pos);
        true
    } else {
        false
    }
}

/// Выставить/снять `iconic` у `hwnd`, если он в кэше.
fn apply_minimize(cache: &mut [WindowInfo], hwnd: usize, iconic: bool) -> bool {
    if let Some(w) = cache.iter_mut().find(|w| w.hwnd == hwnd) {
        w.iconic = iconic;
        true
    } else {
        false
    }
}

/// Обновить `rect` у `hwnd`, если он в кэше и не свёрнут (у свёрнутых rect
/// мусорный — не перезаписывать его свежим мусором).
fn apply_location(cache: &mut [WindowInfo], hwnd: usize, rect: WindowRect) -> bool {
    if let Some(w) = cache.iter_mut().find(|w| w.hwnd == hwnd) {
        if !w.iconic {
            w.rect = rect;
        }
        true
    } else {
        false
    }
}

/// Полное перечисление, заменяющее кэш целиком (SHOW/HIDE/force/unlock).
/// Возвращает копию нового кэша для эмиссии — вызывающему не нужно клонировать
/// отдельно.
fn rebuild_from_enum(cache: &mut Vec<WindowInfo>) -> Vec<WindowInfo> {
    *cache = window_enum::enumerate();
    cache.clone()
}

/// Одна разрешённая операция над кэшом после снятия приоритетов между
/// дельтами одного и того же `hwnd` в пределах окна дебаунса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedOp {
    Destroy(usize),
    /// `refresh_rect` — true только для `MINIMIZEEND` (`!iconic`): DWM
    /// отдаёт свежий rect после восстановления, пересобрать его сразу, не
    /// дожидаясь отдельного `LOCATIONCHANGE`.
    Minimize {
        hwnd: usize,
        iconic: bool,
        refresh_rect: bool,
    },
    Location(usize),
}

/// Снять приоритеты между дельтами одного `hwnd`, накопленными за одно окно
/// дебаунса: снос важнее минимизации/перемещения (мёртвому окну незачем
/// освежать rect или iconic-флаг), минимизация — важнее отдельного
/// `LOCATIONCHANGE` того же `hwnd` (уже покрыта через `refresh_rect`).
/// Чистая функция — тестируется без кэша и без Win32
/// (M4_WINDOW_TRACKER_REVIEW.md, пункт 2.3). Вызывается только когда
/// `pending.needs_full == false` — полное перечисление решает целиком, эта
/// функция для него не нужна.
fn resolve_pending(pending: &Pending) -> Vec<ResolvedOp> {
    let mut ops = Vec::with_capacity(
        pending.destroyed.len() + pending.minimized.len() + pending.location_changed.len(),
    );
    for &hwnd in &pending.destroyed {
        ops.push(ResolvedOp::Destroy(hwnd));
    }
    for (&hwnd, &iconic) in &pending.minimized {
        if pending.destroyed.contains(&hwnd) {
            continue; // уже снесён — минимизация мертва
        }
        ops.push(ResolvedOp::Minimize {
            hwnd,
            iconic,
            refresh_rect: !iconic,
        });
    }
    for &hwnd in &pending.location_changed {
        if pending.destroyed.contains(&hwnd) || pending.minimized.contains_key(&hwnd) {
            continue; // уже обработано выше (снос или minimize/restore)
        }
        ops.push(ResolvedOp::Location(hwnd));
    }
    ops
}

/// Применить накопленные изменения и отправить свежий снимок. Вызывается из
/// `WM_TIMER` (обычный дебаунс) и из мест, требующих немедленного полного
/// перечисления (`set_mask_needed(true)`, разблокировка сессии).
fn flush_pending(state: &mut WndState) {
    let snapshot = if state.pending.needs_full {
        rebuild_from_enum(&mut state.cache)
    } else {
        for op in resolve_pending(&state.pending) {
            match op {
                ResolvedOp::Destroy(hwnd) => {
                    apply_destroy(&mut state.cache, hwnd);
                }
                ResolvedOp::Minimize {
                    hwnd,
                    iconic,
                    refresh_rect,
                } => {
                    apply_minimize(&mut state.cache, hwnd, iconic);
                    if refresh_rect {
                        refresh_rect_or_destroy(&mut state.cache, hwnd);
                    }
                }
                ResolvedOp::Location(hwnd) => {
                    refresh_rect_or_destroy(&mut state.cache, hwnd);
                }
            }
        }
        state.cache.clone()
    };
    state.pending = Pending::default();
    let _ = state.tx.send(WindowEvent::Changed(snapshot));
}

/// Перечитать `rect` через DWM для живого окна; если оно уже умерло без
/// `EVENT_OBJECT_DESTROY` (hwnd переиспользован/гонка) — снести из кэша тем
/// же путём, что настоящий DESTROY (M4_WINDOW_TRACKER_DESIGN.md §4).
fn refresh_rect_or_destroy(cache: &mut Vec<WindowInfo>, hwnd: usize) {
    let h = hwnd_from_usize(hwnd);
    // SAFETY: h собран из ранее увиденного hwnd; IsWindow безопасен и для
    // уже недействительного хэндла (просто вернёт false).
    if unsafe { IsWindow(Some(h)) }.as_bool() {
        apply_location(cache, hwnd, window_enum::extended_frame_bounds(h));
    } else {
        apply_destroy(cache, hwnd);
    }
}

/// Результат классификации одного WinEvent-события — что сделать с
/// `Pending`, без самого `Pending` в сигнатуре: чистая функция, тестируемая
/// без окон и без колбэка (M4_WINDOW_TRACKER_REVIEW.md, пункт 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    NeedsFull,
    Destroyed,
    Minimized(bool),
    LocationChanged,
}

/// Классифицировать событие хука (M4_PREP_NOTES §3.2, таблица хуков):
/// `None` — событие не интересно (лишний код диапазона `SetWinEventHook`
/// или `LOCATIONCHANGE` окна вне кэша) — колбэк ничего не копит и не
/// заводит таймер. `in_cache` учитывается только для `LOCATIONCHANGE`:
/// иначе любая всплывающая подсказка/дропдаун вне кэша будила бы дебаунс
/// впустую.
fn classify_event(event: u32, in_cache: bool) -> Option<PendingOp> {
    match event {
        EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE => Some(PendingOp::NeedsFull),
        EVENT_OBJECT_DESTROY => Some(PendingOp::Destroyed),
        EVENT_SYSTEM_MINIMIZESTART => Some(PendingOp::Minimized(true)),
        EVENT_SYSTEM_MINIMIZEEND => Some(PendingOp::Minimized(false)),
        EVENT_OBJECT_LOCATIONCHANGE if in_cache => Some(PendingOp::LocationChanged),
        _ => None,
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    // Фильтр «это само окно, не его часть» (M4_PREP_NOTES §3.2): дочерние
    // объекты (заголовок, кнопки) шлют те же события отдельно — нам нужен
    // только сам HWND.
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 {
        return;
    }
    if hwnd.0.is_null() {
        return;
    }
    let tracker_hwnd = TRACKER_HWND.with(Cell::get);
    if tracker_hwnd.0.is_null() {
        return; // колбэк долетел раньше, чем TRACKER_HWND записан — не бывает на практике
    }
    // SAFETY: WinEvent-колбэк с WINEVENT_OUTOFCONTEXT доставляется через
    // очередь сообщений потока-установщика хука (ADR-005) — колбэк
    // выполняется на ТОМ ЖЕ потоке, что владеет tracker_hwnd и его
    // GWLP_USERDATA, синхронно и без реентрантности (Windows не вызывает
    // WinEvent-колбэки этого потока параллельно друг другу). Отдельной
    // синхронизации не требуется.
    let state_ptr = unsafe { GetWindowLongPtrW(tracker_hwnd, GWLP_USERDATA) } as *mut WndState;
    let Some(state) = (unsafe { state_ptr.as_mut() }) else {
        return;
    };
    if state.frozen {
        return; // сессия заблокирована — не копим, разморозка сама всё перечитает
    }

    let target = hwnd.0 as usize;
    let in_cache = state.cache.iter().any(|w| w.hwnd == target);
    let Some(op) = classify_event(event, in_cache) else {
        return;
    };
    match op {
        PendingOp::NeedsFull => state.pending.needs_full = true,
        PendingOp::Destroyed => {
            state.pending.destroyed.insert(target);
        }
        PendingOp::Minimized(iconic) => {
            state.pending.minimized.insert(target, iconic);
        }
        PendingOp::LocationChanged => {
            state.pending.location_changed.insert(target);
        }
    }

    // Колбэк не делает НИКАКИХ других Win32-вызовов, кроме SetTimer — вся
    // остальная работа (перечисление, DWM-запросы) — в WM_TIMER
    // (M4_WINDOW_TRACKER_DESIGN.md §2).
    if !state.timer_running {
        // SAFETY: tracker_hwnd — живое окно этого потока.
        let id = unsafe { SetTimer(Some(tracker_hwnd), DEBOUNCE_TIMER_ID, DEBOUNCE_MS, None) };
        if id != 0 {
            state.timer_running = true;
        } else {
            tracing::warn!("SetTimer не удался — дебаунс окон окон не сработает для этого burst'а");
        }
    }
}

/// Разбор `wParam` из `WM_WTSSESSION_CHANGE` (тот же список кодов, что у
/// оверлея, только для заморозки/разморозки кэша, не для событий наружу —
/// координатор уже знает о блокировке из своего собственного окна).
fn session_change_action(wparam: WPARAM) -> Option<bool> {
    match wparam.0 as u32 {
        WTS_SESSION_LOCK => Some(true),
        WTS_SESSION_UNLOCK => Some(false),
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: указатель либо null (до установки состояния/после его снятия),
    // либо владеющий указатель этого потока, установленный в run_message_loop.
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WndState;
    match msg {
        WM_APP_SET_MASK_NEEDED => {
            let needed = wparam.0 != 0;
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                // Идемпотентно: повторный вызов с тем же значением — no-op,
                // не дублирует хуки и не эмитит лишний снимок.
                if needed != state.mask_needed {
                    state.mask_needed = needed;
                    if needed {
                        install_hooks(state);
                        // Немедленный полный снимок — координатор не ждёт
                        // первого системного события
                        // (M4_WINDOW_TRACKER_DESIGN.md §5). Но не на
                        // заблокированной сессии: перечислять там дорого и
                        // бессмысленно (M4_PREP_NOTES §9) — хуки уже
                        // установлены и готовы копить дельты, а сам снимок
                        // придёт из ветки WM_WTSSESSION_CHANGE на
                        // разблокировке (она проверяет тот же
                        // `state.mask_needed`, M4_WINDOW_TRACKER_REVIEW.md,
                        // пункт 2.5).
                        if !state.frozen {
                            let snapshot = rebuild_from_enum(&mut state.cache);
                            let _ = state.tx.send(WindowEvent::Changed(snapshot));
                        }
                    } else {
                        uninstall_hooks(state);
                        kill_debounce_timer(hwnd, state);
                        state.pending = Pending::default();
                        state.cache.clear();
                    }
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == DEBOUNCE_TIMER_ID {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    kill_debounce_timer(hwnd, state);
                    // `KillTimer` не убирает уже поставленный в очередь
                    // WM_TIMER (низкий приоритет диспетчеризации) — если
                    // set_mask_needed(false) успело обработаться раньше
                    // такого устаревшего сообщения, `pending`/`cache` уже
                    // сброшены, и наивный flush отправил бы пустой
                    // Changed(vec![]) уже выключенного трекера
                    // (M4_WINDOW_TRACKER_REVIEW.md, пункт 2.1).
                    if !state.mask_needed || state.frozen {
                        state.pending = Pending::default();
                    } else {
                        flush_pending(state);
                    }
                }
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_WTSSESSION_CHANGE => {
            if let Some(lock) = session_change_action(wparam) {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    if lock {
                        state.frozen = true;
                    } else {
                        state.frozen = false;
                        if state.mask_needed {
                            let snapshot = rebuild_from_enum(&mut state.cache);
                            let _ = state.tx.send(WindowEvent::Changed(snapshot));
                        }
                    }
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_DESTROY => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                uninstall_hooks(state);
                kill_debounce_timer(hwnd, state);
            }
            // SAFETY: hwnd валиден в WM_DESTROY.
            let _ = unsafe { WTSUnRegisterSessionNotification(hwnd) };
            // SAFETY: стандартный вызов из обработчика WM_DESTROY.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: делегирование необработанных сообщений системному обработчику.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(hwnd: usize, iconic: bool) -> WindowInfo {
        WindowInfo {
            hwnd,
            iconic,
            ..Default::default()
        }
    }

    #[test]
    fn apply_destroy_removes_present_hwnd() {
        let mut cache = vec![info(1, false), info(2, false)];
        assert!(apply_destroy(&mut cache, 1));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].hwnd, 2);
    }

    #[test]
    fn apply_destroy_absent_hwnd_is_noop() {
        let mut cache = vec![info(1, false)];
        assert!(!apply_destroy(&mut cache, 99));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn apply_minimize_sets_iconic_flag() {
        let mut cache = vec![info(1, false)];
        assert!(apply_minimize(&mut cache, 1, true));
        assert!(cache[0].iconic);
        assert!(apply_minimize(&mut cache, 1, false));
        assert!(!cache[0].iconic);
    }

    #[test]
    fn apply_minimize_absent_hwnd_is_noop() {
        let mut cache = vec![info(1, false)];
        assert!(!apply_minimize(&mut cache, 99, true));
    }

    #[test]
    fn apply_location_updates_rect_when_not_iconic() {
        let mut cache = vec![info(1, false)];
        let rect = WindowRect {
            x: 10,
            y: 20,
            w: 300,
            h: 400,
        };
        assert!(apply_location(&mut cache, 1, rect));
        assert_eq!(cache[0].rect, rect);
    }

    #[test]
    fn apply_location_skips_iconic_window() {
        let mut cache = vec![info(1, true)];
        let original = cache[0].rect;
        let rect = WindowRect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };
        assert!(apply_location(&mut cache, 1, rect));
        assert_eq!(cache[0].rect, original, "rect свёрнутого окна не трогаем");
    }

    #[test]
    fn apply_location_absent_hwnd_is_noop() {
        let mut cache = vec![info(1, false)];
        let original = cache.clone();
        assert!(!apply_location(
            &mut cache,
            99,
            WindowRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            }
        ));
        assert_eq!(cache, original);
    }

    #[test]
    fn session_change_action_maps_lock_and_unlock_only() {
        assert_eq!(
            session_change_action(WPARAM(WTS_SESSION_LOCK as usize)),
            Some(true)
        );
        assert_eq!(
            session_change_action(WPARAM(WTS_SESSION_UNLOCK as usize)),
            Some(false)
        );
        assert_eq!(session_change_action(WPARAM(9999)), None);
    }

    #[test]
    fn classify_event_show_and_hide_need_full() {
        assert_eq!(
            classify_event(EVENT_OBJECT_SHOW, false),
            Some(PendingOp::NeedsFull)
        );
        assert_eq!(
            classify_event(EVENT_OBJECT_HIDE, false),
            Some(PendingOp::NeedsFull)
        );
        // in_cache не важен для SHOW/HIDE — источник полных перечислений
        // не фильтруется членством (M4_WINDOW_TRACKER_DESIGN.md §3).
        assert_eq!(
            classify_event(EVENT_OBJECT_SHOW, true),
            Some(PendingOp::NeedsFull)
        );
    }

    #[test]
    fn classify_event_destroy_and_minimize() {
        assert_eq!(
            classify_event(EVENT_OBJECT_DESTROY, false),
            Some(PendingOp::Destroyed)
        );
        assert_eq!(
            classify_event(EVENT_SYSTEM_MINIMIZESTART, false),
            Some(PendingOp::Minimized(true))
        );
        assert_eq!(
            classify_event(EVENT_SYSTEM_MINIMIZEEND, false),
            Some(PendingOp::Minimized(false))
        );
    }

    #[test]
    fn classify_event_location_change_requires_cache_membership() {
        assert_eq!(
            classify_event(EVENT_OBJECT_LOCATIONCHANGE, true),
            Some(PendingOp::LocationChanged)
        );
        assert_eq!(
            classify_event(EVENT_OBJECT_LOCATIONCHANGE, false),
            None,
            "окно вне кэша не должно будить дебаунс"
        );
    }

    #[test]
    fn classify_event_unknown_is_none() {
        assert_eq!(classify_event(0, true), None);
        // EVENT_SYSTEM_FOREGROUND (3) — намеренно не в таблице хуков
        // (z-order маске не нужен, M4_WINDOW_TRACKER_DESIGN.md §3).
        assert_eq!(classify_event(3, true), None);
    }

    #[test]
    fn resolve_pending_empty_yields_no_ops() {
        assert!(resolve_pending(&Pending::default()).is_empty());
    }

    #[test]
    fn resolve_pending_destroy_preempts_minimize_and_location_for_same_hwnd() {
        let mut pending = Pending::default();
        pending.destroyed.insert(1);
        pending.minimized.insert(1, true);
        pending.location_changed.insert(1);
        // Другое окно — не должно быть затронуто приоритетом первого.
        pending.location_changed.insert(2);

        let ops = resolve_pending(&pending);
        assert_eq!(ops, vec![ResolvedOp::Destroy(1), ResolvedOp::Location(2)]);
    }

    #[test]
    fn resolve_pending_minimize_preempts_location_for_same_hwnd() {
        let mut pending = Pending::default();
        pending.minimized.insert(1, false);
        pending.location_changed.insert(1);

        let ops = resolve_pending(&pending);
        assert_eq!(
            ops,
            vec![ResolvedOp::Minimize {
                hwnd: 1,
                iconic: false,
                refresh_rect: true,
            }]
        );
    }

    #[test]
    fn resolve_pending_minimize_start_does_not_refresh_rect() {
        let mut pending = Pending::default();
        pending.minimized.insert(1, true);

        let ops = resolve_pending(&pending);
        assert_eq!(
            ops,
            vec![ResolvedOp::Minimize {
                hwnd: 1,
                iconic: true,
                refresh_rect: false,
            }]
        );
    }

    #[test]
    fn resolve_pending_independent_hwnds_all_kept() {
        let mut pending = Pending::default();
        pending.destroyed.insert(1);
        pending.minimized.insert(2, true);
        pending.location_changed.insert(3);

        let mut ops = resolve_pending(&pending);
        ops.sort_by_key(|op| match op {
            ResolvedOp::Destroy(h) => *h,
            ResolvedOp::Minimize { hwnd, .. } => *hwnd,
            ResolvedOp::Location(h) => *h,
        });
        assert_eq!(
            ops,
            vec![
                ResolvedOp::Destroy(1),
                ResolvedOp::Minimize {
                    hwnd: 2,
                    iconic: true,
                    refresh_rect: false
                },
                ResolvedOp::Location(3),
            ]
        );
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_tracker -- --ignored"]
    fn start_then_drop_destroys_window() {
        let (tracker, _rx) = WindowTracker::start().expect("создание трекера");
        let hwnd = tracker.hwnd;
        // SAFETY: hwnd — наше живое окно, создание выше проверено.
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());

        drop(tracker);

        // SAFETY: после Drop окно уничтожено; чтение мёртвого хэндла безопасно.
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_tracker -- --ignored"]
    fn set_mask_needed_true_emits_full_snapshot_without_external_events() {
        let (tracker, rx) = WindowTracker::start().expect("создание трекера");
        tracker.set_mask_needed(true);
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(WindowEvent::Changed(windows)) => {
                assert!(
                    !windows.is_empty(),
                    "хотя бы одно окно на реальном десктопе"
                );
            }
            Err(e) => panic!("не дождались Changed после set_mask_needed(true): {e}"),
        }
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_tracker -- --ignored"]
    fn wts_lock_suppresses_emit_unlock_rebuilds() {
        // Прямой PostMessage с кодом WM_WTSSESSION_CHANGE на message-only
        // окно трекера — обработчик читает только wParam и не проверяет
        // источник сообщения, так что это эквивалентно реальному приходу
        // события от системы (M4_WINDOW_TRACKER_REVIEW.md, пункт 2.5).
        let (tracker, rx) = WindowTracker::start().expect("создание трекера");
        tracker.set_mask_needed(true);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("начальный Changed");

        // SAFETY: tracker.hwnd — наше живое message-only окно.
        unsafe {
            let _ = PostMessageW(
                Some(tracker.hwnd),
                WM_WTSSESSION_CHANGE,
                WPARAM(WTS_SESSION_LOCK as usize),
                LPARAM(0),
            );
        }
        match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Ok(event) => panic!("блокировка сессии не должна эмитить, получено: {event:?}"),
            Err(e) => panic!("канал событий трекера закрылся: {e}"),
        }

        // SAFETY: tracker.hwnd — наше живое message-only окно.
        unsafe {
            let _ = PostMessageW(
                Some(tracker.hwnd),
                WM_WTSSESSION_CHANGE,
                WPARAM(WTS_SESSION_UNLOCK as usize),
                LPARAM(0),
            );
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(WindowEvent::Changed(_)) => {}
            Err(e) => panic!("не дождались Changed после разблокировки сессии: {e}"),
        }
    }

    unsafe extern "system" fn test_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_DESTROY {
            // SAFETY: стандартный вызов из обработчика WM_DESTROY — иначе
            // GetMessageW этого потока не завершился бы после DestroyWindow
            // (DefWindowProc сам PostQuitMessage не шлёт).
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Реальное видимое top-level окно на СВОЁМ потоке-пампе — «реальное»
    /// для фильтра `is_real_window` (без ex-style, `WS_VISIBLE`), чтобы хуки
    /// трекера правда его увидели. Собственный поток обязателен: без него
    /// окно перестаёт отвечать на сообщения сразу после создания, и любой
    /// Win32-вызов, кросс-поточно читающий его состояние (`GetWindowTextW` и
    /// иже с ним делают `SendMessage` под капотом, если окно принадлежит
    /// другому потоку) виснет на ~5 с — классический таймаут «зависшего
    /// окна». У настоящего приложения такого не бывает, оно всегда что-то
    /// пампит; тестовое окно должно вести себя так же.
    struct RealWindow {
        hwnd: HWND,
        thread: Option<JoinHandle<()>>,
    }

    impl RealWindow {
        fn create() -> Self {
            let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
            let thread = thread::spawn(move || {
                use windows::Win32::UI::WindowsAndMessaging::{
                    SW_SHOW, ShowWindow, WS_OVERLAPPED, WS_VISIBLE,
                };
                // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
                let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(test_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: w!("resticker_window_tracker_test"),
                    ..Default::default()
                };
                // SAFETY: wc заполнена корректно; повторная регистрация
                // (параллельные тесты) — не ошибка.
                if unsafe { RegisterClassExW(&wc) } == 0 {
                    let err = unsafe { GetLastError() };
                    assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
                }
                // SAFETY: все аргументы — валидные константы/только что
                // зарегистрированный класс; окно видимое, реальное для фильтра.
                let hwnd = unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("resticker_window_tracker_test"),
                        w!("resticker window_tracker test"),
                        WS_OVERLAPPED | WS_VISIBLE,
                        0,
                        0,
                        200,
                        150,
                        None,
                        None,
                        Some(hinstance.into()),
                        None,
                    )
                }
                .expect("создание тестового окна");
                // SAFETY: hwnd только что создано этим потоком.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
                ready_tx.send(SendHwnd(hwnd)).expect("получатель ещё жив");

                let mut msg = MSG::default();
                // SAFETY: стандартный цикл сообщений для окна этого потока.
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            });
            let hwnd = ready_rx.recv().expect("поток тестового окна не упал").0;
            Self {
                hwnd,
                thread: Some(thread),
            }
        }
    }

    impl Drop for RealWindow {
        fn drop(&mut self) {
            // SAFETY: self.hwnd — окно потока-пампа; PostMessage безопасен
            // и для уже уничтоженного окна.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// Ждать `Changed`, для которого `pred` вернёт `Some`, максимум ~30 с
    /// (10 попыток × 3 с). На реальном занятом десктопе `Changed` может
    /// приходить и из-за постороннего системного шума (другие приложения,
    /// трей и т.п.) — ждать ровно ОДНО сообщение недостаточно, нужен
    /// ограниченный поиск среди нескольких.
    fn recv_until<T>(
        rx: &Receiver<WindowEvent>,
        mut pred: impl FnMut(&[WindowInfo]) -> Option<T>,
    ) -> Option<T> {
        for _ in 0..10 {
            match rx.recv_timeout(std::time::Duration::from_secs(3)) {
                Ok(WindowEvent::Changed(windows)) => {
                    if let Some(v) = pred(&windows) {
                        return Some(v);
                    }
                }
                Err(_) => return None,
            }
        }
        None
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_tracker -- --ignored"]
    fn real_window_lifecycle_show_move_destroy() {
        use windows::Win32::UI::WindowsAndMessaging::MoveWindow;

        let (tracker, rx) = WindowTracker::start().expect("создание трекера");
        let win = RealWindow::create();
        let hwnd = win.hwnd.0 as usize;

        tracker.set_mask_needed(true);
        // Начальный снимок (сразу после set_mask_needed(true)) должен уже
        // содержать окно — оно создано и показано ДО включения хуков.
        let initial_rect = recv_until(&rx, |windows| {
            windows.iter().find(|w| w.hwnd == hwnd).map(|w| w.rect)
        })
        .expect("тестовое окно должно быть в начальном снимке");

        // MoveWindow → EVENT_OBJECT_LOCATIONCHANGE → дебаунс 16 мс → Changed
        // с изменившимся rect. Точные w/h с MoveWindow не сравниваем:
        // DWMWA_EXTENDED_FRAME_BOUNDS может отступать от них на скрытую
        // рамку/тень окна — сравниваем только сам факт изменения.
        // SAFETY: hwnd создано этим же потоком, ещё живо.
        unsafe {
            let _ = MoveWindow(win.hwnd, 300, 300, 250, 200, true);
        }
        let moved_rect = recv_until(&rx, |windows| {
            windows
                .iter()
                .find(|w| w.hwnd == hwnd)
                .map(|w| w.rect)
                .filter(|rect| *rect != initial_rect)
        });
        assert_eq!(
            moved_rect.map(|r| r != initial_rect),
            Some(true),
            "rect должен измениться после MoveWindow"
        );

        // DestroyWindow → EVENT_OBJECT_DESTROY → Changed без этого hwnd.
        drop(win);
        let removed = recv_until(&rx, |windows| {
            (!windows.iter().any(|w| w.hwnd == hwnd)).then_some(())
        });
        assert!(
            removed.is_some(),
            "окно должно пропасть из снимка после уничтожения"
        );
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_tracker -- --ignored"]
    fn set_mask_needed_false_suppresses_events() {
        use windows::Win32::UI::WindowsAndMessaging::MoveWindow;

        let (tracker, rx) = WindowTracker::start().expect("создание трекера");
        tracker.set_mask_needed(true);
        // Слить начальный снимок, чтобы не спутать его с fast-path проверкой.
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("начальный Changed");
        tracker.set_mask_needed(false);
        // Дать set_mask_needed(false) время снять хуки на своём потоке.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let win = RealWindow::create();
        // SAFETY: hwnd создано этим же потоком, ещё живо.
        unsafe {
            let _ = MoveWindow(win.hwnd, 300, 300, 250, 200, true);
        }
        drop(win);

        match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Ok(event) => panic!("fast path не должен эмитить, получено: {event:?}"),
            Err(e) => panic!("канал событий трекера закрылся: {e}"),
        }
    }
}
