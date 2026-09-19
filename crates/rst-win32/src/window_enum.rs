//! Перечисление top-level окон с фильтром «реальных» окон (ROADMAP.md M4;
//! docs/M4_PREP_NOTES.md, §2; ARCHITECTURE.md, раздел 3.3).
//!
//! Наружу — платформенно-чистые данные (как `input::InputEvent` в M2):
//! `HWND` присутствует только как числовой ключ для кэша трекера, никаких
//! Win32-типов в публичном API. Инкрементальный кэш на WinEvent-хуках —
//! отдельный модуль (`WindowTracker`, M4_PREP_NOTES §3), здесь только
//! одноразовое перечисление.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, GetLastError, HWND, LPARAM, RECT, SetLastError, TRUE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LWIN, VK_MENU, VK_RWIN};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor,
    GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowPlacement,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, MINMAXINFO,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WINDOWPLACEMENT, WM_GETMINMAXINFO, WM_GETTEXT,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_THICKFRAME,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

use crate::window_icon;

/// Таймаут кросс-поточного чтения заголовка (мс), [`window_text`]. Верх
/// диапазона 200–500 мс: `SMTO_ABORTIFHUNG` и так возвращает сразу на
/// зависших потоках, таймаут ограничивает только «живые, но медленные»
/// окна — им 500 мс хватает вернуть заголовок (пустой заголовок деградирует
/// панель выбора M4, а `title_pattern` с ним просто не совпадает).
const TITLE_FETCH_TIMEOUT_MS: u32 = 500;

/// Прямоугольник окна в **физических** пикселях (маска перекрытия живёт в
/// физических — M4_PREP_NOTES §2.1; перевод в DIP — на стороне координатора).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl From<RECT> for WindowRect {
    fn from(r: RECT) -> Self {
        Self {
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
        }
    }
}

/// Иконка окна для панели выбора (M4_PREP_NOTES §7): RGBA-пиксели.
/// Заполняется отдельным срезом (`ExtractIconExW`/`SHGetFileInfoW`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Одно «реальное» top-level окно из перечисления [`enum_windows`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowInfo {
    /// `HWND` как числовой ключ (для кэша трекера M4); для Win32-вызовов
    /// снаружи не предназначен — окно может уже не существовать.
    pub hwnd: usize,
    /// Границы по `DWMWA_EXTENDED_FRAME_BOUNDS` (то, что видит DWM, — ровно
    /// то, что вырезает маска). У свёрнутых окон — мусорные, не использовать.
    pub rect: WindowRect,
    /// Процесс-владелец окна.
    pub pid: u32,
    /// Путь к exe процесса (пустой при недостатке прав на `OpenProcess` —
    /// окно всё равно перечисляется). Для сопоставления с
    /// `OverlapRule::process_name`.
    pub exe_path: PathBuf,
    /// Заголовок окна (для `title_pattern` и панели выбора).
    pub title: String,
    /// Класс окна (для панели и будущих расширений правил).
    pub class: String,
    /// Позиция в «сыром» перечислении (z-order сверху вниз): монотонно
    /// возрастает, пропуски — отфильтрованные окна.
    pub z_order: u32,
    /// Окно свёрнуто: не оклюдер (прямоугольник мусорный), но показывается
    /// в панели выбора (M4_PREP_NOTES §2.2).
    pub iconic: bool,
    /// Место под иконку окна (панель выбора M4): заполняется [`window_icon`]
    /// при перечислении; `None` — у окна нет exe-пути (protected process),
    /// извлечение не удалось или иконки нет (панель рисует плейсхолдер).
    pub icon: Option<WindowIcon>,
}

/// Кэш иконок по пути exe (M4, docs/M4_WINDOW_PICKER_DESIGN.md §6): окна
/// одного процесса делят одну иконку, а `SHGetFileInfoW` на первом
/// обращении к файлу стоит единицы миллисекунд — и то, и другое решается
/// «вытащить один раз на уникальный exe и хранить». Кэшируется и неудача
/// (`None`): exe мог быть удалён/стать недоступным — повторный пробой на
/// каждое перечисление не нужен. Процесс-wide статик: трекер окон (и его
/// полные перечисления) живёт на своём потоке, а иконки не зависят ни от
/// времени, ни от окна — вытеснение не требуется (число уникальных exe за
/// сессию — десятки, растр 16×16 ≈ 1 КБ).
static ICON_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<WindowIcon>>>> = OnceLock::new();

/// Иконка процесса по пути exe: кэш, иначе извлечение + запись в кэш
/// (успех и неудача одинаково). Пустой путь (protected process, сбой
/// `OpenProcess`) — `None` без обращения к кэшу.
fn window_icon(exe_path: &Path) -> Option<WindowIcon> {
    if exe_path.as_os_str().is_empty() {
        return None;
    }
    let cache = ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        // Отравленный мьютекс (паника во время извлечения) — продолжаем
        // работать со старым содержимым, иконки не критичны.
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(icon) = cache.get(exe_path) {
        return icon.clone();
    }
    let icon = window_icon::extract_icon(exe_path);
    cache.insert(exe_path.to_path_buf(), icon.clone());
    icon
}

/// Снимок признаков окна для чистого фильтра (собирается Win32-вызовами
/// в [`collect_window`]; отдельно — чтобы фильтр тестировать без окон).
#[derive(Debug, Clone, Copy, Default)]
struct WindowFlags {
    visible: bool,
    cloaked: bool,
    /// `GetAncestor(hwnd, GA_ROOT) == hwnd` (top-level, не child).
    is_root: bool,
    no_activate: bool,
    tool_window: bool,
    app_window: bool,
    has_owner: bool,
    iconic: bool,
}

/// Фильтр «реальных окон» дословно по ARCHITECTURE.md 3.3 и
/// M4_PREP_NOTES §2.2. `iconic` отбраковкой не является — лишь отметка.
fn is_real_window(f: &WindowFlags) -> bool {
    if !f.visible {
        return false;
    }
    if f.cloaked {
        return false;
    }
    if !f.is_root {
        return false;
    }
    if f.no_activate {
        return false;
    }
    if f.tool_window && !f.app_window {
        return false;
    }
    if f.has_owner && !f.app_window {
        return false;
    }
    true
}

