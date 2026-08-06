//! Чистые хелперы форматов: PTS → `Duration`, размеры плоскостей поддерживаемых
//! пиксельных форматов (YUV420P/YUVA420P/YUVA444P10LE/qtrle), понижение
//! 10-бит → 8-бит и конверсия packed RGB (qtrle) в YUVA420P на CPU. Единственная
//! FFmpeg-зависимость — enum-константы `AVPixelFormat` (compile-time, не FFI):
//! юнит-тесты не требуют ни видеофайлов, ни DLL.

use std::time::Duration;

use ffmpeg_sys_next::AVPixelFormat;

/// Пересчёт PTS из таймбезы потока (num/den, обычно 1/90000, 1/1000, 1/30)
/// в длительность от начала потока. Аналог `av_rescale_q(pts, tb, AV_TIME_BASE_Q)`
/// без FFmpeg: микросекунды считаются в i128 (переполнения i64 не бывает для
/// реальных таймбез), отрицательный/невалидный PTS (в т.ч. `AV_NOPTS_VALUE`)
/// клэмпятся к нулю.
pub(crate) fn pts_to_duration(pts: i64, tb_num: i32, tb_den: i32) -> Duration {
    if pts < 0 || tb_den <= 0 {
        return Duration::ZERO;
    }
    // Секунды = pts * num / den; результат — в микросекундах (×1e6), как у
    // av_rescale_q(pts, tb, AV_TIME_BASE_Q). Считается в i128 — переполнения
    // i64 для реальных таймбез не бывает.
    let us = (pts as i128 * tb_num as i128 * 1_000_000) / tb_den as i128;
    if us <= 0 {
        return Duration::ZERO;
    }
    Duration::from_micros(us.min(u64::MAX as i128) as u64)
}

/// Размеры плоскостей YUV420P (4:2:0) для кадра `w×h`: Y — полное разрешение,
/// U/V — половина по каждой оси с округлением вверх (нечётные размеры).
pub(crate) fn yuv420p_plane_sizes(w: u32, h: u32) -> (usize, usize, usize) {
    let y = w as usize * h as usize;
    let cw = (w as usize).div_ceil(2);
    let ch = (h as usize).div_ceil(2);
    (y, cw * ch, cw * ch)
}

/// Пиксельные форматы выходных кадров, которые умеет распаковывать пайплайн
/// (M5e, ROADMAP.md): обычное непрозрачное видео, три варианта с альфой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoPixelFormat {
    /// YUV420P: Y — полное разрешение, U/V — половина (4:2:0), без альфы.
    /// Стандартный выход H.264/HEVC/VP9 без альфа-потока.
    Yuv420p,
    /// YUVA420P: как YUV420P + альфа-плоскость полного разрешения
    /// (WebM/VP9 с альфой, Matroska BlockAdditional).
    Yuva420p,
    /// YUVA444P10LE (ProRes 4444/4444 XQ в MOV): 4:4:4 — все четыре
    /// плоскости полного разрешения, 10 бит в 16-бит LE контейнере.
    /// При распаковке понижается до 8 бит на CPU (полноценный 10-бит
    /// рендер вне скоупа M5e — задокументированное упрощение).
    Yuva444p10le,
    /// Packed RGB(A) с qtrle-декодера (QuickTime Animation, SPEC §7.2):
    /// конкретный порядок каналов — в [`RgbPacked`]. Конвертируется в
    /// YUVA420P на CPU при распаковке (swscale в этой сборке FFmpeg
    /// отключён — дизайн M5b §1; qtrle-файлы исторически маленькие).
    Rgb32,
}

/// Packed-подвид [`VideoPixelFormat::Rgb32`]: порядок каналов в памяти.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RgbPacked {
    Argb,
    Bgra,
    /// Без альфа-канала (24 бита) — распаковывается с a = 255.
    Rgb24,
}

impl RgbPacked {
    /// Байт на пиксель packed-строки.
    pub(crate) fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Argb | Self::Bgra => 4,
            Self::Rgb24 => 3,
        }
    }
}

