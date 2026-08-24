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
//! * **Move-lock** — «окно нельзя двигать». Три эшелона: страж ввода
//!   [`input_guard`] глотает нажатие мыши по заголовку/рамке (драг не
//!   начинается вовсе), он же обрывает уже начатый модальный цикл через
//!   `EVENT_SYSTEM_MOVESIZESTART` + `WM_CANCELMODE`, а описанный ниже
//!   реактивный snap-back остаётся третьим эшелоном — для программных
//!   перемещений (чужой `SetWindowPos`, Win+стрелки, snap-раскладки), где
//!   нажатия мыши нет вовсе. Сам snap-back: [`WindowPins::set_move_lock`]
//!   хранит «правильный» прямоугольник per-hwnd (в DWM-координатах
//!   `extended_frame_bounds`, как у снимков трекера),
//!   [`WindowPins::enforce_move_lock`] сравнивает с фактическим и при
//!   расхождении принудительно возвращает окно `SetWindowPos`'ом. Драг
//!   окна пользователем в этот момент не трогается (см. доккомент
//!   `enforce_move_lock`): snap-back происходит один раз после отпускания.
//! * **Interact-lock** — «с окном нельзя взаимодействовать»: тот же страж
//!   [`input_guard`] глотает клики по СОДЕРЖИМОМУ окна и колесо, но не
//!   трогает заголовок и рамки — окно с этим замком по-прежнему можно
//!   двигать (это и есть смысл разделения двух замков).
//!   `EnableWindow(hwnd, FALSE)`, стоявший здесь до 2026-08-21, отбирал у
//!   окна вообще всё, включая перемещение, и заставлял систему пищать на
//!   каждый клик — см. [`WindowPins::set_interact_lock`].
//!
//! Обе блокировки — рантайм-состояние, per-hwnd, обе по умолчанию ВЫКЛ.
//! У структуры нет понятия «режим редактирования»: гейтинг вызовов на
//! edit-mode — обязанность вызывающего кода (координатора), см. доккоменты
//! соответствующих методов.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, HWND, LPARAM, RECT, SetLastError, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    GUI_INMOVESIZE, GUITHREADINFO, GW_HWNDPREV, GWL_EXSTYLE, GetGUIThreadInfo, GetPropW, GetWindow,
    GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, GetWindowThreadProcessId, HWND_NOTOPMOST,
    HWND_TOP, HWND_TOPMOST, IsIconic, IsWindow, PostMessageW, RemovePropW, SW_MINIMIZE,
    SW_SHOWMAXIMIZED, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SetPropW, SetWindowPlacement, SetWindowPos, ShowWindowAsync, WINDOWPLACEMENT,
    WM_CANCELMODE, WS_EX_TOPMOST,
};
use windows::core::{BOOL, HRESULT, PCWSTR, w};

use crate::error::Win32Error;
use crate::window_enum::{WindowInfo, extended_frame_bounds};

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

    /// Убрать закреплённое окно с экрана, пока не активен ни один его
    /// «хозяин» (правила «показывать только на этих окнах», решение
    /// [`rst_core::pinned_window::host_action`]) — сворачиванием.
    ///
    /// Почему именно сворачивание, а не `SW_HIDE`, не DWM-cloak и не вынос
    /// за экран (выбор пользователя 2026-08-22): свёрнутое окно остаётся в
    /// панели задач и в Alt+Tab, поэтому пользователь в любой момент может
    /// вызвать его сам — это часть постановки задачи. `SW_HIDE`/cloak
    /// убирают окно и из панели задач, и из Alt+Tab; вынос за экран рискует
    /// тем, что приложение запомнит позицию вне экрана при закрытии.
    ///
    /// `ShowWindowAsync`, а не `ShowWindow`: команда чужому окну не должна
    /// блокировать координатор на чужой очереди сообщений (окно может
    /// «задуматься» — координатор при этом обязан продолжать рисовать).
    pub fn hide_until_host(&self, hwnd: HWND) -> bool {
        // SAFETY: ShowWindowAsync безопасен для чужого и мёртвого окна —
        // просто вернёт FALSE.
        unsafe { ShowWindowAsync(hwnd, SW_MINIMIZE) }.as_bool()
    }

    /// Вернуть окно, свёрнутое [`Self::hide_until_host`]: развернуть БЕЗ
    /// активации и заново утвердить topmost.
    ///
    /// `SW_SHOWNOACTIVATE` принципиален: хозяин только что стал активным
    /// окном, и забирать у него фокус ради нашего показа нельзя — окно
    /// должно всплыть НАД ним, оставив ввод там, где его ждёт пользователь.
    /// Разворот сбрасывает `WS_EX_TOPMOST` у части приложений, поэтому
    /// сразу же переутверждаем его тем же путём, что и обычная коррекция.
    pub fn show_for_host(&self, hwnd: HWND) -> bool {
        // SAFETY: см. `hide_until_host`.
        let shown = unsafe { ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE) }.as_bool();
        self.reassert_topmost_if_needed(hwnd);
        shown
    }

    /// Выключить/включить обратно анимации сворачивания и разворачивания
    /// окна (`DWMWA_TRANSITIONS_FORCEDISABLED`).
    ///
    /// Зачем: окно с правилами «показывать только на этих окнах» мы
    /// сворачиваем в тот момент, когда пользователь уходит с хозяина. Штатная
    /// анимация сворачивания длится порядка четверти секунды, и всё это
    /// время закреплённое окно ещё видно — пользователь читает это как
    /// «оно пропадает не сразу, а с задержкой» (репорт 2026-08-22: «очень
    /// важно»). С выключенными переходами окно исчезает и возвращается
    /// мгновенно.
    ///
    /// Атрибут ставится ТОЛЬКО пока у окна есть правила, и снимается вместе
    /// с ними и при откреплении: чужому окну мы не вправе навсегда менять
    /// поведение. Запрос идёт в DWM, а не в процесс окна, поэтому не зависит
    /// от его отзывчивости; ошибку игнорируем — окно могло умереть.
    pub fn set_transitions_disabled(&self, hwnd: HWND, disabled: bool) {
        let value: BOOL = disabled.into();
        // SAFETY: значение живёт до конца вызова, размер соответствует
        // типу атрибута (BOOL); DwmSetWindowAttribute безопасен для чужих
        // и мёртвых окон.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&raw const value).cast(),
                size_of::<BOOL>() as u32,
            )
        };
    }

    /// Снять НАШ маркер с окон, которых нет в книжке закреплений, — уборка
    /// после аварийного завершения прошлого запуска.
    ///
    /// Зачем это нужно (репорт пользователя 2026-08-21: «программа не очень
    /// хочет закреплять и откреплять Проводник и Блокнот, остальные норм»):
    /// маркер [`PIN_PROP_NAME`] живёт на ЧУЖОМ окне, а не у нас, поэтому
    /// переживает наш процесс. Если resticker завершился жёстко (крэш,
    /// `taskkill`, выключение питания) с закреплённым окном, маркер
    /// остаётся на нём навсегда — до закрытия самого окна. Дальше любой
    /// новый запуск отказывается закреплять такое окно
    /// ([`Win32Error::AlreadyPinned`]), а открепить его нельзя: в книжке
    /// нового запуска этого окна нет. Долгоживущие системные окна
    /// (Проводник, Блокнот) переживают десятки наших перезапусков и копят
    /// такие «вечные» маркеры, а браузеры и редакторы закрываются вместе с
    /// маркером — отсюда и «остальные норм».
    ///
    /// Единственность процесса гарантируется `single_instance`, поэтому
    /// любой чужой для книжки маркер — заведомо наш собственный мусор, а не
    /// метка живого второго экземпляра.
    ///
    /// Возвращает число вычищенных окон. Ошибки игнорируются: окно могло
    /// умереть между перечислением и снятием, а UIPI-отказ на чужом
    /// повышенном окне не наша беда (мы его и закрепить не смогли бы).
    pub fn clear_orphan_markers(&self, windows: &[WindowInfo]) -> usize {
        let mut cleared = 0;
        for win in windows {
            if self.pinned.contains_key(&win.hwnd) {
                continue;
            }
            let hwnd = hwnd_from_usize(win.hwnd);
            // SAFETY: GetPropW безопасен для чужих и мёртвых окон.
            if unsafe { GetPropW(hwnd, PIN_PROP_NAME) }.0.is_null() {
                continue;
            }
            // SAFETY: RemovePropW безопасен для чужих окон; отказ игнорируем.
            if unsafe { RemovePropW(hwnd, PIN_PROP_NAME) }.is_ok() {
                cleared += 1;
            }
        }
        cleared
    }

    /// Перенять окно, на котором уже стоит наш маркер, но которого нет в
    /// книжке, — то есть осиротевшее закрепление прошлого запуска (см.
    /// [`Self::clear_orphan_markers`]). Отличается от [`Self::pin`] ровно
    /// одним: не считает существующий маркер ошибкой.
    ///
    /// Нужен как второй рубеж к стартовой уборке: окно могло быть скрыто
    /// или свёрнуто в момент уборки (перечисление его не отдаёт), а всплыть
    /// позже — пользователь не должен упираться в «уже закреплено» и
    /// невозможность открепить.
    pub fn adopt(&mut self, marker: u64, target: usize) -> Result<(), Win32Error> {
        let target_hwnd = hwnd_from_usize(target);
        // SAFETY: IsWindow безопасен для любых значений, включая мёртвые.
        if !unsafe { IsWindow(Some(target_hwnd)) }.as_bool() {
            return Err(Win32Error::PinWindowGone);
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: то же, что в `pin`.
        unsafe { SetWindowPos(target_hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) }
            .map_err(map_pin_err)?;
        // SAFETY: то же, что в `pin` — перезапись значения маркера.
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
    /// Открепление заодно освобождает обе блокировки (редизайн пинов): обе
    /// забываются и снимаются со стража ввода [`input_guard`] — иначе
    /// откреплённое окно осталось бы под чужими правилами навсегда
    /// (нельзя двигать / нельзя кликать). Фокус при этом НЕ трогается
    /// (SPEC: «unpin … no forced refocus»); ничего восстанавливать в самом
    /// окне не нужно — страж работает снаружи, стилей окна не меняет.
    pub fn unpin(&mut self, target: usize) -> Result<(), Win32Error> {
        self.pinned.remove(&target);
        self.move_locked.remove(&(target as isize));
        self.interact_locked.remove(&(target as isize));
        self.sync_guard(hwnd_from_usize(target));
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
            // Блокировки мертвого окна тоже чистим: иначе запись висела бы
            // в страже ввода и на переиспользованном системой hwnd.
            self.move_locked.remove(&(target as isize));
            self.interact_locked.remove(&(target as isize));
            self.sync_guard(hwnd_from_usize(target));
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
                    self.sync_guard(hwnd);
                    return;
                }
            }
            // Окна нет (или границы не отдались) — блокировать нечего.
            self.move_locked.remove(&key);
        } else {
            self.move_locked.remove(&key);
        }
        self.sync_guard(hwnd);
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
        if !self.set_dwm_bounds(hwnd, good) {
            // Окно умерло между проверкой и перестановкой — чистим, чтобы
            // состояние не копилось.
            self.move_locked.remove(&key);
            return false;
        }
        true
    }

    /// Поставить окну ТАКИЕ границы, чтобы его DWM-габариты
    /// (`DWMWA_EXTENDED_FRAME_BOUNDS` — та же система координат, в которой
    /// живут снимки трекера и весь UI поверх окна) совпали с `target`.
    ///
    /// Зачем отдельный примитив: `SetWindowPos` работает в
    /// `GetWindowRect`-координатах, которые у окон Win11 отличаются от
    /// DWM-габаритов на невидимые поля ресайза (сверху ~1 px, по бокам и
    /// снизу ~7–8 px). Складывать эти пространства напрямую — значит
    /// systematically промахиваться на размер рамки и, при повторных
    /// применениях, дрейфовать. Смещение между системами от позиции не
    /// зависит (метрики рамки постоянны), поэтому считаем его здесь и
    /// применяем один раз.
    ///
    /// `false` — окна нет или система отказала.
    pub fn set_dwm_bounds(&self, hwnd: HWND, target: RECT) -> bool {
        // SAFETY: IsWindow безопасен для любых значений.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return false;
        }
        let mut gwr = RECT::default();
        // SAFETY: GetWindowRect — чтение экранного прямоугольника, безопасно
        // и для чужих окон.
        if unsafe { GetWindowRect(hwnd, &mut gwr) }.is_err() {
            return false;
        }
        let dwm = extended_frame_bounds(hwnd);
        let dx = dwm.x - gwr.left;
        let dy = dwm.y - gwr.top;
        let dw = dwm.w - (gwr.right - gwr.left);
        let dh = dwm.h - (gwr.bottom - gwr.top);
        let good = target;
        let good_w = good.right - good.left;
        let good_h = good.bottom - good.top;
        if is_maximized(hwnd) {
            // Развёрнутое окно `SetWindowPos` ужать нельзя честно: стиль
            // `WS_MAXIMIZE` остаётся, и система вправе вернуть окну полный
            // размер на следующем же пересчёте. Единственный корректный
            // выход из развёрнутого состояния с ОДНОВРЕМЕННОЙ установкой
            // нормального прямоугольника — `SetWindowPlacement`
            // (`SW_SHOWNOACTIVATE` не трогает фокус).
            return self
                .move_resize(
                    hwnd.0 as usize,
                    good.left - dx,
                    good.top - dy,
                    good_w - dw,
                    good_h - dh,
                )
                .is_ok();
        }
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
    /// блокировка №2): в заблокированное окно не доходят клики по его
    /// СОДЕРЖИМОМУ (клиентская область, меню, системное меню, полосы
    /// прокрутки) и колесо мыши. Заголовок, рамки и кнопки свернуть/
    /// развернуть/закрыть остаются рабочими — окно по-прежнему можно
    /// двигать и закрыть. Механизм — страж ввода [`input_guard`]
    /// (глобальный `WH_MOUSE_LL`), состояние per-hwnd — в `interact_locked`.
    ///
    /// ПОЧЕМУ НЕ `EnableWindow(hwnd, FALSE)` (как было до 2026-08-21):
    /// `WS_DISABLED` гасит окно ЦЕЛИКОМ, вместе с заголовком — «замок на
    /// взаимодействие» отбирал заодно и перемещение окна, чего он делать не
    /// должен (репорт пользователя). Плюс система играла на каждый клик по
    /// disabled-окну системный «динг», который приходилось глушить тем же
    /// хуком. Хук без `EnableWindow` решает обе проблемы разом: стили чужого
    /// окна не трогаются вовсе, восстанавливать при откреплении нечего.
    ///
    /// Гейтинг на edit-mode — на вызывающем, как и у move-lock (в
    /// edit-mode блокировки приостановлены, SPEC).
    ///
    /// ЧЕСТНАЯ ГРАНИЦА: блокируется мышь, не клавиатура. Окно, оставшееся
    /// с фокусом, продолжит принимать ввод с клавиатуры; фокус здесь
    /// сознательно НЕ отбирается (у фонового процесса Windows и так вправе
    /// отказать в `SetForegroundWindow`, а молча «съеденный» ввод хуже
    /// честно работающей клавиатуры). Полная блокировка клавиатуры —
    /// отдельный `WH_KEYBOARD_LL`, возможная следующая фаза.
    pub fn set_interact_lock(&mut self, hwnd: HWND, locked: bool) {
        let key = hwnd.0 as isize;
        if locked {
            self.interact_locked.insert(key);
        } else {
            self.interact_locked.remove(&key);
        }
        self.sync_guard(hwnd);
    }

    /// Привести регистрацию `hwnd` в страже ввода в соответствие с книжками
    /// `move_locked`/`interact_locked`. Единственная точка, где обе
    /// блокировки встречаются: страж — один хук на процесс, и обе политики
    /// он должен видеть вместе (окно может быть заблокировано и на
    /// перемещение, и на взаимодействие одновременно).
    fn sync_guard(&self, hwnd: HWND) {
        let key = hwnd.0 as isize;
        input_guard::set_policy(
            hwnd,
            input_guard::Policy {
                move_locked: self.move_locked.contains_key(&key),
                interact_locked: self.interact_locked.contains(&key),
            },
        );
    }
}

