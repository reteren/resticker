//! Ввод режима редактирования: захват мыши на оверлей-окне и системные
//! курсоры (SPEC.md, раздел 3.3; ARCHITECTURE.md, разделы 5.2–5.3).
//!
//! Модуль — «переводчик»: wndproc оверлей-окна скармливает ему сырые
//! сообщения, наружу выходит безопасный [`InputEvent`] без Win32-типов.
//! Захват (`SetCapture`/`ReleaseCapture`) делается здесь же, чтобы кнопка,
//! отпущенная за пределами окна во время перетаскивания, не терялась.

use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, VK_LBUTTON, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HCURSOR, HICON, HTCLIENT, ICONINFO, IDC_ARROW, IDC_CROSS,
    IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, LoadCursorW, SetCursor,
    WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
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
    /// Колесо мыши повёрнуто (`WM_MOUSEWHEEL`) — не связано с захватом
    /// мыши/драгом, отдельное событие. `notches` — число «щелчков» колеса,
    /// знак совпадает с сырым `WHEEL_DELTA` (положительное — от себя,
    /// отрицательное — на себя); дробные повороты (touchpad) уже
    /// накоплены системой до целого шага. Живой репорт пользователя:
    /// длинный список окон в панели «Слои видимости» не листался вообще —
    /// колесо мыши никак не обрабатывалось.
    MouseWheel { notches: i32 },
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

/// Реальное физическое состояние левой кнопки мыши прямо сейчас, в обход
/// доставки/интерпретации оконных сообщений — источник истины, которому
/// нельзя не доверять (в отличие от `captured`, который может
/// рассинхронизироваться с ним из-за гонок доставки сообщений между
/// потоками/окнами).
fn left_button_physically_down() -> bool {
    // SAFETY: GetAsyncKeyState безопасен с любого потока, аргумент —
    // константа виртуальной клавиши; отрицательный результат — старший бит
    // установлен, клавиша нажата прямо сейчас (тот же приём, что у
    // `GetKeyState` в `Modifiers::current` выше).
    unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 }
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
    /// Последняя физическая позиция, на которую реально среагировал драг
    /// (не последняя ВООБЩЕ увиденная — см. `handle_message_checked`).
    last_drag_pos: Option<Point>,
}

