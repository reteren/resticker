# Разведка G — инвентаризация под Dark Liquid Glass

Модули: `crates/resticker/src/group_manager.rs` (478 строк), `crates/resticker/src/window_pick_list.rs` (636 строк).
Спецификация: `docs/DESIGN_LIQUID_GLASS.md` (далее — §N).

Код не правился. Чужие файлы не тронуты. Ниже — полная карта переписывания
обоих модулей: цвета, геометрия, старые рамки, анимации, тесты, риски.

---

## 0. Карта модулей: что вообще есть

| Модуль | Панель | Стиль сейчас | Кто переписывает (по §8) |
|---|---|---|---|
| `group_manager.rs` | `PANEL_ID 910`, 380×H, список групп + состав + 3 кнопки | `WidgetStyle::Settings` целиком (панель и все кнопки) | G (OpenCode) |
| `window_pick_list.rs` | `PANEL_ID 500`, 320×H, список окон + скролл | дефолт `WidgetStyle::Overlay` (тёмная схема) | H (Antigravity) — расхождение, см. §6.8 |

Оба модуля импортируют `truncate_to_width` из `window_picker.rs`
(`group_manager.rs:22`, `window_pick_list.rs:30`) — файл поверхности H.
После переписывания G останется зависимость от чужих файлов.

---

## 1. Цвета: все места

### 1.1 Литералы `[0x.., 0x.., 0x..]` в модулях (запрещены §2 и §9.1)

| Файл:строка | Что красит | Токен §2 |
|---|---|---|
| `group_manager.rs:91` | `DANGER_TEXT: [u8; 3] = [0xff, 0xb0, 0xb0]` — подпись кнопки удаления группы | `DANGER` (§2.4). Внимание: существовали ТРИ разных красных (см. §6.1) |
| `window_pick_list.rs` | литералов нет | — |

### 1.2 Косвенные цвета: что рисуется за вызовами модулей

Модули сами почти не называют цветов — цвета лежат в `rst-render/src/widgets.rs`
(поверхность A). Ниже — что именно эти вызовы тянут за собой.

**`group_manager.rs` — всё через `WidgetStyle::Settings`:**

| Строка вызова | Что красится | Текущий токен (widgets.rs) | Токен §2 |
|---|---|---|---|
| `:127` `.with_style(WidgetStyle::Settings)` на Panel | фон панели | `theme::settings::BG` `[0x76,0x76,0x76]` @ `BG_OPACITY 0.82` (widgets.rs:297,334; рисует `settings_frame_radius`, widgets.rs:2581) | тело `GLASS_INK` @ `0.62` (панель, не трей/диалог → §2.1) |
| `:128` `.with_corner_radius(theme::settings::CORNER_RADIUS)` | скругление корпуса | `8.0` (widgets.rs:344) | `RADIUS_WINDOW` 18 (§3) |
| `:181` `.with_style(WidgetStyle::Settings)` на строке группы | фон строки | `BTN_BG` / `BTN_BG_HOVER` (widgets.rs:303,304, ветка `Button::draw` widgets.rs:860-866) | `CTRL_BG` / `CTRL_BG_HOVER` |
| `:195` `.with_style(WidgetStyle::Settings)` на кнопке «x» | фон кнопки удаления | `BTN_BG`/`BTN_BG_HOVER`/`BTN_BG_ARMED` (widgets.rs:860-866) | `CTRL_BG*` |
| `:196` `.with_label_color(DANGER_TEXT)` | подпись «x» | `[0xff,0xb0,0xb0]` | `DANGER` |
| `:243` `.with_style(WidgetStyle::Settings)` на «New group» | фон | `BTN_BG*` | `CTRL_BG*` |
| `:262` `.with_style(WidgetStyle::Settings)` на «Edit windows» | фон | `BTN_BG*` | `CTRL_BG*` |
| `:278` `.with_style(WidgetStyle::Settings)` на «Close» | фон | `BTN_BG*` | `CTRL_BG*` |
| подписи кнопок (ветка `Button::draw`, widgets.rs:911-915) | текст кнопок | `theme::settings::TEXT` `[0xff,0xff,0xff]` | `TEXT` |
| нажатая кнопка (`settings_bevel` при `armed`, widgets.rs:875) | грани | `BORDER_LIGHT`/`BORDER_DARK` | переезжает на `glass_control` (§5 продавливание) |

