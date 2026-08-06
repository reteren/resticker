//! Закрепление окна поверх остальных (ROADMAP.md M6, «стикеры-окна
//! (Always On Top)», SPEC.md §5.2): `pin`/`unpin` — это НЕ отдельное окно
//! (у стикера-окна нет своего HWND, `SPEC.md §5` — резюме: «Закреплённое
//! окно получает `WS_EX_TOPMOST` и остаётся поверх остальных окон»), а
//! `SetWindowPos(target, HWND_TOPMOST, ...)` на самом целевом окне, плюс
//! оконный маркер `SetPropW`, снятый при `unpin`, и отслеживание
//! уничтожения закреплённого окна.
//!
//! Уничтожение таргета ловится БЕЗ собственного WinEvent-хука: общий
//! диспетчер — [`crate::window_tracker::WindowTracker`] (там же живёт
//! единственная регистрация `SetWinEventHook`). Координатор скармливает его
//! снимки через [`WindowPins::handle_snapshot`]; модуль сам сверяет
//! закреплённые окна со снимком и эмитит [`PinEvent::TargetDestroyed`] —
//! безопасное Rust-событие, решение о судьбе стикера принимает координатор.
//!
//! Маркер — window property с уникальным именем `resticker`: переживает
//! процессы (виден другим экземплярам), умирает вместе с окном и позволяет
//! отличать уже закреплённые окна (в т.ч. оставшиеся от аварийного выхода
//! прошлого запуска). Значение маркера — непрозрачный `u64` от вызывающего
//! кода (координатор передаёт хэш `Uuid` стикера — модуль намеренно не
//! знает про `Uuid`/модель стикера, только числовой маркер): по нему `pin`
//! отличает «чужой» маркер от своего и детектит переиспользование hwnd.
//!
//! UIPI (закрепление поверх окон с повышенными правами) сознательно не
//! обходится: `SetWindowPos`/`SetPropW`/`RemovePropW` с
//! `ERROR_ACCESS_DENIED` превращаются в понятный
//! [`Win32Error::PinAccessDenied`] с диагностикой про права администратора.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, HWND, SetLastError};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPropW, HWND_NOTOPMOST, HWND_TOPMOST, IsWindow, RemovePropW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetPropW, SetWindowPos,
};
use windows::core::{HRESULT, PCWSTR, w};

use crate::error::Win32Error;
use crate::window_enum::WindowInfo;

/// Имя маркера-проперти (SetPropW), отличающего закреплённые окна.
/// Уникально для resticker; значение — маркер вызывающего кода.
const PIN_PROP_NAME: PCWSTR = w!("resticker");

/// Безопасное событие закрепления. Никаких Win32-типов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinEvent {
    /// Закреплённое окно уничтожено: закрепление снято на нашей стороне
    /// (маркер умер вместе с окном, книжка очищена). Что делать со
    /// стикером — решает координатор.
    TargetDestroyed { target: usize },
}

/// Состояние закреплений. Живёт на потоке координатора (рядом с каналом
/// трекера): чистые `usize`-ключи, нити и Win32-ресурсы не нужны.
#[derive(Default)]
pub struct WindowPins {
    /// `target hwnd → маркер вызывающего кода` (координатор кладёт сюда
    /// хэш `Uuid` стикера). Единственный источник правды: маркер на чужом
    /// окне может быть снят извне (другой экземпляр), но книжка отражает
    /// наши операции.
    pinned: HashMap<usize, u64>,
}

impl WindowPins {
    pub fn new() -> Self {
        Self::default()
    }

