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
//!
//! Редизайн пинов (задача 2 из 6): поверх базовых `pin`/`unpin` модуль
//! теперь несёт примитивы двух режимов закрепления и двух блокировок:
//!
//! * **Полный topmost vs соседский слот** — [`WindowPins::enforce_slot`]
//!   ставит окно непосредственно над заданным соседом (или на верх полосы
//!   при `None`); [`WindowPins::surface_topmost_temporarily`] временно
//!   поднимает окно в topmost-полосу, [`WindowPins::restore_slot`] — вернёт
//!   в слот. Кто и когда решает «слот vs topmost» (разрешение соседских
//!   правил в живой HWND, реакция на `EVENT_SYSTEM_FOREGROUND`) — координатор;
//!   здесь только чистые Win32-примитивы, своих хуков модуль не ставит.
//! * **Topmost-backstop** (порт PowerToys «Always On Top») —
//!   [`WindowPins::reassert_topmost_if_needed`]: одноразовая реактивная
//!   коррекция — если у закреплённого окна снят `WS_EX_TOPMOST`, вернуть его
//!   тем же `SetWindowPos`, что и `pin`. Вызывается координатором по смене
//!   переднего плана (событие уже стучится через [`crate::window_tracker`]),
//!   таймеров/непрерывных циклов принуждения не заводит.
//! * **Move-lock** — реактивный snap-back: [`WindowPins::set_move_lock`]
//!   хранит «правильный» прямоугольник per-hwnd (в DWM-координатах
//!   `extended_frame_bounds`, как у снимков трекера),
//!   [`WindowPins::enforce_move_lock`] сравнивает с фактическим и при
//!   расхождении принудительно возвращает окно `SetWindowPos`'ом. Драг
//!   окна пользователем в этот момент не трогается (см. доккомент
//!   `enforce_move_lock`): snap-back происходит один раз после отпускания.
//! * **Interact-lock** — настоящий `EnableWindow(hwnd, FALSE)` через
//!   [`WindowPins::set_interact_lock`]; состояние хранится, чтобы
//!   [`WindowPins::unpin`] мог гарантированно вернуть окну ввод.
//!
//! Обе блокировки — рантайм-состояние, per-hwnd, обе по умолчанию ВЫКЛ.
//! У структуры нет понятия «режим редактирования»: гейтинг вызовов на
//! edit-mode — обязанность вызывающего кода (координатора), см. доккоменты
//! соответствующих методов.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, HWND, RECT, SetLastError,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetAsyncKeyState, GetCapture, VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetDesktopWindow, GetForegroundWindow, GetPropW, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GW_HWNDPREV, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, IsWindow,
    RemovePropW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SetForegroundWindow, SetPropW, SetWindowPlacement, SetWindowPos,
    WINDOWPLACEMENT, WS_EX_TOPMOST,
};
use windows::core::{HRESULT, PCWSTR, w};

