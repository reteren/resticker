# Инвентаризация F: `group_strip.rs` + `preset_strip.rs` под Dark Liquid Glass

Разведка выполнена 2026-08-29 по `docs/DESIGN_LIQUID_GLASS.md` (§2–§7).
Правки кода не вносились. Номера строк — на момент разведки, точны.

**Контекст, который надо знать при переписывании:** модуль `theme::settings`
(`rst-render/src/widgets.rs:295–345`) удаляется целиком, `WidgetStyle::Settings`
удаляется (§7 спецификации). Ссылки на него — это точки компиляционного
разрыва. Примитив `Primitive` получает вариант `Glass` (§7) — это точки
компиляционного разрыва в тестах, где match по `Primitive` исчерпывающий.

---

## 1. Все места, где задан цвет

### 1.1 `theme::settings::*` — модуль удаляется целиком (компиляционный разрыв)

| Файл:строка | Что красит | Куда переходит (§2) |
|---|---|---|
| group_strip:403 | Рамка «окно выбрано» (`picked_outline_edges`, 4 полосы) | `STROKE_STRONG` — обводка активного контрола, белый свет 0.30 |
| group_strip:416 | Заливка бейджа номера слота | Тело бейджа `CTRL_BG_ON` (включённый маркер), обводка — `STROKE_STRONG` |
| group_strip:430 | Цифра слота на бейдже | `TEXT` |
| group_strip:445 | Подпись карточки (заголовок окна) | `TEXT` |
| group_strip:608–612 | Фон `ConfirmButton`: `BTN_BG_HOVER` / `BTN_BG` | `CTRL_BG_HOVER` / `CTRL_BG` |
| group_strip:628 | Два штриха галочки подтверждения | `TEXT` |
| group_strip:649 | Грани `bevel()`: `BORDER_LIGHT` / `BORDER_DARK` | **Удаляется** — бевель заменяет `glass_control` (§7), функция `bevel()` (648–671) выпиливается |
| group_strip:651 | Толщина грани бевеля `BEVEL` | Удаляется вместе с `bevel()` |
| group_strip:478 | Толщина рамки выбора `BEVEL` в `picked_outline_edges` | `HAIRLINE` (1.0 DIP) |
| preset_strip:230 | Толщина рамок `BEVEL` в `outline_edges` | `HAIRLINE` (1.0 DIP) |
| preset_strip:447 | Подложка миниатюры «экран» `INPUT_BG` | `SUNKEN_BG` — утопленная поверхность (§4: утопленная поверхность переворачивает свет) |
| preset_strip:454 | Акцентная рамка выбранной миниатюры `ACCENT` | `STROKE_STRONG` |
| preset_strip:519 | Красное кольцо невлезающей раскладки `DANGER` | Новый `theme::DANGER` `#D0463C` (§2.4). **Внимание:** старый `settings::DANGER` = `[0xd0,0x3c,0x3c]`, новый — `(0xd0,0x46,0x3c)` — другой токен, не переносить значение! |

### 1.2 `theme::*` (не settings) — компиляцию переживают, но визуально обязаны смениться

Спецификация (§1, строки 5–7) выводит из употребления и тёмно-серую схему
`theme::PANEL_BG` — эти токены компилироваться продолжат, перекраска
механически НЕ сработает, их надо менять руками.

| Файл:строка | Что красит | Текущее значение | Куда переходит (§2) |
|---|---|---|---|
| group_strip:393 | Плейсхолдер-квадрат снимка окна (карточка без снимка и иконки) | `BUTTON_BG` `[0x3a,0x3a,0x41]` | `SUNKEN_BG` — пустой жёлоб под снимок |
| group_strip:734 | Дорожка горизонтального скроллбара (opacity 0.5) | `SLIDER_TRACK` `[0x55,0x55,0x5e]` | `SUNKEN_BG` (дорожка-жёлоб) |
| group_strip:748 | Ручка скроллбара (opacity 0.9) | `SLIDER_FILL` `[0x4f,0x9c,0xff]` — **синий акцент `#4F9CFF`, запрещён §2.4** | `TEXT` с opacity ~0.9 (белый свет); жёстко убрать синий |
| preset_strip — | `NumericField` внутренне красится своими токенами (rst-render, дорожка A) | — | Отдельная задача координатора; здесь только `.with_style(Settings)` на строке 404 |

