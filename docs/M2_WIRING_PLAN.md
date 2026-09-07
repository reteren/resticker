# M2 — план подключения UI-билдеров к живому циклу (срез 4)

Статус: **план для координатора, без изменений кода**. Прочитаны билдеры
`crates/resticker/src/{toolbar,cursor_panel,confirm_dialog}.rs`,
`crates/rst-render/src/marquee.rs`, `crates/rst-media/src/paste.rs`, текущий
`crates/resticker/src/overlay_manager.rs` (EditState, `handle_input`/`handle_key`/
`redraw`, `Zone`, `Gesture`), виджеты `crates/rst-render/src/widgets.rs`
(`Panel`/`EventResult`/`take_click`/`take_changed`/`take_submitted`), `text.rs`
(`rasterize`/`text_size`), `main.rs`.

Всё ниже — сшивка существующих примитивов, без нового рендера и без новых
зависимостей (кроме пары вспомогательных функций, отмечены явно).

Тулбар при мультивыделении (`selection.len() > 1`) идентичен одиночному (решение пользователя 2026-09-06, отменило скрытие слайдера и поля): состав общий, значения показываются по последнему выделенному, действия применяются ко всей группе (§4, §6).

---

## 1. Текущее состояние и цель

Билдеры существуют и покрыты тестами, но не вызваны: `overlay_manager` вообще не
знает про UI. `EditState` не имеет полей под панели; `redraw` рисует только
затемнение → стикеры → рамки выделения; `handle_input` роутит всё в зоны/жесты
(`Zone::Background/StickerBody/ResizeHandle/Rotate`); `handle_key` знает только
`Esc`/`Ctrl+Z/Y/A`/`Delete`/`Ctrl+D`, без `Ctrl+V`, без диалога.

Цель среза: тулбар, панель у курсора, модал подтверждения, марк (мультивыделение
протяжкой) и `Ctrl+V` вплести в цикл координатора.

---

## 2. Новое состояние координатора (`EditState`)

Добавить к существующим (`active`, `selection`, `gesture`, `snap`, `undo_stack`,
`redo_stack`, `pending_snapshot`):

```rust
struct EditState {
    // … существующие поля …

    // Retained-виджеты (hover/фокус/захват живут внутри Panel).
    toolbar: Option<Panel>,          // есть, когда selection.len() >= 1 (и
                                     // edit.marquee.is_none()), §4 и §6
    cursor_panel: Option<Panel>,     // есть, когда active
    confirm: Option<ConfirmState>,   // модал удаления, когда Some

    /// Кто держит текущий указательный жест (начиная с MouseDown).
    pointer_owner: PointerOwner,

    /// Снапшот для opacity-жеста ползунка (аналог pending_snapshot для сцены).
    ui_pending_snapshot: Option<Config>,

    /// Активная марка: якорь и текущий угол (для marquee_visuals в redraw).
    marquee: Option<(f64, f64, f64, f64)>,   // anchor_x, anchor_y, cur_x, cur_y

    /// Состояние тумблера «Больше не спрашивать» (ведёт координатор).
    dont_ask_checked: bool,

    /// Кэш 1×1 текстур по цвету + кэш текстур текста по (строка, цвет).
    ui_textures: UiTextures,
}
```

```rust
enum PointerOwner { None, Confirm, Toolbar, CursorPanel, Scene }
```

```rust
struct ConfirmState {
    /// Снимок на момент открытия диалога — он и уйдёт в undo по «Удалить».
    snapshot: Config,
    /// id к удалению (зафиксированы на момент открытия).
    ids: Vec<Uuid>,
    panel: Panel,            // confirm_dialog::build(count, center)
}
```

`UiTextures` — кэш: `HashMap<[u8;3], Texture>` для `Primitive::Fill`,
`HashMap<(String, [u8;3]), Texture>` для `Primitive::Text` (кэш по
`text::rasterize(text, color, scale)` → `create_texture_from_rgba`), и
`HashMap<Icon, Texture>` для иконок (см. §3 и §14).

---

## 3. Конвертер `Primitive` → `Sprite` (новая функция в bin-крейте)

`Panel::draw(&mut out)` отдаёт `Vec<Primitive>` (`Fill`/`Icon`/`Text`, виджеты.rs
§81-102). Перед отрисовкой кадра нужен конвертер
`primitives_to_sprites(&[Primitive], &mut UiTextures, renderer, monitor_id) -> Vec<Sprite>`:

- `Fill { rect, color, opacity }` → `solid_sprite(color_texture(color), …)` — 1×1
  текстура цвета из кэша (создаётся один раз на каждый цвет).
- `Text { rect, text, color, opacity }` → `text::rasterize(text, color, scale_px)`
  → `create_texture_from_rgba` → `Sprite` (кэш по `(text, color)`).
- `Icon { rect, icon, opacity }` → текстура иконки из кэша; без ассетов —
  placeholder (§14). 

Все прямоугольники — DIP, рендерер масштабирует по `scale` (как `solid_sprite`).
Это единственный новый рендер-код среза; конвертер живёт в `overlay_manager.rs`
(или рядом в bin-крейте).

---

## 4. Показ/скрытие панелей

| Панель | Показывается | Скрывается |
|---|---|---|
| **Тулбар** | `active && selection.len() >= 1` (состав одинаковый для любого выделения, §6) | выход из режима; изменение числа выделенных; скрыт, пока `edit.marquee.is_some()` |
| **Панель у курсора** | вход в режим (`active`), позиция — курсор | выход из режима |
| **Модал подтверждения** | `ConfirmState::Some` (запрос на удаление) | Cancel/Delete/Esc |

**Тулбар** (`toolbar::build_toolbar(bounds: &DipRect, state: &ToolbarState, screen_h: f64)`):
- билдер **унифицирован** для одиночного и мульти: `bounds` — union AABB
  `selection.bounds(&cfg.stickers)` (для одиночного — тот же ось-выровненный
  прямоугольник, что раньше строил `selection_aabb`); состав одинаков
  (решение пользователя 2026-09-06, отменило скрытие слайдера/поля), значения
  показываются по последнему выделенному, `TOOLBAR_WIDTH` фиксирована;
- `bounds()` → `None` (пустое выделение / все вырожденные стикеры) — тулбар
  не строить;
- пересобрать при смене числа выделенных (клик, `Ctrl+A`, отпускание марки,
  `Ctrl+D`/`TB_DUPLICATE`, undo/redo после `selection.prune`, `Delete`) —
  билдер дёшев, но **сбрасывает hover**;
- **во время марки не показывать**: `rubber_band` пересобирает выделение на
  каждом `MouseMove` (§8), поэтому тулбар виден только при
  `edit.marquee.is_none()`; после `MouseUp` марки пересобрать по финальному
  выделению;
- при **перетаскивании** не пересобирать, а `toolbar.translate(dx, dy)` по дельте
  стикера (SPEC 3.6: тулбар едет за выделением в том же кадре);
- зеркало opacity: после любого изменения `slider/field` —
  `widget_mut::<Slider>(TB_SLIDER).set_value(v)` и
  `widget_mut::<NumericField>(TB_FIELD).set_value(v)` синхронно (§6).

**Панель у курсора** (`cursor_panel::build_cursor_panel(cursor, &screen, all_visible)`):
- построить при входе в режим; далее `translate` по дельте курсора на каждом
  hover-`MouseMove` (не пересобирать — иначе дёргается hover);
- `all_visible = cfg.stickers.iter().all(|s| s.visible)` — пересчитывать при
  показе и после `BTN_TOGGLE_ALL`.

**Модал** (`confirm_dialog::build(count, center)`):
- `center` — центр `selection.bounds(&cfg.stickers)` (fallback — центр экрана);
- `count = edit.selection.len()`.

---

## 5. Маршрутизация указателя: новый `handle_input`

Сейчас `handle_input` роутит по флагу `dragging`. С UI нужен приоритет
**top-down по z-order** (confirm > cursor panel > toolbar > сцена), и
запоминание, кто забрал жест на `MouseDown`:

**`MouseDown`** (pos → DIP):
1. Если `edit.confirm.is_some()` → `confirm.panel.pointer_event(Down)`; если
   `consumed` → `pointer_owner = Confirm`, вернуть `redraw`; иначе (клик мимо
   модала) — ничего, в сцену не пускать (модал модален).
2. Иначе тулбар (`panel.pointer_event(Down)`): `consumed` → `pointer_owner =
   Toolbar`.
