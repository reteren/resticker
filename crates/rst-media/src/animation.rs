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
use std::io::{BufReader, Read};
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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Файл не удалось декодировать выбранным кодеком.
    #[error("could not decode the animation: {0}")]
    Image(#[from] image::ImageError),
    /// Кадров больше лимита атласа (300). Вызывающий код обязан показать
    /// пользователю «анимация слишком большая», а не падать.
    #[error("too many frames: {count} > {limit}")]
    TooManyFrames { count: usize, limit: usize },
    /// Суммарный объём RGBA-кадров больше лимита атласа (256MB).
    #[error("total frame size {bytes} bytes exceeds the {limit} byte limit")]
    TooLargeForAtlas { bytes: usize, limit: usize },
    /// 0 или 1 кадр — вызывающий код обязан трактовать файл как
    /// `MediaType::Image`, а не как ошибку пользователю.
    #[error("the file is not animated (0 or 1 frame)")]
    NotAnimated,
    /// Магические байты не соответствуют ни одному из известных форматов.
    #[error("unknown file format (expected GIF, APNG or WebP)")]
    UnsupportedFormat,
}

/// Декодировать файл (GIF / APNG / animated WebP, по содержимому, не по
/// расширению) во все кадры. Пороги лимитов проверяются инкрементально по
/// мере итерации — при превышении возвращается `Err` сразу, без
/// материализации остальных кадров.
pub fn decode_animation(path: &Path) -> Result<DecodedAnimation, MediaError> {
    let format = detect_format(path)?;
    let frames = open_frames(path, format)?;

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
/// `decode_animation`; на `NotAnimated`/любую другую ошибку декода падает
/// обратно на `MediaType::Image` (координатор должен уметь показать статик,
/// даже если файл на самом деле битый gif). `Animation` — когда
/// `decode_animation` вернул >= 2 кадра, а также когда порог атласа превышен
/// (`TooManyFrames`/`TooLargeForAtlas`) — координатор в этом случае играет
/// файл в потоковом режиме ([`StreamingAnimation`]), а не отказывает
/// пользователю (ROADMAP.md M5a, «потоковый режим для очень длинных
/// анимаций»).
pub fn sniff_media_type(path: &Path) -> MediaType {
    match decode_animation(path) {
        Ok(animation) if animation.frames.len() >= 2 => MediaType::Animation,
        Err(MediaError::TooManyFrames { .. } | MediaError::TooLargeForAtlas { .. }) => {
            MediaType::Animation
        }
        Ok(_) | Err(_) => MediaType::Image,
    }
}

/// Потоковый декод анимации (ROADMAP.md M5a, «потоковый режим для очень
/// длинных анимаций»): вместо материализации всех кадров разом (см.
/// `decode_animation`/`MAX_FRAMES`/`MAX_ATLAS_BYTES`) кадры декодируются по
/// одному, на каждый вызов [`Self::next_frame`] — подходит для анимаций,
/// превышающих лимиты атласа. Луп — не забота вызывающего кода:
/// `next_frame` сама перезапускает декодер с начала файла, когда кадры
/// закончились, и всегда возвращает следующий кадр (`Ok`, а не `None`).
///
/// В отличие от атласного пути (`decode_animation` + `TextureAtlas`),
/// потоковый декодер не может дёшево «досчитать» пропущенные кадры после
/// долгой паузы процесса (сон системы, блокировка) — цена одного кадра
/// здесь реальное декодирование, а не смена индекса в уже готовом атласе.
/// Координатор (`rst-resticker::overlay_manager`) поэтому не пытается
/// воспроизвести точную кадровую позицию по времени: он просто продолжает
/// с текущего кадра, как только процесс снова начинает тикать.
pub struct StreamingAnimation {
    path: std::path::PathBuf,
    format: Format,
    frames: Frames<'static>,
    width: u32,
    height: u32,
    /// Кадры открытия/рестарта, уже декодированные, но ещё не отданные
    /// вызывающему коду через `next_frame`: ровно 2 (не 1) — тот же
    /// контракт «минимум 2 кадра», что у `decode_animation::NotAnimated`,
    /// проверяется здесь декодированием второго кадра сразу при открытии,
    /// а не откладывается до первого вызова `next_frame`.
    pending: std::collections::VecDeque<DecodedFrame>,
}

impl StreamingAnimation {
    /// Открыть файл для потокового декода: определяет формат по магическим
    /// байтам (как `decode_animation`), декодирует только первый кадр —
    /// остальные читаются по требованию через `next_frame`.
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let format = detect_format(path)?;
        let frames = open_frames(path, format)?;
        let mut this = Self {
            path: path.to_path_buf(),
            format,
            frames,
            width: 0,
            height: 0,
            pending: std::collections::VecDeque::new(),
        };
        this.prime()?;
        Ok(this)
    }

    /// Ширина кадра в пикселях (все кадры анимации одного размера).
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Высота кадра в пикселях.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Декодировать следующий кадр. По исчерпании кадров файла перезапускает
    /// декодер с начала (луп) и возвращает первый кадр нового прохода —
    /// вызывающему коду не нужно самому отслеживать конец анимации.
    pub fn next_frame(&mut self) -> Result<DecodedFrame, MediaError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(frame);
        }
        match self.frames.next() {
            Some(Ok(frame)) => Ok(DecodedFrame {
                rgba: frame.buffer().as_raw().clone(),
                delay: clamp_delay(frame.delay()),
            }),
            Some(Err(e)) => Err(e.into()),
            None => {
                self.restart()?;
                self.next_frame()
            }
        }
    }

    /// Перезапустить декодер с начала файла (луп) — заново открывает файл и
    /// декодирует первые 2 кадра, тем же путём, что `open`.
    fn restart(&mut self) -> Result<(), MediaError> {
        self.frames = open_frames(&self.path, self.format)?;
        self.prime()
    }

    /// Декодирует и буферизует первые 2 кадра нового прохода: тот же
    /// контракт «минимум 2 кадра — иначе `NotAnimated`», что у
    /// `decode_animation`, но без материализации всей анимации — только 2
    /// кадра вместо всех.
    fn prime(&mut self) -> Result<(), MediaError> {
        let raw0 = self
            .frames
            .next()
            .transpose()?
            .ok_or(MediaError::NotAnimated)?;
        let (width, height) = raw0.buffer().dimensions();
        self.width = width;
        self.height = height;
        let frame0 = DecodedFrame {
            rgba: raw0.buffer().as_raw().clone(),
            delay: clamp_delay(raw0.delay()),
        };
        let raw1 = self
            .frames
            .next()
            .transpose()?
            .ok_or(MediaError::NotAnimated)?;
        let frame1 = DecodedFrame {
            rgba: raw1.buffer().as_raw().clone(),
            delay: clamp_delay(raw1.delay()),
        };
        self.pending = std::collections::VecDeque::from([frame0, frame1]);
        Ok(())
    }
}

