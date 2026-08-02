//! Оверлей-окно: прозрачное, «клик-прозрачное», поверх всех окон, на весь
//! основной монитор. Живёт на собственном потоке со своим циклом сообщений
//! (ADR-013 — тот же паттерн, что у трея). Здесь только окно и pump: рендер
//! (D3D11 + DirectComposition) подключается снаружи через [`OverlayWindow::hwnd`]
//! (ARCHITECTURE.md, раздел 2). Мультимонитор — M3, здесь ровно одно окно.
//!
//! M2: окно также владеет глобальным хоткеем входа/выхода из режима
//! редактирования и мостом «сырые сообщения окна → безопасные события»
//! (`OverlayEvent`), см. docs/M2_INTEGRATION_PLAN.md, раздел 1. Мышь и курсор
//! обрабатываются здесь ([`crate::input`]); хит-тестинг и жесты — у вызывающего
//! кода (ядро редактора платформенно-независимо).

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_ESCAPE, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWLP_USERDATA,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SetForegroundWindow, SetWindowLongPtrW,
    ShowWindow, TranslateMessage, WM_CAPTURECHANGED, WM_CLOSE, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_SETCURSOR, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::error::Win32Error;
use crate::hotkey::{HotkeyCombo, RegisteredHotkey, message_hotkey_id};
use crate::input::{CursorManager, InputEvent, Modifiers, MouseCapture};

const CLASS_NAME: PCWSTR = w!("resticker_overlay");
const WINDOW_TITLE: PCWSTR = w!("resticker_overlay_wnd");

/// Идентификатор глобального хоткея входа/выхода из режима редактирования —
/// единственный хоткей, который регистрирует оверлей-окно (ROADMAP M2).
const EDIT_HOTKEY_ID: i32 = 1;

/// Безопасное событие оверлей-окна для координатора (docs/M2_INTEGRATION_PLAN.md,
/// раздел 1): мышь и клавиатура уже переведены из сырых Win32-сообщений,
/// хоткей — это именно и только переключатель режима редактирования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayEvent {
    /// Глобальный хоткей входа/выхода из режима редактирования нажат.
    ToggleEditMode,
    /// Событие мыши в клиентской области ([`crate::input::InputEvent`]).
    Input(InputEvent),
    /// Клавиша нажата/отпущена, пока окно в фокусе (режим редактирования —
    /// вне режима окно `WS_EX_NOACTIVATE` и фокус не получает).
    Key {
        vk: u32,
        modifiers: Modifiers,
        pressed: bool,
    },
}

/// Оверлей-окно на основной монитор и его поток сообщений.
/// `Drop` уничтожает окно и останавливает поток.
pub struct OverlayWindow {
    hwnd: HWND,
    size: (u32, u32),
    thread: Option<JoinHandle<()>>,
}

// HWND — просто числовой хэндл (isize), безопасно передавать между потоками.
// Уничтожение окна происходит только в `Drop` (с владением `OverlayWindow`),
// поэтому разделяемый `&OverlayWindow` даёт лишь чтение хэндла и размеров —
// `Sync` безопасен. Что делать с хэндлом дальше (рендер) — зона
// ответственности вызывающего кода.
unsafe impl Send for OverlayWindow {}
unsafe impl Sync for OverlayWindow {}

/// `HWND` не `Send` по умолчанию (внутри — `*mut c_void`), хотя это лишь
/// число. Обёртка нужна только для пересылки готового хэндла через канал.
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

/// Результат инициализации потока оверлея: хэндл окна и его размер.
type ReadyResult = Result<(SendHwnd, (u32, u32)), Win32Error>;

impl OverlayWindow {
    /// Создаёт оверлей-окно и запускает его цикл сообщений на отдельном
    /// потоке; регистрирует на этом же потоке глобальный хоткей `edit_hotkey`
    /// входа/выхода из режима редактирования (конфликт — не паника, только
    /// лог: `Win32Error::HotkeyConflict` из потока не всплывает наружу,
    /// поведение окна от него не зависит). Возвращает управление, когда окно
    /// гарантированно создано, и приёмник событий мыши/клавиатуры/хоткея —
    /// координатор объединяет его со своим каналом команд
    /// (docs/M2_INTEGRATION_PLAN.md, раздел 1).
    pub fn create(edit_hotkey: HotkeyCombo) -> Result<(Self, Receiver<OverlayEvent>), Win32Error> {
        let (ready_tx, ready_rx) = mpsc::channel::<ReadyResult>();
        let (event_tx, event_rx) = mpsc::channel::<OverlayEvent>();

        let thread = thread::spawn(move || run_message_loop(ready_tx, event_tx, edit_hotkey));

        let (hwnd, size) = ready_rx
            .recv()
            .map_err(|_| Win32Error::OverlayThreadCrashed)??;

        Ok((
            Self {
                hwnd: hwnd.0,
                size,
                thread: Some(thread),
            },
            event_rx,
        ))
    }

