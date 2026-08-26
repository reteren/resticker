use std::time::Instant;
use windows::core::w;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::StationsAndDesktops::{
    DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, OpenDesktopW, OpenInputDesktop, OpenWindowStationW,
    SetProcessWindowStation, SetThreadDesktop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetClassNameW, IsIconic, IsWindow, IsWindowVisible,
};

const SHELL_TRANSIENT_CLASS_NAMES: [&windows::core::PCWSTR; 8] = [
    &w!("XamlExplorerHostIslandWindow"),
    &w!("TopLevelWindowForOverflowXamlIsland"),
    &w!("SnapFlyout"),
    &w!("MultitaskingViewFrame"),
    &w!("TaskSwitcherWnd"),
    &w!("TaskSwitcherOverlayWnd"),
    &w!("ForegroundStaging"),
    &w!("Windows.UI.Core.CoreWindow"),
];

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

fn check_shell_transient_visible_all() -> Vec<(String, usize, (i32, i32, i32, i32))> {
    let mut out = Vec::new();
    for &cls_name in &SHELL_TRANSIENT_CLASS_NAMES {
        let mut curr_hwnd = HWND::default();
        while let Ok(hwnd) = unsafe { FindWindowExW(None, Some(curr_hwnd), *cls_name, None) } {
            if hwnd.0.is_null() {
                break;
            }
            curr_hwnd = hwnd;
            unsafe {
                if !IsWindow(Some(hwnd)).as_bool()
                    || !IsWindowVisible(hwnd).as_bool()
                    || IsIconic(hwnd).as_bool()
                {
                    continue;
                }
                let mut cloaked: u32 = 0;
                let _ = DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    (&raw mut cloaked).cast(),
                    size_of::<u32>() as u32,
                );
                if cloaked != 0 {
                    continue;
                }
                let mut rect = RECT::default();
                let _ = DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS,
                    (&raw mut rect).cast(),
                    size_of::<RECT>() as u32,
                );
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;
                if w > 0 && h > 0 {
                    let mut class_buf = [0u16; 256];
                    let class_len = GetClassNameW(hwnd, &mut class_buf);
                    let class = String::from_utf16_lossy(&class_buf[..class_len as usize]);
                    out.push((class, hwnd.0 as usize, (rect.left, rect.top, w, h)));
                }
            }
        }
    }
    out
}

fn main() {
    attach_to_interactive_desktop();

    println!("Testing check_shell_transient_visible_all()...");
    let t0 = Instant::now();
    let res = check_shell_transient_visible_all();
    let dt = t0.elapsed();

    println!("Found {} visible shell transient windows (took {:?}):", res.len(), dt);
    for r in res {
        println!("  {:?}", r);
    }
}
