//! Иконка в трее: скрытое окно + `Shell_NotifyIconW`, на собственном потоке
//! со своим циклом сообщений (ADR-013 — тот же паттерн, что у оверлея).
//! Весь unsafe живёт здесь, наружу — только каналы и безопасные типы.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ExtractIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, HICON,
    IDI_APPLICATION, LoadIconW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
    WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::error::Win32Error;

const WM_TRAYICON: u32 = WM_APP + 1;
const CLASS_NAME: PCWSTR = w!("resticker_tray");

/// Один пункт контекстного меню трея; `id` возвращается в `TrayEvent::MenuItem`.
/// `id == 0` рисуется как разделитель (см. [`separator`]).
pub struct MenuItem {
    pub id: u32,
    pub label: String,
}

/// Разделитель между пунктами меню.
pub fn separator() -> MenuItem {
    MenuItem {
        id: 0,
        label: String::new(),
    }
}

pub enum TrayEvent {
    /// Выбран пункт меню (`id`).
    MenuItem(u32),
    /// Клик левой кнопкой прямо по иконке (не по меню).
    Activate,
}

/// Владеет иконкой трея и её потоком сообщений. `Drop` снимает иконку и
/// останавливает поток.
pub struct TrayIcon {
    hwnd: HWND,
    thread: Option<JoinHandle<()>>,
}

// HWND — просто числовой хэндл (isize), безопасно передавать между потоками;
// `TrayIcon` не предоставляет доступа к нему изнутри (только Drop), поэтому
// разделение между потоками (`Sync`) тоже безопасно.
unsafe impl Send for TrayIcon {}
unsafe impl Sync for TrayIcon {}

/// `HWND` не `Send` по умолчанию (внутри — `*mut c_void`), хотя это лишь
/// число. Обёртка нужна только для пересылки готового хэндла через канал.
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

struct WndState {
    menu: Vec<MenuItem>,
    tx: Sender<TrayEvent>,
}

impl TrayIcon {
    /// Запускает окно трея и цикл сообщений на отдельном потоке.
    /// `tooltip` — подсказка при наведении; `menu` — пункты контекстного меню.
    pub fn new(
        tooltip: &str,
        menu: Vec<MenuItem>,
    ) -> Result<(Self, Receiver<TrayEvent>), Win32Error> {
        let (tx, rx) = mpsc::channel::<TrayEvent>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<SendHwnd, Win32Error>>();
        let tooltip = tooltip.to_string();

        let thread = thread::spawn(move || run_message_loop(tooltip, menu, tx, ready_tx));

        let hwnd = ready_rx
            .recv()
            .map_err(|_| Win32Error::TrayThreadCrashed)??
            .0;

        Ok((
            Self {
                hwnd,
                thread: Some(thread),
            },
            rx,
        ))
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // Окно принадлежит потоку трея, поэтому уничтожает его он сам: шлём
        // WM_CLOSE, DefWindowProc вызовет DestroyWindow на его потоке, а наш
        // обработчик WM_DESTROY завершит цикл сообщений. Прямой вызов
        // DestroyWindow отсюда (другой поток) молча проваливается — окно,
        // созданное другим потоком, он не уничтожает, — и join() ниже
        // зависал бы навсегда.
        // SAFETY: self.hwnd — наше окно; PostMessage безопасен и для уже
        // уничтоженного окна (просто вернёт ошибку, которую игнорируем).
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run_message_loop(
    tooltip: String,
    menu: Vec<MenuItem>,
    tx: Sender<TrayEvent>,
    ready_tx: Sender<Result<SendHwnd, Win32Error>>,
) {
    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let state = Box::new(WndState { menu, tx });
    // SAFETY: hwnd только что создано этим потоком; GWLP_USERDATA хранит
    // единственный владеющий указатель, освобождаемый в конце этой функции.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    }

    if let Err(e) = add_notify_icon(hwnd, &tooltip) {
        let _ = ready_tx.send(Err(e));
        // SAFETY: hwnd действительно и ещё не уничтожено.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        reclaim_state(hwnd);
        return;
    }

    if ready_tx.send(Ok(SendHwnd(hwnd))).is_err() {
        // Получатель уже отброшен (конструктор вернул ошибку раньше) —
        // корректно свернуться, ничего не показывая пользователю.
        let _ = remove_notify_icon(hwnd);
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

    let _ = remove_notify_icon(hwnd);
    reclaim_state(hwnd);
}

/// Забрать и уничтожить `Box<WndState>`, оставленный в GWLP_USERDATA.
fn reclaim_state(hwnd: HWND) {
    // SAFETY: указатель либо null, либо был получен из `Box::into_raw` в
    // этом же потоке и ни разу не был освобождён (окно уже не получает
    // сообщений — цикл завершён/не начат).
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
    // SAFETY: wc заполнена корректно; регистрация класса — идемпотентная
    // Win32-операция, ошибку (0) от повторной регистрации игнорируем нельзя,
    // но при первом вызове на процесс она безопасна.
    unsafe {
        RegisterClassExW(&wc);
    }

    // Скрытое окно только для приёма сообщений — не показывается никогда.
    // SAFETY: все аргументы — валидные константы/только что созданный класс.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("resticker_tray_wnd"),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(Win32Error::Win32)?;

    if hwnd.0.is_null() {
        return Err(Win32Error::TrayWindowCreateFailed);
    }
    Ok(hwnd)
}

/// Иконка приложения (не стоковая `IDI_APPLICATION`): извлекается прямо из
/// своего же exe — `tauri_build`/`winres` уже вшивает `icons/icon.ico` в
/// ресурсы бинарника (build.rs, `tauri_build::try_build`), `ExtractIconW`
/// по собственному пути (`current_exe`) достаёт её без необходимости знать
/// точный числовой ID/имя ресурса, под которым её положил инструмент
/// сборки — тот же принцип независимости от деталей упаковки. `None` —
/// `current_exe()` не удался или в бинарнике нет иконки (например, тестовый
/// прогон без реальной сборки `tauri_build`).
fn app_icon() -> Option<HICON> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.as_os_str().to_str()?;
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    // SAFETY: wide — валидный нуль-терминированный путь на время вызова;
    // индекс 0 — первая (обычно единственная) иконка ресурсов exe.
    let icon = unsafe { ExtractIconW(None, PCWSTR(wide.as_ptr()), 0) };
    if icon.is_invalid() || icon.0.is_null() {
        None
    } else {
        Some(icon)
    }
}

fn notify_icon_data(hwnd: HWND, tooltip: &str) -> NOTIFYICONDATAW {
    // Стоковая IDI_APPLICATION — запасной вариант, если извлечь свою
    // иконку не удалось (например, `current_exe()` недоступен); всегда
    // доступна как системная иконка.
    // SAFETY: IDI_APPLICATION — системная стоковая иконка, всегда доступна.
    let fallback = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: app_icon().unwrap_or(fallback),
        ..Default::default()
    };
    let wide: Vec<u16> = tooltip.encode_utf16().collect();
    let len = wide.len().min(data.szTip.len() - 1);
    data.szTip[..len].copy_from_slice(&wide[..len]);
    data
}

fn add_notify_icon(hwnd: HWND, tooltip: &str) -> Result<(), Win32Error> {
    let data = notify_icon_data(hwnd, tooltip);
    // SAFETY: data полностью инициализирована выше, hwnd действительно.
    if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        Ok(())
    } else {
        Err(Win32Error::TrayNotifyIconFailed)
    }
}

