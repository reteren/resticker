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
use std::process::Command;

/// Библиотеки, линкуемые против пресобранного FFmpeg (сборка LGPL-only:
/// swscale/avdevice/avfilter отключены и не линкуются вовсе).
const FFMPEG_LIBS: &[&str] = &["avformat", "avcodec", "avutil", "swresample"];

/// Обычные импорты FFmpeg заставляют Windows маппить четыре больших DLL ещё
/// до того, как в приложении появился первый видеослайд. Для MSVC оставляем
/// import library, но просим linker заменить вызов на delay-import thunk:
/// delayimp.lib сам подгрузит DLL на первом вызове функции.
const FFMPEG_DELAY_LOAD_DLLS: &[&str] = &[
    "avformat-61.dll",
    "avcodec-61.dll",
    "avutil-59.dll",
    "swresample-5.dll",
];

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

    let delay_lib_dir = prepare_delay_import_libs(&dir);

    // Дублируем директивы `ffmpeg-sys-next` — безвредно при совпадении путей,
    // защищает от изменения его логики поиска.
    println!("cargo:rustc-link-search=native={}", lib.display());
    if let Some(ref dir) = delay_lib_dir {
        // `ffmpeg-sys-next` обычно получает MinGW import libraries (*.lib),
        // а link.exe умеет создать delay-import descriptor только из
        // специальных библиотек. Этот путь ставим последним, чтобы имена
        // avformat.lib/... разрешились в сгенерированные delay libraries.
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    for name in FFMPEG_LIBS {
        println!("cargo:rustc-link-lib=dylib={name}");
    }

    emit_delay_load_args(delay_lib_dir.as_deref());
}

/// `rst-video` — библиотечный крейт, поэтому эти аргументы должны попасть в
/// его test-бинарии; для `resticker.exe` те же аргументы дублируются в
/// `crates/resticker/build.rs`. На GNU/не-Windows таргетах ничего не добавляем:
/// это MSVC-механизм, а не универсальный Rust linker flag.
fn emit_delay_load_args(delay_lib_dir: Option<&Path>) {
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    if let Some(dir) = delay_lib_dir {
        // Native `-l` directives from ffmpeg-sys-next carry their original
        // search path as dependency metadata. Explicit /LIBPATH is needed to
        // put generated delay libraries ahead of that path in link.exe.
        println!("cargo:rustc-link-arg=/LIBPATH:{}", dir.display());
    }
    for dll in FFMPEG_DELAY_LOAD_DLLS {
        println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-lib=dylib=delayimp");
}

/// Создать MSVC-совместимые import libraries из тех же `.def`, которые
/// поставляет LGPL FFmpeg. Обычные MinGW `.lib` пригодны для стандартной
/// линковки, но link.exe предупреждает LNK4199 при `/DELAYLOAD`: в них нет
/// ожидаемого MSVC import descriptor. Пересборка через `lib.exe` оставляет
/// экспортный набор тем же, но даёт link.exe корректную основу для delay-load.
fn prepare_delay_import_libs(ffmpeg_dir: &Path) -> Option<PathBuf> {
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return None;
    }

    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR не задан")).join("ffmpeg-delay-libs");
    std::fs::create_dir_all(&out_dir).expect("не удалось создать каталог delay import libraries");

    let msvc_lib = find_msvc_lib();
    for dll in FFMPEG_DELAY_LOAD_DLLS {
        let stem = dll.strip_suffix(".dll").expect("DLL name has suffix");
        let def = ffmpeg_dir.join("lib").join(format!("{stem}.def"));
        let link_name = stem.split_once('-').map_or(stem, |(name, _)| name);
        let output = out_dir.join(format!("{link_name}.lib"));
        assert!(
            def.is_file(),
            "не найден .def для delay import library: {def:?}"
        );
        let status = Command::new(&msvc_lib)
            .arg("/nologo")
            .arg(format!("/def:{}", def.display()))
            .arg(format!("/name:{dll}"))
            .arg("/machine:x64")
            .arg(format!("/out:{}", output.display()))
            .status()
            .unwrap_or_else(|e| panic!("не удалось запустить MSVC lib.exe для {dll}: {e}"));
        assert!(
            status.success(),
            "MSVC lib.exe не создал import library для {dll}: {status}"
        );
    }
    Some(out_dir)
}

/// Найти `lib.exe` без требования запускать Cargo из Developer Command Prompt:
/// IDE и CI часто передают только обычный PATH, хотя сам MSVC linker уже
/// доступен Rust через настройки target. Сначала доверяем окружению, затем
/// ищем рядом с VCToolsInstallDir и в стандартной установке Build Tools.
fn find_msvc_lib() -> PathBuf {
    if let Some(path) = env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join("lib.exe"))
            .find(|path| path.is_file())
    }) {
        return path;
    }
    if let Some(path) = env::var_os("VCToolsInstallDir")
        .map(PathBuf::from)
        .map(|dir| dir.join("bin").join("Hostx64").join("x64").join("lib.exe"))
        .filter(|path| path.is_file())
    {
        return path;
    }

    let root =
        Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC");
    if let Ok(versions) = std::fs::read_dir(root) {
        let mut candidates = versions
            .filter_map(Result::ok)
            .map(|entry| {
                entry
                    .path()
                    .join("bin")
                    .join("Hostx64")
                    .join("x64")
                    .join("lib.exe")
            })
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        candidates.sort();
        if let Some(path) = candidates.pop() {
            return path;
        }
    }
    panic!(
        "не найден MSVC lib.exe: запустите сборку из Developer Command Prompt или задайте VCToolsInstallDir"
    );
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
    let path = dir
        .join("include")
        .join("libavcodec")
        .join("version_major.h");
    println!("cargo:rerun-if-changed={}", path.display());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Some(major) = text.lines().find_map(|line| {
        let rest = line
            .trim()
            .strip_prefix("#define LIBAVCODEC_VERSION_MAJOR")?;
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
