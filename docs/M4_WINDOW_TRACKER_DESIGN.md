# M4 — проектирование среза: WindowTracker (инкрементальный кэш окон на WinEvent-хуках)

Статус: **скоупинг, код не менялся** (docs only). Ветка `feat/m2-edit-mode`.
Основание: ROADMAP.md «M4 — слои видимости», DECISIONS.md ADR-004/ADR-005,
ARCHITECTURE.md §3.2–3.5, docs/M4_PREP_NOTES.md §2–3, §7–9. Прочитаны:
`crates/rst-win32/src/window_enum.rs` (готов — enumerate/is_real_window/
WindowInfo, DWMWA_EXTENDED_FRAME_BOUNDS), `crates/rst-win32/src/overlay.rs`
(поток+pump+канал, WTSRegisterSessionNotification, Drop→join),
`crates/rst-win32/src/tray.rs` (тот же паттерн), `crates/resticker/src/overlay_manager.rs`
(объединённый канал `OverlayMessage`), `crates/rst-win32/Cargo.toml` (фичи `windows`).

Цель документа: конкретные решения для среза «инкрементальный кэш окон»
(ROADMAP-бокс «Инкрементальный кэш окон на WinEvent-хуках, дебаунс 16 мс»;
M4_PREP_NOTES §8, шаг 2). Маска перекрытия и её шейдер — **отдельный** срез
(M4_PREP_NOTES §4), здесь — только трекер и его стыковка с координатором.
Имена функций/полей/точек вызова — как в текущем коде; номера строк будут
дрейфовать, имена нет.

---

## 0. Текущее состояние (что уже есть)

- `crates/rst-win32/src/window_enum.rs` готов и покрыт тестами (ROADMAP-боксы
  «Перечисление…» и «DWMWA_EXTENDED_FRAME_BOUNDS» закрыты, коммит `d41d86c`):
  `enumerate() -> Vec<WindowInfo>` (полное перечисление, z-order сверху вниз),
  чистый фильтр `is_real_window` (видим/не cloaked/root/не NOACTIVATE/
  tool-window-и-owner-правила), `WindowInfo { hwnd: usize, rect: WindowRect
  (физические px), pid, exe_path, title, class, z_order, iconic, icon: None }`,
  `WindowRect`/`WindowIcon` типы наружу. Отмечено в шапке: «инкрементальный кэш
  на WinEvent-хуках — отдельный модуль (`WindowTracker`)».
- `Cargo.toml` rst-win32: `Win32_Graphics_Dwm` и `Win32_System_Threading` уже
  подключены; **`Win32_UI_Accessibility` (SetWinEventHook/UnhookWinEvent +
  константы событий) отсутствует** — единственная новая фича среза.
  `Win32_System_RemoteDesktop` (WTS) уже есть.
- Паттерн «поток + pump + канал» отработан трижды: `overlay.rs`
  (`create_on_monitor` → `(Self, Receiver<OverlayEvent>)`, Drop → `WM_CLOSE` +
  join), `tray.rs`, per-monitor форвардеры в `overlay_manager.rs`.
- Координатор: один объединённый канал `OverlayMessage`; ветки
  `Event(MonitorId, OverlayEvent)` обрабатываются последовательно (FIFO).
- Модель готова: `VisibilityMode::{Always, Desktop, NeverOverlap,
  OverlapAllowlist}` + `rules: Vec<OverlapRule>` (model.rs:259-292);
  `mask_needed = cfg.stickers.iter().any(|s| s.visibility.mode != Always)`
  (M4_PREP_NOTES §6.4) — предикат, которым гейтится весь срез.
- Оверлеи фильтруются из списка окон самим `is_real_window`
  (WS_EX_NOACTIVATE), лишняя работа не нужна.

Ключевой вывод: трекер — первый **живущий** потребитель готового
`window_enum`; его граница — «кэш живёт на своём потоке, наружу — полные
снимки», маска/панель — потребители снимков.

---

## 1. API и модель потока

