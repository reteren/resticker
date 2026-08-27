//! Примитивы показа и сокрытия окон ГРУППЫ (G2: «показать/спрятать группу
//! окон по хоткею»).
//!
//! Координатору нужно по хоткею вытащить все окна группы на экран поверх
//! всего и по повторному нажатию снова спрятать их. Здесь — чистые
//! Win32-примитивы для ОДНОГО окна: кто входит в группу и когда её
//! показывать/прятать — решает координатор (см. [`rst_core::group_layout`]).
//!
//! Все ключевые решения приняты по ЗАМЕРАМ `spike/visibility_probe`
//! (2026-08-26, собственные окна, чужие не трогались):
//!
//! * **Спрятать = СВЕРНУТЬ, а не скрыть.** Пользователь дословно: «они
//!   никуда не исчезают, ты всё равно можешь на них альт-табнуться».
//!   `SW_HIDE` убирает окно и из Alt+Tab, и из панели задач — поэтому
//!   только свёрнутый вид (`IsWindowVisible` у свёрнутого окна остаётся
//!   TRUE — замер, оно остаётся в переключателе).
//! * **Сворачивание — `SW_SHOWMINNOACTIVE`, а не `SW_MINIMIZE`.**
//!   Измерено: `SW_MINIMIZE` на АКТИВНОМ окне переключает foreground на
//!   «следующее top-level окно» (замер: fg сменился 001508CA→002C07A0),
//!   `SW_SHOWMINNOACTIVE` не трогает активное окно ни в одном сценарии
//!   (активно чужое окно / активен сам сворачиваемый). `SW_FORCEMINIMIZE`
//!   на живом окне сначала делает его НЕВИДИМЫМ (visible=false при t=0) и на
//!   переиспользованном окне не доходил до сворачивания; `WM_SYSCOMMAND/
//!   SC_MINIMIZE` уходит в очередь чужого потока и там не срабатывал
//!   (iconic=false за 1.5 с) — оба отброшены.
//! * **Разворачивание — `SW_RESTORE`**: возвращает окно РОВНО в прежний
//!   прямоугольник (замер §2: точное совпадение `GetWindowRect` до/после,
//!   и синхронно, и асинхронно). Группа хранит позиции окон — если бы
//!   восстановление их теряло, раскладка развалилась бы. НЕ свёрнутое окно
//!   `ShowWindow` не трогаем вовсе (`SW_RESTORE` на нормальном окне — no-op,
//!   лишний вызов чужому окну не нужен).
//! * **Подъём — `HWND_TOP` БЕЗ `WS_EX_TOPMOST`.** Липкий topmost-стиль на
//!   чужом окне уже давал живой баг (чужие окна висели поверх всего после
//!   снятия группы). Замер §3: `HWND_TOP` поднимает окно над полноэкранным
//!   окном ОБЫЧНОЙ полосы (borderless fullscreen — так делают игры и
//!   плееры), стиль чист до и после; против полноэкранного в topmost-полосе
//!   не работает ни `HWND_TOP`, ни приём «TOPMOST→NOTOPMOST» (полоса topmost
//!   выше обычной по построению Windows) — честное ограничение, обходится
//!   только постоянным стилем, который нам запрещён.
//! * **Анимация**: переход в свёрнутое состояние атомарный (<3 мс, опрос
//!   `GetWindowRect` каждые 3 мс — без промежуточных размеров) для всех
//!   способов; «подёргивание» даёт именно переключение foreground, которого
//!   у `SW_SHOWMINNOACTIVE` нет. Видимую DWM-анимацию при желании можно
//!   погасить [`WindowPins::set_transitions_disabled`] — примитив уже есть
//!   в модуле пинов и сюда не дублируется.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindow, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SetForegroundWindow, ShowWindowAsync,
};

use crate::window_pin::WindowPins;

/// Свернуто ли окно прямо сейчас (предикат для «не сворачивать повторно»).
///
/// Отдельная функция, а не встраивание `IsIconic` в `hide_group_window`:
/// координатор сам решает, дёргать ли сворачивание, и должен уметь
/// спросить состояние, не трогая окно. Для мёртвого хэндла — `false`, без
/// паники (конвенция модуля: «missing elements are not a panic»).
pub fn is_minimized(hwnd: HWND) -> bool {
    // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    // SAFETY: IsIconic безопасен для любых значений.
    unsafe { IsIconic(hwnd) }.as_bool()
}

