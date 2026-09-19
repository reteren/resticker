//! Разрешение системного таймера на время воспроизведения.
//!
//! Декодер выдаёт кадр не раньше его `pts` — то есть спит до нужного
//! момента (`decoder::pace_to`). По умолчанию Windows округляет ЛЮБОЕ
//! ожидание с таймаутом (`Sleep`, `WaitForSingleObject`, а значит и
//! `Receiver::recv_timeout`, на котором построена пауза декодера) до
//! системного разрешения таймера — исторически это ~15.6 мс.
//!
//! Для видео это фатально: кадр 60-кадрового ролика живёт 16.7 мс, и
//! ожидание «ещё 16.7 мс» может проснуться через 31 мс. Кадр выходит
//! поздно, следующий — сразу за ним, и зритель видит рывок. Ровно на это
//! жаловался пользователь (2026-08-22, «видео в хорошем качестве иногда
//! пролагивает»): декодер при этом успевает по среднему темпу, но выдаёт
//! кадры неровно, а замер показывал разрывы по реальному времени до 55 мс
//! при периоде кадров 16.7 мс.
//!
//! `timeBeginPeriod(1)` поднимает разрешение до миллисекунды. Начиная с
//! Windows 10 2004 запрос действует ТОЛЬКО на вызвавший процесс, поэтому
//! это не «глобальная настройка системы», а обычная плата за плавное
//! видео — так делает всякий проигрыватель. Разрешение поднимается ровно
//! на время жизни декодер-потоков и опускается, когда закрылся последний:
//! в покое программа не должна держать систему в режиме частых прерываний.

use std::sync::{Mutex, MutexGuard};

use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

/// Запрошенное разрешение, миллисекунды. Единица — минимум, который Windows
/// гарантированно принимает; дробить дальше нечего.
const PERIOD_MS: u32 = 1;

/// Сколько живых держателей сейчас (файлов может играть несколько).
///
/// Под мьютексом, а не атомиком: счётчик и системный вызов обязаны меняться
/// вместе. С атомиком закрывающийся декодер мог уменьшить счётчик до нуля,
/// открывающийся — успеть позвать `timeBeginPeriod`, и только потом первый
/// позвал бы `timeEndPeriod`, сбросив разрешение из-под играющего видео.
/// Теперь, когда разрешение берётся на каждом Play и отдаётся на каждой
/// паузе, такие встречи стали обычным делом.
static HOLDERS: Mutex<usize> = Mutex::new(0);

fn holders() -> MutexGuard<'static, usize> {
    HOLDERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Держатель повышенного разрешения таймера: пока жив хотя бы один,
/// ожидания в процессе просыпаются с точностью до миллисекунды.
#[derive(Debug)]
pub(crate) struct TimerResolution {
    /// Запрос действительно приняли — только тогда его надо снимать.
    active: bool,
}

impl TimerResolution {
    /// Количество активных держателей повышенного разрешения таймера.
    #[allow(dead_code)]
    pub(crate) fn active_holders() -> usize {
        *holders()
    }

    /// Поднять разрешение (или присоединиться к уже поднятому).
    pub(crate) fn acquire() -> Self {
        let mut count = holders();
        if *count == 0 {
            // SAFETY: timeBeginPeriod — потокобезопасный запрос к системе,
            // парный вызов `timeEndPeriod` делается в `Drop`.
            let code = unsafe { timeBeginPeriod(PERIOD_MS) };
            if code != 0 {
                // Система отказала (экзотика): работаем как раньше, просто
                // с грубыми ожиданиями — это хуже по плавности, но не
                // ошибка. Счётчик не трогаем, чтобы не «снять» чужой запрос.
                tracing::warn!(code, "не удалось поднять разрешение таймера");
                return Self { active: false };
            }
        }
        *count += 1;
        Self { active: true }
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut count = holders();
        *count = count.saturating_sub(1);
        if *count == 0 {
            // SAFETY: парный вызов к принятому `timeBeginPeriod`.
            unsafe {
                let _ = timeEndPeriod(PERIOD_MS);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_resolution_increments_and_decrements_holders() {
        let before = TimerResolution::active_holders();
        let t1 = TimerResolution::acquire();
        assert_eq!(TimerResolution::active_holders(), before + 1);
        let t2 = TimerResolution::acquire();
        assert_eq!(TimerResolution::active_holders(), before + 2);
        drop(t2);
        assert_eq!(TimerResolution::active_holders(), before + 1);
        drop(t1);
        assert_eq!(TimerResolution::active_holders(), before);
    }
}
