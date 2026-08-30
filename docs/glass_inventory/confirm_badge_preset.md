# Инвентаризация E — confirm_dialog.rs, monitor_badge.rs, preset_picker.rs (Dark Liquid Glass)

Поверхность E (§8 таблицы спецификации): модал подтверждения, бейдж монитора.
Поверхность G: preset_picker (по §8 закреплён за OpenCode, но задание включает его).
Отчёт сделан по файлам:

- `crates/resticker/src/confirm_dialog.rs` (367 стр.)
- `crates/resticker/src/monitor_badge.rs` (315 стр.)
- `crates/resticker/src/preset_picker.rs` (385 стр.)

Опорные точки rst-render (поверхность A, координатор): `widgets.rs:295-345` (модуль
`theme::settings`), `widgets.rs:368-391` (`settings_bevel`), `widgets.rs:402-487`
(`settings_frame_radius`), `widgets.rs:581-589` (`settings_frame`), `widgets.rs:595+`
(`accent_outline`), `widgets.rs:2578-2585` (Panel Settings-ветка), `widgets.rs:859-875`
(Button), `widgets.rs:2281-2295` (Checkbox), `widgets.rs:1934-1944` (TextField),
`widgets.rs:2909-2914` (Divider), `text.rs:49` (`LINE_HEIGHT = 15.0`),
`text.rs:38/45` (шрифт уже Commissioner Medium).

---

## 1. Все места, где задан цвет

### 1.1 Литералы `[0x.., 0x.., 0x..]` в самих файлах

| Файл | Строка | Что красит | Переход (токен §2) |
|---|---|---|---|
| confirm_dialog.rs | 66 | `const DANGER_TEXT = [0xff, 0xb0, 0xb0]` — подпись кнопки «Delete» (передаётся в стр. 175 → `with_label_color` стр. 204) | `theme::DANGER` (§2.4, `#D0463C`) |
| preset_picker.rs | 246 | `const DANGER_TEXT = [0xff, 0xb0, 0xb0]` — крестик «x» удаления строки (`with_label_color`, стр. 163) | `theme::DANGER` (§2.4, `#D0463C`) |

Оба локальных литерала — бледно-розовые и НЕ совпадают ни с существующим
`theme::settings::DANGER` (`[0xd0,0x3c,0x3c]`), ни с новым `#D0463C`. После перехода
подпись Delete/крестик станут заметно насыщеннее — это ожидаемо, §2.4.

### 1.2 Обращения к `theme::settings::*` (модуль удаляется целиком — §7)

| Файл | Строка | Что красит | Переход |
|---|---|---|---|
| confirm_dialog.rs | 117 | `theme::settings::CORNER_RADIUS` для панели модала | параметр `radius` в `glass_panel` (§7) = `RADIUS_WINDOW` 18 (§3) |
| confirm_dialog.rs | 365 (тест) | assertion цвета подписи Cancel — `theme::settings::TEXT` | `theme::TEXT` (новый токен §2.3) |
| monitor_badge.rs | 96 | `theme::settings::CORNER_RADIUS` для панели бейджа | `glass_panel(..., RADIUS_TIGHT)` — §3 прямо относит «бейдж» к `RADIUS_TIGHT` 7 |
| monitor_badge.rs | 99 | цвет цифры при `rasterize(&label, theme::settings::TEXT, DIGIT_RASTER_SCALE)` | `theme::TEXT` (§2.3) |
| monitor_badge.rs | 280 (тест) | то же в тесте `badge_number_is_reflected_in_the_digit_bitmap` | `theme::TEXT` |
| preset_picker.rs | 110 | `theme::settings::CORNER_RADIUS` для панели | `glass_panel(..., RADIUS_WINDOW)` 18 |

### 1.3 Косвенные цвета — что по факту рисуют Settings-виджеты (в rst-render, не в файлах, но триггерится ими)

Все три файла вешают на виджеты `WidgetStyle::Settings`; палитра приходит из
`theme::settings` (строки 295-345 rst-render) и красится в rst-render. Ниже — что
именно будет заменено стеклом:

| Что | Виджеты (файл:строки) | Код rst-render | Старый цвет | Новый токен/поверхность |
|---|---|---|---|---|
| Тело панели (модал) | confirm 116-117 | `settings_frame_radius` 410-419 | `BG #767676 @ 0.82` | `Surface::Panel`, `GLASS_INK_DEEP` (§2.1 — модал = диалог) |
| Тело панели (пресеты) | preset 109-110 | то же | то же | `Surface::Panel`, `GLASS_INK` |
| Тело панели (бейдж) | monitor 95-96 | то же | то же | `Surface::Panel`, `GLASS_INK` |
| Фон кнопок (покой/наведение) | confirm 202; preset 148, 162, 206, 225, 239 | `Button::draw` 860-866 | `BTN_BG #7b7b7b` / `BTN_BG_HOVER #8c8c8c` | `glass_control`: `CTRL_BG` / `CTRL_BG_HOVER` (§2.2) |
| Текст кнопок | те же | `Button::draw` 911-915 | `settings::TEXT` `#ffffff` | `theme::TEXT` |
| Текст опасной кнопки/крестика | confirm 175; preset 163 | `with_label_color` | локальный `[0xff,0xb0,0xb0]` | `theme::DANGER` |
| Чекбокс (фон, наведение) | confirm 142 | `Checkbox::draw` 2285-2289 | `CHECK_BG #555555` / `CHECK_BG_HOVER #636363` | `glass_control`, `CTRL_BG` / `CTRL_BG_HOVER`; включён — `CTRL_BG_ON` |
| **Галочка чекбокса** | confirm 142 | `Checkbox::draw` 2342 | `SLIDER_FILL #4F9CFF` — синий акцент, см. риск №2 | белый свет поверх `CTRL_BG_ON` |
| Поле имени (фон) | preset 191 | `TextField::draw` 1936-1940 | `INPUT_BG #5a5a5a` | `Surface::Sunken`, `SUNKEN_BG` (§2.2) |
| Текст поля/каретка | preset 191 | `TextField::draw` 1984; каретка ~1989+ | `TEXT` / `CARET #ffffff` | `theme::TEXT` |
| Разделитель | preset 171 | `Divider::draw` 2910-2914 | `PANEL_BORDER #55555e @ 0.6` | `STROKE` (§2.1) |
| Подпись пустого списка (dim) | preset 129 (`set_dim`) | `Label::draw` 2446-2447 | `TEXT` при `opacity 0.5` | `theme::TEXT_DIM` (§2.3) |
| Цифра бейджа (растр) | monitor 99 | `Primitive::Rgba`, цвет в битмапе | `settings::TEXT` | `theme::TEXT`; новый `rasterize` (§6) сам печёт `TEXT_GLOW` |

Устаревшие токены, которые исчезают с `theme::settings` и здесь не используются
напрямую, но участвуют в отрисовке перечисленного: `BORDER_LIGHT`, `BORDER_DARK`,
`BTN_LIGHT`, `BTN_DARK`, `CHECK_LIGHT`, `CHECK_DARK`, `INPUT_LIGHT`, `INPUT_DARK`,
`ACCENT`, `DANGER`, `BEVEL`, `BG_OPACITY`, `ICON_OFF_OPACITY` (все — `widgets.rs:295-345`).

---

## 2. Все геометрические числа

### 2.1 confirm_dialog.rs