/// Окно можно менять в размерах — у него есть рамка ресайза
/// (`WS_THICKFRAME`, она же `WS_SIZEBOX`).
///
/// Тот же признак, по которому решает сама Windows: снап работает только над
/// окнами с этим стилем, а фиксированные диалоги остаются как есть. Поэтому
/// проверка нужна перед тем, как двигать ЧУЖОЕ незакреплённое окно: без неё
/// мы бы раз за разом просили `SetWindowPos` изменить размер окна, которое
/// его изменить не может, и получали бы тихий отказ на каждом снимке.
///
/// Мёртвый `hwnd` — `false`: `GetWindowLongW` вернёт 0, и стиля в нём нет.
pub fn is_resizable(hwnd: usize) -> bool {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: GetWindowLongW принимает любой HWND, в т.ч. уже уничтоженный,
    // и возвращает 0 вместо падения.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    style & WS_THICKFRAME.0 != 0
}

/// Минимальный размер окна в **физических** пикселях DWM-границ
/// (`DWMWA_EXTENDED_FRAME_BOUNDS`) — той же системе координат, в которой
/// живёт раскладка (`WindowRect`/`WindowInfo::rect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowMinSize {
    pub w: i32,
    pub h: i32,
}

/// Таймаут кросс-поточного запроса минимума (мс), [`min_window_size`].
///
/// Выбран ЗАМЕРОМ, а не на глаз — `spike/min_size_probe` (2026-08-26):
/// * живые приложения пользователя (Chrome-семейство, Spotify, Discord,
///   Steam, Nemora, Проводник, Настройки) отвечают на `WM_GETMINMAXINFO`
///   за 13–242 мкс (типично <100 мкс); собственные окна пробы с помпом —
///   до 1.6 мс (там доминирует гранулярность помпа, не приложение);
/// * зависшее окно `SMTO_ABORTIFHUNG` НЕ обрывает сразу: замер показал
///   ожидание ровно в течение таймаута + ~5–14 мс накладных (таймауты
///   10/50/200/500/2000 мс — фактическое ожидание 24.6/62/204/513/2000 мс,
///   ошибка 1460 ERROR_TIMEOUT).
///
/// Итог: 25 мс — 15-кратный запас над худшим измеренным живым ответом
/// (1.6 мс) при неощутимой цене зависшего окна (вызов вернётся через
/// ~30–40 мс, и раскладка честно пойдёт без минимума).
pub const MIN_SIZE_FETCH_TIMEOUT_MS: u32 = 25;

/// Смещение `GetWindowRect` → DWM-границы (dx, dy, dw, dh) — те же четыре
/// числа, что считает `WindowPins::set_dwm_bounds` (window_pin.rs): у окон
/// Win11 между системами ~7 px невидимых полей ресайза с боков и снизу, и
/// складывать пространства напрямую нельзя. Смещение от позиции не зависит
/// (метрики рамки постоянны), поэтому достаточно текущего состояния окна.
///
/// `(0, 0, 0, 0)` — окно свёрнуто или система не отдала границы: перевода
/// нет, вызывающий получает «сырое» значение в GetWindowRect-пространстве.
pub fn dwm_frame_offset(hwnd: HWND) -> (i32, i32, i32, i32) {
    let mut gwr = RECT::default();
    // SAFETY: GetWindowRect — чтение экранного прямоугольника, безопасно
    // и для чужих, и для мёртвых окон (вернёт ошибку).
    let gwr_ok = unsafe { GetWindowRect(hwnd, &mut gwr) }.is_ok();
    let dwm = extended_frame_bounds(hwnd);
    if !gwr_ok || dwm.w == 0 || dwm.h == 0 {
        return (0, 0, 0, 0);
    }
    (
        dwm.x - gwr.left,
        dwm.y - gwr.top,
        dwm.w - (gwr.right - gwr.left),
        dwm.h - (gwr.bottom - gwr.top),
    )
}

/// Перевести заявленный приложением минимум (GetWindowRect-пространство)
/// в DWM-пространство: прибавить рамку `dw`/`dh`. Отрицательный результат
/// (приложение не объявляет минимума, а рамка «съедает» его в минус)
/// схлопывается в ноль — «минимума нет».
fn to_dwm_min(pt_x: i32, pt_y: i32, offset: (i32, i32, i32, i32)) -> WindowMinSize {
    WindowMinSize {
        w: (pt_x + offset.2).max(0),
        h: (pt_y + offset.3).max(0),
    }
}

/// Минимальный размер ЧУЖОГО окна — до которого его реально ужимает Windows
/// (`WM_GETMINMAXINFO`, поле `ptMinTrackSize`), в DWM-координатах
/// [`WindowInfo::rect`].
///
/// Зачем: некоторые приложения (Spotify, Discord, OBS, Steam — живой репорт
/// 2026-08-26 со скриншотом) не дают ужать окно ниже собственного минимума;
/// если слот раскладки меньше этого предела, окно молча остаётся крупнее
/// и НАЛЕЗАЕТ НА СОСЕДА. Число отсюда — то, чем раскладка обязана
/// ограничить слот заранее, а не по факту (`check_layout_discrepancy` в
/// window_pin.rs ловит уже случившееся).
///
/// Почему `SendMessageTimeoutW`, а не `SendMessageW`: запрос идёт в ЧУЖОЙ
/// процесс, и блокирующий вызов на зависшем приложении повис бы вместе
/// с ним — а это координаторский поток, на котором живёт весь интерфейс.
/// `SMTO_ABORTIFHUNG` + [`MIN_SIZE_FETCH_TIMEOUT_MS`] ограничивают ожидание
/// сверху (замер пробы: зависшее окно держит вызов ровно таймаут). Окно, не
/// ответившее в срок, — НЕ ошибка: `None`, раскладка обойдётся без минимума
/// (и наверстает его discrepancy-проверкой, если окно всё-таки не влезет).
///
/// Ноль в ответе (`w == 0 && h == 0`): приложение НЕ объявляет минимум —
/// обработчик не переопределяет системные значения. Это не значит, что окно
/// можно ужать до нуля: для рамковых окон остаётся системный пол
/// `SM_CXMINTRACK`/`SM_CYMINTRACK` (замер на своём окне: 136x60, проба
/// 2026-08-26), который в числе не отражён — пол маленький и раскладку не
/// ломает, но и ноль не следует трактовать как «свободно».
///
/// Свёрнутое окно: `WM_GETMINMAXINFO` доходит, но DWM не отдаёт границы —
/// перевод в DWM-пространство невозможен, возвращается «сырое» значение
/// в GetWindowRect-пространстве (свёрнутые окна раскладка и так не строит).
pub fn min_window_size(hwnd: usize) -> Option<WindowMinSize> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    let mut mmi = MINMAXINFO::default();
    let mut delivered: usize = 0;
    // SAFETY: SetLastError — потоковый регистр ошибки; сброс обязателен:
    // SendMessageTimeoutW возвращает результат СООБЩЕНИЯ, а для
    // WM_GETMINMAXINFO это 0 (данные — в структуре), поэтому «доставлено»
    // от «таймаута» отличает только код ошибки.
    unsafe {
        SetLastError(ERROR_SUCCESS);
    }
    // SAFETY: hwnd проверен IsWindow выше; mmi — валидный буфер под структуру
    // (система заполняет её до вызова обработчика); SMTO_ABORTIFHUNG обрывает
    // зависшие потоки, таймаут ограничивает живые, но занятые.
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM((&raw mut mmi) as isize),
            SMTO_ABORTIFHUNG,
            MIN_SIZE_FETCH_TIMEOUT_MS,
            Some(&mut delivered),
        )
    };
    // SAFETY: GetLastError — потоковый регистр ошибки.
    let err = unsafe { GetLastError() };
    // Ненулевой результат — сообщение доставлено (обработчик что-то вернул);
    // нулевой — доставлено с результатом 0 (норма для WM_GETMINMAXINFO) или
    // таймаут — различает код ошибки, сброшенный выше.
    let ok = result.0 != 0 || err == ERROR_SUCCESS;
    if !ok {
        return None;
    }
    let offset = dwm_frame_offset(hwnd);
    Some(to_dwm_min(
        mmi.ptMinTrackSize.x,
        mmi.ptMinTrackSize.y,
        offset,
    ))
}

