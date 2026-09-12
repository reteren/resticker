//! Оверлей-окно: прозрачное, «клик-прозрачное», поверх всех окон, на
//! границы одного монитора (M3: окно на монитор — позиция и размер
//! передаются при создании, например из [`crate::monitors::MonitorInfo`]).
//! Живёт на собственном потоке со своим циклом сообщений (ADR-013 — тот же
//! паттерн, что у трея). Здесь только окно и pump: рендер (D3D11 +
//! DirectComposition) подключается снаружи через [`OverlayWindow::hwnd`]
//! (ARCHITECTURE.md, раздел 2).
//!
//! M2: окно также владеет глобальными хоткеями — входа/выхода из режима
//! редактирования и «показать/скрыть все стикеры» — и мостом «сырые
//! сообщения окна → безопасные события» (`OverlayEvent`), см.
//! docs/M2_INTEGRATION_PLAN.md, раздел 1. M3: глобальный хоткей
//! регистрирует ровно одно окно на процесс (docs/M3_PREP_NOTES.md,
//! раздел 3.3) — остальные создаются с `None`; системные изменения
//! (`WM_DISPLAYCHANGE`, блокировка/сон сессии) уходят событиями
//! (`MonitorsChanged`/`SessionLocked`/`SystemSuspending` и др., раздел 3.6).
//! Мышь и курсор обрабатываются
//! здесь ([`crate::input`]); хит-тестинг и жесты — у вызывающего кода (ядро
//! редактора платформенно-независимо).

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, RGN_DIFF, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GW_HWNDPREV, GWL_EXSTYLE,
    GWLP_USERDATA, GetMessageW, GetSystemMetrics, GetWindow, GetWindowDisplayAffinity,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, HTCLIENT, HTTRANSPARENT,
    HWND_TOPMOST, IsWindowVisible, KillTimer, LWA_ALPHA, MSG, PBT_APMRESUMEAUTOMATIC,
    PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PostMessageW, PostQuitMessage, RegisterClassExW,
    SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetForegroundWindow, SetLayeredWindowAttributes, SetTimer,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WHEEL_DELTA, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NCHITTEST, WM_POWERBROADCAST,
    WM_SETCURSOR, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use rst_core::model::{Hotkeys, Rect};

use crate::error::Win32Error;
use crate::hotkey::{
    HotkeyCombo, HotkeyRegistrationConflict, RegisteredHotkeySet, message_hotkey_id,
};
use crate::input::{CursorManager, CursorShape, InputEvent, Modifiers, MouseCapture};
use crate::monitors::{self, MonitorInfo};

const CLASS_NAME: PCWSTR = w!("resticker_overlay");
const WINDOW_TITLE: PCWSTR = w!("resticker_overlay_wnd");

/// Идентификатор глобального хоткея входа/выхода из режима редактирования
/// (ROADMAP M2).
const EDIT_HOTKEY_ID: i32 = 1;

/// Идентификатор глобального хоткея «показать/скрыть все стикеры»
/// (M2b7, `hotkeys.toggle_all_stickers` в конфиге).
const TOGGLE_ALL_HOTKEY_ID: i32 = 2;

/// Идентификатор глобального хоткея «заглушить все стикеры» (M5d,
/// `hotkeys.mute_all` в конфиге, `AudioMixer::set_muted`).
const MUTE_ALL_HOTKEY_ID: i32 = 3;

/// Идентификатор глобального хоткея «закрепить/открепить сфокусированное
/// окно» (редизайн пинов, `hotkeys.pin_focused_window` в конфиге, дефолт
/// `"Ctrl+Alt+T"`). Регистрация и эмиссия события — здесь; сама логика
/// `GetForegroundWindow`-пина — задача координатора, этот срез её не
/// подключает.
const PIN_FOCUSED_HOTKEY_ID: i32 = 4;

/// Идентификаторы временных хоткеев управления видео-стикером под курсором
/// (запрос пользователя 2026-08-22: пробел — пауза, PgUp/PgDn — громкость).
///
/// Регистрируются НЕ на всё время работы программы, а ровно пока курсор
/// стоит на видео-стикере с показанной полосой перемотки: `RegisterHotKey`
/// забирает клавишу у всей системы, и постоянно занятый пробел сломал бы
/// набор текста везде. Включает/выключает их координатор
/// ([`OverlayWindow::set_media_hotkeys`]) тем же условием, по которому
/// показывает полосу.
const MEDIA_PLAY_PAUSE_HOTKEY_ID: i32 = 5;
const MEDIA_VOLUME_UP_HOTKEY_ID: i32 = 6;
const MEDIA_VOLUME_DOWN_HOTKEY_ID: i32 = 7;

/// Виртуальные коды клавиш медиа-хоткеев (docs.microsoft.com/Virtual-Key-Codes).
const VK_SPACE: u32 = 0x20;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;

/// Координатор → поток оверлея: сменить форму курсора (зона под курсором
/// меняется на его стороне, хит-тест — не Win32, ARCHITECTURE.md 5.3);
/// `wParam` — форма, закодированная [`cursor_shape_to_wparam`].
const WM_APP_EDIT_CURSOR: u32 = WM_APP + 1;

/// Координатор → поток оверлея: снять Win32-захват мыши безусловно
/// ([`crate::input::MouseCapture::force_release`]) — должно выполняться на
/// потоке окна (`SetCapture`/`ReleaseCapture` — thread-affine Win32 API),
/// поэтому не прямой вызов, а сообщение, как и `WM_APP_EDIT_CURSOR`.
const WM_APP_RELEASE_CAPTURE: u32 = WM_APP + 2;

/// Включить/выключить медиа-хоткеи (`wparam != 0` — включить). Сообщением, а
/// не прямым вызовом: `RegisterHotKey` привязан к ПОТОКУ, снять его может
/// только тот же поток, а просит координатор со своего.
const WM_APP_MEDIA_HOTKEYS: u32 = WM_APP + 3;

/// Полностью заменить набор постоянных хоткеев окна. `lParam` — указатель на
/// `Box<Vec<(i32, HotkeyCombo)>>` (все комбинации разом: режим редактирования,
/// стикеры, звук, пин, группы); владение переходит потоку окна и
/// освобождается в `wndproc`. Сообщением, а не прямым вызовом, по той же
/// причине, что `WM_APP_MEDIA_HOTKEYS`: `RegisterHotKey` привязан к потоку,
/// и регистрировать новые комбинации обязан поток окна — координатор
/// передаёт только описание.
const WM_APP_REREGISTER_HOTKEYS: u32 = WM_APP + 4;

/// Применить политику ввода окна — ЕДИНСТВЕННЫЙ писатель флага
/// `WS_EX_TRANSPARENT` для режимов, которые решает координатор
/// (см. [`OverlayInputPolicy`]).
///
/// Сообщением на поток окна, а не прямыми вызовами с потока координатора:
/// снятие захвата, запись списка областей и смена стиля выполняются ВНУТРИ
/// ОДНОГО обработчика. Поток окна разбирает сообщения по одному, поэтому
/// мышиному событию некуда вклиниться между шагами.
///
/// Именно этого не хватало прежнему `set_hit_rects`: он менял стиль сразу, а
/// список — позже, отдельным сообщением. В промежутке стиль уже «не
/// прозрачен», а список ещё пуст, и обычный путь ставил `SetCapture` —
/// монопольный захват всей мыши системы (аудит 2026-09-11,
/// `scratchpad/y1_audit_report.md` §2.4.1; живой инцидент — мышь залипла у
/// пользователя на всём компьютере).
///
/// `LPARAM` несёт `Box<OverlayInputPolicy>`; обработчик забирает владение.
const WM_APP_INPUT_POLICY: u32 = WM_APP + 5;

/// Смещение кода угла поворота в кодировке `WPARAM` — коды `0..ROTATE_BASE`
/// заняты фиксированными формами, `ROTATE_BASE + N` (`N` — 0..359) кодирует
/// `CursorShape::Rotate` под произвольным углом (фидбэк пользователя
/// 2026-08-09, третий раунд: угол курсора поворота больше не одна из 4
/// констант, а считается динамически от направления угла рамки в
/// пространстве).
const ROTATE_WPARAM_BASE: usize = 1000;

fn cursor_shape_to_wparam(shape: CursorShape) -> WPARAM {
    WPARAM(match shape {
        CursorShape::Arrow => 0,
        CursorShape::Move => 1,
        CursorShape::SizeNS => 2,
        CursorShape::SizeWE => 3,
        CursorShape::SizeNESW => 4,
        CursorShape::SizeNWSE => 5,
        CursorShape::Cross => 6,
        CursorShape::Rotate(angle_deg) => ROTATE_WPARAM_BASE + angle_deg.rem_euclid(360) as usize,
    })
}

fn cursor_shape_from_wparam(wparam: WPARAM) -> Option<CursorShape> {
    Some(match wparam.0 {
        0 => CursorShape::Arrow,
        1 => CursorShape::Move,
        2 => CursorShape::SizeNS,
        3 => CursorShape::SizeWE,
        4 => CursorShape::SizeNESW,
        5 => CursorShape::SizeNWSE,
        6 => CursorShape::Cross,
        n @ ROTATE_WPARAM_BASE..=ROTATE_WPARAM_MAX => {
            CursorShape::Rotate((n - ROTATE_WPARAM_BASE) as i32)
        }
        _ => return None,
    })
}

const ROTATE_WPARAM_MAX: usize = ROTATE_WPARAM_BASE + 359;

/// Какой из глобальных хоткеев не удалось зарегистрировать
/// (docs/M3_PREP_NOTES.md, раздел 3.3; M5d) — без этого различения тост/лог
/// конфликта всегда указывал бы на «режим редактирования», даже когда на
/// самом деле заняты `toggle_all_stickers`/`mute_all` (найдено при разборе
/// бага «программа живёт своей жизнью»: пользователь видел «хоткей режима
/// редактирования занят», хотя реально конфликтовал `mute_all`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyName {
    EditMode,
    ToggleAllStickers,
    MuteAll,
    /// Хоткей «закрепить/открепить сфокусированное окно» (редизайн пинов).
    PinFocusedWindow,
    /// Хоткей режима резки окон — «митоз»
    /// (docs/M9_WINDOW_MITOSIS_DESIGN.md).
    WindowMitosis,
    /// Хоткей отделения куска чужого окна (`Ctrl+Alt+C`, запрос пользователя
    /// 2026-09-10; `rst_core::model::StickerSource::WindowCrop`).
    WindowCrop,
}

/// Как окну оверлея ловить мышь.
///
/// Решает координатор — ОДИН раз за итерацию по полному состоянию, и
/// применяет одним вызовом [`OverlayWindow::apply_input_policy`]. Раньше
/// флаг `WS_EX_TRANSPARENT` писали четыре независимых механизма, и они
/// перезаписывали друг друга: закрытие одной панели выключало ввод другой, а
/// попиксельная кликопрозрачность возвращала прозрачность посреди режима
/// редактирования (аудит 2026-09-11, `scratchpad/y1_audit_report.md` §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayInputPolicy {
    /// Кликопрозрачно целиком: мышь идёт сквозь окно, захвата нет.
    Transparent,
    /// Интерактивно целиком. `take_focus` — забрать фокус; допускается
    /// только у одного окна за раз: два `SetForegroundWindow` подряд на
    /// разные окна дерутся между собой (M3_PREP_NOTES.md §3.5).
    Interactive { take_focus: bool },
    /// Ловить мышь только в этих прямоугольниках (КЛИЕНТСКИЕ координаты,
    /// физические пиксели), остальное — сквозь окно к окнам под ним.
    /// Захват мыши в этом режиме запрещён (см. гейт в `wndproc`).
    HitRects(Vec<(i32, i32, i32, i32)>),
    /// Ловить мышь всем окном, НЕ забирая фокус, с разрешённым захватом —
    /// ровно то, что прежде делал `set_hover_click_target(true)` для полосы
    /// перемотки видео.
    ///
    /// Отдельный режим, а не `HitRects` с прямоугольником полосы: ползунок
    /// перемотки тащат, и перетаскиванию нужен захват мыши, который в
    /// `HitRects` запрещён. Без захвата нажатие пришло бы одним путём, а
    /// отпускание другим, и флаг «тащат» мог бы не сброситься — окно
    /// осталось бы интерактивным на весь монитор. Этот режим воспроизводит
    /// прежнее рабочее поведение полосы бит в бит, но под единым владельцем
    /// флага прозрачности.
    HoverTarget,
}

/// Безопасное событие оверлей-окна для координатора (docs/M2_INTEGRATION_PLAN.md,
/// раздел 1): мышь и клавиатура уже переведены из сырых Win32-сообщений,
/// хоткеи — глобальные переключатели: вход/выход из режима редактирования
/// ([`Self::ToggleEditMode`]) и «показать/скрыть все стикеры»
/// ([`Self::ToggleAllStickers`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayEvent {
    /// Глобальный хоткей входа/выхода из режима редактирования нажат.
    ToggleEditMode,
    /// Глобальный хоткей «показать/скрыть все стикеры» нажат (M2b7).
    ToggleAllStickers,
    /// Глобальный хоткей «заглушить все стикеры» нажат (M5d).
    ToggleMuteAll,
    /// Глобальный хоткей «закрепить/открепить сфокусированное окно» нажат
    /// (редизайн пинов). Что делать с событием (какое окно в фокусе,
    /// денайлист, pin/unpin) — задача координатора.
    PinFocusedWindow,
    /// Открыть или закрыть меню редактирования групп (`Alt+Shift+G` по
    /// умолчанию). Один и тот же хоткей и открывает набор, и подтверждает
    /// его — решает координатор по тому, открыто ли меню.
    ToggleGroupsMenu,
    /// Удалить группу, которая сейчас открыта (`Ctrl+Alt+Shift+G`).
    DeleteOpenGroup,
    /// Открыть группу с этим номером, 1..=9 (`Ctrl+Shift+<цифра>`).
    OpenGroup(u8),
    /// Открепить все закреплённые окна разом (`Ctrl+Alt+U`).
    UnpinAll,
    /// Закрепить текущую (последнюю открытую) группу поверх всех окон —
    /// переключатель (`Ctrl+Alt+Shift+T`). Имя совпадает с
    /// `GroupVisibilityEvent::PinTogglePressed` в rst-core: координатору
    /// останется переложить событие один в один, не придумывая маппинг.
    PinTogglePressed,
    /// Пробел на видео-стикере под курсором: пауза/воспроизведение
    /// (запрос пользователя 2026-08-22). Приходит только пока
    /// медиа-хоткеи включены — см. [`OverlayWindow::set_media_hotkeys`].
    MediaPlayPause,
    /// PgUp на видео-стикере под курсором: громче.
    MediaVolumeUp,
    /// PgDn на видео-стикере под курсором: тише.
    MediaVolumeDown,
    /// Глобальный хоткей не удалось зарегистрировать: комбинация уже занята
    /// другим приложением. Строка — каноничный вид комбинации из конфига
    /// (ARCHITECTURE.md, раздел 5.1). Окно продолжает работать — недоступны
    /// только хоткеи, которые не удалось зарегистрировать.
    HotkeyConflict(HotkeyName, String),
    /// Полный набор постоянных хоткеев переустановлен на лету
    /// ([`OverlayWindow::reload_hotkeys`]): пользователь сменил бинды в
    /// настройках, и поток окна снял прежние комбинации и зарегистрировал
    /// новые. `registered` — сколько зарегистрировалось, `conflicts` — какие
    /// комбинации оказались заняты другими приложениями. Окно при этом
    /// продолжает работать; текст для пользователя формирует координатор.
    HotkeysReloaded {
        registered: usize,
        conflicts: Vec<HotkeyRegistrationConflict>,
    },
    /// Масштаб монитора сменился (`WM_DPICHANGED`): окно уже применило
    /// рекомендованный прямоугольник (`SetWindowPos`), `dpi` — новый DPI
    /// монитора (младшее слово `wParam`), `size` — новый размер окна в
    /// физических пикселях (docs/M3_PREP_NOTES.md, раздел 3.6).
    DpiChanged { dpi: u32, size: (u32, u32) },
    /// Событие мыши в клиентской области ([`crate::input::InputEvent`]).
    Input(InputEvent),
    /// Клавиша нажата/отпущена, пока окно в фокусе (режим редактирования —
    /// вне режима окно `WS_EX_NOACTIVATE` и фокус не получает).
    Key {
        vk: u32,
        modifiers: Modifiers,
        pressed: bool,
    },
    /// Введён символ (`WM_CHAR`) — то, что реально набрал пользователь с
    /// учётом раскладки, регистра и мёртвых клавиш.
    ///
    /// Отдельно от [`OverlayEvent::Key`]: по одному `vk` символ не
    /// восстановить (VK-коды букв — это ФИЗИЧЕСКИЕ клавиши раскладки US, и
    /// на русской раскладке из них получились бы латинские буквы). Пока
    /// этого события не было, текстовые поля панелей принимали только
    /// цифры — репорт пользователя 2026-08-24 про имя пресета.
    Char(char),
    /// Конфигурация мониторов изменилась (`WM_DISPLAYCHANGE`): свежий снапшот
    /// [`crate::monitors::enumerate`] целиком — сравнение «старое ↔ новое» по
    /// device interface path делает координатор (docs/M3_PREP_NOTES.md,
    /// разделы 2.3 и 3.6; здесь diff не выполняется).
    MonitorsChanged(Vec<MonitorInfo>),
    /// Сессия Windows заблокирована (`WM_WTSSESSION_CHANGE`, `WTS_SESSION_LOCK`).
    SessionLocked,
    /// Сессия Windows разблокирована (`WM_WTSSESSION_CHANGE`,
    /// `WTS_SESSION_UNLOCK`).
    SessionUnlocked,
    /// Система уходит в сон/гибернацию (`WM_POWERBROADCAST`, `PBT_APMSUSPEND`).
    SystemSuspending,
    /// Система вышла из сна (`PBT_APMRESUMESUSPEND` / `PBT_APMRESUMEAUTOMATIC`).
    SystemResumed,
    /// Включить или выключить режим резки окон — «митоз» (`Ctrl+Alt+F`,
    /// docs/M9_WINDOW_MITOSIS_DESIGN.md). Переключатель: одно и то же
    /// событие и входит в режим, и выходит из него — решает координатор по
    /// тому, активен ли режим сейчас.
    ToggleMitosisMode,
    /// Включить или выключить режим отделения куска чужого окна
    /// (`Ctrl+Alt+C`, запрос пользователя 2026-09-10;
    /// `rst_core::model::StickerSource::WindowCrop`). Переключатель — как
    /// [`Self::ToggleMitosisMode`]: одно и то же событие и входит в режим, и
    /// выходит из него, решает координатор по тому, активен ли режим сейчас.
    ToggleWindowCropMode,
}

