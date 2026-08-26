//! Проба H4 (репорт 2026-08-26): окно группы не встало в свой слот — осталось
//! на прежнем месте. Подозреваемые: развёрнутое (maximized) и приснапленное
//! Windows окно. Замеряем НА СВОИХ ОКНАХ поведение `set_dwm_bounds` из
//! `crates/rst-win32/src/window_pin.rs` в обоих состояниях.
//!
//! Последовательность вызовов в `set_dwm_bounds_inline` повторяет текущую
//! реализацию один в один (is_maximized → SetWindowPlacement → GetWindowRect →
//! DWM-границы → компенсирующий SetWindowPos); проба лежит в отдельном крейте
//! потому, что на момент замера rst-win32 не компилировался (координатор
//! правил overlay.rs) — поведение Windows от этого не меняется, это те же
//! четыре API на тех же окнах.
//!
//! Дополнительно сравниваются два способа вывести окно из maximized:
//! A) SetWindowPlacement(SW_SHOWNOACTIVATE, rcNormalPosition=цель) — один
//!    вызов, снимает развёрнутость и ставит прямоугольник сразу;
//! B) ShowWindowAsync(SW_RESTORE) + отдельный SetWindowPos — два шага.
//! Критерии: куда в итоге встало окно, украден ли фокус, сколько было
//! WM_MOVE/WM_SIZE (прокси «промежуточных кадров»: каждый move = потенциальная
//! перерисовка).
//!
//! Запуск: cargo run --manifest-path spike/maximize_probe/Cargo.toml

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, HWND, RECT, WPARAM, LPARAM, LRESULT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    KEYBD_EVENT_FLAGS, KEYBDINPUT, SendInput, VK_LEFT, VK_LMENU, VK_LWIN, VIRTUAL_KEY, INPUT,
    INPUT_0, INPUT_KEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, GWL_STYLE, HWND_TOP, IsWindow,
    IsZoomed, MSG, PeekMessageW, PM_REMOVE, RegisterClassExW, SetForegroundWindow,
    SetWindowPos, SetWindowPlacement, ShowWindow, ShowWindowAsync, SW_MAXIMIZE, SW_RESTORE,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    TranslateMessage, WINDOWPLACEMENT, WINDOW_STYLE, WNDCLASSEXW, WM_MOVE, WM_SIZE,
    WS_MAXIMIZE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

/// Счётчики сообщений окна — прокси «сколько раз окно реально двигалось»:
/// каждый `WM_MOVE`/`WM_SIZE` — это потенциальный промежуточный кадр.
static MOVES: AtomicU32 = AtomicU32::new(0);
static SIZES: AtomicU32 = AtomicU32::new(0);