use crate::error::Win32Error;
use crate::window_enum::{extended_frame_bounds, WindowInfo};

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
    /// Move-lock: `сырое значение HWND → «правильный» прямоугольник`
    /// (`isize` — это и есть `HWND.0`). Ключ-`isize` вместо `HWND`: HashMap
    /// сравнивает ключи хэшированием, числовой ключ читается однозначнее.
    /// Запись есть ⟺ окно move-locked; прямоугольник захватывается
    /// `extended_frame_bounds` (DWMWA_EXTENDED_FRAME_BOUNDS) в момент
    /// [`WindowPins::set_move_lock`]`(true)` — ТА ЖЕ система координат, что
    /// у `WindowInfo::rect` в снимках трекера, которыми кормится
    /// [`WindowPins::enforce_move_lock`]. Это принципиально: эталон в
    /// `GetWindowRect`-координатах от DWM-границ отличается на константу
    /// (невидимые рамки/тень) — сравнение разных пространств давало вечный
    /// snap-back-цикл (~60 Гц) даже на неподвижном окне. Только рантайм:
    /// в config.json ничего не пишется, на рестарте пусто.
    move_locked: HashMap<isize, RECT>,
    /// Interact-lock: набор сырых HWND, у которых через
    /// [`WindowPins::set_interact_lock`] вызван `EnableWindow(FALSE)`.
    /// Хранится, чтобы [`WindowPins::unpin`] и снос таргета в
    /// [`WindowPins::handle_snapshot`] могли гарантированно вернуть окну
    /// ввод (иначе откреплённое окно осталось бы навсегда неинтерактивным —
    /// ловушка без восстановления). Только рантайм, как и `move_locked`.
    interact_locked: HashSet<isize>,
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
    ///
    /// Открепление заодно освобождает обе блокировки (редизайн пинов):
    /// move-lock просто забывается, а interact-locked окну возвращается
    /// ввод (`EnableWindow(TRUE)`) — иначе откреплённое окно осталось бы
    /// навсегда неинтерактивным. Фокус при этом НЕ трогается (SPEC:
    /// «unpin … no forced refocus»).
    pub fn unpin(&mut self, target: usize) -> Result<(), Win32Error> {
        self.pinned.remove(&target);
        self.move_locked.remove(&(target as isize));
        if self.interact_locked.remove(&(target as isize)) {
            // SAFETY: EnableWindow безопасен и для уже уничтоженного окна
            // (вернёт ошибку, которую игнорируем — снимать не с чего).
            let _ = unsafe { EnableWindow(hwnd_from_usize(target), true) };
            interact_guard::remove_locked(hwnd_from_usize(target));
        }
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
            // Блокировки мертвого окна тоже чистим (запись в `interact_locked`
            // не даст `unpin`-восстановления позже, окна уже нет).
            self.move_locked.remove(&(target as isize));
            if self.interact_locked.remove(&(target as isize)) {
                interact_guard::remove_locked(hwnd_from_usize(target));
            }
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
    ///
    /// Известный Win32-подвох, из-за которого этот метод НЕ сводится к
    /// голому `SetWindowPos`: пока у окна стоит стиль `WS_MAXIMIZE`
    /// (`IsZoomed` == true — окно развёрнуто на весь монитор/borderless-
    /// fullscreen, не обязательно exclusive-fullscreen игра), Win32
    /// молча ИГНОРИРУЕТ явный размер из `SetWindowPos`: maximized-layout
    /// логика перетирает его обратно на развёрнутый прямоугольник, вызов
    /// возвращает успех, но окно визуально не меняется (баг, найденный
    /// живым тестированием: «пин fullscreen-окна не ужимает его до 90%»).
    /// Поэтому здесь `SetWindowPlacement` вместо `SetWindowPos`:
    /// `SetWindowPlacement` умеет одним атомарным вызовом снять
    /// `WS_MAXIMIZE` (через `showCmd = SW_SHOWNOACTIVATE` — обычный,
    /// не-maximized show-command) И сразу поставить `rcNormalPosition` в
    /// целевой прямоугольник — без промежуточного `ShowWindow(SW_RESTORE)`,
    /// который развернул бы окно на его ДОrestore-позицию перед вторым
    /// вызовом (лишний кадр/мерцание). `SW_SHOWNOACTIVATE` — без активации,
    /// как и `SWP_NOACTIVATE` у остальных методов модуля: драг стикера не
    /// должен красть фокус у другого приложения. Для НЕ-maximized окна
    /// `SetWindowPlacement` с тем же `showCmd` — эквивалент `SetWindowPos`
    /// (`SW_SHOWNOACTIVATE` на уже нормальном окне — no-op по show-состоянию,
    /// меняется только `rcNormalPosition`), так что отдельная ветка для
    /// «не maximized» не нужна — один путь работает для обоих случаев.
    pub fn move_resize(
        &self,
        target: usize,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> Result<(), Win32Error> {
        let target_hwnd = hwnd_from_usize(target);
        let placement = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: SW_SHOWNOACTIVATE.0 as u32,
            rcNormalPosition: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            ..Default::default()
        };
        // SAFETY: SetWindowPlacement безопасен для любого HWND, включая
        // мёртвый — вернёт ошибку, не UB; placement заполнена корректно
        // (length обязателен и выставлен выше).
        unsafe { SetWindowPlacement(target_hwnd, &placement) }.map_err(map_pin_err)
    }

    /// Поставить `hwnd` непосредственно НАД `above_hwnd` в z-order'е —
    /// «соседский слот» (редизайн пинов, пункт 3: окно держится над окнами,
    /// подходящими под соседские правила). `None` — `HWND_TOP`, верх обычной
    /// (не-topmost) полосы. Чистый примитив: РАЗРЕШЕНИЕ соседских правил в
    /// конкретный «верхний сосед» — работа координатора, сюда приходит уже
    /// готовый HWND.
    ///
    /// Механика (важная деталь `SetWindowPos`): переданный в
    /// `hWndInsertAfter` хэндл — это окно, НАД которым система НЕ позволит
    /// встать нашему: позиционируемое окно встаёт непосредственно ПОД
    /// переданным. Поэтому чтобы оказаться НАД `above`, в insert-after
    /// передаётся окно, стоящее сейчас НАД `above` (`GW_HWNDPREV`), а если
    /// такого нет (или в полосе выше никого) — `HWND_TOP`.
    ///
    /// Известные ограничения, которые НЕ обходятся здесь (зона
    /// координатора): (1) окно с живым стилем `WS_EX_TOPMOST` размещается по
    /// topmost-полосе, игнорируя относительную позицию — соседский слот
    /// требует НЕ-topmost окна ([`Self::restore_slot`] стиль снимает);
    /// (2) `SetWindowPos` на мёртвом окне молча возвращает ошибку — окно
    /// просто не подвинется. Ошибок наружу метод не даёт: это реактивный
    /// примитив (вызывается на события z-order/фокуса), падать ему не с чем.
    pub fn enforce_slot(&self, hwnd: HWND, above_hwnd: Option<HWND>) {
        let insert_after = match above_hwnd {
            None => HWND_TOP,
            Some(above) => {
                // SAFETY: GetWindow — чтение z-order живых окон, безопасен и
                // для чужих окон/мёртвых хэндлов (вернёт ошибку).
                match unsafe { GetWindow(above, GW_HWNDPREV) } {
                    Ok(prev) if !prev.0.is_null() => prev,
                    _ => HWND_TOP,
                }
            }
        };
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        // SAFETY: позиционирование без движения/ресайза/активации; безопасно
        // для любого HWND, включая чужой и мёртвый.
        let _ = unsafe { SetWindowPos(hwnd, Some(insert_after), 0, 0, 0, 0, flags) };
    }

    /// Временно поднять окно соседского слота в topmost-полосу
    /// (`HWND_TOPMOST` — ставит `WS_EX_TOPMOST`): пока окно держит фокус
    /// переднего плана, оно рисуется поверх ВСЕХ окон (редизайн пинов,
    /// пункт 4 — Alt-Tab-поведение). Тонкая обёртка для координатора,
    /// который вызывает её по `EVENT_SYSTEM_FOREGROUND` из общего хука
    /// [`crate::window_tracker::WindowTracker`] (своего хука этот модуль не
    /// ставит).
    ///
    /// Парный вызов — [`Self::restore_slot`]: стиль `WS_EX_TOPMOST` сам не
    /// снимается, оставить его навсегда значит сломать соседский слот.
    pub fn surface_topmost_temporarily(&self, hwnd: HWND) {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        // SAFETY: то же, что у `pin` — позиционирование без активации.
        let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) };
    }

    /// Вернуть окно в слот после [`Self::surface_topmost_temporarily`]:
    /// снять `WS_EX_TOPMOST` (`HWND_NOTOPMOST`) и заново применить соседский
    /// слот ([`Self::enforce_slot`]). Двухшаговость обязательна: `SetWindowPos`
    /// с конкретным окном в insert-after НЕ снимает topmost-стиль, а окно со
    /// стилем позиционируется по topmost-полосе, игнорируя соседа — один
    /// вызов `enforce_slot` после временного подъёма просто не сработал бы.
    ///
    /// Годится и как первичный переход «полный topmost → соседский слот»
    /// (например, когда в режиме редактирования окну назначают соседские
    /// правила): семантика та же — снять topmost и встать над соседом.
    pub fn restore_slot(&self, hwnd: HWND, above_hwnd: Option<HWND>) {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        // SAFETY: снятие topmost без движения/активации; безопасно для
        // любого HWND.
        let _ = unsafe { SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags) };
        self.enforce_slot(hwnd, above_hwnd);
    }

    /// Реактивный backstop-проверка topmost (порт механики PowerToys
    /// «Always On Top»): если `hwnd` жив и у него снят `WS_EX_TOPMOST` —
    /// вернуть окно в topmost-полосу тем же одноразовым `SetWindowPos`, что и
    /// [`Self::pin`] (без движения/ресайза/активации). Возвращает `true`, если
    /// корректирующий вызов реально выдан и применился, `false` — если
    /// окно мёртвое, уже topmost, или `SetWindowPos` не сработал (например,
    /// UIPI-отказ на окне с повышенными правами — окно просто остаётся как
    /// было, паниковать/повторять бессмысленно).
    ///
    /// Это НЕ таймер и не непрерывный цикл принуждения: вызов приходит
    /// ТОЛЬКО реактивно, из координатора, по событию смены переднего плана —
    /// тот же модель, что у PowerToys (там проверка стоит в обработчике
    /// `EVENT_OBJECT_FOCUS`: «если у закреплённого окна больше нет
    /// WS_EX_TOPMOST — поставить снова», github.com/microsoft/PowerToys
    /// AlwaysOnTop.cpp `HandleWinHookEvent`). Именно одноразовая коррекция
    /// «проверил стиль → вернул», а не постоянная борьба с ОС, делает схему
    /// надёжной: непрерывные реактивные циклы (как у прежнего
    /// move-lock/z-order-обслуживания resticker на каждом тике трекера) —
    /// источник джиттера и хрупкости, который этот порт и убирает.
    ///
    /// Чистый примитив: НЕ проверяет книжку `pinned` — вызывающий
    /// (координатор) зовёт её только для окон, которые сам считает
    /// закреплёнными (та же конвенция, что у `enforce_slot`/`move_resize`).
    pub fn reassert_topmost_if_needed(&self, hwnd: HWND) -> bool {
        // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return false;
        }
        // SAFETY: GetWindowLongPtrW — чтение стиля живого/чужого окна.
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        if ex & WS_EX_TOPMOST.0 != 0 {
            return false;
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: окно проверено IsWindow выше; флаги — без активации/движения/
        // ресайза; SetWindowPos потокобезопасен для чужого окна.
        unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) }.is_ok()
    }

    /// Включить/выключить move-lock для `hwnd` (редизайн пинов, блокировка
    /// №1). Состояние хранится per-hwnd в `move_locked` (см. доккомент
    /// поля): при `locked == true` текущие DWM-границы окна
    /// (`extended_frame_bounds`) становятся эталоном для snap-back'а в
    /// [`Self::enforce_move_lock`]. Мёртвое окно (или ошибка чтения границ)
    /// — состояние не создаётся вовсе.
    ///
    /// ВАЖНО для вызывающего кода: у структуры нет понятия «режим
    /// редактирования». Пока редактирование активно, вызывающий обязан
    /// НЕ дёргать `enforce_move_lock` (SPEC: блокировки приостановлены в
    /// edit-mode). Если окно в это время двигалось/ресайзилось, перед
    /// возобновлением принудительного контроля эталон устарел — вызвать
    /// `set_move_lock(hwnd, true)` заново, иначе первый же
    /// [`Self::enforce_move_lock`] откатит окно на до-edit позицию.
    pub fn set_move_lock(&mut self, hwnd: HWND, locked: bool) {
        let key = hwnd.0 as isize;
        if locked {
            // SAFETY: IsWindow безопасен для любых значений.
            if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                // Эталон берётся в той же системе координат, что и rect в
                // снимках трекера (`extended_frame_bounds`,
                // DWMWA_EXTENDED_FRAME_BOUNDS) — иначе сравнение в
                // enforce_move_lock с GetWindowRect-эталоном расходилось бы
                // на константу (невидимые рамки/тень), и каждый снимок
                // давал бы snap-back, а snap-back сам порождал location-change
                // → вечный цикл перестановок окна даже на неподвижном окне.
                let dwm = extended_frame_bounds(hwnd);
                if dwm.w != 0 || dwm.h != 0 {
                    self.move_locked.insert(
                        key,
                        RECT {
                            left: dwm.x,
                            top: dwm.y,
                            right: dwm.x + dwm.w,
                            bottom: dwm.y + dwm.h,
                        },
                    );
                    return;
                }
            }
            // Окна нет (или границы не отдались) — блокировать нечего.
            self.move_locked.remove(&key);
        } else {
            self.move_locked.remove(&key);
        }
    }

    /// Реактивная проверка move-lock (редизайн пинов, блокировка №1):
    /// вызывать, когда трекер сообщил location-change для move-locked окна
    /// (и ТОЛЬКО вне режима редактирования — гейтинг на вызывающем, см.
    /// [`Self::set_move_lock`]). Если окно move-locked и `current_rect`
    /// расходится с хранимым эталоном — `SetWindowPos` обратно (snap-back)
    /// и `true`; если заблокированное окно не двигалось — эталон обновляется
    /// на `current_rect` (равенство — фактически no-op) и `false`; если окно
    /// не заблокировано или уже уничтожено — `false` (уничтоженное заодно
    /// вычищается из `move_locked`).
    ///
    /// Сравнение — по полному `RECT`, то есть snap-back ловит и движение, и
    /// изменение размера (строжайшая трактовка «окно нельзя двигать»).
    /// `current_rect` — физические px виртуального десктопа, тот же перевод,
    /// что у `WindowInfo::rect` в снимках трекера.
    ///
    /// Окно, которое В ЭТОТ МОМЕНТ тащит пользователь (`WM_ENTERSIZEMOVE` —
    /// модальный цикл перетаскивания, [`user_is_dragging_window`]): вызов
    /// НЕ возвращает его на эталон, а возвращает `false` — пропускает tick.
    /// Иначе живой OS-драг (настоящий заголовок, не edit-mode) дрался бы с
    /// snap-back'ом каждый кадр снимка трекера (~16 мс): окно двигает мышь
    /// пользователя, а приложение на каждом тике возвращает его назад —
    /// видимая тряска/телепорты (репорт 2026-08-17). Как только драг
    /// закончится (кнопка отпущена, захват снят), следующий же enforce
    /// увидит расхождение и ровно один раз вернёт окно на эталон —
    /// гарантия «заблокированное окно никуда не уезжает» сохраняется,
    /// просто без борьбы с рукой пользователя в реальном времени.
    pub fn enforce_move_lock(&mut self, hwnd: HWND, current_rect: RECT) -> bool {
        let key = hwnd.0 as isize;
        let Some(&good) = self.move_locked.get(&key) else {
            return false; // не заблокировано — snap-back не наш клиент
        };
        // SAFETY: IsWindow безопасен для любых значений.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            // Окно уничтожено — блокировать нечего, чистим, чтобы состояние
            // не копилось (снос через handle_snapshot не обязателен).
            self.move_locked.remove(&key);
            return false;
        }
        if good == current_rect {
            self.move_locked.insert(key, current_rect);
            return false;
        }
        if user_is_dragging_window(hwnd) {
            // Живой драг в процессе — не бороться с ним покадрово; первый же
            // вызов после `WM_EXITSIZEMOVE` вернёт окно на эталон.
            return false;
        }
        // Эталон — в DWM-координатах (`extended_frame_bounds`, как в снимках
        // трекера), а `SetWindowPos` работает в `GetWindowRect`-координатах.
        // Смещение между системами константно (метрики рамки окна от позиции
        // не зависят) — считаем его прямо сейчас и переводим эталон в
        // GetWindowRect-пространство.
        let mut gwr = RECT::default();
        // SAFETY: окно живо (проверка выше); GetWindowRect — чтение экранного
        // прямоугольника, безопасно и для чужих окон.
        if unsafe { GetWindowRect(hwnd, &mut gwr) }.is_err() {
            // Окно умерло между проверкой и чтением — snap-back не наш клиент,
            // чистим, чтобы не копилось.
            self.move_locked.remove(&key);
            return false;
        }
        let dwm = extended_frame_bounds(hwnd);
        let dx = dwm.x - gwr.left;
        let dy = dwm.y - gwr.top;
        let dw = dwm.w - (gwr.right - gwr.left);
        let dh = dwm.h - (gwr.bottom - gwr.top);
        let good_w = good.right - good.left;
        let good_h = good.bottom - good.top;
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: окно живо; SetWindowPos потокобезопасен для чужого окна,
        // флаги исключают активацию/смену z-order. Цель — GetWindowRect,
        // при которой DWM-границы окна совпадут с эталоном `good`.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                good.left - dx,
                good.top - dy,
                good_w - dw,
                good_h - dh,
                flags,
            )
        };
        true
    }

    /// Включить/выключить interact-lock для `hwnd` (редизайн пинов,
    /// блокировка №2): настоящий `EnableWindow(hwnd, FALSE)` — в
    /// заблокированное окно не доходят ни клики, ни клавиатура. Состояние
    /// per-hwnd — в `interact_locked` (см. доккомент поля): оно нужно
    /// [`Self::unpin`], чтобы гарантированно вернуть окну ввод при
    /// откреплении. Визуальный индикатор поверх заблокированного окна —
    /// рендеринг-забота другой задачи; здесь только сам механизм.
    ///
    /// Гейтинг на edit-mode — на вызывающем, как и у move-lock (в
    /// edit-mode блокировки приостановлены, SPEC).
    ///
    /// Решение по краевому случаю «блокируем окно, которое сейчас держит
    /// фокус»: Win32 НЕ двигает фокус сам при `EnableWindow(FALSE)` —
    /// фокус остаётся на заблокированном окне, и ввод с клавиатуры молча
    /// пропадает (окно его не получает, другие окна — тоже), пока
    /// пользователь не кликнет куда-нибудь. Мы делаем одну best-effort
    /// попытку снять фокус с окна (`SetForegroundWindow(GetDesktopWindow())`,
    /// результат игнорируется — у фонового процесса Windows вправе
    /// отказать); `SetFocus` здесь заведомо бесполезен: он требует окна,
    /// привязанного к очереди ВЫЗЫВАЮЩЕГО потока, а таргет — чужое окно
    /// другого потока. Если ОС отказала и фокус остался на заблокированном
    /// окне — последствие ограничено: ввод «молчит» до первого клика, без
    /// краша и потери данных. В реальном потоке resticker блокировка
    /// включается при выходе из edit-mode, когда таргет фокуса НЕ держит
    /// (фокус у оверлея), поэтому краевой случай практически не
    /// достигается — но задокументирован на случай прямого вызова.
    ///
    /// ПОБОЧНЫЙ ЭФФЕКТ WS_DISABLED и его устранение: клик по заблокированному
    /// окну заставил бы Windows сыграть системный «динг» — это встроенное
    /// поведение ОС для disabled top-level окон (окно не получает вообще
    /// никакого сообщения — ни `WM_MOUSEACTIVATE`, ни `WM_LBUTTONDOWN`;
    /// система сама обрабатывает клик и играет звук, проверено живым тестом
    /// `interact_guard_swallows_real_click_on_locked_window`). Механизм,
    /// который этот звук глушит, — [`interact_guard`]: глобальный
    /// `WH_MOUSE_LL`-хук проглатывает клики, попадающие в прямоугольник
    /// заблокированного окна, ДО системного input-routing. Здесь (на
    /// `locked == true`) окно регистрируется в хуке, на `false` — снимается.
    /// Почему это нельзя сделать перехватом сообщений в wndproc — доккомент
    /// модуля `interact_guard`.
    pub fn set_interact_lock(&mut self, hwnd: HWND, locked: bool) {
        let key = hwnd.0 as isize;
        if locked {
            // SAFETY: GetForegroundWindow — безопасное чтение состояния
            // десктопа; сравнение хэндлов — числовое.
            if unsafe { GetForegroundWindow() } == hwnd {
                // SAFETY: GetDesktopWindow всегда валиден; SetForegroundWindow
                // может вернуть FALSE (foreground-lock) — это best-effort,
                // результат сознательно игнорируется (см. доккомент).
                let _ = unsafe { SetForegroundWindow(GetDesktopWindow()) };
            }
            self.interact_locked.insert(key);
        } else {
            self.interact_locked.remove(&key);
        }
        // SAFETY: EnableWindow безопасен с любого потока и для чужого окна;
        // на мёртвом окне просто вернёт ошибку, которую игнорируем.
        let _ = unsafe { EnableWindow(hwnd, !locked) };
        if locked {
            interact_guard::add_locked(hwnd);
        } else {
            interact_guard::remove_locked(hwnd);
        }
    }
}

