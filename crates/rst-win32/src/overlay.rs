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

use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWLP_USERDATA,
    GetMessageW, GetSystemMetrics, GetWindowDisplayAffinity, GetWindowLongPtrW, GetWindowRect,
    LWA_ALPHA, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PostMessageW,
    PostQuitMessage, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_APP, WM_CAPTURECHANGED,
    WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_POWERBROADCAST, WM_SETCURSOR,
    WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WTS_SESSION_LOCK,
    WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use rst_core::model::Rect;

use crate::error::Win32Error;
use crate::hotkey::{HotkeyCombo, RegisteredHotkey, message_hotkey_id};
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

/// Координатор → поток оверлея: сменить форму курсора (зона под курсором
/// меняется на его стороне, хит-тест — не Win32, ARCHITECTURE.md 5.3);
/// `wParam` — форма, закодированная [`cursor_shape_to_wparam`].
const WM_APP_EDIT_CURSOR: u32 = WM_APP + 1;

/// Координатор → поток оверлея: снять Win32-захват мыши безусловно
/// ([`crate::input::MouseCapture::force_release`]) — должно выполняться на
/// потоке окна (`SetCapture`/`ReleaseCapture` — thread-affine Win32 API),
/// поэтому не прямой вызов, а сообщение, как и `WM_APP_EDIT_CURSOR`.
const WM_APP_RELEASE_CAPTURE: u32 = WM_APP + 2;

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
        n @ ROTATE_WPARAM_BASE..=ROTATE_WPARAM_MAX => {
            CursorShape::Rotate((n - ROTATE_WPARAM_BASE) as i32)
        }
        _ => return None,
    })
}

const ROTATE_WPARAM_MAX: usize = ROTATE_WPARAM_BASE + 359;

