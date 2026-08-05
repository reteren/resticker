//! FFmpeg-конвейер `rst-video`: демукс + декод видео/аудио + ресемплинг звука.
//!
//! **Вся работа с `ffmpeg-sys-next` (unsafe FFI) инкапсулирована в этом
//! модуле** — публичный API крейта (`VideoSource`) unsafe не протекает.
//! Все FFmpeg-объекты живут строго на одном потоке (декодер-поток,
//! `decoder.rs`): контексты не передаются между потоками, поэтому
//! потокобезопасность контекстов FFmpeg не требуется.
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

use crate::error::VideoError;
use crate::format::{pts_to_duration, swr_out_count, yuv420p_plane_sizes};

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
#[derive(Debug)]
pub struct VideoFrameOut {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub pts: Duration,
}

/// Порция декодированного звука: f32 interleaved `[L,R,…]`, 48 кГц.
#[derive(Debug)]
pub struct AudioChunkOut {
    pub samples: Vec<f32>,
}

/// Событие декодера (docs/M5B_VIDEO_DESIGN.md §2, потоковая природа).
pub(crate) enum Event {
    /// Готовый видеокадр.
    Video(VideoFrameOut),
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
    /// Ресемплер на целевой формат (f32 stereo 48k); пересоздаётся при
    /// смене формата/частоты/раскладки кадров.
    swr: Option<SwrCtx>,
    swr_in: Option<(u32, AVSampleFormat, AVChannelLayout)>,
}

/// Декодирующий конвейер одного файла. Все поля — состояние одного потока.
pub(crate) struct Pipeline {
    fmt: FmtCtx,
    video: VideoStream,
    audio: Option<AudioStream>,
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
    /// формате, а не посреди воспроизведения.
    pub(crate) fn open(path: &Path) -> Result<Self, VideoError> {
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
        let v_ctx = CodecCtx(new_codec_ctx(codec, v_par)?);
        // SAFETY: контекст инициализирован; codec — валидный указатель;
        // опции не передаём.
        let ret = unsafe { avcodec_open2(v_ctx.0, codec, null_mut()) };
        if ret < 0 {
            return Err(VideoError::Decode(format!(
                "avcodec_open2(видео): {}",
                ff_err(ret)
            )));
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
            fmt,
        };
        pipe.probe_first_video_frame(path)?;
        Ok(pipe)
    }

