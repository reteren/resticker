//! Снимок содержимого чужого окна для своего переключателя окон
//! (M9, docs/TILING_DESIGN.md §Р3; docs/research/tiling/R6_GROUPS_ALTTAB_ANIM.md §2).
//!
//! Путь: `PrintWindow(PW_RENDERFULLCONTENT)` в DIB-секцию → биты BGRA →
//! конверсия в RGBA (тот же формат, что [`crate::window_enum::WindowIcon`])
//! → проверка «не чёрный» → билинейное ужатие по большей стороне.
//!
//! Почему `PrintWindow`, а не `DwmRegisterThumbnail`/`Windows.Graphics.Capture`:
//! R6 §2 — `DwmRegisterThumbnail` рисует превью в клиентскую область НАШЕГО
//! окна (с нашим D3D11+DirectComposition-оверлеем не стыкуется), а
//! `Windows.Graphics.Capture` заводит живую сессию захвата на каждое превью —
//! память/VRAM на 20+ окнах. `PW_RENDERFULLCONTENT` (0x2, Windows 8.1+) —
//! единственный, кто корректно снимает окна с аппаратным композитингом
//! (Chromium, Electron); флаг в документации `PrintWindow` НЕ описан, но
//! общепринят (R6 §2).
//!
//! ## Стоимость и кэширование (НЕ реализовано здесь, это дело вызывающего)
//!
//! Один вызов — синхронные миллисекунды–десятки миллисекунд (R6 §2: «стоимость —
//! миллисекунды–десятки миллисекунд на окно, синхронно»), плюс `PrintWindow`
//! на зависшем окне способен затянуть вызов. Поэтому:
//!
//! * Вызывать `capture` можно ТОЛЬКО на открытии переключателя (и при смене
//!   выделения — не чаще), никогда — на каждый кадр оверлея.
//! * Вызывать желательно с фонового потока (тот же урок, что bounded-fetch
//!   заголовка, `window_enum.rs:694`), и кэшировать `WindowThumb` по `hwnd`
//!   на время жизни переключателя. Кэш по аналогии с `window_enum::ICON_CACHE`
//!   (`window_enum.rs:110`): `HashMap<usize, Option<WindowThumb>>` + вытеснение
//!   по закрытию окна. Иконка остаётся мгновенным fallback'ом на время захвата.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP, HDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindow, IsWindowVisible};

/// Снимок окна в RGBA (тот же формат, что `WindowIcon` в window_enum.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowThumb {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `PW_RENDERFULLCONTENT` (0x2): снимать окно с учётом аппаратного
/// композитинга, а не только GDI-содержимого. В документации `PrintWindow`
/// флага нет (документирован только `PW_CLIENTONLY`) — задаём вручную,
/// см. докмодуль.
const PW_RENDERFULLCONTENT: u32 = 0x2;

/// Среднее значение канала (0–255), ниже которого снимок считается пустым.
///
/// Чёрный снимок от DRM-видео и части GPU-приложений — это кадр из нулей
/// (R6 §2: «часть GPU-окон отдаёт чёрный прямоугольник»). Порог 24 (≈9%)
/// лежит заметно выше нулевого кадра, но НИЖЕ самой тёмной реальной
/// картинки: тёмная тема интерфейса даёт в среднем 10–15% (фон ~#1E1E1E —
/// 12% + текст), чёрный кадр — 0%. Ошибка в обе стороны стоит дёшево:
/// ложное срабатывание показывает иконку вместо превью, пропуск чёрного —
/// чёрный прямоугольник в переключателе.
pub const BLACK_LUMA_THRESHOLD: u8 = 24;

/// Потолок для `max_side`: превью переключателя не бывает больше пары сотен
/// пикселей, а 2048×2048×4 байт — и так 16 МБ на одно окно. Клампим, а не
/// отказываемся: запросивший 10000 хотел «побольше», а не «сломай мне память».
const MAX_CAPTURE_SIDE: u32 = 2048;

