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
    #[error("не удалось создать окно трекера окон")]
    WindowTrackerWindowCreateFailed,
    #[error("поток трекера окон завершился до инициализации")]
    WindowTrackerThreadCrashed,
    #[error("комбинация клавиш «{0}» уже занята другим приложением")]
    HotkeyConflict(String),
    #[error("некорректная комбинация клавиш: {0}")]
    InvalidHotkey(String),
    #[error("буфер обмена занят другим приложением")]
    ClipboardBusy,
    #[error("повреждённые данные в буфере обмена: {0}")]
    ClipboardDataCorrupt(&'static str),
    #[error("путь из системного диалога выбора файла повреждён (не валидный UTF-16)")]
    FileDialogPathInvalid,
    #[error("окно закрепления не существует (закрыто до операции)")]
    PinWindowGone,
    #[error("окно уже закреплено (на нём уже стоит маркер resticker)")]
    AlreadyPinned,
    #[error(
        "закрепление отклонено системой: у процесса нет доступа к целевому окну (UIPI). Перезапустите resticker от имени администратора, чтобы закреплять стикеры за окнами с повышенными правами"
    )]
    PinAccessDenied,
    #[error(
        "менеджер виртуальных рабочих столов недоступен (нужна Windows 10 1607+; на системах без виртуальных столов проверка стола недоступна): {0}"
    )]
    VirtualDesktopManagerUnavailable(String),
    #[error("реестр: {0}")]
    Registry(#[from] std::io::Error),
    #[error("Win32: {0}")]
    Win32(#[from] WinError),
}