    /// Закрепить окно `target` поверх остальных: `SetWindowPos` с
    /// `HWND_TOPMOST` (флаги `SWP_NOACTIVATE`/`SWP_NOMOVE`/`SWP_NOSIZE` —
    /// без активации, фокуса и движения/ресайза — SPEC.md §5.2, «стикер»
    /// это само окно, не отдельный HWND) и маркер `SetPropW` со значением
    /// `marker` (координатор передаёт свой опознавательный номер —
    /// см. модульный доккомент).
    ///
    /// Ошибки: [`Win32Error::PinWindowGone`] — таргет уже закрыт;
    /// [`Win32Error::AlreadyPinned`] — таргет уже закреплён (маркер стоит,
    /// в т.ч. от аварийного выхода прошлого запуска); [`Win32Error::PinAccessDenied`]
    /// — UIPI: таргет с повышенными правами, обходить не пытаемся.
    pub fn pin(&mut self, marker: u64, target: usize) -> Result<(), Win32Error> {
        let target_hwnd = hwnd_from_usize(target);
        // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
        if !unsafe { IsWindow(Some(target_hwnd)) }.as_bool() {
            return Err(Win32Error::PinWindowGone);
        }
        // SAFETY: target проверен IsWindow выше; GetPropW безопасен и для
        // чужих окон.
        if !unsafe { GetPropW(target_hwnd, PIN_PROP_NAME) }.0.is_null() {
            return Err(Win32Error::AlreadyPinned);
        }

        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: target проверен IsWindow; флаги гарантируют отсутствие
        // активации/фокуса и движения/ресайза; SetWindowPos — потокобезопасная
        // операция над чужим окном.
        unsafe { SetWindowPos(target_hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) }
            .map_err(map_pin_err)?;

        // SAFETY: target — живое окно; window properties видны из любого
        // процесса, SetPropW безопасен с любого потока.
        unsafe {
            SetPropW(target_hwnd, PIN_PROP_NAME, Some(marker_to_handle(marker)))
                .map_err(map_pin_err)?;
        }

        self.pinned.insert(target, marker);
        Ok(())
    }

