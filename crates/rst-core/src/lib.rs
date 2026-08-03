//! resticker core: data model, application state, undo history, edit-mode
//! geometry (hit-testing, snapping, transform gestures), configuration
//! and schema migrations.
//!
//! Платформенно-независимый крейт (CONTRIBUTING.md, «Правило зависимостей»):
//! никаких зависимостей от Windows или рендера, вся логика покрывается
//! юнит-тестами на любой ОС.

pub mod config;
mod error;
pub mod hittest;
pub mod migration;
pub mod model;
pub mod monitor_rebind;
pub mod ops;
pub mod selection_set;
pub mod snap;
pub mod transform_ops;
pub mod undo;

pub use error::CoreError;