| Строка | Константа/число | Значение | Переход (§3) |
|---|---|---|---|
| 42 | `DIALOG_MIN_W` | 320 | нет токена — оставить |
| 44 | `PAD` | 16 | `PAD_PANEL` 14 |
| 46 | `GAP` (между кнопками) | 8 | `GAP_ROW` 10 (ближайший; токена для зазора между контролами нет) |
| 48 | `ROW_GAP` (между рядами) | 14 | `GAP_ROW` 10 |
| 50 | `BUTTON_H` | 26 | нет токена; пересчитать: подпись 12.5 DIP (§6) + вертикальный запас → ~30 (по образцу `BUTTON_SIZE`) |
| 52 | `BUTTON_PAD_X` | 14 | `PAD_CTRL_X` 12 |
| 54 | `BUTTON_MIN_W` | 76 | нет токена — оставить (это CSS `min-width: 70px`; при `PAD_CTRL_X` 12 ширина кнопок вырастет на 4, проверку наложения держать) |
| 56 | `CHECK_GAP` | 8 | `GAP_ROW` 10 (или оставить) |
| 79 | `button_width()` | `text + 2·BUTTON_PAD_X` | `text + 2·PAD_CTRL_X` |
| 86, 90, 133, 138, 146 | `theme::CHECKBOX_SIZE` | 16 | размер чекбокса §3 не задаёт — оставить 16, скругление `RADIUS_CTRL` 10 |
| 91 | формула высоты | `2·PAD + LINE_HEIGHT + ROW_GAP + check_row_h + ROW_GAP + BUTTON_H` | символическая — пересчитается сама при смене `LINE_HEIGHT` (поверхность A) и токенов выше |
| 124, 134, 152 | позиционирование | через `LINE_HEIGHT` (15, `text.rs:49`) | то же — символически |

### 2.2 monitor_badge.rs

| Строка | Константа/число | Значение | Переход |
|---|---|---|---|
| 49 | `BADGE_SIZE` | 112 | нет токена — оставить (спец-элемент, §3 его не трогает) |
| 51 | `MARGIN` | 16 | нет токена — оставить (отступ от края экрана, не `PAD_PANEL`) |
| 53 | `DIGIT_FRACTION` | 0.7 | оставить |
| 61 | `DIGIT_RASTER_SCALE` | 20 | оставить (уже покрывает DPI до ~3×) |
| 75-77 | `badge_side()` | `BADGE_SIZE.min(max_w).min(max_h)` | формула от `MARGIN`/`BADGE_SIZE` — не меняется |
| 96 | `CORNER_RADIUS` 8 | радиус панели | `RADIUS_TIGHT` 7 (§3: «бейдж») — примечание: бейдж 112 DIP крупный, 7 DIP даст почти квадратные углы; так велит §3 |

### 2.3 preset_picker.rs

| Строка | Константа/число | Значение | Переход |
|---|---|---|---|
| 51 | `WIDTH` | 320 | нет токена — оставить |
| 53 | `PAD` | 12 | `PAD_PANEL` 14 |
| 55 | `ROW_GAP` (между строками списка) | 4 | `GAP_ROW` 10 (прямой хит §3 «между строками») |
| 57 | `SECTION_GAP` | 10 | `GAP_ROW` 10 (совпадает) |
| 59 | `ROW_H` (строки + кнопки действий) | 28 | нет токена; пересчитать → ~30 (как `BUTTON_H`) |
| 61 | `DELETE_W` (сторона крестика) | 28 | `BUTTON_SIZE` 30 (§3: «было 28») — прямой хит |
| 65 | `VISIBLE_ROWS` | 8 | оставить |
| 88-98 | `height()` | `2·PAD + LINE_HEIGHT + SECTION_GAP + rows·ROW_H + (rows−1)·ROW_GAP + SECTION_GAP + 1.0 + SECTION_GAP + ROW_H + ROW_GAP + ROW_H` | константа `1.0` (стр. 94) = толщина дивайдера → `HAIRLINE`; остальное — символически |
| 134-135 | `row_w` | `content_w − DELETE_W − ROW_GAP`; обрезка по `row_w − 2·BUTTON_PAD` | `DELETE_W`→`BUTTON_SIZE`, `BUTTON_PAD`→`PAD_CTRL_X` |
| 174 | `save_w` | `text + 2·FIELD_PAD + 12.0` | **магическая `12.0`** → `PAD_CTRL_X` 12; `FIELD_PAD` → `PAD_CTRL_X` |
| 211-212 | `import_w`/`close_w` | то же | то же |

