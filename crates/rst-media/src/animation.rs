//! Слой декодирования анимаций (M5A_ANIMATION_DESIGN.md, раздел 2):
//! GIF / animated WebP / APNG в straight-alpha RGBA8 кадры для текстурного
//! атласа. Размещение — `rst-media`, а не `rst-render`: декодирование
//! изображений — зона `rst-media` (CONTRIBUTING.md, таблица ответственности
//! крейтов), атлас и UV-семплирование — `rst-render` (Задача B).
//!
//! Формат определяется по магическим байтам файла, не по расширению.
//! Пороги остановки (лимит кадров атласа и суммарный объём RGBA)
//! проверяются инкрементально по мере итерации `Frames`-итератора, чтобы
//! патологический файл не материализовывался в памяти целиком
//! (ARCHITECTURE.md, §4.4).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frames};
use rst_core::model::MediaType;

/// Максимум кадров в атласе (ARCHITECTURE.md, §4.4: порог стриминга).
const MAX_FRAMES: usize = 300;
/// Максимум суммарного объёма RGBA-кадров в байтах (256MB, ARCHITECTURE.md, §4.4).
const MAX_ATLAS_BYTES: usize = 256 * 1024 * 1024;
/// Минимальная задержка кадра (браузерная конвенция для «нулевой» задержки
/// в GIF): 0 и подкадровые значения клэмпятся к 20ms.
const MIN_FRAME_DELAY: Duration = Duration::from_millis(20);

/// Один кадр анимации: straight-alpha RGBA8, `rgba.len() == width*height*4`.
#[derive(Debug)]
pub struct DecodedFrame {
    pub rgba: Vec<u8>,
    /// Задержка кадра; 0 или подкадровые значения (GIF centisecond rounding)
    /// клэмпятся к минимуму 20ms.
    pub delay: Duration,
}

/// Полностью декодированная анимация, готовая к раскладке в атлас.
#[derive(Debug)]
pub struct DecodedAnimation {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<DecodedFrame>,
}

/// Ошибка декодирования анимации (слой `rst-media::animation`).
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// Ошибка ввода-вывода при открытии/чтении файла.
    #[error("ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),
    /// Файл не удалось декодировать выбранным кодеком.
    #[error("не удалось декодировать анимацию: {0}")]
    Image(#[from] image::ImageError),
    /// Кадров больше лимита атласа (300). Вызывающий код обязан показать
    /// пользователю «анимация слишком большая», а не падать.
    #[error("слишком много кадров: {count} > {limit}")]
    TooManyFrames { count: usize, limit: usize },
    /// Суммарный объём RGBA-кадров больше лимита атласа (256MB).
    #[error("суммарный размер кадров {bytes} байт > лимит {limit} байт")]
    TooLargeForAtlas { bytes: usize, limit: usize },
    /// 0 или 1 кадр — вызывающий код обязан трактовать файл как
    /// `MediaType::Image`, а не как ошибку пользователю.
    #[error("файл не анимированный (0 или 1 кадр)")]
    NotAnimated,
    /// Магические байты не соответствуют ни одному из известных форматов.
    #[error("неизвестный формат файла (ожидались GIF, APNG или WebP)")]
    UnsupportedFormat,
}

/// Декодировать файл (GIF / APNG / animated WebP, по содержимому, не по
/// расширению) во все кадры. Пороги лимитов проверяются инкрементально по
/// мере итерации — при превышении возвращается `Err` сразу, без
/// материализации остальных кадров.
pub fn decode_animation(path: &Path) -> Result<DecodedAnimation, MediaError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 12];
    reader
        .read_exact(&mut magic)
        .map_err(|_| MediaError::UnsupportedFormat)?;
    reader.seek(SeekFrom::Start(0))?;

    let frames: Frames<'_> = match sniff_format(&magic) {
        Some(Format::Gif) => GifDecoder::new(reader)?.into_frames(),
        Some(Format::Apng) => PngDecoder::new(reader)?.apng()?.into_frames(),
        Some(Format::Webp) => WebPDecoder::new(reader)?.into_frames(),
        None => return Err(MediaError::UnsupportedFormat),
    };

    let mut decoded = Vec::new();
    let mut total_bytes = 0usize;
    let mut width = 0u32;
    let mut height = 0u32;

    for frame in frames {
        let frame = frame?;
        if decoded.is_empty() {
            (width, height) = frame.buffer().dimensions();
        }
        let raw = frame.buffer().as_raw();
        total_bytes += raw.len();
        check_thresholds(decoded.len() + 1, total_bytes)?;
        decoded.push(DecodedFrame {
            rgba: raw.clone(),
            delay: clamp_delay(frame.delay()),
        });
    }

    if decoded.len() < 2 {
        return Err(MediaError::NotAnimated);
    }
    Ok(DecodedAnimation {
        width,
        height,
        frames: decoded,
    })
}