```rust
// crates/rst-win32/src/window_tracker.rs (новый модуль)

/// Свежий снимок всех «реальных» окон. Полный список, а не дифф: 25–30 окон,
/// копия дёшева, а идемпотентность избавляет координатор от дифф-логики
/// (M4_PREP_NOTES §3.3 — «решает вопрос „кто владеет копией“»).
pub enum WindowEvent {
    Changed(Vec<WindowInfo>),
}

pub struct WindowTracker { /* tx: Sender<Control>, thread: JoinHandle<()> */ }

impl WindowTracker {
    /// Запустить поток трекера (свой GetMessage-pump) и вернуть приёмник
    /// событий. Хуки на этом этапе НЕ ставятся — трекер «спит» до первого
    /// `set_mask_needed(true)` (fast path, ADR-005).
    pub fn start() -> Result<(Self, Receiver<WindowEvent>), Win32Error>;

    /// Гейт хуков: `true` — поставить WinEvent-хуки и раздать полное
    /// перечисление; `false` — снять хуки, очистить кэш/dirty, эмиссию
    /// прекратить (fast path). Трекер при этом живёт и принимает control.
    pub fn set_mask_needed(&self, needed: bool);
}
```

- **Поток.** Свой `GetMessage`-цикл (ADR-013; хуки `WINEVENT_OUTOFCONTEXT`
  доставляют колбэк в очередь потока-установщика — ему нужен работающий pump,
  ADR-005). Наружу — копии снимков; кэш целиком живёт на потоке трекера,
  мьютексов не требуется.
- **Control-канал** (из координатора): `enum Control { SetMaskNeeded(bool),
  ForceRefresh, Shutdown }` — приёмник control'а живёт на потоке трекера
  (`recv` с таймаутом в цикле или `PostMessage` на message-only окно, §2);
  `ForceRefresh` — полное перечисление + emit (отладка, разблокировка сессии,
  панель). `Drop` → `Shutdown` + join (паттерн `OverlayWindow::drop`,
  overlay.rs:363-378; join обязателен — поток держит хуки).
- **Ошибки.** Новый вариант `Win32Error::WindowTrackerHookFailed` (по образцу
  существующих `Tray*`/`Overlay*` вариантов): `SetWinEventHook`/`SetTimer`
  неуспех — трекер продолжает жить без хуков с warn-логом, либо `start()`
  возвращает ошибку (создание окна/регистрация класса — фатально).

## 2. Pump: message-only окно как якорь таймера и сессии

Колбэки WinEvent приходят в очередь потока и вызываются самим `GetMessageW` —
DispatchMessage для них не нужен. Но дебаунс 16 мс и заморозка по блокировке
сессии требуют пробуждений, которых в чистом `GetMessage` нет. Решение — одно
**message-only окно** (`CreateWindowExW` с `HWND_MESSAGE`), владеющее:

- `WM_TIMER` — дебаунс: первый грязный хук заводит `SetTimer(16)`;
- `WM_WTSSESSION_CHANGE` — `WTSRegisterSessionNotification` на этом окне
  (паттерн overlay.rs:617-623) → заморозка/разморозка кэша (§7);
