# M2 — план интеграции режима редактирования в оверлей-поток

Статус: **план для координатора** (планирование, код не менялся). Опирается на
M1-состояние (`crates/resticker/src/overlay_manager.rs`, `main.rs`) и M2-примитивы
(`rst-core`: `hittest`, `undo`, `snap`, `ops`, `selection_set`; `rst-render`:
`selection`; `rst-win32`: `hotkey`, `input`, `clipboard`).

Пробелы, найденные при ревью, перечислены в `docs/M2_INTEGRATION_REVIEW.md`.
Здесь — пошаговый план их сшивки: что за состояние, как устроен цикл событий,
какая функция из какого модуля вызывается в каком месте.

---

## 1. Схема потоков: сегодня и цель

Сегодня в процессе **два** потока оверлея:

```
Tauri-поток (main)
   └─ overlay_manager::start() ── spawn → поток «координатор» (run())
                                              ├─ OverlayWindow::create() ── spawn → поток «pump» (overlay.rs run_message_loop + wndproc)
                                              ├─ Renderer, cfg, sprites, config_path
                                              └─ for cmd in rx { … }   // канал из OverlayHandle
```

- **Координатор** (`run()` в `overlay_manager.rs:52`) — владелец состояния
  (ADR-013): `cfg`, `sprites`, `renderer`. Принимает команды из `rx`.
- **Pump** (`overlay.rs:103`) — свой `GetMessage`-цикл и `wndproc` (сейчас
  обрабатывает только `WM_DESTROY`, `overlay.rs:207`).

Весь ввод M2 живёт **на pump-потоке**: `MouseCapture` и `CursorManager`
содержат `HWND`/`HCURSOR` и `!Send`; `RegisteredHotkey` `!Send` и обязан
регистрироваться на потоке с циклом сообщений (`hotkey.rs:163`). Значит, сырые
сообщения окна переводит в безопасные события pump, а обрабатывает их
координатор.

**Целевая схема — один канал, два отправителя:**

```
Tauri:     OverlayHandle::send(Command::…)      ─┐
                                                  ├→ rx (один mpsc) → цикл координатора
pump:      wndproc → InputEvent / Hotkey / Key  ─┘

pump ← координатор (только курсор): PostMessageW(hwnd, WM_APP_EDIT_CURSOR, shape, 0)
```

Рекомендация: вместо двух каналов — один объединённый `enum OverlayMessage`
(без новых зависимостей, std-канал):

```rust
enum OverlayMessage {
    Command(OverlayCommand),        // от Tauri: AddSticker, Shutdown (существующие)
    Input(InputEvent),              // от pump: мышь (input.rs)
    Hotkey(i32),                    // от pump: WM_HOTKEY → message_hotkey_id
    Key { vk: u32, modifiers: Modifiers, pressed: bool }, // новая склейка, см. §12
}
```

`OverlayHandle` продолжает слать `OverlayCommand` (оборачивается в
`Command`), а `OverlayWindow::create` получает `Sender<OverlayMessage>`, чтобы
pump отправлял в тот же канал. Цикл координатора — `for msg in rx { … }`.

---

## 2. Новое состояние координатора (run())

Добавляется в `run()` рядом с `cfg`/`sprites`/`renderer`:

```rust
struct EditState {
    active: bool,                       // в режиме редактирования?
    selection: SelectionSet,            // rst_core::selection_set::SelectionSet
    undo: UndoStack,                    // rst_core::undo::UndoStack::default() (100)
    gesture: Option<Gesture>,           // текущий жест (см. ниже)
    snap: SnapConfig,                   // rst_core::snap::SnapConfig (из настроек M4; пока default)
    scale: f32,                         // DIP→px; дубликат Renderer::scale (§5, REVIEW §2)
    edit_hotkey_id: i32,                // id RegisteredHotkey, чтобы фильтровать Hotkey(i32)
    fill_tex: FillTextures,             // белая 1×1, чёрная, шахматка (§11)
    pending_cursor: CursorShape,        // последняя зона → форма (для pump, §7)
}
```

Жест — enum, создаётся в `MouseDown`, завершается/отменяется в `MouseUp`/`CaptureLost`:

```rust
enum Gesture {
    Drag { id: Uuid, grab_dx: f64, grab_dy: f64 },          // позиция взятия (DIP)
    Resize { id: Uuid, handle: Handle, start: Placement, start_aspect: f64 },
    Rotate { id: Uuid, start_angle: f64, start_rotation: f64 },
    Marquee { anchor: (f64, f64) },                          // протяжка по фону (SPEC 3.2)
}
```

`Handle` сейчас живёт в `rst_win32::input::Handle`; по REVIEW §1 его стоит
вынести в `rst-core` (единый тип для `selection`/`input`) — до этого
координатор работает с `input::Handle` и `selection::HandleKind` через явный
маппинг (§7).

`sprites` меняется с `Vec<Sprite>` на id-ключевой `HashMap<Uuid, Sprite>`:
порядок отрисовки берётся из `cfg.stickers` (отсортированы по `order`), а
текстура — по id. Так delete/duplicate/visibility не ломают выравнивание
индексов и не перезагружают текстуры.

`fill_tex` — три текстуры через `Renderer::create_texture_from_rgba`:
1×1 белая (рамка/ручки), 1×1 чёрная (затемнение), шахматка из
`selection::checkerboard_tile(16, 64)` (скрытые стикеры, SPEC 3.7).

---

## 3. Вход в режим редактирования (hotkey toggle)

Хоткей настраивается строкой `cfg.hotkeys.edit_mode` (`CONFIG.md`), по
умолчанию `"Ctrl+Alt+S"`.

1. **Старт (координатор, чистая функция):** `HotkeyCombo::parse(&s)` из
   `rst_win32::hotkey` → зарегистрировать на pump-потоке
   `RegisteredHotkey::register(id, combo)` (id из диапазона `0x0000..=0xBFFF`).
2. **Pump:** в цикле `GetMessageW` сообщения `WM_HOTKEY` приходят с
   `hwnd=NULL` и не доходят до `wndproc` (диспетчеризация с NULL-окном — no-op;
   REVIEW §4). Цикл перед `DispatchMessageW` проверяет
   `msg.hwnd.is_null() && msg.message == WM_HOTKEY`, берёт
   `hotkey::message_hotkey_id(msg.wParam)` и шлёт `OverlayMessage::Hotkey(id)`
   координатору.
3. **Координатор:** `Hotkey(id)` при `id == edit_hotkey_id` → `toggle_edit_mode()`.

`toggle_edit_mode()`:

- **вход:** снять `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`, `SetForegroundWindow`
  (ARCHITECTURE 5.2). Это новая склейка в `overlay.rs` (`set_click_through(bool)`
  через `SetWindowLongPtrW(hwnd, GWL_EXSTYLE, …)`). Затем `active = true`,
  `selection.clear()`, `undo` **не** чистится (SPEC 3.5 — история живёт в рамках
  сессии), красро: затемнение + шахматка для скрытых. Опционально: проверка
  эксклюзивного полноэкранного окна и предупреждение (SPEC 3.1) — отдельный шаг.
- **выход:** отменить незавершённый жест (без push), `selection.clear()`,
  вернуть `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`, `active = false`,
  `config::save(&cfg, &config_path)` (изменения сохраняются, SPEC 3.1), redraw
  без UI.

`Shutdown` и `AddSticker` обрабатываются как раньше; `AddSticker` в режиме
редактирования дополнительно: `undo.push(AddCommand)`, выделить новый стикер,
redraw, `config::save`.

---

## 4. Маршрутизация мыши (мост pump → координатор)

Pump держит `MouseCapture` (`input::MouseCapture::new(hwnd)`) и `CursorManager`.
`wndproc` (в `overlay.rs`) расширяется:

- `WM_LBUTTONDOWN/UP`, `WM_MOUSEMOVE`, `WM_CAPTURECHANGED` →
  `capture.handle_message(msg, wparam, lparam)` → `Some(InputEvent)` →
  `OverlayMessage::Input(event)`. `None` — отдать в `DefWindowProcW`.
  Захват/отпускание `SetCapture`/`ReleaseCapture` делает сам `MouseCapture`
  (`input.rs:167,170`), pump ничего не знает про жест.
