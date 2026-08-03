//! Демо rst-render на реальном окне: полноэкранное NOREDIRECTIONBITMAP-окно,
//! четыре спрайта (обычный, повёрнутый, полупрозрачный, сильно уменьшенный —
//! заодно проверяет мипмапы). Окно живёт ~4 секунды и закрывается само.
//!
//! Запуск: `cargo run -p rst-render --example overlay_demo`

use std::time::{Duration, Instant};

use rst_core::model::{MonitorId, Placement, Transform};
use rst_render::{Device, Sprite, WindowTarget};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: вызов на старте процесса, до создания окон.
    let _ = unsafe { SetProcessDPIAware() };

    // Тестовое изображение 256x256: полупрозрачный градиент + оранжевый диск.
    let (w, h) = (256u32, 256u32);
    let mut img = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w) as u8;
            let b = (y * 255 / h) as u8;
            img.put_pixel(x, y, image::Rgba([r, 30, b, 110]));
        }
    }
    let (cx, cy, rad) = (128.0f32, 128.0f32, 80.0f32);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            if dx * dx + dy * dy < rad * rad {
                img.put_pixel(x, y, image::Rgba([255, 128, 0, 255]));
            }
        }
    }
    let png_path = std::env::temp_dir().join("rst_render_demo.png");
    img.save(&png_path)?;

    // Окно, как в спайке S0: показать содержимое может только DirectComposition.
    let (sw, sh, hwnd);
    // SAFETY: стандартное создание окна; все вызовы с валидными параметрами.
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("rst_render_demo");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
        sw = GetSystemMetrics(SM_CXSCREEN) as u32;
        sh = GetSystemMetrics(SM_CYSCREEN) as u32;
        hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("rst-render demo"),
            WS_POPUP,
            0,
            0,
            sw as i32,
            sh as i32,
            None,
            None,
            Some(hinst),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    println!("overlay_demo: окно создано");
    let device = Device::new()?;
    let target = WindowTarget::new(&device, hwnd, sw, sh)?;
    println!("overlay_demo: устройство и цель созданы");
    let tex = device.load_image(&png_path)?;
    println!(
        "overlay_demo: текстура загружена {}x{}",
        tex.width(),
        tex.height()
    );

    let sprite = |cx: f64, cy: f64, size: f64, rotation: f64, opacity: f64| {
        Sprite::new(
            tex.clone(),
            Placement {
                monitor_id: MonitorId(String::new()),
                cx,
                cy,
                w: size,
                h: size,
            },
            Transform {
                rotation,
                opacity,
                flip_h: false,
                flip_v: false,
            },
        )
    };
    let sprites = vec![
        sprite(960.0, 540.0, 256.0, 0.0, 1.0),  // обычный
        sprite(480.0, 540.0, 256.0, 0.6, 1.0),  // повёрнутый
        sprite(1440.0, 540.0, 256.0, 0.0, 0.5), // полупрозрачный
        sprite(960.0, 220.0, 64.0, 0.0, 1.0),   // уменьшенный (мипмапы)
    ];
    device.draw(&target, &sprites)?;
    println!("overlay_demo: кадр представлен; окно закроется через 4 с");

    // Кадр уже представлен; просто держим окно 4 секунды.
    let deadline = Instant::now() + Duration::from_secs(4);
    // SAFETY: стандартный цикл сообщений своего окна.
    unsafe {
        let mut msg = MSG::default();
        while Instant::now() < deadline {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DESTROY => {
            // SAFETY: стандартный ответ на разрушение окна.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: обработчик по умолчанию.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
