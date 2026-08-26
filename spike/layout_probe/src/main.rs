use std::ffi::c_void;
use std::time::Duration;
use rst_core::model::Rect;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, OpenDesktopW, OpenInputDesktop, OpenWindowStationW,
    SetProcessWindowStation, SetThreadDesktop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowPlacement,
    GetWindowRect, RegisterClassW, SetWindowPlacement, SetWindowPos, ShowWindow, GWL_STYLE,
    SW_MAXIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    WINDOWPLACEMENT, WNDCLASSW, WS_MAXIMIZE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::w;

fn attach_to_interactive_desktop() {
    unsafe {
        if let Ok(winsta) = OpenWindowStationW(w!("winsta0"), false, 0x10000000) {
            let _ = SetProcessWindowStation(winsta);
        }
        if let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(0x10000000)) {
            let _ = SetThreadDesktop(desk);
        } else if let Ok(desk) = OpenDesktopW(w!("default"), DESKTOP_CONTROL_FLAGS(0), false, 0x10000000) {
            let _ = SetThreadDesktop(desk);
        }
    }
}

unsafe extern "system" fn wndproc_standard(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn get_dwm_rect(hwnd: HWND) -> Rect {
    let mut rect = RECT::default();
    unsafe {
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        );
    }
    Rect {
        x: rect.left,
        y: rect.top,
        w: (rect.right - rect.left) as u32,
        h: (rect.bottom - rect.top) as u32,
    }
}

fn get_win32_rect(hwnd: HWND) -> Rect {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    Rect {
        x: rect.left,
        y: rect.top,
        w: (rect.right - rect.left) as u32,
        h: (rect.bottom - rect.top) as u32,
    }
}

fn is_maximized(hwnd: HWND) -> bool {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    (style & WS_MAXIMIZE.0) != 0
}

fn set_dwm_bounds_fixed(hwnd: HWND, target: RECT) -> bool {
    // If window is maximized, restore it first so that we measure true restored borders!
    if is_maximized(hwnd) {
        unsafe {
            let mut wp = WINDOWPLACEMENT::default();
            wp.length = size_of::<WINDOWPLACEMENT>() as u32;
            if GetWindowPlacement(hwnd, &mut wp).is_ok() {
                wp.showCmd = SW_SHOWNOACTIVATE.0 as u32;
                let _ = SetWindowPlacement(hwnd, &wp);
            }
        }
    }

    let mut gwr = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut gwr) }.is_err() {
        return false;
    }
    let dwm = get_dwm_rect(hwnd);
    let dx = dwm.x - gwr.left;
    let dy = dwm.y - gwr.top;
    let dw = dwm.w as i32 - (gwr.right - gwr.left);
    let dh = dwm.h as i32 - (gwr.bottom - gwr.top);

    let good = target;
    let good_w = good.right - good.left;
    let good_h = good.bottom - good.top;

    let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
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

fn main() {
    attach_to_interactive_desktop();
    println!("=== Testing set_dwm_bounds_fixed on Maximized Window ===");

    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap().into() };
    let cls_standard = w!("ProbeClassFixedTest");

    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc_standard),
            hInstance: hinstance,
            lpszClassName: cls_standard,
            hbrBackground: HBRUSH(COLOR_WINDOW.0 as *mut c_void),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }

    let w = unsafe {
        CreateWindowExW(
            Default::default(),
            cls_standard,
            w!("Probe Fixed Maximize Window"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100, 100, 500, 400,
            None, None, Some(hinstance), None,
        ).unwrap()
    };

    // Maximize the window first
    unsafe {
        let _ = ShowWindow(w, SW_MAXIMIZE);
    }
    std::thread::sleep(Duration::from_millis(100));

    let target = Rect { x: 1298, y: 35, w: 1227, h: 648 };
    let r_target = RECT {
        left: target.x,
        top: target.y,
        right: target.x + target.w as i32,
        bottom: target.y + target.h as i32,
    };
    set_dwm_bounds_fixed(w, r_target);
    std::thread::sleep(Duration::from_millis(100));

    let dwm_after = get_dwm_rect(w);
    let gwr_after = get_win32_rect(w);
    println!("After set_dwm_bounds_fixed:\n  Target: {:?}\n  DWM:    {:?} (diff: dx={}, dy={}, dw={}, dh={})\n  GWR:    {:?}",
        target, dwm_after,
        dwm_after.x - target.x, dwm_after.y - target.y,
        dwm_after.w as i32 - target.w as i32, dwm_after.h as i32 - target.h as i32,
        gwr_after
    );

    unsafe {
        let _ = DestroyWindow(w);
    }
}
