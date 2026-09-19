//! Проверка runtime-зависимостей до первого вызова delay-import thunk.
//!
//! Windows по умолчанию превращает отсутствующую delay-loaded DLL в SEH
//! исключение внутри linker helper. Для пользовательского пути это слишком
//! поздно и нечитаемо, поэтому сначала явно проверяем тот же стандартный
//! поиск DLL и возвращаем обычный `VideoError`.

use std::path::Path;
use std::ptr::null_mut;

use crate::error::VideoError;

const FFMPEG_DLLS: &[&str] = &[
    // Сначала зависимости, чтобы ошибка называла отсутствующую причину, а
    // не библиотеку-обёртку, которая не смогла загрузиться из-за неё.
    "avutil-59.dll",
    "swresample-5.dll",
    "avcodec-61.dll",
    "avformat-61.dll",
];

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(
        file_name: *const u16,
        file: *mut std::ffi::c_void,
        flags: u32,
    ) -> *mut std::ffi::c_void;
}
/// Убедиться, что все DLL, нужные первому FFmpeg-вызову, доступны системе.
/// Хэндлы намеренно не освобождаются: delay-import helper переиспользует уже
/// загруженный модуль, а держать его до завершения процесса безопаснее, чем
/// допустить выгрузку кода между проверкой и первым вызовом.
pub(crate) fn ensure_loaded(path: &Path) -> Result<(), VideoError> {
    for &name in FFMPEG_DLLS {
        let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide_name живёт до возврата из Win32-вызова; null file и
        // нулевые flags сохраняют обычную DLL search order приложения.
        let module = unsafe { LoadLibraryExW(wide_name.as_ptr(), null_mut(), 0) };
        if module.is_null() {
            let error = std::io::Error::last_os_error();
            return Err(VideoError::Open {
                path: path.to_path_buf(),
                message: format!("FFmpeg library {name} is unavailable: {error}"),
            });
        }
    }
    Ok(())
}
