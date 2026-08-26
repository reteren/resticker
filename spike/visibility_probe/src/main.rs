//! Проба поведения сворачивания/разворачивания/подъёма окон для G2
//! («Примитивы показа и сокрытия чужого окна»).
//!
//! Измеряет на СОБСТВЕННЫХ окнах (чужие окна пользователя не трогаются):
//! 1. Какой из способов сворачивания не крадёт фокус и не даёт анимацию-
//!    подёргивание: SW_MINIMIZE, SW_SHOWMINNOACTIVE, SW_FORCEMINIMIZE,
//!    WM_SYSCOMMAND/SC_MINIMIZE.
//!    Кража фокуса — по WM_ACTIVATE, полученным ПРЕЖНЕ активным окном
//!    (GetForegroundWindow может и не смениться, если активируется оно же).
//!    Анимация — опрос GetWindowRect каждые 3 мс (промежуточные размеры =
//!    видимый переход) + замер времени до IsIconic.
//! 2. Возвращается ли окно после разворачивания ровно в прежний
//!    прямоугольник (SW_RESTORE синхронным и асинхронным путём).
//! 3. Поднимается ли окно через HWND_TOP поверх полноэкранного окна
//!    (обычного и topmost) и не оставляет ли липкий WS_EX_TOPMOST;
//!    приём «кратковременный TOPMOST → немедленный NOTOPMOST».
//!
//! Разведочный инструмент, в workspace не входит (spike исключён).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect, GW_HWNDPREV, GWL_EXSTYLE,
    HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, IsIconic, IsWindowVisible, MSG, PM_REMOVE,
    PeekMessageW, PostMessageW, RegisterClassExW, SC_MINIMIZE, SetForegroundWindow,
    SetWindowPos, ShowWindow, ShowWindowAsync, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_FORCEMINIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWMINNOACTIVE, SM_CXSCREEN, SM_CYSCREEN,
    TranslateMessage, WA_ACTIVE, WM_ACTIVATE, WM_KILLFOCUS, WM_SYSCOMMAND, WM_SETFOCUS,
    WS_EX_TOPMOST, WS_OVERLAPPED, WS_POPUP, WS_VISIBLE, WINDOW_STYLE, WNDCLASSEXW,
};
use windows::core::{BOOL, w};

/// Журнал сообщений окон пробы: (hwnd, msg, wparam) в порядке получения.
/// Окна пробы живут на главном потоке, помп тоже — локи не нужны, но Mutex
/// защищает от параллельных тестовых потоков (их нет — просто аккуратно).
static LOG: Mutex<Vec<(isize, u32, usize)>> = Mutex::new(Vec::new());

fn log(hwnd: HWND, msg: u32, wparam: WPARAM) {
    LOG.lock().unwrap().push((hwnd.0 as isize, msg, wparam.0));
}

// --- тестовые окна пробы ---

struct ProbeWindow(HWND);