/// Единый мышиный «страж» обеих блокировок (редизайн пинов): один глобальный
/// `WH_MOUSE_LL` на выделенном потоке-помпе, который решает по КАЖДОМУ нажатию
/// кнопки/колесу, доставить его окну или выбросить из input-очереди.
///
/// ПОЧЕМУ ХУК, А НЕ `EnableWindow`/реактивный snap-back:
/// * `EnableWindow(hwnd, FALSE)` (как было у interact-lock) гасит окно
///   ЦЕЛИКОМ — вместе с заголовком, кнопками свернуть/закрыть и системным
///   меню. То есть «замок на взаимодействие» отбирал и перемещение окна, чего
///   он делать не должен (репорт пользователя 2026-08-20). Плюс `WS_DISABLED`
///   заставляет win32k играть системный «динг» на каждый клик.
/// * Реактивный snap-back move-lock'а ([`WindowPins::enforce_move_lock`])
///   структурно не способен НЕ ДАТЬ сдвинуть окно: модальный цикл
///   перетаскивания (`WM_ENTERSIZEMOVE`) крутится в процессе самого окна и
///   переставляет его на каждый `WM_MOUSEMOVE`, а мы узнаём о движении только
///   из снимка трекера (дебаунс 16 мс) и возвращаем окно ПОСЛЕ. На видео это
///   выглядит как «окно свободно ездит ~0.8 с и телепортируется назад».
///
/// ТРИ ЭШЕЛОНА MOVE-LOCK (первый — здесь):
/// 1. Глотать button-down, если hit-test точки даёт «перетащить/ресайзить»
///    (`HTCAPTION`, рамки, `HTGROWBOX`). Драг просто не начинается — окно не
///    сдвигается ни на пиксель.
/// 2. `EVENT_SYSTEM_MOVESIZESTART` (хук WinEvent на этом же потоке) →
///    `PostMessageW(WM_CANCELMODE)`: обрывает уже НАЧАВШИЙСЯ модальный цикл.
///    Ловит пути мимо LL-хука — тач/перо, Alt+Space → «Переместить»,
///    промах кэша hit-test'а.
/// 3. [`WindowPins::enforce_move_lock`] — snap-back для программных move'ов
///    (`SetWindowPos` чужого кода, Win+стрелки, snap-раскладки), где нажатия
///    мыши нет вовсе.
///
/// INTERACT-LOCK — только «содержимое» окна: глотаются `HTCLIENT`, меню,
/// системное меню, полосы прокрутки и КОЛЕСО; заголовок, кнопки свернуть/
/// развернуть/закрыть и рамки не трогаются. Поэтому окно с одним лишь
/// interact-lock'ом по-прежнему можно двигать — ровно то поведение, которого
/// не хватало. `EnableWindow` не вызывается вообще, `WS_DISABLED` не
/// ставится, системного «динга» нет по построению.
///
/// РЕШЕНИЕ ПРИ НЕОПРЕДЕЛЁННОСТИ — FAIL-OPEN (см. `should_swallow_button`):
/// interact-lock глотает нажатие, только если точка ДОКАЗАННО относится к
/// содержимому. Первая версия фикса доверяла геометрической оценке всегда, и
/// у окон с собственным заголовком (Electron/Chrome/VS Code/Steam), где
/// клиентская область покрывает всё окно, замок кликов снова отбирал
/// перетаскивание — репорт 2026-08-21. Теперь сомнение трактуется в пользу
/// перемещения.
///
/// ЧЕСТНЫЕ ОГРАНИЧЕНИЯ (без них блокировка выглядела бы сильнее, чем есть):
/// * Клавиатура НЕ блокируется: interact-lock — про мышь. Окно с фокусом
///   по-прежнему принимает ввод с клавиатуры (отдельный `WH_KEYBOARD_LL` —
///   потенциальная следующая фаза).
/// * Окна процессов с более высоким уровнем целостности (elevated) UIPI
///   закрывает: LL-хук для них не вызывается, `WM_NCHITTEST` не доходит —
///   блокировка на них не держится (тот же класс ограничений, что и
///   [`Win32Error::PinAccessDenied`] у самого закрепления).
/// * Колесо ловится по позиции КУРСОРА. Если «прокрутка неактивных окон»
///   выключена и курсор вне заблокированного окна, колесо уходит в
///   сфокусированное окно мимо нас.
///
/// БЮДЖЕТ КОЛБЭКА — жёсткий: `HKCU\Control Panel\Desktop\LowLevelHooksTimeout`
/// на машине пользователя = 1 мс (дефолт, когда ключа нет, — 300 мс).
/// Превышение = Windows МОЛЧА снимает хук, без ошибки и уведомления. Отсюда
/// два решения:
/// * Hit-test НИКОГДА не запрашивается из колбэка: `SendMessage(WM_NCHITTEST)`
///   — синхронный вызов в чужой процесс, это десятки мс на «задумавшемся»
///   окне. Вместо этого отдельный поток-пробник [`ht_probe`] опрашивает
///   `SendMessageTimeoutW(..., SMTO_ABORTIFHUNG)` по позиции курсора и кладёт
///   результат в кэш; колбэк только читает кэш (промах — геометрическая
///   оценка, см. `ht_from_geometry`).
/// * Цель клика ищется одним `WindowFromPoint` + `GetAncestor(GA_ROOT)`, а не
///   обходом всего z-order десктопа (сотни окон × 3 win32-вызова).
///   `WindowFromPoint` сам пропускает `WS_EX_TRANSPARENT` (клик-сквозные
///   оверлеи resticker) — ровно та же семантика, что была у обхода.
/// * Все локи в колбэке — `try_lock`: занято (координатор в этот момент
///   правит карту) — пропускаем событие, а не блокируемся.
///
/// Плюс сторож: если хук всё-таки сняли, поток-пробник замечает «мышь
/// движется, а событий нет» и просит поток-помп переустановить хук.
///
/// Жизненный цикл — refcount по содержимому карты: хуки и потоки поднимаются
/// при первом заблокированном окне и снимаются после последнего
/// unlock/unpin/сноса. Состояние process-global (`OnceLock`): глобальный хук
/// может быть ровно один, а `WindowPins` в приложении один.
mod input_guard {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock, mpsc};
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
    use windows::Win32::UI::HiDpi::PhysicalToLogicalPointForPerMonitorDPI;
    use windows::Win32::UI::WindowsAndMessaging::{
        CHILDID_SELF, CallNextHookEx, EVENT_SYSTEM_MOVESIZESTART, GA_ROOT, GWL_EXSTYLE,
        GetAncestor, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
        HHOOK, HTBORDER, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTGROWBOX,
        HTHSCROLL, HTLEFT, HTMENU, HTNOWHERE, HTRIGHT, HTSYSMENU, HTTOP, HTTOPLEFT, HTTOPRIGHT,
        HTVSCROLL, IsWindowVisible, MSG, MSLLHOOKSTRUCT, OBJID_WINDOW, PostMessageW,
        PostThreadMessageW, SEND_MESSAGE_TIMEOUT_FLAGS, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL, WINEVENT_OUTOFCONTEXT, WM_APP,
        WM_CANCELMODE, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK,
        WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST,
        WM_QUIT, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
        WS_EX_TRANSPARENT,
    };

    use super::{HWND, IsWindow};

    /// Что именно заблокировано у окна. Обе блокировки независимы и
    /// комбинируются: `move_locked` без `interact_locked` — «окно нельзя
    /// двигать, но можно пользоваться», `interact_locked` без `move_locked` —
    /// «нельзя пользоваться, но можно двигать» (ровно то, что сломал старый
    /// `EnableWindow(FALSE)`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Policy {
        pub(super) move_locked: bool,
        pub(super) interact_locked: bool,
    }

    impl Policy {
        fn is_empty(self) -> bool {
            !self.move_locked && !self.interact_locked
        }
    }

    struct GuardState {
        locked: Mutex<HashMap<isize, Policy>>,
        hook: Mutex<Option<ActiveHook>>,
    }

    struct ActiveHook {
        tid: u32,
        thread: Option<std::thread::JoinHandle<()>>,
        prober: Option<std::thread::JoinHandle<()>>,
    }

    static STATE: OnceLock<GuardState> = OnceLock::new();

    fn state() -> &'static GuardState {
        STATE.get_or_init(|| GuardState {
            locked: Mutex::new(HashMap::new()),
            hook: Mutex::new(None),
        })
    }

    /// Кнопка мыши сейчас «поглощена» (down проглочен — глотаем и его up).
    /// Отдельный атомик: в колбэке нельзя блокироваться на карте, а для
    /// down/up-пар достаточно одного флага (см. `guard_mouse_proc`).
    ///
    /// Сбрасывается при любой смене жизненного цикла хука: если хук исчез
    /// между `down` и `up` (система сняла его по таймауту, либо блокировку
    /// сняли), незакрытый флаг съел бы следующий чужой `up` в любом
    /// приложении — потерянное отпускание кнопки выглядит как застрявший
    /// драг (ревью 2026-08-21).
    static SWALLOWED_DOWN: AtomicBool = AtomicBool::new(false);

    /// Момент последнего события, дошедшего до колбэка (мс от `epoch()`).
    /// Сторож в потоке-пробнике сравнивает его с фактом движения мыши: хук,
    /// снятый системой по таймауту, снимается МОЛЧА — узнать о нём можно
    /// только по тишине (см. доккомент модуля).
    static LAST_EVENT_MS: AtomicU64 = AtomicU64::new(0);

    /// Поток-помп, которому шлётся `WM_APP_REINSTALL` (0 — помпа нет).
    static PUMP_TID: AtomicU32 = AtomicU32::new(0);

    /// Просьба потоку-помпу переустановить `WH_MOUSE_LL` (сторож).
    const WM_APP_REINSTALL: u32 = WM_APP + 0x1a;

    fn epoch() -> Instant {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        *EPOCH.get_or_init(Instant::now)
    }

    fn now_ms() -> u64 {
        epoch().elapsed().as_millis() as u64
    }

    /// Привести регистрацию окна в страже к `policy`. Идемпотентно:
    /// пустая политика — запись удаляется, при опустошении карты хуки и
    /// потоки снимаются. Не блокирующая для вызывающего (координатора),
    /// никогда не паникует.
    pub(super) fn set_policy(hwnd: HWND, policy: Policy) {
        let key = hwnd.0 as isize;
        let (need_install, need_uninstall) = {
            let mut locked = state().locked.lock().unwrap();
            let was_empty = locked.is_empty();
            if policy.is_empty() {
                locked.remove(&key);
                caption_cache::forget(hwnd);
            } else {
                locked.insert(key, policy);
            }
            (
                // Не «переход из пустой карты», а «карта непуста»: прошлая
                // установка могла провалиться (нет интерактивного десктопа,
                // отказ системы), и тогда единственный шанс подняться —
                // следующий вызов. `install_hook` идемпотентен, живой хук
                // повторно не ставится (ревью 2026-08-21, пункт 5.2).
                !locked.is_empty(),
                !was_empty && locked.is_empty(),
            )
        };
        if need_install {
            install_hook();
        }
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

    /// Поставить `WH_MOUSE_LL`. `None` — сессия без интерактивного десктопа
    /// или отказ системы.
    fn install_mouse_hook() -> Option<HHOOK> {
        // SAFETY: WH_MOUSE_LL с dwThreadId = 0 вызывается в контексте
        // УСТАНОВИВШЕГО потока (инъекции в чужие процессы нет), поэтому lpfn —
        // обычная функция этого модуля, а hmod — текущий модуль.
        let hook = unsafe {
            SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(guard_mouse_proc),
                Some(GetModuleHandleW(None).unwrap_or_default().into()),
                0,
            )
        };
        match hook {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "замки окна: не удалось поставить WH_MOUSE_LL — блокировки мыши не работают"
                );
                None
            }
        }
    }

    /// Поток-помп: владелец `WH_MOUSE_LL` и WinEvent-хука
    /// `EVENT_SYSTEM_MOVESIZESTART` (второй эшелон move-lock'а). Оба хука
    /// требуют цикла сообщений на СВОЁМ потоке — отсюда общий помп.
    fn start_hook_thread() -> Option<ActiveHook> {
        let (ready_tx, ready_rx) = mpsc::channel::<Option<u32>>();
        let thread = std::thread::Builder::new()
            .name("resticker-input-guard".into())
            .spawn(move || {
                let Some(first) = install_mouse_hook() else {
                    let _ = ready_tx.send(None);
                    return;
                };
                let mut hook = Some(first);
                // SAFETY: движение/ресайз чужого окна — out-of-context хук
                // (WINEVENT_OUTOFCONTEXT), колбэк доставляется в очередь
                // ЭТОГО потока; idprocess/idthread = 0 — вся сессия.
                let move_hook = unsafe {
                    SetWinEventHook(
                        EVENT_SYSTEM_MOVESIZESTART,
                        EVENT_SYSTEM_MOVESIZESTART,
                        None,
                        Some(movesize_proc),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    )
                };
                if move_hook.0.is_null() {
                    tracing::warn!(
                        "замок перемещения: EVENT_SYSTEM_MOVESIZESTART не поставлен — второй эшелон (обрыв уже начатого драга) недоступен"
                    );
                }
                // SAFETY: GetCurrentThreadId не может провалиться.
                let tid = unsafe { GetCurrentThreadId() };
                PUMP_TID.store(tid, Ordering::Release);
                LAST_EVENT_MS.store(now_ms(), Ordering::Relaxed);
                let _ = ready_tx.send(Some(tid));
                let mut msg = MSG::default();
                // SAFETY: стандартный msg-loop потока-помпа; колбэк хука
                // система вызывает синхронно на этом потоке между итерациями
                // GetMessageW. Выход — по WM_QUIT из uninstall_hook.
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        if msg.message == WM_APP_REINSTALL {
                            if let Some(old) = hook.take() {
                                let _ = UnhookWindowsHookEx(old);
                            }
                            // Между проглоченным down и его up хук исчез —
                            // парность больше не действует (ревью, пункт 1.3).
                            SWALLOWED_DOWN.store(false, Ordering::Relaxed);
                            match install_mouse_hook() {
                                Some(h) => {
                                    hook = Some(h);
                                    LAST_EVENT_MS.store(now_ms(), Ordering::Relaxed);
                                    tracing::warn!(
                                        "замки окна: WH_MOUSE_LL был снят системой (превышен LowLevelHooksTimeout) — переустановлен"
                                    );
                                }
                                // Помп НЕ убиваем: сторож попробует снова
                                // (иначе обе блокировки умирали бы навсегда и
                                // молча — ревью, пункт 5.2).
                                None => tracing::warn!(
                                    "замки окна: переустановить WH_MOUSE_LL не удалось — повторю по сторожу"
                                ),
                            }
                        }
                    }
                }
                PUMP_TID.store(0, Ordering::Release);
                SWALLOWED_DOWN.store(false, Ordering::Relaxed);
                // SAFETY: оба хука сняты с потока-владельца и больше не
                // используются.
                unsafe {
                    if let Some(h) = hook.take() {
                        let _ = UnhookWindowsHookEx(h);
                    }
                    if !move_hook.0.is_null() {
                        let _ = UnhookWinEvent(move_hook);
                    }
                }
            })
            .expect("input-guard: не удалось создать поток");
        match ready_rx.recv() {
            Ok(Some(tid)) => Some(ActiveHook {
                tid,
                thread: Some(thread),
                prober: ht_probe::start(),
            }),
            _ => None,
        }
    }

    fn uninstall_hook() {
        let active = state().hook.lock().unwrap().take();
        let Some(active) = active else {
            return;
        };
        // SAFETY: WM_QUIT в поток-помп — штатное завершение GetMessageW
        // (вернёт 0), поток снимет хуки и выйдет; join дождётся этого.
        unsafe {
            let _ = PostThreadMessageW(active.tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = active.thread {
            let _ = thread.join();
        }
        SWALLOWED_DOWN.store(false, Ordering::Relaxed);
        ht_probe::stop(active.prober);
    }

    /// Диагностика для ignored-тестов: поставлен ли хук сейчас.
    #[cfg(test)]
    pub(super) fn hook_active() -> bool {
        state().hook.lock().unwrap().is_some()
    }

    /// Диагностика для тестов: политика, зарегистрированная за окном.
    #[cfg(test)]
    pub(super) fn policy_of(hwnd: HWND) -> Option<Policy> {
        state()
            .locked
            .lock()
            .unwrap()
            .get(&(hwnd.0 as isize))
            .copied()
    }

    /// Колбэк `WH_MOUSE_LL`. Ненулевой возврат выбрасывает событие из
    /// input-очереди ещё ДО системного hit-testing/активации — окно не
    /// получает ни `WM_NCHITTEST`, ни `WM_MOUSEACTIVATE`, ни самого клика,
    /// модальный цикл перетаскивания не запускается.
    ///
    /// Бюджет — микросекунды (см. доккомент модуля): только `try_lock`,
    /// один `WindowFromPoint` и чтение кэша hit-test'а.
    unsafe extern "system" fn guard_mouse_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            LAST_EVENT_MS.store(now_ms(), Ordering::Relaxed);
            let msg = wparam.0 as u32;
            // SAFETY: lparam от системы указывает на живую MSLLHOOKSTRUCT на
            // время вызова колбэка (контракт WH_MOUSE_LL).
            let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if msg == WM_MOUSEMOVE {
                // Движение мыши никогда не глотается — только будит пробника,
                // чтобы к моменту нажатия у нас был свежий настоящий hit-test.
                ht_probe::wake();
            } else if is_down_message(msg) || is_wheel_message(msg) {
                if let Some((hwnd, policy)) = locked_target_at(ms.pt) {
                    let swallow = if is_wheel_message(msg) {
                        policy.interact_locked
                    } else {
                        should_swallow_button(hwnd, ms.pt, policy)
                    };
                    if swallow {
                        if is_down_message(msg) {
                            SWALLOWED_DOWN.store(true, Ordering::Relaxed);
                        }
                        return LRESULT(1);
                    }
                }
                if is_down_message(msg) {
                    SWALLOWED_DOWN.store(false, Ordering::Relaxed);
                }
            } else if is_up_message(msg) && SWALLOWED_DOWN.swap(false, Ordering::Relaxed) {
                // Down проглочен — глотаем и парный up. Up после драга,
                // начавшегося ВНЕ заблокированного окна, не трогаем (иначе
                // сломали бы перетаскивание, завершающееся над ним).
                return LRESULT(1);
            }
        }
        // SAFETY: CallNextHookEx передаёт событие дальше по цепочке хуков.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// Глотать ли нажатие кнопки по заблокированному окну в точке `pt`.
    ///
    /// ГЛАВНОЕ ПРАВИЛО (репорт 2026-08-21, вторая итерация): замок кликов
    /// НИКОГДА не должен мешать таскать окно. Поэтому решение
    /// **fail-open**: клик глотается, только если точка ДОКАЗАННО относится к
    /// содержимому окна. Доказательством считается либо свежий настоящий
    /// `WM_NCHITTEST` от самого окна, либо геометрия — но геометрии верим
    /// лишь у окон с НАСТОЯЩЕЙ неклиентской полосой заголовка
    /// (`has_real_caption`).
    ///
    /// Почему так: у приложений с собственным заголовком (Chrome, Discord и
    /// прочий Electron, VS Code, Steam, WinUI3, Tauri) клиентская область
    /// покрывает всё окно, включая нарисованный ими заголовок. Геометрия для
    /// такого окна отвечает `HTCLIENT` ВЕЗДЕ — и первая версия фикса глотала
    /// нажатие по заголовку, то есть замок кликов снова отбирал перемещение.
    /// Теперь при неопределённости клик проходит: цена — редкий пропущенный
    /// клик по содержимому (когда окно не ответило на опрос), выгода —
    /// перетаскивание не ломается никогда.
    ///
    /// Move-lock таким ограничением не связан: у него есть второй и третий
    /// эшелоны (`movesize_proc` + `enforce_move_lock`), поэтому промах
    /// геометрии для него не фатален.
    fn should_swallow_button(hwnd: HWND, pt: POINT, policy: Policy) -> bool {
        if let Some(ht) = ht_probe::hit_test(hwnd, pt) {
            if policy.move_locked && is_move_ht(ht) {
                return true;
            }
            return policy.interact_locked
                && is_interact_ht(ht)
                && !geometry_contradicts_content(hwnd, pt);
        }
        let ht = ht_from_geometry(hwnd, pt);
        if policy.move_locked && is_move_ht(ht) {
            return true;
        }
        policy.interact_locked && ht == HTCLIENT && has_real_caption(hwnd)
    }

    /// Геометрия ПРОТИВОРЕЧИТ ответу окна «здесь содержимое».
    ///
    /// Проверка есть только у окон с настоящим системным заголовком: там мы
    /// знаем неклиентскую полосу точно (её считаем мы сами, в физических
    /// координатах), и если точка лежит в ней, а окно ответило `HTCLIENT` —
    /// верить окну нельзя. Два известных источника такого расхождения
    /// (оба найдены ревью 2026-08-21):
    /// * РАЗНАЯ DPI-осведомлённость процессов. Мы per-monitor aware и шлём
    ///   физическую точку, а DPI-unaware приложение читает её через свою
    ///   виртуализацию — точка с полосы заголовка попадает в его логическую
    ///   клиентскую область, и ответ `HTCLIENT` выглядит правдоподобно.
    ///   Фильтр `HTNOWHERE`/`HTERROR` в `ht_probe::probe` такое не ловит.
    /// * Устаревший на пару пикселей кэш у самой границы заголовка и
    ///   содержимого (курсор быстро перешёл вниз и сразу нажал).
    ///
    /// В обоих случаях цена ошибки — заблокированное перетаскивание, то есть
    /// ровно то, что запрещено (см. `should_swallow_button`), поэтому при
    /// расхождении клик пропускается. У окон с собственным заголовком
    /// геометрия ничего не знает и в спор не вступает — там ответ окна
    /// остаётся единственным и главным источником.
    fn geometry_contradicts_content(hwnd: HWND, pt: POINT) -> bool {
        has_real_caption(hwnd) && ht_from_geometry(hwnd, pt) != HTCLIENT
    }

    /// У окна есть НАСТОЯЩАЯ неклиентская полоса заголовка (система рисует
    /// заголовок сама, клиентская область начинается ниже). Признак —
    /// вертикальный зазор между верхом окна и верхом клиентской области.
    ///
    /// Порог 12 px разделяет два мира: у обычного окна Win32 полоса — высота
    /// заголовка (примерно 31 px при 100 процентах) плюс рамка, у окна с
    /// собственным заголовком (`WM_NCCALCSIZE` съедает неклиентскую область)
    /// зазор нулевой или в пределах невидимой рамки ресайза Windows 11
    /// (около 8 px).
    const REAL_CAPTION_MIN_PX: i32 = 12;

    fn has_real_caption(hwnd: HWND) -> bool {
        // Сначала — прямой ответ DWM: зона кнопок заголовка непуста ТОЛЬКО у
        // окон, которым системный заголовок рисует сам DWM. У окна с
        // собственным заголовком (Chrome/Electron/VS Code/Steam) она пуста —
        // это независимое подтверждение «системного заголовка нет», причём
        // работающее и для окон с высоким уровнем целостности (запрос идёт в
        // dwm.exe, а не в процесс окна, UIPI его не режет). Кэшируется
        // потоком-пробником — в колбэке только чтение (см. `caption_cache`).
        if let Some(known) = caption_cache::get(hwnd) {
            return known;
        }
        // SAFETY: все вызовы — чтения геометрии живого/чужого окна.
        unsafe {
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return false;
            }
            let mut cr = RECT::default();
            if GetClientRect(hwnd, &mut cr).is_err() {
                return false;
            }
            let mut tl = POINT {
                x: cr.left,
                y: cr.top,
            };
            if !ClientToScreen(hwnd, &mut tl).as_bool() {
                return false;
            }
            tl.y - wr.top >= REAL_CAPTION_MIN_PX
        }
    }

    /// Второй эшелон move-lock'а: пользователь всё-таки вошёл в модальный
    /// цикл перемещения/ресайза (тач, перо, Alt+Space → «Переместить», промах
    /// кэша hit-test'а) — обрываем цикл `WM_CANCELMODE`'ом. `DefWindowProc`
    /// на это сообщение отпускает захват мыши и выходит из цикла, окно
    /// остаётся там, где было на момент старта; остаточное расхождение
    /// подберёт [`WindowPins::enforce_move_lock`] (третий эшелон).
    unsafe extern "system" fn movesize_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        idobject: i32,
        idchild: i32,
        _tid: u32,
        _time: u32,
    ) {
        if event != EVENT_SYSTEM_MOVESIZESTART
            || idobject != OBJID_WINDOW.0
            || idchild != CHILDID_SELF as i32
        {
            return;
        }
        let move_locked = state()
            .locked
            .try_lock()
            .ok()
            .and_then(|m| m.get(&(hwnd.0 as isize)).copied())
            .is_some_and(|p| p.move_locked);
        if !move_locked {
            return;
        }
        // SAFETY: PostMessageW безопасен для чужого/мёртвого окна (вернёт
        // ошибку, которую игнорируем). Именно Post, не Send: колбэк
        // WinEvent'а не должен блокироваться в чужом процессе.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CANCELMODE, WPARAM(0), LPARAM(0));
        }
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
        matches!(
            msg,
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP
        )
    }

    fn is_wheel_message(msg: u32) -> bool {
        matches!(msg, WM_MOUSEWHEEL | WM_MOUSEHWHEEL)
    }

    /// Hit-test-коды, означающие «пользователь берётся ЗА ОКНО» — заголовок,
    /// рамки, уголок ресайза. Именно их глотает move-lock. Кнопки заголовка
    /// (`HTCLOSE`/`HTMINBUTTON`/`HTMAXBUTTON`) сюда НЕ входят: закрыть или
    /// свернуть закреплённое окно пользователь вправе — замок про положение,
    /// а не про существование окна.
    pub(super) fn is_move_ht(ht: u32) -> bool {
        matches!(
            ht,
            HTCAPTION
                | HTLEFT
                | HTRIGHT
                | HTTOP
                | HTTOPLEFT
                | HTTOPRIGHT
                | HTBOTTOM
                | HTBOTTOMLEFT
                | HTBOTTOMRIGHT
                | HTBORDER
                | HTGROWBOX
        )
    }

    /// Hit-test-коды «содержимого» окна — их глотает interact-lock. Заголовок
    /// и рамки сюда НЕ входят: interact-lock не должен мешать двигать окно
    /// (ради этого он и переписан с `EnableWindow(FALSE)` на хук).
    pub(super) fn is_interact_ht(ht: u32) -> bool {
        matches!(ht, HTCLIENT | HTMENU | HTSYSMENU | HTVSCROLL | HTHSCROLL)
    }

    /// Заблокированное окно, которому система доставила бы клик в `pt`.
    /// `WindowFromPoint` уже учитывает и видимость, и `WS_EX_TRANSPARENT`
    /// (клик-сквозные оверлеи resticker), и z-order — один вызов вместо
    /// обхода всего десктопа. `GetAncestor(GA_ROOT)`: попасть можно в
    /// дочерний контрол, а заблокировано top-level окно.
    fn locked_target_at(pt: POINT) -> Option<(HWND, Policy)> {
        // SAFETY: WindowFromPoint/GetAncestor — чтения состояния десктопа,
        // безопасны для любых координат.
        let root = unsafe {
            let child = window_from_point(pt);
            if child.0.is_null() {
                return None;
            }
            GetAncestor(child, GA_ROOT)
        };
        if root.0.is_null() {
            return None;
        }
        let policy = state()
            .locked
            .try_lock()
            .ok()?
            .get(&(root.0 as isize))
            .copied()?;
        Some((root, policy))
    }

    /// `WindowFromPoint` в обёртке: сигнатура в `windows` принимает POINT по
    /// значению, выделено для читаемости `unsafe`-блока выше.
    unsafe fn window_from_point(pt: POINT) -> HWND {
        // SAFETY: см. вызывающий код.
        unsafe { windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(pt) }
    }

    /// Дешёвая (без z-order) проверка «курсор над заблокированным окном» —
    /// для потока-пробника, который так решает, кого опрашивать. Блокирующий
    /// лок здесь допустим: вызывается НЕ из колбэка хука.
    fn locked_window_under_blocking(pt: POINT) -> Option<isize> {
        let map = state().locked.lock().ok()?;
        map.keys()
            .copied()
            .find(|&key| locked_window_receives_click_at(super::hwnd_from_isize(key), pt))
    }

    /// Живое видимое НЕ-transparent окно, чей прямоугольник содержит `pt`.
    /// Отсекает записи-«призраки» (окно уничтожено, а запись в карте ещё
    /// есть). `IsWindowEnabled` здесь СОЗНАТЕЛЬНО не проверяется: страж
    /// больше не вызывает `EnableWindow(FALSE)`, заблокированное окно
    /// остаётся enabled — прежняя проверка сделала бы предикат вечно ложным.
    fn locked_window_receives_click_at(hwnd: HWND, pt: POINT) -> bool {
        // SAFETY: IsWindowVisible/GetWindowLongPtrW/GetWindowRect безопасны
        // для чужих и мёртвых хэндлов.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
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
            rect_contains(&rect, pt)
        }
    }

    fn rect_contains(rect: &RECT, pt: POINT) -> bool {
        pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom
    }

    /// Грубая оценка hit-test'а по геометрии — запасной путь, когда кэш
    /// пробника пуст (первое нажатие, окно «задумалось», UIPI).
    ///
    /// ЧЕСТНО О ТОЧНОСТИ: оценка врёт на приложениях с собственным
    /// заголовком (Electron/Tauri/Chrome/VS Code/Discord/Steam рисуют
    /// «заголовок» внутри клиентской области и отвечают `HTCAPTION` из неё) —
    /// там клиентский прямоугольник покрывает почти всё окно, и мы вернём
    /// `HTCLIENT`. Поэтому это именно fallback: основной источник —
    /// настоящий `WM_NCHITTEST` из [`ht_probe`].
    pub(super) fn ht_from_geometry(hwnd: HWND, pt: POINT) -> u32 {
        // SAFETY: все вызовы — чтения геометрии, безопасны для чужих окон.
        unsafe {
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() || !rect_contains(&wr, pt) {
                return HTNOWHERE;
            }
            let mut cr = RECT::default();
            if GetClientRect(hwnd, &mut cr).is_err() {
                return HTCAPTION;
            }
            let mut tl = POINT {
                x: cr.left,
                y: cr.top,
            };
            let mut br = POINT {
                x: cr.right,
                y: cr.bottom,
            };
            if ClientToScreen(hwnd, &mut tl).as_bool() && ClientToScreen(hwnd, &mut br).as_bool() {
                let client = RECT {
                    left: tl.x,
                    top: tl.y,
                    right: br.x,
                    bottom: br.y,
                };
                if rect_contains(&client, pt) {
                    return HTCLIENT;
                }
            }
            // Внутри окна, но вне клиентской области — заголовок или рамка;
            // для обеих блокировок это один и тот же класс решений.
            HTCAPTION
        }
    }

    /// Кэш признака «у окна настоящий системный заголовок» (`DWM`-ответ, см.
    /// `has_real_caption`). Наполняется потоком-пробником, читается колбэком
    /// хука через `try_lock`: сам запрос в DWM стоит десятки микросекунд —
    /// заметная доля бюджета колбэка (1 мс), поэтому в колбэке его нет.
    mod caption_cache {
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Dwm::{DWMWA_CAPTION_BUTTON_BOUNDS, DwmGetWindowAttribute};

        use super::HWND;

        /// Стиль окна может смениться на лету (frameless-режим, полноэкранный
        /// вид) — запись живёт недолго и переспрашивается.
        const TTL: Duration = Duration::from_secs(2);

        static CACHE: Mutex<Option<HashMap<isize, (bool, Instant)>>> = Mutex::new(None);

        /// Известный (свежий) ответ для окна. `None` — не спрашивали, ответ
        /// протух или карта занята: вызывающий решает по геометрии.
        pub(super) fn get(hwnd: HWND) -> Option<bool> {
            let guard = CACHE.try_lock().ok()?;
            let map = guard.as_ref()?;
            let &(value, at) = map.get(&(hwnd.0 as isize))?;
            (at.elapsed() <= TTL).then_some(value)
        }

        /// Спросить DWM и запомнить (вызывается только из потока-пробника).
        pub(super) fn refresh(hwnd: HWND) {
            let key = hwnd.0 as isize;
            let fresh = {
                let Ok(guard) = CACHE.lock() else {
                    return;
                };
                guard
                    .as_ref()
                    .and_then(|m| m.get(&key))
                    .is_some_and(|&(_, at)| at.elapsed() <= TTL)
            };
            if fresh {
                return;
            }
            let Some(value) = query_dwm(hwnd) else {
                return; // DWM не ответил — пусть решает геометрия
            };
            if let Ok(mut guard) = CACHE.lock() {
                guard
                    .get_or_insert_with(HashMap::new)
                    .insert(key, (value, Instant::now()));
            }
        }

        /// Убрать запись окна (снятие блокировки/открепление).
        pub(super) fn forget(hwnd: HWND) {
            if let Ok(mut guard) = CACHE.lock() {
                if let Some(map) = guard.as_mut() {
                    map.remove(&(hwnd.0 as isize));
                }
            }
        }

        /// Непустая зона кнопок заголовка ⇒ системный заголовок есть.
        /// `None` — DWM не ответил (окно умирает, композитор не знает окна).
        fn query_dwm(hwnd: HWND) -> Option<bool> {
            let mut rect = RECT::default();
            // SAFETY: rect — валидный буфер под RECT; запрос идёт в DWM и
            // безопасен для чужих окон.
            let ok = unsafe {
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CAPTION_BUTTON_BOUNDS,
                    (&raw mut rect).cast(),
                    size_of::<RECT>() as u32,
                )
            }
            .is_ok();
            ok.then_some(rect.right > rect.left && rect.bottom > rect.top)
        }
    }

    /// Асинхронный опрос настоящего `WM_NCHITTEST` у чужого окна.
    ///
    /// Почему отдельный поток: `SendMessage*` в чужой процесс — синхронное
    /// ожидание чужой очереди сообщений (десятки мс на загруженном окне), а
    /// бюджет колбэка LL-хука на этой машине 1 мс (см. доккомент модуля).
    /// Колбэк только КЛАДЁТ запрос (`request`) и ЧИТАЕТ результат
    /// (`hit_test`), оба — через `try_lock`, без единого блокирующего вызова.
    ///
    /// `SMTO_ABORTIFHUNG` (а не `SMTO_BLOCK`) принципиален: колбэк LL-хука
    /// приходит как ОТПРАВЛЕННОЕ сообщение, и блокирующий режим заморозил бы
    /// его доставку на время опроса.
    mod ht_probe {
        use super::*;

        /// Результат опроса годен, если точка нажатия рядом с опрошенной и
        /// он не протух. Допуск узкий: у самой границы заголовка и
        /// содержимого широкая полоса давала бы устаревший `HTCLIENT` для
        /// точки, уже попавшей на заголовок (ревью 2026-08-21), а промах
        /// кэша безопасен — это fail-open.
        const TOLERANCE_PX: i32 = 3;
        const FRESH: Duration = Duration::from_millis(1_000);
        /// Как часто обновлять ответ для НЕПОДВИЖНОГО курсора над
        /// заблокированным окном.
        const REFRESH: Duration = Duration::from_millis(80);
        /// Такт опроса, пока курсор над заблокированным окном.
        const TICK: Duration = Duration::from_millis(10);
        /// Потолок ожидания чужого окна. Пробник живёт на своём потоке и
        /// никого не задерживает, поэтому таймаут щедрый: Electron-окно под
        /// нагрузкой отвечает не за 30 мс, а промах опроса означает откат к
        /// геометрии, которая для таких окон врёт (см. `should_swallow_button`).
        const PROBE_TIMEOUT_MS: u32 = 80;
        /// Сторож: столько тишины при движущейся мыши считаем «хук сняли».
        const SILENCE_MS: u64 = 2_000;
        /// Не просить переустановку чаще этого интервала.
        const REINSTALL_COOLDOWN_MS: u64 = 5_000;

        /// Когда сторож в последний раз просил переустановить хук (мс от
        /// `epoch()`); 0 — не просил ни разу.
        static LAST_REINSTALL_MS: AtomicU64 = AtomicU64::new(0);

        #[derive(Clone, Copy)]
        struct Sample {
            key: isize,
            pt: POINT,
            ht: u32,
            at: Instant,
        }

        static LAST: Mutex<Option<Sample>> = Mutex::new(None);
        static STOP: AtomicBool = AtomicBool::new(false);
        static WORKER: OnceLock<Mutex<Option<std::thread::Thread>>> = OnceLock::new();

        fn worker_slot() -> &'static Mutex<Option<std::thread::Thread>> {
            WORKER.get_or_init(|| Mutex::new(None))
        }

        /// Разбудить пробника (вызывается из колбэка на любом движении мыши).
        /// Никогда не блокирует: занятый лок — просто пропуск такта.
        pub(super) fn wake() {
            if let Ok(worker) = worker_slot().try_lock() {
                if let Some(thread) = worker.as_ref() {
                    thread.unpark();
                }
            }
        }

        /// Настоящий hit-test точки для `hwnd`, если он у нас есть и свежий.
        /// `None` — опроса нет (окно не ответило, курсор только что пришёл,
        /// UIPI): решение принимает вызывающий по геометрии, консервативно.
        pub(super) fn hit_test(hwnd: HWND, pt: POINT) -> Option<u32> {
            let key = hwnd.0 as isize;
            LAST.try_lock()
                .ok()
                .and_then(|g| *g)
                .filter(|s| {
                    s.key == key
                        && (s.pt.x - pt.x).abs() <= TOLERANCE_PX
                        && (s.pt.y - pt.y).abs() <= TOLERANCE_PX
                        && s.at.elapsed() <= FRESH
                })
                .map(|s| s.ht)
        }

        pub(super) fn start() -> Option<std::thread::JoinHandle<()>> {
            STOP.store(false, Ordering::Release);
            let handle = std::thread::Builder::new()
                .name("resticker-ht-probe".into())
                .spawn(|| {
                    if let Ok(mut slot) = worker_slot().lock() {
                        *slot = Some(std::thread::current());
                    }
                    probe_loop();
                    if let Ok(mut slot) = worker_slot().lock() {
                        *slot = None;
                    }
                })
                .ok()?;
            Some(handle)
        }

        pub(super) fn stop(handle: Option<std::thread::JoinHandle<()>>) {
            STOP.store(true, Ordering::Release);
            wake();
            if let Some(handle) = handle {
                let _ = handle.join();
            }
            if let Ok(mut last) = LAST.lock() {
                *last = None;
            }
        }

        /// Пробник ведёт кэш САМ, по живой позиции курсора, а не по заявкам из
        /// колбэка: так свежий ответ есть даже тогда, когда пользователь
        /// подвёл курсор и нажал сразу, без промежуточных `WM_MOUSEMOVE`.
        /// Пока курсор не над заблокированным окном, цикл ничего не делает и
        /// спит.
        fn probe_loop() {
            let mut watchdog_at = Instant::now();
            let mut watchdog_cursor = cursor_pos();
            while !STOP.load(Ordering::Acquire) {
                let pt = cursor_pos();
                if let Some(key) = super::locked_window_under_blocking(pt) {
                    super::caption_cache::refresh(super::super::hwnd_from_isize(key));
                    if needs_probe(key, pt) {
                        if let Some(ht) = probe(key, pt) {
                            if let Ok(mut last) = LAST.lock() {
                                *last = Some(Sample {
                                    key,
                                    pt,
                                    ht,
                                    at: Instant::now(),
                                });
                            }
                        }
                    }
                }
                if watchdog_at.elapsed() >= Duration::from_millis(SILENCE_MS) {
                    let cursor = cursor_pos();
                    watchdog_if_silent(cursor, watchdog_cursor);
                    watchdog_cursor = cursor;
                    watchdog_at = Instant::now();
                }
                std::thread::park_timeout(TICK);
            }
        }

        /// Опрашивать ли точку заново: другого окна, сдвинувшегося курсора или
        /// протухшего ответа достаточно.
        fn needs_probe(key: isize, pt: POINT) -> bool {
            let Ok(last) = LAST.lock() else {
                return true;
            };
            match last.as_ref() {
                Some(s) => {
                    s.key != key || s.pt.x != pt.x || s.pt.y != pt.y || s.at.elapsed() >= REFRESH
                }
                None => true,
            }
        }

        fn cursor_pos() -> POINT {
            let mut pt = POINT::default();
            // SAFETY: GetCursorPos пишет в наш стек, ошибку игнорируем
            // (сессия без десктопа — вернём (0,0), сторож просто промолчит).
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            pt
        }

        /// Мышь движется, а колбэк молчит — единственный наблюдаемый признак
        /// того, что система сняла LL-хук по превышению
        /// `LowLevelHooksTimeout` (уведомления об этом нет). Просим поток-помп
        /// поставить хук заново.
        fn watchdog_if_silent(cursor: POINT, previous: POINT) {
            if cursor.x == previous.x && cursor.y == previous.y {
                return; // мышь неподвижна — тишина законна
            }
            if now_ms().saturating_sub(LAST_EVENT_MS.load(Ordering::Relaxed)) < SILENCE_MS {
                return;
            }
            // Переустановка могла и не удаться (сессия без десктопа): просить
            // её чаще, чем раз в `REINSTALL_COOLDOWN_MS`, бессмысленно —
            // получился бы поток сообщений в помп (ревью 2026-08-21, 5.3).
            let last_request = LAST_REINSTALL_MS.load(Ordering::Relaxed);
            let now = now_ms();
            if last_request != 0 && now.saturating_sub(last_request) < REINSTALL_COOLDOWN_MS {
                return;
            }
            let tid = PUMP_TID.load(Ordering::Acquire);
            if tid == 0 {
                return;
            }
            LAST_REINSTALL_MS.store(now, Ordering::Relaxed);
            // SAFETY: PostThreadMessageW безопасен; мёртвый tid — ошибка,
            // которую игнорируем (поток-помп как раз завершается).
            unsafe {
                let _ = PostThreadMessageW(tid, WM_APP_REINSTALL, WPARAM(0), LPARAM(0));
            }
        }

        /// Настоящий `WM_NCHITTEST` у чужого окна. `None` — окно мертво,
        /// «задумалось» (`SMTO_ABORTIFHUNG`), закрыто UIPI или ответило
        /// бессмысленным для нас кодом (`HTNOWHERE`/`HTERROR` при том, что
        /// точка внутри окна, — верный признак несовпадения систем координат
        /// при разной DPI-осведомлённости процессов).
        fn probe(key: isize, pt: POINT) -> Option<u32> {
            let hwnd = super::super::hwnd_from_isize(key);
            // SAFETY: IsWindow безопасен для любых значений.
            if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                return None;
            }
            // Точка приходит из хука в ФИЗИЧЕСКИХ пикселях (наш процесс
            // per-monitor aware), а окно прочитает lParam в СВОЁМ
            // DPI-пространстве: USER32 координаты в сообщении не
            // транслирует. У DPI-unaware цели на масштабе 125-200% это дало
            // бы правдоподобно неверный ответ (точка с заголовка попадает в
            // логическую клиентскую область) — то есть проглоченный клик по
            // заголовку и сломанное перетаскивание. `PhysicalToLogicalPoint            // ForPerMonitorDPI` переводит точку в пространство именно этого
            // окна; для цели с нашей осведомлённостью это тождество.
            // HT-код безразмерный, обратный перевод не нужен.
            let mut pt = pt;
            // SAFETY: пишет в нашу переменную; неудача (окно умерло, точка
            // вне окна) оставляет её нетронутой — тогда шлём как есть.
            unsafe {
                let _ = PhysicalToLogicalPointForPerMonitorDPI(Some(hwnd), &mut pt);
            }
            // MAKELPARAM(x, y) — координаты экранные, младшие 16 бит каждая
            // (получатель разворачивает знак через GET_X_LPARAM).
            let packed = (((pt.y as u16 as u32) << 16) | (pt.x as u16 as u32)) as isize;
            let mut result: usize = 0;
            // SAFETY: SendMessageTimeoutW с SMTO_ABORTIFHUNG не зависает на
            // мёртвом/повисшем окне; result пишется только при успехе.
            let ok = unsafe {
                SendMessageTimeoutW(
                    hwnd,
                    WM_NCHITTEST,
                    WPARAM(0),
                    LPARAM(packed),
                    SEND_MESSAGE_TIMEOUT_FLAGS(SMTO_ABORTIFHUNG.0),
                    PROBE_TIMEOUT_MS,
                    Some(&mut result),
                )
            };
            if ok.0 == 0 {
                return None;
            }
            let ht = result as u32;
            (ht != HTNOWHERE && ht != HTERROR_CODE).then_some(ht)
        }

        /// `HTERROR` (-2) в 32-битном виде: окно ответило «ошибка».
        const HTERROR_CODE: u32 = -2i32 as u32;
    }

    #[cfg(test)]
    pub(super) mod tests {
        use super::*;
        use windows::Win32::Foundation::POINT;

        /// Move-lock глотает «взяться за окно» и НЕ трогает содержимое —
        /// иначе замок перемещения запрещал бы ещё и пользоваться окном.
        #[test]
        fn move_ht_covers_caption_and_frame_only() {
            assert!(is_move_ht(HTCAPTION));
            assert!(is_move_ht(HTBOTTOMRIGHT));
            assert!(is_move_ht(HTGROWBOX));
            assert!(!is_move_ht(HTCLIENT));
            assert!(!is_move_ht(HTNOWHERE));
        }

        /// Interact-lock глотает содержимое и НЕ трогает заголовок/рамки —
        /// ровно тот баг, из-за которого «замок взаимодействия» отбирал
        /// перемещение окна (репорт 2026-08-20).
        #[test]
        fn interact_ht_leaves_caption_movable() {
            assert!(is_interact_ht(HTCLIENT));
            assert!(is_interact_ht(HTVSCROLL));
            assert!(!is_interact_ht(HTCAPTION));
            assert!(!is_interact_ht(HTBOTTOMRIGHT));
            assert!(!is_interact_ht(HTBORDER));
        }

        /// `has_real_caption` — единственный сигнал, по которому геометрии
        /// вообще можно верить у interact-lock'а: окно с настоящим
        /// системным заголовком отличается от окна, рисующего заголовок
        /// само (там клиентская область покрывает всё, и геометрия ответила
        /// бы `HTCLIENT` даже в заголовке).
        #[test]
        fn real_caption_detected_only_for_system_caption() {
            use windows::Win32::UI::WindowsAndMessaging::{
                WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
            };

            let with_caption =
                crate::window_pin::tests::TestWindow::create_with(WS_OVERLAPPEDWINDOW | WS_VISIBLE);
            assert!(
                has_real_caption(with_caption.0),
                "окно со штатным заголовком должно опознаваться"
            );

            let without = crate::window_pin::tests::TestWindow::create_with(WS_POPUP | WS_VISIBLE);
            assert!(
                !has_real_caption(without.0),
                "окно без неклиентской полосы не должно считаться заголовочным"
            );
        }

        /// Расхождение «окно сказало содержимое, а геометрия видит
        /// неклиентскую полосу» трактуется в пользу перетаскивания —
        /// но только у окон с настоящим системным заголовком: у окна с
        /// собственным заголовком геометрия не знает ничего и спорить не
        /// вправе (ревью 2026-08-21, пункт 1.1).
        #[test]
        fn geometry_only_argues_for_windows_with_system_caption() {
            use windows::Win32::UI::WindowsAndMessaging::{
                WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
            };

            let framed =
                crate::window_pin::tests::TestWindow::create_with(WS_OVERLAPPEDWINDOW | WS_VISIBLE);
            let mut wr = RECT::default();
            // SAFETY: окно живо.
            unsafe { GetWindowRect(framed.0, &mut wr) }.unwrap();
            let caption = POINT {
                x: (wr.left + wr.right) / 2,
                y: wr.top + 2,
            };
            let inside = POINT {
                x: (wr.left + wr.right) / 2,
                y: (wr.top + wr.bottom) / 2,
            };
            assert!(
                geometry_contradicts_content(framed.0, caption),
                "точка на заголовке обязана опровергать ответ «это содержимое»"
            );
            assert!(
                !geometry_contradicts_content(framed.0, inside),
                "точка в клиентской области ничему не противоречит"
            );

            // Окно без системного заголовка: геометрия молчит всегда.
            let frameless =
                crate::window_pin::tests::TestWindow::create_with(WS_POPUP | WS_VISIBLE);
            // SAFETY: окно живо.
            unsafe { GetWindowRect(frameless.0, &mut wr) }.unwrap();
            let top_edge = POINT {
                x: (wr.left + wr.right) / 2,
                y: wr.top + 2,
            };
            assert!(
                !geometry_contradicts_content(frameless.0, top_edge),
                "у окна с собственным заголовком геометрия не вправе спорить"
            );
        }

        /// Пустая политика снимает запись: не осталось заблокированных окон —
        /// не осталось и хуков (refcount по содержимому карты).
        #[test]
        fn empty_policy_removes_registration() {
            let hwnd = HWND(0x7fff_0001 as *mut core::ffi::c_void);
            set_policy(
                hwnd,
                Policy {
                    move_locked: true,
                    interact_locked: false,
                },
            );
            assert!(
                state()
                    .locked
                    .lock()
                    .unwrap()
                    .contains_key(&(hwnd.0 as isize))
            );
            set_policy(
                hwnd,
                Policy {
                    move_locked: false,
                    interact_locked: false,
                },
            );
            assert!(
                !state()
                    .locked
                    .lock()
                    .unwrap()
                    .contains_key(&(hwnd.0 as isize))
            );
        }
    }
}

