//! M1-сценарии на уровне данных (ROADMAP.md): добавление стикера из настроек
//! с появлением в центре основного монитора и сохранение/восстановление
//! между запусками. Тот же путь, что у боевого `overlay_manager::add_sticker`
//! (crates/resticker/src/overlay_manager.rs), но без GPU и окон: только
//! `Sticker::new_file` + `rst_core::config::save/load`.

use rst_core::config;
use rst_core::model::{
    Config, MediaType, MonitorId, Placement, Sticker, StickerSource, VisibilityMode,
};
use std::path::PathBuf;
use tempfile::tempdir;

/// Экранная геометрия «основного монитора» для тестов.
const SCREEN_W: f64 = 1920.0;
const SCREEN_H: f64 = 1080.0;

/// Путь, который выбирает диалог настроек (M1 «добавление стикера»).
const STICKER_PATH: &str = "C:\\pics\\sticker.png";

#[test]
fn add_from_settings_centers_sticker_on_main_monitor() {
    // OverlayManager::add_sticker передаёт центр монитора как (screen_w/2, screen_h/2)
    // и нативный размер картинки — воспроизводим ровно этот вызов.
    let (img_w, img_h) = (640.0, 480.0);
    let sticker = Sticker::new_file(
        STICKER_PATH.into(),
        MediaType::Image,
        MonitorId::default(),
        SCREEN_W / 2.0,
        SCREEN_H / 2.0,
        img_w,
        img_h,
    );

    assert_eq!(
        sticker.placement,
        Placement {
            monitor_id: MonitorId::default(),
            cx: 960.0,
            cy: 540.0,
            w: 640.0,
            h: 480.0,
        }
    );
    assert!(sticker.enabled && sticker.visible);
    assert_eq!(sticker.visibility.mode, VisibilityMode::Always);
    assert_eq!(sticker.transform.rotation, 0.0);
    assert_eq!(sticker.transform.opacity, 1.0);
    match &sticker.source {
        StickerSource::File { path, media_type } => {
            assert_eq!(path, &PathBuf::from(STICKER_PATH));
            assert_eq!(*media_type, MediaType::Image);
        }
        other => panic!("ожидался StickerSource::File, получено {other:?}"),
    }
}

#[test]
fn save_then_reload_restores_stickers_across_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");
    let monitor = MonitorId(
        "\\\\?\\DISPLAY#HGSMN1#5&2c6f0bc8&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa8}"
            .to_string(),
    );

    let mut cat = Sticker::new_file(
        "C:\\pics\\cat.png".into(),
        MediaType::Image,
        monitor.clone(),
        960.0,
        540.0,
        512.0,
        320.0,
    );
    cat.order = 0;

    let mut dog = Sticker::new_file(
        "C:\\pics\\dog.png".into(),
        MediaType::Image,
        monitor.clone(),
        300.0,
        200.0,
        256.0,
        256.0,
    );
    dog.order = 1;
    dog.transform.rotation = 0.25;
    dog.transform.opacity = 0.8;
    dog.transform.flip_h = true;

    let mut gif = Sticker::new_file(
        "C:\\pics\\wave.gif".into(),
        MediaType::Animation,
        monitor.clone(),
        1600.0,
        900.0,
        320.0,
        240.0,
    );
    gif.order = 2;
    gif.visibility.mode = VisibilityMode::Desktop;

    let config = Config {
        stickers: vec![cat, dog, gif],
        ..Default::default()
    };

    // «Добавление стикера из настроек» сохраняет конфиг на диск.
    config::save(&config, &path).unwrap();

    // «Перезапуск»: load из свежего процесса должен вернуть те же стикеры.
    let loaded = config::load(&path).unwrap();

    assert_eq!(loaded.warning, None);
    assert_eq!(loaded.config.schema_version, config.schema_version);
    assert_eq!(
        loaded.config.stickers, config.stickers,
        "все стикеры восстановлены без потерь"
    );
    assert_eq!(
        loaded.config.stickers[0].placement.cx,
        SCREEN_W / 2.0,
        "центр основного монитора пережил перезапуск"
    );
    assert_eq!(
        loaded.config.stickers[0].placement.cy,
        SCREEN_H / 2.0,
        "центр основного монитора пережил перезапуск"
    );
    assert_eq!(
        loaded.config.stickers[1].transform.opacity, 0.8,
        "трансформация пережила перезапуск"
    );
}
