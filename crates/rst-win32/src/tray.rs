//! Иконка в трее: скрытое окно + `Shell_NotifyIconW`, на собственном потоке
//! со своим циклом сообщений (ADR-013 — тот же паттерн, что у оверлея).
//! Весь unsafe живёт здесь, наружу — только каналы и безопасные типы.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ExtractIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, HICON, HMENU,
    IDI_APPLICATION, LoadIconW, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::error::Win32Error;

const WM_TRAYICON: u32 = WM_APP + 1;
const CLASS_NAME: PCWSTR = w!("resticker_tray");

/// `uID` иконки трея — единый для `NIM_ADD` (создание), `NIM_DELETE`
/// (снятие) и `NIM_MODIFY` (баллон [`TrayIcon::show_balloon`]): `NIF_INFO`
/// обновляет только поля уведомления УЖЕ существующей иконки, hwnd/uID
/// обязаны совпадать с созданными в `notify_icon_data`.
const TRAY_UID: u32 = 1;

/// Максимум заголовка баллона, UTF-16 code units (Windows, `szInfoTitle`
/// вмещает 64 с учётом завершающего NUL — значимых 63).
pub const BALLOON_TITLE_MAX_UNITS: usize = 63;

/// Максимум тела баллона, UTF-16 code units (Windows, `szInfo` вмещает 256
/// с учётом завершающего NUL — значимых 255).
pub const BALLOON_BODY_MAX_UNITS: usize = 255;

/// Один пункт контекстного меню трея; `id` возвращается в `TrayEvent::MenuItem`.
/// Непустой `children` рисует пункт как вложенное подменю ([`MenuItem::submenu`],
/// SPEC.md §12 — «пресеты (подменю)»), а не кликабельный пункт: `id`
/// подменю в `TrayEvent` никогда не приходит. Иначе `id == 0` рисуется как
/// разделитель (см. [`separator`]).
#[derive(Clone)]
pub struct MenuItem {
    pub id: u32,
    pub label: String,
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    /// Обычный кликабельный пункт.
    pub fn new(id: u32, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            children: Vec::new(),
        }
    }

    /// Вложенное подменю: `label` — заголовок, `children` — его пункты.
    /// Само не кликабельно (`id` — не участвует в `TrayEvent::MenuItem`).
    pub fn submenu(label: impl Into<String>, children: Vec<MenuItem>) -> Self {
        Self {
            id: 0,
            label: label.into(),
            children,
        }
    }
}

/// Разделитель между пунктами меню.
pub fn separator() -> MenuItem {
    MenuItem {
        id: 0,
        label: String::new(),
        children: Vec::new(),
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
    menu: Arc<Mutex<Vec<MenuItem>>>,
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
    menu: Arc<Mutex<Vec<MenuItem>>>,
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
        let menu = Arc::new(Mutex::new(menu));
        let menu_for_thread = Arc::clone(&menu);

        let thread =
            thread::spawn(move || run_message_loop(tooltip, menu_for_thread, tx, ready_tx));

        let hwnd = ready_rx
            .recv()
            .map_err(|_| Win32Error::TrayThreadCrashed)??
            .0;

        Ok((
            Self {
                hwnd,
                thread: Some(thread),
                menu,
            },
            rx,
        ))
    }

    /// Заменить пункты контекстного меню целиком. Берёт эффект со
    /// следующего открытия меню ([`show_context_menu`] читает
    /// `WndState.menu` заново при каждом клике правой кнопкой) — сам список
    /// живёт в `Arc<Mutex<..>>`, общем с потоком трея, поэтому обновление
    /// безопасно с любого потока и не требует пересоздания иконки/окна.
    pub fn set_menu(&self, items: Vec<MenuItem>) {
        *self.menu.lock().unwrap_or_else(|e| e.into_inner()) = items;
    }

    /// Показать баллонное уведомление от иконки трея (Windows
    /// balloon-notification, `NOTIFYICONDATAW` с `NIF_INFO` →
    /// `Shell_NotifyIconW(NIM_MODIFY, …)`) — на той же `hwnd`/`uID`, что
    /// создала `NIM_ADD`, поэтому работает только при живом трее
    /// (`Drop` после этой ошибки снимает иконку).
    ///
    /// `title`/`body` — UTF-8, внутри усекаются до лимитов Windows
    /// (63/255 UTF-16 code units) корректно по границе символа (суррогатные
    /// пары не разрываются, [`truncate_utf16_nul`]). `dwInfoFlags` —
    /// `NIIF_WARNING` (предупреждение). Вызов можно делать с любого потока —
    /// `Shell_NotifyIconW` не требует потока окна.
    pub fn show_balloon(&self, title: &str, body: &str) -> Result<(), Win32Error> {
        let title_w = truncate_utf16_nul(title, BALLOON_TITLE_MAX_UNITS);
        let body_w = truncate_utf16_nul(body, BALLOON_BODY_MAX_UNITS);
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            // Только NIF_INFO: NIM_MODIFY обновляет поля уведомления, всё
            // остальное (иконка, тултип) остаётся от NIM_ADD.
            uFlags: NIF_INFO,
            dwInfoFlags: NIIF_WARNING,
            ..Default::default()
        };
        data.szInfoTitle[..title_w.len()].copy_from_slice(&title_w);
        data.szInfo[..body_w.len()].copy_from_slice(&body_w);
        // SAFETY: data полностью инициализирована выше; hwnd/uID — живого
        // трея (пока TrayIcon жив), NIM_MODIFY на чужой/снятый hwnd просто
        // вернёт FALSE, никакого UB.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            Ok(())
        } else {
            Err(Win32Error::TrayNotifyIconFailed)
        }
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
    menu: Arc<Mutex<Vec<MenuItem>>>,
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
        uID: TRAY_UID,
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

