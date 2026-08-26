//! Панель инструментов режима редактирования (SPEC.md, раздел 3.8;
//! ROADMAP.md M2) — горизонтальная полоса кнопок.
//!
//! Чистый билдер: на входе — границы экрана в DIP и два флага состояния, на
//! выходе — собранная [`Panel`]. Действия кнопок — зона координатора, ему
//! доступен [`Button::take_click`] по идентификаторам ниже.
//!
//! С 2026-08-23 панель НЕ ходит за курсором: она прижата к низу экрана по
//! центру (запрос пользователя — «перемести его в низ центра экрана»), а её
//! кнопки вдвое крупнее прежних. Имя модуля и идентификаторы (`CURSOR_*`)
//! остались прежними: их знает конфиг, история и полсотни мест в
//! координаторе — переименование не дало бы ничего, кроме шума в диффе.
//!
//! Пока выделен стикер, панель уезжает вниз за край экрана и оставляет
//! видимой полоску в [`PEEK_DIP`] (второй запрос пользователя): она стоит
//! ровно там, куда тянут стикеры и где всплывает тулбар выделения, и в
//! развёрнутом виде мешала бы. Наведение курсора на полоску возвращает
//! панель целиком — решение о том, свёрнута она или нет, принимает
//! координатор и передаёт сюда флагом.

use rst_core::hittest::DipRect;
use rst_render::{Box2D, Button, ButtonContent, Icon, Panel, WidgetId, WidgetStyle, theme};

/// Идентификатор панели у курсора. Диапазон 100+; тулбар стикера
/// (SPEC 3.6) получит свой диапазон отдельно.
pub const CURSOR_PANEL_ID: WidgetId = 100;
/// «Загрузить файл» — диалог добавления стикера.
pub const BTN_LOAD_FILE: WidgetId = 101;
/// «Показать/скрыть все стикеры» — глобальный переключатель.
pub const BTN_TOGGLE_ALL: WidgetId = 102;
/// «Открыть настройки».
pub const BTN_SETTINGS: WidgetId = 103;
/// «Выйти из режима редактирования».
pub const BTN_EXIT: WidgetId = 104;
/// «Добавить окно» — режим выбора окна для стикера-окна (SPEC.md §5.1,
/// ROADMAP.md M6). Переиспользует [`Icon::Layers`] («слои видимости» —
/// та же тема «окна»): отдельная выделенная иконка «добавить окно» —
/// известное упрощение, не блокирует функциональность.
pub const BTN_ADD_WINDOW: WidgetId = 105;
/// «Пресеты» — быстрое переключение (M7, SPEC.md §3.8 «Загрузить пресет»;
/// ROADMAP.md — «быстрое переключение… из панели редактирования»). Пустой
/// список пресетов не открывает панель (см. `handle_cursor_panel_up`) —
/// кнопка всегда на месте, реагирует иначе только по клику.
pub const BTN_PRESETS: WidgetId = 106;
/// «Группы окон» — менеджер групп (запрос пользователя 2026-08-25: «кнопка в
/// режиме редактирования, по которой вылезет менеджер окон»). Открывает
/// панель `group_manager.rs` — список групп с их составом.
pub const BTN_GROUPS: WidgetId = 107;

/// Сторона кнопки панели, DIP — вдвое больше кнопки тулбара выделения
/// (запрос пользователя 2026-08-23: «увеличь его размер в 2 раза»). Своя
/// константа, а не `theme::BUTTON_SIZE`: тулбар стикера остаётся прежним,
/// он живёт вплотную к стикеру и от размера кнопок там зависит вся
/// раскладка.
pub const BUTTON_SIZE: f64 = 2.0 * theme::BUTTON_SIZE;
/// Внутренний отступ панели, DIP.
const PANEL_PAD: f64 = 12.0;
/// Зазор между кнопками, DIP.
const BUTTON_GAP: f64 = 8.0;
/// Число кнопок панели: открыть файл, добавить окно, пресеты, группы,
/// показать/скрыть все, настройки, выход.
const BUTTON_COUNT: f64 = 7.0;
/// Отступ панели от нижнего края экрана, DIP.
pub const BOTTOM_MARGIN_DIP: f64 = 16.0;
/// Сколько DIP панели видно, когда она свёрнута (выделен стикер).
pub const PEEK_DIP: f64 = 10.0;
/// Насколько зона наведения выходит за видимую полоску, DIP (запрос
/// пользователя 2026-08-23: «чтобы менюшка вылетала за 7 пикселей до
/// вытаскивания»). Тот же запас держит панель развёрнутой, когда курсор
/// чуть съехал с её края.
pub const HOVER_MARGIN_DIP: f64 = 7.0;