/// Поглощение кликов по interact-locked окнам (побочный эффект блокировки
/// №2): системный «динг» при клике по заблокированному окну.
///
/// МЕХАНИЗМ БИПА (подтверждён живым тестом `interact_guard_swallows_real_click_on_locked_window`,
/// запуск вручную — см. его доккомент): `EnableWindow(hwnd, FALSE)` ставит
/// `WS_DISABLED`. Клик по такому top-level окну СИСТЕМА обрабатывает сама:
/// hit-test и активация обходят окно (ему НЕ приходят ни `WM_NCHITTEST`, ни
/// `WM_MOUSEACTIVATE`, ни `WM_LBUTTONDOWN` — проверено сообщениями wndproc),
/// клик выбрасывается, а win32k играет системный звук. Это поведение ОС,
/// resticker его не вызывает (`MessageBeep`/`Beep` в workspace не
/// встречаются); оно целиком провоцируется самим disabled-состоянием.
///
/// Почему фикс именно хук: раз окно не получает вообще никакого сообщения,
/// перехватить бип в wndproc НЕЧЕМ — ни у своего окна (сообщения нет), ни у
/// чужого (Notepad и т.п., смена wndproc через `SetWindowLongPtrW(GWLP_WNDPROC)`
/// для чужого процесса и так запрещена — `ERROR_ACCESS_DENIED`). Единственная
/// точка, где клик ещё можно убрать ДО системной обработки — глобальный
/// низкоуровневый хук мыши `WH_MOUSE_LL` на собственном потоке с помпом
/// сообщений: если нажатие кнопки пришлось в экранный прямоугольник
/// interact-locked окна и это окно — реальная цель клика в этой точке (верхнее
/// видимое НЕ-transparent окно z-order, содержащее точку), хук возвращает
/// ненулевое значение. Событие выбрасывается из input-очереди ЕЩЁ ДО того, как
/// win32k начнёт hit-testing: ни бипа, ни попытки активации, ни доставки клика
/// не происходит вовсе (проверено тем же тестом — окно не получает ни одного
/// сообщения клика/активации при активном хуке). `EnableWindow(FALSE)` при этом
/// остаётся главным механизмом блокировки (клавиатура и все сообщения), а хук
/// закрывает только единственный случай, где система запищала бы —
/// «пользователь кликнул заблокированное окно».
///
/// Жизненный цикл — refcount по содержимому множества: хук ставится, когда
/// появляется первое interact-locked окно, и снимается (с завершением потока)
/// после последнего unlock/unpin/сноса — «промпт-снятие» на выходе из
/// блокировки соблюдено. Состояние — process-global (`OnceLock`): глобальный
/// хук может быть ровно один, а `WindowPins` в приложении один.
mod interact_guard {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Mutex, OnceLock};

    use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetDesktopWindow, GetMessageW, GetWindow, GetWindowLongPtrW,
        GetWindowRect, GW_CHILD, GW_HWNDNEXT, GWL_EXSTYLE, IsWindowVisible, MSG, MSLLHOOKSTRUCT,
        PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL,
        WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN,
        WM_MBUTTONUP, WM_QUIT, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
        WM_XBUTTONUP, WS_EX_TRANSPARENT,
    };

    use super::HWND;

    struct GuardState {
        locked: Mutex<HashSet<isize>>,
        hook: Mutex<Option<ActiveHook>>,
    }

struct ActiveHook {
        tid: u32,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    static STATE: OnceLock<GuardState> = OnceLock::new();