/// Пользователь В ЭТОТ МОМЕНТ тащит `hwnd` настоящим OS-драгом — модальный
/// цикл перемещения/ресайза (`WM_ENTERSIZEMOVE` → `WM_EXITSIZEMOVE`) в
/// процессе самого окна.
///
/// Источник правды — `GetGUIThreadInfo` потока-владельца окна: флаг
/// `GUI_INMOVESIZE` + `hwndMoveSize == hwnd`. Это ЕДИНСТВЕННЫЙ способ
/// узнать о чужом модальном цикле снаружи: подкласса чужого окна у нас нет.
///
/// БЫЛО (баг до 2026-08-21): `GetCapture() == hwnd`. `GetCapture` возвращает
/// окно с захватом мыши в очереди ВЫЗЫВАЮЩЕГО потока — для чужого окна это
/// всегда NULL, то есть предикат был вечно ложным и «не драться с живым
/// драгом» никогда не срабатывало.
fn user_is_dragging_window(hwnd: HWND) -> bool {
    // SAFETY: GetWindowThreadProcessId безопасен для чужих и мёртвых окон
    // (0 — окна нет); GetGUIThreadInfo пишет в наш стек и для чужого потока
    // легален (это и есть его назначение — межпоточная диагностика ввода).
    unsafe {
        let tid = GetWindowThreadProcessId(hwnd, None);
        if tid == 0 {
            return false;
        }
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(tid, &mut info).is_err() {
            return false;
        }
        info.flags.contains(GUI_INMOVESIZE) && info.hwndMoveSize == hwnd
    }
}

