//! Ввод режима редактирования: захват мыши на оверлей-окне и системные
//! курсоры (SPEC.md, раздел 3.3; ARCHITECTURE.md, разделы 5.2–5.3).
//!
//! Модуль — «переводчик»: wndproc оверлей-окна скармливает ему сырые
//! сообщения, наружу выходит безопасный [`InputEvent`] без Win32-типов.
//! Захват (`SetCapture`/`ReleaseCapture`) делается здесь же, чтобы кнопка,
//! отпущенная за пределами окна во время перетаскивания, не терялась.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HCURSOR, HTCLIENT, IDC_ARROW, IDC_CROSS, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE,
    IDC_SIZEWE, LoadCursorW, SetCursor, WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE,
};
use windows::core::PCWSTR;

/// Точка в клиентских координатах оверлей-окна, физические пиксели
/// (процесс PerMonitorV2 — см. манифест; на мультимониторе координаты
/// бывают отрицательными, ARCHITECTURE.md, раздел 7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// Модификаторы клавиатуры в момент события мыши. Редактор обязан видеть
/// их в каждом событии: `Shift` — пропорции при ресайзе и шаг 15° при
/// повороте, `Alt` — масштаб от центра, `Ctrl` — отключить магнит
/// (SPEC 3.3–3.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Modifiers {
    /// Чистое ядро: биты `MK_*` из `wparam` плюс состояние Alt (в `MK_*`
    /// флага для Alt нет).
    fn from_flags(wparam_bits: u32, alt_down: bool) -> Self {
        Self {
            shift: wparam_bits & MK_SHIFT.0 != 0,
            ctrl: wparam_bits & MK_CONTROL.0 != 0,
            alt: alt_down,
        }
    }

    /// Снять состояние для мышиного сообщения. Alt читается через
    /// `GetKeyState`, т.к. в `wparam` мышиных сообщений его нет.
    fn current(wparam: WPARAM) -> Self {
        // SAFETY: чтение состояния клавиши вызывающего потока; старший бит
        // результата — «клавиша нажата».
        let alt_down = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
        Self::from_flags(wparam.0 as u32, alt_down)
    }
}

/// Безопасное событие мыши для режима редактирования. Никаких Win32-типов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Левая кнопка нажата, захват мыши установлен.
    MouseDown { pos: Point, modifiers: Modifiers },
    /// Курсор переместился; `dragging` — зажата левая кнопка (у окна захват).
    MouseMove {
        pos: Point,
        modifiers: Modifiers,
        dragging: bool,
    },
    /// Левая кнопка отпущена после [`InputEvent::MouseDown`], захват снят.
    MouseUp { pos: Point, modifiers: Modifiers },
    /// Захват потерян извне (`WM_CAPTURECHANGED`): система или другое окно
    /// забрали его. Активный жест надо отменить, а не завершить.
    CaptureLost,
}

/// Побочное действие над захватом, которое надо выполнить в Win32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureOp {
    None,
    Set,
    Release,
}

/// Чистый автомат состояния захвата: все решения принимаются без
/// Win32-вызовов и полностью покрыты юнит-тестами; Win32-обёртка —
/// [`MouseCapture`]. Возвращает (новый captured, действие, событие).
fn transition(
    captured: bool,
    msg: u32,
    pos: Point,
    modifiers: Modifiers,
) -> (bool, CaptureOp, Option<InputEvent>) {
    match msg {
        WM_LBUTTONDOWN => (
            true,
            CaptureOp::Set,
            Some(InputEvent::MouseDown { pos, modifiers }),
        ),
        WM_MOUSEMOVE => (
            captured,
            CaptureOp::None,
            Some(InputEvent::MouseMove {
                pos,
                modifiers,
                dragging: captured,
            }),
        ),
        WM_LBUTTONUP if captured => (
            false,
            CaptureOp::Release,
            Some(InputEvent::MouseUp { pos, modifiers }),
        ),
        // Отпускание без нашего захвата — не наше событие, уходит в DefWindowProc.
        WM_LBUTTONUP => (false, CaptureOp::None, None),
        WM_CAPTURECHANGED if captured => (false, CaptureOp::None, Some(InputEvent::CaptureLost)),
        _ => (captured, CaptureOp::None, None),
    }
}

