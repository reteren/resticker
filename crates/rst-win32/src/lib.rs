//! Win32 wrappers for resticker: windows, WinEvent hooks, hotkeys, tray,
//! DPI, monitors, always-on-top, edit-mode input, clipboard.
//!
//! Весь `unsafe` живёт внутри этого крейта за безопасными обёртками
//! (CONTRIBUTING.md, «Правила работы с unsafe»).

pub mod autostart;
pub mod clipboard;
mod error;
pub mod hotkey;
pub mod input;
pub mod overlay;
pub mod tray;

pub use error::Win32Error;