/// Оверлей-окно на один монитор и его поток сообщений.
/// `Drop` уничтожает окно и останавливает поток.
pub struct OverlayWindow {
    hwnd: HWND,
    thread: Option<JoinHandle<()>>,
}

// HWND — просто числовой хэндл (isize), безопасно передавать между потоками.
// Уничтожение окна происходит только в `Drop` (с владением `OverlayWindow`),
// поэтому разделяемый `&OverlayWindow` даёт лишь чтение хэндла и размеров —
// `Sync` безопасен. Что делать с хэндлом дальше (рендер) — зона
// ответственности вызывающего кода.
unsafe impl Send for OverlayWindow {}
unsafe impl Sync for OverlayWindow {}

/// `HWND` не `Send` по умолчанию (внутри — `*mut c_void`), хотя это лишь
/// число. Обёртка нужна только для пересылки готового хэндла через канал.
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

/// Результат инициализации потока оверлея: хэндл окна.
type ReadyResult = Result<SendHwnd, Win32Error>;

impl OverlayWindow {
    /// Создаёт оверлей-окно на границы монитора `bounds_px` (физические
    /// пиксели виртуального десктопа, docs/M3_PREP_NOTES.md, раздел 3.1 —
    /// обычно `crate::monitors::MonitorInfo::bounds_px`) и запускает его
    /// цикл сообщений на отдельном потоке. На процесс может быть несколько
    /// окон (M3: окно на монитор).
    ///
    /// Глобальный хоткей входа/выхода из режима редактирования
    /// регистрируется **ровно один раз на процесс** (M3, раздел 3.3):
    /// `edit_hotkey = Some(combo)` — для окна, владеющего хоткеем (логично
    /// — окно основного монитора), `None` — для остальных, они создаются
    /// без регистрации и без конфликтов. `toggle_all_hotkey` — опциональный
    /// хоткей «показать/скрыть все стикеры» (M2b7): `None`, когда в конфиге
    /// пусто или комбинация не парсится. `mute_all_hotkey` — тот же паттерн
    /// для «заглушить все стикеры» (M5d, `hotkeys.mute_all`).
    /// `pin_focused_hotkey` — тот же опциональный паттерн для
    /// «закрепить/открепить сфокусированное окно» (редизайн пинов,
    /// `hotkeys.pin_focused_window`). Конфликт
    /// регистрации — не паника:
    /// окно работает, а наружу уходит событие
    /// [`OverlayEvent::HotkeyConflict`]. Возвращает управление, когда окно
    /// гарантированно создано, и приёмник событий мыши/клавиатуры/хоткеев —
    /// координатор объединяет его со своим каналом команд
    /// (docs/M2_INTEGRATION_PLAN.md, раздел 1).
    #[allow(clippy::too_many_arguments)]
    pub fn create_on_monitor(
        bounds_px: Rect,
        edit_hotkey: Option<HotkeyCombo>,
        toggle_all_hotkey: Option<HotkeyCombo>,
        mute_all_hotkey: Option<HotkeyCombo>,
        pin_focused_hotkey: Option<HotkeyCombo>,
    ) -> Result<(Self, Receiver<OverlayEvent>), Win32Error> {
        Self::create_on_monitor_with_groups(
            bounds_px,
            edit_hotkey,
            toggle_all_hotkey,
            mute_all_hotkey,
            pin_focused_hotkey,
            Vec::new(),
        )
    }

    /// То же плюс пакет хоткеев групп (`crate::hotkey::group_hotkey_combos`).
    ///
    /// Отдельный конструктор, а не шестой аргумент у существующего: групповых
    /// комбинаций одиннадцать, они приходят одним списком, и добавление
    /// параметра переписало бы два десятка тестовых вызовов ради `Vec::new()`
    /// в каждом.
    ///
    /// Пакет регистрируется через [`RegisteredHotkeySet::register_all`]:
    /// `Ctrl+Shift+<цифра>` занята во множестве приложений, и один занятый
    /// хоткей не должен мешать остальным.
    #[allow(clippy::too_many_arguments)]
    pub fn create_on_monitor_with_groups(
        bounds_px: Rect,
        edit_hotkey: Option<HotkeyCombo>,
        toggle_all_hotkey: Option<HotkeyCombo>,
        mute_all_hotkey: Option<HotkeyCombo>,
        pin_focused_hotkey: Option<HotkeyCombo>,
        group_hotkeys: Vec<(i32, HotkeyCombo)>,
    ) -> Result<(Self, Receiver<OverlayEvent>), Win32Error> {
        let (ready_tx, ready_rx) = mpsc::channel::<ReadyResult>();
        let (event_tx, event_rx) = mpsc::channel::<OverlayEvent>();

        let thread = thread::spawn(move || {
            run_message_loop(
                ready_tx,
                event_tx,
                bounds_px,
                edit_hotkey,
                toggle_all_hotkey,
                mute_all_hotkey,
                pin_focused_hotkey,
                group_hotkeys,
            )
        });

        let hwnd = ready_rx
            .recv()
            .map_err(|_| Win32Error::OverlayThreadCrashed)??;

        Ok((
            Self {
                hwnd: hwnd.0,
                thread: Some(thread),
            },
            event_rx,
        ))
    }

    /// Совместимость с единственным текущим вызывающим кодом
    /// (`overlay_manager.rs` — до подключения per-monitor создания в M3):
    /// окно на основной монитор с геометрией системного экрана
    /// (`GetSystemMetrics`), хоткей режима редактирования регистрируется
    /// безусловно. При миграции координатора на [`Self::create_on_monitor`]
    /// метод удаляется.
    pub fn create(
        edit_hotkey: HotkeyCombo,
        toggle_all_hotkey: Option<HotkeyCombo>,
    ) -> Result<(Self, Receiver<OverlayEvent>), Win32Error> {
        // SAFETY: GetSystemMetrics безопасен с любого потока, аргумент — константа.
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        Self::create_on_monitor(
            Rect {
                x: 0,
                y: 0,
                w: width.max(0) as u32,
                h: height.max(0) as u32,
            },
            Some(edit_hotkey),
            toggle_all_hotkey,
            None,
            None,
        )
    }