/// Байтовые размеры выходных (8-битных) плоскостей кадра `w×h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaneSizes {
    pub y: usize,
    pub u: usize,
    pub v: usize,
    /// Альфа-плоскость (полное разрешение), если формат несёт альфу.
    pub alpha: Option<usize>,
}

impl VideoPixelFormat {
    /// Размеры плоскостей кадра `w×h` в байтах ВЫХОДА (8 бит на сэмпл —
    /// 10-бит понижается до распаковки, а не хранится).
    pub(crate) fn plane_sizes(&self, w: u32, h: u32) -> PlaneSizes {
        let (w, h) = (w as usize, h as usize);
        match self {
            Self::Yuv420p => {
                let (y, u, v) = yuv420p_plane_sizes(w as u32, h as u32);
                PlaneSizes {
                    y,
                    u,
                    v,
                    alpha: None,
                }
            }
            Self::Yuva420p | Self::Rgb32 => {
                let (y, u, v) = yuv420p_plane_sizes(w as u32, h as u32);
                PlaneSizes {
                    y,
                    u,
                    v,
                    alpha: Some(w * h),
                }
            }
            Self::Yuva444p10le => PlaneSizes {
                y: w * h,
                u: w * h,
                v: w * h,
                alpha: Some(w * h),
            },
        }
    }

    /// Размеры плоскостей U/V: 4:2:0 — половина по каждой оси (округление
    /// вверх), 4:4:4 — полное разрешение. Нужны [`crate::pipeline::VideoFrameOut`]
    /// для создания текстур в rst-render.
    pub(crate) fn chroma_dims(&self, w: u32, h: u32) -> (u32, u32) {
        match self {
            Self::Yuva444p10le => (w, h),
            _ => (w.div_ceil(2), h.div_ceil(2)),
        }
    }

    /// Несёт ли формат альфа-канал (тогда выходной кадр содержит 4-ю
    /// плоскость, и рендер обязан умножать цвет на альфу).
    pub(crate) fn has_alpha(&self) -> bool {
        !matches!(self, Self::Yuv420p)
    }

    /// Может ли формат декодироваться аппаратно (d3d11va, M5c). Аппаратные
    /// декодеры не отдают альфа-канал (он лежит отдельным битстримом,
    /// который hwaccel игнорирует) и не поддерживают 4:4:4 10-бит —
    /// форматы с альфой обязаны идти МИМО аппаратного пути, строго
    /// программным декодом (ROADMAP M5e; ARCHITECTURE.md §4.4 «всегда для
    /// видео с альфой»). Предикат — единое место, где будущий hwaccel-путь
    /// отсекает эти форматы.
    pub(crate) fn hwaccel_compatible(&self) -> bool {
        matches!(self, Self::Yuv420p)
    }

    /// Байт на сэмпл плоскости В ИСХОДНИКЕ декодера: 1 — 8-бит, 2 — 10-бит
    /// в 16-бит LE контейнере (строки таких плоскостей вдвое шире).
    pub(crate) fn source_bytes_per_sample(&self) -> usize {
        if matches!(self, Self::Yuva444p10le) {
            2
        } else {
            1
        }
    }
}

/// Классифицировать `AVFrame::format` (i32) в поддерживаемый пайплайном
/// формат. `None` — неподдерживаемый формат (отдаётся
/// [`crate::error::VideoError::UnsupportedPixelFormat`]).
pub(crate) fn classify_pixel_format(fmt: i32) -> Option<(VideoPixelFormat, Option<RgbPacked>)> {
    // AVPixelFormat — C-перечисление, значения сравниваются численно.
    match fmt as u32 {
        x if x == AVPixelFormat::AV_PIX_FMT_YUV420P as u32 => {
            Some((VideoPixelFormat::Yuv420p, None))
        }
        x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P as u32 => {
            Some((VideoPixelFormat::Yuva420p, None))
        }
        x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P10LE as u32 => {
            Some((VideoPixelFormat::Yuva444p10le, None))
        }
        x if x == AVPixelFormat::AV_PIX_FMT_ARGB as u32 => {
            Some((VideoPixelFormat::Rgb32, Some(RgbPacked::Argb)))
        }
        x if x == AVPixelFormat::AV_PIX_FMT_BGRA as u32 => {
            Some((VideoPixelFormat::Rgb32, Some(RgbPacked::Bgra)))
        }
        x if x == AVPixelFormat::AV_PIX_FMT_RGB24 as u32 => {
            Some((VideoPixelFormat::Rgb32, Some(RgbPacked::Rgb24)))
        }
        _ => None,
    }
}

