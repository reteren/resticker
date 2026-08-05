//! Системный диалог выбора файла (M2, панель у курсора: кнопка «Загрузить
//! файл» — `docs/M2_WIRING_PLAN.md`, раздел 12). Обёртка над `IFileOpenDialog`
//! (COM, `Common Item Dialog`), с фильтром по расширениям изображений и,
//! начиная с M5b, видео.
//!
//! Каждый вызов сам инициализирует и деинициализирует COM на вызывающем
//! потоке (`CoInitializeEx`/`CoUninitialize`) — диалог модален и живёт ровно
//! на время вызова, отдельного состояния между вызовами нет.

use std::path::PathBuf;

use rst_core::model::VIDEO_EXTENSIONS;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::core::{GUID, HRESULT, PCWSTR};

use crate::error::Win32Error;

/// CLSID `FileOpenDialog` (`{DC1C5A9C-E88A-4dde-A5A1-60F82A20AEF7}`) —
/// в этой версии `windows-rs` не генерируется как константа (нет фичи
/// `implement`), поэтому задана вручную по значению из `shobjidl_core.h`.
const CLSID_FILE_OPEN_DIALOG: GUID = GUID::from_u128(0xDC1C5A9C_E88A_4DDE_A5A1_60F82A20AEF7);

/// `HRESULT` при отмене диалога пользователем
/// (`HRESULT_FROM_WIN32(ERROR_CANCELLED)`) — не ошибка, а `Ok(None)`.
const ERROR_CANCELLED_HRESULT: HRESULT = HRESULT(0x800704C7_u32 as i32);

/// Имя и маска фильтра «изображения» — расширения, которые умеет
/// декодировать `rst-media` (см. `rst_win32::clipboard::IMAGE_EXTENSIONS`
/// для того же списка на стороне буфера обмена).
const IMAGE_FILTER_NAME: &str = "Images";
const IMAGE_FILTER_SPEC: &str = "*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp";

/// Имя фильтра «видео» (M5b) — маска строится из общего с `add_sticker`
/// списка [`VIDEO_EXTENSIONS`], чтобы диалог и определение `MediaType` не
/// разошлись.
const VIDEO_FILTER_NAME: &str = "Videos";

