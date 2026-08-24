//! Материализация изображения из буфера обмена в файл `pasted/<uuid>.png`
//! (SPEC.md, раздел 2.1: «Вставка из буфера, материализованная
//! в pasted/<uuid>.png»).
//!
//! **Размещение — `rst-media`, а не `rst-core`:** контракт принимает
//! `rst_win32::clipboard::ClipboardImage`, а по «Правилу зависимостей»
//! (CONTRIBUTING.md) `rst-core` не знает про Windows — всё платформенное
//! живёт в `rst-win32`. Декодирование изображений — зона `rst-media`
//! (CONTRIBUTING.md, таблица ответственности крейтов: «Загрузка
//! изображений»); `rst-win32/src/clipboard.rs` также прямо отсылает
//! материализацию на диск сюда.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use image::ImageFormat;
use rst_win32::clipboard::ClipboardImage;
use uuid::Uuid;

/// Ошибка материализации изображения из буфера обмена.
#[derive(Debug, thiserror::Error)]
pub enum PasteError {
    /// Ошибка ввода-вывода при создании каталога или записи файла.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Изображение не удалось декодировать или перекодировать.
    #[error("could not process the image: {0}")]
    Image(#[from] image::ImageError),
    /// Вариант `ClipboardImage::Files` — это не изображение, а пути к файлам.
    #[error("the ClipboardImage::Files variant is not materialized into PNG")]
    UnsupportedFiles,
}

/// Материализовать изображение из буфера обмена в
/// `<target_dir>/pasted/<uuid>.png` и вернуть путь к нему.
///
/// - `Png`: байты валидируются декодированием и записываются как есть
///   (точное сохранение альфы и метаданных).
/// - `Bmp`: декодируется и перекодируется в PNG.
/// - `Files`: ошибка — это не изображение (SPEC 2.1 обрабатывает пути
///   отдельно, как файловые стикеры).
///
/// Каталог `pasted/` создаётся при необходимости; имя файла — свежий UUID.
/// Запись атомарная (tmp + rename), как и в `config::save` (CONFIG.md,
/// «Атомарность обязательна»).
pub fn materialize(image: &ClipboardImage, target_dir: &Path) -> Result<PathBuf, PasteError> {
    let pasted_dir = target_dir.join("pasted");
    fs::create_dir_all(&pasted_dir)?;

    let path = pasted_dir.join(format!("{}.png", Uuid::new_v4()));
    let png = match image {
        ClipboardImage::Png(bytes) => {
            // Валидация: битый PNG должен отсекаться, а не записываться.
            image::load_from_memory_with_format(bytes, ImageFormat::Png)?;
            bytes.clone()
        }
        ClipboardImage::Bmp(bytes) => {
            let decoded = image::load_from_memory_with_format(bytes, ImageFormat::Bmp)?;
            encode_png(&decoded)?
        }
        ClipboardImage::Files(_) => return Err(PasteError::UnsupportedFiles),
    };
    write_png_atomic(&path, &png)?;
    Ok(path)
}

/// Перекодировать изображение в байты PNG.
fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>, PasteError> {
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)?;
    Ok(out)
}

