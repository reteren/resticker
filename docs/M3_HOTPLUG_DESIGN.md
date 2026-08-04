# M3 — проектирование среза: hot-plug мониторов (WM_DISPLAYCHANGE) + автомат ADR-011

Статус: **скоупинг, код не менялся** (docs only). Ветка `feat/m2-edit-mode`.
Прочитаны: docs/M3_PREP_NOTES.md (§2.3, §3.6, §5.3, §6), целиком
`crates/rst-core/src/monitor_loss.rs` (уже готов, покрыт тестами),
`crates/resticker/src/overlay_manager.rs` (run(), ~3000 строк),
`crates/rst-win32/src/overlay.rs` (wndproc: WM_DISPLAYCHANGE, WM_DPICHANGED),
коммиты 0027b0f (rebind_monitor_by_center) и 402eb0b (recover_device).

Цель документа: конкретные решения для следующего среза координатора —
подключение `MonitorLossTracker` к hot-plug и ввод периодического тика.
Имена функций/полей/точек вызова — как в текущем коде (номера строк будут
дрейфовать, имена нет).

---

## 0. Текущее состояние (что уже есть)

- `run()` строит `monitors_map: HashMap<MonitorId, MonitorState>` один раз при
  старте из `monitors::enumerate()` (overlay_manager.rs:607–664); рядом два
  снимка-копии: `monitor_geometry: HashMap<MonitorId, (u32, u32, f32)>`
  (:678) и `monitor_bounds: HashMap<MonitorId, MonitorBounds>` (:689).
- Окно каждого монитора создаётся `OverlayWindow::create_on_monitor(bounds,
  edit_hotkey: Option<HotkeyCombo>, toggle_all_hotkey: Option<HotkeyCombo>)`;
  хоткей режима редактирования (и `toggle_all`) регистрирует ровно одно окно
  — монитора, бывшего основным на старте (:609–613).
- Каждое окно имеет свой pump-поток и свой канал событий; по одному
  поток-форвардеру на окно (:643–652) шлёт `OverlayMessage::Event(MonitorId,
  OverlayEvent)` в общий mpsc-канал, который читает цикл `for msg in rx`.
- `OverlayEvent::MonitorsChanged(Vec<MonitorInfo>)` уже эмитится
  rst-win32 на `WM_DISPLAYCHANGE` (overlay.rs:726–738, `handle_display_change`
  переперечисляет мониторы); в координаторе ветка только логирует
  (overlay_manager.rs:883–894).
- `rst_core::monitor_loss::MonitorLossTracker` готов: `new()`,
  `on_monitor_snapshot(&[MonitorSnapshot], &[Sticker], now: Instant) ->
  Vec<LossAction>`, `is_monitor_lost(&MonitorId) -> bool`,
  `on_user_edit(&Sticker) -> Vec<LossAction>`; `LOSS_TIMEOUT = 20 с`; тик
  часов всегда инжектируется параметром `now` (внутри крейта часы не
  читаются). `MonitorSnapshot { id: MonitorId, bounds_px: Rect,
  is_primary: bool }` — в rst-win32 его «двойник» `MonitorInfo` несёт те же
  поля плюс `friendly_name`/`dpi`, конвертация тривиальная.
- В коде **нет ни одного периодического механизма** (grep по `SetTimer`/
  `WM_TIMER`/`WaitForSingleObject` — пусто; весь цикл событийный, ADR-006).

Ключевой пробел: `on_monitor_snapshot` нужно звать (1) на каждое изменение
топологии — чтобы увидеть «пропал/вернулся», и (2) периодически даже без
изменений — иначе таймер 20 с никогда не истечёт, потому что цикл
координатора чисто событийный. Оба пункта решаются в этом срезе.

---

## 1. Периодический тик: рекомендация — поток-«будильник» в bin-крейте

### Рассмотренные варианты

**(a) Лёгкий поток в `overlay_manager.rs`**, спит ~1 с и шлёт новый вариант
`OverlayMessage::Tick` в тот же канал `rx`. Полностью повторяет уже
существующий паттерн «поток на окно + сообщение в общий канал» — ничего
нового в архитектуре; на rst-win32 ноль изменений. Поток выходит сам, как
только `send` падает (канал закрыт — координатор завершился).

