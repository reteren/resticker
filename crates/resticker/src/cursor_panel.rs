//! Панель инструментов у курсора (SPEC.md, раздел 3.8; ROADMAP.md M2;
//! docs/M2_INTEGRATION_PLAN.md, §11 — горизонтальная полоса из 4 кнопок).
//!
//! Чистый билдер: на входе — позиция курсора и границы экрана в DIP, на
//! выходе — собранная [`Panel`] рядом с курсором, целиком на экране.
//! Перетаскивание панели и запоминание позиции (SPEC 3.8), а также действия
//! кнопок — зона координатора: ему доступны [`Panel::translate`] и
//! [`Button::take_click`] по идентификаторам ниже.

use rst_core::hittest::DipRect;
use rst_render::{Box2D, Button, Icon, Panel, WidgetId, theme};

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

/// Смещение панели от курсора вправо-вниз, DIP (не закрывать сам курсор).
pub const CURSOR_OFFSET_DIP: f64 = 12.0;
/// Внутренний отступ панели, DIP.
const PANEL_PAD: f64 = 6.0;
/// Зазор между кнопками, DIP.
const BUTTON_GAP: f64 = 4.0;
/// Число кнопок панели.
const BUTTON_COUNT: f64 = 5.0;

/// Размер панели (ширина, высота), DIP: пять кнопок `theme::BUTTON_SIZE`
/// с зазорами и отступами.
pub const CURSOR_PANEL_SIZE: (f64, f64) = (
    2.0 * PANEL_PAD + BUTTON_COUNT * theme::BUTTON_SIZE + (BUTTON_COUNT - 1.0) * BUTTON_GAP,
    2.0 * PANEL_PAD + theme::BUTTON_SIZE,
);

/// Собрать панель у курсора. `cursor` — позиция курсора в DIP, `screen` —
/// границы монитора в тех же координатах (начало обычно в `(0, 0)`);
/// `all_visible` — текущее состояние «все стикеры видимы»: кнопка-
/// переключатель показывает предстоящее действие (всё видимо → «скрыть»).
///
/// Панель ставится вправо-вниз от курсора на [`CURSOR_OFFSET_DIP`] и
/// зажимается так, чтобы целиком оставаться в `screen`; если экран меньше
/// панели по какой-то оси, панель центрируется по этой оси. Неконечные
/// координаты курсора заменяются центром экрана.
pub fn build_cursor_panel(cursor: (f64, f64), screen: &DipRect, all_visible: bool) -> Panel {
    let (w, h) = CURSOR_PANEL_SIZE;
    let (cx, cy) = clamp_to_screen(
        cursor.0 + CURSOR_OFFSET_DIP,
        cursor.1 + CURSOR_OFFSET_DIP,
        w,
        h,
        screen,
    );
    let frame = Box2D {
        cx,
        cy,
        w,
        h,
        rotation: 0.0,
    };
    let mut panel = Panel::new(CURSOR_PANEL_ID, frame);

    let toggle_icon = if all_visible {
        Icon::HideAll
    } else {
        Icon::ShowAll
    };
    let buttons = [
        (BTN_LOAD_FILE, Icon::FileOpen),
        (BTN_ADD_WINDOW, Icon::Layers),
        (BTN_TOGGLE_ALL, toggle_icon),
        (BTN_SETTINGS, Icon::Settings),
        (BTN_EXIT, Icon::Exit),
    ];
    // Горизонтальная полоса: кнопки по центру панели, слева направо.
    let first_cx = cx - w / 2.0 + PANEL_PAD + theme::BUTTON_SIZE / 2.0;
    for (i, (id, icon)) in buttons.into_iter().enumerate() {
        let bx = first_cx + i as f64 * (theme::BUTTON_SIZE + BUTTON_GAP);
        panel.add_widget(Button::icon(id, bx, cy, icon));
    }
    panel
}

/// Зажать центр прямоугольника `w`×`h` так, чтобы он целиком лежал
/// в `screen`.
fn clamp_to_screen(cx: f64, cy: f64, w: f64, h: f64, screen: &DipRect) -> (f64, f64) {
    let center = (screen.x + screen.w / 2.0, screen.y + screen.h / 2.0);
    if ![cx, cy, w, h, screen.x, screen.y, screen.w, screen.h]
        .iter()
        .all(|v| v.is_finite())
    {
        return center;
    }
    (
        clamp_axis(cx, w, screen.x, screen.x + screen.w),
        clamp_axis(cy, h, screen.y, screen.y + screen.h),
    )
}

