//! Совместная геометрическая трансформация нескольких выделенных стикеров.
//!
//! Модуль намеренно не знает об окнах, жестах и рендере: координатор передаёт
//! ему снимок размещений и получает новый снимок в том же порядке.

use uuid::Uuid;

use crate::hittest::DipRect;
use crate::model::Placement;

const MIN_SIZE_DIP: f64 = 1.0;
const MIN_FRAME_SIZE_DIP: f64 = 1e-9;

/// Общая рамка выделения: ось-выровненный union AABB (уже посчитан
/// вызывающим через SelectionSet::bounds).
pub fn move_selection(items: &[(Uuid, Placement)], dx: f64, dy: f64) -> Vec<(Uuid, Placement)> {
    if items.is_empty() || !dx.is_finite() || !dy.is_finite() {
        return items.to_vec();
    }

    items
        .iter()
        .map(|(id, placement)| {
            let mut moved = placement.clone();
            // Перемещение — тождественный сдвиг: размеры и monitor_id не
            // меняются, потому что движение рамки не меняет форму стикера и
            // не переводит его между мониторами.
            moved.cx += dx;
            moved.cy += dy;
            (*id, moved)
        })
        .collect()
}

/// Масштабировать размещения вместе с общей рамкой выделения.
pub fn resize_selection(
    items: &[(Uuid, Placement)],
    bounds_before: DipRect,
    bounds_after: DipRect,
    rotations: &[f64],
) -> Vec<(Uuid, Placement)> {
    // Пустое выделение и нечисловая/вырожденная рамка не задают
    // преобразование. Возвращаем снимок как есть, чтобы не породить NaN и не
    // заставить вызывающий код обрабатывать частично изменённый результат.
    if items.is_empty()
        || !valid_frame(&bounds_before)
        || !valid_frame(&bounds_after)
        || bounds_before == bounds_after
    {
        return items.to_vec();
    }

    let sx = bounds_after.w / bounds_before.w;
    let sy = bounds_after.h / bounds_before.h;
    if !sx.is_finite() || !sy.is_finite() {
        return items.to_vec();
    }

    // Несовпадающий массив поворотов нельзя надёжно сопоставить со
    // стикерами, поэтому по контракту весь такой массив считается нулевым.
    let has_rotations = rotations.len() == items.len();
    if has_rotations && rotations.iter().any(|rotation| !rotation.is_finite()) {
        return items.to_vec();
    }
    let uniform_scale = (sx * sy).sqrt();
    if !uniform_scale.is_finite() {
        return items.to_vec();
    }

    items
        .iter()
        .enumerate()
        .map(|(index, (id, placement))| {
            if !placement_values_finite(placement) {
                return (*id, placement.clone());
            }

            let left_before = placement.cx - placement.w / 2.0;
            let top_before = placement.cy - placement.h / 2.0;

            // Положение левого верхнего угла проходит через ту же
            // аффинную карту, что и рамка: так сохраняются точные доли и
            // взаимное расположение стикеров внутри выделения.
            let left_after = map_x(left_before, &bounds_before, &bounds_after, sx);
            let top_after = map_y(top_before, &bounds_before, &bounds_after, sy);

            let rotated = has_rotations && rotations[index] != 0.0;
            let (scale_w, scale_h) = if rotated {
                // Для повёрнутого прямоугольника неравномерное sx/sy не
                // выразить в модели без shear; равномерный k сохраняет его
                // форму и площадьный масштаб вместо превращения в сдвинутую
                // фигуру, которой пользователь не видел.
                (uniform_scale, uniform_scale)
            } else {
                // Неповёрнутый стикер можно растянуть по осям рамки без
                // shear, поэтому он следует sx и sy независимо.
                (sx, sy)
            };
            // Минимум в 1 DIP оставляет стикеру возвращаемую рамку даже при
            // очень сильном сжатии общей рамки.
            let width = (placement.w * scale_w).max(MIN_SIZE_DIP);
            let height = (placement.h * scale_h).max(MIN_SIZE_DIP);

            let mut resized = placement.clone();
            resized.w = width;
            resized.h = height;
            resized.cx = left_after + width / 2.0;
            resized.cy = top_after + height / 2.0;
            (*id, resized)
        })
        // Итерация по исходному срезу сохраняет порядок и длину результата.
        .collect()
}