/// `GET_X_LPARAM`/`GET_Y_LPARAM`: координаты — знаковые 16-битные поля.
fn lparam_point(lparam: LPARAM) -> Point {
    Point {
        x: lparam.0 as i16 as i32,
        y: (lparam.0 >> 16) as i16 as i32,
    }
}
/// Захват мыши для оверлей-окна. Живёт на потоке окна (`HWND` — `!Send`).
pub struct MouseCapture {
    hwnd: HWND,
    captured: bool,
}

impl MouseCapture {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            captured: false,
        }
    }

    /// `true`, пока окно держит захват мыши (между [`InputEvent::MouseDown`]
    /// и [`InputEvent::MouseUp`]/[`InputEvent::CaptureLost`]).
    pub fn is_captured(&self) -> bool {
        self.captured
    }

    /// Обработать сообщение окна. `Some(event)` — сообщение наше, wndproc
    /// возвращает 0; `None` — передать в `DefWindowProcW`.
    pub fn handle_message(
        &mut self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<InputEvent> {
        let pos = lparam_point(lparam);
        let modifiers = Modifiers::current(wparam);
        let (captured, op, event) = transition(self.captured, msg, pos, modifiers);
        self.captured = captured;
        match op {
            CaptureOp::None => {}
            // SAFETY: hwnd — живое окно этого потока (инвариант типа);
            // SetCapture/ReleaseCapture — стандартная пара для перетаскивания.
            CaptureOp::Set => unsafe {
                let _ = SetCapture(self.hwnd);
            },
            CaptureOp::Release => unsafe {
                let _ = ReleaseCapture();
            },
        }
        event
    }
}

/// Одна из восьми ручек рамки выделения (SPEC 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl Handle {
    /// Угловая ручка, если это угол: только у углов есть зона поворота
    /// (SPEC 3.3).
    pub fn corner(self) -> Option<Corner> {
        match self {
            Handle::NorthEast => Some(Corner::NorthEast),
            Handle::SouthEast => Some(Corner::SouthEast),
            Handle::SouthWest => Some(Corner::SouthWest),
            Handle::NorthWest => Some(Corner::NorthWest),
            _ => None,
        }
    }
}

/// Угловая ручка рамки выделения. Зона поворота — кольцо 6–24 логических
/// пикселей наружу от неё (SPEC 3.3, ARCHITECTURE.md 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    NorthEast,
    SouthEast,
    SouthWest,
    NorthWest,
}

/// Зона редактора под курсором. Хит-тест с обратной аффинной трансформацией
/// делает ядро (ARCHITECTURE.md 5.3); здесь зона лишь отображается в форму
/// курсора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorZone {
    /// Пустое место оверлея.
    Background,
    /// Тело выделенного стикера — перемещение.
    StickerBody,
    /// Ручка ресайза.
    ResizeHandle(Handle),
    /// Кольцо поворота за угловой ручкой.
    RotateZone(Corner),
}

/// Системный курсор, соответствующий зоне.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Arrow,
    Move,
    SizeNS,
    SizeWE,
    SizeNESW,
    SizeNWSE,
    /// SPEC 3.3 требует иконку поворота; до появления `.cur`-ресурса
    /// (UI-ассеты M2) показываем Cross — визуально отличный от всех ресайзов.
    Rotate,
}

impl CursorZone {
    /// Форма курсора для зоны. Оси курсоров ресайза — экранные, вслед за
    /// поворотом стикера не вращаются (как в большинстве редакторов).
    pub fn cursor_shape(self) -> CursorShape {
        match self {
            CursorZone::Background => CursorShape::Arrow,
            CursorZone::StickerBody => CursorShape::Move,
            CursorZone::ResizeHandle(handle) => match handle {
                Handle::North | Handle::South => CursorShape::SizeNS,
                Handle::East | Handle::West => CursorShape::SizeWE,
                Handle::NorthEast | Handle::SouthWest => CursorShape::SizeNESW,
                Handle::NorthWest | Handle::SouthEast => CursorShape::SizeNWSE,
            },
            CursorZone::RotateZone(_) => CursorShape::Rotate,
        }
    }
}
/// Загруженные системные курсоры. Они разделяемые (shared), `DestroyCursor`
/// для них вызывать нельзя — поэтому `Drop` не нужен.
struct CursorSet {
    arrow: HCURSOR,
    size_all: HCURSOR,
    size_ns: HCURSOR,
    size_we: HCURSOR,
    size_nesw: HCURSOR,
    size_nwse: HCURSOR,
    rotate: HCURSOR,
}

