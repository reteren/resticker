//! Ошибки rst-core.

/// Ошибка загрузки, сохранения или миграции конфигурации.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Ошибка ввода-вывода при работе с файлами конфига.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Файл не является валидным JSON.
    #[error("could not parse JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Конфиг новее, чем понимает эта версия программы (миграции только вперёд,
    /// CONFIG.md). Такой файл нельзя считать битым и переименовывать.
    #[error("unknown config schema version: {0}")]
    UnknownSchemaVersion(u64),
    /// Конфиг не соответствует схеме после миграции.
    #[error("migrated config does not match the schema: {0}")]
    Migrate(serde_json::Error),
    /// Конфиг v0: верхний уровень не является JSON-объектом.
    #[error("config v0: expected a top-level JSON object")]
    NotAnObject,
}