impl MouseCapture {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            captured: false,
            last_drag_pos: None,
        }
    }

    /// `true`, пока окно держит захват мыши (между [`InputEvent::MouseDown`]
    /// и [`InputEvent::MouseUp`]/[`InputEvent::CaptureLost`]).
    pub fn is_captured(&self) -> bool {
        self.captured
    }

    /// Обработать сообщение окна. `Some(event)` — сообщение наше, wndproc
    /// возвращает 0; `None` — передать в `DefWindowProcW`. Реальное
    /// физическое состояние левой кнопки мыши читается здесь же
    /// ([`left_button_physically_down`]) — см. [`Self::handle_message_checked`]
    /// за самой защитной логикой и тестируемой версией с инъекцией
    /// состояния кнопки.
    pub fn handle_message(
        &mut self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<InputEvent> {
        self.handle_message_checked(msg, wparam, lparam, left_button_physically_down())
    }

    /// Та же обработка, что [`Self::handle_message`], но состояние левой
    /// кнопки мыши передаётся явно — тестируемое ядро без зависимости от
    /// реального `GetAsyncKeyState` (юнит-тесты шлют синтетические
    /// сообщения без настоящего нажатия, поэтому `handle_message` в тестах
    /// всегда видел бы кнопку отпущенной).
    ///
    /// Защитная проверка вместо слепого доверия внутреннему `captured`:
    /// если у нас идёт драг (`self.captured`), а физическая левая кнопка
    /// мыши на самом деле уже не нажата — `WM_MOUSEMOVE` может прийти
    /// рассинхронизированным с реальным состоянием кнопки (источник живого
    /// «стикер дрейфует сам по себе спустя ~0.5с после начала
    /// перетаскивания», разобранного в этой сессии) — обрабатываем
    /// сообщение как настоящее отпускание, тот же путь, что и честный
    /// `WM_LBUTTONUP`, ничего не дублируем.
    pub fn handle_message_checked(
        &mut self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        left_button_down: bool,
    ) -> Option<InputEvent> {
        let pos = lparam_point(lparam);
        let modifiers = Modifiers::current(wparam);
        let effective_msg = if msg == WM_MOUSEMOVE && self.captured && !left_button_down {
            WM_LBUTTONUP
        } else {
            msg
        };
        // Windows документированно может повторно прислать WM_MOUSEMOVE с
        // НЕИЗМЕНИВШИМИСЯ координатами без реального движения курсора —
        // «A window can receive WM_MOUSEMOVE messages even if the mouse did
        // not move» (MSDN, Mouse movement — Win32 apps), в т.ч. когда
        // контент ПОД неподвижным курсором визуально меняется. У этого окна
        // контент под курсором меняется НА КАЖДОМ КАДРЕ драга (сам
        // перетаскиваемый стикер) — то есть именно тот случай, который
        // MSDN описывает, происходит здесь постоянно и может провоцировать
        // Windows на повторную отправку. Раньше любой такой WM_MOUSEMOVE
        // слепо считался «драг продолжается с этой позиции» — источник
        // «стикер дрейфует сам по себе» даже при реально зажатой кнопке:
        // если reset позиции не происходит, а `apply_gesture` пересчитывает
        // от фиксированного `grab_dx`, повтор с той же позицией безвреден,
        // но микро-джиттер реального сенсора мыши между такими
        // спровоцированными пересылками остаётся коротким «шум→движение
        // стикера→снова смена контента под курсором→новая пересылка» и
        // может самоподдерживаться. Дедуп по точному совпадению позиции —
        // самая дешёвая и безопасная защита: настоящее движение мышью
        // почти никогда не даёт бит-в-бит идентичный `lParam` два раза
        // подряд.
        let is_duplicate_move =
            effective_msg == WM_MOUSEMOVE && self.captured && self.last_drag_pos == Some(pos);
        // Диагностика этого пути СНЯТА 2026-09-07: она писала строку в файл
        // лога на каждое WM_MOUSEMOVE во время захвата, то есть делала
        // синхронный ввод-вывод под мьютексом в самом горячем месте жеста.
        // Замер живого лога: 6261 строка из 6505 за день (96 %) — это она;
        // за 24 дня каталог логов вырос до 75 МБ. Причина, ради которой её
        // ставили («стикер дрейфует сам по себе»), найдена и закрыта дедупом
        // повторных WM_MOUSEMOVE выше — сам дедуп остался, ушла только
        // печать.
        if is_duplicate_move {
            return None;
        }
        let (captured, op, event) = transition(self.captured, effective_msg, pos, modifiers);
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
        self.last_drag_pos = if self.captured { Some(pos) } else { None };
        event
    }

    /// Снять захват безусловно, в обход обычного цикла Down→Up/CaptureLost —
    /// для случаев, когда приложение решает выйти из режима, где захват
    /// вообще уместен (выход из режима редактирования), а не дожидается
    /// естественного `WM_LBUTTONUP`. Без этого хоткей выхода, нажатый пока
    /// пользователь ещё держит кнопку мыши (тянет ползунок/жест), оставляет
    /// `SetCapture` за уже клик-прозрачным окном: оно продолжает
    /// монопольно получать ВСЕ мышиные сообщения системы до следующего
    /// физического отпускания кнопки — снаружи выглядит как «мышь
    /// перестала работать» (найдено при разборе бага «HUD/мышь живут своей
    /// жизнью»). Безопасно вызывать и когда захвата уже нет — `ReleaseCapture`
    /// на чужом/отсутствующем захвате не ошибка.
    pub fn force_release(&mut self) {
        self.captured = false;
        // SAFETY: ReleaseCapture не привязан к конкретному HWND — снимает
        // захват с того окна, что его сейчас держит (если оно вообще есть);
        // безвредно, если захвата нет вовсе.
        unsafe {
            let _ = ReleaseCapture();
        }
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
    /// Зона поворота СНАРУЖИ рамки выделения (фидбэк пользователя
    /// 2026-08-09, второй раунд: старое «кольцо вокруг угла» срабатывало
    /// даже ВНУТРИ рамки — теперь это не кольцо на угол, а весь внешний
    /// периметр). Угол наклона стрелки в градусах, экранная конвенция
    /// (по часовой = плюс, см. `create_rotate_cursor`) — ядро считает его
    /// как «направление от центра стикера к ближайшему углу рамки» плюс
    /// текущий поворот стикера, так что стрелка всегда смотрит в реальную
    /// сторону угла в пространстве, а не в фиксированную сторону.
    RotateZone(i32),
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
    /// Маленькая двусторонняя дуга «как в Photoshop» (фидбэк пользователя
    /// 2026-08-09) под произвольным углом (градусы, экранная конвенция) —
    /// изгиб дуги обращён туда же, куда направлен реальный угол рамки
    /// выделения в пространстве (с учётом её собственного поворота), а не
    /// фиксированная константа на 4 угла.
    Rotate(i32),
    /// Перекрестье режима резки окон — «митоз»
    /// (docs/M9_WINDOW_MITOSIS_DESIGN.md, запрос пользователя 2026-09-01).
    /// Системный `IDC_CROSS`: пользователь уже знает эту форму как «сейчас
    /// я укажу точку», и своя рисованная не добавила бы ничего, кроме
    /// расхождения с остальной системой.
    Cross,
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
            CursorZone::RotateZone(angle_deg) => CursorShape::Rotate(angle_deg),
        }
    }
}
/// Размер растрового курсора поворота, px (стандартный некрупный размер
/// курсора — тот же порядок, что у системных 32×32 в 100% DPI).
const ROTATE_CURSOR_SIZE: i32 = 32;

