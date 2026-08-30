# Инвентаризация surface I под Dark Liquid Glass

Модули: `crates/rst-render/src/selection.rs`, `marquee.rs`, `window_highlight.rs`,
`video_timeline.rs`. Спецификация: `docs/DESIGN_LIQUID_GLASS.md` (ниже — «§N»).
Все строки — по текущему состоянию файлов (2026-08-29). Отчёт чисто
инвентаризационный: переписывать модуль по нему можно не открывая исходники.

Кратко: в этих четырёх файлах **нет ни одного** использования
`WidgetStyle::Settings`, `theme::settings::*`, `settings_bevel`,
`settings_frame`, `settings_frame_radius`, `accent_outline` — старый «Settings»
язык тут не живёт. Всё, что тут есть: плоские `Primitive::Fill` и
`solid_sprite`, три цветовых литерала и четыре токена старой тёмно-серой
схемы `theme` (`LOCK_INDICATOR_BG`, `SLIDER_TRACK`, `SLIDER_FILL` — только в
доккомменте, `TEXT`). Основная работа surface I — не «перекрасить», а
перевести плоские заливки на стеклянные примитивы §7 и добавить анимацию §5.

---

## 1. Цвета

### 1.1 Литералы `[0x.., 0x.., 0x..]` в четырёх файлах

| № | Файл:строка | Константа/место | Что красит | Куда по §2 |
|---|---|---|---|---|
| 1 | `selection.rs:46` | `SELECTION_COLOR = [0x3c, 0x98, 0x98]` | рамка выделения (реализация — вызывающий слой), ручки поворота не трогает; **заодно** марка протяжки и заливка полосы видео (см. риск №2) | бирюза запрещена §2.4 → белый: рамка выделения — `STROKE_STRONG` (0.30); заливка полосы видео — `TEXT` |
| 2 | `selection.rs:49` | `CHECKER_MAGENTA = [0xff, 0x00, 0xff]` | клетки шахматки скрытых стикеров | фуксия — не из палитры §2; единственный разрешённый цвет — `DANGER`. Шахматка функциональна, но цвет надо снять: белый `#FFFFFF`, альфа — через `HIDDEN_STICKER_CHECKERBOARD_OPACITY` (см. риск №3) |
| 3 | `selection.rs:52` | `CHECKER_BLACK = [0x00, 0x00, 0x00]` | вторые клетки шахматки | чёрный монохромен, в §2 токена чёрного нет; оставить как есть |
| 4 | `selection.rs:316-317` | `float3 magenta = float3(1.0, 0.0, 1.0); float3 black = ...` **внутри строки HLSL** | шейдер `CHECKERBOARD_HLSL` (мёртвый код, модулем не задействован) | литералы в строке grep по `[0x..]` не находит — чистить/обновлять вручную (риск №6) |
| 5 | `window_highlight.rs:50` | `HighlightKind::Pin => [0x3c, 0x98, 0x98]` | постоянная обводка закреплённого окна (и через него — ручка и заливка полосы видео, риск №2) | бирюза запрещена §2.4 → `STROKE_STRONG` (0.30) или `TEXT`; см. также риск №8 (пульс пина красится литералом в другом крейте) |
| 6 | `window_highlight.rs:52` | `HighlightKind::Hover => [0x4f, 0x9c, 0xff]` | подсветка окна под курсором в режиме выбора | синий запрещён §2.4 → `TEXT` (белый, альфа — `opacity()` 0.65) |

### 1.2 Используемые токены `theme::*` (определены в `widgets.rs`)

| № | Файл:строка | Токен (widgets.rs:определение) | Что красит | Куда по §2 |
|---|---|---|---|---|
| 7 | `video_timeline.rs:270` | `theme::LOCK_INDICATOR_BG` = `[0x1a,0x1a,0x1e]` (widgets.rs:281) | подложка полосы (прямоугольник `self.bounds`) | старая тёмно-серая схема (сосед `PANEL_BG`, выводится из употребления) → `SUNKEN_BG` (жёлоб, `#000` @ 0.34), либо `GLASS_INK` при стеклянной подложке |
| 8 | `video_timeline.rs:275` | `theme::SLIDER_TRACK` = `[0x55,0x55,0x5e]` (widgets.rs:226) | дорожка перемотки | → `SUNKEN_BG` (утопленная дорожка, §4 «Sunken переворачивает свет») |
| 9 | `video_timeline.rs:281, 287` | `HighlightKind::Pin.color()` | заливка уже проигранного и ручка полосы | не токен — сцепление с window_highlight (риск №2); после перекраски Pin станет белым → `TEXT`/`STROKE_STRONG`; лучше дать полосе собственный токен |
| 10 | `video_timeline.rs:306, 317` | `theme::TEXT` = `[0xf0,0xf0,0xf0]` (widgets.rs:238) | подписи времени слева/справа | → `TEXT` `#FFFFFF` @ 0.97 (альфа сейчас домножается константами 0.75/0.95 — см. риск №7) |
| 11 | `marquee.rs:25` | `theme::SLIDER_FILL` — только в доккомменте | нет (модуль цвета не задаёт, цвет даёт вызывающий слой) | доккоммент устарел и фактически неверен: реальный вызывающий слой красит марку `SELECTION_COLOR` (overlay_manager.rs:13738), а не `SLIDER_FILL`; `SLIDER_FILL` — синий акцент, удаляется |

