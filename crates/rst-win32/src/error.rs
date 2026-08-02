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
    #[error("не удалось создать оверлей-окно")]
    OverlayWindowCreateFailed,
    #[error("поток оверлея завершился до инициализации")]
    OverlayThreadCrashed,
    #[error("комбинация клавиш «{0}» уже занята другим приложением")]
    HotkeyConflict(String),
    #[error("некорректная комбинация клавиш: {0}")]
    InvalidHotkey(String),
    #[error("буфер обмена занят другим приложением")]
    ClipboardBusy,
    #[error("повреждённые данные в буфере обмена: {0}")]
    ClipboardDataCorrupt(&'static str),
    #[error("реестр: {0}")]
    Registry(#[from] std::io::Error),
    #[error("Win32: {0}")]
    Win32(#[from] WinError),
}