    /// Декодировать до первого видеокадра: проверить формат пикселя (YUV420P
    /// обязателен — конвертации в этом крейте нет, сборка FFmpeg без swscale)
    /// и реальные размеры кадра. Кадр отбрасывается; состояние конвейера
    /// остаётся консистентным (пара первых аудио-порций при этом теряется —
    /// незаметно, идёт до первого видеокадра).
    fn probe_first_video_frame(&mut self, path: &Path) -> Result<(), VideoError> {
        for _ in 0..64 {
            match self.next()? {
                Event::Video(frame) => {
                    self.width = frame.width;
                    self.height = frame.height;
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

    /// Один шаг конвейера: выкачать готовые кадры декодеров; если их нет —
    /// читать и декодировать пакеты, пока что-то не выйдет (или EOF).
    pub(crate) fn next(&mut self) -> Result<Event, VideoError> {
        // Готовые кадры декодеров выкачиваются раньше чтения новых пакетов.
        // Порядок важен и после EOF: переупорядоченные B-кадры хвоста файла
        // должны выйти наружу, прежде чем будет отдан Event::Eof.
        if let Some(frame) = self.pull_video_frame()? {
            return Ok(Event::Video(frame));
        }
        if let Some(chunk) = self.pull_audio_chunk()? {
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
                // Выкачиваем остатки декодеров (переупорядоченные B-кадры).
                if let Some(frame) = self.pull_video_frame()? {
                    return Ok(Event::Video(frame));
                }
                if let Some(chunk) = self.pull_audio_chunk()? {
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
            if let Some(frame) = self.pull_video_frame()? {
                return Ok(Event::Video(frame));
            }
            if let Some(chunk) = self.pull_audio_chunk()? {
                return Ok(Event::Audio(chunk));
            }
            // Пакет без выхода — читаем следующий.
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

    /// Размеры видеокадра (валидированы первым декодированным кадром).
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
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
        if fmt != AVPixelFormat::AV_PIX_FMT_YUV420P as c_int {
            let name = pixel_fmt_name(fmt);
            // SAFETY: кадр больше не нужен.
            unsafe { av_frame_unref(self.video_frame.0) };
            return Err(VideoError::UnsupportedPixelFormat { name });
        }
        let pts = pts_to_duration(
            frame.best_effort_timestamp,
            self.video.tb_num,
            self.video.tb_den,
        );
        let (ys, us, vs) = yuv420p_plane_sizes(width as u32, height as u32);
        let mut y = vec![0u8; ys];
        let mut u = vec![0u8; us];
        let mut v = vec![0u8; vs];
        // SAFETY: data/linesize — буферы кадра, размеры плоскостей проверены
        // выше (YUV420P); строки в linesize могут быть шире w (паддинг) —
        // копируем построчно ровно w/ceil(w/2) байт.
        unsafe {
            let src = frame.data;
            let ls = frame.linesize;
            for row in 0..height as usize {
                std::ptr::copy_nonoverlapping(
                    src[0].add(row * ls[0] as usize),
                    y.as_mut_ptr().add(row * width as usize),
                    width as usize,
                );
            }
            let cw = (width as usize).div_ceil(2);
            let ch = (height as usize).div_ceil(2);
            for row in 0..ch {
                std::ptr::copy_nonoverlapping(
                    src[1].add(row * ls[1] as usize),
                    u.as_mut_ptr().add(row * cw),
                    cw,
                );
                std::ptr::copy_nonoverlapping(
                    src[2].add(row * ls[2] as usize),
                    v.as_mut_ptr().add(row * cw),
                    cw,
                );
            }
            av_frame_unref(self.video_frame.0);
        }
        Ok(Some(VideoFrameOut {
            y,
            u,
            v,
            width: width as u32,
            height: height as u32,
            pts,
        }))
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
            audio.swr = Some(create_swr(in_rate, in_fmt, &in_layout)?);
            audio.swr_in = Some((in_rate, in_fmt, in_layout));
        }

        // SAFETY: swr — созданный/проверенный контекст; данные кадра живы до
        // unref ниже; out-буфер наш на весь вызов.
        let swr = audio.swr.as_ref().expect("swr только что создан").0;
        let out_cap = swr_out_count(nb_samples as usize, in_rate, AUDIO_TARGET_SAMPLE_RATE);
        let mut out = vec![0.0f32; out_cap * AUDIO_TARGET_CHANNELS];
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
        out.truncate((n as usize) * AUDIO_TARGET_CHANNELS);
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

/// Создать ресемплер из входного формата в f32 stereo 48 кГц.
fn create_swr(
    in_rate: u32,
    in_fmt: AVSampleFormat,
    in_layout: &AVChannelLayout,
) -> Result<SwrCtx, VideoError> {
    let mut swr: *mut SwrContext = null_mut();
    let out_layout = AVChannelLayout {
        order: AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
        nb_channels: AUDIO_TARGET_CHANNELS as c_int,
        // SAFETY: union-инициализация; mask для NATIVE-порядка не читается.
        u: AVChannelLayout__bindgen_ty_1 {
            mask: AV_CH_FRONT_LEFT | AV_CH_FRONT_RIGHT,
        },
        opaque: null_mut(),
    };
    // SAFETY: swr — out-параметр; раскладки живут на время вызова.
    let ret = unsafe {
        swr_alloc_set_opts2(
            &mut swr,
            &out_layout,
            AVSampleFormat::AV_SAMPLE_FMT_FLT,
            AUDIO_TARGET_SAMPLE_RATE as c_int,
            in_layout,
            in_fmt,
            in_rate as c_int,
            0,
            null_mut(),
        )
    };
    if ret < 0 {
        return Err(VideoError::Decode(format!(
            "swr_alloc_set_opts2: {}",
            ff_err(ret)
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