### 1.3 WidgetStyle::Settings и theme::settings

В четырёх файлах surface I — **ни одного использования** (проверено поиском:
`WidgetStyle`, `theme::settings`, `settings_*`, `accent_outline` не встречаются
нигде, кроме доккоммента в `marquee.rs:25` про `theme::SLIDER_FILL`). Удаление
`WidgetStyle::Settings` и модуля `theme::settings` (widgets.rs:295-345) эти
файлы механически не заденет. Ближайшие старые бевель-функции живут в
`widgets.rs` (surface A, координатор): `settings_bevel` :368, `settings_frame_radius` :402,
`settings_frame` :581, `accent_outline` :595, ветки `WidgetStyle::Settings` в
виджетах :859, :1151, :1588, :1934, :2239, :2281, :2578.

### 1.4 Цвета, приходящие из вызывающего слоя (не в этих файлах)

| Что | Где красится | Строка |
|---|---|---|
| марка протяжки (fill + dashes) | `overlay_manager.rs` — текстура `SELECTION_COLOR` | 13738 |
| рамка выделения | `overlay_manager.rs` — текстура `SELECTION_COLOR` | 13773 |
| ручки ресайза (уже белые) | `overlay_manager.rs` — `white_tex` | 13779, 13832 |
| пульс пина/анпина | `overlay_manager.rs` — `PIN_FLASH_COLOR_PIN` `[0x3c,0x98,0x98]`, `PIN_FLASH_COLOR_UNPIN` `[0xd0,0x3c,0x3c]` | 682, 687 |
| постоянная обводка пина | `overlay_manager.rs` — `primitives_custom(PIN_FLASH_COLOR_PIN, PINNED_OUTLINE_OPACITY 0.55, …)` | 13878-13882 |
| превью (dev-инструмент) | `ui_preview.rs` — `SELECTION_COLOR` для рамки, белый для ручек | 338, 345 |

Вывод: часть «перекраски surface I» физически находится в `overlay_manager.rs`
(surface J) — рамка выделения, марка, пульс. Без правки J перекраска
`SELECTION_COLOR`/`HighlightKind::Pin` даст только половину эффекта.

---

## 2. Геометрия

### 2.1 Константы геометрии в четырёх файлах

