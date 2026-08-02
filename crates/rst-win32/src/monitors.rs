//! Перечисление мониторов (M3): снапшот подключённых дисплеев — границы,
//! DPI, флаг основного и стабильный идентификатор устройства
//! (device interface path, ADR-010). Безопасная обёртка над
//! `EnumDisplayMonitors` + `GetMonitorInfoW` + `EnumDisplayDevicesW` +
//! `GetDpiForMonitor` (docs/M3_PREP_NOTES.md, раздел 2; ARCHITECTURE.md §7).
//!
//! Снапшот не зависит от окон: `HMONITOR` — сессионный хендл и наружу не
//! выходит (§2.4). Состояния модуль не хранит: каждый вызов — новый
//! снимок «что подключено сейчас», а сравнение снапшотов (монитор пропал /
//! вернулся, ADR-011) — задача координатора. Замечание по API: рецепт
//! ADR-010 реализуется через `EnumDisplayDevicesW` (фичи `Win32_Graphics_Gdi`
//! достаточно); `DisplayConfigGetDeviceInfo` (фича `Win32_Devices_Display`)
//! даёт тот же device interface path, но не нужен — не подключаем.

use tracing::warn;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    EDD_GET_DEVICE_INTERFACE_NAME, MONITORINFOF_PRIMARY,
};
use windows::core::BOOL;
use windows::core::PCWSTR;

use rst_core::model::{MonitorId, Rect};

use crate::error::Win32Error;

/// DPI «100%» — фолбэк при отказе `GetDpiForMonitor`.
const DEFAULT_DPI: u32 = 96;

/// Снапшот одного подключённого монитора (docs/M3_PREP_NOTES.md, §2.1).
/// Напрямую мапится на `rst_core::model::MonitorRecord` (`last_scale =
/// dpi / 96.0`, `last_bounds = bounds_px`, `last_seen` = момент снапшота).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Стабильный device interface path (ADR-010), форма
    /// `rst_core::model::MonitorId`: `\\?\DISPLAY#<model>#<instance>#{...}`.
    pub id: MonitorId,
    /// Дружественное имя монитора для UI (`DeviceString`).
    pub friendly_name: String,
    /// Границы в физических пикселях виртуального десктопа; у неосновных
    /// мониторов `x`/`y` могут быть отрицательными.
    pub bounds_px: Rect,
    /// Точки на дюйм (96 = 100%); масштаб = `dpi / 96.0`.
    pub dpi: u32,
    /// Основной монитор (`MONITORINFOF_PRIMARY`).
    pub is_primary: bool,
}

/// Перечислить все подключённые мониторы одним снапшотом
/// (docs/M3_PREP_NOTES.md, §2.3: целиком или ошибка, без частичной
/// деградации по одному монитору).
///
/// Порядок — как вернула ОС (обычно основной первым, но не гарантируется;
/// на порядок не полагаться — есть флаг [`MonitorInfo::is_primary`]).
pub fn enumerate() -> Result<Vec<MonitorInfo>, Win32Error> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: `handles` живёт весь вызов и не разделяется; колбэк —
    // синхронный, на этом же потоке, указатель действует только внутри
    // EnumDisplayMonitors.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_proc),
            LPARAM(&raw mut handles as isize),
        )
    }
    .ok()?;
    handles.into_iter().map(monitor_info).collect()
}

