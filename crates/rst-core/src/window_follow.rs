//! Следование стикера за закреплённым окном (ROADMAP.md M6, «Перемещение
//! и ресайз в пределах правил окна»): чистая геометрия — пересчёт
//! `Placement` стикера, когда прямоугольник целевого окна изменился.
//!
//! Стикер закреплён за окном (примитив — в `rst_win32::window_pin`, M6):
//! пока окно стоит на месте, `Placement` стикера остаётся как есть; при
//! перемещении/ресайзе окна доли стикера от прямоугольника окна
//! (положение центра и размер) сохраняются — [`follow_window`] применяет
//! те же доли к новому прямоугольнику.
//!
//! Все входы — в одном координатном пространстве: DIP относительно начала
//! монитора стикера (ADR-010). Прямоугольники сюда приходят уже
//! переведёнными координатором из физических экранных координат трекера
//! (`rst_win32::window_tracker`) — Win32-зависимостей модуль не имеет и
//! юнит-тестируется на любой ОС (CONTRIBUTING.md, «Правило зависимостей»).

use crate::hittest::DipRect;
use crate::model::Placement;

/// Доли стикера от прямоугольника окна: центр по каждой оси (доля ширины/
/// высоты окна) и размер по каждой оси (доля от размера окна).
#[derive(Debug, Clone, Copy, PartialEq)]
struct WindowFractions {
    fx: f64,
    fy: f64,
    fw: f64,
    fh: f64,
}

/// Прямоугольник пригоден для геометрии: размеры конечны и положительны
/// (NaN/бесконечность/ноль/отрицательная сторона — вырождение, долей
/// не существует).
fn is_valid_rect(rect: DipRect) -> bool {
    rect.w > 0.0 && rect.h > 0.0 && rect.w.is_finite() && rect.h.is_finite()
}

/// Доли [`sticker`] от [`anchor`]: центр — как доля ширины/высоты окна
/// (0 — левый/верхний край, 1 — правый/нижний), размер — как доля
/// размеров окна. Вырожденный прямоугольник — `None`.
fn fractions(anchor: DipRect, sticker: &Placement) -> Option<WindowFractions> {
    if !is_valid_rect(anchor) {
        return None;
    }
    Some(WindowFractions {
        fx: (sticker.cx - anchor.x) / anchor.w,
        fy: (sticker.cy - anchor.y) / anchor.h,
        fw: sticker.w / anchor.w,
        fh: sticker.h / anchor.h,
    })
}