| № | Файл:строка | Константа | Значение | Что задаёт | Переход по §3 |
|---|---|---|---|---|---|
| 1 | `selection.rs:20` | `OUTLINE_THICKNESS_DIP` | 2.0 | толщина рамки выделения | токена нет; ближайший — `HAIRLINE` 1.0 («кромка и обводка»). Решение: 2→1 (рамка станет волосинкой) или оставить 2.0 как «обводку активного выделения» — спека молчит |
| 2 | `selection.rs:23` | `HANDLE_SIZE_DIP` | 10.0 | сторона квадратной ручки ресайза | токена нет. `RADIUS_TIGHT` 7 — радиус для «мелкой иконка-кнопки»: если ручки становятся стеклянными контролами — радиус 7 при сохранении размера 10 (см. §3-риск №1) |
| 3 | `selection.rs:28` | `ROTATE_HANDLE_SIZE_DIP` | 22.0 | сторона ручки поворота со стрелкой | токена нет (больше `BUTTON_SIZE` 30 — не подходит); сохранить размер, радиус — `RADIUS_TIGHT` 7 |
| 4 | `selection.rs:34` | `ROTATE_HANDLE_GAP_DIP` | 18.0 | вынос ручки поворота от угла | токена нет; сохранить (участвует в хит-тесте, риск №9) |
| 5 | `selection.rs:37` | `EDIT_OVERLAY_OPACITY` | 0.5 | затемнение режима редактирования | не геометрия; чёрный монохромен, токена в §2 нет — оставить |
| 6 | `selection.rs:268` | `HIDDEN_STICKER_CHECKERBOARD_OPACITY` | 0.5 | прозрачность шахматки | оставить (при смене цвета шахматки на белый, скорее всего, поднять до ~0.6-0.7 — видимость на светлом кадре, см. риск №3) |
| 7 | `marquee.rs:13` | `MARQUEE_THICKNESS_DIP` | 1.0 | толщина штриха пунктира | = `HAIRLINE` 1.0 — совпадает точно |
| 8 | `marquee.rs:15` | `MARQUEE_DASH_DIP` | 6.0 | длина штриха | токена нет (шаг пунктира — паттерн, не величина §3); сохранить константой модуля |
| 9 | `marquee.rs:17` | `MARQUEE_GAP_DIP` | 4.0 | зазор между штрихами | токена нет; сохранить |
| 10 | `marquee.rs:19` | `MARQUEE_FILL_OPACITY` | 0.08 | заливка прямоугольника протяжки | ≈ `CTRL_BG` (0.045) или оставить 0.08 (альфа токенов — см. риск №7) |
| 11 | `marquee.rs:21` | `MARQUEE_STROKE_OPACITY` | 0.9 | прозрачность штрихов | ≈ `TEXT` (0.97) или оставить |
| 12 | `window_highlight.rs:29` | `HIGHLIGHT_THICKNESS_DIP` | 3.0 | толщина рамки подсветки окна | токена нет; функциональная рамка поверх чужого окна — 1.0 `HAIRLINE` потеряет читаемость; предложение: сохранить 3.0 (или 2.0) + белый цвет |
| 13 | `window_highlight.rs:60, 62` | `opacity()` Pin 0.9 / Hover 0.65 | — | непрозрачность рамки | перевыразить через альфу токенов §2 (см. риск №7) |
| 14 | `video_timeline.rs:44` | `TIMELINE_MARGIN` | 8.0 | отступ полосы от краёв стикера | токена нет (не панель); сохранить |
| 15 | `video_timeline.rs:46` | `TIMELINE_HEIGHT` | 24.0 | высота полосы (дорожка + подписи) | токена нет; составная величина — сохранить |
| 16 | `video_timeline.rs:50` | `TIMELINE_MIN_WIDTH` | 120.0 | порог «полоса не помещается» | не геометрия вида — сохранить |
| 17 | `video_timeline.rs:52` | `TIMELINE_TRACK_H` | 2.0 | толщина дорожки в покое | `HAIRLINE` 1.0 или сохранить 2.0 как жёлоб `SUNKEN_BG`; спека молчит |
| 18 | `video_timeline.rs:56` | `TIMELINE_TRACK_H_HOVER` | 3.0 | толщина при наведении/драге | становится частью анимации §5 (наведение 160 мс), а не скачка |
| 19 | `video_timeline.rs:58` | `TIMELINE_KNOB` | 10.0 | сторона ручки | = `RADIUS_CTRL` 10 — ручка как стеклянный контрол с радиусом 10 |
| 20 | `video_timeline.rs:61, 63` | `TIMELINE_BG_OPACITY` 0.5 / `_HOVER` 0.85 | — | подложка полосы | подложка → `SUNKEN_BG` (альфа 0.34) или `GLASS_INK`; hover-переход — по §5 |
| 21 | `video_timeline.rs:65, 67` | `TIMELINE_TEXT_OPACITY` 0.75 / `_HOVER` 0.95 | — | подписи времени | домножить на альфу `TEXT` 0.97 (итог ~0.73/0.92) либо заменить целиком |

### 2.2 Геометрия внутри функций (не константы, но «магические числа» для хит-тестов)

| Файл:строка | Место | Значение | Комментарий |
|---|---|---|---|
| `selection.rs:211` | `FRAC_1_SQRT_2` в `rotate_handle_rects` | 1/√2 | не визуал, математика диагонали — не трогать |
| `overlay_manager.rs:693` | `PIN_FLASH_THICKNESS_DIP = 1.5 * HIGHLIGHT_THICKNESS_DIP` | 4.5 | производная — пересчитается сама при смене `HIGHLIGHT_THICKNESS_DIP`, но тест на 4.5 может быть (проверить) |
| `overlay_manager.rs:1214` | `CHECKERBOARD_CELL_DIP = 16.0` | 16.0 | клетка шахматки, не в наших файлах; упомянуть при перекраске шахматки |