**ВЕСЬ модуль `theme::settings` удаляется (§7):** из `group_manager.rs` он
задействован через `CORNER_RADIUS` (`:128`) и опосредованно — через
`WidgetStyle::Settings` (BTN_BG, BTN_BG_HOVER, TEXT, BEVEL, BG, BORDER_LIGHT,
BORDER_DARK, BG_OPACITY, DANGER). После правки в модуле не должно остаться ни
одного упоминания `WidgetStyle::Settings` и `theme::settings`.

**`window_pick_list.rs` — дефолтный Overlay-стиль:**

| Строка вызова | Что красится | Текущий токен | Токен §2 |
|---|---|---|---|
| `:194` `Primitive::Fill` цвет `theme::BUTTON_BG` | плейсхолдер-квадрат иконки окна | `[0x3a,0x3a,0x41]` (widgets.rs:220) | слот → `SUNKEN_BG` (утопленный жёлоб, §2.2) — кандидат; альтернатива `CTRL_BG`. См. §6.3 |
| `:202` `Primitive::Text` цвет `theme::TEXT` | подпись строки | `[0xf0,0xf0,0xf0]` (widgets.rs:238) | `TEXT` @ 0.97 |
| фон панели (`Panel::draw` дефолт, widgets.rs:2587-2600) | тело панели + рамка | `PANEL_BORDER` `[0x55,0x55,0x5e]` + `PANEL_BG` `[0x2b,0x2b,0x30]` @ `PANEL_BG_OPACITY 0.92` (widgets.rs:214-218) | тело `GLASS_INK` @ 0.62, внешняя `STROKE` |
| фон/ручка скролла (`ScrollBar::draw`, widgets.rs:1281-1303) | дорожка + ручка | `SLIDER_TRACK` `[0x55,0x55,0x5e]`, ручка `SLIDER_FILL` `[0x4f,0x9c,0xff]` — **синий акцент** | `SUNKEN_BG` (дорожка) и `TEXT`-белый разной силы; синий запрещён §2.4. Чинится в rst-render (A), но виден именно в этой панели — см. §6.4 |
| `Label::draw` при `set_dim(true)` (`:241`) | подпись «No windows available» | `theme::TEXT` @ opacity 0.5 (widgets.rs:2446-2447) | `TEXT_DIM` 0.66 или `TEXT_FAINT` 0.42 (решение в rst-render, A) |

**Мягкие подписи `set_dim(true)` в обоих модулях:**
`group_manager.rs:145` (пустой список), `:212` (строки состава);
`window_pick_list.rs:241` (пустой список). Все три — подписи-не-кнопки; на
`TEXT_DIM`/`TEXT_FAINT` (см. §4).

---

## 2. Геометрия: все числа

### 2.1 `group_manager.rs` — локальные константы