/// Площадь исходного кадра, выше которой снимок не берём.
///
/// `PrintWindow` снимает окно в ПОЛНЫЙ размер (масштабировать при захвате
/// нельзя — GDI рисует в координатах окна и обрезает по границе DC), поэтому
/// буфер растёт с площадью окна. 4096×4096 px = 67 МБ на промежуточный кадр —
/// потолок: окна крупнее (8K-мониторы) для превью не нужны, а гигантские
/// «окна» (сломанная геометрия) не должны приводить к выделению сотен МБ.
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;

/// Снять содержимое окна, ужав по большей стороне до `max_side`.
///
/// `None` — окна нет, оно скрыто, имеет нулевой/неправдоподобный размер,
/// `max_side == 0`, `PrintWindow` отказал или снимок оказался чёрным (пустой
/// кадр — вызывающий показывает иконку). Не паникует ни при каких значениях.
///
/// Зависшее окно может затянуть вызов (синхронный `PrintWindow`) — звать с
/// фонового потока и кэшировать (см. докмодуль).
pub fn capture(hwnd: usize, max_side: u32) -> Option<WindowThumb> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // Порядок проверок важен для тестов: дешёвые и не требующие живого окна —
    // первыми. max_side == 0 — бессмысленный запрос («сними в ноль пикселей»).
    if max_side == 0 {
        return None;
    }
    // SAFETY: IsWindow/IsWindowVisible безопасны для любого значения хэндла,
    // включая уже уничтоженное окно.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return None;
    }
    let (w, h) = window_size(hwnd)?;
    if w == 0 || h == 0 {
        return None; // свёрнутое или ещё не отрисованное окно: снимать нечего
    }
    if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return None;
    }

    // GDI-ресурсы создаются ЗДЕСЬ и освобождаются ниже. Отказы после
    // создания DIB-секции (PrintWindow=false, чёрный кадр) выражаются
    // значением `result`, а не выходом из функции.
    //
    // Единственное исключение — отказ самой `create_dib_section`: там DC уже
    // создан, а битмапа ещё нет, и выходить приходится досрочно. Этот путь
    // освобождает DC явно; раньше он утекал, а комментарий утверждал
    // обратное (второе ревью, находка 3).
    // SAFETY: CreateCompatibleDC(None) — десктопный DC без владения окном.
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.0.is_null() {
        return None;
    }
    // SAFETY: CreateDIBSection возвращает битмап, владение нашим; ppvBits —
    // указатель на выделенные системой биты (без отдельного буфера и
    // GetDIBits — в отличие от window_icon.rs:158, где битмап чужой).
    let Some((hbm, bits)) = create_dib_section(dc, w, h) else {
        // SAFETY: dc создан выше и ещё не удалён; DeleteDC безопасен.
        unsafe {
            let _ = DeleteDC(dc);
        }
        return None;
    };
    // SAFETY: SelectObject возвращает предыдущий объект; не восстанавливаем —
    // DC временный, удаляется ниже целиком (тот же приём, что window_icon.rs:136).
    unsafe {
        let _ = SelectObject(dc, hbm.into());
    }
    // SAFETY: hwnd живой (проверен выше); dc держит выбранную DIB-секцию;
    // PrintWindow не блокируется навсегда по контракту, но на «задумавшемся»
    // окне может занять заметное время (см. докмодуль про поток).
    let ok = unsafe { PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)) }.as_bool();

    // ВСЕ отказы после этого — через `result`, ресурсы освобождаются после.
    let result = if ok {
        let len = (w as usize) * (h as usize) * 4;
        // SAFETY: bits указывает на len байт, выделенных CreateDIBSection.
        let bgra = unsafe { std::slice::from_raw_parts(bits as *const u8, len) };
        let rgba = bgra_to_rgba(bgra);
        if is_black_snapshot(&rgba) {
            None // пустой кадр (DRM/GPU-приложения) — иконка вместо чёрного
        } else {
            let side = max_side.min(MAX_CAPTURE_SIDE);
            let (tw, th) = target_size(w, h, side);
            Some(WindowThumb {
                width: tw,
                height: th,
                rgba: downscale_bilinear(&rgba, w, h, tw, th),
            })
        }
    } else {
        None // PrintWindow отказал (окно закрылось в момент захвата и т.п.)
    };

    // SAFETY: hbm/dc — наши, живые; парные вызовы к CreateDIBSection/
    // CreateCompatibleDC выше.
    unsafe {
        let _ = DeleteObject(hbm.into());
        let _ = DeleteDC(dc);
    }
    result
}

