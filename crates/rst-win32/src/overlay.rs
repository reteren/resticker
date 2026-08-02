//! Оверлей-окно: прозрачное, «клик-прозрачное», поверх всех окон, на весь
//! основной монитор. Живёт на собственном потоке со своим циклом сообщений
//! (ADR-013 — тот же паттерн, что у трея). Здесь только окно и pump: рендер
//! (D3D11 + DirectComposition) подключается снаружи через [`OverlayWindow::hwnd`]
//! (ARCHITECTURE.md, раздел 2). Мультимонитор — M3, здесь ровно одно окно.

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXSCREEN,
    SM_CYSCREEN, SW_SHOW, ShowWindow, TranslateMessage, WM_CLOSE, WM_DESTROY, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::error::Win32Error;

const CLASS_NAME: PCWSTR = w!("resticker_overlay");
const WINDOW_TITLE: PCWSTR = w!("resticker_overlay_wnd");

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
    /// потоке. Возвращает управление, когда окно гарантированно создано
    /// (или поток сообщил об ошибке создания).
    pub fn create() -> Result<Self, Win32Error> {
        let (ready_tx, ready_rx) = mpsc::channel::<ReadyResult>();

        let thread = thread::spawn(move || run_message_loop(ready_tx));

        let (hwnd, size) = ready_rx
            .recv()
            .map_err(|_| Win32Error::OverlayThreadCrashed)??;

        Ok(Self {
            hwnd: hwnd.0,
            size,
            thread: Some(thread),
        })
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

fn run_message_loop(ready_tx: Sender<ReadyResult>) {
    let (hwnd, size) = match create_window() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

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
    // SAFETY: стандартный цикл сообщений для окна, созданного этим потоком.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
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
    // пойдёт напрямую через DirectComposition (проверено спайком S0).
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
    match msg {
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

    #[test]
    fn create_then_drop_destroys_window() {
        let overlay = OverlayWindow::create().expect("создание оверлея");
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
}