/// Определяет MediaType по содержимому файла (не по расширению!): пробует
/// `decode_animation`; на `NotAnimated`/любую ошибку декода падает обратно
/// на `MediaType::Image` (координатор должен уметь показать статик, даже
/// если файл на самом деле битый gif). `Animation` — только когда
/// `decode_animation` вернул >= 2 кадра.
pub fn sniff_media_type(path: &Path) -> MediaType {
    match decode_animation(path) {
        Ok(animation) if animation.frames.len() >= 2 => MediaType::Animation,
        Ok(_) | Err(_) => MediaType::Image,
    }
}

/// Чистая проверка порогов атласа; вызывается на каждой итерации декода,
/// тестируется без реального огромного файла-фикстуры.
pub fn check_thresholds(frame_count: usize, total_bytes: usize) -> Result<(), MediaError> {
    if frame_count > MAX_FRAMES {
        return Err(MediaError::TooManyFrames {
            count: frame_count,
            limit: MAX_FRAMES,
        });
    }
    if total_bytes > MAX_ATLAS_BYTES {
        return Err(MediaError::TooLargeForAtlas {
            bytes: total_bytes,
            limit: MAX_ATLAS_BYTES,
        });
    }
    Ok(())
}

/// Задержка кадра в миллисекундах с округлением вверх, клэмпнутая к
/// минимуму 20ms.
fn clamp_delay(delay: image::Delay) -> Duration {
    let (numer, denom) = delay.numer_denom_ms();
    let ms = if denom == 0 {
        0
    } else {
        (numer as u64).div_ceil(denom as u64)
    };
    Duration::from_millis(ms).max(MIN_FRAME_DELAY)
}

enum Format {
    Gif,
    Apng,
    Webp,
}

const GIF_87A: &[u8] = b"GIF87a";
const GIF_89A: &[u8] = b"GIF89a";
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const WEBP_RIFF: &[u8] = b"RIFF";
const WEBP_MAGIC: &[u8] = b"WEBP";

