//! Ошибки аудио-микшера.

/// Ошибка открытия выходного аудио-потока или работы с ним.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// У хоста нет устройства вывода по умолчанию (например, нет звуковой
    /// карты или она отключена) — стикеры-видео остаются без звука, это
    /// не фатальная ошибка для всего процесса.
    #[error("no default output device")]
    NoOutputDevice,
    /// Ошибка cpal: конфигурация устройства, создание или запуск потока.
    #[error("audio device error: {0}")]
    Cpal(#[from] cpal::Error),
    /// Устройство не поддерживает ни один из форматов микшера (f32/i16) —
    /// например, только F64 или I32.
    #[error("device sample format {0:?} is not supported by the mixer (only f32 and i16)")]
    UnsupportedSampleFormat(cpal::SampleFormat),
}
