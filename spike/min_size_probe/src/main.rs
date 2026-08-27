//! Проба C1: чтение минимального размера ЧУЖОГО окна (`WM_GETMINMAXINFO`,
//! `ptMinTrackSize`) без риска повиснуть на координаторском потоке.
//!
//! Измеряет на СВОИХ окнах (создаются на отдельном потоке-помпе, как окна
//! чужих процессов для координатора) и на УЖЕ ЗАПУЩЕННЫХ приложениях
//! пользователя ТОЛЬКО ЧТЕНИЕМ (запрос `WM_GETMINMAXINFO` + чтение
//! `GetWindowRect`/DWM-границ; ничего не двигается и не закрывается):
//!   1. сколько миллисекунд отвечает обычное приложение и сколько — тяжёлое
//!      (Spotify, браузер): выбор таймаута `SendMessageTimeoutW`;
//!   2. совпадает ли добытый минимум с тем, до какого размера окно РЕАЛЬНО
//!      удаётся ужать (на СВОЁМ окне: ставим заведомо маленький размер и
//!      меряем фактический) — и в каком пространстве живёт `ptMinTrackSize`
//!      (GetWindowRect или DWM-границы);
//!   3. что возвращают окна, которые вообще не меняют размер (без
//!      `WS_THICKFRAME`);
//!   4. врут ли приложения: `ptMinTrackSize` меньше настоящего предела, потому
//!      что предел задан в другом обработчике (`WM_WINDOWPOSCHANGING`) —
//!      на своём «врущем» окне, и насколько расходятся запрос и факт.
//!
//! Разведочный инструмент, в workspace не входит (spike исключён).

use std::io::Write;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, w};
use windows::Win32::Foundation::{
    ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SetLastError, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GWL_STYLE, IsIconic,
    IsWindowVisible, MINMAXINFO, MSG, PM_REMOVE, PeekMessageW, RegisterClassExW,
    SendMessageTimeoutW, SM_CXMINTRACK, SM_CYMINTRACK, SMTO_ABORTIFHUNG, SWP_NOSIZE,
    SetWindowPos, TranslateMessage, WM_GETMINMAXINFO, WM_WINDOWPOSCHANGING, WINDOWPOS,
    WS_OVERLAPPED, WS_THICKFRAME, WS_VISIBLE, WINDOW_STYLE, WNDCLASSEXW,
};

/// HWND не `Send` (сырой указатель) — обёртка для пересылки между потоками
/// (тот же паттерн, что в window_enum.rs/tests).
struct SendHwnd(HWND);

unsafe impl Send for SendHwnd {}

/// Каким обработчиком отвечает окно пробы.
#[derive(Clone, Copy, PartialEq)]
enum Handler {
    /// Голый DefWindowProc — типичное приложение без ограничений.
    Default,
    /// Свой `WM_GETMINMAXINFO` с ptMinTrackSize = (400, 300).
    CustomMin,
    /// «Врущее» окно: в `WM_GETMINMAXINFO` обещает (100, 100), а реально
    /// не даёт ужаться ниже (500, 400) в `WM_WINDOWPOSCHANGING`.
    Lying,
}

/// Собственное окно пробы на своём потоке-помпе: снаружи (для главного
/// потока) это чужой процесс с чужим потоком — ровно та ситуация, в которой
/// живёт координатор.
struct ProbeWin {
    hwnd: HWND,
    thread: Option<JoinHandle<()>>,
    kill: Option<mpsc::Sender<()>>,
}