**(b) `WM_TIMER` на одном из оверлей-окон** (`SetTimer` в rst-win32, новое
`OverlayEvent::Tick`). Минусы: новый вариант API rst-win32; таймер привязан к
жизни конкретного окна — после сноса окна по hot-plug его нужно
перевешивать (кто-то должен остаться «хозяином таймера»); событие придёт в
поток одного из окон и уйдёт как `Event(случайный_monitor_id, Tick)` — в
координаторе id бессмысленен. Выигрыш «без нового потока» не стоит этой
связности.

**(c) Готовые механизмы в rst-win32/rst-render**: их нет (см. §0). Трей,
хоткеи, WinEvent-хуки — всё событийное; `GetMessageW` без таймаута.

**Решение: (a).**

```rust
/// Период тика автомата потери монитора (docs/M3_HOTPLUG_DESIGN.md, §1):
/// 20-секундный LOSS_TIMEOUT не требует секундной точности, но и не
/// прощает «подождать следующего WM_DISPLAYCHANGE» — тик нужен, чтобы
/// таймер мог истечь без каких-либо событий.
const LOSS_TICK_PERIOD: Duration = Duration::from_secs(1);

// Новый вариант сообщения:
enum OverlayMessage {
    Command(OverlayCommand),
    Event(MonitorId, OverlayEvent),
    /// Периодический «будильник» автомата потери монитора (§1).
    Tick,
}
```

В `run()`, перед `for msg in rx`:

```rust
let tick_tx = tx.clone();
thread::spawn(move || {
    loop {
        thread::sleep(LOSS_TICK_PERIOD);
        if tick_tx.send(OverlayMessage::Tick).is_err() {
            break; // канал закрыт — координатор завершился
        }
    }
});
```

Ветка в цикле (до §3 диспетчер — пустышка, после — полноценный):

```rust
OverlayMessage::Tick => {
    // Единственная цель тика — дать истечь LOSS_TIMEOUT. Снимок берём из
    // last_snapshot (см. §2): он не меняется между MonitorsChanged, поэтому
    // дифф внутри трекера — no-op, работает только проверка таймеров.
    let actions = loss_tracker.on_monitor_snapshot(
        &last_snapshot, &cfg.stickers, Instant::now(),
    );
    if !actions.is_empty() {
        apply_loss_actions(&mut cfg, &mut sprites, actions);
        if let Err(e) = config::save(&cfg, &config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после миграции монитора");
        }
        need_redraw = true;
    }
}
```

ADR-006 («рисовать только по событию») не нарушается: тик сам по себе не
рисует — `need_redraw` выставляется только когда автомат реально выдал
действия (т.е. случилось системное событие миграции). Красный флаг
«перерисовать по таймеру» в кодовой базе не появляется.

---

## 2. Ветка `MonitorsChanged`: дифф, жизненный цикл окон, хоткей

### Где живёт «старый снимок»

Новое поле рядом с `monitors_map`:

```rust
/// Последний известный снимок мониторов (core-тип, см. §0): «эталон» для
/// диффа incoming-событий и вход для тика (§1). Обновляется в ветке
/// MonitorsChanged; при старте заполняется из первого enumerate().
let mut last_snapshot: Vec<MonitorSnapshot> = Vec::new();
```

Диффинг — **всегда против текущего состояния**, а не против «копии старого
снимка»: WM_DISPLAYCHANGE — широковещательное сообщение, его получает
wndproc **каждого** живого окна, т.е. на одну смену топологии в канал
приходит N одинаковых `MonitorsChanged` (N = число окон). Идемпотентный дифф
по device interface path делает дубликаты естественно безопасными: второе
событие не находит разницы и не делает ничего. Любая попытка хранить
«старый список» отдельно от текущего состояния породила бы двойную бухгалтерию
и гонку с самим собой на дубликатах.

### Конвертер

```rust
fn core_snapshot(info: &rst_win32::monitors::MonitorInfo) -> MonitorSnapshot {
    MonitorSnapshot {
        id: info.id.clone(),
        bounds_px: info.bounds_px,
        is_primary: info.is_primary,
    }
}
```

### Шаги ветки (по порядку)

Последовательность важна для хоткея: сначала снять регистрацию со старого
владельца, потом регистрировать нового — иначе `RegisterHotKey` нового окна
упрётся в ещё занятую комбинацию и уйдёт в `HotkeyConflict` (а не «переедет»).