fn main() {
    let hwnd = create_probe_window();
    let work = work_area();
    let quarter = RECT {
        left: work.left,
        top: work.top,
        right: work.left + (work.right - work.left) / 2,
        bottom: work.top + (work.bottom - work.top) / 2,
    };
    let quarter2 = RECT {
        left: work.left + (work.right - work.left) / 2,
        top: work.top + (work.bottom - work.top) / 2,
        right: work.right,
        bottom: work.bottom,
    };
    println!("work area: {:?}", work);
    println!("target A (верхняя левая четверть): {:?}", quarter);
    println!("target B (нижняя правая четверть): {:?}", quarter2);

    // === 0. Контроль: обычное окно ===
    println!("\n=== 0. ОБЫЧНОЕ окно ===");
    raw_set_window_pos(hwnd, RECT { left: 30, top: 30, right: 530, bottom: 430 });
    pump(10);
    measure("до set_dwm_bounds", hwnd);
    let moves_before = MOVES.load(Ordering::SeqCst);
    set_dwm_bounds_inline(hwnd, quarter);
    pump(10);
    measure("после set_dwm_bounds(target A)", hwnd);
    report_moves("set_dwm_bounds", moves_before);
    assert_dwm(hwnd, quarter, "контроль");

    // === 1. Maximized: путь СТАРОГО кода (прямой SetWindowPos, без снятия
    // максимизации) — воспроизводим механизм бага ===
    println!("\n=== 1. MAXIMIZED: прямой SetWindowPos без восстановления ===");
    maximize(hwnd);
    measure("до SetWindowPos (maximized)", hwnd);
    raw_set_window_pos(hwnd, quarter);
    pump(10);
    measure("после SetWindowPos(target A), окно осталось maximized", hwnd);
    assert_dwm(hwnd, quarter, "SetWindowPos на maximized без restore");

    // === 2. Maximized: текущий set_dwm_bounds ===
    println!("\n=== 2. MAXIMIZED: set_dwm_bounds (текущий код: restore + компенсация) ===");
    maximize(hwnd);
    pump(10);
    measure("до set_dwm_bounds (maximized)", hwnd);
    let moves_before = MOVES.load(Ordering::SeqCst);
    set_dwm_bounds_inline(hwnd, quarter);
    pump(10);
    measure("после set_dwm_bounds(target A)", hwnd);
    report_moves("set_dwm_bounds на maximized", moves_before);
    println!("  IsZoomed после: {}", is_zoomed(hwnd));
    assert_dwm(hwnd, quarter, "maximized + set_dwm_bounds");

    // === 3. Способы снять maximized: A vs B ===
    println!("\n=== 3. Способы снять maximized ===");

    // 3A. SetWindowPlacement(SW_SHOWNOACTIVATE) — один вызов.
    maximize(hwnd);
    pump(10);
    let fg_before = unsafe { GetForegroundWindow() };
    let moves_before = MOVES.load(Ordering::SeqCst);
    let placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: SW_SHOWNOACTIVATE.0 as u32,
        rcNormalPosition: quarter2,
        ..Default::default()
    };
    // SAFETY: hwnd — наше живое окно.
    unsafe { SetWindowPlacement(hwnd, &placement) }.expect("SetWindowPlacement");
    pump(10);
    measure("3A. SetWindowPlacement(SW_SHOWNOACTIVATE, rcNormal=target B)", hwnd);
    report_moves("3A (один вызов)", moves_before);
    println!(
        "  IsZoomed: {}, фокус украден: {} (был {}, стал {})",
        is_zoomed(hwnd),
        unsafe { GetForegroundWindow() } != fg_before,
        fg_before.0 as isize,
        unsafe { GetForegroundWindow() }.0 as isize
    );

    // 3B. ShowWindowAsync(SW_RESTORE) — сначала restore на СТАРЫЙ
    // rcNormalPosition, потом отдельный SetWindowPos на цель.
    maximize(hwnd);
    pump(10);
    let fg_before = unsafe { GetForegroundWindow() };
    let moves_before = MOVES.load(Ordering::SeqCst);
    // SAFETY: наше окно.
    unsafe { let _ = ShowWindowAsync(hwnd, SW_RESTORE); };
    pump(10);
    measure("3B. после ShowWindowAsync(SW_RESTORE) (до SetWindowPos)", hwnd);
    raw_set_window_pos(hwnd, quarter2);
    pump(10);
    measure("3B. после SetWindowPos(target B)", hwnd);
    report_moves("3B (два шага)", moves_before);
    println!(
        "  IsZoomed: {}, фокус украден: {} (был {}, стал {})",
        is_zoomed(hwnd),
        unsafe { GetForegroundWindow() } != fg_before,
        fg_before.0 as isize,
        unsafe { GetForegroundWindow() }.0 as isize
    );

    // === 4. Snapped (Win+Left через SendInput) ===
    println!("\n=== 4. SNAPPED (Win+Left) ===");
    raw_set_window_pos(hwnd, RECT { left: 60, top: 60, right: 660, bottom: 560 });
    pump(10);
    snap_left(hwnd);
    pump(30);
    measure("после Win+Left (снап к половине экрана)", hwnd);
    println!(
        "  IsZoomed: {}; WS_MAXIMIZE в стиле: {}; showCmd в placement: {}",
        is_zoomed(hwnd),
        has_style(hwnd, WS_MAXIMIZE),
        placement_show_cmd(hwnd)
    );
    let moves_before = MOVES.load(Ordering::SeqCst);
    set_dwm_bounds_inline(hwnd, quarter);
    pump(10);
    measure("после set_dwm_bounds(target A) на snapped-окне", hwnd);
    report_moves("set_dwm_bounds на snapped", moves_before);
    assert_dwm(hwnd, quarter, "snapped + set_dwm_bounds");

    // === 5. Окно на полэкрана, поставленное SetWindowPos (не снап) ===
    println!("\n=== 5. Окно на полэкрана (SetWindowPos, не снап) ===");
    let half = RECT {
        left: work.left,
        top: work.top,
        right: work.left + (work.right - work.left) / 2,
        bottom: work.bottom,
    };
    raw_set_window_pos(hwnd, half);
    pump(10);
    measure("после SetWindowPos на левую половину", hwnd);
    println!(
        "  IsZoomed: {}; WS_MAXIMIZE в стиле: {}",
        is_zoomed(hwnd),
        has_style(hwnd, WS_MAXIMIZE)
    );
    set_dwm_bounds_inline(hwnd, quarter2);
    pump(10);
    measure("после set_dwm_bounds(target B)", hwnd);
    assert_dwm(hwnd, quarter2, "полэкранное окно + set_dwm_bounds");

    // SAFETY: наше окно, конец пробы.
    unsafe { let _ = ShowWindow(hwnd, SW_SHOWNORMAL); };
    // SAFETY: наше окно.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    println!("\nготово");
}

