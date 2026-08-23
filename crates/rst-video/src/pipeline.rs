//! FFmpeg-конвейер `rst-video`: демукс + декод видео/аудио + ресемплинг звука.
//!
//! **Вся работа с `ffmpeg-sys-next` (unsafe FFI) инкапсулирована в этом
//! модуле** — публичный API крейта (`VideoSource`) unsafe не протекает.
//! Все FFmpeg-объекты живут строго на одном потоке (декодер-поток,
//! `decoder.rs`): контексты не передаются между потоками, поэтому
//! потокобезопасность контекстов FFmpeg не требуется.
//!
//! Поддерживаемые выходные форматы пикселей (M5e): YUV420P (обычное видео),
//! YUVA420P (VP9 с альфой), YUVA444P10LE (ProRes 4444 — понижается до 8 бит
//! на CPU при распаковке) и packed RGB с qtrle (конвертируется в YUVA420P
//! на CPU — в сборке FFmpeg нет swscale, дизайн M5b §1). Форматы с альфой
//! декодируются строго программно ([`crate::format::VideoPixelFormat::
//! hwaccel_compatible`] — предикат, отсекающий их от будущего аппаратного
//! пути M5c).
//!
//! Тайминги показа — зона вызывающего кода (координатора), контракт
//! (docs/M5B_VIDEO_DESIGN.md §2): кадры отдаются с корректным PTS
//! относительно начала потока; решение «показывать сейчас или ждать» —
//! не этого крейта.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use ffmpeg_sys_next::*;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface;

use crate::error::VideoError;
use crate::format::{
    VideoPixelFormat, classify_pixel_format, downconvert_high_bit_le, full_range_to_limited,
    pts_to_duration,
    rgb_packed_to_yuva420p, swr_out_count,
};
use crate::hwaccel::{self, HwDecode};

/// Целевой формат ресемплера звука: f32, стерео, 48000 Гц (фиксированный,
/// docs/M5B_VIDEO_DESIGN.md §4 — микшер работает в одном формате и не
/// ресемплирует сам). Звук наружу отдаётся interleaved `[L,R,L,R,…]`.
pub const AUDIO_TARGET_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_TARGET_CHANNELS: usize = 2;

/// `AVERROR(EAGAIN)` — макрос FFmpeg, который bindgen не раскрывает
/// (функциональный макрос с аргументом), поэтому в биндингах константы нет.
/// `AVERROR(e) = -(e)`, в CRT Windows `EAGAIN = 11` — та же конвенция, что
/// у верхнеуровневой обёртки ffmpeg-next.
const AVERROR_EAGAIN: c_int = -11;

/// Один кадр видео: плоскости YUV420P (straight, плотно упакованные) и PTS.
/// Для прозрачных форматов (M5e) — ещё и 4-я плоскость `alpha` (полное
/// разрешение, 8 бит) и реальные размеры плоскостей U/V (`u_width`/
/// `u_height`): 4:2:0 — половина по каждой оси, 4:4:4 — полное разрешение.
#[derive(Debug)]
pub struct VideoFrameOut {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// Альфа-плоскость, `width × height` байт (полное разрешение — так у
    /// всех поддерживаемых альфа-форматов), если исходный формат несёт
    /// альфу (YUVA420P/YUVA444P10LE/qtrle); `None` — непрозрачное видео
    /// (YUV420P).
    pub alpha: Option<Vec<u8>>,
    /// Ширина плоскостей U/V (пикселей): 4:2:0 — `ceil(width/2)`, 4:4:4 —
    /// `width`. Текстуры хромы в rst-render создаются по этим размерам.
    pub u_width: u32,
    /// Высота плоскостей U/V: 4:2:0 — `ceil(height/2)`, 4:4:4 — `height`.
    pub u_height: u32,
    pub width: u32,
    pub height: u32,
    pub pts: Duration,
}

/// Порция декодированного звука: f32 interleaved `[L,R,…]`, 48 кГц.
#[derive(Debug)]
pub struct AudioChunkOut {
    pub samples: Vec<f32>,
}

/// Один аппаратный кадр (M5c, ROADMAP.md): элемент NV12-массив-текстуры
/// декодера на ОБЩЕМ с рендером `ID3D11Device` — без readback на CPU.
///
/// Владение:
/// - `texture` — COM-ссылка на массив-текстуру пула (добавочная, пул
///   держит свою; `Drop` обёртки делает `Release`).
/// - `pool_buf` — `AVBufferRef` на элемент пула: поверхность НЕ возвращается
///   в пул (декодер её не переиспользует), пока жив кадр. Снятие ссылки
///   можно делать с любого потока — пул защищён мьютексом, refcount
///   атомарный (`av_buffer.c`); декодер при исчерпании пула ждёт
///   освобождения (деградация до «поверхности закончились», но никогда —
///   до порчи кадра).
/// - `frames_ctx` — `AVBufferRef` на frames-контекст, СОЗДАВШИЙ кадр: пока
///   жив хоть один кадр, контекст (и его пул) не уничтожаются — тот же
///   инвариант, что у штатных кадров FFmpeg (`AVFrame->hw_frames_ctx`).
///   Найдено на железе: без этой ссылки teardown декодера при живых кадрах
///   освобождал пул, и поздний release элемента прыгал в освобождённую
///   память (ip == addr, call через битый указатель).
///
/// `unsafe impl Send`: кадр создан на декодер-потоке, потребляется
/// потоком рендера; все операции в `Drop` потокобезопасны (см. выше).
pub struct HwVideoFrameOut {
    /// Текстура декодера (массив-текстура NV12; элемент — `array_index`).
    pub texture: ID3D11Texture2D,
    /// Индекс элемента в массиве (AVFrame->data[1] как intptr_t).
    pub array_index: u32,
    /// Размеры текстуры (выровненные: 16/32/128 px) — видимая область
    /// `width`×`height` занимает их левый верхний угол.
    pub tex_width: u32,
    pub tex_height: u32,
    /// Видимая область кадра (coded, после кропа декодером).
    pub width: u32,
    pub height: u32,
    pub pts: Duration,
    /// Удержание элемента пула (см. док структуры).
    pub(crate) pool_buf: *mut AVBufferRef,
    /// Удержание frames-контекста (см. док структуры).
    pub(crate) frames_ctx: *mut AVBufferRef,
}

// SAFETY: см. док структуры — все операции владения потокобезопасны.
unsafe impl Send for HwVideoFrameOut {}

impl Drop for HwVideoFrameOut {
    fn drop(&mut self) {
        // SAFETY: ссылки живы (единственные из наших); unref потокобезопасен.
        unsafe {
            av_buffer_unref(&mut self.pool_buf);
            av_buffer_unref(&mut self.frames_ctx);
        }
    }
}

impl std::fmt::Debug for HwVideoFrameOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwVideoFrameOut")
            .field("array_index", &self.array_index)
            .field("tex", &format!("{}x{}", self.tex_width, self.tex_height))
            .field("visible", &format!("{}x{}", self.width, self.height))
            .field("pts", &self.pts)
            .finish_non_exhaustive()
    }
}

/// Событие декодера (docs/M5B_VIDEO_DESIGN.md §2, потоковая природа).
pub(crate) enum Event {
    /// Готовый видеокадр.
    Video(VideoFrameOut),
    /// Готовый аппаратный кадр (M5c): NV12-текстура на общем D3D11-девайсе,
    /// zero-copy. В hw-режиме программные кадры не производятся; для
    /// совместимости (старый `try_recv_frame`) декодер-поток конвертирует
    /// их в `Event::Video` через readback ([`Pipeline::hw_to_yuv`]).
    VideoHw(HwVideoFrameOut),
    /// Готовая порция звука.
    Audio(AudioChunkOut),
    /// Пакет обработан, выхода нет — декодер-поток читает следующий пакет.
    Idle,
    /// Контейнер и декодеры выкачаны до конца; вызывающий код обязан
    /// вызвать [`Pipeline::loop_restart`] перед следующим обращением
    /// (в этом срезе видео всегда зацикливается).
    Eof,
}