/// Размер окна в пикселях. Мусорная геометрия (отрицательные размеры,
/// свёрнутые окна) → `None`, а не паника: размеры выходят из RECT,
/// который может быть чем угодно.
fn window_size(hwnd: HWND) -> Option<(u32, u32)> {
    let mut rect = RECT::default();
    // SAFETY: GetWindowRect пишет в наш стек; для чужих/мёртвых окон безопасен.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    let w = rect.right.saturating_sub(rect.left);
    let h = rect.bottom.saturating_sub(rect.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((w as u32, h as u32))
}

/// Создать DIB-секцию `w×h` (32 bpp, top-down) и вернуть битмап + указатель
/// на биты. Ошибка GDI → `None` (DC остаётся на вызывающем).
fn create_dib_section(dc: HDC, w: u32, h: u32) -> Option<(HBITMAP, *mut core::ffi::c_void)> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            // Отрицательная высота: top-down, первая строка — верх (тот же
            // порядок строк, что у текстур rst-render и у window_icon.rs:147).
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: bmi валиден и живёт на время вызова; bits — out-параметр.
    let hbm = match unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) }
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "CreateDIBSection не удалась — снимок окна невозможен");
            return None;
        }
    };
    if hbm.0.is_null() || bits.is_null() {
        // SAFETY: hbm может быть нулевым (тогда удалять нечего).
        if !hbm.0.is_null() {
            unsafe {
                let _ = DeleteObject(hbm.into());
            }
        }
        return None;
    }
    Some((hbm, bits))
}

/// BGRA (32 bpp BI_RGB) → RGBA.
///
/// Та же конверсия, что `window_icon.rs:187` (там она private, отсюда копия):
/// B и R меняются местами, альфа переносится как есть. Яркость считает
/// отдельная [`is_black_snapshot`] — два прохода по буферу дешевле, чем
/// тащить два смысла в одну функцию.
fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        rgba.push(px[2]); // B → R
        rgba.push(px[1]); // G
        rgba.push(px[0]); // R → B
        rgba.push(px[3]);
    }
    rgba
}

/// Снимок почти чёрный — пустой кадр от DRM/GPU-окон?
///
/// Чистая функция над RGBA-буфером, тестируется без окон. Пустой буфер —
/// `false` (в `capture` пустой буфер не возникает; здесь не чернеем на ровном
/// месте). Порог — [`BLACK_LUMA_THRESHOLD`].
fn is_black_snapshot(rgba: &[u8]) -> bool {
    if rgba.len() < 4 {
        return false;
    }
    let mut sum = 0.0;
    for px in rgba.chunks_exact(4) {
        sum += (f64::from(px[0]) + f64::from(px[1]) + f64::from(px[2])) / 3.0;
    }
    // Скобки вокруг левой части обязательны: `as f64 <` парсится как generic.
    (sum / (rgba.len() / 4) as f64) < f64::from(BLACK_LUMA_THRESHOLD)
}

/// Целевой размер после ужатия по большей стороне до `side` (сохраняя
/// пропорции; уже меньшее окно не увеличивается).
fn target_size(w: u32, h: u32, side: u32) -> (u32, u32) {
    let side = side.max(1);
    let scale = (side as f64 / w as f64)
        .min(side as f64 / h as f64)
        .min(1.0);
    let tw = ((w as f64 * scale).round() as u32).max(1);
    let th = ((h as f64 * scale).round() as u32).max(1);
    (tw, th)
}

