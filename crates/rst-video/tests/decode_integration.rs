//! Интеграционный тест реального декодирования: крошечный H.264 MP4 с
//! AAC-звуком (tests/fixtures/bbb.mp4, 640×360, ~575 КБ).
//!
//! Требует: DLL FFmpeg рядом с тестовым бинарником или в PATH
//! (avcodec-61.dll, avformat-61.dll, avutil-59.dll, swresample-5.dll из
//! W:/ffmpeg_build/install/bin — см. README.md) и FFMPEG_DIR при сборке.
//! Без файла-фикстуры тест падает с понятным сообщением, а не молча
//! проходит: эндошный декод без реального файла не проверить.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rst_video::{VideoError, VideoSource};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb.mp4")
}

/// Дождаться первого кадра (декодер стартует в своём потоке) с дедлайном.
fn wait_first_frame(source: &VideoSource, timeout: Duration) -> rst_video::DecodedVideoFrame {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(frame) = source.try_recv_frame() {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "первый кадр не пришёл за {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn opens_h264_mp4_and_decodes_frames() {
    let source = VideoSource::open(&fixture()).expect("файл-фикстура открывается");

    let (w, h) = source.dimensions();
    assert_eq!((w, h), (854, 480), "размеры из первого кадра");

    let frame = wait_first_frame(&source, Duration::from_secs(15));
    assert_eq!(frame.width, 854);
    assert_eq!(frame.height, 480);
    // YUV420P: Y = w*h, U/V = ceil(w/2)*ceil(h/2).
    assert_eq!(frame.y.len(), (854 * 480) as usize);
    assert_eq!(frame.u.len(), (427 * 240) as usize);
    assert_eq!(frame.v.len(), (427 * 240) as usize);
    // PTS неотрицателен, кадр в пределах разумного начала потока.
    assert!(
        frame.pts <= Duration::from_secs(2),
        "первый кадр — начало потока: {:?}",
        frame.pts
    );
    // В кадре есть хоть какая-то яркость (не пустышка).
    assert!(frame.y.iter().any(|&b| b != 0), "Y-плоскость не пустая");

    // Ещё пара кадров — декодер идёт.
    for _ in 0..3 {
        assert!(wait_first_frame(&source, Duration::from_secs(15)).pts >= Duration::ZERO);
    }
}

#[test]
fn audio_decodes_to_target_format() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");
    assert!(source.has_audio(), "фикстура Sintel содержит AAC-дорожку");
    // Фикстура играет ~30 с; звук появляется почти сразу.
    let deadline = Instant::now() + Duration::from_secs(15);
    let samples = loop {
        if let Some(audio) = source.try_recv_audio_samples() {
            break audio;
        }
        assert!(
            Instant::now() < deadline,
            "звуковые сэмплы не пришли за 15 с"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!samples.samples.is_empty(), "порция не пустая");
    assert_eq!(
        samples.samples.len() % rst_video::AUDIO_TARGET_CHANNELS,
        0,
        "interleaved стерео: чётное число сэмплов"
    );
    // Значения в разумных пределах для f32 PCM.
    assert!(
        samples.samples.iter().all(|&s| (-1.5..=1.5).contains(&s)),
        "f32-сэмплы в допустимом диапазоне"
    );
}

#[test]
fn pause_play_and_seek_work() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");

    // Сначала дождаться первого кадра: пауза, пришедшая до первой выдачи,
    // корректно останавливает декодер до кадров — это поведение паузы,
    // а не повод ждать кадры.
    let _ = wait_first_frame(&source, Duration::from_secs(15));

    source.pause();
    assert!(source.is_paused());
    // Пауза применяется с задержкой ≤ ~100 мс (текущий кадр доезжает);
    // ждём, пока декодер встанет, затем опустошаем очередь.
    std::thread::sleep(Duration::from_millis(300));
    while source.try_recv_frame().is_some() {}
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        source.try_recv_frame().is_none(),
        "на паузе декодер не наполняет очередь"
    );

    source.play();
    assert!(!source.is_paused());
    let frame = wait_first_frame(&source, Duration::from_secs(15));
    assert!(frame.pts >= Duration::ZERO);

    // Перемотка на середину и назад: PTS после seek больше стартового.
    let mid = source.duration().unwrap_or(Duration::from_secs(30)) / 2;
    source.seek(mid).expect("seek на середину не падает");
    source.clear_audio_queue();
    // Выбросить кадры, закэшированные до перемотки (координатор отсекает
    // их по PTS в красрав — здесь просто дожидаемся кадра из новой позиции).
    while source.try_recv_frame().is_some() {}
    let deadline = Instant::now() + Duration::from_secs(15);
    let frame = loop {
        if let Some(f) = source.try_recv_frame() {
            if f.pts >= mid.saturating_sub(Duration::from_secs(2)) {
                break f;
            }
        }
        assert!(Instant::now() < deadline, "после seek кадры не пошли");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        frame.pts >= mid.saturating_sub(Duration::from_secs(2)),
        "после seek PTS близок к цели: {:?} (цель {:?})",
        frame.pts,
        mid
    );
    let _ = frame;
}