Литералов `[0x.., 0x.., 0x..]` в этих двух файлах **нет** — все цвета идут
через `theme::*`, что упрощает перекраску, но создаёт иллюзию «заменил токен —
готово» для 1.2.

---

## 2. Все геометрические числа

### 2.1 `group_strip.rs`

| Строка | Константа | Значение | Переход (§3) |
|---|---|---|---|
| 107 | `SCREEN_MARGIN` | 20.0 | Токена в §3 нет (отступ от края экрана). Оставить как есть — решение локальное |
| 109 | `STRIP_PAD` | 8.0 | `PAD_PANEL` = 14 |
| 111 | `CARD_W` | 132.0 | Токена нет (ширина карточки). Оставить; скругление карточки → `RADIUS_CARD` = 14 |
| 113 | `CARD_H` | 96.0 | Токена нет. Оставить |
| 115 | `CARD_GAP` | 8.0 | `GAP_ROW` = 10 |
| 117 | `CARD_PAD` | 4.0 | Кандидат `PAD_CTRL_X` = 12 (внутренний отступ содержимого); увеличит `THUMB_H`-зависимости — см. тесты |
| 120 | `THUMB_H` | производная: `CARD_H − 2·CARD_PAD − LINE_HEIGHT − CARD_PAD` | Пересчитается сам при смене `CARD_PAD`/`LINE_HEIGHT` (символьная формула) |
| 122 | `BADGE_SIZE` | 18.0 | Токена нет (сторона бейджа). Оставить; скругление бейджа → `RADIUS_TIGHT` = 7 |
| 124 | `BADGE_GAP` | 4.0 | Токена нет. Оставить (или `GAP_ROW` на усмотрение дизайна) |
| 126 | `CHECK_THICKNESS` | 2.0 | Токена нет — это штрих глифа галочки, §8 (иконки) не трогаем. Оставить |
| 129 | `CONFIRM_SIZE` | `theme::BUTTON_SIZE` = **28** | **Автоматически станет 30** — §3 меняет `BUTTON_SIZE` на 30; размер обеих краевых кнопок вырастет на 2 DIP. Проверить `visible_cards_match_the_strip_geometry` |
| 210–211 | Радиус углов панели | `settings::CORNER_RADIUS` = 8.0 | `RADIUS_WINDOW` = 18 (корпус большой панели; исключение тулбара к лентам не относится — оно про узкий тулбар стикера) |
| 302 | Позиция скроллбара | `cy = frame.cy + h/2 − STRIP_PAD/2` | Символьная — пересчитается сама |
| 304 | Высота полосы скролла | `theme::SCROLLBAR_WIDTH` = 3.0 | Токена нет. Оставить тонкой |
| 738 | Мин. ширина ручки скролла | `theme::SCROLLBAR_MIN_THUMB_H` = 16.0 | Токена нет. Оставить |
| 439 | Вертикаль подписи карточки | `bottom − CARD_PAD − LINE_HEIGHT/2` | Символьная. `LINE_HEIGHT` (15.0) может смениться на дорожке A (§6 — новая гарнитура) — здесь правок не требуется |

### 2.2 `preset_strip.rs`

