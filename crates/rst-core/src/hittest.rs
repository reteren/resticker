//! Хит-тестинг точки по трансформации стикера (ROADMAP.md M2,
//! «Хит-тестинг с обратной аффинной трансформацией»).
//!
//! Все координаты — логические (DIP) относительно левого верхнего угла
//! монитора (ADR-010). Поворот — радианы по часовой стрелке (CONFIG.md),
//! конвенция совпадает с шейдером спрайта (rst-render), поэтому хит-тест
//! видит ровно тот прямоугольник, который отрисован.

use crate::model::{Placement, Transform};

/// Ось-выровненный прямоугольник в DIP-координатах монитора:
/// `(x, y)` — левый верхний угол, `(w, h)` — размер.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DipRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl DipRect {
    /// Прямоугольник по левому верхнему углу и размеру.
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Прямоугольник по центру и размеру.
    pub const fn from_center(cx: f64, cy: f64, w: f64, h: f64) -> Self {
        Self {
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            w,
            h,
        }
    }
}

/// Прямая аффинная трансформация: точка из локальных координат стикера
/// (начало — центр, оси вдоль неповёрнутых сторон) в координаты монитора.
///
/// Повторяет матрицу шейдера спрайта: `(x·cos − y·sin, x·sin + y·cos)`.
pub fn to_world(cx: f64, cy: f64, rotation: f64, lx: f64, ly: f64) -> (f64, f64) {
    let (sin, cos) = rotation.sin_cos();
    (cx + lx * cos - ly * sin, cy + lx * sin + ly * cos)
}

/// Обратная аффинная трансформация: точка монитора `(px, py)` в локальные
/// координаты стикера с центром `(cx, cy)` и поворотом `rotation`.
/// Обратна к [`to_world`]; в локальной системе хит-тест сводится
/// к сравнению `|lx| <= w/2`, `|ly| <= h/2`.
pub fn to_local(cx: f64, cy: f64, rotation: f64, px: f64, py: f64) -> (f64, f64) {
    let (sin, cos) = rotation.sin_cos();
    let dx = px - cx;
    let dy = py - cy;
    (dx * cos + dy * sin, -dx * sin + dy * cos)
}

/// Точка `(px, py)` внутри прямоугольника стикера (с учётом поворота)?
///
/// Граница считается попаданием. Отражения (`flip_h`/`flip_v`) и
/// прозрачность геометрию не меняют и здесь не учитываются.
pub fn contains(placement: &Placement, transform: &Transform, px: f64, py: f64) -> bool {
    contains_inflated(placement, transform, px, py, 0.0)
}

/// [`contains`] с допуском `inflate` (DIP) наружу от граней — для зон
/// ручек и смены курсора. Стикер с нулевым/отрицательным размером
/// (или NaN) не хитуется никогда.
pub fn contains_inflated(
    placement: &Placement,
    transform: &Transform,
    px: f64,
    py: f64,
    inflate: f64,
) -> bool {
    let hw = placement.w / 2.0 + inflate;
    let hh = placement.h / 2.0 + inflate;
    if !(hw > 0.0 && hh > 0.0) {
        return false;
    }
    let (lx, ly) = to_local(placement.cx, placement.cy, transform.rotation, px, py);
    lx.abs() <= hw && ly.abs() <= hh
}

/// Углы повёрнутого прямоугольника стикера в координатах монитора:
/// левый верхний, правый верхний, правый нижний, левый нижний (в локальных
/// осях стикера, по часовой стрелке). Для рамки выделения и 8 ручек (M2).
pub fn corners(placement: &Placement, rotation: f64) -> [(f64, f64); 4] {
    let hw = placement.w / 2.0;
    let hh = placement.h / 2.0;
    [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
        .map(|(lx, ly)| to_world(placement.cx, placement.cy, rotation, lx, ly))
}

/// Ось-выровненный ограничивающий прямоугольник повёрнутого стикера
/// в DIP. Используется магнитом ([`crate::snap`]) и ограничением
/// «минимум 10% видно с каждой стороны» (M2).
pub fn aabb(placement: &Placement, rotation: f64) -> DipRect {
    let (sin, cos) = rotation.sin_cos();
    let hw = (placement.w * cos.abs() + placement.h * sin.abs()) / 2.0;
    let hh = (placement.w * sin.abs() + placement.h * cos.abs()) / 2.0;
    DipRect::from_center(placement.cx, placement.cy, hw * 2.0, hh * 2.0)
}

/// Одна из восьми ручек рамки выделения (ROADMAP.md M2: «Рамка выделения +
/// 8 ручек»). Порядок вариантов — по часовой стрелке от северо-западной.
///
/// Единый тип для всех крейтов (docs/M2_INTEGRATION_REVIEW.md, §1): его
/// переиспользуют рендерер (rst-render `selection`), зоны ввода и
/// трансформации ([`crate::transform_ops`]). Новых копий не заводить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
}