    fn state() -> &'static GuardState {
        STATE.get_or_init(|| GuardState {
            locked: Mutex::new(HashSet::new()),
            hook: Mutex::new(None),
        })
    }

    /// Кнопка мыши сейчас «поглощена» (см. `SWALLOWED_DOWN`). Отдельный атомик:
    /// в колбэке хука нельзя блокироваться на `state().locked`, а для
    /// down/up-пар достаточно одного флага (см. `interact_mouse_proc`).
    static SWALLOWED_DOWN: AtomicBool = AtomicBool::new(false);

    /// Зарегистрировать окно как interact-locked; при первом окне — поставить
    /// хук. Не блокирующий, никогда не падает.
    pub(super) fn add_locked(hwnd: HWND) {
        let key = hwnd.0 as isize;
        let need_install = {
            let mut locked = state().locked.lock().unwrap();
            locked.insert(key) && locked.len() == 1
        };
        if need_install {
            install_hook();
        }
    }

    /// Снять регистрацию окна; при последнем окне — снять хук и завершить
    /// поток. Не блокирующий, никогда не падает.
    pub(super) fn remove_locked(hwnd: HWND) {
        let key = hwnd.0 as isize;
        let need_uninstall = {
            let mut locked = state().locked.lock().unwrap();
            locked.remove(&key) && locked.is_empty()
        };
        if need_uninstall {
            uninstall_hook();
        }
    }

    fn install_hook() {
        let mut slot = state().hook.lock().unwrap();
        if slot.is_some() {
            return;
        }
        *slot = start_hook_thread();
    }

    /// Запустить поток-помп и поставить `WH_MOUSE_LL`. Возвращает `None`, если
    /// `SetWindowsHookExW` не сработал (например, сессия без интерактивного
    /// десктопа) — тогда поглощение просто не активно, остальная блокировка
    /// (`EnableWindow(FALSE)`) работает как раньше.
    fn start_hook_thread() -> Option<ActiveHook> {
        let (ready_tx, ready_rx) = mpsc::channel::<Option<u32>>();
        let thread = std::thread::Builder::new()
            .name("resticker-interact-guard".into())
            .spawn({
                move || {
                    // SAFETY: WH_MOUSE_LL вызывается в контексте установившего
                    // ПОТОКА (не инъекция в другие процессы), поэтому lpfn —
                    // обычная функция этого модуля, а hmod — текущий модуль;
                    // dwThreadId = 0 — глобально для сессии.
                    let hook = unsafe {
                        SetWindowsHookExW(
                            WH_MOUSE_LL,
                            Some(interact_mouse_proc),
                            Some(GetModuleHandleW(None).unwrap_or_default().into()),
                            0,
                        )
                    };
                    let hook = match hook {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "interact-lock: не удалось поставить WH_MOUSE_LL — клики по заблокированному окну будут давать системный бип"
                            );
                            let _ = ready_tx.send(None);
                            return;
                        }
                    };
                    let tid = unsafe { GetCurrentThreadId() };
                    let _ = ready_tx.send(Some(tid));
                    let mut msg = MSG::default();
                    // SAFETY: стандартный msg-loop потока-помпа; колбэк хука
                    // вызывается системой синхронно из этого потока между
                    // итерациями GetMessageW, TranslateMessage/DispatchMessageW
                    // здесь не нужны. Выход — по WM_QUIT из uninstall_hook.
                    unsafe {
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
                    }
                    // SAFETY: unhook из потока-владельца хука.
                    let _ = unsafe { UnhookWindowsHookEx(hook) };
                }
            })
            .expect("interact-guard: не удалось создать поток");
        match ready_rx.recv() {
            Ok(Some(tid)) => Some(ActiveHook {
                tid,
                thread: Some(thread),
            }),
            _ => None,
        }
    }

    fn uninstall_hook() {
        let slot = &mut state().hook.lock().unwrap();
        let Some(active) = slot.take() else {
            return;
        };
        let Some(thread) = active.thread else {
            return;
        };
        // SAFETY: WM_QUIT в поток-помп — штатное завершение GetMessageW
        // (вернёт 0), поток снимет хук и выйдет; join дождётся этого.
        unsafe {
            let _ = PostThreadMessageW(active.tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = thread.join();
    }

    /// Диагностика для ignored-тестов: поставлен ли WH_MOUSE_LL сейчас.
    #[cfg(test)]
    pub(super) fn hook_active() -> bool {
        state().hook.lock().unwrap().is_some()
    }

    /// Колбэк `WH_MOUSE_LL`. Ненулевой возврат выбрасывает событие из
    /// input-очереди ещё до системного hit-testing/активации — бипа нет.
    ///
    /// Глотаем только нажатия/отпускания кнопок (не `WM_MOUSEMOVE`):
    /// движение мыши над заблокированным окном должно работать как обычно.
    /// Down/up-пары трекаются `SWALLOWED_DOWN`: если нажатие поглощено, его
    /// отпускание тоже поглощаем, но отпускание после drag, начавшегося вне
    /// заблокированного окна, не трогаем (иначе сломали бы перетаскивание,
    /// завершающееся над заблокированным окном).
    unsafe extern "system" fn interact_mouse_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let msg = wparam.0 as u32;
            if is_down_message(msg) {
                // Снимок множества под коротким локом: клон маленький (обычно
                // 1–3 окна), сам z-order-обход — без удержания лока.
                let locked = state().locked.lock().unwrap().clone();
                // SAFETY: lparam от системы указывает на живую MSLLHOOKSTRUCT
                // на время вызова колбэка (контракт WH_MOUSE_LL).
                let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
                if click_hits_locked_window(ms.pt, &locked) {
                    SWALLOWED_DOWN.store(true, Ordering::Relaxed);
                    return LRESULT(1);
                }
                SWALLOWED_DOWN.store(false, Ordering::Relaxed);
            } else if is_up_message(msg) && SWALLOWED_DOWN.swap(false, Ordering::Relaxed) {
                return LRESULT(1);
            }
        }
        // SAFETY: CallNextHookEx передаёт событие дальше по цепочке хуков.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    fn is_down_message(msg: u32) -> bool {
        matches!(
            msg,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_LBUTTONDBLCLK
                | WM_RBUTTONDBLCLK
                | WM_MBUTTONDBLCLK
        )
    }

    fn is_up_message(msg: u32) -> bool {
        matches!(msg, WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP)
    }

    /// Попадает ли клик в точке `pt` в interact-locked окно из `locked` и
    /// является ли это окно реальной целью клика (см. `is_effective_click_target`).
    /// Чистая функция над переданным множеством — так её можно покрыть
    /// unit-тестами без реального хука.
    fn click_hits_locked_window(pt: POINT, locked: &HashSet<isize>) -> bool {
        for &key in locked {
            let hwnd = HWND(key as *mut core::ffi::c_void);
            if !locked_window_receives_click_at(hwnd, pt) {
                continue;
            }
            if is_effective_click_target(hwnd, pt) {
                return true;
            }
        }
        false
    }

    /// Живое видимое disabled-окно, чей прямоугольник содержит `pt` и которое
    /// не прозрачно для кликов. Двойная фильтрация: отсекает записи-«призраки»
    /// (окно уничтожено или ввод возвращён извне, а запись в множестве ещё
    /// есть) и не глотает клики, которые система и так не доставила бы окну.
    fn locked_window_receives_click_at(hwnd: HWND, pt: POINT) -> bool {
        // SAFETY: IsWindowVisible/IsWindowEnabled/GetWindowLongPtrW/GetWindowRect
        // безопасны для чужих и мёртвых хэндлов.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return false;
            }
            if IsWindowEnabled(hwnd).as_bool() {
                return false;
            }
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TRANSPARENT.0 != 0 {
                return false;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return false;
            }
            pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom
        }
    }

    /// Является ли `target` окном, которому система доставила бы клик в `pt`:
    /// первое видимое НЕ-transparent окно в z-order сверху, содержащее `pt`.
    /// Обход — `GetWindow(desktop, GW_CHILD)` (верх z-order) вниз по
    /// `GW_HWNDNEXT`; `WS_EX_TRANSPARENT`-окна (клик-сквозные, у resticker так
    /// устроены оверлеи) пропускаются — система и сама роутит клик сквозь них
    /// на окно ниже.
    fn is_effective_click_target(target: HWND, pt: POINT) -> bool {
        let mut current = unsafe { GetWindow(GetDesktopWindow(), GW_CHILD) };
        let zorder = std::iter::from_fn(move || match current {
            Ok(hwnd) if !hwnd.0.is_null() => {
                // SAFETY: GetWindow — чтение z-order живого десктопа.
                current = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
                Some(hwnd)
            }
            _ => None,
        });
        target_wins_click(target, pt, zorder)
    }

    /// Чистое решение «кто выигрывает клик в точке `pt`»: перебираем окна
    /// z-order сверху вниз (переданы итератором — для тестируемости без
    /// живого десктопа). Как только встречаем `target` — он верхний, клик
    /// уходит в него (true). Раньше него встречаем видимый НЕ-transparent
    /// блокер с точкой в прямоугольнике — клик уходит в блокер (false).
    fn target_wins_click(
        target: HWND,
        pt: POINT,
        zorder: impl Iterator<Item = HWND>,
    ) -> bool {
        for hwnd in zorder {
            if hwnd == target {
                return true;
            }
            if window_blocks_click_at(hwnd, pt) {
                return false;
            }
        }
        false
    }

    fn window_blocks_click_at(hwnd: HWND, pt: POINT) -> bool {
        // SAFETY: то же, что у `locked_window_receives_click_at`.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return false;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return false;
            }
            if !(pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom) {
                return false;
            }
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            ex & WS_EX_TRANSPARENT.0 == 0
        }
    }

    #[cfg(test)]
    pub(super) mod tests {
        use super::*;
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
        use windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW;

        fn pt(x: i32, y: i32) -> POINT {
            POINT { x, y }
        }

        /// Создать окно interact-guard-теста. Позиции УНИКАЛЬНЫ для каждого
        /// теста (и в стороне от (0,0), где живут окна остальных тестов
        /// модуля) — окна создаются только для проверок прямоугольников/
        /// стилей, сам z-order в unit-тестах НЕ используется (см. ниже).
        fn window_for(x: i32, y: i32) -> crate::window_pin::tests::TestWindow {
            crate::window_pin::tests::TestWindow::create_at_topmost(x, y, 240, 170)
        }

        fn center(hwnd: HWND) -> POINT {
            unsafe {
                let mut r = RECT::default();
                GetWindowRect(hwnd, &mut r).unwrap();
                pt((r.left + r.right) / 2, (r.top + r.bottom) / 2)
            }
        }

        /// Множество с одним hwnd.
        fn locked_with(hwnd: HWND) -> HashSet<isize> {
            let mut s = HashSet::new();
            s.insert(hwnd.0 as isize);
            s
        }

        /// Вызвать `click_hits_locked_window` с ПОДМЕНЁННЫМ источником
        /// z-order (вместо живого десктопа): unit-тесты передают собственные
        /// окна в нужном порядке, и результат не зависит от параллельных
        /// тестов/реальных окон десктопа. Живой z-order покрывается отдельным
        /// `#[ignore]`-тестом.
        fn click_with_zorder(pt: POINT, locked: &HashSet<isize>, zorder: Vec<HWND>) -> bool {
            let locked = locked.clone();
            for &hwnd in &zorder {
                if locked.contains(&(hwnd.0 as isize)) {
                    return target_wins_click(hwnd, pt, zorder.iter().copied());
                }
            }
            false
        }

        #[test]
        fn swallows_click_on_locked_window() {
            let win = window_for(140, 140);
            let hwnd = win.0;
            // interact-lock: окно реально disabled (как после set_interact_lock).
            unsafe { let _ = EnableWindow(hwnd, false); };
            let c = center(hwnd);
            let set = locked_with(hwnd);
            assert!(
                click_with_zorder(c, &set, vec![hwnd]),
                "клик по центру interact-locked окна должен поглощаться"
            );
        }

        #[test]
        fn does_not_swallow_outside_locked_rect() {
            let win = window_for(1600, 1600);
            let hwnd = win.0;
            unsafe { let _ = EnableWindow(hwnd, false); };
            let set = locked_with(hwnd);
            assert!(
                !click_hits_locked_window(pt(10, 10), &set),
                "клик вне прямоугольника заблокированного окна не поглощается"
            );
            assert!(
                !click_hits_locked_window(pt(1600 + 30, 1600 + 200), &set),
                "клик под прямоугольником не поглощается"
            );
        }

        #[test]
        fn does_not_swallow_when_enabled_window_covers_locked() {
            let locked = window_for(320, 320);
            unsafe { let _ = EnableWindow(locked.0, false); };
            let cover = crate::window_pin::tests::TestWindow::create_at_topmost(330, 330, 120, 120);
            let set = locked_with(locked.0);
            let c = center(cover.0);
            // Злой (enabled) блокер стоит НАД заблокированным в z-order —
            // клик уходит в него.
            assert!(
                !click_with_zorder(c, &set, vec![cover.0, locked.0]),
                "клик по перекрывающему enabled-окну поверх заблокированного не поглощается"
            );
            // Тот же клик при z-order без блокера снова уходит в
            // заблокированное.
            assert!(click_with_zorder(c, &set, vec![locked.0]));
        }

        #[test]
        fn transparent_overlay_does_not_block_swallow() {
            // Оверлей resticker — WS_EX_TRANSPARENT (клик-сквозной): он НЕ
            // должен отменять поглощение клика, уходящего в заблокированное
            // окно под ним (система сама роутит клик сквозь прозрачное окно).
            let locked = window_for(500, 500);
            unsafe { let _ = EnableWindow(locked.0, false); };
            let overlay = crate::window_pin::tests::TestWindow::create_at_topmost(500, 500, 400, 300);
            unsafe {
                let _ = SetWindowLongPtrW(overlay.0, GWL_EXSTYLE, WS_EX_TRANSPARENT.0 as isize);
            }
            let set = locked_with(locked.0);
            let c = center(locked.0);
            assert!(click_with_zorder(c, &set, vec![overlay.0, locked.0]));
        }

        #[test]
        fn does_not_swallow_for_unregistered_disabled_window() {
            // Окно disabled, но НЕ в множестве interact-lock — чужие
            // disabled-окна (модальные диалоги и т.п.) мы не трогаем.
            let win = window_for(1000, 1000);
            let hwnd = win.0;
            unsafe { let _ = EnableWindow(hwnd, false); };
            let empty = HashSet::new();
            assert!(!click_hits_locked_window(pt(1120, 1085), &empty));
        }

        #[test]
        fn stale_dead_window_is_ignored() {
            let win = window_for(1200, 1200);
            let hwnd = win.0;
            let mut set = HashSet::new();
            set.insert(hwnd.0 as isize);
            drop(win); // окно уничтожено — запись-призрак
            assert!(!click_hits_locked_window(pt(1320, 1285), &set));
        }

        #[test]
        #[ignore = "живой z-order десктопа; запуск вручную: cargo test -p rst-win32 interact_guard -- --ignored"]
        fn real_desktop_zorder_swallow() {
            // Сквозная проверка настоящего обхода десктопа: клик по видимому
            // disabled-окну, созданному последним (оно наверху z-order),
            // поглощается. Может флакать при параллельном запуске с другими
            // тестами, создающими topmost-окна — потому и ignored.
            let win = window_for(700, 700);
            let hwnd = win.0;
            unsafe { let _ = EnableWindow(hwnd, false); };
            let set = locked_with(hwnd);
            let c = center(hwnd);
            assert!(click_hits_locked_window(c, &set));
        }
    }
}