| Строка | Константа | Значение | Куда в §3 |
|---|---|---|---|
| `:56` | `WIDTH` | 380.0 | нет токена — ширина панели остаётся (уменьшить нельзя: строки несут номер+счётчик, состав — заголовки чужих окон) |
| `:58` | `PAD` | 12.0 | `PAD_PANEL` 14 (внутренний отступ корпуса) |
| `:60` | `ROW_GAP` | 4.0 | `GAP_ROW` 10 (§3 «между строками»). Следствие: панель растёт на ~54 DIP в пределе (9 строк × +6) — формула `height()` пересчитывается автоматически, тесты самосогласованы (см. §5) |
| `:62` | `SECTION_GAP` | 10.0 | `GAP_ROW` 10 — совпадает 1:1 |
| `:64` | `ROW_H` | 28.0 | `BUTTON_SIZE` 30 (после правки токена в rst-render; сейчас `theme::BUTTON_SIZE = 28`, widgets.rs:241) |
| `:67` | `MEMBER_H` | 20.0 | `LINE_HEIGHT` 15 (строка состава — подпись, не кнопка; габарит текста) — кандидат, см. §6.5 |
| `:69` | `DELETE_W` | 28.0 | `BUTTON_SIZE` 30 |
| `:71` | `MEMBER_INDENT` | 18.0 | нет токена в §3 — кандидат на локальную константу с обоснованием или новый токен. См. §6.6 |
| `:114` | `+ 1.0` (разделитель в `height()`) | 1.0 | `HAIRLINE` 1.0 — совпадает |
| `:179` | `row_w - 2.0 * theme::BUTTON_PAD` | 2×4 = 8 | `PAD_CTRL_X`-семейство: `2 × PAD_CTRL_X` (12) — подпись строки не должна упираться в край кнопки |
| `:230`, `:249`, `:265` | `2.0 * theme::FIELD_PAD + 12.0` в ширине кнопок New/Edit/Close | 2×4+12 = 20 | `2 × PAD_CTRL_X` (2×12 = 24). Магическое `12.0` — ошибка по §9.1; кнопки станут шире на 4 DIP, ряд действий (new + edit + close) должен поместиться: 356 DIP контентной ширины хватает, см. §6.7 |

### 2.2 `window_pick_list.rs` — локальные константы

| Строка | Константа | Значение | Куда в §3 |
|---|---|---|---|
| `:54` | `WIDTH` | 320.0 | нет токена — остаётся |
| `:56` | `PAD` | 6.0 | `PAD_PANEL` 14 — контентная ширина падает с 301 до 285 DIP, строки ужимаются. Кандидат на проверку: список — компактная панель, а не корпус; §6.9 |
| `:58` | `ROW_GAP` | 4.0 | `GAP_ROW` 10 (как в §2.1) |
| `:62` | `ICON_SIZE` | 20.0 | нет токена в §3 — остаётся (тест `row_without_icon_draws_placeholder_fill` завязан на 20, §5) |
| `:65` | `ICON_GAP` | 8.0 | нет токена — кандидат на локальную константу, §6.6 |
| `:74` | `height()`: `2*PAD + rows*BUTTON_SIZE + (rows-1)*ROW_GAP` | — | формула переписывается на новые токены целиком |
| `:232`, `:233`, `:298` | `theme::SCROLLBAR_WIDTH` (3.0) | 3.0 | токен в rst-render; при стеклянной панели полоса скролла — волосинка (см. §6.4) |
| `:248`, `:260`, `:294` | `theme::BUTTON_SIZE` | 28.0 | `BUTTON_SIZE` 30 |

### 2.3 Высоты строк и кнопок — сводно

| Модуль | Что | Было | Стало (§3) | Эффект |
|---|---|---|---|---|
| group_manager | строка группы, кнопки действий | ROW_H 28 | BUTTON_SIZE 30 | панель выше на ~20 DIP в пределе |
| group_manager | строка состава | MEMBER_H 20 | LINE_HEIGHT 15 | панель ниже на ~40 DIP в пределе (8×5) — частично компенсирует рост строк |
| window_pick_list | строка | BUTTON_SIZE 28 | 30 | 10 строк × 2 = +20 DIP |

---

## 3. Рамки/бевель/объём старого стиля → `glass_panel`/`glass_control`

Сами функции `settings_bevel` (widgets.rs:368), `settings_frame` (:581),
`settings_frame_radius` (:402), `accent_outline` (:595) и растр углов
`settings_corner_rgba` (:505) живут в rst-render (поверхность A) и удаляются
там. В МОИХ модулях старый стиль включается так:

| Файл:строка | Вызов | Что рисует сейчас | Замена (§7) |
|---|---|---|---|
| `group_manager.rs:127-128` | `with_style(Settings)` + `with_corner_radius(CORNER_RADIUS)` | `settings_frame_radius` → тело BG + бевель-грани + 4 растра углов | `glass_panel(out, frame, RADIUS_WINDOW, opacity)` |
| `group_manager.rs:181,195,243,262,278` | `with_style(Settings)` на каждой кнопке | `Fill BTN_BG` + `settings_bevel` (поднятая/вдавленная грань при armed) | `glass_control(out, rect, RADIUS_CTRL, hover_t, press_t, primary, opacity)` — см. §4 |
| `window_pick_list.rs:254-264` | `Button::new` без стиля | `Fill BUTTON_BG*` + hover/armed (widgets.rs:860-866) без бевеля | `glass_control` для каждой строки |
| `window_pick_list.rs:227` | `Panel::new` без стиля | два `Fill` (PANEL_BORDER поверх + PANEL_BG внутри), widgets.rs:2587-2600 | `glass_panel` с `RADIUS_CTRL` или `RADIUS_CARD` (список — узкая панель; решение — см. §6.9) |

Дополнительно в `group_manager.rs`:
- `:225` `Divider::new(...)` — разделитель, сейчас `Fill PANEL_BORDER @ 0.6`
  (widgets.rs:2909-2914). В стекле это `RIM_BOTTOM`-подобная кромка или
  `STROKE`-волосинка; толщина 1.0 = `HAIRLINE`. Решение в rst-render (A).

**Итого вызовов старых рамок (по модулям G):** 6 мест в group_manager
(1 панель + 5 кнопок), 1 место в window_pick_list (панель; строки — без бевеля,
но с hover-заливками). Плюс `Divider` — 1.

---

## 4. Анимации наведения/нажатия (§5)

### Где НУЖНА (`glass_control` с фазами hover/press; нужен UiTick-тик §5 «Важно про перерисовку»)

| Модуль | Виджет | id | Строки | Замечание |
|---|---|---|---|---|
| group_manager | строка группы | `ROW_BASE + i` (911+) | `:169-182` | клик раскрывает группу |
| group_manager | кнопка удаления «x» | `DELETE_BASE + i` (940+) | `:183-197` | |
| group_manager | «New group» | `BTN_NEW` 976 | `:231-244` | |
| group_manager | «Edit windows» | `BTN_EDIT` 977 | `:250-263` | |
| group_manager | «Close» | `BTN_CLOSE` 972 | `:266-279` | |
| window_pick_list | строка списка | `ROW_BASE + i` (501+) | `:254-264` | виртуализирована: фазы по индексу строки, при скролле виджеты пересоздаются — фазы сбрасываются, анимация не «зависает» на ушедшей строке |

### Где НЕ нужна (статика)

| Модуль | Виджет | id | Строки |
|---|---|---|---|
| group_manager | заголовок «Window groups» | `ID_TITLE` 973 | `:134-135` |
| group_manager | подпись пустого списка | `ID_EMPTY` 974 | `:139-146` |
| group_manager | строки состава (метки) | `MEMBER_BASE + m` (980+) | `:206-213` |
| group_manager | разделитель | `ID_DIVIDER` 975 | `:225` |
| window_pick_list | плейсхолдер «No windows available» | `EMPTY_LABEL_ID` 598 | `:240-242` |
| window_pick_list | иконка+подпись строки (`RowContent`) | `id + LABEL_FLAG` | `:165-215, :267-285` — не интерактивен (`hit_test` → false, `:178`) |
| window_pick_list | полоса скролла | `SCROLLBAR_ID` 599 | `:295-307` — не интерактивна (`hit_test` → false, widgets.rs:1307) |

Внимание: в window_pick_list hover-состояние держит `Button` (`:254`), а визуальный
контент рисует `RowContent` поверх. При стеклянных фазах кнопка сама ужимается
(`scale`), а `RowContent` (иконка+текст) остаётся в старых координатах — либо
`RowContent` должен следовать за ужатой кнопкой, либо контент вшивается в
`glass_control`-фазу. См. §6.10.

---

## 5. Тесты, привязанные к геометрии

### `group_manager.rs` (строки 336-477)

