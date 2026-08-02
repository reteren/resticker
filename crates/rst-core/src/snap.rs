//! Геометрия перемещения стикера в режиме редактирования: магнит к краям,
//! центрам и углам монитора (ROADMAP.md M2 «Магнит к краям и центрам
//! монитора, Ctrl — отключить») и ограничение «минимум 10% видно с каждой
//! стороны» (M2).
//!
//! Чистая логика: на вход — ось-выровненный bbox стикера и размер монитора
//! в DIP (ADR-010), на выход — смещение и прилипшие направляющие для
//! отрисовки линий-подсказок. Прилипание к углу получается автоматически:
//! одновременное срабатывание по обеим осям.

use crate::hittest::{DipRect, aabb};
use crate::model::Placement;

/// Настройки магнита.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapConfig {
    /// Магнит включён в настройках.
    pub enabled: bool,
    /// Радиус притягивания в DIP; прилипание при расстоянии `<= threshold`.
    /// Отрицательное значение или NaN эквивалентны отключению.
    pub threshold: f64,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 8.0,
        }
    }
}

/// Вертикальная направляющая монитора, к которой прилип стикер (ось X).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalLine {
    /// `x = 0`.
    Left,
    /// `x = monitor_w / 2`.
    Center,
    /// `x = monitor_w`.
    Right,
}

/// Горизонтальная направляющая монитора (ось Y).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalLine {
    /// `y = 0`.
    Top,
    /// `y = monitor_h / 2`.
    Center,
    /// `y = monitor_h`.
    Bottom,
}

/// Результат магнита: куда сдвинуть стикер и какие направляющие показать.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SnapResult {
    /// Смещение по X, которое надо прибавить к позиции стикера.
    pub dx: f64,
    /// Смещение по Y.
    pub dy: f64,
    /// Прилипшая вертикальная направляющая и её координата X.
    pub vline: Option<(VerticalLine, f64)>,
    /// Прилипшая горизонтальная направляющая и её координата Y.
    pub hline: Option<(HorizontalLine, f64)>,
}

impl SnapResult {
    /// Прилипли хотя бы по одной оси?
    pub fn is_snapped(&self) -> bool {
        self.vline.is_some() || self.hline.is_some()
    }
}

/// Магнит при перемещении. `sticker` — текущий ось-выровненный bbox
/// стикера (для повёрнутого — через [`aabb`]), `monitor` — прямоугольник
/// монитора в тех же координатах (начало обычно в `(0, 0)`); всё в DIP.
/// `ctrl_held` — временно отключить магнит (ROADMAP.md M2: «Ctrl — отключить»).
///
/// На каждой оси кандидатами служат края и середина стикера, целями —
/// края и центр монитора; побеждает ближайшая пара в пределах порога.
pub fn snap_move(
    sticker: DipRect,
    monitor: DipRect,
    config: &SnapConfig,
    ctrl_held: bool,
) -> SnapResult {
    let mut result = SnapResult::default();
    if ctrl_held || !config.enabled || config.threshold < 0.0 || config.threshold.is_nan() {
        return result;
    }
    let (dx, guide) = snap_axis(
        [
            sticker.x,
            sticker.x + sticker.w / 2.0,
            sticker.x + sticker.w,
        ],
        [
            monitor.x,
            monitor.x + monitor.w / 2.0,
            monitor.x + monitor.w,
        ],
        config.threshold,
    );
    result.dx = dx;
    result.vline = guide.map(|(i, x)| {
        (
            [
                VerticalLine::Left,
                VerticalLine::Center,
                VerticalLine::Right,
            ][i],
            x,
        )
    });
    let (dy, guide) = snap_axis(
        [
            sticker.y,
            sticker.y + sticker.h / 2.0,
            sticker.y + sticker.h,
        ],
        [
            monitor.y,
            monitor.y + monitor.h / 2.0,
            monitor.y + monitor.h,
        ],
        config.threshold,
    );
    result.dy = dy;
    result.hline = guide.map(|(i, y)| {
        (
            [
                HorizontalLine::Top,
                HorizontalLine::Center,
                HorizontalLine::Bottom,
            ][i],
            y,
        )
    });
    result
}

/// Магнит по [`Placement`] с учётом поворота: bbox вычисляется через
/// [`aabb`], результат (смещение) применяется к `cx`/`cy`.
pub fn snap_placement(
    placement: &Placement,
    rotation: f64,
    monitor: DipRect,
    config: &SnapConfig,
    ctrl_held: bool,
) -> SnapResult {
    snap_move(aabb(placement, rotation), monitor, config, ctrl_held)
}