impl ProbeWin {
    fn create(class: &'static str, style: WINDOW_STYLE, handler: Handler) -> Self {
        let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
        let (kill_tx, kill_rx) = mpsc::channel::<()>();
        let thread = thread::spawn(move || {
            // Класс и имя окна живут только внутри потока (PCWSTR не Send).
            let class_wide: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
            let class_pw = PCWSTR(class_wide.as_ptr());
            // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
            let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(mk_proc(handler)),
                hInstance: hinstance.into(),
                lpszClassName: class_pw,
                ..Default::default()
            };
            // SAFETY: wc заполнена корректно; повторная регистрация (параллельные
            // окна) — не ошибка.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                let _err = unsafe { GetLastError() };
            }
            // SAFETY: валидные константы и зарегистрированный класс.
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    class_pw,
                    w!("min-size probe"),
                    style,
                    100,
                    100,
                    700,
                    500,
                    None,
                    None,
                    Some(hinstance.into()),
                    None,
                )
            }
            .expect("создание окна пробы");
            let _ = ready_tx.send(SendHwnd(hwnd));
            // Помп: сообщения (в т.ч. кросс-поточные WM_GETMINMAXINFO) обязаны
            // доходить до wndproc, иначе окно «зависшее».
            loop {
                if kill_rx.try_recv().is_ok() {
                    break;
                }
                let mut msg = MSG::default();
                // SAFETY: msg — валидный буфер; None — сообщения любых окон потока.
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                    // SAFETY: msg пришёл из PeekMessageW.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        let _ = DispatchMessageW(&msg);
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
            // SAFETY: окно создано этим же потоком.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        });
        let hwnd = ready_rx.recv().expect("поток окна пробы не упал").0;
        Self {
            hwnd,
            thread: Some(thread),
            kill: Some(kill_tx),
        }
    }
}