// --- RAII-обёртки над FFmpeg-объектами: аллокация через av*_alloc,
// --- освобождение через парный av*_free. Никаких ручных free в коде.

struct FmtCtx(*mut AVFormatContext);

impl FmtCtx {
    fn open(path: &Path) -> Result<Self, VideoError> {
        let c_path = path_to_cstring(path)?;
        let mut fmt: *mut AVFormatContext = null_mut();
        // SAFETY: c_path — живой CString на время вызова; fmt — out-параметр
        // (нулевой указатель); опции не передаём.
        let ret = unsafe { avformat_open_input(&mut fmt, c_path.as_ptr(), null(), null_mut()) };
        if ret < 0 {
            return Err(VideoError::Open {
                path: path.to_path_buf(),
                message: ff_err(ret),
            });
        }
        let fmt = Self(fmt);
        // SAFETY: fmt валиден и жив (хранится в Self); out-параметр null.
        let ret = unsafe { avformat_find_stream_info(fmt.0, null_mut()) };
        if ret < 0 {
            return Err(VideoError::Open {
                path: path.to_path_buf(),
                message: ff_err(ret),
            });
        }
        Ok(fmt)
    }

    /// Поток по индексу (индексы из av_find_best_stream — валидны).
    fn stream(&self, index: c_int) -> &AVStream {
        // SAFETY: index валиден; streams — массив указателей, живущий вместе
        // с fmt; двойное разыменование даёт ссылку на AVStream.
        unsafe { &**(*self.0).streams.offset(index as isize) }
    }
}

impl Drop for FmtCtx {
    fn drop(&mut self) {
        // SAFETY: fmt жив и не используется другими потоками; после закрытия
        // обнуляем указатель, как требует документация avformat_close_input.
        unsafe { avformat_close_input(&mut self.0) };
    }
}

struct CodecCtx(*mut AVCodecContext);

impl Drop for CodecCtx {
    fn drop(&mut self) {
        // SAFETY: контекст жив и больше не используется; см. avcodec_free_context.
        unsafe { avcodec_free_context(&mut self.0) };
    }
}

struct Packet(*mut AVPacket);

impl Drop for Packet {
    fn drop(&mut self) {
        // SAFETY: пакет жив; см. av_packet_free.
        unsafe { av_packet_free(&mut self.0) };
    }
}

struct Frame(*mut AVFrame);

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: кадр жив; см. av_frame_free.
        unsafe { av_frame_free(&mut self.0) };
    }
}

struct SwrCtx(*mut SwrContext);

impl Drop for SwrCtx {
    fn drop(&mut self) {
        // SAFETY: контекст ресемплера жив; см. swr_free.
        unsafe { swr_free(&mut self.0) };
    }
}

/// Видеопоток декодера.
struct VideoStream {
    ctx: CodecCtx,
    index: c_int,
    /// Таймбеза потока (пересчёт PTS → Duration).
    tb_num: i32,
    tb_den: i32,
}

/// Аудиопоток декодера (опционален — не во всех файлах есть звук).
struct AudioStream {
    ctx: CodecCtx,
    index: c_int,
    /// Ресемплер на целевой формат ([`AudioTarget`]); пересоздаётся при
    /// смене формата/частоты/раскладки кадров ВХОДНОГО потока (целевой
    /// формат при этом не меняется — задан один раз в `Pipeline::open`).
    swr: Option<SwrCtx>,
    swr_in: Option<(u32, AVSampleFormat, AVChannelLayout)>,
}

/// Целевой формат ресемплера звука — то, подо что реально настроено
/// устройство вывода (`rst_audio::AudioMixer::sample_rate`/`channels`), а
/// не жёстко зашитое значение. Найдено независимым ревью: раньше
/// декодер всегда ресемплировал в 48 000 Гц/стерео, а микшер писал сэмплы
/// в буфер устройства как есть, без ресемплинга — на устройстве с другой
/// частотой (44.1kHz USB-DAC и т.п.) или раскладкой каналов (5.1) звук
/// звучал бы на неверной скорости/с испорченными каналами. Теперь
/// координатор передаёт реальный формат устройства при открытии файла
/// ([`crate::VideoSource::open_with_audio_target`]);
/// [`AUDIO_TARGET_SAMPLE_RATE`]/[`AUDIO_TARGET_CHANNELS`] остаются дефолтом
/// для простого [`crate::VideoSource::open`] (тесты, вызывающий код без
/// живого микшера под рукой).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioTarget {
    pub rate: u32,
    pub channels: u16,
}

impl Default for AudioTarget {
    fn default() -> Self {
        Self {
            rate: AUDIO_TARGET_SAMPLE_RATE,
            channels: AUDIO_TARGET_CHANNELS as u16,
        }
    }
}

/// Декодирующий конвейер одного файла. Все поля — состояние одного потока.
pub(crate) struct Pipeline {
    fmt: FmtCtx,
    video: VideoStream,
    audio: Option<AudioStream>,
    audio_target: AudioTarget,
    /// Переиспользуемый буфер пакета для av_read_frame.
    packet: Packet,
    video_frame: Frame,
    audio_frame: Frame,
    /// av_read_frame вернул EOF (контейнер выкачан, декодеры пусты).
    eof_seen: bool,
    /// Пакет, не принятый кодеком (EAGAIN) на прошлой итерации, — повторяем
    /// отправку до успеха, пакеты не теряются.
    packet_pending: bool,
    /// Индекс потока отложенного пакета.
    pending_idx: Option<c_int>,
    /// Длительность контейнера (None — неизвестна).
    duration: Option<Duration>,
    /// Размеры первого кадра (валидированы в open).
    width: u32,
    height: u32,
    /// Пиксельный формат выходных кадров (M5e): YUV420P или один из
    /// альфа-форматов. Определяется первым декодированным кадром
    /// (`probe_first_video_frame`); формат кодек не меняет на протяжении
    /// потока — после перемоток/перезапусков значения остаются валидными.
    video_fmt: VideoPixelFormat,
    /// Аппаратный декод (M5c): `Some` — hwaccel D3D11VA активен, кадры
    /// выходят как [`HwVideoFrameOut`] (NV12-текстуры на общем девайсе);
    /// `None` — программный путь. `HwDecode` владеет FFmpeg-контекстами
    /// устройства/кадров и держит их живыми до закрытия пайплайна.
    hw: Option<HwDecode>,
}

/// Декодер для потока: указатель из av_find_best_stream или поиск по codec_id.
fn decoder_for(par: &AVCodecParameters, codec: *const AVCodec) -> *const AVCodec {
    if !codec.is_null() {
        return codec;
    }
    // SAFETY: codec_id — валидный enum из codecpar живого потока.
    unsafe { avcodec_find_decoder(par.codec_id) }
}

impl Pipeline {
    /// Открыть файл, найти видеопоток (и аудио, если есть), создать декодеры
    /// и проверить первый кадр (формат пикселя и реальные размеры) — чтобы
    /// `VideoSource::open` падал с понятной ошибкой на неподдерживаемом
    /// формате, а не посреди воспроизведения. `audio_target` — формат,
    /// под который звук ресемплируется (обычно реальный формат устройства
    /// вывода — `AudioTarget::default()`, если вызывающему коду он
    /// неизвестен).
    pub(crate) fn open(path: &Path, audio_target: AudioTarget) -> Result<Self, VideoError> {
        check_runtime_versions();
        Self::open_inner(path, audio_target, None)
    }

    /// Как [`Self::open`], но с аппаратным декодом D3D11VA (M5c): hwaccel
    /// включается на ОБЩЕМ с рендером `ID3D11Device`, кадры выходят как
    /// [`Event::VideoHw`] (NV12-текстуры, zero-copy). **Ключевое требование
    /// ROADMAP — корректный fallback**: при ЛЮБОЙ ошибке инициализации
    /// (нет железа/драйвера, кодек не поддерживает d3d11va, формат кадра не
    /// NV12, память) или ошибке открытия/первого кадра в hw-режиме файл
    /// переоткрывается программно — существующий программный путь остаётся
    /// дефолтом при любом сомнении.
    pub(crate) fn open_with_hw(
        path: &Path,
        audio_target: AudioTarget,
        device: &ID3D11Device,
    ) -> Result<Self, VideoError> {
        match Self::open_inner(path, audio_target, Some(device)) {
            Ok(pipe) => Ok(pipe),
            Err(e) => {
                tracing::warn!(
                    ?path,
                    error = %e,
                    "аппаратный декод не удался — переоткрываю программно"
                );
                Self::open_inner(path, audio_target, None)
            }
        }
    }