---

## 3. Рамки / бевель / объём старого стиля (замена на glass_panel / glass_control §7)

Прямых вызовов `settings_bevel` / `settings_frame` / `settings_frame_radius` /
`accent_outline` в четырёх файлах **нет** (все они в `widgets.rs`, surface A).
Но старый «объёмный» язык тут воспроизводится плоскими заливками — это и есть
точки замены:

| № | Место | Что сейчас | Что заменит |
|---|---|---|---|
| 1 | `selection.rs:159-171` (`outline_rects`), `235-238` (`visuals`) | рамка выделения — 4 плоские полосы 2 DIP, цвет из вызывающего слоя | рамка как стеклянная кромка: `Primitive::Glass` с `Surface::ControlHover`-подобным свечением либо остаётся `Fill` + белый `STROKE_STRONG`; выбор влияет на кэш растра (поворот, риск №10) |
| 2 | `selection.rs:175-189` (`handle_rects`) + вызывающий слой 13779 | 8 белых плоских квадратов 10×10 | `glass_control` с радиусом `RADIUS_TIGHT` 7 (мелкая иконка-кнопка) |
| 3 | `selection.rs:201-230` (`rotate_handle_rects`) + вызывающий слой 13784-13795 | 4 квадрата 22×22 с `Icon::Rotate` | `glass_control` + `Icon::Rotate` (иконки в этот заход не переделываются, §8) |
| 4 | `window_highlight.rs:163-172` (`primitives`), `187-201` (`primitives_custom`) | 4 плоских `Primitive::Fill` 3 DIP | рамка пина — белая обводка (решение по §2); «стекло» для рамки вокруг ЧУЖОГО окна не требуется (это не панель) — вероятно остаётся `Fill` с новым цветом |
| 5 | `video_timeline.rs:263-320` (`draw`) | композиция 5 плоских Fill: подложка → дорожка → заливка → ручка → подписи | это единственное в surface I настоящее «объёмное» место старого стиля (жёлоб + ручка, как у `Slider` в widgets.rs:1098/1133): подложка/дорожка → утопленный жёлоб `Surface::Sunken` (`SUNKEN_BG`, тёмная кромка сверху, §4), заливка — белый `TEXT`, ручка — `glass_control` (`RADIUS_CTRL` 10), при наведении/драге — `hover_t`/`press_t` §5 |

---

## 4. Анимация наведения/нажатия (§5)

### 4.1 Нужна (курсор реально наводится)

| № | Что | Файл:строка | Какая фаза | Комментарий |
|---|---|---|---|---|
| 1 | 8 ручек ресайза | `selection.rs:175-189` (+ зоны `resolve_zone` в overlay_manager.rs:6331) | hover 160 мс + press 110 мс (ресайз — зажатие) | сейчас статичные белые квадраты; §5 «продавливание»: scale 0.972/0.955, `CTRL_BG → CTRL_BG_HOVER → CTRL_BG_ACTIVE` |
| 2 | 4 ручки поворота | `selection.rs:201-230` (зоны overlay_manager.rs:6339) | hover + press (захват вращения) | то же, что ручки ресайза |
| 3 | полоса видео целиком | `video_timeline.rs:322-328` (`set_hovered`), `330-350` (`pointer_event` Down/Move/Up) | hover 160 мс (подложка 0.5→0.85, дорожка 2→3, тексты 0.75→0.95 — сейчас скачком), press 110 мс (драг ручки) | состояние hovered/dragging уже есть; не хватает плавности: нужны `hover_t`/`press_t`, `Widget::animate` (§7) и тик `UiTick` (surface J) |
| 4 | рамка выделения | `selection.rs:20`, `159-171` | появление/уход выделения | кандидат на появление 240 мс (`opacity 0→1`, сдвиг +8→0, scale 0.985→1) — опционально; сама рамка под курсором не «наводится» |

### 4.2 Не нужна (статика, без курсоравой зоны)