/// Понизить 10-бит планарный кадр до 8 бит на месте: сэмплы лежат в 16-бит
/// LE контейнере (2 байта на сэмпл), берутся старшие 8 бит (`>> 2`).
/// Запись идёт с начала буфера, чтение — парами по 2 байта, записанный байт
/// не перечитывается (нечётные байты контейнера не читаются) — сжатие
/// in-place безопасно. Полноценный 10-бит рендер вне скоупа M5e (упрощение,
/// задокументировано в ROADMAP.md).
pub(crate) fn downconvert_10bit_le(plane: &mut [u8]) {
    debug_assert!(plane.len() % 2 == 0, "16-бит контейнер: чётное число байт");
    for i in 0..plane.len() / 2 {
        let sample = u16::from_le_bytes([plane[2 * i], plane[2 * i + 1]]);
        plane[i] = (sample >> 2) as u8;
    }
}

/// Packed RGB(A) кадр (qtrle) → YUVA420P (8 бит, BT.709 limited range):
/// Y — полное разрешение, U/V — среднее по 2×2 блокам (кодовая точка 4:2:0),
/// альфа — полное разрешение (для RGB24 — 255). Единственный честный путь
/// для qtrle в этой сборке FFmpeg: swscale отключён (дизайн M5b §1), шейдер
/// rst-render конвертирует только YUV. Коэффициенты — зеркало `mainVideoPS`
/// (219/224, 1.5748/0.1873/0.4681/1.8556), иначе цвета qtrle-кадров
/// «поплывут» относительно обычного видео.
pub(crate) fn rgb_packed_to_yuva420p(
    src: &[u8],
    width: u32,
    height: u32,
    fmt: RgbPacked,
) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let bpp = fmt.bytes_per_pixel();
    let mut y = vec![0u8; w * h];
    let mut a = vec![255u8; w * h];
    let mut u_acc = vec![0u32; cw * ch];
    let mut v_acc = vec![0u32; cw * ch];
    let mut cnt = vec![0u8; cw * ch];
    for py in 0..h {
        for px in 0..w {
            let i = (py * w + px) * bpp;
            let (r, g, b, alpha) = match fmt {
                RgbPacked::Argb => (src[i + 1], src[i + 2], src[i + 3], src[i]),
                RgbPacked::Bgra => (src[i + 2], src[i + 1], src[i], src[i + 3]),
                RgbPacked::Rgb24 => (src[i], src[i + 1], src[i + 2], 255),
            };
            let j = py * w + px;
            y[j] = rgb_to_y_bt709(r, g, b);
            a[j] = alpha;
            let (uu, vv) = rgb_to_uv_bt709(r, g, b);
            let bj = (py / 2) * cw + px / 2;
            u_acc[bj] += u32::from(uu);
            v_acc[bj] += u32::from(vv);
            cnt[bj] += 1;
        }
    }
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for i in 0..cw * ch {
        // Среднее с округлением к ближайшему (cnt ≥ 1 всегда: каждый блок
        // 2×2 задет хотя бы одним пикселем кадра).
        let c = u32::from(cnt[i]);
        u[i] = ((u_acc[i] + c / 2) / c) as u8;
        v[i] = ((v_acc[i] + c / 2) / c) as u8;
    }
    (y, u, v, a)
}

/// Яркость BT.709 limited range (Y 16..235): зеркало формулы `mainVideoPS`
/// (знаменатель 219). Прямая формула `rgb_to_yuv_bt709_limited` из
/// rst-render/video.rs.
fn rgb_to_y_bt709(r: u8, g: u8, b: u8) -> u8 {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    (16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b)).round() as u8
}

