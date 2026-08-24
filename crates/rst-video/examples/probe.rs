//! Диагностический пробник: открыть видеофайл(ы) тем же путём, что и
//! приложение, и напечатать, что получилось.
//!
//! Зачем отдельный бинарь: симптом «видео не добавляется» и симптом «видео
//! стоит картинкой» в приложении выглядят по-разному, а причина у них может
//! быть одна (репорт пользователя 2026-08-22 — оба давал отказ инициализации
//! ресемплера звука). Проверять это через UI дорого: надо запустить
//! приложение, добавить файл руками и идти читать журнал. Здесь то же самое
//! делается по списку файлов сразу и печатается в терминал.
//!
//! Запуск (DLL FFmpeg должны быть в PATH — они лежат рядом с собранным
//! приложением, `target/release`):
//!
//! ```text
//! $env:PATH = "C:\resticker\target\release;" + $env:PATH
//! cargo run --release -p rst-video --example probe -- C:\path\video.mp4 ...
//! ```
//!
//! Каждый файл: открытие, размеры, длительность, ожидание первого кадра и
//! проверка, что ВРЕМЯ ИДЁТ — второй кадр с бо́льшим `pts`. Именно это
//! отличает «играет» от «показывает одну картинку».
//!
//! Режим `--bench` вместо этого измеряет, УСПЕВАЕТ ЛИ декодер за реальным
//! временем: играет файл несколько секунд и считает, сколько кадров реально
//! дошло против того, сколько их в эти секунды записано. Это ответ на
//! «видео в хорошем качестве иногда лагает» — программный декод 1440p60
//! может просто не укладываться в бюджет кадра.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rst_video::VideoSource;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2DARRAY;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_SHADER_RESOURCE_VIEW_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_ARRAY_SRV, D3D11_TEXTURE2D_DESC,
    D3D11CreateDevice, ID3D11Device, ID3D11ShaderResourceView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
};

/// Сколько ждать кадр, прежде чем считать, что его не будет.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Длительность замера в режиме `--bench`.
const BENCH_TIME: Duration = Duration::from_secs(6);

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let bench = args.iter().any(|a| a == "--bench");
    let hw = args.iter().any(|a| a == "--hw");
    args.retain(|a| a != "--bench" && a != "--hw");
    // Устройство под аппаратный декод создаётся так же, как это делает
    // рендер приложения: одно на процесс, BGRA-совместимое.
    let device = hw.then(create_device).flatten();
    if hw && device.is_none() {
        println!("не удалось создать D3D11-устройство — аппаратный декод не проверить");
    }
    let files: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    if files.is_empty() {
        eprintln!("укажите один или несколько видеофайлов (флаг --bench — замер темпа)");
        std::process::exit(2);
    }

    let mut failed = 0usize;
    for path in &files {
        println!("\n=== {}", path.display());
        // Тот же вызов, что делает координатор при добавлении стикера:
        // формат цели — реальный формат устройства вывода (48 кГц/стерео —
        // типовой на этой машине).
        let opened = match device.as_ref() {
            Some(d) => VideoSource::open_with_audio_target_hw(path, 48_000, 2, d),
            None => VideoSource::open_with_audio_target(path, 48_000, 2),
        };
        let source = match opened {
            Ok(s) => s,
            Err(e) => {
                println!("НЕ ОТКРЫЛСЯ: {e}");
                failed += 1;
                continue;
            }
        };
        let (w, h) = source.dimensions();
        println!(
            "открыт: {w}x{h}, длительность {:?}, декод {}",
            source.duration(),
            if source.hw_accel() {
                "АППАРАТНЫЙ"
            } else {
                "программный"
            }
        );
        source.play();
        if let Some(d) = device.as_ref() {
            probe_hw_srv(&source, d);
        }
        if bench {
            if !bench_playback(&source, hw) {
                failed += 1;
            }
            continue;
        }
        match wait_frame(&source) {
            Some(first) => {
                println!("первый кадр: pts {:?}", first);
                // Второй кадр с бо́льшим pts — доказательство, что поток
                // реально идёт вперёд, а не крутится на месте.
                match wait_advance(&source, first) {
                    Some(next) => println!("время идёт: следующий pts {next:?}"),
                    None => {
                        println!("КАДРЫ НЕ ИДУТ ВПЕРЁД — видео выглядит картинкой");
                        failed += 1;
                    }
                }
            }
            None => {
                println!("НИ ОДНОГО КАДРА за {FRAME_TIMEOUT:?}");
                failed += 1;
            }
        }
    }
    println!("\nитог: {} из {} файлов с проблемами", failed, files.len());
    if failed > 0 {
        std::process::exit(1);
    }
}