| № | Что | Файл:строка | Почему |
|---|---|---|---|
| 1 | шахматка скрытых стикеров | `selection.rs:276-290` | статичный узор поверх замороженного кадра |
| 2 | затемнение режима редактирования | `selection.rs:254-262` | фон, не контрол |
| 3 | марка протяжки | `marquee.rs:43-78` | транзиентный жест (движется за курсором самим жестом); hover-фазы не бывает |
| 4 | рамка пина (`HighlightKind::Pin`) | `window_highlight.rs:50`, `163-172` | постоянная обводка, курсор не влияет |
| 5 | подсветка окна `HighlightKind::Hover` | `window_highlight.rs:52`, `163-172` | сама является следствием наведения (режим выбора окна); наводить на неё нечего — она уже «реакция» |
| 6 | подписи времени | `video_timeline.rs:298-319` | текст, реагирует только opacity вместе с полосой (часть п. 4.1.3) |
| 7 | ручки поворота и ресайза в покое | — | рамка без курсора — статична (анимация только в 4.1.1/4.1.2) |

Важно: при анимации любой из п. 4.1 координатор обязан держать тик ~60 Гц
(`UiTick`), иначе анимация замрёт на середине (§5, «Важно про перерисовку») —
это работа surface J, закладывать поля `hover_t`/`press_t` в виджеты нужно
сейчас.

---

## 5. Тесты, привязанные к числам (сломаются при переходе на §2/§3)

### selection.rs (`mod tests` :356-603)

| Тест | Строки | Что зашито | Что пересчитать |
|---|---|---|---|
| `outline_rects_follow_edges` | 415-457 | толщина 2.0 в аргументе и в ожиданиях | при `OUTLINE_THICKNESS_DIP` 2→1 — поменять аргументы и ожидаемые `h` |
| `handle_rects_in_clockwise_order` | 460-481 | `w == h == 10.0` | при смене `HANDLE_SIZE_DIP` |
| `visuals_bundle_outline_and_handles` | 484-490 | `== OUTLINE_THICKNESS_DIP`, `== HANDLE_SIZE_DIP` | константно-относительный — сломается только при смене констант |
| `all_rects_flattens_outline_then_handles` | 493-506 | `rects[0].h == OUTLINE_THICKNESS_DIP` | то же |
| `checkerboard_tile_two_pixel_cells` | 546-560 | точные пиксели `[255,0,255,255]` / `[0,0,0,255]` | при смене `CHECKER_MAGENTA` на белый — пересчитать половину ожиданий |
| `checkerboard_tile_single_pixel_cells` | 563-571 | весь вектор из 255/0-пикселей | то же |
| `checkerboard_tile_non_square` | 574-588 | пиксели фуксии/чёрного | то же |
| `checkerboard_hlsl_has_both_entry_points` | 597-602 | строки `"magenta"`, `"black"` | при правке/удалении HLSL-константы |
| `edit_overlay_covers_full_screen_at_half_dim` | 530-543 | `EDIT_OVERLAY_OPACITY == 0.5` | только если меняется прозрачность |
| `corners_*`, `handle_center_*`, `from_sticker_*` | 369-412, 509-527 | чистая математика поворота | не ломаются |

### marquee.rs (`mod tests` :98-207)

| Тест | Строки | Что зашито | Что пересчитать |
|---|---|---|---|
| `dashes_step_and_partial_last` | 110-129 | шаг dash+gap = 10, длины 6, центры 103/113/120.5 | при смене `MARQUEE_DASH_DIP`/`MARQUEE_GAP_DIP` |
| `right_down_drag_geometry` | 132-157 | число штрихов 10+10+6+6, позиции 13.0/103.0/23.0, `MARQUEE_THICKNESS_DIP` | при смене любой из трёх констант пунктира |
| `reverse_drag_normalizes`, `degenerate_*`, `nan_inputs_*` | 160-207 | логика, не числа | не ломаются |

### window_highlight.rs (`mod tests` :204-452)

| Тест | Строки | Что зашито | Что пересчитать |
|---|---|---|---|
| `default_thickness_matches_dip_contract` | 326-329 | `outline_rects(HIGHLIGHT_THICKNESS_DIP)[0].h == 3.0` | при смене `HIGHLIGHT_THICKNESS_DIP` |
| `highlight_kind_colors_are_distinct_enums` | 332-348 | `Pin == [0x3c,0x98,0x98]`, `Hover == [0x4f,0x9c,0xff]` | **сломается сразу** при перекраске в белый; переписать на токены §2 |
| `primitives_emit_fills_with_kind_style` | 351-375 | сравнение с `kind.color()`/`opacity()` | константно-относительный — переживёт перекраску, сломается при смене структуры (не-Fill) |
| `primitives_custom_emits_fills_with_exact_color_and_opacity` | 391-413 | фикстура `[0x3c,0x98,0x98]`, opacity 0.37 | фикстуру обновить на белый; 0.37 — произвольное число, ок |
| `primitives_custom_clamps_opacity_to_unit_range` | 416-440 | фикстуры `[0x3c,0x98,0x98]` | то же |
| `primitives_custom_matches_solid_sprite_contract` | 443-452 | фикстура цвета | то же |
| `outline_rects_*` | 210-317 | явные толщины 2.0/3.0 | ломаются только если поменяется сам метод; если `HIGHLIGHT_THICKNESS_DIP` станет ≠3 — обновить 326-329 |