/// Определить формат файла по магическим байтам (без чтения кадров) —
/// общий первый шаг `decode_animation` и `StreamingAnimation::open`.
fn detect_format(path: &Path) -> Result<Format, MediaError> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 12];
    file.read_exact(&mut magic)
        .map_err(|_| MediaError::UnsupportedFormat)?;
    sniff_format(&magic).ok_or(MediaError::UnsupportedFormat)
}

/// Открыть декодер нужного формата на свежем файловом хендле и вернуть его
/// как единый `Frames`-итератор — общий путь `decode_animation` (полная
/// материализация) и `StreamingAnimation` (по кадру). Отдельный `File` на
/// каждый вызов, а не переиспользование ридера через `seek`: `restart()`
/// проще и надёжнее с чистого хендла, чем с перемоткой декодера, который
/// сам мог продвинуть внутренний буфер непредсказуемо.
fn open_frames(path: &Path, format: Format) -> Result<Frames<'static>, MediaError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    Ok(match format {
        Format::Gif => GifDecoder::new(reader)?.into_frames(),
        Format::Apng => PngDecoder::new(reader)?.apng()?.into_frames(),
        Format::Webp => WebPDecoder::new(reader)?.into_frames(),
    })
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

#[derive(Clone, Copy)]
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

    #[test]
    fn sniff_media_type_treats_oversized_animation_as_animation() {
        // decode_animation честно отказывает файлу с > MAX_FRAMES кадрами
        // (TooManyFrames) — но для sniff_media_type это всё ещё Animation:
        // координатор играет такой файл в потоковом режиме, а не показывает
        // статику (ROADMAP.md M5a).
        let colors: Vec<[u8; 4]> = (0..301).map(|_| [255, 0, 0, 255]).collect();
        let delays = vec![100u32; 301];
        let gif = gif_fixture(&colors, &delays);
        let (_dir, path) = write_fixture(&gif, "huge.gif");

        assert!(matches!(
            decode_animation(&path),
            Err(MediaError::TooManyFrames {
                count: 301,
                limit: 300
            })
        ));
        assert_eq!(sniff_media_type(&path), MediaType::Animation);
    }

    #[test]
    fn streaming_animation_reports_dimensions_and_frame_count_via_loop() {
        let gif = gif_fixture(
            &[[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]],
            &[100, 100, 100],
        );
        let (_dir, path) = write_fixture(&gif, "stream.gif");

        let mut stream = StreamingAnimation::open(&path).expect("открытие потоковой анимации");
        assert_eq!(stream.width(), 2);
        assert_eq!(stream.height(), 2);

        let f0 = stream.next_frame().expect("кадр 0");
        let f1 = stream.next_frame().expect("кадр 1");
        let f2 = stream.next_frame().expect("кадр 2");
        assert_eq!(f0.rgba[0..4], [255, 0, 0, 255], "кадр 0 — красный");
        assert_eq!(f1.rgba[0..4], [0, 255, 0, 255], "кадр 1 — зелёный");
        assert_eq!(f2.rgba[0..4], [0, 0, 255, 255], "кадр 2 — синий");

        // Кадры закончились — next_frame зацикливает сама, без Option/Err.
        let looped = stream.next_frame().expect("рестарт лупа на кадр 0");
        assert_eq!(
            looped.rgba[0..4],
            [255, 0, 0, 255],
            "после конца анимации next_frame возвращает кадр 0 заново"
        );
    }

    #[test]
    fn streaming_animation_single_frame_gif_is_not_animated() {
        let gif = gif_fixture(&[[255, 0, 0, 255]], &[100]);
        let (_dir, path) = write_fixture(&gif, "static.gif");

        let err = StreamingAnimation::open(&path).err();
        assert!(matches!(err, Some(MediaError::NotAnimated)), "{err:?}");
    }

    #[test]
    fn streaming_animation_unknown_format_is_rejected() {
        let (_dir, path) = write_fixture(b"not a media file at all", "junk.bin");
        let err = StreamingAnimation::open(&path).err();
        assert!(
            matches!(err, Some(MediaError::UnsupportedFormat)),
            "{err:?}"
        );
    }
}