    /// Сырой `HWND` для передачи в `rst-render::Renderer::new(hwnd, width, height)`.
    /// Окном владеет поток оверлея: рисовать в него можно, уничтожать — нельзя.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Текущий размер окна в физических пикселях. Живое значение
    /// (`GetWindowRect`, процесс PerMonitorV2 — без растяжения ОС): меняется
    /// после `WM_DPICHANGED`/`SetWindowPos`, а не фиксируется на момент
    /// создания.
    pub fn size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        // SAFETY: hwnd — наше живое окно, уничтожается только в `Drop`;
        // GetWindowRect допустим с любого потока.
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        )
    }

    /// Текущий DPI монитора, на котором находится окно (96 = 100%).
    /// Используется для `Renderer::set_dpi_scale` и перевода координат мыши
    /// физика→DIP (M2, docs/M2_INTEGRATION_REVIEW.md, раздел 2). Живое
    /// значение (`GetDpiForWindow`): при `WM_DPICHANGED` окно переезжает на
    /// рекомендованный прямоугольник, и геттер сразу возвращает новый DPI;
    /// то же значение координатор получает заранее событием
    /// [`OverlayEvent::DpiChanged`] (M3, раздел 3.6).
    pub fn dpi(&self) -> u32 {
        // SAFETY: hwnd — наше живое окно.
        unsafe { GetDpiForWindow(self.hwnd) }
    }

    /// Переключить клик-прозрачность окна (ARCHITECTURE.md, раздел 5.2):
    /// `true` — вне режима редактирования, окно снова полностью
    /// клик-прозрачно и не берёт фокус; `false` — вход в режим
    /// редактирования, окно принимает мышь/клавиатуру на всей площади
    /// монитора и забирает фокус (`SetForegroundWindow`). На процесс с
    /// несколькими мониторами (M3) фокус имеет смысл забирать только у
    /// **одного** окна — того, что инициировало вход/выход (обычно то, что
    /// владеет глобальным хоткеем); остальные снимают клик-прозрачность без
    /// перетягивания фокуса через [`Self::set_interactive`], иначе несколько
    /// `SetForegroundWindow` подряд боролись бы друг с другом за фокус
    /// (M3_PREP_NOTES.md, раздел 3.5).
    pub fn set_click_through(&self, click_through: bool) {
        self.set_exstyle_bits(click_through);
        if !click_through {
            // SAFETY: hwnd — наше живое окно; вызов безопасен с любого потока.
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
    }

    /// Вырезать в окне оверлея дыру под прямоугольником `hole` (экранные
    /// физические пиксели) — или убрать вырез (`None`).
    ///
    /// Зачем: в режиме редактирования оверлей растянут на весь монитор и НЕ
    /// кликопрозрачен, поэтому любое окно поверх него (окно настроек) не
    /// получало бы ни кликов, ни колеса — даже будучи topmost, оно уходит
    /// под оверлей, как только пользователь щёлкнет по сцене и активирует
    /// его (запрос пользователя 2026-08-23: «хочу тыкаться в настройки, не
    /// выходя из режима»). Регион окна решает это независимо от z-order:
    /// система не считает вырезанную область принадлежащей окну ни при
    /// отрисовке, ни при хит-тесте.
    ///
    /// Возвращает `false`, если система отказала (`SetWindowRgn`).
    pub fn set_hole(&self, hole: Option<(i32, i32, i32, i32)>) -> bool {
        // SAFETY: hwnd — наше живое окно; регион после SetWindowRgn
        // принадлежит системе, поэтому удаляем только временный.
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(self.hwnd, &mut rect).is_err() {
                return false;
            }
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            let Some((hx, hy, hr, hb)) = hole else {
                return SetWindowRgn(self.hwnd, None, true) != 0;
            };
            // Пересечение в координатах окна; пустое — выреза нет.
            let (lx, ly) = (hx - rect.left, hy - rect.top);
            let (rx, ry) = (hr - rect.left, hb - rect.top);
            let (lx, ly) = (lx.max(0), ly.max(0));
            let (rx, ry) = (rx.min(w), ry.min(h));
            if rx <= lx || ry <= ly {
                return SetWindowRgn(self.hwnd, None, true) != 0;
            }
            let full = CreateRectRgn(0, 0, w, h);
            let cut = CreateRectRgn(lx, ly, rx, ry);
            let _ = CombineRgn(Some(full), Some(full), Some(cut), RGN_DIFF);
            let _ = DeleteObject(cut.into());
            let ok = SetWindowRgn(self.hwnd, Some(full), true) != 0;
            if !ok {
                let _ = DeleteObject(full.into());
            }
            ok
        }
    }

    /// То же переключение `WS_EX_TRANSPARENT|WS_EX_NOACTIVATE`, что и
    /// [`Self::set_click_through`], но без `SetForegroundWindow` — для окон
    /// других мониторов при входе/выходе из режима редактирования (M3):
    /// клик-прозрачность должна сняться у всех окон сразу (иначе мышь
    /// проваливалась бы сквозь режим на них), а фокус — только у окна,
    /// которое инициировало переключение.
    pub fn set_interactive(&self, interactive: bool) {
        self.set_exstyle_bits(!interactive);
    }

    /// Принимать клики, НЕ забирая фокус и НЕ активируясь: снимается только
    /// `WS_EX_TRANSPARENT`, `WS_EX_NOACTIVATE` остаётся.
    ///
    /// Для таймлайна видео-стикера вне режима редактирования (запрос
    /// пользователя 2026-08-22): полоса перемотки обязана ловить клик, но
    /// оверлей при этом остаётся фоновым — пользователь мотает ролик, не
    /// теряя фокус в приложении, где работает.
    ///
    /// Клик-прозрачность — свойство ОКНА, а не области, поэтому вызывающий
    /// снимает её ровно на то время, пока курсор физически над полосой, и
    /// возвращает сразу же, как он ушёл: иначе оверлей на весь монитор
    /// начнёт перехватывать чужие клики.
    pub fn set_hover_click_target(&self, target: bool) {
        self.toggle_exstyle(WS_EX_TRANSPARENT.0, !target);
    }

    /// Применить политику ввода (см. [`OverlayInputPolicy`] и
    /// [`WM_APP_INPUT_POLICY`]).
    ///
    /// Вся работа — снять захват, записать области, сменить стиль, забрать
    /// фокус — делается на потоке окна внутри одного обработчика. Отсюда
    /// только постится сообщение.
    pub fn apply_input_policy(&self, policy: OverlayInputPolicy) {
        let ptr = Box::into_raw(Box::new(policy));
        // SAFETY: hwnd — наше живое окно; PostMessageW потокобезопасен,
        // владение боксом переходит обработчику.
        unsafe {
            if PostMessageW(
                Some(self.hwnd),
                WM_APP_INPUT_POLICY,
                WPARAM(0),
                LPARAM(ptr as isize),
            )
            .is_err()
            {
                // Сообщение не встало в очередь — забираем бокс обратно,
                // иначе это утечка на каждый несостоявшийся вызов.
                drop(Box::from_raw(ptr));
            }
        }
    }

    /// Включить/выключить временные хоткеи управления видео-стикером под
    /// курсором: пробел — пауза/воспроизведение, PgUp/PgDn — громкость
    /// (запрос пользователя 2026-08-22).
    ///
    /// Пока они включены, клавиши забраны у ВСЕЙ системы, поэтому включать
    /// их можно только на время наведения на конкретный стикер — иначе
    /// пробел перестанет работать во всех программах. Вне режима
    /// редактирования иначе никак: оверлей не получает фокус (и не должен),
    /// а низкоуровневый клавиатурный хук в этом проекте запрещён (ADR-009).
    pub fn set_media_hotkeys(&self, enabled: bool) {
        // SAFETY: hwnd — наше живое окно; PostMessageW потокобезопасен.
        unsafe {
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_APP_MEDIA_HOTKEYS,
                WPARAM(usize::from(enabled)),
                LPARAM(0),
            );
        }
    }

    /// Полностью заменить набор постоянных хоткеев окна без перезапуска
    /// (живой репорт 2026-08-26: пользователь сменил бинд открытия группы в
    /// настройках, а он не поменялся — регистрация была только при создании
    /// окна).
    ///
    /// `combos` — ВСЕ комбинации разом (режим редактирования, показ стикеров,
    /// звук, пин, группы), каждая под своим id; налетевший на место старой
    /// комбинации новый набор заменяет её целиком, а не дополняет. Снятие
    /// старых и регистрация новых происходят на потоке окна
    /// (`RegisterHotKey` привязан к потоку, [`RegisteredHotkey`] намеренно
    /// `!Send`) — сюда передаётся только описание, см.
    /// [`WM_APP_REREGISTER_HOTKEYS`].
    ///
    /// Безопасен, если окно уже закрыто: сообщение не доставится, и набор,
    /// не переданный потоку, освобождается здесь же. Итог перерегистрации
    /// (сколько зарегистрировалось, какие комбинации заняты) придёт событием
    /// [`OverlayEvent::HotkeysReloaded`].
    pub fn reload_hotkeys(&self, combos: Vec<(i32, HotkeyCombo)>) {
        // SAFETY: Box::into_raw передаёт владение потоку окна; если
        // PostMessageW не доставил сообщение (окно уже уничтожено),
        // владение возвращается сюда и освобождается. Доставленное сообщение
        // гарантированно обрабатывается: уничтожение окна идёт через ту же
        // очередь сообщений, FIFO.
        let ptr = Box::into_raw(Box::new(combos));
        let delivered = unsafe {
            PostMessageW(
                Some(self.hwnd),
                WM_APP_REREGISTER_HOTKEYS,
                WPARAM(0),
                LPARAM(ptr as isize),
            )
        };
        if delivered.is_err() {
            // SAFETY: ptr — наш, поток окна его не получил.
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    }

    /// Скрыть/показать окно для захвата экрана (SPEC.md, раздел 8):
    /// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`/`WDA_NONE`.
    /// Возвращает `Ok(true)`, если проверка (`GetWindowDisplayAffinity`)
    /// подтвердила, что аффинити реально применилась — на отдельных сборках
    /// Windows 11 вызов может «молча» не сработать (SPEC: «программа
    /// СЛЕДУЕТ проверять результат и предупреждать»); `Ok(false)` — вызов
    /// не вернул ошибку, но проверка показала иное значение; `Err` — сам
    /// вызов `SetWindowDisplayAffinity` завершился с ошибкой.
    pub fn set_capture_affinity(&self, hide: bool) -> Result<bool, Win32Error> {
        let wanted = if hide {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        // SAFETY: hwnd — наше живое окно; аффинити окна можно менять с
        // любого потока.
        unsafe { SetWindowDisplayAffinity(self.hwnd, wanted) }?;
        let mut actual: u32 = 0;
        // SAFETY: `actual` — валидный out-параметр на весь вызов.
        unsafe { GetWindowDisplayAffinity(self.hwnd, &mut actual) }?;
        Ok(actual == wanted.0)
    }

    /// Общая часть `set_click_through`/`set_interactive`: переключить
    /// `WS_EX_TRANSPARENT|WS_EX_NOACTIVATE` без побочных эффектов на фокус.
    fn set_exstyle_bits(&self, click_through: bool) {
        self.toggle_exstyle(WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0, click_through);
    }

    /// Выставить/снять произвольные биты `GWL_EXSTYLE` с обязательным
    /// `SWP_FRAMECHANGED` (см. ниже, почему без него смена не вступает в
    /// силу).
    fn toggle_exstyle(&self, bits: u32, set: bool) {
        apply_exstyle(self.hwnd, bits, set);
    }
}

/// Выставить биты `set` и снять биты `clear` в `GWL_EXSTYLE` ОДНОЙ записью, с
/// обязательным `SWP_FRAMECHANGED` (см. [`apply_exstyle`]).
fn apply_exstyle_masks(hwnd: HWND, set: u32, clear: u32) {
    // SAFETY: hwnd — наше живое окно; смена GWL_EXSTYLE безопасна с любого
    // потока (в отличие от владения самим HWND).
    unsafe {
        let ex = (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & !clear) | set;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Выставить/снять биты `GWL_EXSTYLE` с обязательным `SWP_FRAMECHANGED`.
///
/// Свободная функция, а не метод: её зовёт и обёртка на `OverlayWindow`, и
/// обработчик [`WM_APP_INPUT_POLICY`] прямо на потоке окна, где
/// `OverlayWindow` нет — только `HWND`.
fn apply_exstyle(hwnd: HWND, bits: u32, set: bool) {
    {
        // SAFETY: hwnd — наше живое окно; смена GWL_EXSTYLE безопасна с
        // любого потока (в отличие от владения самим HWND).
        unsafe {
            let mut ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if set {
                ex |= bits;
            } else {
                ex &= !bits;
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
            // SetWindowLongPtrW само по себе не гарантирует, что менеджер
            // окон немедленно перечитает кэшированные ex-стили (MS Learn:
            // «Some window data is cached, so changes you make ... will not
            // take effect until you call SetWindowPos»); SWP_FRAMECHANGED —
            // штатный способ форсировать пересчёт без реального
            // перемещения/ресайза/z-order/фокуса (найдено брейнштормом
            // ботов-воркеров, 2026-08-09).
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

impl OverlayWindow {
    /// Переставить и/или изменить размер уже созданного окна под новые
    /// физические границы монитора — тот же монитор (`MonitorId` не
    /// поменялся), но сменилось разрешение/позиция в `WM_DISPLAYCHANGE`
    /// (M3_HOTPLUG_DESIGN.md, известный gap: раньше на такое событие окно не
    /// реагировало вовсе). Приём — как в `handle_dpi_changed`: `SWP_NOZORDER`
    /// сохраняет `WS_EX_TOPMOST` в стеке окон, `SWP_NOACTIVATE` не трогает
    /// фокус. Возвращает `false` при отказе `SetWindowPos` (редкий системный
    /// сбой) — вызывающий код логирует и оставляет старую геометрию.
    pub fn set_bounds(&self, bounds_px: Rect) -> bool {
        // SAFETY: hwnd — наше живое окно; SetWindowPos безопасен с любого потока.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                bounds_px.x,
                bounds_px.y,
                bounds_px.w as i32,
                bounds_px.h as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .is_ok()
        }
    }

    /// Поднять оверлей над закреплёнными окнами, если хоть одно из них
    /// стоит ВЫШЕ него в z-order.
    ///
    /// Зачем: закреплённое окно тоже живёт в topmost-полосе
    /// ([`crate::window_pin::WindowPins::pin`] ставит ему `WS_EX_TOPMOST`), а
    /// система при активации кладёт активное окно на верх полосы — выше
    /// нашего оверлея. Вся графика, которую оверлей рисует поверх окна
    /// (бейдж «закреплено», индикаторы замков, рамка-пульс), уходит под окно
    /// и становится невидимой; пользователь видит её лишь мгновение в
    /// момент пина (репорт 2026-08-21).
    ///
    /// Условие «выше нас стоит именно НАШ пин», а не «мы не первые»:
    /// проверять `GW_HWNDPREV == null` бессмысленно — над оверлеем всегда
    /// есть системные окна, и такая проверка вырождалась бы в безусловный
    /// `SetWindowPos` на каждом снимке трекера (измерено воркером-
    /// исследователем 2026-08-21: `GetWindow` — 0.3–0.5 мкс,
    /// `SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE)` — 14–16 мкс). Обход
    /// вверх дешевле и заодно сам себя останавливает: после подъёма пин
    /// оказывается ниже, следующий снимок ничего не находит и не трогает
    /// z-order — z-order-войны с чужими topmost-приложениями не возникает.
    ///
    /// `SWP_NOACTIVATE` обязателен: оверлей `WS_EX_NOACTIVATE` и фокус не
    /// забирает. Возвращает `true`, если подъём реально выполнялся.
    pub fn raise_above_pinned(&self, pinned: &[usize]) -> bool {
        if pinned.is_empty() {
            return false;
        }
        // SAFETY: hwnd — наше живое окно; GetWindow — чтение z-order.
        let mut above = unsafe { GetWindow(self.hwnd, GW_HWNDPREV) };
        let mut found = false;
        while let Ok(hwnd) = above {
            if hwnd.0.is_null() {
                break;
            }
            if pinned.contains(&(hwnd.0 as usize)) {
                found = true;
                break;
            }
            // SAFETY: hwnd получен из GetWindow, чтение z-order живого окна.
            above = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
        }
        if !found {
            return false;
        }
        // SAFETY: наше окно; SetWindowPos без активации и без движения.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
            .is_ok()
        }
    }

    /// Попросить поток оверлея сменить форму курсора (зона под курсором
    /// вычисляется координатором, не Win32-потоком, ARCHITECTURE.md 5.3).
    pub fn post_cursor_shape(&self, shape: CursorShape) {
        // SAFETY: hwnd — наше окно; PostMessage безопасен с любого потока.
        unsafe {
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_APP_EDIT_CURSOR,
                cursor_shape_to_wparam(shape),
                LPARAM(0),
            );
        }
    }

    /// Попросить поток оверлея безусловно снять Win32-захват мыши —
    /// защитная мера на выходе из режима редактирования (см.
    /// [`WM_APP_RELEASE_CAPTURE`]): если хоткей выхода сработал, пока
    /// пользователь ещё держит кнопку мыши (тянет ползунок/жест), обычный
    /// цикл Down→Up, который снял бы захват сам, не наступит вовремя, и
    /// уже клик-прозрачное окно продолжает монопольно получать всю мышь
    /// системы.
    pub fn force_release_capture(&self) {
        // SAFETY: hwnd — наше окно; PostMessage безопасен с любого потока.
        unsafe {
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_APP_RELEASE_CAPTURE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // Окно принадлежит потоку оверлея, поэтому уничтожает его он сам:
        // шлём WM_CLOSE, DefWindowProc вызовет DestroyWindow на его потоке,
        // а наш обработчик WM_DESTROY завершит цикл сообщений. Прямой вызов
        // DestroyWindow отсюда недопустим: окно, созданное другим потоком,
        // он не уничтожает, и join() ниже завис бы навсегда.
        // SAFETY: hwnd — наше окно; PostMessage безопасен и для уже
        // уничтоженного окна (просто вернёт ошибку, которую игнорируем).
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Удержание оверлея наверху topmost-полосы
// ---------------------------------------------------------------------------

/// Идентификатор таймера, который держит оверлей наверху topmost-полосы.
const TOPMOST_TIMER_ID: usize = 1;

/// Период проверки z-order, мс.
///
/// Сама проверка стоит микросекунды (`GetWindow` — 0.3–0.5 мкс на окно,
/// замер 2026-08-21), поэтому период выбран по времени реакции глаза, а не
/// по цене: накрытый чужим окном стикер возвращается наверх за долю секунды
/// и это читается как «не пропадал».
const TOPMOST_TIMER_MS: u32 = 400;

/// Окно меньше этого по любой стороне за помеху не считается: в
/// topmost-полосе постоянно висят служебные окна 1×1 (замер 2026-09-02: два
/// `ThumbnailDeviceHelperWnd` проводника, всегда выше всех). Они ничего не
/// закрывают, а реагировать на них значило бы дёргать `SetWindowPos` каждые
/// [`TOPMOST_TIMER_MS`] до конца сеанса.
const TOPMOST_MIN_INTRUDER_PX: i32 = 8;

/// Сколько тиков подряд одно и то же чужое окно отвоёвывает верх, прежде чем
/// мы перестаём с ним бороться.
///
/// Приложение, которое тоже держит себя наверху по таймеру, иначе устроило бы
/// с нами бесконечную перестановку — мигание несколько раз в секунду, которое
/// выглядит хуже честно уступленного верха.
const TOPMOST_WAR_STRIKES: u32 = 8;

/// Решение «возвращаться ли наверх» на один тик таймера — без Win32, чтобы
/// перестановочная война проверялась тестом, а не глазами.
#[derive(Debug, Default)]
struct TopmostGuard {
    /// Кого поднимали на прошлом тике.
    last: Option<usize>,
    /// Сколько тиков подряд он возвращается.
    strikes: u32,
    /// Кому уступили верх: с этим окном больше не боремся, пока оно не уйдёт
    /// с нашей дороги само.
    surrendered: Option<usize>,
}

impl TopmostGuard {
    /// `intruder` — чужое видимое окно над оверлеем, реально его
    /// закрывающее; `None` — над нами чисто. `true` — сейчас стоит вызвать
    /// `SetWindowPos(HWND_TOPMOST)`.
    fn decide(&mut self, intruder: Option<usize>) -> bool {
        let Some(hwnd) = intruder else {
            // Верх наш — счётчики прошлой борьбы больше ни о чём не говорят.
            *self = Self::default();
            return false;
        };
        if self.last != Some(hwnd) {
            self.last = Some(hwnd);
            self.strikes = 0;
            self.surrendered = None;
        }
        if self.surrendered == Some(hwnd) {
            return false;
        }
        self.strikes += 1;
        if self.strikes > TOPMOST_WAR_STRIKES {
            self.surrendered = Some(hwnd);
            tracing::warn!(
                hwnd,
                "чужое окно удерживает верх topmost-полосы — уступаем, чтобы не мигать"
            );
            return false;
        }
        true
    }
}

/// Чужое окно `other` закрывает оверлей `ours`? Оба прямоугольника — в
/// экранных координатах с семантикой `RECT` (`right`/`bottom` исключительно).
fn intruder_covers(ours: (i32, i32, i32, i32), other: (i32, i32, i32, i32)) -> bool {
    let (l, t, r, b) = other;
    if r - l < TOPMOST_MIN_INTRUDER_PX || b - t < TOPMOST_MIN_INTRUDER_PX {
        return false;
    }
    let (ol, ot, orr, ob) = ours;
    l < orr && r > ol && t < ob && b > ot
}

/// Одно окно над нами: чужое, видимое, не скрытое DWM и пересекается с
/// оверлеем?
///
/// # Safety
/// `other` — окно, полученное обходом z-order, живо на время вызова.
unsafe fn covers_overlay(other: HWND, our_pid: u32, ours: (i32, i32, i32, i32)) -> bool {
    // SAFETY: чтение свойств живого окна.
    if !unsafe { IsWindowVisible(other) }.as_bool() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: то же; pid — наша переменная на стеке.
    unsafe { GetWindowThreadProcessId(other, Some(&mut pid)) };
    if pid == our_pid {
        // Наши же оверлеи на соседних мониторах помехой не считаются.
        return false;
    }
    let mut rect = RECT::default();
    // SAFETY: то же.
    if unsafe { GetWindowRect(other, &mut rect) }.is_err() {
        return false;
    }
    if !intruder_covers(ours, (rect.left, rect.top, rect.right, rect.bottom)) {
        return false;
    }
    // Скрытые DWM окна (свёрнутые UWP и прочее) видимы по `IsWindowVisible`,
    // но не нарисованы — тот же фильтр, что у перечисления окон
    // (`window_enum::is_real_window`).
    let mut cloaked = 0u32;
    // SAFETY: DWMWA_CLOAKED пишет ровно `u32` по переданному указателю.
    let cloaked_ok = unsafe {
        DwmGetWindowAttribute(
            other,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok();
    !(cloaked_ok && cloaked != 0)
}

/// Найти над оверлеем чужое окно, которое его закрывает.
///
/// Всё, что стоит выше нас, — тоже topmost (система держит полосу цельной),
/// поэтому стиль не проверяем: достаточно, что окно видимо, не наше, не
/// скрыто DWM и пересекается с нами.
///
/// # Safety
/// `hwnd` — живое окно оверлея.
unsafe fn intruder_above(hwnd: HWND) -> Option<usize> {
    let mut ours = RECT::default();
    // SAFETY: наше живое окно.
    if unsafe { GetWindowRect(hwnd, &mut ours) }.is_err() {
        return None;
    }
    let ours = (ours.left, ours.top, ours.right, ours.bottom);
    // SAFETY: чтение идентификатора собственного процесса.
    let our_pid = unsafe { GetCurrentProcessId() };
    // SAFETY: чтение z-order живого окна.
    let mut above = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
    while let Ok(other) = above {
        if other.0.is_null() {
            break;
        }
        // SAFETY: `other` получен обходом z-order и живёт на время проверки.
        if unsafe { covers_overlay(other, our_pid, ours) } {
            return Some(other.0 as usize);
        }
        // SAFETY: то же.
        above = unsafe { GetWindow(other, GW_HWNDPREV) };
    }
    None
}

/// Тик таймера: вернуть оверлей наверх topmost-полосы, если его оттуда
/// вытеснили.
///
/// `WS_EX_TOPMOST` — это не «выше всех», а «в верхней полосе». Любое чужое
/// topmost-окно, созданное или активированное позже нашего, встаёт НАД
/// оверлеем и остаётся там навсегда: стикер оказывается «между окнами» — над
/// обычными, под этим. Замер 2026-09-02: чужое topmost-окно перекрывает
/// оверлей, и через восемь секунд resticker сам наверх не возвращается.
/// Заметнее всего после перезагрузки компьютера — оверлей стартует рано, а
/// автозапуск чужих приложений происходит позже (репорт пользователя
/// 2026-09-02: «после перезапуска стикеры отображаются только между
/// некоторыми окнами»).
///
/// Здесь, на потоке окна по таймеру, а не у координатора: у координатора нет
/// собственного пульса — его цикл спит на `recv_timeout` до часа
/// (`IDLE_POLL`), а трекер окон на конфиге, где все стикеры `Always`, не
/// просыпается вовсе (`tracker_mask_needed`). То есть ровно в том случае,
/// который здесь чинится, поднимать оверлей было бы некому.
///
/// [`OverlayWindow::raise_above_pinned`] этого не заменяет: та функция знает
/// только про НАШИ закреплённые окна и молчит, когда их нет.
///
/// # Safety
/// `hwnd` — живое окно оверлея.
unsafe fn keep_topmost(hwnd: HWND, guard: &mut TopmostGuard) {
    // SAFETY: наше живое окно.
    let intruder = unsafe { intruder_above(hwnd) };
    if !guard.decide(intruder) {
        return;
    }
    // SAFETY: наше окно; без активации, движения и изменения размера.
    let raised = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
    if let Err(e) = raised {
        tracing::warn!(error = %e, "не удалось вернуть оверлей наверх topmost-полосы");
    }
}

/// Состояние, живущее на потоке оверлея между сообщениями: захват мыши,
/// курсор и канал событий. Хранится через `GWLP_USERDATA` (стандартный Win32
/// паттерн — `wndproc` не может захватывать переменные, это `extern "system"
/// fn`), владение — у `run_message_loop`, освобождается в `WM_NCDESTROY`.
struct WndState {
    capture: MouseCapture,
    /// Прямоугольники попиксельной кликопрозрачности в КЛИЕНТСКИХ
    /// координатах окна (физические пиксели) — см. [`WM_APP_INPUT_POLICY`].
    /// Пустой список означает «правило не действует»: окно ведёт себя как
    /// раньше, и режим редактирования, делающий его интерактивным целиком,
    /// ничего не замечает.
    hit_rects: Vec<RECT>,
    cursor: CursorManager,
    tx: Sender<OverlayEvent>,
    /// Временные хоткеи управления видео (см. [`MEDIA_PLAY_PAUSE_HOTKEY_ID`]).
    /// Живут здесь, потому что `RegisteredHotkey` привязан к потоку окна —
    /// а `WndState` живёт ровно на нём.
    media_hotkeys: Vec<crate::hotkey::RegisteredHotkey>,
    /// ВСЕ постоянные хоткеи окна (режим редактирования, стикеры, звук, пин,
    /// группы) — их владельцы. Поменять набор на лету — снять прежних
    /// (`clear()`, Drop зовёт `UnregisterHotKey` на этом потоке) и добавить
    /// новых; это делает [`install_hotkeys`] на `WM_APP_REREGISTER_HOTKEYS`.
    /// Медиа-хоткеи сюда намеренно НЕ входят — они временные, живут в
    /// [`Self::media_hotkeys`].
    hotkeys: Vec<crate::hotkey::RegisteredHotkey>,
    /// id хоткеев, которые ЭТОТ поток реально зарегистрировал, — разбор
    /// `WM_HOTKEY` решает по ним, своё ли это сообщение. Живёт здесь, а не в
    /// цикле сообщений, потому что перерегистрация происходит в `wndproc`, и
    /// набор обязан обновляться вместе с регистрацией — два хранилища
    /// разошлись бы ровно так, как это уже случалось (см. доккомент у
    /// `install_hotkeys`).
    owned: std::collections::HashSet<i32>,
    /// Id, которые обслуживает низкоуровневый клавиатурный хук
    /// ([`crate::hotkey_hook`]) вместо `RegisterHotKey`, — те, чьи
    /// комбинации занял кто-то другой. Разбираются они ровно так же, как
    /// настоящие: хук кладёт в очередь то же самое `WM_HOTKEY`.
    hooked: std::collections::HashSet<i32>,
    /// Постоянные комбинации, которые не отдала `RegisterHotKey`.
    fallback_permanent: Vec<(i32, HotkeyCombo)>,
    /// То же для временных медиа-клавиш (пробел/PgUp/PgDn на видео-стикере
    /// под курсором). Отдельный вектор, потому что живут они по другому
    /// расписанию — включаются и гаснут по наведению курсора, и путать их с
    /// постоянными нельзя: набор хука собирается из обоих сразу.
    fallback_media: Vec<(i32, HotkeyCombo)>,
    /// Верхняя половина суррогатной пары из предыдущего `WM_CHAR`: символы
    /// вне BMP (эмодзи) Windows шлёт двумя сообщениями.
    pending_surrogate: Option<u16>,
    /// Состояние удержания верха topmost-полосы ([`keep_topmost`]).
    topmost: TopmostGuard,
}

/// Собрать символ из `WM_CHAR`: обычный код возвращается сразу, суррогатная
/// пара — по второму сообщению (первое запоминается в `pending`).
///
/// Чистая функция ради тестов: суррогатные пары приходят редко, а ломаются
/// молча — проверять их на живой машине эмодзи неудобно.
fn char_from_wm_char(unit: u16, pending: &mut Option<u16>) -> Option<char> {
    const HIGH: std::ops::Range<u16> = 0xD800..0xDC00;
    const LOW: std::ops::Range<u16> = 0xDC00..0xE000;
    if let Some(high) = pending.take() {
        if LOW.contains(&unit) {
            let code = 0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
            return char::from_u32(code);
        }
        // Пара разорвана (так быть не должно) — вторую половину разбираем
        // как самостоятельный символ, а первую выбрасываем.
    }
    if HIGH.contains(&unit) {
        *pending = Some(unit);
        return None;
    }
    char::from_u32(u32::from(unit))
}

/// Полный набор постоянных хоткеев по конфигу — то, что принимает
/// [`OverlayWindow::reload_hotkeys`].
///
/// Живёт здесь, а не у координатора, потому что номера (`EDIT_HOTKEY_ID` и
/// соседи) — внутреннее дело этого модуля: он же их и разбирает в `WM_HOTKEY`.
/// Заставлять координатора собирать пары «номер + комбинация» значило бы
/// разложить одно знание по двум крейтам, и первое же добавление хоткея
/// разошлось бы между ними.
///
/// Медиа-хоткеи сюда не входят: они временные, регистрируются и снимаются по
/// наведению курсора на стикер (`WM_APP_MEDIA_HOTKEYS`), и смена биндов в
/// настройках их не касается.
pub fn all_hotkey_combos(hotkeys: &Hotkeys) -> Vec<(i32, HotkeyCombo)> {
    let mut combos = Vec::new();
    for (id, raw) in [
        (EDIT_HOTKEY_ID, hotkeys.edit_mode.as_deref()),
        (TOGGLE_ALL_HOTKEY_ID, hotkeys.toggle_all_stickers.as_deref()),
        (MUTE_ALL_HOTKEY_ID, hotkeys.mute_all.as_deref()),
        (PIN_FOCUSED_HOTKEY_ID, hotkeys.pin_focused_window.as_deref()),
    ] {
        if let Some(combo) = raw.and_then(|s| HotkeyCombo::parse(s).ok()) {
            combos.push((id, combo));
        }
    }
    combos.extend(crate::hotkey::group_hotkey_combos(hotkeys));
    combos
}

/// Имя глобального хоткея по его id — для сообщения о конфликте пользователю.
/// Медиа-хоткеи (5..=7) и хоткеи групп (8..) сюда не входят: у медиа
/// конфликт не сообщается вовсе (см. `WM_APP_MEDIA_HOTKEYS`), а групповые
/// перечислены отдельным списком [`HotkeyRegistrationConflict`], который
/// координатор разбирает по `id` сам.
fn hotkey_name_of(id: i32) -> Option<HotkeyName> {
    match id {
        EDIT_HOTKEY_ID => Some(HotkeyName::EditMode),
        TOGGLE_ALL_HOTKEY_ID => Some(HotkeyName::ToggleAllStickers),
        MUTE_ALL_HOTKEY_ID => Some(HotkeyName::MuteAll),
        PIN_FOCUSED_HOTKEY_ID => Some(HotkeyName::PinFocusedWindow),
        // Митоз регистрируется пакетом хоткеев групп, но конфликт по нему
        // сообщать НАДО: это самостоятельная функция программы, а не
        // одна из девяти взаимозаменяемых цифр.
        crate::hotkey::MITOSIS_HOTKEY_ID => Some(HotkeyName::WindowMitosis),
        // Ровно по той же причине, что митоз: самостоятельная функция
        // программы, а не одна из девяти взаимозаменяемых цифр, — молчать
        // о занятой комбинации нельзя.
        crate::hotkey::WINDOW_CROP_HOTKEY_ID => Some(HotkeyName::WindowCrop),
        _ => None,
    }
}

/// Отчёт об установке набора хоткеев: сколько зарегистрировалось и какие
/// комбинации оказались заняты другими приложениями.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HotkeysInstallReport {
    registered: usize,
    conflicts: Vec<HotkeyRegistrationConflict>,
}

/// Установить полный набор постоянных хоткеев окна: снять прежние,
/// зарегистрировать новые и обновить `owned`. Вызывается при создании окна
/// и на каждое `WM_APP_REREGISTER_HOTKEYS`.
///
/// Два требования, которые собирает эта функция:
/// * `RegisterHotKey` привязан к ПОТОКУ (`RegisteredHotkey` намеренно
///   `!Send`), поэтому снятие и установка происходят здесь, на потоке окна;
/// * старые хоткеи снимаются ДО регистрации новых — иначе новая комбинация,
///   совпадающая со старой, получила бы конфликт сама с собой.
///
/// Пакетная регистрация (`RegisteredHotkeySet::register_all`) изолирует
/// конфликты: один занятый хоткей не мешает остальным, а его id и комбинация
/// попадают в отчёт.
/// Пересобрать набор комбинаций низкоуровневого хука из обоих источников и
/// обновить список id, которые он обслуживает.
///
/// Одной функцией, а не двумя по месту: хук в потоке ОДИН, и его набор —
/// объединение постоянных и медиа-комбинаций. Обновлять его из двух мест
/// независимо значило бы, что второе стирает работу первого.
fn refresh_hotkey_hook(state: &mut WndState) {
    let mut combos = state.fallback_permanent.clone();
    combos.extend(state.fallback_media.iter().cloned());
    state.hooked = combos.iter().map(|(id, _)| *id).collect();
    crate::hotkey_hook::set_fallback_combos(combos);
}

/// Комбинации, которые не удалось зарегистрировать, — вход для хука.
///
/// Отчёт несёт id и текстовый вид комбинации, а хуку нужна разобранная
/// [`HotkeyCombo`]; берём её из того же набора, который и пытались
/// зарегистрировать, — так исключён разбор строки обратно.
fn conflicting_combos(
    combos: &[(i32, HotkeyCombo)],
    report: &HotkeysInstallReport,
) -> Vec<(i32, HotkeyCombo)> {
    report
        .conflicts
        .iter()
        .filter_map(|c| combos.iter().find(|(id, _)| *id == c.id).cloned())
        .collect()
}

fn install_hotkeys(state: &mut WndState, combos: &[(i32, HotkeyCombo)]) -> HotkeysInstallReport {
    let report = register_hotkeys(state, combos);
    // Занятые комбинации не теряются: их подхватывает низкоуровневый хук
    // ([`crate::hotkey_hook`]). Здесь, а не у вызывающего, потому что
    // вызывающих двое (создание окна и перерегистрация из настроек), и
    // забытый вызов в одном из них выглядел бы как «хоткей работает, пока
    // не откроешь настройки».
    state.fallback_permanent = conflicting_combos(combos, &report);
    refresh_hotkey_hook(state);
    report
}

/// Собственно регистрация набора через `RegisterHotKey` — без запасного пути.
fn register_hotkeys(state: &mut WndState, combos: &[(i32, HotkeyCombo)]) -> HotkeysInstallReport {
    // Снятие прежних — Drop зовёт UnregisterHotKey на этом же потоке.
    state.hotkeys.clear();
    state.owned.clear();
    match RegisteredHotkeySet::register_all(combos.iter().copied()) {
        Ok(mut set) => {
            state.owned = set.registered_ids();
            state.hotkeys.append(&mut set.take_hotkeys());
            HotkeysInstallReport {
                registered: state.hotkeys.len(),
                conflicts: set.conflicts().to_vec(),
            }
        }
        Err(e) => {
            // Не-конфликтная ошибка (например, id вне диапазона) —
            // программистская, а не занятая пользователем комбинация.
            tracing::warn!(error = %e, "не удалось зарегистрировать набор хоткеев");
            HotkeysInstallReport {
                registered: 0,
                conflicts: Vec::new(),
            }
        }
    }
}

/// Номер группы по id хоткея, или `None`, если id не из девятки открытия.
///
/// Обратная операция к [`crate::hotkey::GROUP_OPEN_HOTKEY_ID_BASE`]: разбор
/// `WM_HOTKEY` и регистрация обязаны считать номер одинаково, иначе хоткей
/// открывал бы не ту группу.
fn group_number_of(id: i32) -> Option<u8> {
    let base = crate::hotkey::GROUP_OPEN_HOTKEY_ID_BASE;
    let slots = rst_core::model::Hotkeys::GROUP_OPEN_SLOTS as i32;
    (base..base + slots)
        .contains(&id)
        .then(|| (id - base + 1) as u8)
}

#[allow(clippy::too_many_arguments)]
fn run_message_loop(
    ready_tx: Sender<ReadyResult>,
    event_tx: Sender<OverlayEvent>,
    bounds_px: Rect,
    edit_hotkey: Option<HotkeyCombo>,
    toggle_all_hotkey: Option<HotkeyCombo>,
    mute_all_hotkey: Option<HotkeyCombo>,
    pin_focused_hotkey: Option<HotkeyCombo>,
    group_hotkeys: Vec<(i32, HotkeyCombo)>,
) {
    let hwnd = match create_window(bounds_px) {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // Клон для перехвата WM_HOTKEY прямо в цикле сообщений (см. ниже) —
    // wndproc его не увидит (сообщение с hwnd=NULL). Тем же каналом уходит
    // стартовый конфликт-отчёт.
    let hotkey_tx = event_tx.clone();

    let state = Box::new(WndState {
        capture: MouseCapture::new(hwnd),
        hit_rects: Vec::new(),
        cursor: CursorManager::new(),
        tx: event_tx,
        media_hotkeys: Vec::new(),
        hotkeys: Vec::new(),
        owned: std::collections::HashSet::new(),
        hooked: std::collections::HashSet::new(),
        fallback_permanent: Vec::new(),
        fallback_media: Vec::new(),
        pending_surrogate: None,
        topmost: TopmostGuard::default(),
    });
    // SAFETY: hwnd — наше окно этого потока; указатель освобождается в
    // WM_NCDESTROY ниже (единственное место, где он читается и дропается).
    let state_ptr = Box::into_raw(state);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
    }

    // Стартовый набор — все постоянные хоткеи программы одним пакетом.
    // `None` в аргументах конструктора означает «не регистрировать»: глобальные
    // хоткеи регистрирует ровно одно окно на процесс (M3, docs/M3_PREP_NOTES.md,
    // раздел 3.3), остальные создаются без них.
    let mut initial_combos: Vec<(i32, HotkeyCombo)> = Vec::new();
    if let Some(combo) = edit_hotkey {
        initial_combos.push((EDIT_HOTKEY_ID, combo));
    }
    if let Some(combo) = toggle_all_hotkey {
        initial_combos.push((TOGGLE_ALL_HOTKEY_ID, combo));
    }
    if let Some(combo) = mute_all_hotkey {
        initial_combos.push((MUTE_ALL_HOTKEY_ID, combo));
    }
    if let Some(combo) = pin_focused_hotkey {
        initial_combos.push((PIN_FOCUSED_HOTKEY_ID, combo));
    }
    initial_combos.extend(group_hotkeys);

    // Хоткеи — на этом же потоке (тип `!Send`, ADR-009). Конфликт окно не
    // ломает (ARCHITECTURE.md, раздел 5.1), но наружу уходит событием
    // `OverlayEvent::HotkeyConflict`, чтобы координатор мог предупредить
    // пользователя, а не только warn-лог в трассировке (M2b6). Групповые
    // конфликты (id >= 8) только логируются: их набор большой, и каждый раз
    // слать отдельное событие — шум; понадобится — координатор прочитает их
    // из `HotkeysReloaded` при следующей перерегистрации.
    // SAFETY: state_ptr установлен строкой выше и живёт до WM_NCDESTROY.
    {
        let state = unsafe { state_ptr.as_mut() }.expect("state установлен выше");
        let report = install_hotkeys(state, &initial_combos);
        for conflict in &report.conflicts {
            if let Some(name) = hotkey_name_of(conflict.id) {
                let _ = hotkey_tx.send(OverlayEvent::HotkeyConflict(name, conflict.combo.clone()));
            } else {
                tracing::warn!(
                    id = conflict.id,
                    combo = %conflict.combo,
                    "хоткей занят другим приложением"
                );
            }
        }
    }

    if ready_tx.send(Ok(SendHwnd(hwnd))).is_err() {
        // Получатель уже отброшен (конструктор вернул ошибку раньше) —
        // корректно свернуться, не оставляя окно висеть.
        // SAFETY: hwnd действительно и ещё не уничтожено.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: стандартный цикл сообщений для окна, созданного этим потоком.
        let has_msg = unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool();
        if !has_msg {
            break;
        }
        // WM_HOTKEY для хоткея потока (hwnd=None при регистрации) приходит с
        // msg.hwnd = NULL; DispatchMessageW для такого сообщения не вызывает
        // wndproc (у NULL-окна его нет), поэтому перехватываем здесь
        // (docs/M2_INTEGRATION_REVIEW.md, раздел 4).
        if msg.message == WM_HOTKEY {
            let id = message_hotkey_id(msg.wParam);
            // Диагностика живого репорта 2026-08-25 («не работает Ctrl+Alt+S»):
            // хоткей зарегистрирован и комбинацию держим мы, но до
            // координатора событие не доезжало. Эта строка разделяет два
            // совершенно разных случая — «Windows не прислала сообщение»
            // и «прислала, но мы его не разобрали», — а различить их иначе
            // нечем.
            tracing::info!(id, hwnd_null = msg.hwnd.0.is_null(), "WM_HOTKEY получен");
        }
        if msg.message == WM_HOTKEY && msg.hwnd.0.is_null() {
            let id = message_hotkey_id(msg.wParam);
            // Какие хоткеи ЭТОТ поток реально зарегистрировал.
            //
            // Без этой проверки поток реагировал бы на любое `WM_HOTKEY` со
            // знакомым id, даже если хоткей принадлежит другому окну оверлея:
            // у процесса их столько же, сколько мониторов, а регистрирует
            // хоткеи ровно одно. Найдено 2026-08-25 замером — посланное
            // вручную сообщение переключило режим редактирования дважды, по
            // разу на монитор. Набор живёт в `WndState`, а не здесь, потому
            // что перерегистрация (`WM_APP_REREGISTER_HOTKEYS`) меняет его в
            // `wndproc` — две копии разошлись бы.
            // SAFETY: state_ptr живёт до WM_NCDESTROY, цикл завершается
            // раньше; сообщения с hwnd=NULL разбираются на том же потоке.
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                continue;
            };
            // Медиа-хоткеи регистрируются и снимаются на лету (`wndproc`,
            // `WM_APP_MEDIA_HOTKEYS`), поэтому их id в набор не входят —
            // для них проверка владения не нужна и была бы неверной.
            let media = (MEDIA_PLAY_PAUSE_HOTKEY_ID..=MEDIA_VOLUME_DOWN_HOTKEY_ID).contains(&id);
            if !media && !state.owned.contains(&id) && !state.hooked.contains(&id) {
                continue;
            }
            if id == EDIT_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleEditMode);
            } else if id == TOGGLE_ALL_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleAllStickers);
            } else if id == MUTE_ALL_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleMuteAll);
            } else if id == PIN_FOCUSED_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::PinFocusedWindow);
            } else if id == MEDIA_PLAY_PAUSE_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::MediaPlayPause);
            } else if id == MEDIA_VOLUME_UP_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::MediaVolumeUp);
            } else if id == MEDIA_VOLUME_DOWN_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::MediaVolumeDown);
            } else if id == crate::hotkey::GROUP_MENU_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleGroupsMenu);
            } else if id == crate::hotkey::GROUP_DELETE_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::DeleteOpenGroup);
            } else if id == crate::hotkey::UNPIN_ALL_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::UnpinAll);
            } else if id == crate::hotkey::MITOSIS_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleMitosisMode);
            } else if id == crate::hotkey::WINDOW_CROP_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleWindowCropMode);
            } else if id == crate::hotkey::PIN_OPEN_GROUP_HOTKEY_ID {
                // ДО `group_number_of`: id 20 вне диапазона открытия (8..=16),
                // но ветка держится рядом с unpin_all, где живёт её константа.
                let _ = hotkey_tx.send(OverlayEvent::PinTogglePressed);
            } else if let Some(n) = group_number_of(id) {
                let _ = hotkey_tx.send(OverlayEvent::OpenGroup(n));
            }
            continue;
        }
        // SAFETY: msg заполнено предыдущим GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Бит 30 `lParam` клавиатурных сообщений: предыдущее состояние клавиши