impl Drop for ProbeWin {
    fn drop(&mut self) {
        if let Some(k) = self.kill.take() {
            let _ = k.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Wndproc по варианту обработчика. Обработчики возвращают 0 (как
/// DefWindowProc для WM_GETMINMAXINFO) — это важно для проверки различения
/// «доставлено, результат 0» и «таймаут» через GetLastError.
fn mk_proc(handler: Handler) -> unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT {
    match handler {
        Handler::Default => probe_wndproc_default,
        Handler::CustomMin => probe_wndproc_custom_min,
        Handler::Lying => probe_wndproc_lying,
    }
}

unsafe extern "system" fn probe_wndproc_default(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: делегирование системному обработчику.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn probe_wndproc_custom_min(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_GETMINMAXINFO {
        // SAFETY: lparam — указатель на MINMAXINFO от системы.
        let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
        mmi.ptMinTrackSize = POINT { x: 400, y: 300 };
        return LRESULT(0);
    }
    // SAFETY: делегирование системному обработчику.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn probe_wndproc_lying(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_GETMINMAXINFO {
        // SAFETY: lparam — указатель на MINMAXINFO от системы.
        let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
        mmi.ptMinTrackSize = POINT { x: 100, y: 100 };
        return LRESULT(0);
    }
    if msg == WM_WINDOWPOSCHANGING {
        // SAFETY: lparam — указатель на WINDOWPOS от системы.
        let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        if wp.flags & SWP_NOSIZE == windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(0) {
            wp.cx = wp.cx.max(500);
            wp.cy = wp.cy.max(400);
        }
        return LRESULT(0);
    }
    // SAFETY: делегирование системному обработчику.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

// --- общие помощники ---

fn pump(ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        let mut msg = MSG::default();
        // SAFETY: msg — валидный буфер; None — сообщения любых окон потока.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: msg пришёл из PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// DWM-границы окна (видимая рамка) в физических пикселях.
fn dwm_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: rect — валидный буфер под RECT.
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

fn gwr(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: окно живо; GetWindowRect — чтение экранного прямоугольника.
    let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
    rect
}

/// Запрос минимума как его будет делать координатор: SendMessageTimeoutW +
/// SMTO_ABORTIFHUNG; 0-результат различается через GetLastError
/// (ERROR_SUCCESS — доставлено, но результат сообщения 0).
fn query_min(hwnd: HWND, timeout_ms: u32) -> (Option<POINT>, Duration, u32) {
    let mut mmi = MINMAXINFO::default();
    let mut delivered: usize = 0;
    let t0 = Instant::now();
    // SAFETY: SetLastError — потоковый регистр ошибки.
    unsafe {
        SetLastError(ERROR_SUCCESS);
    }
    // SAFETY: hwnd — живое окно (проверено вызывающим); mmi — валидный буфер;
    // SMTO_ABORTIFHUNG обрывает зависшие потоки; таймаут ограничивает «живые,
    // но медленные».
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM((&raw mut mmi) as isize),
            SMTO_ABORTIFHUNG,
            timeout_ms,
            Some(&mut delivered),
        )
    };
    let elapsed = t0.elapsed();
    // SAFETY: GetLastError — потоковый регистр ошибки.
    let err = unsafe { GetLastError() };
    let ok = result.0 != 0 || err == ERROR_SUCCESS;
    let min = ok.then_some(mmi.ptMinTrackSize);
    (min, elapsed, err.0)
}

/// Статистика латентности запроса по `reps` повторам.
fn latency_stats(hwnd: HWND, timeout_ms: u32, reps: u32) -> (Duration, Duration, Duration) {
    let mut all = Vec::new();
    for _ in 0..reps {
        let (_, elapsed, _) = query_min(hwnd, timeout_ms);
        all.push(elapsed);
    }
    let min = *all.iter().min().unwrap();
    let max = *all.iter().max().unwrap();
    let avg = all.iter().sum::<Duration>() / reps;
    (min, avg, max)
}

/// Смещение GetWindowRect → DWM-границы (те же dx/dy/dw/dh, что в
/// `WindowPins::set_dwm_bounds`): на Win11 ~7 px невидимых полей ресайза.
fn dwm_offset(hwnd: HWND) -> (i32, i32, i32, i32) {
    let g = gwr(hwnd);
    let d = dwm_rect(hwnd);
    (
        d.left - g.left,
        d.top - g.top,
        (d.right - d.left) - (g.right - g.left),
        (d.bottom - d.top) - (g.bottom - g.top),
    )
}

fn style_has_thickframe(hwnd: HWND) -> bool {
    // SAFETY: GetWindowLongPtrW — чтение стиля живого окна.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    style & WS_THICKFRAME.0 != 0
}

/// Действительно ли окно удаётся ужать до `(w, h)` (GetWindowRect-пространство):
/// ставим заведомо маленький размер и меряем фактический.
fn actual_shrink(hwnd: HWND, want_w: i32, want_h: i32) -> (RECT, RECT) {
    // SAFETY: окно живо (своё); SetWindowPos без z-order/активации.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            200,
            200,
            want_w,
            want_h,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
        )
    };
    pump(80);
    (gwr(hwnd), dwm_rect(hwnd))
}

fn describe(hwnd: HWND) -> String {
    let mut title = [0u16; 256];
    let mut class = [0u16; 128];
    // SAFETY: чтение заголовка и класса — безопасно для любых хэндлов.
    let tl = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
    let cl = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    format!(
        "'{}' [{}]",
        String::from_utf16_lossy(&title[..tl]),
        String::from_utf16_lossy(&class[..cl])
    )
}

/// Колбэк EnumWindows: собрать top-level окна в переданный через lparam Vec.
extern "system" fn enum_cb(
    hwnd: HWND,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: data — указатель на Vec<HWND>, созданный нами.
    let v = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
    v.push(hwnd);
    windows::core::BOOL(1)
}

// --- замеры ---

/// 1. Латентность ответа: свои окна (разные обработчики) + живые приложения
///    пользователя (только чтение).
fn part1_latency() {
    println!("--- 1. Латентность ответа на WM_GETMINMAXINFO ---");
    let default = ProbeWin::create(
        "msp_default",
        WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE,
        Handler::Default,
    );
    let custom = ProbeWin::create(
        "msp_custom",
        WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE,
        Handler::CustomMin,
    );
    let fixed = ProbeWin::create("msp_fixed", WS_OVERLAPPED | WS_VISIBLE, Handler::Default);
    for (name, hwnd) in [
        ("своё: Default", default.hwnd),
        ("своё: CustomMin", custom.hwnd),
        ("своё: FixedSize", fixed.hwnd),
    ] {
        let (min, avg, max) = latency_stats(hwnd, 500, 5);
        let q = query_min(hwnd, 500).0;
        println!(
            "  {name:20} min={min:.3?} avg={avg:.3?} max={max:.3?}  (query: {q:?})",
        );
    }

    // Живые приложения пользователя — только чтение, ничего не двигаем.
    println!("  живые приложения (только чтение):");
    let mut top_levels: Vec<HWND> = Vec::new();
    // SAFETY: EnumWindows с валидным колбэком.
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM((&raw mut top_levels) as isize));
    }
    for hwnd in top_levels {
        // SAFETY: IsWindowVisible безопасен для любых хэндлов.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            continue;
        }
let (min, elapsed, err) = query_min(hwnd, 500);
        if let Some(min) = min {
            println!(
                "  0x{:08X} {:<70} min={}x{}  отклик={elapsed:.3?} err={err} thickframe={}",
                hwnd.0 as usize,
                describe(hwnd),
                min.x,
                min.y,
                style_has_thickframe(hwnd),
            );
        }
    }
}

