//! Миграции схемы (CONFIG.md, «Миграции»): реальные файлы из tests/fixtures/.

use rst_core::CoreError;
use rst_core::config;
use rst_core::model::Config;
use serde_json::{Value, json};

#[test]
fn v0_fixture_migrates_to_v1_keeping_values() {
    let value: Value =
        serde_json::from_str(include_str!("fixtures/config_v0_minimal.json")).unwrap();
    let migrated = config::migrate(value).unwrap();
    let cfg: Config = serde_json::from_value(migrated).unwrap();

    assert_eq!(cfg.schema_version, 1);
    // Существующие в v0 значения сохранились…
    assert!(!cfg.settings.autostart);
    assert_eq!(cfg.settings.language, "en");
    // …а недостающие секции достроились дефолтами.
    assert!(cfg.settings.silent_start);
    assert_eq!(cfg.hotkeys.edit_mode.as_deref(), Some("Ctrl+Alt+S"));
    assert!(cfg.monitors.is_empty());
    assert!(cfg.stickers.is_empty());
}

#[test]
fn migrate_rejects_future_version() {
    let err = config::migrate(json!({ "schema_version": 99 })).unwrap_err();
    assert!(matches!(err, CoreError::UnknownSchemaVersion(99)));
}

#[test]
fn migrate_v0_rejects_non_object() {
    let err = config::migrate(json!("просто строка")).unwrap_err();
    assert!(matches!(err, CoreError::NotAnObject));
}

#[test]
fn migrate_current_version_is_noop() {
    let value = serde_json::to_value(Config::default()).unwrap();
    let migrated = config::migrate(value.clone()).unwrap();
    assert_eq!(migrated, value);
}