### video_timeline.rs (`mod tests` :406-701)

| Тест | Строки | Что зашито | Что пересчитать |
|---|---|---|---|
| `zero_duration_never_uses_division` | 558-582 | `out.len() == 5` (ровно 5 примитивов) | **сломается структурно**: стеклянный жёлоб/ручка = больше слоёв (§4). Пересчитать число примитивов |
| `hover_changes_visuals_and_returns_redraw_flag` | 667-701 | `out[0]` — подложка с `TIMELINE_BG_OPACITY`, `out[1]` — дорожка с `TIMELINE_TRACK_H` | **сломается структурно** при glass_control (индексы/типы примитивов изменятся); константы — пересчитать, если меняются значения |
| `timeline_bounds_sits_at_bottom_of_sticker` | 601-615 | `TIMELINE_MARGIN`/`TIMELINE_HEIGHT` в формуле | константно-относительный — только при смене констант |
| `timeline_bounds_rotated_sticker_keeps_bottom_of_its_frame` | 618-633 | то же | то же |
| `timeline_bounds_rejects_tiny_stickers` | 636-643 | `TIMELINE_MIN_WIDTH + 2*TIMELINE_MARGIN` | то же |
| `x_maps_to_seconds_*`, `click_at_edges_*`, `pointer_beyond_*`, `drag_emits_*`, `set_position_*`, `format_time_*`, `rotated_timeline_*` | 451-597, 646-664 | логика маппинга/драга | не ломаются (математика, не константы вида) |

---

## 6. Риски и странности

1. **`[u8;3]` без альфы vs токены §2 «цвет+альфа».** Все цвета в четырёх файлах
   — триплеты; альфа живёт отдельными константами (`opacity()` 0.9/0.65,
   `TIMELINE_*_OPACITY`, `MARQUEE_*_OPACITY`). Механически «заменить hex на hex»
   не получится: нужно решить, как складываются константа-альфа и альфа токена
   §2 (домножать или заменять). `theme::TEXT` к тому же `[0xf0,0xf0,0xf0]`, а не
   чистый белый.
2. **Сцепление цвета полосы видео с `HighlightKind::Pin`.** `video_timeline.rs:281,287`
   берут цвет заливки и ручки из `HighlightKind::Pin.color()` — при перекраске/
   удалении этого enum полоса молча сменит цвет (или не соберётся). Полосе нужен
   собственный токен.
3. **Шахматка скрытых стикеров.** Функциональный узор «фуксия/чёрный»; фуксия —
   не палитра §2, но и `DANGER` тут не годится (запрещено «заливать площади»).
   Белая/чёрная шахматка при 0.5 поверх светлого кадра потеряет контраст —
   потребуется поднять `HIDDEN_STICKER_CHECKERBOARD_OPACITY` и/или поменять
   пропорцию клеток. Решение за дизайном, а не механикой.
4. **Цвет марки/рамки задаёт вызывающий слой.** `marquee.rs` вообще не содержит
   цвета; `SELECTION_COLOR` потребляют `overlay_manager.rs:13738/13773` и
   `ui_preview.rs:338`. Перекраска в `selection.rs` без surface J даст
   рассинхрон (превью останется бирюзовым).
5. **Пульс пина красится литералом вне surface I.** `PIN_FLASH_COLOR_PIN`
   `[0x3c,0x98,0x98]` и `PIN_FLASH_COLOR_UNPIN` `[0xd0,0x3c,0x3c]` —
   `overlay_manager.rs:682/687`; `PINNED_OUTLINE_OPACITY` 0.55 :660. Рамка пина
   станет белой, а пульс останется бирюзовым — будет выглядеть как «два разных
   состояния». Запланировать J.
6. **Литералы внутри HLSL-строки** (`selection.rs:316-317`) не находят ни grep
   по `[0x..]`, ни компилятор — мёртвый шейдер `CHECKERBOARD_HLSL` останется
   бирюзово-фуксийным и «сломает» правило §9.1 формально. Либо обновить, либо
   удалить константу.
