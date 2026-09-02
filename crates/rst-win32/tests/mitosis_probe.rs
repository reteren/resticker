//! Сквозной замер митоза окон на ОДНОРАЗОВОМ «Блокноте»: запуск второго
//! экземпляра, обнаружение его окна и расстановка обеих половин
//! (docs/M9_WINDOW_MITOSIS_DESIGN.md).
//!
//! `#[ignore]` и отдельным файлом: тест открывает и закрывает настоящие окна
//! на живом рабочем столе — в обычном прогоне ему делать нечего. Но это
//! единственная проверка САМОЙ рискованной части функции: юнит-тесты знают
//! только чистую арифметику и синтетические снимки, а «второй экземпляр
//! приложения вообще открывает окно, и это окно можно поставить куда надо» —
//! утверждение про живую Windows, и проверяется оно только так.
//!
//! Замер 2026-09-01 на этой машине: второе окно «Блокнота» появилось за
//! 168 мс и принадлежало ТОМУ ЖЕ pid, что и первое (22972), тогда как
//! запущенный нами процесс был 42804 — то есть нашла его ветка сравнения по
//! пути к exe, а не по pid (см. `window_mitosis::match_sibling_window`).
//! Обе половины встали пиксель в пиксель: 840 + 560 на исходных 1400.
//!
//! Запуск: cargo test -p rst-win32 --test mitosis_probe -- --ignored --nocapture

use std::collections::HashSet;
use std::time::{Duration, Instant};

use rst_core::mitosis::{self, SplitAxis};
use rst_win32::window_enum::{self, WindowRect};
use rst_win32::window_mitosis;
use rst_win32::window_pin::WindowPins;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

const NOTEPAD: &str = r"C:\Windows\System32\notepad.exe";

fn hwnd_of(h: usize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

fn close(h: usize) {
    unsafe {
        let _ = PostMessageW(Some(hwnd_of(h)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn find_new(known: &HashSet<usize>, exe: &str, timeout: Duration) -> Option<usize> {
    let deadline = Instant::now() + timeout;
    loop {
        std::thread::sleep(Duration::from_millis(120));
        let snap = window_enum::enumerate();
        let found = snap.iter().find(|w| {
            !known.contains(&w.hwnd)
                && w.exe_path.to_string_lossy().to_lowercase().contains(exe)
                && w.rect.w > 100
        });
        if let Some(w) = found {
            return Some(w.hwnd);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

#[test]
#[ignore = "проба: открывает и закрывает окна на живом рабочем столе"]
fn mitosis_end_to_end_on_a_throwaway_notepad() {
    let pins = WindowPins::new();
    let before: HashSet<usize> = window_enum::enumerate().iter().map(|w| w.hwnd).collect();

    // 1. Свой одноразовый «Блокнот» — жертва разреза.
    let first = window_mitosis::spawn_sibling(std::path::Path::new(NOTEPAD))
        .expect("запуск первого блокнота");
    let victim = find_new(&before, "notepad", Duration::from_secs(10))
        .expect("окно первого блокнота не появилось");
    println!("жертва: hwnd={victim}, запущенный pid={}", first.pid);

    // 2. Ставим его в известный прямоугольник, как это делает пользователь.
    let start = WindowRect {
        x: 200,
        y: 150,
        w: 1400,
        h: 800,
    };
    assert!(
        pins.set_dwm_bounds(
            hwnd_of(victim),
            RECT {
                left: start.x,
                top: start.y,
                right: start.x + start.w,
                bottom: start.y + start.h,
            }
        ),
        "не удалось поставить жертву в исходный прямоугольник"
    );
    std::thread::sleep(Duration::from_millis(400));

    let live = window_enum::enumerate();
    let victim_rect = live
        .iter()
        .find(|w| w.hwnd == victim)
        .map(|w| w.rect)
        .expect("жертва пропала из перечисления");
    println!("исходный DWM-прямоугольник: {victim_rect:?}");
    let exe = live
        .iter()
        .find(|w| w.hwnd == victim)
        .map(|w| w.exe_path.clone())
        .expect("нет пути к exe жертвы");
    let victim_pid = live
        .iter()
        .find(|w| w.hwnd == victim)
        .map(|w| w.pid)
        .unwrap();
    println!(
        "exe={}, pid={victim_pid}, приватная память={:?} МБ",
        exe.display(),
        window_mitosis::process_private_bytes(victim_pid).map(|b| b / 1024 / 1024)
    );

    // 3. Считаем разрез ровно тем же кодом, что и координатор.
    let pix = mitosis::PixRect {
        x: victim_rect.x,
        y: victim_rect.y,
        w: victim_rect.w,
        h: victim_rect.h,
    };
    let cursor_x = victim_rect.x + victim_rect.w * 6 / 10;
    let fraction = mitosis::split_fraction(pix, SplitAxis::Vertical, cursor_x, 0);
    mitosis::preflight(
        pix,
        SplitAxis::Vertical,
        fraction,
        window_mitosis::process_private_bytes(victim_pid),
        4096 * 1024 * 1024,
        window_mitosis::available_physical_bytes(),
    )
    .expect("предполётная проверка не должна отказать на блокноте");
    let (left, right) = mitosis::split_rects(pix, SplitAxis::Vertical, fraction);
    println!("доля={fraction}, левая={left:?}, правая={right:?}");

    // 4. Ужимаем оригинал и запускаем второй экземпляр.
    let known: HashSet<usize> = window_enum::enumerate().iter().map(|w| w.hwnd).collect();
    assert!(pins.set_dwm_bounds(
        hwnd_of(victim),
        RECT {
            left: left.x,
            top: left.y,
            right: left.x + left.w,
            bottom: left.y + left.h,
        }
    ));
    let t0 = Instant::now();
    let sibling = window_mitosis::spawn_sibling(&exe).expect("запуск второго экземпляра");
    let found = window_mitosis::wait_for_sibling_window(
        sibling.pid,
        &exe,
        &known,
        Duration::from_millis(150),
        Duration::from_secs(8),
    );
    let clone = found.expect("второе окно не появилось за 8 с");
    println!(
        "второе окно: hwnd={}, pid={} (запускали {}), за {} мс",
        clone.hwnd,
        clone.pid,
        sibling.pid,
        t0.elapsed().as_millis()
    );

    assert!(pins.set_dwm_bounds(
        hwnd_of(clone.hwnd),
        RECT {
            left: right.x,
            top: right.y,
            right: right.x + right.w,
            bottom: right.y + right.h,
        }
    ));
    std::thread::sleep(Duration::from_millis(500));

    // 5. Замеряем, что вышло на самом деле.
    let after = window_enum::enumerate();
    let a = after.iter().find(|w| w.hwnd == victim).map(|w| w.rect);
    let b = after.iter().find(|w| w.hwnd == clone.hwnd).map(|w| w.rect);
    println!("ФАКТ левая={a:?}");
    println!("ФАКТ правая={b:?}");

    close(victim);
    close(clone.hwnd);

    let a = a.expect("левая половина пропала");
    let b = b.expect("правая половина пропала");
    let tol = 2;
    assert!(
        (a.x - left.x).abs() <= tol && (a.w - left.w).abs() <= tol,
        "левая: ждали {left:?}, получили {a:?}"
    );
    assert!(
        (b.x - right.x).abs() <= tol && (b.w - right.w).abs() <= tol,
        "правая: ждали {right:?}, получили {b:?}"
    );
    assert!(
        (a.x + a.w - b.x).abs() <= tol,
        "между половинами щель или нахлёст: {} px",
        a.x + a.w - b.x
    );
}
