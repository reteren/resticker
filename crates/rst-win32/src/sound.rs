//! Однократное воспроизведение WAV для UI-звука пина (запрос пользователя
//! 2026-08-19: «прогрывается звук при закреплении биндом», отдельная
//! громкость в настройках). Сознательно НЕ через `rst-audio::AudioMixer` —
//! тот крейт заточен под непрерывный push-поток сэмплов видео-стикеров
//! (`AudioSource::push_samples`, один WASAPI-поток на процесс) и его
//! `set_muted`/`set_global_volume` относятся к звуку стикеров, а не к этому
//! UI-отклику; пользователь явно просил отдельный, независимый регулятор.
//! `PlaySoundW(SND_MEMORY)` — простейший штатный способ Windows проиграть
//! короткий сэмпл без отдельного аудио-потока/устройства.

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

/// Встроенный сэмпл (7.9 КБ, 16-бит моно PCM 44100 Гц) — файл лежит рядом
/// с крейтом, а не где-то во внешнем пути: звук должен работать всегда,
/// без внешней зависимости от файла на диске пользователя.
const PIN_HOTKEY_WAV: &[u8] = include_bytes!("../assets/pin_hotkey.wav");

/// Играет звук закрепления окна хоткеем на громкости `volume_percent`
/// (0..=100, лишнее клампится). 0 — не звонить вовсе (не проигрывать
/// беззвучный сэмпл впустую).
pub fn play_pin_sound(volume_percent: u8) {
    let volume_percent = volume_percent.min(100);
    if volume_percent == 0 {
        return;
    }
    let Some(scaled) = scale_wav_volume(PIN_HOTKEY_WAV, volume_percent) else {
        tracing::warn!("не удалось разобрать встроенный WAV звука пина — пропущено");
        return;
    };
    // SAFETY: `scaled` — буфер этого потока, `PlaySoundW` с `SND_MEMORY`
    // копирует данные сам (не хранит указатель на них после возврата),
    // так что буфер безопасно дропнуть сразу после вызова. `SND_ASYNC` —
    // не блокировать поток хоткея на длительность сэмпла.
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(scaled.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

/// Разбирает чанки `RIFF`/`fmt `/`data` WAV-файла, масштабирует сэмплы в
/// `data` на `volume_percent` (только 16-бит PCM — формат встроенного
/// ассета, проверяется через `fmt `; что-то другое — `None`, а не
/// неправильное масштабирование наугад), возвращает полный буфер WAV
/// (заголовок как есть, заменена только `data`) — готов для
/// `PlaySoundW(SND_MEMORY)`.
fn scale_wav_volume(wav: &[u8], volume_percent: u8) -> Option<Vec<u8>> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut bits_per_sample = None;
    let mut data_range = None;
    while pos + 8 <= wav.len() {
        let chunk_id = &wav[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body_start = pos + 8;
        let body_end = body_start.checked_add(chunk_size)?;
        if body_end > wav.len() {
            break;
        }
        match chunk_id {
            b"fmt " if chunk_size >= 16 => {
                bits_per_sample = Some(u16::from_le_bytes(
                    wav[body_start + 14..body_start + 16].try_into().ok()?,
                ));
            }
            b"data" => data_range = Some(body_start..body_end),
            _ => {}
        }
        // Чанки выровнены по слову: нечётный размер — байт паддинга следом.
        pos = body_end + (chunk_size % 2);
    }
    if bits_per_sample != Some(16) {
        return None;
    }
    let data_range = data_range?;
    let mut out = wav.to_vec();
    let scale = volume_percent as f32 / 100.0;
    for sample in out[data_range].chunks_exact_mut(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]);
        let scaled = (value as f32 * scale)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let bytes = scaled.to_le_bytes();
        sample[0] = bytes[0];
        sample[1] = bytes[1];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает минимальный валидный WAV (44-байтовый заголовок, 16-бит
    /// моно PCM) из готовых сэмплов — для юнит-тестов без зависимости от
    /// встроенного ассета.
    fn build_wav(samples: &[i16]) -> Vec<u8> {
        let data_bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&88200u32.to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_bytes.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data_bytes);
        wav
    }

    fn samples_of(wav: &[u8]) -> Vec<i16> {
        // data-чанк у build_wav фиксирован в конце 44-байтового заголовка.
        wav[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn volume_100_leaves_samples_unchanged() {
        let wav = build_wav(&[100, -200, 32000, -32000]);
        let scaled = scale_wav_volume(&wav, 100).expect("валидный WAV");
        assert_eq!(samples_of(&scaled), vec![100, -200, 32000, -32000]);
    }

    #[test]
    fn volume_50_halves_samples() {
        let wav = build_wav(&[1000, -1000, 200]);
        let scaled = scale_wav_volume(&wav, 50).expect("валидный WAV");
        assert_eq!(samples_of(&scaled), vec![500, -500, 100]);
    }

    #[test]
    fn volume_0_zeroes_samples() {
        let wav = build_wav(&[1000, -1000]);
        let scaled = scale_wav_volume(&wav, 0).expect("валидный WAV");
        assert_eq!(samples_of(&scaled), vec![0, 0]);
    }

    #[test]
    fn volume_over_100_is_not_called_with_clamp_responsibility_on_caller() {
        // scale_wav_volume сам не клампит вход сверху 100 — это делает
        // play_pin_sound перед вызовом; здесь проверяем математику как есть
        // (150% реально усиливает, с клампом сэмпла по диапазону i16).
        let wav = build_wav(&[30000]);
        let scaled = scale_wav_volume(&wav, 150).expect("валидный WAV");
        assert_eq!(samples_of(&scaled), vec![i16::MAX]);
    }

    #[test]
    fn header_and_riff_size_unchanged_by_scaling() {
        let wav = build_wav(&[1, 2, 3]);
        let scaled = scale_wav_volume(&wav, 50).expect("валидный WAV");
        assert_eq!(wav.len(), scaled.len(), "тот же размер буфера");
        assert_eq!(&wav[0..44], &scaled[0..44], "заголовок не тронут");
    }

    #[test]
    fn not_riff_returns_none() {
        assert_eq!(scale_wav_volume(b"not a wav file at all", 50), None);
    }

    #[test]
    fn truncated_riff_returns_none() {
        assert_eq!(scale_wav_volume(b"RIFF\x00\x00\x00\x00WAVE", 50), None);
    }

    #[test]
    fn embedded_asset_is_valid_16bit_pcm() {
        // Встроенный ассет реально парсится этим кодом — если формат файла
        // когда-нибудь заменят на что-то другое (не 16-бит PCM), тест
        // упадёт здесь, а не тихо перестанет играть звук в рантайме.
        assert!(scale_wav_volume(PIN_HOTKEY_WAV, 100).is_some());
    }
}
