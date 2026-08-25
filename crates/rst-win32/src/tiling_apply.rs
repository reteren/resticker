//! Применение раскладки к живым окнам (M9, docs/TILING_DESIGN.md §T2).
//!
//! Тонкий слой: сюда приходят УЖЕ посчитанные целевые прямоугольники (их
//! считает `rst_core::tiling::layout`, платформенно-чисто и с тестами), а
//! здесь остаётся только честно отправить их в Windows и рассказать, что из
//! этого вышло.
//!
//! ## Почему через [`WindowPins`], а не своими вызовами
//!
//! Три примитива, которые нужны тайлингу, уже написаны и отлажены на живых
//! багах M6 — переписывать их значило бы разойтись в тонкостях:
//!
//! * [`WindowPins::set_dwm_bounds`] ставит окну такие границы, чтобы совпали
//!   его DWM-габариты, а не `GetWindowRect`. У окон Win11 они отличаются на
//!   невидимые поля ресайза (по бокам и снизу ~7–8 px). Без этой коррекции
//!   гэпы тайлинга врали бы на рамку у каждого окна, а повторные применения
//!   ещё и дрейфовали бы (window_pin.rs:686).
//! * Внутри той же функции живёт обход подвоха с `WS_MAXIMIZE`: развёрнутое
//!   окно МОЛЧА игнорирует размер из `SetWindowPos`, и лечится это только
//!   `SetWindowPlacement` (window_pin.rs:424). Тайлинг натыкается на это
//!   постоянно — развёрнутые окна встречаются чаще прочих.
//! * [`WindowPins::set_transitions_disabled`] гасит системную анимацию, из-за
//!   которой окно ещё четверть секунды едет само по себе после нашего вызова.
//!
//! Формально это связывает тайлинг с модулем «стикеры-окна». Связь
//! односторонняя и по значению (`&WindowPins`, состояние закреплений не
//! читается и не меняется); если она начнёт мешать, три примитива выносятся
//! в свободные функции того же модуля одной механической правкой.
//!
//! ## Чего этот слой НЕ делает
//!
//! Не проверяет, доехало ли окно. `set_dwm_bounds` возвращает `true` даже
//! когда система отказала (elevated-окно под UIPI) — проверять надо
//! СЛЕДУЮЩИМ снимком трекера, а не сразу после вызова: DWM-габариты
//! обновляются к следующему кадру композиции, и мгновенная проверка дала бы
//! ложные «не доехало». Это ровно то, для чего существует слой сведения
//! (`rst_core::tiling::reconcile`): окно, которое не встало на место за
//! несколько заходов, там и будет признано неуправляемым.

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsHungAppWindow, IsWindow, PostMessageW,
    SetForegroundWindow, WM_CLOSE,
};

use crate::window_enum::WindowRect;
use crate::window_pin::WindowPins;

/// Куда поставить одно окно. Прямоугольник — в DWM-координатах, тех же, в
/// которых живут снимки трекера (`WindowInfo::rect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileTarget {
    pub hwnd: usize,
    pub rect: WindowRect,
}

/// Что вышло из применения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Окна, которым мы отправили новую геометрию.
    pub attempted: Vec<usize>,
    /// Окна, которых уже нет: успели закрыться между снимком и применением.
    /// Координатор убирает их из раскладки, не считая это ошибкой.
    pub dead: Vec<usize>,
    /// Система отказала прямо на вызове (окно есть, но `SetWindowPos`
    /// не прошёл).
    pub refused: Vec<usize>,
}

impl ApplyReport {
    pub fn is_empty(&self) -> bool {
        self.attempted.is_empty() && self.dead.is_empty() && self.refused.is_empty()
    }
}