/// Проиграть [`BENCH_TIME`] и сравнить темп кадров с записанным в файле.
///
/// Ключевая величина — не «сколько кадров в секунду выдал декодер», а
/// **сколько времени видео проиграно за реальную секунду**: декодер держит
/// темп по PTS и при отставании просто не спит. Если за 6 секунд ушло 4
/// секунды видео — треть кадров зритель не увидел, и выглядит это как
/// рывки.
///
/// Возвращает `false`, если темп заметно отстаёт от реального времени.
fn bench_playback(source: &VideoSource, hw: bool) -> bool {
    // Первый кадр — точка отсчёта: до него декодер ещё разгоняется.
    let Some(first) = wait_any_frame(source, hw) else {
        println!("НИ ОДНОГО КАДРА за {FRAME_TIMEOUT:?}");
        return false;
    };
    let start = Instant::now();
    let mut frames = 0usize;
    let mut last_pts = first;
    let mut max_gap = Duration::ZERO;
    let mut prev_arrival = start;
    let mut max_stall = Duration::ZERO;
    while start.elapsed() < BENCH_TIME {
        let mut got = false;
        // В аппаратном режиме программная очередь пуста по контракту —
        // кадры приходят текстурами.
        if hw {
            while let Some(frame) = source.try_recv_hw_frame() {
                got = true;
                frames += 1;
                max_gap = max_gap.max(frame.pts.saturating_sub(last_pts));
                last_pts = frame.pts;
            }
        } else {
            while let Some(frame) = source.try_recv_frame() {
                got = true;
                frames += 1;
                max_gap = max_gap.max(frame.pts.saturating_sub(last_pts));
                last_pts = frame.pts;
            }
        }
        if got {
            let now = Instant::now();
            max_stall = max_stall.max(now - prev_arrival);
            prev_arrival = now;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let elapsed = start.elapsed();
    let played = last_pts.saturating_sub(first);
    let ratio = played.as_secs_f64() / elapsed.as_secs_f64();
    let fps = frames as f64 / elapsed.as_secs_f64();
    println!(
        "темп: {:.0} кадр/с, проиграно {:.2} с видео за {:.2} с реального времени ({:.0}% скорости)",
        fps,
        played.as_secs_f64(),
        elapsed.as_secs_f64(),
        ratio * 100.0
    );
    println!(
        "худший разрыв между кадрами: {:.0} мс по видео, {:.0} мс по реальному времени",
        max_gap.as_secs_f64() * 1000.0,
        max_stall.as_secs_f64() * 1000.0
    );
    // 90% — уже заметная на глаз потеря плавности; ниже этого считаем, что
    // декодер не справляется.
    if ratio < 0.9 {
        println!("ДЕКОДЕР НЕ УСПЕВАЕТ за реальным временем");
        return false;
    }
    true
}

/// Проверить, можно ли сделать SRV на текстуре декодера — ровно то, что
/// делает рендер (`Nv12VideoTextures::from_decoder_texture`). Печатает
/// описание текстуры и результат по каждой плоскости: зелёный кадр в
/// приложении означал именно отказ этого шага (репорт 2026-08-22).
fn probe_hw_srv(source: &VideoSource, device: &ID3D11Device) {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let frame = loop {
        if let Some(f) = source.try_recv_hw_frame() {
            break Some(f);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let Some(frame) = frame else {
        println!("аппаратных кадров нет — SRV проверить не на чем");
        return;
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: живая текстура декодера, out-параметр.
    unsafe { frame.texture.GetDesc(&mut desc) };
    println!(
        "текстура декодера: {}x{}, формат {:?}, ArraySize {}, MipLevels {}, Usage {:?}, Bind 0x{:x}, Misc 0x{:x}, Sample {}x{}",
        desc.Width,
        desc.Height,
        desc.Format.0,
        desc.ArraySize,
        desc.MipLevels,
        desc.Usage.0,
        desc.BindFlags,
        desc.MiscFlags,
        desc.SampleDesc.Count,
        desc.SampleDesc.Quality,
    );
    for (name, format, array_size, first) in [
        ("Y  весь массив", DXGI_FORMAT_R8_UNORM, desc.ArraySize, 0),
        ("UV весь массив", DXGI_FORMAT_R8G8_UNORM, desc.ArraySize, 0),
        ("Y  один слой", DXGI_FORMAT_R8_UNORM, 1, frame.array_index),
        ("UV один слой", DXGI_FORMAT_R8G8_UNORM, 1, frame.array_index),
    ] {
        let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    FirstArraySlice: first,
                    ArraySize: array_size,
                },
            },
        };
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: описание валидно, текстура и устройство живы.
        let hr = unsafe {
            device.CreateShaderResourceView(&frame.texture, Some(&srv_desc), Some(&mut srv))
        };
        match hr {
            Ok(()) => println!("  SRV {name}: ОК"),
            Err(e) => println!("  SRV {name}: ОШИБКА {e}"),
        }
    }
    let _ = DXGI_FORMAT(0);
}

/// D3D11-устройство для аппаратного декода (аналог устройства рендера).
fn create_device() -> Option<ID3D11Device> {
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: out-параметр живёт до конца вызова; остальные аргументы —
    // константы. Ошибку превращаем в None: пробник должен уметь работать и
    // без аппаратного пути.
    let hr = unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    };
    hr.ok().and(device)
}

/// Дождаться первого кадра любого режима.
fn wait_any_frame(source: &VideoSource, hw: bool) -> Option<Duration> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    while Instant::now() < deadline {
        if hw {
            if let Some(f) = source.try_recv_hw_frame() {
                return Some(f.pts);
            }
        } else if let Some(f) = source.try_recv_frame() {
            return Some(f.pts);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

/// Дождаться первого кадра (или сдаться по таймауту).
fn wait_frame(source: &VideoSource) -> Option<Duration> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(frame) = source.try_recv_frame() {
            return Some(frame.pts);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Дождаться кадра позже `after` — признак живого воспроизведения.
fn wait_advance(source: &VideoSource, after: Duration) -> Option<Duration> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(frame) = source.try_recv_frame() {
            if frame.pts > after {
                return Some(frame.pts);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}