/// Контекст колбэка `EnumWindows`: `raw_index` считает **все** окна из
/// сырого перечисления (даже отфильтрованные) — так `WindowInfo::z_order`
/// остаётся монотонным с пропусками, как задокументировано на поле.
struct EnumCtx<'a> {
    out: Vec<WindowInfo>,
    raw_index: u32,
    process_cache: &'a mut HashMap<u32, PathBuf>,
}

/// Перечислить все «реальные» top-level окна одним снимком (одноразовое
/// перечисление; инкрементальный кэш на WinEvent-хуках — отдельный модуль,
/// M4_PREP_NOTES §3).
pub fn enumerate() -> Vec<WindowInfo> {
    let mut process_cache = HashMap::new();
    enumerate_with_process_cache(&mut process_cache)
}

/// Полное перечисление с кэшем `pid → exe_path`, принадлежащим вызывающему
/// потоку. Кэш позволяет не делать `OpenProcess`/`QueryFullProcessImageNameW`
/// повторно для каждого окна одного процесса при каждом полном снимке.
pub(crate) fn enumerate_with_process_cache(
    process_cache: &mut HashMap<u32, PathBuf>,
) -> Vec<WindowInfo> {
    let mut ctx = EnumCtx {
        out: Vec::new(),
        raw_index: 0,
        process_cache,
    };
    // SAFETY: `ctx` живёт весь вызов и не разделяется; колбэк — синхронный,
    // на этом же потоке, указатель действует только внутри EnumWindows.
    unsafe {
        let _ = EnumWindows(Some(enum_windows_proc), LPARAM(&raw mut ctx as isize));
    }
    ctx.out
}

extern "system" fn enum_windows_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` — &mut EnumCtx из `enumerate`, живой на всё время вызова
    // EnumWindows; колбэк синхронный, гонок нет.
    let ctx = unsafe { &mut *(data.0 as *mut EnumCtx) };
    let z_order = ctx.raw_index;
    ctx.raw_index += 1;
    if let Some(info) = collect_window_with_process_cache(hwnd, z_order, ctx.process_cache) {
        ctx.out.push(info);
    }
    TRUE
}

/// Только дешёвая часть полного перечисления: hwnd реальных окон и их сырой
/// индекс в z-order. Заголовок, exe, иконка и DWM-границы здесь не читаются.
/// `None` означает отказ самого `EnumWindows`, поэтому вызывающий должен
/// считать кэш устаревшим и выполнить полное перечисление.
pub(crate) fn enumerate_real_window_order() -> Option<Vec<(usize, u32)>> {
    let mut ctx = OrderCtx {
        out: Vec::new(),
        raw_index: 0,
    };
    // SAFETY: `ctx` живёт весь вызов и не разделяется; колбэк синхронный,
    // на этом же потоке, указатель действует только внутри EnumWindows.
    let ok = unsafe {
        EnumWindows(
            Some(enum_real_window_order_proc),
            LPARAM(&raw mut ctx as isize),
        )
    }
    .is_ok();
    ok.then_some(ctx.out)
}

struct OrderCtx {
    out: Vec<(usize, u32)>,
    raw_index: u32,
}

extern "system" fn enum_real_window_order_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` — &mut OrderCtx из `enumerate_real_window_order`, живой
    // на всё время вызова EnumWindows; колбэк синхронный, гонок нет.
    let ctx = unsafe { &mut *(data.0 as *mut OrderCtx) };
    let z_order = ctx.raw_index;
    ctx.raw_index += 1;
    if is_real_window_handle(hwnd) {
        ctx.out.push((hwnd.0 as usize, z_order));
    }
    TRUE
}

/// Собрать [`WindowInfo`] для `hwnd` с полными данными, если оно проходит
/// фильтр [`is_real_window`]; иначе `None`.
pub(crate) fn collect_window_with_process_cache(
    hwnd: HWND,
    z_order: u32,
    process_cache: &mut HashMap<u32, PathBuf>,
) -> Option<WindowInfo> {
    let flags = window_flags(hwnd);
    if !is_real_window(&flags) {
        // Диагностика на живой репорт пользователя («список окон в панели
        // короче, чем реально открытых окон») — почему именно окно
        // отфильтровано, без похода на диск/сеть: title/class дёшевы, уже
        // читаются ниже для принятых окон, здесь читаем только при отказе.
        tracing::debug!(
            hwnd = hwnd.0 as usize,
            title = %window_text(hwnd),
            class = %window_class(hwnd),
            ?flags,
            "is_real_window отбраковал окно"
        );
        return None;
    }
    let (pid, exe_path) = process_info_cached(hwnd, process_cache);
    let icon = window_icon(&exe_path);
    Some(WindowInfo {
        hwnd: hwnd.0 as usize,
        rect: extended_frame_bounds(hwnd),
        pid,
        exe_path,
        title: window_text(hwnd),
        class: window_class(hwnd),
        z_order,
        iconic: flags.iconic,
        icon,
    })
}