/// Спрятать окно группы: свернуть, оставив в Alt+Tab и панели задач
/// (см. модульный доккомент — «они никуда не исчезают»), БЕЗ переключения
/// активного окна.
///
/// `SW_SHOWMINNOACTIVE` вместо `SW_MINIMIZE` — измерено в
/// `spike/visibility_probe`: `SW_MINIMIZE` на активном окне переключает
/// foreground на следующее top-level окно (кража фокуса при сворачивании
/// группы), `SW_SHOWMINNOACTIVE` активное окно не трогает никогда.
///
/// `ShowWindowAsync`, а не `ShowWindow`: команда чужому окну не должна
/// блокировать координатор на чужой очереди сообщений (окно может
/// «задуматься» — координатор обязан продолжать рисовать).
///
/// Идемпотентность: уже свёрнутое окно повторно не дёргаем (без нужды
/// чужое окно не трогаем) — `true`, результат уже достигнут. `false` —
/// окно мёртвое или системный отказ.
pub fn hide_group_window(hwnd: HWND) -> bool {
    if is_minimized(hwnd) {
        return true;
    }
    // SAFETY: ShowWindowAsync безопасен для чужого и мёртвого окна —
    // просто вернёт FALSE.
    unsafe { ShowWindowAsync(hwnd, SW_SHOWMINNOACTIVE) }.as_bool()
}

/// Показать окно группы: развернуть из свёрнутого состояния в ПРЕЖНИЙ
/// размер (не на весь экран!) и поднять наверх.
///
/// Разворачивание — `SW_RESTORE`: возвращает окно ровно в тот
/// прямоугольник, который оно занимало до сворачивания (замер
/// `spike/visibility_probe` §2: точное совпадение `GetWindowRect`
/// до/после — и синхронно, и асинхронно). НЕ свёрнутое окно `ShowWindow`
/// не трогаем вовсе — только поднимаем.
///
/// Подъём идёт ПОСЛЕ разворачивания: замер §2b показал, что при любом
/// порядке (restore→raise, raise→restore, raise после ожидания !iconic)
/// окно остаётся наверху, но только порядок restore→raise покрывает и
/// случай «окно вообще не сворачивалось» (там restore не вызывается).
///
/// `false` — окно мёртвое (тогда поднять нечего); ошибки подъёма
/// (например, UIPI-отказ на окне с повышенными правами) не различаем —
/// тот же контракт, что у [`WindowPins::raise_without_topmost`].
pub fn show_group_window(hwnd: HWND) -> bool {
    // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    if is_minimized(hwnd) {
        // `SW_SHOWNOACTIVATE`, а НЕ `SW_RESTORE`: последний разворачивает
        // окно И делает его активным. При показе группы окон несколько, и
        // каждое такое разворачивание перехватывало фокус у соседа — а когда
        // очередь доходила до конца, Windows возвращала передний план тому
        // окну, которое было активно до всей этой возни.
        //
        // Замер на живом приложении 2026-08-26: после показа группы фокус
        // держался на её первом окне ~400 мс, а затем возвращался в браузер,
        // с которого хоткей и нажимали, — и механика «ушёл на постороннее
        // окно» тут же прятала только что показанную группу.
        //
        // Кто получит фокус, решает вызывающий, и ровно один раз
        // ([`focus_group_window`]); разворачивание чужого окна на это права
        // не имеет.
        //
        // SAFETY: см. `hide_group_window`.
        let _ = unsafe { ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE) };
    }
    raise_group_window(hwnd)
}