    /// Общий путь открытия: `hw_device: None` — чистый программный декод
    /// (поведение M5b/M5e, ничего не меняется); `Some` — попытка включить
    /// d3d11va с безусловным внутренним fallback на программный путь при
    /// любой ошибке инициализации hw (см. [`Self::open_with_hw`]).
    fn open_inner(
        path: &Path,
        audio_target: AudioTarget,
        hw_device: Option<&ID3D11Device>,
    ) -> Result<Self, VideoError> {
        let fmt = FmtCtx::open(path)?;

        // --- Видеопоток ---
        // SAFETY: codec — nullable out-параметр; остальные аргументы — константы.
        let mut codec: *const AVCodec = null();
        let v_index = unsafe {
            av_find_best_stream(
                fmt.0,
                AVMediaType::AVMEDIA_TYPE_VIDEO,
                -1,
                -1,
                &mut codec,
                0,
            )
        };
        if v_index < 0 {
            return Err(VideoError::NoVideoStream {
                path: path.to_path_buf(),
            });
        }
        let v_stream = fmt.stream(v_index);
        // SAFETY: codecpar валиден на время жизни потока.
        let v_par = unsafe { &*v_stream.codecpar };
        let codec = decoder_for(v_par, codec);
        if codec.is_null() {
            return Err(VideoError::UnsupportedVideoCodec {
                name: codec_name(v_par.codec_id),
            });
        }
        let mut v_ctx = CodecCtx(new_codec_ctx(codec, v_par)?);

        // Аппаратный декод (M5c): hw-контексты на ОБЩЕМ ID3D11Device.
        // При любой ошибке инициализации — warn + программный путь (контекст
        // остаётся чистым, `hw` — None). Форматы с альфой (M5e) hwaccel не
        // отдаёт: для них avcodec_open2 с hw_frames_ctx провалится, и ниже
        // сработает переоткрытие без hw — единая точка fallback.
        let mut hw: Option<HwDecode> = None;
        if let Some(device) = hw_device {
            match hwaccel::enable(v_ctx.0, codec, device) {
                Ok(state) => {
                    tracing::debug!(?path, "D3D11VA: hwaccel включён");
                    hw = Some(state);
                }
                Err(reason) => {
                    tracing::warn!(?path, reason, "D3D11VA недоступен — программный декод");
                }
            }
        }
        if hw.is_some() {
            // Frame-threading выносит init декодера (get_format → hwaccel →
            // ff_decode_get_hw_frames_ctx) на контексты-воркеры, которые
            // пересоздают frames-контекст (наш — с SHADER_RESOURCE — они
            // отбрасывают: см. pthread_frame.c update_context_*). Для
            // аппаратного пути декодируем в один поток — hwaccel и рендер
            // делят ОДИН кодек-контекст (тот же приём, что у VLC d3d11va).
            // SAFETY: контекст жив; thread_count читается в avcodec_open2.
            unsafe {
                (*v_ctx.0).thread_count = 1;
            }
        }

        // SAFETY: контекст инициализирован; codec — валидный указатель;
        // опции не передаём. В hw-режиме в контексте уже выставлены
        // hw_device_ctx/hw_frames_ctx.
        let ret = unsafe { avcodec_open2(v_ctx.0, codec, null_mut()) };
        if ret < 0 {
            if hw.is_some() {
                // Кодек/поток несовместимы с hw-путём (например, alpha-поток):
                // освобождаем hw-контексты и переоткрываем программно —
                // это и есть fallback ROADMAP («при любом сомнении»).
                tracing::warn!(
                    ?path,
                    "avcodec_open2(hw): {} — переоткрываю программно",
                    ff_err(ret)
                );
                drop(v_ctx); // hw-ссылки контекста освобождаются здесь
                hw = None;
                v_ctx = CodecCtx(new_codec_ctx(codec, v_par)?);
                let ret = unsafe { avcodec_open2(v_ctx.0, codec, null_mut()) };
                if ret < 0 {
                    return Err(VideoError::Decode(format!(
                        "avcodec_open2(видео): {}",
                        ff_err(ret)
                    )));
                }
            } else {
                return Err(VideoError::Decode(format!(
                    "avcodec_open2(видео): {}",
                    ff_err(ret)
                )));
            }
        }

        // --- Аудиопоток (опционален) ---
        let audio = {
            // related = v_index: аудиодорожка, ассоциированная с видео.
            let mut a_codec: *const AVCodec = null();
            let a_index = unsafe {
                av_find_best_stream(
                    fmt.0,
                    AVMediaType::AVMEDIA_TYPE_AUDIO,
                    -1,
                    v_index,
                    &mut a_codec,
                    0,
                )
            };
            if a_index < 0 {
                None // в файле нет звука — легально (M5B §2: has_audio=false)
            } else {
                let a_stream = fmt.stream(a_index);
                // SAFETY: codecpar валиден на время жизни потока.
                let a_par = unsafe { &*a_stream.codecpar };
                let a_codec = decoder_for(a_par, a_codec);
                if a_codec.is_null() {
                    // Кодек не собран — звук пропускается, видео играет
                    // (лучше тихий стикер, чем отказ открыть файл).
                    tracing::warn!(
                        ?path,
                        "аудиокодек {} не собран в этой сборке FFmpeg — звук пропущен",
                        codec_name(a_par.codec_id)
                    );
                    None
                } else {
                    let a_ctx = CodecCtx(new_codec_ctx(a_codec, a_par)?);
                    // SAFETY: как для видео.
                    let ret = unsafe { avcodec_open2(a_ctx.0, a_codec, null_mut()) };
                    if ret < 0 {
                        tracing::warn!(?path, "не удалось открыть аудиодекодер: {}", ff_err(ret));
                        None
                    } else {
                        Some(AudioStream {
                            ctx: a_ctx,
                            index: a_index,
                            swr: None,
                            swr_in: None,
                        })
                    }
                }
            }
        };

        // SAFETY: fmt жив; duration — i64 в AV_TIME_BASE (микросекундах),
        // отрицательное значение означает «неизвестна».
        let duration = unsafe {
            let d = (*fmt.0).duration;
            (d >= 0).then(|| Duration::from_micros(d as u64))
        };

        let mut pipe = Self {
            video: VideoStream {
                ctx: v_ctx,
                index: v_index,
                tb_num: v_stream.time_base.num,
                tb_den: v_stream.time_base.den,
            },
            audio,
            audio_target,
            packet: Packet(alloc_checked(
                unsafe { av_packet_alloc() },
                "av_packet_alloc",
            )?),
            video_frame: Frame(alloc_checked(
                unsafe { av_frame_alloc() },
                "av_frame_alloc",
            )?),
            audio_frame: Frame(alloc_checked(
                unsafe { av_frame_alloc() },
                "av_frame_alloc",
            )?),
            eof_seen: false,
            packet_pending: false,
            pending_idx: None,
            duration,
            width: 0,
            height: 0,
            video_fmt: VideoPixelFormat::Yuv420p,
            hw,
            fmt,
        };
        pipe.probe_first_video_frame(path)?;
        // Пул NV12 создан get_format-колбэком при первом кадре — берём свою
        // ссылку (для readback-пути) и запоминаем размеры текстур. v_ctx уже
        // передан в pipe (video.ctx) — используем указатель из него.
        if let Some(hw) = pipe.hw.as_mut() {
            hwaccel::attach_frames_ctx(hw, pipe.video.ctx.0);
        }
        Ok(pipe)
    }