- `WM_SETCURSOR` → `CursorManager::handle_set_cursor(lparam)` (вернёт
  `Some(LRESULT(1))` для клиентской зоны или `None` → `DefWindowProcW`).
- Кастомное `WM_APP_EDIT_CURSOR` (координатор → pump, §7) →
  `CursorManager::set_shape(shape)`.

Координатор получает `InputEvent`:
- вне режима редактирования мышь игнорируется (кроме хоткея);
- в режиме — обрабатывается в цикле (§6).

Перевод координат: `input::Point` — **физические** пиксели клиента окна
(оверлей на (0,0) во весь основной монитор), а `hittest`/`snap`/`selection` —
**DIP** (ADR-010). Перед хит-тестом: `dip = phys / scale` (REVIEW §2).

---

## 5. Масштаб DIP→px (блокер, по REVIEW §2)

`Renderer::scale` приватна, геттера нет, `set_dpi_scale` нигде не вызывается.
Для M2 обязательно:

- при старте: `renderer.set_dpi_scale(dpi / 96.0)` (dpi — из `GetDpiForWindow`
  или `GetDpiForSystem`, новая склейка);
- хранить ту же `scale` в `EditState` (или добавить `Renderer::dpi_scale()` —
  на выбор координатора) для конверсии ввода.

Без этого: стикеры на не-100% DPI уже в M1 рендерятся неверно, а хит-тест в
M2 физически не считаем.

---

## 6. Цикл редактирования: поток событий

Цикл координатора по одному сообщению из `rx`:

```
match msg {
    Command(AddSticker(path))      → add_sticker (M1) + undo.Add + select + save + redraw
    Command(Shutdown)              → выход
    Hotkey(id)                     → если id == edit_hotkey_id: toggle_edit_mode()
    Input(e) if edit_state.active  → handle_input(e)
    Key{..} if edit_state.active   → handle_key(..)          // §12
    _                              → игнор (вне режима ввод не принимаем)
}
```

`handle_input` (координаты уже в DIP):

- `MouseDown { pos, modifiers }` → **разрешение зоны** (§7):
  - `RotateZone(corner)` → `gesture = Rotate{..}`, курсор `Rotate`
  - `ResizeHandle(h)` → `gesture = Resize{..}`, курсор по зоне
  - `StickerBody` → выделение (§9.1), `gesture = Drag{..}`, курсор `Move`
  - `Background` → `selection.click(None)`; при удержании мыши начать
    `gesture = Marquee{anchor}` (SPEC 3.2 протяжка); курсор `Arrow`
  - redraw (изменилось выделение)
- `MouseMove { pos, modifiers, dragging }`:
  - `gesture` активен → применить жест (§8); redraw
  - нет жеста (hover) → пересчитать зону, `pending_cursor = zone.cursor_shape()`,
    отправить shape pump-у (§7); **без redraw** (ADR-006)
- `MouseUp { .. }` → завершить жест: `undo.push(...)` (§10), `config::save`,
  `gesture = None`, redraw
- `CaptureLost` → отменить жест **без** push (`gesture = None`), redraw

---

## 7. Разрешение зоны под курсором

Порядок проверки — обратный порядку отрисовки (сверху вниз, ARCHITECTURE 5.3).
Выполняется над `Placement`/`Transform` каждого стикера (все в DIP).

1. **Поворот** (кольцо 6–24 px наружу от угловой ручки, SPEC 3.3;
   `input.rs:205`): для каждой угловой ручки
   `SelectionBox::handle_center(corner_handle)` (через
   `rst_render::selection::SelectionBox::new(&placement, &transform)`), если
   `!contains_inflated(pl, tr, px, py, 6.0)` и дистанция до угла `<= 24.0` →
   `RotateZone(corner)`. Смаппить `selection::HandleKind` → `input::Corner`
   (REVIEW §1: три типа ручек; пока — явный маппинг по именам).
2. **Ручки ресайза:** `SelectionBox::handle_center(kind)` для 8 ручек, попадание
   `|p − c| <= HANDLE_SIZE_DIP/2 + 1` → `ResizeHandle(handle)`
   (`selection::HANDLE_SIZE_DIP` = 10).
