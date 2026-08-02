//! Вставка изображения из буфера обмена (`Ctrl+V` в режиме редактирования,
//! SPEC.md, раздел 2.1). Приоритет форматов: `PNG` (зарегистрированный
//! формат) → `CF_DIBV5` (с альфой) → `CF_DIB` → `CF_HDROP` (пути к файлам
//! из проводника).
//!
//! Наружу — сырые байты изображения (готовый PNG/BMP-файл) либо пути;
//! материализация на диск (`%APPDATA%\resticker\pasted\<uuid>.png`) и
//! декодирование — задача rst-media и ядра.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::core::w;

use crate::error::Win32Error;

/// Расширения файлов, принимаемых из `CF_HDROP` (M2 — только изображения;
/// видео появятся в M5).
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];

/// Найденное в буфере обмена содержимое для нового стикера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardImage {
    /// Готовый PNG-файл (буферный формат «PNG» — его кладут браузеры
    /// и графические редакторы).
    Png(Vec<u8>),
    /// Готовый BMP-файл, собранный из `CF_DIBV5`/`CF_DIB` (буферный DIB не
    /// содержит заголовок `BITMAPFILEHEADER` — дописываем сами; альфа-канал
    /// DIBV5 при этом сохраняется в потоке байт).
    Bmp(Vec<u8>),
    /// Пути к файлам изображений (`CF_HDROP` — копирование файлов
    /// в проводнике). Добавляются как обычные файловые стикеры (SPEC 2.1).
    Files(Vec<PathBuf>),
}

/// Прочитать изображение из буфера обмена. `Ok(None)` — изображения в
/// буфере нет (это не ошибка). Вызывается на потоке, обрабатывающем `Ctrl+V`
/// (оверлей-поток в режиме редактирования).
pub fn read_image() -> Result<Option<ClipboardImage>, Win32Error> {
    let _clipboard = Clipboard::open()?;

    // 1. «PNG» — наивысший приоритет: единственный формат, где альфа
    // гарантированно корректна (SPEC 2.1).
    if let Some(format) = png_format() {
        if let Some(bytes) = clipboard_bytes(format)? {
            return Ok(Some(ClipboardImage::Png(bytes)));
        }
    }
    // 2–3. DIBV5, затем DIB: собираем полноценный BMP-файл.
    for format in [CF_DIBV5.0 as u32, CF_DIB.0 as u32] {
        if let Some(dib) = clipboard_bytes(format)? {
            return Ok(Some(ClipboardImage::Bmp(dib_to_bmp(&dib)?)));
        }
    }
    // 4. CF_HDROP — пути из проводника; не-изображения отфильтровываем.
    if let Some(paths) = clipboard_hdrop()? {
        if !paths.is_empty() {
            return Ok(Some(ClipboardImage::Files(paths)));
        }
    }
    Ok(None)
}

/// RAII над парой `OpenClipboard`/`CloseClipboard`: пока живо значение,
/// буфер открыт нами (и содержимое его стабильно).
struct Clipboard;

impl Clipboard {
    /// Открыть буфер обмена. Другие приложения держат его доли секунды
    /// (например, во время собственного копирования), поэтому сначала честно
    /// ждём до ~500 мс; потом — понятная ошибка [`Win32Error::ClipboardBusy`].
    fn open() -> Result<Self, Win32Error> {
        const ATTEMPTS: u32 = 25;
        for attempt in 0..ATTEMPTS {
            // SAFETY: стандартный вызов; окна-владельца нет (None) —
            // буфер ассоциируется с текущей задачей.
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Self);
            }
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        Err(Win32Error::ClipboardBusy)
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: буфер открыт нами в `open()` и закрывается ровно один раз —
        // здесь. Ошибка игнорируется: закрывать при демонтаже всё равно нечем.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}
/// ID зарегистрированного формата «PNG». Регистрация идемпотентна, ID
/// стабилен в рамках сессии — кэшируем.
fn png_format() -> Option<u32> {
    static PNG_FORMAT: OnceLock<u32> = OnceLock::new();
    // SAFETY: w!("PNG") — статичная nul-terminated wide-строка; 0 означает
    // сбой регистрации (практически недостижим) — тогда формата для нас нет.
    let id = *PNG_FORMAT.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("PNG")) });
    (id != 0).then_some(id)
}