/// Цветоразности BT.709 limited range (Cb/Cr 16..240): зеркало `mainVideoPS`
/// (знаменатель 224, коэффициенты 1.8556/0.4681/0.1873/1.5748).
fn rgb_to_uv_bt709(r: u8, g: u8, b: u8) -> (u8, u8) {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let u = 128.0 + 224.0 * (-0.1146 * r - 0.3854 * g + 0.5 * b);
    let v = 128.0 + 224.0 * (0.5 * r - 0.4542 * g - 0.0458 * b);
    (u.round() as u8, v.round() as u8)
}

/// Число выходных сэмплов ресемплера для `in_samples` входных: отношение
/// частот с округлением вверх плюс запас на передискретизацию (конвенция
/// `swr_get_out_samples`).
pub(crate) fn swr_out_count(in_samples: usize, in_rate: u32, out_rate: u32) -> usize {
    if in_rate == 0 {
        return 0;
    }
    (in_samples as u64 * out_rate as u64).div_ceil(in_rate as u64) as usize + 16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pts_positive_converts_by_timebase() {
        // MPEG TS: 90 000 тиков в секунду.
        assert_eq!(pts_to_duration(90_000, 1, 90_000), Duration::from_secs(1));
        assert_eq!(
            pts_to_duration(45_000, 1, 90_000),
            Duration::from_millis(500)
        );
        // MKV: миллисекунды.
        assert_eq!(pts_to_duration(1500, 1, 1000), Duration::from_millis(1500));
    }

    #[test]
    fn pts_negative_or_nopts_clamps_to_zero() {
        assert_eq!(pts_to_duration(-1, 1, 90_000), Duration::ZERO);
        assert_eq!(pts_to_duration(i64::MIN, 1, 90_000), Duration::ZERO);
    }

    #[test]
    fn pts_bad_timebase_clamps_to_zero() {
        assert_eq!(pts_to_duration(100, 1, 0), Duration::ZERO);
        assert_eq!(pts_to_duration(100, 1, -5), Duration::ZERO);
    }

    #[test]
    fn pts_fractional_micros_round_down() {
        // 1 тик из 3 по 1/3 c — 333 333 мкс.
        assert_eq!(pts_to_duration(1, 1, 3), Duration::from_micros(333_333));
    }

    #[test]
    fn plane_sizes_even_dimensions() {
        assert_eq!(
            yuv420p_plane_sizes(1920, 1080),
            (2_073_600, 518_400, 518_400)
        );
        assert_eq!(yuv420p_plane_sizes(2, 2), (4, 1, 1));
    }

    #[test]
    fn plane_sizes_odd_dimensions_round_up() {
        // 4:2:0 для нечётных размеров округляет вверх: 3×3 → хрома 2×2.
        assert_eq!(yuv420p_plane_sizes(3, 3), (9, 4, 4));
        assert_eq!(yuv420p_plane_sizes(1, 1), (1, 1, 1));
    }

    #[test]
    fn swr_count_scales_by_rate_ratio() {
        // 1024 сэмпла 44.1k → 48k.
        assert_eq!(swr_out_count(1024, 44_100, 48_000), 1115 + 16);
        assert_eq!(swr_out_count(0, 44_100, 48_000), 16);
    }

    #[test]
    fn swr_count_zero_rate_is_zero() {
        assert_eq!(swr_out_count(100, 0, 48_000), 0);
    }

    // --- M5e: форматы с альфой ---

    #[test]
    fn yuv420p_has_no_alpha_plane() {
        assert!(!VideoPixelFormat::Yuv420p.has_alpha());
        assert!(VideoPixelFormat::Yuva420p.has_alpha());
        assert!(VideoPixelFormat::Yuva444p10le.has_alpha());
        assert!(VideoPixelFormat::Rgb32.has_alpha());
    }

    #[test]
    fn only_yuv420p_is_hwaccel_compatible() {
        // Аппаратные декодеры альфа-канал не отдают (ARCHITECTURE.md §4.4):
        // все форматы с альфой обязаны идти программным декодом.
        assert!(VideoPixelFormat::Yuv420p.hwaccel_compatible());
        assert!(!VideoPixelFormat::Yuva420p.hwaccel_compatible());
        assert!(!VideoPixelFormat::Yuva444p10le.hwaccel_compatible());
        assert!(!VideoPixelFormat::Rgb32.hwaccel_compatible());
    }

    #[test]
    fn yuva420p_planes_are_420_with_full_alpha() {
        // YUVA420P: Y/A — полное разрешение, U/V — половина (как YUV420P).
        let s = VideoPixelFormat::Yuva420p.plane_sizes(1920, 1080);
        assert_eq!((s.y, s.alpha), (2_073_600, Some(2_073_600)));
        assert_eq!((s.u, s.v), (518_400, 518_400));
        // Нечётные размеры: хрома округляется вверх, альфа — точно w*h.
        let odd = VideoPixelFormat::Yuva420p.plane_sizes(3, 3);
        assert_eq!((odd.y, odd.u, odd.v, odd.alpha), (9, 4, 4, Some(9)));
    }

    #[test]
    fn yuva444p10le_planes_are_all_full_resolution() {
        // 4:4:4: все четыре плоскости полного разрешения (выход 8-битный,
        // понижение происходит при распаковке).
        let s = VideoPixelFormat::Yuva444p10le.plane_sizes(64, 48);
        assert_eq!((s.y, s.u, s.v, s.alpha), (3072, 3072, 3072, Some(3072)));
        let odd = VideoPixelFormat::Yuva444p10le.plane_sizes(3, 3);
        assert_eq!((odd.y, odd.u, odd.v), (9, 9, 9));
        // Хрома 4:4:4 — полное разрешение (в отличие от 4:2:0).
        assert_eq!(VideoPixelFormat::Yuva444p10le.chroma_dims(64, 48), (64, 48));
        assert_eq!(VideoPixelFormat::Yuva420p.chroma_dims(64, 48), (32, 24));
        // 10-бит: исходные сэмплы в 16-бит контейнере.
        assert_eq!(VideoPixelFormat::Yuva444p10le.source_bytes_per_sample(), 2);
        assert_eq!(VideoPixelFormat::Yuva420p.source_bytes_per_sample(), 1);
    }

    #[test]
    fn rgb32_planes_are_yuva420p() {
        // qtrle конвертируется в YUVA420P: те же геометрии, что у Yuva420p.
        let s = VideoPixelFormat::Rgb32.plane_sizes(4, 4);
        assert_eq!((s.y, s.u, s.v, s.alpha), (16, 4, 4, Some(16)));
        assert_eq!(VideoPixelFormat::Rgb32.chroma_dims(4, 4), (2, 2));
        assert_eq!(RgbPacked::Argb.bytes_per_pixel(), 4);
        assert_eq!(RgbPacked::Bgra.bytes_per_pixel(), 4);
        assert_eq!(RgbPacked::Rgb24.bytes_per_pixel(), 3);
    }

    #[test]
    fn classify_maps_supported_formats() {
        use ffmpeg_sys_next::AVPixelFormat as P;
        let cases = [
            (P::AV_PIX_FMT_YUV420P, VideoPixelFormat::Yuv420p, None),
            (P::AV_PIX_FMT_YUVA420P, VideoPixelFormat::Yuva420p, None),
            (
                P::AV_PIX_FMT_YUVA444P10LE,
                VideoPixelFormat::Yuva444p10le,
                None,
            ),
            (
                P::AV_PIX_FMT_ARGB,
                VideoPixelFormat::Rgb32,
                Some(RgbPacked::Argb),
            ),
            (
                P::AV_PIX_FMT_BGRA,
                VideoPixelFormat::Rgb32,
                Some(RgbPacked::Bgra),
            ),
            (
                P::AV_PIX_FMT_RGB24,
                VideoPixelFormat::Rgb32,
                Some(RgbPacked::Rgb24),
            ),
            // AV_PIX_FMT_RGB32 — макрос-алиас BGRA на LE (Windows).
            (
                ffmpeg_sys_next::AV_PIX_FMT_RGB32,
                VideoPixelFormat::Rgb32,
                Some(RgbPacked::Bgra),
            ),
        ];
        for (pix, want_fmt, want_rgb) in cases {
            assert_eq!(
                classify_pixel_format(pix as i32),
                Some((want_fmt, want_rgb)),
                "{pix:?}"
            );
        }
    }

    #[test]
    fn classify_rejects_unsupported_formats() {
        use ffmpeg_sys_next::AVPixelFormat as P;
        for fmt in [
            P::AV_PIX_FMT_YUV444P,
            P::AV_PIX_FMT_NV12,
            P::AV_PIX_FMT_RGB555LE,
            P::AV_PIX_FMT_PAL8,
            P::AV_PIX_FMT_NONE,
        ] {
            assert_eq!(classify_pixel_format(fmt as i32), None, "{fmt:?}");
        }
    }

    #[test]
    fn downconvert_10bit_takes_high_bits() {
        // 10-бит в 16-бит LE: берутся старшие 8 бит (>> 2).
        let cases = [
            ([0x00u8, 0x00], 0u8), // 0x0000 = 0 → 0
            ([0x00, 0x02], 128),   // 0x0200 = 512 → 128
            ([0x00, 0x03], 192),   // 0x0300 = 768 → 192
            ([0x01, 0x03], 192),   // 0x0301 = 769 → 192 (старшие биты)
            ([0x03, 0x03], 192),   // 0x0303 = 771 → 192
            ([0xFF, 0x03], 255),   // 0x03FF = 1023 → 255 (максимум)
            ([0x01, 0x00], 0),     // LE: 0x0001 = 1 → 0
        ];
        for (input, want) in cases {
            let mut plane = input.to_vec();
            downconvert_10bit_le(&mut plane);
            // Сжатие in-place: значащие байты — первая половина буфера.
            assert_eq!(&plane[..plane.len() / 2], [want], "вход {input:?}");
        }
    }

    #[test]
    fn downconvert_10bit_odd_bytes_panics_in_debug() {
        // Контракт функции — чётное число байт (16-бит контейнер);
        // debug_assert срабатывает только в debug-сборке.
        if cfg!(debug_assertions) {
            let mut plane = vec![0u8; 3];
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                downconvert_10bit_le(&mut plane)
            }));
            assert!(result.is_err(), "нечётная длина — panic в debug");
        }
    }

    #[test]
    fn rgb_conversion_known_red_pair() {
        // Чистый красный в BT.709 limited: Y=63, Cb=102, Cr=240 — та же
        // пара значений, что в GPU-тесте rst-render (`known_yuv_pair_for_red`).
        let (y, u, v, a) = rgb_packed_to_yuva420p(&[255, 255, 0, 0], 1, 1, RgbPacked::Argb);
        assert_eq!(y, [63]);
        assert_eq!(u, [102]);
        assert_eq!(v, [240]);
        assert_eq!(a, [255]);
    }

    #[test]
    fn rgb_channels_order_matters() {
        // ARGB: A,R,G,B в памяти; BGRA: B,G,R,A. Оба — тот же красный пиксель.
        let argb = rgb_packed_to_yuva420p(&[255, 255, 0, 0], 1, 1, RgbPacked::Argb);
        let bgra = rgb_packed_to_yuva420p(&[0, 0, 255, 255], 1, 1, RgbPacked::Bgra);
        assert_eq!(argb, bgra);
        // RGB24: тот же красный, без альфа-канала — a = 255.
        let rgb24 = rgb_packed_to_yuva420p(&[255, 0, 0], 1, 1, RgbPacked::Rgb24);
        assert_eq!(rgb24, bgra);
    }

    #[test]
    fn rgb_transparent_pixel_keeps_alpha_zero() {
        // Прозрачный пиксель: Y/U/V как обычно, альфа = 0 — рендер умножит
        // цвет на альфу (premultiplied), пиксель исчезнет.
        let (y, u, v, a) = rgb_packed_to_yuva420p(&[0, 255, 0, 0], 1, 1, RgbPacked::Argb);
        assert_eq!(y, [63]);
        assert_eq!(u, [102]);
        assert_eq!(v, [240]);
        assert_eq!(a, [0]);
    }

    #[test]
    fn rgb_gray_is_neutral_chroma() {
        // Серый: U = V = 128, яркость по шкале 16..235.
        let (y, u, v, a) = rgb_packed_to_yuva420p(&[255, 128, 128, 128], 1, 1, RgbPacked::Argb);
        assert_eq!(u, [128]);
        assert_eq!(v, [128]);
        assert_eq!(a, [255]);
        // 128/255 ≈ 0.502 → Y = 16 + 219*0.502 = 125.9 → 126.
        assert_eq!(y, [126]);
    }

    #[test]
    fn rgb_uv_averages_over_2x2_blocks() {
        // 2×2 из красного, зелёного, синего и белого: U/V — среднее
        // покомпонентных цветоразностей блока (4:2:0, то же округление).
        let px = |rgb: [u8; 3]| {
            let (y, u, v, _) =
                rgb_packed_to_yuva420p(&[255, rgb[0], rgb[1], rgb[2]], 1, 1, RgbPacked::Argb);
            (y[0], u[0], v[0])
        };
        let mut u_sum = 0u32;
        let mut v_sum = 0u32;
        for rgb in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]] {
            let (_, u, v) = px(rgb);
            u_sum += u as u32;
            v_sum += v as u32;
        }
        let (y, u, v, _) = rgb_packed_to_yuva420p(
            &[
                255, 255, 0, 0, // R
                255, 0, 255, 0, // G
                255, 0, 0, 255, // B
                255, 255, 255, 255, // W
            ],
            2,
            2,
            RgbPacked::Argb,
        );
        assert_eq!((u[0] as u32, v[0] as u32), (u_sum / 4, v_sum / 4));
        // Y — покомпонентный, без усреднения.
        assert_eq!((y[0], y[1]), (px([255, 0, 0]).0, px([0, 255, 0]).0));
    }

    #[test]
    fn rgb_odd_dimensions_round_up_chroma() {
        // 3×3: хрома 2×2 (div_ceil), блоки по краям усредняют меньше пикселей.
        let src = vec![255u8; 3 * 3 * 4]; // непрозрачный белый
        let (y, u, v, a) = rgb_packed_to_yuva420p(&src, 3, 3, RgbPacked::Argb);
        assert_eq!(y.len(), 9);
        assert_eq!(u.len(), 4);
        assert_eq!(v.len(), 4);
        assert_eq!(a, vec![255; 9]);
        // Белый: Y=235, U=V=128 — все блоки.
        assert!(y.iter().all(|&b| b == 235));
        assert!(u.iter().all(|&b| b == 128) && v.iter().all(|&b| b == 128));
    }

    #[test]
    fn rgb_packed_roundtrip_back_to_rgb_matches_bt709() {
        // Проверка против полного цикла: YUV → RGB обратным зеркалом даёт
        // исходный цвет с точностью 8-битного квантования (±3 на канал).
        let rgb = [220u8, 180, 140];
        let (y, u, v, _) =
            rgb_packed_to_yuva420p(&[255, rgb[0], rgb[1], rgb[2]], 1, 1, RgbPacked::Argb);
        let yp = (f64::from(y[0]) - 16.0) / 219.0;
        let up = (f64::from(u[0]) - 128.0) / 224.0;
        let vp = (f64::from(v[0]) - 128.0) / 224.0;
        let r = yp + 1.5748 * vp;
        let g = yp - 0.1873 * up - 0.4681 * vp;
        let b = yp + 1.8556 * up;
        let out = [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        for (got, want) in out.iter().zip(rgb) {
            assert!(
                (*got as i16 - want as i16).abs() <= 3,
                "roundtrip {rgb:?} → {out:?}"
            );
        }
    }
}