3. **Тело стикера:** `hittest::contains(&pl, &tr, px, py)` →
   `StickerBody`.
4. Иначе — `Background`.

Зона → курсор: `input::CursorZone` строится вручную
(`Background`/`StickerBody`/`ResizeHandle(h)`/`RotateZone(c)`) и
`zone.cursor_shape()` даёт `CursorShape` (`input.rs:247`). Отправка формы pump-у:
`PostMessageW(hwnd, WM_APP_EDIT_CURSOR, WPARAM(shape as usize), 0)`; wndproc →
`CursorManager::set_shape` (идемпотентна, `input.rs:331`). `CursorManager` живёт
на pump, `pending_cursor` на координаторе — источник истины.

При **мультивыделении** зоны тела/ручек/поворота считаются по
`selection::SelectionBox::new` от общего bbox `SelectionSet::bounds(&stickers)`
(для ручек и поворота) и по телу того стикера, который под курсором
(первый в порядке отрисовки сверху).

---

## 8. Жесты: drag / resize / rotate

Все вычисления — в DIP; `Placement`/`Transform` правятся в `cfg.stickers[id]` и
в `sprites[id]` одновременно.

**Drag** (`hittest::to_local`/`aabb` + `snap::snap_placement` + `snap::clamp_min_visible`):
```
new_cx = mouse_dip.x - grab_dx;  new_cy = mouse_dip.y - grab_dy;
r = snap::snap_placement(&placement, rotation, monitor_dip, &snap_cfg, modifiers.ctrl);
placement.cx = new_cx + r.dx;  placement.cy = new_cy + r.dy;
placement = snap::clamp_min_visible(&placement, rotation, monitor_dip);
```
- `monitor_dip = DipRect::new(0,0, screen_w/scale, screen_h/scale)`
  (`overlay.size()` px / `scale`).
- `modifiers.ctrl` — отключить магнит (SPEC 3.4, `snap_placement` уже принимает
  флаг). `SnapResult::vline/hline` — координаты направляющих; по ним рисуются
  тонкие квады-подсказки (§11) при `r.is_snapped()`.
- Мультивыделение: дельта мыши (текущая минус предыдущая позиция жеста)
  прибавляется к `cx/cy` всех выделенных; магнит — только к первичному
  (по которому начали жест).

**Resize** (локальные координаты, ось повёрнута вместе со стикером):
```
(lx, ly) = hittest::to_local(pl.cx, pl.cy, transform.rotation, px, py);
// «противоположная грань» фиксируется, грань ручки следует за мышью:
//  East: w = 2*lx;  West: w = -2*lx, центр сдвигается на (w - old_w)/2 по локальной оси
//  South: h = 2*ly;  North: h = -2*ly …
if modifiers.shift { пропорции: w/h = start_aspect, берём доминирующую ось }
if modifiers.alt  { масштаб от центра: центр не двигается }
w,h = w.max(16.0), h.max(16.0);                 // SPEC 3.3, минимальный размер
// обратно в мировые: cx/cy корректируются на половину дельты для North/West ручек
placement = snap::clamp_min_visible(&placement, rotation, monitor_dip);
```
Отрицательный размер (протащили ручку через противоположную грань) — зеркалирование
(SPEC 3.3 «отрицательное масштабирование СЛЕДУЕТ поддерживать»): центр и размер
пересчитываются так, чтобы `w,h > 0`.

**Rotate:**
```
angle = (px - pl.cx).atan2(py - pl.cy);               // по конвенции шейдера
delta = angle - start_angle;
transform.rotation = start_rotation + delta;
if modifiers.shift { rotation = (rotation / 15°).round() * 15°; }   // SPEC 3.3
```

Каждый `MouseMove` при активном жесте → `renderer.draw(...)` (§11).

---

## 9. Выделение (rst_core::selection_set)

Вся логика выделения — чистые функции `SelectionSet`:

- клик по стикеру без модификаторов → `selection.click(Some(id))`;
- `Shift`+клик → `selection.shift_click(id)`;
- клик по фону → `selection.click(None)`;
- `Ctrl+A` → `selection.select_all(&cfg.stickers)`;
- протяжка по фону (Marquee) → на каждый `MouseMove`:
  `selection.rubber_band(&cfg.stickers, &DipRect::new(anchor.x, anchor.y, cur.x-anchor.x, cur.y-anchor.y))`;
- после `ops::delete` → `selection.prune(&cfg.stickers)`.

Общий bbox мультивыделения для рамки/тулбара —
`selection.bounds(&cfg.stickers) -> Option<DipRect>`.

---

## 10. Undo / Redo (rst_core::undo)

`UndoStack` — трайт `Command` + стек (`undo.rs:23,39`). `Box<dyn Command>` не
может держать `&mut Config` (REVIEW §3) — для M2 достаточно **снапшот-команды**:
`ConfigSnapshot { before: Config, after: Config }`, где `execute()` кладёт
`after`, `undo()` — `before`, `name()` — «Перемещение»/«Ресайз»/«Поворот»/….
На 25 стикеров клонирование дёшево; точечные команды — позже при необходимости.

Моменты push (каждый = **один** шаг истории, SPEC 3.5 — не по кадру):
- отпускание после Drag / Resize / Rotate (before/after по затронутым стикерам);
- `AddSticker` (добавление);
- `ops::delete`, `ops::duplicate`, `ops::toggle_visibility`,
  `ops::step_up`/`step_down` (тулбар, §14);
- изменение opacity (тулбар).

Обработка команд:
- `Ctrl+Z` → `undo.undo()` → `sync_sprites_and_save()` (пересборка/правка
  `sprites` по `cfg`, `config::save`, redraw);
- `Ctrl+Shift+Z`/`Ctrl+Y` → `undo.redo()` → то же;
- `undo.clear()` — на выходе из режима не нужен (история сессионная), но понадобится
  при загрузке пресета (M7).

`undo.can_undo/can_redo/undo_name/redo_name` — для пунктов меню/тулбара.

---

## 11. Отрисовка кадра редактора

`Renderer::draw(&[Sprite])` — **только по событию** (ADR-006). Список спрайтов
собирается каждый раз заново (дёшево) в порядке `cfg.stickers` (по `order`,
больше — выше), для `StickerSource::File` ищем текстуру в `sprites[id]`:

```
снизу вверх:
1. затемнение:  solid_sprite(&black_tex, monitor_id, &selection::edit_overlay(w_dip, h_dip),
                selection::EDIT_OVERLAY_OPACITY)
2. стикеры:     видимый — своя текстура; скрытый (edit) — шахматка
                (fill_tex.checker); вне режима скрытый не рисуется вовсе (SPEC 3.7)
3. рамка/ручки: SelectionBox::new(&pl,&tr).visuals() → solid_sprite(&white_tex, …, opacity 1)
   (мультивыделение: SelectionBox от selection.bounds(..))
4. направляющие магнита: тонкие квады из SnapResult.vline/hline (координата ±0.5px)
   — только при активном жесте и r.is_snapped()
5. (M2-фаза 2) тулбар, панель у курсора — см. docs/M2_UI_NOTES.md (вариант Б)
```

Все Box2D → Sprite через `selection::solid_sprite` (координаты DIP, рендерер
умножит на `scale`). Шахматка: `checkerboard_tile(16, 64)` →
`renderer.create_texture_from_rgba` один раз; `CHECKERBOARD_HLSL` — точка
расширения (стабильный узор при ресайзе), сейчас не задействован.

---

## 12. Клавиатура и Ctrl+V

Готового примитива клавиатуры нет (input.rs — только мышь). Новая склейка:
wndproc переводит `WM_KEYDOWN`/`WM_CHAR` оверлей-окна (в режиме окно в фокусе,
SPEC 3.1) в `OverlayMessage::Key { vk, modifiers, pressed }` и шлёт координатору.
`Modifiers` уже есть в `input::Modifiers`.

Обработка на координаторе:
- `VK_ESCAPE` → выход из режима (`toggle_edit_mode()`);
- `Ctrl+Z` → undo; `Ctrl+Shift+Z`/`Ctrl+Y` → redo (§10);
- `Ctrl+A` → `selection.select_all`; `Delete` → удалить выделенные
  (`ops::should_confirm_delete` → диалог подтверждения (SPEC «Больше не
  спрашивать»; M2-фаза 2), `ops::delete`, `undo.push(Delete)`, `selection.prune`);