/// Тот же фильтр, что в полном `collect_window`, но без дорогих данных окна.
/// Нужен только для проверки, что быстрый z-order-снимок содержит ровно тот
/// же набор окон, что и полное перечисление.
fn is_real_window_handle(hwnd: HWND) -> bool {
    is_real_window(&window_flags(hwnd))
}

/// Собрать [`WindowFlags`] Win32-вызовами (ARCHITECTURE.md 3.3, M4_PREP_NOTES §2.2).
fn window_flags(hwnd: HWND) -> WindowFlags {
    // SAFETY: hwnd приходит из EnumWindows, действительно на время колбэка.
    unsafe {
        let visible = IsWindowVisible(hwnd).as_bool();
        let iconic = IsIconic(hwnd).as_bool();
        let is_root = GetAncestor(hwnd, GA_ROOT) == hwnd;
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let no_activate = ex_style & WS_EX_NOACTIVATE.0 != 0;
        let tool_window = ex_style & WS_EX_TOOLWINDOW.0 != 0;
        let app_window = ex_style & WS_EX_APPWINDOW.0 != 0;
        let has_owner = GetWindow(hwnd, GW_OWNER)
            .map(|owner| !owner.0.is_null())
            .unwrap_or(false);
        let mut cloaked: u32 = 0;
        let cloaked_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
        .is_ok();
        WindowFlags {
            visible,
            cloaked: cloaked_ok && cloaked != 0,
            is_root,
            no_activate,
            tool_window,
            app_window,
            has_owner,
            iconic,
        }
    }
}

/// Границы окна по DWM (то, что реально рисует композитор — ровно то, что
/// вырежет маска перекрытия, M4_PREP_NOTES §2.1). При отказе API — нулевой
/// прямоугольник (свёрнутые окна и так помечены `iconic`, не оклюдеры).
/// `pub(crate)`: переиспользуется `window_tracker` для точечного обновления
/// rect одного окна на `EVENT_OBJECT_LOCATIONCHANGE`/`MINIMIZEEND`, не через
/// полное `enumerate()` (M4_WINDOW_TRACKER_DESIGN.md §4).
/// Классы окон, которые шелл показывает НА ВРЕМЯ переключения или своего
/// меню: переключатель Alt+Tab и Win+Tab, меню Пуск, поиск, панель задач.
///
/// Пока такое окно на переднем плане, «активного приложения» фактически нет:
/// пользователь ещё выбирает. Любое вмешательство в чужие окна в этот момент
/// ломает сам переключатель — см. [`shell_switching`].
/// Классы окон, которые шелл показывает НА ВРЕМЯ переключения или своего
/// меню: переключатель Alt+Tab и Win+Tab, меню снап-раскладок Windows 11
/// (Snap Layouts flyout), меню переполнения трея, меню Пуск, поиск, панель задач.
///
/// Пока такое окно на переднем плане или видимо на экране, «активного приложения»
/// фактически нет: пользователь выбирает действие в системном UI. Любое
/// вмешательство в чужие окна в этот момент ломает сам переключатель или закрывает
/// всплывающее меню — см. [`shell_switching`] и [`is_shell_transient_visible`].
///
/// Измерено на Windows 11 Build 26200 (2026-08-26):
/// - `XamlExplorerHostIslandWindow`: хост XAML-островков в explorer.exe, используется
///   для Alt+Tab, Win+Tab и всплывающего меню снап-раскладок (поток `SnapFlyoutHost Thread`
///   в `twinui.pcshell.dll`).
/// - `TopLevelWindowForOverflowXamlIsland`: всплывающее меню области уведомлений (трея).
/// - `SnapFlyout`: резервный класс всплывающего меню снап-раскладок.
/// - `Windows.UI.Core.CoreWindow`: меню Пуск, поиск, системные панели.
/// - `MultitaskingViewFrame`: переключатель задач Windows 10.
/// - `TaskSwitcherWnd`, `TaskSwitcherOverlayWnd`: классический переключатель Alt+Tab.
/// - `ForegroundStaging`: промежуточное окно переключения.
/// - `Shell_TrayWnd`, `Shell_SecondaryTrayWnd`: панели задач.
pub const SHELL_TRANSIENT_CLASSES: [&str; 10] = [
    "MultitaskingViewFrame",               // Win10 Task View / Alt+Tab
    "XamlExplorerHostIslandWindow", // Win11 Alt+Tab, Win+Tab и Snap Layouts Flyout (SnapFlyoutHost)
    "TopLevelWindowForOverflowXamlIsland", // Win11 меню переполнения трея
    "SnapFlyout",                   // Резервный класс меню снап-раскладок
    "TaskSwitcherWnd",              // классический Alt+Tab
    "TaskSwitcherOverlayWnd",       // его оверлей
    "ForegroundStaging",            // промежуточное окно переключения
    "Windows.UI.Core.CoreWindow",   // меню Пуск, поиск
    "Shell_TrayWnd",                // панель задач
    "Shell_SecondaryTrayWnd",       // панель задач на втором мониторе
];

/// Проверяет, принадлежит ли имя класса к системным всплывающим/переключающим классам шелла.
pub fn is_shell_transient_class(class_name: &str) -> bool {
    SHELL_TRANSIENT_CLASSES
        .iter()
        .any(|known| class_name.eq_ignore_ascii_case(known))
}

/// Проверяет, показано ли прямо сейчас системное всплывающее окно
/// шелла (Snap Layouts flyout, меню переполнения трея, Alt+Tab и др.).
///
/// Зачем: всплывающее меню снап-раскладок Windows 11 (`XamlExplorerHostIslandWindow`,
/// `SnapFlyout`) не всегда забирает фокус ввода (GetForegroundWindow остаётся на
/// окне с кнопкой разворачивания). Без прямой проверки видимости окна шелла
/// закреплённое окно с `WS_EX_TOPMOST` перекрывает всплывающее системное меню
/// (живой репорт 2026-08-26 со скриншотом).
///
/// Если окно не найдено или система другой сборки — безопасно возвращает `false`
/// без паник и без побочных эффектов.
pub fn is_shell_transient_visible() -> bool {
    for &known in &SHELL_TRANSIENT_CLASSES {
        // Пропускаем панели задач — они постоянные элементы интерфейса, а не всплывающие меню
        if known.eq_ignore_ascii_case("Shell_TrayWnd")
            || known.eq_ignore_ascii_case("Shell_SecondaryTrayWnd")
        {
            continue;
        }

        let mut curr_hwnd = HWND::default();
        let class_wide: Vec<u16> = known.encode_utf16().chain(std::iter::once(0)).collect();
        let pcwstr = PCWSTR(class_wide.as_ptr());

        while let Ok(hwnd) = unsafe { FindWindowExW(None, Some(curr_hwnd), pcwstr, None) } {
            if hwnd.0.is_null() {
                break;
            }
            curr_hwnd = hwnd;
            unsafe {
                if !IsWindow(Some(hwnd)).as_bool()
                    || !IsWindowVisible(hwnd).as_bool()
                    || IsIconic(hwnd).as_bool()
                {
                    continue;
                }
                let mut cloaked: u32 = 0;
                let _ = DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    (&raw mut cloaked).cast(),
                    size_of::<u32>() as u32,
                );
                if cloaked != 0 {
                    continue;
                }
                let rect = extended_frame_bounds(hwnd);
                if rect.w > 0 && rect.h > 0 {
                    return true;
                }
            }
        }
    }
    false
}

