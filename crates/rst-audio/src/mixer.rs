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

/// Чистая логика микшера: очередь источников, глобальная громкость/mute.
/// Не знает про cpal — тестируется юнитами напрямую.
struct MixerCore {
    sources: Mutex<HashMap<Uuid, Arc<SourceState>>>,
    /// Биты f32 глобальной громкости (0.0..=1.0).
    global_volume: AtomicU32,
    muted: AtomicBool,
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
    fn mix_into(&self, out: &mut [f32]) {
        let muted = self.muted.load(Ordering::Relaxed);
        let global = f32::from_bits(self.global_volume.load(Ordering::Relaxed));
        let global = if muted || !global.is_finite() {
            0.0
        } else {
            global.clamp(0.0, 1.0)
        };
        out.fill(0.0);
        let sources = lock(&self.sources);
        if sources.is_empty() {
            return;
        }
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
            } // Потребляем всегда, даже при mute/нулевой громкости: молчание
            // тоже «проигрывается», иначе после unmute зазвучали бы
            // устаревшие сэмплы вразнобой с видео.
            queue.drain(..n);
        }
        for v in out.iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
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
/// играет тишину, пока источников нет. Источники создаются `add_source`
/// (id берётся у координатора — тот же `Uuid`, что у стикера) и живут,
/// пока жив `AudioSource`-хендл или пока не вызван `remove_source`.
pub struct AudioMixer {
    core: Arc<MixerCore>,
    /// Удерживается только ради Drop: закрытие потока останавливает звук.
    _stream: Stream,
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
        Ok(Self {
            sample_rate: stream_config.sample_rate,
            channels: stream_config.channels,
            core,
            _stream: stream,
            _device: device,
        })
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
}
