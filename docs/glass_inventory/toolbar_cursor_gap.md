# Инвентаризация D — тулбар стикера, панель у курсора, зазор (toolbar.rs, cursor_panel.rs, gap_panel.rs)

Отчёт по поверхности D (docs/DESIGN_LIQUID_GLASS.md §8, таблица). Снят 2026-08-29.
Источники: `crates/resticker/src/toolbar.rs` (572 стр.), `crates/resticker/src/cursor_panel.rs` (400 стр.),
`crates/resticker/src/gap_panel.rs` (152 стр.), `crates/rst-render/src/widgets.rs` (4695 стр. — тема и draw-пути виджетов).

**Ключевой вывод:** все три файла — чистые билдеры на `rst_render::widgets`. Собственных цветов и литералов
`[0x..]` в них **нет** (правило §9.1 уже соблюдено); вся палитра приходит через `WidgetStyle::Settings`
и константы `theme::*`, потребляемые draw-путями виджетов. Перекраска = удаление `WidgetStyle::Settings`
из билдеров + переопределение `theme` (дорожка A). Строки `rst-render` ниже даны, чтобы понять,
что именно красит каждый вызов, — сам `rst-render` правит координатор.

---

## 1. ВСЕ места, где задан цвет

### 1.1 Прямые обращения в трёх файлах

Цветовых литералов в трёх файлах нет. Прямых обращений к палитре — одно:

| Строка | Файл | Что красит | Переход на §2 |
|---|---|---|---|
| cursor_panel.rs:133 | `theme::settings::CORNER_RADIUS` | радиус скругления корпуса панели | радиус — не цвет; см. §2 настоящего отчёта (`RADIUS_CARD`/`RADIUS_TIGHT`) |
| gap_panel.rs:57 | `theme::settings::CORNER_RADIUS` | то же для панели зазора | то же |

Оба — из модуля `theme::settings`, который удаляется целиком (§7). Заменить: радиус в вызове
`glass_panel` (§7 API).

### 1.2 Цвета, которыми красятся эти панели через rst-render (module удаляется — `theme::settings`)

Все три панели собираются с `WidgetStyle::Settings`, поэтому цвета берутся исключительно из
`theme::settings` (widgets.rs:295–345). Модуль удаляется целиком; соответствие токенам §2:

| Константа (rst-render) | Значение | Строка | Что красит (draw-путь) | Токен §2 |
|---|---|---|---|---|
| `settings::BG` | `[0x76,0x76,0x76]` | widgets.rs:297 | фон корпуса панели — `Panel::draw` → `settings_frame_radius` (2581) → `settings_frame` (585) | `GLASS_INK` |
| `settings::BG_OPACITY` | `0.82` | widgets.rs:334 | непрозрачность фона панели | `GLASS_INK` @ `0.62` |
| `settings::BORDER_LIGHT` | `[0xbb,0xba,0xba]` | widgets.rs:299 | верхняя+левая грань bevel (поднятые поверхности) — `settings_bevel` (369) | `RIM_TOP` / `RIM_SIDE` |
| `settings::BORDER_DARK` | `[0x43,0x43,0x43]` | widgets.rs:301 | нижняя+правая грань bevel | `RIM_BOTTOM` |
| `settings::BTN_BG` | `[0x7b,0x7b,0x7b]` | widgets.rs:303 | фон кнопок, ручка слайдера | `CTRL_BG` |
| `settings::BTN_BG_HOVER` | `[0x8c,0x8c,0x8c]` | widgets.rs:304 | кнопка под курсором, ручка слайдера при drag | `CTRL_BG_HOVER` |
| `settings::BTN_LIGHT`/`BTN_DARK` | `[0xbb,0xbb,0xbb]`/`[0x45,0x45,0x45]` | widgets.rs:305–306 | грани кнопки (bevel) | `RIM_TOP` / `RIM_BOTTOM` |
| `settings::INPUT_BG` | `[0x5a,0x5a,0x5a]` | widgets.rs:315 | фон числовых полей (toolbar TB_FIELD, gap FIELD_GAP) и жёлоб слайдера | `SUNKEN_BG` |
| `settings::INPUT_LIGHT`/`INPUT_DARK` | `[0xb7,0xb7,0xb7]`/`[0x34,0x34,0x34]` | widgets.rs:316–317 | перевёрнутые грани полей (sunken) | кромки Sunken §4: тёмная сверху, `RIM_BOTTOM` снизу |
| `settings::ACCENT` | `[0x3c,0x98,0x98]` бирюзовый | widgets.rs:322 | заполнение дорожки слайдера (1112), рамка фокуса поля (1600, 1943) | **запрещён §2.4**: заполнение → белый (`TEXT`), рамка фокуса → `STROKE_STRONG` |
| `settings::TEXT` | `[0xff,0xff,0xff]` | widgets.rs:330 | подпись кнопки (912), текст числового поля (1637) | `TEXT` |
| `settings::DANGER` | `[0xd0,0x3c,0x3c]` | widgets.rs:328 | в трёх файлах **не используется** (лента групп) | `DANGER` §2.4 — остаётся |
| `settings::CHECK_*` | `[0x55..]` и др. | widgets.rs:308–311 | в трёх файлах не используется (чекбоксы — window_picker) | `CTRL_BG*` / `CTRL_BG_ON` |
| `settings::ICON_OFF_OPACITY` | `0.5` | widgets.rs:337 | в трёх файлах не используется | — |