/// Билинейное ужатие RGBA-кадра. Чистая функция — тестируется без Win32.
///
/// Для каждого целевого пикселя берётся точка в исходном кадре
/// `((dx + 0.5) * src_w / dst_w - 0.5, ...)` — центр целевого пикселя в
/// координатах источника — и интерполируются четыре соседа по каждой оси.
/// Координаты клампятся к границам: края не «дырявят» (1×1 → 2×2 даёт
/// копию, а не мусор).
fn downscale_bilinear(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for dy in 0..dst_h {
        let sy = (dy as f64 + 0.5) * src_h as f64 / dst_h as f64 - 0.5;
        for dx in 0..dst_w {
            let sx = (dx as f64 + 0.5) * src_w as f64 / dst_w as f64 - 0.5;
            let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
            let fx = sx - f64::from(x0);
            let fy = sy - f64::from(y0);
            for c in 0..4 {
                let a = sample(src, src_w, src_h, x0, y0, c);
                let b = sample(src, src_w, src_h, x0 + 1, y0, c);
                let c2 = sample(src, src_w, src_h, x0, y0 + 1, c);
                let d = sample(src, src_w, src_h, x0 + 1, y0 + 1, c);
                let top = a + (b - a) * fx;
                let bot = c2 + (d - c2) * fx;
                out.push((top + (bot - top) * fy) as u8);
            }
        }
    }
    out
}