/// Радиус дуги курсора поворота от хотспота, DIP-эквивалент в пикселях
/// растра курсора (фидбэк пользователя 2026-08-09: «7-15 пикселей по
/// диагонали» — маленькая дуга у самого угла, не кольцо во весь курсор).
const ROTATE_ARC_RADIUS_PX: f64 = 11.0;
/// Угловой охват дуги, градусы.
const ROTATE_ARC_SPAN_DEG: f64 = 100.0;
/// Толщина дуги/остриёв, px.
const ROTATE_ARC_THICKNESS_PX: f64 = 2.2;
/// Длина треугольного остриёв стрелок, px.
const ROTATE_ARROWHEAD_LEN_PX: f64 = 4.5;
/// Раствор остриёв стрелок, градусы.
const ROTATE_ARROWHEAD_SPREAD_DEG: f64 = 55.0;

/// RGBA (straight alpha, НЕ premultiplied — конвертируется ниже перед
/// заливкой в DIB) маленькой двусторонней изогнутой стрелки для курсора
/// поворота — «как в Photoshop» (фидбэк пользователя 2026-08-09: дуга
/// радиусом ~7-15px от хотспота, под углом, с остриями на обоих концах —
/// не крестик-плейсхолдер и не кольцо на весь курсор из первой версии).
/// `base_angle_deg` — наклон дуги (0° — вдоль +X): угла NW/SE используют
/// одно значение, NE/SW — отражённое, чтобы стрелка визуально шла вдоль
/// диагонали СВОЕГО угла (см. `create_rotate_cursor`). Белая заливка с
/// чёрной обводкой в один пиксель — контраст на любом фоне, та же
/// конвенция, что у стандартных курсоров Windows. Антиалиасинг 2×2
/// суперсемплингом (не завозим зависимость от rst-render — небольшой
/// самостоятельный алгоритм, та же техника, что у иконок тулбара).
fn rotate_cursor_rgba(base_angle_deg: f64) -> Vec<u8> {
    const N: usize = ROTATE_CURSOR_SIZE as usize;
    const SUB: [(f64, f64); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
    let s = N as f64;
    let (cx, cy) = (0.5 * s, 0.5 * s);
    let r = ROTATE_ARC_RADIUS_PX;
    let h = ROTATE_ARC_THICKNESS_PX / 2.0;
    let arc_span = ROTATE_ARC_SPAN_DEG.to_radians();
    let base_angle = base_angle_deg.to_radians();
    let start_a = base_angle - arc_span / 2.0;
    let end_a = base_angle + arc_span / 2.0;

    let in_arc = |x: f64, y: f64| -> bool {
        let (dx, dy) = (x - cx, y - cy);
        let dist = (dx * dx + dy * dy).sqrt();
        if (dist - r).abs() > h {
            return false;
        }
        let mut angle = dy.atan2(dx);
        if angle < start_a {
            angle += 2.0 * std::f64::consts::PI;
        }
        (start_a..=end_a).contains(&angle)
    };

    let sign = |ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64| -> f64 {
        (ax - cx) * (by - cy) - (bx - cx) * (ay - cy)
    };
    let in_triangle = |x: f64, y: f64, p: (f64, f64), b1: (f64, f64), b2: (f64, f64)| -> bool {
        let d1 = sign(x, y, p.0, p.1, b1.0, b1.1);
        let d2 = sign(x, y, b1.0, b1.1, b2.0, b2.1);
        let d3 = sign(x, y, b2.0, b2.1, p.0, p.1);
        let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(has_neg && has_pos)
    };
    // Остриё на каждом конце дуги: у `start_a` — развёрнуто назад (стрелка
    // «входит» в дугу), у `end_a` — вперёд по касательной (та же логика,
    // что у обоих концов двусторонней дуги в проверенном прототипе).
    let arrowhead = |end_angle: f64, reversed: bool| -> ((f64, f64), (f64, f64), (f64, f64)) {
        let (tx, ty) = (cx + r * end_angle.cos(), cy + r * end_angle.sin());
        let tangent = end_angle + std::f64::consts::FRAC_PI_2;
        let tip_dir = if reversed {
            tangent + std::f64::consts::PI
        } else {
            tangent
        };
        let head_len = ROTATE_ARROWHEAD_LEN_PX;
        let (tipx, tipy) = (tx + head_len * tip_dir.cos(), ty + head_len * tip_dir.sin());
        let perp = tip_dir + std::f64::consts::FRAC_PI_2;
        let spread = head_len * (ROTATE_ARROWHEAD_SPREAD_DEG.to_radians() / 2.0).tan() * 2.0;
        let b1 = (tx + spread * perp.cos(), ty + spread * perp.sin());
        let b2 = (tx - spread * perp.cos(), ty - spread * perp.sin());
        ((tipx, tipy), b1, b2)
    };
    let (tip_start, b1_start, b2_start) = arrowhead(start_a, true);
    let (tip_end, b1_end, b2_end) = arrowhead(end_a, false);

    let mut fill = vec![0.0f32; N * N];
    for py in 0..N {
        for px in 0..N {
            let (x, y) = (px as f64, py as f64);
            let mut hits = 0u32;
            for (sx, sy) in SUB {
                let (sx, sy) = (x + sx, y + sy);
                if in_arc(sx, sy)
                    || in_triangle(sx, sy, tip_start, b1_start, b2_start)
                    || in_triangle(sx, sy, tip_end, b1_end, b2_end)
                {
                    hits += 1;
                }
            }
            fill[py * N + px] = hits as f32 / SUB.len() as f32;
        }
    }

    let mut out = vec![0u8; N * N * 4];
    for py in 0..N {
        for px in 0..N {
            let f = fill[py * N + px];
            let (color, a) = if f > 0.0 {
                ([255u8, 255, 255], (f * 255.0).round() as u8)
            } else {
                // Обводка: чёрный, если хоть один из 8 соседей заполнен —
                // стандартная дилатация на 1px для контурной окантовки.
                let mut has_filled_neighbor = false;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (px as i32 + dx, py as i32 + dy);
                        if nx >= 0
                            && nx < N as i32
                            && ny >= 0
                            && ny < N as i32
                            && fill[ny as usize * N + nx as usize] > 0.5
                        {
                            has_filled_neighbor = true;
                        }
                    }
                }
                if has_filled_neighbor {
                    ([0u8, 0, 0], 255u8)
                } else {
                    ([0, 0, 0], 0)
                }
            };
            let i = (py * N + px) * 4;
            out[i] = color[0];
            out[i + 1] = color[1];
            out[i + 2] = color[2];
            out[i + 3] = a;
        }
    }
    out
}

