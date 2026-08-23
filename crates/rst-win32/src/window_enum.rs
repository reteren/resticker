//! Перечисление top-level окон с фильтром «реальных» окон (ROADMAP.md M4;
//! docs/M4_PREP_NOTES.md, §2; ARCHITECTURE.md, раздел 3.3).
//!
//! Наружу — платформенно-чистые данные (как `input::InputEvent` в M2):
//! `HWND` присутствует только как числовой ключ для кэша трекера, никаких
//! Win32-типов в публичном API. Инкрементальный кэш на WinEvent-хуках —
//! отдельный модуль (`WindowTracker`, M4_PREP_NOTES §3), здесь только
//! одноразовое перечисление.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETTEXT, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LWIN, VK_MENU, VK_RWIN,
};
use windows::core::{BOOL, PWSTR};

use crate::window_icon;

/// Таймаут кросс-поточного чтения заголовка (мс), [`window_text`]. Верх
/// диапазона 200–500 мс: `SMTO_ABORTIFHUNG` и так возвращает сразу на
/// зависших потоках, таймаут ограничивает только «живые, но медленные»
/// окна — им 500 мс хватает вернуть заголовок (пустой заголовок деградирует
/// панель выбора M4, а `title_pattern` с ним просто не совпадает).
const TITLE_FETCH_TIMEOUT_MS: u32 = 500;

/// Прямоугольник окна в **физических** пикселях (маска перекрытия живёт в
/// физических — M4_PREP_NOTES §2.1; перевод в DIP — на стороне координатора).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl From<RECT> for WindowRect {
    fn from(r: RECT) -> Self {
        Self {
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
        }
    }
}

/// Иконка окна для панели выбора (M4_PREP_NOTES §7): RGBA-пиксели.
/// Заполняется отдельным срезом (`ExtractIconExW`/`SHGetFileInfoW`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Одно «реальное» top-level окно из перечисления [`enum_windows`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowInfo {
    /// `HWND` как числовой ключ (для кэша трекера M4); для Win32-вызовов
    /// снаружи не предназначен — окно может уже не существовать.
    pub hwnd: usize,
    /// Границы по `DWMWA_EXTENDED_FRAME_BOUNDS` (то, что видит DWM, — ровно
    /// то, что вырезает маска). У свёрнутых окон — мусорные, не использовать.
    pub rect: WindowRect,
    /// Процесс-владелец окна.
    pub pid: u32,
    /// Путь к exe процесса (пустой при недостатке прав на `OpenProcess` —
    /// окно всё равно перечисляется). Для сопоставления с
    /// `OverlapRule::process_name`.
    pub exe_path: PathBuf,
    /// Заголовок окна (для `title_pattern` и панели выбора).
    pub title: String,
    /// Класс окна (для панели и будущих расширений правил).
    pub class: String,
    /// Позиция в «сыром» перечислении (z-order сверху вниз): монотонно
    /// возрастает, пропуски — отфильтрованные окна.
    pub z_order: u32,
    /// Окно свёрнуто: не оклюдер (прямоугольник мусорный), но показывается
    /// в панели выбора (M4_PREP_NOTES §2.2).
    pub iconic: bool,
    /// Место под иконку окна (панель выбора M4): заполняется [`window_icon`]
    /// при перечислении; `None` — у окна нет exe-пути (protected process),
    /// извлечение не удалось или иконки нет (панель рисует плейсхолдер).
    pub icon: Option<WindowIcon>,
}

/// Кэш иконок по пути exe (M4, docs/M4_WINDOW_PICKER_DESIGN.md §6): окна
/// одного процесса делят одну иконку, а `SHGetFileInfoW` на первом
/// обращении к файлу стоит единицы миллисекунд — и то, и другое решается
/// «вытащить один раз на уникальный exe и хранить». Кэшируется и неудача
/// (`None`): exe мог быть удалён/стать недоступным — повторный пробой на
/// каждое перечисление не нужен. Процесс-wide статик: трекер окон (и его
/// полные перечисления) живёт на своём потоке, а иконки не зависят ни от
/// времени, ни от окна — вытеснение не требуется (число уникальных exe за
/// сессию — десятки, растр 16×16 ≈ 1 КБ).
static ICON_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<WindowIcon>>>> = OnceLock::new();

