//! Крейт `rst-video`: программный декод видео через FFmpeg (M5b,
//! docs/M5B_VIDEO_DESIGN.md §2) — демукс + декод + ресемплинг звука, без GPU
//! и окон.
//!
//! Разделение ответственности, как в M5a: декодирование — здесь, в изоляции
//! от GPU; `rst-render` (задача B этого среза) получает плоскости Y/U/V и
//! конвертирует в шейдере; микшер звука (задача C, `rst-audio`/cpal) берёт
//! готовые f32-сэмплы.
//!
//! # Модель
//!
//! [`VideoSource`] — дескриптор открытого файла: свой поток-декодер на файл
//! (демукс/декод/ресемплинг), очередь на 2-3 готовых видеокадра и звуковой
//! буфер; координатор вызывает [`VideoSource::try_recv_frame`] перед каждым
//! redraw (неблокирующе). Часы показа — зона координатора: кадры приходят
//! с корректным PTS относительно начала потока, решение «показывать сейчас
//! или ждать» этот крейт не принимает. Декодер сам держит темп, близкий к
//! PTS, чтобы не декодировать всё видео разом.
//!
//! # Безопасность
//!
//! Все unsafe-вызовы FFmpeg инкапсулированы во внутреннем модуле `pipeline`:
//! публичный API — полностью безопасный Rust, FFmpeg-контексты живут только
//! на декодер-потоке.
//!
//! # Сборка и рантайм
//!
//! Требуется пресобранный FFmpeg (LGPL-only, `W:/ffmpeg_build/install`,
//! docs/M5B_VIDEO_DESIGN.md §1) и переменная окружения `FFMPEG_DIR` при
//! сборке (см. `build.rs` и `README.md`). В рантайме DLL (`avcodec-61.dll`,
//! `avformat-61.dll`, `avutil-59.dll`, `swresample-5.dll`) должны лежать
//! рядом с исполняемым файлом или в `PATH`.

mod decoder;
mod error;
mod format;
mod pipeline;

pub use error::VideoError;
pub use pipeline::{AUDIO_TARGET_CHANNELS, AUDIO_TARGET_SAMPLE_RATE};

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use decoder::{Control, Shared, VideoInfo};
use pipeline::{AudioChunkOut, VideoFrameOut};

/// Один кадр видео: плоскости YUV420P (straight alpha, плотно упакованные)
/// и PTS относительно начала потока.
#[derive(Debug)]
pub struct DecodedVideoFrame {
    /// Плоскость Y (яркость), `width × height` байт.
    pub y: Vec<u8>,
    /// Плоскость U, `ceil(width/2) × ceil(height/2)` байт (4:2:0).
    pub u: Vec<u8>,
    /// Плоскость V, `ceil(width/2) × ceil(height/2)` байт (4:2:0).
    pub v: Vec<u8>,
    /// Ширина кадра в пикселях.
    pub width: u32,
    /// Высота кадра в пикселях.
    pub height: u32,
    /// Момент кадра от начала потока.
    pub pts: Duration,
}

/// Порция декодированного звука: f32, **interleaved** `[L,R,L,R,…]`,
/// 48 000 Гц, 2 канала (целевой формат ресемплера — фиксированный,
/// `AUDIO_TARGET_SAMPLE_RATE`/`AUDIO_TARGET_CHANNELS`).
#[derive(Debug)]
pub struct DecodedAudioSamples {
    /// Сэмплы interleaved (левый/правый канал попарно).
    pub samples: Vec<f32>,
}

/// Дескриптор открытого видеофайла: демукс/декод идут в отдельном потоке,
/// кадры и звук — через неблокирующие очереди.
///
/// Владение потоком — у этого дескриптора: `Drop` останавливает декодер
/// (Shutdown + join; поток никогда не блокируется навсегда, поэтому join
/// гарантированно завершается).
pub struct VideoSource {
    shared: Arc<Shared>,
    ctl: mpsc::Sender<Control>,
    frame_rx: mpsc::Receiver<VideoFrameOut>,
    audio_rx: mpsc::Receiver<AudioChunkOut>,
    thread: Option<thread::JoinHandle<()>>,
    info: VideoInfo,
}

impl VideoSource {
    /// Открыть файл, найти видеопоток и проверить первый кадр (формат
    /// пикселя YUV420P, размеры) — `Err` возвращается с понятной причиной,
    /// если файл не видео, кодек не собран или формат пикселя не
    /// поддерживается. Блокирует до готовности декодера (обычно миллисекунды).
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        let (info_tx, info_rx) = mpsc::channel();
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(decoder::FRAME_QUEUE_CAPACITY);
        let (audio_tx, audio_rx) = mpsc::sync_channel(decoder::AUDIO_QUEUE_CAPACITY);
        let shared = Arc::new(Shared {
            paused: std::sync::atomic::AtomicBool::new(false),
            volume: std::sync::Mutex::new(1.0),
        });