/// Одномерный магнит: ближайшая пара «точка стикера — направляющая»
/// в пределах `threshold`. Возвращает смещение и `(индекс направляющей,
/// её координату)`. При равном расстоянии побеждает более ранняя пара
/// (порядок: сначала направляющие, внутри — точки стикера).
fn snap_axis(points: [f64; 3], guides: [f64; 3], threshold: f64) -> (f64, Option<(usize, f64)>) {
    let mut best: Option<(usize, f64, f64)> = None; // (индекс, смещение, |смещение|)
    for (gi, &g) in guides.iter().enumerate() {
        for &p in &points {
            let delta = g - p;
            let dist = delta.abs();
            if dist <= threshold && best.as_ref().is_none_or(|&(_, _, d)| dist < d) {
                best = Some((gi, delta, dist));
            }
        }
    }
    match best {
        Some((gi, delta, _)) => (delta, Some((gi, guides[gi]))),
        None => (0.0, None),
    }
}

/// Минимальная доля bbox стикера, которая обязана оставаться видимой
/// с каждой стороны монитора (SPEC.md, раздел 1).
pub const MIN_VISIBLE_FRACTION: f64 = 0.1;

/// Ограничение «минимум 10% видно с каждой стороны» (ROADMAP.md M2):
/// возвращает копию `placement` со скорректированным центром, при которой
/// ось-выровненный bbox стикера (с учётом поворота, [`crate::hittest::aabb`])
/// уходит за каждую сторону `monitor` не более чем на
/// `1 - MIN_VISIBLE_FRACTION` своей ширины/высоты.
///
/// Позиция, уже удовлетворяющая ограничению, не меняется. Стикер
/// с дегенеративным размером (нулевым, отрицательным или NaN) или монитор
/// нулевого размера возвращаются как есть — ограничение для них не определено.
pub fn clamp_min_visible(placement: &Placement, rotation: f64, monitor: DipRect) -> Placement {
    let bbox = aabb(placement, rotation);
    if !(bbox.w > 0.0 && bbox.h > 0.0 && monitor.w > 0.0 && monitor.h > 0.0) {
        return placement.clone();
    }
    let min_x = monitor.x - (1.0 - MIN_VISIBLE_FRACTION) * bbox.w;
    let max_x = monitor.x + monitor.w - MIN_VISIBLE_FRACTION * bbox.w;
    let min_y = monitor.y - (1.0 - MIN_VISIBLE_FRACTION) * bbox.h;
    let max_y = monitor.y + monitor.h - MIN_VISIBLE_FRACTION * bbox.h;
    let dx = bbox.x.clamp(min_x, max_x) - bbox.x;
    let dy = bbox.y.clamp(min_y, max_y) - bbox.y;
    let mut clamped = placement.clone();
    clamped.cx += dx;
    clamped.cy += dy;
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    const MONITOR: DipRect = DipRect::new(0.0, 0.0, 1920.0, 1080.0);

    fn snap(left: f64, top: f64, w: f64, h: f64, cfg: &SnapConfig, ctrl: bool) -> SnapResult {
        snap_move(DipRect::new(left, top, w, h), MONITOR, cfg, ctrl)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "ожидалось {expected}, получено {actual}"
        );
    }

    /// Табличные тесты (CONTRIBUTING.md, «Правило границ и магнит»).
    /// Стикер 100x50; порог по умолчанию 8 DIP.
    #[test]
    fn snap_move_table() {
        struct Case {
            name: &'static str,
            left: f64,
            top: f64,
            dx: f64,
            dy: f64,
            vline: Option<VerticalLine>,
            hline: Option<HorizontalLine>,
        }
        let cases = [
            Case {
                name: "далеко от направляющих",
                left: 500.0,
                top: 400.0,
                dx: 0.0,
                dy: 0.0,
                vline: None,
                hline: None,
            },
            Case {
                name: "левый край в пределах порога",
                left: 5.0,
                top: 400.0,
                dx: -5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Left),
                hline: None,
            },
            Case {
                name: "правый край к правому краю",
                left: 1815.0,
                top: 400.0,
                dx: 5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Right),
                hline: None,
            },
            Case {
                name: "центр к центру по X",
                left: 905.0,
                top: 400.0,
                dx: 5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Center),
                hline: None,
            },
            Case {
                name: "верх в пределах порога",
                left: 500.0,
                top: 3.0,
                dx: 0.0,
                dy: -3.0,
                vline: None,
                hline: Some(HorizontalLine::Top),
            },
            Case {
                name: "низ к низу",
                left: 500.0,
                top: 1026.0,
                dx: 0.0,
                dy: 4.0,
                vline: None,
                hline: Some(HorizontalLine::Bottom),
            },
            Case {
                name: "середина к центру по Y",
                left: 500.0,
                top: 512.0,
                dx: 0.0,
                dy: 3.0,
                vline: None,
                hline: Some(HorizontalLine::Center),
            },
            Case {
                name: "угол: лево + верх одновременно",
                left: 6.0,
                top: -7.0,
                dx: -6.0,
                dy: 7.0,
                vline: Some(VerticalLine::Left),
                hline: Some(HorizontalLine::Top),
            },
            Case {
                name: "точно на пороге — прилипает (включительно)",
                left: 8.0,
                top: 400.0,
                dx: -8.0,
                dy: 0.0,
                vline: Some(VerticalLine::Left),
                hline: None,
            },
            Case {
                name: "за порогом — не прилипает",
                left: 9.0,
                top: 400.0,
                dx: 0.0,
                dy: 0.0,
                vline: None,
                hline: None,
            },
            Case {
                name: "центр стикера к краю монитора",
                left: -45.0,
                top: 400.0,
                dx: -5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Left),
                hline: None,
            },
            Case {
                name: "уже выровнен: смещение 0, направляющая есть",
                left: 910.0,
                top: 400.0,
                dx: 0.0,
                dy: 0.0,
                vline: Some(VerticalLine::Center),
                hline: None,
            },
        ];
        for c in &cases {
            let r = snap(c.left, c.top, 100.0, 50.0, &SnapConfig::default(), false);
            assert_close(r.dx, c.dx);
            assert_close(r.dy, c.dy);
            assert_eq!(r.vline.map(|(g, _)| g), c.vline, "{}: vline", c.name);
            assert_eq!(r.hline.map(|(g, _)| g), c.hline, "{}: hline", c.name);
            assert_eq!(
                r.is_snapped(),
                c.vline.is_some() || c.hline.is_some(),
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn guide_coordinates_reported() {
        let r = snap(905.0, 400.0, 100.0, 50.0, &SnapConfig::default(), false);
        let Some((VerticalLine::Center, x)) = r.vline else {
            panic!("ожидался центр")
        };
        assert_close(x, 960.0);
        let r = snap(5.0, 400.0, 100.0, 50.0, &SnapConfig::default(), false);
        let Some((VerticalLine::Left, x)) = r.vline else {
            panic!("ожидался левый край")
        };
        assert_close(x, 0.0);
    }

    #[test]
    fn ctrl_disables_snap() {
        let r = snap(5.0, 3.0, 100.0, 50.0, &SnapConfig::default(), true);
        assert!(!r.is_snapped());
        assert_close(r.dx, 0.0);
        assert_close(r.dy, 0.0);
    }

    #[test]
    fn disabled_config_disables_snap() {
        let cfg = SnapConfig {
            enabled: false,
            ..SnapConfig::default()
        };
        let r = snap(5.0, 3.0, 100.0, 50.0, &cfg, false);
        assert!(!r.is_snapped());
    }

    #[test]
    fn invalid_threshold_disables_snap() {
        for threshold in [-1.0, f64::NAN] {
            let cfg = SnapConfig {
                enabled: true,
                threshold,
            };
            let r = snap(5.0, 3.0, 100.0, 50.0, &cfg, false);
            assert!(!r.is_snapped(), "threshold={threshold}");
        }
    }

    #[test]
    fn axis_nearest_pair_wins() {
        // Точка 7.0 ближе к 0 (7), точка 17.0 ближе к 20 (3): побеждает 17->20.
        let (delta, guide) = snap_axis([7.0, 17.0, 27.0], [0.0, 20.0, 40.0], 8.0);
        assert_close(delta, 3.0);
        assert_eq!(guide.map(|(i, _)| i), Some(1));
    }

    #[test]
    fn axis_tie_prefers_earlier_guide() {
        // Оба расстояния равны 10 и на пороге: побеждает первая направляющая.
        let (delta, guide) = snap_axis([10.0, 10.0, 10.0], [0.0, 20.0, 40.0], 10.0);
        assert_close(delta, -10.0);
        assert_eq!(guide.map(|(i, _)| i), Some(0));
    }

    #[test]
    fn axis_nan_points_do_not_snap() {
        let (_, guide) = snap_axis([f64::NAN; 3], [0.0, 20.0, 40.0], 8.0);
        assert_eq!(guide, None);
    }

    #[test]
    fn sticker_wider_than_monitor_snaps_center_aligned() {
        // Центр уже на центре: смещение 0, но направляющая активна.
        let r = snap(-40.0, 0.0, 2000.0, 50.0, &SnapConfig::default(), false);
        assert_close(r.dx, 0.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Center));
        assert_eq!(r.hline.map(|(g, _)| g), Some(HorizontalLine::Top));
    }

    #[test]
    fn snap_placement_respects_rotation() {
        let p = Placement {
            cx: 25.0,
            cy: 540.0,
            w: 100.0,
            h: 40.0,
            ..Placement::default()
        };
        let cfg = SnapConfig::default();
        // Без поворота: левый край bbox на -25 — до края монитора 25 DIP,
        // дальше порога, магнит по X молчит.
        let r = snap_placement(&p, 0.0, MONITOR, &cfg, false);
        assert_close(r.dx, 0.0);
        assert_eq!(r.vline.map(|(g, _)| g), None);
        // Поворот на 90°: bbox 40x100, левый край на 5 — в пределах порога.
        let r = snap_placement(&p, FRAC_PI_2, MONITOR, &cfg, false);
        assert_close(r.dx, -5.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Left));
    }

    fn placement_at(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            cx,
            cy,
            w,
            h,
            ..Placement::default()
        }
    }

    fn clamped(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        clamp_min_visible(&placement_at(cx, cy, w, h), 0.0, MONITOR)
    }

    #[test]
    fn inside_monitor_is_unchanged() {
        let p = clamped(960.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, 960.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn partially_out_within_90_percent_is_unchanged() {
        // bbox 100x50 уходит влево на 80% ширины — допустимо.
        let p = clamped(-30.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, -30.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn far_left_clamps_to_10_percent_visible() {
        let p = clamped(-5000.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, -40.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn far_right_clamps_to_10_percent_visible() {
        let p = clamped(5000.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, 1960.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn far_top_clamps_to_10_percent_visible() {
        let p = clamped(960.0, -5000.0, 100.0, 50.0);
        assert_close(p.cx, 960.0);
        assert_close(p.cy, -20.0);
    }

    #[test]
    fn far_bottom_clamps_to_10_percent_visible() {
        let p = clamped(960.0, 5000.0, 100.0, 50.0);
        assert_close(p.cx, 960.0);
        assert_close(p.cy, 1100.0);
    }

    #[test]
    fn corner_clamps_both_axes() {
        let p = clamped(-5000.0, -5000.0, 100.0, 50.0);
        assert_close(p.cx, -40.0);
        assert_close(p.cy, -20.0);
    }

    #[test]
    fn exactly_at_ten_percent_is_unchanged() {
        // bbox.x = -90 ровно: слева видно ровно 10% ширины.
        let p = clamped(-40.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, -40.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn rotation_uses_aabb_dimensions() {
        // 100x40 при повороте 90°: bbox 40x100 — ограничение по ширине 40.
        let p = clamp_min_visible(
            &placement_at(-5000.0, 540.0, 100.0, 40.0),
            FRAC_PI_2,
            MONITOR,
        );
        assert_close(p.cx, -16.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn offset_monitor_clamps_relatively() {
        let m = DipRect::new(100.0, 200.0, 1920.0, 1080.0);
        let p = clamp_min_visible(&placement_at(-5000.0, -5000.0, 100.0, 50.0), 0.0, m);
        assert_close(p.cx, 60.0);
        assert_close(p.cy, 180.0);
    }

    #[test]
    fn sticker_wider_than_monitor_still_clamps() {
        // bbox 2000x50 в мониторе 1920: уход влево ограничен 90% ширины.
        let p = clamped(-5000.0, 540.0, 2000.0, 50.0);
        assert_close(p.cx, -800.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn degenerate_sticker_is_unchanged() {
        for (w, h) in [(0.0, 50.0), (-10.0, 50.0), (100.0, 0.0)] {
            let p = clamp_min_visible(&placement_at(-5000.0, -5000.0, w, h), 0.0, MONITOR);
            assert_close(p.cx, -5000.0);
            assert_close(p.cy, -5000.0);
        }
    }

    #[test]
    fn nan_size_is_unchanged() {
        let p = clamp_min_visible(
            &placement_at(-5000.0, -5000.0, f64::NAN, 50.0),
            0.0,
            MONITOR,
        );
        assert_close(p.cx, -5000.0);
        assert_close(p.cy, -5000.0);
    }

    #[test]
    fn zero_size_monitor_is_unchanged() {
        let zero = DipRect::new(0.0, 0.0, 0.0, 0.0);
        let p = clamp_min_visible(&placement_at(-5000.0, -5000.0, 100.0, 50.0), 0.0, zero);
        assert_close(p.cx, -5000.0);
        assert_close(p.cy, -5000.0);
    }
}
