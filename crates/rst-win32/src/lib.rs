//! Win32 wrappers for resticker: windows, WinEvent hooks, hotkeys, tray,
//! DPI, monitors, always-on-top.
//!
//! Весь `unsafe` живёт внутри этого крейта за безопасными обёртками
//! (CONTRIBUTING.md, «Правила работы с unsafe»). Каркас M0 — наполнение
//! появится в следующих срезах.

pub mod autostart;
mod error;
pub mod overlay;
pub mod tray;

pub use error::Win32Error;
