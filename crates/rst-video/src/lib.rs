//! Крейт `rst-video`: программный декод видео через FFmpeg (M5b,
//! docs/M5B_VIDEO_DESIGN.md §2) — демукс + декод + ресемплинг звука; с M5c —
//! опциональный аппаратный декод D3D11VA (кадры как NV12-текстуры на общем
//! с рендером `ID3D11Device`, zero-copy). GPU-зависимостей нет: hw-девайс
//! приходит извне, весь FFmpeg-код живёт на декодер-потоке.
//!
//! Разделение ответственности, как в M5a: декодирование — здесь, в изоляции
//! от GPU; `rst-render` (задача B этого среза) получает плоскости Y/U/V
//! (+ опциональную альфу, M5e) и конвертирует в шейдере; микшер звука
//! (задача C, `rst-audio`/cpal) берёт готовые f32-сэмплы.
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
//! # Аппаратный декод (M5c)
//!
//! [`VideoSource::open_with_hw_device`]/[`VideoSource::open_with_audio_target_hw`]
//! принимают `ID3D11Device` рендера: hwaccel D3D11VA создаётся НА ЭТОМ ЖЕ
//! устройстве (иначе GPU-текстуру нельзя рендерить без дорогого копирования
//! между девайсами). При любой ошибке инициализации или несовместимости
//! файла (нет железа, кодек без d3d11va, не-NV12 формат, alpha-поток) —
//! безусловный fallback на программный путь: файл открывается и играет как
//! без hw (ROADMAP M5c: «корректный fallback ... остаётся дефолтом при
//! любом сомнении»). Кадры hw-режима отдаются через
//! [`VideoSource::try_recv_hw_frame`] (NV12-текстура + индекс массива,
//! без readback); старый [`VideoSource::try_recv_frame`] в hw-режиме тоже
//! работает (readback-копия для совместимости).
//!
//! # Безопасность
//!
//! Все unsafe-вызовы FFmpeg инкапсулированы во внутренних модулях
//! `pipeline`/`hwaccel`: публичный API — полностью безопасный Rust,
//! FFmpeg-контексты живут только на декодер-потоке.
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
mod hwaccel;
mod pipeline;

pub use error::VideoError;
pub use pipeline::{AUDIO_TARGET_CHANNELS, AUDIO_TARGET_SAMPLE_RATE};

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};

use decoder::{Control, Shared, VideoInfo};
use pipeline::{AudioChunkOut, HwVideoFrameOut, VideoFrameOut};