impl HandleKind {
    /// Все ручки в порядке объявления (по часовой от северо-западной).
    pub const ALL: [HandleKind; 8] = [
        HandleKind::NorthWest,
        HandleKind::North,
        HandleKind::NorthEast,
        HandleKind::East,
        HandleKind::SouthEast,
        HandleKind::South,
        HandleKind::SouthWest,
        HandleKind::West,
    ];

    /// Знаки смещения ручки от центра рамки в локальных осях стикера
    /// (доли полуразмера): восток +1 по x, юг +1 по y.
    pub const fn local_sign(self) -> (f64, f64) {
        match self {
            HandleKind::NorthWest => (-1.0, -1.0),
            HandleKind::North => (0.0, -1.0),
            HandleKind::NorthEast => (1.0, -1.0),
            HandleKind::East => (1.0, 0.0),
            HandleKind::SouthEast => (1.0, 1.0),
            HandleKind::South => (0.0, 1.0),
            HandleKind::SouthWest => (-1.0, 1.0),
            HandleKind::West => (-1.0, 0.0),
        }
    }

    /// Угловая ручка, если это угол: только у углов есть зона поворота
    /// (SPEC 3.3).
    pub const fn corner(self) -> Option<Corner> {
        match self {
            HandleKind::NorthWest => Some(Corner::NorthWest),
            HandleKind::NorthEast => Some(Corner::NorthEast),
            HandleKind::SouthEast => Some(Corner::SouthEast),
            HandleKind::SouthWest => Some(Corner::SouthWest),
            _ => None,
        }
    }

    /// Угловая ли ручка.
    pub const fn is_corner(self) -> bool {
        matches!(
            self,
            HandleKind::NorthWest
                | HandleKind::NorthEast
                | HandleKind::SouthEast
                | HandleKind::SouthWest
        )
    }
}

/// Угловая ручка рамки выделения. Порядок — по часовой от северо-западной,
/// совпадает с порядком углов в [`corners`], поэтому [`Corner::index`]
/// индексирует массив из [`corners`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    NorthWest,
    NorthEast,
    SouthEast,
    SouthWest,
}

impl Corner {
    /// Все углы в порядке объявления.
    pub const ALL: [Corner; 4] = [
        Corner::NorthWest,
        Corner::NorthEast,
        Corner::SouthEast,
        Corner::SouthWest,
    ];

    /// Индекс угла в массивах вида [`corners`] (NW, NE, SE, SW).
    pub const fn index(self) -> usize {
        match self {
            Corner::NorthWest => 0,
            Corner::NorthEast => 1,
            Corner::SouthEast => 2,
            Corner::SouthWest => 3,
        }
    }

    /// Знаки смещения угла от центра рамки (доли полуразмера), как у
    /// [`HandleKind::local_sign`].
    pub const fn local_sign(self) -> (f64, f64) {
        match self {
            Corner::NorthWest => (-1.0, -1.0),
            Corner::NorthEast => (1.0, -1.0),
            Corner::SouthEast => (1.0, 1.0),
            Corner::SouthWest => (-1.0, 1.0),
        }
    }

    /// Соответствующая угловая ручка.
    pub const fn handle(self) -> HandleKind {
        match self {
            Corner::NorthWest => HandleKind::NorthWest,
            Corner::NorthEast => HandleKind::NorthEast,
            Corner::SouthEast => HandleKind::SouthEast,
            Corner::SouthWest => HandleKind::SouthWest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, SQRT_2};

    fn placement(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            cx,
            cy,
            w,
            h,
            ..Placement::default()
        }
    }

