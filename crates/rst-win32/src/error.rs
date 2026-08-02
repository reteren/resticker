//! Ошибки Win32-обёрток.

use windows::core::Error as WinError;

#[derive(Debug, thiserror::Error)]
pub enum Win32Error {
    #[error("не удалось создать окно трея")]
    TrayWindowCreateFailed,
    #[error("поток трея завершился до инициализации")]
    TrayThreadCrashed,
    #[error("Shell_NotifyIconW провалился")]
    TrayNotifyIconFailed,
    #[error("реестр: {0}")]
    Registry(#[from] std::io::Error),
    #[error("Win32: {0}")]
    Win32(#[from] WinError),
}