    /// Снять закрепление с таргета: снять `WS_EX_TOPMOST` (`SetWindowPos`
    /// с `HWND_NOTOPMOST` — парная операция к [`Self::pin`]), снять маркер
    /// `RemovePropW` и очистить книжку. Идемпотентно: незакреплённое/уже
    /// уничтоженное окно — `Ok` без действий (маркер умер вместе с окном,
    /// либо был снят извне). Единственная ошибка — [`Win32Error::PinAccessDenied`].
    pub fn unpin(&mut self, target: usize) -> Result<(), Win32Error> {
        self.pinned.remove(&target);
        let target_hwnd = hwnd_from_usize(target);
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: SetWindowPos безопасен и для уже уничтоженного окна
        // (просто возвращает ошибку, которую мы здесь игнорируем — unpin
        // мёртвого таргета не ошибка, снимать WS_EX_TOPMOST не с чего).
        let _ = unsafe { SetWindowPos(target_hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags) };
        // SAFETY: SetLastError — потоковый регистр ошибки; RemovePropW
        // безопасен и для несуществующего окна (вернёт NULL + ошибку хэндла).
        unsafe {
            SetLastError(ERROR_SUCCESS);
        }
        // SAFETY: RemovePropW безопасен с любого потока и для чужих окон;
        // NULL («маркера нет» или «окна нет») — не ошибка для нас.
        let result = unsafe { RemovePropW(target_hwnd, PIN_PROP_NAME) };
        match result {
            Ok(_) => Ok(()),
            Err(e) if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) => {
                Err(Win32Error::PinAccessDenied)
            }
            // Окно уничтожено или маркера не было — снимать нечего.
            Err(_) => Ok(()),
        }
    }

    /// Снять все закрепления (выход приложения, гарантированное открепление).
    /// Ошибки игнорируются: на выходе делаем лучшее из возможного, а
    /// `ERROR_ACCESS_DENIED` уже отражён в [`WindowPins::unpin`] для
    /// точечных вызовов.
    pub fn unpin_all(&mut self) {
        let targets: Vec<usize> = self.pinned.keys().copied().collect();
        for target in targets {
            let _ = self.unpin(target);
        }
    }

    /// Закреплён ли таргет в данный момент. Проверяется маркер на самом
    /// окне (а не книжка): маркер виден даже другому экземпляру resticker,
    /// поэтому «уже закреплено» детектится и после аварийного выхода.
    pub fn is_pinned(&self, target: usize) -> bool {
        let hwnd = hwnd_from_usize(target);
        // SAFETY: GetPropW безопасен и для несуществующих/чужих окон
        // (вернёт NULL).
        !unsafe { GetPropW(hwnd, PIN_PROP_NAME) }.0.is_null()
    }

    /// Свежий снимок кэша трекера: если закреплённого окна в нём нет —
    /// проверить, живо ли оно с нашим маркером. Уничтоженное (или с
    /// переиспользованным hwnd) окно снимается из книжки и возвращается
    /// [`PinEvent::TargetDestroyed`]. Окно, которое живо, но снимок не видит
    /// (скрыто/свёрнуто), — закрепление сохраняется.
    pub fn handle_snapshot(&mut self, windows: &[WindowInfo]) -> Vec<PinEvent> {
        let present: HashSet<usize> = windows.iter().map(|w| w.hwnd).collect();
        let mut destroyed = Vec::new();
        for (&target, &marker) in &self.pinned {
            if present.contains(&target) {
                continue;
            }
            if !window_still_pinned(target, marker) {
                destroyed.push(target);
            }
        }
        let mut events = Vec::with_capacity(destroyed.len());
        for target in destroyed {
            self.pinned.remove(&target);
            events.push(PinEvent::TargetDestroyed { target });
        }
        events
    }

    /// Переместить/изменить размер закреплённого окна (SPEC.md §5.2:
    /// «перемещение — как у обычного стикера, свободное; изменение размера
    /// — в рамках возможностей самого окна, как при перетаскивании его
    /// собственной границы»). `rect` — физические px виртуального
    /// десктопа, тот же перевод, что у `WindowInfo::rect`. Z-order не
    /// трогается (`SWP_NOZORDER` — окно уже топовое из `pin`), фокус тоже
    /// (`SWP_NOACTIVATE`) — драг стикера не должен красть фокус у другого
    /// приложения.
    ///
    /// Не проверяет `is_pinned`/книжку — вызывающий (координатор) уже знает,
    /// что таргет закреплён (иначе для него не было бы `Placement` живого
    /// стикера-окна); `SetWindowPos` на мёртвом окне просто вернёт ошибку.
    /// Windows сам ограничивает итоговый размер минимумом/максимумом окна
    /// (SPEC: «в рамках возможностей окна») — clamp на нашей стороне не
    /// нужен, `SetWindowPos` не проваливается на «слишком маленький» размер,
    /// просто применяет ближайший допустимый.
    pub fn move_resize(
        &self,
        target: usize,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> Result<(), Win32Error> {
        let target_hwnd = hwnd_from_usize(target);
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: SetWindowPos безопасен для любого HWND, включая мёртвый —
        // вернёт ошибку, не UB; x/y/w/h — обычные пиксельные координаты.
        unsafe { SetWindowPos(target_hwnd, None, x, y, w, h, flags) }.map_err(map_pin_err)
    }
}

/// Окно живо И несёт наш маркер. Мёртвое окно — `false`; живое окно с
/// другим (или отсутствующим) маркером — hwnd переиспользован чужим окном,
/// наш таргет мёртв — тоже `false`.
fn window_still_pinned(target: usize, marker: u64) -> bool {
    let hwnd = hwnd_from_usize(target);
    // SAFETY: IsWindow безопасен для любых значений; GetPropW — для чужих
    // и несуществующих окон (вернёт NULL).
    unsafe {
        IsWindow(Some(hwnd)).as_bool()
            && GetPropW(hwnd, PIN_PROP_NAME).0 == marker_to_handle(marker).0
    }
}

/// Свести ошибку Win32 с понятной диагностикой (ROADMAP.md M6, UIPI):
/// `ERROR_ACCESS_DENIED` — это права администратора, а не случайный сбой.
fn map_pin_err(e: windows::core::Error) -> Win32Error {
    if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
        Win32Error::PinAccessDenied
    } else {
        Win32Error::Win32(e)
    }
}

fn hwnd_from_usize(hwnd: usize) -> HWND {
    HWND(hwnd as *mut core::ffi::c_void)
}