/// Пользователь В ЭТОТ МОМЕНТ тащит `hwnd` настоящим OS-драгом — модальный
/// цикл перетаскивания заголовка (`WM_ENTERSIZEMOVE` → `WM_EXITSIZEMOVE`),
/// который удерживает захват мыши (`GetCapture`) на перемещаемом окне.
/// Эвристика, а не хук: у resticker нет подкласса чужого окна, а сам факт
/// «окно в модальном цикле» наружу не выставляется — поэтому по
/// `GetCapture() == hwnd` (захват модального цикла принадлежит именно
/// перемещаемому окну) + «левая кнопка нажата». Для обычного драга заголовка
/// левой кнопкой оба условия выполняются весь драг целиком. Сценарии вне
/// нашего кейса, где первое условие ложно, не двигают окно и, значит, не
/// порождают location-change — на enforce не влияют.
fn user_is_dragging_window(hwnd: HWND) -> bool {
    // SAFETY: GetCapture/GetAsyncKeyState — потокобезопасные чтения
    // глобального состояния ввода, ресурсов не создают и не требуют; на
    // мёртвом hwnd сравнение хэндлов — простое числовое, безопасно.
    unsafe { GetCapture() == hwnd && GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 }
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
    use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GW_HWNDNEXT, GW_HWNDPREV, GWLP_USERDATA,
        GWL_STYLE, GetWindow, GetWindowLongPtrW, RegisterClassExW, WINDOW_STYLE, WNDCLASSEXW,
        WS_DISABLED, WS_OVERLAPPED, WS_VISIBLE,
    };
    use windows::core::w;

    /// Числовой ключ HWND (тестовый аналог `hwnd.0 as usize`).
    fn key(h: HWND) -> usize {
        h.0 as usize
    }

    /// Скрытое окно текущего тест-потока (тот же паттерн, что
    /// `TestWindow` в input.rs): нити сообщений не требует — для
    /// `IsWindow`/`SetPropW`/`SetWindowPos`/`DestroyWindow` помп не нужен.
    /// `pub(super)`: используется и тестами `interact_guard` (предикат
    /// поглощения кликов).
    pub(super) struct TestWindow(pub(super) HWND);

    impl TestWindow {
        fn create() -> Self {
            Self::create_with(WS_OVERLAPPED)
        }

        /// Видимое окно — только для тестов z-order'а: невидимые окна в
        /// z-order не участвуют. Создаётся и обслуживается тем же
        /// (тестовым) потоком: все z-order-вызовы (`SetWindowPos`/
        /// `GetWindow`) — синхронные SendMessage самому себе, помп не нужен.
        fn create_visible() -> Self {
            Self::create_with(WS_OVERLAPPED | WS_VISIBLE)
        }

        /// Видимое окно в topmost-полосе на явной позиции (тесты
        /// `interact_guard`): topmost гарантирует детерминированный z-order
        /// относительно чужих окон десктопа (оверлеев resticker в т.ч.),
        /// позиция задаётся явно, чтобы параллельные тесты не пересекались.
        pub(super) fn create_at_topmost(x: i32, y: i32, w: i32, h: i32) -> Self {
            let win = Self::create_with(WS_OVERLAPPED | WS_VISIBLE);
            // SAFETY: окно живо; позиционирование без активации/ресайза.
            unsafe {
                let _ = SetWindowPos(
                    win.0,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
            win
        }

        fn create_with(style: WINDOW_STYLE) -> Self {
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
            // класс; окно принадлежит текущему потоку.
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("resticker_window_pin_test"),
                    w!("test"),
                    style,
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

    /// DWM-границы окна как `RECT` — та же система координат, что у
    /// `WindowInfo::rect` в снимках трекера (тестовый аналог того, чем
    /// кормит `enforce_move_lock` координатор).
    fn dwm_rect(hwnd: HWND) -> RECT {
        let r = extended_frame_bounds(hwnd);
        RECT {
            left: r.x,
            top: r.y,
            right: r.x + r.w,
            bottom: r.y + r.h,
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

    /// Снять `WS_EX_TOPMOST` с окна — имитация внешнего вмешательства
    /// (другое topmost-приложение, сброс стиля самим окном), которое
    /// `reassert_topmost_if_needed` обязан обнаружить и исправить.
    fn knock_out_of_topmost(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_NOTOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
        };
        // SAFETY: окно живо (вызывается для живого тестового окна); флаги —
        // только снять topmost, не двигая/не меняя размер.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
        assert!(!is_topmost(hwnd), "имитация выбивания из topmost не сработала");
    }

    #[test]
    fn reassert_is_noop_when_already_topmost() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        assert!(is_topmost(target.0));

        // Уже topmost — коррекция не нужна, ничего не меняется.
        assert!(
            !pins.reassert_topmost_if_needed(target.0),
            "topmost-окно не требует повторного вызова"
        );
        assert!(is_topmost(target.0));
    }

    #[test]
    fn reassert_restores_knocked_out_topmost() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        knock_out_of_topmost(target.0);

        // Стиль сбит извне — backstop обязан вернуть окно в topmost-полосу
        // тем же одноразовым SetWindowPos, что и pin.
        assert!(
            pins.reassert_topmost_if_needed(target.0),
            "сбитый topmost должен восстанавливаться"
        );
        assert!(is_topmost(target.0), "WS_EX_TOPMOST вернулся");

        // Повторная проверка — снова no-op (стиль на месте).
        assert!(!pins.reassert_topmost_if_needed(target.0));
    }

    #[test]
    fn reassert_dead_window_is_noop() {
        let target = TestWindow::create();
        let dead = target.0;
        drop(target);
        let pins = WindowPins::new();
        // Мёртвое окно — безопасный no-op, false без паники (конвенция
        // модуля: «missing elements are not a panic»).
        assert!(!pins.reassert_topmost_if_needed(dead));
    }

    #[test]
    fn reassert_unpinned_window_can_be_restored_as_primitive() {
        // Примитив не знает про книжку `pinned` — он просто приводит стиль
        // переданного окна в порядок. Для закреплённого окна после внешнего
        // сброса стиля (но ДО нашего unpin) backstop обязан работать.
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        knock_out_of_topmost(target.0);
        assert!(pins.reassert_topmost_if_needed(target.0));
        assert!(is_topmost(target.0));
        // Окно осталось в книжке — пин не тронут backstop'ом.
        assert!(pins.is_pinned(key(target.0)));
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
    fn move_resize_restores_maximized_window_to_target_rect() {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsZoomed, SW_MAXIMIZE, ShowWindow};

        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");

        // SAFETY: target — живое окно текущего потока; SW_MAXIMIZE — обычный
        // show-command.
        unsafe {
            let _ = ShowWindow(target.0, SW_MAXIMIZE);
        }
        // SAFETY: чтение состояния живого окна.
        assert!(unsafe { IsZoomed(target.0) }.as_bool(), "окно должно стать maximized");

        pins.move_resize(key(target.0), 10, 20, 300, 150)
            .expect("move_resize maximized-окна");

        // SAFETY: чтение состояния живого окна.
        assert!(
            !unsafe { IsZoomed(target.0) }.as_bool(),
            "move_resize обязан снять WS_MAXIMIZE, иначе Windows игнорирует запрошенный размер"
        );
        let mut rect = RECT::default();
        // SAFETY: target — живое окно текущего потока.
        unsafe { GetWindowRect(target.0, &mut rect) }.expect("GetWindowRect");
        assert_eq!((rect.left, rect.top), (10, 20));
        assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (300, 150));
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

    /// Пройти ВВЕРХ по z-order от `from` до конца полосы (NULL) и проверить,
    /// что `want` встретится по пути (`GW_HWNDPREV` пересекает topmost-полосу,
    /// так что проверка «выше ли `want`» работает для обоих режимов). Кап в 64
    /// шага — защита от патологических ситуаций на очень загруженном десктопе,
    /// не от обычных расстояний (свежее topmost-окно от низа полосы отделяют
    /// обычно 20–40 окон).
    fn window_above_in_walk(from: HWND, want: HWND) -> bool {
        let mut cur = from;
        for _ in 0..64 {
            if cur == want {
                return true;
            }
            // SAFETY: чтение z-order живых окон.
            let Ok(prev) = (unsafe { GetWindow(cur, GW_HWNDPREV) }) else {
                return false;
            };
            if prev.0.is_null() {
                return false;
            }
            cur = prev;
        }
        false
    }

    #[test]
    fn enforce_slot_none_puts_window_on_top_of_band() {
        let bottom = TestWindow::create_visible();
        let above = TestWindow::create_visible(); // создано позже → выше
        let pins = WindowPins::new();

        pins.enforce_slot(bottom.0, None);

        assert!(
            window_above_in_walk(above.0, bottom.0),
            "enforce_slot(None) должен поднять окно на верх обычной полосы"
        );
    }

    #[test]
    fn enforce_slot_places_window_directly_above_neighbor() {
        let bottom = TestWindow::create_visible();
        let above = TestWindow::create_visible();
        let pins = WindowPins::new();

        pins.enforce_slot(bottom.0, Some(above.0));

        // Контракт соседского слота: bottom непосредственно НАД above —
        // в обе стороны (выше above стоит bottom, ниже bottom стоит above).
        // SAFETY: чтение z-order живых окон.
        let prev = unsafe { GetWindow(above.0, GW_HWNDPREV) }.expect("GW_HWNDPREV");
        assert_eq!(
            prev, bottom.0,
            "bottom должен стоять непосредственно над above"
        );
        // SAFETY: чтение z-order живых окон.
        let next = unsafe { GetWindow(bottom.0, GW_HWNDNEXT) }.expect("GW_HWNDNEXT");
        assert_eq!(
            next, above.0,
            "above должен стоять непосредственно под bottom"
        );
    }

    #[test]
    fn surface_then_restore_slot_roundtrip() {
        // Режимный цикл «соседский слот → временный topmost → обратно в
        // слот» (редизайн пинов, пункт 4 — фокус переднего плана).
        let bottom = TestWindow::create_visible();
        let above = TestWindow::create_visible();
        let pins = WindowPins::new();
        pins.enforce_slot(bottom.0, Some(above.0));

        pins.surface_topmost_temporarily(bottom.0);
        assert!(is_topmost(bottom.0), "временный подъём ставит WS_EX_TOPMOST");
        assert!(
            window_above_in_walk(above.0, bottom.0),
            "topmost-окно обязано быть выше обычной полосы"
        );

        pins.restore_slot(bottom.0, Some(above.0));
        assert!(!is_topmost(bottom.0), "restore_slot снимает WS_EX_TOPMOST");
        // SAFETY: чтение z-order живых окон.
        let prev = unsafe { GetWindow(above.0, GW_HWNDPREV) }.expect("GW_HWNDPREV");
        assert_eq!(
            prev, bottom.0,
            "после restore_slot окно снова непосредственно над соседом"
        );
    }

    #[test]
    fn move_lock_snaps_back_to_lock_time_rect() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        // Эталон — DWM-границы в момент блокировки (`extended_frame_bounds`):
        // та же система координат, что у `WindowInfo::rect` в снимках трекера,
        // которыми кормится enforce. `GetWindowRect` от DWM-границ отличается
        // на константу (невидимые рамки/тень) — сравнивать с ним нельзя,
        // это давало вечный snap-back-цикл на неподвижном окне.
        let baseline = dwm_rect(hwnd);
        pins.set_move_lock(hwnd, true);

        // «Внешняя сила» двигает окно (напрямую SetWindowPos, не move_resize —
        // тот был бы нашей операцией).
        // SAFETY: окно живо, флаги исключают активацию/z-order.
        unsafe {
            SetWindowPos(hwnd, None, 40, 50, 100, 100, SWP_NOZORDER | SWP_NOACTIVATE)
        }
        .expect("движение тестового окна");

        // Координатор увидел location-change и передаёт фактический rect —
        // в DWM-координатах, как в снимке трекера.
        let current = dwm_rect(hwnd);
        assert_ne!(current, baseline, "окно реально уехало от эталона");
        assert!(pins.enforce_move_lock(hwnd, current), "snap-back обязан сработать");

        // Окно вернулось на эталонный прямоугольник (позиция и размер).
        let restored = dwm_rect(hwnd);
        assert_eq!(
            restored, baseline,
            "окно возвращено на эталонный rect (позиция и размер)"
        );

        // Окно на месте — повторный enforce ничего не двигает и даёт false.
        assert!(!pins.enforce_move_lock(hwnd, restored));
    }

    #[test]
    fn move_lock_does_not_touch_unlocked_window() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        let mut current = RECT::default();
        // SAFETY: чтение прямоугольника живого окна.
        unsafe { GetWindowRect(hwnd, &mut current) }.expect("GetWindowRect");
        assert!(!pins.enforce_move_lock(hwnd, current));
    }

    #[test]
    fn move_lock_idle_window_does_not_snap() {
        // Регрессия на вечный snap-back-цикл (репорт 2026-08-17): эталон
        // брался в координатах `GetWindowRect`, а сравнение в enforce шло с
        // DWM-границами из снимков трекера — константное расхождение
        // (невидимые рамки) заставляло возвращать окно на каждый снимок, а
        // сам возврат порождал location-change → окно «тряслось» на месте
        // даже без движения. Эталон обязан жить в той же системе координат,
        // что и снимки: тогда неподвижное окно никогда не триггерит snap-back.
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        pins.set_move_lock(hwnd, true);

        // Снимок трекера для неподвижного окна = его текущие DWM-границы.
        let snapshot_rect = dwm_rect(hwnd);
        assert!(
            !pins.enforce_move_lock(hwnd, snapshot_rect),
            "неподвижное окно не должно возвращаться на каждый снимок (вечный цикл)"
        );
        assert_eq!(dwm_rect(hwnd), snapshot_rect, "окно осталось на месте");
    }

    #[test]
    fn move_lock_toggle_off_releases_control() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        pins.set_move_lock(hwnd, true);
        pins.set_move_lock(hwnd, false);

        // SAFETY: окно живо, флаги исключают активацию/z-order.
        unsafe {
            SetWindowPos(hwnd, None, 40, 50, 100, 100, SWP_NOZORDER | SWP_NOACTIVATE)
        }
        .expect("движение тестового окна");
        let mut current = RECT::default();
        // SAFETY: чтение прямоугольника живого окна.
        unsafe { GetWindowRect(hwnd, &mut current) }.expect("GetWindowRect");
        assert!(
            !pins.enforce_move_lock(hwnd, current),
            "снятая блокировка не должна возвращать окно"
        );
        assert_eq!((current.left, current.top), (40, 50));
    }

    #[test]
    fn move_lock_dead_window_cleans_state() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        pins.set_move_lock(hwnd, true);
        drop(target);
        // enforce на мёртвом окне: без паники, без SetWindowPos, false, а
        // состояние вычищено (повторный вызов — тот же путь).
        assert!(!pins.enforce_move_lock(hwnd, RECT::default()));
    }

    /// Реальный OS-драг move-locked окна (репорт 2026-08-17): закрепляем
    /// настоящее видимое окно, включаем move-lock, затем инъекцией реальной
    /// мыши (SendInput) тащим окно за настоящий заголовок и на каждом шаге
    /// кормим [`WindowPins::enforce_move_lock`] тем же rect, что скармливал
    /// бы трекер (`extended_frame_bounds`, как в `maintain_pinned_windows`).
    ///
    /// До фикса каждый такой шаг давал snap-back (тяга-перетяга: окно тащит
    /// мышь, а приложение возвращает его на эталон каждый тик снимка) —
    /// тест это ловит по `snapped == true` во время драга. После фикса
    /// move-lock не дерётся с драгом, а ровно один раз возвращает окно на
    /// эталон ПОСЛЕ отпускания кнопки.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_pin -- --ignored"]
    fn move_lock_does_not_fight_live_drag() {
        use std::time::{Duration, Instant};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, GetCapture, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT,
            MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
            VK_LBUTTON,
        };
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetCursorPos, GetSystemMetrics, GetWindowRect, HWND_TOP, SM_CYCAPTION, SetCursorPos,
            SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, WindowFromPoint,
        };

        use crate::window_enum::extended_frame_bounds;

        /// Один `INPUT`-пакет мыши для `SendInput`. Движение — относительное
        /// (`dx`/`dy`), DPI-независимо: виртуализация координат DPI-неведающего
        /// процесса на относительные дельты не влияет.
        fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }

        #[derive(Debug)]
        struct Step {
            /// `left` окна ДО этого tick'а enforce (позиция, куда ушёл драг).
            window_left: i32,
            /// Сработал ли на этом шаге snap-back (возврат на эталон).
            snapped: bool,
            /// Захват модального цикла принадлежит окну (наш эвристический
            /// признак «пользователь тащит именно это окно»).
            our_capture: bool,
            /// Левая кнопка ещё нажата (этот шаг — внутри драга).
            lbutton_down: bool,
            /// Позиция курсора после инъекции движения (проверка, что
            /// инъекция реально двигает мышь в этом сеансе).
            cursor: (i32, i32),
        }

        let win = RealWindow::create();
        let hwnd = win.hwnd;
        let mut pins = WindowPins::new();

        // Положить окно в известное место ВТОРОГО монитора (физические px
        // виртуального десктопа; второй монитор здесь свободен — на основном
        // может жить полноэкранное приложение, которое съедало бы инъекцию).
        // Позиция подбирается так, чтобы заголовок не перекрывали чужие окна.
        let place = (-1700i32, 500i32);
        // SAFETY: окно живо; флаги исключают активацию/смену полосы z-order.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                place.0,
                place.1,
                200,
                150,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .expect("позиционирование тестового окна");
        std::thread::sleep(Duration::from_millis(200));

        // Точка в заголовке: центр по X, середина по высоте caption.
        let mut baseline = RECT::default();
        // SAFETY: окно живо.
        unsafe { GetWindowRect(hwnd, &mut baseline) }.expect("GetWindowRect");
        let caption_h = unsafe { GetSystemMetrics(SM_CYCAPTION) };
        let cx = baseline.left + (baseline.right - baseline.left) / 2;
        let cy = baseline.top + caption_h / 2;
        // Диагностика: окно, которому достанется клик в точке (cx, cy), —
        // обязано быть нашим, иначе драг не начнётся (полноэкранное чужое
        // окно сверху съест инъекцию). На занятом десктопе (игра/стрим на
        // мониторе) так и есть — тест тогда честно пропускает реальный драг
        // (env busy), а не падает: сам механизм уже покрыт unit-тестами.
        // SAFETY: WindowFromPoint — чтение hwnd под точкой, безопасно.
        let hit = unsafe { WindowFromPoint(POINT { x: cx, y: cy }) };
        if hit != hwnd {
            eprintln!(
                "live drag: десктоп занят (клик в ({cx},{cy}) достанется чужому окну {hit:?}, \
                 не нашему {hwnd:?}) — реальный драг пропущен, механизм покрыт unit-тестами"
            );
            return;
        }
        pins.set_move_lock(hwnd, true);

        // SAFETY: SetCursorPos — установка курсора в экранных координатах.
        let _ = unsafe { SetCursorPos(cx, cy) };
        std::thread::sleep(Duration::from_millis(80));

        // Зажать левую кнопку: DefWindowProc входит в модальный цикл
        // перетаскивания (WM_ENTERSIZEMOVE) и берёт захват мыши на окно.
        // SAFETY: SendInput — системная инъекция ввода.
        let sent =
            unsafe { SendInput(&[mouse_input(MOUSEEVENTF_LEFTDOWN, 0, 0)], size_of::<INPUT>() as i32) };
        assert_eq!(sent, 1, "SendInput(LEFTDOWN) не применился");
        std::thread::sleep(Duration::from_millis(120));

        let deltas: [(i32, i32); 10] = [
            (15, 0),
            (15, 8),
            (15, 8),
            (15, 0),
            (15, 8),
            (15, 8),
            (15, 0),
            (15, 8),
            (15, 8),
            (15, 0),
        ];
        let mut steps: Vec<Step> = Vec::new();
        for (i, (dx, dy)) in deltas.iter().enumerate() {
            // SAFETY: инъекция относительного движения мыши.
            let sent = unsafe {
                SendInput(&[mouse_input(MOUSEEVENTF_MOVE, *dx, *dy)], size_of::<INPUT>() as i32)
            };
            assert_eq!(sent, 1, "SendInput(MOVE) шаг {i}");
            std::thread::sleep(Duration::from_millis(16));

            // Позиция после шага драга — ДО enforce (видна «голая» реакция
            // окна на мышь, без нашего влияния).
            let mut wr = RECT::default();
            // SAFETY: окно живо.
            unsafe { GetWindowRect(hwnd, &mut wr) }.expect("GetWindowRect");

            // То же, что делает координатор: снимок трекера → enforce_move_lock.
            let dwm = extended_frame_bounds(hwnd);
            let rect = RECT {
                left: dwm.x,
                top: dwm.y,
                right: dwm.x + dwm.w,
                bottom: dwm.y + dwm.h,
            };
            let snapped = pins.enforce_move_lock(hwnd, rect);

            // SAFETY: чтения глобального состояния ввода.
            let our_capture = unsafe { GetCapture() } == hwnd;
            let lbutton_down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
            let mut cur = POINT::default();
            // SAFETY: GetCursorPos — чтение позиции курсора.
            let _ = unsafe { GetCursorPos(&mut cur) };
            steps.push(Step {
                window_left: wr.left,
                snapped,
                our_capture,
                lbutton_down,
                cursor: (cur.x, cur.y),
            });
        }

        // Отпустить кнопку — модальный цикл завершён (WM_EXITSIZEMOVE).
        // SAFETY: инъекция отпускания кнопки.
        let sent =
            unsafe { SendInput(&[mouse_input(MOUSEEVENTF_LEFTUP, 0, 0)], size_of::<INPUT>() as i32) };
        assert_eq!(sent, 1, "SendInput(LEFTUP) не применился");

        // После отпускания кормить enforce, пока окно не вернётся на эталон
        // (реальный трекер сам доставил бы снимок — здесь кормим вручную).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut post_release_snaps = 0u32;
        let mut final_rect = RECT::default();
        loop {
            let dwm = extended_frame_bounds(hwnd);
            let rect = RECT {
                left: dwm.x,
                top: dwm.y,
                right: dwm.x + dwm.w,
                bottom: dwm.y + dwm.h,
            };
            if pins.enforce_move_lock(hwnd, rect) {
                post_release_snaps += 1;
            }
            // SAFETY: окно живо.
            unsafe { GetWindowRect(hwnd, &mut final_rect) }.expect("GetWindowRect");
            if final_rect == baseline {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "окно не вернулось на эталон за 5 с: текущий {final_rect:?}, эталон {baseline:?}"
            );
            std::thread::sleep(Duration::from_millis(16));
        }

        // Сводка для `--nocapture`: тряска видна как snap-back'и во время
        // драга и как окно, не доехавшее за мышью.
        let during: Vec<&Step> = steps.iter().filter(|s| s.lbutton_down).collect();
        let snaps = during.iter().filter(|s| s.snapped).count();
        let xs: Vec<i32> = during.iter().map(|s| s.window_left).collect();
        let (min_x, max_x) = if xs.is_empty() {
            (0, 0)
        } else {
            (*xs.iter().min().unwrap(), *xs.iter().max().unwrap())
        };
        let spread = max_x - min_x;
        let cursor_moved = steps
            .first()
            .zip(steps.last())
            .map(|(a, b)| a.cursor != b.cursor)
            .unwrap_or(false);
        eprintln!(
            "live drag: шагов во время драга={}, snap-back'ов во время драга={}, \
             разброс left=[{min_x}..{max_x}] ({spread}px), курсор двигался={cursor_moved}, \
             захват наш={}, финал={final_rect:?} эталон={baseline:?}, \
             snap-back'ов после отпускания={post_release_snaps}",
            during.len(),
            snaps,
            during.iter().any(|s| s.our_capture),
        );

        // Гарантия move-lock: после отпускания окно вернулось на эталон.
        assert_eq!(final_rect, baseline, "после драга окно обязано вернуться на эталон");
        // Драг реально шёл (окно уезжало от эталона) — иначе тест не воспроизвёл
        // перетаскивание и проверки бессмысленны.
        assert!(spread > 50, "окно не уехало при драге — тест не воспроизвёл перетаскивание");
        // Захват модального цикла обязан принадлежать окну: на нём стоит
        // эвристика `user_is_dragging_window`, без этого фикс не найдёт драг.
        assert!(
            during.iter().any(|s| s.our_capture),
            "заголовок не схвачен окном (GetCapture никогда не был нашим) — драг не начался? {steps:#?}"
        );
        // Главная проверка фикса: во время драга move-lock НЕ дерётся с рукой
        // пользователя покадрово. До фикса каждый шаг с уехавшим окном давал
        // snap-back — окно дёргалось между «сдвинуто» и «эталон» на каждом
        // тике снимка трекера (~16 мс), что и есть тряска/телепорты репорта.
        assert_eq!(
            snaps, 0,
            "move-lock дёргал окно во время живого драга ({snaps} snap-back'ов на {} шагов) — \
             тяга-перетяга с рукой пользователя. Шаги: {steps:#?}",
            during.len(),
        );
        // Нет вечного snap-back-цикла на покое: после возврата на эталон
        // следующий снимок трекера не должен снова дёргать окно.
        let dwm = extended_frame_bounds(hwnd);
        let at_rest = RECT {
            left: dwm.x,
            top: dwm.y,
            right: dwm.x + dwm.w,
            bottom: dwm.y + dwm.h,
        };
        assert!(
            !pins.enforce_move_lock(hwnd, at_rest),
            "покоящееся окно на эталоне не должно снова snap-back'аться"
        );
    }

    #[test]
    fn interact_lock_disables_and_reenables_window() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        // SAFETY: чтение стиля живого окна.
        let style = |h: HWND| unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;

        pins.set_interact_lock(hwnd, true);
        // SAFETY: IsWindowEnabled — чтение состояния живого окна.
        assert!(!unsafe { IsWindowEnabled(hwnd) }.as_bool(), "ввод заблокирован");
        assert_ne!(style(hwnd) & WS_DISABLED.0, 0, "EnableWindow ставит WS_DISABLED");

        pins.set_interact_lock(hwnd, false);
        // SAFETY: IsWindowEnabled — чтение состояния живого окна.
        assert!(unsafe { IsWindowEnabled(hwnd) }.as_bool(), "ввод возвращён");
        assert_eq!(style(hwnd) & WS_DISABLED.0, 0);
    }

    #[test]
    fn unpin_releases_both_locks() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        pins.set_move_lock(target.0, true);
        pins.set_interact_lock(target.0, true);
        // SAFETY: чтение состояния живого окна.
        assert!(!unsafe { IsWindowEnabled(target.0) }.as_bool());

        pins.unpin(key(target.0)).expect("unpin");

        // Открепление вернуло ввод и забыло move-lock: enforce не двигает
        // окно даже при расхождении rect'а.
        // SAFETY: чтение состояния живого окна.
        assert!(unsafe { IsWindowEnabled(target.0) }.as_bool());
        let mut rect = RECT::default();
        // SAFETY: чтение прямоугольника живого окна.
        unsafe { GetWindowRect(target.0, &mut rect) }.expect("GetWindowRect");
        assert!(!pins.enforce_move_lock(target.0, rect));
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

    /// Окно с логом получаемых сообщений — для live-теста interact-guard'а
    /// (механизм бипа + поглощение клика). Живёт на своём потоке с помпом.
    struct MsgLogWindow {
        hwnd: HWND,
        log: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl MsgLogWindow {
        fn create(x: i32, y: i32, w: i32, h: i32) -> Self {
            use std::sync::mpsc;
            use std::sync::{Arc, Mutex};
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DispatchMessageW, GetMessageW, GWLP_USERDATA, MSG,
                RegisterClassExW, SetWindowLongPtrW, ShowWindow, SW_SHOW, TranslateMessage,
                WNDCLASSEXW, WS_EX_TOPMOST, WS_OVERLAPPED, WS_VISIBLE,
            };

            struct SendHwnd(HWND);
            unsafe impl Send for SendHwnd {}

            let log: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
            let (ready_tx, ready_rx) = mpsc::channel::<SendHwnd>();
            let thread = std::thread::spawn({
                let log = log.clone();
                move || {
                    // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
                    let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
                    let wc = WNDCLASSEXW {
                        cbSize: size_of::<WNDCLASSEXW>() as u32,
                        lpfnWndProc: Some(log_wndproc),
                        hInstance: hinstance.into(),
                        lpszClassName: w!("resticker_window_pin_msglog_test"),
                        ..Default::default()
                    };
                    // SAFETY: wc заполнена корректно; повторная регистрация
                    // (параллельные тесты) — не ошибка.
                    if unsafe { RegisterClassExW(&wc) } == 0 {
                        // SAFETY: осмысленна сразу после провалившегося вызова.
                        let err = unsafe { GetLastError() };
                        assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
                    }
                    // SAFETY: все аргументы — валидные константы и только что
                    // зарегистрированный класс; WS_EX_TOPMOST — чтобы тестовое
                    // окно стояло выше чужих окон десктопа (оверлеев в т.ч.).
                    let hwnd = unsafe {
                        CreateWindowExW(
                            WS_EX_TOPMOST,
                            w!("resticker_window_pin_msglog_test"),
                            w!("msglog"),
                            WS_OVERLAPPED | WS_VISIBLE,
                            x,
                            y,
                            w,
                            h,
                            None,
                            None,
                            Some(hinstance.into()),
                            None,
                        )
                    }
                    .expect("создание msglog-окна");
                    // SAFETY: hwnd только что создано этим потоком.
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOW);
                    }
                    // Лог в GWLP_USERDATA: wndproc складывает сюда получаемые
                    // сообщения; освобождается в WM_NCDESTROY.
                    let boxed = Box::into_raw(Box::new(log.clone()));
                    // SAFETY: hwnd живо, boxed живёт до WM_NCDESTROY.
                    unsafe {
                        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, boxed as isize);
                    }
                    ready_tx.send(SendHwnd(hwnd)).expect("получатель ещё жив");
                    let mut msg = MSG::default();
                    // SAFETY: стандартный цикл сообщений окна этого потока.
                    unsafe {
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
            });
            let hwnd = ready_rx.recv().expect("поток msglog-окна не упал").0;
            Self {
                hwnd,
                log,
                thread: Some(thread),
            }
        }

        fn clear_log(&self) {
            self.log.lock().unwrap().clear();
        }

        fn received_any(&self, msgs: &[u32]) -> bool {
            let log = self.log.lock().unwrap();
            msgs.iter().any(|m| log.contains(m))
        }
    }

    impl Drop for MsgLogWindow {
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

    unsafe extern "system" fn log_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::{PostQuitMessage, WM_DESTROY, WM_NCDESTROY};
        // SAFETY: чтение указателя, положенного в GWLP_USERDATA при создании
        // (см. MsgLogWindow::create); валиден до WM_NCDESTROY.
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if ptr != 0 {
            let arc = ptr as *const std::sync::Arc<std::sync::Mutex<Vec<u32>>>;
            unsafe { arc.as_ref() }
                .expect("логический Arc жив до WM_NCDESTROY")
                .lock()
                .unwrap()
                .push(msg);
        }
        if msg == WM_DESTROY {
            // SAFETY: стандартный вызов из обработчика WM_DESTROY — иначе
            // GetMessageW этого потока не завершился бы после DestroyWindow.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        if msg == WM_NCDESTROY {
            // SAFETY: окно умирает — освобождаем клон Arc из GWLP_USERDATA;
            // оригинал живёт в MsgLogWindow::log.
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            if ptr != 0 {
                drop(unsafe {
                    Box::from_raw(ptr as *mut std::sync::Arc<std::sync::Mutex<Vec<u32>>>)
                });
            }
        }
        // SAFETY: делегирование системному обработчику (в т.ч. WM_MOUSEACTIVATE
        // — ветка DefWindowProc для disabled-окна и играет системный бип).
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    #[test]
    #[ignore = "требует реальный десктоп и реальный ввод; запуск вручную: cargo test -p rst-win32 window_pin interact_guard_swallows_real_click -- --ignored"]
    fn interact_guard_swallows_real_click_on_locked_window() {
        use std::time::Duration;
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSE_EVENT_FLAGS,
            MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, SetCursorPos, WindowFromPoint, WM_ACTIVATE, WM_LBUTTONDOWN,
            WM_MOUSEACTIVATE, WM_NCACTIVATE,
        };

        fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }

        // Реальный клик ЛКМ в позиции курсора (SendInput → полный путь
        // input-routing: hit-test, активация, WM_MOUSEACTIVATE, бип).
        fn click_at(x: i32, y: i32) {
            // SAFETY: SetCursorPos — установка курсора в экранных координатах.
            let _ = unsafe { SetCursorPos(x, y) };
            std::thread::sleep(Duration::from_millis(60));
            // SAFETY: SendInput — системная инъекция нажатия/отпускания ЛКМ.
            let sent = unsafe {
                SendInput(
                    &[mouse_input(MOUSEEVENTF_LEFTDOWN), mouse_input(MOUSEEVENTF_LEFTUP)],
                    size_of::<INPUT>() as i32,
                )
            };
            assert_eq!(sent, 2, "SendInput(клик) не применился");
            // Дать системе разобрать клик (активация/бип/сообщения).
            std::thread::sleep(Duration::from_millis(500));
        }

        let win = MsgLogWindow::create(300, 300, 300, 200);
        let hwnd = win.hwnd;
        let mut pins = WindowPins::new();
        let mut rect = RECT::default();
        // SAFETY: окно живо.
        unsafe { GetWindowRect(hwnd, &mut rect) }.expect("GetWindowRect");
        let (cx, cy) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
        let fg_before = unsafe { GetForegroundWindow() };

        // --- Фаза 1: МЕХАНИЗМ БИПА. Окно disabled БЕЗ interact-guard'а (голая
        // EnableWindow) → реальный клик по нему. Факт (проверен этим тестом):
        // окно НЕ получает ни WM_MOUSEACTIVATE, ни WM_NCHITTEST, ни
        // WM_LBUTTONDOWN — система сама обрабатывает клик по disabled top-level
        // окну (играет звук) и лишь шлёт окну служебное WM_NCACTIVATE/WM_ACTIVATE
        // из неудачной попытки активации. Именно поэтому перехват в wndproc
        // бесполезен (ловить нечего), а фикс — WH_MOUSE_LL (фаза 2).
        // SAFETY: EnableWindow безопасен для своего окна.
        unsafe { let _ = EnableWindow(hwnd, false); };
        assert!(!unsafe { IsWindowEnabled(hwnd) }.as_bool());
        win.clear_log();
        click_at(cx, cy);
        let msgs = win.log.lock().unwrap().clone();
        // SAFETY: WindowFromPoint — чтение hwnd под точкой.
        let hit = unsafe { WindowFromPoint(POINT { x: cx, y: cy }) };
        assert_eq!(
            hit, hwnd,
            "клик должен попадать в тестовое окно (point=({cx},{cy}), hit={hit:?})"
        );
        assert!(
            !win.received_any(&[WM_MOUSEACTIVATE, 0x0084, WM_LBUTTONDOWN]),
            "механизм: клик по disabled top-level окну НЕ доходит до wndproc (нет WM_MOUSEACTIVATE/WM_NCHITTEST/WM_LBUTTONDOWN) — бип играет система до диспетчеризации; сообщения: {msgs:?}"
        );
        assert!(
            win.received_any(&[WM_NCACTIVATE, WM_ACTIVATE]),
            "механизм: система должна обработать клик против окна (WM_NCACTIVATE/WM_ACTIVATE из неудачной активации); сообщения: {msgs:?}"
        );

        // --- Фаза 2: ФИКС. Регистрируем окно в interact-lock (ставится
        // WH_MOUSE_LL на своём потоке-помпе) → тот же клик поглощается ДО
        // системного input-routing: окно не получает НИ ОДНОГО сообщения
        // клика/активации (даже WM_NCACTIVATE) — бипа нет, foreground не
        // трогается, ввод по-прежнему заблокирован.
        pins.set_interact_lock(hwnd, true);
        assert!(
            interact_guard::hook_active(),
            "WH_MOUSE_LL должен быть активен после set_interact_lock"
        );
        // Дать потоку-помпу хука войти в GetMessageW.
        std::thread::sleep(Duration::from_millis(200));
        win.clear_log();
        click_at(cx, cy);
        let msgs2 = win.log.lock().unwrap().clone();
        assert!(
            !win.received_any(&[WM_MOUSEACTIVATE, WM_LBUTTONDOWN, 0x0084, WM_NCACTIVATE, WM_ACTIVATE]),
            "клик по interact-locked окну должен быть поглощён ДО системного input-routing (иначе будет бип); сообщения: {msgs2:?}"
        );
        assert_eq!(
            unsafe { GetForegroundWindow() },
            fg_before,
            "поглощённый клик не должен менять foreground"
        );
        assert!(
            !unsafe { IsWindowEnabled(hwnd) }.as_bool(),
            "окно по-прежнему заблокировано (ввод реально не доходит)"
        );

        // Снять блокировку — хук снимается, окну возвращается ввод.
        pins.set_interact_lock(hwnd, false);
        assert!(
            !interact_guard::hook_active(),
            "WH_MOUSE_LL должен сняться после последнего unlock"
        );
        assert!(unsafe { IsWindowEnabled(hwnd) }.as_bool(), "ввод возвращён");
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_pin reassert_topmost -- --ignored"]
    fn reassert_topmost_restores_after_external_knockout_live() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, ShowWindow, SW_MINIMIZE, SW_RESTORE,
        };

        let win = RealWindow::create();
        let hwnd = win.hwnd;
        let mut pins = WindowPins::new();

        // Пин реального окна — одноразовый SetWindowPos(HWND_TOPMOST), ровно
        // как в PowerToys' PinTopmostWindow: размер/позиция не трогаются.
        pins.pin(MARKER, key(hwnd)).expect("пин реального окна");
        assert!(is_topmost(hwnd), "после pin окно в topmost-полосе");

        // 1) Уже topmost — реактивная проверка обязана быть no-op.
        assert!(
            !pins.reassert_topmost_if_needed(hwnd),
            "topmost-окно не требует коррекции"
        );

        // 2) Внешнее вмешательство: напрямую снимаем topmost (как сделало бы
        //    другое приложение, сбрасывающее стиль/переставляющее z-order).
        // SAFETY: hwnd — живое окно; флаги — только снять topmost.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        assert!(!is_topmost(hwnd), "имитация выбивания из topmost не сработала");
        assert!(
            pins.reassert_topmost_if_needed(hwnd),
            "backstop должен обнаружить сбитый topmost и вернуть его"
        );
        assert!(is_topmost(hwnd), "WS_EX_TOPMOST восстановлен");
        // Повторная проверка — снова no-op.
        assert!(!pins.reassert_topmost_if_needed(hwnd));

        // 3) Проба PowerToys#17332: у части сборок Windows topmost-флаг может
        //    теряться после minimize/restore. Здесь просто документируем
        //    поведение ПЛАТФОРМЫ: если флаг сбился — backstop его возвращает;
        //    если нет — no-op. Оба исхода валидны.
        // SAFETY: ShowWindow потокобезопасен для чужого окна.
        unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        }
        std::thread::sleep(Duration::from_millis(300));
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        std::thread::sleep(Duration::from_millis(300));
        let lost_after_restore = !is_topmost(hwnd);
        eprintln!(
            "[reassert live] minimize/restore потерял topmost на этой сборке: {lost_after_restore}"
        );
        if lost_after_restore {
            assert!(pins.reassert_topmost_if_needed(hwnd));
            assert!(is_topmost(hwnd), "topmost возвращён после restore");
        }

        // Открепить — окно возвращается в обычную полосу, topmost снят.
        pins.unpin(key(hwnd)).expect("unpin");
        assert!(!is_topmost(hwnd), "unpin снял WS_EX_TOPMOST");
    }
}