/// Поднять окно группы наверх обычной полосы — разово, БЕЗ постоянного
/// стиля `WS_EX_TOPMOST`.
///
/// Замер `spike/visibility_probe` §3: `HWND_TOP` поднимает окно над
/// полноэкранным окном обычной полосы (borderless fullscreen), и
/// `WS_EX_TOPMOST` не появляется (стиль чист до и после — оба замера в
/// отчёте задачи G2). Липкий topmost-стиль на чужом окне оставлять нельзя:
/// уже был живой баг «чужие окна висят поверх всего после снятия группы».
///
/// Единственный примитив подъёма живёт в
/// [`WindowPins::raise_without_topmost`] — намеренно НЕ дублируем (G2.4):
/// у пинов и группы одна механика подъёма, и если она когда-нибудь
/// изменится, поправить нужно одно место.
///
/// `false` — окно мёртвое (поднимать нечего); мёртвый между проверкой и
/// вызовом — обычная гонка, `SetWindowPos` просто вернёт ошибку.
pub fn raise_group_window(hwnd: HWND) -> bool {
    // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    // Пара TOPMOST -> NOTOPMOST, а НЕ простой HWND_TOP.
    //
    // Замер spike/zorder_probe (B4, 2026-08-26): `SetWindowPos(HWND_TOP,
    // SWP_NOACTIVATE)` у ЧУЖОГО обычного окна не меняет его ранг вовсе, пока
    // наверху полосы стоит активное окно пользователя, — ни сразу, ни через
    // секунды, ни с правом на передний план, ни с повышенным токеном.
    // Поднималось ровно одно окно — то, которому отдавали фокус: это
    // дословно симптом пользователя «выбрал 4 окна, а вылетело одно».
    //
    // Пара с временным `HWND_TOPMOST` блокировку обходит: окно поднимается
    // над активным и там остаётся (замер: четыре окна выше постороннего,
    // устойчиво 3 секунды). Липкого стиля не остаётся — второй вызов
    // снимает `WS_EX_TOPMOST` немедленно, и это проверено чтением
    // `GWL_EXSTYLE` до и после.
    let pins = WindowPins::new();
    pins.surface_topmost_temporarily(hwnd);
    pins.drop_topmost(hwnd);
    true
}

