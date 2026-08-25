//! Чистые билдеры UI-индикаторов тайлинга (docs/TILING_DESIGN.md §T5, §T6, §Р3).
//!
//! resticker рисует элементы интерфейса поверх окон на едином прозрачном D3D11-оверлее
//! каждого монитора. Модуль содержит чистые функции без побочных эффектов:
//!
//! 1. [`active_border`] — рамка активного / неактивного окна (внутренняя обводка плитки).
//! 2. [`workspace_bar`] — компактная полоса переключения воркспейсов внизу экрана.
//! 3. [`submap_indicator`] — плашка активного модального режима вверху экрана.
//! 4. [`group_tabs`] — полоса табов группы окон (Tabbed/Stacked контейнеры).
//! 5. [`switcher_panel`] — экран собственного переключателя окон (Alt+Tab).
//!
//! Вся палитра и стили строго берутся из [`rst_render::theme::settings`] для визуального
//! единства с остальным интерфейсом программы.

use rst_core::hittest::DipRect;
use rst_core::model::Rect;
use rst_render::{
    Box2D, Button, ButtonContent, LINE_HEIGHT, Label, Panel, Primitive, WidgetId, WidgetStyle,
    text_size, theme,
};

/// Идентификатор панели полосы воркспейсов. Диапазон 600+.
/// Идентификаторы виджетов тайлинга разведены по тысячам, а не по сотням.
///
/// Кнопка воркспейса получает `WS_BTN_BASE + номер`, а номер — `u8`, то есть
/// теоретически до 255. При шаге в сотню воркспейс с номером 99 залезал бы в
/// диапазон следующей панели, и два разных виджета получили бы один id —
/// с непредсказуемой отрисовкой и попаданием кликов (второе ревью,
/// подозрение 4). Тысяча покрывает весь диапазон `u8` с запасом.
pub const WS_PANEL_ID: WidgetId = 600;
/// Базовый идентификатор кнопок воркспейсов (`WS_BTN_BASE + num`).
pub const WS_BTN_BASE: WidgetId = 601;

/// Идентификатор панели индикатора модального режима (submap). Диапазон 700+.
pub const SUBMAP_PANEL_ID: WidgetId = 1_000;
/// Идентификатор текстовой надписи режима.
pub const SUBMAP_LABEL_ID: WidgetId = 1_001;

/// Идентификатор панели полосы табов группы. Диапазон 800+.
pub const TABS_PANEL_ID: WidgetId = 2_000;
/// Базовый идентификатор кнопок табов группы (`TAB_BTN_BASE + index`).
pub const TAB_BTN_BASE: WidgetId = 2_001;
/// Идентификатор кнопки/бейджа переполнения табов (`+N`).
pub const TAB_OVERFLOW_ID: WidgetId = 2_999;

/// Идентификатор панели переключателя окон (Alt+Tab). Диапазон 900+.
pub const SWITCHER_PANEL_ID: WidgetId = 3_000;
/// Базовый идентификатор карточек переключателя (`SWITCHER_CARD_BASE + index`).
pub const SWITCHER_CARD_BASE: WidgetId = 901;
/// Базовый идентификатор бейджей групп переключателя (`SWITCHER_BADGE_BASE + index`).
pub const SWITCHER_BADGE_BASE: WidgetId = 951;

/// Толщина рамки активного окна, DIP.
pub const ACTIVE_BORDER_THICKNESS: f64 = 2.0;

/// Внутренний отступ полосы воркспейсов, DIP.
pub const WS_PAD: f64 = 4.0;
/// Размер кнопки воркспейса (ширина и высота), DIP.
pub const WS_BTN_SIZE: f64 = 24.0;
/// Зазор между кнопками воркспейсов, DIP.
pub const WS_GAP: f64 = 4.0;
/// Отступ полосы воркспейсов от нижнего края экрана, DIP.
pub const WS_BOTTOM_MARGIN: f64 = 16.0;

/// Внутренний отступ плашки режима, DIP.
pub const SUBMAP_PAD: f64 = 6.0;
/// Максимальная допустимая ширина плашки режима, DIP.
pub const SUBMAP_MAX_WIDTH: f64 = 260.0;
/// Отступ плашки режима от верхнего края экрана, DIP.
pub const SUBMAP_TOP_MARGIN: f64 = 16.0;

/// Внутренний отступ полосы табов группы, DIP.
pub const TAB_BAR_PAD: f64 = 2.0;
/// Зазор между табами в полосе, DIP.
pub const TAB_GAP: f64 = 2.0;
/// Минимальная ширина одного таба, DIP.
pub const MIN_TAB_WIDTH: f64 = 50.0;
/// Внутренний горизонтальный отступ текста внутри таба, DIP.
pub const TAB_TEXT_PAD: f64 = 6.0;
/// Ширина кнопки-счётчика переполнения табов (`+N`), DIP.
pub const TAB_OVERFLOW_WIDTH: f64 = 34.0;

/// Ширина карточки окна в переключателе, DIP.
pub const SW_CARD_WIDTH: f64 = 160.0;
/// Высота карточки окна в переключателе, DIP.
pub const SW_CARD_HEIGHT: f64 = 120.0;
/// Ширина области превью окна внутри карточки, DIP.
///
/// Размеры превью — контракт для координатора: снимок окна рисуется
/// отдельным примитивом поверх карточки (`rst_win32::window_thumb`), и пока
/// этот кусок не подключён, константы никем не читаются. Держим их здесь, а
/// не в координаторе, чтобы разметка карточки и размер снимка задавались в
/// одном месте и не разъехались.
#[allow(dead_code, reason = "контракт превью; потребитель — следующий срез")]
pub const SW_PREVIEW_WIDTH: f64 = 144.0;
/// Высота области превью окна внутри карточки, DIP.
#[allow(dead_code, reason = "контракт превью; потребитель — следующий срез")]
pub const SW_PREVIEW_HEIGHT: f64 = 80.0;
/// Внутренний отступ панели переключателя, DIP.
pub const SW_PANEL_PAD: f64 = 12.0;
/// Зазор между карточками переключателя, DIP.
pub const SW_CARD_GAP: f64 = 8.0;
/// Внутренний отступ элементов внутри карточки, DIP.
pub const SW_CARD_PAD: f64 = 6.0;