/// Усечь строку до `max_units` UTF-16 code units и завершить NUL — для
/// `szInfoTitle`/`szInfo` баллона (`NIF_INFO`). Лимиты Windows заданы в
/// code units, а не в символах: суррогатная пара эмодзи — 2 units. Усечение
/// идёт по границе символа: если последний взятый unit — верхний (старший)
/// суррогат разорванной пары, он отбрасывается (влезает весь символ или
/// ни одного). Результат — ровно `min(len, max_units)` (минус разорванная
/// пара) единиц + NUL: помещается в `[u16; max_units + 1]`.
fn truncate_utf16_nul(text: &str, max_units: usize) -> Vec<u16> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out: Vec<u16> = units.iter().copied().take(max_units).collect();
    // Высокий суррогат (0xD800..=0xDBFF) последним — пара разорвана на
    // границе усечения: выкидываем старшую половину, низкий суррогат не
    // влез (или текст целиком валиден, и тогда высокий суррогат последним
    // не бывает — непарных суррогатов в валидном UTF-16 нет).
    if out.last().is_some_and(|&u| (0xD800..=0xDBFF).contains(&u)) {
        out.pop();
    }
    out.push(0);
    out
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
        uID: TRAY_UID,
        ..Default::default()
    };
    // SAFETY: data полностью инициализирована выше.
    if unsafe { Shell_NotifyIconW(NIM_DELETE, &data) }.as_bool() {
        Ok(())
    } else {
        Err(Win32Error::TrayNotifyIconFailed)
    }
}

