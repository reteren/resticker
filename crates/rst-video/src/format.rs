//! Чистые хелперы форматов: PTS → `Duration`, размеры плоскостей YUV420P,
//! оценка числа выходных сэмплов ресемплера. Без FFmpeg FFI — юнит-тесты не
//! требуют ни библиотек, ни видеофайлов.

use std::time::Duration;

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
}
