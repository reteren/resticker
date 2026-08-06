//! Извлечение иконки exe-файла в RGBA-растр (M4, панель выбора окон:
//! docs/M4_WINDOW_PICKER_DESIGN.md §6 — последний незакрытый пункт M4).
//!
//! Путь: `SHGetFileInfoW` (SHGFI_ICON | SHGFI_SMALLICON) → `HICON` →
//! `GetIconInfo` → цветовой `HBITMAP` → `GetDIBits` (32 bpp, BI_RGB,
//! top-down) → BGRA→RGBA. Чистая конверсия вынесена отдельной функцией
//! [`bgra_to_rgba`] — тестируется юнитами без Win32.
//!
//! Иконки общие для всех окон одного exe — дедупликация кэшем по пути
//! лежит в `window_enum` (см. `window_enum::ICON_CACHE`), здесь только
//! «вытащить иконку файла».

use std::path::Path;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, SelectObject,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON, SHGFI_SMALLICON, SHGetFileInfoW,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use crate::window_enum::WindowIcon;

/// Иконка exe-файла как RGBA-растр (16×16 для SHGFI_SMALLICON на 96 DPI;
/// на DPI-осведомлённых потоках может быть 20/24 — размер читается из
/// самого битмапа, а не предполагается). `None` — файл не существует,
/// у него нет иконки (монохромная — без цветового битмапа) или сбой API.
///
/// `SHGetFileInfoW` с `SHGFI_ICON` ходит в шелл (IShellItemImageFactory) —
/// требует COM на вызывающем потоке; инициализация/деинициализация — на
/// время вызова, тем же паттерном, что `file_dialog` (вызов модален и живёт
/// ровно до возврата). Вызовы шелла на первом обращении к exe могут занять
/// единицы миллисекунд — вызывающий код обязан кэшировать результат по
/// пути (это делает `window_enum::ICON_CACHE`).
pub(crate) fn extract_icon(exe_path: &Path) -> Option<WindowIcon> {
    // SAFETY: CoInitializeEx(COINIT_APARTMENTTHREADED) — стандартная
    // инициализация COM на текущем потоке; HRESULT игнорируется, как в
    // file_dialog.rs (повторная инициализация на уже инициализированном
    // потоке — RPC_E_CHANGED_MODE, не фатальна для шелл-вызовов).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let result = extract_icon_inner(exe_path);
    // SAFETY: парный вызов к CoInitializeEx выше, на том же потоке.
    unsafe {
        CoUninitialize();
    }
    result
}

fn extract_icon_inner(exe_path: &Path) -> Option<WindowIcon> {
    let wide = wide_path(exe_path)?;
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: wide — живой CString на время вызова; sfi — out-буфер;
    // SHGFI_ICON без SHGFI_USEFILEATTRIBUTES требует существующий файл —
    // путь exe из QueryFullProcessImageNameW, файл существует.
    let handle = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut sfi),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_FLAGS(SHGFI_ICON.0 | SHGFI_SMALLICON.0),
        )
    };
    if handle == 0 || sfi.hIcon.0.is_null() {
        return None;
    }
    let icon = HICON(sfi.hIcon.0);
    let result = icon_to_rgba(icon);
    // SAFETY: hIcon из SHGetFileInfoW — наш, должен быть уничтожен.
    unsafe {
        let _ = DestroyIcon(icon);
    }
    result
}

/// HICON → RGBA-растр: цветовой битмап иконки через GetIconInfo +
/// GetDIBits (32 bpp BI_RGB, top-down — строки сверху вниз, как у
/// текстур). `None` — монохромная иконка (hbmColor пуст) или сбой.
fn icon_to_rgba(icon: HICON) -> Option<WindowIcon> {
    let mut info = ICONINFO::default();
    // SAFETY: icon — валидный HICON; info — out-буфер.
    if unsafe { GetIconInfo(icon, &mut info) }.is_err() {
        return None;
    }
    // SAFETY: hbmColor/hbmMask принадлежат нам после GetIconInfo и
    // уничтожаются ниже в любом случае (DeleteObject безопасен для
    // валидного HBITMAP, в т.ч. NULL-дескриптора нет — HBITMAP нулевой
    // означает «нет битмапа», удалять нечего).
    let hbm_color = info.hbmColor;
    let hbm_mask = info.hbmMask;
    let result = hbm_to_rgba(hbm_color);
    unsafe {
        let _ = DeleteObject(hbm_color.into());
        let _ = DeleteObject(hbm_mask.into());
    }
    result
}