---

## 3. Рамки / бевель / объём старого стиля (заменяются glass_panel / glass_control из §7)

Прямых вызовов `settings_bevel`/`settings_frame`/`accent_outline` в трёх файлах нет —
они все в rst-render. Но каждый `WidgetStyle::Settings` в файлах включает старый
конвейер. Перечисление вызовов по файлам:

| Файл | Строки | Триггер | Что рисуется старым конвейером (rst-render) |
|---|---|---|---|
| confirm_dialog.rs | 116-117 | `Panel.with_style(Settings).with_corner_radius(CORNER_RADIUS)` | `Panel::draw` 2578-2581 → `settings_frame_radius` 402-487: 2 полосы `BG`, 4 грани `BORDER_LIGHT/DARK`, 4 `Rgba`-угла `SETTINGS_CORNER_KEY` (495) |
| confirm_dialog.rs | 202 | `dialog_button` (Cancel/Delete) | `Button::draw` 859-875: Fill `BTN_BG` + `settings_bevel(!armed)` 368-391 |
| confirm_dialog.rs | 142 | `Checkbox.standard(...).with_style(Settings)` | `Checkbox::draw` 2281-2295: Fill `CHECK_BG` + `settings_bevel(armed)` |
| monitor_badge.rs | 95-96 | `Panel.with_style(Settings).with_corner_radius(...)` | `settings_frame_radius` (тот же) |
| preset_picker.rs | 109-110 | `Panel` — то же | `settings_frame_radius` |
| preset_picker.rs | 148, 162, 206, 225, 239 | 5 кнопок Settings | `Button::draw` — Fill + `settings_bevel` |
| preset_picker.rs | 191 | `TextField.with_style(Settings).keep_on_blur()` | `TextField::draw` 1934-1944: Fill `INPUT_BG` + `settings_bevel(false)` + при фокусе `accent_outline` (595, `ACCENT`) |
| preset_picker.rs | 171 | `Divider::new` | `Divider::draw` 2909-2914: Fill `PANEL_BORDER` (не объём, но старый цвет) |

Замена: все перечисленные `Fill`+бевель → `glass_panel` (панели) и
`glass_control(rect, radius, hover_t, press_t, primary, opacity)` (кнопки/чекбокс/
поле). `with_corner_radius` умирает вместе со стилем `Settings` (работает только для
него, `widgets.rs:2476-2477`) — радиус уходит параметром в `glass_panel`.
`accent_outline` (фокус поля) → `STROKE_STRONG` (§2.1). Поле → `Surface::Sunken`
(перевёрнутый свет, §4).

---

## 4. Где нужна анимация наведения/нажатия (§5), где нет

### Нужна (под курсором, интерактив)

| Файл | Строки | Виджет | Примечание |
|---|---|---|---|
| confirm_dialog.rs | 161-168 | кнопка Cancel | hover 160 мс + press 110 мс |
| confirm_dialog.rs | 169-176 | кнопка Delete | то же; гало `HOVER_GLOW` + `primary` НЕ ставится (это danger, а не primary) |
| confirm_dialog.rs | 135-143 | чекбокс | hover; зажатие — мгновенное, но §5 press-фаза есть и у чекбокса |
| preset_picker.rs | 136-149 | кнопки-строки пресетов | hover/press |
| preset_picker.rs | 150-164 | крестики удаления | hover/press |
| preset_picker.rs | 194-207 | Save | hover/press |
| preset_picker.rs | 213-226 | Import | hover/press |
| preset_picker.rs | 227-240 | Close | hover/press |
| preset_picker.rs | 177-193 | TextField | hover не обязателен (§5 про контролы; поле — Sunken, реагирует фокусом), press-анимации нет; каретка мигает — не §5 |

Итого в трёх файлах 11 интерактивных виджетов; все в confirm_dialog и preset_picker.
Это значит: пока панель открыта и курсор над любым из них, координатору нужен тик
~60 Гц (`UiTick`, §5 «Важно про перерисовку») — см. `overlay_manager.rs` (поверхность J).