/// Записать PNG-файл атомарно: tmp-файл в той же папке → flush + fsync →
/// rename. Имя файла — свежий UUID, поэтому целевой файл не существует
/// и rename не перезаписывает ничего.
fn write_png_atomic(path: &Path, bytes: &[u8]) -> Result<(), PasteError> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};
    use std::io::Cursor;

    /// Маленький RGBA-образец 2×2 с известными пикселями, закодированный в PNG.
    fn png_fixture() -> (RgbaImage, Vec<u8>) {
        let mut img = RgbaImage::new(2, 2);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, Rgba([0, 255, 0, 128]));
        img.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, Rgba([255, 255, 0, 64]));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .expect("PNG-кодирование фикстуры");
        (img, buf)
    }

    /// Маленький RGB-образец 2×2 с известными пикселями, закодированный в BMP.
    fn bmp_fixture() -> (RgbImage, Vec<u8>) {
        let mut img = RgbImage::new(2, 2);
        img.put_pixel(0, 0, Rgb([10, 20, 30]));
        img.put_pixel(1, 0, Rgb([40, 50, 60]));
        img.put_pixel(0, 1, Rgb([70, 80, 90]));
        img.put_pixel(1, 1, Rgb([100, 110, 120]));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Bmp)
            .expect("BMP-кодирование фикстуры");
        (img, buf)
    }

    /// Запись — PNG под pasted/ с UUID в имени.
    fn assert_png_location(path: &Path, target_dir: &Path) {
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(
            path.parent().map(Path::to_path_buf),
            Some(target_dir.join("pasted"))
        );
        assert_eq!(
            path.file_stem().and_then(|s| s.to_str()).map(str::len),
            Some(36),
            "имя файла — канонический UUID (36 символов)"
        );
    }

    #[test]
    fn png_materializes_roundtrip() {
        let (expected, png) = png_fixture();
        let dir = tempfile::tempdir().expect("временный каталог");
        let path =
            materialize(&ClipboardImage::Png(png.clone()), dir.path()).expect("материализация");
        assert_png_location(&path, dir.path());

        let written = fs::read(&path).expect("чтение записанного файла");
        assert_eq!(written, png, "PNG записывается как есть");

        let decoded = image::load_from_memory(&written)
            .expect("декодирование записанного файла")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), expected.dimensions());
        assert_eq!(decoded.as_raw(), expected.as_raw());
    }

    #[test]
    fn bmp_materializes_as_png_roundtrip() {
        let (expected, bmp) = bmp_fixture();
        let dir = tempfile::tempdir().expect("временный каталог");
        let path = materialize(&ClipboardImage::Bmp(bmp), dir.path()).expect("материализация");
        assert_png_location(&path, dir.path());

        let written = fs::read(&path).expect("чтение записанного файла");
        let decoded = image::load_from_memory(&written)
            .expect("декодирование PNG")
            .to_rgb8();
        assert_eq!(decoded.dimensions(), expected.dimensions());
        assert_eq!(decoded.as_raw(), expected.as_raw(), "BMP → PNG без потерь");
    }

    #[test]
    fn creates_pasted_dir_if_missing() {
        let (_, png) = png_fixture();
        let base = tempfile::tempdir().expect("временный каталог");
        let target = base.path().join("deep").join("nested");
        assert!(!target.exists());
        let path = materialize(&ClipboardImage::Png(png), &target).expect("материализация");
        assert!(path.exists());
        assert!(target.join("pasted").is_dir());
    }

    #[test]
    fn each_call_writes_fresh_file() {
        let (_, png) = png_fixture();
        let dir = tempfile::tempdir().expect("временный каталог");
        let first = materialize(&ClipboardImage::Png(png.clone()), dir.path()).expect("1");
        let second = materialize(&ClipboardImage::Png(png), dir.path()).expect("2");
        assert_ne!(first, second, "новый UUID на каждую вставку");
        assert_eq!(
            dir.path()
                .join("pasted")
                .read_dir()
                .expect("список")
                .count(),
            2
        );
    }

    #[test]
    fn invalid_png_is_rejected_and_not_written() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let garbage = b"\x89PNG\r\n\x1a\nnot-a-real-png".to_vec();
        let result = materialize(&ClipboardImage::Png(garbage), dir.path());
        assert!(matches!(result, Err(PasteError::Image(_))), "{result:?}");
        assert_eq!(
            dir.path()
                .join("pasted")
                .read_dir()
                .expect("список")
                .count(),
            0,
            "битый PNG не создаёт файл"
        );
    }

    #[test]
    fn files_variant_is_unsupported() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let files = ClipboardImage::Files(vec![PathBuf::from("C:\\img\\cat.png")]);
        let result = materialize(&files, dir.path());
        assert!(
            matches!(result, Err(PasteError::UnsupportedFiles)),
            "{result:?}"
        );
        assert_eq!(
            dir.path()
                .join("pasted")
                .read_dir()
                .expect("список")
                .count(),
            0
        );
    }
}