7. **Альфа-константы не описаны §2.** `MARQUEE_FILL/STROKE_OPACITY`,
   `HIGHLIGHT opacity()`, `TIMELINE_*_OPACITY`, `EDIT_OVERLAY_OPACITY`,
   `HIDDEN_STICKER_CHECKERBOARD_OPACITY` — старая схема «непрозрачность по месту».
   Спека даёт альфу только в токенах; нужен единый принцип для оверлеев
   (например: оверлеи поверх чужих окон остаются на своих альфах, контролы —
   на токенах).
8. **Ручки уже белые, рамка станет белой — потеряется различимость.**
   Сейчас ручки ресайза белые намеренно (selection.rs:43-45 — «иначе читались
   бы как утолщения линии»). После перекраски рамки в белый они сольются:
   понадобится различать их гало (`HOVER_GLOW`), размером или толщиной кромки.
9. **Хит-тесты зависят от геометрии.** `ROTATE_HANDLE_SIZE_DIP`/`GAP_DIP`/
   `HANDLE_SIZE_DIP` участвуют в `resolve_zone` (overlay_manager.rs:6331, 6339-6341,
   6447) и в тестах overlay_manager.rs:17616+ (ожидания 22/18). Любая смена
   размеров ручек меняет зоны захвата — синхронизировать с J.
10. **Повёрнутые стеклянные примитивы.** Рамка выделения, ручки поворота и
    полоса видео несут `Box2D.rotation`; кэш растра §7 ключуется по
    (surface, w, h, radius, glow, DPI) — поворот должен жить в transform, а не
    в растре. Тонкие повёрнутые ленты (рамка 2 DIP) стеклянным растром рисовать
    неэффективно — вероятно, рамки остаются `Fill` + белый цвет, стекло
    получают только панельные поверхности (ручки, полоса).
11. **`window_highlight` сознательно не вводил новый `Primitive`**
    (доккомент :20-22): исчерпывающий match в `overlay_manager` сломается при
    добавлении `Primitive::Glass` — это известно и запланировано, но любой
    примитив из этих файлов обязан пройти через `primitives_to_sprites` J.
12. **`timeline_bounds` на повёрнутом стикере** (video_timeline.rs:368-391) —
    геометрия уже правильная (локальная система); при изменении
    `TIMELINE_MARGIN`/`HEIGHT` проверять тесты 601-643.
13. **`theme::settings` / `WidgetStyle::Settings` в surface I отсутствуют** —
    удаление модуля (widgets.rs:295-345) безопасно для этих файлов; единственная
    связь — `lib.rs:70` реэкспорт `settings_frame` и `theme` (surface A).

---

## Итоговая карта правок по файлам

| Файл | Цвета (§2) | Геометрия (§3) | Стекло (§7) | Анимация (§5) | Тесты |
|---|---|---|---|---|---|
| `selection.rs` | 46, 49, 52, 316-317 | 20, 23, 28, 34 | ручки → `glass_control`, рамка → белая обводка | ручки ресайза/поворота: hover+press | 415, 460, 484, 493, 546-602 |
| `marquee.rs` | — (док 25) | 13 (уже 1.0), 15, 17 | нет (Fill остаются) | нет | 110, 132 |
| `window_highlight.rs` | 50, 52 | 29 | остаётся `Fill` (рамка чужого окна) | нет | 326, 332, 391-452 |
| `video_timeline.rs` | 270, 275, 281, 287, 306, 317 | 44-67 | draw() 268-319 → Sunken-жёлоб + `glass_control` | hover 160 мс / press 110 мс + `UiTick` | 558, 667 (структурно), 601-643 (условно) |

Зависимости от surface J (`overlay_manager.rs`): цвета марки/рамки (13738,
13773), пульс пина (682, 687, 693, 13878, 13920), `primitives_to_sprites` для
`Primitive::Glass`, `UiTick`, тесты геометрии хит-тестов (17616+).

---

## Статус после переписывания (задача I, 2026-08-29)

Модули переписаны; `cargo fmt` + `cargo clippy -- -D warnings` +
`cargo test -p rst-render` (210 тестов) — зелёные. `resticker` (поверхность J
и другие агенты) в этом срезе собирается не полностью — ошибки в чужих
файлах (`group_manager.rs`, `preset_strip.rs`: не обновлены импорты
`glass_card`/`glass_on`/`Label`/`WidgetStyle`), к нашим модулям отношения не
имеют.

### Что заменено

