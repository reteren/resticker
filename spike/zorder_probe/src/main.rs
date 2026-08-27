//! Проба B4: Z-порядок при подъёме нескольких окон группы.
//!
//! Показ группы поднимает окна по одному: каждому `SetWindowPos(HWND_TOP,
//! SWP_NOACTIVATE|SWP_NOMOVE|SWP_NOSIZE)` (WindowPins::raise_without_topmost),
//! затем окну первого слота отдают фокус (SetForegroundWindow).
//!
//! ПЕРВЫЙ ЗАМЕР (запуск без права на передний план) дал неожиданный
//! результат: HWND_TOP-подъём вообще НЕ двигал окна, пока активным оставалось
//! чужое (пользовательское) окно, — тогда как SetForegroundWindow после
//! получения права (SendInput) окно поднял. Поэтому проба разбита на две
//! серии: (A) без права на передний план — что происходит; (B) с правом
//! (как у resticker, получившего глобальный хоткей) — поднимаются ли ВСЕ
//! четыре выше активного окна пользователя, в каком порядке, и не
//! возвращает ли Windows активное окно наверх.
//!
//! Все окна — СОБСТВЕННЫЕ: родитель запускает дочерние процессы пробы, каждый
//! создаёт одно окно (разные процессы, как у настоящей группы). Чужие окна
//! пользователя не трогаются (только читается ранг активного окна), фокус
//! пользователю возвращается в конце каждого замера, где был отобран.
//!
//! Режимы:
//!   zorder_probe.exe                     — родитель, гоняет все замеры;
//!   zorder_probe.exe --child --name <N> [--fullscreen] --event <EV>
//!       — дочерний процесс: создаёт одно окно, печатает READY <имя> <hwnd>,
//!         ждёт сигнала именованного события, по сигналу закрывает окно.
//!
//! Разведочный инструмент, в workspace не входит (spike исключён).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, HANDLE, LPARAM, LRESULT, WPARAM, WAIT_OBJECT_0};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    KEYEVENTF_KEYUP, VK_MENU, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetForegroundWindow, GetSystemMetrics, GetTopWindow, GetWindow, GetWindowLongPtrW,
    GetWindowThreadProcessId, GW_HWNDNEXT, GW_HWNDPREV, GWL_EXSTYLE, HWND_TOP, IsIconic, IsWindow,
    IsWindowVisible, MSG, MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, PM_REMOVE,
    PeekMessageW, PostMessageW, QS_ALLINPUT, RegisterClassExW, SetForegroundWindow, SetWindowPos,
    ShowWindowAsync, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, TranslateMessage, WM_CLOSE, WS_EX_TOPMOST,
    WS_OVERLAPPED, WS_POPUP, WS_VISIBLE, WINDOW_STYLE, WNDCLASSEXW,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

/// Подъём ровно как в производстве: WindowPins::raise_without_topmost —
/// `SetWindowPos(HWND_TOP)` без активации/движения/ресайза. Возвращает
/// ошибку SetWindowPos (если есть), чтобы было видно, что именно не вышло.
fn raise_no_activate(hwnd: HWND) -> Option<String> {
    // SAFETY: окно живо (создано пробой); флаги — без активации/движения/ресайза.
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    }
    .err()
    .map(|e| format!("{:?} {}", e.code(), e.message()))
}

/// Все видимые top-level окна в z-порядке сверху вниз (GetTopWindow(NULL) →
/// GW_HWNDNEXT). Topmost-полоса попадает в начало списка.
fn visible_top_level() -> Vec<HWND> {
    let mut v = Vec::new();
    // SAFETY: GetTopWindow — чтение z-order, безопасен с NULL.
    let mut cur = unsafe { GetTopWindow(None) }.ok();
    while let Some(h) = cur {
        // SAFETY: IsWindowVisible безопасен для любых хэндлов.
        if unsafe { IsWindowVisible(h) }.as_bool() {
            v.push(h);
        }
        // SAFETY: GetWindow — чтение z-order, безопасен для любых хэндлов.
        cur = unsafe { GetWindow(h, GW_HWNDNEXT) }.ok().filter(|x| !x.0.is_null());
    }
    v
}

fn rank_of(hwnd: HWND) -> usize {
    visible_top_level()
        .iter()
        .position(|w| *w == hwnd)
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Ранги всех `probes` из ОДНОГО снимка (без гонок между окнами).
fn snapshot_ranks(probes: &[HWND]) -> Vec<usize> {
    let all = visible_top_level();
    probes
        .iter()
        .map(|h| {
            all.iter()
                .position(|w| *w == *h)
                .map(|i| i + 1)
                .unwrap_or(0)
        })
        .collect()
}

/// Класс и заголовок окна (для опознания чужих окон в z-order).
fn class_and_title(hwnd: HWND) -> String {
    let mut class = [0u16; 128];
    let mut title = [0u16; 256];
    // SAFETY: GetClassNameW/GetWindowTextW — чтение, безопасно для любых хэндлов.
    let cl = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut class) };
    let tl = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut title) };
    let c = String::from_utf16_lossy(&class[..cl.max(0) as usize]);
    let t = String::from_utf16_lossy(&title[..tl.max(0) as usize]);
    format!("'{t}' [{c}]")
}

/// Дамп z-order сверху вниз (первые `n` видимых top-level окон) с именами —
/// чтобы видеть, ЧТО именно стоит выше наших окон.
fn dump_z(label: &str, n: usize) {
    let all = visible_top_level();
    println!("  {label}: первые {n} окон сверху:");
    for (i, h) in all.iter().take(n).enumerate() {
        println!("    {:2}. 0x{:08X} {}", i + 1, h.0 as usize, class_and_title(*h));
    }
}

/// Печать порядка: ранги (1 = самый верх) среди ВСЕХ видимых top-level окон,
/// отсортированы сверху вниз — это и есть фактический z-порядок.
/// ВАЖНО: все ранги берутся из ОДНОГО снимка z-order.
fn print_z(label: &str, probes: &[(&str, HWND)]) {
    let all = visible_top_level();
    let mut ranks: Vec<(&str, usize)> = probes
        .iter()
        .filter_map(|(n, hwnd)| {
            all.iter()
                .position(|w| *w == *hwnd)
                .map(|i| (*n, i + 1))
        })
        .collect();
    ranks.sort_by_key(|(_, r)| *r);
    let chain: Vec<String> = ranks.iter().map(|(n, r)| format!("{n}[{r}]")).collect();
    println!("  {label}: {}", chain.join(" > "));
}