| Строка | Константа | Значение | Переход (§3) |
|---|---|---|---|
| 101 | `WARN_RING_INSET` | 2.0 | Токена нет (отступ красного кольца от акцентного). Оставить |
| 121 | `ADAPTIVE_CAPTION_GAP` | 4.0 | Токена нет. Оставить |
| 124 | `THUMB_SIZE` | 72.0 | Токена нет (размер миниатюры; сжат `thumb_size`, 188–200). Оставить; скругление миниатюры → `RADIUS_CARD` = 14 |
| 126 | `THUMB_GAP` | 8.0 | `GAP_ROW` = 10 |
| 129 | `SLOT_GAP` | 1.0 | `HAIRLINE` = 1.0 — совпадает один-в-один |
| 131 | `PAD` | 12.0 | `PAD_PANEL` = 14 |
| 133 | `STRIP_GAP` | 14.0 | `GAP_ROW` = 10 |
| 135 | `LABEL_GAP` | 8.0 | `GAP_ROW` = 10 |
| 137 | `TOP_GAP` | 16.0 | Токена нет (отступ от верха экрана). Оставить |
| 139 | `FIELD_W` | 56.0 | Токена нет. Оставить |
| 188–200 | `thumb_size()` | `((screen.w − fixed) / count).clamp(0.0, THUMB_SIZE)` | Символьная — пересчитается сама при смене PAD/GAP |
| 207–224 | `unit_to_box()` | зазор `SLOT_GAP` | Символьная |
| 313–314 | Радиус углов панели | `settings::CORNER_RADIUS` = 8.0 | `RADIUS_WINDOW` = 18 |
| 345 | Слоты миниатюр (кнопки) | — | Скругление слотов → `RADIUS_CTRL` = 10 |
| 504 | Кольцо: `inner_w = frame.w − 2·inset` | — | Символьная |

---

## 3. Места, где рисуется рамка/бевель/объём старого стиля (замена на glass_panel/glass_control)

| Файл:строка | Вызов/конструкция | Замена (§7) |
|---|---|---|
| group_strip:209–211 | `Panel::new(...).with_style(WidgetStyle::Settings).with_corner_radius(theme::settings::CORNER_RADIUS)` | `glass_panel(out, frame, RADIUS_WINDOW, GLASS_INK.opacity=0.62)`; `Panel::with_style` убирается |
| group_strip:247–254 | `Button::new(card).with_style(WidgetStyle::Settings)` — карточка-подложка (фон + hover/armed) | `glass_control(rect, RADIUS_CARD, hover_t, press_t, primary=false)` — «карточка внутри панели» |
| group_strip:268–281 | `Button::new(BTN_MANAGER, ...).with_style(WidgetStyle::Settings)` | `glass_control(rect, RADIUS_TIGHT, ...)` — мелкая иконка-кнопка |
| group_strip:283–293 | `ConfirmButton::new(...)` — свой виджет | Рисование заменить на `glass_control(primary=true)` (`CTRL_BG_PRIMARY`) в `ConfirmButton::draw` |
| group_strip:604–632 | `ConfirmButton::draw`: `Primitive::Fill` + `bevel(...)` (618) + штрихи | Весь блок → `glass_control`; штрихи галочки (`checkmark_strokes`) поверх, цвет `TEXT` |
| group_strip:648–671 | `fn bevel()` — локальная копия `settings_bevel` (светлая грань сверху/слева, тёмная снизу/справа, `raised` переворачивает) | **Удаляется целиком** |
| group_strip:400–406 | `picked_outline_edges` — 4 полосы `ACCENT` (accent_outline) | Выбранное состояние: обводка `STROKE_STRONG` (или `Surface::ControlOn` на выбор); четыре `Fill` заменить на обводку/состояние |
| group_strip:407–418 | Бейдж слота — квадрат `ACCENT` | Маленький стеклянный бейдж: тело `CTRL_BG_ON`, скругление `RADIUS_TIGHT`, обводка `STROKE_STRONG` |
| preset_strip:312–314 | `Panel::new(...).with_style(WidgetStyle::Settings).with_corner_radius(...)` | `glass_panel(out, frame, RADIUS_WINDOW, ...)` |
| preset_strip:331–338 | `Button::new(THUMB_BASE+i).with_style(WidgetStyle::Settings)` — подложка миниатюры | `glass_control(rect, RADIUS_CARD, hover_t, press_t, ...)` |
| preset_strip:346–353 | `Button::new(SLOT_BASE+...).with_style(WidgetStyle::Settings)` — слоты-кнопки | `glass_control(rect, RADIUS_CTRL, ...)` |
| preset_strip:404 | `NumericField::snap_gap(...).with_style(WidgetStyle::Settings)` | Дождаться дорожки A (NumericField переводит координатор); `.with_style(Settings)` убрать |
| preset_strip:450–458 | `outline_edges` — 4 полосы `ACCENT` у выбранной миниатюры | Обводка `STROKE_STRONG` (или `Surface::ControlOn`) |
| preset_strip:516–522 | `OverflowRing::draw` — `outline_edges` цветом `DANGER` | Остаётся тонким кольцом, но цвет — новый `theme::DANGER`; толщина `HAIRLINE` |
| preset_strip:229–246 | `fn outline_edges()` (общая для рамки выбора и кольца) | Толщина `HAIRLINE`, цвет — параметром |