3. Иначе панель у курсора: `consumed` → `pointer_owner = CursorPanel`.
4. Иначе сцена: существующая логика `resolve_zone` → жесты (§8 добавляет
   `Marquee`). `pointer_owner = Scene`, `edit.gesture = …`.

**`MouseMove`** (после `to_dip`):
1. `pointer_owner` не `None` и не `Scene` → кормить эту панель
   (`panel.pointer_event(Move)`), `consumed` игнорируется (панель сама решит);
   вернуть `redraw` панели.
2. `pointer_owner == Scene` и `dragging` → `apply_gesture` (существующий путь).
3. Иначе hover: панели top-down для hover-состояний
   (`panel.pointer_event(Move)`, только для hover-подсветки — их
   `EventResult.consumed` не влияет на курсор), затем зона сцены →
   `overlay.post_cursor_shape(...)`. Курсор: если указатель над любой панелью —
   `Arrow`, иначе — зона (§6 в плане M2_INTEGRATION_PLAN).

**`MouseUp`**:
1. `pointer_owner` = панель → `panel.pointer_event(Up)`, затем §6 (опрос
   действий), `pointer_owner = None`.
2. `pointer_owner == Scene` → существующий `MouseUp` жеста (§8 — марка).
3. `None` → ничего (клик не начинался).

**`CaptureLost`** → сбросить `pointer_owner = None`; если `pointer_owner` был
Toolbar и копился `ui_pending_snapshot` — отбросить его; если был Scene —
существующий откат жеста.

Ключевое изменение: решение «панель или сцена» принимается на `MouseDown` и
фиксируется в `pointer_owner`; флаг `dragging` больше не диктует маршрут
(ползунок и жест сцены оба держат захват окна).

---

## 6. Действия виджетов → `rst_core::ops` / undo

Опрос выполняется после `Up` (или после каждого события для слайдера), через
`panel.widget_mut::<T>(id).take_*()`.

### Тулбар (одиночное выделение, id = единственный выделенный)

| Виджет | Чтение | Действие |
|---|---|---|
| `TB_SLIDER` | `take_changed() -> Option<u32>` | opacity: `0..=100` → `0.0..=1.0`; при **первом** изменении `ui_pending_snapshot = Some(cfg.clone())` (аналог жеста), далее `apply_transform(cfg, sprites, id, pl, tr{opacity})`; зеркало в `TB_FIELD` |
| `TB_FIELD` | `take_submitted() -> Option<u32>` | та же opacity-запись; снапшот — `commit_undo_snapshot(edit, cfg.clone())` **до** применения; `take_cancelled()` — ничего |
| `TB_EYE` | `take_click()` | `commit_undo_snapshot` + `ops::toggle_visibility(cfg, id)` + save |
| `TB_ORDER_UP` | `take_click()` | `commit_undo_snapshot` + `ops::step_up(cfg, id)` + save |
| `TB_ORDER_DOWN` | `take_click()` | `commit_undo_snapshot` + `ops::step_down(cfg, id)` + save |
| `TB_DUPLICATE` | `take_click()` | `commit_undo_snapshot` + `ops::duplicate(cfg, id)` → `resync_sprites` → выбрать копию → save |
| `TB_DELETE` | `take_click()` | тот же путь, что `Delete`-клавиша (§7): `begin_delete(...)` |

**Коммит opacity-жеста**: на `MouseUp`, когда `pointer_owner == Toolbar` и
`ui_pending_snapshot.is_some()` → если `cfg != before` — `commit_undo_snapshot`,
иначе отбросить (клик по дорожке без движения — не тратить шаг истории).
Модель: opacity живёт в `transform.opacity` — переиспользовать `apply_transform`
(placement не меняется).

### Тулбар (мультивыделение, `selection.len() > 1`)

Состав тулбара унифицирован с одиночным (решение пользователя 2026-09-06, отменило скрытие ползунка и поля: «если я выделяю 2 стикера, чтобы одновременно менять их прозрачность»). Ползунок, числовое поле, сброс масштаба и видео-виджеты (если среди выделенных есть хоть одно видео) присутствуют всегда. Значения отображаются по последнему выделенному стикеру, а действия применяются ко всему выделению.
`ids = selection.ids()` — каждая операция применяется ко всему выделению и делает
**один** `commit_undo_snapshot` до применения (`Ctrl+Z` снимает батч целиком).