- `Ctrl+D` → `ops::duplicate` (сдвиг `ops::DUPLICATE_OFFSET`) + undo + select копии;
- `Ctrl+V` → вставка из буфера:
  1. `clipboard::read_image()` (координатор; `ClipboardImage` без Win32-типов);
  2. `Png(bytes)`/`Bmp(bytes)` → материализовать в
     `%APPDATA%\resticker\pasted\<uuid>.png` (новый helper; декодирование/даунскейл
     — зона `rst-media`), `StickerSource::Pasted`, затем как `AddSticker`;
  3. `Files(paths)` → для каждого `clipboard::is_supported_image(path)` →
     `StickerSource::File` → как `AddSticker`.

---

## 13. Персистентность

`config::save(&cfg, &config_path)` вызывается:
- после каждого завершённого жеста (MouseUp);
- после каждой тулбар-операции и undo/redo;
- при выходе из режима редактирования (SPEC 3.1).

Порядок стикеров нормализуется самим `config::save` (`config.rs:128`), отдельного
вызова не нужно.

---

## 14. Тулбар и панель у курсора (M2-фаза 2)

Логика нажатий — чистые `ops::*` над `cfg` + undo-обёртки (§10) + `config::save` +
redraw:

| Кнопка (SPEC 3.6) | Функция |
|---|---|
| Ползунок/поле opacity | править `sticker.transform.opacity` (1–100 → 0.01–1.0) |
| «Глаз» | `ops::toggle_visibility(&mut cfg, id)` |
| «Выше»/«Ниже» | `ops::step_up` / `ops::step_down` |
| «Дублировать» (`Ctrl+D`) | `ops::duplicate` |
| «Удалить» (`Delete`) | `ops::delete` (+ `should_confirm_delete`/`suppress_delete_confirmation`) |

Рендер тулбара и хит-тест его кнопок — отдельная подзадача (immediate-mode
виджеты, `docs/M2_UI_NOTES.md` вариант Б; ADR-014 — проект в REVIEW §7).

---

## 15. Таблица: существующая функция → где вызывается

| Функция | Модуль | Вызывается в |
|---|---|---|
| `HotkeyCombo::parse`, `display_string` | `rst_win32::hotkey` | старт координатора (чистая) |
| `RegisteredHotkey::register(id, combo)` | `rst_win32::hotkey` | pump-поток перед GetMessage-циклом |
| `hotkey::message_hotkey_id(wparam)` | `rst_win32::hotkey` | pump, обработка WM_HOTKEY (hwnd=NULL) |
| `MouseCapture::new(hwnd)` / `handle_message` | `rst_win32::input` | pump (wndproc) |
| `CursorManager::{set_shape, handle_set_cursor}` | `rst_win32::input` | pump (wndproc + WM_APP_EDIT_CURSOR) |
| `CursorZone::cursor_shape` | `rst_win32::input` | координатор, зона→форма (§7) |
| `hittest::{contains, contains_inflated, to_local, aabb}` | `rst_core::hittest` | координатор: зона, drag, resize |
| `hittest::DipRect::{new, from_center}` | `rst_core::hittest` | координатор: monitor_dip, marquee, направляющие |
| `snap::{snap_placement, snap_move, clamp_min_visible}` | `rst_core::snap` | координатор: drag/resize (§8) |
| `snap::SnapConfig`, `SnapResult::{dx,dy,is_snapped}` | `rst_core::snap` | координатор |
| `undo::UndoStack::{push, undo, redo, clear}` | `rst_core::undo` | координатор (§10) |
| `undo::Command` (снапшот-команда) | `rst_core::undo` | координатор (новый тип) |
| `selection_set::SelectionSet::{click, shift_click, select_all, rubber_band, bounds, prune, clear}` | `rst_core::selection_set` | координатор (§9) |
| `ops::{duplicate, delete, toggle_visibility, step_up, step_down, should_confirm_delete, suppress_delete_confirmation}` | `rst_core::ops` | координатор: тулбар/клавиши (§12, §14) |
| `selection::SelectionBox::new` / `.visuals()` / `.all_rects()` | `rst_render::selection` | координатор: рамка/ручки/зоны (§7, §11) |
| `selection::edit_overlay(w_dip, h_dip)` | `rst_render::selection` | координатор: затемнение |
| `selection::solid_sprite(fill, &monitor_id, &Box2D, opacity)` | `rst_render::selection` | координатор: UI-квады |
| `selection::checkerboard_tile(cell, size)` | `rst_render::selection` | координатор: шахматка скрытых |
| `Renderer::{draw, create_texture_from_rgba, set_dpi_scale, resize}` | `rst_render::Renderer` | координатор |
| `clipboard::read_image`, `is_supported_image` | `rst_win32::clipboard` | координатор: Ctrl+V |
| `config::save(&cfg, path)` | `rst_core::config` | координатор: после изменений |

