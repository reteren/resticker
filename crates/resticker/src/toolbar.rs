//! Тулбар выделения (SPEC.md 3.6, docs/M2_UI_NOTES.md §8): сборка панели из
//! виджетов rst-render и её позиционирование. Одиночное выделение — ползунок
//! прозрачности + числовое поле + 6 кнопок; мультивыделение (SPEC 3.6) —
//! только 6 кнопок, ползунок и поле отсутствуют
//! (docs/M2_MULTISELECT_TOOLBAR_NOTES.md, §2).
//!
//! Модуль только конструирует [`Panel`]: роутинг событий, действия кнопок
//! и перерисовка — у ядра редактирования (overlay_manager). Кнопка «Слои
//! видимости» открывает панель выбора окон (SPEC 3.6, п. 3) — её сборка
//! и состояние живут в [`crate::window_picker`] (docs/M4_WINDOW_PICKER_DESIGN.md).

use rst_core::hittest::DipRect;
use rst_render::{Box2D, Button, Icon, NumericField, Panel, Slider, WidgetId, theme};

/// Идентификаторы виджетов тулбара — для опроса состояния ядром через
/// [`Panel::widget`]/[`Panel::widget_mut`].
pub const TB_PANEL: WidgetId = 0;
pub const TB_SLIDER: WidgetId = 1;
pub const TB_FIELD: WidgetId = 2;
pub const TB_LAYERS: WidgetId = 8;
pub const TB_EYE: WidgetId = 3;
pub const TB_ORDER_UP: WidgetId = 4;
pub const TB_ORDER_DOWN: WidgetId = 5;
pub const TB_DUPLICATE: WidgetId = 6;
pub const TB_DELETE: WidgetId = 7;

/// Отступ тулбара от рамки выделения, DIP.
pub const TOOLBAR_GAP_Y: f64 = 8.0;
/// Внутренний отступ панели, DIP.
pub const TOOLBAR_PAD: f64 = 4.0;
/// Зазор между виджетами, DIP.
pub const TOOLBAR_WIDGET_GAP: f64 = 4.0;
/// Ширина ползунка прозрачности, DIP.
pub const TOOLBAR_SLIDER_W: f64 = 96.0;
/// Ширина числового поля, DIP.
pub const TOOLBAR_FIELD_W: f64 = 40.0;
/// Высота тулбара: кнопка + двойной отступ, DIP.
pub const TOOLBAR_HEIGHT: f64 = theme::BUTTON_SIZE + 2.0 * TOOLBAR_PAD;
/// Ширина тулбара в одиночном режиме: отступы + ползунок + поле + 6 кнопок
/// + зазоры, DIP.
pub const TOOLBAR_WIDTH: f64 = 2.0 * TOOLBAR_PAD
    + TOOLBAR_SLIDER_W
    + TOOLBAR_WIDGET_GAP
    + TOOLBAR_FIELD_W
    + TOOLBAR_WIDGET_GAP
    + 6.0 * theme::BUTTON_SIZE
    + 5.0 * TOOLBAR_WIDGET_GAP;
/// Ширина тулбара в мульти-режиме: отступы + только 6 кнопок + зазоры
/// (без слайдера и поля — SPEC 3.6), DIP.
pub const TOOLBAR_WIDTH_MULTI: f64 =
    2.0 * TOOLBAR_PAD + 6.0 * theme::BUTTON_SIZE + 5.0 * TOOLBAR_WIDGET_GAP;

/// Верхняя координата Y тулбара (SPEC 3.6): под рамкой; если снизу до низа
/// экрана не хватает [`TOOLBAR_HEIGHT`] — над рамкой. Третий случай SPEC
/// («прижимается к ближайшему свободному краю», когда нет места и сверху) —
/// задел: сейчас тулбар уходит над рамку даже в отрицательные координаты.
fn toolbar_top(bounds: &DipRect, screen_h: f64) -> f64 {
    let below = bounds.y + bounds.h + TOOLBAR_GAP_Y;
    if below + TOOLBAR_HEIGHT <= screen_h {
        below
    } else {
        bounds.y - TOOLBAR_GAP_Y - TOOLBAR_HEIGHT
    }
}