/// Пользователь ПРЯМО СЕЙЧАС взаимодействует со системным UI шелла
/// (Alt+Tab, Win+Tab, меню снап-раскладок Snap Layouts, меню Пуск, клик по панели задач).
///
/// Зачем: правила «показывать только на этих окнах» решают судьбу
/// закреплённого окна по активному окну и при неподходящем активном окне
/// сворачивают его. Во время Alt+Tab или показа меню снап-раскладок активным
/// становится либо сам переключатель, либо фокус остаётся у приложения, пока
/// поверх висит системный XAML-островок. Любая смена z-order / сокрытие
/// в этот момент ломает переключатель или закрывает меню под рукой пользователя
/// (критический репорт 2026-08-22 и 2026-08-26).
///
/// Три независимых признака, любой достаточен:
/// * зажат Alt или Win — то есть комбинация переключения ещё удерживается;
/// * переднее окно принадлежит шеллу ([`SHELL_TRANSIENT_CLASSES`]);
/// * всплывающее окно шелла (Snap Layouts flyout, меню переполнения) видимо
///   прямо сейчас ([`is_shell_transient_visible`]).
///
/// Пока это верно, координатор обязан НИЧЕГО не делать с чужими окнами:
/// решение примется само, когда пользователь закончит взаимодействие с шеллом.
pub fn shell_switching() -> bool {
    // 1. Зажаты клавиши переключения (Alt или Win).
    // SAFETY: GetAsyncKeyState — потокобезопасное чтение состояния ввода.
    let keys_held = unsafe {
        GetAsyncKeyState(VK_MENU.0 as i32) < 0
            || GetAsyncKeyState(VK_LWIN.0 as i32) < 0
            || GetAsyncKeyState(VK_RWIN.0 as i32) < 0
    };
    if keys_held {
        return true;
    }

    // 2. Переднее окно принадлежит шеллу.
    // SAFETY: GetForegroundWindow — чтение состояния десктопа.
    let fg = unsafe { GetForegroundWindow() };
    if !fg.0.is_null() {
        let class = window_class(fg);
        if is_shell_transient_class(&class) {
            return true;
        }
    }

    // 3. Системное всплывающее окно шелла (Snap Layouts flyout и др.) видимо на экране.
    is_shell_transient_visible()
}

/// Прямоугольник окна ПРЯМО СЕЙЧАС, мимо кэша трекера (те же
/// DWM-координаты `DWMWA_EXTENDED_FRAME_BOUNDS`, что у [`WindowInfo::rect`]).
///
/// Зачем при живом трекере: снимок трекера дебаунсится (16 мс) и приходит
/// после полного перечисления, поэтому во время ПЕРЕТАСКИВАНИЯ окна
/// пользователем он отстаёт — нарисованная по нему рамка/бейдж/панель
/// «отлетают» от окна (репорт 2026-08-21). Для нескольких закреплённых окон
/// прямой опрос DWM стоит единицы микросекунд на окно и снимает отставание.
///
/// `None` — окна нет, оно скрыто, свёрнуто или DWM не отдал границы:
/// вызывающий в этом случае честно откатывается к снимку.
pub fn live_rect(hwnd: usize) -> Option<WindowRect> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: все три предиката безопасны для чужих и мёртвых хэндлов.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool()
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
        {
            return None;
        }
    }
    let rect = extended_frame_bounds(hwnd);
    (rect.w != 0 && rect.h != 0).then_some(rect)
}

/// Прямоугольник, в который окно вернётся при разворачивании
/// (`WINDOWPLACEMENT::rcNormalPosition`) — единственный способ узнать место
/// СВЁРНУТОГО окна: `DWMWA_EXTENDED_FRAME_BOUNDS` у него отдаёт мусор
/// (`-32000, -32000`), а ждать реального разворота, чтобы прочитать rect,
/// значит гонку с чужой очередью сообщений.
///
/// Нужен закреплению свёрнутого окна (список выбора окна, `window_pick_list`):
/// по этому прямоугольнику определяется монитор и считается кламп ДО того,
/// как окно развернут.
///
/// `None` — окна нет или Windows не отдала placement. Координаты — экранные,
/// как у [`live_rect`], но БЕЗ поправки на теневую рамку DWM: placement
/// хранит оконный rect. Разница — единицы пикселей, и для выбора монитора и
/// клампа она роли не играет.
pub fn restored_rect(hwnd: usize) -> Option<WindowRect> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: буфер заполнен (length обязателен); GetWindowPlacement
    // безопасен для чужого и мёртвого хэндла — вернёт ошибку.
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.ok()?;
    let rect: WindowRect = placement.rcNormalPosition.into();
    (rect.w > 0 && rect.h > 0).then_some(rect)
}

pub(crate) fn extended_frame_bounds(hwnd: HWND) -> WindowRect {
    let mut rect = RECT::default();
    // SAFETY: `rect` — валидный буфер под RECT, hwnd — из EnumWindows.
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if ok {
        rect.into()
    } else {
        WindowRect::default()
    }
}

/// HWND окна переднего плана (`GetForegroundWindow`) как числовой ключ в
/// том же формате, что [`WindowInfo::hwnd`] — для хоткей-пина
/// «закрепить/открепить сфокусированное окно» (SPEC.md, «Закрепление
/// окна»). `None` — фокуса нет вовсе (редко: между переключениями, пустой
/// десктоп) — пинить нечего.
pub fn foreground_hwnd() -> Option<usize> {
    // SAFETY: GetForegroundWindow — чистый запрос состояния десктопа,
    // состояния не меняет, безопасен с любого потока.
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.0.is_null()).then_some(hwnd.0 as usize)
}

