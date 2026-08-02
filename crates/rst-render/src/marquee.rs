//! Визуал рамки-марки (rubber-band) мультивыделения (SPEC.md 3.2: «Протяжка
//! по пустому месту — рамка выделения»; ROADMAP.md M2).
//!
//! Чистая геометрия, как и в `selection`: модуль выдаёт прямоугольники
//! [`Box2D`], которые вызывающий слой превращает в спрайты через
//! [`crate::solid_sprite`] — каждый штрих пунктира это отдельный тонкий
//! квад. Логика выделения (какие стикеры попали в рамку) живёт отдельно в
//! `rst_core::selection_set::SelectionSet::rubber_band` и сюда не входит.

use crate::selection::Box2D;

/// Толщина штриха пунктира, DIP.
pub const MARQUEE_THICKNESS_DIP: f64 = 1.0;
/// Длина штриха, DIP.
pub const MARQUEE_DASH_DIP: f64 = 6.0;
/// Зазор между штрихами, DIP.
pub const MARQUEE_GAP_DIP: f64 = 4.0;
/// Прозрачность заливки прямоугольника протяжки (едва заметная).
pub const MARQUEE_FILL_OPACITY: f64 = 0.08;
/// Прозрачность штрихов пунктира.
pub const MARQUEE_STROKE_OPACITY: f64 = 0.9;

/// Визуал рамки-марки: заливка и сегменты пунктира по периметру.
/// Цвет выбирает вызывающий слой (разумный дефолт — акцент
/// `theme::SLIDER_FILL`), прозрачности — константы выше.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarqueeVisuals {
    /// Заливка прямоугольника протяжки; `None` для вырожденного
    /// (нулевая ширина или высота — заливать нечего).
    pub fill: Option<Box2D>,
    /// Сегменты пунктира по периметру, ось-выровненные (rotation = 0).
    /// Порядок: верхняя сторона, нижняя, левая, правая; внутри стороны —
    /// по ходу координат.
    pub dashes: Vec<Box2D>,
}

/// Собрать визуал по двум углам протяжки: `anchor` — точка `MouseDown`,
/// `current` — текущая позиция курсора, обе в DIP. Порядок точек любой
/// (протяжка влево-вверх нормализуется min/max по осям). Вырожденные входы
/// (точка, линия, NaN) дают пустой либо односторонний визуал без дублей:
/// при нулевой ширине правая сторона совпадает с левой и пропускается,
/// при нулевой высоте — нижняя.
pub fn marquee_visuals(anchor: (f64, f64), current: (f64, f64)) -> MarqueeVisuals {
    let mut out = MarqueeVisuals::default();
    if ![anchor.0, anchor.1, current.0, current.1]
        .iter()
        .all(|v| v.is_finite())
    {
        return out;
    }
    let x0 = anchor.0.min(current.0);
    let y0 = anchor.1.min(current.1);
    let w = (anchor.0 - current.0).abs();
    let h = (anchor.1 - current.1).abs();

    if w > 0.0 && h > 0.0 {
        out.fill = Some(Box2D::from_top_left(x0, y0, w, h));
    }

    let t = MARQUEE_THICKNESS_DIP;
    // Верхняя и нижняя стороны — горизонтальные штрихи.
    for (y, skip) in [(y0, false), (y0 + h, h == 0.0)] {
        if !skip {
            for (cx, seg) in dash_segments(x0, w) {
                out.dashes.push(Box2D::from_center(cx, y, seg, t));
            }
        }
    }
    // Левая и правая стороны — вертикальные штрихи.
    for (x, skip) in [(x0, false), (x0 + w, w == 0.0)] {
        if !skip {
            for (cy, seg) in dash_segments(y0, h) {
                out.dashes.push(Box2D::from_center(x, cy, t, seg));
            }
        }
    }
    out
}