/// Цветовой битмап → RGBA (16–32 bpp; 32 bpp несёт настоящую альфу —
/// иконки Vista+ authored straight alpha, остальные глубины получают
/// альфу 255). `None` — пустой битмап (монохромная иконка) или сбой GDI.
fn hbm_to_rgba(hbm: HBITMAP) -> Option<WindowIcon> {
    if hbm.0.is_null() {
        return None;
    }
    let mut bmp = BITMAP::default();
    // SAFETY: hbm — валидный HBITMAP; bmp — out-буфер под BITMAP.
    let got = unsafe {
        GetObjectW(
            hbm.into(),
            size_of::<BITMAP>() as i32,
            Some((&raw mut bmp).cast()),
        )
    };
    if got == 0 {
        return None;
    }
    let (w, h) = (bmp.bmWidth as u32, bmp.bmHeight as u32);
    if w == 0 || h == 0 || w > 256 || h > 256 {
        return None;
    }
    // SAFETY: CreateCompatibleDC(None) — десктопный DC без владения окном;
    // DeleteDC ниже — парный вызов.
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.0.is_null() {
        return None;
    }
    // SAFETY: SelectObject возвращает предыдущий объект; битмап выбирается
    // в память-DC, предыдущий объект не восстанавливаем (DC временный,
    // удаляется ниже вместе со всем).
    unsafe {
        let _ = SelectObject(dc, hbm.into());
    }

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            // Отрицательная высота: top-down, первая строка — верх
            // (порядок строк как у текстур rst-render).
            biWidth: bmp.bmWidth,
            biHeight: -bmp.bmHeight,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; w as usize * h as usize * 4];
    // SAFETY: bgra — буфер ровно w*h*4 байт; bmi валиден; DC держит
    // выбранный битмап; GetDIBits конвертирует в 32 bpp BI_RGB.
    let lines = unsafe {
        GetDIBits(
            dc,
            hbm,
            0,
            h,
            Some(bgra.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    // SAFETY: парный вызов к CreateCompatibleDC выше.
    unsafe {
        let _ = DeleteDC(dc);
    }
    if lines != h as i32 {
        return None;
    }
    Some(WindowIcon {
        width: w,
        height: h,
        rgba: bgra_to_rgba(&bgra, bmp.bmBitsPixel == 32),
    })
}

/// BGRA (32 bpp BI_RGB, 4 байта/пиксель) → RGBA. При `has_alpha == false`
/// (исходный битмап < 32 bpp — GetDIBits залил альфу нулями) альфа
/// форсируется в 255: у таких иконок альфа-канала нет, они непрозрачны.
/// Чистая функция — тестируется юнитами без Win32.
fn bgra_to_rgba(bgra: &[u8], has_alpha: bool) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        rgba.push(px[2]); // B → R
        rgba.push(px[1]); // G
        rgba.push(px[0]); // R → B
        rgba.push(if has_alpha { px[3] } else { 0xff });
    }
    rgba
}

/// Путь как wide-CString (Windows API принимает UTF-16; пути rst-win32 —
/// UTF-8, как в `file_dialog`/`process_info`).
fn wide_path(path: &Path) -> Option<Vec<u16>> {
    let text = path.as_os_str().to_str()?;
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    if wide.contains(&0) {
        return None; // встроенный NUL испортил бы C-строку
    }
    wide.push(0);
    Some(wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_to_rgba_swaps_channels_and_keeps_alpha() {
        let bgra = [0x11, 0x22, 0x33, 0xaa, 0x00, 0xff, 0x80, 0x00];
        assert_eq!(
            bgra_to_rgba(&bgra, true),
            vec![0x33, 0x22, 0x11, 0xaa, 0x80, 0xff, 0x00, 0x00]
        );
    }

    #[test]
    fn bgra_to_rgba_forces_opaque_without_alpha_channel() {
        // 24 bpp-битмап после GetDIBits в 32 bpp: альфа-байт нулевой,
        // настоящей альфы нет — пиксель обязан стать непрозрачным.
        let bgra = [0x10, 0x20, 0x30, 0x00];
        assert_eq!(bgra_to_rgba(&bgra, false), vec![0x30, 0x20, 0x10, 0xff]);
    }

    #[test]
    fn bgra_to_rgba_length_is_preserved() {
        let bgra = vec![0u8; 64];
        assert_eq!(bgra_to_rgba(&bgra, true).len(), 64);
    }

    #[test]
    fn bgra_to_rgba_trailing_partial_pixel_is_ignored() {
        // chunks_exact(4): хвост меньше пикселя отбрасывается, как в
        // premultiply_rgba (texture.rs).
        let bgra = [1u8, 2, 3, 4, 5];
        assert_eq!(bgra_to_rgba(&bgra, true).len(), 4);
    }
}