/// Пересчитать `Placement` закреплённого стикера под новый прямоугольник
/// окна.
///
/// `sticker_at_anchor` — текущее размещение стикера, соответствующее
/// окну в `anchor_rect`. Доли стикера от `anchor_rect` (центр по каждой
/// оси и размер) применяются к `new_rect`: центр сохраняет относительную
/// позицию, размер — пропорцию от размеров окна при любом перемещении/
/// ресайзе. `monitor_id` стикера не меняется.
///
/// Вырожденный прямоугольник (нулевой/отрицательный/не-конечный размер)
/// с любой стороны — стикер возвращается без изменений: доли не
/// определены, а новый прямоугольник не даёт осмысленной геометрии
/// (окно в процессе уничтожения — его судьбу решает координатор по
/// событиям `rst_win32::window_pin`).
pub fn follow_window(
    anchor_rect: DipRect,
    new_rect: DipRect,
    sticker_at_anchor: &Placement,
) -> Placement {
    let Some(f) = fractions(anchor_rect, sticker_at_anchor) else {
        return sticker_at_anchor.clone();
    };
    if !is_valid_rect(new_rect) {
        return sticker_at_anchor.clone();
    }
    Placement {
        monitor_id: sticker_at_anchor.monitor_id.clone(),
        cx: new_rect.x + f.fx * new_rect.w,
        cy: new_rect.y + f.fy * new_rect.h,
        w: f.fw * new_rect.w,
        h: f.fh * new_rect.h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement_at(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            cx,
            cy,
            w,
            h,
            ..Placement::default()
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "ожидалось {expected}, получено {actual}"
        );
    }

    fn assert_placement(p: &Placement, cx: f64, cy: f64, w: f64, h: f64) {
        assert_close(p.cx, cx);
        assert_close(p.cy, cy);
        assert_close(p.w, w);
        assert_close(p.h, h);
    }

    #[test]
    fn move_without_resize_preserves_center_and_size() {
        // Стикер в центре окна: доли 0.5/0.5 по центру, размер — константа
        // долей размеров окна (окно не менялось — размер стикера тот же).
        let anchor = DipRect::new(100.0, 100.0, 200.0, 150.0);
        let sticker = placement_at(200.0, 175.0, 40.0, 20.0);
        let new = DipRect::new(300.0, 50.0, 200.0, 150.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 400.0, 125.0, 40.0, 20.0);
    }

    #[test]
    fn move_preserves_off_center_position() {
        // Центр на 30% ширины и 40% высоты окна — сохраняется при сдвиге.
        let anchor = DipRect::new(0.0, 0.0, 100.0, 100.0);
        let sticker = placement_at(30.0, 40.0, 20.0, 20.0);
        let new = DipRect::new(50.0, 50.0, 100.0, 100.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 80.0, 90.0, 20.0, 20.0);
    }

    #[test]
    fn resize_without_move_scales_size_and_center() {
        // Окно выросло с 200x100 до 400x300: центр (0.5/0.5) и размер
        // (0.2/0.2 от окна) сохраняют доли.
        let anchor = DipRect::new(0.0, 0.0, 200.0, 100.0);
        let sticker = placement_at(100.0, 50.0, 40.0, 20.0);
        let new = DipRect::new(0.0, 0.0, 400.0, 300.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 200.0, 150.0, 80.0, 60.0);
    }

    #[test]
    fn resize_and_move_together() {
        // Доли 0.3/0.4 по центру, 0.2/0.3 по размеру — применяются
        // к новому прямоугольнику целиком.
        let anchor = DipRect::new(0.0, 0.0, 100.0, 100.0);
        let sticker = placement_at(30.0, 40.0, 20.0, 30.0);
        let new = DipRect::new(50.0, 100.0, 200.0, 200.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 110.0, 180.0, 40.0, 60.0);
    }

    #[test]
    fn sticker_edge_aligned_stays_edge_aligned() {
        // Центр стикера ровно на левом/верхнем крае окна (доля 0) —
        // при сдвиге остаётся на крае.
        let anchor = DipRect::new(10.0, 20.0, 100.0, 80.0);
        let sticker = placement_at(10.0, 20.0, 30.0, 30.0);
        let new = DipRect::new(500.0, 300.0, 120.0, 90.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 500.0, 300.0, 36.0, 33.75);
    }

    #[test]
    fn sticker_beyond_window_bounds_keeps_fraction() {
        // Стикер правее/ниже окна (доля > 1) — пропорция сохраняется.
        let anchor = DipRect::new(0.0, 0.0, 100.0, 100.0);
        let sticker = placement_at(150.0, 0.0, 40.0, 20.0);
        let new = DipRect::new(0.0, 0.0, 200.0, 200.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 300.0, 0.0, 80.0, 40.0);
    }

    #[test]
    fn sticker_bigger_than_window_scales_proportionally() {
        // Стикер шире окна (доля 1.5) — остаётся в 1.5 раза шире.
        let anchor = DipRect::new(0.0, 0.0, 100.0, 100.0);
        let sticker = placement_at(50.0, 50.0, 150.0, 50.0);
        let new = DipRect::new(0.0, 0.0, 300.0, 100.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, 150.0, 50.0, 450.0, 50.0);
    }

    #[test]
    fn negative_anchor_origin_is_supported() {
        // Начало координат может быть отрицательным (DIP относительно
        // монитора на мультимониторных раскладках).
        let anchor = DipRect::new(-200.0, -100.0, 100.0, 80.0);
        let sticker = placement_at(-150.0, -60.0, 20.0, 20.0);
        let new = DipRect::new(-400.0, -200.0, 200.0, 160.0);

        let p = follow_window(anchor, new, &sticker);
        assert_placement(&p, -300.0, -120.0, 40.0, 40.0);
    }

    #[test]
    fn monitor_id_is_preserved() {
        let anchor = DipRect::new(0.0, 0.0, 100.0, 100.0);
        let mut sticker = placement_at(50.0, 50.0, 20.0, 20.0);
        sticker.monitor_id = crate::model::MonitorId("device-path".into());
        let new = DipRect::new(0.0, 0.0, 120.0, 90.0);

        let p = follow_window(anchor, new, &sticker);
        assert_eq!(p.monitor_id, sticker.monitor_id);
    }

    #[test]
    fn degenerate_anchor_returns_sticker_unchanged() {
        let sticker = placement_at(50.0, 50.0, 20.0, 20.0);
        let new = DipRect::new(0.0, 0.0, 100.0, 100.0);
        for anchor in [
            DipRect::new(0.0, 0.0, 0.0, 100.0),      // нулевая ширина
            DipRect::new(0.0, 0.0, 100.0, 0.0),      // нулевая высота
            DipRect::new(0.0, 0.0, -10.0, 100.0),    // отрицательная сторона
            DipRect::new(0.0, 0.0, f64::NAN, 100.0), // NaN
            DipRect::new(0.0, 0.0, 100.0, f64::INFINITY),
        ] {
            let p = follow_window(anchor, new, &sticker);
            assert_eq!(p, sticker, "вырожденный anchor: {anchor:?}");
        }
    }

    #[test]
    fn degenerate_new_rect_returns_sticker_unchanged() {
        let anchor = DipRect::new(0.0, 0.0, 100.0, 100.0);
        let sticker = placement_at(50.0, 50.0, 20.0, 20.0);
        for new in [
            DipRect::new(0.0, 0.0, 0.0, 100.0),
            DipRect::new(0.0, 0.0, 100.0, 0.0),
            DipRect::new(0.0, 0.0, -5.0, 100.0),
            DipRect::new(0.0, 0.0, f64::NAN, 100.0),
            DipRect::new(0.0, 0.0, 100.0, f64::NEG_INFINITY),
        ] {
            let p = follow_window(anchor, new, &sticker);
            assert_eq!(p, sticker, "вырожденный new_rect: {new:?}");
        }
    }

    #[test]
    fn valid_rect_checks() {
        assert!(is_valid_rect(DipRect::new(0.0, 0.0, 1.0, 1.0)));
        assert!(!is_valid_rect(DipRect::new(0.0, 0.0, 0.0, 1.0)));
        assert!(!is_valid_rect(DipRect::new(0.0, 0.0, 1.0, 0.0)));
        assert!(!is_valid_rect(DipRect::new(0.0, 0.0, -1.0, 1.0)));
        assert!(!is_valid_rect(DipRect::new(0.0, 0.0, f64::NAN, 1.0)));
        assert!(!is_valid_rect(DipRect::new(0.0, 0.0, 1.0, f64::INFINITY)));
    }

    #[test]
    fn fractions_of_centered_sticker() {
        let anchor = DipRect::new(100.0, 200.0, 200.0, 100.0);
        let sticker = placement_at(200.0, 250.0, 40.0, 30.0);
        let f = fractions(anchor, &sticker).expect("валидный anchor");
        assert_close(f.fx, 0.5);
        assert_close(f.fy, 0.5);
        assert_close(f.fw, 0.2);
        assert_close(f.fh, 0.3);
    }
}