/// Штрихи вдоль одной стороны: пары (центр, длина) от `start` на длину
/// `len`. Первый штрих начинается у края стороны, последний может быть
/// короче (остаток стороны). Пустая/невалидная длина — пустой список.
fn dash_segments(start: f64, len: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let positive_finite = len.is_finite() && len > 0.0;
    if !positive_finite {
        return out;
    }
    let mut pos = 0.0;
    while pos < len {
        let seg = MARQUEE_DASH_DIP.min(len - pos);
        out.push((start + pos + seg / 2.0, seg));
        pos += MARQUEE_DASH_DIP + MARQUEE_GAP_DIP;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64, ctx: &str) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "{ctx}: ожидалось {expected}, получено {actual}"
        );
    }

    #[test]
    fn dashes_step_and_partial_last() {
        // Шаг dash+gap = 10; сторона 21: полные штрихи в 0 и 10, остаток 1 в 20.
        let segs = dash_segments(100.0, 21.0);
        assert_eq!(segs.len(), 3);
        assert_close(segs[0].0, 103.0, "первый центр");
        assert_close(segs[0].1, 6.0, "первый полный");
        assert_close(segs[1].0, 113.0, "второй центр");
        assert_close(segs[1].1, 6.0, "второй полный");
        assert_close(segs[2].0, 120.5, "остаток центр");
        assert_close(segs[2].1, 1.0, "остаток укорочен");

        // Ровно один полный штрих; ровно длина шага (зазор без штриха не считается).
        assert_eq!(dash_segments(0.0, 6.0).len(), 1);
        assert_eq!(dash_segments(0.0, 10.0).len(), 1);
        assert_eq!(dash_segments(0.0, 11.0).len(), 2);
        // Пустая и невалидная длина.
        assert!(dash_segments(0.0, 0.0).is_empty());
        assert!(dash_segments(0.0, -5.0).is_empty());
        assert!(dash_segments(0.0, f64::NAN).is_empty());
    }

    #[test]
    fn right_down_drag_geometry() {
        let v = marquee_visuals((10.0, 20.0), (110.0, 80.0));
        // Заливка — нормализованный прямоугольник.
        assert_eq!(v.fill, Some(Box2D::from_top_left(10.0, 20.0, 100.0, 60.0)));
        // Верх/низ: ceil(100/10) = 10; боковые: ceil(60/10) = 6.
        assert_eq!(v.dashes.len(), 10 + 10 + 6 + 6);
        // Порядок: верх, низ, лево, право.
        let first = v.dashes[0];
        assert_close(first.cx, 13.0, "первый штрих верха x");
        assert_close(first.cy, 20.0, "первый штрих верха y");
        assert_close(first.w, MARQUEE_DASH_DIP, "горизонтальный штрих w");
        assert_close(first.h, MARQUEE_THICKNESS_DIP, "горизонтальный штрих h");
        assert_close(first.rotation, 0.0, "без поворота");
        // Последний штрих верха — тоже полный (100 кратно шагу).
        let last_top = v.dashes[9];
        assert_close(last_top.cx, 103.0, "последний штрих верха x");
        // Вертикальный штрих — транспонированная форма.
        let left = v.dashes[20];
        assert_close(left.cx, 10.0, "штрих левой стороны x");
        assert_close(left.cy, 23.0, "первый штрих левой стороны y");
        assert_close(left.w, MARQUEE_THICKNESS_DIP, "вертикальный штрих w");
        assert_close(left.h, MARQUEE_DASH_DIP, "вертикальный штрих h");
        // Нижняя и правая стороны лежат на y0+h / x0+w.
        assert_close(v.dashes[10].cy, 80.0, "нижняя сторона y");
        assert_close(v.dashes[26].cx, 110.0, "правая сторона x");
    }

    #[test]
    fn reverse_drag_normalizes() {
        // Якорь справа-снизу от курсора: тот же визуал, что и прямая протяжка.
        let forward = marquee_visuals((10.0, 20.0), (110.0, 80.0));
        let backward = marquee_visuals((110.0, 80.0), (10.0, 20.0));
        assert_eq!(forward, backward);
        let mixed_a = marquee_visuals((110.0, 20.0), (10.0, 80.0));
        let mixed_b = marquee_visuals((10.0, 80.0), (110.0, 20.0));
        assert_eq!(mixed_a, mixed_b);
        assert_eq!(mixed_a, forward);
    }

    #[test]
    fn degenerate_point_is_empty() {
        let v = marquee_visuals((30.0, 40.0), (30.0, 40.0));
        assert_eq!(v.fill, None);
        assert!(v.dashes.is_empty());
    }

    #[test]
    fn zero_width_single_column_no_duplicates() {
        // Вертикальная линия: только левая сторона, правая совпадает и пропускается.
        let v = marquee_visuals((5.0, 5.0), (5.0, 55.0));
        assert_eq!(v.fill, None);
        assert_eq!(v.dashes.len(), 5);
        assert!(v.dashes.iter().all(|d| d.cx == 5.0));
    }

    #[test]
    fn zero_height_single_row_no_duplicates() {
        // Горизонтальная линия: только верхняя сторона.
        let v = marquee_visuals((5.0, 5.0), (55.0, 5.0));
        assert_eq!(v.fill, None);
        assert_eq!(v.dashes.len(), 5);
        assert!(v.dashes.iter().all(|d| d.cy == 5.0));
    }

    #[test]
    fn nan_inputs_are_empty() {
        for (a, c) in [
            ((f64::NAN, 0.0), (10.0, 10.0)),
            ((0.0, 0.0), (10.0, f64::NAN)),
            ((f64::INFINITY, 0.0), (10.0, 10.0)),
        ] {
            let v = marquee_visuals(a, c);
            assert_eq!(v.fill, None, "{a:?} {c:?}");
            assert!(v.dashes.is_empty(), "{a:?} {c:?}");
        }
    }
}