/// Окно свёрнуто (`IsIconic`) — платформенно-чистая обёртка для
/// координатора: он решает по этому признаку, прятать окно или уже нечего
/// (см. `rst_core::pinned_window::host_action`).
pub fn is_window_minimized(hwnd: usize) -> bool {
    // SAFETY: IsIconic безопасен для чужих и мёртвых окон.
    unsafe { IsIconic(hwnd_from_usize(hwnd)) }.as_bool()
}

/// Прервать модальный цикл перемещения/ресайза, который пользователь ведёт
/// над `hwnd` прямо сейчас (`WM_CANCELMODE`).
///
/// Зачем координатору: потолок размера закреплённого окна нельзя навязать,
/// пока цикл жив. Реактивная коррекция каждые 16 мс — это тяга-перетяга с
/// рукой пользователя (дрожь, репорты 2026-08-17 и 2026-08-21), а ожидание
/// конца жеста означает «окно всё-таки выросло, а потом прыгнуло назад».
/// Единственный способ остановить рост ровно на границе — закончить сам
/// жест: `DefWindowProc` на `WM_CANCELMODE` отпускает захват мыши и выходит
/// из цикла, окно остаётся там, где было, и следующая же коррекция ставит
/// ему предельный размер один раз, без спора.
///
/// Именно `Post`, а не `Send`: ждать чужую очередь сообщений из координатора
/// нельзя, а цикл сам её качает и заберёт сообщение на ближайшей итерации.
pub fn cancel_user_gesture(hwnd: usize) {
    let hwnd = hwnd_from_usize(hwnd);
    // SAFETY: PostMessageW безопасен для чужого и мёртвого окна (вернёт
    // ошибку, которую игнорируем).
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CANCELMODE, WPARAM(0), LPARAM(0));
    }
}