| Виджет | Чтение | Действие |
|---|---|---|
| `TB_SLIDER` | `take_changed() -> Option<u32>` | прозрачность всей группы: при первом изменении `ui_pending_snapshot = Some(cfg.clone())`, далее `apply_opacity` ко всем `ids`; зеркало в `TB_FIELD` |
| `TB_FIELD` | `take_submitted() -> Option<u32>` | ввод прозрачности для всей группы; `commit_undo_snapshot` до применения, синхронное обновление ползунка |
| `TB_LAYERS` | `take_click()` | открывает панель выбора окон для группы (`WindowPickerState.applies_to`) |
| `TB_EYE` | `take_click()` | `commit_undo_snapshot` + `ops::set_visible_many(cfg, &ids, target)` + save; `target = !(все выбранные видимы)` — группа сходится к однородному состоянию: если хоть один скрыт → показать всех, иначе скрыть всех |
| `TB_ORDER_UP` | `take_click()` | `commit_undo_snapshot` + `reorder_selection(cfg, &ids, true)` + save — стикеры по очереди поднимаются на самый верх (`ops::bring_to_front`), в итоге последний выделенный оказывается на вершине стека (решение пользователя 2026-09-06) |
| `TB_ORDER_DOWN` | `take_click()` | `commit_undo_snapshot` + `reorder_selection(cfg, &ids, false)` + save — стикеры по очереди опускаются в самый низ (`ops::send_to_back`), в итоге последний выделенный оказывается в самом низу (решение пользователя 2026-09-06) |
| `TB_DUPLICATE` | `take_click()` | общий `duplicate_selection(...)`: один снимок, копии всех выделенных (`ops::duplicate`), `resync_sprites`, перенос выделения на копии, save |
| `TB_RESET_SCALE` | `take_click()` | сброс масштаба: для каждого стикера со спрайтом `ops::reset_transform_and_size` (натуральный размер, сброс поворота/отражений) + save |
| `TB_DELETE` | `take_click()` | `begin_delete(...)` (§7) — батч: `count = selection.len()`, один снимок |
| `TB_PLAY_PAUSE` | `take_click()` | (если есть видео) переключает `playback.paused` у всех выделенных видео: если играет хоть одно → пауза всем, иначе старт + save |
| `TB_TIMELINE` | `take_click()` | (если есть видео) переключает `playback.show_timeline` у всех выделенных видео + save |
| `TB_VOLUME` | `take_mute_click()` / drag | (если есть видео) кнопка-динамик: клик переключает `playback.muted` у всех выделенных видео (уровень громкости сохраняется); драг шкалы меняет `playback.volume` и снимает mute + save |

Чистая функция в `rst_core::ops`: `set_visible_many(config, ids, visible)`. Функции `step_up_many`/`step_down_many` не создавались: для порядка группы используется `reorder_selection` через существующие `bring_to_front`/`send_to_back`, реализуя запрошенную пользователем логику выноса группы на самый верх/низ стека.

Общий helper `duplicate_selection(edit, cfg, renderer, sprites, config_path)`
в `overlay_manager.rs`: извлечь из ветки `Ctrl+D` (существующий хоткей, §10) и
звать его и из неё, и из `TB_DUPLICATE` (одиночного и мульти) — кнопка и хоткей
не расходятся.

### Панель у курсора

| Виджет | Действие |
|---|---|
| `BTN_LOAD_FILE` | отправить `OverlayCommand::OpenFileDialog` координатору → round-trip на main (§12) |
| `BTN_TOGGLE_ALL` | `commit_undo_snapshot` + переключить `visible` у всех стикеров (все → ни одного) + save + перестроить панель (сменилась иконка) |
| `BTN_SETTINGS` | `OverlayCommand::OpenSettings` → round-trip (§12) |
| `BTN_EXIT` | `toggle_edit_mode(...)` (выход) |

### Модал (после `Up` с `pointer_owner == Confirm`)

| Виджет | Действие |
|---|---|
| `ID_DELETE` | `commit_undo_snapshot(edit, confirm.snapshot)`; `ops::delete` для каждого `confirm.ids`; `selection.prune`; `resync_sprites`; save; `confirm = None` |
| `ID_CANCEL` | `confirm = None` (снимок не коммитим) |
| `ID_DONT_ASK` | `ops::suppress_delete_confirmation(cfg)` + `dont_ask_checked` перевернуть + save + redraw (галочка) |
| `ID_MESSAGE` | клики поглощаются, действия нет |