### 1.3 Константы тёмной схемы `theme` (widgets.rs:214–238), которые перестают использоваться

Напрямую три файла их не берут, но draw-пути виджетов под `WidgetStyle::Overlay` ссылаются на них;
после удаления `Settings`-веток эти константы станут единственной схемой и переопределяются под §2:

| Константа | Значение | Строка | Токен §2 |
|---|---|---|---|
| `PANEL_BG` | `[0x2b,0x2b,0x30]` | widgets.rs:214 | `GLASS_INK` |
| `PANEL_BG_OPACITY` | `0.92` | widgets.rs:216 | `GLASS_INK` @ `0.62` |
| `PANEL_BORDER` | `[0x55,0x55,0x5e]` | widgets.rs:218 | `STROKE` |
| `BUTTON_BG` / `BUTTON_BG_HOVER` / `BUTTON_BG_ARMED` | `[0x3a..]`/`[0x4a..]`/`[0x2a..]` | widgets.rs:220–224 | `CTRL_BG` / `CTRL_BG_HOVER` / `CTRL_BG_ACTIVE` |
| `SLIDER_TRACK` | `[0x55,0x55,0x5e]` | widgets.rs:226 | `SUNKEN_BG` |
| `SLIDER_FILL` | `[0x4f,0x9c,0xff]` **синий** | widgets.rs:228 | **запрещён §2.4** → белый `TEXT` (сила света) |
| `FIELD_BG` | `[0x20,0x20,0x24]` | widgets.rs:230 | `SUNKEN_BG` |
| `FIELD_BORDER` | `[0x55,0x55,0x5e]` | widgets.rs:232 | `STROKE` |
| `FIELD_BORDER_FOCUS` | `[0x4f,0x9c,0xff]` **синий** | widgets.rs:234 | `STROKE_STRONG` |
| `CARET` | `[0xff,0xff,0xff]` | widgets.rs:236 | `TEXT` (уже белый) |
| `TEXT` | `[0xf0,0xf0,0xf0]` | widgets.rs:238 | `TEXT` |