```rust
OverlayMessage::Event(_, OverlayEvent::MonitorsChanged(new_infos)) => {
    let new_snapshot: Vec<MonitorSnapshot> =
        new_infos.iter().map(core_snapshot).collect();
    let new_primary_id = new_infos.iter()
        .find(|m| m.is_primary).unwrap_or(&new_infos[0]).id.clone();

    // 1. Смена владельца глобального хоткея (см. «Хоткей» ниже) — ПЕРВОЙ:
    //    окно старого primary (если живо) пересоздаётся с (None, None),
    //    регистрация снимается до того, как новое окно попробует свою.
    if new_primary_id != primary_id {
        if let Some(ms) = monitors_map.remove(&primary_id) { /* drop: хоткей снят */ }
        if let Some(info) = new_infos.iter().find(|m| m.id == new_primary_id) {
            if let Some(ms) = create_monitor_state(&device, &tx, info,
                Some(hotkey), toggle_all_hotkey, edit.active) {
                monitors_map.insert(info.id.clone(), ms);
            }
        }
    }

    // 2. Появившиеся мониторы → новое окно + цель + форвардер.
    let new_ids: HashSet<&MonitorId> = new_snapshot.iter().map(|m| &m.id).collect();
    for info in &new_infos {
        if monitors_map.contains_key(&info.id) { continue; }
        let (edit_hotkey, this_toggle_all) = hotkey_params_for(info, &new_primary_id);
        if let Some(ms) = create_monitor_state(&device, &tx, info,
            edit_hotkey, this_toggle_all, edit.active) {
            monitors_map.insert(info.id.clone(), ms);
        }
    }

    // 3. Пропавшие мониторы → снос (см. «Чек-лист сноса» ниже).
    let gone: Vec<MonitorId> = monitors_map.keys()
        .filter(|id| !new_ids.contains(*id)).cloned().collect();
    for id in gone {
        teardown_monitor_state(&mut monitors_map, &id, &mut edit, &new_primary_id);
    }

    // 4. Обновить снимки-копии (из живого monitors_map, как при старте).
    monitor_geometry = monitors_map.iter()
        .map(|(id, ms)| (id.clone(), (ms.width, ms.height, ms.scale))).collect();
    monitor_bounds = rebuild_bounds(&new_infos, &monitors_map);

    // 5. Новый «эталон» и прогон автомата.
    last_snapshot = new_snapshot;
    let actions = loss_tracker.on_monitor_snapshot(&last_snapshot, &cfg.stickers, Instant::now());
    if !actions.is_empty() {
        apply_loss_actions(&mut cfg, &mut sprites, actions);
        if let Err(e) = config::save(&cfg, &config_path) { ... }
        need_redraw = true;
    } else {
        need_redraw = true; // даже без действий: состав окон изменился
    }
    // 6. primary_id (локальная переменная run()) = new_primary_id: она нужна
    //    AddSticker (overlay_manager.rs:761) и fallback'ам.
}
```

**Стартовый прогон трекера обязателен**: сразу после первого `enumerate()`,
до цикла сообщений — `loss_tracker.on_monitor_snapshot(&last_snapshot,
&cfg.stickers, Instant::now())` (действия ожидаемо пусты — это лишь
инициализация `prev_snapshot`). Без него первый `MonitorsChanged` увидел бы
«потери» относительно **пустого** prev и спутал бы горячее подключение с
пропажей мониторов, существовавших на старте.

### Чек-лист сноса (`teardown_monitor_state`)

1. `monitors_map.remove(&id)` — `MonitorState` дропается в порядке полей:
   `target` (DComp-цепочка на HWND) раньше `overlay` (`WM_CLOSE` → join
   pump-потока). Это уже задокументированный безопасный порядок
   (docs/M3_STEP4_REVIEW.md, пункт 2.3) — простое удаление из HashMap его
   соблюдает.
2. Поток-форвардер окна завершается сам: канал событий окна закрывается при
   выходе pump-потока, цикл `for event in events` (overlay_manager.rs:643–652)
   заканчивается. Ничего ждать/соединять не нужно.
3. `monitor_geometry.remove(&id)`, `monitor_bounds.remove(&id)` — без этого
   панели и rebind продолжат видеть мёртвый монитор.
4. **Edit-state**: если `edit.active`:
   - выделение: `edit.selection` prune до стикеров, чей монитор ещё жив
     (у скрытых трекером стикеров монитор уже «мёртв» — их из выделения
     убрать, иначе тулбар/рамка повиснут на несуществующем окне);
   - если `edit.cursor_monitor == id` → `edit.cursor_monitor = new_primary_id`,
     `edit.cursor_pos = (0.0, 0.0)` (или центр нового монитора);
   - незавершённый жест стикера пропавшего монитора — отменить как
     `CaptureLost` (overlay_manager.rs:2639–2672: `apply_transform` к
     стартовому снимку, `pending_snapshot = None`) — окно уничтожено,
     `WM_CAPTURECHANGED` уже не придёт;
   - `rebuild_ui_panels(&mut edit, &cfg, &monitor_geometry)`.