/// Один кадр видео: плоскости YUV420P (straight alpha, плотно упакованные),
/// опциональная альфа-плоскость (M5e) и PTS относительно начала потока.
#[derive(Debug)]
pub struct DecodedVideoFrame {
    /// Плоскость Y (яркость), `width × height` байт.
    pub y: Vec<u8>,
    /// Плоскость U, `u_width × u_height` байт (4:2:0 — половина по каждой
    /// оси, 4:4:4 — полное разрешение).
    pub u: Vec<u8>,
    /// Плоскость V, `u_width × u_height` байт.
    pub v: Vec<u8>,
    /// Альфа-плоскость `width × height` байт (полное разрешение), если
    /// исходный формат несёт альфу (YUVA420P/YUVA444P10LE/qtrle); `None` —
    /// непрозрачное видео (YUV420P). Цвет обязан умножаться на альфу
    /// (premultiplied — контракт всего рендера, ARCHITECTURE.md).
    pub alpha: Option<Vec<u8>>,
    /// Ширина плоскостей U/V в пикселях (4:2:0 — `ceil(width/2)`, 4:4:4 —
    /// `width`): по ним создаются текстуры хромы в rst-render.
    pub u_width: u32,
    /// Высота плоскостей U/V (4:2:0 — `ceil(height/2)`, 4:4:4 — `height`).
    pub u_height: u32,
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

/// Один АППАРАТНЫЙ кадр (M5c): NV12-элемент массив-текстуры декодера на
/// ОБЩЕМ с рендером `ID3D11Device`. Рендер может семплировать текстуру
/// напрямую (`array_index` в шейдере) — readback на CPU не происходит
/// (zero-copy, ROADMAP M5c). Кадр удерживает элемент пула декодера и его
/// frames-контекст: поверхность не переиспользуется и контекст не
/// уничтожается, пока кадр жив (см. док `rst_video::pipeline::
/// HwVideoFrameOut`). `Drop` снимает ссылки с ЛЮБОГО потока (refcounts
/// атомарны, пул под мьютексом).
pub struct HwDecodedVideoFrame {
    /// Текстура декодера (NV12-массив; элемент — [`Self::array_index`]).
    pub texture: ID3D11Texture2D,
    /// Индекс элемента в массив-текстуре (для `Texture2DArray` в шейдере).
    pub array_index: u32,
    /// Размеры текстуры (выровненные до 16/32/128 px) — видимая область
    /// `width`×`height` занимает её левый верхний угол; рендер компенсирует
    /// это UV-масштабом.
    pub tex_width: u32,
    pub tex_height: u32,
    /// Видимая область кадра (coded, после кропа декодером).
    pub width: u32,
    pub height: u32,
    /// Момент кадра от начала потока.
    pub pts: Duration,
    /// Удержание элемента пула + frames-контекста (см. док структуры).
    pool_buf: *mut ffmpeg_sys_next::AVBufferRef,
    frames_ctx: *mut ffmpeg_sys_next::AVBufferRef,
}

impl Drop for HwDecodedVideoFrame {
    fn drop(&mut self) {
        // SAFETY: ссылки живут (единственные из наших); unref потокобезопасен
        // (refcount атомарный, пул под мьютексом — av_buffer.c).
        unsafe {
            ffmpeg_sys_next::av_buffer_unref(&mut self.pool_buf);
            ffmpeg_sys_next::av_buffer_unref(&mut self.frames_ctx);
        }
    }
}

impl std::fmt::Debug for HwDecodedVideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwDecodedVideoFrame")
            .field("array_index", &self.array_index)
            .field("tex", &format!("{}x{}", self.tex_width, self.tex_height))
            .field("visible", &format!("{}x{}", self.width, self.height))
            .field("pts", &self.pts)
            .finish_non_exhaustive()
    }
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
    /// Очередь аппаратных кадров (M5c, zero-copy): NV12-текстуры на общем
    /// D3D11-девайсе. Наполняется только в hw-режиме (`open_*_hw`).
    hw_frame_rx: mpsc::Receiver<HwVideoFrameOut>,
    audio_rx: mpsc::Receiver<AudioChunkOut>,
    thread: Option<thread::JoinHandle<()>>,
    info: VideoInfo,
}