---

## 7. Confirm dialog перехватывает `Delete`

Общий входной helper `begin_delete(edit, cfg, center) -> bool`:

```rust
if edit.selection.is_empty() { return false; }
if ops::should_confirm_delete(cfg) {
    edit.confirm = Some(ConfirmState {
        snapshot: cfg.clone(),          // на момент открытия
        ids: edit.selection.ids().to_vec(),
        panel: confirm_dialog::build(edit.selection.len() as u32, center),
    });
} else {
    // текущий быстрый путь: snapshot + delete + prune + resync + save
    commit_undo_snapshot(edit, cfg.clone());
    for id in edit.selection.ids().to_vec() { let _ = ops::delete(cfg, id); }
    edit.selection.prune(&cfg.stickers);
    resync_sprites(renderer, cfg, sprites);
    config::save(...);
}
```

Точки вызова: `handle_key` `VK_DELETE` **и** `TB_DELETE` — оба идут через
`begin_delete`, чтобы диалог не зависел от того, чем вызван. Пока
`confirm.is_some()`:

- `handle_key`: `Esc` → `confirm = None` (отмена), **не** выход из режима;
  остальные клавиши → `false` (модал блокирует историю/действия);
- `handle_input`: только модал (§5, п.1), в сцену события не уходят.

Снапшот берётся при открытии, а не при нажатии `Delete` — откат вернёт ровно
состояние «до вопроса», даже если выделение менялось (модал их и так блокирует,
но так инвариант простой).

---

## 8. Marquee: новый вариант жеста

`Gesture` добавляет:

```rust
Marquee { anchor: (f64, f64) },
```

А также поле-флаг в `EditState` (или в жесте) `marquee_started: bool` — чтобы
отличить «клик по пустому месту» от «протяжки» (порог ~4 DIP, константа).

**Изменение существующего `MouseDown` для `Zone::Background`:**
сейчас он сразу `selection.click(None)`. Новое поведение (SPEC 3.2): клик по
пустому месту снимает выделение **на `MouseUp`**, протяжка — рамка:

```rust
Zone::Background => {
    edit.gesture = Some(Gesture::Marquee { anchor: (dip_x, dip_y) });
    edit.marquee_started = false;
    // выделение НЕ трогаем — решит MouseUp
    false
}
```

**`apply_gesture`, ветка `Marquee`:**
```rust
let rect = DipRect::new(anchor.0, anchor.1, dip_x - anchor.0, dip_y - anchor.1);
if rect.w.abs() >= THRESHOLD || rect.h.abs() >= THRESHOLD {
    edit.marquee_started = true;
    edit.marquee = Some((anchor.0, anchor.1, dip_x, dip_y));   // для redraw
    edit.selection.rubber_band(&cfg.stickers, &rect);          // live-обновление
}
true
```

**`MouseUp` для `Marquee`:** если `!marquee_started` → `selection.click(None)`
(простой клик по фону снимает выделение); иначе выделение уже выставлено
`rubber_band` на последнем `Move`. В обоих случаях `edit.marquee = None`,
`edit.gesture = None`, вернуть `true`.

**`CaptureLost` / `toggle_edit_mode` с `Marquee`:** модель не мутируется, поэтому
`Gesture::start()` (сейчас возвращает `&GestureStart`) для `Marquee` не
применим — изменить на `Option<&GestureStart>` (или метод `rollback(cfg, sprites)`,
который для `Marquee` — no-op: просто `edit.marquee = None`). Жест сцены
(`Drag/Resize/Rotate`) откатывается как сейчас.

**Отрисовка:** в `redraw` под рамками выделения (или поверх стикеров, но ниже
UI) — `marquee_visuals(anchor, current)` → `fill` и `dashes` как спрайты через
`solid_sprite` с акцентной текстурой (§3; добавить 1×1 текстуру цвета
`theme::SLIDER_FILL` для штрихов и заливки с прозрачностями
`MARQUEE_FILL_OPACITY`/`MARQUEE_STROKE_OPACITY`).

Примечание: `rubber_band` нормализует прямоугольник сам (`selection_set.rs`),
поэтому знак дельты не важен.

---

