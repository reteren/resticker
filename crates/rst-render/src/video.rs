//! Видео-текстуры и конвертация YUV→RGB (M5b, docs/M5B_VIDEO_DESIGN.md §3).
//!
//! Видеокадр — три отдельные R8-плоскости: Y (полное разрешение) и U/V
//! (половина по каждой оси, 4:2:0 — самый частый вывод декодеров
//! H.264/VP9). Плоскости переиспользуются и обновляются через
//! `Device::update_video_textures` (UpdateSubresource) на каждый показанный
//! кадр — пересоздание текстур на каждый кадр дорого, в отличие от
//! атласа анимации (M5a), который живёт на фиксированном наборе кадров.
//!
//! Рисуется тем же `Sprite`-путём: координатор строит `Sprite` с
//! `with_video` поверх `VideoTextures`, `draw`/`draw_masked` семплируют
//! три плоскости и конвертируют в RGB в пиксельном шейдере
//! (`mainVideoPS`), а не на CPU (`sws_scale` из M5b-скоупа исключён —
//! ARCHITECTURE.md, «Как не убить процессор»).

use crate::texture::Texture;

/// Три R8-плоскости одного видеокадра: Y (width×height), U и V
/// (ceil(width/2)×ceil(height/2) — 4:2:0).
///
/// Владение: COM-указатели текстур — умные указатели windows-rs, `Release`
/// автоматически в `Drop`; `Clone` — COM `AddRef`, дешёвый (спрайт держит
/// клон плоскостей, а координатор — исходник для обновления кадром).
#[derive(Debug, Clone)]
pub struct VideoTextures {
    /// Плоскость яркости, полное разрешение.
    pub y: Texture,
    /// Плоскость синей цветоразности, половина разрешения по каждой оси.
    pub u: Texture,
    /// Плоскость красной цветоразности, половина разрешения по каждой оси.
    pub v: Texture,
}

/// Коэффициенты матрицы BT.709 limited range (Rec. ITU-R BT.709-6,
/// диапазон Y 16..235, Cb/Cr 16..240) — стандарт для подавляющего
/// большинства современных H.264/VP9-исходников.
///
/// Нормализация отличается от наивной `/255`: яркость делится на 219,
/// цветоразности — на 224, именно на этих знаменателях константы
/// 1.5748/0.1873/0.4681/1.8556 дают точный roundtrip (RGB 255 → YUV →
/// RGB 255, проверено юнит-тестами ниже). В шейдере (`mainVideoPS`) —
/// та же формула в f32 с `saturate`; здесь — f64 с округлением к
/// ближайшему, CPU-зеркало для юнит-тестов и GPU-теста readback.
#[cfg(test)]
pub(crate) fn yuv_to_rgb_bt709_limited(y: u8, u: u8, v: u8) -> [u8; 3] {
    let yp = (f64::from(y) - 16.0) / 219.0;
    let up = (f64::from(u) - 128.0) / 224.0;
    let vp = (f64::from(v) - 128.0) / 224.0;
    let r = yp + 1.5748 * vp;
    let g = yp - 0.1873 * up - 0.4681 * vp;
    let b = yp + 1.8556 * up;
    [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// RGB (0..=255) → BT.709 limited-range Y/U/V (8 бит) — прямая формула для
/// построения синтетических кадров в юнит- и GPU-тестах.
#[cfg(test)]
pub(crate) fn rgb_to_yuv_bt709_limited(rgb: [u8; 3]) -> (u8, u8, u8) {
    let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
    let y = 16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b);
    let u = 128.0 + 224.0 * (-0.1146 * r - 0.3854 * g + 0.5 * b);
    let v = 128.0 + 224.0 * (0.5 * r - 0.4542 * g - 0.0458 * b);
    (y.round() as u8, u.round() as u8, v.round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roundtrip RGB → YUV → RGB держит ±3 на канал (потери 8-битного
    /// квантования YUV-плоскостей, не ошибка конверсии).
    fn assert_roundtrip(rgb: [u8; 3], tolerance: u8) {
        let (y, u, v) = rgb_to_yuv_bt709_limited(rgb);
        let out = yuv_to_rgb_bt709_limited(y, u, v);
        for (got, want) in out.iter().zip(rgb) {
            assert!(
                (*got as i16 - want as i16).abs() <= i16::from(tolerance),
                "roundtrip {rgb:?} → YUV ({y},{u},{v}) → {out:?}"
            );
        }
    }

    #[test]
    fn roundtrip_pure_red() {
        assert_roundtrip([255, 0, 0], 3);
    }

    #[test]
    fn roundtrip_pure_green() {
        assert_roundtrip([0, 255, 0], 3);
    }

    #[test]
    fn roundtrip_pure_blue() {
        assert_roundtrip([0, 0, 255], 3);
    }

    #[test]
    fn roundtrip_mid_gray() {
        // Серый — U=V=128, все три канала равны, roundtrip без потерь.
        let (y, u, v) = rgb_to_yuv_bt709_limited([128, 128, 128]);
        assert_eq!((u, v), (128, 128));
        assert_eq!(yuv_to_rgb_bt709_limited(y, u, v), [128, 128, 128]);
    }

    #[test]
    fn roundtrip_skin_tone() {
        assert_roundtrip([220, 180, 140], 3);
    }

    #[test]
    fn known_yuv_pair_for_red() {
        // Чистый красный в BT.709 limited: Y=62.6, Cb=102.3, Cr=240 —
        // проверяем сами плоскости, чтобы GPU-тест не опирался на
        // зеркальную функцию конверсии.
        let (y, u, v) = rgb_to_yuv_bt709_limited([255, 0, 0]);
        assert_eq!((y, u, v), (63, 102, 240));
        // Обратный проход ровно в диапазон полного красного: G и B — не
        // больше 1 (остаток квантования плоскости Y 63 вместо 62.6).
        let out = yuv_to_rgb_bt709_limited(63, 102, 240);
        assert_eq!(out[0], 255, "R — полный");
        assert!(out[1] <= 1 && out[2] <= 1, "G/B ≈ 0: {out:?}");
    }

    #[test]
    fn out_of_range_values_saturate_per_channel() {
        // Патологические значения (яркость выше 235 и ниже 16, цветоразности
        // вне 16..240) обрабатываются покомпонентным `saturate`: каналы,
        // ушедшие выше 1, режутся к 255, ниже 0 — к 0 (матрица смешивает
        // каналы, поэтому все 255 получаются только у чистых R/B-путей,
        // не у (255,255,255) — G у него остаётся серым).
        assert_eq!(yuv_to_rgb_bt709_limited(255, 255, 255)[0], 255);
        assert_eq!(yuv_to_rgb_bt709_limited(0, 0, 0)[0], 0);
    }

    #[test]
    fn gray_extremes_are_exact() {
        // U=V=128 (нейтральные цветоразности): яркость по шкале 16..235
        // даёт ровный серый, без цветового сдвига на краях диапазона.
        assert_eq!(yuv_to_rgb_bt709_limited(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb_bt709_limited(235, 128, 128), [255, 255, 255]);
    }
}
