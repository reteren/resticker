//! Сериализация модели в точности по схеме из CONFIG.md.

use rst_core::config;
use rst_core::model::*;
use serde_json::{Value, json};

/// Разобрать фикстуру тем же путём, что и боевой load: Value → migrate → Config.
fn parse_fixture(raw: &str) -> Config {
    let value: Value = serde_json::from_str(raw).expect("фикстура должна парситься");
    let value = config::migrate(value).expect("миграция фикстуры");
    serde_json::from_value(value).expect("фикстура должна соответствовать схеме")
}

#[test]
fn fixture_v1_loads() {
    let cfg = parse_fixture(include_str!("fixtures/config_v1_example.json"));

    assert_eq!(cfg.schema_version, 1);
    assert_eq!(cfg.settings.battery_fps_limit, 30);
    assert_eq!(cfg.hotkeys.edit_mode.as_deref(), Some("Ctrl+Alt+S"));
    assert_eq!(cfg.monitors.len(), 1);
    assert_eq!(cfg.monitors[0].friendly_name, "LG ULTRAGEAR");
    assert_eq!(cfg.stickers.len(), 2);

    let file = &cfg.stickers[0];
    assert_eq!(file.order, 3);
    assert_eq!(file.transform.opacity, 0.85);
    assert!(
        matches!(
            &file.source,
            StickerSource::File {
                media_type: MediaType::Image,
                ..
            }
        ),
        "первый стикер — файл-картинка"
    );
    assert_eq!(file.visibility.mode, VisibilityMode::OverlapAllowlist);
    assert_eq!(
        file.visibility.rules[0].process_name.as_deref(),
        Some("chrome.exe")
    );

    let pasted = &cfg.stickers[1];
    assert!(matches!(&pasted.source, StickerSource::Pasted { .. }));
    assert!(!pasted.enabled);
    assert_eq!(pasted.playback.loop_mode, LoopMode::Once);
    assert!(
        pasted.origin.is_some(),
        "второй стикер — мигрированный (origin)"
    );
}

#[test]
fn legacy_window_stickers_are_dropped_without_breaking_load() {
    // Редизайн пинов: `"kind": "window"` удалён из модели. Старый конфиг с
    // такой записью не должен ронять загрузку ВСЕГО файла — запись молча
    // отбрасывается (`config::strip_legacy_window_stickers`), остальное
    // грузится как обычно.
    let value = json!({
        "schema_version": 1,
        "stickers": [
            { "source": { "kind": "window", "window": { "process_name": "obs64.exe" } } },
            { "source": { "kind": "pasted", "path": "pasted/x.png" } }
        ],
        "presets": [
            {
                "name": "P",
                "stickers": [
                    { "source": { "kind": "window", "window": { "process_name": "obs64.exe" } } }
                ]
            }
        ]
    });
    let mut value = value;
    config::strip_legacy_window_stickers(&mut value);
    let cfg: Config = serde_json::from_value(value).expect("конфиг без window-записей парсится");
    assert_eq!(cfg.stickers.len(), 1);
    assert!(matches!(
        cfg.stickers[0].source,
        StickerSource::Pasted { .. }
    ));
    assert!(
        cfg.presets[0].stickers.is_empty(),
        "window-стикер пресета тоже отброшен"
    );
}

#[test]
fn enum_tags_match_schema() {
    let v = serde_json::to_value(StickerSource::File {
        path: "a.png".into(),
        media_type: MediaType::Video,
    })
    .unwrap();
    assert_eq!(v["kind"], "file");
    assert_eq!(v["media_type"], "video");

    let v = serde_json::to_value(StickerSource::Pasted {
        path: "pasted/x.png".into(),
    })
    .unwrap();
    assert_eq!(v["kind"], "pasted");

    assert_eq!(
        serde_json::to_value(VisibilityMode::OverlapAllowlist).unwrap(),
        "overlap_allowlist"
    );
    assert_eq!(
        serde_json::to_value(LoopMode::HoldLastFrame).unwrap(),
        "hold_last_frame"
    );
    assert_eq!(
        serde_json::to_value(MediaType::Animation).unwrap(),
        "animation"
    );
}

#[test]
fn roundtrip_all_variants() {
    let mut sticker_file = Sticker {
        order: 2,
        ..Default::default()
    };
    sticker_file.visibility = VisibilityRule {
        mode: VisibilityMode::OverlapAllowlist,
        rules: vec![OverlapRule {
            process_name: Some("chrome.exe".to_string()),
            title_pattern: Some("*YouTube*".to_string()),
        }],
    };
    let sticker_pasted = Sticker {
        order: 1,
        source: StickerSource::Pasted {
            path: "pasted/x.png".into(),
        },
        visibility: VisibilityRule {
            mode: VisibilityMode::Desktop,
            rules: vec![],
        },
        playback: PlaybackSettings {
            loop_mode: LoopMode::HoldLastFrame,
            audio_track: Some(2),
            ..Default::default()
        },
        ..Default::default()
    };
    let config = Config {
        stickers: vec![sticker_file, sticker_pasted],
        ..Default::default()
    };

    let json = serde_json::to_string(&config).unwrap();
    let back: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(back, config);
}

#[test]
fn unknown_fields_ignored() {
    let value = json!({
        "schema_version": 1,
        "future_field": { "nested": [1, 2, 3] },
        "settings": { "autostart": false, "also_future": true }
    });
    let cfg: Config = serde_json::from_value(value).unwrap();
    assert!(!cfg.settings.autostart);
    assert!(cfg.settings.silent_start, "прочие поля — из дефолтов");
}