/// Колбэк `EnumDisplayMonitors`: просто собирает хендлы в вектор.
extern "system" fn enum_monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` — &mut Vec<HMONITOR> из `enumerate`, живой на всё
    // время вызова EnumDisplayMonitors; колбэк синхронный, гонок нет.
    unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) }.push(hmonitor);
    BOOL(1)
}

/// Снапшот одного монитора по его сессионному хендлу.
fn monitor_info(hmonitor: HMONITOR) -> Result<MonitorInfo, Win32Error> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` — валидный буфер MONITORINFOEXW с выставленным cbSize;
    // по repr(C) структура начинается с MONITORINFO, каст указателя легален
    // (документированный Win32-паттерн MONITORINFOEX).
    unsafe { GetMonitorInfoW(hmonitor, (&raw mut info).cast::<MONITORINFO>()) }.ok()?;

    let mi = &info.monitorInfo;
    // Стабильный id (ADR-010): device interface path. GDI-имя `szDevice`
    // (`\\.\DISPLAY1`, нестабильно) — только вход для этого запроса, наружу
    // не уходит.
    let id =
        utf16z_to_string(&display_device(&info.szDevice, EDD_GET_DEVICE_INTERFACE_NAME)?.DeviceID);
    // Дружественное имя — отдельным вызовом без EDD-флага (поле DeviceString).
    let friendly_name = utf16z_to_string(&display_device(&info.szDevice, 0)?.DeviceString);

    Ok(MonitorInfo {
        id: MonitorId(id),
        friendly_name,
        bounds_px: rect_px(mi.rcMonitor),
        dpi: effective_dpi(hmonitor),
        is_primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// Прочитать `DISPLAY_DEVICEW` для GDI-имени с заданными флагами. Отдельный
/// вызов на каждый режим: состав заполненных полей зависит от
/// `EDD_GET_DEVICE_INTERFACE_NAME` (MSDN).
fn display_device(gdi_name: &[u16], flags: u32) -> Result<DISPLAY_DEVICEW, Win32Error> {
    let mut dd = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    // SAFETY: `gdi_name` — нуль-терминированный UTF-16 буфер, живой во время
    // вызова; `dd` — валидный буфер с выставленным cb.
    unsafe { EnumDisplayDevicesW(PCWSTR(gdi_name.as_ptr()), 0, &mut dd, flags) }.ok()?;
    Ok(dd)
}

/// DPI монитора. При отказе API (экзотические драйверы) — 96 с записью в
/// лог: без DPI продолжать можно (масштаб 100%), без снапшота — нет.
fn effective_dpi(hmonitor: HMONITOR) -> u32 {
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: out-параметры валидны.
    let hr = unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    if let Err(e) = hr {
        warn!(error = %e, "GetDpiForMonitor failed, assuming 96 DPI");
        return DEFAULT_DPI;
    }
    dpi_x
}

/// Нуль-терминированный UTF-16 → String: обрезка по первому NUL;
/// незавершённый буфер берётся целиком (для юнит-тестов вынесено из Win32-кода).
fn utf16z_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `RECT` Win32 (left/top/right/bottom; возможны отрицательные координаты
/// виртуального десктопа) → `Rect` модели (x/y + неотрицательные размеры).
fn rect_px(rc: RECT) -> Rect {
    Rect {
        x: rc.left,
        y: rc.top,
        w: (rc.right - rc.left).max(0) as u32,
        h: (rc.bottom - rc.top).max(0) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn utf16z_trims_at_first_nul() {
        assert_eq!(
            utf16z_to_string(&utf16("\\\\?\\DISPLAY#GSM5B09")),
            "\\\\?\\DISPLAY#GSM5B09"
        );
        // Мусор за NUL отбрасывается.
        let mut buf = utf16("abc");
        buf.extend(utf16("garbage"));
        assert_eq!(utf16z_to_string(&buf), "abc");
    }

    #[test]
    fn utf16z_empty_and_unterminated() {
        assert_eq!(utf16z_to_string(&[0]), "");
        assert_eq!(utf16z_to_string(&[]), "");
        // Без завершающего NUL — берём весь буфер.
        let buf: Vec<u16> = "xy".encode_utf16().collect();
        assert_eq!(utf16z_to_string(&buf), "xy");
    }

    #[test]
    fn rect_px_primary_at_origin() {
        let rc = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(
            rect_px(rc),
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
    }

    #[test]
    fn rect_px_negative_virtual_desktop_coords() {
        // Монитор слева от основного: отрицательный x (ARCHITECTURE.md §7).
        let rc = RECT {
            left: -1280,
            top: 120,
            right: 0,
            bottom: 1144,
        };
        assert_eq!(
            rect_px(rc),
            Rect {
                x: -1280,
                y: 120,
                w: 1280,
                h: 1024
            }
        );
    }

    #[test]
    fn rect_px_degenerate_clamps_to_zero() {
        let rc = RECT {
            left: 100,
            top: 100,
            right: 50,
            bottom: 100,
        };
        assert_eq!(
            rect_px(rc),
            Rect {
                x: 100,
                y: 100,
                w: 0,
                h: 0
            }
        );
    }

    #[test]
    #[ignore = "требует реальное железо; запуск вручную: cargo test -p rst-win32 monitors -- --ignored"]
    fn enumerate_returns_consistent_snapshot() {
        let monitors = enumerate().expect("перечисление мониторов");
        assert!(!monitors.is_empty(), "хотя бы один монитор подключён");
        assert_eq!(
            monitors.iter().filter(|m| m.is_primary).count(),
            1,
            "ровно один основной"
        );
        for m in &monitors {
            assert!(
                m.id.0.starts_with("\\\\?\\DISPLAY#"),
                "id не похож на device interface path: {}",
                m.id.0
            );
            assert!(
                m.bounds_px.w > 0 && m.bounds_px.h > 0,
                "нулевые границы: {m:?}"
            );
            assert!(m.dpi >= DEFAULT_DPI, "dpi={} у {m:?}", m.dpi);
            assert!(!m.friendly_name.is_empty(), "пустое friendly_name: {m:?}");
        }
    }
}
