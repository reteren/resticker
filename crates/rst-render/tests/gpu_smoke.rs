//! GPU smoke-тест: создание устройства и цели рендера на реальном окне,
//! текстура, resize, представление кадра. Требует GPU и дисплей — по
//! умолчанию пропускается:
//!   cargo test -p rst-render --test gpu_smoke -- --ignored

use rst_core::model::{MonitorId, Placement, Transform};
use rst_render::{Device, Sprite, WindowTarget};
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

#[test]
#[ignore = "требует GPU и дисплей; запуск вручную: cargo test -- --ignored"]
fn renderer_creates_draws_and_resizes_on_a_real_window() {
    // SAFETY: окно системного класса Static, регистрация своего класса не нужна;
    // все параметры валидны.
    let hwnd = unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("Static"),
            w!("rst-render gpu smoke"),
            WS_POPUP,
            0,
            0,
            320,
            240,
            None,
            None,
            Some(hinst),
            None,
        )
        .unwrap()
    };
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };

    let device = Device::new().expect("устройство создаётся на GPU");
    let mut target =
        WindowTarget::new(&device, hwnd, 320, 240).expect("цель рендера создаётся на GPU");
    let tex = device
        .create_texture_from_rgba(&[255, 128, 0, 255], 1, 1)
        .expect("текстура создаётся");
    assert_eq!((tex.width(), tex.height()), (1, 1));

    let sprite = Sprite::new(
        tex,
        Placement {
            monitor_id: MonitorId(String::new()),
            cx: 160.0,
            cy: 120.0,
            w: 64.0,
            h: 64.0,
        },
        Transform::default(),
    );
    device.draw(&target, &[sprite]).expect("кадр представлен");

    target.resize(&device, 640, 480).expect("resize не падает");
    assert_eq!(target.size(), (640, 480));
    device
        .draw(&target, &[])
        .expect("пустой кадр после resize — ок");

    target
        .resize(&device, 0, 0)
        .expect("нулевой размер — не ошибка");
    device
        .draw(&target, &[])
        .expect("кадр на нулевом размере пропускается");

    // SAFETY: окно больше не нужно; цель уничтожается после окна — для
    // smoke-теста порядок некритичен, окно всё равно умирает с процессом теста.
    unsafe { DestroyWindow(hwnd) }.unwrap();
}