/// Один канал пикселя с клампом координат к границам кадра.
fn sample(src: &[u8], src_w: u32, src_h: u32, x: i32, y: i32, channel: usize) -> f64 {
    let x = x.clamp(0, src_w as i32 - 1) as u32;
    let y = y.clamp(0, src_h as i32 - 1) as u32;
    f64::from(src[((y * src_w + x) * 4 + channel as u32) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_handle_returns_none_not_panic() {
        assert!(capture(0xDEAD_BEEF, 200).is_none());
    }

    #[test]
    fn zero_max_side_returns_none_not_panic() {
        // max_side == 0 проверяется до всех остальных условий — тест не
        // требует живого окна и не должен зависеть от порядка других проверок.
        assert!(capture(0xDEAD_BEEF, 0).is_none());
    }

    #[test]
    fn black_buffer_is_rejected_by_brightness_check() {
        let black = vec![0u8; 64 * 64 * 4];
        assert!(is_black_snapshot(&black));
    }

    #[test]
    fn light_buffer_is_accepted_by_brightness_check() {
        // Серый 50% — средняя яркость 127, заметно выше порога.
        let light = vec![127u8; 64 * 64 * 4];
        assert!(!is_black_snapshot(&light));
    }

    #[test]
    fn dark_theme_grey_is_not_rejected_as_black() {
        // Фон тёмной темы #1E1E1E ≈ 30 из 255: ниже порога его средняя
        // яркость не опускается, иначе тёмные приложения теряли бы превью.
        let dark_theme = [0x1E, 0x1E, 0x1E, 0xFF].repeat(64 * 64);
        assert!(!is_black_snapshot(&dark_theme));
    }

    #[test]
    fn empty_buffer_is_not_black() {
        // Пустой буфер в capture не возникает; здесь — не чернеть на ровном
        // месте, чтобы чистые тесты не зависели от порядка.
        assert!(!is_black_snapshot(&[]));
    }

    #[test]
    fn bgra_to_rgba_swaps_channels() {
        // BGRA: B=0x11, G=0x22, R=0x33, A=0xaa → RGBA: R=0x33, G=0x22, B=0x11, A=0xaa.
        let bgra = [0x11, 0x22, 0x33, 0xaa];
        assert_eq!(bgra_to_rgba(&bgra), vec![0x33, 0x22, 0x11, 0xaa]);
    }

    #[test]
    fn downscale_2x2_to_1x1_averages_the_four_pixels() {
        // Четыре разных угла: билинейная интерполяция в центр (0.5, 0.5) —
        // ровно среднее арифметическое по каждому каналу.
        let mut src = Vec::with_capacity(2 * 2 * 4);
        for v in [0u8, 100, 100, 200] {
            src.extend_from_slice(&[v, 0, 0, 255]); // R=v, G=B=0
        }
        let out = downscale_bilinear(&src, 2, 2, 1, 1);
        assert_eq!(out, vec![100, 0, 0, 255]);
    }

    #[test]
    fn downscale_1x1_to_2x2_clamps_coordinates_and_keeps_color() {
        // Увеличение не по назначению (ужимка), но функция обязана не
        // паниковать и не дырявить: координаты клампятся, все четыре пикселя
        // — копия единственного исходного.
        let src = vec![200, 100, 50, 255];
        let out = downscale_bilinear(&src, 1, 1, 2, 2);
        assert_eq!(
            out,
            vec![
                200, 100, 50, 255, 200, 100, 50, 255, 200, 100, 50, 255, 200, 100, 50, 255
            ]
        );
    }

    #[test]
    fn downscale_preserves_length_and_bounds() {
        let src = vec![7u8; 16 * 16 * 4];
        let out = downscale_bilinear(&src, 16, 16, 4, 4);
        assert_eq!(out.len(), 4 * 4 * 4);
        // 16×16 → 4×4: каждая точка источника попадает в интерполяцию с
        // клампом, результат остаётся в диапазоне исходного значения.
        assert!(out.iter().all(|&v| v == 7));
    }

    #[test]
    fn target_size_respects_the_side_limit() {
        // 1920×1080 → 200: 1080 * 200/1920 = 112.5 → round = 113; большая
        // сторона ровно 200, вторая — не больше пропорциональной доли.
        assert_eq!(target_size(1920, 1080, 200), (200, 113));
        assert_eq!(target_size(1920, 1080, 0), (1, 1), "side клампится в 1");
    }

    #[test]
    fn target_size_never_upscales_small_windows() {
        assert_eq!(target_size(320, 240, 2000), (320, 240));
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_thumb -- --ignored"]
    fn live_window_capture_returns_small_non_black_thumb() {
        let windows = crate::window_enum::enumerate();
        let Some(win) = windows.first() else {
            panic!("на живом десктопе есть хотя бы одно окно");
        };
        let thumb = capture(win.hwnd, 100).expect("снимок живого окна");
        assert!(
            thumb.width <= 100 && thumb.height <= 100,
            "ужато по большей стороне"
        );
        assert_eq!(
            thumb.rgba.len(),
            thumb.width as usize * thumb.height as usize * 4
        );
        assert!(
            !is_black_snapshot(&thumb.rgba),
            "живое окно не отдаёт пустой кадр"
        );
    }

    /// Скрытое тестовое окно текущего потока (тот же паттерн, что
    /// `TestWindow` в window_pin.rs:2176 — нити сообщений не требует).
    struct HiddenWindow(HWND);

    impl HiddenWindow {
        fn create() -> Self {
            use windows::Win32::Foundation::GetLastError;
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, RegisterClassExW, WNDCLASSEXW, WS_OVERLAPPED,
            };
            use windows::core::w;

            // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
            let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(hidden_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: w!("resticker_window_thumb_test"),
                ..Default::default()
            };
            // SAFETY: wc заполнена корректно; повторная регистрация класса
            // (параллельные тесты) — не ошибка.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                // SAFETY: осмысленна сразу после провалившегося вызова.
                let err = unsafe { GetLastError() };
                assert_eq!(err, windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS);
            }
            // SAFETY: аргументы — валидные константы и зарегистрированный
            // класс; WS_OVERLAPPED БЕЗ WS_VISIBLE — окно существует, но
            // скрыто (ровно то, что проверяет тест).
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("resticker_window_thumb_test"),
                    w!("hidden thumb test"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    300,
                    200,
                    None,
                    None,
                    Some(hinstance.into()),
                    None,
                )
            }
            .expect("создание тестового окна");
            Self(hwnd)
        }
    }

    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            // SAFETY: окно создано этим же потоком выше.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0);
            }
        }
    }

    unsafe extern "system" fn hidden_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        // SAFETY: делегирование системному обработчику.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_thumb -- --ignored"]
    fn live_hidden_window_returns_none() {
        let hidden = HiddenWindow::create();
        assert!(capture(hidden.0.0 as usize, 100).is_none());
    }
}