fn remove_notify_icon(hwnd: HWND) -> Result<(), Win32Error> {
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    // SAFETY: data полностью инициализирована выше.
    if unsafe { Shell_NotifyIconW(NIM_DELETE, &data) }.as_bool() {
        Ok(())
    } else {
        Err(Win32Error::TrayNotifyIconFailed)
    }
}

fn show_context_menu(hwnd: HWND) {
    // SAFETY: hwnd было создано этим же потоком (GWLP_USERDATA принадлежит ему).
    let state_ptr = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut WndState;
    if state_ptr.is_null() {
        return;
    }
    // Временно забираем состояние, чтобы получить список пунктов, и сразу
    // возвращаем указатель обратно — единственный владелец не меняется.
    let state = unsafe { &*state_ptr };
    // SAFETY: state_ptr не null и был получен из Box::into_raw в этом потоке.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
    }

    // SAFETY: CreatePopupMenu без аргументов; ошибка (пустой HMENU) обрабатывается ниже.
    let Ok(hmenu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    for item in &state.menu {
        let wide: Vec<u16> = item
            .label
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: hmenu только что создано; wide — валидная nul-terminated строка,
        // живущая до конца вызова AppendMenuW.
        unsafe {
            if item.id == 0 {
                let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let _ = AppendMenuW(hmenu, MF_STRING, item.id as usize, PCWSTR(wide.as_ptr()));
            }
        }
    }

    let mut pt = Default::default();
    // SAFETY: pt — валидный указатель на стековую POINT.
    unsafe {
        let _ = GetCursorPos(&mut pt);
        // Требуется Win32-приёмом, чтобы меню корректно закрывалось по клику мимо.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            hmenu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(
            Some(hwnd),
            windows::Win32::UI::WindowsAndMessaging::WM_NULL,
            WPARAM(0),
            LPARAM(0),
        );
        let _ = DestroyMenu(hmenu);
    }
}

fn with_state<F: FnOnce(&WndState)>(hwnd: HWND, f: F) {
    // SAFETY: указатель либо null (сообщение пришло до установки состояния),
    // либо владеющий указатель этого потока — не освобождается здесь.
    let ptr = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut WndState;
    if ptr.is_null() {
        return;
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        f(&*ptr);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = lparam.0 as u32;
            match event {
                WM_LBUTTONUP => with_state(hwnd, |s| {
                    let _ = s.tx.send(TrayEvent::Activate);
                }),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_context_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u32;
            with_state(hwnd, |s| {
                let _ = s.tx.send(TrayEvent::MenuItem(id));
            });
            LRESULT(0)
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

    #[test]
    fn create_then_drop_destroys_window() {
        let (tray, _rx) = TrayIcon::new("test", vec![]).expect("создание трея");
        let hwnd = tray.hwnd;
        // SAFETY: hwnd — живое окно трея, создание выше проверено.
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());

        drop(tray);

        // Если бы Drop звал DestroyWindow напрямую (чужой поток), эта
        // проверка не выполнилась бы никогда: t.join() зависал бы навсегда,
        // и тест не дошёл бы до сюда — таймаут раньше, чем assert-провал.
        // SAFETY: после Drop окно уничтожено; IsWindow над мёртвым хэндлом —
        // простое чтение, хэндл мы больше никуда не передаём.
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
    }
}