/// Создать Win32-курсор поворота из [`rotate_cursor_rgba`] через
/// `CreateDIBSection`(32bpp top-down BGRA, premultiplied — Windows требует
/// premultiplied alpha для альфа-курсоров, MS Learn «Alpha Cursors and
/// Icons») + `CreateIconIndirect` (`fIcon: FALSE` — курсор, не иконка).
/// AND-маска — сплошной 0 (монохромный битмап без единого установленного
/// бита): вся видимость несёт альфа-канал `hbmColor`, тот же стандартный
/// приём, что у альфа-курсоров/иконок в целом. Хотспот — центр (16,16):
/// дуга рисуется вокруг него радиусом [`ROTATE_ARC_RADIUS_PX`], хотспот
/// остаётся в «дыре» дуги, как и должно быть у курсора. `None` при сбое
/// любого шага GDI (крайне маловероятно) — вызывающий код откатывается на
/// системный курсор.
fn create_rotate_cursor(base_angle_deg: f64) -> Option<HCURSOR> {
    let n = ROTATE_CURSOR_SIZE;
    let rgba = rotate_cursor_rgba(base_angle_deg);

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: n,
            biHeight: -n, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: bmi описывает 32bpp top-down DIB n×n; bits_ptr — валидный
    // out-параметр; hdc=None — GDI использует DC экрана по умолчанию для
    // формата, нам важен только явно заданный BITMAPINFOHEADER.
    let hbm_color =
        unsafe { CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0).ok()? };
    if bits_ptr.is_null() {
        // SAFETY: hbm_color только что создан этим же вызовом.
        unsafe {
            let _ = DeleteObject(hbm_color.into());
        }
        return None;
    }
    // SAFETY: bits_ptr — буфер CreateDIBSection ровно n*n*4 байт (32bpp);
    // premultiply straight-alpha RGBA → BGRA premultiplied на месте записи.
    unsafe {
        let dst = std::slice::from_raw_parts_mut(bits_ptr.cast::<u8>(), rgba.len());
        for (px, out) in rgba.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            let a = px[3] as f32 / 255.0;
            out[0] = (px[2] as f32 * a).round() as u8; // B
            out[1] = (px[1] as f32 * a).round() as u8; // G
            out[2] = (px[0] as f32 * a).round() as u8; // R
            out[3] = px[3]; // A
        }
    }

    // AND-маска: монохромный битмап n×n, все биты 0 (строки выровнены на
    // WORD — при n=32 ровно 4 байта/строку, паддинга не требуется).
    let mask_row_bytes = (n as usize).div_ceil(16) * 2;
    let mask_bits = vec![0u8; mask_row_bytes * n as usize];
    // SAFETY: mask_bits — буфер ровно нужного размера для 1bpp n×n
    // монохромного битмапа с WORD-выровненными строками (контракт CreateBitmap).
    let hbm_mask = unsafe { CreateBitmap(n, n, 1, 1, Some(mask_bits.as_ptr().cast())) };
    if hbm_mask.is_invalid() {
        // SAFETY: hbm_color создан этим же вызовом.
        unsafe {
            let _ = DeleteObject(hbm_color.into());
        }
        return None;
    }

    let info = ICONINFO {
        fIcon: false.into(),
        xHotspot: (n / 2) as u32,
        yHotspot: (n / 2) as u32,
        hbmMask: hbm_mask,
        hbmColor: hbm_color,
    };
    // SAFETY: info валиден; CreateIconIndirect копирует переданные битмапы
    // внутрь себя — оригиналы обязаны быть уничтожены вызывающим кодом
    // (MS Learn CreateIconIndirect), что и делаем ниже независимо от исхода.
    let icon = unsafe { CreateIconIndirect(&info) }.ok();
    unsafe {
        let _ = DeleteObject(hbm_color.into());
        let _ = DeleteObject(hbm_mask.into());
    }
    icon.map(|hicon| HCURSOR(hicon.0))
}