/// Скопировать байты формата из буфера. `Ok(None)` — формата в буфере нет.
/// Вызывается только при открытом буфере (см. [`Clipboard`]).
fn clipboard_bytes(format: u32) -> Result<Option<Vec<u8>>, Win32Error> {
    // SAFETY: проверка наличия формата на открытом буфере.
    if unsafe { IsClipboardFormatAvailable(format) }.is_err() {
        return Ok(None);
    }
    // SAFETY: хэндл принадлежит буферу обмена и валиден, пока он открыт
    // нами (закрытие — в Drop Clipboard, позже этого вызова).
    let handle = match unsafe { GetClipboardData(format) } {
        Ok(h) => h,
        // Владелец мог отозвать формат между проверкой и чтением (delayed
        // rendering) — считаем это «формата нет», а не ошибкой.
        Err(_) => return Ok(None),
    };
    let hglobal = HGLOBAL(handle.0);
    // SAFETY: hglobal — валидный объект буфера; GlobalLock даёт указатель
    // на его память размером GlobalSize, живущий до GlobalUnlock ниже.
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return Err(Win32Error::ClipboardDataCorrupt("GlobalLock вернул null"));
    }
    let size = unsafe { GlobalSize(hglobal) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) }.to_vec();
    // SAFETY: тот же объект, что блокировали выше.
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Ok(Some(bytes))
}

/// Прочитать `CF_HDROP` и оставить только пути к поддерживаемым
/// изображениям. `Ok(None)` — формата в буфере нет.
fn clipboard_hdrop() -> Result<Option<Vec<PathBuf>>, Win32Error> {
    /// Специальное значение индекса — запрос количества файлов.
    const QUERY_COUNT: u32 = 0xFFFF_FFFF;

    // SAFETY: проверка наличия формата на открытом буфере.
    if unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_err() {
        return Ok(None);
    }
    // SAFETY: как в clipboard_bytes — хэндл валиден, пока буфер открыт нами.
    let handle = match unsafe { GetClipboardData(CF_HDROP.0 as u32) } {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    let hdrop = HDROP(handle.0);
    // SAFETY: hdrop валиден; QUERY_COUNT — документированный запрос размера.
    let count = unsafe { DragQueryFileW(hdrop, QUERY_COUNT, None) };
    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: запрос без буфера (None) возвращает длину в символах без
        // завершающего нуля; буфер выделяем с запасом на него.
        let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
        let path = PathBuf::from(String::from_utf16_lossy(&buf[..written]));
        if is_supported_image(&path) {
            paths.push(path);
        }
    }
    Ok(Some(paths))
}

/// Файл — поддерживаемое изображение (по расширению, регистр не важен).
pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Размер `BITMAPFILEHEADER` — буферный DIB его не содержит, дописываем сами.
const FILE_HEADER_SIZE: usize = 14;
/// `biCompression`: маски каналов следуют за 40-байтным заголовком.
const BI_BITFIELDS: u32 = 3;
/// Недокументированное расширение (Adobe): маски RGBA за 40-байтным заголовком.
const BI_ALPHABITFIELDS: u32 = 6;