## 9. Ctrl+V: вставка изображения

`handle_key` добавляет `VK_V if modifiers.ctrl`. Два случая:

**А. Сфокусировано числовое поле тулбара**
(`toolbar.focused_widget() == Some(TB_FIELD)`): вставить только цифры —
прочитать текст буфера и отдать `toolbar.paste(text)` (виджет отфильтрует).
Нужен маленький `rst_win32::clipboard::read_text() -> Option<String>`
(`CF_UNICODETEXT`) — в `clipboard.rs` сейчас только картинки; это единственное
новое чтение буфера.

Ветка работает одинаково в одиночном и мульти-режиме: числовое поле присутствует в обоих случаях (§4), и при фокусе вставка цифр задаёт прозрачность всей группе.

**Б. Обычная вставка (новая картинка):**
```rust
match rst_win32::clipboard::read_image() {
    Ok(Some(ClipboardImage::Png(b))) | Ok(Some(ClipboardImage::Bmp(b))) => {
        let img = ClipboardImage::Png(b) / ClipboardImage::Bmp(b);   // как получен
        let target_dir = config_path.parent();                        // %APPDATA%\resticker
        let path = rst_media::paste::materialize(&img, target_dir)?;  // pasted/<uuid>.png
        commit_undo_snapshot(edit, cfg.clone());   // один шаг undo на всю вставку
        add_sticker(&overlay, &mut renderer, cfg, config_path, sprites, path);  // существующий
        true
    }
    Ok(Some(ClipboardImage::Files(paths))) => {
        commit_undo_snapshot(edit, cfg.clone());
        for p in paths { if rst_win32::clipboard::is_supported_image(&p) { add_sticker(...) } }
        true
    }
    Ok(None) | Err(_) => false,     // картинки в буфере нет — не ошибка
}
```

Точки вставки:
- `materialize` на **координаторе** (дисковой операции нет, работает с любого
  потока); декодирование PNG/BMP для валидации — внутри `materialize`;
- `add_sticker` — существующая функция, ничего не меняем, только оборачиваем
  в один undo-снимок (чтобы `Ctrl+Z` снимал вставку);
- ошибка `PasteError`/`ClipboardImage::Files` без изображений — warn-лог, без
  изменения состояния.

Порядок `Ctrl+V`: сначала проверить фокус поля (§9.А), иначе изображение (§9.Б).
Пока поле в фокусе, `Ctrl+V` в поле не должен создавать стикер.

---

## 10. Новый `handle_key`: порядок разбора

Сверху вниз:

1. `edit.gesture.is_some() && vk != VK_ESCAPE` → `false` (существующая защита;
   распространить и на `pointer_owner == Toolbar`/`CursorPanel`, пока идёт
   слайдер-драг, и на `confirm.is_some()`).
2. `edit.confirm.is_some()` → `Esc` = отмена модала (вернуть `true`), иначе
   `false` (модал блокирует всё, кроме собственных кнопок-кликов).
3. **Тулбар-поле в фокусе**: перевести `vk` в `rst_render::Key` (цифры
   `0x30..=0x39` → `Key::Digit`, `VK_BACK`→`Backspace`, `VK_RETURN`→`Enter`,
   `VK_ESCAPE`→`Escape`, `VK_LEFT`/`VK_RIGHT`→arrows) и отдать
   `toolbar.key_event(key)`; если `consumed` — вернуть `redraw`, **не** выполняя
   сцену. Важно: `Esc` здесь обрабатывает **поле** (`take_cancelled()`),
   а не выход из режима. Ветка работает и для одиночного, и для мультивыделения
   (состав тулбара одинаков, §4).
4. Существующие хоткеи: `Esc` (выход), `Ctrl+Z`/`Ctrl+Shift+Z`/`Ctrl+Y`,
   `Ctrl+A`, `Ctrl+D`, `Delete` (через `begin_delete`), `Ctrl+V` (§9).

Это соответствует M2_UI_NOTES §9: цифры/`Backspace`/`Esc` уходят полю,
`Ctrl`-комбинации — ядру.

---

## 11. `redraw`: состав кадра и порядок слоёв

Порядок отрисовки (снизу вверх), с добавлением UI (M2_UI_NOTES §7):