/// (1 — клавиша уже была нажата, т.е. сообщение — автоповтор удержания).
const KEY_PREVIOUS_STATE_MASK: isize = 0x4000_0000;

/// Решение по сырому клавиатурному сообщению: `Some(true)` — первое
/// нажатие, `Some(false)` — отпускание, `None` — автоповтор `WM_KEYDOWN`
/// (удержание клавиши), который наружу не уходит, иначе удержание
/// Ctrl+D/Ctrl+Z срабатывало бы многократно (docs/M2_SLICE_REVIEW.md,
/// раздел 9 «Автоповтор WM_KEYDOWN»). Чистая функция — тесты без реального
/// окна.
fn key_event_press(msg: u32, lparam: isize) -> Option<bool> {
    match msg {
        WM_KEYDOWN => (lparam & KEY_PREVIOUS_STATE_MASK == 0).then_some(true),
        WM_KEYUP => Some(false),
        _ => None,
    }
}

/// Модификаторы клавиатуры вне мышиного сообщения (для `WM_KEYDOWN`/`WM_KEYUP`,
/// у которых, в отличие от мышиных сообщений, нет битов `MK_*` в `wparam`).
fn current_key_modifiers() -> Modifiers {
    // SAFETY: чтение состояния клавиш вызывающего потока — тот же паттерн,
    // что и в `input::Modifiers::current` (Alt читается так же, как там: в
    // `MK_*` флага для него нет — docs/M2_SLICE_REVIEW.md, раздел 9).
    unsafe {
        Modifiers {
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

fn create_window(bounds_px: Rect) -> Result<HWND, Win32Error> {
    // SAFETY: GetModuleHandleW(None) возвращает хэндл текущего модуля.
    let hinstance = unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .map_err(Win32Error::Win32)?;

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: wc заполнена корректно. Класс — процесс-wide: повторная
    // регистрация (второе окно в этом же процессе, параллельные тесты)
    // проваливается с ERROR_CLASS_ALREADY_EXISTS — это не ошибка, класс
    // уже готов к использованию. Остальные коды — настоящий сбой.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        // SAFETY: GetLastError осмысленна сразу после провалившегося вызова
        // на этом же потоке.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(Win32Error::Win32(err.into()));
        }
    }

    // Границы монитора — из аргумента (M3, docs/M3_PREP_NOTES.md, раздел 3.1):
    // позиция rcMonitor.left/top виртуального десктопа (у неосновных может
    // быть отрицательной) и размер в физических пикселях.
    if bounds_px.w == 0 || bounds_px.h == 0 {
        return Err(Win32Error::OverlayWindowCreateFailed);
    }

    // Стили — по ARCHITECTURE.md, раздел 2: TOPMOST — всегда сверху;
    // TOOLWINDOW — вне Alt+Tab и панели задач; NOACTIVATE — не забирает фокус;
    // TRANSPARENT — клики проходят насквозь; NOREDIRECTIONBITMAP — контент
    // пойдёт напрямую через DirectComposition (проверено спайком S0). Оба
    // флага (NOACTIVATE, TRANSPARENT) снимаются на время режима
    // редактирования через `set_click_through` (M2).
    //
    // LAYERED — обязателен для реального клик-сквозь: по докам Win32
    // (Layered Windows, Raymond Chen 2012-12-17) TRANSPARENT без LAYERED
    // описан только как порядок отрисовки СРЕДИ ОКОН ОДНОГО ПОТОКА, а не
    // маршрутизация кликов чужим процессам (Explorer, панель задач и т. д.);
    // системный проброс кликов сквозь TOPMOST-окно на другие процессы
    // гарантирован только для LAYERED|TRANSPARENT (найдено брейнштормом
    // ботов-воркеров при разборе бага «мышь мертва системно с запуска»,
    // 2026-08-09). DirectComposition официально совместим с LAYERED
    // (DirectComposition Basic Concepts, «composition target window»).
    // SAFETY: все аргументы — валидные константы и только что
    // зарегистрированный класс; размеры окон реальных мониторов влезают в i32.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | WS_EX_TRANSPARENT
                | WS_EX_NOREDIRECTIONBITMAP
                | WS_EX_LAYERED,
            CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            bounds_px.x,
            bounds_px.y,
            bounds_px.w as i32,
            bounds_px.h as i32,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(Win32Error::Win32)?;

    if hwnd.0.is_null() {
        return Err(Win32Error::OverlayWindowCreateFailed);
    }

    // LAYERED-окно без вызова SetLayeredWindowAttributes/UpdateLayeredWindow
    // остаётся невидимым (доки Win32). alpha=255/LWA_ALPHA — полностью
    // непрозрачно по системным меркам (реальный контент рисует
    // DirectComposition отдельно, этот вызов только включает механизм
    // клик-сквозь LAYERED-окна, визуально ничего не меняет).
    // SAFETY: hwnd — только что созданное окно этим потоком.
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
    }

    // SAFETY: hwnd — действительное окно, созданное выше этим потоком.
    // Окно без redirection-битмапа и контента визуально пустое и
    // клик-прозрачное, показ безопасен.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    // Таймер, возвращающий оверлей наверх topmost-полосы (см.
    // [`keep_topmost`]). Отказ окно не ломает — стикеры просто теряют
    // способность возвращаться из-под чужих topmost-окон, поэтому warn, а не
    // ошибка создания.
    // SAFETY: hwnd — окно этого потока; таймер снимается в WM_DESTROY.
    if unsafe { SetTimer(Some(hwnd), TOPMOST_TIMER_ID, TOPMOST_TIMER_MS, None) } == 0 {
        tracing::warn!("SetTimer не удался — оверлей не сможет сам возвращаться наверх");
    }

    // Сессионные события (блокировка/разблокировка, сон/пробуждение) —
    // окно регистрируется получателем WM_WTSSESSION_CHANGE
    // (docs/M3_PREP_NOTES.md, раздел 3.6); снятие — в WM_DESTROY.
    // Отказ регистрации окно не ломает: без событий сессии оно продолжает
    // работать (warn-лог вместо ошибки создания).
    // SAFETY: hwnd — действительное окно этого потока, живёт до WM_DESTROY.
    if unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }.is_err() {
        tracing::warn!("WTSRegisterSessionNotification не удалась; события сессии недоступны");
    }

    Ok(hwnd)
}