        let thread_path = path.to_path_buf();
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("rst-video-decoder".into())
            .spawn(move || {
                decoder::decoder_thread(
                    thread_path,
                    ctl_rx,
                    frame_tx,
                    audio_tx,
                    info_tx,
                    thread_shared,
                );
            })
            .map_err(|e| VideoError::Decode(format!("не удалось создать поток-декодер: {e}")))?;

        let info = match info_rx.recv() {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                let _ = ctl_tx.send(Control::Shutdown);
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                // Поток умер до отправки информации (паника) — закрываем всё.
                let _ = thread.join();
                return Err(VideoError::DecoderThreadGone);
            }
        };

        Ok(Self {
            shared,
            ctl: ctl_tx,
            frame_rx,
            audio_rx,
            thread: Some(thread),
            info,
        })
    }

    /// Продолжить воспроизведение (после `pause` или с самого начала).
    pub fn play(&self) {
        self.shared.paused.store(false, Ordering::Relaxed);
        let _ = self.ctl.send(Control::Play);
    }

    /// Поставить на паузу: декодер-поток перестаёт тянуть пакеты — CPU в
    /// простое нулевой (docs/M5B_VIDEO_DESIGN.md §2 «Пауза»).
    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Relaxed);
        let _ = self.ctl.send(Control::Pause);
    }

    /// На паузе ли воспроизведение.
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Перемотать на момент `to` от начала потока (ближайший ключевой кадр
    /// не позже цели). Синхронно: блокирует, пока декодер-поток не выполнит
    /// перемотку (пакеты, закэшированные до перемотки в очередях, могут
    /// быть устаревшими — координатору стоит сбросить их через
    /// [`Self::clear_audio_queue`] и не показывать старые кадры по PTS).
    pub fn seek(&self, to: Duration) -> Result<(), VideoError> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.ctl
            .send(Control::Seek { to, ack: ack_tx })
            .map_err(|_| VideoError::DecoderThreadGone)?;
        ack_rx.recv().map_err(|_| VideoError::DecoderThreadGone)?
    }

    /// Громкость (0.0..=1.0). Применяется микшером звука (задача C,
    /// docs/M5B_VIDEO_DESIGN.md §4) — декодер сам звук не умножает; здесь
    /// значение хранится и отдаётся по требованию.
    pub fn set_volume(&self, volume: f32) {
        *self
            .shared
            .volume
            .lock()
            .expect("volume: мьютекс не отравлен") = volume.clamp(0.0, 1.0);
    }

    /// Текущая громкость (см. [`Self::set_volume`]).
    pub fn volume(&self) -> f32 {
        *self
            .shared
            .volume
            .lock()
            .expect("volume: мьютекс не отравлен")
    }

    /// Последний готовый кадр (неблокирующе) — вызывается координатором
    /// перед каждым redraw, не из декодер-потока. `None` — свежих кадров
    /// нет (рисуем последний загруженный).
    pub fn try_recv_frame(&self) -> Option<DecodedVideoFrame> {
        self.frame_rx.try_recv().ok().map(|f| DecodedVideoFrame {
            y: f.y,
            u: f.u,
            v: f.v,
            width: f.width,
            height: f.height,
            pts: f.pts,
        })
    }

    /// Готовая порция звука (неблокирующе): f32 interleaved стерео 48 кГц.
    /// Вызывается микшером (задача C) каждый колбэк аудио-драйвера.
    pub fn try_recv_audio_samples(&self) -> Option<DecodedAudioSamples> {
        self.audio_rx
            .try_recv()
            .ok()
            .map(|c: AudioChunkOut| DecodedAudioSamples { samples: c.samples })
    }

    /// Выбросить накопленные порции звука (после `seek` устаревшие сэмплы
    /// в очереди относятся к старой позиции).
    pub fn clear_audio_queue(&self) {
        while self.audio_rx.try_recv().is_ok() {}
    }

    /// Есть ли в файле аудиодорожка.
    pub fn has_audio(&self) -> bool {
        self.info.has_audio
    }

    /// Размеры видеокадра (из первого декодированного кадра).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.info.width, self.info.height)
    }

    /// Длительность файла (None — неизвестна, например потоковое видео).
    pub fn duration(&self) -> Option<Duration> {
        self.info.duration
    }
}

impl Drop for VideoSource {
    fn drop(&mut self) {
        // Shutdown доезжает максимум за 100 мс (срезы сна пайсинга) — join
        // гарантированно завершается, поток не имеет бесконечных ожиданий.
        let _ = self.ctl.send(Control::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for VideoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoSource")
            .field("dimensions", &self.info.width)
            .field("height", &self.info.height)
            .field("duration", &self.info.duration)
            .field("has_audio", &self.info.has_audio)
            .field("paused", &self.is_paused())
            .finish_non_exhaustive()
    }
}