| Тест | Строки | Привязан к числам? | Что делать |
|---|---|---|---|
| `empty_list_still_builds_a_usable_panel` | :336-344 | нет (наличие виджетов) | без изменений |
| `every_group_gets_its_own_row_and_delete_button` | :346-363 | нет | без изменений |
| `a_collapsed_group_does_not_show_its_windows` | :365-373 | нет | без изменений |
| `an_expanded_group_lists_every_window_inside_it` | :375-385 | нет | без изменений |
| `nine_groups_with_a_full_one_expanded_still_fit_the_panel` | :387-403 | ДА: `assert_inside` с допуском ±0.5 против рамки из `height()` | самосогласован (рамка считается той же формулой), но **пересчитать**: при PAD 12→14, ROW_H 28→30, ROW_GAP 4→10 формула `height()` должна совпадать с раскладкой билдера; тест ловит расхождение |
| `expanding_a_group_pushes_the_rows_below_it_down` | :405-424 | нет (только порядок cy) | без изменений |
| `the_row_label_reads_as_words_not_as_a_row_of_numbers` | :426-446 | нет (текст примитивов) | без изменений |
| `a_degenerate_frame_does_not_panic` | :448-460 | нет | без изменений |
| `a_group_with_more_windows_than_the_table_allows_is_clamped` | :462-477 | ДА: `assert_inside` | как `nine_groups_...`: пересчитать после смены токенов |

**Вывод:** ломаются только два теста с `assert_inside` — и только если формула
`height()` и раскладка разъедутся. По §9.4 чинить осмысленно: держать оба места
на одних константах.

### `window_pick_list.rs` (строки 360-635)

| Тест | Строки | Привязан к числам? | Что делать |
|---|---|---|---|
| `empty_snapshot_builds_no_rows` | :360-366 | нет | без изменений |
| `empty_snapshot_shows_placeholder_message` | :370-375 | нет | без изменений |
| `nonempty_snapshot_has_no_placeholder_message` | :378-384 | нет | без изменений |
| `rows_sorted_by_z_order_top_first` | :386-399 | нет | без изменений |
| `empty_title_falls_back_to_exe_file_name` | :401-407 | нет | без изменений |
| `empty_title_and_path_falls_back_to_placeholder` | :409-415 | нет | без изменений |
| `scroll_skips_rows_from_top_and_keeps_original_ids` | :417-430 | нет (тексты строк) | без изменений |
| `scrollbar_appears_only_when_list_overflows_visible_rows` | :434-455 | нет | без изменений |
| `visible_rows_capped_even_with_more_windows` | :457-466 | нет | без изменений |
| `long_title_is_truncated_to_row_width` | :468-483 | частично: многоточие; порог зависит от ширины строки | пересчитать порог, если `max_text_w` изменится (PAD 6→14, BUTTON_SIZE 28→30, SCROLLBAR_WIDTH) |
| `row_without_icon_draws_placeholder_fill` | :488-502 | ДА: `rect.w == ICON_SIZE && rect.h == ICON_SIZE` (20) | пересчитать только если `ICON_SIZE` тронется (по §2.1 — не трогается) |
| `row_with_icon_draws_rgba_primitive` | :505-531 | ДА, но на размер растра иконки (16×16, из WindowIcon), не на геометрию панели | без изменений |
| `height_grows_with_visible_count_but_caps_at_visible_rows` | :533-546 | монотонность + кламп, не абсолюты | без изменений (значения станут другими, инварианты те же) |
| `eligible_snapshot_removes_denylisted_windows` | :548-581 | нет (логика фильтра) | без изменений |
| `eligible_snapshot_matches_process_by_file_name_or_path` | :583-600 | нет | без изменений |
| `eligible_snapshot_removes_iconic_windows` | :602-613 | нет | без изменений |
| `rows_stack_top_to_bottom_inside_frame` | :615-635 | частично: «в рамке» против рамки из `height()` | самосогласован — пересчитать не нужно, но проверить при новых токенах |

