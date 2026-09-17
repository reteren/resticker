//! Маленькие окна для живого куска чужого окна.
//!
//! Проверенный скелет взят из `scratchpad/probe_v1`: DWM-превью живёт поверх
//! собственного popup-окна, а полоса вынесена во второе окно, потому что
//! превью закрывает всё содержимое первого. Фаза 2 отчёта показала, что
//! `HTCAPTION` не двигает окно с `NOACTIVATE`, поэтому drag реализован явно и
//! завершает захват ровно одним событием `Moved`.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rst_core::model::Rect;
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
    DWM_TNP_VISIBLE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmRegisterThumbnail,
    DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, LineTo,
    MoveToEx, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect, HTCLIENT, IDC_ARROW, KillTimer,
    LoadCursorW, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetCursor, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_APP,
    WM_CANCELMODE, WM_CAPTURECHANGED, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT,
    WM_SETCURSOR, WM_TIMER, WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongW, HTTRANSPARENT, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, WINDOWPOS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    MINMAXINFO, WM_GETMINMAXINFO, WM_NCCALCSIZE, WM_SIZING, WMSZ_BOTTOM, WMSZ_BOTTOMLEFT,
    WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT,
};
use windows::Win32::UI::WindowsAndMessaging::{WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE};
use windows::core::{BOOL, PCWSTR, w};

const CLASS_NAME: PCWSTR = w!("resticker_crop_window");
/// Класс обоих окон куска — тем же текстом, что [`CLASS_NAME`], но строкой
/// Rust.
///
/// Нужен координатору, чтобы опознать СВОЁ окно куска в перечислении окон.
/// Окна собственного процесса он в остальном отбрасывает (иначе в кандидаты
/// группы попали бы оверлеи и панели), а кусок обязан быть доступен наравне с
/// чужими окнами: он и есть обычное окно (разбор P2, 2026-09-13).
pub const WINDOW_CLASS: &str = "resticker_crop_window";
const WINDOW_TITLE: PCWSTR = w!("resticker_crop_window");
const WM_APP_COMMAND: u32 = WM_APP + 1;
/// Снять окна куска по решению координатора — БЕЗ события «кусок закрыт».
///
/// Отдельно от `WM_CLOSE` намеренно. `WM_CLOSE` приходит и от системы:
/// Alt+F4, закрытие кнопкой на панели задач, закрытие из Alt+Tab — окно куска
/// теперь обычное окно приложения, и все эти пути ему доступны. Пока оба
/// случая обрабатывались одинаково, системное закрытие уничтожало окна, но
/// НЕ удаляло стикер: кусок выглядел удалённым, оставаясь в конфиге, и
/// возвращался, как только окна пересоздавались — например, после выхода из
/// режима выделения нового куска (живой репорт пользователя 2026-09-12:
/// «закрытое ранее окно снова появится», «работает через раз»).
const WM_APP_SHUTDOWN: u32 = WM_APP + 2;
const STRIP_HEIGHT_DIP: u32 = 28;
/// Сколько полоса держится после ухода курсора, мс.
///
/// Без задержки полоса гасла от любого мига, когда курсор оказывался вне
/// обоих окон, — например, на границе между ними или при быстром движении к
/// кнопке (репорт пользователя 2026-09-11: «пропадает когда не надо»).
/// Задержка живёт только пока полоса показана и курсор ушёл: в покое таймера
/// нет, и обещание SPEC §13 не задето.
const STRIP_HIDE_DELAY_MS: u32 = 320;
/// Запас вокруг куска, внутри которого полоса ещё считается «под курсором».
/// Курсор, проскочивший на пиксель мимо кромки, не должен гасить её.
const STRIP_KEEP_MARGIN_PX: i32 = 12;
/// Идентификатор таймера отложенного скрытия.
const STRIP_HIDE_TIMER: usize = 1;
/// Ширина полосы у края окна, за которую его тянут, DIP.
const RESIZE_EDGE_DIP: i32 = 6;
/// Наименьшая сторона окна куска, DIP: меньше — и в нём нечего разглядывать,
/// а полоса с двумя кнопками перестаёт помещаться.
const MIN_SIDE_DIP: i32 = 48;
const DEFAULT_DPI: u32 = 96;
// COLORREF хранит цвет как 0x00BBGGRR.
/// Тело полосы — `BUTTON_BG` (#1B1B20) из палитры Dark Liquid Glass, а не
/// `GLASS_INK` (#07070A): полоса стоит вплотную над тёмным содержимым, и
/// почти чёрная на почти чёрном читалась бы как дыра, а не как заголовок.
const COLOR_STRIP: COLORREF = COLORREF(0x00201b1b);
/// Подпись — белый с приглушением, как `TEXT` в теме.
const COLOR_TEXT: COLORREF = COLORREF(0x00ebebeb);
/// Глифы кнопок.
const COLOR_GLYPH: COLORREF = COLORREF(0x00d8d8d8);
/// Подсветка «свернуть» под курсором — `BUTTON_BG_HOVER` (#2F2F36).
const COLOR_HOVER: COLORREF = COLORREF(0x00362f2f);
/// Подсветка «закрыть» под курсором — красный закрытия Windows (#C42B1C):
/// человек узнаёт его без подписи, и деструктивная кнопка обязана
/// отличаться от соседней.
const COLOR_CLOSE_HOVER: COLORREF = COLORREF(0x001c2bc4);
/// Кегль подписи, DIP.
const TEXT_SIZE_DIP: i32 = 12;
/// Сторона глифа кнопки, DIP.
const GLYPH_DIP: i32 = 10;
/// Отступ подписи от левого края, DIP.
const TEXT_PAD_DIP: i32 = 10;

/// Прямоугольник куска в пикселях окна-источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Настройки двух окон куска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CropWindowOptions {
    /// Начальная позиция и размер окна содержимого в физических пикселях.
    pub bounds: Rect,
    /// Верхняя граница монитора в виртуальных координатах.
    /// Нужна, чтобы полоса у верхнего края легла поверх, а не за экран.
    pub screen_top: i32,
    /// DPI монитора; 96 означает 100% масштаб.
    pub dpi: u32,
    /// Область окна-источника, которую DWM должен показывать.
    pub source_rect: SourceRect,
    /// Имя приложения в левой части полосы.
    pub app_name: String,
    /// Заголовок окна содержимого — то, что показывают Alt+Tab и панель
    /// задач (запрос пользователя 2026-09-11: кусок в Alt+Tab, открывается
    /// как окно). Отдельно от `app_name`: подпись в полосе — просто имя
    /// приложения, а в Alt+Tab кусок обязан отличаться от самого приложения,
    /// иначе там будут два одинаковых «Калькулятора».
    pub window_title: String,
    /// Непрозрачность DWM-превью, 0..=255.
    pub opacity: u8,
    /// Держать кусок поверх всех окон.
    ///
    /// `false` — обычное окно, уходящее под другие (репорт пользователя
    /// 2026-09-12). Включается булавкой на полосе и хранится в конфиге, так
    /// что окно пересоздаётся уже в нужном состоянии, без мигания.
    pub always_on_top: bool,
}

impl CropWindowOptions {
    /// Собрать настройки с физическими пикселями и стандартным DPI.
    pub fn new(bounds: Rect, source_rect: SourceRect, app_name: impl Into<String>) -> Self {
        Self {
            bounds,
            screen_top: bounds.y,
            dpi: DEFAULT_DPI,
            source_rect,
            window_title: String::new(),
            app_name: app_name.into(),
            opacity: u8::MAX,
            always_on_top: false,
        }
    }
}

/// События, которые координатор получает от окна куска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CropWindowEvent {
    /// Геометрия окна куска изменилась: его перетащили за полосу ИЛИ
    /// подвинул кто-то снаружи — раскладка группы, закрепление окна, привязка
    /// Windows (Win+стрелки), пользователь мышью за край.
    ///
    /// Окно куска — настоящее окно (`WS_EX_APPWINDOW`), и его двигают те же
    /// механизмы, что и любое другое. Без этого события координатор возвращал
    /// бы его на место из конфига, и окно дёргалось бы под раскладкой группы.
    ///
    /// За перетаскивание отправляется РОВНО ОДИН раз — в конце.
    Geometry { x: i32, y: i32, w: u32, h: u32 },
    /// Нажата кнопка закрытия в полосе.
    CloseClicked,
    /// Нажата кнопка сворачивания в полосе.
    MinimizeClicked,
    /// Нажат квадрат свёрнутого куска.
    RestoreClicked,
    /// Нажата булавка в полосе: человек включил или выключил «поверх всех
    /// окон». Окно применяет новое состояние сразу само — событие нужно
    /// координатору, чтобы записать решение в конфиг и пережить перезапуск.
    AlwaysOnTopToggled(bool),
    /// Источник исчез или DWM больше не принимает обновления превью.
    SourceGone,
}

/// Ошибки запуска потока собственных окон.
#[derive(Debug, thiserror::Error)]
pub enum CropWindowError {
    #[error("could not create the crop window")]
    CreateFailed,
    #[error("the crop-window thread exited before initialization")]
    ThreadCrashed,
    #[error("Win32: {0}")]
    Win32(#[from] windows::core::Error),
}

/// Окно содержимого и отдельная полоса над ним.
///
/// Оба окна принадлежат одному GUI-потоку; наружный поток общается с ним
/// только сообщениями. Это важно: уничтожение окон и снятие capture остаются
/// последовательными и не требуют прямого вызова thread-affine API.
pub struct CropWindow {
    hwnd: HWND,
    strip_hwnd: HWND,
    thread: Option<JoinHandle<()>>,
}

// HWND содержит сырой указатель, но здесь он лишь числовой идентификатор.
// Владение обоими окнами остаётся у GUI-потока и Drop дожидается его.
unsafe impl Send for CropWindow {}
unsafe impl Sync for CropWindow {}

struct ReadyWindows {
    content: isize,
    strip: isize,
}

impl CropWindow {
    /// Создать содержимое с DWM-превью и скрытую до наведения полосу.
    ///
    /// `source` не перемещается в поток как Rust-объект: передаётся только
    /// числовой HWND, а все вызовы, использующие его, выполняются внутри
    /// потока окон.
    pub fn create(
        source: HWND,
        options: CropWindowOptions,
    ) -> Result<(Self, Receiver<CropWindowEvent>), CropWindowError> {
        let source_raw = source.0 as isize;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread =
            thread::spawn(move || run_message_loop(ready_tx, event_tx, source_raw, options));
        let ready = ready_rx
            .recv()
            .map_err(|_| CropWindowError::ThreadCrashed)??;
        Ok((
            Self {
                hwnd: hwnd_from_raw(ready.content),
                strip_hwnd: hwnd_from_raw(ready.strip),
                thread: Some(thread),
            },
            event_rx,
        ))
    }

    /// Сырой HWND окна содержимого для диагностики/интеграции рендера.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Сырой HWND полосы; обычно координатору нужен только `hwnd`.
    pub fn strip_hwnd(&self) -> HWND {
        self.strip_hwnd
    }

    /// Поменять opacity именно DWM-превью, не альфу окна.
    pub fn set_opacity(&self, opacity: u8) -> Result<(), CropWindowError> {
        self.post_command(Command::SetOpacity(opacity))
    }