5. Стикеры пропавшего монитора **не трогать руками** — их скрывает трекер
   (`LossAction::HideSticker`). Снос окна и скрытие стикеров — два разных
   эффекта одного события; координатор отвечает только за первый.

Новое окно, созданное **во время активного режима редактирования**, должно
сразу стать интерактивным: `ms.overlay.set_interactive(true)` — иначе клики
на новом мониторе проваливаются сквозь режим (тот же инвариант, что
docs/M3_PREP_NOTES.md §3.5). Отсюда параметр `edit_active` у
`create_monitor_state`.

### Хоткей при смене primary

Инвариант: глобальные хоткеи (edit_mode и toggle_all) регистрирует ровно
одно окно — монитора, который сейчас primary. `RegisteredHotkey` живёт на
pump-потоке своего окна и **снимается сам** при сносе окна (`Drop` →
`UnregisterHotKey` тем же потоком — тип `!Send`, hotkey.rs). Поэтому
перерегистрация = пересоздание окна:

- старый primary (если его окно ещё живо — смена primary без пропажи):
  пересоздать с `(None, None)` — окно остаётся, хоткей снимается;
- новый primary: пересоздать с `(Some(edit_hotkey), toggle_all_hotkey)`.

Специальной «перенести регистрацию» механики не существует и не нужно:
создание с `Some` регистрирует, с `None` — нет. Пересоздание окна —
тот же `teardown_monitor_state` + `create_monitor_state`, что и для
горячего подключения. Событие о том, что primary сменился, — тот же
`MonitorsChanged` (в снапшоте `is_primary` переехал; предположение:
смена primary в Windows всегда сопровождается `WM_DISPLAYCHANGE` — смена
раскладки виртуального десктопа; если на практике найдутся исключения —
перечислить мониторы и на `SessionUnlocked`/`SystemResumed`, см. §7).

### `create_monitor_state` — вынос общего блока

Блок «окно + цель + форвардер» из стартового цикла (overlay_manager.rs:608–663)
выносится в функцию — она же используется и стартом, и hot-plug:

```rust
/// Окно + WindowTarget + поток-форвардер событий для одного монитора.
/// Возвращает None (с warn-логом) при неудаче — так же, как стартовый цикл.
/// `edit_active` — новое окно в режиме редактирования сразу интерактивно.
#[allow(clippy::too_many_arguments)]
fn create_monitor_state(
    device: &Device,
    tx: &Sender<OverlayMessage>,
    info: &rst_win32::monitors::MonitorInfo,
    edit_hotkey: Option<HotkeyCombo>,
    toggle_all_hotkey: Option<HotkeyCombo>,
    edit_active: bool,
) -> Option<MonitorState> { ... }
```

---

## 3. Применение `LossAction` — системные мутации без undo

Поля вариантов — из `monitor_loss.rs` (см. там же доки). Диспетчер:

```rust
/// Применить системные действия автомата потери монитора. НЕ в undo-историю:
/// снимки Config до действий кладут только пользовательские пути (жест на
/// MouseUp, тулбар, клавиатура); системная миграция в истории оживила бы
/// Ctrl+Z'ом стикеры на физически отсутствующем мониторе
/// (docs/M3_PREP_NOTES.md, §5.3 — «иначе Ctrl+Z воскрешал бы»).
fn apply_loss_actions(cfg: &mut Config, sprites: &mut Vec<(Uuid, Sprite)>, actions: Vec<LossAction>) {
    for action in actions {
        match action {
            LossAction::HideSticker { sticker_id } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.visible = false;
                }
                // Спрайт не трогаем: он рисуется по placement, видимость
                // читается из cfg при redraw (шахматка в режиме, ничего вне).
            }
            LossAction::RestoreVisibility { sticker_id, visible } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.visible = visible;
                }
            }
            LossAction::Migrate { sticker_id, placement, origin, visible } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.placement = placement.clone();
                    s.origin = Some(origin);
                    s.visible = visible;
                }
                // Спрайт несёт копию placement — обновить синхронно, иначе
                // жест/хит-тест продолжат видеть старую геометрию.
                if let Some((_, sprite)) = sprites.iter_mut().find(|(id, _)| *id == sticker_id) {
                    sprite.placement = placement;
                }
            }
            LossAction::ReturnHome { sticker_id, placement, rotation } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.placement = placement.clone();
                    s.transform.rotation = rotation;
                    s.origin = None; // дом вернулся — «страховка» съедена
                }
                if let Some((_, sprite)) = sprites.iter_mut().find(|(id, _)| *id == sticker_id) {
                    sprite.placement = placement;
                    sprite.transform.rotation = rotation;
                }
            }
            LossAction::ClearOrigin { sticker_id } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.origin = None;
                }
            }
        }
    }
}
```

