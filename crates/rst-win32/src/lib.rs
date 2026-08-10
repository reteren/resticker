//! Win32 wrappers for resticker: windows, WinEvent hooks, hotkeys, tray,
//! DPI, monitors, always-on-top, edit-mode input, clipboard.
//!
//! Весь `unsafe` живёт внутри этого крейта за безопасными обёртками
//! (CONTRIBUTING.md, «Правила работы с unsafe»).

pub mod autostart;
pub mod clipboard;
mod error;
pub mod file_dialog;
pub mod hotkey;
pub mod input;
pub mod monitors;
pub mod overlay;
pub mod single_instance;
pub mod tray;
pub mod virtual_desktops;
pub mod window_enum;
mod window_icon;
pub mod window_pick;
pub mod window_pin;
pub mod window_tracker;

pub use error::Win32Error;