**Вывод:** в window_pick_list тесты почти не ломаются (все геометрии считаются
через `height()`). Реально пересчитать: `long_title_is_truncated_to_row_width`
(если изменится ширина текста) и не трогать `ICON_SIZE`, чтобы
`row_without_icon_draws_placeholder_fill` остался зелёным.

---

## 6. Риски и странности

1. **Три разных красных.** `group_manager.rs:91` `[0xff,0xb0,0xb0]`,
   `theme::settings::DANGER` `[0xd0,0x3c,0x3c]` (widgets.rs:328), новый
   `DANGER #D0463C`. Привести все к одному `theme::DANGER`. Подпись «x» —
   тонкий текстовый глиф, под §2.4 проходит (заливать площади запрещено —
   здесь не заливка).

2. **`WidgetStyle::Settings` удаляется целиком (§7).** В `group_manager.rs` он
   в 6 местах (`:127,:181,:195,:243,:262,:278`) + `theme::settings::CORNER_RADIUS`
   (`:128`). Удаление переживёт не только модуль: комментарий к
   `settings_corner_rgba` завязан на радиус 8 (widgets.rs:490-491: «48 px хватает
   и на 200% DPI — радиус 8 DIP = 16 px»), а при `RADIUS_WINDOW` 18 нужно 36 px
   на 200% — растр 48 px формально хватает, но это дело поверхности A, не моих
   модулей. Мои модули просто перестают вызывать старый путь.

3. **Плейсхолдер иконки (`window_pick_list.rs:194`)** — сейчас `BUTTON_BG`
   (цвет кнопки). Если строки станут `glass_control` (CTRL_BG), плейсхолдер на
   том же фоне исчезнет — пустой слот читается только тенью. Лучше `SUNKEN_BG`
   (утопленный жёлоб под иконку) — это и есть «слот». Тест
   `row_without_icon_draws_placeholder_fill` ловит только размер (20×20), не
   цвет — не сломается.

4. **Скролл — синий акцент.** Ручка `ScrollBar` рисуется `SLIDER_FILL`
   `[0x4f,0x9c,0xff]` (widgets.rs:1300) — синий, запрещённый §2.4. Чинится в
   rst-render (поверхность A), но `window_pick_list` — единственная из моих
   панелей, которая его видит. Координатору: не забыть скроллбар в дорожке A.

5. **`MEMBER_H` 20 → `LINE_HEIGHT` 15** — строки состава станут плотнее
   (8 строк = −40 DIP). Альтернатива: оставить «дышащую» высоту как
   `LINE_HEIGHT + GAP_ROW`-производную. Решение за переписывающим, но оба
   варианта без магических чисел.

6. **Нет токена в §3 для** `MEMBER_INDENT` (18) и `ICON_GAP` (8). Оба — кандидаты
   на локальные именованные константы с доккоментом (формально §9.1 запрещает
   «магические числа», но именованная константа с русским пояснением — не
   магическое число; либо запросить токен у координатора).

7. **Ширина кнопок действий** (group_manager `:230,249,265`): `text_size + 2×4 + 12`
   — три разных «отступа» в одной формуле. После перехода на `2×PAD_CTRL_X`
   кнопки станут шире на 4 DIP каждая. Ряд New + Edit + Close на 356 DIP
   помещается, но «Edit» появляется только при раскрытой группе — проверять
   влезание при обоих состояниях (теста на это нет).

8. **Расхождение с §8.** По §8 `group_manager.rs` — G, но `window_pick_list.rs`
   — поверхность H (Antigravity), а в G входит `preset_picker.rs`, который мне
   НЕ выдан. Если переписывать «всю G», нужен ещё инвентарь `preset_picker.rs`;
   сейчас отчёт покрывает group_manager + window_pick_list. Плюс оба модуля
   импортируют `truncate_to_width` из `window_picker.rs` (тоже H) — после
   правки H функция может переехать/измениться, зависимость G→H останется.