/// Иконка процесса по пути exe: кэш, иначе извлечение + запись в кэш
/// (успех и неудача одинаково). Пустой путь (protected process, сбой
/// `OpenProcess`) — `None` без обращения к кэшу.
fn window_icon(exe_path: &Path) -> Option<WindowIcon> {
    if exe_path.as_os_str().is_empty() {
        return None;
    }
    let cache = ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        // Отравленный мьютекс (паника во время извлечения) — продолжаем
        // работать со старым содержимым, иконки не критичны.
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(icon) = cache.get(exe_path) {
        return icon.clone();
    }
    let icon = window_icon::extract_icon(exe_path);
    cache.insert(exe_path.to_path_buf(), icon.clone());
    icon
}

/// Снимок признаков окна для чистого фильтра (собирается Win32-вызовами
/// в [`collect_window`]; отдельно — чтобы фильтр тестировать без окон).
#[derive(Debug, Clone, Copy, Default)]
struct WindowFlags {
    visible: bool,
    cloaked: bool,
    /// `GetAncestor(hwnd, GA_ROOT) == hwnd` (top-level, не child).
    is_root: bool,
    no_activate: bool,
    tool_window: bool,
    app_window: bool,
    has_owner: bool,
    iconic: bool,
}

/// Фильтр «реальных окон» дословно по ARCHITECTURE.md 3.3 и
/// M4_PREP_NOTES §2.2. `iconic` отбраковкой не является — лишь отметка.
fn is_real_window(f: &WindowFlags) -> bool {
    if !f.visible {
        return false;
    }
    if f.cloaked {
        return false;
    }
    if !f.is_root {
        return false;
    }
    if f.no_activate {
        return false;
    }
    if f.tool_window && !f.app_window {
        return false;
    }
    if f.has_owner && !f.app_window {
        return false;
    }
    true
}

/// Контекст колбэка `EnumWindows`: `raw_index` считает **все** окна из
/// сырого перечисления (даже отфильтрованные) — так `WindowInfo::z_order`
/// остаётся монотонным с пропусками, как задокументировано на поле.
struct EnumCtx {
    out: Vec<WindowInfo>,
    raw_index: u32,
}

/// Перечислить все «реальные» top-level окна одним снимком (одноразовое
/// перечисление; инкрементальный кэш на WinEvent-хуках — отдельный модуль,
/// M4_PREP_NOTES §3).
pub fn enumerate() -> Vec<WindowInfo> {
    let mut ctx = EnumCtx {
        out: Vec::new(),
        raw_index: 0,
    };
    // SAFETY: `ctx` живёт весь вызов и не разделяется; колбэк — синхронный,
    // на этом же потоке, указатель действует только внутри EnumWindows.
    unsafe {
        let _ = EnumWindows(Some(enum_windows_proc), LPARAM(&raw mut ctx as isize));
    }
    ctx.out
}