/// Маркер вызывающего кода → значение window property. `HANDLE` внутри —
/// просто числовое значение (как `HWND`), не настоящий хэндл: `SetPropW`/
/// `GetPropW` не разыменовывают его, годится любой `usize`. На 32-битной
/// сборке (не таргет resticker, но на всякий случай) усечение маркера до
/// младших 32 бит — не проблема: маркер используется только для сравнения
/// «свой/чужой», не как уникальный на всё пространство `u64` идентификатор.
fn marker_to_handle(marker: u64) -> HANDLE {
    HANDLE(marker as usize as *mut core::ffi::c_void)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{
        ERROR_CLASS_ALREADY_EXISTS, GetLastError, LPARAM, LRESULT, WPARAM,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW,
        WS_OVERLAPPED,
    };
    use windows::core::w;

    /// Числовой ключ HWND (тестовый аналог `hwnd.0 as usize`).
    fn key(h: HWND) -> usize {
        h.0 as usize
    }

    /// Скрытое окно текущего тест-потока (тот же паттерн, что
    /// `TestWindow` в input.rs): нити сообщений не требует — для
    /// `IsWindow`/`SetPropW`/`SetWindowPos`/`DestroyWindow` помп не нужен.
    struct TestWindow(HWND);

    impl TestWindow {
        fn create() -> Self {
            // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
            let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(test_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: w!("resticker_window_pin_test"),
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
                    w!("resticker_window_pin_test"),
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

    fn info(hwnd: usize) -> WindowInfo {
        WindowInfo {
            hwnd,
            ..Default::default()
        }
    }

    /// Произвольный опознавательный маркер для теста (координатор передаёт
    /// хэш `Uuid` стикера — здесь просто константа, значение не важно).
    const MARKER: u64 = 0xC0FF_EE12_3456_7890;
    const MARKER_B: u64 = 0xDEAD_BEEF_0000_0001;

    /// Флаг `WS_EX_TOPMOST` окна.
    fn is_topmost(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW};
        // SAFETY: чтение стиля живого окна.
        (unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32
            & windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST.0)
            != 0
    }

    #[test]
    fn pin_sets_marker_and_topmost() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        assert!(!pins.is_pinned(key(target.0)));
        assert!(!is_topmost(target.0), "до pin не topmost");

        pins.pin(MARKER, key(target.0))
            .expect("пин скрытого окна должен работать");
        assert!(pins.is_pinned(key(target.0)));
        assert!(is_topmost(target.0), "pin выставляет WS_EX_TOPMOST");
        // SAFETY: маркер только что поставлен этим же тестом.
        let marker = unsafe { GetPropW(target.0, PIN_PROP_NAME) };
        assert_eq!(
            marker.0,
            marker_to_handle(MARKER).0,
            "маркер сохранён как есть"
        );
    }

    #[test]
    fn double_pin_same_target_is_rejected() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("первичный пин");
        let err = pins.pin(MARKER_B, key(target.0));
        match err {
            Err(Win32Error::AlreadyPinned) => {}
            Err(e) => panic!("ожидался AlreadyPinned, получено: {e}"),
            Ok(_) => panic!("повторный пин того же окна должен отвергаться"),
        }
    }

    #[test]
    fn pin_dead_target_is_rejected() {
        let target = TestWindow::create();
        let dead = key(target.0);
        drop(target);
        let mut pins = WindowPins::new();
        match pins.pin(MARKER, dead) {
            Err(Win32Error::PinWindowGone) => {}
            Err(e) => panic!("ожидался PinWindowGone, получено: {e}"),
            Ok(_) => panic!("пин мёртвого окна должен отвергаться"),
        }
    }

    #[test]
    fn unpin_removes_marker_and_topmost_and_allows_repin() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        pins.unpin(key(target.0)).expect("unpin");
        assert!(!pins.is_pinned(key(target.0)));
        assert!(!is_topmost(target.0), "unpin снимает WS_EX_TOPMOST");

        // Идемпотентность: повторный unpin не ошибка.
        pins.unpin(key(target.0)).expect("повторный unpin");

        // Маркер снят — можно пинить заново.
        pins.pin(MARKER_B, key(target.0))
            .expect("повторный пин после unpin");
        assert!(pins.is_pinned(key(target.0)));
    }

    #[test]
    fn unpin_dead_target_is_ok() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        let dead = key(target.0);
        drop(target);
        // Окно уничтожено — маркер умер с ним, снимать нечего, это не ошибка.
        pins.unpin(dead).expect("unpin мёртвого таргета");
    }

    #[test]
    fn unpin_all_clears_all_pins() {
        let target_a = TestWindow::create();
        let target_b = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target_a.0)).expect("пин A");
        pins.pin(MARKER_B, key(target_b.0)).expect("пин B");
        pins.unpin_all();
        assert!(!pins.is_pinned(key(target_a.0)));
        assert!(!pins.is_pinned(key(target_b.0)));
    }

    #[test]
    fn move_resize_moves_and_resizes_pinned_window() {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");

        pins.move_resize(key(target.0), 10, 20, 300, 150)
            .expect("move_resize закреплённого окна");

        let mut rect = RECT::default();
        // SAFETY: target — живое окно текущего потока.
        unsafe { GetWindowRect(target.0, &mut rect) }.expect("GetWindowRect");
        assert_eq!((rect.left, rect.top), (10, 20));
        assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (300, 150));
        // Пин не тронут: z-order/маркер — SWP_NOZORDER, move_resize не
        // трогает WindowPins::pinned вовсе.
        assert!(pins.is_pinned(key(target.0)));
    }

    #[test]
    fn move_resize_dead_target_errors() {
        let target = TestWindow::create();
        let dead = key(target.0);
        drop(target);
        let pins = WindowPins::new();
        assert!(pins.move_resize(dead, 0, 0, 100, 100).is_err());
    }

    #[test]
    fn snapshot_with_target_present_keeps_pin() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        assert!(pins.handle_snapshot(&[info(key(target.0))]).is_empty());
        assert!(pins.is_pinned(key(target.0)));
    }

    #[test]
    fn snapshot_alive_but_hidden_target_keeps_pin() {
        // Окно живо и помечено, но снимок его не видит (скрыто/свёрнуто) —
        // это не уничтожение, закрепление сохраняется.
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        let events = pins.handle_snapshot(&[]);
        assert!(
            events.is_empty(),
            "живое окно вне снимка — не DESTROY: {events:?}"
        );
        assert!(pins.is_pinned(key(target.0)));
    }

    #[test]
    fn snapshot_after_destroy_emits_event_and_clears() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        let dead = key(target.0);
        drop(target);

        let events = pins.handle_snapshot(&[]);
        assert_eq!(
            events,
            vec![PinEvent::TargetDestroyed { target: dead }],
            "уничтожение закреплённого окна должно эмитить событие"
        );
        assert!(!pins.is_pinned(dead));

        // Повторный снимок больше не эмитит (книжка очищена).
        assert!(pins.handle_snapshot(&[]).is_empty());
    }

    #[test]
    fn snapshot_ignores_unpinned_window_destroyed() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        drop(target);
        // Незакреплённое окно уничтожено — событий нет.
        assert!(pins.handle_snapshot(&[]).is_empty());
    }

    #[test]
    fn snapshot_detects_reused_hwnd() {
        // hwnd переиспользован другим окном: живой hwnd без нашего маркера —
        // таргет мёртв, закрепление снимается. Имитация: маркер снят, окно
        // живо и числится в книжке.
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        pins.unpin(key(target.0)).expect("снятие маркера");
        // Вернуть таргет в книжку вручную — сценарий «маркер исчез извне,
        // окно живо».
        pins.pinned.insert(key(target.0), MARKER);

        let events = pins.handle_snapshot(&[]);
        assert_eq!(
            events,
            vec![PinEvent::TargetDestroyed {
                target: key(target.0)
            }],
            "живой hwnd без маркера — переиспользование, таргет мёртв"
        );
    }

    /// Реальное видимое top-level окно на СВОЁМ потоке-пампе (тот же
    /// паттерн, что `RealWindow` в window_tracker.rs): без помпа сообщений
    /// кэш трекера такое окно не увидит, а он нужен для интеграционного
    /// сценария «снимки трекера → WindowPins».
    struct RealWindow {
        hwnd: HWND,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl RealWindow {
        fn create() -> Self {
            use std::sync::mpsc;
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, MSG, SW_SHOW, ShowWindow, TranslateMessage,
                WS_VISIBLE,
            };

            struct SendHwnd(HWND);
            unsafe impl Send for SendHwnd {}

            let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
            let thread = std::thread::spawn(move || {
                // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
                let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(real_wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: w!("resticker_window_pin_real_test"),
                    ..Default::default()
                };
                // SAFETY: wc заполнена корректно; повторная регистрация
                // (параллельные тесты) — не ошибка.
                if unsafe { RegisterClassExW(&wc) } == 0 {
                    // SAFETY: осмысленна сразу после провалившегося вызова.
                    let err = unsafe { GetLastError() };
                    assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
                }
                // SAFETY: все аргументы — валидные константы/только что
                // зарегистрированный класс; окно видимое, реальное для кэша.
                let hwnd = unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("resticker_window_pin_real_test"),
                        w!("resticker window_pin test"),
                        WS_OVERLAPPED | WS_VISIBLE,
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
                // SAFETY: hwnd только что создано этим потоком.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
                ready_tx.send(SendHwnd(hwnd)).expect("получатель ещё жив");

                let mut msg = MSG::default();
                // SAFETY: стандартный цикл сообщений для окна этого потока.
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            });
            let hwnd = ready_rx.recv().expect("поток тестового окна не упал").0;
            Self {
                hwnd,
                thread: Some(thread),
            }
        }
    }

    impl Drop for RealWindow {
        fn drop(&mut self) {
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
            // SAFETY: self.hwnd — окно потока-пампа; PostMessage безопасен
            // и для уже уничтоженного окна.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    unsafe extern "system" fn real_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::{PostQuitMessage, WM_DESTROY};
        if msg == WM_DESTROY {
            // SAFETY: стандартный вызов из обработчика WM_DESTROY — иначе
            // GetMessageW этого потока не завершился бы после DestroyWindow.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Скармливать снимки трекера в `pred` до успеха, максимум ~30 с
    /// (10 попыток × 3 с): на занятом десктопе снимки приходят и из-за
    /// постороннего шума, ждать ровно одно сообщение недостаточно
    /// (тот же паттерн, что `recv_until` в window_tracker.rs).
    fn feed_until<T>(
        rx: &std::sync::mpsc::Receiver<crate::window_tracker::WindowEvent>,
        mut pred: impl FnMut(&[WindowInfo]) -> Option<T>,
    ) -> Option<T> {
        for _ in 0..10 {
            use crate::window_tracker::WindowEvent;
            use std::time::Duration;
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(WindowEvent::Changed(windows)) => {
                    if let Some(v) = pred(&windows) {
                        return Some(v);
                    }
                }
                Err(_) => return None,
            }
        }
        None
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_pin -- --ignored"]
    fn tracker_feed_detects_destroy_of_pinned_window() {
        use crate::window_tracker::WindowTracker;

        let (tracker, rx) = WindowTracker::start().expect("создание трекера");
        tracker.set_mask_needed(true);
        let win = RealWindow::create();
        let mut pins = WindowPins::new();
        let target = win.hwnd.0 as usize;

        // Дождаться таргета в кэше трекера и закрепить его.
        let seen = feed_until(&rx, |windows| {
            windows.iter().any(|w| w.hwnd == target).then_some(())
        });
        assert!(seen.is_some(), "тестовое окно должно попасть в кэш трекера");
        pins.pin(MARKER, target).expect("пин реального окна");
        assert!(pins.is_pinned(target));

        // Уничтожить таргет: EVENT_OBJECT_DESTROY → снимок трекера без окна →
        // TargetDestroyed из WindowPins (единственная регистрация хука —
        // в трекере, см. шапку модуля).
        drop(win);
        let destroyed = feed_until(&rx, |windows| {
            pins.handle_snapshot(windows).into_iter().find(|e| {
                matches!(
                    e,
                    PinEvent::TargetDestroyed {
                        target: t
                    } if *t == target
                )
            })
        });
        assert!(
            destroyed.is_some(),
            "не дождались TargetDestroyed после уничтожения закреплённого окна"
        );
        assert!(!pins.is_pinned(target), "книжка очищена после уничтожения");
    }
}