    /// Декодировать до первого видеокадра: проверить формат пикселя
    /// (YUV420P, YUVA420P, YUVA444P10LE или packed RGB qtrle — см.
    /// [`crate::format::VideoPixelFormat`]; в hw-режиме — D3D11) и реальные
    /// размеры кадра. Кадр отбрасывается; состояние конвейера остаётся
    /// консистентным (пара первых аудио-порций при этом теряется — незаметно,
    /// идёт до первого видеокадра).
    ///
    /// В hw-режиме размеры текстуры (выровненные) известны из frames-контекста,
    /// а видимая область — из `avctx->width/height` (первый кадр несёт полный
    /// SPS/PPS): hw-кадры докладывают выровненные размеры, рендер без
    /// видимой области показал бы чёрные полосы выравнивания.
    fn probe_first_video_frame(&mut self, path: &Path) -> Result<(), VideoError> {
        for _ in 0..64 {
            match self.next()? {
                Event::Video(frame) => {
                    self.width = frame.width;
                    self.height = frame.height;
                    return Ok(());
                }
                Event::VideoHw(_) => {
                    // SAFETY: первый кадр получен — декодер заполнил
                    // avctx->width/height кодовой (crop-нутой) областью.
                    let (w, h) = unsafe {
                        (
                            (*self.video.ctx.0).width as u32,
                            (*self.video.ctx.0).height as u32,
                        )
                    };
                    if w == 0 || h == 0 {
                        return Err(VideoError::Decode(
                            "hw-кадр без размеров видимой области".into(),
                        ));
                    }
                    self.width = w;
                    self.height = h;
                    return Ok(());
                }
                Event::Eof => break,
                Event::Audio(_) | Event::Idle => {}
            }
        }
        Err(VideoError::NoFrames {
            path: path.to_path_buf(),
        })
    }