/// Монитор, на котором лежит окно (`MonitorFromWindow`), как числовой
/// ключ — сравнивать мониторы двух окон можно, ничего не зная о геометрии.
///
/// `None` — окно ни на одном мониторе: свёрнутое окно живёт в координатах
/// вроде (-32000, -32000), и `MONITOR_DEFAULTTONULL` честно отвечает
/// «нигде» вместо того, чтобы приписать его ближайшему экрану. Вызывающему
/// это и нужно: «монитор неизвестен» — не то же самое, что «монитор тот
/// же».
pub fn monitor_of(hwnd: usize) -> Option<isize> {
    // SAFETY: чистый запрос состояния десктопа для чужого HWND; невалидный
    // или свёрнутый дескриптор даёт нулевой HMONITOR, а не UB.
    let monitor =
        unsafe { MonitorFromWindow(HWND(hwnd as *mut core::ffi::c_void), MONITOR_DEFAULTTONULL) };
    (!monitor.0.is_null()).then_some(monitor.0 as isize)
}

/// Заголовок окна. Пустая строка — нет заголовка, сбой API или таймаут
/// (не критично, окно остаётся в списке).
///
/// Заголовок чужого процесса читается кросс-поточным `WM_GETTEXT` (как и
/// `GetWindowTextW` под капотом): без ограничения зависшее окно держит
/// вызов до системного таймаута (~5 с), а это перечисление стоит на пути
/// маски M4 и цикла координатора (сообщения обрабатываются
/// последовательно, ADR-006). `SendMessageTimeoutW` с `SMTO_ABORTIFHUNG` и
/// [`TITLE_FETCH_TIMEOUT_MS`] обрывают зависшие потоки сразу, а живые — не
/// дольше таймаута.
fn window_text(hwnd: HWND) -> String {
    // Один обход с фиксированным буфером вместо пары GetWindowTextLengthW +
    // GetWindowTextW (два кросс-поточных обхода при зависании в два раза
    // дольше); размер буфера — как у exe-пути в `process_info` (1024).
    let mut buf = [0u16; 1024];
    let mut copied: usize = 0;
    // SAFETY: hwnd — из EnumWindows, действительно на время вызова; buf —
    // буфер достаточного размера (WM_GETTEXT копирует максимум len-1
    // символов и нуль-терминатор); copied — валидный out-параметр.
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETTEXT,
            WPARAM(buf.len()),
            LPARAM(buf.as_mut_ptr() as isize),
            SMTO_ABORTIFHUNG,
            TITLE_FETCH_TIMEOUT_MS,
            Some(&mut copied),
        )
    };
    if result.0 == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied.min(buf.len())])
}

/// Класс окна (для панели выбора и будущих правил перекрытия).
fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: hwnd — из EnumWindows; buf — буфер фиксированного размера.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Вариант `process_info`, переиспользующий путь exe для всех окон одного PID.
/// Пустой путь тоже кэшируется: повторный отказ UIPI не должен повторять
/// `OpenProcess` на каждом окне этого процесса.
fn process_info_cached(hwnd: HWND, process_cache: &mut HashMap<u32, PathBuf>) -> (u32, PathBuf) {
    let mut pid: u32 = 0;
    // SAFETY: hwnd — из EnumWindows; pid — валидный out-параметр.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    process_info_for_pid(pid, process_cache)
}