/// Размер панели (ширина, высота), DIP: [`BUTTON_COUNT`] кнопок
/// [`BUTTON_SIZE`] с зазорами и отступами.
pub const CURSOR_PANEL_SIZE: (f64, f64) = (
    2.0 * PANEL_PAD + BUTTON_COUNT * BUTTON_SIZE + (BUTTON_COUNT - 1.0) * BUTTON_GAP,
    2.0 * PANEL_PAD + BUTTON_SIZE,
);

/// Центр панели (DIP) на экране `screen` при степени раскрытия `progress`
/// (`0` — свёрнута в полоску, `1` — целиком на экране; промежуточные
/// значения — кадры анимации).
///
/// Развёрнутая стоит по центру внизу с отступом [`BOTTOM_MARGIN_DIP`];
/// свёрнутая уезжает за нижний край так, что сверху остаётся ровно
/// [`PEEK_DIP`]. Если экран уже панели — она всё равно центрируется по
/// горизонтали (обрезать нечего, лучше симметрично).
pub fn panel_center(screen: &DipRect, progress: f64) -> (f64, f64) {
    let (_, h) = CURSOR_PANEL_SIZE;
    let bottom = screen.y + screen.h;
    let cx = screen.x + screen.w / 2.0;
    let hidden = bottom - PEEK_DIP + h / 2.0;
    let shown = bottom - BOTTOM_MARGIN_DIP - h / 2.0;
    let t = progress.clamp(0.0, 1.0);
    (cx, hidden + (shown - hidden) * t)
}

/// Полоса-«язычок» свёрнутой панели (DIP): по ней координатор понимает, что
/// курсор навёлся и панель пора развернуть. Выходит за видимую часть на
/// [`HOVER_MARGIN_DIP`] вверх и в стороны — панель встречает курсор чуть
/// раньше, чем он доедет до самой полоски.
pub fn peek_hot_zone(screen: &DipRect) -> Box2D {
    let (w, _) = CURSOR_PANEL_SIZE;
    let hot_h = PEEK_DIP + HOVER_MARGIN_DIP;
    Box2D {
        cx: screen.x + screen.w / 2.0,
        cy: screen.y + screen.h - hot_h / 2.0,
        w: w + 2.0 * HOVER_MARGIN_DIP,
        h: hot_h,
        rotation: 0.0,
    }
}