impl CursorSet {
    fn load() -> Self {
        let arrow = load_system_cursor(IDC_ARROW);
        // Если какой-то из курсоров не загрузился (практически недостижимо),
        // откатываемся на стрелку; если и она — на null (SetCursor его примет).
        let or_arrow = |idc: PCWSTR| load_system_cursor(idc).or(arrow).unwrap_or_default();
        Self {
            arrow: arrow.unwrap_or_default(),
            size_all: or_arrow(IDC_SIZEALL),
            size_ns: or_arrow(IDC_SIZENS),
            size_we: or_arrow(IDC_SIZEWE),
            size_nesw: or_arrow(IDC_SIZENESW),
            size_nwse: or_arrow(IDC_SIZENWSE),
            rotate: or_arrow(IDC_CROSS),
        }
    }

    fn get(&self, shape: CursorShape) -> HCURSOR {
        match shape {
            CursorShape::Arrow => self.arrow,
            CursorShape::Move => self.size_all,
            CursorShape::SizeNS => self.size_ns,
            CursorShape::SizeWE => self.size_we,
            CursorShape::SizeNESW => self.size_nesw,
            CursorShape::SizeNWSE => self.size_nwse,
            CursorShape::Rotate => self.rotate,
        }
    }
}

fn load_system_cursor(idc: PCWSTR) -> Option<HCURSOR> {
    // SAFETY: стандартный системный курсор; hinstance=None — разделяемый
    // ресурс системы, освобождать не требуется.
    unsafe { LoadCursorW(None, idc) }.ok()
}

/// Форма курсора оверлей-окна в режиме редактирования: кэш системных
/// курсоров + обработка `WM_SETCURSOR`.
pub struct CursorManager {
    cursors: CursorSet,
    current: CursorShape,
}

impl CursorManager {
    pub fn new() -> Self {
        Self {
            cursors: CursorSet::load(),
            current: CursorShape::Arrow,
        }
    }

    /// Текущая форма курсора.
    pub fn current(&self) -> CursorShape {
        self.current
    }

    /// Сменить форму (вызывается при смене зоны под курсором). Возвращает
    /// `false`, если форма не изменилась, — лишний `SetCursor` не делаем.
    pub fn set_shape(&mut self, shape: CursorShape) -> bool {
        if shape == self.current {
            return false;
        }
        self.current = shape;
        self.apply_current();
        true
    }

    /// Обработать `WM_SETCURSOR`: `Some(LRESULT(1))`, если курсор в
    /// клиентской зоне (мы восстановили свою форму — Windows сбрасывает её
    /// на классовую при каждом перемещении); `None` — отдать в
    /// `DefWindowProcW` (неклиентская зона).
    pub fn handle_set_cursor(&mut self, lparam: LPARAM) -> Option<LRESULT> {
        if (lparam.0 as u32 & 0xFFFF) == HTCLIENT {
            self.apply_current();
            Some(LRESULT(1))
        } else {
            None
        }
    }

    fn apply_current(&self) {
        // SAFETY: системный разделяемый курсор, живёт вечно; SetCursor
        // не требует принадлежности к конкретному окну.
        unsafe {
            let _ = SetCursor(Some(self.cursors.get(self.current)));
        }
    }
}

