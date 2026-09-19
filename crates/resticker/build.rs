use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Такие аргументы нужны именно финальному `resticker.exe`: build.rs
/// библиотечного `rst-video` не передаёт link flags зависимому бинарю.
const FFMPEG_DELAY_LOAD_DLLS: &[&str] = &[
    "avformat-61.dll",
    "avcodec-61.dll",
    "avutil-59.dll",
    "swresample-5.dll",
];

fn main() {
    emit_delay_load_args();

    // Должно выполниться до tauri_build::try_build: он сам читает
    // bundle.resources из tauri.conf.json и падает, если glob ничего не
    // находит — resources/ffmpeg-dlls/ должна существовать заранее.
    copy_ffmpeg_dlls();

    // PROCESS_PER_MONITOR_DPI_AWARE_V2 (ADR-010) via an explicit manifest,
    // see resources/resticker.exe.manifest.
    let attrs = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("resources/resticker.exe.manifest")),
    );
    tauri_build::try_build(attrs).expect("tauri_build failed");
}

/// До первого вызова FFmpeg Windows не маппит DLL, поэтому обычный старт
/// приложения не оплачивает память видеопайплайна. delayimp.lib нужен для
/// обработчика thunk; на остальных toolchain'ах этот MSVC-флаг неприменим.
fn emit_delay_load_args() {
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for dll in FFMPEG_DELAY_LOAD_DLLS {
        println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-lib=dylib=delayimp");
}

/// FFmpeg собран тулчейном MinGW-w64 gcc (docs/M5B_VIDEO_DESIGN.md §1), и
/// его DLL сами линкуются против рантайма этого тулчейна — не только
/// системных DLL. Подтверждено через `objdump -p avformat-61.dll` и т. д.:
/// avformat/avcodec тянут `libiconv-2.dll`+`zlib1.dll`, avutil тянет
/// `libwinpthread-1.dll`. Без них рядом с exe получаем ту же ошибку "не
/// обнаружен *.dll", что и без самих FFmpeg-библиотек.
const MINGW_RUNTIME_DLLS: &[&str] = &["libiconv-2.dll", "zlib1.dll", "libwinpthread-1.dll"];

/// resticker линкуется против FFmpeg динамически (rst-video, LGPL shared
/// build) — без avformat/avutil/swresample/avcodec *.dll рядом с exe
/// приложение падает на старте с "не обнаружен avformat-61.dll" и т. п.
/// `cargo build`/`cargo run` этого не делают автоматически, поэтому копируем
/// сами: один раз в `target/<profile>/` (для запуска exe напрямую) и один раз
/// в `resources/ffmpeg-dlls/` (откуда их подхватывает `bundle.resources` в
/// tauri.conf.json при сборке NSIS-инсталлятора).
fn copy_ffmpeg_dlls() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=LIBCLANG_PATH");
    println!("cargo:rerun-if-env-changed=MINGW_RUNTIME_DIR");

    let ffmpeg_dir = env::var_os("FFMPEG_DIR")
        .map(PathBuf::from)
        .expect("FFMPEG_DIR не задана — см. crates/rst-video/build.rs за инструкцией");
    let bin_dir = ffmpeg_dir.join("bin");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let bundled_dlls_dir = manifest_dir.join("resources").join("ffmpeg-dlls");
    fs::create_dir_all(&bundled_dlls_dir).expect("не удалось создать resources/ffmpeg-dlls");

    let target_dir = target_dir_from_out_dir(&PathBuf::from(env::var_os("OUT_DIR").unwrap()));
    // Копия в target/<profile>/ нужна только для release (реальный запуск
    // собранного exe) — в target/debug/ она ломает сборку САМОГО проекта:
    // Cargo добавляет target/debug/ в PATH build-скриптов ПЕРЕД clang64/bin,
    // и скопированные сюда же libwinpthread-1.dll/zlib1.dll из mingw64/bin
    // (другой ABI/сборка MSYS2, не совместимая с clang64) начинают
    // резолвиться раньше настоящих зависимостей `libclang.dll`, ломая
    // `bindgen` в rst-video с "Unable to find libclang" — найдено на
    // практике (тесты `cargo test -p resticker` стабильно падали именно на
    // этом после того, как эта копия появилась). Для debug профиля этого
    // не делаем; `resources/ffmpeg-dlls/` (для NSIS) копируется всегда —
    // она не участвует в PATH сборки.
    let is_release = env::var("PROFILE").as_deref() == Ok("release");
    let mut dests = vec![bundled_dlls_dir.as_path()];
    if is_release {
        dests.push(target_dir.as_path());
    }

    let mut copied = 0;
    for entry in
        fs::read_dir(&bin_dir).unwrap_or_else(|e| panic!("не удалось прочитать {bin_dir:?}: {e}"))
    {
        let entry = entry.expect("ошибка чтения записи каталога");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("dll") {
            continue;
        }
        copy_to_all(&path, &dests);
        copied += 1;
    }
    assert!(
        copied > 0,
        "в {bin_dir:?} не найдено ни одной *.dll — сборка FFmpeg неполная"
    );

    for name in MINGW_RUNTIME_DLLS {
        let src = find_mingw_runtime_dll(name, &bin_dir).unwrap_or_else(|| {
            panic!(
                "не найден {name} (транзитивная зависимость FFmpeg-DLL от тулчейна \
                     MinGW-w64) — искали в {bin_dir:?}, $MINGW_RUNTIME_DIR и рядом с \
                     $LIBCLANG_PATH. Задайте MINGW_RUNTIME_DIR=путь\\к\\msys64\\mingw64\\bin"
            )
        });
        copy_to_all(&src, &dests);
    }
}

fn copy_to_all(src: &Path, dests: &[&Path]) {
    let file_name = src.file_name().unwrap();
    for dest_dir in dests {
        fs::copy(src, dest_dir.join(file_name))
            .unwrap_or_else(|e| panic!("не удалось скопировать {src:?} в {dest_dir:?}: {e}"));
    }
}

/// Ищет `name` по порядку: рядом с самим FFmpeg (на случай если будущая
/// сборка станет вендорить рантайм-зависимости сама), в явном
/// `MINGW_RUNTIME_DIR`, и в `mingw64/bin` рядом с `clang64/bin` из
/// `LIBCLANG_PATH` (оба ставятся вместе тем же MSYS2, см.
/// docs/M5B_VIDEO_DESIGN.md §1 и README «Сборка»).
fn find_mingw_runtime_dll(name: &str, ffmpeg_bin_dir: &Path) -> Option<PathBuf> {
    let mut candidates = vec![ffmpeg_bin_dir.to_path_buf()];
    if let Some(dir) = env::var_os("MINGW_RUNTIME_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(libclang) = env::var_os("LIBCLANG_PATH") {
        // .../msys64/clang64/bin -> .../msys64/mingw64/bin
        if let Some(msys_root) = Path::new(&libclang).ancestors().nth(2) {
            candidates.push(msys_root.join("mingw64").join("bin"));
        }
    }
    candidates
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// `OUT_DIR` — это `target/<profile>/build/<pkg>-<hash>/out`; поднимаемся на
/// три уровня, чтобы получить `target/<profile>/` (куда cargo кладёт итоговый
/// exe).
fn target_dir_from_out_dir(out_dir: &Path) -> PathBuf {
    out_dir
        .ancestors()
        .nth(3)
        .expect("неожиданная структура OUT_DIR")
        .to_path_buf()
}
