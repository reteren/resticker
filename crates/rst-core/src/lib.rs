//! resticker core: data model, application state, configuration and schema
//! migrations.
//!
//! Платформенно-независимый крейт (CONTRIBUTING.md, «Правило зависимостей»):
//! никаких зависимостей от Windows или рендера, вся логика покрывается
//! юнит-тестами на любой ОС.

pub mod config;
mod error;
pub mod model;

pub use error::CoreError;