**Особо:** `Label` в gap_panel.rs:74–76 красит текст всегда `theme::TEXT`, а «приглушение» делает
непрозрачностью `0.5` (widgets.rs:2447). По §6 это должно стать цветом `TEXT_DIM` (#FFFFFF @ 0.66),
а не полупрозрачностью. Механизм `Label::set_dim` уходит или переопределяется в `rst-render`.

---

## 2. ВСЕ геометрические числа

### 2.1 toolbar.rs (локальные константы)

| Строка | Константа | Значение | Переход по §3 |
|---|---|---|---|
| 43 | `TOOLBAR_GAP_Y` | 8.0 | §3 не покрывает (внешний отступ от рамки); оставить локально |
| 45 | `TOOLBAR_PAD` | 4.0 | `PAD_PANEL` = 14 — слишком велик для узкой панели; оставить (решение) |
| 47 | `TOOLBAR_WIDGET_GAP` | 4.0 | `GAP_ROW` = 10 — между строками, не кнопками; оставить (решение) |
| 49 | `TOOLBAR_SLIDER_W` | 96.0 | оставить |
| 51 | `TOOLBAR_FIELD_W` | 40.0 | оставить |
| 53 | `TOOLBAR_HEIGHT` | `BUTTON_SIZE + 2*PAD` = **36** | станет 38 при `BUTTON_SIZE` 30 |
| 56–62 | `TOOLBAR_WIDTH` | 372 (комментарий на 269) | пересчитать: 2·4 + 96 + 4 + 40 + 4 + 7·30 + 6·4 = **388** |
| 65–66 | `TOOLBAR_WIDTH_MULTI` | 4·2+7·28+6·4 = 288 | пересчитать: 4·2+7·30+6·4 = **308** |
| 70–75 | `TOOLBAR_VIDEO_EXTRA_W` | 4+28+4+28+4+96 = 164 | пересчитать: 4+30+4+30+4+96 = **168** |
| 53, 61, 66, 71–75, 184, 187, 200, 203, 214, 217, 225 | `theme::BUTTON_SIZE` (=28, widgets.rs:241) | сторона кнопки | **30 DIP** (§3, «было 28») |

### 2.2 cursor_panel.rs (локальные константы)

| Строка | Константа | Значение | Переход по §3 |
|---|---|---|---|
| 55 | `BUTTON_SIZE` | `2.0 * theme::BUTTON_SIZE` = **56** | станет 60 при BUTTON_SIZE 30 (множитель 2× — пользовательское требование 2026-08-23, сохранить) |
| 57 | `PANEL_PAD` | 12.0 | §3 не покрывает; оставить (решение) |
| 59 | `BUTTON_GAP` | 8.0 | §3 не покрывает; оставить (решение) |
| 62 | `BUTTON_COUNT` | 7.0 | без изменений |
| 64 | `BOTTOM_MARGIN_DIP` | 16.0 | без изменений |
| 66 | `PEEK_DIP` | 10.0 | без изменений |
| 71 | `HOVER_MARGIN_DIP` | 7.0 | без изменений |
| 75–78 | `CURSOR_PANEL_SIZE` | 2·12+7·56+6·8 = 440 × 80 | пересчитать: 2·12+7·60+6·8 = **468 × 84** |
| 92–93 | `panel_center` hidden/shown | формула | без изменений |
| 102–112 | `peek_hot_zone` | формула от CURSOR_PANEL_SIZE | самопересчитается |

### 2.3 gap_panel.rs (локальные константы)

| Строка | Константа | Значение | Переход по §3 |
|---|---|---|---|
| 33 | `WIDTH` | 260.0 | без изменений |
| 35 | `PAD` | 12.0 | §3 не покрывает; оставить (решение) |
| 37 | `SECTION_GAP` | 10.0 | `GAP_ROW` = 10 — совпадает, заменить на токен |
| 39 | `ROW_H` | 28.0 | высота поля/кнопки; поле — `RADIUS_CTRL` 10, высота §3 не задана; оставить 28 (решение) |
| 42 | `FIELD_W` | 64.0 | без изменений |
| 49–51 | `height()` | 2·12 + LINE_HEIGHT·2 + ROW_H·2 + SECTION_GAP·3 | формулу сохранить, пересчитать при смене слагаемых |
| 85 | кнопка Close `w: 96.0` | ширина кнопки | `PAD_CTRL_X` = 12 — горизонтальный отступ подписи; ширина не задана, оставить |

### 2.4 Геометрия rst-render, потребляемая виджетами трёх панелей

| Константа | Значение | Строка rst-render | Кто потребляет | Переход по §3 |
|---|---|---|---|---|
| `BUTTON_SIZE` | 28 | widgets.rs:241 | toolbar, cursor_panel (×2) | **30** |
| `BUTTON_PAD` | 4.0 | widgets.rs:243 | отступ иконки в кнопке (877, 2257) | без изменений |
| `SLIDER_HEIGHT` | 20.0 | widgets.rs:245 | коробка слайдера (1009) | без изменений |
| `SLIDER_KNOB` | 12.0 | widgets.rs:247 | ход ручки (1041) | без изменений |
| `SLIDER_TRACK_H` | 2.0 | widgets.rs:249 | дорожка (Overlay-ветка) | заменяется жёлобом Sunken |
| `SETTINGS_GROOVE_H` | 6.0 | widgets.rs:253 | жёлоб слайдера (1090) | Sunken-поверхность (§4), высота — решение |
| `SETTINGS_KNOB_W` | 10.0 | widgets.rs:257 | ручка (1120) | `glass_control`, ширина — решение |
| `FIELD_HEIGHT` | 22.0 | widgets.rs:259 | высота числовых полей (1405, 1424) | без изменений |
| `FIELD_PAD` | 4.0 | widgets.rs:261 | отступ текста в поле | без изменений |
| `BEVEL` | 1.0 | widgets.rs:332 | толщина всех граней | `HAIRLINE` = 1.0 |
| `CORNER_RADIUS` | 8.0 | widgets.rs:344 | корпус панелей | тулбар → `RADIUS_TIGHT` 7 (явно §3); cursor_panel и gap → `RADIUS_CARD` 14 (решение; см. §6.7) |

---

## 3. Рамка / бевель / объём старого стиля — замена glass_panel / glass_control (§7)

Вызовы, которые производят старый объём:

**rst-render (исполняются для всех трёх панелей):**

| Строка rst-render | Вызов | Когда |
|---|---|---|
| 2581 | `settings_frame_radius(out, frame, corner_radius, 1.0)` | `Panel::draw` при `WidgetStyle::Settings` — фон всех трёх панелей → **`glass_panel`** |
| 581–589 | `settings_frame` (внутри: `settings_bevel` 588) | падение для `radius<=0` → **`glass_panel`** |
| 875 | `settings_bevel(out, bounds, !armed, 1.0)` | `Button::draw` — 9 кнопок тулбара, 7 кнопок cursor_panel, Close → **`glass_control`** |
| 1098 | `settings_bevel(out, groove, false, 1.0)` | `Slider::draw_settings` — жёлоб (вдавленный) → **Sunken** |
| 1133 | `settings_bevel(out, knob, true, 1.0)` | ручка слайдера → **`glass_control`** |
| 1598 | `settings_bevel(out, bounds, false, 1.0)` | `NumericField::draw` — TB_FIELD, FIELD_GAP → **Sunken** |
| 1600 | `accent_outline(out, bounds, ACCENT)` | рамка фокуса поля → **`STROKE_STRONG`** (или гало §4 п.6) |
| 1941 / 1943 | `settings_bevel` / `accent_outline` | `TextField::draw` — в трёх файлах не используется (окно настроек/списки) |
| 2255 / 2295 | `settings_bevel(out, bounds, armed, opacity)` | `Checkbox` — в трёх файлах не используется |

**Вызовы из трёх файлов, из-за которых всё выше работает:**

| Строка | Файл | Что стилизуется |
|---|---|---|
| 151 | toolbar.rs | корпус панели (`Panel::new(...).with_style(Settings)`) |
| 160 | toolbar.rs | слайдер прозрачности |
| 167 | toolbar.rs | числовое поле прозрачности |
| 185 | toolbar.rs | 7 кнопок (цикл) |
| 201 | toolbar.rs | кнопка play/pause |
| 215 | toolbar.rs | кнопка timeline |
| 232 | toolbar.rs | слайдер громкости |
| 132 | cursor_panel.rs | корпус панели |
| 165 | cursor_panel.rs | 7 кнопок (цикл) |
| 56 | gap_panel.rs | корпус панели |
| 91 | gap_panel.rs | кнопка Close |

Все 11 `.with_style(WidgetStyle::Settings)` удаляются вместе с enum `WidgetStyle` (§7);
панели/виджеты по умолчанию рисуются стеклом. Никакого ручного вызова билдеров из трёх файлов
не требуется — только снятие стиля и (для корпусов) явный радиус в `glass_panel`/`with_corner_radius`.

---

## 4. Кому нужна анимация наведения/нажатия (§5), кому — нет

**Нужна (`glass_control` с hover_t/press_t):**

| Виджет | Файл:строка | ID |
|---|---|---|
| кнопки ×7 | toolbar.rs:182–188 | TB_LAYERS, TB_EYE, TB_ORDER_UP, TB_ORDER_DOWN, TB_DUPLICATE, TB_RESET_SCALE, TB_DELETE |
| кнопка play/pause | toolbar.rs:199–203 | TB_PLAY_PAUSE |
| кнопка timeline | toolbar.rs:213–217 | TB_TIMELINE |
| кнопка-динамик громкости | toolbar.rs:239–243 | TB_VOLUME (`VolumeControl`: клик переключает mute, шкала выпадает при наведении) |
| кнопки ×7 | cursor_panel.rs:151–167 | BTN_LOAD_FILE, BTN_ADD_WINDOW, BTN_PRESETS, BTN_GROUPS, BTN_TOGGLE_ALL, BTN_SETTINGS, BTN_EXIT |
| кнопка Close | gap_panel.rs:79–92 | BTN_CLOSE |
| ручка слайдера (drag) | toolbar.rs:158–162 | TB_SLIDER |

**Фокусные (не hover, а focus):**

| Виджет | Файл:строка | ID | Поведение |
|---|---|---|---|
| числовое поле прозрачности | toolbar.rs:165–170 | TB_FIELD | фокус → `STROKE_STRONG`-рамка |
| числовое поле зазора | gap_panel.rs:66–69 | FIELD_GAP | фокус → `STROKE_STRONG`-рамка |

**НЕ нужна (статическое):**
- заголовок «Snap gap» — gap_panel.rs:62 (Label, hit_test=false)
- подсказка «Scroll to change…» — gap_panel.rs:74–76 (Label, dim)

**Внимание к слайдерам:** у `Slider` нет поля hovered — есть только `dragging`; «наведение» на
жёлоб не отслеживается вообще (widgets.rs:965–976). Т.е. для слайдера анимация = ручка при drag.
Либо вводить hover в слайдер (за рамками поверхности D — rst-render, дорожка A), либо не ждать
его здесь.

**Тик координатора (§5 «Важно про перерисовку»):** под курсором над любым из этих виджетов
координатор обязан планировать `UiTick` 60 Гц, пока есть незавершённые hover_t/press_t.
В cursor_panel уже существует собственная анимация выезда (progress, panel_center:88–96) —
с ней координатор уже умеет работать; новые фазы виджетов подключаются тем же сообщением.

---

## 5. Тесты, привязанные к числам геометрии (сломаются при переходе на §3)

### toolbar.rs — сломаются (литые пиксели и константные размеры)

| Тест | Строка | Что пересчитать |
|---|---|---|
| `toolbar_below_selection_when_space` | 258–264 | хит-точки **476** и **457**: при TOOLBAR_HEIGHT 38 низ рамки = 450+8=458, cy = 477, «зазор» на 457 остаётся вне (458−1); верхнюю точку 476 → **477** |
| `toolbar_centered_horizontally_on_bbox` | 267–274 | точки **775/773** и комментарий «x ∈ [774, 1146], TOOLBAR_WIDTH = 372»: при BUTTON_SIZE 30 тулбар x ∈ [766, 1154] (center 960, w 388), край = 766 |
| `toolbar_above_when_no_space_below` | 277–285 | cy-точка **984**: cy = 1010−8−19 = **983** |
| `toolbar_below_boundary_is_inclusive` | 288–301 | формула на константах — пройдёт автоматически; проверить после пересчёта TOOLBAR_HEIGHT |
| `toolbar_multi_is_narrower_by_slider_and_field` | — | тест удалён 2026-09-06: тулбар мультивыделения больше не сужается, ширина и состав унифицированы |
| `button_icon` (хелпер) | 377–391 | комментарий «в стилистике настроек между фоном и иконкой лежат ещё четыре грани объёмной рамки (settings_bevel)» — устареет; поиск `Primitive::Icon` по типу переживёт смену примитивов |
| `toolbar_video_adds_play_pause_and_volume_after_buttons` | 478–508 | `p.frame().w == TOOLBAR_WIDTH + TOOLBAR_VIDEO_EXTRA_W` — формула, переживёт |
| остальные (опacity-мироринг, порядок, отсутствие виджетов) | 314–363, 426–467, 470–571 | логика, не геометрия — не сломаются |

### cursor_panel.rs — сломаются только при ручной правке

Все тесты выведены из констант (`CURSOR_PANEL_SIZE`, `BUTTON_SIZE`, `PEEK_DIP` и т.д.) и
**переживут пересчёт автоматически**, включая `buttons_are_twice_the_toolbar_size_and_ordered`
(267–294, утверждение `b.w == 2.0 * theme::BUTTON_SIZE`). Сломаются, только если кто-то заменит
`BUTTON_SIZE = 2.0 * theme::BUTTON_SIZE` на фиксированное число. Не менять механику.

### gap_panel.rs — формульный, переживёт при синхронизации

| Тест | Строка | Что проверить |
|---|---|---|
| `the_bottom_button_fits_inside_the_panel` | 123–140 | держится на `height()` (49–51) и `PAD`; переживёт, пока формула `height()` синхронизирована с раскладкой. Допуск `+0.5` — сохранить |
| `field_shows_the_current_value`, `value_above_the_ceiling_is_clamped_by_the_field` | 114–151 | логика, не геометрия |

**Дополнительно к тестам:** числовые комментарии в toolbar.rs:268–270 (372, x ∈ [774, 1146]) —
обновить по факту.

---

## 6. Риски и странности

1. **`WidgetStyle::Settings` удаляется целиком** — 11 вызовов `.with_style` в трёх файлах исчезают
   бесследно. Если панель после снятия стиля должна остаться стеклянной по умолчанию, то
   `Panel::draw` без стиля (Overlay-ветка, widgets.rs:2587–2600) тоже должна перейти на
   `glass_panel` — иначе панели вернутся к тёмно-серой схеме `PANEL_BG`. Это решает координатор
   в дорожке A, но три файла обязаны не заново проставлять `Settings`-стиль.
2. **`theme::settings` — два потребителя в трёх файлах** (cursor_panel.rs:133, gap_panel.rs:57):
   компиляция упадёт в момент удаления модуля. Заменять на радиус, передаваемый в
   `glass_panel`/`with_corner_radius` (RADIUS_*).
3. **Бирюза и синь запрещены (§2.4), а они в двух местах, видимых на этих панелях:**
   заполнение слайдера `SLIDER_FILL` (0x4f9cff, widgets.rs:228) и `settings::ACCENT`-рамка фокуса
   (widgets.rs:1600). Машинально «перекрасить» нельзя — это смена языка оформления: заливка
   становится белой силой света (`TEXT`), рамка фокуса — `STROKE_STRONG`.
4. **`Label::set_dim` — это не цвет, а opacity 0.5** (widgets.rs:2447). Подсказка gap_panel (74–76)
   по §6 должна стать `TEXT_DIM` цветом, а не полупрозрачным `TEXT`. Механизм `dim` живёт в
   rst-render — нужна правка дорожки A либо локальный обход.
5. **Слайдеры не знают hover** (widgets.rs:965–976) — для «продавливания» жёлоба/ручки при
   наведении (§5) в `Slider` нет поля фазы. Только drag ручки. Либо расширять rst-render
   (дорожка A), либо принять, что слайдер анимирует только ручку при перетаскивании.
6. **Хит-тест при scale 0.972/0.955** (§5): `glass_control` сам ужимает rect для отрисовки,
   но `hit_test`/`bounds()` виджета остаются на полном прямоугольнике — иначе клик «в пустоте»
   у краёв кнопки начнёт проваливаться в панель. Сохранить хит-тест по несжатому rect.
7. **Радиус корпусов неоднозначен в §3:** тулбар явно `RADIUS_TIGHT` (7). Для cursor_panel
   (полоса кнопок) и gap_panel (модалка) спека прямо не отвечает: `RADIUS_WINDOW`=18 — «окно
   настроек», `RADIUS_CARD`=14 — «карточка внутри панели». Предложение: **cursor_panel и gap →
   RADIUS_CARD 14**; решение за координатором.
8. **gap_panel WIDTH 260 не покрыт §3** — ширина панели от спеки не зависит; не трогать.
9. **Кнопка Delete (TB_DELETE) и Close не должны стать `primary`** (`CTRL_BG_PRIMARY` §2.2 —
   только «подтверждающая» кнопка; в трёх панелях таковой нет).
10. **Комментарии с устаревшими числами:** toolbar.rs:268–270 (372/774–1146), toolbar.rs:377–391
    (четыре грани bevel у кнопки). Обновить в рамках правки, иначе разойдутся с кодом.
11. **Иконки не трогать** (§8): в трёх панелях 16 кнопок с иконками — толщина штриха/цвет
    иконок вне этого захода.
12. **Громкость TB_VOLUME — не слайдер, а кнопка-динамик `VolumeControl`** (toolbar.rs):
    кнопка размера `theme::BUTTON_SIZE` с выпадающей вертикальной шкалой 0..=100 при наведении
    и переключателем `muted` по клику (решение пользователя 2026-09-06).
13. **cursor_panel уже анимирует выезд** (progress 0..1, panel_center:88–96) — это готовый прецедент
    покадровой анимации в координаторе; новые 160/110 мс переходы не конфликтуют, но тик 60 Гц
    должен планироваться и для неподвижной панели с «зависшим» курсором (§5).
14. **gap_panel кнопка Close шириной 96 фиксирована** — под `PAD_CTRL_X` 12 подпись «Close»
    (≈40 px) центрируется с запасом; менять не требуется.

---

## Итог для переписывания (без открытия исходников)

1. Снять все 11 `.with_style(WidgetStyle::Settings)`; корпусы — `glass_panel` с радиусом
   `RADIUS_TIGHT` (тулбар) / `RADIUS_CARD` (cursor_panel, gap).
2. `theme::settings::CORNER_RADIUS` (2 места) заменить на токены §3.
3. `BUTTON_SIZE` 28→30; пересчитать `TOOLBAR_HEIGHT` (38), `TOOLBAR_WIDTH` (388),
   `TOOLBAR_WIDTH_MULTI` (308), `TOOLBAR_VIDEO_EXTRA_W` (168), `CURSOR_PANEL_SIZE` (468×84);
   обновить 3 литых теста toolbar (476→477, 775→766, 984→983) и комментарии 268–270, 377–391.
4. Полю/полям — Sunken-поверхность, фокус — `STROKE_STRONG`; слайдеру — Sunken-жёлоб +
   белая заливка + `glass_control`-ручка; подсказке gap — `TEXT_DIM` вместо dim-opacity.
5. Кнопкам — `glass_control` с фазами; ничего «primary» не помечать; анимации §5 + `UiTick`
   в координаторе (дорожка J).
6. Тексты кнопок/полей — токен `TEXT`; ярлыки — `TEXT`/`TEXT_DIM`.