/// Собрать панель. `screen` — границы монитора в DIP (начало обычно в
/// `(0, 0)`); `all_visible` — текущее состояние «все стикеры видимы»
/// (кнопка-переключатель показывает предстоящее действие: всё видимо →
/// «скрыть»); `progress` — степень раскрытия, см. [`panel_center`].
pub fn build_cursor_panel(screen: &DipRect, all_visible: bool, progress: f64) -> Panel {
    let (w, h) = CURSOR_PANEL_SIZE;
    let (cx, cy) = panel_center(screen, progress);
    let frame = Box2D {
        cx,
        cy,
        w,
        h,
        rotation: 0.0,
    };
    // Оформление — стилистика окна настроек (Source VGUI), как у тулбара
    // стикера и панели свойств закреплённого окна (запрос пользователя
    // 2026-08-23): весь UI поверх экрана читается как одно окно продукта.
    let mut panel = Panel::new(CURSOR_PANEL_ID, frame)
        .with_style(WidgetStyle::Settings)
        .with_corner_radius(theme::settings::CORNER_RADIUS);

    let toggle_icon = if all_visible {
        Icon::HideAll
    } else {
        Icon::ShowAll
    };
    let buttons = [
        (BTN_LOAD_FILE, Icon::FileOpen),
        (BTN_ADD_WINDOW, Icon::Layers),
        (BTN_PRESETS, Icon::PresetLoad),
        (BTN_GROUPS, Icon::Groups),
        (BTN_TOGGLE_ALL, toggle_icon),
        (BTN_SETTINGS, Icon::Settings),
        (BTN_EXIT, Icon::Exit),
    ];
    // Горизонтальная полоса: кнопки по центру панели, слева направо.
    let first_cx = cx - w / 2.0 + PANEL_PAD + BUTTON_SIZE / 2.0;
    for (i, (id, icon)) in buttons.into_iter().enumerate() {
        let bx = first_cx + i as f64 * (BUTTON_SIZE + BUTTON_GAP);
        panel.add_widget(
            Button::new(
                id,
                Box2D {
                    cx: bx,
                    cy,
                    w: BUTTON_SIZE,
                    h: BUTTON_SIZE,
                    rotation: 0.0,
                },
                ButtonContent::Icon(icon),
            )
            .with_style(WidgetStyle::Settings),
        );
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::{Primitive, Widget};

    fn screen() -> DipRect {
        DipRect::new(0.0, 0.0, 1920.0, 1080.0)
    }

    fn assert_close(actual: f64, expected: f64, ctx: &str) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "{ctx}: ожидалось {expected}, получено {actual}"
        );
    }

    /// Иконки кнопок в порядке отрисовки (кнопки — после фона панели).
    fn icons(panel: &Panel) -> Vec<Icon> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.iter()
            .filter_map(|p| match p {
                Primitive::Icon { icon, .. } => Some(*icon),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn panel_sits_at_bottom_center_of_the_screen() {
        let scr = screen();
        let panel = build_cursor_panel(&scr, true, 1.0);
        let f = panel.frame();
        let (w, h) = CURSOR_PANEL_SIZE;
        assert_close(f.cx, scr.x + scr.w / 2.0, "cx по центру экрана");
        assert_close(
            f.cy + h / 2.0,
            scr.y + scr.h - BOTTOM_MARGIN_DIP,
            "низ панели — на BOTTOM_MARGIN_DIP от края",
        );
        assert_close(f.w, w, "w");
        assert_close(f.h, h, "h");
        assert_eq!(panel.id(), CURSOR_PANEL_ID);
    }

    #[test]
    fn panel_follows_screen_origin_on_second_monitor() {
        // Монитор со смещённым началом координат: якорь считается от его
        // собственных границ, а не от нуля.
        let scr = DipRect::new(-1920.0, 357.0, 1920.0, 1080.0);
        let f = build_cursor_panel(&scr, true, 1.0).frame();
        let (_, h) = CURSOR_PANEL_SIZE;
        assert_close(f.cx, scr.x + scr.w / 2.0, "cx");
        assert_close(f.cy + h / 2.0, scr.y + scr.h - BOTTOM_MARGIN_DIP, "низ");
    }

    #[test]
    fn collapsed_panel_leaves_only_the_peek_strip_on_screen() {
        let scr = screen();
        let f = build_cursor_panel(&scr, true, 0.0).frame();
        let (_, h) = CURSOR_PANEL_SIZE;
        let top = f.cy - h / 2.0;
        assert_close(top, scr.y + scr.h - PEEK_DIP, "видно ровно PEEK_DIP");
        assert!(
            f.cy + h / 2.0 > scr.y + scr.h,
            "остальная часть панели — за нижним краем экрана"
        );
    }

    #[test]
    fn peek_hot_zone_covers_the_visible_strip_and_panel_width() {
        let scr = screen();
        let zone = peek_hot_zone(&scr);
        let (w, _) = CURSOR_PANEL_SIZE;
        assert_close(
            zone.w,
            w + 2.0 * HOVER_MARGIN_DIP,
            "полоса шире панели на запас с каждой стороны",
        );
        assert_close(
            zone.cy + zone.h / 2.0,
            scr.y + scr.h,
            "полоса прижата к краю",
        );
        assert!(
            zone.h >= PEEK_DIP,
            "в полоску нужно попадать мышью: {} < {PEEK_DIP}",
            zone.h
        );
        // Точка внутри видимой полоски свёрнутой панели попадает в зону.
        let f = build_cursor_panel(&scr, true, 0.0).frame();
        let strip_y = f.cy - CURSOR_PANEL_SIZE.1 / 2.0 + PEEK_DIP / 2.0;
        assert!(rst_render::box_contains(&zone, (f.cx, strip_y)));
    }

    #[test]
    fn buttons_are_twice_the_toolbar_size_and_ordered() {
        let panel = build_cursor_panel(&screen(), true, 1.0);
        let f = panel.frame();
        let ids = [
            BTN_LOAD_FILE,
            BTN_ADD_WINDOW,
            BTN_PRESETS,
            BTN_GROUPS,
            BTN_TOGGLE_ALL,
            BTN_SETTINGS,
            BTN_EXIT,
        ];
        let mut prev_cx = f64::NEG_INFINITY;
        for id in ids {
            let b = panel
                .widget::<Button>(id)
                .unwrap_or_else(|| panic!("кнопка {id} должна существовать"))
                .bounds();
            assert_close(b.w, 2.0 * theme::BUTTON_SIZE, "button w");
            assert_close(b.h, 2.0 * theme::BUTTON_SIZE, "button h");
            assert_close(b.cy, f.cy, "button cy == panel cy");
            assert!(b.cx > prev_cx, "кнопки упорядочены слева направо");
            prev_cx = b.cx;
            // Кнопка целиком внутри рамки панели.
            assert!(b.cx - b.w / 2.0 >= f.cx - f.w / 2.0, "кнопка не левее");
            assert!(b.cx + b.w / 2.0 <= f.cx + f.w / 2.0, "кнопка не правее");
        }
    }

    #[test]
    fn buttons_move_with_the_panel_when_collapsed() {
        // Кнопки уезжают вместе с рамкой — иначе они остались бы висеть
        // посреди экрана без панели под ними.
        let scr = screen();
        let open = build_cursor_panel(&scr, true, 1.0);
        let hidden = build_cursor_panel(&scr, true, 0.0);
        let dy = hidden.frame().cy - open.frame().cy;
        assert!(dy > 0.0, "свёрнутая панель ниже развёрнутой");
        for id in [BTN_LOAD_FILE, BTN_EXIT] {
            let a = open.widget::<Button>(id).unwrap().bounds();
            let b = hidden.widget::<Button>(id).unwrap().bounds();
            assert_close(b.cy - a.cy, dy, "кнопка {id} съехала вместе с панелью");
            assert_close(b.cx, a.cx, "по горизонтали кнопка не двигается");
        }
    }

    #[test]
    fn icons_match_spec_and_toggle_state() {
        // Всё видимо → кнопка-переключатель предлагает «скрыть все».
        let panel = build_cursor_panel(&screen(), true, 1.0);
        assert_eq!(
            icons(&panel),
            vec![
                Icon::FileOpen,
                Icon::Layers,
                Icon::PresetLoad,
                Icon::Groups,
                Icon::HideAll,
                Icon::Settings,
                Icon::Exit
            ]
        );
        // Часть скрыта → предлагает «показать все».
        let panel = build_cursor_panel(&screen(), false, 1.0);
        assert_eq!(
            icons(&panel),
            vec![
                Icon::FileOpen,
                Icon::Layers,
                Icon::PresetLoad,
                Icon::Groups,
                Icon::ShowAll,
                Icon::Settings,
                Icon::Exit
            ]
        );
    }

    #[test]
    fn tiny_screen_still_centers_the_panel() {
        // Экран уже панели: обрезать нечего, но по горизонтали она обязана
        // остаться симметричной.
        let scr = DipRect::new(0.0, 0.0, 200.0, 200.0);
        let f = build_cursor_panel(&scr, true, 1.0).frame();
        assert_close(f.cx, 100.0, "cx по центру узкого экрана");
    }

    #[test]
    fn half_open_panel_sits_between_the_two_states() {
        // Кадр анимации: панель ровно посередине между свёрнутым и
        // развёрнутым положением — по нему видно, что выезд непрерывен, а
        // не переключается двумя состояниями.
        let scr = screen();
        let hidden = build_cursor_panel(&scr, true, 0.0).frame().cy;
        let shown = build_cursor_panel(&scr, true, 1.0).frame().cy;
        let half = build_cursor_panel(&scr, true, 0.5).frame().cy;
        assert_close(half, (hidden + shown) / 2.0, "середина выезда");
        // Значения вне диапазона зажимаются — анимация не выкинет панель
        // за пределы своих же двух положений.
        assert_close(
            build_cursor_panel(&scr, true, -3.0).frame().cy,
            hidden,
            "clamp снизу",
        );
        assert_close(
            build_cursor_panel(&scr, true, 9.0).frame().cy,
            shown,
            "clamp сверху",
        );
    }

    #[test]
    fn hot_zone_reaches_above_the_strip_by_the_hover_margin() {
        // Панель обязана «встречать» курсор заранее (запрос пользователя:
        // за ~7 пикселей до полоски).
        let scr = screen();
        let zone = peek_hot_zone(&scr);
        let strip_top = scr.y + scr.h - PEEK_DIP;
        assert!(
            zone.cy - zone.h / 2.0 <= strip_top - HOVER_MARGIN_DIP + 1e-9,
            "зона наведения не поднимается на HOVER_MARGIN_DIP над полоской"
        );
        // Точка на HOVER_MARGIN_DIP выше полоски уже считается наведением.
        assert!(rst_render::box_contains(
            &zone,
            (zone.cx, strip_top - HOVER_MARGIN_DIP + 0.5)
        ));
        // А заметно выше — уже нет.
        assert!(!rst_render::box_contains(
            &zone,
            (zone.cx, strip_top - HOVER_MARGIN_DIP - 2.0)
        ));
    }
}