fn valid_frame(frame: &DipRect) -> bool {
    frame.x.is_finite()
        && frame.y.is_finite()
        && frame.w.is_finite()
        && frame.h.is_finite()
        && frame.w > MIN_FRAME_SIZE_DIP
        && frame.h > MIN_FRAME_SIZE_DIP
}

fn placement_values_finite(placement: &Placement) -> bool {
    [placement.cx, placement.cy, placement.w, placement.h]
        .into_iter()
        .all(f64::is_finite)
}

fn map_x(x: f64, before: &DipRect, after: &DipRect, scale: f64) -> f64 {
    after.x + (x - before.x) * scale
}

fn map_y(y: f64, before: &DipRect, after: &DipRect, scale: f64) -> f64 {
    after.y + (y - before.y) * scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonitorId;

    fn placement(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            monitor_id: MonitorId("DISPLAY#TEST".to_owned()),
            cx,
            cy,
            w,
            h,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "ожидалось {expected}, получено {actual}"
        );
    }

    #[test]
    fn horizontal_frame_growth_scales_gap_and_widths() {
        let first = (Uuid::from_u128(1), placement(15.0, 20.0, 10.0, 8.0));
        let second = (Uuid::from_u128(2), placement(45.0, 20.0, 10.0, 8.0));
        let result = resize_selection(
            &[first, second],
            DipRect::new(10.0, 16.0, 40.0, 8.0),
            DipRect::new(10.0, 16.0, 80.0, 8.0),
            &[0.0, 0.0],
        );

        assert_close(result[1].1.cx - result[0].1.cx, 60.0);
        assert_close(result[0].1.w, 20.0);
        assert_close(result[1].1.w, 20.0);
        assert_eq!(result[0].1.monitor_id, MonitorId("DISPLAY#TEST".to_owned()));
    }

    #[test]
    fn identical_bounds_are_a_bitwise_noop() {
        let items = vec![
            (Uuid::from_u128(1), placement(15.25, -20.5, 10.0, 8.0)),
            (Uuid::from_u128(2), placement(45.75, 20.125, 7.0, 13.0)),
        ];
        let bounds = DipRect::new(-100.0, -50.0, 300.0, 200.0);
        assert_eq!(
            resize_selection(&items, bounds, bounds, &[30.0, -12.0]),
            items
        );
    }

    #[test]
    fn rotated_sticker_uses_uniform_scale_and_keeps_aspect() {
        let item = (Uuid::from_u128(1), placement(50.0, 50.0, 40.0, 20.0));
        let result = resize_selection(
            &[item],
            DipRect::new(0.0, 0.0, 100.0, 100.0),
            DipRect::new(0.0, 0.0, 200.0, 100.0),
            &[30.0],
        );

        assert_close(result[0].1.w, 40.0 * 2.0_f64.sqrt());
        assert_close(result[0].1.h, 20.0 * 2.0_f64.sqrt());
        assert_close(result[0].1.w / result[0].1.h, 2.0);
    }

    #[test]
    fn moving_forward_and_back_restores_coordinates() {
        let items = vec![
            (Uuid::from_u128(1), placement(10.0, 20.0, 30.0, 40.0)),
            (Uuid::from_u128(2), placement(-5.0, 3.0, 1.0, 1.0)),
        ];
        let moved = move_selection(&items, 12.0, -4.0);
        let restored = move_selection(&moved, -12.0, 4.0);
        assert_eq!(restored, items);
    }

    #[test]
    fn zero_width_frame_returns_input_unchanged() {
        let items = vec![(Uuid::from_u128(1), placement(10.0, 20.0, 30.0, 40.0))];
        let result = resize_selection(
            &items,
            DipRect::new(0.0, 0.0, 0.0, 100.0),
            DipRect::new(0.0, 0.0, 200.0, 200.0),
            &[0.0],
        );
        assert_eq!(result, items);
    }

    #[test]
    fn center_stays_at_same_relative_fraction_of_new_frame() {
        let item = (Uuid::from_u128(1), placement(50.0, 50.0, 10.0, 10.0));
        let result = resize_selection(
            &[item],
            DipRect::new(0.0, 0.0, 100.0, 100.0),
            DipRect::new(10.0, 20.0, 200.0, 300.0),
            &[0.0],
        );

        assert_close(result[0].1.cx, 110.0);
        assert_close(result[0].1.cy, 170.0);
    }
}