    /// Переместить содержимое и полосу в новую позицию.
    pub fn set_bounds(&self, bounds: Rect) -> Result<(), CropWindowError> {
        self.post_command(Command::SetBounds(bounds))
    }

    /// Держать кусок поверх всех окон (или перестать).
    ///
    /// Нужно координатору для двух случаев: восстановить состояние из конфига
    /// у уже созданного окна и отменить переключение, если записать решение в
    /// конфиг не удалось.
    pub fn set_always_on_top(&self, on: bool) -> Result<(), CropWindowError> {
        self.post_command(Command::SetAlwaysOnTop(on))
    }

    /// Ужать содержимое до квадрата 32×32 DIP в точке координатора.
    pub fn minimize(&self, point: Point) -> Result<(), CropWindowError> {
        self.post_command(Command::Minimize(point))
    }

    /// Вернуть содержимое к заданным границам; полоса снова появляется только
    /// после наведения, чтобы восстановление не перехватило лишний клик.
    pub fn restore(&self, bounds: Rect) -> Result<(), CropWindowError> {
        self.post_command(Command::Restore(bounds))
    }

    fn post_command(&self, command: Command) -> Result<(), CropWindowError> {
        let boxed = Box::new(command);
        let ptr = Box::into_raw(boxed);
        // SAFETY: hwnd принадлежит живому потоку до Drop; Box освобождается
        // обработчиком, а при отказе PostMessage — здесь же.
        match unsafe {
            PostMessageW(
                Some(self.hwnd),
                WM_APP_COMMAND,
                WPARAM(0),
                LPARAM(ptr as isize),
            )
        } {
            Ok(()) => Ok(()),
            Err(error) => {
                // SAFETY: сообщение не принято, владение Box осталось у нас.
                unsafe {
                    drop(Box::from_raw(ptr));
                }
                Err(error.into())
            }
        }
    }
}

impl Drop for CropWindow {
    fn drop(&mut self) {
        // `WM_APP_SHUTDOWN`, а НЕ `WM_CLOSE`: закрытие по решению координатора
        // не должно выглядеть как «человек закрыл кусок» и удалять стикер —
        // окна снимаются и при входе в режим выделения, и в режиме
        // редактирования, где кусок рисует оверлей.
        if !self.hwnd.0.is_null() {
            // SAFETY: hwnd — наше окно; сообщение безопасно с любого потока.
            let _ = unsafe { PostMessageW(Some(self.hwnd), WM_APP_SHUTDOWN, WPARAM(0), LPARAM(0)) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Точка в физических пикселях виртуального рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// Зона полосы, в которую попало нажатие.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripHit {
    None,
    Drag,
    /// Булавка «поверх всех окон» — крайняя левая из трёх кнопок.
    Pin,
    Minimize,
    Close,
}

/// Ширина одной кнопки полосы. Кнопки квадратные (сторона равна высоте
/// полосы, 28 DIP), но у диагностически узкой полосы им отдаётся не больше
/// четверти ширины на каждую: три кнопки и зона перетаскивания обязаны
/// поместиться, иначе полосу стало бы не за что взять.
fn strip_button_side(width: u32, height: u32) -> u32 {
    height.min(width / 4)
}

/// Проверить полосу без окна: тесты геометрии не трогают рабочий стол.
pub fn hit_test_strip(width: u32, height: u32, x: i32, y: i32) -> StripHit {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 || height == 0 {
        return StripHit::None;
    }
    let button = strip_button_side(width, height);
    if button == 0 {
        return StripHit::Drag;
    }
    let close_left = width.saturating_sub(button) as i32;
    let minimize_left = width.saturating_sub(button.saturating_mul(2)) as i32;
    // Булавка — СЛЕВА, там же, где её значок у закреплённых окон (запрос
    // пользователя 2026-09-14: «верни старую визуализацию булавки слева
    // сверху как и на всех окнах»). Справа остаются только «свернуть» и
    // «закрыть» — тот порядок, который Windows приучила читать справа.
    if x >= close_left {
        StripHit::Close
    } else if x >= minimize_left {
        StripHit::Minimize
    } else if x < button as i32 {
        StripHit::Pin
    } else {
        StripHit::Drag
    }
}

/// Куда попал курсор у края окна: код зоны изменения размера для
/// `WM_NCHITTEST`, либо `HTCLIENT` внутри.
///
/// Чистая функция: проверяется тестами без окна. `x`/`y` — ЭКРАННЫЕ
/// координаты, как их присылает `WM_NCHITTEST`.
pub fn resize_hit(rect: Rect, dpi: u32, x: i32, y: i32) -> u32 {
    let edge = dip_to_px(RESIZE_EDGE_DIP as u32, dpi.max(1)) as i32;
    let (l, t) = (rect.x, rect.y);
    let (r, b) = (rect.x + rect.w as i32, rect.y + rect.h as i32);
    if x < l || y < t || x >= r || y >= b {
        return HTCLIENT;
    }
    let left = x < l + edge;
    let right = x >= r - edge;
    let top = y < t + edge;
    let bottom = y >= b - edge;
    match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, true, ..) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ => HTCLIENT,
    }
}

/// Подправить прямоугольник изменения размера так, чтобы сохранилась
/// пропорция `aspect` (ширина/высота). `edge` — код стороны из `WM_SIZING`.
///
/// Правим ту сторону, за которую НЕ тянут: если человек ведёт правый край,
/// подстраивается высота, и наоборот. За угол — подстраивается высота, чтобы
/// движение по горизонтали оставалось главным.
pub fn keep_aspect(rect: &mut RECT, edge: u32, aspect: Option<f64>) {
    let Some(aspect) = aspect else {
        return;
    };
    if aspect <= 0.0 {
        return;
    }
    let w = (rect.right - rect.left).max(1);
    let h = (rect.bottom - rect.top).max(1);
    match edge {
        WMSZ_LEFT | WMSZ_RIGHT => {
            let want = (f64::from(w) / aspect).round() as i32;
            rect.bottom = rect.top + want.max(1);
        }
        WMSZ_TOP | WMSZ_BOTTOM => {
            let want = (f64::from(h) * aspect).round() as i32;
            rect.right = rect.left + want.max(1);
        }
        WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT => {
            let want = (f64::from(w) / aspect).round() as i32;
            if edge == WMSZ_TOPLEFT {
                rect.top = rect.bottom - want.max(1);
            } else {
                rect.bottom = rect.top + want.max(1);
            }
        }
        WMSZ_TOPRIGHT | WMSZ_BOTTOMRIGHT => {
            let want = (f64::from(w) / aspect).round() as i32;
            if edge == WMSZ_TOPRIGHT {
                rect.top = rect.bottom - want.max(1);
            } else {
                rect.bottom = rect.top + want.max(1);
            }
        }
        _ => {}
    }
}

/// Геометрия полосы: ПОВЕРХ верхней кромки содержимого.
///
/// Раньше полоса стояла НАД содержимым, вплотную. Это дало два живых бага
/// (репорт пользователя 2026-09-11):
///
/// 1. Между двумя отдельными окнами со скруглёнными углами остаётся зазор.
///    Ведя курсор с содержимого на полосу, человек проходил место, где нет
///    ни одного из двух окон, — оба наведения сбрасывались, и полоса гасла
///    ровно в тот момент, когда до неё тянулись.
/// 2. У верхнего края экрана полоса упиралась в край и ПЕРЕСТАВАЛА следовать
///    за содержимым, которое продолжало уезжать вверх, — «бар отлетает».
///
/// Наложение убирает оба: зазора нет по построению, а у края экрана полосе
/// некуда отставать. Цена — верхние 28 DIP содержимого перекрыты, но только
/// пока курсор на куске.
pub fn strip_rect(content: Rect, screen_top: i32, dpi: u32) -> Rect {
    let height = dip_to_px(STRIP_HEIGHT_DIP, dpi.max(1));
    // Не выше края экрана: у куска, уехавшего верхом за экран, полоса иначе
    // ушла бы туда же и стала недоступной.
    let y = content.y.max(screen_top);
    Rect {
        x: content.x,
        y,
        w: content.w,
        h: height,
    }
}

fn dip_to_px(dip: u32, dpi: u32) -> u32 {
    ((u64::from(dip) * u64::from(dpi) + 48) / 96).max(1) as u32
}

/// Чистое состояние собственного перетаскивания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragTracker {
    active: bool,
    offset: Point,
    current: Point,
}

/// Результат шага drag-state; `Ended` выдаётся только один раз.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragUpdate {
    Moved(Point),
    Ended(Point),
}

impl Default for DragTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DragTracker {
    /// Пустой tracker без активного capture.
    pub const fn new() -> Self {
        Self {
            active: false,
            offset: Point { x: 0, y: 0 },
            current: Point { x: 0, y: 0 },
        }
    }

    /// Запомнить смещение указателя от левого верхнего угла содержимого.
    pub fn begin(&mut self, cursor: Point, content: Point) {
        self.active = true;
        self.offset = Point {
            x: cursor.x.saturating_sub(content.x),
            y: cursor.y.saturating_sub(content.y),
        };
        self.current = content;
    }

    /// Обработать движение; физически отпущенная кнопка завершает drag до
    /// любого перемещения и тем самым не оставляет захват висеть.
    pub fn move_cursor(&mut self, cursor: Point, left_down: bool) -> Option<DragUpdate> {
        if !self.active {
            return None;
        }
        if !left_down {
            return self.end();
        }
        self.current = Point {
            x: cursor.x.saturating_sub(self.offset.x),
            y: cursor.y.saturating_sub(self.offset.y),
        };
        Some(DragUpdate::Moved(self.current))
    }

    /// Завершить drag по WM_LBUTTONUP, WM_CAPTURECHANGED или WM_CANCELMODE.
    /// Повторный вызов ничего не выдаёт — это инвариант «ровно один Moved».
    pub fn end(&mut self) -> Option<DragUpdate> {
        if !self.active {
            return None;
        }
        self.active = false;
        Some(DragUpdate::Ended(self.current))
    }

    /// Идёт ли сейчас drag.
    pub const fn is_active(&self) -> bool {
        self.active
    }
}

