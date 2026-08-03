//! Перепривязка стикера к другому монитору по центру bbox при пересечении
//! границы (docs/M3_PREP_NOTES.md, раздел 5.4 — «простой» вариант первой
//! итерации): драг зажат своим монитором (`snap::clamp_min_visible` уже это
//! делает — центр стикера может уйти за край до 90% bbox), на `MouseUp`
//! монитор перепривязывается по центру.
//!
//! Чистая геометрия без платформы (CONTRIBUTING.md, «Правило зависимостей»):
//! вход — DIP-размещение относительно своего монитора (ADR-010) + физические
//! границы и масштаб мониторов (снапшот `rst_win32::monitors`, те же поля,
//! что у `MonitorRecord::last_bounds`/`last_scale`); выход — то же размещение
//! под новым `monitor_id`, координаты пересчитаны в DIP нового монитора так,
//! что физическая позиция центра на виртуальном десктопе не меняется.

use crate::model::{MonitorId, Placement, Rect};

/// Физические границы и масштаб монитора для перепривязки — только то, что
/// нужно геометрии (id для нового `Placement::monitor_id`, `bounds_px` и
/// `scale` для перевода DIP ↔ физические пиксели).
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorBounds {
    pub id: MonitorId,
    /// Границы в физических пикселях виртуального десктопа; у неосновных
    /// мониторов `x`/`y` могут быть отрицательными.
    pub bounds_px: Rect,
    /// Масштаб: `dpi / 96.0` (1.0 = 100%).
    pub scale: f64,
}

/// Перепривязать `placement` к другому монитору, если центр bbox стикера
/// оказался вне монитора-источника.
///
/// Центр bbox повёрнутого стикера совпадает с центром размещения
/// (`placement.cx`/`cy` — центр прямоугольника, AABB симметричен вокруг
/// него при любом повороте), поэтому трансформация на вход не нужна.
///
/// Шаги:
/// 1. центр из DIP источника → физические пиксели виртуального десктопа:
///    `g = bounds_px.origin + center_dip * scale`;
/// 2. центр внутри `source` → размещение возвращается без изменений;
/// 3. иначе — первый монитор из `others`, чьи `bounds_px` содержат точку;
/// 4. новый `Placement`: `monitor_id` — id найденного монитора, центр
///    пересчитан в DIP нового монитора `(g − bounds_px.origin) / scale` —
///    физическая позиция центра сохраняется ровно; `w`/`h` переносятся без
///    изменений (размер в DIP — свойство стикера, а не монитора; пересчёт
///    физического размера при смене DPI — отдельное решение, вне шага).
///
/// Соглашения границ: прямоугольники полуоткрыты `[x, x+w) × [y, y+h)` —
/// точка на общей границе достаётся монитору за ней; при перекрытии границ
/// побеждает первый в списке `others` (тай-брейк фиксируется, порядок
/// списка — ответственность вызывающего кода).
///
/// Возвращает клон исходного `placement` без изменений, если центр остался
/// в `source` или его не забирает ни один монитор из `others` (в т.ч. если
/// точка попала в разрыв между мониторами). Мониторы с неположительным/
/// неконечным масштабом или нулевыми границами целью стать не могут.
/// NaN в координатах размещения даёт «без изменений»: NaN не содержится
/// ни в одном прямоугольнике.
pub fn rebind_monitor_by_center(
    placement: &Placement,
    source: &MonitorBounds,
    others: &[MonitorBounds],
) -> Placement {
    let (gx, gy) = local_dip_to_global_px(placement.cx, placement.cy, source);
    if !gx.is_finite() || !gy.is_finite() {
        return placement.clone();
    }
    if contains_px(source.bounds_px, gx, gy) {
        return placement.clone();
    }
    let Some(target) = others
        .iter()
        .find(|m| m.scale > 0.0 && m.scale.is_finite() && contains_px(m.bounds_px, gx, gy))
    else {
        return placement.clone();
    };
    let mut rebound = placement.clone();
    rebound.monitor_id = target.id.clone();
    rebound.cx = (gx - target.bounds_px.x as f64) / target.scale;
    rebound.cy = (gy - target.bounds_px.y as f64) / target.scale;
    rebound
}

/// Локальные DIP источника → физические пиксели виртуального десктопа:
/// масштаб источника + смещение его границ (ADR-010: DIP — от левого
/// верхнего угла своего монитора).
fn local_dip_to_global_px(x_dip: f64, y_dip: f64, monitor: &MonitorBounds) -> (f64, f64) {
    (
        monitor.bounds_px.x as f64 + x_dip * monitor.scale,
        monitor.bounds_px.y as f64 + y_dip * monitor.scale,
    )
}