#[test]
fn volume_clamps_and_round_trips() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");
    source.set_volume(0.5);
    assert_eq!(source.volume(), 0.5);
    source.set_volume(3.0);
    assert_eq!(source.volume(), 1.0, "громкость клэмпнута к 1.0");
    source.set_volume(-1.0);
    assert_eq!(source.volume(), 0.0, "громкость клэмпнута к 0.0");
}

#[test]
fn open_failures_are_clean_errors() {
    // Несуществующий файл.
    let missing = fixture().with_extension("nope.mp4");
    let err = VideoSource::open(&missing).expect_err("битый файл — ошибка");
    assert!(matches!(err, VideoError::Open { .. }), "{err}");

    // Невидеофайл (текст) — тоже чистая ошибка, не паника.
    let text = std::env::temp_dir().join("rst_video_not_a_video.txt");
    std::fs::write(&text, b"hello, not a video").unwrap();
    let err = VideoSource::open(&text).expect_err("невидеофайл — ошибка");
    assert!(matches!(err, VideoError::Open { .. }), "{err}");
    let _ = std::fs::remove_file(&text);
}

#[test]
fn drop_stops_decoder_thread() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");
    let _ = wait_first_frame(&source, Duration::from_secs(15));
    // Drop: Shutdown + join без зависания (если поток не останавливается,
    // тест зависнет — рантайм-таймаута нет, но join гарантируется кодом).
    drop(source);
}

#[test]
fn loops_back_to_start_at_end_of_stream() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");
    let _ = wait_first_frame(&source, Duration::from_secs(15));
    let duration = source.duration().expect("у фикстуры есть длительность");

    // Перемотать почти в конец: после проигрывания хвоста (и выкачивания
    // переупорядоченных B-кадров) декодер обязан перезапустить цикл —
    // признак: снова кадры с маленьким PTS.
    source
        .seek(duration.saturating_sub(Duration::from_secs(2)))
        .expect("seek в конец");
    while source.try_recv_frame().is_some() {}
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(frame) = source.try_recv_frame() {
            if frame.pts > Duration::ZERO && frame.pts < Duration::from_secs(2) {
                break; // кадр из перезапущенного цикла
            }
        }
        assert!(Instant::now() < deadline, "цикл не перезапустился за 20 с");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn drop_while_paused_exits_promptly() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");
    let _ = wait_first_frame(&source, Duration::from_secs(15));
    source.pause();
    assert!(source.is_paused());
    std::thread::sleep(Duration::from_millis(200));

    let start = Instant::now();
    // Поток блокирующе ждет в recv(); drop обязан разбудить его и завершить join быстро.
    drop(source);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "drop на паузе должен завершаться быстро, прошло: {:?}",
        start.elapsed()
    );
}

#[test]
fn pause_releases_timer_resolution_and_resume_reacquires() {
    let source = VideoSource::open(&fixture()).expect("фикстура открывается");
    let _ = wait_first_frame(&source, Duration::from_secs(15));

    let holders_playing = rst_video::active_timer_resolution_holders();
    assert!(
        holders_playing >= 1,
        "при воспроизведении таймер 1 мс должен удерживаться: holders = {holders_playing}"
    );

    source.pause();
    assert!(source.is_paused());
    std::thread::sleep(Duration::from_millis(300));

    let holders_paused = rst_video::active_timer_resolution_holders();
    assert!(
        holders_paused < holders_playing,
        "на паузе число держателей должно уменьшиться: было {holders_playing}, стало {holders_paused}"
    );

    source.play();
    assert!(!source.is_paused());
    let _ = wait_first_frame(&source, Duration::from_secs(15));

    let holders_resumed = rst_video::active_timer_resolution_holders();
    assert!(
        holders_resumed > holders_paused,
        "при возобновлении число держателей должно вырасти: было {holders_paused}, стало {holders_resumed}"
    );

    drop(source);
}