/// Загруженные курсоры. Системные (arrow/size_*) — разделяемые (shared),
/// `DestroyCursor` для них вызывать нельзя. `rotate_cache` — СОБСТВЕННЫЕ
/// ресурсы набора: курсор поворота нужен под ПРОИЗВОЛЬНЫЙ угол (фидбэк
/// пользователя 2026-08-09, третий раунд — угол считается от реального
/// направления угла рамки в пространстве с учётом её поворота, а не
/// фиксирован по 4 константам), поэтому генерируется лениво через
/// [`create_rotate_cursor`]/`CreateIconIndirect` и кэшируется по углу
/// (округлённому до целого градуса в [`CursorShape::Rotate`]) — повторный
/// запрос того же угла не создаёт новый GDI-ресурс. Только записи кэша,
/// которые реально были созданы (не откат на `IDC_CROSS`), уничтожаются в
/// `Drop`.
struct CursorSet {
    arrow: HCURSOR,
    cross: HCURSOR,
    size_all: HCURSOR,
    size_ns: HCURSOR,
    size_we: HCURSOR,
    size_nesw: HCURSOR,
    size_nwse: HCURSOR,
    fallback_rotate: HCURSOR,
    rotate_cache: HashMap<i32, HCURSOR>,
}

impl CursorSet {
    fn load() -> Self {
        let arrow = load_system_cursor(IDC_ARROW);
        // Если какой-то из курсоров не загрузился (практически недостижимо),
        // откатываемся на стрелку; если и она — на null (SetCursor его примет).
        let or_arrow = |idc: PCWSTR| load_system_cursor(idc).or(arrow).unwrap_or_default();
        Self {
            arrow: arrow.unwrap_or_default(),
            cross: or_arrow(IDC_CROSS),
            size_all: or_arrow(IDC_SIZEALL),
            size_ns: or_arrow(IDC_SIZENS),
            size_we: or_arrow(IDC_SIZEWE),
            size_nesw: or_arrow(IDC_SIZENESW),
            size_nwse: or_arrow(IDC_SIZENWSE),
            fallback_rotate: or_arrow(IDC_CROSS),
            rotate_cache: HashMap::new(),
        }
    }