    /// Готовая порция звука; сбой ЗВУКА не убивает ВИДЕО.
    ///
    /// Так было не всегда, и это стоило пользователю всей фичи (репорт
    /// 2026-08-22): ошибка инициализации ресемплера поднималась наверх как
    /// ошибка шага конвейера, декодер-поток лечил её перезапуском цикла — и
    /// файл вечно крутился на первом кадре, «как картинка». Файлы, у
    /// которых звуковой пакет попадался раньше первого видеокадра, вообще
    /// не открывались.
    ///
    /// Звук — не обязательная часть стикера (`VideoPlayback::audio` и так
    /// `None`, когда устройство вывода не открылось), поэтому при ошибке
    /// поток звука выключается насовсем ДЛЯ ЭТОГО ФАЙЛА: дальше пакеты
    /// звука просто не декодируются (`codec_ctx_for` перестаёт их узнавать),
    /// видео продолжает играть. Лог — один раз на файл, а не на каждый
    /// пакет: прошлая версия успевала написать в журнал тысячи одинаковых
    /// строк за минуту.
    fn pull_audio_chunk_soft(&mut self) -> Option<AudioChunkOut> {
        match self.pull_audio_chunk() {
            Ok(chunk) => chunk,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "звук отключён для этого файла, видео продолжает играть"
                );
                self.audio = None;
                None
            }
        }
    }

    /// Один шаг конвейера: выкачать готовые кадры декодеров; если их нет —
    /// читать и декодировать пакеты, пока что-то не выйдет (или EOF).
    pub(crate) fn next(&mut self) -> Result<Event, VideoError> {
        // Готовые кадры декодеров выкачиваются раньше чтения новых пакетов.
        // Порядок важен и после EOF: переупорядоченные B-кадры хвоста файла
        // должны выйти наружу, прежде чем будет отдан Event::Eof.
        if let Some(frame) = self.pull_video()? {
            return Ok(frame);
        }
        if let Some(chunk) = self.pull_audio_chunk_soft() {
            return Ok(Event::Audio(chunk));
        }
        if self.eof_seen {
            // Контейнер выкачан, декодеры пусты — вызывающий код обязан
            // перезапустить цикл (loop_restart) перед следующим next().
            return Ok(Event::Eof);
        }
        if self.packet_pending {
            // Повторная отправка пакета, не принятого кодеком (EAGAIN):
            // кадры уже выкачаны выше, пробуем send ещё раз.
            let ctx = self.pending_target();
            if self.send_packet_now(ctx)? {
                self.packet_pending = false;
                self.pending_idx = None;
            }
            return Ok(Event::Idle);
        }

        // Читаем пакеты до появления выхода.
        loop {
            // SAFETY: packet — валидный аллоцированный пакет; av_read_frame
            // перезаписывает его содержимое (внутренние рефы управляются
            // самим FFmpeg).
            let ret = unsafe { av_read_frame(self.fmt.0, self.packet.0) };
            if ret == AVERROR_EOF {
                self.eof_seen = true;
                // Сигнал «данных больше нет» декодерам (NULL-пакет — штатный
                // способ FFmpeg перевести декодер в режим дренажа): без него
                // кадры, задержанные в reorder-буфере (B-кадры), никогда не
                // выйдут через avcodec_receive_frame — тот вернёт EAGAIN
                // вместо буферизованных кадров, и следующий loop_restart
                // (avcodec_flush_buffers) их уничтожит. Найдено независимым
                // ревью: каждое зацикливание теряло хвост в ~1-5 кадров.
                // SAFETY: контексты живы; NULL — валидный аргумент send_packet
                // в режиме дренажа.
                unsafe { avcodec_send_packet(self.video.ctx.0, null_mut()) };
                if let Some(audio) = self.audio.as_ref() {
                    // SAFETY: контекст жив.
                    unsafe { avcodec_send_packet(audio.ctx.0, null_mut()) };
                }
                // Выкачиваем остатки декодеров (переупорядоченные B-кадры).
                if let Some(frame) = self.pull_video()? {
                    return Ok(frame);
                }
                if let Some(chunk) = self.pull_audio_chunk_soft() {
                    return Ok(Event::Audio(chunk));
                }
                return Ok(Event::Eof);
            }
            if ret < 0 {
                return Err(VideoError::Decode(format!(
                    "av_read_frame: {}",
                    ff_err(ret)
                )));
            }
            // SAFETY: пакет заполнен av_read_frame; stream_index валиден.
            let stream_index = unsafe { (*self.packet.0).stream_index };
            let target = self.codec_ctx_for(stream_index);
            if let Some(ctx) = target {
                if !self.send_packet_now(ctx)? {
                    // EAGAIN: кодек ещё не готов — пакет остаётся нашим.
                    self.packet_pending = true;
                    self.pending_idx = Some(stream_index);
                    return Ok(Event::Idle);
                }
            }
            if let Some(frame) = self.pull_video()? {
                return Ok(frame);
            }
            if let Some(chunk) = self.pull_audio_chunk_soft() {
                return Ok(Event::Audio(chunk));
            }
            // Пакет без выхода — читаем следующий.
        }
    }

    /// Выкачать готовый видеокадр в формате текущего режима: программный
    /// (`Event::Video`, YUV-плоскости) или аппаратный (`Event::VideoHw`,
    /// NV12-текстура на общем D3D11-девайсе — M5c).
    fn pull_video(&mut self) -> Result<Option<Event>, VideoError> {
        if self.hw.is_some() {
            self.pull_video_hw_frame().map(|o| o.map(Event::VideoHw))
        } else {
            self.pull_video_frame().map(|o| o.map(Event::Video))
        }
    }

    /// Перемотать на `to` от начала потока (BACKWARD — на ближайший ключевой
    /// кадр не позже цели), сбросить декодеры и ресемплер.
    pub(crate) fn seek(&mut self, to: Duration) -> Result<(), VideoError> {
        let target_us = to.as_micros().min(i64::MAX as u128) as i64;
        // SAFETY: fmt/video живут в Self; таймбеза из константы.
        let ts = unsafe {
            av_rescale_q(
                target_us,
                AVRational {
                    num: 1,
                    den: 1_000_000,
                },
                AVRational {
                    num: self.video.tb_num,
                    den: self.video.tb_den,
                },
            )
        };
        // SAFETY: fmt жив; index валиден; флаг — константа.
        let ret = unsafe { av_seek_frame(self.fmt.0, self.video.index, ts, AVSEEK_FLAG_BACKWARD) };
        if ret < 0 {
            return Err(VideoError::Seek(ff_err(ret)));
        }
        self.reset_decoders();
        Ok(())
    }

    /// Зациклить: перемотать в начало и сбросить декодеры (в этом срезе
    /// всегда Loop, docs/M5B_VIDEO_DESIGN.md §2/§8).
    pub(crate) fn loop_restart(&mut self) -> Result<(), VideoError> {
        // SAFETY: fmt жив; index валиден; таргет 0 с BACKWARD — начало.
        let ret = unsafe { av_seek_frame(self.fmt.0, self.video.index, 0, AVSEEK_FLAG_BACKWARD) };
        if ret < 0 {
            return Err(VideoError::Seek(ff_err(ret)));
        }
        self.reset_decoders();
        Ok(())
    }

    /// Длительность контейнера (None — неизвестна, например потоковое видео).
    pub(crate) fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Есть ли в файле аудиопоток.
    pub(crate) fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    /// Несёт ли видео альфа-канал (YUVA420P/YUVA444P10LE/qtrle): выходные
    /// кадры содержат 4-ю плоскость `alpha`, рендер обязан умножать цвет
    /// на альфу (premultiplied). Определено первым декодированным кадром.
    pub(crate) fn has_alpha(&self) -> bool {
        self.video_fmt.has_alpha()
    }

    /// Будет ли файл декодироваться программно: `true` для форматов, которые
    /// аппаратный декодер (M5c) не отдаёт — с альфой и qtrle (hwaccel
    /// альфа-канал игнорирует, 4:4:4 10-бит не поддерживает); для
    /// YUV420P `false` (аппаратно-совместим — после M5c пойдёт на d3d11va,
    /// сейчас всё равно программно). Единый предикат —
    /// [`VideoPixelFormat::hwaccel_compatible`], им же будущий аппаратный
    /// путь отсекает альфа-форматы.
    pub(crate) fn software_decode(&self) -> bool {
        !self.video_fmt.hwaccel_compatible()
    }

    /// Размеры видеокадра (валидированы первым декодированным кадром).
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Активен ли аппаратный декод (M5c): `true` — кадры выходят как
    /// [`Event::VideoHw`] (NV12-текстуры на общем с рендером D3D11-девайсе).
    pub(crate) fn hw_accel(&self) -> bool {
        self.hw.is_some()
    }

    // --- внутренности ---

    /// Сброс декодеров после перемотки/перезапуска: буферы декодеров
    /// очищаются, ресемплер пересоздаётся, EOF-флаг снимается.
    fn reset_decoders(&mut self) {
        // SAFETY: контексты живы; см. avcodec_flush_buffers.
        unsafe { avcodec_flush_buffers(self.video.ctx.0) };
        if let Some(audio) = self.audio.as_mut() {
            // SAFETY: аудиоконтекст жив.
            unsafe { avcodec_flush_buffers(audio.ctx.0) };
            audio.swr = None;
            audio.swr_in = None;
        }
        self.eof_seen = false;
    }

    /// Выкачать готовый видеокадр из декодера (None — EAGAIN/EOF, кадров нет).
    fn pull_video_frame(&mut self) -> Result<Option<VideoFrameOut>, VideoError> {
        // SAFETY: контекст/кадр живы; receive перезаполняет кадр.
        let ret = unsafe { avcodec_receive_frame(self.video.ctx.0, self.video_frame.0) };
        if ret == AVERROR_EAGAIN || ret == AVERROR_EOF {
            return Ok(None);
        }
        if ret < 0 {
            return Err(VideoError::Decode(format!(
                "avcodec_receive_frame(видео): {}",
                ff_err(ret)
            )));
        }
        // SAFETY: receive вернул 0 — кадр заполнен.
        let frame = unsafe { &*self.video_frame.0 };
        let width = frame.width;
        let height = frame.height;
        if width == 0 || height == 0 {
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.video_frame.0) };
            return Ok(None);
        }
        let fmt = frame.format;
        let Some((pix_fmt, rgb)) = classify_pixel_format(fmt) else {
            let name = pixel_fmt_name(fmt);
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.video_frame.0) };
            return Err(VideoError::UnsupportedPixelFormat { name });
        };
        let pts = pts_to_duration(
            frame.best_effort_timestamp,
            self.video.tb_num,
            self.video.tb_den,
        );
        let (u_width, u_height) = pix_fmt.chroma_dims(width as u32, height as u32);
        let sizes = pix_fmt.plane_sizes(width as u32, height as u32);
        let mut y = vec![0u8; sizes.y];
        let mut u = vec![0u8; sizes.u];
        let mut v = vec![0u8; sizes.v];
        let mut alpha = sizes.alpha.map(|n| vec![0u8; n]);
        // SAFETY: `frame.data`/`frame.linesize` — буферы живого кадра, формат
        // проверен классификатором выше; строки могут быть паддированы
        // (linesize шире данных) — копируем построчно ровно нужную ширину.
        unsafe {
            let src = frame.data;
            let ls = frame.linesize;
            match (pix_fmt, rgb) {
                (VideoPixelFormat::Yuv420p, _) => {
                    copy_plane_rows_8(src[0], ls[0], width as usize, height, &mut y);
                    copy_plane_rows_8(src[1], ls[1], u_width as usize, u_height as i32, &mut u);
                    copy_plane_rows_8(src[2], ls[2], u_width as usize, u_height as i32, &mut v);
                }
                (VideoPixelFormat::Yuvj420p, _) => {
                    // Раскладка та же, что у YUV420P, — отличается только
                    // диапазон значений (полный вместо телевизионного);
                    // приводим его здесь, шейдер знает лишь один режим.
                    copy_plane_rows_8(src[0], ls[0], width as usize, height, &mut y);
                    copy_plane_rows_8(src[1], ls[1], u_width as usize, u_height as i32, &mut u);
                    copy_plane_rows_8(src[2], ls[2], u_width as usize, u_height as i32, &mut v);
                    full_range_to_limited(&mut y, &mut u, &mut v);
                }
                (VideoPixelFormat::Yuva420p, _) => {
                    copy_plane_rows_8(src[0], ls[0], width as usize, height, &mut y);
                    copy_plane_rows_8(src[1], ls[1], u_width as usize, u_height as i32, &mut u);
                    copy_plane_rows_8(src[2], ls[2], u_width as usize, u_height as i32, &mut v);
                    // Альфа — полное разрешение (4-я плоскость кадра).
                    copy_plane_rows_8(
                        src[3],
                        ls[3],
                        width as usize,
                        height,
                        alpha.as_mut().expect("YUVA420P несёт альфу"),
                    );
                }
                (VideoPixelFormat::Yuva444p10le | VideoPixelFormat::Yuva444p12le, _) => {
                    // 10-бит в 16-бит LE контейнере: строки вдвое шире,
                    // затем понижение до 8 бит на CPU (упрощение M5e).
                    let sample_bytes = pix_fmt.source_bytes_per_sample();
                    let mut staged = vec![0u8; (width as usize) * (height as usize) * sample_bytes];
                    for (plane, out) in [
                        (0usize, &mut y),
                        (1, &mut u),
                        (2, &mut v),
                        (3, alpha.as_mut().expect("YUVA444P1xLE несёт альфу")),
                    ] {
                        copy_plane_rows_16_le(
                            src[plane],
                            ls[plane],
                            width,
                            height,
                            sample_bytes,
                            &mut staged,
                        );
                        downconvert_high_bit_le(&mut staged, pix_fmt.high_bit_shift());
                        let out_len = out.len();
                        out.copy_from_slice(&staged[..out_len]);
                    }
                }
                (VideoPixelFormat::Rgb32, Some(rgb_fmt)) => {
                    // Packed RGB с qtrle-декодера: одна межстрочная плоскость,
                    // конверсия в YUVA420P на CPU (swscale в сборке отключён).
                    let row_bytes = width as usize * rgb_fmt.bytes_per_pixel();
                    let mut packed = vec![0u8; row_bytes * height as usize];
                    copy_plane_rows_8(src[0], ls[0], row_bytes, height, &mut packed);
                    let (py, pu, pv, pa) =
                        rgb_packed_to_yuva420p(&packed, width as u32, height as u32, rgb_fmt);
                    y = py;
                    u = pu;
                    v = pv;
                    alpha = Some(pa);
                }
                (VideoPixelFormat::Rgb32, None) => {
                    unreachable!("classify_pixel_format всегда даёт RgbPacked для Rgb32")
                }
            }
            av_frame_unref(self.video_frame.0);
        }
        self.video_fmt = pix_fmt;
        Ok(Some(VideoFrameOut {
            y,
            u,
            v,
            alpha,
            u_width,
            u_height,
            width: width as u32,
            height: height as u32,
            pts,
        }))
    }

    /// Выкачать готовый АППАРАТНЫЙ видеокадр (M5c, hw-режим): NV12-элемент
    /// массив-текстуры декодера, без readback на CPU. None — EAGAIN/EOF.
    ///
    /// Формат кадра обязан быть `AV_PIX_FMT_D3D11` (контракт hwaccel при
    /// выставленном `hw_frames_ctx`); иное — ошибка: первый кадр вызовет
    /// fallback на программный путь (`open_with_hw`), поздний — перезапуск
    /// цикла декодер-потоком (существующее лечение битых участков).
    fn pull_video_hw_frame(&mut self) -> Result<Option<HwVideoFrameOut>, VideoError> {
        // SAFETY: контекст/кадр живы; receive перезаполняет кадр.
        let ret = unsafe { avcodec_receive_frame(self.video.ctx.0, self.video_frame.0) };
        if ret == AVERROR_EAGAIN || ret == AVERROR_EOF {
            return Ok(None);
        }
        if ret < 0 {
            return Err(VideoError::Decode(format!(
                "avcodec_receive_frame(видео, hw): {}",
                ff_err(ret)
            )));
        }
        // SAFETY: receive вернул 0 — кадр заполнен.
        let frame = unsafe { &*self.video_frame.0 };
        if frame.format != AVPixelFormat::AV_PIX_FMT_D3D11 as c_int {
            let name = pixel_fmt_name(frame.format);
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.video_frame.0) };
            return Err(VideoError::UnsupportedPixelFormat { name });
        }
        let tex_width = frame.width;
        let tex_height = frame.height;
        if tex_width <= 0 || tex_height <= 0 {
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.video_frame.0) };
            return Ok(None);
        }
        let pts = pts_to_duration(
            frame.best_effort_timestamp,
            self.video.tb_num,
            self.video.tb_den,
        );
        // Контракт AV_PIX_FMT_D3D11 (hwcontext.h/ffmpeg docs): data[0] —
        // ID3D11Texture2D*, data[1] — индекс элемента массива (intptr_t).
        // SAFETY: данные кадра валидны до unref ниже; извлекаем текстуру
        // (добавочная COM-ссылка — `from_raw` забирает одну ссылку, `clone`
        // добавляет вторую, drop обёртки возвращает первую пулу) и ссылку
        // на элемент пула (удерживает поверхность от переиспользования,
        // пока кадр жив — см. док `HwVideoFrameOut`).
        let texture = unsafe {
            let owned = ID3D11Texture2D::from_raw(frame.data[0].cast());
            let extra = owned.clone();
            drop(owned);
            extra
        };
        let array_index = frame.data[1] as usize as u32;
        // Элемент пула + frames-контекст (см. док HwVideoFrameOut: контекст
        // жив, пока жив хоть один кадр — тот же инвариант, что у штатных
        // кадров FFmpeg).
        let pool_buf = unsafe { av_buffer_ref(frame.buf[0]) };
        let frames_ctx = unsafe { av_buffer_ref(frame.hw_frames_ctx) };
        // SAFETY: кадр больше не нужен (данные извлечены).
        unsafe { av_frame_unref(self.video_frame.0) };
        if pool_buf.is_null() || frames_ctx.is_null() {
            return Err(VideoError::Decode(
                "av_buffer_ref(элемент пула/frames-контекст): не хватило памяти".into(),
            ));
        }
        // Выровненные размеры текстуры — из frames-контекста (инициализирован
        // при включении hw); видимая область — из кодовых размеров кадра
        // (выставлены в probe, для первого кадра — кадр отбрасывается там).
        let (tex_w, tex_h) = self
            .hw
            .as_ref()
            .map(|h| (h.tex_width, h.tex_height))
            .expect("hw-режим подразумевает активный HwDecode");
        Ok(Some(HwVideoFrameOut {
            texture,
            array_index,
            tex_width: tex_w,
            tex_height: tex_h,
            width: self.width,
            height: self.height,
            pts,
            pool_buf,
            frames_ctx,
        }))
    }

    /// Программная копия аппаратного кадра (readback, M5c): NV12-текстура →
    /// YUV420P-плоскости на CPU (`av_hwframe_transfer_data` + разделение
    /// interleaved UV).
    ///
    /// Сейчас не вызывается никем: в аппаратном режиме кадр отдаётся как
    /// текстура (`try_recv_hw_frame`), а копировать его в системную память
    /// «на всякий случай» — 5.5 МБ через шину на каждый кадр 1440p и
    /// ожидание готовности GPU, то есть ровно та работа, ради отказа от
    /// которой аппаратный декод и включают (замер 2026-08-22). Код оставлен:
    /// он понадобится, когда кадр реально потребуется на процессоре — снимок
    /// кадра, экспорт, фильтр.
    #[allow(dead_code, reason = "нужен для будущего доступа к кадру на CPU")]
    pub(crate) fn hw_to_yuv(
        &mut self,
        hw_frame: &HwVideoFrameOut,
    ) -> Result<VideoFrameOut, VideoError> {
        let width = self.width as i32;
        let height = self.height as i32;
        if width <= 0 || height <= 0 {
            return Err(VideoError::Decode(
                "hw_to_yuv: размеры не выставлены".into(),
            ));
        }
        // Реконструкция исходного D3D11-кадра (трансферу нужен AVFrame с
        // hw_frames_ctx) и приёмник NV12 видимой области.
        let src = Frame(alloc_checked(
            unsafe { av_frame_alloc() },
            "av_frame_alloc",
        )?);
        let dst = Frame(alloc_checked(
            unsafe { av_frame_alloc() },
            "av_frame_alloc",
        )?);
        // SAFETY: поля кадра заполняются по контракту AV_PIX_FMT_D3D11;
        // ссылки (элемент пула, frames-контекст) живут на время вызова.
        let ret = unsafe {
            let s = &mut *src.0;
            s.format = AVPixelFormat::AV_PIX_FMT_D3D11 as c_int;
            s.width = hw_frame.tex_width as c_int;
            s.height = hw_frame.tex_height as c_int;
            s.data[0] = hw_frame.texture.as_raw().cast();
            s.data[1] = hw_frame.array_index as usize as *mut u8;
            s.buf[0] = av_buffer_ref(hw_frame.pool_buf);
            // frames-контекст — из самого кадра (держится ссылкой в кадре;
            // после перезапуска декодера может отличаться от HwDecode).
            s.hw_frames_ctx = av_buffer_ref(hw_frame.frames_ctx);
            let d = &mut *dst.0;
            d.format = AVPixelFormat::AV_PIX_FMT_NV12 as c_int;
            d.width = width;
            d.height = height;
            let ret = av_frame_get_buffer(dst.0, 32);
            if ret < 0 {
                av_frame_unref(src.0);
                av_frame_unref(dst.0);
                return Err(VideoError::Decode(format!(
                    "av_frame_get_buffer(NV12): {}",
                    ff_err(ret)
                )));
            }
            let ret = av_hwframe_transfer_data(dst.0, src.0, 0);
            av_frame_unref(src.0);
            ret
        };
        if ret < 0 {
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(dst.0) };
            return Err(VideoError::Decode(format!(
                "av_hwframe_transfer_data: {}",
                ff_err(ret)
            )));
        }
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut y = vec![0u8; w * h];
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        // SAFETY: dst — валидный NV12-кадр; строки паддированы (linesize ≥
        // данных) — копируем построчно, UV разделяем попарно.
        unsafe {
            let d = &*dst.0;
            copy_plane_rows_8(d.data[0], d.linesize[0], w, height, &mut y);
            for row in 0..ch {
                let src_row = d.data[1].add(row * d.linesize[1] as usize);
                for px in 0..cw {
                    u[row * cw + px] = *src_row.add(2 * px);
                    v[row * cw + px] = *src_row.add(2 * px + 1);
                }
            }
            av_frame_unref(dst.0);
        }
        Ok(VideoFrameOut {
            y,
            u,
            v,
            alpha: None,
            u_width: cw as u32,
            u_height: ch as u32,
            width: self.width,
            height: self.height,
            pts: hw_frame.pts,
        })
    }

    /// Выкачать готовый аудиокадр, ресемплировать в f32 stereo 48k и отдать
    /// порцией. None — кадров нет (или звука в файле нет вообще).
    fn pull_audio_chunk(&mut self) -> Result<Option<AudioChunkOut>, VideoError> {
        let Some(audio) = self.audio.as_mut() else {
            return Ok(None);
        };
        // SAFETY: аудиоконтекст/кадр живы.
        let ret = unsafe { avcodec_receive_frame(audio.ctx.0, self.audio_frame.0) };
        if ret == AVERROR_EAGAIN || ret == AVERROR_EOF {
            return Ok(None);
        }
        if ret < 0 {
            return Err(VideoError::Decode(format!(
                "avcodec_receive_frame(аудио): {}",
                ff_err(ret)
            )));
        }
        // SAFETY: receive вернул 0 — кадр заполнен.
        let frame = unsafe { &*self.audio_frame.0 };
        let in_rate = frame.sample_rate;
        let nb_samples = frame.nb_samples;
        if in_rate <= 0 || nb_samples <= 0 {
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.audio_frame.0) };
            return Ok(None);
        }
        let in_rate = in_rate as u32;
        // SAFETY: format кадра — валидный AVSampleFormat (C-перечисление);
        // transmute i32 → enum сохраняет значение бит-в-бит.
        let in_fmt = unsafe { std::mem::transmute::<c_int, AVSampleFormat>(frame.format) };
        let in_layout = frame.ch_layout;

        // Пересоздаём ресемплер при смене формата/частоты/раскладки.
        let swr_needs_recreate = match audio.swr_in.as_ref() {
            Some((rate, fmt, layout)) => {
                *rate != in_rate || *fmt != in_fmt || !ch_layout_eq(layout, &in_layout)
            }
            None => true,
        };
        if swr_needs_recreate {
            audio.swr = Some(create_swr(in_rate, in_fmt, &in_layout, self.audio_target)?);
            audio.swr_in = Some((in_rate, in_fmt, in_layout));
        }

        let target_channels = self.audio_target.channels as usize;
        // SAFETY: swr — созданный/проверенный контекст; данные кадра живы до
        // unref ниже; out-буфер наш на весь вызов.
        let swr = audio.swr.as_ref().expect("swr только что создан").0;
        let out_cap = swr_out_count(nb_samples as usize, in_rate, self.audio_target.rate);
        let mut out = vec![0.0f32; out_cap * target_channels];
        let mut out_planes: [*mut u8; 1] = [out.as_mut_ptr().cast()];
        let in_planes = frame.data.as_ptr() as *const *const u8;
        let n = unsafe {
            swr_convert(
                swr,
                out_planes.as_mut_ptr(),
                out_cap as c_int,
                in_planes,
                nb_samples,
            )
        };
        if n < 0 {
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.audio_frame.0) };
            return Err(VideoError::Decode(format!("swr_convert: {}", ff_err(n))));
        }
        out.truncate((n as usize) * target_channels);
        // SAFETY: данные скопированы в out; кадр больше не нужен.
        unsafe { av_frame_unref(self.audio_frame.0) };
        Ok(Some(AudioChunkOut { samples: out }))
    }

    /// Отправить текущий пакет в декодер указанного потока. `Ok(true)` —
    /// пакет принят (пакет больше не наш), `Ok(false)` — EAGAIN, пакет
    /// остаётся у нас и будет отправлен повторно.
    fn send_packet_now(&mut self, ctx: *mut AVCodecContext) -> Result<bool, VideoError> {
        // SAFETY: ctx жив (декодер потока); packet жив и заполнен.
        let ret = unsafe { avcodec_send_packet(ctx, self.packet.0) };
        if ret == 0 || ret == AVERROR_EOF {
            // AVERROR_EOF: декодер уже выкачан — пакет не нужен.
            // SAFETY: пакет больше не наш (send берёт собственный реф).
            unsafe { av_packet_unref(self.packet.0) };
            return Ok(true);
        }
        if ret == AVERROR_EAGAIN {
            return Ok(false);
        }
        Err(VideoError::Decode(format!(
            "avcodec_send_packet: {}",
            ff_err(ret)
        )))
    }

    /// Кодек-контекст для пакета из потока `stream_index` (None — поток,
    /// который мы не декодируем, например субтитры).
    fn codec_ctx_for(&self, stream_index: c_int) -> Option<*mut AVCodecContext> {
        if stream_index == self.video.index {
            Some(self.video.ctx.0)
        } else {
            self.audio
                .as_ref()
                .and_then(|a| (a.index == stream_index).then_some(a.ctx.0))
        }
    }

    /// Контекст для отложенного (pending) пакета.
    fn pending_target(&self) -> *mut AVCodecContext {
        self.pending_idx
            .and_then(|idx| self.codec_ctx_for(idx))
            .unwrap_or(self.video.ctx.0)
    }
}

