//! Снятие отметки текущего сеанса загрузки Windows
//! ([`rst_core::boot_session`]): по ней при старте решается, пережили ли
//! группы окон перезагрузку компьютера.

use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::core::PCWSTR;

use rst_core::boot_session::BootStamp;

/// Ключ, куда ядро пишет момент последнего выключения системы.
const WINDOWS_KEY: &str = "SYSTEM\\CurrentControlSet\\Control\\Windows";
const SHUTDOWN_VALUE: &str = "ShutdownTime";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Отметка текущего сеанса загрузки.
///
/// Не возвращает ошибку: неудача любого из двух замеров — не повод не
/// запуститься. Непрочитанная метка выключения оставляет `None`, и решение
/// принимается по одному моменту старта.
pub fn current_stamp() -> BootStamp {
    BootStamp {
        booted_at: booted_at_unix_secs(),
        shutdown_tag: shutdown_tag(),
    }
}

/// Момент старта системы: «сейчас» минус время работы.
///
/// Время работы, а не запрос момента загрузки у WMI: WMI из процесса — это
/// COM, инициализация и десятки миллисекунд на пути запуска, тогда как
/// `GetTickCount64` стоит один вызов. Сдвиг от подводки часов внутри сеанса
/// покрывает допуск в `rst_core::boot_session`.
fn booted_at_unix_secs() -> i64 {
    // SAFETY: GetTickCount64 не принимает аргументов и не может завершиться
    // ошибкой.
    let uptime_ms = unsafe { GetTickCount64() };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now - (uptime_ms / 1000) as i64
}

/// Метка последнего выключения из реестра, шестнадцатеричной строкой.
///
/// `None` — значения нет или прочитать не удалось.
fn shutdown_tag() -> Option<String> {
    let subkey = to_wide(WINDOWS_KEY);
    let mut hkey = HKEY::default();
    // SAFETY: subkey — валидная nul-terminated wide-строка; при успехе hkey
    // получает действительный ключ, который закрывается ниже.
    let ret = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_QUERY_VALUE,
            &mut hkey,
        )
    };
    if ret != ERROR_SUCCESS {
        tracing::debug!(code = ret.0, "не удалось открыть ключ сеанса загрузки");
        return None;
    }
    let value = to_wide(SHUTDOWN_VALUE);
    let mut buf = [0u8; 32];
    let mut size = buf.len() as u32;
    // SAFETY: hkey действителен; буфер и его размер согласованы, функция
    // пишет не больше `size` байт и корректирует `size` фактическим.
    let ret = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(value.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    // SAFETY: hkey открыт успешно и больше не используется.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if ret != ERROR_SUCCESS {
        tracing::debug!(code = ret.0, "не удалось прочитать метку выключения");
        return None;
    }
    let size = (size as usize).min(buf.len());
    if size == 0 {
        return None;
    }
    Some(buf[..size].iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_moment_is_in_the_past_and_stable_between_calls() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("часы до 1970")
            .as_secs() as i64;
        let first = booted_at_unix_secs();
        assert!(first <= now, "система не могла загрузиться в будущем");
        assert!(first > 0, "момент старта обязан быть вычислимым");
        // Два замера подряд обязаны совпасть: иначе сравнение с сохранённой
        // отметкой ловило бы не перезагрузку, а собственный шум.
        assert_eq!(first, booted_at_unix_secs());
    }

    #[test]
    fn the_shutdown_tag_is_stable_between_calls() {
        // Метка может отсутствовать (ключа нет, прав нет) — но если она есть,
        // она обязана быть одной и той же в пределах сеанса.
        assert_eq!(shutdown_tag(), shutdown_tag());
    }
}
