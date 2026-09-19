//! Микшер нескольких одновременных источников звука в один выходной поток.
//!
//! M5B_VIDEO_DESIGN.md, раздел 4: один общий `cpal::Stream` на весь процесс,
//! в колбэке аудио-драйвера (реальное время, без аллокаций) сэмплы всех
//! играющих источников суммируются с per-source громкостью и глобальной
//! громкостью/mute, результат hard-clamp в `[-1.0, 1.0]` — простое сложение
//! нескольких источников может выйти за границы.
//!
//! Слой разделён на две части:
//! - `MixerCore` — чистая логика микширования из очередей (тестируется
//!   юнитами без устройства);
//! - `AudioMixer` — тонкий адаптер над cpal: поднимает поток устройства
//!   (не тестируется юнитами, покрыт `#[ignore]`-смоуком на реальное
//!   устройство).
//!
//! Декодеры кладут ресемплированные под формат устройства f32-сэмплы через
//! `AudioSource::push_samples` (кольцевая очередь с небольшим мьютексом,
//! без аллокаций в колбэке). Если у источника сэмплов не хватает — колбэк
//! пишет тишину вместо недостающих, не блокируясь на ожидании декодера.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Data, SampleFormat, Stream};
use uuid::Uuid;

use crate::error::AudioError;

/// Максимум буферизованных сэмплов на один источник (~20 секунд стерео
/// 48 кГц). Декодер обязан успевать за потреблением; при переполнении
/// отбрасываются самые старые сэмплы (устаревший звук хуже, чем потерянный).
const MAX_BUFFERED_SAMPLES: usize = 1 << 20;

/// Начальная ёмкость очереди источника: хватает на секунду-две стерео
/// 48 кГц, растёт лениво до `MAX_BUFFERED_SAMPLES`.
const INITIAL_QUEUE_CAPACITY: usize = 1 << 16;

/// Снимает мьютекс с восстановлением после отравления (паникующий поток
/// не должен ронять аудио-колбэк).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Микширует N источников в один выходной буфер.
///
/// Чистая функция без устройства и очередей — контракт микширования,
/// покрывается юнит-тестами. Для каждого выходного сэмпла `out[i]`
/// суммируются сэмплы всех источников `sources[j][i]`, каждый умножен на
/// свою громкость `volumes[j]` и на `global_volume`; итог hard-clamp в
/// `[-1.0, 1.0]`.
///
/// - Источник короче `out` вносит тишину (0.0) за недостающие сэмплы;
///   длиннее `out` — его хвост игнорируется (будет потреблён следующим
///   колбэком).
/// - `muted == true` обнуляет выход целиком (глобальный «Заглушить все»,
///   SPEC.md §7.1).
/// - Громкости ожидаются в `0.0..=1.0`; не-конечные значения трактуются
///   как 0.0 (защита от битого значения из декодера), отрицательные —
///   обрезаются.
///
/// # Panics
///
/// Паникует, если `sources` и `volumes` разной длины — это программная
/// ошибка вызывающего кода.
pub fn mix_frame(
    out: &mut [f32],
    sources: &[&[f32]],
    volumes: &[f32],
    global_volume: f32,
    muted: bool,
) {
    assert_eq!(
        sources.len(),
        volumes.len(),
        "источников и громкостей должно быть поровну"
    );
    out.fill(0.0);
    let global = if muted || !global_volume.is_finite() {
        0.0
    } else {
        global_volume.clamp(0.0, 1.0)
    };
    if global == 0.0 || sources.is_empty() {
        return;
    }
    for (samples, &volume) in sources.iter().zip(volumes) {
        let v = volume * global;
        // NaN и ноль не дают вклада; отрицательная громкость не имеет смысла.
        if v.is_nan() || v <= 0.0 {
            continue;
        }
        for (dst, &s) in out.iter_mut().zip(*samples) {
            *dst += s * v;
        }
    }
    for v in out.iter_mut() {
        *v = v.clamp(-1.0, 1.0);
    }
}

/// Порог тишины по умолчанию (~1.5 секунды), после которого поток вывода
/// переводится в режим паузы (cpal stream.pause()) для экономии CPU и питания.
pub const DEFAULT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Состояние воспроизведения аудио-потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPlaybackState {
    Playing,
    Paused,
}

/// Действие, которое необходимо применить к потоку cpal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAction {
    None,
    Play,
    Pause,
}

/// Чистая логика отслеживания активности аудио-потока и управления паузой/воспроизведением.
///
/// Не зависит от cpal и аудио-устройств — полностью покрывается юнит-тестами.
/// Реализует гистерезис:
/// - Переход в `Paused` только после непрерывной тишины длительностью не менее `threshold_frames`.
/// - Переход в `Playing` немедленно при появлении данных у любого источника.
/// - Исключает частое переключение (chatter) и потерю первых сэмплов.
#[derive(Debug)]
pub struct IdleDetector {
    state: StreamPlaybackState,
    silence_frames: u64,
    threshold_frames: u64,
}