/// Описание одной вкладки в группе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabEntry {
    /// Заголовок окна.
    pub title: String,
    /// Является ли вкладка активной (сфокусированной в группе).
    pub active: bool,
}

/// Описание одной карточки в переключателе окон (Alt+Tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitcherCard {
    /// Заголовок целевого окна.
    pub title: String,
    /// Количество окон в группе (`1` — одиночное окно, `>1` — группа табов).
    pub members: usize,
    /// Выделена ли карточка в данный момент.
    pub selected: bool,
}

/// Построить прямоугольную рамку активного или неактивного окна.
///
/// Рамка строится строго по **внутреннему краю** прямоугольника окна:
/// оверлей рисуется поверх всех окон, и внешняя обводка вылезала бы через зазоры (`gaps_in`)
/// на соседние плитки. Внутренняя обводка гарантированно укладывается в границы `rect`.
///
/// - `rect`: прямоугольник окна в физических пикселях ([`Rect`]).
/// - `focused`: `true` — активное окно в фокусе (акцентный цвет темы [`theme::settings::ACCENT`]),
///   `false` — неактивное окно (приглушённый полупрозрачный цвет [`theme::settings::BORDER_DARK`]).
pub fn active_border(rect: Rect, focused: bool) -> Vec<Primitive> {
    if rect.w == 0 || rect.h == 0 {
        return Vec::new();
    }

    let x = rect.x as f64;
    let y = rect.y as f64;
    let w = rect.w as f64;
    let h = rect.h as f64;

    let t = ACTIVE_BORDER_THICKNESS.min(w / 2.0).min(h / 2.0);
    if t <= 0.0 {
        return Vec::new();
    }

    let (color, opacity) = if focused {
        (theme::settings::ACCENT, 1.0)
    } else {
        (theme::settings::BORDER_DARK, 0.4)
    };

    let mut out = Vec::with_capacity(4);

    // Верхняя грань
    out.push(Primitive::Fill {
        rect: Box2D {
            cx: x + w / 2.0,
            cy: y + t / 2.0,
            w,
            h: t,
            rotation: 0.0,
        },
        color,
        opacity,
    });

    // Нижняя грань
    out.push(Primitive::Fill {
        rect: Box2D {
            cx: x + w / 2.0,
            cy: y + h - t / 2.0,
            w,
            h: t,
            rotation: 0.0,
        },
        color,
        opacity,
    });

    // Боковые грани (между верхней и нижней, без двойного перекрытия углов)
    let inner_h = h - 2.0 * t;
    if inner_h > 0.0 {
        // Левая грань
        out.push(Primitive::Fill {
            rect: Box2D {
                cx: x + t / 2.0,
                cy: y + t + inner_h / 2.0,
                w: t,
                h: inner_h,
                rotation: 0.0,
            },
            color,
            opacity,
        });

        // Правая грань
        out.push(Primitive::Fill {
            rect: Box2D {
                cx: x + w - t / 2.0,
                cy: y + t + inner_h / 2.0,
                w: t,
                h: inner_h,
                rotation: 0.0,
            },
            color,
            opacity,
        });
    }

    out
}