/// Отправить окнам новую геометрию.
///
/// Последовательно, а не пакетом через `BeginDeferWindowPos`: пакет не умеет
/// `SetWindowPlacement`, без которого не обойтись с развёрнутыми окнами (см.
/// доктрину модуля), а выигрыш от него на чужих процессах не измерен —
/// [`docs/research/tiling/R3_WIN32_MECHANICS.md`] отмечает это как гипотезу,
/// а не факт. Мерить и оптимизировать имеет смысл тогда, когда станет видно,
/// что перекладка мигает; сейчас это была бы оптимизация вслепую.
pub fn apply(pins: &WindowPins, targets: &[TileTarget]) -> ApplyReport {
    let mut report = ApplyReport::default();
    for target in targets {
        let hwnd = HWND(target.hwnd as *mut core::ffi::c_void);
        // SAFETY: IsWindow безопасен для любого значения хэндла, включая
        // уже уничтоженное окно.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            report.dead.push(target.hwnd);
            continue;
        }
        if pins.set_dwm_bounds(hwnd, to_rect(target.rect)) {
            report.attempted.push(target.hwnd);
        } else {
            report.refused.push(target.hwnd);
        }
    }
    report
}

/// Подготовить окно к жизни в плитке: погасить системные анимации.
///
/// Вызывается один раз, когда окно ВХОДИТ в раскладку, а не на каждой
/// перестановке: атрибут живёт на чужом окне, и дёргать DWM на каждый кадр
/// незачем.
pub fn prepare(pins: &WindowPins, hwnd: usize) {
    pins.set_transitions_disabled(HWND(hwnd as *mut core::ffi::c_void), true);
}

/// Вернуть окну штатное поведение: тайлинг его больше не держит.
///
/// Обязательно парная к [`prepare`]. Чужому окну мы не вправе навсегда
/// менять поведение — тот же принцип, по которому пины снимают свой маркер
/// (window_pin.rs:228).
pub fn release(pins: &WindowPins, hwnd: usize) {
    pins.set_transitions_disabled(HWND(hwnd as *mut core::ffi::c_void), false);
}

/// Передать окну фокус ввода.
///
/// Самое ненадёжное место всей подсистемы, и это свойство Windows, а не наш
/// недосмотр. Система разрешает менять активное окно только процессу,
/// который сам сейчас на переднем плане или только что получил ввод
/// (docs/research/tiling/R3_WIN32_MECHANICS.md §6). Оверлеи resticker —
/// `WS_EX_NOACTIVATE`, фокуса у нас нет по построению, поэтому голый
/// `SetForegroundWindow` часто возвращает `false`, и вместо переключения
/// пользователь видит мигающую кнопку на панели задач.
///
/// Отсюда двухступенчатость: сначала честная попытка, и только при отказе —
/// приём с временным присоединением к очереди ввода потока активного окна
/// (`AttachThreadInput`), после которого система считает нас «своими».
///
/// У приёма есть цена, поэтому он под условием: присоединение к очереди
/// ЗАВИСШЕГО окна утащит за собой и нашу очередь ввода, то есть подвесит
/// координатор. `IsHungAppWindow` — дешёвая проверка, которая это
/// предотвращает; если окно зависло, честнее не переключить фокус, чем
/// повесить программу.
pub fn focus(hwnd: usize) -> bool {
    let target = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: обе функции безопасны для любого значения хэндла.
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return false;
    }
    // SAFETY: см. выше.
    if unsafe { SetForegroundWindow(target) }.as_bool() {
        return true;
    }

    // SAFETY: чтение переднего окна безопасно и не блокируется.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() || unsafe { IsHungAppWindow(foreground) }.as_bool() {
        return false;
    }
    // SAFETY: GetWindowThreadProcessId с null-указателем на pid — документированный
    // способ спросить только поток.
    let fg_thread = unsafe { GetWindowThreadProcessId(foreground, None) };
    let us = unsafe { GetCurrentThreadId() };
    if fg_thread == 0 || fg_thread == us {
        return false;
    }

    // SAFETY: присоединение симметрично и снимается ниже при любом исходе;
    // зависшее окно отсеяно проверкой выше.
    let attached = unsafe { AttachThreadInput(us, fg_thread, true) }.as_bool();
    // Только `SetForegroundWindow`: `SetFocus` здесь бесполезен и вреден. Он
    // работает в пределах ОДНОЙ очереди ввода и на чужое окно ничего не
    // меняет, зато его возврат ничего не говорит о реальном фокусе, а на
    // неотзывчивом окне добавляет ещё одну точку блокировки (находка ревью 5).
    // SAFETY: окно проверено выше; вызов не блокируется.
    let ok = unsafe { SetForegroundWindow(target) }.as_bool();
    if attached {
        // SAFETY: парная отвязка от той же очереди.
        let _ = unsafe { AttachThreadInput(us, fg_thread, false) };
    }
    ok
}