fn process_info_for_pid(pid: u32, process_cache: &mut HashMap<u32, PathBuf>) -> (u32, PathBuf) {
    if pid == 0 {
        return (0, PathBuf::new());
    }
    if let Some(path) = process_cache.get(&pid) {
        return (pid, path.clone());
    }
    // SAFETY: pid — только что полученный от системы; хэндл процесса
    // закрывается ниже в любом случае (в т.ч. при ошибке — CloseHandle
    // безопасен для валидного хэндла).
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    let Ok(process) = process else {
        process_cache.insert(pid, PathBuf::new());
        return (pid, PathBuf::new());
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: process — только что открытый хэндл; buf/len — валидный
    // выходной буфер и его размер.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    // SAFETY: process — валидный хэндл, открытый выше этим же вызовом.
    unsafe {
        let _ = CloseHandle(process);
    }
    let path = if ok {
        PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        PathBuf::new()
    };
    process_cache.insert(pid, path.clone());
    (pid, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, LRESULT, POINT};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
        PeekMessageW, RegisterClassExW, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
        TranslateMessage, WM_GETMINMAXINFO, WNDCLASSEXW, WS_OVERLAPPED, WS_THICKFRAME, WS_VISIBLE,
    };
    use windows::core::w;

    /// HWND не `Send` (сырой указатель) — обёртка для пересылки между
    /// потоками, как в window_tracker.rs/tray.rs/overlay.rs.
    struct SendHwnd(HWND);

    unsafe impl Send for SendHwnd {}

    unsafe extern "system" fn test_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Окно, чей поток НЕ пампит сообщения — в отличие от `RealWindow` в
    /// window_tracker.rs (ему цикл обязателен, чтобы хуки его увидели).
    /// Снаружи такое окно выглядит «зависшим», и кросс-поточное чтение
    /// заголовка не должно блокироваться на нём.
    struct HungWindow {
        hwnd: HWND,
        thread: Option<JoinHandle<()>>,
        kill: Option<mpsc::Sender<()>>,
    }

    impl HungWindow {
        fn create() -> Self {
            let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
            let (kill_tx, kill_rx) = mpsc::channel::<()>();
            let thread = thread::spawn(move || {
                // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
                let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(test_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: w!("resticker_window_enum_test"),
                    ..Default::default()
                };
                // SAFETY: wc заполнена корректно; повторная регистрация
                // (параллельные тесты) — не ошибка.
                if unsafe { RegisterClassExW(&wc) } == 0 {
                    let err = unsafe { GetLastError() };
                    assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
                }
                // SAFETY: все аргументы — валидные константы/только что
                // зарегистрированный класс; видимость для window_text не важна.
                let hwnd = unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("resticker_window_enum_test"),
                        w!("resticker window_enum test"),
                        WS_OVERLAPPED,
                        0,
                        0,
                        200,
                        150,
                        None,
                        None,
                        Some(hinstance.into()),
                        None,
                    )
                }
                .expect("создание тестового окна");
                ready_tx.send(SendHwnd(hwnd)).expect("получатель ещё жив");
                // Намеренно НЕ пампим: поток блокируется, пока тест не
                // попросит завершиться.
                let _ = kill_rx.recv();
                // SAFETY: hwnd создано этим потоком; DestroyWindow на своём
                // потоке — корректный способ закрыть окно без цикла сообщений.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            });
            let hwnd = ready_rx.recv().expect("поток тестового окна не упал").0;
            Self {
                hwnd,
                thread: Some(thread),
                kill: Some(kill_tx),
            }
        }
    }

    impl Drop for HungWindow {
        fn drop(&mut self) {
            if let Some(k) = self.kill.take() {
                let _ = k.send(());
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// Wndproc с собственным `WM_GETMINMAXINFO` (минимум 400x300) — как у
    /// приложений, которые не дают ужать окно (Spotify/Discord/OBS).
    unsafe extern "system" fn minmax_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_GETMINMAXINFO {
            // SAFETY: lparam — указатель на MINMAXINFO, заполненный системой.
            let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            mmi.ptMinTrackSize = POINT { x: 400, y: 300 };
            return LRESULT(0);
        }
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// «Чужое» окно с минимумом: живёт на своём помп-потоке и отвечает на
    /// `WM_GETMINMAXINFO` (400x300). Снаружи — ровно та ситуация, в которой
    /// живёт координатор (запрос с другого потока в чужой процесс).
    struct MinMaxWindow {
        hwnd: HWND,
        thread: Option<JoinHandle<()>>,
        kill: Option<mpsc::Sender<()>>,
    }

    impl MinMaxWindow {
        fn create() -> Self {
            let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
            let (kill_tx, kill_rx) = mpsc::channel::<()>();
            let thread = thread::spawn(move || {
                // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
                let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(minmax_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: w!("resticker_window_enum_minmax"),
                    ..Default::default()
                };
                // SAFETY: wc заполнена корректно; повторная регистрация
                // (параллельные тесты) — не ошибка.
                if unsafe { RegisterClassExW(&wc) } == 0 {
                    let err = unsafe { GetLastError() };
                    assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
                }
                // SAFETY: все аргументы — валидные константы/только что
                // зарегистрированный класс.
                let hwnd = unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("resticker_window_enum_minmax"),
                        w!("minmax test"),
                        WS_OVERLAPPED | WS_THICKFRAME | WS_VISIBLE,
                        100,
                        100,
                        700,
                        500,
                        None,
                        None,
                        Some(hinstance.into()),
                        None,
                    )
                }
                .expect("создание тестового окна");
                let _ = ready_tx.send(SendHwnd(hwnd));
                // Помп: кросс-поточные сообщения (WM_GETMINMAXINFO,
                // WM_WINDOWPOSCHANGING от SetWindowPos) обязаны доходить.
                loop {
                    if kill_rx.try_recv().is_ok() {
                        break;
                    }
                    let mut msg = MSG::default();
                    // SAFETY: msg — валидный буфер; None — сообщения любых окон потока.
                    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                        // SAFETY: msg пришёл из PeekMessageW.
                        unsafe {
                            let _ = TranslateMessage(&msg);
                            let _ = DispatchMessageW(&msg);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                // SAFETY: окно создано этим же потоком.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            });
            let hwnd = ready_rx.recv().expect("поток тестового окна не упал").0;
            Self {
                hwnd,
                thread: Some(thread),
                kill: Some(kill_tx),
            }
        }
    }

    impl Drop for MinMaxWindow {
        fn drop(&mut self) {
            if let Some(k) = self.kill.take() {
                let _ = k.send(());
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn flags(overrides: impl Fn(&mut WindowFlags)) -> WindowFlags {
        let mut f = WindowFlags {
            visible: true,
            is_root: true,
            ..Default::default()
        };
        overrides(&mut f);
        f
    }

    #[test]
    fn plain_visible_root_window_is_real() {
        assert!(is_real_window(&flags(|_| {})));
    }

    #[test]
    fn invisible_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.visible = false)));
    }

    #[test]
    fn cloaked_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.cloaked = true)));
    }

    #[test]
    fn child_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.is_root = false)));
    }

    #[test]
    fn no_activate_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.no_activate = true)));
    }

    #[test]
    fn tool_window_without_app_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.tool_window = true)));
    }

    #[test]
    fn tool_window_with_app_window_override_is_kept() {
        assert!(is_real_window(&flags(|f| {
            f.tool_window = true;
            f.app_window = true;
        })));
    }

    #[test]
    fn owned_window_without_app_window_is_rejected() {
        assert!(!is_real_window(&flags(|f| f.has_owner = true)));
    }

    #[test]
    fn owned_window_with_app_window_override_is_kept() {
        assert!(is_real_window(&flags(|f| {
            f.has_owner = true;
            f.app_window = true;
        })));
    }

    #[test]
    fn iconic_is_not_a_rejection() {
        assert!(is_real_window(&flags(|f| f.iconic = true)));
    }

    #[test]
    fn window_rect_from_rect_computes_size() {
        let rc = RECT {
            left: 10,
            top: 20,
            right: 110,
            bottom: 220,
        };
        let wr: WindowRect = rc.into();
        assert_eq!(
            wr,
            WindowRect {
                x: 10,
                y: 20,
                w: 100,
                h: 200
            }
        );
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn hung_window_title_fetch_is_bounded() {
        let win = HungWindow::create();
        let started = std::time::Instant::now();
        let title = window_text(win.hwnd);
        let elapsed = started.elapsed();
        // Зависшее окно не должно держать чтение заголовка дольше таймаута
        // (500 мс); старый GetWindowTextW висел бы ~5 с на системном
        // таймауте SendMessage. 3 с — запас на планировщик, но на порядок
        // меньше системного умолчания.
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "чтение заголовка зависшего окна заняло {elapsed:?}"
        );
        assert!(title.is_empty(), "зависшее окно не должно иметь заголовка");
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn enumerate_returns_only_real_windows() {
        let windows = enumerate();
        assert!(
            !windows.is_empty(),
            "хотя бы одно окно на реальном десктопе"
        );
        for w in &windows {
            assert!(w.hwnd != 0, "нулевой hwnd: {w:?}");
        }
        let mut prev = None;
        for w in &windows {
            if let Some(p) = prev {
                assert!(w.z_order > p, "z_order не монотонен: {w:?}");
            }
            prev = Some(w.z_order);
        }
    }

    /// Иконки окон (M4 §6) на реальном десктопе: хотя бы у одного окна
    /// иконка извлечена (у окна с exe-путём шелл обязан её отдать), растр
    /// квадратный и полный (`rgba.len() == w*h*4`). Окна без exe-пути
    /// (protected process) остаются `None` — это не ошибка.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn enumerate_populates_icons_for_real_windows() {
        let windows = enumerate();
        let mut with_icon = 0;
        for w in &windows {
            if let Some(icon) = &w.icon {
                with_icon += 1;
                assert_eq!(
                    icon.rgba.len(),
                    icon.width as usize * icon.height as usize * 4,
                    "растр иконки полный: {w:?}"
                );
                assert!(icon.width > 0 && icon.height > 0, "размер иконки: {w:?}");
            }
            if w.exe_path.as_os_str().is_empty() {
                assert!(
                    w.icon.is_none(),
                    "окно без exe-пути не может иметь иконку: {w:?}"
                );
            }
        }
        assert!(
            with_icon > 0,
            "хотя бы одно окно с иконкой на реальном десктопе"
        );
    }

    /// Классы системных меню и переключателей шелла (включая всплывающее меню
    /// Snap Layouts Windows 11 `XamlExplorerHostIslandWindow`, меню переполнения
    /// `TopLevelWindowForOverflowXamlIsland`, резервный класс `SnapFlyout`)
    /// обязаны распознаваться без учёта регистра (репорты 2026-08-22 и 2026-08-26).
    #[test]
    fn shell_transient_classes_recognize_windows11_snap_layouts() {
        assert!(is_shell_transient_class("XamlExplorerHostIslandWindow"));
        assert!(is_shell_transient_class("xamlexplorerhostislandwindow"));
        assert!(is_shell_transient_class(
            "TopLevelWindowForOverflowXamlIsland"
        ));
        assert!(is_shell_transient_class("SnapFlyout"));
        assert!(is_shell_transient_class("Windows.UI.Core.CoreWindow"));
        assert!(is_shell_transient_class("MultitaskingViewFrame"));
        assert!(is_shell_transient_class("TaskSwitcherWnd"));
        assert!(is_shell_transient_class("Shell_TrayWnd"));

        // Обычные прикладные окна не должны ложно определяться как шелл
        assert!(!is_shell_transient_class("CabinetWClass"));
        assert!(!is_shell_transient_class("Chrome_WidgetWin_1"));
        assert!(!is_shell_transient_class("Notepad"));
        assert!(!is_shell_transient_class(""));
    }

    /// Проверка видимости системных всплывающих окон шелла обязана безопасно
    /// деградировать и не паниковать в любой среде (включая CI и сборки Windows без меню).
    #[test]
    fn is_shell_transient_visible_degrades_gracefully() {
        let _ = is_shell_transient_visible();
        let _ = shell_switching();
    }

    // --- минимальный размер окна ([`min_window_size`]) ---

    /// Перевод заявленного минимума из GetWindowRect-пространства в
    /// DWM-пространство: рамка `dw`/`dh` прибавляется, отрицательный
    /// результат («минимума нет» + рамка) схлопывается в ноль.
    #[test]
    fn min_window_size_translates_claim_from_gwr_to_dwm_space() {
        // Заявленный (400,300) + рамка (-14,-7) → (386,293): ровно случай,
        // измеренный пробой spike/min_size_probe на своём окне.
        assert_eq!(
            to_dwm_min(400, 300, (7, 0, -14, -7)),
            WindowMinSize { w: 386, h: 293 }
        );
        // Приложение без минимума (0,0): рамка уводит в минус — схлопываем в ноль.
        assert_eq!(
            to_dwm_min(0, 0, (7, 0, -14, -7)),
            WindowMinSize { w: 0, h: 0 }
        );
        // Custom-chrome окно (рамка 0, как Discord/Spotify): минимум как есть.
        assert_eq!(
            to_dwm_min(800, 600, (0, 0, 0, 0)),
            WindowMinSize { w: 800, h: 600 }
        );
    }

    /// Мёртвый hwnd — `None` без единого кросс-поточного вызова: окна нет,
    /// спрашивать нечего (и не о ком).
    #[test]
    fn min_window_size_of_dead_window_is_none() {
        // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
        let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("resticker_window_enum_plain"),
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация — не ошибка.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            let err = unsafe { GetLastError() };
            assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
        }
        // SAFETY: валидные константы и зарегистрированный класс.
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                w!("resticker_window_enum_plain"),
                w!("plain"),
                WS_OVERLAPPED,
                0,
                0,
                200,
                150,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание тестового окна");
        let dead = hwnd.0 as usize;
        // SAFETY: окно создано этим же потоком.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        assert_eq!(min_window_size(dead), None);
    }

    /// Заявленный минимум совпадает с ФАКТИЧЕСКИМ пределом ужатия чужого
    /// окна: просим ужаться до 60x60, а окно (как и обещало в
    /// WM_GETMINMAXINFO) останавливается ровно на 400x300 в
    /// GetWindowRect-пространстве, то есть на заявленном минимуме в
    /// DWM-пространстве. Это главная проверка доверия к числу.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn min_window_size_matches_actual_shrink_limit_of_foreign_window() {
        let win = MinMaxWindow::create();
        let queried = min_window_size(win.hwnd.0 as usize).expect("живое окно отвечает");
        let (_, _, dw, dh) = dwm_frame_offset(win.hwnd);
        assert_eq!(
            queried,
            to_dwm_min(400, 300, (0, 0, dw, dh)),
            "запрос обязан вернуть заявленный обработчиком минимум с рамкой"
        );
        // SAFETY: окно живо; SetWindowPos без z-order/активации.
        let _ = unsafe {
            SetWindowPos(
                win.hwnd,
                None,
                100,
                100,
                60,
                60,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut actual = WindowRect::default();
        while Instant::now() < deadline {
            actual = extended_frame_bounds(win.hwnd);
            if actual.w == queried.w && actual.h == queried.h {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            (actual.w, actual.h),
            (queried.w, queried.h),
            "фактический предел ужатия обязан совпасть с заявленным минимумом"
        );
    }

    /// Зависшее окно (поток не пампит) не держит запрос минимума дольше
    /// таймаута и возвращает `None` — раскладка обойдётся без минимума,
    /// а не повиснет вместе с приложением.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_enum -- --ignored"]
    fn hung_window_min_size_query_is_bounded_and_none() {
        let win = HungWindow::create();
        let started = Instant::now();
        let min = min_window_size(win.hwnd.0 as usize);
        let elapsed = started.elapsed();
        assert_eq!(min, None, "зависшее окно не даёт минимума");
        // 3 с — запас на планировщик, на порядок меньше системного умолчания
        // SendMessage (~5 с); фактический таймаут — 25 мс + накладные.
        assert!(
            elapsed < Duration::from_secs(3),
            "запрос минимума зависшего окна занял {elapsed:?}"
        );
    }
}