impl IdleDetector {
    /// Создаёт детектор с заданным порогом тишины в аудио-фреймах.
    pub fn new(threshold_frames: u64) -> Self {
        Self {
            state: StreamPlaybackState::Playing,
            silence_frames: 0,
            threshold_frames,
        }
    }

    /// Текущее состояние воспроизведения.
    pub fn state(&self) -> StreamPlaybackState {
        self.state
    }

    /// Число непрерывных фреймов тишины.
    pub fn silence_frames(&self) -> u64 {
        self.silence_frames
    }

    /// Порог тишины в фреймах.
    pub fn threshold_frames(&self) -> u64 {
        self.threshold_frames
    }

    /// Обработка результата рендера одного буфера.
    ///
    /// `has_data` — содержал ли буфер реальные сэмплы из очередей источников.
    /// `frames` — количество фреймов в буфере.
    pub fn on_render(&mut self, has_data: bool, frames: u64) -> StreamAction {
        if has_data {
            self.silence_frames = 0;
            if self.state == StreamPlaybackState::Paused {
                self.state = StreamPlaybackState::Playing;
                return StreamAction::Play;
            }
            StreamAction::None
        } else {
            if self.state == StreamPlaybackState::Playing {
                self.silence_frames = self.silence_frames.saturating_add(frames);
                if self.silence_frames >= self.threshold_frames {
                    self.state = StreamPlaybackState::Paused;
                    return StreamAction::Pause;
                }
            }
            StreamAction::None
        }
    }

    /// Обработка поступления новых данных в любой источник.
    pub fn on_data_available(&mut self) -> StreamAction {
        self.silence_frames = 0;
        if self.state == StreamPlaybackState::Paused {
            self.state = StreamPlaybackState::Playing;
            StreamAction::Play
        } else {
            StreamAction::None
        }
    }
}

/// Интерфейс управления аудио-потоком из микшера.
pub(crate) trait StreamControl: Send + Sync {
    /// Уведомление о рендере буфера (вызывается из audio callback).
    fn on_render(&self, has_data: bool, frames: u64);
    /// Возобновление потока при появлении данных (вызывается из push_samples).
    fn resume(&self);
}

/// Контроллер потока cpal: связывает IdleDetector со слабым указателем на cpal::Stream.
struct CpalStreamController {
    detector: Mutex<IdleDetector>,
    weak_stream: std::sync::Weak<Stream>,
    is_paused: Arc<AtomicBool>,
}

impl CpalStreamController {
    fn new(
        weak_stream: std::sync::Weak<Stream>,
        threshold_frames: u64,
        is_paused: Arc<AtomicBool>,
    ) -> Self {
        Self {
            detector: Mutex::new(IdleDetector::new(threshold_frames)),
            weak_stream,
            is_paused,
        }
    }
}

impl StreamControl for CpalStreamController {
    fn on_render(&self, has_data: bool, frames: u64) {
        let mut det = lock(&self.detector);
        let action = det.on_render(has_data, frames);
        match action {
            StreamAction::Pause => {
                self.is_paused.store(true, Ordering::Relaxed);
                if let Some(stream) = self.weak_stream.upgrade() {
                    if let Err(e) = stream.pause() {
                        tracing::warn!(error = %e, "не удалось приостановить cpal поток");
                    }
                }
            }
            StreamAction::Play => {
                self.is_paused.store(false, Ordering::Relaxed);
                if let Some(stream) = self.weak_stream.upgrade() {
                    if let Err(e) = stream.play() {
                        tracing::warn!(error = %e, "не удалось возобновить cpal поток");
                    }
                }
            }
            StreamAction::None => {}
        }
    }

    fn resume(&self) {
        let mut det = lock(&self.detector);
        if det.on_data_available() == StreamAction::Play {
            self.is_paused.store(false, Ordering::Relaxed);
            if let Some(stream) = self.weak_stream.upgrade() {
                if let Err(e) = stream.play() {
                    tracing::warn!(error = %e, "не удалось возобновить cpal поток");
                }
            }
        }
    }
}

/// Чистая логика микшера: очередь источников, глобальная громкость/mute.
/// Не знает про cpal — тестируется юнитами напрямую.
struct MixerCore {
    sources: Mutex<HashMap<Uuid, Arc<SourceState>>>,
    /// Биты f32 глобальной громкости (0.0..=1.0).
    global_volume: AtomicU32,
    muted: AtomicBool,
    channels: AtomicU32,
    controller: Mutex<Option<Arc<dyn StreamControl>>>,
    is_stream_paused: Arc<AtomicBool>,
}

