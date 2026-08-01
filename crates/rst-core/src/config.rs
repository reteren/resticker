//! Чтение и запись config.json: атомарность, версионирование, миграции,
//! обработка битых файлов (CONFIG.md, «Правила записи»).
//!
//! Дебаунс записи (CONFIG.md) здесь намеренно отсутствует: он относится к
//! состоянию приложения, а не к чистому слою ввода-вывода.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::Config;

/// Текущая версия схемы config.json.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Предупреждение о нештатной загрузке (конфиг при этом возвращается всегда).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    /// config.json не парсился; загружена резервная копия config.bak.
    CorruptRecoveredFromBak,
    /// config.json и config.bak не парсились; загружены значения по умолчанию.
    CorruptLoadedDefaults,
}

/// Результат загрузки: сам конфиг плюс возможное предупреждение.
#[derive(Debug, Clone)]
pub struct ConfigLoad {
    pub config: Config,
    pub warning: Option<ConfigWarning>,
}

/// Загрузить конфиг (CONFIG.md, «Правила записи»):
/// отсутствующий файл → дефолты (первый запуск); старая схема → config.bak
/// перед миграцией; битый файл → config.corrupt.\<timestamp\>.json, затем
/// попытка config.bak, затем дефолты с предупреждением. Конфиг новее текущей
/// схемы — ошибка, файл не трогаем.
pub fn load(path: &Path) -> Result<ConfigLoad, CoreError> {
    if !path.exists() {
        return Ok(ConfigLoad {
            config: Config::default(),
            warning: None,
        });
    }
    match try_load(path) {
        Ok(config) => Ok(ConfigLoad {
            config,
            warning: None,
        }),
        Err(CoreError::UnknownSchemaVersion(v)) => Err(CoreError::UnknownSchemaVersion(v)),
        Err(_) => {
            // Битый основной файл: сохраняем его для разбора и пробуем .bak.
            let _ = fs::rename(path, corrupt_path(path));
            let bak = bak_path(path);
            if bak.exists() {
                if let Ok(config) = try_load(&bak) {
                    return Ok(ConfigLoad {
                        config,
                        warning: Some(ConfigWarning::CorruptRecoveredFromBak),
                    });
                }
            }
            Ok(ConfigLoad {
                config: Config::default(),
                warning: Some(ConfigWarning::CorruptLoadedDefaults),
            })
        }
    }
}

/// Прочитать и разобрать файл, при необходимости применив миграции.
fn try_load(path: &Path) -> Result<Config, CoreError> {
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if version > CURRENT_SCHEMA_VERSION as u64 {
        return Err(CoreError::UnknownSchemaVersion(version));
    }
    let value = if version < CURRENT_SCHEMA_VERSION as u64 {
        // CONFIG.md: перед первой миграцией сохраняется config.bak.
        let _ = fs::copy(path, bak_path(path));
        migrate(value)?
    } else {
        value
    };
    serde_json::from_value(value).map_err(CoreError::Migrate)
}

/// Миграции схемы — чистые функции vN → vN+1 (CONFIG.md, «Миграции»).
pub fn migrate(mut value: Value) -> Result<Value, CoreError> {
    let mut version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if version > CURRENT_SCHEMA_VERSION as u64 {
        return Err(CoreError::UnknownSchemaVersion(version));
    }
    while version < CURRENT_SCHEMA_VERSION as u64 {
        value = match version {
            0 => migrate_v0_to_v1(value)?,
            _ => return Err(CoreError::UnknownSchemaVersion(version)),
        };
        version += 1;
    }
    Ok(value)
}

/// Заглушка v0 → v1: v0 — «конфиг без номера версии». Существующие значения
/// сохраняются, недостающие секции достраиваются дефолтами.
fn migrate_v0_to_v1(mut value: Value) -> Result<Value, CoreError> {
    let Some(obj) = value.as_object_mut() else {
        return Err(CoreError::NotAnObject);
    };
    obj.insert("schema_version".to_string(), Value::from(1));
    let defaults = serde_json::to_value(Config::default()).map_err(CoreError::Migrate)?;
    for key in ["settings", "hotkeys", "monitors", "stickers"] {
        obj.entry(key).or_insert_with(|| defaults[key].clone());
    }
    Ok(value)
}
/// Атомарная запись (CONFIG.md, «Атомарность обязательна»):
/// tmp-файл в той же папке → flush + fsync → rename поверх целевого.
/// Перед сериализацией порядок стикеров нормализуется к 0..N.
pub fn save(config: &Config, path: &Path) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut config = config.clone();
    normalize_orders(&mut config);
    let json = serde_json::to_string_pretty(&config).map_err(CoreError::Migrate)?;
    let tmp = tmp_path(path);
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?; // FlushFileBuffers
    }
    // На Windows std::fs::rename использует MoveFileExW с
    // MOVEFILE_REPLACE_EXISTING, то есть замена целевого файла атомарна.
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Нормализация порядка отрисовки к последовательности 0..N при сохранении
/// (CONFIG.md, «order»). Относительный порядок при равных значениях
/// сохраняется (сортировка стабильная).
fn normalize_orders(config: &mut Config) {
    let mut order: Vec<usize> = (0..config.stickers.len()).collect();
    order.sort_by_key(|&i| config.stickers[i].order);
    for (new_order, idx) in order.into_iter().enumerate() {
        config.stickers[idx].order = new_order as i64;
    }
}

/// tmp-файл рядом с целевым: config.json.tmp.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Резервная копия рядом с целевым: config.bak.
fn bak_path(path: &Path) -> PathBuf {
    path.with_file_name("config.bak")
}

/// Имя для битого файла: config.corrupt.<timestamp>.json.
fn corrupt_path(path: &Path) -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.corrupt.{ts}.json"))
}