#[derive(Debug)]
enum Command {
    SetOpacity(u8),
    SetBounds(Rect),
    Minimize(Point),
    Restore(Rect),
    SetAlwaysOnTop(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowRole {
    Content,
    Strip,
}

struct WindowState {
    content: HWND,
    strip: HWND,
    source: HWND,
    thumbnail: Option<isize>,
    content_rect: Rect,
    normal_rect: Rect,
    screen_top: i32,
    dpi: u32,
    source_rect: SourceRect,
    opacity: u8,
    app_name: Vec<u16>,
    minimized: bool,
    /// Кусок держится поверх всех окон (булавка в полосе нажата).
    ///
    /// Хранится в состоянии, а не вычитывается из стиля окна каждый раз:
    /// по нему рисуется вид булавки, и он же решает, в какой полосе z-порядка
    /// утверждать полосу над содержимым.
    always_on_top: bool,
    hover_content: bool,
    hover_strip: bool,
    /// Кнопка полосы под курсором — для подсветки. `StripHit::None`/`Drag` —
    /// ни одна.
    hover_button: StripHit,
    /// Окно сейчас двигаем МЫ САМИ (`apply_bounds`, перетаскивание,
    /// сворачивание). Пока флаг поднят, `WM_WINDOWPOSCHANGED` не считается
    /// внешним изменением — иначе координатор получал бы эхо собственной
    /// команды и они гоняли бы окно по кругу.
    self_move: bool,
    /// Windows сейчас ведёт свой цикл перемещения или изменения размера
    /// (между `WM_ENTERSIZEMOVE` и `WM_EXITSIZEMOVE`).
    ///
    /// Пока он идёт, о геометрии наружу НЕ сообщаем: `WM_WINDOWPOSCHANGED`
    /// прилетает на каждый шаг мыши, а координатор на каждое такое событие
    /// пишет `config.json` — за одно перетаскивание вышли бы сотни записей на
    /// диск. Сообщаем один раз, когда цикл закончился.
    in_size_move: bool,
    drag: DragTracker,
    source_gone_sent: bool,
    tx: Sender<CropWindowEvent>,
}

impl WindowState {
    fn role(&self, hwnd: HWND) -> WindowRole {
        if hwnd == self.strip {
            WindowRole::Strip
        } else {
            WindowRole::Content
        }
    }

    fn emit(&self, event: CropWindowEvent) {
        let _ = self.tx.send(event);
    }

    fn mark_source_gone(&mut self) {
        if !self.source_gone_sent {
            self.source_gone_sent = true;
            self.emit(CropWindowEvent::SourceGone);
        }
    }

    fn sync_thumbnail(&mut self) {
        let Some(thumbnail) = self.thumbnail else {
            return;
        };
        let destination = RECT {
            left: 0,
            top: 0,
            right: self.content_rect.w.min(i32::MAX as u32) as i32,
            bottom: self.content_rect.h.min(i32::MAX as u32) as i32,
        };
        let source = RECT {
            left: self.source_rect.x,
            top: self.source_rect.y,
            right: self
                .source_rect
                .x
                .saturating_add(self.source_rect.w.min(i32::MAX as u32) as i32),
            bottom: self
                .source_rect
                .y
                .saturating_add(self.source_rect.h.min(i32::MAX as u32) as i32),
        };
        let properties = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_RECTSOURCE
                | DWM_TNP_OPACITY
                | DWM_TNP_VISIBLE,
            rcDestination: destination,
            rcSource: source,
            opacity: self.opacity,
            fVisible: BOOL(1),
            fSourceClientAreaOnly: BOOL(0),
        };
        // SAFETY: thumbnail was registered for this content window and the
        // packed properties match the DWM ABI.
        if unsafe { DwmUpdateThumbnailProperties(thumbnail, &properties) }.is_err() {
            self.mark_source_gone();
        }
    }

    fn register_thumbnail(&mut self) {
        // SAFETY: both HWNDs are live windows on this GUI thread; DWM keeps
        // only the registration and does not take ownership of either HWND.
        match unsafe { DwmRegisterThumbnail(self.content, self.source) } {
            Ok(thumbnail) => {
                self.thumbnail = Some(thumbnail);
                self.sync_thumbnail();
            }
            Err(_) => self.mark_source_gone(),
        }
    }

    fn update_strip_visibility(&self) {
        // Замер, а не отладочный мусор: полоса куска пропадала уже трижды и
        // каждый раз по новой причине. Одна строка на КАЖДОЕ решение показать
        // или скрыть её — и журнал сразу говорит, что именно решило, вместо
        // очередного круга догадок. Уровень `debug`: в обычной работе он
        // выключен и ничего не стоит.
        tracing::debug!(
            minimized = self.minimized,
            dragging = self.drag.is_active(),
            hover_content = self.hover_content,
            hover_strip = self.hover_strip,
            topmost = self.content_is_topmost(),
            pin = self.always_on_top,
            "решение о показе полосы куска"
        );
        if self.minimized {
            // Свёрнутый квадрат не имеет полосы: иначе она перекрыла бы
            // единственную область, по которой его можно восстановить.
            unsafe {
                let _ = ShowWindow(self.strip, SW_HIDE);
            }
        } else if self.drag.is_active() || self.hover_content || self.hover_strip {
            unsafe {
                let _ = ShowWindow(self.strip, SW_SHOWNOACTIVATE);
            }
            // Порядок окон утверждается при КАЖДОМ показе: пока полоса была
            // скрыта, содержимое могло всплыть над ней.
            self.raise_strip();
        } else {
            unsafe {
                let _ = ShowWindow(self.strip, SW_HIDE);
            }
        }
    }

    /// Пропорция показываемого куска источника — по ней окно сохраняет форму
    /// при изменении размера. `None` — вырожденный кусок.
    fn aspect(&self) -> Option<f64> {
        let (w, h) = (self.source_rect.w, self.source_rect.h);
        if w == 0 || h == 0 {
            return None;
        }
        Some(f64::from(w) / f64::from(h))
    }

    /// Обновить верх экрана и масштаб по монитору, на котором окно СЕЙЧАС.
    ///
    /// Живой баг 2026-09-11: оба значения запоминались один раз, при создании
    /// окна. У пользователя второй монитор начинается не с нуля, а с 357, и
    /// кусок, созданный на нём, помнил 357 навсегда. Стоило перетащить его на
    /// основной монитор (верх = 0), как полоса упиралась в запомненные 357 и
    /// оказывалась на 257 точек НИЖЕ куска — «бар отлетает».
    ///
    /// Масштаб по той же причине: на мониторах с разным DPI полоса иначе
    /// осталась бы высотой от прежнего монитора.
    fn refresh_monitor_metrics(&mut self) {
        // SAFETY: своё живое окно; `MONITOR_DEFAULTTONEAREST` не даёт null.
        unsafe {
            let monitor = MonitorFromWindow(self.content, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                self.screen_top = info.rcMonitor.top;
            }
            let dpi = GetDpiForWindow(self.content);
            if dpi > 0 {
                self.dpi = dpi;
            }
        }
    }

    /// Поднять полосу над содержимым.
    ///
    /// Живой баг 2026-09-12: при изменении размера окно содержимого
    /// АКТИВИРУЕТСЯ (запрет активации снят ради Alt+Tab), Windows поднимает
    /// его на верх topmost-полосы — и оно накрывает собой полосу, которая
    /// лежит на его же верхней кромке. Полоса не пропадала, она уходила ПОД
    /// содержимое и больше никогда не всплывала: все наши `SetWindowPos`
    /// стояли с `SWP_NOZORDER`, то есть порядок окон не задавал никто.
    ///
    /// Поэтому порядок утверждается явно всякий раз, когда полосу показывают
    /// или двигают — и обязательно в ТОЙ ЖЕ полосе z-порядка, где сейчас
    /// содержимое.
    ///
    /// Полоса z-порядка берётся у самого окна содержимого, а не из нашего
    /// флага булавки. Содержимое могут поднять в topmost снаружи — например,
    /// человек закрепил кусок как обычное окно (`crate::window_pin`), и это
    /// теперь разрешено. Пока полоса оставалась в обычной полосе, topmost-кусок
    /// накрывал её собой, и она исчезала навсегда: «когда я закрепляю окно у
    /// меня пропадает верхний контрол бар и я не могу двигать окно… при этом
    /// всём я могу скейлить окно» (репорт пользователя 2026-09-13). Размер
    /// менялся потому, что рамка живёт у содержимого, а перетаскивание — только
    /// за полосу, которой не стало.
    ///
    /// `HWND_NOTOPMOST` для обычного куска — не «опустить», а «встать наверх
    /// обычной полосы»: это ставит полосу выше содержимого, не поднимая её над
    /// чужими окнами, под которыми сам кусок уже лежит.
    fn raise_strip(&self) {
        let insert = if self.content_is_topmost() {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = SetWindowPos(
                self.strip,
                Some(insert),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }

    /// Окно содержимого сейчас в topmost-полосе.
    ///
    /// Читается у самого окна, а не из поля: topmost могли изменить снаружи,
    /// и наш флаг булавки об этом не знает.
    fn content_is_topmost(&self) -> bool {
        // SAFETY: своё живое окно; `GetWindowLongW` не падает и на мёртвом.
        let ex = unsafe { GetWindowLongW(self.content, GWL_EXSTYLE) } as u32;
        ex & WS_EX_TOPMOST.0 != 0
    }

    /// Включить или выключить «поверх всех окон» у обоих окон куска.
    ///
    /// Стиль `WS_EX_TOPMOST` не пишется напрямую: Windows признаёт его только
    /// через `SetWindowPos` с `HWND_TOPMOST`/`HWND_NOTOPMOST` — прямая запись
    /// в стиль оставила бы флаг в `GetWindowLongW` и не изменила бы порядок.
    ///
    /// Сначала содержимое, потом полоса: полосу ставит `raise_strip`, который
    /// смотрит на фактическую полосу z-порядка содержимого. Обратный порядок
    /// на миг оставил бы служебную полосу в topmost над обычным содержимым, и
    /// она мигнула бы поверх чужого окна.
    fn set_always_on_top(&mut self, on: bool) {
        if self.always_on_top == on {
            return;
        }
        self.always_on_top = on;
        let insert = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
        // SAFETY: свои окна этого потока.
        unsafe {
            let _ = SetWindowPos(
                self.content,
                Some(insert),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
        self.raise_strip();
        // Булавка нарисована в полосе — её вид обязан обновиться сразу.
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = InvalidateRect(Some(self.strip), None, false);
        }
    }

    /// Поставить полосу по текущему прямоугольнику содержимого.
    fn reposition_strip(&mut self) {
        self.refresh_monitor_metrics();
        let strip = strip_rect(self.content_rect, self.screen_top, self.dpi);
        self.self_move = true;
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = SetWindowPos(
                self.strip,
                None,
                strip.x,
                strip.y,
                strip.w as i32,
                strip.h as i32,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        self.raise_strip();
        self.self_move = false;
    }

    /// Завести отложенное скрытие полосы.
    fn arm_hide_timer(&self) {
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = SetTimer(
                Some(self.content),
                STRIP_HIDE_TIMER,
                STRIP_HIDE_DELAY_MS,
                None,
            );
        }
    }

    fn kill_hide_timer(&self) {
        // SAFETY: своё окно этого потока; снятие несуществующего таймера
        // безвредно.
        unsafe {
            let _ = KillTimer(Some(self.content), STRIP_HIDE_TIMER);
        }
    }

    /// Курсор у куска: внутри содержимого, полосы или в запасе вокруг них.
    ///
    /// Спрашиваем позицию курсора, а не полагаемся на `WM_MOUSELEAVE`: между
    /// двумя окнами есть места, где не срабатывает ни вход, ни выход, и
    /// только прямая проверка отвечает на вопрос «человек ещё здесь?».
    fn cursor_near_piece(&self) -> bool {
        let Some(c) = cursor_position() else {
            return false;
        };
        let strip = strip_rect(self.content_rect, self.screen_top, self.dpi);
        let inside = |r: Rect| {
            let m = STRIP_KEEP_MARGIN_PX;
            c.x >= r.x - m
                && c.y >= r.y - m
                && c.x < r.x + r.w as i32 + m
                && c.y < r.y + r.h as i32 + m
        };
        inside(self.content_rect) || inside(strip)
    }

    fn move_windows(&mut self, x: i32, y: i32) {
        self.self_move = true;
        self.content_rect.x = x;
        self.content_rect.y = y;
        // SAFETY: both handles belong to this thread; flags preserve size,
        // z-order and activation while dragging.
        unsafe {
            let _ = SetWindowPos(
                self.content,
                None,
                self.content_rect.x,
                self.content_rect.y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
        }
        // Метрики монитора — ПОСЛЕ перемещения содержимого: перетаскивание
        // переносит кусок между экранами, и до `SetWindowPos` система ещё
        // считает окно на прежнем.
        self.refresh_monitor_metrics();
        let strip = strip_rect(self.content_rect, self.screen_top, self.dpi);
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = SetWindowPos(
                self.strip,
                None,
                strip.x,
                strip.y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
        }
        self.self_move = false;
    }

    fn apply_bounds(&mut self, bounds: Rect) {
        self.content_rect = bounds;
        let (cw, ch) = dimensions(bounds);
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = SetWindowPos(
                self.content,
                None,
                bounds.x,
                bounds.y,
                cw,
                ch,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        // Метрики монитора — ПОСЛЕ перемещения содержимого: новое место может
        // быть на другом экране, а до `SetWindowPos` система считает окно на
        // прежнем (живой баг 2026-09-11: полоса уезжала вниз на 257 точек,
        // помня верх второго монитора).
        self.refresh_monitor_metrics();
        let strip = strip_rect(self.content_rect, self.screen_top, self.dpi);
        let (sw, sh) = dimensions(strip);
        // SAFETY: своё окно этого потока.
        unsafe {
            let _ = SetWindowPos(
                self.strip,
                None,
                strip.x,
                strip.y,
                sw,
                sh,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        self.sync_thumbnail();
    }

    fn minimize(&mut self, point: Point) {
        if !self.minimized {
            self.normal_rect = self.content_rect;
        }
        self.minimized = true;
        self.content_rect = Rect {
            x: point.x,
            y: point.y,
            w: dip_to_px(32, self.dpi.max(1)),
            h: dip_to_px(32, self.dpi.max(1)),
        };
        self.apply_bounds(self.content_rect);
        self.update_strip_visibility();
    }

    fn restore(&mut self, bounds: Rect) {
        self.minimized = false;
        self.normal_rect = bounds;
        self.apply_bounds(bounds);
        self.hover_content = false;
        self.hover_strip = false;
        self.update_strip_visibility();
    }

    fn finish_drag_update(&mut self, ended: Option<DragUpdate>, release_capture: bool) {
        if let Some(DragUpdate::Ended(point)) = ended {
            self.emit(CropWindowEvent::Geometry {
                x: point.x,
                y: point.y,
                w: self.content_rect.w,
                h: self.content_rect.h,
            });
        }
        if release_capture {
            // `drag` уже неактивен, поэтому синхронный `WM_CAPTURECHANGED`
            // от `ReleaseCapture` не выдаст второе событие геометрии.
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.update_strip_visibility();
    }

    fn finish_drag(&mut self, release_capture: bool) {
        let ended = self.drag.end();
        self.finish_drag_update(ended, release_capture);
    }

    fn start_tracking(&self, hwnd: HWND) {
        let mut event = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: event points to a live stack value and hwnd is our window.
        let _ = unsafe { TrackMouseEvent(&mut event) };
    }

    fn on_message(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let role = self.role(hwnd);
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            // Кусок выбрали в Alt+Tab. Мышью он не активируется
            // (`MA_NOACTIVATE`), значит активация пришла с клавиатуры —
            // человек «открыл кусок как окно». Свёрнутый при этом
            // разворачивается: иначе выбор в Alt+Tab ничего бы не показал.
            WM_ACTIVATE if role == WindowRole::Content => {
                let active = (wp.0 & 0xFFFF) != 0; // WA_INACTIVE == 0
                if active && self.minimized {
                    self.emit(CropWindowEvent::RestoreClicked);
                }
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
            // Рамка изменения размера есть, но не видна: клиентская область
            // занимает всё окно. Иначе поверх куска появилась бы системная
            // рамка, чужая всему оформлению.
            WM_NCCALCSIZE if role == WindowRole::Content && wp.0 != 0 => LRESULT(0),
            WM_NCHITTEST if role == WindowRole::Content && !self.minimized => {
                let (x, y) = (
                    (lp.0 & 0xFFFF) as i16 as i32,
                    ((lp.0 >> 16) & 0xFFFF) as i16 as i32,
                );
                LRESULT(resize_hit(self.content_rect, self.dpi, x, y) as isize)
            }
            // Полоса лежит ПОВЕРХ верхней кромки куска и закрывала собой
            // верхнюю зону изменения размера: сверху окно тянулось только
            // когда полоса спрятана (живой репорт 2026-09-12).
            //
            // `HTTRANSPARENT` отдаёт попадание окну НИЖЕ в том же потоке —
            // а содержимое как раз в том же потоке, что и полоса. Это тот
            // самый случай, для которого приём и предназначен; с окнами
            // чужих процессов он бы не работал (замер X3, 2026-09-11).
            WM_NCHITTEST if role == WindowRole::Strip => {
                let (x, y) = (
                    (lp.0 & 0xFFFF) as i16 as i32,
                    ((lp.0 >> 16) & 0xFFFF) as i16 as i32,
                );
                let strip = strip_rect(self.content_rect, self.screen_top, self.dpi);
                // Кнопки важнее кромки. Они стоят у самого правого края, и их
                // крайние точки попадают в зону изменения размера: без этой
                // проверки клик по «закрыть» у края начинал бы тянуть окно.
                let local = hit_test_strip(strip.w, strip.h, x - strip.x, y - strip.y);
                if matches!(local, StripHit::Pin | StripHit::Minimize | StripHit::Close) {
                    LRESULT(HTCLIENT as isize)
                } else if resize_hit(strip, self.dpi, x, y) != HTCLIENT {
                    LRESULT(HTTRANSPARENT as isize)
                } else {
                    LRESULT(HTCLIENT as isize)
                }
            }
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            // Пропорции сохраняются: DWM растягивает превью на всё окно, и
            // свободное растяжение искажало бы картинку. Shift отпускает
            // пропорцию тем, кому нужно вписать кусок в конкретное место.
            WM_SIZING if role == WindowRole::Content => {
                // SAFETY: `lp` в `WM_SIZING` — указатель на RECT окна.
                let rect = unsafe { &mut *(lp.0 as *mut RECT) };
                let shift = unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) < 0 };
                if !shift {
                    keep_aspect(rect, wp.0 as u32, self.aspect());
                }
                LRESULT(1)
            }
            WM_GETMINMAXINFO if role == WindowRole::Content => {
                // SAFETY: `lp` в `WM_GETMINMAXINFO` — указатель на MINMAXINFO.
                let info = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                let min = dip_to_px(MIN_SIDE_DIP as u32, self.dpi.max(1)) as i32;
                info.ptMinTrackSize.x = min;
                info.ptMinTrackSize.y = min;
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if role == WindowRole::Strip && self.drag.is_active() {
                    // Это намеренно первая операция обработчика: физическое
                    // состояние кнопки решает, можно ли ещё двигать окна.
                    let left_down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 };
                    if let Some(cursor) = cursor_position() {
                        if let Some(update) = self.drag.move_cursor(cursor, left_down) {
                            match update {
                                DragUpdate::Moved(point) => self.move_windows(point.x, point.y),
                                DragUpdate::Ended(point) => {
                                    self.finish_drag_update(Some(DragUpdate::Ended(point)), true)
                                }
                            }
                        }
                    } else if !left_down {
                        self.finish_drag(true);
                    }
                    self.start_tracking(hwnd);
                    return LRESULT(0);
                }
                if role == WindowRole::Content {
                    self.hover_content = true;
                } else {
                    self.hover_strip = true;
                    let (x, y) = low_words(lp);
                    let mut client = RECT::default();
                    // SAFETY: hwnd — живое окно полосы этого потока.
                    unsafe {
                        let _ = GetClientRect(hwnd, &mut client);
                    }
                    let hit = hit_test_strip(
                        (client.right - client.left).max(0) as u32,
                        (client.bottom - client.top).max(0) as u32,
                        x,
                        y,
                    );
                    if hit != self.hover_button {
                        self.hover_button = hit;
                        // SAFETY: перерисовка своего окна.
                        unsafe {
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
                self.start_tracking(hwnd);
                self.kill_hide_timer();
                self.update_strip_visibility();
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if role == WindowRole::Content {
                    self.hover_content = false;
                } else {
                    self.hover_strip = false;
                    if self.hover_button != StripHit::None {
                        self.hover_button = StripHit::None;
                        // SAFETY: перерисовка своего окна.
                        unsafe {
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
                // Гасим не сразу: курсор мог всего лишь пересечь границу
                // между полосой и содержимым. Таймер перепроверит, где он
                // на самом деле.
                self.arm_hide_timer();
                LRESULT(0)
            }
            WM_ENTERSIZEMOVE if role == WindowRole::Content => {
                self.in_size_move = true;
                LRESULT(0)
            }
            WM_EXITSIZEMOVE if role == WindowRole::Content => {
                self.in_size_move = false;
                // Windows подняла содержимое, пока тянули, — возвращаем полосу
                // наверх, иначе она осталась бы под ним навсегда.
                self.raise_strip();
                self.update_strip_visibility();
                // Один итоговый отчёт за весь цикл Windows.
                self.emit(CropWindowEvent::Geometry {
                    x: self.content_rect.x,
                    y: self.content_rect.y,
                    w: self.content_rect.w,
                    h: self.content_rect.h,
                });
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGED if role == WindowRole::Content => {
                // Полосу z-порядка содержимого могли сменить снаружи —
                // закрепление окна (`crate::window_pin`), раскладка группы,
                // показ группы. Полоса обязана уйти в ту же полосу немедленно,
                // а не при следующем наведении: иначе topmost-содержимое
                // накрывает её, и кусок становится нечем двигать (репорт
                // пользователя 2026-09-13).
                //
                // `SWP_NOZORDER` в сообщении означает «порядок не менялся» —
                // такие сообщения (их большинство: любое перемещение) проходят
                // мимо, лишних `SetWindowPos` не будет.
                //
                let zorder_changed = if lp.0 == 0 {
                    // Структуры нет — судить не о чем; считаем, что порядок
                    // мог измениться, и утверждаем его заново. Дешевле одного
                    // лишнего `SetWindowPos`, чем потерянная полоса.
                    true
                } else {
                    // SAFETY: `lp` у этого сообщения — указатель на `WINDOWPOS`,
                    // живой на время обработки; читаем только флаги.
                    let flags = unsafe { (*(lp.0 as *const WINDOWPOS)).flags };
                    !flags.contains(SWP_NOZORDER)
                };
                if zorder_changed && !self.minimized {
                    self.raise_strip();
                }
                // Окно куска подвинул кто-то снаружи: раскладка группы,
                // закрепление, привязка Windows. Сообщаем координатору, иначе
                // он вернёт окно на место из конфига.
                if !self.self_move && !self.drag.is_active() && !self.minimized {
                    let mut rect = RECT::default();
                    // SAFETY: своё окно этого потока.
                    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                        let moved = Rect {
                            x: rect.left,
                            y: rect.top,
                            w: (rect.right - rect.left).max(0) as u32,
                            h: (rect.bottom - rect.top).max(0) as u32,
                        };
                        if moved != self.content_rect {
                            let resized =
                                moved.w != self.content_rect.w || moved.h != self.content_rect.h;
                            self.content_rect = moved;
                            self.reposition_strip();
                            if resized {
                                // Область, в которую DWM рисует превью, задана
                                // в пикселях окна: без пересчёта картинка
                                // осталась бы прежнего размера в новом окне.
                                self.sync_thumbnail();
                            }
                            // Пока идёт цикл Windows — молчим: итог отправит
                            // `WM_EXITSIZEMOVE`, иначе конфиг писался бы на
                            // каждый шаг мыши.
                            if !self.in_size_move {
                                self.emit(CropWindowEvent::Geometry {
                                    x: moved.x,
                                    y: moved.y,
                                    w: moved.w,
                                    h: moved.h,
                                });
                            }
                        }
                    }
                }
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
            WM_TIMER if wp.0 == STRIP_HIDE_TIMER => {
                self.kill_hide_timer();
                if self.cursor_near_piece() {
                    // Курсор всё ещё у куска — держим полосу и ждём дальше.
                    self.arm_hide_timer();
                } else {
                    self.hover_content = false;
                    self.hover_strip = false;
                    self.hover_button = StripHit::None;
                    self.update_strip_visibility();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN if role == WindowRole::Strip => {
                let (x, y) = low_words(lp);
                let (width, height) = client_size(hwnd);
                match hit_test_strip(width, height, x, y) {
                    StripHit::Close => self.emit(CropWindowEvent::CloseClicked),
                    StripHit::Minimize => self.emit(CropWindowEvent::MinimizeClicked),
                    StripHit::Pin => {
                        // Применяем сразу, не дожидаясь координатора: человек
                        // нажал кнопку и обязан увидеть результат в тот же
                        // кадр. Координатор получит событие и запишет решение
                        // в конфиг, чтобы оно пережило перезапуск.
                        let on = !self.always_on_top;
                        self.set_always_on_top(on);
                        self.emit(CropWindowEvent::AlwaysOnTopToggled(on));
                    }
                    StripHit::Drag => {
                        if let Some(cursor) = cursor_position() {
                            self.drag.begin(
                                cursor,
                                Point {
                                    x: self.content_rect.x,
                                    y: self.content_rect.y,
                                },
                            );
                            // Only the strip owns capture; the thumbnail window
                            // remains a normal no-activate popup.
                            unsafe {
                                SetCapture(self.strip);
                            }
                            self.update_strip_visibility();
                        }
                    }
                    StripHit::None => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN if role == WindowRole::Content && self.minimized => {
                self.minimized = false;
                let bounds = self.normal_rect;
                self.apply_bounds(bounds);
                self.emit(CropWindowEvent::RestoreClicked);
                self.update_strip_visibility();
                LRESULT(0)
            }
            WM_LBUTTONUP if role == WindowRole::Strip => {
                self.finish_drag(true);
                LRESULT(0)
            }
            WM_CAPTURECHANGED if role == WindowRole::Strip => {
                // Capture was already lost; never call ReleaseCapture again.
                self.finish_drag(false);
                LRESULT(0)
            }
            WM_CANCELMODE if role == WindowRole::Strip => {
                self.finish_drag(true);
                LRESULT(0)
            }
            WM_ERASEBKGND if role == WindowRole::Content => LRESULT(1),
            WM_PAINT if role == WindowRole::Strip => {
                paint_strip(
                    hwnd,
                    &self.app_name,
                    self.dpi,
                    self.hover_button,
                    self.always_on_top,
                );
                LRESULT(0)
            }
            WM_PAINT if role == WindowRole::Content => {
                let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                // DWM owns the pixels, but Begin/EndPaint still clear the
                // update region so the window does not spin WM_PAINT.
                unsafe {
                    let _ = BeginPaint(hwnd, &mut paint);
                    let _ = EndPaint(hwnd, &paint);
                }
                LRESULT(0)
            }
            // У края окна курсор ставит САМА система — двусторонняя стрелка
            // и есть единственная подсказка «здесь можно тянуть». Прежний
            // обработчик выставлял стрелку всегда и перебивал их: человек не
            // видел, где хвататься, и попадал в кромку случайно (живой репорт
            // 2026-09-12).
            WM_SETCURSOR
                if matches!(
                    (lp.0 & 0xFFFF) as u32,
                    HTLEFT
                        | HTRIGHT
                        | HTTOP
                        | HTBOTTOM
                        | HTTOPLEFT
                        | HTTOPRIGHT
                        | HTBOTTOMLEFT
                        | HTBOTTOMRIGHT
                ) =>
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            WM_SETCURSOR => {
                // Курсор ОБЯЗАН выставляться явно. Прежний обработчик возвращал
                // `1` — «курсор выставлен» — но `SetCursor` не звал, а своего
                // курсора у класса окна не было. Над куском курсор тогда не
                // выставлял никто, и оставался тот, что был до входа, —
                // у пользователя это оказался кружок «занят», висящий
                // бесконечно (живой скриншот 2026-09-11).
                //
                // Стрелка и над полосой, и над содержимым: у заголовков окон
                // Windows тоже стрелка, а кусок не кликается — рука или
                // I-образный курсор обещали бы то, чего нет.
                // SAFETY: системный курсор, указателей не принимает.
                unsafe {
                    if let Ok(arrow) = LoadCursorW(None, IDC_ARROW) {
                        SetCursor(Some(arrow));
                    }
                }
                LRESULT(1)
            }
            WM_APP_COMMAND if role == WindowRole::Content => {
                // SAFETY: pointer was allocated by post_command and is
                // consumed exactly once by this message.
                let command = unsafe { Box::from_raw(lp.0 as *mut Command) };
                self.apply_command(*command);
                LRESULT(0)
            }
            WM_CLOSE => {
                // Сюда доходит ТОЛЬКО системное закрытие: своё координатор
                // шлёт через `WM_APP_SHUTDOWN`. Для человека Alt+F4 по куску
                // означает ровно то же, что крестик на полосе, — удалить его.
                self.emit(CropWindowEvent::CloseClicked);
                self.finish_drag(true);
                let content = self.content;
                let strip = self.strip;
                // DestroyWindow is synchronous; both handles are copied before
                // the nested window procedure starts.
                unsafe {
                    if role == WindowRole::Content {
                        if strip != content && !strip.0.is_null() {
                            let _ = DestroyWindow(strip);
                        }
                        let _ = DestroyWindow(content);
                    } else {
                        let _ = DestroyWindow(strip);
                    }
                }
                LRESULT(0)
            }
            WM_APP_SHUTDOWN => {
                self.finish_drag(true);
                let content = self.content;
                let strip = self.strip;
                // SAFETY: свои окна этого потока; `DestroyWindow` синхронен,
                // хэндлы скопированы до вложенной оконной процедуры.
                unsafe {
                    if strip != content && !strip.0.is_null() {
                        let _ = DestroyWindow(strip);
                    }
                    let _ = DestroyWindow(content);
                }
                LRESULT(0)
            }
            WM_DESTROY if role == WindowRole::Content => {
                if let Some(thumbnail) = self.thumbnail.take() {
                    // SAFETY: registration belongs to this GUI thread and is
                    // released before its destination window disappears.
                    let _ = unsafe { DwmUnregisterThumbnail(thumbnail) };
                }
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            WM_NCDESTROY => LRESULT(0),
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }

    fn apply_command(&mut self, command: Command) {
        match command {
            Command::SetOpacity(opacity) => {
                self.opacity = opacity;
                self.sync_thumbnail();
            }
            Command::SetBounds(bounds) => {
                self.normal_rect = bounds;
                if !self.minimized {
                    self.apply_bounds(bounds);
                }
            }
            Command::Minimize(point) => self.minimize(point),
            Command::Restore(bounds) => self.restore(bounds),
            Command::SetAlwaysOnTop(on) => self.set_always_on_top(on),
        }
    }
}

fn run_message_loop(
    ready_tx: Sender<Result<ReadyWindows, CropWindowError>>,
    event_tx: Sender<CropWindowEvent>,
    source_raw: isize,
    options: CropWindowOptions,
) {
    let result = create_windows(source_raw, options, event_tx);
    let (content, strip, state_ptr) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    if ready_tx
        .send(Ok(ReadyWindows {
            content: content.0 as isize,
            strip: strip.0 as isize,
        }))
        .is_err()
    {
        unsafe {
            let _ = DestroyWindow(strip);
            let _ = DestroyWindow(content);
        }
        return;
    }

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // Both windows are gone and no more messages can use the shared pointer.
    unsafe {
        drop(Box::from_raw(state_ptr));
    }
}

fn create_windows(
    source_raw: isize,
    options: CropWindowOptions,
    tx: Sender<CropWindowEvent>,
) -> Result<(HWND, HWND, *mut WindowState), CropWindowError> {
    let hinstance = unsafe { GetModuleHandleW(None) }?.into();
    register_class(hinstance)?;
    if options.bounds.w == 0 || options.bounds.h == 0 {
        return Err(CropWindowError::CreateFailed);
    }
    let (width, height) = dimensions(options.bounds);
    let strip = strip_rect(options.bounds, options.screen_top, options.dpi);
    let (strip_width, strip_height) = dimensions(strip);
    // Окно содержимого — ОБЫЧНОЕ окно приложения (`WS_EX_APPWINDOW`), без
    // `WS_EX_TOOLWINDOW` и `WS_EX_NOACTIVATE`: оба флага прячут окно из
    // Alt+Tab, а кусок обязан там быть (запрос пользователя 2026-09-11).
    //
    // Свойство «клик по куску не уводит фокус из окна, где человек печатает»
    // (замер V1 фаза 2) держится не флагом, а ответом `MA_NOACTIVATE` на
    // `WM_MOUSEACTIVATE`: мышью окно не активируется, а Alt+Tab — может.
    //
    // `WS_EX_TOPMOST` НЕ ставится по умолчанию: кусок — обычное окно и обязан
    // уходить под другие, как любое другое (репорт пользователя 2026-09-12:
    // «я не хочу чтобы вырезаные окна были алвейз он топ по умолчанию»). Пока
    // топмост был безусловным, куски накрывали собой даже панели самого
    // resticker. Поверх всех окон кусок поднимает только булавка на полосе.
    let topmost = if options.always_on_top {
        WS_EX_TOPMOST
    } else {
        WINDOW_EX_STYLE(0)
    };
    let content_style = topmost | WS_EX_APPWINDOW;
    // Полоса остаётся служебным окном: в Alt+Tab был бы второй пункт на один
    // и тот же кусок.
    let style = topmost | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let title_text = if options.window_title.trim().is_empty() {
        options.app_name.clone()
    } else {
        options.window_title.clone()
    };
    let title_wide: Vec<u16> = title_text
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: class is registered above; sizes are checked and HWND ownership
    // stays on this thread.
    let content = unsafe {
        CreateWindowExW(
            content_style,
            CLASS_NAME,
            PCWSTR(title_wide.as_ptr()),
            // `WS_THICKFRAME` даёт окну рамку изменения размера. Саму рамку
            // мы убираем в `WM_NCCALCSIZE`, оставляя только её поведение:
            // тянуть кусок за край, как любое окно. Размер меняет САМА
            // Windows своим циклом — наш код захвата мыши в этом не
            // участвует, и залипнуть ему негде.
            WS_POPUP | WS_THICKFRAME,
            options.bounds.x,
            options.bounds.y,
            width,
            height,
            None,
            None,
            Some(hinstance),
            None,
        )
    }?;
    // Владелец полосы — окно содержимого. Это не косметика: Windows сама
    // держит владеемое окно ВЫШЕ владельца в z-порядке, и полоса перестаёт
    // зависеть от того, кто последний всплыл. Раньше её держал наверху только
    // безусловный `WS_EX_TOPMOST`; без топмоста (а теперь он по умолчанию
    // снят) полоса ушла бы под собственный кусок при первом же клике по нему —
    // тот же баг, что уже был при изменении размера 2026-09-12, когда
    // содержимое активировалось и накрывало полосу.
    let strip_hwnd = match unsafe {
        CreateWindowExW(
            style,
            CLASS_NAME,
            WINDOW_TITLE,
            WS_POPUP,
            strip.x,
            strip.y,
            strip_width,
            strip_height,
            Some(content),
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(hwnd) => hwnd,
        Err(error) => {
            unsafe {
                let _ = DestroyWindow(content);
            }
            return Err(error.into());
        }
    };
    let app_name = utf16(&options.app_name);
    let mut state = Box::new(WindowState {
        content,
        strip: strip_hwnd,
        source: hwnd_from_raw(source_raw),
        thumbnail: None,
        content_rect: options.bounds,
        normal_rect: options.bounds,
        screen_top: options.screen_top,
        dpi: options.dpi.max(1),
        source_rect: options.source_rect,
        opacity: options.opacity,
        app_name,
        minimized: false,
        always_on_top: options.always_on_top,
        hover_content: false,
        hover_strip: false,
        hover_button: StripHit::None,
        self_move: false,
        in_size_move: false,
        drag: DragTracker::new(),
        source_gone_sent: false,
        tx,
    });
    let state_ptr = &mut *state as *mut WindowState;
    unsafe {
        SetWindowLongPtrW(content, GWLP_USERDATA, state_ptr as isize);
        SetWindowLongPtrW(strip_hwnd, GWLP_USERDATA, state_ptr as isize);
    }
    state.register_thumbnail();
    state.update_strip_visibility();
    set_own_icon(content);
    unsafe {
        let _ = ShowWindow(content, SW_SHOWNOACTIVATE);
        let _ = ShowWindow(strip_hwnd, SW_HIDE);
        // DWM rounding is best effort on systems that do not expose the
        // Windows 11 attribute; preview and hit-testing do not depend on it.
        let preference = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            content,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const c_void,
            size_of_val(&preference) as u32,
        );
        let _ = DwmSetWindowAttribute(
            strip_hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const c_void,
            size_of_val(&preference) as u32,
        );
    }
    std::mem::forget(state);
    Ok((content, strip_hwnd, state_ptr))
}

fn register_class(hinstance: windows::Win32::Foundation::HINSTANCE) -> Result<(), CropWindowError> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        hInstance: hinstance,
        lpfnWndProc: Some(crop_wndproc),
        lpszClassName: CLASS_NAME,
        // Курсор класса — вторая линия защиты к явному `SetCursor` в
        // `WM_SETCURSOR`: без него окно, до которого `WM_SETCURSOR` не дошёл,
        // оставило бы чужой курсор.
        // SAFETY: системный курсор, указателей не принимает.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        let error = unsafe { windows::Win32::Foundation::GetLastError() };
        if error != ERROR_CLASS_ALREADY_EXISTS {
            return Err(CropWindowError::Win32(windows::core::Error::from_thread()));
        }
    }
    Ok(())
}

unsafe extern "system" fn crop_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // SAFETY: pointer is installed before the window is shown and freed only
    // after the message loop exits.
    unsafe { (&mut *ptr).on_message(hwnd, msg, wp, lp) }
}

/// Нарисовать полосу: подпись и две кнопки.
///
/// Прежняя версия (2026-09-11) рисовала глифы кнопок `LineTo`, задав цвет
/// через `SetTextColor`. Но `SetTextColor` красит только ТЕКСТ, а линии
/// идут пером, и перо по умолчанию чёрное — на почти чёрной полосе кнопок не
/// было видно вовсе (живой скриншот пользователя). Здесь у линий своё перо.
/// Подпись выводилась растровым системным шрифтом, прижатая к верху полосы;
/// теперь — Segoe UI со сглаживанием, по центру по вертикали, с многоточием,
/// если не влезает до кнопок.
fn paint_strip(hwnd: HWND, app_name: &[u16], dpi: u32, hover: StripHit, always_on_top: bool) {
    let scale = |dip: i32| -> i32 { ((dip * dpi.max(1) as i32) + 48) / 96 };
    let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
    if hdc.0.is_null() {
        return;
    }
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let height = (client.bottom - client.top).max(1);
    let width = (client.right - client.left).max(1);
    // Та же раскладка, что у `hit_test_strip`: то, что нарисовано, обязано
    // совпадать с тем, куда попадает клик.
    let button = strip_button_side(width.max(0) as u32, height.max(0) as u32) as i32;
    // Булавка слева, «свернуть» и «закрыть» справа — см. `hit_test_strip`.
    let pin_left = 0;
    let min_left = width - button * 2;
    let close_left = width - button;

    unsafe {
        // --- фон ---
        let bg = CreateSolidBrush(COLOR_STRIP);
        let _ = FillRect(hdc, &client, bg);
        let _ = DeleteObject(bg.into());

        // --- подсветка кнопки под курсором ---
        let hovered = match hover {
            StripHit::Pin => Some((pin_left, COLOR_HOVER)),
            StripHit::Minimize => Some((min_left, COLOR_HOVER)),
            StripHit::Close => Some((close_left, COLOR_CLOSE_HOVER)),
            StripHit::None | StripHit::Drag => None,
        };

        if let Some((left, color)) = hovered {
            let rect = RECT {
                left,
                top: 0,
                right: left + button,
                bottom: height,
            };
            let brush = CreateSolidBrush(color);
            let _ = FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush.into());
        }

        // --- подпись ---
        let mut lf = windows::Win32::Graphics::Gdi::LOGFONTW {
            lfHeight: -scale(TEXT_SIZE_DIP),
            lfWeight: 600,
            lfQuality: windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY,
            ..Default::default()
        };
        for (dst, src) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *dst = src;
        }
        let font = windows::Win32::Graphics::Gdi::CreateFontIndirectW(&lf);
        let old_font = windows::Win32::Graphics::Gdi::SelectObject(hdc, font.into());
        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLOR_TEXT);
        let mut text_rect = RECT {
            left: button + scale(TEXT_PAD_DIP),
            top: 0,
            right: (min_left - scale(TEXT_PAD_DIP) / 2).max(button + scale(TEXT_PAD_DIP)),
            bottom: height,
        };
        let mut text: Vec<u16> = app_name.strip_suffix(&[0]).unwrap_or(app_name).to_vec();
        let _ = windows::Win32::Graphics::Gdi::DrawTextW(
            hdc,
            &mut text,
            &mut text_rect,
            windows::Win32::Graphics::Gdi::DT_SINGLELINE
                | windows::Win32::Graphics::Gdi::DT_VCENTER
                | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS
                | windows::Win32::Graphics::Gdi::DT_NOPREFIX,
        );
        let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);
        let _ = DeleteObject(font.into());

        // --- глифы: у линий СВОЁ перо, `SetTextColor` на них не действует ---
        let pen_w = scale(1).max(1);
        let pen = windows::Win32::Graphics::Gdi::CreatePen(
            windows::Win32::Graphics::Gdi::PS_SOLID,
            pen_w,
            COLOR_GLYPH,
        );
        let old_pen = windows::Win32::Graphics::Gdi::SelectObject(hdc, pen.into());
        let g = scale(GLYPH_DIP) / 2;
        let cy = height / 2;
        // «свернуть» — горизонтальная черта по центру своей кнопки
        let mx = min_left + button / 2;
        let _ = MoveToEx(hdc, mx - g, cy, None);
        let _ = LineTo(hdc, mx + g + 1, cy);
        // «закрыть» — крест по центру своей кнопки
        let cx = close_left + button / 2;
        let _ = MoveToEx(hdc, cx - g, cy - g, None);
        let _ = LineTo(hdc, cx + g + 1, cy + g + 1);
        let _ = MoveToEx(hdc, cx + g, cy - g, None);
        let _ = LineTo(hdc, cx - g - 1, cy + g + 1);
        let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_pen);
        let _ = DeleteObject(pen.into());

        // Булавка — тот же рисунок, что у значка закреплённого окна: человек
        // уже знает его по всем окнам, и второй значок для того же смысла
        // только путал бы (запрос пользователя 2026-09-14).
        draw_pin(hdc, pin_left, button, height, always_on_top);

        let _ = EndPaint(hwnd, &paint);
    }
}

/// Рисунок булавки — тот же PNG, которым помечены закреплённые окна.
///
/// Полоса куска рисуется GDI, а не через D3D-оверлей, поэтому картинку
/// приходится расшифровывать и класть на контекст самим. Исходник берётся из
/// ресурсов `rst-render`: две копии одного рисунка разошлись бы при первой же
/// правке, а два разных значка для одного смысла человек читает как две
/// разные функции.
const PIN_PNG: &[u8] = include_bytes!("../../rst-render/assets/pinned_badge.png");

/// Расшифрованный рисунок булавки (RGBA, как в файле) — один раз на процесс.
fn pin_rgba() -> Option<&'static (Vec<u8>, u32, u32)> {
    static PIN: std::sync::OnceLock<Option<(Vec<u8>, u32, u32)>> = std::sync::OnceLock::new();
    PIN.get_or_init(|| match image::load_from_memory(PIN_PNG) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            Some((rgba.into_raw(), w, h))
        }
        Err(e) => {
            // Битый ресурс — не повод ронять окно: полоса останется без
            // значка, а причина видна в журнале (тот же принцип, что у
            // иконок оверлея).
            tracing::warn!(error = %e, "рисунок булавки не расшифровался");
            None
        }
    })
    .as_ref()
}

/// Положить булавку по центру её кнопки.
///
/// Включённая — в полную силу, выключенная — приглушённая: состояние читается
/// яркостью самого значка, без заливки кнопки. Заливка выглядела как «другая
/// кнопка», а не как «та же булавка в другом состоянии».
fn draw_pin(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    left: i32,
    button: i32,
    height: i32,
    on: bool,
) {
    use windows::Win32::Graphics::Gdi::AlphaBlend;
    use windows::Win32::Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, SelectObject,
    };

    let Some((src, src_w, src_h)) = pin_rgba() else {
        return;
    };
    // Значок соразмерен подписи полосы, а не кнопке целиком: булавка во всю
    // высоту кнопки выглядела бы тяжелее, чем сама полоса.
    let side = ((button * 5) / 8).max(8);
    if side <= 0 || *src_w == 0 || *src_h == 0 {
        return;
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: bmi описывает 32bpp top-down DIB side×side, bits — out-параметр.
    let bitmap = match unsafe { CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) } {
        Ok(b) if !bits.is_null() => b,
        _ => return,
    };
    // Усреднение при уменьшении съедает плотность тонких штрихов: рисунок
    // становится полупрозрачным весь, а не только по краям. Поэтому покрытие
    // слегка поджимается к единице — штрих остаётся штрихом, сглаживание
    // краёв сохраняется.
    const INK: f32 = 1.5;
    // Приглушение выключенной булавки — по альфе, как у текста полосы.
    let fade = if on { 1.0_f32 } else { 0.45 };
    // SAFETY: bits — буфер ровно side*side*4 байта; пишем premultiplied BGRA,
    // как требует AlphaBlend.
    //
    // Уменьшение — усреднением по всем исходным пикселям, попавшим в точку
    // назначения, а не «по ближайшему»: рисунок булавки состоит из тонких
    // штрихов, и выборка одного пикселя рвала бы их в пунктир.
    unsafe {
        let dst = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (side * side * 4) as usize);
        let side_u = side as u32;
        for y in 0..side_u {
            let y0 = y * *src_h / side_u;
            let y1 = (((y + 1) * *src_h).div_ceil(side_u))
                .max(y0 + 1)
                .min(*src_h);
            for x in 0..side_u {
                let x0 = x * *src_w / side_u;
                let x1 = (((x + 1) * *src_w).div_ceil(side_u))
                    .max(x0 + 1)
                    .min(*src_w);
                let mut sum = 0.0_f32;
                let mut count = 0.0_f32;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let si = ((sy * *src_w + sx) * 4) as usize;
                        sum += f32::from(src[si + 3]);
                        count += 1.0;
                    }
                }
                let a = if count > 0.0 {
                    (sum / count / 255.0 * INK).min(1.0) * fade
                } else {
                    0.0
                };
                let di = ((y * side_u + x) * 4) as usize;
                // Цвет штриха — тот же светлый, что у глифов полосы.
                let lit = (f32::from(0xD8u8) * a).round() as u8;
                dst[di] = lit;
                dst[di + 1] = lit;
                dst[di + 2] = lit;
                dst[di + 3] = (a * 255.0).round() as u8;
            }
        }
    }
    // SAFETY: свой контекст и свой битмап; все хэндлы освобождаются ниже.
    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        if !mem.is_invalid() {
            let old = SelectObject(mem, bitmap.into());
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = AlphaBlend(
                hdc,
                left + (button - side) / 2,
                (height - side) / 2,
                side,
                side,
                mem,
                0,
                0,
                side,
                side,
                blend,
            );
            let _ = SelectObject(mem, old);
            let _ = DeleteDC(mem);
        }
        let _ = DeleteObject(bitmap.into());
    }
}

/// Поставить окну куска иконку САМОГО resticker.
///
/// Не иконку окна-источника: в Alt+Tab и на панели задач кусок Discord с
/// иконкой Discord неотличим от самого Discord, и человек путается (запрос
/// пользователя 2026-09-12). Своя иконка сразу говорит, чьё это окно.
///
/// Иконка берётся из ресурсов собственного exe (`tray::app_icon`) — тем же
/// способом, что и значок в трее. Если извлечь не удалось, окно останется с
/// иконкой по умолчанию: это не ошибка.
fn set_own_icon(content: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{ICON_BIG, ICON_SMALL, SendMessageW, WM_SETICON};
    let Some(icon) = crate::tray::app_icon() else {
        return;
    };
    let handle = icon.0 as isize;
    // SAFETY: своё окно; иконка принадлежит ресурсам процесса, `WM_SETICON`
    // её во владение не забирает.
    unsafe {
        for kind in [ICON_BIG, ICON_SMALL] {
            let _ = SendMessageW(
                content,
                WM_SETICON,
                Some(WPARAM(kind as usize)),
                Some(LPARAM(handle)),
            );
        }
    }
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn hwnd_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn dimensions(rect: Rect) -> (i32, i32) {
    (
        rect.w.min(i32::MAX as u32) as i32,
        rect.h.min(i32::MAX as u32) as i32,
    )
}

fn cursor_position() -> Option<Point> {
    let mut point = windows::Win32::Foundation::POINT::default();
    unsafe {
        GetCursorPos(&mut point).ok()?;
    }
    Some(Point {
        x: point.x,
        y: point.y,
    })
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return (0, 0);
    }
    (
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
    )
}

fn low_words(lp: LPARAM) -> (i32, i32) {
    let value = lp.0 as u32;
    (
        (value as u16 as i16) as i32,
        ((value >> 16) as u16 as i16) as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_hit_marks_edges_and_corners() {
        let r = Rect {
            x: 100,
            y: 100,
            w: 300,
            h: 200,
        };
        // Углы важнее сторон: в углу тянут сразу по двум осям.
        assert_eq!(resize_hit(r, 96, 101, 101), HTTOPLEFT);
        assert_eq!(resize_hit(r, 96, 398, 101), HTTOPRIGHT);
        assert_eq!(resize_hit(r, 96, 101, 298), HTBOTTOMLEFT);
        assert_eq!(resize_hit(r, 96, 398, 298), HTBOTTOMRIGHT);
        assert_eq!(resize_hit(r, 96, 101, 200), HTLEFT);
        assert_eq!(resize_hit(r, 96, 398, 200), HTRIGHT);
        assert_eq!(resize_hit(r, 96, 250, 101), HTTOP);
        assert_eq!(resize_hit(r, 96, 250, 298), HTBOTTOM);
        // Середина — обычное содержимое, а не край.
        assert_eq!(resize_hit(r, 96, 250, 200), HTCLIENT);
        // Точка вне окна не должна выдавать зону края.
        assert_eq!(resize_hit(r, 96, 50, 50), HTCLIENT);
    }

    #[test]
    fn resize_edge_grows_with_dpi() {
        // На мониторе со 150% полоса края обязана быть шире в пикселях,
        // иначе на нём в неё было бы вдвое труднее попасть.
        let r = Rect {
            x: 0,
            y: 0,
            w: 300,
            h: 200,
        };
        assert_eq!(
            resize_hit(r, 96, 8, 100),
            HTCLIENT,
            "при 100% 8 px — уже содержимое"
        );
        assert_eq!(
            resize_hit(r, 144, 8, 100),
            HTLEFT,
            "при 150% та же точка — ещё край"
        );
    }

    #[test]
    fn keep_aspect_derives_the_other_side() {
        // Тянут за правый край — подстраивается высота.
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 400,
            bottom: 100,
        };
        keep_aspect(&mut r, WMSZ_RIGHT, Some(2.0));
        assert_eq!(r.bottom - r.top, 200, "высота = ширина / пропорция");
        // Тянут за нижний край — подстраивается ширина.
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 150,
        };
        keep_aspect(&mut r, WMSZ_BOTTOM, Some(2.0));
        assert_eq!(r.right - r.left, 300, "ширина = высота * пропорция");
    }

    #[test]
    fn keep_aspect_leaves_degenerate_input_alone() {
        // Без пропорции (вырожденный кусок) прямоугольник не трогаем: иначе
        // окно схлопнулось бы в ноль.
        let before = RECT {
            left: 10,
            top: 20,
            right: 110,
            bottom: 220,
        };
        let mut r = before;
        keep_aspect(&mut r, WMSZ_RIGHT, None);
        assert_eq!((r.left, r.top, r.right, r.bottom), (10, 20, 110, 220));
        let mut r = before;
        keep_aspect(&mut r, WMSZ_RIGHT, Some(0.0));
        assert_eq!((r.left, r.top, r.right, r.bottom), (10, 20, 110, 220));
    }

    #[test]
    fn strip_follows_content_dragged_above_the_screen_top() {
        // Живой репорт 2026-09-11: «тяну окно вверх — бар отлетает». Полоса
        // упиралась в край экрана и переставала следовать за содержимым.
        let high = Rect {
            x: 0,
            y: -100,
            w: 320,
            h: 220,
        };
        let strip = strip_rect(high, 0, 96);
        assert_eq!(strip.y, 0, "полоса не уезжает за экран вслед за куском");
        assert!(
            strip.y >= high.y && strip.y < high.y + high.h as i32,
            "и остаётся НА содержимом, а не отдельно от него"
        );
    }

    #[test]
    fn strip_never_leaves_a_gap_over_the_content() {
        // Живой репорт 2026-09-11: между полосой и содержимым был зазор, и
        // курсор, пересекая его, гасил полосу.
        for y in [-50, 0, 5, 200, 1000] {
            let content = Rect {
                x: 10,
                y,
                w: 300,
                h: 200,
            };
            let strip = strip_rect(content, 0, 96);
            assert!(
                strip.y >= content.y,
                "полоса выше содержимого — между ними зазор (y={y})"
            );
            assert!(
                strip.y < content.y + content.h as i32,
                "полоса оторвалась от содержимого (y={y})"
            );
            assert_eq!(strip.x, content.x, "по горизонтали полоса совпадает");
            assert_eq!(strip.w, content.w, "и по ширине тоже");
        }
    }

    #[test]
    fn strip_lies_over_the_top_edge_of_content() {
        let content = Rect {
            x: 100,
            y: 200,
            w: 320,
            h: 220,
        };
        // Поверх верхней кромки, а не над ней: иначе между двумя окнами
        // остаётся зазор, гасящий полосу при переводе курсора.
        assert_eq!(
            strip_rect(content, 0, 96),
            Rect {
                x: 100,
                y: 200,
                w: 320,
                h: 28
            }
        );
    }

    #[test]
    fn strip_at_monitor_top_is_over_content_not_offscreen() {
        let content = Rect {
            x: -10,
            y: -50,
            w: 320,
            h: 220,
        };
        assert_eq!(
            strip_rect(content, -50, 144),
            Rect {
                x: -10,
                y: -50,
                w: 320,
                h: 42
            }
        );
    }

    #[test]
    fn strip_scales_dip_height_to_physical_pixels() {
        let content = Rect {
            x: 0,
            y: 100,
            w: 320,
            h: 220,
        };
        assert_eq!(strip_rect(content, 0, 192).h, 56);
    }

    #[test]
    fn strip_buttons_and_drag_zone_are_disjoint() {
        // Булавка — слева, там же, где значок у закреплённых окон; справа
        // только «свернуть» и «закрыть».
        assert_eq!(hit_test_strip(320, 28, 10, 14), StripHit::Pin);
        assert_eq!(hit_test_strip(320, 28, 120, 14), StripHit::Drag);
        assert_eq!(hit_test_strip(320, 28, 280, 14), StripHit::Minimize);
        assert_eq!(hit_test_strip(320, 28, 315, 14), StripHit::Close);
        assert_eq!(hit_test_strip(320, 28, 10, 30), StripHit::None);
    }

    #[test]
    fn small_strip_still_has_all_three_button_zones() {
        // Диагностически узкая полоса: кнопки жмутся, но ни одна не исчезает и
        // ни одна не съедает зону перетаскивания — иначе кусок стало бы не за
        // что взять.
        assert_eq!(hit_test_strip(40, 28, 2, 10), StripHit::Pin);
        assert_eq!(hit_test_strip(40, 28, 12, 10), StripHit::Drag);
        assert_eq!(hit_test_strip(40, 28, 22, 10), StripHit::Minimize);
        assert_eq!(hit_test_strip(40, 28, 39, 10), StripHit::Close);
    }

    /// Включённая булавка не должна прятать полосу под собственным куском.
    ///
    /// Живой репорт пользователя 2026-09-13: «когда я закрепляю окно у меня
    /// пропадает верхний контрол бар и я не могу двигать окно, даже когда я уже
    /// снимаю алвейз он топ то контрол бар не появится. при этом всём я могу
    /// скейлить окно». Размер менялся потому, что рамка живёт у самого
    /// содержимого, а тянут кусок ТОЛЬКО за полосу — и её не стало.
    ///
    /// Причина была в порядке двух `SetWindowPos`: внутри одной полосы
    /// z-порядка выигрывает тот, кого переставили последним, а содержимое шло
    /// вторым — и в обе стороны, поэтому выключение булавки положения не
    /// исправляло.
    ///
    /// Тест на реальных окнах: другой проверки тут быть не может — речь ровно
    /// про то, как Windows упорядочивает два живых окна. Свои окна создаются и
    /// уничтожаются в одном запуске, чужие не трогаются, ввод не синтезируется.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 --lib pin_keeps_the_strip -- --ignored"]
    fn pin_keeps_the_strip_above_the_piece() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GW_HWNDNEXT, GetWindow, WS_EX_TOOLWINDOW, WS_POPUP,
            WS_VISIBLE,
        };

        let hinstance = unsafe { GetModuleHandleW(None) }.expect("модуль").into();
        // Собственное окно-источник: чужие окна для тестов не берём.
        let source = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("resticker_crop_pin_test"),
                WS_POPUP | WS_VISIBLE,
                80,
                80,
                240,
                180,
                None,
                None,
                Some(hinstance),
                None,
            )
        }
        .expect("окно-источник");

        let options = CropWindowOptions::new(
            Rect {
                x: 100,
                y: 100,
                w: 200,
                h: 150,
            },
            SourceRect {
                x: 0,
                y: 0,
                w: 200,
                h: 150,
            },
            "test",
        );
        let (crop, _events) = CropWindow::create(source, options).expect("окно куска");
        let content = crop.hwnd();
        let strip = crop.strip_hwnd();

        // Полоса выше содержимого, если, спускаясь от неё по z-порядку, мы
        // встречаем содержимое. Именно это решает, за что человек может взять
        // кусок: полоса под содержимым недостижима для мыши.
        let strip_is_above_content = || -> bool {
            let mut below = unsafe { GetWindow(strip, GW_HWNDNEXT) };
            while let Ok(hwnd) = below {
                if hwnd.0.is_null() {
                    return false;
                }
                if hwnd == content {
                    return true;
                }
                below = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
            }
            false
        };
        // Дать потоку окна разобрать сообщения о смене порядка.
        let settle = || std::thread::sleep(Duration::from_millis(150));

        settle();
        assert!(
            strip_is_above_content(),
            "у нового куска полоса лежит на его верхней кромке, а не под ним"
        );

        crop.set_always_on_top(true).expect("включить булавку");
        settle();
        assert!(
            strip_is_above_content(),
            "булавка включена — полоса обязана остаться над куском, иначе его нечем двигать"
        );

        crop.set_always_on_top(false).expect("выключить булавку");
        settle();
        assert!(
            strip_is_above_content(),
            "после выключения булавки полоса обязана вернуться над куском"
        );

        drop(crop);
        unsafe {
            let _ = DestroyWindow(source);
        }
    }

    /// Полоса обязана появляться по наведению и при включённой булавке.
    ///
    /// Замер, а не догадка: репорт «закрепляю — пропадает контрол бар»
    /// (2026-09-13) объясняли то порядком `SetWindowPos`, то полосой
    /// z-порядка, и обе версии проверка отвергла — Windows сама держит
    /// владеемое окно над владельцем. Значит проверять надо не порядок, а
    /// ПОКАЗ полосы.
    ///
    /// Наведение имитируется сообщением СВОЕМУ окну (`PostMessageW`), а не
    /// синтетическим вводом: курсор пользователя при этом не трогается вовсе.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 --lib strip_shows_on_hover -- --ignored"]
    fn strip_shows_on_hover_with_pin_on_and_off() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, IsWindowVisible, WM_MOUSEMOVE, WS_EX_TOOLWINDOW,
            WS_POPUP, WS_VISIBLE,
        };

        let hinstance = unsafe { GetModuleHandleW(None) }.expect("модуль").into();
        let source = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("resticker_crop_hover_test"),
                WS_POPUP | WS_VISIBLE,
                80,
                80,
                240,
                180,
                None,
                None,
                Some(hinstance),
                None,
            )
        }
        .expect("окно-источник");

        let options = CropWindowOptions::new(
            Rect {
                x: 100,
                y: 100,
                w: 200,
                h: 150,
            },
            SourceRect {
                x: 0,
                y: 0,
                w: 200,
                h: 150,
            },
            "test",
        );
        let (crop, _events) = CropWindow::create(source, options).expect("окно куска");
        let content = crop.hwnd();
        let strip = crop.strip_hwnd();

        let settle = || std::thread::sleep(Duration::from_millis(120));
        let hover = || {
            // Точка в середине клиентской области содержимого.
            let lp = LPARAM(((75_i32) << 16 | 100_i32) as isize);
            // SAFETY: своё окно; сообщение безопасно с любого потока.
            unsafe {
                let _ = PostMessageW(Some(content), WM_MOUSEMOVE, WPARAM(0), lp);
            }
        };
        let strip_visible = || unsafe { IsWindowVisible(strip) }.as_bool();

        settle();
        assert!(!strip_visible(), "без наведения полосы не видно");

        hover();
        settle();
        assert!(
            strip_visible(),
            "наведение на обычный кусок обязано показывать полосу"
        );

        crop.set_always_on_top(true).expect("включить булавку");
        settle();
        hover();
        settle();
        assert!(
            strip_visible(),
            "с включённой булавкой полоса обязана показываться так же — иначе кусок нечем двигать"
        );

        crop.set_always_on_top(false).expect("выключить булавку");
        settle();
        hover();
        settle();
        assert!(
            strip_visible(),
            "после выключения булавки полоса обязана возвращаться"
        );

        drop(crop);
        unsafe {
            let _ = DestroyWindow(source);
        }
    }

    /// Снять полосу куска в PNG — глазами координатора, а не на веру.
    ///
    /// Вид полосы уже один раз оказался не тем, что ожидал человек, потому
    /// что его никто не посмотрел. Тест не проверяет пиксели автоматически:
    /// он даёт файл, который можно открыть. Путь задаётся `CROP_STRIP_SNAP`.
    #[test]
    #[ignore = "снимок для глаз; запуск: CROP_STRIP_SNAP=<путь.png> cargo test -p rst-win32 --lib strip_snapshot -- --ignored"]
    fn strip_snapshot_for_review() {
        use std::time::Duration;
        use windows::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, SelectObject,
        };
        use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WM_MOUSEMOVE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
        };

        let Ok(out_path) = std::env::var("CROP_STRIP_SNAP") else {
            return;
        };
        let hinstance = unsafe { GetModuleHandleW(None) }.expect("модуль").into();
        let source = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("resticker_strip_snapshot"),
                WS_POPUP | WS_VISIBLE,
                80,
                80,
                240,
                180,
                None,
                None,
                Some(hinstance),
                None,
            )
        }
        .expect("окно-источник");
        let mut options = CropWindowOptions::new(
            Rect {
                x: 120,
                y: 120,
                w: 360,
                h: 220,
            },
            SourceRect {
                x: 0,
                y: 0,
                w: 360,
                h: 220,
            },
            "Orca",
        );
        options.always_on_top = std::env::var("CROP_STRIP_PIN").is_ok();
        let (crop, _events) = CropWindow::create(source, options).expect("окно куска");
        // Показать полосу: сообщение СВОЕМУ окну, курсор пользователя не трогаем.
        unsafe {
            let _ = PostMessageW(
                Some(crop.hwnd()),
                WM_MOUSEMOVE,
                WPARAM(0),
                LPARAM(((100_i32) << 16 | 100_i32) as isize),
            );
        }
        std::thread::sleep(Duration::from_millis(250));

        let strip = crop.strip_hwnd();
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(strip, &mut rect);
        }
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        assert!(w > 0 && h > 0, "полоса имеет размер");
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let pixels = unsafe {
            let mem = CreateCompatibleDC(None);
            let bitmap = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("DIB под снимок");
            let old = SelectObject(mem, bitmap.into());
            let _ = PrintWindow(strip, mem, PRINT_WINDOW_FLAGS(0));
            let raw = std::slice::from_raw_parts(bits.cast::<u8>(), (w * h * 4) as usize).to_vec();
            let _ = SelectObject(mem, old);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(mem);
            raw
        };
        // BGRA → RGBA, непрозрачно: полоса рисуется без альфы.
        let mut rgba = Vec::with_capacity(pixels.len());
        for px in pixels.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        image::save_buffer(
            &out_path,
            &rgba,
            w as u32,
            h as u32,
            image::ExtendedColorType::Rgba8,
        )
        .expect("снимок сохранён");

        drop(crop);
        unsafe {
            let _ = DestroyWindow(source);
        }
    }

    #[test]
    fn strip_zones_never_overlap_across_sizes() {
        // Три кнопки и зона перетаскивания обязаны оставаться раздельными при
        // любом размере полосы: раскладку рисует та же `strip_button_side`, и
        // рассинхрон «нарисовано одно, нажимается другое» уже был живым багом
        // (кнопки у правого края попадали в зону изменения размера).
        for width in [24_u32, 40, 64, 120, 320, 1920] {
            for height in [16_u32, 28, 40] {
                let button = strip_button_side(width, height);
                if button == 0 {
                    assert_eq!(
                        hit_test_strip(width, height, (width / 2) as i32, 1),
                        StripHit::Drag,
                        "полоса без места под кнопки остаётся целиком ручкой"
                    );
                    continue;
                }
                let seen: Vec<StripHit> = [width - 1, width - button - 1, 0]
                    .iter()
                    .map(|x| hit_test_strip(width, height, *x as i32, (height / 2) as i32))
                    .collect();
                assert_eq!(
                    seen,
                    vec![StripHit::Close, StripHit::Minimize, StripHit::Pin],
                    "справа закрыть и свернуть, слева булавка (width={width}, height={height})"
                );
            }
        }
    }

    #[test]
    fn physical_release_ends_drag_once_without_move() {
        let mut drag = DragTracker::new();
        drag.begin(Point { x: 120, y: 130 }, Point { x: 100, y: 100 });
        assert_eq!(
            drag.move_cursor(Point { x: 140, y: 150 }, false),
            Some(DragUpdate::Ended(Point { x: 100, y: 100 }))
        );
        assert_eq!(drag.end(), None);
    }

    #[test]
    fn button_up_ends_drag_once_and_capture_change_is_idempotent() {
        let mut drag = DragTracker::new();
        drag.begin(Point { x: 120, y: 130 }, Point { x: 100, y: 100 });
        assert_eq!(
            drag.move_cursor(Point { x: 160, y: 180 }, true),
            Some(DragUpdate::Moved(Point { x: 140, y: 150 }))
        );
        assert_eq!(
            drag.end(),
            Some(DragUpdate::Ended(Point { x: 140, y: 150 }))
        );
        assert_eq!(drag.end(), None);
    }

    #[test]
    fn drag_offset_is_preserved_for_both_axes() {
        let mut drag = DragTracker::new();
        drag.begin(Point { x: 143, y: 167 }, Point { x: 100, y: 120 });
        assert_eq!(
            drag.move_cursor(Point { x: 203, y: 217 }, true),
            Some(DragUpdate::Moved(Point { x: 160, y: 170 }))
        );
    }
}