/// Построчно скопировать 8-битную плоскость кадра: `width_bytes` байт на
/// строку, `height` строк; строки в исходнике могут быть паддированы
/// (linesize шире данных) — копируется ровно `width_bytes`.
///
/// # SAFETY
///
/// `src` — валидный буфер плоскости живого кадра с шагом строки `linesize`
/// (≥ `width_bytes`); `dst` — ровно `width_bytes × height` байт.
unsafe fn copy_plane_rows_8(
    src: *const u8,
    linesize: c_int,
    width_bytes: usize,
    height: i32,
    dst: &mut [u8],
) {
    for row in 0..height as usize {
        // SAFETY: контракт функции — см. док.
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.add(row * linesize as usize),
                dst.as_mut_ptr().add(row * width_bytes),
                width_bytes,
            );
        }
    }
}

/// Как [`copy_plane_rows_8`], но для multi-byte LE плоскостей (10-бит в
/// 16-бит контейнере, ProRes 4444): `width` сэмплов на строку =
/// `width × sample_bytes`. Копирует всю плоскость в `staged` (ровно
/// `width × height × sample_bytes` байт); понижение до 8 бит делает
/// вызывающий код.
///
/// # SAFETY
///
/// `src` — валидный буфер плоскости живого кадра с шагом строки `linesize`
/// (≥ `width × sample_bytes`); `staged` — ровно `width × height ×
/// sample_bytes` байт.
unsafe fn copy_plane_rows_16_le(
    src: *const u8,
    linesize: c_int,
    width: i32,
    height: i32,
    sample_bytes: usize,
    staged: &mut [u8],
) {
    let width_bytes = width as usize * sample_bytes;
    for row in 0..height as usize {
        // SAFETY: контракт функции — см. док.
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.add(row * linesize as usize),
                staged.as_mut_ptr().add(row * width_bytes),
                width_bytes,
            );
        }
    }
}