/// Собрать BMP-файл из буферного DIB: дописать `BITMAPFILEHEADER` и
/// вычислить смещение пиксельных данных (заголовок + маски + палитра).
/// Поддерживаются заголовки 40 (BITMAPINFOHEADER), 108 (V4) и 124 (V5).
fn dib_to_bmp(dib: &[u8]) -> Result<Vec<u8>, Win32Error> {
    let read_u32 = |offset: usize| -> Result<u32, Win32Error> {
        dib.get(offset..offset + 4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("срез длиной 4")))
            .ok_or(Win32Error::ClipboardDataCorrupt("DIB обрезан"))
    };

    let header_size = read_u32(0)? as usize;
    if !matches!(header_size, 40 | 108 | 124) {
        return Err(Win32Error::ClipboardDataCorrupt(
            "неизвестный заголовок DIB",
        ));
    }
    if dib.len() < header_size {
        return Err(Win32Error::ClipboardDataCorrupt(
            "DIB короче своего заголовка",
        ));
    }
    // Поля bitCount/compression/clrUsed лежат по одним смещениям во всех
    // трёх заголовках (первые 40 байт у V4/V5 совпадают с INFOHEADER).
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]) as u32;
    let compression = read_u32(16)?;
    let clr_used = read_u32(32)?;

    // Маски каналов для 40-байтного заголовка лежат отдельно за ним;
    // в V4/V5 они внутри заголовка.
    let masks_size = match (header_size, compression) {
        (40, BI_BITFIELDS) => 12,
        (40, BI_ALPHABITFIELDS) => 16,
        _ => 0,
    };
    // Палитра — только у индексированных форматов (≤ 8 bpp).
    let palette_entries = if bit_count <= 8 {
        if clr_used != 0 {
            clr_used as usize
        } else {
            1usize << bit_count
        }
    } else {
        0
    };
    let palette_size = palette_entries
        .checked_mul(4)
        .ok_or(Win32Error::ClipboardDataCorrupt(
            "слишком большая палитра DIB",
        ))?;
    let meta_size = header_size
        .checked_add(masks_size)
        .and_then(|v| v.checked_add(palette_size))
        .ok_or(Win32Error::ClipboardDataCorrupt(
            "слишком большая палитра DIB",
        ))?;
    if dib.len() < meta_size {
        return Err(Win32Error::ClipboardDataCorrupt("DIB обрезан на палитре"));
    }

    let pixel_offset = (FILE_HEADER_SIZE + meta_size) as u32;
    let file_size = (FILE_HEADER_SIZE + dib.len()) as u32;
    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]); // bfReserved1 + bfReserved2
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Ok(bmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc};
    use windows::Win32::UI::Shell::DROPFILES;

    /// Буфер обмена — общий системный ресурс: интеграционные тесты
    /// сериализуются, иначе они затирали бы данные друг друга.
    static CLIPBOARD_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn le_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 байта"))
    }

    /// DIB 2×2, 24 bpp, заголовок BITMAPINFOHEADER, пиксели — мусор-маркер.
    fn test_dib_24bpp() -> Vec<u8> {
        let mut dib = vec![0u8; 40 + 16]; // заголовок + 2 строки по 8 байт
        dib[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize
        dib[4..8].copy_from_slice(&2i32.to_le_bytes()); // biWidth
        dib[8..12].copy_from_slice(&2i32.to_le_bytes()); // biHeight
        dib[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib[14..16].copy_from_slice(&24u16.to_le_bytes()); // biBitCount
        for (i, b) in dib[40..].iter_mut().enumerate() {
            *b = i as u8;
        }
        dib
    }

    /// DIB 2×2, 32 bpp, заголовок BITMAPV5HEADER (CF_DIBV5).
    fn test_dib_v5() -> Vec<u8> {
        let mut dib = vec![0u8; 124 + 16]; // заголовок V5 + пиксели
        dib[0..4].copy_from_slice(&124u32.to_le_bytes()); // bV5Size
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&(-2i32).to_le_bytes()); // top-down
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[124..].fill(0xAB);
        dib
    }

    #[test]
    fn bmp_header_for_24bpp_dib() {
        let dib = test_dib_24bpp();
        let bmp = dib_to_bmp(&dib).expect("DIB валиден");
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(le_u32(&bmp, 2), (14 + dib.len()) as u32); // bfSize
        assert_eq!(le_u32(&bmp, 10), 54); // bfOffBits: 14 + 40, без палитры
        assert_eq!(&bmp[14..], &dib[..]); // DIB перенесён без изменений
    }

    #[test]
    fn bmp_offsets_for_masks_palette_v5() {
        // 32 bpp + BI_BITFIELDS: 3 маски (12 байт) за 40-байтным заголовком.
        let mut dib = vec![0u8; 40 + 12 + 16];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
        let bmp = dib_to_bmp(&dib).expect("DIB валиден");
        assert_eq!(le_u32(&bmp, 10), 14 + 40 + 12);

        // 8 bpp без biClrUsed: палитра по умолчанию 256 × 4 байта.
        let mut dib = vec![0u8; 40 + 1024 + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&8u16.to_le_bytes());
        let bmp = dib_to_bmp(&dib).expect("DIB валиден");
        assert_eq!(le_u32(&bmp, 10), (14 + 40 + 1024) as u32);

        // 8 bpp с явным biClrUsed = 16.
        let mut dib = vec![0u8; 40 + 64 + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&8u16.to_le_bytes());
        dib[32..36].copy_from_slice(&16u32.to_le_bytes());
        let bmp = dib_to_bmp(&dib).expect("DIB валиден");
        assert_eq!(le_u32(&bmp, 10), (14 + 40 + 64) as u32);

        // V5-заголовок: маски внутри, дополнительного смещения нет.
        let bmp = dib_to_bmp(&test_dib_v5()).expect("DIB валиден");
        assert_eq!(le_u32(&bmp, 10), (14 + 124) as u32);
    }

    #[test]
    fn dib_to_bmp_rejects_garbage() {
        // Меньше 4 байт — даже biSize не прочитать.
        assert!(dib_to_bmp(&[1, 2]).is_err());
        // Неизвестный заголовок (12 — OS/2 CORE, не поддерживаем).
        assert!(dib_to_bmp(&12u32.to_le_bytes()).is_err());
        // Заголовок заявлен, но сам DIB короче.
        assert!(dib_to_bmp(&40u32.to_le_bytes()).is_err());
        // 8 bpp с палитрой 256, но данных нет даже на палитру.
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&8u16.to_le_bytes());
        assert!(dib_to_bmp(&dib).is_err());
    }

    #[test]
    fn supported_image_extensions() {
        for ok in ["a.png", "b.JPG", "c.jpeg", "d.WebP", "e.bmp", "f.gif"] {
            assert!(is_supported_image(Path::new(ok)), "{ok} должен приниматься");
        }
        for bad in ["a.txt", "b.exe", "c", ".png", "d.png.txt"] {
            assert!(
                !is_supported_image(Path::new(bad)),
                "{bad} не должен приниматься"
            );
        }
    }

    /// Положить набор форматов в буфер (содержимое предварительно очищается).
    fn set_clipboard_formats(items: &[(u32, Vec<u8>)]) {
        let _guard = Clipboard::open().expect("открыть буфер обмена");
        // SAFETY: буфер открыт нами (guard выше жив до конца функции).
        unsafe { EmptyClipboard().expect("очистить буфер обмена") };
        for (format, bytes) in items {
            // SAFETY: выделяем перемещаемый блок, копируем байты и передаём
            // владение системе через SetClipboardData — после успеха блок
            // освобождать нельзя, владелец теперь буфер обмена.
            unsafe {
                let hg = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).expect("GlobalAlloc");
                let ptr = GlobalLock(hg);
                assert!(!ptr.is_null());
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
                let _ = GlobalUnlock(hg);
                SetClipboardData(*format, Some(HANDLE(hg.0))).expect("SetClipboardData");
            }
        }
    }

    fn clear_clipboard() {
        let _guard = Clipboard::open().expect("открыть буфер обмена");
        // SAFETY: буфер открыт нами.
        unsafe { EmptyClipboard().expect("очистить буфер обмена") };
    }

    /// Собрать содержимое CF_HDROP: заголовок DROPFILES + wide-строки
    /// с двойным нулём в конце.
    fn dropfiles_bytes(paths: &[&str]) -> Vec<u8> {
        let header_size = size_of::<DROPFILES>() as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header_size.to_le_bytes()); // pFiles
        bytes.extend_from_slice(&[0; 8]); // pt
        bytes.extend_from_slice(&[0; 4]); // fNC = FALSE
        bytes.extend_from_slice(&1i32.to_le_bytes()); // fWide = TRUE
        for path in paths {
            for u in path.encode_utf16() {
                bytes.extend_from_slice(&u.to_le_bytes());
            }
            bytes.extend_from_slice(&[0, 0]);
        }
        bytes.extend_from_slice(&[0, 0]); // финальный двойной нуль
        bytes
    }

    #[test]
    fn empty_clipboard_yields_none() {
        let _lock = CLIPBOARD_TEST_LOCK.lock().expect("мьютекс тестов");
        clear_clipboard();
        assert_eq!(read_image().expect("чтение буфера"), None);
    }

    #[test]
    fn png_roundtrip() {
        let _lock = CLIPBOARD_TEST_LOCK.lock().expect("мьютекс тестов");
        let png = b"\x89PNG\r\n\x1a\nfake-png-bytes".to_vec();
        let format = png_format().expect("формат PNG должен зарегистрироваться");
        set_clipboard_formats(&[(format, png.clone())]);

        let result = read_image().expect("чтение буфера");
        assert_eq!(result, Some(ClipboardImage::Png(png)));
        clear_clipboard();
    }

    #[test]
    fn dibv5_roundtrip_becomes_bmp() {
        let _lock = CLIPBOARD_TEST_LOCK.lock().expect("мьютекс тестов");
        let dib = test_dib_v5();
        set_clipboard_formats(&[(CF_DIBV5.0 as u32, dib.clone())]);

        let result = read_image().expect("чтение буфера");
        let Some(ClipboardImage::Bmp(bmp)) = result else {
            panic!("ожидался Bmp, получено: {result:?}");
        };
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(le_u32(&bmp, 10), (14 + 124) as u32);
        assert_eq!(&bmp[14..], &dib[..]);
        clear_clipboard();
    }

    #[test]
    fn png_wins_over_dib() {
        let _lock = CLIPBOARD_TEST_LOCK.lock().expect("мьютекс тестов");
        let png = b"\x89PNG\r\n\x1a\npriority".to_vec();
        let format = png_format().expect("формат PNG должен зарегистрироваться");
        // Приложения кладут несколько форматов сразу; приоритет — PNG (SPEC 2.1).
        set_clipboard_formats(&[(CF_DIB.0 as u32, test_dib_24bpp()), (format, png.clone())]);

        let result = read_image().expect("чтение буфера");
        assert_eq!(result, Some(ClipboardImage::Png(png)));
        clear_clipboard();
    }

    #[test]
    fn hdrop_filters_to_images() {
        let _lock = CLIPBOARD_TEST_LOCK.lock().expect("мьютекс тестов");
        let bytes = dropfiles_bytes(&["C:\\img\\cat.png", "C:\\docs\\note.txt", "D:\\dog.JPG"]);
        set_clipboard_formats(&[(CF_HDROP.0 as u32, bytes)]);

        let result = read_image().expect("чтение буфера");
        assert_eq!(
            result,
            Some(ClipboardImage::Files(vec![
                PathBuf::from("C:\\img\\cat.png"),
                PathBuf::from("D:\\dog.JPG")
            ]))
        );
        clear_clipboard();
    }

    #[test]
    fn hdrop_without_images_yields_none() {
        let _lock = CLIPBOARD_TEST_LOCK.lock().expect("мьютекс тестов");
        let bytes = dropfiles_bytes(&["C:\\docs\\note.txt"]);
        set_clipboard_formats(&[(CF_HDROP.0 as u32, bytes)]);

        assert_eq!(read_image().expect("чтение буфера"), None);
        clear_clipboard();
    }
}