/// 2. Точность: совпадает ли добытый минимум с фактическим пределом ужатия.
fn part2_accuracy() {
    println!("--- 2. Совпадение запроса и факта (свои окна) ---");
    for (name, hwnd) in [
        ("Default", ProbeWin::create("msp_a1", WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE, Handler::Default).hwnd),
        ("CustomMin 400x300", ProbeWin::create("msp_a2", WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE, Handler::CustomMin).hwnd),
        ("Lying 100x100/500x400", ProbeWin::create("msp_a3", WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE, Handler::Lying).hwnd),
    ] {
let (min, _, _) = query_min(hwnd, 500);
        let (dx, dy, dw, dh) = dwm_offset(hwnd);
        let g = gwr(hwnd);
        let (shrunk_gwr, shrunk_dwm) = actual_shrink(hwnd, 60, 60);
        println!(
            "  {name}:\n    запрос ptMinTrackSize = {:?}\n    смещение GWR→DWM: dx={dx} dy={dy} dw={dw} dh={dh} (до: gwr {}x{})\n    после SetWindowPos(60x60): gwr {}x{}  dwm {}x{}",
            min,
            g.right - g.left,
            g.bottom - g.top,
            shrunk_gwr.right - shrunk_gwr.left,
            shrunk_gwr.bottom - shrunk_gwr.top,
            shrunk_dwm.right - shrunk_dwm.left,
            shrunk_dwm.bottom - shrunk_dwm.top,
        );
        if let Some(m) = min {
            println!(
                "    минимум в DWM-пространстве (ptMin + dw/dh): {}x{}",
                m.x + dw,
                m.y + dh
            );
        }
    }
}

/// 3. Окна без WS_THICKFRAME — что возвращают.
fn part3_fixed() {
    println!("--- 3. Окно без WS_THICKFRAME ---");
    let fixed = ProbeWin::create(
        "msp_fixed2",
        WS_OVERLAPPED | WS_VISIBLE,
        Handler::Default,
    );
    // SAFETY: GetSystemMetrics — чтение параметров системы.
    let sys_min_w = unsafe { GetSystemMetrics(SM_CXMINTRACK) };
    let sys_min_h = unsafe { GetSystemMetrics(SM_CYMINTRACK) };
    let (min, elapsed, err) = query_min(fixed.hwnd, 500);
    println!(
        "  окно без WS_THICKFRAME: запрос = {:?} за {elapsed:.3?} (err={err}); системные умолчания SM_CXMINTRACK/SM_CYMINTRACK = {sys_min_w}x{sys_min_h}",
        min
    );
}

/// 4. «Врущие» приложения и честность числа на живых окнах.
fn part4_honesty() {
    println!("--- 4. Честность числа ---");
    // Своё «врущее» окно: запрос (100,100), факт (500,400).
    let lying = ProbeWin::create(
        "msp_lying",
        WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE,
        Handler::Lying,
    );
    let (min, _, _) = query_min(lying.hwnd, 500);
    let (g, _) = actual_shrink(lying.hwnd, 60, 60);
    println!(
        "  своё «врущее» окно: запрос = {:?}, факт после SetWindowPos(60x60) = {}x{} (gwr) — расхождение {:?}",
        min,
        g.right - g.left,
        g.bottom - g.top,
        min.map(|m| ((g.right - g.left) - m.x, (g.bottom - g.top) - m.y))
    );

    // Живые приложения: запрошенный минимум (в DWM-пространстве) против
    // ТЕКУЩЕГО размера (только чтение): если окно сейчас уже уже/ниже
    // «минимума» — число либо врёт, либо не исполняется.
    println!("  живые приложения: минимум (DWM) vs текущий размер:");
    let mut top_levels: Vec<HWND> = Vec::new();
    // SAFETY: EnumWindows с валидным колбэком.
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM((&raw mut top_levels) as isize));
    }