/// Создать кодек-контекст и скопировать параметры потока.
fn new_codec_ctx(
    codec: *const AVCodec,
    par: &AVCodecParameters,
) -> Result<*mut AVCodecContext, VideoError> {
    // SAFETY: codec — валидный указатель; контекст создаётся пустым.
    let mut ctx = unsafe { avcodec_alloc_context3(codec) };
    if ctx.is_null() {
        return Err(VideoError::Decode(
            "avcodec_alloc_context3: не хватило памяти".into(),
        ));
    }
    // SAFETY: ctx только что создан; par жив (из fmt).
    let ret = unsafe { avcodec_parameters_to_context(ctx, par) };
    if ret < 0 {
        // SAFETY: контекст больше не нужен (владелец ещё не создан).
        unsafe { avcodec_free_context(&mut ctx) };
        return Err(VideoError::Decode(format!(
            "avcodec_parameters_to_context: {}",
            ff_err(ret)
        )));
    }
    Ok(ctx)
}

/// Сверить версии ЗАГРУЖЕННЫХ библиотек FFmpeg с теми, под которые
/// сгенерированы биндинги, — один раз за процесс.
///
/// Смешать версии на удивление легко: DLL кладутся рядом с exe отдельным
/// шагом сборки, и достаточно, чтобы папка установки FFmpeg разъехалась с
/// той, по которой bindgen читал заголовки. Программа при этом запускается
/// и почти работает — поля в начале структур совпадают, дальние читаются по
/// чужим смещениям. Так выглядел репорт 2026-08-22: видео стояло картинкой,
/// потому что `AVFrame::ch_layout` читался мимо и ресемплер звука отвечал
/// EINVAL. Сборку от этого страхует `build.rs`, но проверить стоит и то, что
/// реально загрузилось: подменить DLL можно и после сборки.
fn check_runtime_versions() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: функции запроса версии не трогают состояние и безопасны с
        // любого потока.
        let (codec, util, swr) = unsafe {
            (
                avcodec_version(),
                avutil_version(),
                swresample_version(),
            )
        };
        let major = |v: u32| v >> 16;
        let expected = (
            LIBAVCODEC_VERSION_MAJOR as u32,
            LIBAVUTIL_VERSION_MAJOR as u32,
            LIBSWRESAMPLE_VERSION_MAJOR as u32,
        );
        let actual = (major(codec), major(util), major(swr));
        if actual == expected {
            tracing::debug!(?actual, "версии FFmpeg совпадают с биндингами");
        } else {
            tracing::error!(
                ?actual,
                ?expected,
                "ЗАГРУЖЕНЫ DLL FFmpeg ДРУГОЙ МАЖОРНОЙ ВЕРСИИ, чем биндинги: \\
                 раскладка структур не совпадает, поведение декодера \\
                 непредсказуемо (видео может стоять картинкой). Проверьте, \\
                 какие *.dll лежат рядом с exe."
            );
        }
    });
}

