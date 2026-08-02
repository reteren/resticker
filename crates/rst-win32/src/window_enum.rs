//! Перечисление top-level окон с фильтром «реальных» окон (ROADMAP.md M4;
//! docs/M4_PREP_NOTES.md, §2; ARCHITECTURE.md, раздел 3.3).
//!
//! Наружу — платформенно-чистые данные (как `input::InputEvent` в M2):
//! `HWND` присутствует только как числовой ключ для кэша трекера, никаких
//! Win32-типов в публичном API. Инкрементальный кэш на WinEvent-хуках —
//! отдельный модуль (`WindowTracker`, M4_PREP_NOTES §3), здесь только
//! одноразовое перечисление.

use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetWindow,
    GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

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
    /// Место под иконку окна (панель выбора M4): пока всегда `None`.
    pub icon: Option<WindowIcon>,
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
        return None;
    }
    let (pid, exe_path) = process_info(hwnd);
    Some(WindowInfo {
        hwnd: hwnd.0 as usize,
        rect: extended_frame_bounds(hwnd),
        pid,
        exe_path,
        title: window_text(hwnd),
        class: window_class(hwnd),
        z_order,
        iconic: flags.iconic,
        icon: None,
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
fn extended_frame_bounds(hwnd: HWND) -> WindowRect {
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

/// Заголовок окна. Пустая строка — нет заголовка или сбой API (не критично,
/// окно остаётся в списке).
fn window_text(hwnd: HWND) -> String {
    // SAFETY: hwnd — из EnumWindows, действительно на время вызова.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: buf — буфер достаточного размера (len зарезервирован выше).
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    buf.truncate(copied.max(0) as usize);
    String::from_utf16_lossy(&buf)
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
}