Гарантия «не в undo» достигается структурно: `apply_loss_actions` вызывается
ровно из двух мест — ветки `MonitorsChanged` и ветки `Tick`, и ни одно из них
не касается `edit.undo_stack`/`redo_stack`/`pending_snapshot`. `commit_undo_snapshot`
вызывается только пользовательскими путями (MouseUp-коммит жеста,
`handle_toolbar_up`, `handle_key`) — пересечения нет. Системные мутации при
этом **сохраняются** `config::save` (переживают перезапуск; undo-история — нет,
и это правильно: после рестарта монитор, мигрировавший 10 минут назад, не
должен «откатываться»).

---

## 4. `on_user_edit` — точки вызова

Требование (SPEC 6.1 п.4): стикер, тронутый пользователем после миграции, не
возвращается домой; «основной сигнал» — очистка origin правкой placement
(docs/M3_PREP_NOTES.md §5.3). `on_user_edit(&Sticker)` у трекера выдаёт
`ClearOrigin` и запоминает id в страховочном множестве.

Точки вызова в текущем коде:

1. **MouseUp-коммит жеста** (overlay_manager.rs:2590–2608) — одна точка на
   все три жеста (Drag/Resize/Rotate) и на cross-monitor rebind-блок
   (:2531–2589). Жест забирается из `edit.gesture` ДО коммита снимка:

   ```rust
   if let Some(gesture) = edit.gesture.take() {
       if let Some(before) = edit.pending_snapshot.take() {
           if before != *cfg {
               // Пользователь изменил геометрию стикера — origin (если был
               // от миграции) подлежит очистке: автовозврат не должен ни
               // отменить правку, ни вернуть стикер домой (SPEC 6.1 п.4).
               if let Some(start) = gesture.start() {
                   if let Some(s) = cfg.stickers.iter().find(|s| s.id == start.id) {
                       let actions = loss_tracker.on_user_edit(s);
                       apply_loss_actions(&mut cfg, &mut sprites, actions);
                   }
               }
               commit_undo_snapshot(edit, before);
           }
       }
       ...
   }
   ```

   Rotate попадает сюда намеренно: автовозврат восстанавливает
   `origin.rotation` (monitor_loss::LossAction::ReturnHome несёт rotation), и
   без очистки origin вернул бы пользователю перезаписанный поворот. Opacity
   origin не содержит — её правки origin не требуют, но и не мешают: тулбар
   её сегодня меняет через `apply_opacity` (transform), call site не нужен.

2. **Rebind-блок** (overlay_manager.rs:2572–2575) отдельной точки **не
   требует**: он мутирует тот же стикер того же жеста и попадает в п.1 по
   факту изменения cfg.

3. **Тулбар** (`handle_toolbar_up`, :1875–1985): сегодня ни одно действие не
   мутирует placement (opacity — transform, eye — visible, order — order,
   duplicate — новый стикер, delete — удаление) — точек нет. Правило для
   будущего: любое действие, меняющее placement (числовые поля позиции и
   т.п.), обязано звать `on_user_edit` там же, где коммитится снимок
   (`commit_undo_snapshot`).

4. **Клавиатурные мутации placement** — в коде отсутствуют (Ctrl+D — новый
   стикер, Delete — удаление), точек нет.

---

## 5. Взаимодействие с rebind (0027b0f) и recover_device (402eb0b)

- **`recover_device` (402eb0b) — независимый путь, переиспользовать НЕ**
  нужно: он пересоздаёт D3D-устройство и цели **текущего** `monitors_map`
  (overlay_manager.rs:3026–3035). Hot-plug создаёт **новое окно + цель на
  существующем устройстве** — общий кусок с ним только конструкция
  per-monitor состояния, которая выносится в `create_monitor_state` (§2).
  Потеря устройства посреди hot-plug обрабатывается существующей цепочкой
  `need_redraw → redraw_all → recover_device` — она итерирует уже
  пост-hot-plug карту, изменений не требует.