/// Создать ресемплер из входного формата в f32 stereo 48 кГц.
fn create_swr(
    in_rate: u32,
    in_fmt: AVSampleFormat,
    in_layout: &AVChannelLayout,
    target: AudioTarget,
) -> Result<SwrCtx, VideoError> {
    // Целевой формат приходит от реального устройства вывода. Ноль каналов
    // или нулевая частота — заведомо невалидная раскладка, и ресемплер
    // отказал бы с EINVAL; документированный дефолт лучше молчаливой
    // потери звука.
    let target = if target.rate == 0 || target.channels == 0 {
        tracing::warn!(
            rate = target.rate,
            channels = target.channels,
            "устройство вывода отдало невалидный формат — берём дефолт"
        );
        AudioTarget::default()
    } else {
        target
    };
    let mut swr: *mut SwrContext = null_mut();
    // Стандартная раскладка на N каналов (моно/стерео/5.1/…) — не хардкодим
    // стерео-маску: целевой формат теперь приходит от реального устройства
    // вывода (`AudioTarget`), которое не обязательно стерео.
    let mut out_layout = AVChannelLayout {
        order: AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC,
        nb_channels: 0,
        u: AVChannelLayout__bindgen_ty_1 { mask: 0 },
        opaque: null_mut(),
    };
    // SAFETY: out_layout — валидный (пусть и незаполненный) AVChannelLayout;
    // av_channel_layout_default заполняет его стандартной раскладкой на
    // channels каналов и не требует предварительной инициализации полей.
    unsafe { av_channel_layout_default(&mut out_layout, i32::from(target.channels)) };
    // SAFETY: swr — out-параметр; раскладки живут на время вызова.
    let ret = unsafe {
        swr_alloc_set_opts2(
            &mut swr,
            &out_layout,
            AVSampleFormat::AV_SAMPLE_FMT_FLT,
            target.rate as c_int,
            in_layout,
            in_fmt,
            in_rate as c_int,
            0,
            null_mut(),
        )
    };
    if ret < 0 {
        // В сообщение идут ВСЕ входные величины: по одному коду -22 в
        // журнале причину не найти, а воспроизводится это только на файле
        // пользователя (репорт 2026-08-22).
        return Err(VideoError::Decode(format!(
            "swr_alloc_set_opts2: {} (вход: {} Гц, формат {:?}, порядок {:?}, каналов {}; \
             выход: {} Гц, каналов {})",
            ff_err(ret),
            in_rate,
            in_fmt,
            in_layout.order,
            in_layout.nb_channels,
            target.rate,
            target.channels,
        )));
    }
    // SAFETY: swr инициализирован set_opts2; см. swr_init.
    let init = unsafe { swr_init(swr) };
    if init < 0 {
        // SAFETY: контекст больше не нужен.
        unsafe { swr_free(&mut swr) };
        return Err(VideoError::Decode(format!("swr_init: {}", ff_err(init))));
    }
    Ok(SwrCtx(swr))
}

/// Равенство раскладок каналов для пересоздания ресемплера. `u` — union:
/// для NATIVE/UNSPEC порядок каналов фиксирован таблицей FFmpeg (mask не
/// используется), для BITMASK важен mask; CUSTOM (map-указатель) — сравнение
/// адресов, ресемплер будет пересоздаваться каждый кадр (редкий случай).
fn ch_layout_eq(a: &AVChannelLayout, b: &AVChannelLayout) -> bool {
    if a.order != b.order || a.nb_channels != b.nb_channels {
        return false;
    }
    // SAFETY: чтение union-поля mask.
    unsafe { a.u.mask == b.u.mask }
}

/// Проверка аллокации FFmpeg-объекта (null = не хватило памяти).
fn alloc_checked<T>(ptr: *mut T, what: &str) -> Result<*mut T, VideoError> {
    if ptr.is_null() {
        Err(VideoError::Decode(format!("{what}: не хватило памяти")))
    } else {
        Ok(ptr)
    }
}

/// Путь файла как CString (FFmpeg принимает UTF-8; на Windows конвертирует
/// сам в UTF-16 внутри avio).
fn path_to_cstring(path: &Path) -> Result<CString, VideoError> {
    let text = path
        .as_os_str()
        .to_str()
        .ok_or_else(|| VideoError::NonUtf8Path {
            path: path.to_path_buf(),
        })?;
    CString::new(text).map_err(|_| VideoError::NonUtf8Path {
        path: path.to_path_buf(),
    })
}

/// Человекочитаемое имя кодека.
fn codec_name(id: AVCodecID) -> String {
    // SAFETY: возвращаемый указатель валиден до следующего вызова; копируем
    // сразу. Может быть null для невалидного id.
    let ptr = unsafe { avcodec_get_name(id) };
    if ptr.is_null() {
        format!("{id:?}")
    } else {
        // SAFETY: ptr — NUL-терминированная строка из FFmpeg.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Человекочитаемое имя формата пикселя.
fn pixel_fmt_name(fmt: c_int) -> String {
    // SAFETY: формат кадра — валидный AVPixelFormat (C-перечисление);
    // transmute i32 → enum сохраняет значение бит-в-бит.
    let pix = unsafe { std::mem::transmute::<c_int, AVPixelFormat>(fmt) };
    // SAFETY: см. av_get_pix_fmt_name; null при неизвестном формате.
    let ptr = unsafe { av_get_pix_fmt_name(pix) };
    if ptr.is_null() {
        format!("{fmt}")
    } else {
        // SAFETY: NUL-терминированная строка из FFmpeg.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Текст ошибки FFmpeg (av_strerror).
fn ff_err(code: c_int) -> String {
    let mut buf = [0 as c_char; 128];
    // SAFETY: buf — валидный буфер с размером; см. av_strerror.
    unsafe { av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: av_strerror гарантирует NUL-терминацию при успехе.
    let text = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    format!("{code}: {text}")
}