/// Одна ось: центр в `[lo + size/2, hi - size/2]`; если прямоугольник не
/// помещается в диапазон — середина диапазона.
fn clamp_axis(center: f64, size: f64, lo: f64, hi: f64) -> f64 {
    if hi - lo <= size {
        (lo + hi) / 2.0
    } else {
        center.clamp(lo + size / 2.0, hi - size / 2.0)
    }
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
    fn panel_offset_in_free_space() {
        let panel = build_cursor_panel((960.0, 540.0), &screen(), true);
        let f = panel.frame();
        let (w, h) = CURSOR_PANEL_SIZE;
        // Вдали от краёв — ровно смещение от курсора, без зажимания.
        assert_close(f.cx, 960.0 + CURSOR_OFFSET_DIP, "cx");
        assert_close(f.cy, 540.0 + CURSOR_OFFSET_DIP, "cy");
        assert_close(f.w, w, "w");
        assert_close(f.h, h, "h");
        assert_close(f.rotation, 0.0, "rotation");
        assert_eq!(panel.id(), CURSOR_PANEL_ID);
    }

    #[test]
    fn buttons_layout_and_ids() {
        let panel = build_cursor_panel((960.0, 540.0), &screen(), true);
        let f = panel.frame();
        let ids = [
            BTN_LOAD_FILE,
            BTN_ADD_WINDOW,
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
            assert_close(b.w, theme::BUTTON_SIZE, "button w");
            assert_close(b.h, theme::BUTTON_SIZE, "button h");
            assert_close(b.cy, f.cy, "button cy == panel cy");
            assert!(b.cx > prev_cx, "кнопки упорядочены слева направо");
            prev_cx = b.cx;
            // Кнопка целиком внутри рамки панели.
            assert!(b.cx - b.w / 2.0 >= f.cx - f.w / 2.0, "кнопка не левее");
            assert!(b.cx + b.w / 2.0 <= f.cx + f.w / 2.0, "кнопка не правее");
        }
    }

    #[test]
    fn icons_match_spec_and_toggle_state() {
        // Всё видимо → кнопка-переключатель предлагает «скрыть все».
        let panel = build_cursor_panel((960.0, 540.0), &screen(), true);
        assert_eq!(
            icons(&panel),
            vec![
                Icon::FileOpen,
                Icon::Layers,
                Icon::HideAll,
                Icon::Settings,
                Icon::Exit
            ]
        );
        // Часть скрыта → предлагает «показать все».
        let panel = build_cursor_panel((960.0, 540.0), &screen(), false);
        assert_eq!(
            icons(&panel),
            vec![
                Icon::FileOpen,
                Icon::Layers,
                Icon::ShowAll,
                Icon::Settings,
                Icon::Exit
            ]
        );
    }

    #[test]
    fn clamps_at_screen_edges_table() {
        let (w, h) = CURSOR_PANEL_SIZE;
        let cases: [((f64, f64), (f64, f64)); 5] = [
            // (курсор, ожидаемый центр панели): левый, правый, верхний,
            // нижний край и угол.
            ((5.0, 540.0), (w / 2.0, 540.0 + CURSOR_OFFSET_DIP)),
            (
                (1915.0, 540.0),
                (1920.0 - w / 2.0, 540.0 + CURSOR_OFFSET_DIP),
            ),
            ((960.0, 2.0), (960.0 + CURSOR_OFFSET_DIP, h / 2.0)),
            (
                (960.0, 1075.0),
                (960.0 + CURSOR_OFFSET_DIP, 1080.0 - h / 2.0),
            ),
            ((1915.0, 1075.0), (1920.0 - w / 2.0, 1080.0 - h / 2.0)),
        ];
        for (cursor, (ex, ey)) in cases {
            let f = build_cursor_panel(cursor, &screen(), true).frame();
            assert_close(f.cx, ex, &format!("{cursor:?} cx"));
            assert_close(f.cy, ey, &format!("{cursor:?} cy"));
            // Панель целиком на экране.
            assert!(f.cx - f.w / 2.0 >= 0.0, "{cursor:?} левый край");
            assert!(f.cx + f.w / 2.0 <= 1920.0, "{cursor:?} правый край");
            assert!(f.cy - f.h / 2.0 >= 0.0, "{cursor:?} верхний край");
            assert!(f.cy + f.h / 2.0 <= 1080.0, "{cursor:?} нижний край");
        }
    }

    #[test]
    fn clamps_with_nonzero_origin() {
        // Границы с ненулевым началом (обобщение под M3, координаты монитора).
        let screen = DipRect::new(100.0, 50.0, 1920.0, 1080.0);
        let (w, h) = CURSOR_PANEL_SIZE;
        let f = build_cursor_panel((105.0, 55.0), &screen, true).frame();
        assert_close(f.cx, 100.0 + w / 2.0, "левый край у начала");
        assert_close(f.cy, 50.0 + h / 2.0, "верхний край у начала");
        let f = build_cursor_panel((2010.0, 1125.0), &screen, true).frame();
        assert_close(f.cx, 100.0 + 1920.0 - w / 2.0, "правый край");
        assert_close(f.cy, 50.0 + 1080.0 - h / 2.0, "нижний край");
    }

    #[test]
    fn tiny_screen_centers_panel() {
        // Экран меньше панели по обеим осям: центрируем (вылезает
        // симметрично с двух сторон — лучшего варианта нет).
        let screen = DipRect::new(0.0, 0.0, 100.0, 30.0);
        let f = build_cursor_panel((10.0, 10.0), &screen, true).frame();
        assert_close(f.cx, 50.0, "cx по центру крошечного экрана");
        assert_close(f.cy, 15.0, "cy по центру крошечного экрана");
    }

    #[test]
    fn nan_cursor_falls_back_to_screen_center() {
        let f = build_cursor_panel((f64::NAN, 540.0), &screen(), true).frame();
        assert_close(f.cx, 960.0, "NaN → центр экрана x");
        assert_close(f.cy, 540.0, "NaN → центр экрана y");
        assert!(f.cx.is_finite() && f.cy.is_finite());
    }

    #[test]
    fn panel_stays_on_screen_grid() {
        // Свойство: ни одна позиция курсора не выводит панель за край.
        let xs = [-100.0, 0.0, 1.0, 500.0, 1919.0, 1920.0, 2100.0];
        let ys = [-100.0, 0.0, 1.0, 300.0, 1079.0, 1080.0, 2100.0];
        for x in xs {
            for y in ys {
                let f = build_cursor_panel((x, y), &screen(), true).frame();
                assert!(
                    f.cx - f.w / 2.0 >= 0.0 && f.cx + f.w / 2.0 <= 1920.0,
                    "({x}, {y}): панель вылезла по горизонтали"
                );
                assert!(
                    f.cy - f.h / 2.0 >= 0.0 && f.cy + f.h / 2.0 <= 1080.0,
                    "({x}, {y}): панель вылезла по вертикали"
                );
            }
        }
    }
}