- **`rebind_monitor_by_center` (0027b0f)** работает по `monitor_bounds`:
  hot-plug обязан пересобирать этот снимок (§2, шаг 2), иначе rebind
  предложит таргетом мёртвый монитор (или не увидит новый). Пересечение
  «стикер тащится в момент пропажи монитора» — доброкачественное: если
  MonitorsChanged обработается первым, стикер уже скрыт трекером, а
  MouseUp-коммит всё равно отработает по текущему cfg (скрытый стикер
  редактировать можно — шахматка интерактивна); если MouseUp первым — rebind
  просто не найдёт источник в свежем `monitor_bounds` и не сработает, а
  трекер догонит скрытием. Специальной синхронизации не требуется.
- **Оба пути не конфликтуют с автоматом**: трекер живёт в своей
  `HashMap<MonitorId, LossState>` и знает только снимки + стикеры; его
  действия применяются диспетчером (§3) к тем же `cfg`/`sprites`, которыми
  пользуются жесты — но никогда не в одном кадре с ними (разные ветки
  сообщений).

---

## 6. Порядок работ (для координатора)

1. **Тик-скелет**: `OverlayMessage::Tick` + поток-будильник + пустая ветка
   (трассировка), константа `LOSS_TICK_PERIOD`. Проверка: тик приходит ~1 раз
   в секунду, цикл не тормозит.
2. **Снимки и трекер**: конвертер `core_snapshot`, поля `last_snapshot` и
   `loss_tracker` в `run()`, стартовый прогон трекера (обязателен — см. §2).
3. **Диспетчер `apply_loss_actions`** (§3) + вызов из ветки `Tick` + save.
4. **`create_monitor_state`** — вынос из стартового цикла (поведение старта
   не меняется, тест — прежний ручной прогон на одном мониторе).
5. **Ветка `MonitorsChanged`**: хоткей-пересоздание (первым — §2 «Шаги
   ветки») → создание окон появившихся мониторов → снос пропавших +
   `teardown_monitor_state` (чек-лист §2, включая edit-state) → пересборка
   `monitor_geometry`/`monitor_bounds` → `last_snapshot` → прогон трекера →
   `apply_loss_actions` → redraw. Дубликаты `MonitorsChanged` (N окон) —
   идемпотентностью диффа, см. §2.
6. **Смена primary**: пересоздание окон-владельцев хоткея (§2 «Хоткей»),
   обновление `primary_id`, `edit.cursor_monitor`.
7. **`on_user_edit`** в MouseUp-коммите (§4).
8. **QA-сценарии** (ручные): unplug/replug монитора <20 с (возврат «как
   было», включая видимость), unplug → 20+ с → миграция на основной →
   replug → автовозврат, hot-plug во время активного режима редактирования
   (новое окно интерактивно, выделение почищено), смена primary в настройках
   Windows (хоткей переехал), двойной unplug двух мониторов сразу (две
   потери, одна миграция на основной), повторные MonitorsChanged (дубликаты
   не плодят окна).

---

## 7. Открытые вопросы и известные допущения

- **Стартовое состояние при уже отключённом мониторе**: трекер видит только
  переходы — монитор, отсутствующий на первом снимке, «потерянным» не
  считается (тест `first_snapshot_does_not_report_losses`). Поведение:
  стикеры на нём остаются visible и не рисуются (окна нет), при возврате
  монитора просто появляются; миграции/таймера нет. Данные не теряются
  (SPEC-инвариант соблюдён); для полной симметрии можно добавить в трекер
  seed-метод (`seed_from_records(&[MonitorRecord])`, берёт `last_bounds` из
  Config.monitors) — вне этого среза, отдельная задача.
- **Смена primary без WM_DISPLAYCHANGE**: допущение, что Windows всегда
  шлёт WM_DISPLAYCHANGE при смене раскладки виртуального десктопа. Если
  проверка на реальном железе покажет иное — добавить переперечисление на
  `SessionUnlocked`/`SystemResumed` (ветки уже есть, overlay_manager.rs:895–906).
- **`edit.cursor_pos` при смене монитора курсора**: текущее значение — DIP
  в системе координат старого `cursor_monitor`; при сносе сбрасывается в
  (0,0). Приемлемо (следующий MouseMove пересоберёт позицию), но не идеально
  — можно пересчитывать через `monitor_bounds`, если всплывёт визуальный
  скачок панели у курсора.