---

## 4. Анимация наведения/нажатия (§5)

### Нужна (курсор наводится, 160 мс hover / 110 мс press)

| Файл:строка | Виджет | Примечание |
|---|---|---|
| group_strip:247–254 | Карточки ленты (Button `CARD_BASE+…`) | Hover/арм у кнопки-подложки; содержимое (`CardContent`) поверх не трогаем |
| group_strip:268–281 | Кнопка `BTN_MANAGER` | Обычная иконка-кнопка тулбара |
| group_strip:283–293 | `ConfirmButton` (`BTN_CONFIRM`) | **Свой виджет — надо добавить поля `hover_t`/`press_t` и `animate()`** (§7: `Widget::animate`). Неактивное состояние (`disabled`, opacity 0.45) — решить, как выглядит в стекле (пока не зажигается гало, фон `CTRL_BG`, без нажатия) |
| preset_strip:331–338 | Кнопки-подложки миниатюр (`THUMB_BASE+…`) | Hover миниатюры зажигает гало всей карточки |
| preset_strip:346–353 | Кнопки слотов (`SLOT_BASE+…`) | Ховер отдельных слотов |
| preset_strip:403–406 | `NumericField` | Фокус/ховер поля — часть rst-render (дорожка A); гало `HOVER_GLOW` по §2.3 |

### Не нужна (статика)

| Файл:строка | Виджет | Причина |
|---|---|---|
| group_strip:334–458 | `CardContent` (подпись, снимок, плейсхолдер, рамка выбора, бейдж, цифра) | `hit_test` — false (347–349), интерактив несёт Button под ним |
| group_strip:677–760 | `HScrollBar` | Неинтерактивна (720–722), скролл — колесо мыши на вызывающем слое |
| preset_strip:419–468 | `ThumbBackdrop` (подложка + рамка выбора) | `hit_test` — false |
| preset_strip:478–532 | `OverflowRing` | `hit_test` — false |
| preset_strip:397–402, 384–389 | `Label` «Gap %», «adaptive» | Неинтерактивны |
| group_strip:107 / preset_strip:137 | Отступы от краёв экрана | Не виджеты |

**Замечание §5:** пока хоть один виджет лент анимируется, координатор обязан
планировать `UiTick` ~60 Гц; ленты сейчас «статичные» строители — фазы
придётся хранить в виджетах и продвигать через `Widget::animate`.

---

## 5. Тесты, привязанные к конкретным числам геометрии / цветам

### Сломаются жёстко (пересчитывать)