/// Сделать окно группы активным (передний план).
///
/// Нужно ровно один раз на весь показ группы, для окна первого слота. Без
/// этого группа гасла сама собой: показ хоткеем не меняет активное окно, а
/// раз активным осталось что-то постороннее, механика «ушёл на чужое окно —
/// группа прячется» срабатывала на первом же снимке трекера и прятала группу
/// через долю секунды после того, как её показали (найдено замером на живом
/// приложении 2026-08-26).
///
/// Windows разрешает менять передний план не всякому процессу, но приложению,
/// которое только что получило глобальный хоткей, — разрешает: ввод пришёл к
/// нам. Отказ (`false`) не ошибка и не повод паниковать: группа останется на
/// экране, просто без фокуса.
pub fn focus_group_window(hwnd: HWND) -> bool {
    // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    // SAFETY: SetForegroundWindow принимает любой живой HWND и при отказе
    // возвращает FALSE, а не падает.
    unsafe { SetForegroundWindow(hwnd) }.as_bool()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GW_HWNDPREV, GWL_EXSTYLE,
        GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect, MSG, PM_REMOVE,
        PeekMessageW, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER,
        SetWindowPos, TranslateMessage, WINDOW_STYLE, WNDCLASSEXW, WS_EX_TOPMOST, WS_OVERLAPPED,
        WS_POPUP, WS_VISIBLE,
    };
    use windows::core::w;

    use super::*;

    /// Скрытое окно текущего тест-потока (тот же паттерн, что `TestWindow`
    /// в window_pin.rs): нити сообщений не требует — для
    /// `ShowWindowAsync`/`SetWindowPos`/`DestroyWindow` помп крутит `pump`.
    struct TestWindow(HWND);

    impl TestWindow {
        fn create(style: WINDOW_STYLE) -> Self {
            // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
            let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(test_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: w!("resticker_window_visibility_test"),
                ..Default::default()
            };
            // SAFETY: wc заполнена корректно. Класс процесс-wide: повторная
            // регистрация (параллельные тесты) — не ошибка.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                // SAFETY: осмысленна сразу после провалившегося вызова.
                let err = unsafe { windows::Win32::Foundation::GetLastError() };
                assert_eq!(err, windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS);
            }
            // SAFETY: все аргументы — валидные константы и зарегистрированный
            // класс; окно принадлежит текущему потоку.
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("resticker_window_visibility_test"),
                    w!("test"),
                    style,
                    0,
                    0,
                    400,
                    300,
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

    /// Крутить помп текущего потока: `ShowWindowAsync` из модуля постит
    /// событие в очередь окна (оно живёт на тест-потоке) — без помпа
    /// свёрнутое состояние не наступит.
    fn pump(ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
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
    }

    /// Дождаться предиката с таймаутом: состояние меняется асинхронно
    /// (событие уходит в очередь окна), но обязано наступить за разумное
    /// время — иначе тест падает с понятным сообщением.
    fn pump_until(what: &str, timeout_ms: u64, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            pump(2);
            if cond() {
                return;
            }
        }
        panic!("состояние «{what}» не наступило за {timeout_ms} мс");
    }

    /// Стоит ли окно `above` где-то выше окна `below` (обход z-order вверх).
    fn window_above(above: HWND, below: HWND) -> bool {
        // SAFETY: GetWindow — чтение z-order, безопасно для любых хэндлов.
        let mut cur = unsafe { GetWindow(below, GW_HWNDPREV) }.unwrap_or_default();
        while !cur.0.is_null() {
            if cur == above {
                return true;
            }
            // SAFETY: то же — обход вверх до верха стопки.
            cur = unsafe { GetWindow(cur, GW_HWNDPREV) }.unwrap_or_default();
        }
        false
    }

    /// Липкий стиль: WS_EX_TOPMOST в расширенном стиле окна.
    fn has_topmost_style(hwnd: HWND) -> bool {
        // SAFETY: GetWindowLongPtrW — чтение стиля живого окна.
        (unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0) != 0
    }

    fn rect_of(hwnd: HWND) -> RECT {
        let mut r = RECT::default();
        // SAFETY: hwnd — живое окно текущего потока.
        unsafe { GetWindowRect(hwnd, &mut r) }.expect("GetWindowRect");
        r
    }

    /// Окно группы спрятано = свёрнуто, но осталось ВИДИМЫМ: именно
    /// видимость (`WS_VISIBLE`) держит окно в Alt+Tab и панели задач —
    /// «они никуда не исчезают» (G2.1).
    #[test]
    fn hiding_a_group_window_minimizes_it_and_keeps_it_visible() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        assert!(!is_minimized(win.0));

        assert!(hide_group_window(win.0), "сворачивание живого окна");
        pump_until("окно свёрнуто", 2000, || is_minimized(win.0));

        // SAFETY: окно живо (свёрнутое — живое).
        assert!(
            unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(win.0) }.as_bool(),
            "свёрнутое окно обязано остаться видимым (Alt+Tab)"
        );
    }

    /// Повторное сворачивание уже свёрнутого окна — no-op с `true`
    /// («результат уже достигнут»): чужое окно без нужды не трогаем.
    #[test]
    fn hiding_already_minimized_window_is_noop_but_reports_success() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        assert!(hide_group_window(win.0));
        pump_until("окно свёрнуто", 2000, || is_minimized(win.0));
        assert!(
            hide_group_window(win.0),
            "повторное сворачивание не должно считаться ошибкой"
        );
        assert!(is_minimized(win.0));
    }

    /// Мёртвый HWND (окно закрылось между перечислением и вызовом) — `false`
    /// без паники (G2.5).
    #[test]
    fn hiding_a_dead_window_is_false_without_panicking() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        let dead = win.0;
        drop(win);
        assert!(!hide_group_window(dead));
        assert!(!is_minimized(dead));
    }

    /// Предикат `is_minimized` честно различает свёрнутое/нормальное/мёртвое
    /// (G2.6): «свёрнуто ли» — вопрос, а не дёрганье окна.
    #[test]
    fn is_minimized_distinguishes_minimized_normal_and_dead_windows() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        assert!(!is_minimized(win.0), "нормальное окно — не свёрнуто");

        assert!(hide_group_window(win.0));
        pump_until("окно свёрнуто", 2000, || is_minimized(win.0));
        assert!(is_minimized(win.0), "свёрнутое окно — свёрнуто");

        let dead = win.0;
        drop(win);
        assert!(
            !is_minimized(dead),
            "мёртвое окно — не свёрнуто, без паники"
        );
    }

    /// Разворачивание возвращает окно РОВНО в прежний прямоугольник
    /// (G2.2): группа хранит позиции окон, потеря прямоугольника при
    /// восстановлении развалила бы раскладку. Замер пробы: точное
    /// совпадение до/после, здесь — то же на асинхронном пути модуля.
    #[test]
    fn showing_restores_minimized_window_to_exact_previous_rect() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        // SAFETY: окно живо; известный прямоугольник без z-order/активации.
        unsafe {
            SetWindowPos(
                win.0,
                None,
                137,
                259,
                613,
                447,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
        .expect("позиционирование");
        pump(50);

        let before = rect_of(win.0);
        assert!(hide_group_window(win.0));
        pump_until("окно свёрнуто", 2000, || is_minimized(win.0));

        assert!(show_group_window(win.0), "показ живого окна");
        pump_until("окно развёрнуто", 2000, || {
            !is_minimized(win.0)
        });

        assert_eq!(
            rect_of(win.0),
            before,
            "развёрнутое окно обязано встать ровно в прежний прямоугольник"
        );
    }

    /// НЕ свёрнутое окно `ShowWindow` не трогаем вовсе — только поднимаем
    /// (G2.2): прямоугольник не должен измениться от показа.
    #[test]
    fn showing_a_normal_window_only_raises_it() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        // SAFETY: окно живо; известный прямоугольник.
        unsafe { SetWindowPos(win.0, None, 47, 83, 500, 400, SWP_NOACTIVATE | SWP_NOZORDER) }
            .expect("позиционирование");
        pump(50);
        let before = rect_of(win.0);

        assert!(show_group_window(win.0));
        pump(100);

        assert!(
            !is_minimized(win.0),
            "окно не сворачивалось — показ не сворачивает"
        );
        assert_eq!(rect_of(win.0), before, "показ не меняет прямоугольник");
    }

    /// Показ группы поднимает окно НАД полноэкранным соседом и не
    /// оставляет липкий WS_EX_TOPMOST (G2.3 — регрессия живого бага:
    /// чужие окна висели поверх всего после снятия группы).
    ///
    /// Полноэкранное окно здесь — обычная полоса (borderless fullscreen,
    /// как у игр и плееров); против topmost-полосы не работает ни один
    /// стиль-свободный приём — это честная граница Windows, замерена в
    /// пробе §3.
    #[test]
    fn showing_raises_above_fullscreen_peer_without_sticky_topmost() {
        let target = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        let fullscreen = TestWindow::create(WS_POPUP | WS_VISIBLE);
        // SAFETY: окно живо; растягиваем во весь монитор (как borderless
        // fullscreen) и поднимаем наверх — эталон «полноэкранная программа».
        unsafe {
            SetWindowPos(
                fullscreen.0,
                Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOP),
                0,
                0,
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN),
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
        .expect("позиционирование полноэкранного");
        pump(100);
        assert!(
            !window_above(target.0, fullscreen.0),
            "предусловие: целевое окно ниже полноэкранного"
        );
        assert!(
            !has_topmost_style(target.0),
            "предусловие: стиль до подъёма чист"
        );

        assert!(show_group_window(target.0), "показ окна группы");
        pump(150);

        assert!(
            window_above(target.0, fullscreen.0),
            "показ обязан поднять окно над полноэкранным соседом"
        );
        assert!(
            !has_topmost_style(target.0),
            "подъём не должен оставить WS_EX_TOPMOST (липкий стиль)"
        );
    }

    /// Подъём сам по себе никогда не ставит липкий стиль: замер стиля
    /// «до и после» — регрессия живого бага 2026-08-26.
    #[test]
    fn raising_never_sticks_topmost_style() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        assert!(!has_topmost_style(win.0), "до подъёма стиль чист");

        assert!(raise_group_window(win.0));
        pump(100);

        assert!(
            !has_topmost_style(win.0),
            "raise_group_window не имеет права выставлять WS_EX_TOPMOST"
        );
    }

    /// Мёртвый HWND в показе — `false` без паники (G2.5), даже если окно
    /// умерло МЕЖДУ проверкой живости и разворачиванием (обычная гонка).
    #[test]
    fn showing_a_dead_window_is_false_without_panicking() {
        let win = TestWindow::create(WS_OVERLAPPED | WS_VISIBLE);
        let dead = win.0;
        drop(win);
        assert!(!show_group_window(dead));
        assert!(!raise_group_window(dead));
    }
}