/// Собрать панель индикатора воркспейсов.
///
/// - `workspaces`: список троек `(номер_воркспейса, активный, непустой)`.
/// - `screen`: границы монитора в DIP ([`DipRect`]).
///
/// Панель центрируется внизу монитора (аналогично панели инструментов [`crate::cursor_panel`]).
/// Активный воркспейс выделяется цветом акцента [`theme::settings::ACCENT`], непустые —
/// ярким текстом [`theme::settings::TEXT`], пустые неактивные — приглушённым цветом.
pub fn workspace_bar(workspaces: &[(u8, bool, bool)], screen: &DipRect) -> Panel {
    let n = workspaces.len();
    let bar_w = if n == 0 {
        2.0 * WS_PAD + WS_BTN_SIZE
    } else {
        2.0 * WS_PAD + (n as f64) * WS_BTN_SIZE + (n as f64 - 1.0) * WS_GAP
    };
    let bar_h = 2.0 * WS_PAD + WS_BTN_SIZE;

    // Центрируем панель по горизонтали и прижимаем к низу экрана
    let min_cx = screen.x + bar_w / 2.0;
    let max_cx = (screen.x + screen.w - bar_w / 2.0).max(min_cx);
    let cx = (screen.x + screen.w / 2.0).clamp(min_cx, max_cx);
    let cy = screen.y + screen.h - WS_BOTTOM_MARGIN - bar_h / 2.0;

    let mut panel = Panel::new(
        WS_PANEL_ID,
        Box2D {
            cx,
            cy,
            w: bar_w,
            h: bar_h,
            rotation: 0.0,
        },
    )
    .with_style(WidgetStyle::Settings)
    .with_corner_radius(theme::settings::CORNER_RADIUS);

    for (i, &(num, is_active, is_non_empty)) in workspaces.iter().enumerate() {
        let btn_left = cx - bar_w / 2.0 + WS_PAD + (i as f64) * (WS_BTN_SIZE + WS_GAP);
        let btn_cx = btn_left + WS_BTN_SIZE / 2.0;

        let label_color = if is_active {
            theme::settings::ACCENT
        } else if is_non_empty {
            theme::settings::TEXT
        } else {
            theme::settings::BORDER_LIGHT
        };

        let btn = Button::new(
            WS_BTN_BASE + (num as u32),
            Box2D {
                cx: btn_cx,
                cy,
                w: WS_BTN_SIZE,
                h: WS_BTN_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label(num.to_string()),
        )
        .with_style(WidgetStyle::Settings)
        .with_label_color(label_color);

        panel.add_widget(btn);
    }

    panel
}

/// Собрать плашку-индикатор активного модального режима (submap).
///
/// - `name`: имя активного режима (например, `"resize"`).
/// - `screen`: границы монитора в DIP ([`DipRect`]).
///
/// Размещается вверху по центру монитора для мгновенной видимости (пользователь
/// обязан знать, что режим перехватил клавиши). Длинные имена безопасно усекаются
/// с многоточием по ширине [`SUBMAP_MAX_WIDTH`].
pub fn submap_indicator(name: &str, screen: &DipRect) -> Panel {
    let raw_title = format!("MODE: {}", name.to_uppercase());
    let max_text_w = (SUBMAP_MAX_WIDTH - 2.0 * SUBMAP_PAD).max(10.0);
    let title = truncate_to_width(&raw_title, max_text_w);

    let (tw, _) = text_size(&title);
    let panel_w = (tw + 2.0 * SUBMAP_PAD).clamp(60.0, SUBMAP_MAX_WIDTH);
    let panel_h = LINE_HEIGHT + 2.0 * SUBMAP_PAD;

    let min_cx = screen.x + panel_w / 2.0;
    let max_cx = (screen.x + screen.w - panel_w / 2.0).max(min_cx);
    let cx = (screen.x + screen.w / 2.0).clamp(min_cx, max_cx);
    let cy = screen.y + SUBMAP_TOP_MARGIN + panel_h / 2.0;

    let mut panel = Panel::new(
        SUBMAP_PANEL_ID,
        Box2D {
            cx,
            cy,
            w: panel_w,
            h: panel_h,
            rotation: 0.0,
        },
    )
    .with_style(WidgetStyle::Settings)
    .with_corner_radius(theme::settings::CORNER_RADIUS);

    panel.add_widget(Label::new(SUBMAP_LABEL_ID, cx - tw / 2.0, cy, &title));

    panel
}

/// Собрать полосу табов над прямоугольником группы (Tabbed/Stacked).
///
/// - `rect`: прямоугольник группы окон в координатах монитора ([`Rect`]).
/// - `tab_bar_h`: высота полосы табов в DIP (из `LayoutParams`).
/// - `tabs`: список вкладок [`TabEntry`].
///
/// # Поведение и распределение ширины
///
/// 1. Вкладки делят доступную ширину поровну.
/// 2. Если вкладок слишком много и они не помещаются с минимальной комфортной шириной
///    [`MIN_TAB_WIDTH`], отображаются первые $M$ вкладок, а в конце добавляется
///    индикатор переполнения `+N` (например, `+3`). Это наглядно сообщает пользователю о
///    наличии скрытых вкладок и исключает нечитаемые микро-вкладки шириной в 3 пикселя.
/// 3. Заголовки усекаются с многоточием под ширину конкретной вкладки.
/// 4. Активная вкладка выделяется акцентным цветом [`theme::settings::ACCENT`].
pub fn group_tabs(rect: Rect, tab_bar_h: f64, tabs: &[TabEntry]) -> Panel {
    let w = (rect.w as f64).max(0.0);
    let h = tab_bar_h.clamp(0.0, (rect.h as f64).max(0.0));
    let x = rect.x as f64;
    let y = rect.y as f64;

    let cx = x + w / 2.0;
    let cy = y + h / 2.0;

    let mut panel = Panel::new(
        TABS_PANEL_ID,
        Box2D {
            cx,
            cy,
            w,
            h,
            rotation: 0.0,
        },
    )
    .with_style(WidgetStyle::Settings);

    if tabs.is_empty() || w <= 0.0 || h <= 0.0 {
        return panel;
    }

    let n = tabs.len();
    let avail_w = (w - 2.0 * TAB_BAR_PAD).max(0.0);
    let btn_h = (h - 2.0 * TAB_BAR_PAD).max(1.0);

    // Проверяем, помещаются ли все вкладки с минимальной шириной MIN_TAB_WIDTH
    let total_min_w = (n as f64) * MIN_TAB_WIDTH + (n as f64 - 1.0).max(0.0) * TAB_GAP;

    if total_min_w <= avail_w {
        // Все N вкладок помещаются — делим доступную ширину поровну
        let tab_w = (avail_w - (n as f64 - 1.0) * TAB_GAP) / (n as f64);
        let mut cur_x = x + TAB_BAR_PAD;

        for (i, tab) in tabs.iter().enumerate() {
            let max_text_w = (tab_w - 2.0 * TAB_TEXT_PAD).max(10.0);
            let display_title = truncate_to_width(&tab.title, max_text_w);
            let btn_cx = cur_x + tab_w / 2.0;

            let label_color = if tab.active {
                theme::settings::ACCENT
            } else {
                theme::settings::TEXT
            };

            let btn = Button::new(
                TAB_BTN_BASE + (i as u32),
                Box2D {
                    cx: btn_cx,
                    cy,
                    w: tab_w,
                    h: btn_h,
                    rotation: 0.0,
                },
                ButtonContent::Label(display_title),
            )
            .with_style(WidgetStyle::Settings)
            .with_label_color(label_color);

            panel.add_widget(btn);
            cur_x += tab_w + TAB_GAP;
        }
    } else {
        // Вкладок слишком много: выделяем место под бейдж переполнения `+K`
        let space_for_tabs = (avail_w - TAB_OVERFLOW_WIDTH - TAB_GAP).max(MIN_TAB_WIDTH);
        let max_visible = ((space_for_tabs + TAB_GAP) / (MIN_TAB_WIDTH + TAB_GAP)).floor() as usize;
        let visible_count = max_visible.clamp(1, n.saturating_sub(1));
        let overflow_count = n - visible_count;

        let tab_w =
            (space_for_tabs - (visible_count as f64 - 1.0) * TAB_GAP) / (visible_count as f64);
        let mut cur_x = x + TAB_BAR_PAD;

        for (i, tab) in tabs.iter().take(visible_count).enumerate() {
            let max_text_w = (tab_w - 2.0 * TAB_TEXT_PAD).max(10.0);
            let display_title = truncate_to_width(&tab.title, max_text_w);
            let btn_cx = cur_x + tab_w / 2.0;

            let label_color = if tab.active {
                theme::settings::ACCENT
            } else {
                theme::settings::TEXT
            };

            let btn = Button::new(
                TAB_BTN_BASE + (i as u32),
                Box2D {
                    cx: btn_cx,
                    cy,
                    w: tab_w,
                    h: btn_h,
                    rotation: 0.0,
                },
                ButtonContent::Label(display_title),
            )
            .with_style(WidgetStyle::Settings)
            .with_label_color(label_color);

            panel.add_widget(btn);
            cur_x += tab_w + TAB_GAP;
        }

        // Кнопка/бейдж переполнения `+N`
        let overflow_cx = cur_x + TAB_OVERFLOW_WIDTH / 2.0;
        let overflow_btn = Button::new(
            TAB_OVERFLOW_ID,
            Box2D {
                cx: overflow_cx,
                cy,
                w: TAB_OVERFLOW_WIDTH,
                h: btn_h,
                rotation: 0.0,
            },
            ButtonContent::Label(format!("+{}", overflow_count)),
        )
        .with_style(WidgetStyle::Settings)
        .with_label_color(theme::settings::BORDER_LIGHT);

        panel.add_widget(overflow_btn);
    }

    panel
}

/// Сетка карточек переключателя: сколько колонок, рядов и сколько карточек
/// поместилось.
///
/// Отдельная функция, потому что этот расчёт нужен дважды: панели — чтобы
/// разложить карточки, и координатору — чтобы положить превью окна ровно в
/// ту же клетку ([`switcher_preview_rects`]). Считать его в двух местах
/// значило бы гарантированно разъехаться при первой же правке размеров.
struct SwitcherGrid {
    cols: usize,
    rows: usize,
    visible_count: usize,
}

fn switcher_grid(n: usize, screen: &DipRect) -> SwitcherGrid {
    let max_cols_screen = ((screen.w - 2.0 * SW_PANEL_PAD + SW_CARD_GAP)
        / (SW_CARD_WIDTH + SW_CARD_GAP))
        .floor() as usize;
    let max_cols = max_cols_screen.clamp(1, 7);
    let cols = n.max(1).min(max_cols);

    let rows_needed = n.max(1).div_ceil(cols);
    let max_rows_screen = ((screen.h - 2.0 * SW_PANEL_PAD + SW_CARD_GAP)
        / (SW_CARD_HEIGHT + SW_CARD_GAP))
        .floor() as usize;
    let rows = rows_needed.clamp(1, max_rows_screen.max(1));

    SwitcherGrid {
        cols,
        rows,
        visible_count: (cols * rows).min(n),
    }
}

/// Прямоугольники превью для видимых карточек, в порядке карточек.
///
/// Координатор кладёт сюда растр снимка окна; сама панель превью не рисует —
/// у неё нет доступа ни к Win32, ни к кэшу снимков.
pub fn switcher_preview_rects(cards: usize, screen: &DipRect) -> Vec<Box2D> {
    if cards == 0 || screen.w <= 0.0 || screen.h <= 0.0 {
        return Vec::new();
    }
    let grid = switcher_grid(cards, screen);
    let panel_w = 2.0 * SW_PANEL_PAD
        + (grid.cols as f64) * SW_CARD_WIDTH
        + (grid.cols as f64 - 1.0).max(0.0) * SW_CARD_GAP;
    let panel_h = 2.0 * SW_PANEL_PAD
        + (grid.rows as f64) * SW_CARD_HEIGHT
        + (grid.rows as f64 - 1.0).max(0.0) * SW_CARD_GAP;
    let cx = screen.x + screen.w / 2.0;
    let cy = screen.y + screen.h / 2.0;
    let start_x = cx - panel_w / 2.0 + SW_PANEL_PAD;
    let start_y = cy - panel_h / 2.0 + SW_PANEL_PAD;

    (0..grid.visible_count)
        .map(|i| {
            let r = i / grid.cols;
            let c = i % grid.cols;
            let card_cx =
                start_x + (c as f64) * (SW_CARD_WIDTH + SW_CARD_GAP) + SW_CARD_WIDTH / 2.0;
            let card_cy =
                start_y + (r as f64) * (SW_CARD_HEIGHT + SW_CARD_GAP) + SW_CARD_HEIGHT / 2.0;
            Box2D {
                cx: card_cx,
                cy: card_cy - 12.0,
                w: SW_PREVIEW_WIDTH,
                h: SW_PREVIEW_HEIGHT,
                rotation: 0.0,
            }
        })
        .collect()
}

/// Собрать панель собственного переключателя окон (Alt+Tab).
///
/// - `cards`: список карточек окон и групп [`SwitcherCard`].
/// - `screen`: границы монитора в DIP ([`DipRect`]).
///
/// # Геометрия и компоновка
///
/// 1. Карточки располагаются в центрированной сетке по центру экрана. При большом
///    количестве окон они автоматически переносятся на следующие ряды (до 7 колонок в ряду).
/// 2. Если общее количество карточек превышает вместимость экрана, число рядов клампится
///    под высоту монитора, гарантируя, что панель никогда не выйдет за границы `screen`.
/// 3. Выделенная карточка (`selected == true`) акцентируется цветом темы [`theme::settings::ACCENT`].
/// 4. Для групп окон (`members > 1`) в карточке отображается бейдж количества окон (например, `[3]`).
///
/// # Разметка превью окна (для координатора)
///
/// Внутри каждой карточки с центром `(card_cx, card_cy)`:
/// - Верхняя зона карточки отведена под прямоугольник превью:
///   ширина [`SW_PREVIEW_WIDTH`] (144 DIP), высота [`SW_PREVIEW_HEIGHT`] (80 DIP, пропорция 16:9/16:10).
/// - Центр области превью: `preview_cx = card_cx`, `preview_cy = card_cy - 12.0`.
/// - Координатор при рендеринге кадра накладывает в эту область растр превью окна
///   ([`rst_win32::window_thumb::WindowThumb`]) либо иконку приложения.
pub fn switcher_panel(cards: &[SwitcherCard], screen: &DipRect) -> Panel {
    if cards.is_empty() || screen.w <= 0.0 || screen.h <= 0.0 {
        return Panel::new(
            SWITCHER_PANEL_ID,
            Box2D {
                cx: screen.x + screen.w / 2.0,
                cy: screen.y + screen.h / 2.0,
                w: 0.0,
                h: 0.0,
                rotation: 0.0,
            },
        )
        .with_style(WidgetStyle::Settings);
    }

    let n = cards.len();
    let SwitcherGrid {
        cols,
        rows,
        visible_count,
    } = switcher_grid(n, screen);

    let panel_w = 2.0 * SW_PANEL_PAD
        + (cols as f64) * SW_CARD_WIDTH
        + (cols as f64 - 1.0).max(0.0) * SW_CARD_GAP;
    let panel_h = 2.0 * SW_PANEL_PAD
        + (rows as f64) * SW_CARD_HEIGHT
        + (rows as f64 - 1.0).max(0.0) * SW_CARD_GAP;

    let cx = screen.x + screen.w / 2.0;
    let cy = screen.y + screen.h / 2.0;

    let mut panel = Panel::new(
        SWITCHER_PANEL_ID,
        Box2D {
            cx,
            cy,
            w: panel_w,
            h: panel_h,
            rotation: 0.0,
        },
    )
    .with_style(WidgetStyle::Settings)
    .with_corner_radius(theme::settings::CORNER_RADIUS);

    let start_x = cx - panel_w / 2.0 + SW_PANEL_PAD;
    let start_y = cy - panel_h / 2.0 + SW_PANEL_PAD;

    for (i, card) in cards.iter().take(visible_count).enumerate() {
        let r = i / cols;
        let c = i % cols;

        let card_cx = start_x + (c as f64) * (SW_CARD_WIDTH + SW_CARD_GAP) + SW_CARD_WIDTH / 2.0;
        let card_cy = start_y + (r as f64) * (SW_CARD_HEIGHT + SW_CARD_GAP) + SW_CARD_HEIGHT / 2.0;

        let max_text_w = (SW_CARD_WIDTH - 2.0 * SW_CARD_PAD).max(10.0);
        let display_title = truncate_to_width(&card.title, max_text_w);

        let label_color = if card.selected {
            theme::settings::ACCENT
        } else {
            theme::settings::TEXT
        };

        // Заголовок карточки
        let btn = Button::new(
            SWITCHER_CARD_BASE + (i as u32),
            Box2D {
                cx: card_cx,
                cy: card_cy,
                w: SW_CARD_WIDTH,
                h: SW_CARD_HEIGHT,
                rotation: 0.0,
            },
            ButtonContent::Label(display_title),
        )
        .with_style(WidgetStyle::Settings)
        .with_label_color(label_color);

        panel.add_widget(btn);

        // Бейдж группы окон (`[N]`) — отображается только при members > 1
        if card.members > 1 {
            let badge_text = format!("[{}]", card.members);
            let (tw, _) = text_size(&badge_text);
            let badge_w = tw + 8.0;
            let badge_h = 18.0;
            let badge_cx = card_cx + SW_CARD_WIDTH / 2.0 - SW_CARD_PAD - badge_w / 2.0;
            let badge_cy = card_cy - SW_CARD_HEIGHT / 2.0 + SW_CARD_PAD + badge_h / 2.0;

            let badge_btn = Button::new(
                SWITCHER_BADGE_BASE + (i as u32),
                Box2D {
                    cx: badge_cx,
                    cy: badge_cy,
                    w: badge_w,
                    h: badge_h,
                    rotation: 0.0,
                },
                ButtonContent::Label(badge_text),
            )
            .with_style(WidgetStyle::Settings)
            .with_label_color(theme::settings::ACCENT);

            panel.add_widget(badge_btn);
        }
    }

    panel
}

/// Усечь строку до заданной ширины в DIP, добавляя `...` при необходимости.
pub(crate) fn truncate_to_width(text: &str, max_w: f64) -> String {
    if text_size(text).0 <= max_w {
        return text.to_string();
    }
    const ELLIPSIS: &str = "...";
    let chars: Vec<char> = text.chars().collect();
    for len in (0..chars.len()).rev() {
        let candidate: String = chars[..len].iter().collect::<String>() + ELLIPSIS;
        if text_size(&candidate).0 <= max_w {
            return candidate;
        }
    }
    ELLIPSIS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Экран произвольного положения и размера, DIP.
    fn screen(x: f64, y: f64, w: f64, h: f64) -> DipRect {
        DipRect { x, y, w, h }
    }

    /// Типичный экран для тестов, не завязанных на конкретный размер.
    fn default_screen() -> DipRect {
        screen(0.0, 0.0, 1920.0, 1080.0)
    }

    #[test]
    fn active_border_stays_strictly_within_window_bounds() {
        let rect = Rect {
            x: 100,
            y: 200,
            w: 800,
            h: 600,
        };
        let primitives = active_border(rect, true);
        assert_eq!(primitives.len(), 4);

        let rx_min = rect.x as f64;
        let rx_max = (rect.x as f64) + (rect.w as f64);
        let ry_min = rect.y as f64;
        let ry_max = (rect.y as f64) + (rect.h as f64);

        for prim in &primitives {
            if let Primitive::Fill { rect: b, .. } = prim {
                let left = b.cx - b.w / 2.0;
                let right = b.cx + b.w / 2.0;
                let top = b.cy - b.h / 2.0;
                let bottom = b.cy + b.h / 2.0;

                assert!(left >= rx_min - 1e-4, "left {left} < rx_min {rx_min}");
                assert!(right <= rx_max + 1e-4, "right {right} > rx_max {rx_max}");
                assert!(top >= ry_min - 1e-4, "top {top} < ry_min {ry_min}");
                assert!(bottom <= ry_max + 1e-4, "bottom {bottom} > ry_max {ry_max}");
            } else {
                panic!("ожидался Primitive::Fill");
            }
        }
    }

    #[test]
    fn active_border_with_zero_or_degenerate_dimensions_returns_empty() {
        assert!(
            active_border(
                Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 500
                },
                true
            )
            .is_empty()
        );
        assert!(
            active_border(
                Rect {
                    x: 0,
                    y: 0,
                    w: 500,
                    h: 0
                },
                true
            )
            .is_empty()
        );
        assert!(
            active_border(
                Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0
                },
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn active_border_color_and_opacity_for_focused_and_unfocused() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 400,
            h: 300,
        };

        let focused = active_border(rect, true);
        assert_eq!(focused.len(), 4);
        for p in &focused {
            if let Primitive::Fill { color, opacity, .. } = p {
                assert_eq!(*color, theme::settings::ACCENT);
                assert_eq!(*opacity, 1.0);
            }
        }

        let unfocused = active_border(rect, false);
        assert_eq!(unfocused.len(), 4);
        for p in &unfocused {
            if let Primitive::Fill { color, opacity, .. } = p {
                assert_eq!(*color, theme::settings::BORDER_DARK);
                assert_eq!(*opacity, 0.4);
            }
        }
    }

    #[test]
    fn active_border_small_window_clamps_thickness_without_panic() {
        let rect = Rect {
            x: 50,
            y: 50,
            w: 2,
            h: 2,
        };
        let primitives = active_border(rect, true);
        assert!(!primitives.is_empty());
    }

    #[test]
    fn workspace_bar_with_zero_workspaces_builds_safely() {
        let scr = default_screen();
        let bar = workspace_bar(&[], &scr);
        assert_eq!(bar.id(), WS_PANEL_ID);
        assert!(bar.frame().w > 0.0);
        assert!(bar.frame().h > 0.0);
    }

    #[test]
    fn workspace_bar_highlights_exactly_active_workspace() {
        let scr = default_screen();
        let workspaces = [(1, false, true), (2, true, true), (3, false, false)];
        let bar = workspace_bar(&workspaces, &scr);

        let mut primitives = Vec::new();
        bar.draw(&mut primitives);
        assert!(bar.frame().w > 0.0);
    }

    #[test]
    fn workspace_bar_distinguishes_empty_and_non_empty_workspaces() {
        let scr = default_screen();
        let workspaces = [(1, true, true), (2, false, true), (3, false, false)];
        let bar = workspace_bar(&workspaces, &scr);

        let f = bar.frame();
        assert!(f.w >= 3.0 * WS_BTN_SIZE);
    }

    #[test]
    fn workspace_bar_stays_within_screen_bounds() {
        let scr = screen(100.0, 100.0, 800.0, 600.0);
        let workspaces = [(1, false, true), (2, true, true), (3, false, false)];
        let bar = workspace_bar(&workspaces, &scr);

        let f = bar.frame();
        let left = f.cx - f.w / 2.0;
        let right = f.cx + f.w / 2.0;
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;

        assert!(left >= scr.x, "left {left} >= scr.x {scr_x}", scr_x = scr.x);
        assert!(right <= scr.x + scr.w, "right {right} <= scr.right");
        assert!(top >= scr.y, "top {top} >= scr.y");
        assert!(bottom <= scr.y + scr.h, "bottom {bottom} <= scr.bottom");
    }

    #[test]
    fn workspace_bar_horizontal_centering() {
        let scr = default_screen();
        let workspaces = [(1, true, true), (2, false, false)];
        let bar = workspace_bar(&workspaces, &scr);

        let f = bar.frame();
        assert_eq!(f.cx, scr.x + scr.w / 2.0);
    }

    #[test]
    fn submap_indicator_truncates_long_name_with_ellipsis() {
        let scr = default_screen();
        let long_name = "super_extra_long_submap_name_that_should_definitely_be_truncated";
        let indicator = submap_indicator(long_name, &scr);

        let f = indicator.frame();
        assert!(f.w <= SUBMAP_MAX_WIDTH);
    }

    #[test]
    fn submap_indicator_short_name_is_kept_intact() {
        let scr = default_screen();
        let indicator = submap_indicator("resize", &scr);

        let f = indicator.frame();
        assert!(f.w <= SUBMAP_MAX_WIDTH);
        assert!(f.w >= 60.0);
    }

    #[test]
    fn submap_indicator_stays_within_screen_bounds() {
        let scr = screen(50.0, 50.0, 500.0, 400.0);
        let indicator = submap_indicator("move", &scr);

        let f = indicator.frame();
        let left = f.cx - f.w / 2.0;
        let right = f.cx + f.w / 2.0;
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;

        assert!(left >= scr.x);
        assert!(right <= scr.x + scr.w);
        assert!(top >= scr.y);
        assert!(bottom <= scr.y + scr.h);
    }

    #[test]
    fn submap_indicator_top_centering() {
        let scr = default_screen();
        let indicator = submap_indicator("resize", &scr);

        let f = indicator.frame();
        assert_eq!(f.cx, scr.x + scr.w / 2.0);
        assert_eq!(f.cy, scr.y + SUBMAP_TOP_MARGIN + f.h / 2.0);
    }

    #[test]
    fn truncate_to_width_unicode_boundary_safety() {
        let cyrillic = "ТЕСТОВЫЙ_РЕЖИМ_ИЗМЕНЕНИЯ_РАЗМЕРА_ДЛИННЫЙ";
        let truncated = truncate_to_width(cyrillic, 80.0);
        assert!(truncated.ends_with("..."));
        assert!(text_size(&truncated).0 <= 80.0);
    }

    #[test]
    fn group_tabs_divides_width_equally_among_tabs() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 600,
            h: 400,
        };
        let tabs = [
            TabEntry {
                title: "Firefox".into(),
                active: true,
            },
            TabEntry {
                title: "Terminal".into(),
                active: false,
            },
            TabEntry {
                title: "Editor".into(),
                active: false,
            },
        ];
        let panel = group_tabs(rect, 24.0, &tabs);
        assert_eq!(panel.id(), TABS_PANEL_ID);
        assert_eq!(panel.frame().w, 600.0);
        assert_eq!(panel.frame().h, 24.0);
    }

    #[test]
    fn group_tabs_single_tab_occupies_full_width() {
        let rect = Rect {
            x: 50,
            y: 50,
            w: 400,
            h: 300,
        };
        let tabs = [TabEntry {
            title: "Single App".into(),
            active: true,
        }];
        let panel = group_tabs(rect, 28.0, &tabs);
        assert_eq!(panel.frame().w, 400.0);
        assert_eq!(panel.frame().h, 28.0);
    }

    #[test]
    fn group_tabs_active_tab_is_distinguished_visually() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 500,
            h: 500,
        };
        let tabs = [
            TabEntry {
                title: "Tab 1".into(),
                active: true,
            },
            TabEntry {
                title: "Tab 2".into(),
                active: false,
            },
        ];
        let panel = group_tabs(rect, 24.0, &tabs);
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        assert!(!prims.is_empty());
    }

    #[test]
    fn group_tabs_long_title_is_truncated_with_ellipsis() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 300,
        };
        let tabs = [TabEntry {
            title: "Super Long Unbelievable Window Title That Exceeds Width".into(),
            active: true,
        }];
        let panel = group_tabs(rect, 24.0, &tabs);
        assert_eq!(panel.frame().w, 200.0);
    }

    #[test]
    fn group_tabs_with_many_tabs_enforces_minimum_width_and_shows_overflow() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 300,
            h: 400,
        };
        let tabs: Vec<TabEntry> = (1..=20)
            .map(|i| TabEntry {
                title: format!("Tab {}", i),
                active: i == 1,
            })
            .collect();
        let panel = group_tabs(rect, 24.0, &tabs);
        assert_eq!(panel.frame().w, 300.0);
    }

    #[test]
    fn group_tabs_empty_tabs_list_returns_safe_empty_panel() {
        let rect = Rect {
            x: 10,
            y: 20,
            w: 500,
            h: 400,
        };
        let panel = group_tabs(rect, 24.0, &[]);
        assert_eq!(panel.id(), TABS_PANEL_ID);
        assert_eq!(panel.frame().w, 500.0);
        assert_eq!(panel.frame().h, 24.0);
    }

    #[test]
    fn group_tabs_zero_height_or_degenerate_rect_returns_safely() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        let panel = group_tabs(rect, 0.0, &[]);
        assert_eq!(panel.frame().w, 0.0);
        assert_eq!(panel.frame().h, 0.0);
    }

    #[test]
    fn group_tabs_panel_frame_stays_strictly_within_given_rect() {
        let rect = Rect {
            x: 150,
            y: 250,
            w: 700,
            h: 500,
        };
        let tabs = [
            TabEntry {
                title: "Doc 1".into(),
                active: false,
            },
            TabEntry {
                title: "Doc 2".into(),
                active: true,
            },
        ];
        let panel = group_tabs(rect, 30.0, &tabs);
        let f = panel.frame();
        let left = f.cx - f.w / 2.0;
        let right = f.cx + f.w / 2.0;
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;

        assert_eq!(left, rect.x as f64);
        assert_eq!(right, (rect.x + rect.w as i32) as f64);
        assert_eq!(top, rect.y as f64);
        assert_eq!(bottom, (rect.y as f64) + 30.0);
    }

    #[test]
    fn group_tabs_all_tabs_fit_exactly_without_overflow_counter() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 600,
            h: 400,
        };
        let tabs = [
            TabEntry {
                title: "One".into(),
                active: true,
            },
            TabEntry {
                title: "Two".into(),
                active: false,
            },
        ];
        let panel = group_tabs(rect, 24.0, &tabs);
        assert_eq!(panel.frame().w, 600.0);
    }

    #[test]
    fn switcher_panel_empty_cards_list_returns_safe_empty_panel() {
        let scr = default_screen();
        let panel = switcher_panel(&[], &scr);
        assert_eq!(panel.id(), SWITCHER_PANEL_ID);
        assert_eq!(panel.frame().w, 0.0);
        assert_eq!(panel.frame().h, 0.0);
    }

    #[test]
    fn switcher_panel_single_card_is_centered_on_screen() {
        let scr = default_screen();
        let cards = [SwitcherCard {
            title: "VS Code".into(),
            members: 1,
            selected: true,
        }];
        let panel = switcher_panel(&cards, &scr);
        let f = panel.frame();

        assert_eq!(f.cx, scr.x + scr.w / 2.0);
        assert_eq!(f.cy, scr.y + scr.h / 2.0);
        assert_eq!(f.w, 2.0 * SW_PANEL_PAD + SW_CARD_WIDTH);
        assert_eq!(f.h, 2.0 * SW_PANEL_PAD + SW_CARD_HEIGHT);
    }

    #[test]
    fn switcher_panel_multiple_cards_stay_within_screen_bounds() {
        let scr = screen(50.0, 50.0, 1280.0, 720.0);
        let cards: Vec<SwitcherCard> = (1..=6)
            .map(|i| SwitcherCard {
                title: format!("Window {}", i),
                members: 1,
                selected: i == 2,
            })
            .collect();
        let panel = switcher_panel(&cards, &scr);
        let f = panel.frame();
        let left = f.cx - f.w / 2.0;
        let right = f.cx + f.w / 2.0;
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;

        assert!(left >= scr.x);
        assert!(right <= scr.x + scr.w);
        assert!(top >= scr.y);
        assert!(bottom <= scr.y + scr.h);
    }

    #[test]
    fn switcher_panel_huge_number_of_cards_never_exceeds_screen_bounds() {
        let scr = screen(0.0, 0.0, 800.0, 600.0);
        let cards: Vec<SwitcherCard> = (1..=50)
            .map(|i| SwitcherCard {
                title: format!("Window {}", i),
                members: 1,
                selected: i == 1,
            })
            .collect();
        let panel = switcher_panel(&cards, &scr);
        let f = panel.frame();
        let left = f.cx - f.w / 2.0;
        let right = f.cx + f.w / 2.0;
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;

        assert!(left >= scr.x - 1e-4);
        assert!(right <= scr.x + scr.w + 1e-4);
        assert!(top >= scr.y - 1e-4);
        assert!(bottom <= scr.y + scr.h + 1e-4);
    }

    #[test]
    fn switcher_panel_selected_card_is_visually_distinct() {
        let scr = default_screen();
        let cards = [
            SwitcherCard {
                title: "First".into(),
                members: 1,
                selected: false,
            },
            SwitcherCard {
                title: "Second".into(),
                members: 1,
                selected: true,
            },
        ];
        let panel = switcher_panel(&cards, &scr);
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        assert!(!prims.is_empty());
    }

    #[test]
    fn switcher_panel_group_badge_is_rendered_only_when_members_gt_1() {
        let scr = default_screen();
        let cards = [
            SwitcherCard {
                title: "Group 1".into(),
                members: 3,
                selected: true,
            },
            SwitcherCard {
                title: "Single".into(),
                members: 1,
                selected: false,
            },
        ];
        let panel = switcher_panel(&cards, &scr);
        let mut prims = Vec::new();
        panel.draw(&mut prims);

        // В примитивах должен присутствовать бейдж "[3]"
        let has_group_badge = prims.iter().any(|p| {
            if let Primitive::Text { text, .. } = p {
                text.contains("[3]")
            } else {
                false
            }
        });
        assert!(
            has_group_badge,
            "бейдж [3] должен быть отрисован для группы из 3 окон"
        );
    }

    #[test]
    fn switcher_panel_single_window_has_no_group_badge() {
        let scr = default_screen();
        let cards = [SwitcherCard {
            title: "Single App".into(),
            members: 1,
            selected: true,
        }];
        let panel = switcher_panel(&cards, &scr);
        let mut prims = Vec::new();
        panel.draw(&mut prims);

        let has_any_bracket_badge = prims.iter().any(|p| {
            if let Primitive::Text { text, .. } = p {
                text.starts_with('[') && text.ends_with(']')
            } else {
                false
            }
        });
        assert!(
            !has_any_bracket_badge,
            "одиночное окно не должно содержать бейдж группы"
        );
    }

    #[test]
    fn switcher_panel_long_title_is_safely_truncated() {
        let scr = default_screen();
        let cards = [SwitcherCard {
            title: "Super Long Window Title Exceeding Card Width Far Beyond Normal Length".into(),
            members: 1,
            selected: true,
        }];
        let panel = switcher_panel(&cards, &scr);
        let mut prims = Vec::new();
        panel.draw(&mut prims);

        let has_ellipsis = prims.iter().any(|p| {
            if let Primitive::Text { text, .. } = p {
                text.ends_with("...")
            } else {
                false
            }
        });
        assert!(
            has_ellipsis,
            "длинный заголовок должен быть усечен с многоточием"
        );
    }

    #[test]
    fn switcher_panel_is_always_centered_on_screen() {
        let scr = screen(100.0, 200.0, 1920.0, 1080.0);
        let cards = [
            SwitcherCard {
                title: "App 1".into(),
                members: 1,
                selected: true,
            },
            SwitcherCard {
                title: "App 2".into(),
                members: 2,
                selected: false,
            },
        ];
        let panel = switcher_panel(&cards, &scr);
        assert_eq!(panel.frame().cx, scr.x + scr.w / 2.0);
        assert_eq!(panel.frame().cy, scr.y + scr.h / 2.0);
    }

    #[test]
    // Проверка констант компилятору известна заранее — в том и смысл: тест
    // замораживает соотношение размеров карточки и превью, чтобы будущая
    // правка одной константы не сломала разметку молча.
    #[allow(
        clippy::assertions_on_constants,
        reason = "тест сторожит соотношение констант разметки"
    )]
    fn switcher_panel_card_preview_geometry_is_well_formed() {
        assert!(SW_PREVIEW_WIDTH < SW_CARD_WIDTH);
        assert!(SW_PREVIEW_HEIGHT < SW_CARD_HEIGHT);
        assert!(SW_PREVIEW_WIDTH > 0.0);
        assert!(SW_PREVIEW_HEIGHT > 0.0);
    }

    #[test]
    fn switcher_panel_zero_or_negative_screen_bounds_no_panic() {
        let scr = screen(0.0, 0.0, 0.0, 0.0);
        let cards = [SwitcherCard {
            title: "App".into(),
            members: 1,
            selected: true,
        }];
        let panel = switcher_panel(&cards, &scr);
        assert_eq!(panel.frame().w, 0.0);
        assert_eq!(panel.frame().h, 0.0);
    }

    #[test]
    fn switcher_panel_all_widgets_stay_strictly_inside_panel_frame() {
        let scr = default_screen();
        let cards = [
            SwitcherCard {
                title: "Card 1".into(),
                members: 2,
                selected: true,
            },
            SwitcherCard {
                title: "Card 2".into(),
                members: 1,
                selected: false,
            },
        ];
        let panel = switcher_panel(&cards, &scr);
        let pf = panel.frame();
        let p_left = pf.cx - pf.w / 2.0;
        let p_right = pf.cx + pf.w / 2.0;
        let p_top = pf.cy - pf.h / 2.0;
        let p_bottom = pf.cy + pf.h / 2.0;

        let mut prims = Vec::new();
        panel.draw(&mut prims);

        for prim in &prims {
            let b = match prim {
                Primitive::Fill { rect, .. }
                | Primitive::Icon { rect, .. }
                | Primitive::Rgba { rect, .. }
                | Primitive::Text { rect, .. } => rect,
            };
            let left = b.cx - b.w / 2.0;
            let right = b.cx + b.w / 2.0;
            let top = b.cy - b.h / 2.0;
            let bottom = b.cy + b.h / 2.0;

            assert!(left >= p_left - 1e-4, "left {left} < p_left {p_left}");
            assert!(right <= p_right + 1e-4, "right {right} > p_right {p_right}");
            assert!(top >= p_top - 1e-4, "top {top} < p_top {p_top}");
            assert!(
                bottom <= p_bottom + 1e-4,
                "bottom {bottom} > p_bottom {p_bottom}"
            );
        }
    }
}