/// Состояние одного источника: очередь сэмплов + громкость.
struct SourceState {
    queue: Mutex<VecDeque<f32>>,
    /// Биты f32 громкости источника (0.0..=1.0).
    volume: AtomicU32,
    /// Источник помечен на удаление: колбэк обязан пропустить его.
    dropped: AtomicBool,
}

impl MixerCore {
    fn new() -> Self {
        Self {
            sources: Mutex::new(HashMap::new()),
            global_volume: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            channels: AtomicU32::new(2),
            controller: Mutex::new(None),
            is_stream_paused: Arc::new(AtomicBool::new(false)),
        }
    }

    fn set_channels(&self, channels: u16) {
        self.channels.store(channels.max(1) as u32, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn set_controller(&self, controller: Arc<dyn StreamControl>) {
        *lock(&self.controller) = Some(controller);
    }

    #[cfg(not(test))]
    fn set_controller(&self, controller: Arc<dyn StreamControl>) {
        *lock(&self.controller) = Some(controller);
    }

    fn on_data_pushed(&self) {
        // Без предварительной проверки `is_stream_paused`: флаг выставляет
        // колбэк, и push, пришедший между решением «уснуть» и записью флага,
        // оставил бы сэмплы в очереди остановленного потока. Решение
        // «проснуться» принимается под тем же мьютексом детектора, что и
        // «уснуть», и он же сбрасывает счётчик тишины — поэтому колбэк,
        // посчитавший тишину до этого push, уснуть уже не успеет.
        let ctrl = lock(&self.controller).clone();
        if let Some(ctrl) = ctrl {
            ctrl.resume();
        }
    }

    fn register_source(&self, id: Uuid) -> Arc<SourceState> {
        let state = Arc::new(SourceState {
            queue: Mutex::new(VecDeque::with_capacity(INITIAL_QUEUE_CAPACITY)),
            volume: AtomicU32::new(1.0f32.to_bits()),
            dropped: AtomicBool::new(false),
        });
        lock(&self.sources).insert(id, Arc::clone(&state));
        state
    }

    /// Помечает источник удалённым и убирает из мапы. Повторный вызов с тем
    /// же id — no-op; `AudioSource::drop` тоже вызывает его (идемпотентно).
    fn remove_source(&self, id: &Uuid) {
        if let Some(state) = lock(&self.sources).remove(id) {
            state.dropped.store(true, Ordering::Relaxed);
        }
    }

    /// Горячий путь: смешивает все активные источники в `out` (буфер
    /// устройства). Вызывается аудио-потоком в реальном времени — без
    /// аллокаций и блокировок длиннее микросекунд: один короткий мьютекс
    /// на мапу и по одному на очередь.
    /// Возвращает `true`, если хотя бы один источник предоставил сэмплы.
    fn mix_into(&self, out: &mut [f32]) -> bool {
        let muted = self.muted.load(Ordering::Relaxed);
        let global = f32::from_bits(self.global_volume.load(Ordering::Relaxed));
        let global = if muted || !global.is_finite() {
            0.0
        } else {
            global.clamp(0.0, 1.0)
        };
        out.fill(0.0);
        let channels = self.channels.load(Ordering::Relaxed).max(1) as usize;
        let frames = (out.len() / channels) as u64;

        let (any_samples, controller) = {
            let sources = lock(&self.sources);
            if sources.is_empty() {
                (false, lock(&self.controller).clone())
            } else {
                let mut any_samples = false;
                for state in sources.values() {
                    if state.dropped.load(Ordering::Relaxed) {
                        continue;
                    }
                    let volume = f32::from_bits(state.volume.load(Ordering::Relaxed));
                    let mut queue = lock(&state.queue);
                    if queue.is_empty() {
                        continue;
                    }
                    let samples = queue.make_contiguous();
                    let n = samples.len().min(out.len());
                    let v = volume * global;
                    if v > 0.0 {
                        for (dst, &s) in out.iter_mut().zip(samples[..n].iter()) {
                            *dst += s * v;
                        }
                    }
                    // Потребляем всегда, даже при mute/нулевой громкости: молчание
                    // тоже «проигрывается», иначе после unmute зазвучали бы
                    // устаревшие сэмплы вразнобой с видео.
                    queue.drain(..n);
                    if n > 0 {
                        any_samples = true;
                    }
                }
                for v in out.iter_mut() {
                    *v = v.clamp(-1.0, 1.0);
                }
                (any_samples, lock(&self.controller).clone())
            }
        };

        if let Some(ctrl) = controller {
            ctrl.on_render(any_samples, frames);
        }

        any_samples
    }

    fn set_global_volume(&self, volume: f32) {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.global_volume.store(v.to_bits(), Ordering::Relaxed);
    }

    fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

/// Микшер с живым выходным потоком cpal: один на весь процесс.
///
/// Открывает устройство вывода по умолчанию и поток в `new()` — поток
/// автоматически встаёт на паузу (stream.pause()) при отсутствии звука
/// дольше ~1.5 с и возобновляется (stream.play()) при поступлении сэмплов.
/// Источники создаются `add_source` (id берётся у координатора — тот же `Uuid`,
/// что у стикера) и живут, пока жив `AudioSource`-хендл или пока не вызван `remove_source`.
pub struct AudioMixer {
    core: Arc<MixerCore>,
    /// Удерживается только ради Drop: закрытие потока останавливает звук.
    _stream: Arc<Stream>,
    _device: cpal::Device,
    sample_rate: u32,
    channels: u16,
}

impl AudioMixer {
    /// Открывает устройство вывода по умолчанию и поднимает поток.
    ///
    /// Если формат конфигурации устройства не f32/i16 — ищет первый
    /// подходящий среди поддерживаемых конфигураций; если таких нет,
    /// возвращает [`AudioError::UnsupportedSampleFormat`].
    pub fn new() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;
        let default = device.default_output_config()?;
        let (sample_format, stream_config) = if matches!(
            default.sample_format(),
            SampleFormat::F32 | SampleFormat::I16
        ) {
            (default.sample_format(), default.config())
        } else {
            let mut chosen = None;
            for range in device.supported_output_configs()? {
                if matches!(range.sample_format(), SampleFormat::F32 | SampleFormat::I16) {
                    chosen = Some((range.sample_format(), range.with_max_sample_rate().config()));
                    break;
                }
            }
            chosen.ok_or(AudioError::UnsupportedSampleFormat(default.sample_format()))?
        };

        let core = Arc::new(MixerCore::new());
        core.set_channels(stream_config.channels);
        let callback_core = Arc::clone(&core);
        let stream = device
            .build_output_stream_raw(
                stream_config,
                sample_format,
                move |data: &mut Data, _info: &cpal::OutputCallbackInfo| {
                    write_output(&callback_core, data);
                },
                |err| {
                    tracing::error!(error = %err, "ошибка аудио-потока: устройство может быть отключено");
                },
                None,
            )?;
        stream.play()?;

        let stream = Arc::new(stream);
        let threshold_frames = (stream_config.sample_rate as u64
            * DEFAULT_IDLE_TIMEOUT.as_millis() as u64)
            / 1000;
        let controller = Arc::new(CpalStreamController::new(
            Arc::downgrade(&stream),
            threshold_frames,
            Arc::clone(&core.is_stream_paused),
        ));
        core.set_controller(controller);

        // Формат устройства уходит в декодер видео как цель ресемплинга
        // (`VideoSource::open_with_audio_target`), и неправдоподобные
        // значения там превращаются в отказ инициализировать звук — а
        // разбираться в этом по симптому «видео не играет» очень дорого
        // (репорт 2026-08-22). Пишем в лог один раз при старте.
        tracing::info!(
            sample_rate = stream_config.sample_rate,
            channels = stream_config.channels,
            ?sample_format,
            "аудиоустройство открыто"
        );
        Ok(Self {
            sample_rate: stream_config.sample_rate,
            channels: stream_config.channels,
            core,
            _stream: stream,
            _device: device,
        })
    }

    /// Находится ли поток вывода устройства в режиме паузы (покой без звука).
    pub fn is_stream_paused(&self) -> bool {
        self.core.is_stream_paused.load(Ordering::Relaxed)
    }

    /// Регистрирует новый источник звука с уникальным `id` (id стикера).
    pub fn add_source(&self, id: Uuid) -> AudioSource {
        let state = self.core.register_source(id);
        AudioSource {
            id,
            state,
            core: Arc::clone(&self.core),
        }
    }

    /// Убирает источник по id (при удалении стикера). Идемпотентен.
    pub fn remove_source(&self, id: &Uuid) {
        self.core.remove_source(id);
    }

    /// Глобальная громкость всех источников сразу: `0.0..=1.0`, вне
    /// диапазона обрезается, не-конечные значения трактуются как 0.0.
    pub fn set_global_volume(&self, volume: f32) {
        self.core.set_global_volume(volume);
    }

    /// Глобальный «Заглушить все» (SPEC.md §7.1): `true` обнуляет звук
    /// всех источников, `false` возвращает громкость.
    pub fn set_muted(&self, muted: bool) {
        self.core.set_muted(muted);
    }

    /// Частота дискретизации устройства — декодер ресемплирует под неё
    /// (libswresample) до `push_samples`.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Число каналов устройства — декодеру для ресемплинга.
    pub fn channels(&self) -> u16 {
        self.channels
    }
}

/// Пишет в динамически типизированный буфер устройства: микширует во f32,
/// конвертирует в формат потока. Неподдерживаемый формат — тишина.
fn write_output(core: &MixerCore, data: &mut Data) {
    match data.sample_format() {
        SampleFormat::F32 => {
            let out = data.as_slice_mut::<f32>().expect("формат буфера известен");
            core.mix_into(out);
        }
        SampleFormat::I16 => {
            let out = data.as_slice_mut::<i16>().expect("формат буфера известен");
            thread_local! {
                static SCRATCH: std::cell::RefCell<Vec<f32>> =
                    const { std::cell::RefCell::new(Vec::new()) };
            }
            SCRATCH.with(|cell| {
                let mut scratch = cell.borrow_mut();
                scratch.resize(out.len(), 0.0);
                core.mix_into(&mut scratch);
                // Сэмплы уже в [-1, 1]; приведение float->int насыщается
                // (Rust >= 1.45), NaN даёт 0.
                for (dst, &s) in out.iter_mut().zip(scratch.iter()) {
                    *dst = (s * i16::MAX as f32) as i16;
                }
            });
        }
        // DSD/нецелые и нестандартные форматы не поддерживаются — тишина.
        _ => data.bytes_mut().fill(0),
    }
}

/// Хендл одного источника звука, принадлежащего стикеру-видео.
///
/// Декодер кладёт ресемплированные сэмплы через `push_samples` (короткий
/// мьютекс, не блокирует колбэк), громкость меняет через `set_volume`.
/// При drop хендла источник убирается из микшера.
pub struct AudioSource {
    id: Uuid,
    state: Arc<SourceState>,
    core: Arc<MixerCore>,
}

impl AudioSource {
    /// Кладёт порцию f32-сэмплов (уже в формате устройства — частота,
    /// каналы) в очередь источника. Неблокирующе для аудио-колбэка: тот
    /// сам решает, сколько взять. При переполнении `MAX_BUFFERED_SAMPLES`
    /// отбрасываются самые старые сэмплы.
    pub fn push_samples(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut queue = lock(&self.state.queue);
        // Переполнение: самые старые сэмплы (из очереди, затем голову
        // порции) вытесняются, чтобы очередь не выросла за ёмкость.
        let excess = queue
            .len()
            .saturating_add(samples.len())
            .saturating_sub(MAX_BUFFERED_SAMPLES);
        if excess > 0 {
            let from_queue = excess.min(queue.len());
            queue.drain(..from_queue);
        }
        let space = MAX_BUFFERED_SAMPLES.saturating_sub(queue.len());
        let chunk = &samples[samples.len().saturating_sub(space)..];
        queue.extend(chunk.iter().copied());
        drop(queue);

        self.core.on_data_pushed();
    }

    /// Громкость этого источника: `0.0..=1.0`, вне диапазона обрезается,
    /// не-конечные значения трактуются как 0.0.
    pub fn set_volume(&self, volume: f32) {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.state.volume.store(v.to_bits(), Ordering::Relaxed);
    }
}

impl Drop for AudioSource {
    fn drop(&mut self) {
        self.core.remove_source(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1e-6,
            "ожидалось {expected}, получено {actual}"
        );
    }

    #[test]
    fn mix_two_in_phase_signals_without_clipping() {
        // Сумма 0.3 + 0.4 = 0.7 в допустимых пределах: результат — точная
        // сумма, без клиппинга.
        let a: [f32; 8] = [0.3, 0.3, 0.3, 0.3, -0.3, -0.3, 0.0, 0.3];
        let b: [f32; 8] = [0.4, -0.4, 0.0, 0.1, 0.4, 0.2, -0.2, -0.3];
        let mut out = [0.0f32; 8];
        mix_frame(&mut out, &[&a, &b], &[1.0, 1.0], 1.0, false);
        for i in 0..8 {
            assert_close(out[i], a[i] + b[i]);
        }
        assert!(out.iter().all(|v| v.abs() <= 1.0));
    }

    #[test]
    fn mix_clamps_on_overflow() {
        let a = [0.8f32; 4];
        let b = [0.8f32; 4];
        let mut out = [0.0f32; 4];
        mix_frame(&mut out, &[&a, &b], &[1.0, 1.0], 1.0, false);
        assert_eq!(out, [1.0; 4], "0.8 + 0.8 клэмпится к 1.0");

        let a = [-0.8f32; 4];
        let b = [-0.8f32; 4];
        mix_frame(&mut out, &[&a, &b], &[1.0, 1.0], 1.0, false);
        assert_eq!(out, [-1.0; 4], "симметричный клэмп к -1.0");
    }

    #[test]
    fn mix_zero_volume_source_contributes_nothing() {
        let a = [0.5f32; 4];
        let b = [0.25f32; 4];
        let mut out = [0.0f32; 4];
        mix_frame(&mut out, &[&a, &b], &[0.0, 1.0], 1.0, false);
        for v in out {
            assert_close(v, 0.25);
        }
    }

    #[test]
    fn mix_muted_zeroes_everything() {
        let a = [0.8f32; 4];
        let mut out = [-1.0f32; 4];
        mix_frame(&mut out, &[&a], &[1.0], 1.0, true);
        assert_eq!(out, [0.0; 4], "mute обнуляет выход, а не оставляет прошлое");
    }

    #[test]
    fn mix_short_source_pads_silence() {
        let a = [0.5, 0.5];
        let mut out = [0.0f32; 4];
        mix_frame(&mut out, &[&a], &[1.0], 1.0, false);
        assert_eq!(out, [0.5, 0.5, 0.0, 0.0], "недостающие сэмплы — тишина");
    }

    #[test]
    fn mix_empty_sources_is_silence() {
        let mut out = [1.0f32; 4];
        mix_frame(&mut out, &[], &[], 1.0, false);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn mix_global_volume_scales_all_sources() {
        let a = [0.8f32; 4];
        let b = [0.2f32; 4];
        let mut out = [0.0f32; 4];
        mix_frame(&mut out, &[&a, &b], &[1.0, 1.0], 0.5, false);
        for v in out {
            assert_close(v, 0.5);
        }
    }

    #[test]
    fn mix_non_finite_volumes_treated_as_zero() {
        let a = [0.8f32; 4];
        let mut out = [0.0f32; 4];
        mix_frame(&mut out, &[&a], &[f32::NAN], 1.0, false);
        assert_eq!(out, [0.0; 4], "NaN громкости не отравляет выход");
        mix_frame(&mut out, &[&a], &[1.0], f32::INFINITY, false);
        assert_eq!(out, [0.0; 4], "не-конечная глобальная громкость — тишина");
    }

    fn core_with_source(id: Uuid, volume: f32) -> (Arc<MixerCore>, Arc<SourceState>) {
        let core = Arc::new(MixerCore::new());
        let state = core.register_source(id);
        state.volume.store(volume.to_bits(), Ordering::Relaxed);
        (core, state)
    }

    #[test]
    fn core_mixes_and_consumes_queued_samples() {
        let id = Uuid::new_v4();
        let (core, state) = core_with_source(id, 1.0);
        lock(&state.queue).extend([0.2f32; 4]);
        let mut out = [0.0f32; 8];
        core.mix_into(&mut out);
        assert_eq!(out[..4], [0.2; 4], "сэмплы смикшированы");
        assert_eq!(out[4..], [0.0; 4], "хвост — тишина");
        assert!(lock(&state.queue).is_empty(), "потреблённые сэмплы удалены");
    }

    #[test]
    fn core_consumes_only_what_the_callback_needs() {
        let id = Uuid::new_v4();
        let (core, state) = core_with_source(id, 1.0);
        lock(&state.queue).extend([0.5f32; 6]);
        let mut out = [0.0f32; 4];
        core.mix_into(&mut out);
        assert_eq!(out, [0.5; 4]);
        assert_eq!(
            lock(&state.queue).len(),
            2,
            "лишние сэмплы ждут следующего колбэка"
        );
    }

    #[test]
    fn core_remove_source_stops_contribution() {
        let id = Uuid::new_v4();
        let (core, state) = core_with_source(id, 1.0);
        lock(&state.queue).extend([0.9f32; 4]);
        core.remove_source(&id);
        assert!(state.dropped.load(Ordering::Relaxed));
        let mut out = [0.0f32; 4];
        core.mix_into(&mut out);
        assert_eq!(out, [0.0; 4], "удалённый источник не вносит вклад");
    }

    #[test]
    fn core_mute_zeroes_out_but_still_consumes() {
        let id = Uuid::new_v4();
        let (core, state) = core_with_source(id, 1.0);
        lock(&state.queue).extend([0.9f32; 4]);
        core.set_muted(true);
        let mut out = [-1.0f32; 4];
        core.mix_into(&mut out);
        assert_eq!(out, [0.0; 4], "mute даёт тишину");
        assert!(
            lock(&state.queue).is_empty(),
            "mute потребляет сэмплы, чтобы после unmute не звучало устаревшее"
        );
    }

    #[test]
    fn core_set_global_volume_clamps_and_ignores_nan() {
        let id = Uuid::new_v4();
        let (core, state) = core_with_source(id, 1.0);
        lock(&state.queue).extend([0.5f32; 2]);
        core.set_global_volume(2.0);
        let mut out = [0.0f32; 2];
        core.mix_into(&mut out);
        assert_close(out[0], 0.5);

        lock(&state.queue).extend([0.5f32; 2]);
        core.set_global_volume(f32::NAN);
        core.mix_into(&mut out);
        assert_eq!(out, [0.0; 2], "NaN глобальной громкости — тишина");
    }

    #[test]
    fn push_samples_drops_oldest_when_queue_overflows() {
        let id = Uuid::new_v4();
        let core = Arc::new(MixerCore::new());
        let state = core.register_source(id);
        let source = AudioSource {
            id,
            state: Arc::clone(&state),
            core: Arc::clone(&core),
        };

        // Обычная порция: очередь ниже ёмкости, ничего не теряется.
        source.push_samples(&[0.1f32; 16]);
        assert_eq!(lock(&state.queue).len(), 16);

        // Порция, превышающая ёмкость целиком: остаются только её хвост.
        let huge = vec![0.2f32; MAX_BUFFERED_SAMPLES + 64];
        source.push_samples(&huge);
        let queue = lock(&state.queue);
        assert_eq!(queue.len(), MAX_BUFFERED_SAMPLES);
        assert_eq!(queue.front(), Some(&0.2), "старые сэмплы вытеснены");
        assert_eq!(queue.back(), Some(&0.2));
    }

    #[test]
    #[ignore = "требует реальное аудио-устройство; вручную: cargo test -p rst-audio --lib -- --ignored"]
    fn smoke_plays_sine_through_device() {
        use std::f32::consts::PI;
        use std::time::{Duration, Instant};

        let mixer = AudioMixer::new().expect("устройство вывода доступно");
        let source = mixer.add_source(Uuid::new_v4());
        source.set_volume(0.5);

        // 440 Гц, подача в реальном темпе (60ms сэмплов на 60ms сна) —
        // очередь держится в стационаре, звук непрерывный, ~2.5 с.
        let rate = mixer.sample_rate();
        let chunk = (rate as f32 * 0.06) as usize;
        let mut phase = 0.0f32;
        let deadline = Instant::now() + Duration::from_millis(2500);
        while Instant::now() < deadline {
            let buf: Vec<f32> = (0..chunk)
                .map(|_| {
                    let s = (phase * 2.0 * PI).sin() * 0.5;
                    phase += 440.0 / rate as f32;
                    s
                })
                .collect();
            source.push_samples(&buf);
            std::thread::sleep(Duration::from_millis(60));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn push_between_silent_mix_and_decision_prevents_pause() {
        // Колбэк смешал пустой буфер почти на пороге тишины, в этот момент
        // пришёл push — решение колбэка, принятое после, не должно усыпить поток.
        let mut det = IdleDetector::new(1000);
        assert_eq!(det.on_render(false, 990), StreamAction::None);
        assert_eq!(det.on_data_available(), StreamAction::None);
        assert_eq!(det.on_render(false, 10), StreamAction::None);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
    }

    #[test]
    fn push_after_pause_decision_wakes_stream() {
        let mut det = IdleDetector::new(100);
        assert_eq!(det.on_render(false, 100), StreamAction::Pause);
        assert_eq!(det.on_data_available(), StreamAction::Play);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
    }

    #[test]
    fn idle_detector_starts_playing_with_zero_silence() {
        let det = IdleDetector::new(1000);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
        assert_eq!(det.silence_frames(), 0);
        assert_eq!(det.threshold_frames(), 1000);
    }

    #[test]
    fn idle_detector_hysteresis_and_pause_threshold() {
        let mut det = IdleDetector::new(1000);

        // Накопление тишины ниже порога: состояние остаётся Playing, действий нет
        assert_eq!(det.on_render(false, 300), StreamAction::None);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
        assert_eq!(det.silence_frames(), 300);

        assert_eq!(det.on_render(false, 699), StreamAction::None);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
        assert_eq!(det.silence_frames(), 999);

        // Достижение порога: переход в Paused и возврат действия Pause
        assert_eq!(det.on_render(false, 1), StreamAction::Pause);
        assert_eq!(det.state(), StreamPlaybackState::Paused);
        assert_eq!(det.silence_frames(), 1000);

        // Продолжение тишины на паузе: повторных действий нет, состояние остаётся Paused
        assert_eq!(det.on_render(false, 500), StreamAction::None);
        assert_eq!(det.state(), StreamPlaybackState::Paused);
    }

    #[test]
    fn idle_detector_resets_silence_on_active_data() {
        let mut det = IdleDetector::new(1000);
        assert_eq!(det.on_render(false, 800), StreamAction::None);
        assert_eq!(det.silence_frames(), 800);

        // Появление реального звука сбрасывает счётчик тишины
        assert_eq!(det.on_render(true, 100), StreamAction::None);
        assert_eq!(det.silence_frames(), 0);
        assert_eq!(det.state(), StreamPlaybackState::Playing);

        // После сброса нужно снова накопить полный порог для паузы
        assert_eq!(det.on_render(false, 800), StreamAction::None);
        assert_eq!(det.silence_frames(), 800);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
    }

    #[test]
    fn idle_detector_resumes_when_data_becomes_available() {
        let mut det = IdleDetector::new(500);
        // Загоняем в паузу
        assert_eq!(det.on_render(false, 500), StreamAction::Pause);
        assert_eq!(det.state(), StreamPlaybackState::Paused);

        // Поступление данных в очередь источника будит поток
        assert_eq!(det.on_data_available(), StreamAction::Play);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
        assert_eq!(det.silence_frames(), 0);

        // Повторный вызов во время Playing не шлёт лишний Play
        assert_eq!(det.on_data_available(), StreamAction::None);
        assert_eq!(det.state(), StreamPlaybackState::Playing);
    }

    #[test]
    fn core_mix_into_reports_has_data_accurately() {
        let core = Arc::new(MixerCore::new());
        let mut out = [0.0f32; 8];

        // Без источников — has_data == false
        assert!(!core.mix_into(&mut out));

        // С пустым источником — has_data == false
        let id = Uuid::new_v4();
        let state = core.register_source(id);
        assert!(!core.mix_into(&mut out));

        // С сэмплами — has_data == true
        lock(&state.queue).extend([0.5f32; 4]);
        assert!(core.mix_into(&mut out));

        // Снова пусто — has_data == false
        assert!(!core.mix_into(&mut out));

        // Mute: сэмплы всё равно потребляются (для синхронизации темпа видео), has_data == true
        core.set_muted(true);
        lock(&state.queue).extend([0.5f32; 4]);
        assert!(core.mix_into(&mut out));
        assert_eq!(out, [0.0; 8], "выход занулён из-за mute");
    }

    struct MockStreamController {
        detector: Mutex<IdleDetector>,
        is_paused: Arc<AtomicBool>,
        play_count: AtomicU32,
        pause_count: AtomicU32,
    }

    impl StreamControl for MockStreamController {
        fn on_render(&self, has_data: bool, frames: u64) {
            let mut det = lock(&self.detector);
            match det.on_render(has_data, frames) {
                StreamAction::Pause => {
                    self.is_paused.store(true, Ordering::Relaxed);
                    self.pause_count.fetch_add(1, Ordering::Relaxed);
                }
                StreamAction::Play => {
                    self.is_paused.store(false, Ordering::Relaxed);
                    self.play_count.fetch_add(1, Ordering::Relaxed);
                }
                StreamAction::None => {}
            }
        }

        fn resume(&self) {
            let mut det = lock(&self.detector);
            if det.on_data_available() == StreamAction::Play {
                self.is_paused.store(false, Ordering::Relaxed);
                self.play_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn core_with_mock_controller_pauses_and_resumes_cleanly() {
        let core = Arc::new(MixerCore::new());
        core.set_channels(2); // stereo: 8 samples = 4 frames
        let mock = Arc::new(MockStreamController {
            detector: Mutex::new(IdleDetector::new(500)), // порог 500 фреймов
            is_paused: Arc::clone(&core.is_stream_paused),
            play_count: AtomicU32::new(0),
            pause_count: AtomicU32::new(0),
        });
        core.set_controller(Arc::clone(&mock) as Arc<dyn StreamControl>);

        let mut out = [0.0f32; 200]; // 100 фреймов стерео
        // 4 прохода по 100 фреймов = 400 фреймов тишины (< 500)
        for _ in 0..4 {
            core.mix_into(&mut out);
            assert_eq!(mock.pause_count.load(Ordering::Relaxed), 0);
            assert!(!core.is_stream_paused.load(Ordering::Relaxed));
        }

        // 5-й проход: 500 фреймов тишины -> пауза!
        core.mix_into(&mut out);
        assert_eq!(mock.pause_count.load(Ordering::Relaxed), 1);
        assert!(core.is_stream_paused.load(Ordering::Relaxed));

        // Дальнейшие вызовы на паузе не спамят Pause
        core.mix_into(&mut out);
        assert_eq!(mock.pause_count.load(Ordering::Relaxed), 1);

        // Поступление данных в источник: AudioSource::push_samples будит поток
        let id = Uuid::new_v4();
        let state = core.register_source(id);
        let source = AudioSource {
            id,
            state: Arc::clone(&state),
            core: Arc::clone(&core),
        };

        source.push_samples(&[0.3f32; 16]);
        assert_eq!(mock.play_count.load(Ordering::Relaxed), 1);
        assert!(!core.is_stream_paused.load(Ordering::Relaxed));

        // Повторный push_samples во время работы не вызывает лишний play
        source.push_samples(&[0.4f32; 16]);
        assert_eq!(mock.play_count.load(Ordering::Relaxed), 1);

        // Вызов mix_into потребляет данные и сбрасывает тишину
        assert!(core.mix_into(&mut out));
        assert_eq!(lock(&mock.detector).silence_frames(), 0);
        assert_eq!(mock.pause_count.load(Ordering::Relaxed), 1);
    }
}