9. **`PAD` 6 → `PAD_PANEL` 14 в window_pick_list** — контентная ширина падает
   301→285 DIP. Панель 320 DIP — не «большой корпус» (§3 определяет `PAD_PANEL`
   для корпуса). Варианты: честно 14 (строки уже, заголовки режутся раньше)
   или новый токен «компактный отступ». Тот же вопрос с радиусом корпуса:
   `RADIUS_CARD` 14 или `RADIUS_CTRL` 10 — решить до правки.

10. **Контент строки не ужимается.** В window_pick_list `RowContent` (иконка +
    текст) рисуется по координатам, посчитанным от `row_left`/`text_left`
    (`:234-237`), а не от bounds кнопки. При продавливании (`scale 0.972/0.955`)
    кнопка ужмётся вокруг центра, а текст останется — «кнопка сжалась, надпись
    висит». Варианты: передавать фазы в `RowContent` и ужимать вместе, либо
    рисовать текст как часть стеклянного примитива. Аналогично в
    group_manager: `ButtonContent::Label` центрируется сам (widgets.rs:901-916)
    — там контент внутри кнопки, проблема не возникает.

11. **UiTick-тик (§5).** `group_manager` и `window_pick_list` — панели с
    кнопками; анимация hover/нажатия требует планируемого тика ~60 Гц, пока
    фазы не достигли 0/1. Механизм — дорожка J (overlay_manager), но фазы
    виджетов должны продвигаться через `Widget::animate` (§7). Моим модулям
    нужно держать `hover_t`/`press_t` по кнопкам и не забыть: 160 мс hover,
    110 мс press (не локальные магические числа — через хелпер в theme, §5).

12. **`+ 1.0` в `height()` (group_manager:114)** — высота разделителя зашита
    формулой. Если `Divider` станет `HAIRLINE`-кромкой (1.0) — совпадает, но
    лучше вынести в константу, чтобы смена толщины не разъезжалась с формулой.

13. **`truncate_to_width` и `EMPTY_LABEL`/`TITLE_LABEL`** — подписи остаются
    латинскими; §6 требует Commissioner + Bitcount Grid Single для номеров
    групп. Номер группы в подписи «Group 3 — 4 windows» (`:158-168`) —
    кандидат на Bitcount-акцент, но сейчас это один `String` в одной надписи:
    разбиение «цифра другой гарнитурой» потребует либо двух Label, либо
    поддержки в растризаторе (дорога A). Пометить, не делать.

14. **Внимание на `Label::draw` (widgets.rs:2442-2448):** `set_dim` = opacity 0.5.
    После токенов §2.3 подписи-«dim» должны брать `TEXT_DIM`/`TEXT_FAINT`, а не
    половинную непрозрачность `TEXT`. Это поверхность A, но оба модуля зовут
    `set_dim(true)` в трёх местах (см. §1.2).

---

## 7. Порядок переписывания (сводно для исполнителя)

1. Дождаться дорожки A (`Primitive::Glass`, `glass_panel`, `glass_control`,
   `theme::*` токены, `Widget::animate`, UiTick) — §7 «до его готовности панели
   не переписываются».
2. `group_manager.rs`: удалить `WidgetStyle::Settings` и `theme::settings` (6
   мест), панель → `glass_panel(RADIUS_WINDOW)`, кнопки → `glass_control`
   (RADIUS_CTRL), `DANGER_TEXT` → `theme::DANGER`, геометрия по §2.1, фазы
   hover/press, пересчитать `height()` и 2 теста `assert_inside`.
3. `window_pick_list.rs`: панель → `glass_panel`, строки → `glass_control`
   (10 строк, фазы по индексу), плейсхолдер иконки → `SUNKEN_BG`, геометрия по
   §2.2, контент строки решить по §6.10.
4. Проверить влезание кнопок действий (§6.7) и «Edit»-состояние.
5. После правки: `cargo fmt`, `cargo clippy -- -D warnings`,
   `cargo test -p resticker` (окружение по §9.5; процесс `resticker.exe`
   остановлен). Тесты геометрии чинить осмысленно (§9.4).
6. Ничего не коммитить (§9.7).