/// Точная копия последовательности вызовов `WindowPins::set_dwm_bounds`
/// (crates/rst-win32/src/window_pin.rs, строки ~761-812): is_maximized →
/// SetWindowPlacement(rcNormalPosition=цель) → замер рамки → компенсирующий
/// SetWindowPos. Проба обязана измерять ИМЕННО эту последовательность, а не
/// «примерно такую».
fn set_dwm_bounds_inline(hwnd: HWND, target: RECT) -> bool {
    // SAFETY: IsWindow безопасен для любых значений.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    let good = target;
    let good_w = good.right - good.left;
    let good_h = good.bottom - good.top;

    if is_maximized(hwnd) {
        let placement = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: SW_SHOWNOACTIVATE.0 as u32,
            rcNormalPosition: good,
            ..Default::default()
        };
        // SAFETY: наше живое окно; placement заполнена корректно.
        let _ = unsafe { SetWindowPlacement(hwnd, &placement) };
    }

    let mut gwr = RECT::default();
    // SAFETY: чтение прямоугольника живого окна.
    if unsafe { GetWindowRect(hwnd, &mut gwr) }.is_err() {
        return false;
    }
    let dwm = dwm_bounds(hwnd);
    let (dx, dy, dw, dh) = if dwm.right - dwm.left > 0 && dwm.bottom - dwm.top > 0 {
        (
            dwm.left - gwr.left,
            dwm.top - gwr.top,
            (dwm.right - dwm.left) - (gwr.right - gwr.left),
            (dwm.bottom - dwm.top) - (gwr.bottom - gwr.top),
        )
    } else {
        (0, 0, 0, 0)
    };

    let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
    // SAFETY: окно живо; флаги исключают активацию/смену z-order.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            good.left - dx,
            good.top - dy,
            good_w - dw,
            good_h - dh,
            flags,
        )
    };
    true
}

// ---------- helpers ----------

fn create_probe_window() -> HWND {
    // SAFETY: GetModuleHandleW(None) — хэндл текущего процесса.
    let hinstance = unsafe { GetModuleHandleW(None) }.expect("модуль");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(probe_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: w!("resticker_maximize_probe"),
        ..Default::default()
    };
    // SAFETY: wc заполнен корректно.
    let _ = unsafe { RegisterClassExW(&wc) };
    // SAFETY: класс зарегистрирован выше.
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("resticker_maximize_probe"),
            w!("probe"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            30,
            30,
            500,
            400,
            None,
            None,
            Some(windows::Win32::Foundation::HINSTANCE(hinstance.0)),
            None,
        )
    }
    .expect("создание окна");
    pump(10);
    hwnd
}

// SAFETY: сообщения делегируем системе; WM_MOVE/WM_SIZE только считаем.
unsafe extern "system" fn probe_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_MOVE {
        MOVES.fetch_add(1, Ordering::SeqCst);
    } else if msg == WM_SIZE {
        SIZES.fetch_add(1, Ordering::SeqCst);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn is_maximized(hwnd: HWND) -> bool {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: placement заполнен (length обязателен); GetWindowPlacement
    // безопасен для чужих и мёртвых окон.
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.is_ok()
        && placement.showCmd == SW_MAXIMIZE.0 as u32
}

fn work_area() -> RECT {
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi заполнен.
    let mon = unsafe { MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTONEAREST) };
    // SAFETY: mi валиден.
    let _ = unsafe { GetMonitorInfoW(mon, &mut mi) };
    mi.rcWork
}