| Файл:строка | Тест | Что пересчитать |
|---|---|---|
| group_strip:1126–1141 | `visible_cards_match_the_strip_geometry` | Жёстко `visible_cards == 10` при 40 карточках и 1600×900. Ломается от `CARD_GAP` 8→10, `CONFIRM_SIZE` 28→30, `STRIP_PAD` 8→14 |
| group_strip:1160–1196 | `thumbnail_is_letterboxed_into_the_card_slot` | Жёстко «Слот 124×69»: `rect.w == 124.0`, `rect.h == 62.0` — производные от `CARD_W`/`CARD_PAD`. Пересчитать при смене `CARD_PAD` (4→12) |
| group_strip:1220–1242 | `picked_card_draws_accent_outline_and_badge` | Считает заливки по `theme::settings::ACCENT`, жёстко `== 5` (4 грани + бейдж). Акцента не будет — переписать на обводку/состояние (`STROKE_STRONG`), число примитивов изменится |
| preset_strip:714–732 | `reported_slot_rects_match_the_drawn_slot_fills` | Ищет заливки цвета `theme::settings::BTN_BG` и сравнивает прямоугольники с `slots`. Кнопки станут `Primitive::Glass` — переписать на сопоставление Glass-примитивов |
| preset_strip:835–869 | `selected_layout_draws_an_accent_ring_and_others_do_not` | Считает `fills(ACCENT) == 4` и раскладывает по ширине/высоте. Переписать под обводку `STROKE_STRONG` |
| preset_strip:1059–1071 | `an_overflowing_layout_draws_a_danger_ring_only_on_that_thumbnail` | `fills(DANGER) == outline_edges(...)` — сравнение прямоугольников заливок. Переписать (новый токен DANGER, возможно Glass-примитив) |
| preset_strip:1076–1102 | `a_selected_layout_that_overflows_draws_accent_and_danger_rings_together` | То же: цветовые `fills` + `WARN_RING_INSET` |
| preset_strip:1107–1134 | `an_empty_verdict_slice_marks_nothing` | Счёт по цветам `DANGER`/`ACCENT` — переписать |
| preset_strip:1164–1183 | `a_shorter_or_longer_verdict_slice_marks_only_what_it_covers` | `fills(DANGER).len() == presets.len() * 4` — переписать |
| preset_strip:1294–1351 | `adaptive_card_still_selects_and_draws_both_rings` | Цветовые `fills` — переписать |

### Сломаются при добавлении `Primitive::Glass` (компиляция, не числа)

| Файл:строка | Тест | Причина |
|---|---|---|
| group_strip:880–905 | `cards_never_leave_the_screen_at_any_count_and_scroll` | `match &prim { Fill | Icon | Rgba | Text => … }` (889–894) исчерпывающий — добавление варианта `Glass` даёт compile error. Добавить руку `Glass` |

### Граничные (проверить вручную, скорее всего устоят)

| Файл:строка | Тест | Причина |
|---|---|---|
| group_strip:1108–1122 | `scrollbar_appears_only_when_cards_overflow_the_strip` | «20 карточек не влезают» — при росте `CONFIRM_SIZE`+2 и `STRIP_PAD`+6 влезает меньше, тест устоит, но пересчёт не помешает |
| group_strip:1200–1216 | `card_without_image_draws_a_placeholder_slot` | Ищет `Fill` с размерами `CARD_W−2·CARD_PAD`/`THUMB_H` — символьный по константам, но сломается, если плейсхолдер станет не `Fill`, а `Glass` (SUNKEN_BG) |

### Не сломаются (символьные или логические)

`group_strip`: `the_window_strip_is_no_wider_than_its_cards_need` (794),
`a_long_window_list_stops_at_the_screen_edge` (811), `an_empty_window_list_still_builds…` (828),
`every_card_stays_inside_the_strip` (837), `slot_digit_is_drawn_on_the_picked_card` (909),
`the_groups_list_button_sits_at_the_left_edge…` (956), `confirm_button_disabled_until_two…` (994),
`confirm_click_registers_only_when_enabled` (1020), `degenerate_screen_builds_without_panicking` (1090),
`confirm_button_sits_at_the_strip_right_edge` (1271), `strip_sits_at_the_bottom_of_the_screen` (1292),
`empty_strip_shows_only_a_disabled_confirm_button` (1305), `long_title_is_truncated_to_the_card_width` (1145),
`checkmark_strokes_are_two_rotated_segments…` (1246).

`preset_strip`: `any_number_of_layouts_fit_any_screen…` (623), `slot_rects_stay_inside_their_thumbnail…` (678),
`zero_and_one_picked_windows…` (738), `an_empty_layout_slice_yields_no_thumbnails…` (761 — формула символьная),
`degenerate_screen_builds_without_panic` (779), `strip_hangs_at_the_top…` (801), `full_size_strip_keeps_thumbnails…` (811),
`every_slot_shows_its_number` (871), `three_layouts_are_drawn…` (889), `slot_numbers_follow…` (911),
`clicking_a_slot_registers…` (948), `clicking_an_empty_spot…` (990), `gap_field_reports…` (1020),
`verdicts_do_not_break_a_strip…` (1140), `an_overflowing_thumbnail_still_accepts_clicks` (1188),
`adaptive_label_appears_once…` (1236), `adaptive_label_sits_under…` (1259), `adaptive_caption_keeps…` (1356),
`an_empty_slice_with_adaptive_flag…` (1381).