/// Содержит ли прямоугольник точку. Полуоткрытый интервал по обеим осям:
/// правый/нижний край не принадлежит (точка на общей границе уходит
/// монитору за ней); вырожденный прямоугольник (нулевой размер) ничего
/// не содержит; NaN ни в чём не содержится.
fn contains_px(bounds: Rect, x: f64, y: f64) -> bool {
    let (x0, y0) = (bounds.x as f64, bounds.y as f64);
    let (x1, y1) = (x0 + bounds.w as f64, y0 + bounds.h as f64);
    x >= x0 && x < x1 && y >= y0 && y < y1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: &str, x: i32, y: i32, w: u32, h: u32, scale: f64) -> MonitorBounds {
        MonitorBounds {
            id: MonitorId(id.to_string()),
            bounds_px: Rect { x, y, w, h },
            scale,
        }
    }

    fn placement(monitor: &MonitorBounds, cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            monitor_id: monitor.id.clone(),
            cx,
            cy,
            w,
            h,
        }
    }

    #[test]
    fn center_inside_source_is_noop() {
        let (src, right) = (
            mon("src", 0, 0, 1920, 1080, 1.0),
            mon("right", 1920, 0, 1920, 1080, 1.0),
        );
        let p = placement(&src, 960.0, 540.0, 300.0, 200.0);
        // Центр внутри источника — размещение возвращается как есть, даже
        // если соседний монитор перекрывает ту же область.
        assert_eq!(rebind_monitor_by_center(&p, &src, &[right]), p);
    }

    #[test]
    fn crosses_into_adjacent_monitor_each_direction() {
        // Источник с каждой стороны сосед; центр выходит за грань источника
        // на 30 физических px. Ожидаемый центр — в локальных DIP соседа.
        let cases = [
            (
                "right",
                mon("src", 0, 0, 1920, 1080, 1.0),
                mon("right", 1920, 0, 1920, 1080, 1.0),
                (1925.0, 540.0),
                (5.0, 540.0),
            ),
            (
                "left",
                mon("src", 1920, 0, 1920, 1080, 1.0),
                mon("left", 0, 0, 1920, 1080, 1.0),
                (-10.0, 540.0),
                (1910.0, 540.0),
            ),
            (
                "above",
                mon("src", 0, 1080, 1920, 1080, 1.0),
                mon("above", 0, 0, 1920, 1080, 1.0),
                (960.0, -10.0),
                (960.0, 1070.0),
            ),
            (
                "below",
                mon("src", 0, 0, 1920, 1080, 1.0),
                mon("below", 0, 1080, 1920, 1080, 1.0),
                (960.0, 1090.0),
                (960.0, 10.0),
            ),
        ];
        for (name, src, nbr, (cx, cy), (new_cx, new_cy)) in cases {
            let p = placement(&src, cx, cy, 300.0, 200.0);
            let r = rebind_monitor_by_center(&p, &src, std::slice::from_ref(&nbr));
            assert_eq!(r.monitor_id, nbr.id, "{name}: монитор");
            assert_eq!(r.cx, new_cx, "{name}: cx");
            assert_eq!(r.cy, new_cy, "{name}: cy");
            // Размер в DIP переносится без изменений.
            assert_eq!((r.w, r.h), (300.0, 200.0), "{name}: размер");
        }
    }

    #[test]
    fn point_on_shared_boundary_goes_to_neighbor() {
        let (src, right) = (
            mon("src", 0, 0, 1920, 1080, 1.0),
            mon("right", 1920, 0, 1920, 1080, 1.0),
        );
        // Центр ровно на правом краю источника: граница не принадлежит
        // источнику (полуоткрытый интервал) — стикер уходит соседу.
        let p = placement(&src, 1920.0, 540.0, 300.0, 200.0);
        let r = rebind_monitor_by_center(&p, &src, &[right]);
        assert_eq!(r.monitor_id, MonitorId("right".to_string()));
        assert_eq!((r.cx, r.cy), (0.0, 540.0));
    }

    #[test]
    fn overlapping_targets_pick_first_in_list() {
        let (src, clone_a, clone_b) = (
            mon("src", 0, 0, 1920, 1080, 1.0),
            mon("clone_a", 1920, 0, 1920, 1080, 1.0),
            mon("clone_b", 1920, 0, 1920, 1080, 1.0),
        );
        // Оба клона содержат точку — побеждает первый в списке (тай-брейк).
        let p = placement(&src, 2500.0, 540.0, 300.0, 200.0);
        let r = rebind_monitor_by_center(&p, &src, &[clone_a, clone_b]);
        assert_eq!(r.monitor_id, MonitorId("clone_a".to_string()));
    }

    #[test]
    fn mixed_dpi_keeps_exact_physical_position() {
        // Источник 100% (0,0,1920,1080); цель 150% (1920,0,1280,720):
        // центр на 6 физических px правее края источника.
        let (src, target) = (
            mon("src", 0, 0, 1920, 1080, 1.0),
            mon("target", 1920, 0, 1280, 720, 1.5),
        );
        let p = placement(&src, 1926.0, 540.0, 300.0, 200.0);
        let r = rebind_monitor_by_center(&p, &src, std::slice::from_ref(&target));
        assert_eq!(r.monitor_id, MonitorId("target".to_string()));
        // DIP нового монитора: (1926−1920)/1.5 = 4, 540/1.5 = 360 — ровно.
        assert_eq!((r.cx, r.cy), (4.0, 360.0));
        // Round-trip в физические пиксели виртуального десктопа.
        let (gx, gy) = local_dip_to_global_px(r.cx, r.cy, &target);
        assert_eq!((gx, gy), (1926.0, 540.0));
    }

    #[test]
    fn mixed_dpi_source_not_100_percent() {
        // Источник 150% (1920,0,1280,720); цель 100% (3200,0,1920,1080).
        let (src, target) = (
            mon("src", 1920, 0, 1280, 720, 1.5),
            mon("target", 3200, 0, 1920, 1080, 1.0),
        );
        // 1290 DIP источника → 1935 физических px от его начала → 3855 глобально.
        let p = placement(&src, 1290.0, 360.0, 300.0, 200.0);
        let r = rebind_monitor_by_center(&p, &src, std::slice::from_ref(&target));
        assert_eq!(r.monitor_id, MonitorId("target".to_string()));
        assert_eq!((r.cx, r.cy), (655.0, 540.0));
        let (gx, gy) = local_dip_to_global_px(r.cx, r.cy, &target);
        assert_eq!((gx, gy), (3855.0, 540.0));
    }

    #[test]
    fn center_in_gap_or_beyond_all_is_noop() {
        let (src, far) = (
            mon("src", 0, 0, 1920, 1080, 1.0),
            mon("far", 3000, 0, 1920, 1080, 1.0),
        );
        // Разрыв между мониторами (1920..3000) — никто не забирает.
        let p = placement(&src, 2500.0, 540.0, 300.0, 200.0);
        assert_eq!(
            rebind_monitor_by_center(&p, &src, std::slice::from_ref(&far)),
            p
        );
        // Центр дальше всех мониторов.
        let p = placement(&src, 5000.0, 5000.0, 300.0, 200.0);
        assert_eq!(rebind_monitor_by_center(&p, &src, &[far]), p);
    }

    #[test]
    fn degenerate_targets_are_not_claimed() {
        let src = mon("src", 0, 0, 1920, 1080, 1.0);
        // Нулевой масштаб и нулевые границы — цель не может быть выбрана.
        let bad_scale = mon("bad_scale", 1920, 0, 1920, 1080, 0.0);
        let zero_bounds = mon("zero_bounds", 1920, 0, 0, 0, 1.0);
        let p = placement(&src, 2500.0, 540.0, 300.0, 200.0);
        assert_eq!(
            rebind_monitor_by_center(&p, &src, &[bad_scale, zero_bounds]),
            p
        );
    }

    #[test]
    fn nan_center_is_noop() {
        let (src, right) = (
            mon("src", 0, 0, 1920, 1080, 1.0),
            mon("right", 1920, 0, 1920, 1080, 1.0),
        );
        let p = placement(&src, f64::NAN, 540.0, 300.0, 200.0);
        let r = rebind_monitor_by_center(&p, &src, &[right]);
        // NaN не содержится ни в одном прямоугольнике — размещение без
        // изменений (монитор тот же, координаты перенесены как были).
        assert_eq!(r.monitor_id, p.monitor_id);
        assert!(r.cx.is_nan());
        assert_eq!((r.cy, r.w, r.h), (540.0, 300.0, 200.0));
    }

    #[test]
    fn empty_others_is_noop() {
        let src = mon("src", 0, 0, 1920, 1080, 1.0);
        let p = placement(&src, 2500.0, 540.0, 300.0, 200.0);
        assert_eq!(rebind_monitor_by_center(&p, &src, &[]), p);
    }
}