    fn transform(rotation: f64) -> Transform {
        Transform {
            rotation,
            ..Transform::default()
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "ожидалось {expected}, получено {actual}"
        );
    }

    #[test]
    fn axis_aligned_hit_and_miss() {
        // Прямоугольник 40x20 с центром (100, 50): x в [80, 120], y в [40, 60].
        let p = placement(100.0, 50.0, 40.0, 20.0);
        let t = transform(0.0);
        assert!(contains(&p, &t, 100.0, 50.0), "центр");
        assert!(contains(&p, &t, 81.0, 41.0), "возле угла внутри");
        assert!(contains(&p, &t, 120.0, 60.0), "граница — попадание");
        assert!(!contains(&p, &t, 120.1, 50.0), "правее края");
        assert!(!contains(&p, &t, 100.0, 60.1), "ниже края");
        assert!(!contains(&p, &t, 0.0, 0.0), "далеко");
    }

    #[test]
    fn rotated_90_swaps_axes() {
        // Повёрнутый на 90° широкий стикер становится высоким.
        let p = placement(200.0, 100.0, 100.0, 40.0);
        let t = transform(FRAC_PI_2);
        assert!(
            contains(&p, &t, 200.0, 149.0),
            "длинная ось теперь вертикальна"
        );
        assert!(contains(&p, &t, 220.0, 100.0), "короткая ось — граница");
        assert!(!contains(&p, &t, 221.0, 100.0), "за пределами короткой оси");
        assert!(!contains(&p, &t, 200.0, 151.0), "за пределами длинной оси");
    }

    #[test]
    fn rotated_45_diagonal() {
        let p = placement(0.0, 0.0, 100.0, 100.0);
        let t = transform(FRAC_PI_4);
        // (30, 30) в локальных: (42.4, 0) — внутри; (60, 60): (84.9, 0) — снаружи.
        assert!(contains(&p, &t, 30.0, 30.0));
        assert!(!contains(&p, &t, 60.0, 60.0));
    }

    #[test]
    fn negative_rotation() {
        // Симметрично повороту на +90°: длинная ось вертикальна.
        let p = placement(200.0, 100.0, 100.0, 40.0);
        let t = transform(-FRAC_PI_2);
        assert!(contains(&p, &t, 200.0, 149.0));
        assert!(!contains(&p, &t, 221.0, 100.0));
    }