    /// Курсор для угла `angle_deg` (градусы, экранная конвенция) — из кэша,
    /// либо создаётся и кладётся в кэш. `&mut self`, т.к. может populate
    /// кэш; `IDC_CROSS`-заглушка при сбое `CreateIconIndirect`, в кэш не
    /// кладётся (нет собственного ресурса — нечего кэшировать/уничтожать).
    fn rotate_cursor(&mut self, angle_deg: i32) -> HCURSOR {
        if let Some(&cur) = self.rotate_cache.get(&angle_deg) {
            return cur;
        }
        match create_rotate_cursor(angle_deg as f64) {
            Some(cur) => {
                self.rotate_cache.insert(angle_deg, cur);
                cur
            }
            None => self.fallback_rotate,
        }
    }

    fn get(&mut self, shape: CursorShape) -> HCURSOR {
        match shape {
            CursorShape::Arrow => self.arrow,
            CursorShape::Move => self.size_all,
            CursorShape::SizeNS => self.size_ns,
            CursorShape::SizeWE => self.size_we,
            CursorShape::SizeNESW => self.size_nesw,
            CursorShape::SizeNWSE => self.size_nwse,
            CursorShape::Rotate(angle_deg) => self.rotate_cursor(angle_deg),
            CursorShape::Cross => self.cross,
        }
    }
}