impl Default for CursorManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetCapture;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW,
        WS_OVERLAPPED,
    };
    use windows::core::w;

    fn mk_lparam(x: i16, y: i16) -> LPARAM {
        LPARAM((((y as u16 as u32) << 16) | (x as u16 as u32)) as isize)
    }

    #[test]
    fn lparam_point_decodes_signed_coords() {
        assert_eq!(lparam_point(mk_lparam(10, 20)), Point { x: 10, y: 20 });
        // Отрицательные координаты (второй монитор слева) — знаковые поля.
        assert_eq!(lparam_point(mk_lparam(-100, -5)), Point { x: -100, y: -5 });
    }

    #[test]
    fn modifiers_from_flags() {
        let m = Modifiers::from_flags(MK_SHIFT.0 | MK_CONTROL.0, true);
        assert!(m.shift && m.ctrl && m.alt);
        let m = Modifiers::from_flags(0, false);
        assert!(!m.shift && !m.ctrl && !m.alt);
    }

    #[test]
    fn transition_state_machine() {
        let pos = Point { x: 1, y: 2 };
        let mods = Modifiers::default();

        // Нажатие: захват ставится, событие MouseDown.
        let (captured, op, event) = transition(false, WM_LBUTTONDOWN, pos, mods);
        assert!(captured);
        assert_eq!(op, CaptureOp::Set);
        assert_eq!(
            event,
            Some(InputEvent::MouseDown {
                pos,
                modifiers: mods
            })
        );

        // Движение с захватом — dragging.
        let (captured, op, event) = transition(true, WM_MOUSEMOVE, pos, mods);
        assert!(captured);
        assert_eq!(op, CaptureOp::None);
        assert_eq!(
            event,
            Some(InputEvent::MouseMove {
                pos,
                modifiers: mods,
                dragging: true
            })
        );

        // Движение без захвата — hover.
        let (_, _, event) = transition(false, WM_MOUSEMOVE, pos, mods);
        assert_eq!(
            event,
            Some(InputEvent::MouseMove {
                pos,
                modifiers: mods,
                dragging: false
            })
        );

        // Отпускание при захвате: захват снимается, событие MouseUp.
        let (captured, op, event) = transition(true, WM_LBUTTONUP, pos, mods);
        assert!(!captured);
        assert_eq!(op, CaptureOp::Release);
        assert_eq!(
            event,
            Some(InputEvent::MouseUp {
                pos,
                modifiers: mods
            })
        );

        // Отпускание без захвата — не наше сообщение.
        let (captured, op, event) = transition(false, WM_LBUTTONUP, pos, mods);
        assert!(!captured);
        assert_eq!(op, CaptureOp::None);
        assert_eq!(event, None);

        // Потеря захвата извне.
        let (captured, op, event) = transition(true, WM_CAPTURECHANGED, pos, mods);
        assert!(!captured);
        assert_eq!(op, CaptureOp::None);
        assert_eq!(event, Some(InputEvent::CaptureLost));

        // WM_CAPTURECHANGED без захвата и посторонние сообщения — игнор.
        let (_, _, event) = transition(false, WM_CAPTURECHANGED, pos, mods);
        assert_eq!(event, None);
        let (captured, op, event) = transition(true, 0x9999, pos, mods);
        assert!(captured);
        assert_eq!(op, CaptureOp::None);
        assert_eq!(event, None);
    }

    #[test]
    fn zone_to_shape_mapping() {
        assert_eq!(CursorZone::Background.cursor_shape(), CursorShape::Arrow);
        assert_eq!(CursorZone::StickerBody.cursor_shape(), CursorShape::Move);
        assert_eq!(
            CursorZone::ResizeHandle(Handle::North).cursor_shape(),
            CursorShape::SizeNS
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::South).cursor_shape(),
            CursorShape::SizeNS
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::East).cursor_shape(),
            CursorShape::SizeWE
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::West).cursor_shape(),
            CursorShape::SizeWE
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::NorthEast).cursor_shape(),
            CursorShape::SizeNESW
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::SouthWest).cursor_shape(),
            CursorShape::SizeNESW
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::NorthWest).cursor_shape(),
            CursorShape::SizeNWSE
        );
        assert_eq!(
            CursorZone::ResizeHandle(Handle::SouthEast).cursor_shape(),
            CursorShape::SizeNWSE
        );
        assert_eq!(
            CursorZone::RotateZone(Corner::NorthEast).cursor_shape(),
            CursorShape::Rotate
        );
    }

    #[test]
    fn handle_corner_detection() {
        assert_eq!(Handle::North.corner(), None);
        assert_eq!(Handle::West.corner(), None);
        assert_eq!(Handle::NorthEast.corner(), Some(Corner::NorthEast));
        assert_eq!(Handle::SouthEast.corner(), Some(Corner::SouthEast));
        assert_eq!(Handle::SouthWest.corner(), Some(Corner::SouthWest));
        assert_eq!(Handle::NorthWest.corner(), Some(Corner::NorthWest));
    }

    /// Скрытое окно текущего тест-потока для проверки захвата по-настоящему.
    struct TestWindow(HWND);

    impl TestWindow {
        fn create() -> Self {
            // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
            let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(test_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: w!("resticker_input_test"),
                ..Default::default()
            };
            // SAFETY: wc заполнена корректно. Класс процесс-wide: повторная
            // регистрация (параллельные тесты) — не ошибка.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                // SAFETY: осмысленна сразу после провалившегося вызова.
                let err = unsafe { GetLastError() };
                assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
            }
            // SAFETY: все аргументы — валидные константы и зарегистрированный
            // класс; окно скрытое, принадлежит текущему потоку.
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("resticker_input_test"),
                    w!("test"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    100,
                    100,
                    None,
                    None,
                    Some(hinstance.into()),
                    None,
                )
            }
            .expect("создание тестового окна");
            Self(hwnd)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: окно создано этим же потоком выше.
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    unsafe extern "system" fn test_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    #[test]
    fn capture_lifecycle_with_real_window() {
        let wnd = TestWindow::create();
        let mut cap = MouseCapture::new(wnd.0);
        assert!(!cap.is_captured());

        let shift_only = Modifiers {
            shift: true,
            ctrl: false,
            alt: false,
        };
        let down = cap.handle_message(
            WM_LBUTTONDOWN,
            WPARAM(MK_SHIFT.0 as usize),
            mk_lparam(10, 20),
        );
        assert_eq!(
            down,
            Some(InputEvent::MouseDown {
                pos: Point { x: 10, y: 20 },
                modifiers: shift_only
            })
        );
        assert!(cap.is_captured());
        // SAFETY: простое чтение захвата текущего потока.
        assert_eq!(unsafe { GetCapture() }, wnd.0);

        let mv = cap.handle_message(WM_MOUSEMOVE, WPARAM(0), mk_lparam(-5, 7));
        assert_eq!(
            mv,
            Some(InputEvent::MouseMove {
                pos: Point { x: -5, y: 7 },
                modifiers: Modifiers::default(),
                dragging: true
            })
        );

        let up = cap.handle_message(WM_LBUTTONUP, WPARAM(0), mk_lparam(-5, 7));
        assert_eq!(
            up,
            Some(InputEvent::MouseUp {
                pos: Point { x: -5, y: 7 },
                modifiers: Modifiers::default()
            })
        );
        assert!(!cap.is_captured());
        // SAFETY: простое чтение захвата текущего потока.
        assert!(unsafe { GetCapture() }.0.is_null());

        // Отпускание без захвата — не наше сообщение.
        assert_eq!(
            cap.handle_message(WM_LBUTTONUP, WPARAM(0), mk_lparam(0, 0)),
            None
        );

        // Повторный захват и потеря его извне: система сначала отзывает
        // захват, затем шлёт WM_CAPTURECHANGED — воспроизводим это.
        let _ = cap.handle_message(WM_LBUTTONDOWN, WPARAM(0), mk_lparam(1, 1));
        assert!(cap.is_captured());
        // SAFETY: снимаем захват, поставленный этим же потоком выше.
        unsafe {
            let _ = ReleaseCapture();
        }
        let lost = cap.handle_message(WM_CAPTURECHANGED, WPARAM(0), LPARAM(0));
        assert_eq!(lost, Some(InputEvent::CaptureLost));
        assert!(!cap.is_captured());
    }

    #[test]
    fn cursor_manager_set_shape_idempotent() {
        let mut cm = CursorManager::new();
        assert_eq!(cm.current(), CursorShape::Arrow);
        // Форма не изменилась — лишний SetCursor не делаем.
        assert!(!cm.set_shape(CursorShape::Arrow));
        assert!(cm.set_shape(CursorShape::Move));
        assert_eq!(cm.current(), CursorShape::Move);
        assert!(cm.set_shape(CursorShape::Rotate));
        assert_eq!(cm.current(), CursorShape::Rotate);
    }

    #[test]
    fn set_cursor_message_handling() {
        let mut cm = CursorManager::new();
        // Клиентская зона — показываем свою форму и гасим сообщение.
        assert_eq!(
            cm.handle_set_cursor(LPARAM(HTCLIENT as isize)),
            Some(LRESULT(1))
        );
        // Неклиентская зона (HTCAPTION) — отдаём в DefWindowProc.
        assert_eq!(cm.handle_set_cursor(LPARAM(2)), None);
    }
}