    #[test]
    fn flips_do_not_change_geometry() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        let t = Transform {
            flip_h: true,
            flip_v: true,
            ..transform(0.0)
        };
        assert!(contains(&p, &t, 100.0, 50.0));
        assert!(!contains(&p, &t, 130.0, 50.0));
    }

    #[test]
    fn inflate_expands_hit_area() {
        let p = placement(0.0, 0.0, 40.0, 20.0);
        let t = transform(0.0);
        assert!(!contains_inflated(&p, &t, 24.0, 0.0, 3.9));
        assert!(
            contains_inflated(&p, &t, 24.0, 0.0, 4.0),
            "допуск включительно"
        );
        assert!(
            !contains_inflated(&p, &t, 24.0, 0.0, -1.0),
            "отрицательный допуск сжимает"
        );
    }

    #[test]
    fn degenerate_and_nan_never_hit() {
        let t = transform(0.0);
        assert!(!contains(&placement(0.0, 0.0, 0.0, 20.0), &t, 0.0, 0.0));
        assert!(!contains(&placement(0.0, 0.0, 40.0, -5.0), &t, 0.0, 0.0));
        assert!(!contains(
            &placement(0.0, 0.0, f64::NAN, 20.0),
            &t,
            0.0,
            0.0
        ));
        assert!(!contains(
            &placement(0.0, 0.0, 40.0, 20.0),
            &t,
            f64::NAN,
            0.0
        ));
    }

    #[test]
    fn world_local_roundtrip() {
        for rotation in [0.0, FRAC_PI_4, FRAC_PI_2, -2.3, 15.0] {
            let (lx, ly) = (12.3, -7.7);
            let (wx, wy) = to_world(100.0, 50.0, rotation, lx, ly);
            let (bx, by) = to_local(100.0, 50.0, rotation, wx, wy);
            assert_close(bx, lx);
            assert_close(by, ly);
        }
    }

    #[test]
    fn corners_axis_aligned() {
        let c = corners(&placement(10.0, 20.0, 4.0, 2.0), 0.0);
        let expected = [(8.0, 19.0), (12.0, 19.0), (12.0, 21.0), (8.0, 21.0)];
        for (actual, expected) in c.iter().zip(expected.iter()) {
            assert_close(actual.0, expected.0);
            assert_close(actual.1, expected.1);
        }
    }

    #[test]
    fn corners_rotated_90() {
        // Поворот на 90° по часовой: правый верхний (2, -1) -> (1, 2),
        // правый нижний (2, 1) -> (-1, 2); прямоугольник 4x2 становится 2x4.
        let c = corners(&placement(0.0, 0.0, 4.0, 2.0), FRAC_PI_2);
        assert_close(c[1].0, 1.0);
        assert_close(c[1].1, 2.0);
        assert_close(c[2].0, -1.0);
        assert_close(c[2].1, 2.0);
    }

    #[test]
    fn aabb_axis_aligned_matches_rect() {
        let r = aabb(&placement(10.0, 20.0, 4.0, 2.0), 0.0);
        assert_close(r.x, 8.0);
        assert_close(r.y, 19.0);
        assert_close(r.w, 4.0);
        assert_close(r.h, 2.0);
    }

    #[test]
    fn aabb_rotated() {
        // 90°: размеры меняются местами; 45° квадрат: сторона w*sqrt(2).
        let r = aabb(&placement(0.0, 0.0, 4.0, 2.0), FRAC_PI_2);
        assert_close(r.w, 2.0);
        assert_close(r.h, 4.0);
        let r = aabb(&placement(0.0, 0.0, 10.0, 10.0), FRAC_PI_4);
        assert_close(r.w, 10.0 * SQRT_2);
        assert_close(r.h, 10.0 * SQRT_2);
    }

    #[test]
    fn handle_local_sign_covers_all_handles() {
        let expected = [
            (HandleKind::NorthWest, (-1.0, -1.0)),
            (HandleKind::North, (0.0, -1.0)),
            (HandleKind::NorthEast, (1.0, -1.0)),
            (HandleKind::East, (1.0, 0.0)),
            (HandleKind::SouthEast, (1.0, 1.0)),
            (HandleKind::South, (0.0, 1.0)),
            (HandleKind::SouthWest, (-1.0, 1.0)),
            (HandleKind::West, (-1.0, 0.0)),
        ];
        for (handle, sign) in expected {
            assert_eq!(handle.local_sign(), sign, "{handle:?}");
        }
    }

    #[test]
    fn handle_corner_mapping() {
        for handle in HandleKind::ALL {
            match handle {
                HandleKind::NorthWest => assert_eq!(handle.corner(), Some(Corner::NorthWest)),
                HandleKind::NorthEast => assert_eq!(handle.corner(), Some(Corner::NorthEast)),
                HandleKind::SouthEast => assert_eq!(handle.corner(), Some(Corner::SouthEast)),
                HandleKind::SouthWest => assert_eq!(handle.corner(), Some(Corner::SouthWest)),
                _ => assert_eq!(handle.corner(), None),
            }
            assert_eq!(handle.is_corner(), handle.corner().is_some(), "{handle:?}");
        }
        for corner in Corner::ALL {
            assert_eq!(corner.handle().corner(), Some(corner), "{corner:?}");
        }
    }

    #[test]
    fn corner_index_matches_corners_array_order() {
        let p = placement(10.0, 20.0, 4.0, 2.0);
        let c = corners(&p, 0.0);
        for corner in Corner::ALL {
            let (sx, sy) = corner.local_sign();
            // Угол по индексу обязан быть to_world от локальной точки (sx·hw, sy·hh).
            let (wx, wy) = to_world(p.cx, p.cy, 0.0, sx * p.w / 2.0, sy * p.h / 2.0);
            assert_close(c[corner.index()].0, wx);
            assert_close(c[corner.index()].1, wy);
        }
    }
}