/// pid процесса, владеющего окном (доказательство «разные процессы»).
fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: чтение pid владельца окна, безопасно для любого хэндла.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Сделать окно переднего плана: прямой SetForegroundWindow (система может
/// отказать), при отказе — классический трюк SendInput(Alt)+SetForegroundWindow.
fn force_foreground(hwnd: HWND) -> &'static str {
    // SAFETY: AllowSetForegroundWindow(ASFW_ANY) — снятие ограничения
    // переднего плана для всех процессов.
    unsafe {
        let _ = AllowSetForegroundWindow(0xFFFF_FFFF);
    }
    pump(50);
    // SAFETY: SetForegroundWindow принимает любой живой HWND.
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        pump(150);
        if unsafe { GetForegroundWindow() } == hwnd {
            return "SetForegroundWindow напрямую";
        }
    }
    send_alt();
    pump(60);
    // SAFETY: SetForegroundWindow принимает любой живой HWND.
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        pump(150);
        if unsafe { GetForegroundWindow() } == hwnd {
            return "SendInput(Alt)+SetForegroundWindow";
        }
    }
    "НЕ получилось (fg остался прежним)"
}

/// Фейковое нажатие Alt: даёт вызывающему процессу право на передний план
/// (то же право, что resticker получает вместе с глобальным хоткеем).
fn send_alt() {
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: unsafe {
                INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        wScan: 0,
                        dwFlags: Default::default(),
                        time: 0,
                        dwExtraInfo: 0,
                    },
                }
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: unsafe {
                INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                }
            },
        },
    ];
    // SAFETY: inputs — корректный массив структур INPUT.
    unsafe {
        let _ = SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Вернуть передний план окну пользователя, если мы его отобрали.
fn restore_user_foreground(user_fg: HWND) {
    // SAFETY: AllowSetForegroundWindow(ASFW_ANY) + SetForegroundWindow —
    // best effort, отказ не страшен.
    unsafe {
        let _ = AllowSetForegroundWindow(0xFFFF_FFFF);
        let _ = SetForegroundWindow(user_fg);
    }
    pump(100);
}

fn iconic(hwnd: HWND) -> bool {
    // SAFETY: IsIconic безопасен для любых значений.
    unsafe { IsIconic(hwnd) }.as_bool()
}

// --- дочерний процесс: одно окно ---

struct ProbeChild {
    name: String,
    hwnd: HWND,
    proc: Option<Child>,
    event: HANDLE,
}

impl ProbeChild {
    fn kill(&mut self) {
        // SAFETY: SetEvent — сигнал именованного события, свой хэндл.
        unsafe {
            let _ = SetEvent(self.event);
        }
        if let Some(mut proc) = self.proc.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if proc.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = proc.kill();
            let _ = proc.wait();
        }
        // SAFETY: CloseHandle — собственный хэндл события.
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Запустить дочерний процесс пробы, создать его окно, дождаться READY.
/// `raise_ms` — дочерний процесс сам поднимет своё окно через это число мс
/// (замер «окно поднимает свой процесс»).
fn spawn_child(name: &str, fullscreen: bool) -> ProbeChild {
    spawn_child_ex(name, fullscreen, None)
}

fn spawn_child_ex(name: &str, fullscreen: bool, raise_ms: Option<u64>) -> ProbeChild {
    let exe = std::env::current_exe().expect("текущий exe");
    let event_name = format!("zorder_probe_{}_{}", std::process::id(), name);
    let mut cmd = Command::new(&exe);
    cmd.arg("--child")
        .arg("--name")
        .arg(name)
        .arg("--event")
        .arg(&event_name);
    if fullscreen {
        cmd.arg("--fullscreen");
    }
    if let Some(ms) = raise_ms {
        cmd.arg("--raise-ms").arg(ms.to_string());
    }
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut proc = cmd.spawn().expect("запуск дочернего процесса");

    let stdout = proc.stdout.take().expect("stdout дочернего");
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("READY от дочернего процесса");
    // "READY <имя> 0xHWND"
    let parts: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(parts.len(), 3, "неожиданный READY: {line}");
    let raw = usize::from_str_radix(parts[2].trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("не-HWND в READY: {line}"));
    let hwnd = HWND(raw as *mut core::ffi::c_void);

    let wide: Vec<u16> = event_name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: CreateEventW с валидным именем; общий с дочерним процессом.
    let event = unsafe { CreateEventW(None, true, false, PCWSTR(wide.as_ptr())) }.expect("event");

    ProbeChild {
        name: name.to_string(),
        hwnd,
        proc: Some(proc),
        event,
    }
}

unsafe extern "system" fn probe_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: делегирование системному обработчику.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn run_child(name: &str, fullscreen: bool, event_name: &str, raise_ms: Option<u64>) -> ! {
    // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
    let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
    let class_wide: Vec<u16> = format!("zorder_{name}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let class_pw = PCWSTR(class_wide.as_ptr());
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(probe_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: class_pw,
        ..Default::default()
    };
    // SAFETY: wc заполнена корректно; повторная регистрация класса — не ошибка.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        let _err = unsafe { windows::Win32::Foundation::GetLastError() };
    }
    let title_wide: Vec<u16> = format!("zorder-{name}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let (x, y, w, h, style): (i32, i32, i32, i32, WINDOW_STYLE) = if fullscreen {
        // SAFETY: GetSystemMetrics — чтение параметров системы.
        (
            0,
            0,
            unsafe { GetSystemMetrics(SM_CXSCREEN) },
            unsafe { GetSystemMetrics(SM_CYSCREEN) },
            WS_POPUP | WS_VISIBLE,
        )
    } else {
        (60, 60, 260, 170, WS_OVERLAPPED | WS_VISIBLE)
    };
    // SAFETY: валидные константы и зарегистрированный класс.
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_pw,
            PCWSTR(title_wide.as_ptr()),
            style,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
    }
    .expect("создание окна дочернего процесса");
    println!("READY {name} 0x{:X}", hwnd.0 as usize);
    let _ = std::io::stdout().flush();

    if let Some(ms) = raise_ms {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            let mut msg = MSG::default();
            // SAFETY: msg — валидный буфер.
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                // SAFETY: msg пришёл из PeekMessageW.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    let _ = DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: окно живо; подъём собственного окна — как raise_without_topmost.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            )
        };
        println!("SELFRAISE {name} 0x{:X}", hwnd.0 as usize);
        let _ = std::io::stdout().flush();
    }

    let ev_wide: Vec<u16> = event_name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: CreateEventW с валидным именем; общий с родителем.
    let event = unsafe { CreateEventW(None, true, false, PCWSTR(ev_wide.as_ptr())) }.expect("event");

    let deadline_exit = Instant::now() + Duration::from_secs(600);
    loop {
        // SAFETY: event — валидный хэндл; QS_ALLINPUT будит по сообщениям.
        let r = unsafe {
            MsgWaitForMultipleObjectsEx(Some(&[event]), 200, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if r == WAIT_OBJECT_0 {
            break; // родитель велел закрыться
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
        // SAFETY: IsWindow безопасен для любых значений.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            break;
        }
        if Instant::now() > deadline_exit {
            break;
        }
    }
    // SAFETY: PostMessageW — безопасен для любого живого HWND.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    // Дождаться уничтожения (WM_CLOSE → DefWindowProc → DestroyWindow).
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let mut msg = MSG::default();
        // SAFETY: msg — валидный буфер.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: msg пришёл из PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
        // SAFETY: IsWindow безопасен для любых значений.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::process::exit(0);
}

// --- замеры ---

/// Колбэк EnumWindows: считает top-level окна сверху вниз и фиксирует номер
/// окна, переданного через lparam.
extern "system" fn enum_cb(
    hwnd: HWND,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    // SAFETY: lparam — указатель на (счётчик, искомый hwnd), создан нами.
    unsafe {
        let p = &mut *(lparam.0 as *mut (usize, usize));
        p.0 += 1;
        if hwnd.0 as usize == p.1 {
            // Окно найдено: счётчик уже хранит его позицию. Останавливаться
            // не нужно — позиция зафиксирована.
        }
    }
    windows::core::BOOL(1)
}

/// Санитарный замер: что именно делает SetWindowPos в этой среде.
fn test_g_sanity() {
    println!("\n=== G. Санитария: SetWindowPos в этой среде ===");
    // SAFETY: GetForegroundWindow — чтение.
    let user_fg = unsafe { GetForegroundWindow() };
    let user_pid = pid_of(user_fg);
    let mut our_session = 0;
    // SAFETY: ProcessIdToSessionId — чтение системной информации.
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut our_session) };
    let mut user_session = 0;
    // SAFETY: ProcessIdToSessionId — чтение системной информации.
    unsafe {
        ProcessIdToSessionId(user_pid, &mut user_session);
    }
    println!(
        "  сессия пробы: {our_session} (ok={}); сессия активного окна пользователя: {user_session} (pid {user_pid})",
        ok.is_ok()
    );

    let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
    let mut mk = |name: &str| -> HWND {
        let class_wide: Vec<u16> = format!("zorder_{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let class_pw = PCWSTR(class_wide.as_ptr());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(probe_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_pw,
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация — не ошибка.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            let _err = unsafe { windows::Win32::Foundation::GetLastError() };
        }
        let title: Vec<u16> = format!("zorder-{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: валидные константы и зарегистрированный класс.
        unsafe {
            CreateWindowExW(
                Default::default(),
                class_pw,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPED | WS_VISIBLE,
                100,
                100,
                240,
                150,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание окна")
    };
    let a = mk("sa");
    let b = mk("sb");
    pump(150);
    // Что непосредственно над/под окном (GW_HWNDPREV / GW_HWNDNEXT).
    let prev = |h: HWND| unsafe { GetWindow(h, GW_HWNDPREV) }.ok().filter(|x| !x.0.is_null());
    let next = |h: HWND| unsafe { GetWindow(h, GW_HWNDNEXT) }.ok().filter(|x| !x.0.is_null());
    let show_peers = |label: &str, h: HWND| {
        println!(
            "    {label}: над 0x{:X}={:?}, под ={:?}, ранг={}",
            h.0 as usize,
            prev(h).map(|p| p.0 as usize),
            next(h).map(|n| n.0 as usize),
            rank_of(h)
        );
    };
    println!("  окна: A=0x{:X}, B=0x{:X}", a.0 as usize, b.0 as usize);
    show_peers("A (создано первым)", a);
    show_peers("B (создано вторым)", b);

    // EnumWindows-кросс-проверка ранга A.
    let mut counter = 0usize;
    // SAFETY: EnumWindows с валидным колбэком; безопасен всегда.
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(enum_cb),
            windows::Win32::Foundation::LPARAM(
                (&mut (counter, a.0 as usize)) as *mut _ as isize,
            ),
        )
    };
    // EnumWindows идёт сверху вниз: позиция A в этом списке.
    println!(
        "  кросс-проверка EnumWindows: A = {counter}-й top-level (GetWindow-ранг {})",
        rank_of(a)
    );

    println!("  --- op1: SetWindowPos(A, HWND_TOP, NOACTIVATE) ---");
    let r = unsafe {
        SetWindowPos(
            a,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    println!("    результат: {:?}", r.err().map(|e| format!("{:?} {}", e.code(), e.message())));
    show_peers("A сразу после вызова (t=0)", a);
    pump(50);
    show_peers("A через 50 мс", a);
    pump(400);
    show_peers("A через ~450 мс", a);

    println!("  --- op2: SetWindowPos(A, над B, NOACTIVATE) — подъём над конкретным соседом ---");
    let r = unsafe {
        SetWindowPos(
            a,
            Some(b),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    println!("    результат: {:?}", r.err().map(|e| format!("{:?} {}", e.code(), e.message())));
    show_peers("A сразу после вызова (t=0)", a);
    pump(150);
    show_peers("A через 150 мс", a);

    println!("  --- op3: SetWindowPos(A, HWND_TOP, БЕЗ NOACTIVATE) ---");
    let r = unsafe {
        SetWindowPos(a, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE)
    };
    println!("    результат: {:?}", r.err().map(|e| format!("{:?} {}", e.code(), e.message())));
    show_peers("A сразу после вызова (t=0)", a);
    pump(150);
    show_peers("A через 150 мс", a);
    println!(
        "    активное окно стало A? {}",
        unsafe { GetForegroundWindow() } == a
    );

    // Уборка: вернуть передний план пользователю, если мы его отобрали.
    restore_user_foreground(user_fg);
    // SAFETY: окна пробы живые.
    let _ = unsafe { DestroyWindow(a) };
    let _ = unsafe { DestroyWindow(b) };
    pump(100);
}

/// Тайминг подъёма: куда реально встаёт окно и не возвращается ли оно
/// обратно (и что в это время делает активное окно пользователя).
fn test_h_timing() {
    println!("\n=== H. Тайминг подъёма: t=0 … t=1000 ===");
    let user_fg = unsafe { GetForegroundWindow() };
    println!(
        "  активное окно пользователя: 0x{:X} (pid {})",
        user_fg.0 as usize,
        pid_of(user_fg)
    );
    dump_z("до создания", 6);

    let g1 = spawn_child("HG1", false);
    let f = spawn_child("HF", false); // F создан позже — наверху
    pump(200);
    println!(
        "  до подъёма: ранги из одного снимка: G1={}, F={}, USER={}",
        snapshot_ranks(&[g1.hwnd])[0],
        snapshot_ranks(&[f.hwnd])[0],
        snapshot_ranks(&[user_fg])[0]
    );
    dump_z("до подъёма (кто выше)", 8);

    raise_no_activate(g1.hwnd);
    for (label, delay) in [
        ("t=0", 0u64),
        ("t=5", 5),
        ("t=15", 15),
        ("t=40", 40),
        ("t=100", 100),
        ("t=300", 300),
        ("t=1000", 1000),
    ] {
        if delay > 0 {
            pump(delay);
        }
        let r = snapshot_ranks(&[g1.hwnd, f.hwnd, user_fg]);
        println!("  {label}: G1={} F={} USER={}", r[0], r[1], r[2]);
    }
    dump_z("после подъёма и ожидания (кто выше G1)", 8);

    // Четыре окна подряд — та же серия, сразу после подъёма всех.
    println!("  --- четыре окна: подъём G2→G3→G4 (прямой порядок), замеры ---");
    let set: Vec<ProbeChild> = (2..=4)
        .map(|i| spawn_child(&format!("HG{i}b"), false))
        .collect();
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    pump(200);
    for h in &g {
        raise_no_activate(*h);
        pump(0);
    }
    let r0 = snapshot_ranks(&[g[0], g[1], g[2], f.hwnd, user_fg]);
    println!(
        "  сразу после 3 подъёмов (t=0): G2={} G3={} G4={} F={} USER={}",
        r0[0], r0[1], r0[2], r0[3], r0[4]
    );
    pump(500);
    let r1 = snapshot_ranks(&[g[0], g[1], g[2], f.hwnd, user_fg]);
    println!(
        "  через 500 мс: G2={} G3={} G4={} F={} USER={}",
        r1[0], r1[1], r1[2], r1[3], r1[4]
    );
    restore_user_foreground(user_fg);
}

/// Изолированный замер: кто кого может поднять. Дампится ВЕСЬ верх z-order
/// с именами — видно и активное окно пользователя, и рестикер-оверлеи.
fn test_i_isolate() {
    println!("\n=== I. Изоляция: same-process / cross-process / self-raise ===");
    let user_fg = unsafe { GetForegroundWindow() };
    println!(
        "  активное окно пользователя: 0x{:X} (pid {})",
        user_fg.0 as usize,
        pid_of(user_fg)
    );
    dump_z("старт", 10);

    // --- I1: same-process raise ---
    let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
    let mut mk = |name: &str| -> HWND {
        let class_wide: Vec<u16> = format!("zorder_{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let class_pw = PCWSTR(class_wide.as_ptr());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(probe_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_pw,
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация — не ошибка.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            let _err = unsafe { windows::Win32::Foundation::GetLastError() };
        }
        let title: Vec<u16> = format!("zorder-{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: валидные константы и зарегистрированный класс.
        unsafe {
            CreateWindowExW(
                Default::default(),
                class_pw,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPED | WS_VISIBLE,
                120,
                120,
                240,
                150,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание окна")
    };
    let a = mk("ia");
    let b = mk("ib");
    pump(100);
    println!("  I1: same-process: A=0x{:X} B=0x{:X}", a.0 as usize, b.0 as usize);
    dump_z("I1 до подъёма A", 10);
    raise_no_activate(a);
    dump_z("I1 после подъёма A (t=0)", 10);
    pump(400);
    dump_z("I1 через 400 мс", 10);

    // --- I2: cross-process raise (родитель поднимает окно ребёнка) ---
    let c1 = spawn_child("IC1", false);
    pump(150);
    println!("  I2: cross-process: C1=0x{:X}", c1.hwnd.0 as usize);
    dump_z("I2 до подъёма C1", 10);
    raise_no_activate(c1.hwnd);
    dump_z("I2 после подъёма C1 (t=0)", 10);
    pump(400);
    dump_z("I2 через 400 мс", 10);

    // --- I3: self-raise (ребёнок поднимает своё окно сам) ---
    let c2 = spawn_child_ex("IC2SELF", false, Some(500));
    pump(150);
    println!("  I3: self-raise: C2=0x{:X}", c2.hwnd.0 as usize);
    dump_z("I3 до self-raise C2", 10);
    pump(600);
    dump_z("I3 после self-raise C2 (600 мс)", 10);

    // --- I4: cross-process raise с правом на передний план (send_alt) ---
    let c3 = spawn_child("IC3LOCK", false);
    pump(150);
    send_alt();
    pump(80);
    println!("  I4: cross-process с локом: C3=0x{:X}", c3.hwnd.0 as usize);
    dump_z("I4 до подъёма C3", 10);
    raise_no_activate(c3.hwnd);
    dump_z("I4 после подъёма C3 (t=0)", 10);
    pump(400);
    dump_z("I4 через 400 мс", 10);
    restore_user_foreground(user_fg);
}

/// Решающий замер: где ОСТАНАВЛИВАЕТСЯ подъём (потолок = активное окно?),
/// что происходит с окном, которое ПОДНИМАЮТ, когда оно выше активного окна,
/// и не вмешивается ли система в z-order активного окна.
fn test_j_cap() {
    println!("\n=== J. Потолок активного окна: где останавливается подъём ===");
    let user_fg = unsafe { GetForegroundWindow() };
    println!(
        "  активное окно пользователя: 0x{:X} (pid {})",
        user_fg.0 as usize,
        pid_of(user_fg)
    );

    // J1: окно ЗАКОПАНО (свёрнуто → низ z-order), подъём снаружи (cross-process).
    let c1 = spawn_child("JC1", false);
    pump(150);
    // SAFETY: ShowWindowAsync безопасен для живых окон пробы.
    let _ = unsafe { ShowWindowAsync(c1.hwnd, SW_SHOWMINNOACTIVE) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !iconic(c1.hwnd) {
        pump(5);
    }
    println!(
        "  J1: C1 свёрнут={} (ушёл в низ z-order)",
        iconic(c1.hwnd)
    );
    dump_z("J1 до подъёма C1", 8);
    raise_no_activate(c1.hwnd);
    dump_z("J1 после подъёма C1 (t=0)", 8);
    pump(300);
    dump_z("J1 через 300 мс", 8);

    // J2: окно ВЫШЕ активного, подъём снаружи (cross-process) — утонет ли?
    let c2 = spawn_child("JC2", false);
    pump(150);
    dump_z("J2: C2 создан (создание кладёт окно выше активного)", 8);
    raise_no_activate(c2.hwnd);
    dump_z("J2 после подъёма C2 (t=0)", 8);
    for d in [50u64, 200, 600] {
        pump(d);
        dump_z(&format!("J2 через {d} мс"), 8);
    }

    // J3: self-raise окна, лежащего ВЫШЕ активного.
    let c3 = spawn_child_ex("JC3SELF", false, Some(400));
    pump(200);
    dump_z("J3: C3 создан (выше активного), self-raise через 400 мс", 8);
    pump(500);
    dump_z("J3 после self-raise C3 (+500 мс)", 8);
    pump(500);
    dump_z("J3 через +1000 мс", 8);
    restore_user_foreground(user_fg);
}

/// Решающий замер: с правом на передний план (как у resticker, получившего
/// хоткей) подъём НАД активным окном — работает ли и держится ли; контроль
/// без права; полная производственная последовательность (подъём всех +
/// SetForegroundWindow первого слота) против активного чужого окна.
fn test_k_lock() {
    println!("\n=== K. Право на передний план: подъём над активным окном ===");
    let user_fg = unsafe { GetForegroundWindow() };
    println!(
        "  активное окно пользователя: 0x{:X} (pid {})",
        user_fg.0 as usize,
        pid_of(user_fg)
    );

    // K1: без права — окно выше активного, подъём снаружи (контроль J2).
    let c1 = spawn_child("KC1", false);
    pump(150);
    dump_z("K1: C1 создан (выше активного), БЕЗ права", 8);
    raise_no_activate(c1.hwnd);
    dump_z("K1: после подъёма (t=0)", 8);
    for d in [50u64, 200, 600] {
        pump(d);
        dump_z(&format!("K1 через {d} мс"), 8);
    }

    // K2: с правом (send_alt) — то же самое.
    let c2 = spawn_child("KC2", false);
    pump(150);
    dump_z("K2: C2 создан (выше активного)", 8);
    send_alt();
    pump(80);
    raise_no_activate(c2.hwnd);
    dump_z("K2: после подъёма С ПРАВОМ (t=0)", 8);
    for d in [50u64, 200, 600] {
        pump(d);
        dump_z(&format!("K2 через {d} мс"), 8);
    }
    restore_user_foreground(user_fg);
}

/// Полная производственная последовательность против активного чужого окна:
/// 4 окна, подъём прямым порядком, SetForegroundWindow(G1) — и наблюдение,
/// не возвращается ли активное окно поверх группы.
fn test_l_production() {
    println!("\n=== L. Производственная последовательность: 4 окна + фокус, против активного чужого ===");
    let (set, f) = spawn_set("L");
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = set
        .iter()
        .map(|c| (c.name.as_str(), c.hwnd))
        .chain(std::iter::once(("L-F", f.hwnd)))
        .chain(std::iter::once(("L-USER(игра)", user_fg)))
        .collect();
    pump(200);
    print_z("до: порядок создания", &probes);
    send_alt(); // право на передний план = «получил хоткей»
    pump(80);
    // Подъём прямым порядком (как apply_visibility_decisions).
    for h in &g {
        raise_no_activate(*h);
        pump(40);
    }
    pump(120);
    print_z("после подъёма всех (прямой порядок)", &probes);
    // Фокус первому слоту (как focus_group_window).
    // SAFETY: SetForegroundWindow принимает любой живой HWND.
    let ok = unsafe { SetForegroundWindow(g[0]) }.as_bool();
    println!("  SetForegroundWindow(G1): {ok}");
    pump(150);
    print_z("сразу после фокуса", &probes);
    for d in [400u64, 1200, 3000] {
        pump(d);
        print_z(&format!("через {d} мс"), &probes);
    }
    println!(
        "  все 4 выше чужого F: {}",
        snapshot_ranks(&g).iter().all(|r| {
            let f_rank = rank_of(f.hwnd);
            *r < f_rank && *r != 0
        })
    );
    println!(
        "  все 4 выше активного окна: {}",
        snapshot_ranks(&g).iter().all(|r| {
            let u_rank = rank_of(user_fg);
            *r < u_rank && *r != 0
        })
    );
    restore_user_foreground(user_fg);
}

/// Финальное подтверждение: (1) заблокирован ли подъём чужого окна, когда
/// передний план у нашего процесса; (2) исключение для свёрнутых окон;
/// (3) обход через временный HWND_TOPMOST → NOTOPMOST (тот самый приём,
/// которым группа могла бы поднимать чужие окна).
fn test_m_final() {
    println!("\n=== M. Финальное подтверждение ===");
    let user_fg = unsafe { GetForegroundWindow() };

    // M1: передний план у НАШЕГО окна — работает ли подъём чужого окна?
    let p = spawn_child("MP", false);
    pump(150);
    send_alt();
    pump(80);
    let ok = unsafe { SetForegroundWindow(p.hwnd) }.as_bool();
    println!(
        "  M1: наше окно стало передним планом: {ok} (fg=0x{:X})",
        unsafe { GetForegroundWindow() }.0 as usize
    );
    let c1 = spawn_child("MC1", false);
    pump(150);
    dump_z("M1: C1 создан, fg — наше окно", 8);
    raise_no_activate(c1.hwnd);
    dump_z("M1: после подъёма C1 (t=0)", 8);
    pump(300);
    dump_z("M1: через 300 мс", 8);

    // M2: свёрнутое чужое окно — подтверждение исключения.
    let c2 = spawn_child("MC2", false);
    pump(150);
    // SAFETY: ShowWindowAsync безопасен для живых окон пробы.
    let _ = unsafe { ShowWindowAsync(c2.hwnd, SW_SHOWMINNOACTIVE) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !iconic(c2.hwnd) {
        pump(5);
    }
    raise_no_activate(c2.hwnd);
    dump_z("M2: свёрнутое C2 поднято (t=0)", 8);
    pump(300);
    dump_z("M2: через 300 мс", 8);

    // M3: обход — временный HWND_TOPMOST, затем NOTOPMOST.
    let c3 = spawn_child("MC3", false);
    pump(150);
    dump_z("M3: C3 создан (обычное окно)", 8);
    // SAFETY: окно живо; HWND_TOPMOST ставит стиль.
    let _ = unsafe {
        SetWindowPos(
            c3.hwnd,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(80);
    // SAFETY: окно живо; HWND_NOTOPMOST снимает стиль.
    let _ = unsafe {
        SetWindowPos(
            c3.hwnd,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(100);
    dump_z("M3: после TOPMOST→NOTOPMOST (t≈200 мс)", 8);
    pump(800);
    dump_z("M3: через ~1 с", 8);
    restore_user_foreground(user_fg);
}

/// Полная производственная последовательность, но с РАБОЧИМ механизмом
/// подъёма: каждому члену временный HWND_TOPMOST → HWND_NOTOPMOST
/// (приём из visibility_probe §3, теперь против активного чужого окна),
/// затем фокус первому слоту. Проверяется итоговый порядок и отсутствие
/// липкого WS_EX_TOPMOST.
fn test_n_fix_mechanism() {
    println!("\n=== N. Рабочий механизм: TOPMOST→NOTOPMOST на каждом члене ===");
    let (set, f) = spawn_set("N");
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = set
        .iter()
        .map(|c| (c.name.as_str(), c.hwnd))
        .chain(std::iter::once(("N-F", f.hwnd)))
        .chain(std::iter::once(("N-USER(игра)", user_fg)))
        .collect();
    pump(200);
    println!(
        "  raw: F=0x{:X} игра=0x{:X} (равны? {})",
        f.hwnd.0 as usize,
        user_fg.0 as usize,
        f.hwnd == user_fg
    );
    print_z("до: порядок создания", &probes);
    send_alt(); // право на передний план = «получил хоткей»
    pump(80);
    // Подъём прямым порядком слотов через TOPMOST→NOTOPMOST.
    for h in &g {
        // SAFETY: окно живо; временный topmost.
        let _ = unsafe {
            SetWindowPos(
                *h,
                Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            )
        };
        // SAFETY: окно живо; снятие topmost.
        let _ = unsafe {
            SetWindowPos(
                *h,
                Some(windows::Win32::UI::WindowsAndMessaging::HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            )
        };
        pump(40);
    }
    pump(120);
    print_z("после TOPMOST→NOTOPMOST всех (прямой порядок)", &probes);
    // Фокус первому слоту (как focus_group_window).
    // SAFETY: SetForegroundWindow принимает любой живой HWND.
    let ok = unsafe { SetForegroundWindow(g[0]) }.as_bool();
    println!("  SetForegroundWindow(G1): {ok}");
    pump(150);
    print_z("сразу после фокуса", &probes);
    for d in [400u64, 1200, 3000] {
        pump(d);
        print_z(&format!("через {d} мс"), &probes);
    }
    let ranks = snapshot_ranks(&g);
    let f_rank = rank_of(f.hwnd);
    let u_rank = rank_of(user_fg);
    println!(
        "  все 4 выше чужого F (F на {f_rank}): {}",
        ranks.iter().all(|r| *r < f_rank && *r != 0)
    );
    println!(
        "  все 4 выше активного окна (на {u_rank}): {}",
        ranks.iter().all(|r| *r < u_rank && *r != 0)
    );
    // Липкий стиль не остался?
    for (i, h) in g.iter().enumerate() {
        // SAFETY: GetWindowLongPtrW — чтение стиля живого окна.
        let ex = unsafe { GetWindowLongPtrW(*h, GWL_EXSTYLE) } as u32;
        let sticky = ex & WS_EX_TOPMOST.0 != 0;
        if sticky {
            println!("  ВНИМАНИЕ: у члена {i} остался WS_EX_TOPMOST!");
        }
    }
    println!("  липкий WS_EX_TOPMOST: не обнаружен (если нет ВНИМАНИЯ выше)");
    restore_user_foreground(user_fg);
}

/// Сценарий «спрятанная группа»: члены свёрнуты, показ = restore (async)
/// + сразу raise. Попадает ли raise в ещё-свёрнутое окно (и работает ли
/// тогда), и что выходит, если окно уже развёрнуто.
fn test_o_restore_race() {
    println!("\n=== O. Показ свёрнутой группы: restore(async) + сразу raise ===");
    let user_fg = unsafe { GetForegroundWindow() };
    println!(
        "  активное окно пользователя: 0x{:X} (pid {})",
        user_fg.0 as usize,
        pid_of(user_fg)
    );

    let c1 = spawn_child("OC1", false);
    pump(150);
    // SAFETY: ShowWindowAsync безопасен для живых окон пробы.
    let _ = unsafe { ShowWindowAsync(c1.hwnd, SW_SHOWMINNOACTIVE) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !iconic(c1.hwnd) {
        pump(5);
    }
    println!("  O1: C1 свёрнут");
    // Точная последовательность show_group_window + raise_group_window:
    // ShowWindowAsync(SW_SHOWNOACTIVATE), затем сразу SetWindowPos(HWND_TOP).
    // SAFETY: окно живо; разворот без активации.
    let _ = unsafe { ShowWindowAsync(c1.hwnd, SW_SHOWNOACTIVATE) };
    raise_no_activate(c1.hwnd);
    dump_z("O1: restore+raise сразу (t=0)", 8);
    pump(100);
    dump_z("O1: через 100 мс", 8);
    pump(500);
    dump_z("O1: через 600 мс", 8);

    // O2: контроль — окно НЕ свёрнуто (просто под другими окнами).
    let c2 = spawn_child("OC2", false);
    pump(150);
    // SAFETY: окно живо; опускаем C2 ниже активного, чтобы был «под».
    let _ = unsafe {
        SetWindowPos(
            c2.hwnd,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(100);
    dump_z("O2: C2 внизу, не свёрнут", 8);
    raise_no_activate(c2.hwnd);
    dump_z("O2: после raise (t=0)", 8);
    pump(300);
    dump_z("O2: через 300 мс", 8);
    restore_user_foreground(user_fg);
}

/// Набор: 4 окна группы (4 разных процесса) + постороннее окно (5-й процесс).
fn spawn_set(prefix: &str) -> (Vec<ProbeChild>, ProbeChild) {
    let set: Vec<ProbeChild> = (1..=4)
        .map(|i| spawn_child(&format!("{prefix}G{i}"), false))
        .collect();
    let f = spawn_child(&format!("{prefix}F"), false);
    (set, f)
}

/// Серия A: БЕЗ права на передний план — что реально делает HWND_TOP.
fn test_a_no_lock() {
    println!("\n=== A. БЕЗ права на передний план (активно окно пользователя) ===");
    let (set, f) = spawn_set("A");
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    // SAFETY: GetForegroundWindow — чтение.
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = set
        .iter()
        .map(|c| (c.name.as_str(), c.hwnd))
        .chain(std::iter::once(("A-USER-FG", user_fg)))
        .chain(std::iter::once(("A-F", f.hwnd)))
        .collect();
    println!(
        "  активное окно пользователя: 0x{:X} (pid {})",
        user_fg.0 as usize,
        pid_of(user_fg)
    );
    pump(200);
    print_z("до подъёма", &probes);
    for h in &g {
        let err = raise_no_activate(*h);
        if let Some(e) = err {
            println!("  SetWindowPos(G): ОШИБКА {e}");
        }
        pump(40);
    }
    pump(150);
    print_z("после HWND_TOP-подъёма 4 окон", &probes);
    println!(
        "  все 4 выше активного окна пользователя: {}",
        g.iter().all(|h| rank_of(*h) < rank_of(user_fg))
    );
    println!(
        "  все 4 выше постороннего F: {}",
        g.iter().all(|h| rank_of(*h) < rank_of(f.hwnd))
    );
}

/// Серия B: с правом на передний план (фейковый Alt — как получение хоткея).
fn test_b_with_lock() {
    println!("\n=== B. С правом на передний план (SendInput Alt = «получил хоткей») ===");
    let (set, f) = spawn_set("B");
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    // SAFETY: GetForegroundWindow — чтение.
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = set
        .iter()
        .map(|c| (c.name.as_str(), c.hwnd))
        .chain(std::iter::once(("B-USER-FG", user_fg)))
        .chain(std::iter::once(("B-F", f.hwnd)))
        .collect();
    pump(200);
    print_z("до подъёма", &probes);

    send_alt(); // право на передний план
    pump(80);

    // B1: прямой порядок слотов (как apply_visibility_decisions).
    println!("  подъём прямым порядком: G1 → G2 → G3 → G4");
    for h in &g {
        let err = raise_no_activate(*h);
        if let Some(e) = err {
            println!("  SetWindowPos(G): ОШИБКА {e}");
        }
        pump(40);
    }
    pump(120);
    print_z("сразу после прямого порядка", &probes);
    println!(
        "  все 4 выше активного окна пользователя: {}",
        g.iter().all(|h| rank_of(*h) < rank_of(user_fg))
    );
    println!(
        "  все 4 выше постороннего F: {}",
        g.iter().all(|h| rank_of(*h) < rank_of(f.hwnd))
    );
    // Возврат активного окна наверх? Опрос рангов.
    for delay in [300u64, 1500, 4000] {
        pump(delay);
        print_z(&format!("через {delay} мс"), &probes);
    }

    // B2: обратный порядок слотов.
    println!("  обратный порядок: G4 → G3 → G2 → G1");
    raise_no_activate(f.hwnd);
    pump(120);
    print_z("до (F снова наверху)", &probes);
    for h in g.iter().rev() {
        raise_no_activate(*h);
        pump(40);
    }
    pump(120);
    print_z("после обратного порядка", &probes);
}

/// Серия C: SetForegroundWindow на первом слоте после подъёма — роняет ли
/// остальных; не возвращается ли активное окно пользователя наверх.
fn test_c_focus_after_raise() {
    println!("\n=== C. Подъём (прямой порядок) + SetForegroundWindow(G1) ===");
    let (set, f) = spawn_set("C");
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    // SAFETY: GetForegroundWindow — чтение.
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = set
        .iter()
        .map(|c| (c.name.as_str(), c.hwnd))
        .chain(std::iter::once(("C-USER-FG", user_fg)))
        .chain(std::iter::once(("C-F", f.hwnd)))
        .collect();
    pump(200);
    send_alt();
    pump(80);
    for h in &g {
        raise_no_activate(*h);
        pump(40);
    }
    pump(120);
    print_z("после подъёма прямым порядком (до фокуса)", &probes);
    // SAFETY: SetForegroundWindow принимает любой живой HWND.
    let ok = unsafe { SetForegroundWindow(g[0]) }.as_bool();
    println!("  SetForegroundWindow(G1): {ok}");
    pump(150);
    print_z("сразу после фокуса на G1", &probes);
    pump(1000);
    print_z("через ~1 с после фокуса", &probes);
    println!(
        "  остальные 3 окна всё ещё выше F: {}",
        g[1..].iter().all(|h| rank_of(*h) < rank_of(f.hwnd))
    );
    println!(
        "  активное окно пользователя не вернулось наверх: {}",
        rank_of(user_fg) > g.iter().map(|h| rank_of(*h)).max().unwrap()
    );
    restore_user_foreground(user_fg);
}

/// Серия D: полноэкранное окно другого процесса — в обычной полосе, активное;
/// и в topmost-полосе.
fn test_d_fullscreen() {
    println!("\n=== D. Полноэкранное окно другого процесса ===");
    let g = spawn_child("DG", false);
    let ffs = spawn_child("DFS", true);
    let user_fg = unsafe { GetForegroundWindow() };
    pump(200);
    // Полноэкранное окно делаем активным (как игра/плеер).
    let how = force_foreground(ffs.hwnd);
    println!(
        "  полноэкранное (pid {}) активно: {how}; наше окно ниже? {}",
        pid_of(ffs.hwnd),
        rank_of(g.hwnd) > rank_of(ffs.hwnd)
    );

    // D1: без права на передний план.
    println!("  --- D1: БЕЗ права на передний план ---");
    let err = raise_no_activate(g.hwnd);
    if let Some(e) = err {
        println!("  SetWindowPos: ОШИБКА {e}");
    }
    pump(200);
    println!(
        "  наше окно над активным полноэкранным: {}",
        rank_of(g.hwnd) < rank_of(ffs.hwnd)
    );

    // D2: с правом (хоткей).
    println!("  --- D2: С правом на передний план ---");
    send_alt();
    pump(80);
    let err = raise_no_activate(g.hwnd);
    if let Some(e) = err {
        println!("  SetWindowPos: ОШИБКА {e}");
    }
    pump(200);
    println!(
        "  наше окно над активным полноэкранным: {}",
        rank_of(g.hwnd) < rank_of(ffs.hwnd)
    );

    // D3: полноэкранное в topmost-полосе (WS_EX_TOPMOST) — честная граница.
    println!("  --- D3: полноэкранное в topmost-полосе ---");
    // SAFETY: окно живо; HWND_TOPMOST ставит стиль.
    let _ = unsafe {
        SetWindowPos(
            ffs.hwnd,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(150);
    let err = raise_no_activate(g.hwnd);
    if let Some(e) = err {
        println!("  SetWindowPos: ОШИБКА {e}");
    }
    pump(200);
    println!(
        "  наше окно над полноэкранным (topmost): {}",
        rank_of(g.hwnd) < rank_of(ffs.hwnd)
    );
    // SAFETY: окно живо; HWND_NOTOPMOST снимает стиль.
    let _ = unsafe {
        SetWindowPos(
            ffs.hwnd,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
    };
    pump(100);
    restore_user_foreground(user_fg);
}

/// Серия E: реальный цикл показа — все свёрнуты → развернуть всех
/// (SW_SHOWNOACTIVATE) → поднять (с правом на передний план).
fn test_e_realistic_cycle() {
    println!("\n=== E. Реальный цикл: все свёрнуты → развернуть → поднять ===");
    let (set, f) = spawn_set("E");
    let g: Vec<HWND> = set.iter().map(|c| c.hwnd).collect();
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = set
        .iter()
        .map(|c| (c.name.as_str(), c.hwnd))
        .chain(std::iter::once(("E-F", f.hwnd)))
        .collect();
    pump(200);
    // Сворачиваем всех (как hide_group_window).
    // SAFETY: ShowWindowAsync безопасен для живых окон пробы.
    for h in g.iter().chain(std::iter::once(&f.hwnd)) {
        let _ = unsafe { ShowWindowAsync(*h, SW_SHOWMINNOACTIVE) };
    }
    pump(400);
    println!(
        "  все свёрнуты: {}",
        g.iter().all(|h| iconic(*h)) && iconic(f.hwnd)
    );
    send_alt();
    pump(80);
    // Разворачиваем всех (show_group_window) и сразу поднимаем прямым
    // порядком — тот же порядок вызовов, что в toggle_group_by_number.
    for h in &g {
        // SAFETY: ShowWindowAsync безопасен для живых окон пробы.
        let _ = unsafe { ShowWindowAsync(*h, SW_SHOWNOACTIVATE) };
    }
    pump(80);
    for h in &g {
        raise_no_activate(*h);
        pump(40);
    }
    pump(250);
    print_z("после разворота+подъёма (прямой порядок)", &probes);
    println!(
        "  все 4 выше постороннего F: {}",
        g.iter().all(|h| rank_of(*h) < rank_of(f.hwnd))
    );
    restore_user_foreground(user_fg);
}

/// Серия F: четыре окна ОДНОГО процесса — та же механика, что у разных?
fn test_f_same_process() {
    println!("\n=== F. Четыре окна одного процесса + постороннее другого ===");
    let mut own: Vec<HWND> = Vec::new();
    for i in 1..=4 {
        let class_wide: Vec<u16> = format!("zorder_own{i}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let class_pw = PCWSTR(class_wide.as_ptr());
        // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
        let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(probe_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_pw,
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация — не ошибка.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            let _err = unsafe { windows::Win32::Foundation::GetLastError() };
        }
        let title: Vec<u16> = format!("zorder-own{i}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: валидные константы и зарегистрированный класс.
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                class_pw,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPED | WS_VISIBLE,
                80,
                80,
                260,
                170,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание окна пробы");
        own.push(hwnd);
    }
    let f = spawn_child("FF", false);
    let user_fg = unsafe { GetForegroundWindow() };
    let probes: Vec<(&str, HWND)> = own
        .iter()
        .enumerate()
        .map(|(i, h)| (format!("own{}", i + 1), *h))
        .chain(std::iter::once(("FF".to_string(), f.hwnd)))
        .map(|(n, h)| (Box::leak(n.into_boxed_str()) as &str, h))
        .collect();
    pump(200);
    print_z("до (порядок создания)", &probes);
    send_alt();
    pump(80);
    for h in &own {
        raise_no_activate(*h);
        pump(40);
    }
    pump(150);
    print_z("после подъёма (прямой порядок)", &probes);
    println!(
        "  все 4 выше постороннего F: {}",
        own.iter().all(|h| rank_of(*h) < rank_of(f.hwnd))
    );
    for h in &own {
        // SAFETY: окна пробы живые.
        let _ = unsafe { DestroyWindow(*h) };
    }
    pump(100);
    restore_user_foreground(user_fg);
}

fn main() {
    if std::env::args().any(|a| a == "--child") {
        let args: Vec<String> = std::env::args().collect();
        let name = args
            .iter()
            .position(|a| a == "--name")
            .map(|i| args[i + 1].clone())
            .expect("--name");
        let event = args
            .iter()
            .position(|a| a == "--event")
            .map(|i| args[i + 1].clone())
            .expect("--event");
        let fullscreen = args.iter().any(|a| a == "--fullscreen");
        let raise_ms = args
            .iter()
            .position(|a| a == "--raise-ms")
            .map(|i| args[i + 1].parse::<u64>().expect("--raise-ms число"));
        run_child(&name, fullscreen, &event, raise_ms);
    }

    println!("=== B4: Z-порядок при подъёме нескольких окон (свои окна, чужие не трогаем) ===");
    println!("ранги [n] — позиция окна среди ВСЕХ видимых top-level окон (1 = самый верх)");

    test_a_no_lock();
    test_b_with_lock();
    test_c_focus_after_raise();
    test_d_fullscreen();
    test_e_realistic_cycle();
    test_f_same_process();
    test_g_sanity();
    test_n_fix_mechanism();
    test_o_restore_race();

    println!("\nГотово.");
}