impl ProbeWindow {
    /// Создать видимое окно заданного класса/стиля. Класс регистрируется
    /// один раз (повторная регистрация — не ошибка).
    fn create(class: &'static str, style: WINDOW_STYLE) -> Self {
        // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
        let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
        let class_wide: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
        let class_pw = windows::core::PCWSTR(class_wide.as_ptr());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(probe_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_pw,
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация класса —
        // не ошибка (возвращает 0 + ERROR_CLASS_ALREADY_EXISTS).
        if unsafe { RegisterClassExW(&wc) } == 0 {
            // SAFETY: осмысленна сразу после провалившегося вызова.
            let _err = unsafe { windows::Win32::Foundation::GetLastError() };
        }
        // SAFETY: валидные константы и зарегистрированный класс; окно
        // принадлежит текущему потоку (помп крутит pump).
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                class_pw,
                w!("probe"),
                style,
                0,
                0,
                400,
                300,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание окна пробы");
        Self(hwnd)
    }
}

impl Drop for ProbeWindow {
    fn drop(&mut self) {
        // SAFETY: окно создано этим же потоком.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn probe_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ACTIVATE || msg == WM_SETFOCUS || msg == WM_KILLFOCUS {
        log(hwnd, msg, wparam);
    }
    // SAFETY: делегирование системному обработчику.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

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

fn rect_of(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    // SAFETY: hwnd — живое окно пробы.
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok() {
        r
    } else {
        RECT::default()
    }
}

fn iconic(hwnd: HWND) -> bool {
    // SAFETY: IsIconic безопасен для любых значений.
    unsafe { IsIconic(hwnd) }.as_bool()
}

fn visible(hwnd: HWND) -> bool {
    // SAFETY: IsWindowVisible безопасен для любых значений.
    unsafe { IsWindowVisible(hwnd) }.as_bool()
}

/// WS_EX_TOPMOST в расширенном стиле окна (липкий стиль, которого мы не
/// хотим оставлять на чужих окнах).
fn has_topmost_style(hwnd: HWND) -> bool {
    // SAFETY: GetWindowLongPtrW — чтение стиля живого окна.
    (unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0) != 0
}

/// Стоит ли окно `above` где-то выше окна `below` (обход z-order вверх).
fn window_above(above: HWND, below: HWND) -> bool {
    // SAFETY: GetWindow — чтение z-order, безопасен для любых хэндлов.
    let mut cur = unsafe { GetWindow(below, GW_HWNDPREV) }.unwrap_or_default();
    while !cur.0.is_null() {
        if cur == above {
            return true;
        }
        // SAFETY: то же — обход вверх до верха стопки.
        cur = unsafe { GetWindow(cur, GW_HWNDPREV) }.unwrap_or_default();
    }
    false
}

/// Краткая сводка одного замера сворачивания.
#[derive(Default)]
struct MinimizeReport {
    method: &'static str,
    /// Прежне активное окно потеряло foreground (GetForegroundWindow сменился).
    fg_changed: bool,
    fg_before: HWND,
    fg_after: HWND,
    /// Окно-«свидетель» (следующее в z-order за целевым) получило
    /// WM_ACTIVATE(WA_ACTIVE) — это и есть «активация следующего окна»,
    /// которую документирует SW_MINIMIZE.
    bystander_activated: bool,
    iconic: bool,
    visible: bool,
    /// Все различный высоты прямоугольника, наблюдённые за замером.
    heights_seen: Vec<i32>,
}

/// Полный замер одного способа сворачивания: целевое окно сначала
/// разворачивается и ставится в известный прямоугольник; активным делаем
/// окно `active` (свидетель `bystander` стоит в z-order МЕЖДУ fg и target —
/// «следующее окно», которое SW_MINIMIZE обязан активировать при
/// сворачивании активного target); затем вызываем `call` и наблюдаем.
fn measure_minimize(
    method: &'static str,
    active: HWND,
    bystander: HWND,
    target: HWND,
    call: impl FnOnce(HWND),
) -> MinimizeReport {
    // SAFETY: окна пробы живые; SW_RESTORE — штатный show-command.
    let _ = unsafe { ShowWindow(target, SW_RESTORE) };
    pump(150);
    // SAFETY: окно живо; известный прямоугольник без z-order/активации.
    let _ = unsafe {
        SetWindowPos(
            target,
            None,
            60,
            70,
            400,
            300,
            SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
        )
    };
    pump(100);
    // SAFETY: активация собственного окна — best effort (система может
    // отказать без права на foreground; тогда в отчёте будет видно).
    unsafe {
        let _ = SetForegroundWindow(active);
    }
    pump(150);

    let fg_before = unsafe { GetForegroundWindow() };
    LOG.lock().unwrap().clear();
    let mut heights: Vec<i32> = Vec::new();

    let t0 = Instant::now();
    call(target);
    while t0.elapsed() < Duration::from_millis(1500) {
        let h = rect_of(target).bottom - rect_of(target).top;
        if heights.last() != Some(&h) {
            heights.push(h);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    let fg_after = unsafe { GetForegroundWindow() };
    let msgs = LOG.lock().unwrap().clone();
    MinimizeReport {
        method,
        fg_changed: fg_before != fg_after,
        fg_before,
        fg_after,
        bystander_activated: msgs.iter().any(|&(h, m, w)| {
            h == bystander.0 as isize && m == WM_ACTIVATE && w == WA_ACTIVE as usize
        }),
        iconic: iconic(target),
        visible: visible(target),
        heights_seen: heights,
    }
}

fn main() {
    println!("=== G2: проба сворачивания / разворачивания / подъёма (свои окна) ===");
    // SAFETY: GetSystemMetrics — чтение параметров системы.
    println!(
        "screen: {}x{}",
        unsafe { GetSystemMetrics(SM_CXSCREEN) },
        unsafe { GetSystemMetrics(SM_CYSCREEN) }
    );
    println!();

    // --- 1. Способы сворачивания: кража фокуса и анимация ---
    println!("--- 1. Сворачивание: фокус и анимация ---");
    // z-order сверху вниз: target, свидетель, fg (порядок создания).
    // Свидетель стоит МЕЖДУ fg и target — это «следующее top-level окно»,
    // которое SW_MINIMIZE обязан активировать при сворачивании target.
    let fg = ProbeWindow::create("g2_fg", WS_OVERLAPPED | WS_VISIBLE);
    let bystander = ProbeWindow::create("g2_bystander", WS_OVERLAPPED | WS_VISIBLE);
    let target = ProbeWindow::create("g2_target", WS_OVERLAPPED | WS_VISIBLE);
    println!(
        "  окна пробы: fg=0x{:X} свидетель=0x{:X} target=0x{:X}",
        fg.0.0 as usize,
        bystander.0.0 as usize,
        target.0.0 as usize
    );
    pump(200);
    assert!(
        window_above(target.0, bystander.0) && window_above(bystander.0, fg.0),
        "порядок создания окон обязан дать z-order target > свидетель > fg"
    );

    let methods: Vec<(&str, Box<dyn Fn(HWND)>)> = vec![
        ("SW_MINIMIZE", Box::new(|h| {
            let _ = unsafe { ShowWindow(h, SW_MINIMIZE) };
        })),
        ("SW_SHOWMINNOACTIVE", Box::new(|h| {
            let _ = unsafe { ShowWindow(h, SW_SHOWMINNOACTIVE) };
        })),
        ("SW_FORCEMINIMIZE", Box::new(|h| {
            let _ = unsafe { ShowWindow(h, SW_FORCEMINIMIZE) };
        })),
        ("WM_SYSCOMMAND/SC_MINIMIZE", Box::new(|h| {
            let _ = unsafe {
                PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0))
            };
        })),
        ("SW_SHOWMINNOACTIVE (async)", Box::new(|h| {
            let _ = unsafe { ShowWindowAsync(h, SW_SHOWMINNOACTIVE) };
        })),
        ("SW_MINIMIZE + transitions off", Box::new(|h| {
            let value: BOOL = true.into();
            // SAFETY: значение живёт до конца вызова; размер соответствует
            // типу атрибута (BOOL).
            let _ = unsafe {
                DwmSetWindowAttribute(
                    h,
                    DWMWA_TRANSITIONS_FORCEDISABLED,
                    (&raw const value).cast(),
                    size_of::<BOOL>() as u32,
                )
            };
            let _ = unsafe { ShowWindow(h, SW_MINIMIZE) };
        })),
    ];

    // Два сценария: (A) активное окно — чужое (fg), сворачиваем target;
    // (B) активное окно — сам target (сворачиваемое): именно тут
    // SW_MINIMIZE документированно «активирует следующее окно».
    for (label, active) in [("АКТИВНО чужое окно", fg.0), ("АКТИВЕН сам target", target.0)] {
        println!("  -- сценарий: {label} --");
        for (name, call) in &methods {
            let rep = measure_minimize(name, active, bystander.0, target.0, call.as_ref());
            println!(
                "  {:32} fg {:08X}->{:08X} {}  свидетель актив.= {}  iconic={} visible={}  высоты: {:?}",
                rep.method,
                rep.fg_before.0 as usize,
                rep.fg_after.0 as usize,
                if rep.fg_changed { "СМЕНИЛСЯ" } else { "(тот же)" },
                if rep.bystander_activated { "КРАЖА" } else { "нет" },
                rep.iconic,
                rep.visible,
                rep.heights_seen,
            );
        }
    }
    // Снять атрибут transitions off после замера (мы его ставили).
    let value: BOOL = false.into();
    // SAFETY: значение живёт до конца вызова.
    let _ = unsafe {
        DwmSetWindowAttribute(
            target.0,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const value).cast(),
            size_of::<BOOL>() as u32,
        )
    };
    drop(target);
    drop(fg);
    println!();