impl Drop for CursorSet {
    fn drop(&mut self) {
        // SAFETY: каждый курсор в кэше создан этим же набором через
        // `create_rotate_cursor`/`CreateIconIndirect` — наш собственный
        // ресурс, не разделяемый системный курсор; уничтожается ровно один
        // раз здесь.
        for (_, cursor) in self.rotate_cache.drain() {
            unsafe {
                let _ = DestroyIcon(HICON(cursor.0));
            }
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

    fn apply_current(&mut self) {
        let cursor = self.cursors.get(self.current);
        // SAFETY: hcursor — из CursorSet (системный разделяемый или наш
        // созданный, живёт как минимум пока жив self.cursors); SetCursor не
        // требует принадлежности к конкретному окну.
        unsafe {
            let _ = SetCursor(Some(cursor));
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

    /// Геометрия проверена визуально отдельным прототипом (даёт узнаваемую
    /// маленькую двустороннюю дугу «как в Photoshop») — здесь только
    /// структурные инварианты растра, которые ловят грубую поломку
    /// геометрии/альфы при рефакторинге. Угол произвольный (курсор теперь
    /// генерируется под любой градус, см. `CursorShape::Rotate`) — 45.0
    /// просто для конкретики теста.
    #[test]
    fn rotate_cursor_rgba_has_expected_size() {
        let rgba = rotate_cursor_rgba(45.0);
        assert_eq!(
            rgba.len(),
            (ROTATE_CURSOR_SIZE * ROTATE_CURSOR_SIZE * 4) as usize
        );
    }

    #[test]
    fn rotate_cursor_rgba_has_white_fill_black_outline_and_transparent_background() {
        let rgba = rotate_cursor_rgba(45.0);
        let pixels: Vec<(u8, u8, u8, u8)> = rgba
            .chunks_exact(4)
            .map(|p| (p[0], p[1], p[2], p[3]))
            .collect();
        assert!(
            pixels
                .iter()
                .any(|&(r, g, b, a)| r == 255 && g == 255 && b == 255 && a == 255),
            "должна быть непрозрачная белая заливка фигуры"
        );
        assert!(
            pixels
                .iter()
                .any(|&(r, g, b, a)| r == 0 && g == 0 && b == 0 && a == 255),
            "должна быть непрозрачная чёрная обводка"
        );
        assert!(
            pixels.iter().any(|&(_, _, _, a)| a == 0),
            "фон вне фигуры должен быть полностью прозрачным"
        );
    }

    #[test]
    fn rotate_cursor_rgba_center_is_transparent() {
        // Дуга радиусом ROTATE_ARC_RADIUS_PX от центра — самый центр
        // (хотспот курсора) обязан остаться прозрачным, не задет фигурой.
        let rgba = rotate_cursor_rgba(45.0);
        let n = ROTATE_CURSOR_SIZE as usize;
        let (cx, cy) = (n / 2, n / 2);
        let i = (cy * n + cx) * 4;
        assert_eq!(rgba[i + 3], 0, "центр (хотспот) должен быть прозрачным");
    }

    #[test]
    fn rotate_cursor_rgba_different_angles_give_different_rasters() {
        let a = rotate_cursor_rgba(45.0);
        let b = rotate_cursor_rgba(-45.0);
        let c = rotate_cursor_rgba(135.0);
        assert_ne!(a, b, "разные углы обязаны давать разные растры");
        assert_ne!(a, c, "разные углы обязаны давать разные растры");
        assert_ne!(b, c, "разные углы обязаны давать разные растры");
    }

    #[test]
    fn create_rotate_cursor_succeeds_and_produces_distinct_handle() {
        let a = create_rotate_cursor(45.0).expect("CreateIconIndirect должен создать курсор");
        let b = create_rotate_cursor(-45.0).expect("CreateIconIndirect должен создать курсор");
        assert!(!a.is_invalid(), "хендл курсора не должен быть невалидным");
        assert_ne!(
            a.0, b.0,
            "два независимых вызова обязаны давать разные хендлы GDI-ресурса"
        );
        // SAFETY: оба хендла — наши, только что созданные этим же тестом;
        // не участвуют ни в каком CursorSet, освобождаем вручную.
        unsafe {
            let _ = DestroyIcon(HICON(a.0));
            let _ = DestroyIcon(HICON(b.0));
        }
    }

    #[test]
    fn cursor_set_caches_rotate_cursor_by_angle_bucket() {
        let mut set = CursorSet::load();
        let a1 = set.rotate_cursor(45);
        let a2 = set.rotate_cursor(45);
        let b = set.rotate_cursor(-45);
        assert_eq!(
            a1.0, a2.0,
            "повторный запрос того же угла возвращает тот же хендл из кэша"
        );
        assert_ne!(a1.0, b.0, "разные углы — разные хендлы");
        assert_eq!(
            set.rotate_cache.len(),
            2,
            "в кэше ровно две записи (45 и -45)"
        );
    }

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
            CursorZone::RotateZone(45).cursor_shape(),
            CursorShape::Rotate(45)
        );
        assert_eq!(
            CursorZone::RotateZone(-135).cursor_shape(),
            CursorShape::Rotate(-135)
        );
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

        // Реальная кнопка «нажата» передаётся явно (true) — тест не зависит
        // от настоящего GetAsyncKeyState (см. handle_message_checked).
        let mv = cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), mk_lparam(-5, 7), true);
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
    fn mousemove_while_captured_but_button_physically_up_is_treated_as_release() {
        let wnd = TestWindow::create();
        let mut cap = MouseCapture::new(wnd.0);

        let _ = cap.handle_message(WM_LBUTTONDOWN, WPARAM(0), mk_lparam(10, 10));
        assert!(cap.is_captured());

        // Реальная кнопка уже отпущена (false) — MouseMove не должен
        // продолжать драг: захват снимается, наружу уходит MouseUp, а не
        // MouseMove{dragging: true}. Это и есть защита от «стикер дрейфует
        // сам по себе», найденного в этой сессии — desync между captured и
        // реальным состоянием кнопки.
        let result = cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), mk_lparam(50, 50), false);
        assert_eq!(
            result,
            Some(InputEvent::MouseUp {
                pos: Point { x: 50, y: 50 },
                modifiers: Modifiers::default(),
            })
        );
        assert!(!cap.is_captured());
    }

    #[test]
    fn mousemove_while_captured_and_button_down_continues_dragging() {
        let wnd = TestWindow::create();
        let mut cap = MouseCapture::new(wnd.0);

        let _ = cap.handle_message(WM_LBUTTONDOWN, WPARAM(0), mk_lparam(10, 10));
        let result = cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), mk_lparam(20, 20), true);
        assert_eq!(
            result,
            Some(InputEvent::MouseMove {
                pos: Point { x: 20, y: 20 },
                modifiers: Modifiers::default(),
                dragging: true,
            })
        );
        assert!(cap.is_captured());
    }

    #[test]
    fn mousemove_with_same_position_while_dragging_is_suppressed() {
        // Windows документированно может повторно прислать WM_MOUSEMOVE с
        // неизменившимися координатами без реального движения курсора
        // (MSDN, «Mouse movement — Win32 apps») — источник живого дрейфа,
        // разобранного в этой сессии. Повтор той же позиции не должен
        // порождать новое MouseMove-событие.
        let wnd = TestWindow::create();
        let mut cap = MouseCapture::new(wnd.0);

        let _ = cap.handle_message(WM_LBUTTONDOWN, WPARAM(0), mk_lparam(10, 10));
        let first = cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), mk_lparam(20, 20), true);
        assert!(
            first.is_some(),
            "первое движение на новую позицию — не дубликат"
        );

        let duplicate =
            cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), mk_lparam(20, 20), true);
        assert_eq!(duplicate, None, "повтор той же позиции подавляется");
        assert!(cap.is_captured(), "захват не снимается дубликатом");

        // Реальное движение на НОВУЮ позицию после дубликата снова проходит.
        let moved = cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), mk_lparam(30, 30), true);
        assert_eq!(
            moved,
            Some(InputEvent::MouseMove {
                pos: Point { x: 30, y: 30 },
                modifiers: Modifiers::default(),
                dragging: true,
            })
        );
    }

    #[test]
    fn force_release_clears_captured_state_and_os_capture() {
        let wnd = TestWindow::create();
        let mut cap = MouseCapture::new(wnd.0);

        // Захват реально держится (аналог хоткея выхода посреди драга —
        // WM_LBUTTONUP ещё не приходил).
        let _ = cap.handle_message(WM_LBUTTONDOWN, WPARAM(0), mk_lparam(1, 1));
        assert!(cap.is_captured());
        // SAFETY: простое чтение захвата текущего потока.
        assert_eq!(unsafe { GetCapture() }, wnd.0);

        cap.force_release();
        assert!(!cap.is_captured());
        // SAFETY: простое чтение захвата текущего потока.
        assert!(unsafe { GetCapture() }.0.is_null());
    }

    #[test]
    fn force_release_without_prior_capture_is_a_harmless_no_op() {
        let wnd = TestWindow::create();
        let mut cap = MouseCapture::new(wnd.0);
        assert!(!cap.is_captured());
        cap.force_release();
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
        assert!(cm.set_shape(CursorShape::Rotate(-135)));
        assert_eq!(cm.current(), CursorShape::Rotate(-135));
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