#[test]
fn missing_fields_defaulted() {
    let cfg: Config = serde_json::from_value(json!({})).unwrap();
    assert_eq!(cfg, Config::default());
}

#[test]
fn cursor_panel_offset_roundtrip_with_value() {
    let config = Config {
        settings: Settings {
            cursor_panel_offset: Some((20.0, -8.0)),
            ..Settings::default()
        },
        ..Config::default()
    };

    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(
        json["settings"]["cursor_panel_offset"],
        json!([20.0, -8.0]),
        "кортеж сериализуется как JSON-массив [dx, dy]"
    );

    let back: Config = serde_json::from_value(json).unwrap();
    assert_eq!(
        back.settings.cursor_panel_offset,
        Some((20.0, -8.0)),
        "поле переживает round-trip"
    );
}

#[test]
fn cursor_panel_offset_old_format_without_field() {
    // Старый config.json (без поля) грузится без ошибок — дефолт None.
    let cfg: Config = serde_json::from_value(json!({
        "schema_version": 1,
        "settings": { "language": "en" }
    }))
    .unwrap();

    assert_eq!(cfg.settings.language, "en");
    assert_eq!(
        cfg.settings.cursor_panel_offset, None,
        "отсутствующее поле достраивается дефолтом None"
    );
}

#[test]
fn cursor_panel_offset_default_is_none() {
    assert_eq!(
        Settings::default().cursor_panel_offset,
        None,
        "дефолтная настройка — дефолтное смещение панели у курсора"
    );
}

#[test]
fn denylist_and_pin_hotkey_defaults_for_old_configs() {
    // Старый config.json без `settings.denylist` и без
    // `hotkeys.pin_focused_window` грузится без миграции схемы: денй-лист
    // пуст, хоткей достраивается дефолтом (тот же паттерн, что
    // `PlaybackSettings.paused`).
    let cfg: Config = serde_json::from_value(json!({
        "schema_version": 1,
        "settings": { "language": "en" },
        "hotkeys": { "edit_mode": "Ctrl+Alt+S" }
    }))
    .unwrap();

    assert_eq!(cfg.settings.language, "en");
    assert!(
        cfg.settings.denylist.is_empty(),
        "отсутствующий denylist достраивается пустым списком"
    );
    assert_eq!(
        cfg.hotkeys.pin_focused_window.as_deref(),
        Some("Ctrl+Alt+T"),
        "отсутствующий хоткей достраивается дефолтом"
    );
}

#[test]
fn denylist_roundtrips() {
    let config = Config {
        settings: Settings {
            denylist: vec![OverlapRule {
                process_name: Some("obs64.exe".to_string()),
                title_pattern: Some("*OBS*".to_string()),
            }],
            ..Settings::default()
        },
        ..Config::default()
    };

    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(
        json["settings"]["denylist"][0]["process_name"],
        json!("obs64.exe")
    );

    let back: Config = serde_json::from_value(json).unwrap();
    assert_eq!(back.settings.denylist, config.settings.denylist);
}

#[test]
fn new_file_sticker_is_centered_and_enabled() {
    let sticker = Sticker::new_file(
        "C:\\pics\\cat.png".into(),
        MediaType::Image,
        MonitorId("\\\\?\\DISPLAY#TEST".to_string()),
        960.0,
        540.0,
        300.0,
        200.0,
    );
    assert!(sticker.enabled);
    assert!(sticker.visible);
    assert_eq!(sticker.placement.cx, 960.0);
    assert_eq!(sticker.placement.cy, 540.0);
    assert_eq!(sticker.placement.w, 300.0);
    assert_eq!(sticker.placement.h, 200.0);
    assert_eq!(sticker.visibility.mode, VisibilityMode::Always);
    assert!(sticker.origin.is_none());
    match sticker.source {
        StickerSource::File { path, media_type } => {
            assert_eq!(path, std::path::PathBuf::from("C:\\pics\\cat.png"));
            assert_eq!(media_type, MediaType::Image);
        }
        other => panic!("ожидался StickerSource::File, получено {other:?}"),
    }
}

#[test]
fn new_pasted_sticker_uses_pasted_source() {
    let sticker = Sticker::new_pasted(
        "C:\\Users\\u\\AppData\\Roaming\\resticker\\pasted\\abc.png".into(),
        MonitorId("\\\\?\\DISPLAY#TEST".to_string()),
        960.0,
        540.0,
        300.0,
        200.0,
    );
    assert!(sticker.enabled);
    assert!(sticker.visible);
    match &sticker.source {
        StickerSource::Pasted { path } => {
            assert_eq!(
                path,
                &std::path::PathBuf::from(
                    "C:\\Users\\u\\AppData\\Roaming\\resticker\\pasted\\abc.png"
                )
            );
        }
        other => panic!("ожидался StickerSource::Pasted, получено {other:?}"),
    }

    let json = serde_json::to_value(&sticker).unwrap();
    assert_eq!(json["source"]["kind"], "pasted");
}

#[test]
fn zdbg_pasted_source() {
    let v = json!({
        "schema_version": 1,
        "stickers": [{ "kind": "pasted", "path": "pasted/x.png" }],
        "presets": [{ "name": "P", "stickers": [] }]
    });
    eprintln!("value: {v}");
    let cfg: Result<Config, _> = serde_json::from_value(v);
    eprintln!("config parse: {cfg:?}");
}