    // --- 1a. Диагностика SW_FORCEMINIMIZE и SC_MINIMIZE по шагам ---
    println!("--- 1a. SW_FORCEMINIMIZE / SC_MINIMIZE по шагам (тайминг состояния) ---");
    for (name, call) in [
        (
            "SW_FORCEMINIMIZE",
            Box::new(|h: HWND| {
                // SAFETY: окно живо; штатный show-command.
                let _ = unsafe { ShowWindow(h, SW_FORCEMINIMIZE) };
            }) as Box<dyn Fn(HWND)>,
        ),
        (
            "SC_MINIMIZE",
            Box::new(|h: HWND| {
                // SAFETY: окно живо; системная команда сворачивания.
                let _ = unsafe {
                    PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0))
                };
            }) as Box<dyn Fn(HWND)>,
        ),
    ] {
        let w = ProbeWindow::create("g2_diag", WS_OVERLAPPED | WS_VISIBLE);
        pump(150);
        let _ = call(w.0);
        for t in [0u64, 30, 100, 400] {
            if t > 0 {
                pump(t);
            }
            println!(
                "  {name:24} через {t:>4} мс: iconic={} visible={} rect={:?}",
                iconic(w.0),
                visible(w.0),
                rect_of(w.0)
            );
        }
        // SAFETY: окно живо; возвращаем в нормальное состояние.
        let _ = unsafe { ShowWindow(w.0, SW_RESTORE) };
        pump(200);
        println!(
            "  {name:24} после SW_RESTORE:     iconic={} visible={}",
            iconic(w.0),
            visible(w.0)
        );
    }
    println!();

    // --- 2. Разворачивание: возврат в прежний прямоугольник ---
    println!("--- 2. Разворачивание: прямоугольник ---");
    let target = ProbeWindow::create("g2_target2", WS_OVERLAPPED | WS_VISIBLE);
    pump(150);
    // SAFETY: окно живо; известный прямоугольник без z-order/активации.
    let _ = unsafe {
        SetWindowPos(
            target.0,
            None,
            123,
            456,
            731,
            519,
            SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
        )
    };
    pump(100);
    let before = rect_of(target.0);
    println!(
        "  прямоугольник до сворачивания: L{} T{} R{} B{} ({}x{})",
        before.left,
        before.top,
        before.right,
        before.bottom,
        before.right - before.left,
        before.bottom - before.top
    );
    // SAFETY: окно живо; SW_SHOWMINNOACTIVE — выбранный способ сворачивания.
    let _ = unsafe { ShowWindowAsync(target.0, SW_SHOWMINNOACTIVE) };
    pump(300);
    println!("  после сворачивания: iconic={}", iconic(target.0));

    // SAFETY: окно живо; SW_RESTORE — синхронный путь.
    let _ = unsafe { ShowWindow(target.0, SW_RESTORE) };
    pump(300);
    let sync_restore = rect_of(target.0);
    println!(
        "  SW_RESTORE (синхр.):   L{} T{} R{} B{} -> {}",
        sync_restore.left,
        sync_restore.top,
        sync_restore.right,
        sync_restore.bottom,
        if sync_restore == before {
            "ТОЧНОЕ совпадение"
        } else {
            "ОТЛИЧАЕТСЯ"
        }
    );

    // SAFETY: окно живо; снова сворачиваем и разворачиваем асинхронно.
    let _ = unsafe { ShowWindowAsync(target.0, SW_SHOWMINNOACTIVE) };
    pump(300);
    let _ = unsafe { ShowWindowAsync(target.0, SW_RESTORE) };
    pump(300);
    let async_restore = rect_of(target.0);
    println!(
        "  SW_RESTORE (async):    L{} T{} R{} B{} -> {}",
        async_restore.left,
        async_restore.top,
        async_restore.right,
        async_restore.bottom,
        if async_restore == before {
            "ТОЧНОЕ совпадение"
        } else {
            "ОТЛИЧАЕТСЯ"
        }
    );

    // НЕ свёрнутое окно: SW_RESTORE — no-op по прямоугольнику.
    let before_noop = rect_of(target.0);
    // SAFETY: окно живо (и не свёрнуто).
    let _ = unsafe { ShowWindow(target.0, SW_RESTORE) };
    pump(100);
    let after_noop = rect_of(target.0);
    println!(
        "  SW_RESTORE на НЕ свёрнутом: {}",
        if before_noop == after_noop {
            "прямоугольник не тронут"
        } else {
            "прямоугольник ИЗМЕНИЛСЯ"
        }
    );
    drop(target);
    println!();

    // --- 2b. Гонка «развернуть (async) и поднять»: в каком порядке вызовы,
    // чтобы окно гарантированно осталось НАВЕРХУ после разворачивания ---
    println!("--- 2b. Порядок разворачивания и подъёма ---");
    let target = ProbeWindow::create("g2_target2b", WS_OVERLAPPED | WS_VISIBLE);
    let peer = ProbeWindow::create("g2_peer", WS_OVERLAPPED | WS_VISIBLE);
    pump(150);
    // SAFETY: окно живо; peer поднимаем наверх — эталон «ниже верха».
    let _ = unsafe {
        SetWindowPos(
            peer.0,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(150);
    println!(
        "  до сворачивания: target НАД peer = {}",
        if window_above(target.0, peer.0) { "ДА" } else { "НЕТ" }
    );

    for (label, order) in [
        (
            "A: restore (async), затем сразу raise",
            Box::new(|t: HWND| {
                let _ = unsafe { ShowWindowAsync(t, SW_RESTORE) };
                let _ = unsafe {
                    SetWindowPos(t, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE)
                };
            }) as Box<dyn Fn(HWND)>,
        ),
        (
            "B: raise, затем restore (async)",
            Box::new(|t: HWND| {
                let _ = unsafe {
                    SetWindowPos(t, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE)
                };
                let _ = unsafe { ShowWindowAsync(t, SW_RESTORE) };
            }) as Box<dyn Fn(HWND)>,
        ),
        (
            "C: restore (async), ждём !iconic, затем raise",
            Box::new(|t: HWND| {
                let _ = unsafe { ShowWindowAsync(t, SW_RESTORE) };
                let deadline = Instant::now() + Duration::from_millis(500);
                while Instant::now() < deadline {
                    pump(5);
                    if !iconic(t) {
                        break;
                    }
                }
                let _ = unsafe {
                    SetWindowPos(t, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE)
                };
            }) as Box<dyn Fn(HWND)>,
        ),
    ] {
        // SAFETY: окно живо; сворачиваем (async) перед каждой пробой.
        let _ = unsafe { ShowWindowAsync(target.0, SW_SHOWMINNOACTIVE) };
        pump(250);
        assert!(iconic(target.0), "предусловие: окно свёрнуто");
        // SAFETY: окно живо; peer снова наверху — эталон для проверки.
        let _ = unsafe {
            SetWindowPos(
                peer.0,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            )
        };
        pump(100);
        order(target.0);
        pump(600);
        println!(
            "  {label}: target НАД peer = {}",
            if window_above(target.0, peer.0) { "ДА" } else { "НЕТ" }
        );
    }
    drop(target);
    drop(peer);
    println!();

    // --- 3. Подъём поверх полноэкранного окна ---
    println!("--- 3. Подъём поверх полноэкранного окна ---");
    let target = ProbeWindow::create("g2_target3", WS_OVERLAPPED | WS_VISIBLE);
    // Полноэкранное окно-«программа»: borderless popup во весь монитор.
    let fs = ProbeWindow::create("g2_fs", WS_POPUP | WS_VISIBLE);
    println!(
        "  окна пробы: target=0x{:X} fs=0x{:X}",
        target.0.0 as usize,
        fs.0.0 as usize
    );
    // SAFETY: окно живо; растягиваем во весь монитор (как borderless fullscreen).
    let _ = unsafe {
        SetWindowPos(
            fs.0,
            Some(HWND_TOP),
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
            windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
        )
    };
    pump(200);
    println!(
        "  полноэкранное окно topmost-стиль: {}",
        if has_topmost_style(fs.0) {
            "ДА"
        } else {
            "нет (обычная полоса)"
        }
    );
    println!(
        "  до подъёма: target НАД полноэкранным = {}",
        if window_above(target.0, fs.0) { "ДА" } else { "НЕТ" }
    );
    println!(
        "  стиль target до подъёма: WS_EX_TOPMOST = {}",
        if has_topmost_style(target.0) { "ДА" } else { "НЕТ" }
    );

    // Подъём через HWND_TOP (обычная полоса, полноэкранное НЕ topmost).
    // SAFETY: окно живо; HWND_TOP + без активации/движения/ресайза.
    let _ = unsafe {
        SetWindowPos(
            target.0,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(200);
    println!(
        "  после HWND_TOP: target НАД полноэкранным = {}",
        if window_above(target.0, fs.0) { "ДА" } else { "НЕТ" }
    );
    println!(
        "  стиль target после HWND_TOP: WS_EX_TOPMOST = {}",
        if has_topmost_style(target.0) { "ДА" } else { "НЕТ" }
    );

    // Полноэкранное окно само становится topmost (игроки/презентации так
    // делают) — HWND_TOP больше не должен пробивать topmost-полосу.
    // SAFETY: окно живо; HWND_TOPMOST ставит стиль.
    let _ = unsafe {
        SetWindowPos(
            fs.0,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(200);
    // SAFETY: окно живо; HWND_TOP.
    let _ = unsafe {
        SetWindowPos(
            target.0,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(200);
    println!(
        "  полноэкранное = topmost, после HWND_TOP: target НАД ним = {}",
        if window_above(target.0, fs.0) { "ДА" } else { "НЕТ" }
    );

    // Приём «кратковременный TOPMOST → немедленный NOTOPMOST».
    println!("  --- приём TOPMOST → NOTOPMOST (полноэкранное = topmost) ---");
    // SAFETY: окно живо; временный topmost.
    let _ = unsafe {
        SetWindowPos(
            target.0,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(150);
    println!(
        "  стиль ПОСЛЕ TOPMOST (до снятия): WS_EX_TOPMOST = {}",
        if has_topmost_style(target.0) { "ДА" } else { "НЕТ" }
    );
    // SAFETY: окно живо; немедленный NOTOPMOST.
    let _ = unsafe {
        SetWindowPos(
            target.0,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(200);
    println!(
        "  после NOTOPMOST: target НАД полноэкранным = {}",
        if window_above(target.0, fs.0) { "ДА" } else { "НЕТ" }
    );
    println!(
        "  стиль ПОСЛЕ NOTOPMOST (итог): WS_EX_TOPMOST = {}",
        if has_topmost_style(target.0) { "ДА" } else { "НЕТ" }
    );

    // Тот же приём, но полноэкранное окно в ОБЫЧНОЙ полосе (сняли topmost
    // с fs): после TOPMOST→NOTOPMOST окно обязано остаться над ним.
    // SAFETY: окно живо; снимаем topmost с полноэкранного.
    let _ = unsafe {
        SetWindowPos(
            fs.0,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(200);
    // SAFETY: окно живо; приём TOPMOST → NOTOPMOST.
    let _ = unsafe {
        SetWindowPos(
            target.0,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    let _ = unsafe {
        SetWindowPos(
            target.0,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(200);
    println!(
        "  полноэкранное = обычная полоса, TOPMOST→NOTOPMOST: target НАД ним = {}",
        if window_above(target.0, fs.0) { "ДА" } else { "НЕТ" }
    );
    println!(
        "  стиль target в итоге: WS_EX_TOPMOST = {}",
        if has_topmost_style(target.0) { "ДА" } else { "НЕТ" }
    );
    drop(target);
    drop(fs);
    println!();
    println!("Готово.");
}