impl VideoSource {
    /// Открыть файл с ресемплингом звука под дефолтный целевой формат
    /// ([`AUDIO_TARGET_SAMPLE_RATE`]/[`AUDIO_TARGET_CHANNELS`]) — удобно для
    /// тестов и вызывающего кода без живого микшера под рукой. Продакшен-код
    /// координатора должен использовать [`Self::open_with_audio_target`] с
    /// реальным форматом устройства вывода (`rst_audio::AudioMixer::
    /// sample_rate`/`channels`) — иначе на устройстве с другой частотой/
    /// раскладкой каналов звук будет играть на неверной скорости или с
    /// испорченными каналами (найдено независимым ревью сшивки).
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        Self::open_with_audio_target(path, AUDIO_TARGET_SAMPLE_RATE, AUDIO_TARGET_CHANNELS as u16)
    }

    /// Открыть файл, найти видеопоток и проверить первый кадр (формат
    /// пикселя — YUV420P/YUVA420P/YUVA444P10LE/qtrle, размеры) — `Err`
    /// возвращается с понятной причиной, если файл не видео, кодек не
    /// собран или формат пикселя не поддерживается. Блокирует до готовности
    /// декодера (обычно миллисекунды).
    /// Звук ресемплируется в `audio_sample_rate`/`audio_channels` — обычно
    /// реальный формат устройства вывода, чтобы микшер (`rst-audio`) не
    /// ресемплировал сам (design §4).
    pub fn open_with_audio_target(
        path: &Path,
        audio_sample_rate: u32,
        audio_channels: u16,
    ) -> Result<Self, VideoError> {
        Self::open_inner(path, audio_sample_rate, audio_channels, None)
    }

    /// Открыть файл с АППАРАТНЫМ декодом (M5c, ROADMAP): d3d11va hwaccel
    /// создаётся на `device` — ОБЩЕМ D3D11-устройстве рендера (`rst-render`
    /// создаёт одно на процесс, ARCHITECTURE.md §1), иначе GPU-текстуры
    /// декодера нельзя семплировать без дорогого копирования между
    /// девайсами. Звук — под дефолтный целевой формат (см. [`Self::open`]).
    ///
    /// **Fallback**: при любой ошибке инициализации hw (нет железа/драйвера,
    /// кодек без d3d11va, не-NV12, alpha-поток) или при любом сбое открытия
    /// файла в hw-режиме открытие повторяется программно — файл играет как
    /// без hw; ошибкой возвращается только случай, когда и программный путь
    /// не смог. Проверка аппаратного пути в заголовке: [`Self::hw_accel`].
    pub fn open_with_hw_device(path: &Path, device: &ID3D11Device) -> Result<Self, VideoError> {
        Self::open_inner(
            path,
            AUDIO_TARGET_SAMPLE_RATE,
            AUDIO_TARGET_CHANNELS as u16,
            Some(device),
        )
    }

    /// Как [`Self::open_with_hw_device`], но звук ресемплируется под
    /// `audio_sample_rate`/`audio_channels` (обычно реальный формат
    /// устройства вывода — см. [`Self::open_with_audio_target`]).
    pub fn open_with_audio_target_hw(
        path: &Path,
        audio_sample_rate: u32,
        audio_channels: u16,
        device: &ID3D11Device,
    ) -> Result<Self, VideoError> {
        Self::open_inner(path, audio_sample_rate, audio_channels, Some(device))
    }

    /// Общий путь открытия; `hw_device: Some` — аппаратный декод с
    /// безусловным fallback на программный (см. [`Self::open_with_hw_device`]).
    fn open_inner(
        path: &Path,
        audio_sample_rate: u32,
        audio_channels: u16,
        hw_device: Option<&ID3D11Device>,
    ) -> Result<Self, VideoError> {
        let audio_target = pipeline::AudioTarget {
            rate: audio_sample_rate,
            channels: audio_channels,
        };
        let (info_tx, info_rx) = mpsc::channel();
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(decoder::FRAME_QUEUE_CAPACITY);
        let (hw_frame_tx, hw_frame_rx) = mpsc::sync_channel(decoder::FRAME_QUEUE_CAPACITY);
        let (audio_tx, audio_rx) = mpsc::sync_channel(decoder::AUDIO_QUEUE_CAPACITY);
        let shared = Arc::new(Shared {
            paused: std::sync::atomic::AtomicBool::new(false),
            volume: std::sync::Mutex::new(1.0),
        });

        let thread_path = path.to_path_buf();
        let thread_shared = Arc::clone(&shared);
        // Девайс живёт в VideoSource (AddRef), декодер-поток получает свою
        // ссылку на время жизни потока.
        let thread_hw_device = hw_device.cloned();
        let thread = thread::Builder::new()
            .name("rst-video-decoder".into())
            .spawn(move || {
                decoder::decoder_thread(
                    thread_path,
                    ctl_rx,
                    frame_tx,
                    hw_frame_tx,
                    audio_tx,
                    info_tx,
                    thread_shared,
                    audio_target,
                    thread_hw_device,
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
            hw_frame_rx,
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
            alpha: f.alpha,
            u_width: f.u_width,
            u_height: f.u_height,
            width: f.width,
            height: f.height,
            pts: f.pts,
        })
    }

    /// Последний готовый АППАРАТНЫЙ кадр (M5c, zero-copy, неблокирующе):
    /// NV12-текстура на общем с рендером D3D11-девайсе — семплируется
    /// шейдером напрямую, без readback на CPU. Только в hw-режиме
    /// (`open_*_hw` и [`Self::hw_accel`] == true); иначе всегда `None`.
    /// Возвращённый кадр удерживает элемент пула декодера и frames-контекст
    /// — держать его дольше пары кадров нельзя (пул конечен, декодер
    /// встанет в ожидание), а ДО закрытия источника кадры стоит отпустить
    /// (см. док [`Self::try_recv_frame`]).
    pub fn try_recv_hw_frame(&self) -> Option<HwDecodedVideoFrame> {
        self.hw_frame_rx
            .try_recv()
            .ok()
            .map(|mut f: HwVideoFrameOut| {
                // Ссылки пула/контекста ПЕРЕХОДЯТ в публичный кадр: элемент не
                // переиспользуется и frames-контекст не уничтожается, пока кадр
                // жив (см. док структуры). f дропается с нулевыми указателями.
                let texture = f.texture.clone(); // COM AddRef
                let pool_buf = std::mem::replace(&mut f.pool_buf, std::ptr::null_mut());
                let frames_ctx = std::mem::replace(&mut f.frames_ctx, std::ptr::null_mut());
                HwDecodedVideoFrame {
                    texture,
                    array_index: f.array_index,
                    tex_width: f.tex_width,
                    tex_height: f.tex_height,
                    width: f.width,
                    height: f.height,
                    pts: f.pts,
                    pool_buf,
                    frames_ctx,
                }
            })
    }

    /// Активен ли аппаратный декод (M5c): `true` — кадры отдаются через
    /// [`Self::try_recv_hw_frame`] (zero-copy NV12); старый
    /// [`Self::try_recv_frame`] при этом тоже работает (readback-копия).
    /// `false` — файл декодируется программно (не запрошен hw, несовместимый
    /// формат/кодек, нет железа — сработал fallback).
    pub fn hw_accel(&self) -> bool {
        self.info.hw
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

    /// Несёт ли видео альфа-канал (M5e): YUVA420P (WebM/VP9), YUVA444P10LE
    /// (ProRes 4444) или qtrle. Если да — кадры содержат
    /// [`DecodedVideoFrame::alpha`], и рендер обязан умножать цвет на альфу
    /// (premultiplied — контракт всего проекта, ARCHITECTURE.md).
    pub fn has_alpha(&self) -> bool {
        self.info.has_alpha
    }

    /// Будет ли файл декодироваться программно: `true` для файлов с альфой
    /// (YUVA420P/YUVA444P10LE/qtrle) — аппаратные декодеры альфа-канал не
    /// отдают, такие файлы обязаны идти мимо hwaccel (ARCHITECTURE.md §4.4;
    /// после M5c это сохранится); `false` для обычного YUV420P (сейчас
    /// тоже декодируется программно — аппаратного пути нет). UI может
    /// честно показывать «программный декод» для таких файлов (ROADMAP M5e).
    pub fn software_decode(&self) -> bool {
        self.info.software_decode
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
        // Очереди кадров дренируются ДО Shutdown: декодер-поток при
        // завершении освобождает пул NV12 (frames-контекст) — если в очереди
        // останутся живые аппаратные кадры с удержаниями пула, их поздний
        // release наткнётся на освобождённый пул (найдено на железе: double
        // release текстуры декодера, ip == addr). Дренаж здесь + снятие
        // ссылок в `HwDecodedVideoFrame` (см. док) гарантируют, что к
        // моменту освобождения контекста живых удержаний нет.
        while self.hw_frame_rx.try_recv().is_ok() {}
        while self.frame_rx.try_recv().is_ok() {}
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
            .field("hw_accel", &self.info.hw)
            .field("paused", &self.is_paused())
            .finish_non_exhaustive()
    }
}