extern "system" fn enum_windows_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` — &mut EnumCtx из `enumerate`, живой на всё время вызова
    // EnumWindows; колбэк синхронный, гонок нет.
    let ctx = unsafe { &mut *(data.0 as *mut EnumCtx) };
    let z_order = ctx.raw_index;
    ctx.raw_index += 1;
    if let Some(info) = collect_window(hwnd, z_order) {
        ctx.out.push(info);
    }
    TRUE
}

/// Собрать [`WindowInfo`] для `hwnd`, если оно проходит фильтр
/// [`is_real_window`]; иначе `None`.
fn collect_window(hwnd: HWND, z_order: u32) -> Option<WindowInfo> {
    let flags = window_flags(hwnd);
    if !is_real_window(&flags) {
        // Диагностика на живой репорт пользователя («список окон в панели
        // короче, чем реально открытых окон») — почему именно окно
        // отфильтровано, без похода на диск/сеть: title/class дёшевы, уже
        // читаются ниже для принятых окон, здесь читаем только при отказе.
        tracing::debug!(
            hwnd = hwnd.0 as usize,
            title = %window_text(hwnd),
            class = %window_class(hwnd),
            ?flags,
            "is_real_window отбраковал окно"
        );
        return None;
    }
    let (pid, exe_path) = process_info(hwnd);
    let icon = window_icon(&exe_path);
    Some(WindowInfo {
        hwnd: hwnd.0 as usize,
        rect: extended_frame_bounds(hwnd),
        pid,
        exe_path,
        title: window_text(hwnd),
        class: window_class(hwnd),
        z_order,
        iconic: flags.iconic,
        icon,
    })
}

/// Собрать [`WindowFlags`] Win32-вызовами (ARCHITECTURE.md 3.3, M4_PREP_NOTES §2.2).
fn window_flags(hwnd: HWND) -> WindowFlags {
    // SAFETY: hwnd приходит из EnumWindows, действительно на время колбэка.
    unsafe {
        let visible = IsWindowVisible(hwnd).as_bool();
        let iconic = IsIconic(hwnd).as_bool();
        let is_root = GetAncestor(hwnd, GA_ROOT) == hwnd;
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let no_activate = ex_style & WS_EX_NOACTIVATE.0 != 0;
        let tool_window = ex_style & WS_EX_TOOLWINDOW.0 != 0;
        let app_window = ex_style & WS_EX_APPWINDOW.0 != 0;
        let has_owner = GetWindow(hwnd, GW_OWNER)
            .map(|owner| !owner.0.is_null())
            .unwrap_or(false);
        let mut cloaked: u32 = 0;
        let cloaked_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
        .is_ok();
        WindowFlags {
            visible,
            cloaked: cloaked_ok && cloaked != 0,
            is_root,
            no_activate,
            tool_window,
            app_window,
            has_owner,
            iconic,
        }
    }
}

/// Границы окна по DWM (то, что реально рисует композитор — ровно то, что
/// вырежет маска перекрытия, M4_PREP_NOTES §2.1). При отказе API — нулевой
/// прямоугольник (свёрнутые окна и так помечены `iconic`, не оклюдеры).
/// `pub(crate)`: переиспользуется `window_tracker` для точечного обновления
/// rect одного окна на `EVENT_OBJECT_LOCATIONCHANGE`/`MINIMIZEEND`, не через
/// полное `enumerate()` (M4_WINDOW_TRACKER_DESIGN.md §4).
/// Классы окон, которые шелл показывает НА ВРЕМЯ переключения или своего
/// меню: переключатель Alt+Tab и Win+Tab, меню Пуск, поиск, панель задач.
///
/// Пока такое окно на переднем плане, «активного приложения» фактически нет:
/// пользователь ещё выбирает. Любое вмешательство в чужие окна в этот момент
/// ломает сам переключатель — см. [`shell_switching`].
const SHELL_TRANSIENT_CLASSES: [&str; 8] = [
    "MultitaskingViewFrame",         // Win10 Task View / Alt+Tab
    "XamlExplorerHostIslandWindow",  // Win11 Alt+Tab и Win+Tab
    "TaskSwitcherWnd",               // классический Alt+Tab
    "TaskSwitcherOverlayWnd",        // его оверлей
    "ForegroundStaging",             // промежуточное окно переключения
    "Windows.UI.Core.CoreWindow",    // меню Пуск, поиск
    "Shell_TrayWnd",                 // панель задач
    "Shell_SecondaryTrayWnd",        // панель задач на втором мониторе
];

/// Пользователь ПРЯМО СЕЙЧАС переключается между окнами средствами шелла
/// (Alt+Tab, Win+Tab, меню Пуск, клик по панели задач).
///
/// Зачем: правила «показывать только на этих окнах» решают судьбу
/// закреплённого окна по активному окну и при неподходящем активном окне
/// сворачивают его. Во время Alt+Tab активным становится сам переключатель,
/// и наивная логика немедленно сворачивала/разворачивала окно прямо под
/// рукой пользователя — переключатель ломался, Alt+Tab переставал работать
/// до перехода в другое приложение через панель задач (критический репорт
/// пользователя 2026-08-22).
///
/// Два независимых признака, любой достаточен:
/// * зажат Alt или Win — то есть комбинация переключения ещё удерживается;
/// * переднее окно принадлежит шеллу ([`SHELL_TRANSIENT_CLASSES`]).
///
/// Пока это верно, координатор обязан НИЧЕГО не делать с чужими окнами:
/// решение примется само, когда пользователь отпустит клавиши и шелл отдаст
/// передний план настоящему окну.
pub fn shell_switching() -> bool {
    // SAFETY: GetAsyncKeyState — потокобезопасное чтение состояния ввода.
    let keys_held = unsafe {
        GetAsyncKeyState(VK_MENU.0 as i32) < 0
            || GetAsyncKeyState(VK_LWIN.0 as i32) < 0
            || GetAsyncKeyState(VK_RWIN.0 as i32) < 0
    };
    if keys_held {
        return true;
    }
    // SAFETY: GetForegroundWindow — чтение состояния десктопа.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return false;
    }
    let class = window_class(fg);
    SHELL_TRANSIENT_CLASSES
        .iter()
        .any(|known| class.eq_ignore_ascii_case(known))
}

/// Прямоугольник окна ПРЯМО СЕЙЧАС, мимо кэша трекера (те же
/// DWM-координаты `DWMWA_EXTENDED_FRAME_BOUNDS`, что у [`WindowInfo::rect`]).
///
/// Зачем при живом трекере: снимок трекера дебаунсится (16 мс) и приходит
/// после полного перечисления, поэтому во время ПЕРЕТАСКИВАНИЯ окна
/// пользователем он отстаёт — нарисованная по нему рамка/бейдж/панель
/// «отлетают» от окна (репорт 2026-08-21). Для нескольких закреплённых окон
/// прямой опрос DWM стоит единицы микросекунд на окно и снимает отставание.
///
/// `None` — окна нет, оно скрыто, свёрнуто или DWM не отдал границы:
/// вызывающий в этом случае честно откатывается к снимку.
pub fn live_rect(hwnd: usize) -> Option<WindowRect> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: все три предиката безопасны для чужих и мёртвых хэндлов.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool()
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
        {
            return None;
        }
    }
    let rect = extended_frame_bounds(hwnd);
    (rect.w != 0 && rect.h != 0).then_some(rect)
}

pub(crate) fn extended_frame_bounds(hwnd: HWND) -> WindowRect {
    let mut rect = RECT::default();
    // SAFETY: `rect` — валидный буфер под RECT, hwnd — из EnumWindows.
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if ok {
        rect.into()
    } else {
        WindowRect::default()
    }
}

/// HWND окна переднего плана (`GetForegroundWindow`) как числовой ключ в
/// том же формате, что [`WindowInfo::hwnd`] — для хоткей-пина
/// «закрепить/открепить сфокусированное окно» (SPEC.md, «Закрепление
/// окна»). `None` — фокуса нет вовсе (редко: между переключениями, пустой
/// десктоп) — пинить нечего.
pub fn foreground_hwnd() -> Option<usize> {
    // SAFETY: GetForegroundWindow — чистый запрос состояния десктопа,
    // состояния не меняет, безопасен с любого потока.
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.0.is_null()).then_some(hwnd.0 as usize)
}

/// Монитор, на котором лежит окно (`MonitorFromWindow`), как числовой
/// ключ — сравнивать мониторы двух окон можно, ничего не зная о геометрии.
///
/// `None` — окно ни на одном мониторе: свёрнутое окно живёт в координатах
/// вроде (-32000, -32000), и `MONITOR_DEFAULTTONULL` честно отвечает
/// «нигде» вместо того, чтобы приписать его ближайшему экрану. Вызывающему
/// это и нужно: «монитор неизвестен» — не то же самое, что «монитор тот
/// же».
pub fn monitor_of(hwnd: usize) -> Option<isize> {
    // SAFETY: чистый запрос состояния десктопа для чужого HWND; невалидный
    // или свёрнутый дескриптор даёт нулевой HMONITOR, а не UB.
    let monitor = unsafe {
        MonitorFromWindow(
            HWND(hwnd as *mut core::ffi::c_void),
            MONITOR_DEFAULTTONULL,
        )
    };
    (!monitor.0.is_null()).then_some(monitor.0 as isize)
}

/// Заголовок окна. Пустая строка — нет заголовка, сбой API или таймаут
/// (не критично, окно остаётся в списке).
///
/// Заголовок чужого процесса читается кросс-поточным `WM_GETTEXT` (как и
/// `GetWindowTextW` под капотом): без ограничения зависшее окно держит
/// вызов до системного таймаута (~5 с), а это перечисление стоит на пути
/// маски M4 и цикла координатора (сообщения обрабатываются
/// последовательно, ADR-006). `SendMessageTimeoutW` с `SMTO_ABORTIFHUNG` и
/// [`TITLE_FETCH_TIMEOUT_MS`] обрывают зависшие потоки сразу, а живые — не
/// дольше таймаута.
fn window_text(hwnd: HWND) -> String {
    // Один обход с фиксированным буфером вместо пары GetWindowTextLengthW +
    // GetWindowTextW (два кросс-поточных обхода при зависании в два раза
    // дольше); размер буфера — как у exe-пути в `process_info` (1024).
    let mut buf = [0u16; 1024];
    let mut copied: usize = 0;
    // SAFETY: hwnd — из EnumWindows, действительно на время вызова; buf —
    // буфер достаточного размера (WM_GETTEXT копирует максимум len-1
    // символов и нуль-терминатор); copied — валидный out-параметр.
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETTEXT,
            WPARAM(buf.len()),
            LPARAM(buf.as_mut_ptr() as isize),
            SMTO_ABORTIFHUNG,
            TITLE_FETCH_TIMEOUT_MS,
            Some(&mut copied),
        )
    };
    if result.0 == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied.min(buf.len())])
}

/// Класс окна (для панели выбора и будущих правил перекрытия).
fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: hwnd — из EnumWindows; buf — буфер фиксированного размера.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// PID окна и путь к его exe. Путь пуст, если `OpenProcess` отказал
/// (недостаточно прав — например, защищённый процесс) — окно всё равно
/// перечисляется (M4_PREP_NOTES §2.2).
fn process_info(hwnd: HWND) -> (u32, PathBuf) {
    let mut pid: u32 = 0;
    // SAFETY: hwnd — из EnumWindows; pid — валидный out-параметр.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return (0, PathBuf::new());
    }
    // SAFETY: pid — только что полученный от системы; хэндл процесса
    // закрывается ниже в любом случае (в т.ч. при ошибке — CloseHandle
    // безопасен для валидного хэндла).
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    let Ok(process) = process else {
        return (pid, PathBuf::new());
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: process — только что открытый хэндл; buf/len — валидный
    // выходной буфер и его размер.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    // SAFETY: process — валидный хэндл, открытый выше этим же вызовом.
    unsafe {
        let _ = CloseHandle(process);
    }
    let path = if ok {
        PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        PathBuf::new()
    };
    (pid, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};
    use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, LRESULT};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW,
        WS_OVERLAPPED,
    };
    use windows::core::w;

    /// HWND не `Send` (сырой указатель) — обёртка для пересылки между
    /// потоками, как в window_tracker.rs/tray.rs/overlay.rs.
    struct SendHwnd(HWND);

    unsafe impl Send for SendHwnd {}

    unsafe extern "system" fn test_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Окно, чей поток НЕ пампит сообщения — в отличие от `RealWindow` в
    /// window_tracker.rs (ему цикл обязателен, чтобы хуки его увидели).
    /// Снаружи такое окно выглядит «зависшим», и кросс-поточное чтение
    /// заголовка не должно блокироваться на нём.
    struct HungWindow {
        hwnd: HWND,
        thread: Option<JoinHandle<()>>,
        kill: Option<mpsc::Sender<()>>,
    }

    impl HungWindow {
        fn create() -> Self {
            let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
            let (kill_tx, kill_rx) = mpsc::channel::<()>();
            let thread = thread::spawn(move || {
                // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
                let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(test_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: w!("resticker_window_enum_test"),
                    ..Default::default()
                };
                // SAFETY: wc заполнена корректно; повторная регистрация
                // (параллельные тесты) — не ошибка.
                if unsafe { RegisterClassExW(&wc) } == 0 {
                    let err = unsafe { GetLastError() };
                    assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
                }
                // SAFETY: все аргументы — валидные константы/только что
                // зарегистрированный класс; видимость для window_text не важна.
                let hwnd = unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("resticker_window_enum_test"),
                        w!("resticker window_enum test"),
                        WS_OVERLAPPED,
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
                ready_tx.send(SendHwnd(hwnd)).expect("получатель ещё жив");
                // Намеренно НЕ пампим: поток блокируется, пока тест не
                // попросит завершиться.
                let _ = kill_rx.recv();
                // SAFETY: hwnd создано этим потоком; DestroyWindow на своём
                // потоке — корректный способ закрыть окно без цикла сообщений.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            });
            let hwnd = ready_rx.recv().expect("поток тестового окна не упал").0;
            Self {
                hwnd,
                thread: Some(thread),
                kill: Some(kill_tx),
            }
        }
    }

    impl Drop for HungWindow {
        fn drop(&mut self) {
            if let Some(k) = self.kill.take() {
                let _ = k.send(());
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn flags(overrides: impl Fn(&mut WindowFlags)) -> WindowFlags {
        let mut f = WindowFlags {
            visible: true,
            is_root: true,
            ..Default::default()
        };
        overrides(&mut f);
        f
    }

    #[test]
    fn plain_visible_root_window_is_real() {
        assert!(is_real_window(&flags(|_| {})));
    }

    #[test]
    fn invisible_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.visible = false)));
    }

    #[test]
    fn cloaked_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.cloaked = true)));
    }

    #[test]
    fn child_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.is_root = false)));
    }

    #[test]
    fn no_activate_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.no_activate = true)));
    }

    #[test]
    fn tool_window_without_app_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.tool_window = true)));
    }

    #[test]
    fn tool_window_with_app_window_override_is_kept() {
        assert!(is_real_window(&flags(|f| {
            f.tool_window = true;
            f.app_window = true;
        })));
    }

    #[test]
    fn owned_window_without_app_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.has_owner = true)));
    }

    #[test]
    fn owned_window_with_app_window_override_is_kept() {
        assert!(is_real_window(&flags(|f| {
            f.has_owner = true;
            f.app_window = true;
        })));
    }

    #[test]
    fn iconic_is_not_a_rejection() {
        assert!(is_real_window(&flags(|f| f.iconic = true)));
    }

    #[test]
    fn window_rect_from_rect_computes_size() {
        let rc = RECT {
            left: 10,
            top: 20,
            right: 110,
            bottom: 220,
        };
        let wr: WindowRect = rc.into();
        assert_eq!(
            wr,
            WindowRect {
                x: 10,
                y: 20,
                w: 100,
                h: 200
            }
        );
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn hung_window_title_fetch_is_bounded() {
        let win = HungWindow::create();
        let started = std::time::Instant::now();
        let title = window_text(win.hwnd);
        let elapsed = started.elapsed();
        // Зависшее окно не должно держать чтение заголовка дольше таймаута
        // (500 мс); старый GetWindowTextW висел бы ~5 с на системном
        // таймауте SendMessage. 3 с — запас на планировщик, но на порядок
        // меньше системного умолчания.
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "чтение заголовка зависшего окна заняло {elapsed:?}"
        );
        assert!(title.is_empty(), "зависшее окно не должно иметь заголовка");
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn enumerate_returns_only_real_windows() {
        let windows = enumerate();
        assert!(
            !windows.is_empty(),
            "хотя бы одно окно на реальном десктопе"
        );
        for w in &windows {
            assert!(w.hwnd != 0, "нулевой hwnd: {w:?}");
        }
        let mut prev = None;
        for w in &windows {
            if let Some(p) = prev {
                assert!(w.z_order > p, "z_order не монотонен: {w:?}");
            }
            prev = Some(w.z_order);
        }
    }

    /// Иконки окон (M4 §6) на реальном десктопе: хотя бы у одного окна
    /// иконка извлечена (у окна с exe-путём шелл обязан её отдать), растр
    /// квадратный и полный (`rgba.len() == w*h*4`). Окна без exe-пути
    /// (protected process) остаются `None` — это не ошибка.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn enumerate_populates_icons_for_real_windows() {
        let windows = enumerate();
        let mut with_icon = 0;
        for w in &windows {
            if let Some(icon) = &w.icon {
                with_icon += 1;
                assert_eq!(
                    icon.rgba.len(),
                    icon.width as usize * icon.height as usize * 4,
                    "растр иконки полный: {w:?}"
                );
                assert!(icon.width > 0 && icon.height > 0, "размер иконки: {w:?}");
            }
            if w.exe_path.as_os_str().is_empty() {
                assert!(
                    w.icon.is_none(),
                    "окно без exe-пути не может иметь иконку: {w:?}"
                );
            }
        }
        assert!(
            with_icon > 0,
            "хотя бы одно окно с иконкой на реальном десктопе"
        );
    }
}