for hwnd in top_levels {
        // SAFETY: IsWindowVisible/IsIconic безопасны для любых хэндлов.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            continue;
        }
        if unsafe { IsIconic(hwnd) }.as_bool() {
            println!(
                "  0x{:08X} {:<60} (свёрнуто — DWM-границы мусорные, пропущено)",
                hwnd.0 as usize,
                describe(hwnd),
            );
            continue;
        }
        let (min, _, _) = query_min(hwnd, 200);
        let Some(min) = min else { continue };
        let d = dwm_rect(hwnd);
        let cur_w = d.right - d.left;
        let cur_h = d.bottom - d.top;
        let (_, _, dw, dh) = dwm_offset(hwnd);
        let (min_dw, min_dh) = (min.x + dw, min.y + dh);
        let below = cur_w < min_dw || cur_h < min_dh;
        if below {
            println!(
                "  0x{:08X} {:<60} мин(DWM) {}x{} vs ТЕКУЩИЙ {}x{}  <-- ОКНО УЖЕ МЕНЬШЕ ЗАЯВЛЕННОГО МИНИМУМА",
                hwnd.0 as usize,
                describe(hwnd),
                min_dw,
                min_dh,
                cur_w,
                cur_h,
            );
        }
    }
    println!("  (строк выше нет — все живые окна ≥ заявленного минимума)");
}

/// 5. Зависшее окно: возвращается ли вызов быстро, и с какой ошибкой.
fn part5_hung() {
    println!("--- 5. Зависшее окно ---");
    // Поток создаёт окно и НЕ пампит сообщения — снаружи это «зависшее»
    // приложение.
    let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
    let (kill_tx, kill_rx) = mpsc::channel::<()>();
    let t = thread::spawn(move || {
        // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
        let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(probe_wndproc_default),
            hInstance: hinstance.into(),
            lpszClassName: w!("msp_hung"),
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация — не ошибка.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            let _err = unsafe { GetLastError() };
        }
        // SAFETY: валидные константы и зарегистрированный класс.
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                w!("msp_hung"),
                w!("hung"),
                WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE,
                100,
                100,
                700,
                500,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание окна пробы");
        let _ = ready_tx.send(SendHwnd(hwnd));
        // НЕ пампим: поток блокируется, пока тест не попросит завершиться.
        let _ = kill_rx.recv();
        // SAFETY: окно создано этим же потоком.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    });
    let hwnd = ready_rx.recv().expect("поток окна не упал").0;

    for timeout in [10u32, 50, 200, 500, 2000] {
        let t0 = Instant::now();
        let (min, elapsed, err) = query_min(hwnd, timeout);
        println!(
            "  таймаут={timeout:4} мс: заняло {elapsed:?}  (запрошено {timeout} мс, вызов {})  result={:?} err={err}",
            if elapsed.as_millis() as u32 >= timeout {
                "ОТРАБОТАЛ ПО ТАЙМАУТУ"
            } else {
                "вернулся сразу"
            },
            min
        );
        let _ = t0;
    }
    let _ = kill_tx.send(());
    let _ = t.join();
}

fn main() {
    println!("=== C1: минимальный размер чужого окна (WM_GETMINMAXINFO) ===");
    part1_latency();
    println!();
    part2_accuracy();
    println!();
    part3_fixed();
    println!();
    part4_honesty();
    println!();
    part5_hung();
    println!();
    println!("Готово.");
    let _ = std::io::stdout().flush();
}