/// Попросить окно закрыться (`WM_CLOSE`).
///
/// `PostMessageW`, а не `SendMessageW`: сообщение кладётся в очередь и
/// возвращает управление немедленно, поэтому зависшее приложение не утянет
/// за собой координатор. Ответ «закрылось» этот вызов не даёт и дать не
/// может — окно вправе показать «сохранить изменения?» и остаться. Именно
/// поэтому слой исполнения не удаляет окно из дерева сам: оно уйдёт оттуда,
/// когда трекер увидит, что окна больше нет.
pub fn close(hwnd: usize) -> bool {
    let target = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: IsWindow безопасен для любого значения хэндла.
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return false;
    }
    // SAFETY: PostMessageW не блокируется и безопасен для чужого окна.
    unsafe { PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
}

/// [`WindowRect`] (x/y/w/h) → Win32 `RECT` (left/top/right/bottom).
fn to_rect(r: WindowRect) -> RECT {
    RECT {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_rect_becomes_a_win32_rect() {
        let r = to_rect(WindowRect {
            x: 100,
            y: 50,
            w: 800,
            h: 600,
        });
        assert_eq!(r.left, 100);
        assert_eq!(r.top, 50);
        assert_eq!(r.right, 900);
        assert_eq!(r.bottom, 650);
    }

    #[test]
    fn negative_origin_survives_the_conversion() {
        // Мониторы слева от основного дают отрицательные координаты —
        // это норма, а не ошибка (monitors.rs, `bounds_px`).
        let r = to_rect(WindowRect {
            x: -1920,
            y: -200,
            w: 1920,
            h: 1080,
        });
        assert_eq!(r.left, -1920);
        assert_eq!(r.right, 0);
        assert_eq!(r.top, -200);
        assert_eq!(r.bottom, 880);
    }

    #[test]
    fn focusing_a_dead_window_fails_instead_of_panicking() {
        assert!(!focus(0xDEAD_BEEF));
    }

    #[test]
    fn closing_a_dead_window_fails_instead_of_panicking() {
        assert!(!close(0xDEAD_BEEF));
    }

    #[test]
    fn an_empty_report_is_empty() {
        assert!(ApplyReport::default().is_empty());
    }

    #[test]
    fn a_report_with_only_dead_windows_is_not_empty() {
        // Мёртвые окна — не ошибка, но и не «ничего не произошло»:
        // координатор обязан убрать их из раскладки.
        let report = ApplyReport {
            dead: vec![1],
            ..Default::default()
        };
        assert!(!report.is_empty());
    }

    #[test]
    fn applying_nothing_reports_nothing() {
        let pins = WindowPins::new();
        assert!(apply(&pins, &[]).is_empty());
    }

    #[test]
    fn a_dead_handle_is_reported_as_dead_and_not_attempted() {
        // Заведомо невалидный хэндл: окно, закрывшееся между снимком
        // трекера и применением раскладки, — штатная гонка.
        let pins = WindowPins::new();
        let report = apply(
            &pins,
            &[TileTarget {
                hwnd: 0xDEAD_BEEF,
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                },
            }],
        );
        assert_eq!(report.dead, vec![0xDEAD_BEEF]);
        assert!(report.attempted.is_empty());
        assert!(report.refused.is_empty());
    }
}