fn video_filter_spec() -> String {
    VIDEO_EXTENSIONS
        .iter()
        .map(|ext| format!("*.{ext}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// Показать системный диалог выбора файла-изображения или видео
/// (`owner_hwnd` — окно-владелец; `HWND(0)`/`HWND::default()`, если владельца
/// нет). `Ok(None)` — пользователь отменил выбор, это не ошибка. Диалог
/// открывается на фильтре «Images» (индекс 1) — видео выбирается
/// переключением фильтра в самом диалоге, тем же способом, что и раньше для
/// любого не первого по списку расширения.
pub fn pick_media_file(owner_hwnd: HWND) -> Result<Option<PathBuf>, Win32Error> {
    // SAFETY: инициализация COM на вызывающем потоке для длительности одного
    // модального диалога; деинициализация — в конце этой же функции, на том
    // же потоке, независимо от исхода (см. `result` ниже).
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
    let result = show_dialog(owner_hwnd);
    // SAFETY: парная `CoUninitialize` для успешной `CoInitializeEx` выше —
    // на том же потоке, после того как диалог и все его COM-объекты уже
    // отпущены (они локальны для `show_dialog` и падают из области видимости
    // до этого вызова).
    unsafe { CoUninitialize() };
    result
}

fn show_dialog(owner_hwnd: HWND) -> Result<Option<PathBuf>, Win32Error> {
    // SAFETY: `CLSID_FILE_OPEN_DIALOG` — валидный CLSID общего системного
    // диалога; COM уже инициализирован вызывающей `pick_media_file`.
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&CLSID_FILE_OPEN_DIALOG, None, CLSCTX_INPROC_SERVER) }?;

    let image_filter_name: Vec<u16> = IMAGE_FILTER_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let image_filter_spec: Vec<u16> = IMAGE_FILTER_SPEC
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let video_filter_name: Vec<u16> = VIDEO_FILTER_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let video_filter_spec_owned = video_filter_spec();
    let video_filter_spec: Vec<u16> = video_filter_spec_owned
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let filters = [
        COMDLG_FILTERSPEC {
            pszName: PCWSTR(image_filter_name.as_ptr()),
            pszSpec: PCWSTR(image_filter_spec.as_ptr()),
        },
        COMDLG_FILTERSPEC {
            pszName: PCWSTR(video_filter_name.as_ptr()),
            pszSpec: PCWSTR(video_filter_spec.as_ptr()),
        },
    ];
    // SAFETY: `filters` содержит указатели в `image_filter_name`/
    // `image_filter_spec`/`video_filter_name`/`video_filter_spec`, все живут
    // до конца этой функции — дольше, чем нужен вызов.
    unsafe { dialog.SetFileTypes(&filters) }?;
    // SAFETY: индексация фильтров у `IFileDialog` с единицы (не с нуля) —
    // 1 открывает диалог на фильтре «Images» по умолчанию.
    unsafe { dialog.SetFileTypeIndex(1) }?;

    let owner = if owner_hwnd.is_invalid() {
        None
    } else {
        Some(owner_hwnd)
    };
    // SAFETY: модальный вызов — блокирует поток до закрытия диалога;
    // owner (если есть) должен быть валиден на это время — обеспечивает
    // вызывающая сторона (диалог открывается по клику в оверлей-потоке,
    // окно которого живо всё это время).
    if let Err(e) = unsafe { dialog.Show(owner) } {
        if e.code() == ERROR_CANCELLED_HRESULT {
            return Ok(None);
        }
        return Err(e.into());
    }

    // SAFETY: `Show` вернул успех — результат гарантированно доступен.
    let item = unsafe { dialog.GetResult() }?;
    // SAFETY: `item` — валидный `IShellItem` из `GetResult`.
    let path_pwstr = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }?;
    // SAFETY: `path_pwstr` — валидный нуль-терминированный буфер, выделенный
    // COM (`CoTaskMemAlloc`); освобождаем ниже через `CoTaskMemFree`.
    let path = unsafe { path_pwstr.to_string() };
    // SAFETY: `path_pwstr.0` — валидный указатель, выделенный COM именно под
    // `CoTaskMemFree` (документированный контракт `IShellItem::GetDisplayName`).
    unsafe { CoTaskMemFree(Some(path_pwstr.0.cast())) };

    match path {
        Ok(path) => Ok(Some(PathBuf::from(path))),
        Err(_) => Err(Win32Error::FileDialogPathInvalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_u128` и `from_values` — два независимых способа собрать один и
    /// тот же CLSID; совпадение подтверждает, что литерал `from_u128`
    /// действительно кодирует `{DC1C5A9C-E88A-4dde-A5A1-60F82A20AEF7}`
    /// побайтово так же, как канонические поля `Data1..Data4`.
    #[test]
    fn clsid_matches_canonical_fields() {
        let expected = GUID::from_values(
            0xDC1C5A9C,
            0xE88A,
            0x4DDE,
            [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
        );
        assert_eq!(CLSID_FILE_OPEN_DIALOG, expected);
    }

    #[test]
    fn error_cancelled_hresult_matches_win32_error_1223() {
        // HRESULT_FROM_WIN32(1223) = (1223 & 0xFFFF) | (FACILITY_WIN32 << 16) | 0x80000000.
        let win32_error_cancelled: u32 = 1223;
        let expected = 0x8000_0000u32 | (7u32 << 16) | (win32_error_cancelled & 0xFFFF);
        assert_eq!(ERROR_CANCELLED_HRESULT.0 as u32, expected);
    }

    #[test]
    fn image_filter_spec_covers_supported_extensions() {
        for ext in ["png", "jpg", "jpeg", "bmp", "gif", "webp"] {
            assert!(
                IMAGE_FILTER_SPEC.contains(ext),
                "фильтр не содержит расширение {ext}"
            );
        }
    }

    #[test]
    fn video_filter_spec_covers_video_extensions() {
        let spec = video_filter_spec();
        for ext in VIDEO_EXTENSIONS {
            assert!(
                spec.contains(&format!("*.{ext}")),
                "фильтр не содержит расширение {ext}"
            );
        }
    }

    #[test]
    fn filter_strings_are_null_terminated_when_encoded() {
        // То же преобразование, что и в `show_dialog`, — конечный элемент
        // должен быть 0 (PCWSTR читает до первого нуля).
        let encoded: Vec<u16> = IMAGE_FILTER_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        assert_eq!(*encoded.last().unwrap(), 0);
        assert!(encoded[..encoded.len() - 1].iter().all(|&c| c != 0));
    }

    #[test]
    #[ignore = "открывает реальный системный диалог; запуск вручную: cargo test -p rst-win32 file_dialog -- --ignored"]
    fn pick_media_file_manual_smoke() {
        // Ручная проверка: диалог должен открыться с фильтром «Images» (и
        // доступным переключением на «Videos») и вернуть выбранный путь либо
        // `None` при отмене.
        let result = pick_media_file(HWND::default());
        assert!(result.is_ok(), "{result:?}");
    }
}