    /// Сырой `HWND` для передачи в `rst-render::Renderer::new(hwnd, width, height)`.
    /// Окном владеет поток оверлея: рисовать в него можно, уничтожать — нельзя.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Размер окна в физических пикселях на момент создания
    /// (процесс PerMonitorV2, см. манифест).
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// DPI монитора, на котором создано окно (96 = 100%). Используется для
    /// `Renderer::set_dpi_scale` и перевода координат мыши физика→DIP (M2,
    /// docs/M2_INTEGRATION_REVIEW.md, раздел 2). Полноценная реакция на
    /// `WM_DPICHANGED` при смене монитора/масштаба — M3.
    pub fn dpi(&self) -> u32 {
        // SAFETY: hwnd — наше живое окно.
        unsafe { GetDpiForWindow(self.hwnd) }
    }

    /// Переключить клик-прозрачность окна (ARCHITECTURE.md, раздел 5.2):
    /// `true` — вне режима редактирования, окно снова полностью
    /// клик-прозрачно и не берёт фокус; `false` — вход в режим
    /// редактирования, окно принимает мышь/клавиатуру на всей площади
    /// монитора и забирает фокус (`SetForegroundWindow`).
    pub fn set_click_through(&self, click_through: bool) {
        // SAFETY: hwnd — наше живое окно; смена GWL_EXSTYLE безопасна с
        // любого потока (в отличие от владения самим HWND).
        unsafe {
            let mut ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            let bits = WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0;
            if click_through {
                ex |= bits;
            } else {
                ex &= !bits;
            }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex as isize);
            if !click_through {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // Окно принадлежит потоку оверлея, поэтому уничтожает его он сам:
        // шлём WM_CLOSE, DefWindowProc вызовет DestroyWindow на его потоке,
        // а наш обработчик WM_DESTROY завершит цикл сообщений. Прямой вызов
        // DestroyWindow отсюда недопустим: окно, созданное другим потоком,
        // он не уничтожает, и join() ниже завис бы навсегда.
        // SAFETY: hwnd — наше окно; PostMessage безопасен и для уже
        // уничтоженного окна (просто вернёт ошибку, которую игнорируем).
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Состояние, живущее на потоке оверлея между сообщениями: захват мыши,
/// курсор и канал событий. Хранится через `GWLP_USERDATA` (стандартный Win32
/// паттерн — `wndproc` не может захватывать переменные, это `extern "system"
/// fn`), владение — у `run_message_loop`, освобождается в `WM_NCDESTROY`.
struct WndState {
    capture: MouseCapture,
    cursor: CursorManager,
    tx: Sender<OverlayEvent>,
}

fn run_message_loop(
    ready_tx: Sender<ReadyResult>,
    event_tx: Sender<OverlayEvent>,
    edit_hotkey: HotkeyCombo,
) {
    let (hwnd, size) = match create_window() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // Хоткей — на этом же потоке (тип `!Send`, ADR-009); конфликт логируется,
    // но не мешает окну работать (ARCHITECTURE.md, раздел 5.1).
    let _hotkey = match RegisteredHotkey::register(EDIT_HOTKEY_ID, edit_hotkey) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(error = %e, "не удалось зарегистрировать хоткей режима редактирования");
            None
        }
    };

    // Клон для перехвата WM_HOTKEY прямо в цикле сообщений (см. ниже) —
    // wndproc его не увидит (сообщение с hwnd=NULL).
    let hotkey_tx = event_tx.clone();

    let state = Box::new(WndState {
        capture: MouseCapture::new(hwnd),
        cursor: CursorManager::new(),
        tx: event_tx,
    });
    // SAFETY: hwnd — наше окно этого потока; указатель освобождается в
    // WM_NCDESTROY ниже (единственное место, где он читается и дропается).
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    }

    if ready_tx.send(Ok((SendHwnd(hwnd), size))).is_err() {
        // Получатель уже отброшен (конструктор вернул ошибку раньше) —
        // корректно свернуться, не оставляя окно висеть.
        // SAFETY: hwnd действительно и ещё не уничтожено.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: стандартный цикл сообщений для окна, созданного этим потоком.
        let has_msg = unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool();
        if !has_msg {
            break;
        }
        // WM_HOTKEY для хоткея потока (hwnd=None при регистрации) приходит с
        // msg.hwnd = NULL; DispatchMessageW для такого сообщения не вызывает
        // wndproc (у NULL-окна его нет), поэтому перехватываем здесь
        // (docs/M2_INTEGRATION_REVIEW.md, раздел 4).
        if msg.message == WM_HOTKEY && msg.hwnd.0.is_null() {
            if message_hotkey_id(msg.wParam) == EDIT_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleEditMode);
            }
            continue;
        }
        // SAFETY: msg заполнено предыдущим GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Модификаторы клавиатуры вне мышиного сообщения (для `WM_KEYDOWN`/`WM_KEYUP`,
/// у которых, в отличие от мышиных сообщений, нет битов `MK_*` в `wparam`).
fn current_key_modifiers() -> Modifiers {
    // SAFETY: чтение состояния клавиш вызывающего потока — тот же паттерн,
    // что и в `input::Modifiers::current`.
    unsafe {
        Modifiers {
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            alt: false,
        }
    }
}

