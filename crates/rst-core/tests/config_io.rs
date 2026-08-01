//! Файловые сценарии config.json (CONFIG.md, «Правила записи»).

use rst_core::CoreError;
use rst_core::config::{ConfigWarning, load, save};
use rst_core::model::{Config, Sticker};
use std::fs;
use uuid::Uuid;

fn config_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("config.json")
}

#[test]
fn missing_file_returns_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(&config_path(&dir)).unwrap();
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warning, None);
}

#[test]
fn save_then_load_roundtrip_and_normalizes_orders() {
    let dir = tempfile::tempdir().unwrap();
    let path = config_path(&dir);

    let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let config = Config {
        stickers: vec![
            Sticker {
                id: a,
                order: 5,
                ..Default::default()
            },
            Sticker {
                id: b,
                order: 5,
                ..Default::default()
            },
            Sticker {
                id: c,
                order: 9,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    save(&config, &path).unwrap();
    let loaded = load(&path).unwrap();

    assert_eq!(loaded.warning, None);
    // Порядок нормализован к 0..N, относительный при равных сохранён (CONFIG.md).
    let order_of = |id: Uuid| {
        loaded
            .config
            .stickers
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .order
    };
    assert_eq!(order_of(a), 0);
    assert_eq!(order_of(b), 1);
    assert_eq!(order_of(c), 2);
}

#[test]
fn save_leaves_no_tmp_file() {
    let dir = tempfile::tempdir().unwrap();
    save(&Config::default(), &config_path(&dir)).unwrap();
    let leftover = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(!leftover, "после атомарной записи tmp-файла быть не должно");
}

#[test]
fn corrupt_main_without_bak_loads_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = config_path(&dir);
    fs::write(&path, "это не json {{{").unwrap();

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warning, Some(ConfigWarning::CorruptLoadedDefaults));
    assert!(!path.exists(), "битый файл переименован");
    let corrupt_exists = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.corrupt.")
        });
    assert!(
        corrupt_exists,
        "битый файл сохранён как config.corrupt.<ts>.json"
    );
}

#[test]
fn corrupt_main_with_bak_recovers_bak() {
    let dir = tempfile::tempdir().unwrap();
    let path = config_path(&dir);
    let bak = dir.path().join("config.bak");

    let mut good = Config::default();
    good.settings.autostart = false;
    fs::write(&bak, serde_json::to_string(&good).unwrap()).unwrap();
    fs::write(&path, "битый { json").unwrap();

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.warning, Some(ConfigWarning::CorruptRecoveredFromBak));
    assert!(
        !loaded.config.settings.autostart,
        "поднялись данные из .bak"
    );
}

#[test]
fn migration_creates_bak_with_original_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = config_path(&dir);
    let v0 = include_str!("fixtures/config_v0_minimal.json");
    fs::write(&path, v0).unwrap();

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.warning, None);
    assert_eq!(loaded.config.schema_version, 1);
    assert!(!loaded.config.settings.autostart, "значение v0 сохранилось");

    let bak = fs::read_to_string(dir.path().join("config.bak")).unwrap();
    assert!(bak.contains("\"autostart\": false"), "в .bak — исходный v0");
    assert!(
        !bak.contains("schema_version"),
        "в .bak — исходный v0 без версии"
    );
}

#[test]
fn future_schema_version_is_an_error_and_file_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = config_path(&dir);
    fs::write(&path, r#"{ "schema_version": 99 }"#).unwrap();

    let err = load(&path).unwrap_err();
    assert!(
        matches!(err, CoreError::UnknownSchemaVersion(99)),
        "ожидалась ошибка UnknownSchemaVersion(99), получено: {err}"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{ "schema_version": 99 }"#,
        "файл с более новой схемой нельзя трогать"
    );
}
