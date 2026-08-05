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

    // Дублируем директивы `ffmpeg-sys-next` — безвредно при совпадении путей,
    // защищает от изменения его логики поиска.
    println!("cargo:rustc-link-search=native={}", lib.display());
    for name in FFMPEG_LIBS {
        println!("cargo:rustc-link-lib=dylib={name}");
    }
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