---

## 16. Новая склейка (изменения в существующих файлах)

Только то, чего нет в примитивах (REVIEW §4, §2):

1. **`overlay.rs`**: (а) `OverlayWindow::create` — принимать `Sender<OverlayMessage>`
   и `HotkeyCombo` (или post-create-хук), регистрировать `RegisteredHotkey` и
   создавать `MouseCapture`/`CursorManager` на pump-потоке; (б) обработка
   `WM_HOTKEY` с `hwnd=NULL` до `DispatchMessageW`; (в) `wndproc`: мышь → `MouseCapture`,
   `WM_SETCURSOR` → `CursorManager`, `WM_APP_EDIT_CURSOR` → `set_shape`,
   `WM_KEYDOWN/CHAR` → `Key`; (г) `set_click_through(bool)` для входа/выхода.
2. **`overlay_manager.rs`**: `EditState`, объединённый `OverlayMessage`, цикл
   событий, жесты, undo-снапшот-команды, сборка кадра редактора; `sprites` →
   `HashMap<Uuid, Sprite>`.
3. **`rst-render`**: (опц.) `Renderer::dpi_scale()` геттер; `From<DipRect> for Box2D`.
4. **`rst-core`**: (по REVIEW §1) единый `Handle`/`Corner`/зона вместо трёх типов.
5. **`rst-media`/helper**: материализация байт буфера в `pasted/<uuid>.png`.

`main.rs` и логика `overlay_manager`-функций `add_sticker`/`run` в части M1
не меняются; M2-код добавляется отдельными функциями.

---

## 17. Порядок работ (для координатора)

1. **Мост событий** (п.16.1–2): `OverlayMessage`, вход/выход режима,
   хоткей-цикл, мышь из pump в координатор. Выходной критерий: хоткей включает
   режим, затемнение рисуется, клик по фону снимает выделение (рамки ещё нет).
2. **Масштаб DIP** (§5): `set_dpi_scale` + конверсия ввода.
3. **Хит-тест и курсор** (§7): зона под курсором → форма.
4. **Выделение** (§9): клик/Shift+клик/Ctrl+A, рамка марки.
5. **Жесты** (§8): drag → resize → rotate, затем магнит и `clamp_min_visible`.
6. **Undo/Redo** (§10): снапшот-команды, клавиши.
7. **Клавиатура и Ctrl+V** (§12): Esc, Delete, Ctrl+D, вставка из буфера.
8. **Тулбар/панель** (§14) — фаза 2, отдельно.

---

## 18. Открытые вопросы и расхождения (перенесено из REVIEW)

- Порог магнита: SPEC 3.4 «10 px» против `SnapConfig::default()` = `8.0`
  (`snap.rs:26`).
- Смещение дубликата: SPEC 3.6 «+20/+20» против `ops::DUPLICATE_OFFSET` = `16.0`.
- Три типа ручек (`input::Handle`, `selection::HandleKind`, `input::Corner`) —
  единый тип в core (REVIEW §1).
- Клавиатура между числовым полем тулбара и хоткеями (M2_UI_NOTES §9).
- Уведомление об эксклюзивном полноэкранном окне при входе (SPEC 3.1) — отдельный
  шаг, в этот план не вошёл.