/// Определить формат по магическим байтам (не по расширению файла).
fn sniff_format(magic: &[u8]) -> Option<Format> {
    if magic.starts_with(GIF_87A) || magic.starts_with(GIF_89A) {
        Some(Format::Gif)
    } else if magic.starts_with(PNG_MAGIC) {
        Some(Format::Apng)
    } else if magic.starts_with(WEBP_RIFF) && magic.get(8..12) == Some(WEBP_MAGIC) {
        Some(Format::Webp)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, Rgba, RgbaImage};

    /// 2×2 GIF-фикстура в памяти: каждый кадр — сплошной цвет, задержка —
    /// centiseconds в units кодируется через `Delay::from_numer_denom_ms`.
    fn gif_fixture(frame_colors: &[[u8; 4]], delays_ms: &[u32]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut buf);
        for (color, delay_ms) in frame_colors.iter().zip(delays_ms) {
            let img = RgbaImage::from_pixel(2, 2, Rgba(*color));
            let frame = Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(*delay_ms, 1));
            encoder
                .encode_frame(frame)
                .expect("GIF-кодирование фикстуры");
        }
        drop(encoder);
        buf
    }

    fn write_fixture(bytes: &[u8], name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("временный каталог");
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("запись фикстуры");
        (dir, path)
    }

    #[test]
    fn two_frame_gif_decodes_with_delays_and_colors() {
        let gif = gif_fixture(&[[255, 0, 0, 255], [0, 0, 255, 255]], &[100, 200]);
        let (_dir, path) = write_fixture(&gif, "anim.gif");

        let anim = decode_animation(&path).expect("декодирование 2-кадрового gif");
        assert_eq!(anim.width, 2);
        assert_eq!(anim.height, 2);
        assert_eq!(anim.frames.len(), 2);
        for frame in &anim.frames {
            assert_eq!(frame.rgba.len(), 2 * 2 * 4, "len == width*height*4");
        }
        assert_eq!(anim.frames[0].delay, Duration::from_millis(100));
        assert_eq!(anim.frames[1].delay, Duration::from_millis(200));
        assert_eq!(
            anim.frames[0].rgba,
            vec![
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255
            ],
            "кадр 0 — сплошной красный"
        );
        assert_eq!(
            anim.frames[1].rgba,
            vec![
                0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255
            ],
            "кадр 1 — сплошной синий"
        );
    }

    #[test]
    fn single_frame_gif_is_not_animated() {
        let gif = gif_fixture(&[[255, 0, 0, 255]], &[100]);
        let (_dir, path) = write_fixture(&gif, "static.gif");

        let result = decode_animation(&path);
        assert!(matches!(result, Err(MediaError::NotAnimated)), "{result:?}");
    }

    #[test]
    fn static_png_is_not_animated() {
        let img = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("PNG-кодирование фикстуры");
        let (_dir, path) = write_fixture(&png, "static.png");

        let result = decode_animation(&path);
        assert!(matches!(result, Err(MediaError::NotAnimated)), "{result:?}");
    }

    #[test]
    fn zero_delay_clamps_to_20ms() {
        let gif = gif_fixture(&[[255, 0, 0, 255], [0, 0, 255, 255]], &[0, 100]);
        let (_dir, path) = write_fixture(&gif, "zero_delay.gif");

        let anim = decode_animation(&path).expect("декодирование gif");
        assert_eq!(
            anim.frames[0].delay,
            Duration::from_millis(20),
            "0ms → 20ms"
        );
        assert_eq!(anim.frames[1].delay, Duration::from_millis(100));
    }

    #[test]
    fn unknown_format_is_rejected() {
        let (_dir, path) = write_fixture(b"not a media file at all", "junk.bin");
        let result = decode_animation(&path);
        assert!(
            matches!(result, Err(MediaError::UnsupportedFormat)),
            "{result:?}"
        );
    }

    #[test]
    fn check_thresholds_rejects_too_many_frames() {
        assert!(check_thresholds(300, 0).is_ok(), "ровно 300 кадров — ок");
        let result = check_thresholds(301, 0);
        assert!(
            matches!(
                result,
                Err(MediaError::TooManyFrames {
                    count: 301,
                    limit: 300
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn check_thresholds_rejects_too_many_bytes() {
        let limit = 256 * 1024 * 1024;
        assert!(check_thresholds(1, limit).is_ok(), "ровно 256MB — ок");
        let result = check_thresholds(1, limit + 1);
        assert!(
            matches!(
                result,
                Err(MediaError::TooLargeForAtlas {
                    bytes,
                    limit
                }) if bytes == limit + 1 && limit == 256 * 1024 * 1024
            ),
            "{result:?}"
        );
    }

    #[test]
    fn sniff_media_type_detects_animation() {
        let (_dir, animated) = write_fixture(
            &gif_fixture(&[[255, 0, 0, 255], [0, 0, 255, 255]], &[100, 100]),
            "animated.gif",
        );
        assert_eq!(sniff_media_type(&animated), MediaType::Animation);

        let (_dir, static_gif) =
            write_fixture(&gif_fixture(&[[255, 0, 0, 255]], &[100]), "static.gif");
        assert_eq!(sniff_media_type(&static_gif), MediaType::Image);

        let (_dir, garbage) = write_fixture(b"garbage", "junk.bin");
        assert_eq!(sniff_media_type(&garbage), MediaType::Image);
    }
}