fn create_window() -> Result<(HWND, (u32, u32)), Win32Error> {
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
    // SAFETY: wc заполнена корректно. Класс — процесс-wide: повторная
    // регистрация (второе окно в этом же процессе, параллельные тесты)
    // проваливается с ERROR_CLASS_ALREADY_EXISTS — это не ошибка, класс
    // уже готов к использованию. Остальные коды — настоящий сбой.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        // SAFETY: GetLastError осмысленна сразу после провалившегося вызова
        // на этом же потоке.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(Win32Error::Win32(err.into()));
        }
    }

    // SAFETY: GetSystemMetrics безопасен с любого потока, аргумент — константа.
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    if width <= 0 || height <= 0 {
        return Err(Win32Error::OverlayWindowCreateFailed);
    }

    // Стили — по ARCHITECTURE.md, раздел 2: TOPMOST — всегда сверху;
    // TOOLWINDOW — вне Alt+Tab и панели задач; NOACTIVATE — не забирает фокус;
    // TRANSPARENT — клики проходят насквозь; NOREDIRECTIONBITMAP — контент
    // пойдёт напрямую через DirectComposition (проверено спайком S0). Оба
    // флага (NOACTIVATE, TRANSPARENT) снимаются на время режима
    // редактирования через `set_click_through` (M2).
    // SAFETY: все аргументы — валидные константы и только что
    // зарегистрированный класс.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | WS_EX_TRANSPARENT
                | WS_EX_NOREDIRECTIONBITMAP,
            CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            0,
            0,
            width,
            height,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(Win32Error::Win32)?;

    if hwnd.0.is_null() {
        return Err(Win32Error::OverlayWindowCreateFailed);
    }

    // SAFETY: hwnd — действительное окно, созданное выше этим потоком.
    // Окно без redirection-битмапа и контента визуально пустое и
    // клик-прозрачное, показ безопасен.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    Ok((hwnd, (width as u32, height as u32)))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA установлен в run_message_loop сразу после
    // создания окна этим же потоком, до входа в цикл сообщений; читается и
    // освобождается только здесь. До установки (между CreateWindowExW и
    // SetWindowLongPtrW) указатель — null, обрабатываем это явно.
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WndState;

    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_CAPTURECHANGED => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if let Some(event) = state.capture.handle_message(msg, wparam, lparam) {
                    let _ = state.tx.send(OverlayEvent::Input(event));
                    return LRESULT(0);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SETCURSOR => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if let Some(res) = state.cursor.handle_set_cursor(lparam) {
                    return res;
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_KEYDOWN | WM_KEYUP => {
            let vk = wparam.0 as u32;
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                let _ = state.tx.send(OverlayEvent::Key {
                    vk,
                    modifiers: current_key_modifiers(),
                    pressed: msg == WM_KEYDOWN,
                });
            }
            if vk == VK_ESCAPE.0 as u32 {
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_NCDESTROY => {
            if !state_ptr.is_null() {
                // SAFETY: последнее использование этого указателя — окно
                // уничтожается, WM_NCDESTROY приходит ровно один раз.
                unsafe {
                    drop(Box::from_raw(state_ptr));
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => {
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
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    fn test_hotkey() -> HotkeyCombo {
        // Экзотическая комбинация — не конфликтует с реальными приложениями
        // на машине разработчика/CI.
        HotkeyCombo::parse("Ctrl+Alt+Shift+F23").expect("валидная комбинация")
    }

    #[test]
    fn create_then_drop_destroys_window() {
        let (overlay, _events) = OverlayWindow::create(test_hotkey()).expect("создание оверлея");
        let hwnd = overlay.hwnd();
        assert!(!hwnd.0.is_null());

        let (w, h) = overlay.size();
        assert!(w > 0 && h > 0);

        // SAFETY: hwnd — наше живое окно, создание выше проверено.
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());

        drop(overlay);

        // SAFETY: после Drop окно уничтожено; IsWindow над мёртвым хэндлом —
        // простое чтение, хэндл мы больше никуда не передаём.
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
    }

    #[test]
    fn set_click_through_toggles_exstyle_bits() {
        let (overlay, _events) = OverlayWindow::create(test_hotkey()).expect("создание оверлея");
        // SAFETY: чтение стиля своего же окна.
        let initial = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(
            initial & WS_EX_TRANSPARENT.0,
            0,
            "по умолчанию клик-прозрачно"
        );

        overlay.set_click_through(false);
        let editing = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_eq!(
            editing & WS_EX_TRANSPARENT.0,
            0,
            "в режиме редактирования — нет"
        );
        assert_eq!(editing & WS_EX_NOACTIVATE.0, 0);

        overlay.set_click_through(true);
        let restored = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(
            restored & WS_EX_TRANSPARENT.0,
            0,
            "выход восстанавливает клик-прозрачность"
        );
        assert_ne!(restored & WS_EX_NOACTIVATE.0, 0);
    }
}
