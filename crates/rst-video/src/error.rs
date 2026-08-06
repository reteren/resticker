//! Ошибки слоя `rst-video` (docs/M5B_VIDEO_DESIGN.md §2): все ошибки декода
//! приходят как значения, а не паники; `Open`/`Seek` возвращаются синхронно
//! вызывающему коду, ошибки среднего потока (битый файл в середине) лечатся
//! внутри декодер-потока перезапуском цикла и наружу не всплывают.

use std::path::PathBuf;

/// Ошибка декодирования видео.
#[derive(Debug, Clone, thiserror::Error)]
pub enum VideoError {
    /// `avformat_open_input`/`avformat_find_stream_info` не смогли открыть
    /// файл как мультимедийный контейнер (файл отсутствует, битый, не видео).
    #[error("не удалось открыть видео {path}: {message}")]
    Open { path: PathBuf, message: String },

    /// В контейнере нет видеопотока (например, аудиофайл).
    #[error("в файле {path} нет видеопотока")]
    NoVideoStream { path: PathBuf },

    /// Кодек видео не собран в этой сборке FFmpeg (LGPL-only сборка без
    /// проприетарных кодеков).
    #[error("видеокодек {name} не поддерживается сборкой FFmpeg")]
    UnsupportedVideoCodec { name: String },

    /// Декодер дал кадр в неподдерживаемом формате пикселя. Пайплайн
    /// принимает YUV420P (обычное видео), YUVA420P (WebM/VP9 с альфой),
    /// YUVA444P10LE (ProRes 4444 — понижается до 8 бит) и packed RGB
    /// qtrle (QuickTime Animation — конвертируется в YUVA420P на CPU).
    #[error(
        "формат пикселя {name} не поддерживается (поддерживаются: YUV420P, YUVA420P, YUVA444P10LE, qtrle)"
    )]
    UnsupportedPixelFormat { name: String },

    /// Файл открылся, но ни один видеокадр так и не декодировался.
    #[error("в файле {path} не декодировался ни один видеокадр")]
    NoFrames { path: PathBuf },

    /// Ошибка декодирования в середине потока (лечится перезапуском цикла).
    #[error("ошибка декодирования: {0}")]
    Decode(String),

    /// `av_seek_frame` не смог перемотать поток.
    #[error("не удалось перемотать: {0}")]
    Seek(String),

    /// Путь не представим как UTF-8 (FFmpeg принимает пути в UTF-8).
    #[error("путь не в UTF-8: {path:?}")]
    NonUtf8Path { path: PathBuf },

    /// Поток-декодер завершился раньше времени (например, при закрытии).
    #[error("поток-декодер завершился неожиданно")]
    DecoderThreadGone,
}
