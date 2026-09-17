//! Сериализация модели в точности по схеме из CONFIG.md.

use rst_core::config;
use rst_core::model::*;
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;

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
        ..Default::default()
    };
    let sticker_pasted = Sticker {
        order: 1,
        source: StickerSource::Pasted {
            path: "pasted/x.png".into(),
        },
        visibility: VisibilityRule {
            mode: VisibilityMode::Desktop,
            rules: vec![],
            ..Default::default()
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
fn old_visibility_without_desktop_defaults_to_visible() {
    let visibility: VisibilityRule = serde_json::from_value(json!({
        "mode": "overlap_allowlist",
        "rules": []
    }))
    .unwrap();

    assert!(visibility.desktop);
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

// ==== Группы окон (запрос пользователя 2026-08-25) ====
fn group(number: u8) -> WindowGroup {
    WindowGroup {
        id: Uuid::new_v4(),
        number,
        name: number.to_string(),
        members: Vec::new(),
        gap_pct: 0,
    }
}

#[test]
fn first_group_takes_number_one() {
    assert_eq!(WindowGroup::next_number(&[]), Some(1));
}

#[test]
fn new_group_fills_the_hole_left_by_a_deleted_one() {
    // Удалили группу 2 — следующая новая обязана занять именно двойку,
    // иначе Ctrl+Alt+2 перестал бы открывать хоть что-нибудь, пока
    // номера уползают вверх.
    let existing = vec![group(1), group(3)];
    assert_eq!(WindowGroup::next_number(&existing), Some(2));
}

#[test]
fn nine_groups_leave_no_free_number() {
    let existing: Vec<WindowGroup> = (1..=MAX_GROUP_NUMBER).map(group).collect();
    assert_eq!(
        WindowGroup::next_number(&existing),
        None,
        "цифровых хоткеев всего девять — десятая группа не открывалась бы ничем"
    );
}

#[test]
fn config_without_groups_field_reads_as_empty() {
    // Старые config.json поля `groups` не содержат: миграция схемы не
    // нужна, значение приходит из `#[serde(default)]`.
    let raw = r#"{"schema_version":1,"stickers":[]}"#;
    let cfg: Config = serde_json::from_str(raw).expect("конфиг без groups обязан читаться");
    assert!(cfg.groups.is_empty());
}

#[test]
fn group_survives_a_round_trip_through_json() {
    let mut g = group(4);
    g.gap_pct = 12;
    g.members.push(GroupMember {
        exe_path: PathBuf::from("C:/Windows/explorer.exe"),
        title: "Проводник".to_string(),
        class: "CabinetWClass".to_string(),
        place: Some(GroupPlace {
            monitor_id: MonitorId("mon-1".to_string()),
            x: 10.0,
            y: 20.0,
            w: 800.0,
            h: 600.0,
        }),
    });
    let json = serde_json::to_string(&g).expect("сериализация группы");
    let back: WindowGroup = serde_json::from_str(&json).expect("разбор группы");
    assert_eq!(back, g);
}

#[test]
fn member_without_a_saved_place_reads_back_as_none() {
    // Только что собранная группа мест ещё не знает — это не ошибка и не
    // повод писать в файл нули, которые потом уедут в левый верхний угол.
    let raw = r#"{"exe_path":"a.exe","title":"t","class":"c"}"#;
    let m: GroupMember = serde_json::from_str(raw).expect("член без места");
    assert_eq!(m.place, None);
}

// ==== Хоткеи групп (T5) ====
#[test]
fn group_hotkey_defaults_are_exactly_the_spec_combos() {
    let h = Hotkeys::default();
    assert_eq!(h.edit_groups_menu.as_deref(), Some("Ctrl+Alt+G"));
    assert_eq!(h.delete_open_group.as_deref(), Some("Ctrl+Alt+Shift+G"));
    for n in 1..=9 {
        assert_eq!(
            h.open_group(n),
            Some(format!("Ctrl+Alt+{n}").as_str()),
            "группа {n} открывается по Ctrl+Alt+{n}"
        );
    }
}

#[test]
fn old_config_without_t5_fields_gains_group_hotkey_defaults() {
    // Старый config.json (без T5-полей) читается без миграции схемы:
    // недостающие поля достраиваются из `Hotkeys::default()` — тот же
    // прецедент, что `pin_focused_window` (тест
    // `denylist_and_pin_hotkey_defaults_for_old_configs` выше).
    let cfg: Config = serde_json::from_value(json!({
        "schema_version": 1,
        "hotkeys": { "edit_mode": "Ctrl+Alt+S" }
    }))
    .unwrap();

    assert_eq!(cfg.hotkeys.edit_mode.as_deref(), Some("Ctrl+Alt+S"));
    assert_eq!(cfg.hotkeys.edit_groups_menu.as_deref(), Some("Ctrl+Alt+G"));
    assert_eq!(
        cfg.hotkeys.delete_open_group.as_deref(),
        Some("Ctrl+Alt+Shift+G")
    );
    let expected: Vec<Option<String>> = (1..=9).map(|n| Some(format!("Ctrl+Alt+{n}"))).collect();
    assert_eq!(cfg.hotkeys.open_group_by_number, expected);
}

#[test]
fn fixture_v1_gains_group_hotkey_defaults() {
    // Боевой старый формат: фикстура v1 с тремя хоткеями — после чтения
    // новые поля приходят дефолтами, а старые не теряются.
    let cfg = parse_fixture(include_str!("fixtures/config_v1_example.json"));
    assert_eq!(
        cfg.hotkeys.toggle_all_stickers.as_deref(),
        Some("Ctrl+Alt+H")
    );
    assert_eq!(cfg.hotkeys.edit_groups_menu.as_deref(), Some("Ctrl+Alt+G"));
    assert_eq!(cfg.hotkeys.open_group_by_number.len(), 9);
}

#[test]
fn open_group_by_number_roundtrips_with_nulls() {
    // Смешанная конфигурация: назначенные хоткеи — строками, неназначенные
    // — null; позиция в массиве = номер группы минус один.
    let mut h = Hotkeys::default();
    h.open_group_by_number[2] = None; // группа 3
    h.open_group_by_number[8] = None; // группа 9
    let v = serde_json::to_value(&h).unwrap();
    assert_eq!(
        v["open_group_by_number"],
        json!([
            "Ctrl+Alt+1",
            "Ctrl+Alt+2",
            null,
            "Ctrl+Alt+4",
            "Ctrl+Alt+5",
            "Ctrl+Alt+6",
            "Ctrl+Alt+7",
            "Ctrl+Alt+8",
            null
        ])
    );
    let back: Hotkeys = serde_json::from_value(v).unwrap();
    assert_eq!(back.open_group_by_number, h.open_group_by_number);
}

#[test]
fn open_group_accessor_rejects_out_of_range_and_short_vecs() {
    let h = Hotkeys::default();
    assert_eq!(h.open_group(1), Some("Ctrl+Alt+1"));
    assert_eq!(h.open_group(9), Some("Ctrl+Alt+9"));
    assert_eq!(h.open_group(0), None, "номера групп начинаются с 1");
    assert_eq!(h.open_group(10), None, "номеров больше девяти нет");

    // Хвост короче девяти читается как «не назначен», а не как ошибка
    // конфигурации: настройки могут сохранить частичный список.
    let mut short = Hotkeys::default();
    short.open_group_by_number.truncate(3);
    assert_eq!(short.open_group(3), Some("Ctrl+Alt+3"));
    assert_eq!(short.open_group(4), None);
}