/// Собрать тулбар выделения: в одиночном режиме (`opacity: Some(v)`) —
/// ползунок прозрачности, зеркалирующее числовое поле и шесть кнопок
/// (SPEC 3.6: слои видимости, глаз, выше, ниже, дублировать, удалить);
/// в мульти-режиме (`opacity: None`) — только шесть кнопок (SPEC 3.6;
/// docs/M2_MULTISELECT_TOOLBAR_NOTES.md, §2).
///
/// `bounds` — ось-выровненный bbox выделения в DIP (для одиночного —
/// aabb рамки выделения, для мульти — union `selection.bounds()`); вычисляет
/// его вызывающий код. `screen_h` — высота монитора в DIP. По горизонтали
/// тулбар центрируется под bbox; зажим по краям экрана — задел вместе
/// с третьим случаем SPEC 3.6.
pub fn build_toolbar(bounds: &DipRect, opacity: Option<f64>, screen_h: f64) -> Panel {
    let width = if opacity.is_some() {
        TOOLBAR_WIDTH
    } else {
        TOOLBAR_WIDTH_MULTI
    };
    let top = toolbar_top(bounds, screen_h);
    let left = bounds.x + bounds.w / 2.0 - width / 2.0;
    let cy = top + TOOLBAR_HEIGHT / 2.0;

    let mut panel = Panel::new(
        TB_PANEL,
        Box2D {
            cx: left + width / 2.0,
            cy,
            w: width,
            h: TOOLBAR_HEIGHT,
            rotation: 0.0,
        },
    );

    let mut x = left + TOOLBAR_PAD;
    if let Some(opacity) = opacity {
        // Прозрачность: модель хранит 0.0–1.0, виджеты — целые проценты.
        let value = (opacity.clamp(0.0, 1.0) * 100.0).round() as u32;

        let mut slider =
            Slider::opacity(TB_SLIDER, x + TOOLBAR_SLIDER_W / 2.0, cy, TOOLBAR_SLIDER_W);
        slider.set_value(value);
        panel.add_widget(slider);
        x += TOOLBAR_SLIDER_W + TOOLBAR_WIDGET_GAP;

        let mut field =
            NumericField::opacity(TB_FIELD, x + TOOLBAR_FIELD_W / 2.0, cy, TOOLBAR_FIELD_W);
        field.set_value(value);
        panel.add_widget(field);
        x += TOOLBAR_FIELD_W + TOOLBAR_WIDGET_GAP;
    }

    let buttons = [
        (TB_LAYERS, Icon::Layers),
        (TB_EYE, Icon::Eye),
        (TB_ORDER_UP, Icon::OrderUp),
        (TB_ORDER_DOWN, Icon::OrderDown),
        (TB_DUPLICATE, Icon::Duplicate),
        (TB_DELETE, Icon::Delete),
    ];
    for (id, icon) in buttons {
        panel.add_widget(Button::icon(id, x + theme::BUTTON_SIZE / 2.0, cy, icon));
        x += theme::BUTTON_SIZE + TOOLBAR_WIDGET_GAP;
    }

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::{Primitive, Widget};

    const SCREEN_H: f64 = 1080.0;

    /// Ось-выровненный bbox по центру и размеру — как его передаёт
    /// вызывающий код (`selection.bounds()` / aabb рамки выделения).
    fn aabb(cx: f64, cy: f64, w: f64, h: f64) -> DipRect {
        DipRect::from_center(cx, cy, w, h)
    }

    /// Центр строки виджетов тулбара (совпадает с cy рамки панели).
    fn toolbar_cy(p: &Panel) -> f64 {
        p.frame().cy
    }

    #[test]
    fn toolbar_below_selection_when_space() {
        // Рамка y ∈ [350, 450]; снизу места достаточно — тулбар под ней.
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(1.0), SCREEN_H);
        assert_eq!(toolbar_cy(&p), 450.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT / 2.0);
        assert!(p.hit_test((960.0, 476.0)));
        assert!(!p.hit_test((960.0, 457.0)), "зазор между рамкой и тулбаром");
    }

    #[test]
    fn toolbar_centered_horizontally_on_bbox() {
        // Рамка x ∈ [860, 1060] → центр 960; рамка тулбара x ∈ [790, 1130].
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(1.0), SCREEN_H);
        assert!(p.hit_test((791.0, 476.0)), "левый край тулбара");
        assert!(!p.hit_test((789.0, 476.0)));
    }

    #[test]
    fn toolbar_above_when_no_space_below() {
        // Рамка y ∈ [1010, 1070]; снизу всего 10 DIP — тулбар над рамкой.
        let p = build_toolbar(&aabb(960.0, 1040.0, 200.0, 60.0), Some(1.0), SCREEN_H);
        assert_eq!(
            toolbar_cy(&p),
            1010.0 - TOOLBAR_GAP_Y - TOOLBAR_HEIGHT / 2.0
        );
        assert!(p.hit_test((960.0, 984.0)));
    }

    #[test]
    fn toolbar_below_boundary_is_inclusive() {
        // Высота экрана ровно «низ рамки + зазор + высота тулбара» — ещё снизу.
        let screen_h = 450.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT;
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(1.0), screen_h);
        assert_eq!(toolbar_cy(&p), 450.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT / 2.0);
        // Один DIP меньше — уже сверху.
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(1.0), screen_h - 1.0);
        assert_eq!(toolbar_cy(&p), 350.0 - TOOLBAR_GAP_Y - TOOLBAR_HEIGHT / 2.0);
    }

    #[test]
    fn toolbar_uses_passed_aabb() {
        // aabb повёрнутого стикера (для 90° поворота 100×40 это 40×100,
        // y ∈ [850, 950]) вычисляет вызывающий код — билдер позиционируется
        // под переданным прямоугольником. Без учёта поворота низ был бы 920
        // и тулбар встал бы на 928+18.
        let p = build_toolbar(&aabb(960.0, 900.0, 40.0, 100.0), Some(1.0), SCREEN_H);
        assert_eq!(toolbar_cy(&p), 950.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT / 2.0);
    }

    #[test]
    fn toolbar_opacity_mirrored_in_slider_and_field() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(0.85), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 85);
        assert_eq!(p.widget::<NumericField>(TB_FIELD).unwrap().value(), 85);
    }

    #[test]
    fn toolbar_opacity_clamped_to_percent_range() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(1.5), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 100);
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(-0.5), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 0);
        // Поле по SPEC 3.6 живёт в 1–100: нулевой стикер зеркалится единицей.
        assert_eq!(p.widget::<NumericField>(TB_FIELD).unwrap().value(), 1);
    }

    #[test]
    fn toolbar_multi_hides_slider_and_field() {
        let p = build_toolbar(&aabb(960.0, 400.0, 400.0, 200.0), None, SCREEN_H);
        assert!(
            p.widget::<Slider>(TB_SLIDER).is_none(),
            "в мульти-режиме слайдера нет (SPEC 3.6)"
        );
        assert!(
            p.widget::<NumericField>(TB_FIELD).is_none(),
            "в мульти-режиме числового поля нет (SPEC 3.6)"
        );
        // Кнопки на месте — панель не пустая.
        for id in [
            TB_LAYERS,
            TB_EYE,
            TB_ORDER_UP,
            TB_ORDER_DOWN,
            TB_DUPLICATE,
            TB_DELETE,
        ] {
            assert!(p.widget::<Button>(id).is_some(), "кнопка {id} есть");
        }
    }

    #[test]
    fn toolbar_multi_is_narrower_by_slider_and_field() {
        // Ширина мульти-тулбара меньше одиночного ровно на ширину слайдера,
        // поля и двух зазоров между ними.
        assert_eq!(
            TOOLBAR_WIDTH - TOOLBAR_WIDTH_MULTI,
            TOOLBAR_SLIDER_W + TOOLBAR_WIDGET_GAP + TOOLBAR_FIELD_W + TOOLBAR_WIDGET_GAP
        );
        let p = build_toolbar(&aabb(960.0, 400.0, 400.0, 200.0), None, SCREEN_H);
        assert_eq!(p.frame().w, TOOLBAR_WIDTH_MULTI);
    }

    fn button_icon(p: &Panel, id: WidgetId) -> Icon {
        let b = p.widget::<Button>(id).unwrap();
        let mut out = Vec::new();
        b.draw(&mut out);
        let Some(Primitive::Icon { icon, .. }) = out.into_iter().nth(1) else {
            panic!("второй примитив кнопки — иконка")
        };
        icon
    }

    #[test]
    fn toolbar_six_buttons_in_spec_order() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), Some(1.0), SCREEN_H);
        let expected = [
            (TB_LAYERS, Icon::Layers),
            (TB_EYE, Icon::Eye),
            (TB_ORDER_UP, Icon::OrderUp),
            (TB_ORDER_DOWN, Icon::OrderDown),
            (TB_DUPLICATE, Icon::Duplicate),
            (TB_DELETE, Icon::Delete),
        ];
        for (id, icon) in expected {
            assert_eq!(button_icon(&p, id), icon);
        }
        // Порядок слева направо: ползунок, поле, затем кнопки.
        let centers = [
            p.widget::<Slider>(TB_SLIDER).unwrap().bounds().cx,
            p.widget::<NumericField>(TB_FIELD).unwrap().bounds().cx,
            p.widget::<Button>(TB_LAYERS).unwrap().bounds().cx,
            p.widget::<Button>(TB_EYE).unwrap().bounds().cx,
            p.widget::<Button>(TB_ORDER_UP).unwrap().bounds().cx,
            p.widget::<Button>(TB_ORDER_DOWN).unwrap().bounds().cx,
            p.widget::<Button>(TB_DUPLICATE).unwrap().bounds().cx,
            p.widget::<Button>(TB_DELETE).unwrap().bounds().cx,
        ];
        assert!(
            centers.windows(2).all(|w| w[0] < w[1]),
            "виджеты слева направо"
        );
    }

    #[test]
    fn toolbar_multi_buttons_in_spec_order_centered_on_bounds() {
        // Union-рамка мультивыделения x ∈ [760, 1160] (центр 960).
        let bounds = aabb(960.0, 400.0, 400.0, 200.0);
        let p = build_toolbar(&bounds, None, SCREEN_H);

        let expected = [
            (TB_LAYERS, Icon::Layers),
            (TB_EYE, Icon::Eye),
            (TB_ORDER_UP, Icon::OrderUp),
            (TB_ORDER_DOWN, Icon::OrderDown),
            (TB_DUPLICATE, Icon::Duplicate),
            (TB_DELETE, Icon::Delete),
        ];
        for (id, icon) in expected {
            assert_eq!(button_icon(&p, id), icon);
        }
        // Тот же порядок слева направо, без слайдера/поля слева.
        let centers: Vec<f64> = [
            TB_LAYERS,
            TB_EYE,
            TB_ORDER_UP,
            TB_ORDER_DOWN,
            TB_DUPLICATE,
            TB_DELETE,
        ]
        .map(|id| p.widget::<Button>(id).unwrap().bounds().cx)
        .to_vec();
        assert!(
            centers.windows(2).all(|w| w[0] < w[1]),
            "кнопки слева направо"
        );
        // Строка кнопок центрирована под bounds.
        let mid = (centers[0] + centers[5]) / 2.0;
        assert!(
            (mid - bounds.x - bounds.w / 2.0).abs() < 1e-9,
            "центр кнопок {mid} != центр bounds {}",
            bounds.x + bounds.w / 2.0
        );
    }
}