/// Какой из трёх глобальных хоткеев не удалось зарегистрировать
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
    /// Глобальный хоткей не удалось зарегистрировать: комбинация уже занята
    /// другим приложением. Строка — каноничный вид комбинации из конфига
    /// (ARCHITECTURE.md, раздел 5.1). Окно продолжает работать — недоступны
    /// только хоткеи, которые не удалось зарегистрировать.
    HotkeyConflict(HotkeyName, String),
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
    /// для «заглушить все стикеры» (M5d, `hotkeys.mute_all`). Конфликт
    /// регистрации — не паника:
    /// окно работает, а наружу уходит событие
    /// [`OverlayEvent::HotkeyConflict`]. Возвращает управление, когда окно
    /// гарантированно создано, и приёмник событий мыши/клавиатуры/хоткеев —
    /// координатор объединяет его со своим каналом команд
    /// (docs/M2_INTEGRATION_PLAN.md, раздел 1).
    pub fn create_on_monitor(
        bounds_px: Rect,
        edit_hotkey: Option<HotkeyCombo>,
        toggle_all_hotkey: Option<HotkeyCombo>,
        mute_all_hotkey: Option<HotkeyCombo>,
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

    /// То же переключение `WS_EX_TRANSPARENT|WS_EX_NOACTIVATE`, что и
    /// [`Self::set_click_through`], но без `SetForegroundWindow` — для окон
    /// других мониторов при входе/выходе из режима редактирования (M3):
    /// клик-прозрачность должна сняться у всех окон сразу (иначе мышь
    /// проваливалась бы сквозь режим на них), а фокус — только у окна,
    /// которое инициировало переключение.
    pub fn set_interactive(&self, interactive: bool) {
        self.set_exstyle_bits(!interactive);
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
        // SAFETY: hwnd — наше живое окно; смена GWL_EXSTYLE безопасна с
        // любого потока (в отличие от владения самим HWND).
        unsafe {
            let mut ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            let bits = WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0;
            if click_through {
                ex |= bits;
            } else {
                ex &= !bits;
            }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex as isize);
            // SetWindowLongPtrW само по себе не гарантирует, что менеджер
            // окон немедленно перечитает кэшированные ex-стили (MS Learn:
            // «Some window data is cached, so changes you make ... will not
            // take effect until you call SetWindowPos»); SWP_FRAMECHANGED —
            // штатный способ форсировать пересчёт без реального
            // перемещения/ресайза/z-order/фокуса (найдено брейнштормом
            // ботов-воркеров, 2026-08-09).
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

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
            let _ = PostMessageW(Some(self.hwnd), WM_APP_RELEASE_CAPTURE, WPARAM(0), LPARAM(0));
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

/// Состояние, живущее на потоке оверлея между сообщениями: захват мыши,
/// курсор и канал событий. Хранится через `GWLP_USERDATA` (стандартный Win32
/// паттерн — `wndproc` не может захватывать переменные, это `extern "system"
/// fn`), владение — у `run_message_loop`, освобождается в `WM_NCDESTROY`.
struct WndState {
    capture: MouseCapture,
    cursor: CursorManager,
    tx: Sender<OverlayEvent>,
}

/// Смаппить ошибку регистрации хоткея на событие оверлея. Наружу уходит
/// только пользовательский случай — [`Win32Error::HotkeyConflict`]: комбинация
/// занята другим приложением, пользователя надо предупредить и предложить
/// другую (M2b6). Прочие ошибки `RegisterHotKey` — редкие системные сбои,
/// им достаточно warn-лога в `run_message_loop`.
fn hotkey_conflict_event(err: &Win32Error, name: HotkeyName) -> Option<OverlayEvent> {
    match err {
        Win32Error::HotkeyConflict(combo) => {
            Some(OverlayEvent::HotkeyConflict(name, combo.clone()))
        }
        _ => None,
    }
}

fn run_message_loop(
    ready_tx: Sender<ReadyResult>,
    event_tx: Sender<OverlayEvent>,
    bounds_px: Rect,
    edit_hotkey: Option<HotkeyCombo>,
    toggle_all_hotkey: Option<HotkeyCombo>,
    mute_all_hotkey: Option<HotkeyCombo>,
) {
    let hwnd = match create_window(bounds_px) {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // Хоткеи — на этом же потоке (тип `!Send`, ADR-009). Конфликт окно не
    // ломает (ARCHITECTURE.md, раздел 5.1), но наружу уходит событием
    // `OverlayEvent::HotkeyConflict`, чтобы координатор мог предупредить
    // пользователя, а не только warn-лог в трассировке (M2b6).
    let _hotkey = match edit_hotkey {
        Some(combo) => match RegisteredHotkey::register(EDIT_HOTKEY_ID, combo) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(error = %e, "не удалось зарегистрировать хоткей режима редактирования");
                if let Some(event) = hotkey_conflict_event(&e, HotkeyName::EditMode) {
                    let _ = event_tx.send(event);
                }
                None
            }
        },
        // M3: глобальный хоткей регистрирует ровно одно окно на процесс
        // (docs/M3_PREP_NOTES.md, раздел 3.3); остальные создаются без него.
        None => None,
    };
    // «Показать/скрыть все стикеры» — опциональный хоткей: при `None`
    // (пусто/не парсится в конфиге) просто не регистрируется (M2b7).
    let _toggle_all = match toggle_all_hotkey {
        Some(combo) => match RegisteredHotkey::register(TOGGLE_ALL_HOTKEY_ID, combo) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "не удалось зарегистрировать хоткей «показать/скрыть все стикеры»"
                );
                if let Some(event) = hotkey_conflict_event(&e, HotkeyName::ToggleAllStickers) {
                    let _ = event_tx.send(event);
                }
                None
            }
        },
        None => None,
    };
    // «Заглушить все стикеры» — опциональный хоткей, тот же паттерн, что
    // и «показать/скрыть все стикеры» выше (M5d).
    let _mute_all = match mute_all_hotkey {
        Some(combo) => match RegisteredHotkey::register(MUTE_ALL_HOTKEY_ID, combo) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "не удалось зарегистрировать хоткей «заглушить все стикеры»"
                );
                if let Some(event) = hotkey_conflict_event(&e, HotkeyName::MuteAll) {
                    let _ = event_tx.send(event);
                }
                None
            }
        },
        None => None,
    };

    // Клон для перехвата WM_HOTKEY прямо в цикле сообщений (см. ниже) —
    // wndproc его не увидит (сообщение с hwnd=NULL).
    let hotkey_tx = event_tx.clone();

    let state = Box::new(WndState {
        capture: MouseCapture::new(hwnd),
        cursor: CursorManager::new(),
        tx: event_tx,
    });
    // SAFETY: hwnd — наше окно этого потока; указатель освобождается в
    // WM_NCDESTROY ниже (единственное место, где он читается и дропается).
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
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
        if msg.message == WM_HOTKEY && msg.hwnd.0.is_null() {
            let id = message_hotkey_id(msg.wParam);
            if id == EDIT_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleEditMode);
            } else if id == TOGGLE_ALL_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleAllStickers);
            } else if id == MUTE_ALL_HOTKEY_ID {
                let _ = hotkey_tx.send(OverlayEvent::ToggleMuteAll);
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
        WM_NCDESTROY => {
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
            let CursorShape::Rotate(decoded_angle) = decoded.expect("Rotate декодируется") else {
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
        assert_eq!(cursor_shape_from_wparam(WPARAM(6)), None);
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
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None)
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
        let (overlay, _events) = OverlayWindow::create_on_monitor(test_bounds(), None, None, None)
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
        let (overlay, _events) = OverlayWindow::create_on_monitor(test_bounds(), None, None, None)
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

    #[test]
    fn set_click_through_toggles_exstyle_bits() {
        let (overlay, _events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None)
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
            OverlayWindow::create_on_monitor(test_bounds(), Some(test_hotkey()), None, None)
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
        let (overlay, _events) = OverlayWindow::create_on_monitor(test_bounds(), None, None, None)
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
    fn hotkey_conflict_maps_only_conflicts() {
        // Конфликт → событие с той же каноничной комбинацией.
        let conflict = Win32Error::HotkeyConflict("Ctrl+Alt+Shift+F22".to_string());
        assert_eq!(
            hotkey_conflict_event(&conflict, HotkeyName::EditMode),
            Some(OverlayEvent::HotkeyConflict(
                HotkeyName::EditMode,
                "Ctrl+Alt+Shift+F22".to_string()
            ))
        );
        // Имя хоткея прокидывается насквозь, а не всегда EditMode.
        assert_eq!(
            hotkey_conflict_event(&conflict, HotkeyName::MuteAll),
            Some(OverlayEvent::HotkeyConflict(
                HotkeyName::MuteAll,
                "Ctrl+Alt+Shift+F22".to_string()
            ))
        );
        // Прочие ошибки регистрации события не порождают — им хватает warn-лога.
        assert_eq!(
            hotkey_conflict_event(&Win32Error::OverlayWindowCreateFailed, HotkeyName::EditMode),
            None
        );
        assert_eq!(
            hotkey_conflict_event(
                &Win32Error::InvalidHotkey("Ctrl".to_string()),
                HotkeyName::EditMode
            ),
            None
        );
    }

    #[test]
    fn second_window_with_same_hotkey_reports_conflict() {
        // Экзотическая комбинация — не конфликтует с реальными приложениями
        // на машине разработчика/CI (F22 не используется другими тестами).
        let combo = HotkeyCombo::parse("Ctrl+Alt+Shift+F22").expect("валидная комбинация");
        let (_first, _first_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(combo), None, None)
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(combo), None, None)
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
            OverlayWindow::create_on_monitor(test_bounds(), Some(combo), None, None)
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, None).expect("второе окно");

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
        )
        .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), Some(edit2), Some(toggle), None)
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
            OverlayWindow::create_on_monitor(test_bounds(), None, None, Some(mute))
                .expect("первое окно");
        let (_second, second_events) =
            OverlayWindow::create_on_monitor(test_bounds(), None, None, Some(mute))
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
    fn dpi_changed_applies_recommended_rect_and_reports_event() {
        let (overlay, _events) = OverlayWindow::create_on_monitor(test_bounds(), None, None, None)
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
        let (overlay, _events) = OverlayWindow::create_on_monitor(test_bounds(), None, None, None)
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
        let (overlay, events) = OverlayWindow::create_on_monitor(test_bounds(), None, None, None)
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
}