/// Обработка `WM_DPICHANGED` (docs/M3_PREP_NOTES.md, раздел 3.6):
/// `wParam` — новый DPI (младшее слово — dpiX), `lParam` — рекомендованный
/// прямоугольник. Применяет прямоугольник через `SetWindowPos` (z-order
/// и фокус не трогаем) и возвращает событие с новым DPI и размером окна;
/// `None` — сообщение с нулевым DPI (быть не должно): окно не трогаем.
/// Чистая по отношению к wndproc часть логики — тестируется напрямую.
fn handle_dpi_changed(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<OverlayEvent> {
    let dpi = (wparam.0 & 0xFFFF) as u32;
    if dpi == 0 {
        return None;
    }
    // SAFETY: lParam WM_DPICHANGED всегда указывает на действительный RECT
    // (документировано Windows); читается только в течение этого вызова.
    let rect = unsafe { &*(lparam.0 as *const RECT) };
    // SAFETY: hwnd — наше живое окно; SWP_NOZORDER сохраняет позицию в
    // z-order (WS_EX_TOPMOST не слетает), SWP_NOACTIVATE — фокус не трогает.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .ok()?;
    }
    Some(OverlayEvent::DpiChanged {
        dpi,
        size: (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ),
    })
}

/// Реакция на `WM_DISPLAYCHANGE` (docs/M3_PREP_NOTES.md, раздел 2.3):
/// переперечисление мониторов и свежий снапшот наружу событием
/// [`OverlayEvent::MonitorsChanged`]; сопоставление старого и нового
/// снапшотов — на стороне координатора (только по device interface path).
/// Ошибка перечисления — warn-лог и `None`: событие не уходит, следующий
/// `WM_DISPLAYCHANGE` повторит попытку.
fn handle_display_change() -> Option<OverlayEvent> {
    match monitors::enumerate() {
        Ok(snapshot) => Some(OverlayEvent::MonitorsChanged(snapshot)),
        Err(e) => {
            tracing::warn!(error = %e, "WM_DISPLAYCHANGE: перечисление мониторов не удалось");
            None
        }
    }
}

/// Разбор `wParam` из `WM_WTSSESSION_CHANGE`: блокировка/разблокировка
/// сессии → событие. Прочие события сессии (вход/выход пользователя,
/// переключение консоли и т.п.) оверлею не нужны. Чистая функция — тесты
/// без реального окна.
fn session_change_event(wparam: WPARAM) -> Option<OverlayEvent> {
    match wparam.0 as u32 {
        WTS_SESSION_LOCK => Some(OverlayEvent::SessionLocked),
        WTS_SESSION_UNLOCK => Some(OverlayEvent::SessionUnlocked),
        _ => None,
    }
}