### Не нужна (статика)

| Файл | Строки | Что |
|---|---|---|
| confirm_dialog.rs | 125-130 | Label сообщения |
| confirm_dialog.rs | 144-149 | Label подписи тумблера |
| preset_picker.rs | 118 | Label заголовка «Presets» |
| preset_picker.rs | 123-130 | Label пустого списка |
| preset_picker.rs | 171 | Divider |
| monitor_badge.rs | 104-117, 125-169 | BadgeDigit — `hit_test` = false (147-149), панель неинтерактивна целиком |

Бейдж: анимаций нет вообще, `Panel` без виджетов под курсором; `UiTick` для него
никогда не нужен.

---

## 5. Тесты, привязанные к числам геометрии/цветов, и что пересчитать

### confirm_dialog.rs (стр. 209-367)

| Тест | Строки | Привязка | Что сделать |
|---|---|---|---|
| `delete_label_is_red_and_cancel_is_not` | 353-366 | жёстко: `DANGER_TEXT` (364) и `theme::settings::TEXT` (365) | **сломается при компиляции** (модуль удалён); цвет → `theme::DANGER` (#D0463C) и новый `theme::TEXT` |
| `button_labels_fit_inside_their_buttons` | 244-260 | `theme::BUTTON_PAD` (253) | токен меняет смысл (4 → `PAD_CTRL_X` 12); кнопки шире — пересмотреть неравенство; при `BUTTON_MIN_W` 76 держится |
| `buttons_do_not_overlap_and_delete_is_rightmost` | 263-277 | константа `GAP` (273) | пересчитать под новый `GAP` (10); структура не меняется |
| `everything_fits_inside_the_dialog` | 280-304 | match по примитивам без wildcard (289-294) | **добавить arm `Primitive::Glass`** — иначе компиляция падает; проверка «всё внутри модала» остаётся |
| `dialog_grows_with_a_long_message` | 307-313 | `DIALOG_MIN_W`, равенство высот | структурный — пройдёт; высота изменится (PAD/ROW_GAP/BUTTON_H), равенство сохраняется |
| `rows_are_stacked_top_down` | 316-326 | порядок рядов | структурный — пройдёт |
| `message_matches_count`, `checkbox_toggles_on_click`, `click_inside_the_dialog_does_not_reach_the_scene` | 238-241, 329-339, 342-350 | нет чисел | пройдут; `hit_test` панели по frame сохраняется |

### monitor_badge.rs (стр. 171-315)

| Тест | Строки | Привязка | Что сделать |
|---|---|---|---|
| `badge_shrinks_to_fit_small_work_areas` | 287-298 | жёстко `88.0` и `58.0` (292, 297) = f(`BADGE_SIZE` 112, `MARGIN` 16) | пересчитать только если тронут `BADGE_SIZE`/`MARGIN` (не тронуты) |
| `badge_number_is_reflected_in_the_digit_bitmap` | 265-284 | `rasterize(..., theme::settings::TEXT, ...)` (280) | **сломается при компиляции**; цвет → новый `theme::TEXT`; равенство битмапов самосогласовано (тест и модуль зовут одну `rasterize`), пройдёт, но битмап изменится из-за `TEXT_GLOW` (§6) |
| `digit_is_centered_in_the_badge_and_keeps_bitmap_proportions` | 241-262 | `DIGIT_FRACTION` 0.7, пропорции | структурный — пройдёт (пропорции следуют за битмапом) |
| `badge_stays_inside_work_area_on_any_monitor_size` | 198-227 | match по примитивам без wildcard (212-216) | **добавить arm `Glass`** |
| `badge_anchors_to_top_left_of_a_second_monitor_origin` | 230-238 | `MARGIN`, `BADGE_SIZE` | пройдёт (значения не меняются) |
| `degenerate_work_area_builds_without_panic` | 301-314 | нет чисел | пройдёт; проверить, что `glass_panel` с нулевым rect не паникует |

### preset_picker.rs (стр. 248-385)

| Тест | Строки | Привязка | Что сделать |
|---|---|---|---|
| `everything_fits_inside_the_panel` | 344-369 | match без wildcard (355-358) | **добавить arm `Glass`** |
| `long_names_are_truncated_to_the_row` | 322-332 | `row.w` (зависит от `DELETE_W`/`ROW_GAP`/`BUTTON_PAD`) | структурный, но порог усечения меняется с новым `PAD_CTRL_X` — проверить «...» |
| `empty_list_still_offers_...` | 284-297 | только текст | пройдёт |
| `one_row_and_one_delete_per_preset_in_order` | 300-319 | порядок/центровка | пройдёт |
| `name_draft_survives_a_rebuild` | 335-341 | только текст поля | пройдёт |
| `list_is_capped_at_visible_rows` | 372-384 | `height()` равенство | пройдёт (формула символическая) |

Итог: жёстко ломаются при компиляции 4 точки — confirm 365 (`theme::settings::TEXT`),
badge 280 (`theme::settings::TEXT`), и три match по `Primitive` без wildcard
(confirm 289-294, badge 212-216, preset 355-358) — в последних нужно добавить arm
`Primitive::Glass`, иначе enum с новым вариантом не скомпилируется.

---

## 6. Риски и странности

1. **Два «красных» уже рассинхронизированы.** Локальные `[0xff, 0xb0, 0xb0]`
   (confirm 66, preset 246) ≠ `theme::settings::DANGER [0xd0,0x3c,0x3c]` ≠ новый
   `#D0463C`. После перехода подпись Delete и крестик станут заметно насыщеннее —
   механическая перекраска пройдёт, но вид изменится сильнее, чем у остального UI.
2. **Галочка чекбокса — синий акцент, который §2.4 запрещает.** `Checkbox::draw`
   красит галочку `theme::SLIDER_FILL` `#4f9cff` (widgets.rs:2342). Это ровно тот
   «синий акцент», которого «больше нет нигде». Поверхность E это не видит (код в
   rst-render), но модал подтверждения — единственное место в оверлее, где синий
   останется, если rst-render не перепишет Checkbox. Пометка для поверхности A.
3. **`with_corner_radius` умирает вместе со стилем.** Три вызова (confirm 117,
   badge 96, preset 110) — это не «заменить число», это смена API: радиус уходит
   параметром в `glass_panel`. `Panel.corner_radius` работает только при
   `WidgetStyle::Settings` (widgets.rs:2476-2477).
4. **Ключи кэша.** Бейдж опирается на то, что все `Rgba`-примитивы с ключом
   < `BADGE_KEY_BASE` (0xBAD6_2026_0000_0000, стр. 67) — это углы скругления
   (`SETTINGS_CORNER_KEY`, widgets.rs:495). Когда углы исчезнут, фильтр теста
   `digit_prim` (193) останется корректен, НО: новый стеклянный растр (§7)
   кэшируется в `ui_textures` — его ключи не должны попасть в диапазон
   `>= BADGE_KEY_BASE`, иначе тест найдёт не цифру. Пометка для поверхности A.
5. **Прозрачность бейджа.** `BG_OPACITY` 0.82 → `GLASS_INK` 0.62. Бейдж на светлом
   десктопе станет заметно светлее; доккомент (24-31) явно ссылается на «примерно
   тот же серый, что Identify у Windows» — контраст белой цифры ослабнет. Если для
   бейджа нужна плотность — `GLASS_INK_DEEP` (0.74, «меню трея и модальные
   диалоги» — бейдж формально не диалог). Решение по вкусу.
6. **Match по примитивам без wildcard — тройной компиляционный взрыв.**
   confirm 289-294, badge 212-216, preset 355-358. Не добавишь `Glass` — `cargo
   clippy`/`cargo test` упадут. Это не баг, а принудительная точка синхронизации
   с поверхностью A: переписывать модули можно только после появления
   `Primitive::Glass` (§7: «до его готовности панели не переписываются»).
7. **`LINE_HEIGHT` 15 (text.rs:49) и формулы вёрстки** — все три файла верстают
   через `LINE_HEIGHT` символически (confirm 90-91/124/134, preset 88-98/117-121).
   Смена типографики (§6, поверхность A) пересчитает ритм сама; но визуальные
   зазоры (PAD 16/12, ROW_GAP 14/4) заданы жёстко в файлах — их и меняем по §3.
8. **Магическая `12.0`** в preset_picker 174/211/212 (`text + 2·FIELD_PAD + 12.0`)
   — уже сейчас нарушение §9.1 «никаких магических чисел». При переходе обязана
   стать `PAD_CTRL_X`; при пересчёте ширины кнопок Save/Import/Close проверить, что
   они не вылезли за `content_w` (вписывание — тест 344).
9. **Цифра бейджа и Bitcount Grid Single (§6).** Цифра — «счётчик» по духу §6
   (цифры → Bitcount Grid Single), но её кегль ~78 DIP против табличных 15 — §6 не
   даёт указаний для крупных цифр. Сейчас растр делает `rasterize` на Commissioner
   Medium (text.rs:38) — оставить как есть, решение не за этой поверхностью.
10. **Растр цифры вырастет из-за `TEXT_GLOW` (§6).** Новый `rasterize` расширяет
    битмап на `GLOW_PAD` с каждой стороны; `digit_w` в monitor_badge (102-103)
    считается из пропорций битмапа — пропорции чуть изменятся (формально),
    выравнивание по центру сохранится. Механически ничего не ломается.
11. **TextField в preset_picker — единственный Sunken.** После перехода поле —
    `Surface::Sunken` (§4: тёмная кромка сверху, `RIM_BOTTOM` снизу, без градиента),
    фокус — `STROKE_STRONG`. Плейсхолдер в rst-render живёт на `opacity 0.45`
    (widgets.rs:1985) — при §2.3 это `TEXT_FAINT`; пометка для поверхности A.
12. **Порядок слоёв (§4) для бейджа.** `Panel::draw` рисует фон первым, цифра —
    виджет поверх (monitor 104-117): при `glass_panel` порядок сохраняется, цифра
    остаётся над стеклом. Гало `HOVER_GLOW` бейджу не нужно (нет hover) — вызывать
    `glass_panel`, а не `glass_control`.

---

### Сводный план переписывания (без открытия исходников)

1. Удалить оба локальных `DANGER_TEXT` (confirm 66, preset 246) → `theme::DANGER`.
2. Заменить `theme::settings::TEXT` (confirm 365, badge 99/280) → `theme::TEXT`.
3. Все 7 панельных вызовов `with_style(Settings).with_corner_radius(...)` →
   `glass_panel` с радиусами: модал 18, пресеты 18, бейдж 7.
4. Все 11 кнопок/чекбокс/поле → `glass_control` (+`Sunken` для поля), с
   `hover_t`/`press_t` и `primary=false`.
5. Геометрия по §3 (таблицы 2.1-2.3): PAD 16→14 / 12→14, GAP 8→10, ROW_GAP 14→10
   и 4→10, SECTION_GAP 10→10, BUTTON_H 26→30, ROW_H 28→30, DELETE_W 28→30,
   BUTTON_PAD_X 14→12, FIELD_PAD+12 → PAD_CTRL_X, «1.0» высоты → HAIRLINE.
6. Дивайдер → `STROKE`; пустой список → `TEXT_DIM`.
7. В тестах: поправить 2 обращения к `theme::settings::TEXT`, 3 match по `Primitive`
   (добавить `Glass`), пересчитать 2 теста GAP/цвета; остальное структурно.
8. Сверить зависимости: переписывание возможно только после `Primitive::Glass`,
   `glass_panel`, `glass_control` и `Widget::animate` в rst-render (поверхность A).