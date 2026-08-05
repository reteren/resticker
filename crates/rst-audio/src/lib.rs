//! WASAPI audio mixer (via cpal) for video sticker sound: one output
//! stream per process, audio clock as the sync master.
//!
//! M5B_VIDEO_DESIGN.md, раздел 4: один общий выходной поток на весь
//! процесс. Декодеры видео кладут ресемплированные под формат устройства
//! f32-сэмплы через [`AudioSource::push_samples`]; колбэк аудио-потока
//! микширует все источники с их громкостями и глобальным mute в реальном
//! времени. Крейт платформенно-независим на уровне логики: математика
//! микширования ( [`mix_frame`]) покрыта юнит-тестами без устройства,
//! cpal-адаптер (`AudioMixer::new`) — `#[ignore]`-смоуком на реальное
//! аудио-устройство.

mod error;
mod mixer;

pub use error::AudioError;
pub use mixer::{AudioMixer, AudioSource, mix_frame};