/// Разбор `wParam` из `WM_POWERBROADCAST`: уход в сон и пробуждение →
/// события. Прочие `PBT_*` (запросы приостановки, «питание почти
/// кончилось» и т.п.) не эмитим. Чистая функция — тесты без реального окна.
fn power_broadcast_event(wparam: WPARAM) -> Option<OverlayEvent> {
    match wparam.0 as u32 {
        PBT_APMSUSPEND => Some(OverlayEvent::SystemSuspending),
        PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => Some(OverlayEvent::SystemResumed),
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA установлен в run_message_loop сразу после
    // создания окна этим же потоком, до входа в цикл сообщений; читается и
    // освобождается только здесь. До установки (между CreateWindowExW и
    // SetWindowLongPtrW) указатель — null, обрабатываем это явно.
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WndState;

    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_CAPTURECHANGED => {
            // Жёсткий гейт: вне режима редактирования окно клик-прозрачно
            // (WS_EX_TRANSPARENT, `set_click_through`/`set_interactive`), и
            // Win32-мышь должна идти сквозь него полностью — ни `SetCapture`,
            // ни какая-либо реакция. Раньше `MouseCapture::handle_message`
            // вызывался безусловно на любое сообщение, дошедшее до этого
            // wndproc: если хоть один клик всё же попадал в клик-прозрачное
            // окно (редкая, но реальная гонка DWM-хиттеста при смене стиля,
            // либо WM_CAPTURECHANGED извне), окно молча ставило `SetCapture`
            // и с этого момента монопольно поглощало ВСЮ мышь системы, пока
            // не пришло бы совпадающее `WM_LBUTTONUP` — то есть потенциально
            // никогда, если этот клик не был «нашим» началом драга (найдено
            // при разборе бага «мышь не реагирует даже на панель задач»).
            // Проверяем текущий стиль напрямую (а не кэш) — источник истины
            // тот же, что у `set_exstyle_bits`.
            // SAFETY: hwnd — валидное окно этого потока.
            let click_through =
                unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TRANSPARENT.0 != 0;
            // ВНЕШНЕГО сторожа захвата здесь нет — и это намеренно.
            //
            // 2026-09-11 здесь стояла проверка «захват держится, а кнопка
            // физически отпущена → снять захват». Она срабатывала РАНЬШЕ
            // автомата `MouseCapture` и на каждом `WM_LBUTTONUP`: в этот момент
            // кнопка уже отпущена, захват ещё есть — проверка снимала его, и
            // автомат, увидев «захвата нет», отпускание ПРОГЛАТЫВАЛ
            // (`transition`: `WM_LBUTTONUP` без захвата не даёт события).
            // Итог — ни одно отпускание кнопки во всей программе не доходило:
            // выделение куска не завершалось, перетаскивания не заканчивались.
            //
            // Нужная защита уже встроена в сам автомат:
            // `MouseCapture::handle_message` зовёт `handle_message_checked` с
            // физическим состоянием кнопки и превращает `WM_MOUSEMOVE` при
            // отпущенной кнопке в настоящее отпускание — С СОБЫТИЕМ. Снимать
            // захват снаружи раньше автомата нельзя.
            // Захват разрешён ТОЛЬКО в режиме редактирования, где окно
            // интерактивно целиком. В режиме попиксельной
            // кликопрозрачности (`hit_rects` непуст) окно ловит мышь лишь
            // над кусками, и монопольный захват всей системы ради кнопки
            // размером с иконку — несоразмерная и опасная плата.
            let hit_rect_mode =
                unsafe { state_ptr.as_ref() }.is_some_and(|state| !state.hit_rects.is_empty());
            if hit_rect_mode {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    // Событие уходит координатору БЕЗ `SetCapture`: клики по
                    // полосе куска в захвате не нуждаются.
                    if let Some(event) = state.capture.handle_message_no_capture(msg, lparam) {
                        let _ = state.tx.send(OverlayEvent::Input(event));
                        return LRESULT(0);
                    }
                }
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            if !click_through {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    if let Some(event) = state.capture.handle_message(msg, wparam, lparam) {
                        let _ = state.tx.send(OverlayEvent::Input(event));
                        return LRESULT(0);
                    }
                }
            } else if let Some(state) = unsafe { state_ptr.as_mut() } {
                // Защитно: если что-то всё же успело поставить захват до
                // того, как стиль стал клик-прозрачным (гонка), снимаем его
                // здесь же — не полагаемся только на `force_release_capture`
                // из `toggle_edit_mode`.
                if state.capture.is_captured() {
                    state.capture.force_release();
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEWHEEL => {
            // Тот же клик-прозрачный гейт, что у мышиных кнопок выше — вне
            // режима редактирования колесо тоже должно уходить сквозь окно
            // (`DefWindowProcW`), а не листать невидимую панель. Захвата
            // мышью колесо не касается — не через `MouseCapture`, отдельное
            // событие напрямую (живой репорт пользователя: длинный список
            // окон в панели «Слои видимости» не листался).
            let click_through =
                unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TRANSPARENT.0 != 0;
            if !click_through {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    // Старшее слово `wParam` — знаковый `i16` дельты колеса
                    // (MSDN `WM_MOUSEWHEEL`); `WHEEL_DELTA` = 120 на один
                    // «щелчок» физического колеса.
                    let raw_delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                    let notches = raw_delta / WHEEL_DELTA as i32;
                    if notches != 0 {
                        let _ = state
                            .tx
                            .send(OverlayEvent::Input(InputEvent::MouseWheel { notches }));
                    }
                    return LRESULT(0);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SETCURSOR => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if let Some(res) = state.cursor.handle_set_cursor(lparam) {
                    return res;
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_APP_EDIT_CURSOR => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if let Some(shape) = cursor_shape_from_wparam(wparam) {
                    state.cursor.set_shape(shape);
                }
            }
            LRESULT(0)
        }
        WM_APP_RELEASE_CAPTURE => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                state.capture.force_release();
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // Попиксельная кликопрозрачность. Пустой список — правило не
            // действует: окно ведёт себя как раньше (режим редактирования
            // делает его интерактивным целиком, и трогать его здесь нельзя).
            let inside = match unsafe { state_ptr.as_ref() } {
                Some(state) if !state.hit_rects.is_empty() => {
                    // `lparam` несёт ЭКРАННУЮ точку; прямоугольники заданы в
                    // клиентских координатах, поэтому переводим точку, а не
                    // прямоугольники: окно двигают, а список — нет.
                    let sx = (lparam.0 & 0xFFFF) as i16 as i32;
                    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let mut pt = POINT { x: sx, y: sy };
                    // SAFETY: hwnd — живое окно этого потока.
                    let ok =
                        unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt) }
                            .as_bool();
                    if !ok {
                        // Перевод не удался — безопаснее пропустить мышь
                        // насквозь, чем перехватить весь монитор.
                        Some(false)
                    } else {
                        Some(state.hit_rects.iter().any(|r| {
                            pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
                        }))
                    }
                }
                _ => None,
            };
            match inside {
                Some(true) => LRESULT(HTCLIENT as isize),
                // `HTTRANSPARENT` — система повторит поиск в окне под нами,
                // то есть клик и движение уйдут туда, куда ушли бы без
                // оверлея вовсе.
                Some(false) => LRESULT(HTTRANSPARENT as isize),
                None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }
        WM_APP_INPUT_POLICY => {
            if lparam.0 == 0 {
                return LRESULT(0);
            }
            // SAFETY: указатель пришёл из `Box::into_raw` в
            // `apply_input_policy`; владение забираем здесь ровно один раз.
            let policy = *unsafe { Box::from_raw(lparam.0 as *mut OverlayInputPolicy) };
            // Пустой список областей — это «ловить нигде», то есть та же
            // прозрачность. Разворачиваем здесь, а не надеемся на вызывающего:
            // `HitRects(vec![])` со снятым флагом прозрачности означал бы окно,
            // которое не прозрачно, но и не ловит ничего, — худшее из обоих.
            let policy = match policy {
                OverlayInputPolicy::HitRects(r) if r.is_empty() => OverlayInputPolicy::Transparent,
                other => other,
            };
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                // ШАГ 1 — снять захват ДО смены стиля. Захват, переживший
                // смену режима, — это и есть залипшая мышь. Держать его
                // позволено только режиму, где окно интерактивно целиком.
                if !matches!(
                    policy,
                    OverlayInputPolicy::Interactive { .. } | OverlayInputPolicy::HoverTarget
                ) {
                    state.capture.force_release();
                }
                // ШАГ 2 — области. До смены стиля, чтобы первое же мышиное
                // сообщение после неё видело уже правильный список.
                state.hit_rects = match &policy {
                    OverlayInputPolicy::HitRects(rects) => rects
                        .iter()
                        .map(|&(x, y, w, h)| RECT {
                            left: x,
                            top: y,
                            right: x + w,
                            bottom: y + h,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
            }
            // ШАГ 3 — стиль.
            //   Transparent — прозрачно и без фокуса;
            //   Interactive — не прозрачно, фокус разрешён;
            //   HitRects    — не прозрачно (иначе до WM_NCHITTEST дело не
            //                 дойдёт), но NOACTIVATE остаётся: клик по кнопке
            //                 куска не должен уводить фокус из окна, в
            //                 котором человек печатает.
            let (transparent, noactivate) = match &policy {
                OverlayInputPolicy::Transparent => (true, true),
                OverlayInputPolicy::Interactive { .. } => (false, false),
                OverlayInputPolicy::HitRects(_) => (false, true),
                // Как прежний `set_hover_click_target(true)`: снята только
                // прозрачность, NOACTIVATE остаётся — клик по полосе
                // перемотки не должен уводить фокус из окна пользователя.
                OverlayInputPolicy::HoverTarget => (false, true),
            };
            // Оба бита — ОДНОЙ записью. Две отдельные записи оставляли окно на
            // мгновение в состоянии, которого нет ни в одном режиме
            // (прозрачность уже снята, `NOACTIVATE` ещё прежний), и его видели
            // другие потоки — в том числе системный поиск окна под курсором.
            // Так же ловил его тест при параллельном прогоне (2026-09-11).
            let mut set = 0u32;
            let mut clear = 0u32;
            if transparent {
                set |= WS_EX_TRANSPARENT.0;
            } else {
                clear |= WS_EX_TRANSPARENT.0;
            }
            if noactivate {
                set |= WS_EX_NOACTIVATE.0;
            } else {
                clear |= WS_EX_NOACTIVATE.0;
            }
            apply_exstyle_masks(hwnd, set, clear);
            // ШАГ 4 — фокус, только после смены стиля: окну с NOACTIVATE
            // система фокус не отдаст.
            if matches!(policy, OverlayInputPolicy::Interactive { take_focus: true }) {
                // SAFETY: hwnd — живое окно этого потока.
                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_APP_MEDIA_HOTKEYS => {
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                // Снятие — просто отпустить владельцев (Drop зовёт
                // UnregisterHotKey на этом же потоке).
                state.media_hotkeys.clear();
                state.fallback_media.clear();
                if wparam.0 != 0 {
                    for (id, vk) in [
                        (MEDIA_PLAY_PAUSE_HOTKEY_ID, VK_SPACE),
                        (MEDIA_VOLUME_UP_HOTKEY_ID, VK_PRIOR),
                        (MEDIA_VOLUME_DOWN_HOTKEY_ID, VK_NEXT),
                    ] {
                        let combo = crate::hotkey::HotkeyCombo {
                            ctrl: false,
                            alt: false,
                            shift: false,
                            win: false,
                            vk,
                        };
                        // Клавишу уже держит другая программа — уходим на
                        // низкоуровневый хук, а не остаёмся без сочетания.
                        // Голые пробел и PgUp/PgDn заняты чаще любых других
                        // (репорт пользователя 2026-09-01: громкость видео
                        // не менялась вовсе), и «молча не работает» здесь —
                        // худший из возможных исходов.
                        match crate::hotkey::RegisteredHotkey::register(id, combo) {
                            Ok(h) => state.media_hotkeys.push(h),
                            Err(e) => {
                                tracing::info!(
                                    id,
                                    error = %e,
                                    "медиа-клавиша занята — уходим на клавиатурный хук"
                                );
                                state.fallback_media.push((id, combo));
                            }
                        }
                    }
                }
                refresh_hotkey_hook(state);
            }
            LRESULT(0)
        }
        WM_APP_REREGISTER_HOTKEYS => {
            // `lParam` — `Box<Vec<(id, combo)>>`, владение перешло нам в
            // момент доставки сообщения. Освобождаем его в ЛЮБОМ случае:
            // если state уже нет (окно догоняет уничтожение), набор просто
            // выбросить — регистрировать некуда.
            let combos = unsafe { Box::from_raw(lparam.0 as *mut Vec<(i32, HotkeyCombo)>) };
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                let report = install_hotkeys(state, &combos);
                for conflict in &report.conflicts {
                    tracing::warn!(
                        id = conflict.id,
                        combo = %conflict.combo,
                        "хоткей занят другим приложением — комбинация не перерегистрирована"
                    );
                }
                tracing::info!(
                    registered = report.registered,
                    conflicts = report.conflicts.len(),
                    "набор хоткеев переустановлен"
                );
                // Отчёт наружу целиком: текст для пользователя формирует
                // координатор, он же знает, какие id что означают.
                let _ = state.tx.send(OverlayEvent::HotkeysReloaded {
                    registered: report.registered,
                    conflicts: report.conflicts,
                });
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // Масштаб монитора сменился: применяем рекомендованный
            // прямоугольник и сообщаем координатору новый DPI/размер
            // (docs/M3_PREP_NOTES.md, раздел 3.6).
            if let Some(event) = handle_dpi_changed(hwnd, wparam, lparam) {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    let _ = state.tx.send(event);
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DISPLAYCHANGE => {
            // Конфигурация мониторов сменилась: переперечисление и свежий
            // снапшот наружу — окно на пропавшем мониторе уничтожит
            // координатор, реагируя на событие (docs/M3_PREP_NOTES.md,
            // раздел 2.3 и 3.6); diff по device interface path — не здесь.
            if let Some(event) = handle_display_change() {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    let _ = state.tx.send(event);
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_WTSSESSION_CHANGE => {
            // Блокировка/разблокировка сессии (окно зарегистрировано в
            // create_window): событие координатору. Прочие события сессии
            // игнорируем — системному обработчику.
            if let Some(event) = session_change_event(wparam) {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    let _ = state.tx.send(event);
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_POWERBROADCAST => {
            // Уход в сон/пробуждение: событие координатору. TRUE в ответ —
            // «сообщение обработано»; для PBT_APMSUSPEND это заодно
            // «приложение готово к приостановке» (MSDN, WM_POWERBROADCAST).
            if let Some(event) = power_broadcast_event(wparam) {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    let _ = state.tx.send(event);
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CHAR => {
            // `TranslateMessage` в цикле сообщений уже применил раскладку и
            // мёртвые клавиши; здесь остаётся собрать суррогатную пару и
            // отсеять управляющие коды (Backspace/Enter/Esc приходят и
            // сюда, но их обрабатывает ветка WM_KEYDOWN выше).
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                if let Some(ch) = char_from_wm_char(wparam.0 as u16, &mut state.pending_surrogate)
                    && !ch.is_control()
                {
                    let _ = state.tx.send(OverlayEvent::Char(ch));
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_KEYUP => {
            let vk = wparam.0 as u32;
            // Автоповтор нажатия (удержание) наружу не уходит: наружу — только
            // первое нажатие и отпускание.
            if let Some(pressed) = key_event_press(msg, lparam.0) {
                if let Some(state) = unsafe { state_ptr.as_mut() } {
                    let _ = state.tx.send(OverlayEvent::Key {
                        vk,
                        modifiers: current_key_modifiers(),
                        pressed,
                    });
                }
            }
            if vk == VK_ESCAPE.0 as u32 {
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_TIMER if wparam.0 == TOPMOST_TIMER_ID => {
            // SAFETY: state_ptr живёт до WM_NCDESTROY; hwnd — наше окно.
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                unsafe { keep_topmost(hwnd, &mut state.topmost) };
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // Хук снимается вместе с окном: он привязан к этому потоку, а
            // поток вот-вот закончится. Оставленный хук Windows выбросит
            // сама, но до того каждое нажатие в системе ходило бы в мёртвый
            // колбэк.
            crate::hotkey_hook::clear();
            if !state_ptr.is_null() {
                // SAFETY: последнее использование этого указателя — окно
                // уничтожается, WM_NCDESTROY приходит ровно один раз.
                unsafe {
                    drop(Box::from_raw(state_ptr));
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => {
            // Таймер удержания верха — парно к `SetTimer` в create_window.
            // SAFETY: hwnd валиден в WM_DESTROY.
            let _ = unsafe { KillTimer(Some(hwnd), TOPMOST_TIMER_ID) };
            // Снять регистрацию сессионных уведомлений (парно к регистрации
            // в create_window); окно в WM_DESTROY ещё валидно, ошибка
            // игнорируется — окно и так уничтожается.
            // SAFETY: hwnd валиден в WM_DESTROY.
            let _ = unsafe { WTSUnRegisterSessionNotification(hwnd) };
            // SAFETY: стандартный вызов из обработчика WM_DESTROY.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: делегирование необработанных сообщений системному обработчику.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {

    // --- Удержание верха topmost-полосы (репорт пользователя 2026-09-02) ---

    /// Служебные окна 1×1, вечно висящие над всеми (`ThumbnailDeviceHelperWnd`
    /// проводника), помехой не считаются: иначе оверлей дёргал бы
    /// `SetWindowPos` каждые 400 мс до конца сеанса.
    #[test]
    fn tiny_helper_windows_are_not_intruders() {
        let ours = (0, 0, 2560, 1440);
        assert!(!intruder_covers(ours, (0, 0, 1, 1)));
        // Порог — по КАЖДОЙ стороне: узкая полоска во весь экран тоже мимо.
        assert!(!intruder_covers(
            ours,
            (0, 0, 2560, TOPMOST_MIN_INTRUDER_PX - 1)
        ));
        assert!(intruder_covers(ours, (0, 0, 2560, TOPMOST_MIN_INTRUDER_PX)));
    }

    /// Чужое topmost-окно на СОСЕДНЕМ мониторе нас не закрывает — за верх с
    /// ним бороться незачем.
    #[test]
    fn a_window_on_another_monitor_is_not_an_intruder() {
        let ours = (0, 0, 2560, 1440);
        assert!(!intruder_covers(ours, (-1920, 357, 0, 1437)));
        // Кромка в кромку — тоже не пересечение (RECT полуоткрыт справа).
        assert!(!intruder_covers(ours, (2560, 0, 3000, 400)));
        assert!(intruder_covers(ours, (2400, 0, 3000, 400)));
    }

    /// Обычный случай: чужое окно всплыло над нами — поднимаемся, и на
    /// следующем тике над нами уже чисто.
    #[test]
    fn topmost_guard_raises_once_and_calms_down() {
        let mut guard = TopmostGuard::default();
        assert!(guard.decide(Some(0x1234)));
        assert!(!guard.decide(None));
        // Тот же нарушитель позже — снова поднимаемся: счётчик обнулён.
        assert!(guard.decide(Some(0x1234)));
    }

    /// Приложение, которое тоже держит себя наверху по таймеру, отвоёвывает
    /// верх каждый тик. После [`TOPMOST_WAR_STRIKES`] попыток уступаем —
    /// мигание несколько раз в секунду хуже, чем чужое окно сверху.
    #[test]
    fn topmost_guard_gives_up_on_a_window_that_keeps_winning() {
        let mut guard = TopmostGuard::default();
        for attempt in 1..=TOPMOST_WAR_STRIKES {
            assert!(
                guard.decide(Some(0x1234)),
                "попытка {attempt} должна быть боевой"
            );
        }
        assert!(!guard.decide(Some(0x1234)), "дальше — капитуляция");
        assert!(
            !guard.decide(Some(0x1234)),
            "и она не отменяется сама собой"
        );
    }

    /// Капитуляция привязана к конкретному окну: другое окно на его месте
    /// получает свой полный набор попыток, а уход прежнего сбрасывает всё.
    #[test]
    fn topmost_guard_surrender_is_per_window() {
        let mut guard = TopmostGuard::default();
        for _ in 0..=TOPMOST_WAR_STRIKES {
            guard.decide(Some(0x1234));
        }
        assert!(!guard.decide(Some(0x1234)));
        assert!(
            guard.decide(Some(0x5678)),
            "другой нарушитель — другой счёт"
        );
        // Ушли оба — прежняя капитуляция забыта.
        assert!(!guard.decide(None));
        assert!(guard.decide(Some(0x1234)));
    }

    #[test]
    fn wm_char_returns_plain_characters() {
        let mut pending = None;
        assert_eq!(char_from_wm_char(b'a' as u16, &mut pending), Some('a'));
        // Кириллическая Ж — из BMP, приходит одним сообщением.
        assert_eq!(char_from_wm_char(0x0416, &mut pending), Some('\u{416}'));
        assert_eq!(pending, None, "обычный символ не оставляет хвоста");
    }

    #[test]
    fn wm_char_assembles_surrogate_pair() {
        // U+1F600 приходит двумя сообщениями: D83D DE00.
        let mut pending = None;
        assert_eq!(
            char_from_wm_char(0xD83D, &mut pending),
            None,
            "ждём вторую половину"
        );
        assert_eq!(pending, Some(0xD83D));
        assert_eq!(char_from_wm_char(0xDE00, &mut pending), Some('\u{1F600}'));
        assert_eq!(pending, None);
    }

    #[test]
    fn wm_char_recovers_from_a_broken_pair() {
        // За верхней половиной пришёл обычный символ — разбираем его как
        // самостоятельный, а не молчим и не паникуем.
        let mut pending = Some(0xD83Du16);
        assert_eq!(char_from_wm_char(b'x' as u16, &mut pending), Some('x'));
        assert_eq!(pending, None);
    }
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    #[test]
    fn cursor_shape_wparam_round_trips_fixed_shapes() {
        for shape in [
            CursorShape::Arrow,
            CursorShape::Move,
            CursorShape::SizeNS,
            CursorShape::SizeWE,
            CursorShape::SizeNESW,
            CursorShape::SizeNWSE,
        ] {
            assert_eq!(
                cursor_shape_from_wparam(cursor_shape_to_wparam(shape)),
                Some(shape)
            );
        }
    }

    #[test]
    fn cursor_shape_wparam_round_trips_rotate_angles() {
        // 0/359 — границы диапазона; -45/-135 — реальные значения,
        // используемые ядром (нормализуются rem_euclid(360) при кодировании,
        // см. доккомент ROTATE_WPARAM_BASE); 400 — больше 360, тоже обязан
        // нормализоваться корректно.
        for angle in [0, 45, 90, 180, 270, 359, -45, -135, 400] {
            let shape = CursorShape::Rotate(angle);
            let decoded = cursor_shape_from_wparam(cursor_shape_to_wparam(shape));
            let CursorShape::Rotate(decoded_angle) = decoded.expect("Rotate декодируется")
            else {
                panic!("ожидался CursorShape::Rotate");
            };
            assert_eq!(
                decoded_angle,
                angle.rem_euclid(360),
                "угол {angle} должен нормализоваться в 0..360"
            );
        }
    }

    #[test]
    fn cursor_shape_from_wparam_rejects_out_of_range() {
        // 6 — `CursorShape::Cross` (митоз), первый свободный код теперь 7.
        assert_eq!(
            cursor_shape_from_wparam(WPARAM(6)),
            Some(CursorShape::Cross)
        );
        assert_eq!(cursor_shape_from_wparam(WPARAM(7)), None);
        assert_eq!(cursor_shape_from_wparam(WPARAM(999)), None);
        assert_eq!(
            cursor_shape_from_wparam(WPARAM(ROTATE_WPARAM_MAX + 1)),
            None
        );
    }

    fn test_hotkey() -> HotkeyCombo {
        // Экзотическая комбинация — не конфликтует с реальными приложениями
        // на машине разработчика/CI.
        HotkeyCombo::parse("Ctrl+Alt+Shift+F23").expect("валидная комбинация")
    }

    /// Границы «второго» монитора справа от основного — геометрия, которую
    /// захардкоженный основной экран дать не мог: ненулевая позиция.
    fn test_bounds() -> Rect {
        Rect {
            x: 1920,
            y: 120,
            w: 1280,
            h: 1024,
        }
    }

    #[test]
    fn create_then_drop_destroys_window() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        let hwnd = overlay.hwnd();
        assert!(!hwnd.0.is_null());

        // Размер — из границ монитора, переданных при создании.
        assert_eq!(overlay.size(), (1280, 1024));

        // SAFETY: hwnd — наше живое окно, создание выше проверено.
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());

        drop(overlay);

        // SAFETY: после Drop окно уничтожено; IsWindow над мёртвым хэндлом —
        // простое чтение, хэндл мы больше никуда не передаём.
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
    }

    #[test]
    fn window_uses_given_bounds() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("создание оверлея");

        // Позиция и размер окна — ровно границы монитора, не (0, 0) и не
        // системный экран (docs/M3_PREP_NOTES.md, раздел 3.1).
        let mut rect = RECT::default();
        // SAFETY: hwnd — наше живое окно.
        unsafe {
            GetWindowRect(overlay.hwnd(), &mut rect).expect("GetWindowRect");
        }
        assert_eq!((rect.left, rect.top), (1920, 120));
        assert_eq!(
            (rect.right - rect.left, rect.bottom - rect.top),
            (1280, 1024)
        );
        assert_eq!(overlay.size(), (1280, 1024));
    }

    #[test]
    fn set_bounds_moves_and_resizes_window() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("создание оверлея");
        assert_eq!(overlay.size(), (1280, 1024));

        let moved = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert!(overlay.set_bounds(moved), "SetWindowPos должен успеть");

        let mut rect = RECT::default();
        // SAFETY: hwnd — наше живое окно.
        unsafe {
            GetWindowRect(overlay.hwnd(), &mut rect).expect("GetWindowRect");
        }
        assert_eq!((rect.left, rect.top), (0, 0));
        assert_eq!(
            (rect.right - rect.left, rect.bottom - rect.top),
            (1920, 1080)
        );
        assert_eq!(overlay.size(), (1920, 1080));
    }

    /// Дождаться, пока стиль окна удовлетворит условию: политика ввода
    /// применяется СООБЩЕНИЕМ на потоке окна, то есть асинхронно, и читать
    /// стиль сразу после вызова значило бы проверять старое состояние.
    fn wait_exstyle(hwnd: HWND, want: impl Fn(u32) -> bool) -> u32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            // SAFETY: чтение стиля своего же окна.
            let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            if want(ex) || std::time::Instant::now() > deadline {
                return ex;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // В этих тестах фокус НЕ забирается (`take_focus: false`): прогон тестов
    // на рабочей машине иначе уводил бы фокус из окна, в котором человек
    // печатает.

    #[test]
    fn input_policy_interactive_clears_transparency_and_noactivate() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        overlay.apply_input_policy(OverlayInputPolicy::Interactive { take_focus: false });
        let ex = wait_exstyle(overlay.hwnd(), |ex| {
            ex & (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) == 0
        });
        assert_eq!(
            ex & WS_EX_TRANSPARENT.0,
            0,
            "интерактивное окно не прозрачно"
        );
        assert_eq!(
            ex & WS_EX_NOACTIVATE.0,
            0,
            "интерактивному окну фокус разрешён"
        );
    }

    #[test]
    fn input_policy_transparent_restores_both_bits() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        overlay.apply_input_policy(OverlayInputPolicy::Interactive { take_focus: false });
        wait_exstyle(overlay.hwnd(), |ex| ex & WS_EX_TRANSPARENT.0 == 0);
        overlay.apply_input_policy(OverlayInputPolicy::Transparent);
        let ex = wait_exstyle(overlay.hwnd(), |ex| {
            ex & (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0)
                == (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0)
        });
        assert_ne!(ex & WS_EX_TRANSPARENT.0, 0, "прозрачность вернулась");
        assert_ne!(ex & WS_EX_NOACTIVATE.0, 0, "и без фокуса");
    }

    #[test]
    fn input_policy_hit_rects_keeps_noactivate() {
        // Попиксельный режим снимает прозрачность (иначе до WM_NCHITTEST
        // дело не дойдёт), но НЕ даёт окну фокус: клик по кнопке куска не
        // должен уводить фокус из окна, в котором человек печатает.
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        overlay.apply_input_policy(OverlayInputPolicy::HitRects(vec![(10, 10, 50, 50)]));
        let ex = wait_exstyle(overlay.hwnd(), |ex| ex & WS_EX_TRANSPARENT.0 == 0);
        assert_eq!(
            ex & WS_EX_TRANSPARENT.0,
            0,
            "области требуют снятой прозрачности"
        );
        assert_ne!(ex & WS_EX_NOACTIVATE.0, 0, "но фокус окно не получает");
    }

    #[test]
    fn input_policy_hover_target_matches_old_hover_click_target() {
        // Полоса перемотки обязана вести себя ровно как раньше: снята только
        // прозрачность, фокус окно не получает.
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        overlay.apply_input_policy(OverlayInputPolicy::HoverTarget);
        let ex = wait_exstyle(overlay.hwnd(), |ex| ex & WS_EX_TRANSPARENT.0 == 0);
        assert_eq!(ex & WS_EX_TRANSPARENT.0, 0, "полоса ловит мышь");
        assert_ne!(ex & WS_EX_NOACTIVATE.0, 0, "и не уводит фокус");
    }

    #[test]
    fn input_policy_empty_hit_rects_means_transparent() {
        // `HitRects(vec![])` со снятой прозрачностью — окно, которое не
        // прозрачно, но и не ловит ничего. Примитив обязан развернуть такой
        // вход в прозрачность сам, не надеясь на вызывающего.
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        overlay.apply_input_policy(OverlayInputPolicy::Interactive { take_focus: false });
        wait_exstyle(overlay.hwnd(), |ex| ex & WS_EX_TRANSPARENT.0 == 0);
        overlay.apply_input_policy(OverlayInputPolicy::HitRects(Vec::new()));
        let ex = wait_exstyle(overlay.hwnd(), |ex| ex & WS_EX_TRANSPARENT.0 != 0);
        assert_ne!(
            ex & WS_EX_TRANSPARENT.0,
            0,
            "пустой список областей обязан дать прозрачное окно"
        );
    }

    #[test]
    fn set_click_through_toggles_exstyle_bits() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");
        // SAFETY: чтение стиля своего же окна.
        let initial = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(
            initial & WS_EX_TRANSPARENT.0,
            0,
            "по умолчанию клик-прозрачно"
        );

        overlay.set_click_through(false);
        let editing = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_eq!(
            editing & WS_EX_TRANSPARENT.0,
            0,
            "в режиме редактирования — нет"
        );
        assert_eq!(editing & WS_EX_NOACTIVATE.0, 0);

        overlay.set_click_through(true);
        let restored = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(
            restored & WS_EX_TRANSPARENT.0,
            0,
            "выход восстанавливает клик-прозрачность"
        );
        assert_ne!(restored & WS_EX_NOACTIVATE.0, 0);
    }

    #[test]
    fn set_capture_affinity_round_trips_and_verifies() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None, None)
                .expect("создание оверлея");

        let hidden = overlay
            .set_capture_affinity(true)
            .expect("SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)");
        assert!(
            hidden,
            "GetWindowDisplayAffinity должен подтвердить применение"
        );
        // SAFETY: чтение аффинити своего же окна.
        let mut actual: u32 = 0;
        unsafe { GetWindowDisplayAffinity(overlay.hwnd(), &mut actual) }
            .expect("GetWindowDisplayAffinity");
        assert_eq!(actual, WDA_EXCLUDEFROMCAPTURE.0);

        let shown = overlay
            .set_capture_affinity(false)
            .expect("SetWindowDisplayAffinity(WDA_NONE)");
        assert!(shown, "снятие аффинити тоже должно подтверждаться");
        unsafe { GetWindowDisplayAffinity(overlay.hwnd(), &mut actual) }
            .expect("GetWindowDisplayAffinity");
        assert_eq!(actual, WDA_NONE.0);
    }

    #[test]
    fn set_interactive_toggles_same_bits_as_click_through() {
        // M3: окна других мониторов используют set_interactive, а не
        // set_click_through, чтобы не бороться за фокус — но сами биты
        // WS_EX_TRANSPARENT|WS_EX_NOACTIVATE переключаются одинаково.
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("создание оверлея");

        overlay.set_interactive(true);
        let editing = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_eq!(
            editing & WS_EX_TRANSPARENT.0,
            0,
            "интерактивно — не клик-прозрачно"
        );
        assert_eq!(editing & WS_EX_NOACTIVATE.0, 0);

        overlay.set_interactive(false);
        let restored = unsafe { GetWindowLongPtrW(overlay.hwnd(), GWL_EXSTYLE) } as u32;
        assert_ne!(
            restored & WS_EX_TRANSPARENT.0,
            0,
            "не интерактивно — клик-прозрачно"
        );
        assert_ne!(restored & WS_EX_NOACTIVATE.0, 0);
    }

    #[test]
    fn create_covers_system_screen() {
        // Совместимость с текущим единственным вызывающим кодом: окно на
        // основной монитор, размер — системный экран.
        let (overlay, _events) =
            OverlayWindow::create(test_hotkey(), None).expect("создание оверлея");
        // SAFETY: GetSystemMetrics безопасен с любого потока.
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        assert_eq!(overlay.size(), (w.max(0) as u32, h.max(0) as u32));
    }

    #[test]
    fn hotkey_name_maps_global_ids_and_ignores_media_and_group_ids() {
        assert_eq!(hotkey_name_of(EDIT_HOTKEY_ID), Some(HotkeyName::EditMode));
        assert_eq!(
            hotkey_name_of(TOGGLE_ALL_HOTKEY_ID),
            Some(HotkeyName::ToggleAllStickers)
        );
        assert_eq!(
            hotkey_name_of(MUTE_ALL_HOTKEY_ID),
            Some(HotkeyName::MuteAll)
        );
        assert_eq!(
            hotkey_name_of(PIN_FOCUSED_HOTKEY_ID),
            Some(HotkeyName::PinFocusedWindow)
        );
        for foreign in [
            MEDIA_PLAY_PAUSE_HOTKEY_ID,
            crate::hotkey::GROUP_MENU_HOTKEY_ID,
            crate::hotkey::UNPIN_ALL_HOTKEY_ID,
            crate::hotkey::GROUP_OPEN_HOTKEY_ID_BASE,
        ] {
            assert_eq!(
                hotkey_name_of(foreign),
                None,
                "id {foreign} — не глобальный хоткей с именем"
            );
        }
    }

    /// Перерегистрация набора: конфликт одной комбинации не роняет остальные
    /// и попадает в отчёт; владельцы успешных передаются хранилищу потока, а
    /// `owned` обновляется ровно по зарегистрированным id (тест без живого
    /// окна — `WndState` собирается вручную).
    #[test]
    fn install_hotkeys_isolates_conflicts_and_updates_owned_set() {
        let state = WndState {
            capture: MouseCapture::new(HWND(std::ptr::null_mut())),
            hit_rects: Vec::new(),
            cursor: CursorManager::new(),
            tx: mpsc::channel::<OverlayEvent>().0,
            media_hotkeys: Vec::new(),
            hotkeys: Vec::new(),
            owned: std::collections::HashSet::new(),
            hooked: std::collections::HashSet::new(),
            fallback_permanent: Vec::new(),
            fallback_media: Vec::new(),
            pending_surrogate: None,
            topmost: TopmostGuard::default(),
        };
        let mut state = Box::new(state);

        let ok = HotkeyCombo::parse("Ctrl+Alt+Shift+F21").expect("валидная комбинация");
        let taken = HotkeyCombo::parse("Ctrl+Alt+Shift+F20").expect("валидная комбинация");
        // Первая установка: оба свободны.
        let report = install_hotkeys(
            &mut state,
            &[(EDIT_HOTKEY_ID, ok), (TOGGLE_ALL_HOTKEY_ID, taken)],
        );
        assert_eq!(report.registered, 2);
        assert!(report.conflicts.is_empty());
        assert_eq!(state.owned.len(), 2);
        assert_eq!(state.hotkeys.len(), 2);

        // Вторая установка: `ok` теперь занят нами же (та же комбинация —
        // классический случай «сменили конфиг на ту же клавишу»), а `taken`
        // свободен. Снятие старых происходит ДО регистрации, поэтому
        // «сам с собой» конфликта нет — если бы порядок был обратным, вторая
        // установка увидела бы свою же комбинацию как занятую.
        let report = install_hotkeys(
            &mut state,
            &[(EDIT_HOTKEY_ID, taken), (TOGGLE_ALL_HOTKEY_ID, ok)],
        );
        assert_eq!(report.registered, 2, "обе комбинации перерегистрировались");
        assert!(report.conflicts.is_empty());
        assert_eq!(state.owned.len(), 2);
        assert_eq!(state.hotkeys.len(), 2);
    }

    #[test]
    fn second_window_with_same_hotkey_reports_conflict() {
        // Экзотическая комбинация — не конфликтует с реальными приложениями
        // на машине разработчика/CI (F22 не используется другими тестами).
        let combo = HotkeyCombo::parse("Ctrl+Alt+Shift+F22").expect("валидная комбинация");
        let (_first, _first_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(combo), None, None, None)
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(combo), None, None, None)
                .expect("второе окно");

        // Хоткей регистрируется на pump-потоке до сигнала готовности, поэтому
        // к моменту возврата create() конфликт уже лежит в канале событий.
        // Создание второго окна не провалилось — конфликт лишь событие, окно
        // продолжает работать.
        match second_events.recv_timeout(Duration::from_secs(5)) {
            Ok(OverlayEvent::HotkeyConflict(name, s)) => {
                assert_eq!(name, HotkeyName::EditMode);
                assert_eq!(s, "Ctrl+Alt+Shift+F22");
            }
            Ok(other) => panic!("ожидался HotkeyConflict, получено: {other:?}"),
            Err(e) => panic!("второе окно не прислало HotkeyConflict: {e}"),
        }
    }

    #[test]
    fn window_without_hotkey_does_not_conflict() {
        // M3: глобальный хоткей — ровно один на процесс (docs/M3_PREP_NOTES.md,
        // раздел 3.3). Первое окно регистрирует комбинацию, второе создаётся
        // с `None` — без регистрации и без события HotkeyConflict.
        let combo = HotkeyCombo::parse("Ctrl+Alt+Shift+F21").expect("валидная комбинация");
        let (_first, _first_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(combo), None, None, None)
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("второе окно");

        match second_events.recv_timeout(Duration::from_millis(300)) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(event) => panic!("окно без хоткея не должно слать событий, получено: {event:?}"),
            Err(e) => panic!("канал событий второго окна закрылся: {e}"),
        }
    }

    #[test]
    fn second_window_with_same_toggle_all_hotkey_reports_conflict() {
        // Экзотическая комбинация — не конфликтует с реальными приложениями
        // и с другими тестами (F20; F21/F22/F23/F24 заняты соседними тестами).
        let toggle = HotkeyCombo::parse("Ctrl+Alt+Shift+F20").expect("валидная комбинация");
        // У второго окна другой edit-хоткей (F19), чтобы конфликт пришёл
        // именно от toggle_all, а не от режима редактирования.
        let edit2 = HotkeyCombo::parse("Ctrl+Alt+Shift+F19").expect("валидная комбинация");
        let (_first, _first_events) = OverlayWindow::create_on_monitor(
            test_bounds(),
            Some(test_hotkey()),
            Some(toggle),
            None,
            None,
        )
        .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(edit2), Some(toggle), None, None)
                .expect("второе окно");

        // Тот же паттерн, что и для edit-хоткея: конфликт — событие, а не
        // ошибка создания окна.
        match second_events.recv_timeout(Duration::from_secs(5)) {
            Ok(OverlayEvent::HotkeyConflict(name, s)) => {
                assert_eq!(name, HotkeyName::ToggleAllStickers);
                assert_eq!(s, "Ctrl+Alt+Shift+F20");
            }
            Ok(other) => panic!("ожидался HotkeyConflict, получено: {other:?}"),
            Err(e) => panic!("второе окно не прислало HotkeyConflict: {e}"),
        }
    }

    #[test]
    fn second_window_with_same_mute_all_hotkey_reports_conflict() {
        // Тот же паттерн, что и toggle_all выше, для третьего хоткея (M5d).
        // Экзотическая комбинация — не конфликтует с реальными приложениями
        // и с другими тестами (F18; F19/F20/F21/F22/F23/F24 заняты соседними
        // тестами).
        let mute = HotkeyCombo::parse("Ctrl+Alt+Shift+F18").expect("валидная комбинация");
        let (_first, _first_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, Some(mute), None)
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, Some(mute), None)
                .expect("второе окно");

        match second_events.recv_timeout(Duration::from_secs(5)) {
            Ok(OverlayEvent::HotkeyConflict(name, s)) => {
                assert_eq!(name, HotkeyName::MuteAll);
                assert_eq!(s, "Ctrl+Alt+Shift+F18");
            }
            Ok(other) => panic!("ожидался HotkeyConflict, получено: {other:?}"),
            Err(e) => panic!("второе окно не прислало HotkeyConflict: {e}"),
        }
    }

    #[test]
    fn second_window_with_same_pin_focused_hotkey_reports_conflict() {
        // Тот же паттерн, что и для трёх существующих хоткеев, для четвёртого
        // (редизайн пинов). Экзотическая комбинация — не конфликтует с
        // реальными приложениями и с другими тестами (F17; F18–F24 заняты
        // соседними тестами).
        let pin = HotkeyCombo::parse("Ctrl+Alt+Shift+F17").expect("валидная комбинация");
        let (_first, _first_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, Some(pin))
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, Some(pin))
                .expect("второе окно");

        match second_events.recv_timeout(Duration::from_secs(5)) {
            Ok(OverlayEvent::HotkeyConflict(name, s)) => {
                assert_eq!(name, HotkeyName::PinFocusedWindow);
                assert_eq!(s, "Ctrl+Alt+Shift+F17");
            }
            Ok(other) => panic!("ожидался HotkeyConflict, получено: {other:?}"),
            Err(e) => panic!("второе окно не прислало HotkeyConflict: {e}"),
        }
    }

    #[test]
    fn dpi_changed_applies_recommended_rect_and_reports_event() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("создание оверлея");

        // Как в настоящем WM_DPICHANGED: wParam — новый DPI (младшее слово —
        // dpiX, старшее — dpiY, тут мусор, чтобы проверить что берём LOWORD),
        // lParam — рекомендованный прямоугольник.
        let rect = RECT {
            left: 100,
            top: 200,
            right: 700,
            bottom: 500,
        };
        let event = handle_dpi_changed(
            overlay.hwnd(),
            WPARAM(0x0001_00B0),
            LPARAM(&raw const rect as isize),
        )
        .expect("должно вернуться событие DpiChanged");
        assert_eq!(
            event,
            OverlayEvent::DpiChanged {
                dpi: 0xB0,
                size: (600, 300)
            }
        );

        // Окно переехало ровно на рекомендованный прямоугольник; геттеры
        // сразу отдают свежие значения (docs/M3_PREP_NOTES.md, раздел 3.6).
        let mut actual = RECT::default();
        // SAFETY: hwnd — наше живое окно.
        unsafe {
            GetWindowRect(overlay.hwnd(), &mut actual).expect("GetWindowRect");
        }
        assert_eq!((actual.left, actual.top), (100, 200));
        assert_eq!(
            (actual.right - actual.left, actual.bottom - actual.top),
            (600, 300)
        );
        assert_eq!(overlay.size(), (600, 300));
    }

    #[test]
    fn dpi_changed_with_zero_dpi_is_ignored() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("создание оверлея");
        let before = overlay.size();
        // Нулевой DPI в wParam быть не должен; окно не трогаем (и lParam
        // с нулевым указателем не читаем).
        assert_eq!(
            handle_dpi_changed(overlay.hwnd(), WPARAM(0), LPARAM(0)),
            None
        );
        assert_eq!(overlay.size(), before, "окно не тронуто");
    }

    #[test]
    fn key_event_press_filters_autorepeat() {
        // Первое нажатие: бит 30 == 0 (в младших битах — счётчик повторов 1).
        assert_eq!(key_event_press(WM_KEYDOWN, 1), Some(true));
        // Автоповтор удержания: бит 30 == 1 — наружу не уходит.
        assert_eq!(
            key_event_press(WM_KEYDOWN, KEY_PREVIOUS_STATE_MASK | 7),
            None
        );
        // Отпускание — всегда pressed=false (у WM_KEYUP бит 30 тоже == 1,
        // и это нормально: фильтр к нему не применяется).
        assert_eq!(
            key_event_press(WM_KEYUP, KEY_PREVIOUS_STATE_MASK | 1),
            Some(false)
        );
        assert_eq!(key_event_press(WM_KEYUP, 0), Some(false));
        // Прочие сообщения клавиатурных событий не порождают.
        assert_eq!(key_event_press(WM_MOUSEMOVE, 0), None);
    }

    #[test]
    fn session_change_maps_lock_and_unlock_only() {
        assert_eq!(
            session_change_event(WPARAM(WTS_SESSION_LOCK as usize)),
            Some(OverlayEvent::SessionLocked)
        );
        assert_eq!(
            session_change_event(WPARAM(WTS_SESSION_UNLOCK as usize)),
            Some(OverlayEvent::SessionUnlocked)
        );
        // Прочие события сессии (логин/логаут, переключение консоли и т.п.)
        // оверлею не нужны.
        assert_eq!(session_change_event(WPARAM(0)), None);
        assert_eq!(session_change_event(WPARAM(0x1)), None); // WTS_SESSION_LOGON
    }

    #[test]
    fn power_broadcast_maps_suspend_and_resume() {
        assert_eq!(
            power_broadcast_event(WPARAM(PBT_APMSUSPEND as usize)),
            Some(OverlayEvent::SystemSuspending)
        );
        // Пробуждение приходит двумя разными PBT_* — оба → SystemResumed.
        assert_eq!(
            power_broadcast_event(WPARAM(PBT_APMRESUMESUSPEND as usize)),
            Some(OverlayEvent::SystemResumed)
        );
        assert_eq!(
            power_broadcast_event(WPARAM(PBT_APMRESUMEAUTOMATIC as usize)),
            Some(OverlayEvent::SystemResumed)
        );
        // Запросы приостановки наружу не уходят — оверлей только докладывает
        // о фактическом уходе в сон и пробуждении.
        assert_eq!(power_broadcast_event(WPARAM(0)), None); // PBT_APMQUERYSUSPEND
        assert_eq!(power_broadcast_event(WPARAM(0x9)), None); // PBT_APMBATTERYLOW
    }

    #[test]
    fn system_and_session_events_reach_the_channel() {
        // Сообщения шлём реальному окну вручную — диспетчеризация от wndproc
        // до канала событий проверяется целиком; системная регистрация
        // (WTSRegisterSessionNotification) здесь не участвует.
        let (overlay, events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None, None)
                .expect("создание оверлея");
        // SAFETY: hwnd — наше живое окно; порядок сообщений в очереди окна
        // гарантируется (FIFO), значит и порядок событий в канале.
        unsafe {
            PostMessageW(
                Some(overlay.hwnd()),
                WM_POWERBROADCAST,
                WPARAM(PBT_APMSUSPEND as usize),
                LPARAM(0),
            )
            .expect("PostMessageW");
            PostMessageW(
                Some(overlay.hwnd()),
                WM_WTSSESSION_CHANGE,
                WPARAM(WTS_SESSION_UNLOCK as usize),
                LPARAM(0),
            )
            .expect("PostMessageW");
        }
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(OverlayEvent::SystemSuspending) => {}
            Ok(other) => panic!("ожидался SystemSuspending, получено: {other:?}"),
            Err(e) => panic!("SystemSuspending не пришёл: {e}"),
        }
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(OverlayEvent::SessionUnlocked) => {}
            Ok(other) => panic!("ожидался SessionUnlocked, получено: {other:?}"),
            Err(e) => panic!("SessionUnlocked не пришёл: {e}"),
        }
    }

    #[test]
    #[ignore = "требует реальное железо; запуск вручную: cargo test -p rst-win32 overlay -- --ignored"]
    fn display_change_returns_fresh_snapshot() {
        let event = handle_display_change().expect("перечисление мониторов");
        let OverlayEvent::MonitorsChanged(monitors) = event else {
            panic!("ожидался MonitorsChanged, получено: {event:?}");
        };
        assert!(!monitors.is_empty(), "хотя бы один монитор подключён");
        assert_eq!(
            monitors.iter().filter(|m| m.is_primary).count(),
            1,
            "ровно один основной"
        );
        for m in &monitors {
            assert!(m.id.0.starts_with("\\\\?\\DISPLAY#"), "id: {}", m.id.0);
        }
    }

    #[test]
    fn all_hotkey_ids_are_unique() {
        // id хоткея — единственный ключ разбора WM_HOTKEY (цепочка else-if
        // выше): дубликат значил бы «одно нажатие включает два действия» и
        // нашёлся бы только вживую. Тест собирает ВСЕ id программы —
        // оверлейные константы и пакет групп — в один набор: новый хоткей
        // с уже занятым id упадёт здесь, а не в поле.
        let mut ids = std::collections::HashSet::new();
        for id in [
            EDIT_HOTKEY_ID,
            TOGGLE_ALL_HOTKEY_ID,
            MUTE_ALL_HOTKEY_ID,
            PIN_FOCUSED_HOTKEY_ID,
            MEDIA_PLAY_PAUSE_HOTKEY_ID,
            MEDIA_VOLUME_UP_HOTKEY_ID,
            MEDIA_VOLUME_DOWN_HOTKEY_ID,
            crate::hotkey::GROUP_MENU_HOTKEY_ID,
            crate::hotkey::GROUP_DELETE_HOTKEY_ID,
            crate::hotkey::UNPIN_ALL_HOTKEY_ID,
            crate::hotkey::PIN_OPEN_GROUP_HOTKEY_ID,
        ] {
            assert!(ids.insert(id), "id хоткея {id} уже занят другим хоткеем");
        }
        for n in 0..rst_core::model::Hotkeys::GROUP_OPEN_SLOTS {
            let id = crate::hotkey::GROUP_OPEN_HOTKEY_ID_BASE + n as i32;
            assert!(ids.insert(id), "id открытия группы {id} уже занят");
        }
    }
}
