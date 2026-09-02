//! Ошибки Win32-обёрток.
//!
//! Тексты английские: `Display` этих ошибок попадает не только в лог, но и в
//! баллон-уведомления (`PinAccessDenied` — тост «Could not pin the window»),
//! а программа с 2026-08-23 англоязычная целиком.

use windows::core::Error as WinError;

#[derive(Debug, thiserror::Error)]
pub enum Win32Error {
    #[error("could not create the tray window")]
    TrayWindowCreateFailed,
    #[error("the tray thread exited before initialization")]
    TrayThreadCrashed,
    #[error("Shell_NotifyIconW failed")]
    TrayNotifyIconFailed,
    #[error("could not create the overlay window")]
    OverlayWindowCreateFailed,
    #[error("the overlay thread exited before initialization")]
    OverlayThreadCrashed,
    #[error("could not create the window-tracker window")]
    WindowTrackerWindowCreateFailed,
    #[error("the window-tracker thread exited before initialization")]
    WindowTrackerThreadCrashed,
    #[error("the {0} shortcut is already taken by another application")]
    HotkeyConflict(String),
    #[error("malformed key combination: {0}")]
    InvalidHotkey(String),
    #[error("the clipboard is locked by another application")]
    ClipboardBusy,
    #[error("corrupted clipboard data: {0}")]
    ClipboardDataCorrupt(&'static str),
    #[error("the path from the system file dialog is corrupted (not valid UTF-16)")]
    FileDialogPathInvalid,
    #[error("the window to pin no longer exists (closed before the operation)")]
    PinWindowGone,
    #[error("the window is already pinned (it already carries the resticker marker)")]
    AlreadyPinned,
    #[error(
        "pinning was rejected by the system: this process has no access to the target window (UIPI). Restart resticker as administrator to pin stickers to windows running with elevated rights"
    )]
    PinAccessDenied,
    #[error(
        "the virtual desktop manager is unavailable (needs Windows 10 1607+; on systems without virtual desktops the desktop check is not available): {0}"
    )]
    VirtualDesktopManagerUnavailable(String),
    #[error("could not spawn the second instance: {0}")]
    SiblingSpawnFailed(std::io::Error),
    #[error("registry: {0}")]
    Registry(#[from] std::io::Error),
    #[error("Win32: {0}")]
    Win32(#[from] WinError),
}