/// Собрать `HMENU` из списка пунктов, рекурсивно (вложенные `children` →
/// `MF_POPUP`-подпункты, [`MenuItem::submenu`]). `DestroyMenu` на корневом
/// `HMENU` уничтожает и все вложенные подменю — Win32 делает это сам
/// (MSDN: `DestroyMenu` "also destroys any submenus"), поэтому вызывающий
/// код освобождает только корень.
fn build_hmenu(items: &[MenuItem]) -> Option<HMENU> {
    // SAFETY: CreatePopupMenu без аргументов; ошибка (пустой HMENU) обрабатывается ниже.
    let hmenu = unsafe { CreatePopupMenu() }.ok()?;
    for item in items {
        let wide: Vec<u16> = item
            .label
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: hmenu только что создано; wide — валидная nul-terminated строка,
        // живущая до конца вызова AppendMenuW.
        unsafe {
            if !item.children.is_empty() {
                let Some(submenu) = build_hmenu(&item.children) else {
                    continue;
                };
                let _ = AppendMenuW(hmenu, MF_POPUP, submenu.0 as usize, PCWSTR(wide.as_ptr()));
            } else if item.id == 0 {
                let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let _ = AppendMenuW(hmenu, MF_STRING, item.id as usize, PCWSTR(wide.as_ptr()));
            }
        }
    }
    Some(hmenu)
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

    let items = state.menu.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(hmenu) = build_hmenu(&items) else {
        return;
    };

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

    // --- чистая функция усечения (без Win32) ---

    fn as_utf16(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    #[test]
    fn truncate_keeps_short_string_nul_terminated() {
        assert_eq!(truncate_utf16_nul("abc", 63), as_utf16("abc"));
        assert_eq!(truncate_utf16_nul("", 255), vec![0]);
    }

    #[test]
    fn truncate_exact_limit_keeps_everything() {
        let s = "x".repeat(63);
        let out = truncate_utf16_nul(&s, BALLOON_TITLE_MAX_UNITS);
        assert_eq!(out.len(), 63 + 1);
        assert_eq!(&out[..63], &as_utf16(&s)[..63]);
    }

    #[test]
    fn truncate_cuts_at_unit_limit() {
        // Кириллица — 1 code unit на символ: 70 символов режутся до 63.
        let s = "я".repeat(70);
        let out = truncate_utf16_nul(&s, BALLOON_TITLE_MAX_UNITS);
        assert_eq!(out.len(), 63 + 1);
        assert!(out[..63].iter().all(|&u| u == 'я' as u16));
        assert_eq!(out[63], 0);
    }

    #[test]
    fn truncate_body_limit_is_255() {
        let s = "abc".repeat(100); // 300 единиц
        let out = truncate_utf16_nul(&s, BALLOON_BODY_MAX_UNITS);
        assert_eq!(out.len(), 255 + 1);
    }

    #[test]
    fn truncate_does_not_split_surrogate_pair() {
        // «😀» — суррогатная пара из 2 units. Лимит 3: «😀» (2) + старший
        // суррогат второй пары — разорванная пара отбрасывается целиком.
        let s = "😀😀";
        let out = truncate_utf16_nul(s, 3);
        assert_eq!(out, as_utf16("😀"));
        // Лимит ровно 2 — вся первая пара влезает.
        assert_eq!(truncate_utf16_nul(s, 2), as_utf16("😀"));
        // Лимит 4 — обе пары.
        assert_eq!(truncate_utf16_nul(s, 4), as_utf16("😀😀"));
    }

    #[test]
    fn truncate_zero_limit_yields_only_nul() {
        assert_eq!(truncate_utf16_nul("что-нибудь", 0), vec![0]);
    }

    #[test]
    fn truncate_mixed_cjk_and_emoji_keeps_char_boundary() {
        // CJK — 1 unit, эмодзи — 2. «漢漢漢😀» = 3 + 2 = 5 units.
        // Лимит 5: всё влезает, не режем.
        assert_eq!(truncate_utf16_nul("漢漢漢😀", 5), as_utf16("漢漢漢😀"));
        // Лимит 4: взяты 3 CJK + старший суррогат пары — пара разорвана,
        // суррогат отбрасывается, остаются только «漢漢漢».
        assert_eq!(truncate_utf16_nul("漢漢漢😀", 4), as_utf16("漢漢漢"));
    }

    #[test]
    fn truncate_surrogate_then_ascii_boundary() {
        // «A😀» — A (1 unit) + пара (2): лимит 2 берёт [A, старший суррогат],
        // пара разорвана на границе → старшая половина отбрасывается.
        assert_eq!(truncate_utf16_nul("A😀", 2), as_utf16("A"));
        // Лимит 3 — пара влезает целиком (A + обе половины), не режем.
        assert_eq!(truncate_utf16_nul("A😀", 3), as_utf16("A😀"));
        // «A😀B» при лимите 3: те же 3 units — «A😀», B не влез, пару не режем.
        assert_eq!(truncate_utf16_nul("A😀B", 3), as_utf16("A😀"));
    }

    // --- интеграция: реальный трей ---

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

    #[test]
    fn build_hmenu_with_submenu_succeeds_and_is_destroyable() {
        let items = vec![
            MenuItem::new(1, "Top"),
            separator(),
            MenuItem::submenu(
                "Presets",
                vec![MenuItem::new(10, "A"), MenuItem::new(11, "B")],
            ),
        ];
        let hmenu = build_hmenu(&items).expect("меню с подменю строится");
        // SAFETY: hmenu только что построено выше, ещё не показано/уничтожено.
        unsafe {
            let _ = DestroyMenu(hmenu);
        }
    }

    #[test]
    fn set_menu_replaces_items_seen_by_next_build() {
        let (tray, _rx) = TrayIcon::new("test", vec![MenuItem::new(1, "one")]).expect("трей");
        tray.set_menu(vec![MenuItem::new(2, "two"), MenuItem::new(3, "three")]);
        let items = tray.menu.lock().unwrap().clone();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 2);
        assert_eq!(items[1].id, 3);
        drop(tray);
    }

    #[test]
    fn show_balloon_succeeds_on_real_tray() {
        let (tray, _rx) = TrayIcon::new("test", vec![]).expect("создание трея");
        // NIM_MODIFY с NIF_INFO на живой иконке трея (hwnd/uID из NIM_ADD) —
        // Shell_NotifyIconW обязан вернуть TRUE; реальный показ баллона
        // зависит от shell, но ошибка API — нет. Длинный текст проверяет
        // усечение в реальном буфере (не вылез бы за [u16; 64]/[u16; 256]).
        tray.show_balloon(
            &"Очень длинный заголовок".repeat(10),
            &"Очень длинное тело уведомления с текстом".repeat(50),
        )
        .expect("Shell_NotifyIconW(NIM_MODIFY) не вернул ошибку");
        drop(tray);
    }
}
