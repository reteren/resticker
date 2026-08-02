//! Автозапуск через `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! (SPEC.md, раздел 12).

use std::io;
use std::path::Path;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::PCWSTR;

use crate::error::Win32Error;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "resticker";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32_err(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// Обёртка над `HKEY`, закрывающая ключ в `Drop` (CONTRIBUTING.md, «Правила unsafe»).
struct RegKey(HKEY);

impl RegKey {
    fn open(sam: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<Self, Win32Error> {
        let subkey = to_wide(RUN_KEY);
        let mut hkey = HKEY::default();
        // SAFETY: subkey — валидная nul-terminated wide-строка, hkey получает
        // владение действительным HKEY при успехе (ERROR_SUCCESS).
        let ret = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                sam,
                &mut hkey,
            )
        };
        if ret != ERROR_SUCCESS {
            return Err(Win32Error::Registry(win32_err(ret.0)));
        }
        Ok(Self(hkey))
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: self.0 всегда действительный открытый ключ (см. `open`).
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Включить или выключить автозапуск, прописывая/удаляя значение в Run-ключе.
/// `exe_path` — путь к исполняемому файлу (обычно `std::env::current_exe()`).
pub fn set_enabled(enabled: bool, exe_path: &Path) -> Result<(), Win32Error> {
    let key = RegKey::open(KEY_SET_VALUE)?;
    let value_name = to_wide(VALUE_NAME);
    if enabled {
        let quoted = format!("\"{}\"", exe_path.display());
        let data = to_wide(&quoted);
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
        // SAFETY: key.0 действителен (открыт выше); bytes — валидный слайс,
        // построенный из уже выделенного Vec<u16>, живущего до конца вызова.
        let ret = unsafe {
            RegSetValueExW(
                key.0,
                PCWSTR(value_name.as_ptr()),
                Some(0),
                REG_SZ,
                Some(bytes),
            )
        };
        if ret != ERROR_SUCCESS {
            return Err(Win32Error::Registry(win32_err(ret.0)));
        }
    } else {
        // SAFETY: key.0 действителен.
        let ret = unsafe { RegDeleteValueW(key.0, PCWSTR(value_name.as_ptr())) };
        // Отсутствие значения — не ошибка (уже выключено).
        if ret != ERROR_SUCCESS && ret.0 != windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.0 {
            return Err(Win32Error::Registry(win32_err(ret.0)));
        }
    }
    Ok(())
}

/// Прочитать текущее состояние автозапуска напрямую из реестра
/// (источник истины для UI — не то, что записано в config.json).
pub fn is_enabled() -> Result<bool, Win32Error> {
    let key = RegKey::open(KEY_QUERY_VALUE)?;
    let value_name = to_wide(VALUE_NAME);
    let mut size: u32 = 0;
    // SAFETY: key.0 действителен; запрос без буфера (None) только читает
    // требуемый размер в `size`.
    let ret = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(value_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        )
    };
    if ret.0 == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.0 {
        return Ok(false);
    }
    if ret != ERROR_SUCCESS {
        return Err(Win32Error::Registry(win32_err(ret.0)));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::to_wide;

    #[test]
    fn to_wide_is_nul_terminated_ascii() {
        assert_eq!(
            to_wide("resticker"),
            [114, 101, 115, 116, 105, 99, 107, 101, 114, 0]
        );
    }

    #[test]
    fn to_wide_empty_is_single_nul() {
        assert_eq!(to_wide(""), [0]);
    }

    #[test]
    fn to_wide_encodes_non_ascii_as_utf16() {
        let wide = to_wide("стикер");
        assert_eq!(
            wide.len(),
            "стикер".chars().count() + 1,
            "BMP-символы — по одному u16"
        );
        assert_eq!(wide[0], 0x0441, "«с» — U+0441");
        assert_eq!(wide.last(), Some(&0));
    }
}