- `WM_APP_*` — внутренние команды (в т.ч. `ForceRefresh` из control-канала,
  чтобы не смешивать блокирующий `recv` с pump'ом).

Цикл: `GetMessage → Translate → Dispatch` (стандартный, как в
`overlay.rs::run_message_loop`). Рассмотренный альтернативный вариант
`MsgWaitForMultipleObjectsEx` + waitable timer — отклонён: в кодовой базе нет
прецедента, а message-only окно нужно в любом случае (WTS требует HWND), так
что таймер через него — нулевая новая механика.

**Дебаунс (M4_PREP_NOTES §3.3, ARCHITECTURE §3.4):**
- колбэк хука делает ровно одно: кладёт hwnd в `dirty: HashSet<usize>` и, если
  таймер не заведён, `SetTimer(16)` — **никаких Win32-запросов в колбэке**
  (он может вызываться часто; вся работа — в обработчике `WM_TIMER`);
- `WM_TIMER`: `now - first_dirty >= 16 мс` → применить отложенные изменения к
  кэшу (§4), собрать `Vec<WindowInfo>`, `send(Changed)`, очистить `dirty`,
  `KillTimer`; иначе — перезавести на остаток. События, пришедшие во время
  таймера, просто добавляются в `dirty` — естественное коалесцирование
  (перетаскивание окна = один emit на 16 мс, а не десятки).

## 3. Хуки: таблица событий и фильтр

`SetWinEventHook` с флагами `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS`
(свои оверлеи и так отсечены фильтром `is_real_window` — SKIP дешевле).

| Событие | Ставим (mask-режим) | Действие |
|---|---|---|
| `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_HIDE` | да | полное `enum_windows()` (появилось/исчезло окно; заодно свежий z-order) |
| `EVENT_OBJECT_DESTROY` | да | инкрементально убрать hwnd из кэша |
| `EVENT_SYSTEM_MINIMIZESTART` / `MINIMIZEEND` | да | инкрементально выставить/снять `iconic` (у свёрнутых rect мусорный — не оклюдер) |
| `EVENT_OBJECT_LOCATIONCHANGE` | да | инкрементально обновить rect конкретного hwnd (только если он в кэше) |
| `EVENT_SYSTEM_FOREGROUND` | **нет** | z-order маске не нужен (ADR-004 §4.7 — вырезаем объединение rect'ов); полное перечисление даст только лишний emit. Понадобится в panel-режиме (панель выбора, §8) |
| `EVENT_OBJECT_NAMECHANGE` | нет | заголовки нужны панели, не маске — panel-срез |
| `EVENT_SYSTEM_MOVESIZESTART` / `MOVESIZEEND` | нет | 16 мс дебаунс и так коалесцирует поток LOCATIONCHANGE (M4_PREP_NOTES §3.2 — «опционально», откладываем) |

**Фильтр колбэка** (первая строка, до любых действий):
`idObject == OBJID_WINDOW && idChild == CHILDID_SELF` — иначе return. Для
LOCATIONCHANGE дополнительно: hwnd в кэше? нет → return (не «реальное» окно —
не отслеживаем; M4_PREP_NOTES §3.2).

## 4. Кэш и инкрементальные обновления (чистые функции — юнит-тесты без окон)

Кэш: `Vec<WindowInfo>` (порядок = z-order) + `HashSet<usize>` для проверки
членства. Все мутации — через чистые функции (прецедент извлечения: `is_real_window`,
`handle_dpi_changed`):

```rust
/// Удалить hwnd; `true` — был в кэше (и надо ли emit — решает caller).
fn apply_destroy(cache: &mut Vec<WindowInfo>, hwnd: usize) -> bool;
/// Выставить/снять iconic. На MINIMIZEEND заодно пересобрать rect (DWM
/// отдаст свежий после восстановления).
fn apply_minimize(cache: &mut Vec<WindowInfo>, hwnd: usize, iconic: bool) -> bool;
/// Обновить rect (только член кэша; у iconic — пропустить, rect мусорный).
fn apply_location(cache: &mut Vec<WindowInfo>, hwnd: usize, rect: WindowRect) -> bool;
/// Полное перечисление (SHOW/HIDE/FOREGROUND(panel)/force): заменяет кэш.
fn rebuild_from_enum(cache: &mut Vec<WindowInfo>) -> Vec<WindowInfo>;
```

- **DESTROY-гонка с hwnd-reuse**: перед инкрементальными апдейтами колбэк не
  зовёт Win32; в `WM_TIMER` перед `apply_location` — `IsWindow(hwnd)`-гард
  (окно могло умереть без DESTROY-события — редкий, но документированный случай).
- **Процесс-кэш `pid → exe_path`** (M4_PREP_NOTES §9): `window_enum::process_info`
  делает `OpenProcess` на каждое окно при каждом полном перечислении — дорого
  и может отказывать для защищённых процессов. Трекер держит
  `HashMap<u32, PathBuf>`, наполняемый при `rebuild_from_enum`; новые окна
  (SHOW) берут путь из кэша по pid, `OpenProcess` — только при промахе.
  Staleness пути при перезапуске процесса с тем же pid — приемлема (правило
  матчится по пути, а не pid; следующий полный enum перечитает).
- **Эмиссия**: после применения отложенных изменений — полный снимок
  `Vec<WindowInfo>` (порядок = порядок кэша). Координатор решает, менять ли
  маску (отсечка пересечений — его зона, M4_PREP_NOTES §3.5), трекер диффы
  не считает.

## 5. Fast path: `mask_needed == false` → хуков нет вовсе

- `set_mask_needed(false)` (с потока координатора): `UnhookWinEvent` для всех
  зарегистрированных событий, `dirty.clear()`, `KillTimer`, кэш очистить.
  Поток продолжает крутить pump и обрабатывать control (память и CPU ≈ 0 —
  очередь пуста).
- `set_mask_needed(true)`: `SetWinEventHook` (все события §3) → полное
  перечисление → `emit Changed` — координатор сразу получает актуальный
  снимок, не дожидаясь первого события.
- Переходы идемпотентны (повторный `set_mask_needed(true)` не дублирует хуки —
  guard по «уже установлено»; ADR-005: «если ни у одного стикера нет
  ограничений — хуки не ставятся» — и снимаются при уходе в fast path).
- **Панель выбора окон** (M4 §7) в fast path останется без живого списка —
  это осознанное следствие ADR-005; панель откроет свой режим позже (§8).

## 6. Стыковка с координатором (overlay_manager.rs)

- Новый вариант объединённого канала: `OverlayMessage::Windows(WindowEvent)`
  (по образцу `Event(MonitorId, OverlayEvent)`; события окон не привязаны к
  монитору). Форвардер — как per-monitor: `thread::spawn(move || for ev in
  tracker_rx { if tx.send(OverlayMessage::Windows(ev)).is_err() { break; } })`.
  FIFO общего канала гарантирует: перерисовка маски не обгоняет обработанное
  перемещение окна (M4_PREP_NOTES §3.6).
- `run()`: `let (tracker, tracker_rx) = WindowTracker::start()?` рядом со
  стартом оверлеев; начальное `set_mask_needed(mask_needed(&cfg))`; на
  `OverlayMessage::Windows(WindowEvent::Changed(windows))` — на этом срезе
  только принять снимок (координатор хранит его под `window_snapshot`) и
  логировать; вычисление окклюдеров/маски — следующий срез.
- **Точки пересчёта `mask_needed`** — после любого изменения `cfg`:
  `perform_undo`/`perform_redo`, кнопки тулбара/панели, `begin_delete`,
  `paste_from_clipboard`, `Ctrl+D`, round-trip настроек, загрузка конфига.
  Рекомендация: один helper `fn refresh_mask_needed(cfg, tracker)` и вызовы в
  существующих точках (список выше) — плюс единое место в `run()` для
  начального состояния.
  **Фактическая реализация (коммит 88507e8) отклонилась от этой
  рекомендации осознанно** — вместо вызовов в точках мутации `mask_needed`
  пересчитывается раз за итерацию главного цикла (после `match`, до
  `redraw_all`), сравнивается с закэшированным `last_mask_needed`, и трекеру
  шлётся только при реальном изменении. Независимое ревью
  (docs/M4_WINDOWTRACKER_WIRING_REVIEW.md) проверило это построчно: все
  десять точек `continue` главного цикла мутируют `cfg` только ДО `continue`
  (если вообще мутируют), так что пропуск пересчёта на них безвреден;
  задержка гейта относительно мутации — ноль итераций. Проще (нет точек
  вызова, которые можно забыть добавить при следующей мутирующей ветке) и
  дешевле, чем кажется (O(n) по стикерам на сообщение, наносекунды при
  n≈25) — но: (а) в этой сборке **ни один живой путь координатора ещё не
  меняет `VisibilityMode`** (кнопка-глаз тулбара трогает только `visible`,
  режимы видимости приходят из окна настроек через `config.json`, а
  live-reload конфига в `run()` нет), так что «пер-итерационный» пересчёт
  сегодня фактически статичен — реально флипать гейт он начнёт только
  вместе с round-trip настроек/панелью выбора окон; (б) при масштабировании
  до тысяч стикеров стоит вернуться к точкам мутации или считать `mask_needed`
  инкрементально. Не баг — просто не там искать, если гейт «не флипает».
- Шатдаун: `tracker` — локальная переменная `run()`; локальные переменные
  дропаются в обратном порядке объявления, а хуки трекера не зависят от окон
  оверлеев, поэтому порядок относительно `monitors_map` не критичен. Одно
  требование — `Drop` (Shutdown + join) не должен блокировать шатдаун:
  в Drop трекера нет долгих операций (join ждёт выхода из pump по
  WM_QUIT/Shutdown — мгновенно), так что достаточно объявить трекер рядом с
  остальными локальными переменными `run()`.

## 7. Заморозка кэша при блокировке сессии

Message-only окно регистрирует `WTSRegisterSessionNotification(NOTIFY_FOR_THIS_SESSION)`
(паттерн overlay.rs:617-623, фича `Win32_System_RemoteDesktop` уже в
Cargo.toml). `WM_WTSSESSION_CHANGE`:

- `WTS_SESSION_LOCK` → «заморозить»: не эмитить, кэш остаётся как был
  (окна за экраном блокировки перестают быть валидными оклюдерами, но
  перечисление их в этот момент дорого и бессмысленно — M4_PREP_NOTES §9);
- `WTS_SESSION_UNLOCK` → полное `rebuild_from_enum` + `emit Changed` (свежий
  снимок после разблокировки).
- Координатор про блокировку уже знает из своих `SessionLocked`/`Unlocked`
  (M3) — дублирование не нужно: трекер самодостаточен через своё окно.

## 8. Панель выбора окон (следующий срез M4 — здесь только границы)

- Иконки **не** входят в `Changed`: `ExtractIconExW`/`SHGetFileInfoW` на 30
  окон при каждом emit — дорого и для маски не нужно. `WindowInfo.icon`
  остаётся `None`; панель грузит иконки сама (тип `WindowIcon` уже готов в
  window_enum.rs) по hwnd из снимка.
- Панели нужны: z-order (FOREGROUND → полный enum), заголовки (NAMECHANGE),
  live-обновление — т.е. «panel-режим» трекера (хуки §3 + FOREGROUND +
  NAMECHANGE) поверх того же кэша. Проектировать его вместе с панелью, не
  сейчас; API `set_mask_needed(bool)` при необходимости обобщается до
  `set_mode(Mask | Panel | Off)` без изменения кэш-ядра.

## 9. Тесты

- **Юнит (без окон, всегда зелёные):** `apply_destroy`/`apply_minimize`/
  `apply_location`/`rebuild_from_enum` — мутации кэша на синтетических
  `WindowInfo`; логика дебаунса — чистый предикат
  `should_emit(first_dirty: Instant, now: Instant) -> bool`; фильтр колбэка —
  предикат `accept_event(id_object, id_child, in_cache) -> bool`.
- **Интеграционные (`#[ignore]`, реальный десктоп — образец
  `enumerate_returns_only_real_windows` и тесты overlay.rs с `recv_timeout`):**
  `start()` → создать реальное окно `CreateWindowExW` → `Changed` содержит его
  (после SHOW); `MoveWindow` → `Changed` с новым rect (дебаунс: recv_timeout
  ~100 мс); `DestroyWindow` → `Changed` без него; `set_mask_needed(false)` →
  создание/движение окна НЕ эмитит (хуки сняты); `set_mask_needed(true)` →
  мгновенный `Changed` с полным снимком без внешних событий. Асинхронность
  событий — recv_timeout с запасом (флейки возможны на медленных машинах —
  та же политика, что в существующих тестах rst-win32).

## 10. Cargo.toml и порядок работ

Единственная новая фича: `"Win32_UI_Accessibility"` (SetWinEventHook,
UnhookWinEvent, WINEVENT_OUTOFCONTEXT, EVENT_SYSTEM_FOREGROUND,
EVENT_OBJECT_SHOW/HIDE/DESTROY/LOCATIONCHANGE/NAMECHANGE,
EVENT_SYSTEM_MINIMIZESTART/MINIMIZEEND, OBJID_WINDOW, CHILDID_SELF).

1. `window_tracker.rs`: message-only окно + pump + control-канал + `start()`
   (без хуков) + чистые функции кэша (§4) + юнит-тесты.
2. Хуки и фильтр (§3) + дебаунс (§2) + `set_mask_needed` (§5) +
   интеграционные тесты (§9).
3. WTS-заморозка (§7) + процесс-кэш (§4).
4. Сшивка в `overlay_manager`: `OverlayMessage::Windows`, `mask_needed`
   helper + точки пересчёта (§6). Маска/шейдер — следующий срез.
5. Прогон: build/clippy/test + ручная проверка «перетащи чужое окно поверх
   стикера — маска следует за окном с задержкой ≤16 мс» (после среза маски).

## 11. Открытые вопросы

- **pid → exe_path**: оставить `OpenProcess`-путь с процесс-кэшем (§4) или
  завести `CreateToolhelp32Snapshot`-снимок процессов (M4_PREP_NOTES §9) —
  решить по замерам доли промахов на реальном десктопе.
- **DESTROY без события / hwnd-reuse**: `IsWindow`-гард в `WM_TIMER`
  покрывает; оставить как есть или добавить «verify-проход» в каждом N-м
  полном перечислении — на усмотрение реализации.
- **144 Гц мониторы**: 16 мс дебаунс = маска отстаёт от движения окна на
  кадр при высоких частотах. Принято (ADR-005; CPU-цель важнее), зафиксировать
  в ROADMAP-проверке «CPU при перетаскивании окна».
- **Смена ex-style окна** (tool window ↔ app window) событий не порождает —
  кэш расходится до следующего SHOW/HIDE/FOREGROUND. Практически редко;
  принять как ограничение (документировано в ARCHITECTURE §3.5 духе).
- **Панель в fast path** (§8): живой список окон при `mask_needed == false`
  появится только с panel-режимом трекера — решение за панельным срезом.