fn pump(ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    loop {
        // SAFETY: очередь сообщений нашего потока.
        let mut msg = MSG::default();
        let has = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool();
        if has {
            // SAFETY: msg валиден из PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        } else if std::time::Instant::now() >= deadline {
            break;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn measure(tag: &str, hwnd: HWND) {
    let mut gwr = RECT::default();
    // SAFETY: наше живое окно.
    unsafe { GetWindowRect(hwnd, &mut gwr) }.expect("GetWindowRect");
    let dwm = dwm_bounds(hwnd);
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: placement заполнен.
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.expect("GetWindowPlacement");
    println!(
        "  {tag}:\n    GetWindowRect:  l={} t={} r={} b={} (w={} h={})\n    DWM bounds:     l={} t={} r={} b={} (w={} h={})\n    showCmd={} rcNormal=({},{},{},{}) IsZoomed={} WS_MAXIMIZE={}",
        gwr.left, gwr.top, gwr.right, gwr.bottom, gwr.right - gwr.left, gwr.bottom - gwr.top,
        dwm.left, dwm.top, dwm.right, dwm.bottom, dwm.right - dwm.left, dwm.bottom - dwm.top,
        placement.showCmd,
        placement.rcNormalPosition.left,
        placement.rcNormalPosition.top,
        placement.rcNormalPosition.right,
        placement.rcNormalPosition.bottom,
        is_zoomed(hwnd),
        has_style(hwnd, WS_MAXIMIZE),
    );
}

fn report_moves(tag: &str, before: u32) {
    let moves = MOVES.load(Ordering::SeqCst) - before;
    let sizes = SIZES.load(Ordering::SeqCst);
    println!("  {tag}: WM_MOVE при операции: {moves} (всего WM_SIZE: {sizes})");
}

fn dwm_bounds(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: rect — валидный буфер.
    let _ = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    };
    rect
}

fn is_zoomed(hwnd: HWND) -> bool {
    // SAFETY: чтение состояния живого окна.
    unsafe { IsZoomed(hwnd) }.as_bool()
}

fn placement_show_cmd(hwnd: HWND) -> u32 {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: placement заполнен.
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.expect("GetWindowPlacement");
    placement.showCmd
}

fn has_style(hwnd: HWND, bit: WINDOW_STYLE) -> bool {
    // SAFETY: чтение стилей живого окна.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    style as u32 & bit.0 != 0
}

fn raw_set_window_pos(hwnd: HWND, r: RECT) {
    raw_set_window_pos_impl(hwnd, r.left, r.top, r.right - r.left, r.bottom - r.top);
}

fn raw_set_window_pos_impl(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    // SAFETY: наше окно; без активации и z-order.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    };
    pump(5);
}

fn maximize(hwnd: HWND) {
    // SAFETY: наше окно.
    let _ = unsafe { ShowWindow(hwnd, SW_MAXIMIZE) };
    pump(30);
}

fn snap_left(hwnd: HWND) {
    // Классический трюк для фонового процесса: сначала «пощупать» Alt —
    // тогда Windows разрешает SetForegroundWindow (без этого консольное
    // приложение фокус не получит и Win+Left уйдёт в окно консоли).
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYBD_EVENT_FLAGS(2) } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: отправка обычного ввода.
    let _ = unsafe { SendInput(&[key(VK_LMENU, false), key(VK_LMENU, true)], size_of::<INPUT>() as i32) };
    pump(10);
    // SAFETY: наше окно.
    let _ = unsafe { SetForegroundWindow(hwnd) };
    pump(10);
    println!(
        "  foreground после трюка с Alt: {} (наше окно: {})",
        unsafe { GetForegroundWindow() }.0 as isize,
        hwnd.0 as isize
    );
    // SAFETY: клавиши сжимаем через SendInput — обычная отправка ввода.
    let inputs = [
        key(VK_LWIN, false),
        key(VK_LEFT, false),
        key(VK_LEFT, true),
        key(VK_LWIN, true),
    ];
    // SAFETY: массив INPUT валиден.
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent == 0 {
        println!("  ! SendInput не сработал (код {})", unsafe {
            GetLastError().0
        });
    }
    pump(50);
}

fn assert_dwm(hwnd: HWND, target: RECT, tag: &str) {
    let dwm = dwm_bounds(hwnd);
    let dx = dwm.left - target.left;
    let dy = dwm.top - target.top;
    let dw = (dwm.right - dwm.left) - (target.right - target.left);
    let dh = (dwm.bottom - dwm.top) - (target.bottom - target.top);
    let ok = dx.abs() <= 4 && dy.abs() <= 4 && dw.abs() <= 4 && dh.abs() <= 4;
    println!(
        "  {tag}: DWM против target: dx={dx} dy={dy} dw={dw} dh={dh} -> {}",
        if ok { "ВСТАЛО" } else { "НЕ ВСТАЛО" }
    );
}