/// Окно развёрнуто на весь монитор (`SW_SHOWMAXIMIZED`) — платформенно-чистая
/// обёртка для координатора: разворот это тоже «расширение до 100%», и
/// принудительный потолок размера обязан его снимать, но не бесконечно (см.
/// `enforce_pinned_geometry`: повторный разворот в течение секунды не
/// оспаривается, иначе спор с приложением, которое разворачивает себя само,
/// превратился бы в мигание).
pub fn is_window_maximized(hwnd: usize) -> bool {
    is_maximized(hwnd_from_usize(hwnd))
}

/// Окно развёрнуто на весь монитор (`SW_SHOWMAXIMIZED`). Отдельная
/// проверка нужна там, где мы навязываем окну размер: развёрнутому окну
/// `SetWindowPos` меняет прямоугольник, но не снимает `WS_MAXIMIZE`.
fn is_maximized(hwnd: HWND) -> bool {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: placement заполнена (length обязателен); GetWindowPlacement
    // безопасен для чужих и мёртвых окон.
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.is_ok()
        && placement.showCmd == SW_SHOWMAXIMIZED.0 as u32
}

/// Пользователь ПРЯМО СЕЙЧАС тащит или ресайзит это окно (модальный цикл
/// `WM_ENTERSIZEMOVE` в процессе окна) — платформенно-чистая обёртка над
/// [`user_is_dragging_window`] для координатора: тому нужно знать, «жест ещё
/// идёт» или «пользователь уже отпустил», чтобы не драться с рукой и
/// применять магнит/кламп размера ровно один раз после отпускания.
pub fn is_user_dragging(hwnd: usize) -> bool {
    user_is_dragging_window(hwnd_from_usize(hwnd))
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

/// То же, но из сырого `HWND.0` (ключи книжек блокировок — `isize`).
fn hwnd_from_isize(hwnd: isize) -> HWND {
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
        CreateWindowExW, DefWindowProcW, DestroyWindow, GW_HWNDNEXT, GW_HWNDPREV, GWL_STYLE,
        GWLP_USERDATA, GetWindow, GetWindowLongPtrW, RegisterClassExW, WINDOW_STYLE, WNDCLASSEXW,
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

        pub(super) fn create_with(style: WINDOW_STYLE) -> Self {
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
        assert!(
            !is_topmost(hwnd),
            "имитация выбивания из topmost не сработала"
        );
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
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowRect, IsZoomed, SW_MAXIMIZE, ShowWindow,
        };

        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");

        // SAFETY: target — живое окно текущего потока; SW_MAXIMIZE — обычный
        // show-command.
        unsafe {
            let _ = ShowWindow(target.0, SW_MAXIMIZE);
        }
        // SAFETY: чтение состояния живого окна.
        assert!(
            unsafe { IsZoomed(target.0) }.as_bool(),
            "окно должно стать maximized"
        );

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
    /// Идти вверх по z-order от `from` и искать `want`.
    ///
    /// Предел шагов — не «сколько окон бывает», а страховка от зацикливания:
    /// раньше стояло 64, и тест падал на машине с большой сессией, где между
    /// обычной полосой и topmost-полосой оказывалось больше окон (найдено
    /// 2026-08-24 — падало без единой правки в этом файле).
    fn window_above_in_walk(from: HWND, want: HWND) -> bool {
        let mut cur = from;
        for _ in 0..4096 {
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
        assert!(
            is_topmost(bottom.0),
            "временный подъём ставит WS_EX_TOPMOST"
        );
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
        unsafe { SetWindowPos(hwnd, None, 40, 50, 100, 100, SWP_NOZORDER | SWP_NOACTIVATE) }
            .expect("движение тестового окна");

        // Координатор увидел location-change и передаёт фактический rect —
        // в DWM-координатах, как в снимке трекера.
        let current = dwm_rect(hwnd);
        assert_ne!(current, baseline, "окно реально уехало от эталона");
        assert!(
            pins.enforce_move_lock(hwnd, current),
            "snap-back обязан сработать"
        );

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
        unsafe { SetWindowPos(hwnd, None, 40, 50, 100, 100, SWP_NOZORDER | SWP_NOACTIVATE) }
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

    /// Реальный OS-драг move-locked окна (репорты 2026-08-17 и 2026-08-20):
    /// настоящее видимое окно тащат за настоящий заголовок инъекцией мыши
    /// (`SendInput` — полный путь input-routing, включая LL-хуки).
    ///
    /// Фаза A (контроль): БЕЗ замка драг обязан реально двигать окно —
    /// иначе тест не воспроизвёл перетаскивание и фаза B ничего не значит.
    /// Фаза B (фикс): с move-lock'ом окно не должно сдвинуться НИ НА ПИКСЕЛЬ:
    /// страж ввода глотает нажатие по заголовку, модальный цикл не
    /// запускается. До фикса (реактивный snap-back) окно свободно ездило всё
    /// время драга и телепортировалось назад только после отпускания — ровно
    /// то, что видно на видео пользователя.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_pin -- --ignored"]
    fn move_lock_blocks_live_drag() {
        use std::time::{Duration, Instant};
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput, VK_LBUTTON,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetCursorPos, GetSystemMetrics, GetWindowRect, HWND_TOP, SM_CYCAPTION, SWP_NOACTIVATE,
            SWP_NOZORDER, SetCursorPos, SetWindowPos, WindowFromPoint,
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

        fn inject(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) {
            // SAFETY: SendInput — системная инъекция ввода.
            let sent =
                unsafe { SendInput(&[mouse_input(flags, dx, dy)], size_of::<INPUT>() as i32) };
            assert_eq!(sent, 1, "SendInput не применился");
        }

        fn window_rect(hwnd: HWND) -> RECT {
            let mut r = RECT::default();
            // SAFETY: окно живо.
            unsafe { GetWindowRect(hwnd, &mut r) }.expect("GetWindowRect");
            r
        }

        fn tracker_rect(hwnd: HWND) -> RECT {
            let dwm = extended_frame_bounds(hwnd);
            RECT {
                left: dwm.x,
                top: dwm.y,
                right: dwm.x + dwm.w,
                bottom: dwm.y + dwm.h,
            }
        }

        #[derive(Debug)]
        struct Step {
            /// `left` окна на этом шаге драга (куда его увела мышь).
            window_left: i32,
            /// Сработал ли snap-back (третий эшелон) на этом шаге.
            snapped: bool,
            /// Окно в модальном цикле перемещения (`GUI_INMOVESIZE`).
            dragging: bool,
            /// Левая кнопка ещё нажата — шаг внутри драга.
            lbutton_down: bool,
            /// Позиция курсора: проверка, что инъекция реально двигает мышь.
            cursor: (i32, i32),
        }

        const DELTAS: [(i32, i32); 10] = [
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

        let win = RealWindow::create();
        let hwnd = win.hwnd;
        let mut pins = WindowPins::new();

        // Положить окно в известное место ВТОРОГО монитора (физические px
        // виртуального десктопа; второй монитор здесь свободен — на основном
        // может жить полноэкранное приложение, которое съедало бы инъекцию).
        let place = (-1700i32, 500i32);
        let put_at_baseline = || {
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
        };
        put_at_baseline();

        // Точка в заголовке: центр по X, середина по высоте caption.
        let baseline = window_rect(hwnd);
        // SAFETY: GetSystemMetrics — чтение системной метрики.
        let caption_h = unsafe { GetSystemMetrics(SM_CYCAPTION) };
        let cx = baseline.left + (baseline.right - baseline.left) / 2;
        let cy = baseline.top + caption_h / 2;
        // Диагностика: клик в (cx, cy) обязан достаться нашему окну, иначе
        // драг не начнётся (чужое полноэкранное окно сверху съест инъекцию).
        // На занятом десктопе тест честно пропускает реальный драг, а не
        // падает: сам механизм покрыт unit-тестами.
        // SAFETY: WindowFromPoint — чтение hwnd под точкой, безопасно.
        let hit = unsafe { WindowFromPoint(POINT { x: cx, y: cy }) };
        if hit != hwnd {
            eprintln!(
                "live drag: десктоп занят (клик в ({cx},{cy}) достанется чужому окну {hit:?}, \
                 не нашему {hwnd:?}) — реальный драг пропущен, механизм покрыт unit-тестами"
            );
            return;
        }

        // Один сеанс драга за заголовок с кормлением enforce на каждом шаге
        // (ровно то, что делает координатор по снимку трекера).
        let drag = |pins: &mut WindowPins| -> Vec<Step> {
            // SAFETY: SetCursorPos — установка курсора в экранных координатах.
            let _ = unsafe { SetCursorPos(cx, cy) };
            std::thread::sleep(Duration::from_millis(120));
            inject(MOUSEEVENTF_LEFTDOWN, 0, 0);
            std::thread::sleep(Duration::from_millis(120));
            let mut steps = Vec::new();
            for (dx, dy) in DELTAS {
                inject(MOUSEEVENTF_MOVE, dx, dy);
                std::thread::sleep(Duration::from_millis(16));
                let wr = window_rect(hwnd);
                let snapped = pins.enforce_move_lock(hwnd, tracker_rect(hwnd));
                let mut cur = POINT::default();
                // SAFETY: чтения глобального состояния ввода/курсора.
                let (lbutton_down, cursor) = unsafe {
                    let down = GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0;
                    let _ = GetCursorPos(&mut cur);
                    (down, (cur.x, cur.y))
                };
                steps.push(Step {
                    window_left: wr.left,
                    snapped,
                    dragging: user_is_dragging_window(hwnd),
                    lbutton_down,
                    cursor,
                });
            }
            inject(MOUSEEVENTF_LEFTUP, 0, 0);
            std::thread::sleep(Duration::from_millis(200));
            steps
        };

        let spread = |steps: &[Step]| -> i32 {
            let xs: Vec<i32> = steps
                .iter()
                .filter(|s| s.lbutton_down)
                .map(|s| s.window_left)
                .collect();
            match (xs.iter().min(), xs.iter().max()) {
                (Some(min), Some(max)) => max - min,
                _ => 0,
            }
        };

        // --- Фаза A: БЕЗ замка. Инъекция обязана реально таскать окно.
        let free = drag(&mut pins);
        let free_spread = spread(&free);
        let cursor_moved = free
            .first()
            .zip(free.last())
            .map(|(a, b)| a.cursor != b.cursor)
            .unwrap_or(false);
        eprintln!(
            "live drag: без замка разброс left={free_spread}px, курсор двигался={cursor_moved}, \
             модальный цикл замечен={}",
            free.iter().any(|s| s.dragging)
        );
        assert!(
            free_spread > 50,
            "окно не уехало при драге БЕЗ замка — тест не воспроизвёл перетаскивание: {free:#?}"
        );
        // Предикат «пользователь тащит окно» обязан срабатывать на живом
        // драге: на нём стоит защита от драки snap-back'а с рукой
        // пользователя. Старый `GetCapture() == hwnd` был вечно ложным для
        // чужого окна — это и был баг (см. `user_is_dragging_window`).
        assert!(
            free.iter().any(|s| s.dragging),
            "GUI_INMOVESIZE не замечен ни на одном шаге живого драга: {free:#?}"
        );

        // --- Фаза B: С замком. Окно не должно сдвинуться вовсе.
        put_at_baseline();
        let baseline = window_rect(hwnd);
        pins.set_move_lock(hwnd, true);
        // Дать потоку-стражу поставить WH_MOUSE_LL.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            input_guard::hook_active(),
            "страж ввода должен быть активен при move-lock"
        );
        let locked = drag(&mut pins);
        let locked_spread = spread(&locked);
        let snaps = locked.iter().filter(|s| s.snapped).count();
        eprintln!(
            "live drag: с замком разброс left={locked_spread}px, snap-back'ов={snaps}, \
             модальный цикл замечен={}",
            locked.iter().any(|s| s.dragging)
        );
        assert_eq!(
            locked_spread, 0,
            "move-locked окно сдвинулось во время драга на {locked_spread}px — \
             нажатие по заголовку не было проглочено: {locked:#?}"
        );
        assert_eq!(
            window_rect(hwnd),
            baseline,
            "move-locked окно уехало с эталона за время драга"
        );
        // Раз окно не двигалось, третьему эшелону нечего возвращать.
        assert_eq!(
            snaps, 0,
            "snap-back сработал, хотя окно не двигалось: {locked:#?}"
        );

        // Нет вечного snap-back-цикла на покое: следующий снимок трекера не
        // должен дёргать неподвижное окно.
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            assert!(
                !pins.enforce_move_lock(hwnd, tracker_rect(hwnd)),
                "покоящееся окно на эталоне не должно snap-back'аться"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        pins.set_move_lock(hwnd, false);
    }

    #[test]
    fn interact_lock_never_disables_window() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        let hwnd = target.0;
        // SAFETY: чтение стиля живого окна.
        let style = |h: HWND| unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;

        pins.set_interact_lock(hwnd, true);
        // РЕГРЕССИЯ (репорт 2026-08-20): раньше здесь стоял
        // `EnableWindow(FALSE)`, и `WS_DISABLED` отбирал у окна ВСЁ, включая
        // перемещение за заголовок. Теперь блокировка живёт только в страже
        // ввода: стили чужого окна не трогаются вовсе.
        // SAFETY: IsWindowEnabled — чтение состояния живого окна.
        assert!(
            unsafe { IsWindowEnabled(hwnd) }.as_bool(),
            "interact-lock не должен делать окно disabled"
        );
        assert_eq!(style(hwnd) & WS_DISABLED.0, 0, "WS_DISABLED не ставится");

        pins.set_interact_lock(hwnd, false);
        // SAFETY: IsWindowEnabled — чтение состояния живого окна.
        assert!(unsafe { IsWindowEnabled(hwnd) }.as_bool());
        assert_eq!(style(hwnd) & WS_DISABLED.0, 0);
    }

    /// Осиротевший маркер прошлого запуска: окно помечено, но нашей книжки
    /// оно не знает. Такое окно обязано убираться стартовой уборкой, а если
    /// всплыло позже — перениматься (репорт 2026-08-21: Проводник и Блокнот
    /// переживают наши перезапуски и копят «вечные» маркеры).
    #[test]
    fn orphan_marker_is_cleared_and_adoptable() {
        let target = TestWindow::create();
        let hwnd = target.0;
        let mut previous_run = WindowPins::new();
        previous_run
            .pin(MARKER, key(hwnd))
            .expect("пин прошлого запуска");
        // Прошлый запуск «умер» без unpin — маркер остался на окне.
        drop(previous_run);

        let mut pins = WindowPins::new();
        assert!(pins.is_pinned(key(hwnd)), "маркер пережил процесс");
        assert!(
            matches!(pins.pin(MARKER, key(hwnd)), Err(Win32Error::AlreadyPinned)),
            "обычный пин обязан отказать — иначе мы бы затирали чужую метку вслепую"
        );

        // Второй рубеж: перенять окно можно всегда.
        pins.adopt(MARKER, key(hwnd))
            .expect("перенять осиротевшее окно");
        pins.unpin(key(hwnd)).expect("и открепить его");
        assert!(!pins.is_pinned(key(hwnd)), "маркер снят");

        // Первый рубеж: стартовая уборка снимает маркер по снимку окон.
        let mut previous_run = WindowPins::new();
        previous_run
            .pin(MARKER, key(hwnd))
            .expect("пин прошлого запуска");
        drop(previous_run);
        let fresh = WindowPins::new();
        let snapshot = vec![WindowInfo {
            hwnd: key(hwnd),
            ..Default::default()
        }];
        assert_eq!(fresh.clear_orphan_markers(&snapshot), 1);
        assert!(
            !fresh.is_pinned(key(hwnd)),
            "уборка сняла осиротевший маркер"
        );
    }

    #[test]
    fn unpin_releases_both_locks() {
        let target = TestWindow::create();
        let mut pins = WindowPins::new();
        pins.pin(MARKER, key(target.0)).expect("пин");
        pins.set_move_lock(target.0, true);
        pins.set_interact_lock(target.0, true);
        let policy = input_guard::policy_of(target.0).expect("окно зарегистрировано в страже");
        assert!(policy.move_locked && policy.interact_locked);

        pins.unpin(key(target.0)).expect("unpin");

        // Открепление сняло окно со стража и забыло move-lock: enforce не
        // двигает окно даже при расхождении rect'а.
        assert_eq!(input_guard::policy_of(target.0), None);
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
                CreateWindowExW, DispatchMessageW, GWLP_USERDATA, GetMessageW, MSG,
                RegisterClassExW, SW_SHOW, SetWindowLongPtrW, ShowWindow, TranslateMessage,
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
    #[ignore = "требует реальный десктоп и реальный ввод; запуск вручную: cargo test -p rst-win32 window_pin input_guard_swallows_real_click -- --ignored"]
    fn input_guard_swallows_real_click_on_locked_window() {
        use std::time::Duration;
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, SetCursorPos, WM_ACTIVATE, WM_LBUTTONDOWN, WM_MOUSEACTIVATE,
            WM_NCACTIVATE, WindowFromPoint,
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
                    &[
                        mouse_input(MOUSEEVENTF_LEFTDOWN),
                        mouse_input(MOUSEEVENTF_LEFTUP),
                    ],
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

        // --- Фаза 1: БАЗА. Окно не заблокировано — реальный клик проходит
        // весь системный input-routing и доходит до wndproc.
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
            win.received_any(&[WM_MOUSEACTIVATE, WM_LBUTTONDOWN]),
            "база: незаблокированное окно должно получать клик; сообщения: {msgs:?}"
        );

        // --- Фаза 2: ФИКС. interact-lock (страж ввода, WH_MOUSE_LL на своём
        // потоке-помпе) → тот же клик по КЛИЕНТСКОЙ области поглощается ДО
        // системного input-routing: окно не получает ни одного сообщения
        // клика/активации, foreground не трогается.
        pins.set_interact_lock(hwnd, true);
        assert!(
            input_guard::hook_active(),
            "WH_MOUSE_LL должен быть активен после set_interact_lock"
        );
        // Дать потоку-помпу хука войти в GetMessageW.
        std::thread::sleep(Duration::from_millis(200));
        let fg_before = unsafe { GetForegroundWindow() };
        win.clear_log();
        click_at(cx, cy);
        let msgs2 = win.log.lock().unwrap().clone();
        assert!(
            !win.received_any(&[
                WM_MOUSEACTIVATE,
                WM_LBUTTONDOWN,
                0x0084,
                WM_NCACTIVATE,
                WM_ACTIVATE
            ]),
            "клик по interact-locked окну должен быть поглощён ДО системного input-routing; сообщения: {msgs2:?}"
        );
        assert_eq!(
            unsafe { GetForegroundWindow() },
            fg_before,
            "поглощённый клик не должен менять foreground"
        );
        // РЕГРЕССИЯ (репорт 2026-08-20): interact-lock больше НЕ ставит
        // WS_DISABLED — иначе он отбирал бы у окна и перемещение, и кнопки
        // заголовка, и заставлял систему пищать на каждый клик.
        assert!(
            unsafe { IsWindowEnabled(hwnd) }.as_bool(),
            "interact-lock не должен делать окно disabled (иначе ломается перемещение)"
        );

        // Снять блокировку — хук снимается, клик снова доходит до окна.
        pins.set_interact_lock(hwnd, false);
        assert!(
            !input_guard::hook_active(),
            "WH_MOUSE_LL должен сняться после последнего unlock"
        );
        std::thread::sleep(Duration::from_millis(100));
        win.clear_log();
        click_at(cx, cy);
        let msgs3 = win.log.lock().unwrap().clone();
        assert!(
            win.received_any(&[WM_MOUSEACTIVATE, WM_LBUTTONDOWN]),
            "после снятия блокировки клик должен снова доходить; сообщения: {msgs3:?}"
        );
    }

    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 window_pin reassert_topmost -- --ignored"]
    fn reassert_topmost_restores_after_external_knockout_live() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            SW_MINIMIZE, SW_RESTORE, SetWindowPos, ShowWindow,
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
        assert!(
            !is_topmost(hwnd),
            "имитация выбивания из topmost не сработала"
        );
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
