//! Миграция стикера при отключении монитора (SPEC.md, раздел 6.1; ADR-011):
//! пропорциональный перенос на другой монитор с сохранением «дома» в
//! [`Origin`] и обратный возврат. Чистая логика: без ввода-вывода и без
//! таймеров — обнаружение пропажи монитора и 20-секундный таймер это
//! интеграция следующего среза (docs/M3_PREP_NOTES.md, раздел 5.3).

use chrono::Utc;

use crate::hittest::DipRect;
use crate::model::{Origin, Placement, Rect, Transform};
use crate::snap::clamp_min_visible;

/// Мигрировать стикер с `old_monitor` на `new_monitor` (SPEC 6.1 п.3):
/// позиция и размер масштабируются пропорционально относительно границ
/// мониторов, затем применяется правило границ ([`clamp_min_visible`],
/// SPEC 3.4: минимум 10% видимо с каждой стороны). Исходные монитор,
/// координаты и поворот сохраняются в возвращаемый [`Origin`] — «дом» для
/// автовозврата (п.4), `migrated_at` — момент миграции.
///
/// Прямоугольники мониторов задают пропорции переноса; единицы внутри пары
/// не смешиваются (в сигнатуре `Rect` в физических пикселях, `Placement`
/// в DIP — важен только относительный перенос). Монитор-приёмник функция
/// не знает (в сигнатуре — только границы): возвращаемый `Placement`
/// сохраняет исходный `monitor_id`, вызывающий слой подставляет реальный id
/// монитора-приёмника.
///
/// Вырожденные входы (нулевой размер монитора, нечисловая/нулевая/отрицательная
/// геометрия стикера, нечисловой поворот) не мигрируются — геометрия
/// возвращается как есть (то же соглашение, что у `clamp_min_visible`),
/// а `origin` всё равно сохраняется.
pub fn migrate_to_monitor(
    placement: &Placement,
    transform: &Transform,
    old_monitor: &Rect,
    new_monitor: &Rect,
) -> (Placement, Origin) {
    let origin = Origin {
        monitor_id: placement.monitor_id.clone(),
        cx: placement.cx,
        cy: placement.cy,
        w: placement.w,
        h: placement.h,
        rotation: transform.rotation,
        migrated_at: Utc::now(),
    };

    // Пропорциональное масштабирование определено только для невырожденных
    // мониторов и валидного стикера; иначе миграция — no-op по геометрии.
    let scalable = old_monitor.w > 0
        && old_monitor.h > 0
        && new_monitor.w > 0
        && new_monitor.h > 0
        && placement.cx.is_finite()
        && placement.cy.is_finite()
        && finite_positive(placement.w)
        && finite_positive(placement.h)
        && transform.rotation.is_finite();
    if !scalable {
        return (placement.clone(), origin);
    }

    let scale_x = new_monitor.w as f64 / old_monitor.w as f64;
    let scale_y = new_monitor.h as f64 / old_monitor.h as f64;
    let mut migrated = Placement {
        monitor_id: placement.monitor_id.clone(),
        cx: new_monitor.x as f64 + (placement.cx - old_monitor.x as f64) * scale_x,
        cy: new_monitor.y as f64 + (placement.cy - old_monitor.y as f64) * scale_y,
        w: placement.w * scale_x,
        h: placement.h * scale_y,
    };
    // Правило границ (SPEC 3.4) — после масштабирования, как в SPEC 6.1 п.3.
    migrated = clamp_min_visible(
        &migrated,
        transform.rotation,
        DipRect::new(
            new_monitor.x as f64,
            new_monitor.y as f64,
            new_monitor.w as f64,
            new_monitor.h as f64,
        ),
    );
    (migrated, origin)
}

/// Двигался ли стикер после миграции: `true` — пользователь его не трогал,
/// значит при возврате монитора его можно автоматически вернуть домой из
/// `origin` (SPEC 6.1 п.4); `false` — геометрия изменена, оставить на месте.
///
/// Проверка — точное сравнение `current_placement` с геометрией «дома» из
/// `origin`. Корректно, когда миграция не меняла геометрию (одинаковые
/// границы мониторов) — тогда результат миграции совпадает с `origin`.
/// В общем случае результат миграции (масштабирование по соотношению
/// размеров мониторов) из пары `(current, origin)` восстановить нельзя —
/// сигнатура не содержит прямоугольников мониторов, — поэтому при
/// изменившейся геометрии функция консервативна (вернёт `false`). Основной
/// сигнал «не двигал» — очистка `origin` любой пользовательской правкой
/// placement (docs/M3_PREP_NOTES.md, §5.3); этот вызов — страховка-проверка.
pub fn should_auto_return(current_placement: &Placement, origin: &Origin) -> bool {
    current_placement.cx == origin.cx
        && current_placement.cy == origin.cy
        && current_placement.w == origin.w
        && current_placement.h == origin.h
}