```
1. затемнение (существует)
2. стикеры по order (существует)
3. марка — marquee_visuals (заливка + штрихи), только если edit.marquee.is_some()
4. рамки выделения (существует)
5. тулбар — panel.draw() → primitives_to_sprites
6. панель у курсора — то же
7. модал подтверждения — то же (самый верх)
```

Конвертер (§3) вызывается после `panel.draw(&mut prims)`; каждый примитив →
`Sprite`; все спрайты добавляются в `frame` перед `renderer.draw(&frame)`.

**Курсор** (hover): если указатель над любой видимой панелью (confirm →
cursor_panel → toolbar) — `post_cursor_shape(Arrow)`; иначе зона сцены (как
сейчас).

---

## 12. Round-trip на main-поток (файл-диалог, настройки)

`BTN_LOAD_FILE` и `BTN_SETTINGS` требуют Tauri (главный поток). Существующий
канал — только «main → overlay». Добавить обратный канал запросов:

- `enum CoordinatorRequest { OpenFileDialog, OpenSettings }` в `overlay_manager`;
- `OverlayHandle::start` возвращает ещё и `Receiver<CoordinatorRequest>`
  (или `overlay_manager::start` принимает `Sender<CoordinatorRequest>` из main);
- в `main.rs` поток из `setup` читает запросы:
  - `OpenFileDialog` → `tauri_plugin_dialog` (плагин уже инициализирован) →
    по выбранному пути `overlay.send(OverlayCommand::AddSticker(path))`
    (существующая команда, существующий `add_sticker`);
  - `OpenSettings` → показать `get_webview_window("settings")` (как в
    tray-обработчике).

Дополнительно: пункт трея «Показать/скрыть все стикеры» сейчас no-op
(`main.rs:108`) — его можно перенаправить в тот же путь, что `BTN_TOGGLE_ALL`
(новый `OverlayCommand::ToggleAllVisible`), но это опционально и не блокирует
срез.

---

## 13. Порядок работ (для координатора)

1. `EditState`: новые поля + `UiTextures` + конвертер `primitives_to_sprites`
   (§2–3) с кэшем цветов; placeholder-текстуры иконок (§14).
2. `redraw`: слои 5–7 + марка (§11).
3. `handle_input`: `PointerOwner` и новый маршрут (§5); тулбар/панель собираются
   и показываются (§4), действия кнопок и opacity (§6).
4. Confirm-диалог через `begin_delete` (§7), перехват в `handle_key`.
5. `Gesture::Marquee` + `rubber_band` + `marquee_visuals` (§8).
6. Мультивыделение тулбара: условие показа `>= 1` + скрытие во время марки (§4);
   `ops::set_visible_many`/`step_up_many`/`step_down_many` и мульти-действия
   кнопок (§6); общий `duplicate_selection` для `Ctrl+D` и `TB_DUPLICATE` (§6).
7. `Ctrl+V` (§9) + фокус-поле (§10.3).
8. Round-trip `CoordinatorRequest` (§12) для `BTN_LOAD_FILE`/`BTN_SETTINGS`.

Каждый шаг — build/clippy/test + ручной прогон соответствующего действия.

---

## 14. Зависимости и подзадачи (за пределами чистой сшивки)

- **Иконки кнопок** (`Primitive::Icon`): текстур из `assets/` нет. Минимум для
  видимости кнопок — placeholder (заливка квадратом цвета `theme::BUTTON_BG`),
  настоящие иконки — отдельная задача ассетов.
- **`clipboard::read_text()`** для вставки цифр в поле (§9.А) — маленькая новая
  функция в `rst-win32::clipboard` (`CF_UNICODETEXT`).
- **Акцентная 1×1 текстура** для марки (§8) — цвет `theme::SLIDER_FILL`.
- **Позиция панели у курсора** пока в памяти (`EditState`); персистентность
  позиции — SPEC 3.8, отдельный шаг (поле в config.json).
- **Скрытые стикеры** рисуются/хитятся только `visible` (M2-срез 3); шахматка
  скрытых в режиме редактирования — остаётся следующим срезом (не блокер UI).
- **Мульти-действия тулбара** — `ops::set_visible_many`/`step_up_many`/
  `step_down_many` (§6), чистые функции с юнит-тестами.
- **Мульти-драг/ресайз** (`resolve_zone`, ручки мультивыделения) — не задача
  тулбара; в срезе не трогаем (§4), задел на следующий шаг.
