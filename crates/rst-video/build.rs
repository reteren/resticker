//! Сборка `rst-video`: линковка против пресобранного FFmpeg.
//!
//! FFmpeg собирается из исходников отдельно (LGPL-only, MinGW-w64 gcc) и
//! ставится в `W:/ffmpeg_build/install` (docs/M5B_VIDEO_DESIGN.md §1). Биндинги
//! генерирует `ffmpeg-sys-next` (bindgen); его `build.rs` ищет пресобранный
//! FFmpeg по переменной окружения **`FFMPEG_DIR`** (папка с `lib/`, `include/`,
//! `bin/`) — этот крейт поддерживает её нативно, поэтому отдельного
//! конфигурирования здесь не требуется. Этот `build.rs` лишь проверяет, что
//! окружение корректно, и дублирует линковочные директивы (на случай, если
//! логика поиска у `ffmpeg-sys-next` изменится в будущем).

use std::env;
use std::path::{Path, PathBuf};

/// Библиотеки, линкуемые против пресобранного FFmpeg (сборка LGPL-only:
/// swscale/avdevice/avfilter отключены и не линкуются вовсе).
const FFMPEG_LIBS: &[&str] = &["avformat", "avcodec", "avutil", "swresample"];

/// Мажорная версия libavcodec, под которую сгенерированы биндинги
/// (`ffmpeg-sys-next` 7.1.x = FFmpeg 7.1 = libavcodec 61).
///
/// Проверять её обязательно: FFmpeg меняет РАСКЛАДКУ публичных структур
/// между мажорными версиями, а `bindgen` разбирает те заголовки, что нашёл.
/// Собрав биндинги под одну версию и подложив рядом с exe библиотеки другой,
/// получаешь программу, которая запускается и почти работает: поля в начале
/// структур совпадают, а те, что дальше, читаются по чужим смещениям.
/// Именно так выглядел репорт 2026-08-22 — «видео стоит картинкой и половина
/// файлов не добавляется»: `AVFrame::ch_layout` читался мимо, ресемплер
/// звука отвечал EINVAL, и декодер бесконечно перезапускал файл. Час на
/// диагностику вместо одной понятной ошибки сборки.
const EXPECTED_AVCODEC_MAJOR: u32 = 61;

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    let dir = ffmpeg_dir();
    let lib = dir.join("lib");
    let include = dir.join("include");
    for required in [&lib, &include] {
        if !required.is_dir() {
            panic!(
                "FFMPEG_DIR={dir:?} не содержит {required:?} — проверьте путь установки FFmpeg \
                 (ожидается структура lib/, include/, bin/ от ./configure --prefix)"
            );
        }
    }
    for name in FFMPEG_LIBS {
        if !lib.join(format!("{name}.lib")).exists()
            && !lib.join(format!("lib{name}.dll.a")).exists()
        {
            panic!(
                "FFMPEG_DIR={dir:?}: не найдена импортная библиотека {name} в {lib:?} — \
                 сборка FFmpeg неполная"
            );
        }
    }

    check_version(&dir);

    // Дублируем директивы `ffmpeg-sys-next` — безвредно при совпадении путей,
    // защищает от изменения его логики поиска.
    println!("cargo:rustc-link-search=native={}", lib.display());
    for name in FFMPEG_LIBS {
        println!("cargo:rustc-link-lib=dylib={name}");
    }
}

/// Проверить, что заголовки в `FFMPEG_DIR` — той же мажорной версии, под
/// которую написан вендоренный `ffmpeg-sys-next` (см.
/// [`EXPECTED_AVCODEC_MAJOR`]). Несовпадение — ошибка сборки, а не
/// предупреждение: собранная программа была бы внешне рабочей и неверной.
///
/// Заголовок читается текстом, без запуска компилятора: `version_major.h`
/// содержит ровно одну строку `#define LIBAVCODEC_VERSION_MAJOR <n>`.
/// Прочитать не удалось — не мешаем сборке: у чужой раскладки установки
/// файл может лежать иначе, а ложный отказ хуже пропущенной проверки.
fn check_version(dir: &Path) {
    let path = dir.join("include").join("libavcodec").join("version_major.h");
    println!("cargo:rerun-if-changed={}", path.display());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Some(major) = text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("#define LIBAVCODEC_VERSION_MAJOR")?;
        rest.trim().parse::<u32>().ok()
    }) else {
        return;
    };
    assert!(
        major == EXPECTED_AVCODEC_MAJOR,
        "FFMPEG_DIR={dir:?} — это FFmpeg с libavcodec {major}, а биндинги (vendor/ffmpeg-sys-next 7.1.x) написаны под libavcodec {EXPECTED_AVCODEC_MAJOR} (FFmpeg 7.1). Смешивать нельзя: раскладка структур между мажорными версиями разная — программа собралась бы и молча читала поля по чужим смещениям (репорт 2026-08-22: видео переставало играть). Укажите установку FFmpeg 7.1 или обновите vendor/ffmpeg-sys-next под новую версию."
    );
}

/// Папка установки FFmpeg: `FFMPEG_DIR` обязателен (его же читает build.rs
/// `ffmpeg-sys-next` для bindgen include-путей — без него биндинги не
/// сгенерируются вовсе, поэтому ошибка здесь фатальна и понятна).
fn ffmpeg_dir() -> PathBuf {
    match env::var_os("FFMPEG_DIR") {
        Some(dir) => Path::new(&dir).to_path_buf(),
        None => panic!(
            "переменная окружения FFMPEG_DIR не задана: rst-video линкуется против \
             пресобранного FFmpeg (W:/ffmpeg_build/install, docs/M5B_VIDEO_DESIGN.md §1). \
             Перед сборкой выполните:\n  set FFMPEG_DIR=W:/ffmpeg_build/install\n\
             (подробности: crates/rst-video/README.md)"
        ),
    }
}