| Файл | Было | Стало |
|---|---|---|
| `selection.rs` | `OUTLINE_THICKNESS_DIP = 2.0`; шахматка фуксия/чёрная | `= theme::HAIRLINE` (1.0, §3); шахматка белая/чёрная (`CHECKER_MAGENTA = [0xff,0xff,0xff]`, имя legacy — экспортируется lib.rs); HLSL-константа `magenta` → `white` |
| `marquee.rs` | `MARQUEE_THICKNESS_DIP = 1.0`; док-коммент про `theme::SLIDER_FILL` | `= theme::HAIRLINE`; док-коммент: цвет задаёт вызывающий слой — белый (§2.4) |
| `window_highlight.rs` | `Pin`/`Hover` = cyan/синий литералы | `Pin` → `glass::STROKE_STRONG_RGB`, `Hover` → `glass::STROKE_RGB`; сила света — через `opacity()` 0.9/0.65 |
| `video_timeline.rs` | 5 плоских `Fill`: подложка `LOCK_INDICATOR_BG`, дорожка `SLIDER_TRACK`, заливка/ручка `HighlightKind::Pin.color()` | жёлоб `glass_sunken` (RADIUS_CTRL); подсветка наведения — слой `Surface::ControlHover` с силой фазы; дорожка — внутренний `glass_sunken` (RADIUS_TIGHT); заливка проигранного — белая `theme::TEXT`; ручка — `glass_control(primary=true)` (диск 10×10 при RADIUS_CTRL); подписи `theme::TEXT` с `TEXT_DIM_OPACITY`/`TEXT_OPACITY` по фазе |

### Анимация §5 (video_timeline)

`hover_phase`/`press_phase` (`ui_motion::Phase`, `theme::HOVER_MS`/`PRESS_MS`),
`Widget::animate` продвигает; `set_hovered` и `Down`/`Up` ставят цели. Толщина
дорожки 2→3, свет жёлоба и непрозрачность подписей интерполируются
`lerp` по `max(hover, press)` — драг за краем полосы подсветку не гасит
(прежний контракт `active()`). Координатору J: вызывать `animate` у
`VideoTimeline` и тикать `UiTick`, пока `true`.

### Тесты, пересчитанные осмысленно

- `checkerboard_tile_*` (3 шт.) — пиксели `[255,0,255,255]` → `[255,255,255,255]`.
- `checkerboard_hlsl_has_both_entry_points` — `"magenta"` → `"white"`.
- `highlight_kind_colors_are_distinct_enums` → `highlight_kind_colors_are_monochrome_and_opacities_distinct`: оба белые, различаются непрозрачностью.
- `primitives_custom_*` — фикстуры `[0x3c,0x98,0x98]` → `glass::STROKE_STRONG_RGB`.
- `hover_changes_visuals_and_returns_redraw_flag` → `hover_animates_glass_visuals_and_returns_redraw_flag`: жёлоб — `Glass{Surface::Sunken}`, свет появляется только после продвижения фазы `animate(HOVER_MS)`.
- `zero_duration_never_uses_division` — та же структура (5 примитивов), добавлена проверка первого слоя `Glass{Sunken}`.

### Осталось на поверхность J (overlay_manager.rs)

1. **Цвет рамки выделения и марки**: `fill_texture(renderer, rst_render::SELECTION_COLOR)` (13922, 13957) — `SELECTION_COLOR` остался бирюзовым намеренно; заменить передаваемый цвет на белый (например `theme::TEXT`/`glass::STROKE_STRONG_RGB`) и убрать/перекрасить саму константу.
2. **Пульс пина/обводка**: `PIN_FLASH_COLOR_PIN [0x3c,0x98,0x98]` (682), `PIN_FLASH_COLOR_UNPIN` (687), `PINNED_OUTLINE_OPACITY` (660) — белый свет; DANGER остаётся только у UNPIN-пульса (§2.4).
3. **Ручки ресайза/поворота**: сейчас белые плоские квадраты через `solid_sprite`/`Icon::Rotate`; по §7 их место — `glass_control` с `RADIUS_TIGHT`, фазы hover/press ведёт J через `UiTick`.
4. **`UiTick` для `VideoTimeline::animate`** — см. выше; без него hover замрёт на середине (§5).
5. **Визуальная проверка**: рамка выделения стала волосинкой (1 DIP); если тонко — вернуть толщину отдельной константой.
6. `ui_preview.rs:338` использует `SELECTION_COLOR` — перекрасить вместе с п. 1.