/// Собрать `Placement` обратно из [`Origin`] (обратная операция миграции,
/// SPEC 6.1 п.4): монитор и координаты «дома». Поворот в `Placement` не
/// переносится — его вызывающий слой восстанавливает отдельно из
/// `origin.rotation` в `Transform`.
pub fn return_from_origin(origin: &Origin) -> Placement {
    Placement {
        monitor_id: origin.monitor_id.clone(),
        cx: origin.cx,
        cy: origin.cy,
        w: origin.w,
        h: origin.h,
    }
}

/// Конечный и положительный размер (для f64-геометрии стикера): `NaN`,
/// `±inf` и неположительные значения вырожденны и не мигрируются.
fn finite_positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonitorId;
    use std::f64::consts::FRAC_PI_2;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn placement(monitor: &str, cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            monitor_id: MonitorId(monitor.to_string()),
            cx,
            cy,
            w,
            h,
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

    /// Сравнение геометрии с учётом NaN: совпадает, если оба значения NaN
    /// либо равны (обычный `assert_eq!` на f64 с NaN ломается — NaN != NaN).
    fn assert_close_or_nan(actual: f64, expected: f64) {
        if expected.is_nan() {
            assert!(actual.is_nan(), "ожидался NaN, получено {actual}");
        } else {
            assert_close(actual, expected);
        }
    }

    fn assert_same_geometry(a: &Placement, b: &Placement) {
        assert_eq!(a.monitor_id, b.monitor_id);
        assert_close_or_nan(a.cx, b.cx);
        assert_close_or_nan(a.cy, b.cy);
        assert_close_or_nan(a.w, b.w);
        assert_close_or_nan(a.h, b.h);
    }

    #[test]
    fn scales_position_and_size_proportionally() {
        // 1920x1080 -> 3840x2160 (вдвое): центр и размер удваиваются.
        let home = placement("home", 960.0, 540.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 3840, 2160);
        let (migrated, _) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert_close(migrated.cx, 1920.0);
        assert_close(migrated.cy, 1080.0);
        assert_close(migrated.w, 200.0);
        assert_close(migrated.h, 100.0);
        // id не знаем — сохраняется исходный (вызывающий слой подставит свой).
        assert_eq!(migrated.monitor_id, home.monitor_id);
    }

    #[test]
    fn offset_target_monitor_translates_center() {
        // Те же размеры — геометрия как есть, позиция со сдвигом на new.x/new.y.
        let home = placement("home", 100.0, 200.0, 80.0, 40.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(1920, 0, 1920, 1080);
        let (migrated, _) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert_close(migrated.cx, 2020.0);
        assert_close(migrated.cy, 200.0);
        assert_close(migrated.w, 80.0);
        assert_close(migrated.h, 40.0);
    }

    #[test]
    fn negative_target_coordinates_are_respected() {
        // Второй монитор слева от основного: x отрицательный.
        let home = placement("home", 960.0, 540.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(-1920, 0, 1920, 1080);
        let (migrated, _) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert_close(migrated.cx, -960.0);
        assert_close(migrated.cy, 540.0);
    }

    #[test]
    fn different_aspect_ratios_scale_axes_independently() {
        // sx = 1000/1920, sy = 2000/1080 — оси масштабируются раздельно.
        let home = placement("home", 960.0, 540.0, 200.0, 100.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 1000, 2000);
        let (migrated, _) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert_close(migrated.cx, 960.0 * 1000.0 / 1920.0);
        assert_close(migrated.cy, 540.0 * 2000.0 / 1080.0);
        assert_close(migrated.w, 200.0 * 1000.0 / 1920.0);
        assert_close(migrated.h, 100.0 * 2000.0 / 1080.0);
    }

    #[test]
    fn applies_boundary_rule_after_scaling() {
        // Стикер далеко за правым краем мигрировавшего монитора: после
        // масштабирования — правило границ (SPEC 3.4). Те же границы — геометрия
        // после масштаба = исходная, клампится как в snap.rs.
        let home = placement("home", 5000.0, 540.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 1920, 1080);
        let (migrated, _) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert_close(migrated.cx, 1960.0);
        assert_close(migrated.cy, 540.0);
    }

    #[test]
    fn boundary_rule_uses_rotation_for_aabb() {
        // 100x40 при повороте 90°: bbox 40x100 — по ширине ограничение 40.
        let home = placement("home", -5000.0, 540.0, 100.0, 40.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 1920, 1080);
        let (migrated, _) = migrate_to_monitor(&home, &transform(FRAC_PI_2), &old, &new);
        assert_close(migrated.cx, -16.0);
        assert_close(migrated.cy, 540.0);
    }

    #[test]
    fn origin_saves_home_monitor_and_geometry() {
        let home = placement("home", 100.0, 200.0, 300.0, 150.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 3840, 2160);
        let (_, origin) = migrate_to_monitor(&home, &transform(1.25), &old, &new);
        assert_eq!(origin.monitor_id, MonitorId("home".to_string()));
        assert_close(origin.cx, 100.0);
        assert_close(origin.cy, 200.0);
        assert_close(origin.w, 300.0);
        assert_close(origin.h, 150.0);
        assert_close(origin.rotation, 1.25);
        let age = Utc::now() - origin.migrated_at;
        assert!(age.num_seconds() < 5, "migrated_at свежий");
    }

    #[test]
    fn degenerate_monitor_keeps_geometry() {
        for (old, new) in [
            (rect(0, 0, 0, 1080), rect(0, 0, 1920, 1080)),
            (rect(0, 0, 1920, 0), rect(0, 0, 1920, 1080)),
            (rect(0, 0, 1920, 1080), rect(0, 0, 0, 1080)),
            (rect(0, 0, 1920, 1080), rect(0, 0, 1920, 0)),
        ] {
            let home = placement("home", 960.0, 540.0, 100.0, 50.0);
            let (migrated, _) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
            assert_eq!(migrated, home, "вырожденный монитор: {old:?} -> {new:?}");
        }
    }

    #[test]
    fn nan_or_degenerate_placement_keeps_geometry() {
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 3840, 2160);
        for bad in [
            placement("home", f64::NAN, 540.0, 100.0, 50.0),
            placement("home", 960.0, f64::NAN, 100.0, 50.0),
            placement("home", 960.0, 540.0, f64::NAN, 50.0),
            placement("home", 960.0, 540.0, 100.0, f64::NAN),
            placement("home", 960.0, 540.0, 0.0, 50.0),
            placement("home", 960.0, 540.0, -10.0, 50.0),
        ] {
            let (migrated, _) = migrate_to_monitor(&bad, &transform(0.0), &old, &new);
            assert_same_geometry(&migrated, &bad);
        }
        // NaN-поворот: кламп с ним неприменим — геометрия не трогается.
        let home = placement("home", 960.0, 540.0, 100.0, 50.0);
        let (migrated, _) = migrate_to_monitor(&home, &transform(f64::NAN), &old, &new);
        assert_same_geometry(&migrated, &home);
    }

    #[test]
    fn return_from_origin_reconstructs_home_placement() {
        let origin = Origin {
            monitor_id: MonitorId("home".to_string()),
            cx: 100.0,
            cy: 200.0,
            w: 300.0,
            h: 150.0,
            rotation: 0.5,
            migrated_at: Utc::now(),
        };
        let p = return_from_origin(&origin);
        assert_eq!(p.monitor_id, MonitorId("home".to_string()));
        assert_close(p.cx, 100.0);
        assert_close(p.cy, 200.0);
        assert_close(p.w, 300.0);
        assert_close(p.h, 150.0);
    }

    #[test]
    fn migrate_then_return_restores_home() {
        let home = placement("home", 123.0, 456.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 3840, 2160);
        let (_, origin) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        let back = return_from_origin(&origin);
        assert_eq!(back.monitor_id, home.monitor_id);
        assert_close(back.cx, home.cx);
        assert_close(back.cy, home.cy);
        assert_close(back.w, home.w);
        assert_close(back.h, home.h);
    }

    #[test]
    fn untouched_sticker_should_auto_return() {
        // Одинаковые границы мониторов: миграция геометрию не меняет, поэтому
        // результат миграции совпадает с origin — «не двигал» определяется.
        let home = placement("home", 960.0, 540.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 1920, 1080);
        let (migrated, origin) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert!(should_auto_return(&migrated, &origin));
    }

    #[test]
    fn moved_sticker_should_not_auto_return() {
        let home = placement("home", 960.0, 540.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 1920, 1080);
        let (mut migrated, origin) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        migrated.cx += 100.0;
        assert!(!should_auto_return(&migrated, &origin), "сдвиг");
        migrated.cx -= 100.0;
        migrated.w *= 1.5;
        assert!(!should_auto_return(&migrated, &origin), "ресайз");
    }

    #[test]
    fn scaled_migration_is_conservative() {
        // Разные границы мониторов: миграция масштабирует геометрию, а
        // сигнатура не позволяет восстановить результат миграции — функция
        // консервативна (false). Основной сигнал «не двигал» — очистка origin
        // пользовательской правкой (docs/M3_PREP_NOTES.md, §5.3).
        let home = placement("home", 960.0, 540.0, 100.0, 50.0);
        let old = rect(0, 0, 1920, 1080);
        let new = rect(0, 0, 3840, 2160);
        let (migrated, origin) = migrate_to_monitor(&home, &transform(0.0), &old, &new);
        assert!(!should_auto_return(&migrated, &origin));
    }
}