---

## 6. Риски и странности

1. **Тихое «выживание» старых цветов (главный риск).** `theme::BUTTON_BG`
   (group_strip:393), `theme::SLIDER_TRACK`/`SLIDER_FILL` (734/748) живут вне
   `theme::settings` — компиляция не упадёт, а синий акцент `#4F9CFF` в ручке
   скроллбара останется, пока не перекрасить руками. Спецификация §1 их выводит
   из употребления, но механически ничего не сломается.
2. **Новый DANGER ≠ старый.** `settings::DANGER` = `#D03C3C`, спец. `DANGER` =
   `#D0463C`. При переносе значения копировать из спецификации, а не из старого
   кода.
3. **`ConfirmButton` — единственный виджет с собственным рисованием.** У него
   нет `with_style(Settings)` — он сам красится (fill + bevel + галочка). При
   удалении `settings::*` не упадёт на компиляции (кроме цветов) — его надо
   переписывать вручную на `glass_control(primary)` + фазы `hover_t`/`press_t`
   + `animate()`. `disabled` (opacity 0.45) — придумать стеклянный аналог.
4. **Смена `BUTTON_SIZE` 28→30 дёргает геометрию ленты** (group_strip:129,
   `CONFIRM_SIZE`): обе краевые кнопки растут, `visible_cards` меняется —
   см. тест 1126.
5. **Тестовая обвязка `fills(panel, color)`** (preset_strip:595–608) построена
   на сравнении `[u8;3]` заливок — после Glass-примитивов хелпер бесполезен,
   его надо заменить на извлечение `Primitive::Glass { surface, .. }` по
   поверхности, а не по цвету.
6. **Исчерпывающий match по `Primitive` в тесте** group_strip:889–894 —
   единственная компиляционная мина в тестах; в основном коде лент match-ей по
   `Primitive` нет (только `out.push`).
7. **Цифры слотов/бейджей.** Кегль и шрифт (Bitcount Grid Single, 15 DIP,
   §6) запекаются на дорожке A в `rst_render::text` — вызывающие модули
   отступов не меняют (§6). Ленты сами ничего не правят, но бейдж слота
   (group_strip:407–432) — единственное место, где цифра на цветной подложке:
   при `CTRL_BG_ON` контраст белой цифры надо проверить.
8. **Прокрутка ленты карточек остаётся «нестеклянной»** — `HScrollBar` —
   локальный виджет (не `rst_render::ScrollBar`); дорожка `SUNKEN_BG`, ручка
   белая — спецификация §3 её не покрывает, токены для неё взять из §2.
9. **Токенов §3 не хватает** на `SCREEN_MARGIN` (20), `TOP_GAP` (16),
   `BADGE_GAP` (4), `BADGE_SIZE` (18), `THUMB_SIZE` (72), `FIELD_W` (56),
   `WARN_RING_INSET` (2), `ADAPTIVE_CAPTION_GAP` (4), `CHECK_THICKNESS` (2),
   `SCROLLBAR_WIDTH` (3) — они остаются локальными константами модулей; правило
   §9.1 («никаких магических чисел») на них не распространяется, но и в
   `theme` их переносить некуда.
10. **`WidgetStyle::Settings` — точки компиляционного разрыва**: group_strip
    210, 253, 280; preset_strip 313, 337, 352, 404. Модули не соберутся, пока
    дорожка A не уберёт вариант — порядок работ: A → F.
11. **Слоты-кнопки внутри миниатюры** (preset_strip:346–353) получат ховер-
    анимацию каждая в отдельности; по §1.3 «контрол продавливается телом» —
    решить, анимируется ли вся миниатюра целиком (гало по карточке) или
    каждый слот отдельно. Архитектурно сейчас ховер несёт `Button` под
    `ThumbBackdrop`, который перекрывает всю миниатюру и ховер карточки уже
    держит (331–338).