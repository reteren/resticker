//! Трансформации стикера перетаскиванием ручек: ресайз за 8 ручек и поворот
//! за кольцевую зону угловой ручки (SPEC.md, раздел 3.3; ROADMAP.md M2).
//!
//! Чистые функции без зависимостей от окна и ввода. Контракт — «стартовое
//! состояние жеста + суммарная дельта мыши»: координатор хранит снимок
//! `Placement`/`Transform` на момент `MouseDown` и каждый `MouseMove`
//! пересчитывает результат от него, а не инкрементально — так не копится
//! ошибка округления и модификаторы можно зажимать/отпускать на лету.
//!
//! Все координаты — DIP (ADR-010), поворот — радианы по часовой стрелке
//! (конвенция шейдера спрайта, см. hittest.rs).

use std::f64::consts::PI;

use crate::hittest::{HandleKind, to_local, to_world};
use crate::model::{Placement, Transform};

/// Минимальный размер стикера, DIP (SPEC 3.3). По модулю: при
/// зеркалировании (ручка протащена через якорь) размер проходит через
/// «мёртвую зону» (−16..+16) скачком — размер не бывает меньше по величине.
pub const MIN_SIZE_DIP: f64 = 16.0;

/// Шаг поворота с зажатым `Shift` — 15° (SPEC 3.3), в радианах.
pub const ROTATE_SNAP_STEP: f64 = PI / 12.0;

/// Модификаторы, влияющие на жест трансформации (Ctrl — отключение магнита,
/// обрабатывается не здесь, а в `snap`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DragModifiers {
    /// Ресайз — сохранить пропорции стартового состояния; поворот — шаг 15°.
    pub shift: bool,
    /// Ресайз от центра вместо фиксации противоположного края.
    pub alt: bool,
}

/// Результат жеста: новое состояние стикера.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformedState {
    pub placement: Placement,
    pub transform: Transform,
}

impl TransformedState {
    fn new(placement: &Placement, transform: &Transform) -> Self {
        Self {
            placement: placement.clone(),
            transform: *transform,
        }
    }
}

/// Ресайз перетаскиванием ручки `handle` на суммарную дельту мыши `delta`
/// (DIP, мировые координаты монитора) от начала жеста.
///
/// Семантика (SPEC 3.3):
/// - противоположная ручке грань/угол — якорь, в миру он неподвижен; с
///   `Alt` якорем становится центр, размер меняется симметрично (удвоенная
///   дельта);
/// - `Shift` — пропорции стартового состояния: общий масштаб задаёт
///   доминирующая ось (наибольшее относительное изменение);
/// - минимальный размер — [`MIN_SIZE_DIP`];
/// - `allow_mirror: true` — ручку можно протащить через якорь: отрицательный
///   размер превращается в переключение `flip_h`/`flip_v` (зеркалирование,
///   фидбэк пользователя 2026-08-09), размеры в `placement` остаются
///   положительными; `allow_mirror: false` — видео (та же дата, «эта
///   механика не касается видео»): размер просто не уходит ниже
///   [`MIN_SIZE_DIP`], зеркалирования не происходит.
pub fn resize(
    placement: &Placement,
    transform: &Transform,
    handle: HandleKind,
    delta: (f64, f64),
    modifiers: DragModifiers,
    allow_mirror: bool,
) -> TransformedState {
    let mut out = TransformedState::new(placement, transform);
    let start_ok = [placement.w, placement.h, transform.rotation]
        .iter()
        .all(|v| v.is_finite())
        && placement.w >= 0.0
        && placement.h >= 0.0;
    if !start_ok || !delta.0.is_finite() || !delta.1.is_finite() {
        return out;
    }

    let (sx, sy) = handle.local_sign();
    // Дельта мыши — в локальные оси стикера (они же оси рамки выделения).
    let (dx, dy) = to_local(0.0, 0.0, transform.rotation, delta.0, delta.1);
    // Вклад в размеры: рост при движении «от якоря». У боковой ручки знак
    // по незадействованной оси равен нулю — та ось не меняется.
    let mut dw = sx * dx;
    let mut dh = sy * dy;
    if modifiers.alt {
        // От центра: симметричный рост с двух сторон.
        dw *= 2.0;
        dh *= 2.0;
    }
    let mut new_w = placement.w + dw;
    let mut new_h = placement.h + dh;

    if modifiers.shift && placement.w > 0.0 && placement.h > 0.0 {
        let scale_w = new_w / placement.w;
        let scale_h = new_h / placement.h;
        let s = if (scale_w - 1.0).abs() >= (scale_h - 1.0).abs() {
            scale_w
        } else {
            scale_h
        };
        new_w = placement.w * s;
        new_h = placement.h * s;
    }

    if allow_mirror {
        // Знаковые размеры: зеркалирование ещё не свёрнуто в flip-флаги.
        new_w = clamp_min_abs(new_w, MIN_SIZE_DIP);
        new_h = clamp_min_abs(new_h, MIN_SIZE_DIP);
    } else {
        // Видео не зеркалим — размер не уходит ниже минимума ни в какую
        // сторону, знак не несёт зеркалирования.
        new_w = new_w.max(MIN_SIZE_DIP);
        new_h = new_h.max(MIN_SIZE_DIP);
    }

    if !modifiers.alt {
        // Якорь (противоположная грань/угол) обязан остаться на месте в миру.
        // Считаем со знаковыми размерами: после зеркалирования якорь
        // оказывается на противоположной стороне нового прямоугольника.
        let (ax_old, ay_old) = (-sx * placement.w / 2.0, -sy * placement.h / 2.0);
        let (ax_new, ay_new) = (-sx * new_w / 2.0, -sy * new_h / 2.0);
        let (ox, oy) = to_world(
            0.0,
            0.0,
            transform.rotation,
            ax_old - ax_new,
            ay_old - ay_new,
        );
        out.placement.cx = placement.cx + ox;
        out.placement.cy = placement.cy + oy;
    }

    // Зеркалирование складывается в flip-флаги; размеры храним положительными.
    // При `allow_mirror: false` знак сюда не доходит — new_w/new_h уже
    // неотрицательны после `.max(MIN_SIZE_DIP)` выше.
    if new_w < 0.0 {
        new_w = -new_w;
        out.transform.flip_h = !out.transform.flip_h;
    }
    if new_h < 0.0 {
        new_h = -new_h;
        out.transform.flip_v = !out.transform.flip_v;
    }
    out.placement.w = new_w;
    out.placement.h = new_h;
    out
}
/// Поворот перетаскиванием ручки поворота (кольцевая зона за угловой
/// ручкой, SPEC 3.3). `grab` — мировая точка захвата (позиция `MouseDown`),
/// `current` — текущая мировая позиция курсора, обе в DIP. Дельта угла
/// считается между векторами «центр → захват» и «центр → курсор», поэтому
/// сама ручка (какой угол рамки) на результат не влияет — поворот всегда
/// вокруг центра ограничивающего прямоугольника (SPEC 3.3).
///
/// `Shift` — шаг [`ROTATE_SNAP_STEP`] (15°), применяется к итоговому углу.
/// Результат нормализуется в (−π, π], чтобы `rotation` не рос между жестами.
pub fn rotate(
    placement: &Placement,
    transform: &Transform,
    grab: (f64, f64),
    current: (f64, f64),
    modifiers: DragModifiers,
) -> TransformedState {
    let mut out = TransformedState::new(placement, transform);
    let (gx, gy) = (grab.0 - placement.cx, grab.1 - placement.cy);
    let (px, py) = (current.0 - placement.cx, current.1 - placement.cy);
    if ![gx, gy, px, py, transform.rotation]
        .iter()
        .all(|v| v.is_finite())
    {
        return out;
    }
    // Угол между векторами со знаком: по часовой положительный (экранная
    // ось Y направлена вниз, конвенция шейдера — см. hittest::to_world).
    let cross = gx * py - gy * px;
    let dot = gx * px + gy * py;
    let mut rotation = transform.rotation + cross.atan2(dot);
    if modifiers.shift {
        rotation = (rotation / ROTATE_SNAP_STEP).round() * ROTATE_SNAP_STEP;
    }
    out.transform.rotation = normalize_angle(rotation);
    out
}

/// Нормализация угла в (−π, π] (конфиг хранит радианы без канонического
/// диапазона; нормализуем на каждом жесте, чтобы значение не копилось).
fn normalize_angle(angle: f64) -> f64 {
    let a = angle.rem_euclid(2.0 * PI);
    if a > PI { a - 2.0 * PI } else { a }
}

/// Ограничить значение снизу по модулю, сохранив знак (знак несёт
/// зеркалирование до сворачивания в flip-флаги).
fn clamp_min_abs(v: f64, min_abs: f64) -> f64 {
    if v < 0.0 {
        v.min(-min_abs)
    } else {
        v.max(min_abs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonitorId;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

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

    fn mods(shift: bool, alt: bool) -> DragModifiers {
        DragModifiers { shift, alt }
    }

    fn assert_close_ctx(actual: f64, expected: f64, ctx: &str) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "{ctx}: ожидалось {expected}, получено {actual}"
        );
    }

    fn assert_placement(out: &TransformedState, cx: f64, cy: f64, w: f64, h: f64, ctx: &str) {
        assert_close_ctx(out.placement.cx, cx, ctx);
        assert_close_ctx(out.placement.cy, cy, ctx);
        assert_close_ctx(out.placement.w, w, ctx);
        assert_close_ctx(out.placement.h, h, ctx);
    }

    #[test]
    fn resize_axis_aligned_all_handles() {
        // Старт (100, 50, 40, 20), дельта (10, 6): якорь — противоположная
        // грань; N/NW/NE по вертикали упираются в минимум 16.
        let cases: [(HandleKind, (f64, f64, f64, f64)); 8] = [
            (HandleKind::NorthWest, (105.0, 52.0, 30.0, 16.0)),
            (HandleKind::North, (100.0, 52.0, 40.0, 16.0)),
            (HandleKind::NorthEast, (105.0, 52.0, 50.0, 16.0)),
            (HandleKind::East, (105.0, 50.0, 50.0, 20.0)),
            (HandleKind::SouthEast, (105.0, 53.0, 50.0, 26.0)),
            (HandleKind::South, (100.0, 53.0, 40.0, 26.0)),
            (HandleKind::SouthWest, (105.0, 53.0, 30.0, 26.0)),
            (HandleKind::West, (105.0, 50.0, 30.0, 20.0)),
        ];
        for (handle, (cx, cy, w, h)) in cases {
            let ctx = format!("{handle:?}");
            let out = resize(
                &placement(100.0, 50.0, 40.0, 20.0),
                &transform(0.0),
                handle,
                (10.0, 6.0),
                mods(false, false),
                true,
            );
            assert_placement(&out, cx, cy, w, h, &ctx);
            assert!(!out.transform.flip_h && !out.transform.flip_v, "{ctx}");
        }
    }

    #[test]
    fn resize_rotated_90_east_and_west() {
        // Поворот +90°: мировая дельта «вниз» — это локальный +x.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(FRAC_PI_2),
            HandleKind::East,
            (0.0, 10.0),
            mods(false, false),
            true,
        );
        assert_placement(&out, 100.0, 55.0, 50.0, 20.0, "East rot=+90");

        // Поворот −90°: мировая дельта «вниз» — локальный −x (рост для West).
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(-FRAC_PI_2),
            HandleKind::West,
            (0.0, 10.0),
            mods(false, false),
            true,
        );
        assert_placement(&out, 100.0, 55.0, 50.0, 20.0, "West rot=-90");
    }

    #[test]
    fn resize_shift_keeps_proportions_table() {
        // Угол SE: доминирует горизонталь (130/100 > 50/50) → 130x65.
        let out = resize(
            &placement(0.0, 0.0, 100.0, 50.0),
            &transform(0.0),
            HandleKind::SouthEast,
            (30.0, 0.0),
            mods(true, false),
            true,
        );
        assert_placement(&out, 15.0, 7.5, 130.0, 65.0, "SE, dx dominant");

        // Угол SE: доминирует вертикаль (80/50 > 100/100) → 160x80.
        let out = resize(
            &placement(0.0, 0.0, 100.0, 50.0),
            &transform(0.0),
            HandleKind::SouthEast,
            (0.0, 30.0),
            mods(true, false),
            true,
        );
        assert_placement(&out, 30.0, 15.0, 160.0, 80.0, "SE, dy dominant");

        // Боковая ручка E: ведущая ось — ширина, высота следует пропорции.
        let out = resize(
            &placement(0.0, 0.0, 100.0, 50.0),
            &transform(0.0),
            HandleKind::East,
            (30.0, 999.0),
            mods(true, false),
            true,
        );
        assert_placement(&out, 15.0, 0.0, 130.0, 65.0, "E side");

        // Боковая ручка N: ведущая ось — высота (30/50 = 0.6), ширина следует.
        let out = resize(
            &placement(0.0, 0.0, 100.0, 50.0),
            &transform(0.0),
            HandleKind::North,
            (999.0, 20.0),
            mods(true, false),
            true,
        );
        assert_placement(&out, 0.0, 10.0, 60.0, 30.0, "N side");
    }

    #[test]
    fn resize_alt_from_center() {
        // Боковая ручка: дельта удваивается, центр не двигается.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (10.0, 0.0),
            mods(false, true),
            true,
        );
        assert_placement(&out, 100.0, 50.0, 60.0, 20.0, "E alt");

        // Угловая ручка: обе оси удваиваются.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::SouthEast,
            (10.0, 10.0),
            mods(false, true),
            true,
        );
        assert_placement(&out, 100.0, 50.0, 60.0, 40.0, "SE alt");

        // Alt + Shift: пропорции от центра (доминирует ширина: 160/100).
        let out = resize(
            &placement(0.0, 0.0, 100.0, 50.0),
            &transform(0.0),
            HandleKind::SouthEast,
            (30.0, 0.0),
            mods(true, true),
            true,
        );
        assert_placement(&out, 0.0, 0.0, 160.0, 80.0, "SE alt+shift");
    }

    #[test]
    fn resize_min_size_clamps() {
        // Положительная сторона: 40 - 30 = 10 → 16; якорь (западный край) стоит.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (-30.0, 0.0),
            mods(false, false),
            true,
        );
        assert_placement(&out, 88.0, 50.0, 16.0, 20.0, "positive clamp");
    }

    #[test]
    fn resize_flip_through_anchor() {
        // Ручку протащили через якорь: 40 - 70 = -30 → w=30, flip_h,
        // прямоугольник [50, 80] — якорь (западный край, x=80) неподвижен.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (-70.0, 0.0),
            mods(false, false),
            true,
        );
        assert_placement(&out, 65.0, 50.0, 30.0, 20.0, "flip");
        assert!(out.transform.flip_h);
        assert!(!out.transform.flip_v);

        // Возврат за якорь обратно в рамках того же жеста (состояние
        // считается от стартового снимка, а не инкрементально): flip_h
        // не переключается.
        let back = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (10.0, 0.0),
            mods(false, false),
            true,
        );
        assert!(!back.transform.flip_h);

        // Следующий жест от зеркального результата: якорь не пересекается
        // (30 + 70 > 0), поэтому flip_h сохраняется, меняется только размер.
        let grown = resize(
            &out.placement,
            &out.transform,
            HandleKind::East,
            (70.0, 0.0),
            mods(false, false),
            true,
        );
        assert_placement(&grown, 100.0, 50.0, 100.0, 20.0, "grow after flip");
        assert!(grown.transform.flip_h);
    }

    #[test]
    fn resize_flip_with_alt_keeps_center() {
        // 40 + 2·(−70) = −100 → w=100, flip_h, центр на месте.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (-70.0, 0.0),
            mods(false, true),
            true,
        );
        assert_placement(&out, 100.0, 50.0, 100.0, 20.0, "alt flip");
        assert!(out.transform.flip_h);
    }

    #[test]
    fn resize_flip_clamped_negative_side() {
        // 40 - 50 = -10 → −16 по модулю: w=16, flip_h, прямоугольник [64, 80].
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (-50.0, 0.0),
            mods(false, false),
            true,
        );
        assert_placement(&out, 72.0, 50.0, 16.0, 20.0, "negative clamp");
        assert!(out.transform.flip_h);
    }

    #[test]
    fn resize_no_mirror_clamps_instead_of_flipping() {
        // Тот же жест, что resize_flip_through_anchor (протащено далеко за
        // якорь), но allow_mirror=false (видео, фидбэк 2026-08-09): вместо
        // flip_h размер просто останавливается на MIN_SIZE_DIP.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (-70.0, 0.0),
            mods(false, false),
            false,
        );
        assert_placement(&out, 88.0, 50.0, 16.0, 20.0, "no-mirror clamp");
        assert!(!out.transform.flip_h);
        assert!(!out.transform.flip_v);
    }

    #[test]
    fn resize_no_mirror_never_flips_regardless_of_drag_distance() {
        // Ещё дальше за якорь — размер остаётся на минимуме, флаги не
        // переключаются вообще, сколько бы ни тянули.
        let out = resize(
            &placement(100.0, 50.0, 40.0, 20.0),
            &transform(0.0),
            HandleKind::SouthEast,
            (-500.0, -500.0),
            mods(false, false),
            false,
        );
        assert_close_ctx(out.placement.w, MIN_SIZE_DIP, "w stays at minimum");
        assert_close_ctx(out.placement.h, MIN_SIZE_DIP, "h stays at minimum");
        assert!(!out.transform.flip_h);
        assert!(!out.transform.flip_v);
    }

    #[test]
    fn resize_shift_flip_both_axes() {
        // Shift: доминирует ширина (|−0.5 − 1| > |1 − 1|) → масштаб −0.5
        // к обеим осям: зеркалирование по обоим, пропорции сохранены.
        let out = resize(
            &placement(0.0, 0.0, 100.0, 50.0),
            &transform(0.0),
            HandleKind::SouthEast,
            (-150.0, 0.0),
            mods(true, false),
            true,
        );
        assert_placement(&out, -75.0, -37.5, 50.0, 25.0, "shift flip");
        assert!(out.transform.flip_h && out.transform.flip_v);
    }

    #[test]
    fn resize_preserves_monitor_and_transform_fields() {
        let p = Placement {
            monitor_id: MonitorId("DISPLAY#TEST".to_string()),
            ..placement(100.0, 50.0, 40.0, 20.0)
        };
        let t = Transform {
            opacity: 0.5,
            ..transform(0.0)
        };
        let out = resize(
            &p,
            &t,
            HandleKind::East,
            (10.0, 0.0),
            mods(false, false),
            true,
        );
        assert_eq!(out.placement.monitor_id, p.monitor_id);
        assert_close_ctx(out.transform.opacity, 0.5, "opacity");
        assert_close_ctx(out.transform.rotation, 0.0, "rotation");
    }

    #[test]
    fn resize_rejects_garbage() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        let t = transform(0.0);
        // NaN-дельта — состояние не меняется.
        let out = resize(
            &p,
            &t,
            HandleKind::East,
            (f64::NAN, 0.0),
            mods(false, false),
            true,
        );
        assert_eq!(out.placement, p);
        assert_eq!(out.transform, t);
        // Отрицательный стартовый размер (битый конфиг) — состояние не меняется.
        let bad = placement(0.0, 0.0, -5.0, 20.0);
        let out = resize(
            &bad,
            &t,
            HandleKind::East,
            (10.0, 0.0),
            mods(false, false),
            true,
        );
        assert_eq!(out.placement, bad);
    }

    #[test]
    fn resize_degenerate_zero_size_no_nan() {
        // Нулевая ширина: пропорции не определены (Shift пропускается),
        // но функция обязана отработать без NaN.
        let out = resize(
            &placement(100.0, 50.0, 0.0, 20.0),
            &transform(0.0),
            HandleKind::East,
            (10.0, 0.0),
            mods(true, false),
            true,
        );
        assert!(out.placement.w.is_finite());
        assert_close_ctx(out.placement.w, MIN_SIZE_DIP, "clamped from zero");
    }

    #[test]
    fn anchor_stays_fixed_for_all_handles_and_rotations() {
        for rotation in [0.0, FRAC_PI_4, FRAC_PI_2, -1.2] {
            for handle in HandleKind::ALL {
                let ctx = format!("{handle:?} rot={rotation}");
                let p = placement(120.0, 80.0, 60.0, 30.0);
                let t = transform(rotation);
                let out = resize(&p, &t, handle, (14.0, -9.0), mods(false, false), true);
                let (sx, sy) = handle.local_sign();
                // Мировая позиция якоря (противоположной грани/угла) до и
                // после ресайза обязана совпасть.
                let before = to_world(p.cx, p.cy, rotation, -sx * p.w / 2.0, -sy * p.h / 2.0);
                let after = to_world(
                    out.placement.cx,
                    out.placement.cy,
                    rotation,
                    -sx * out.placement.w / 2.0,
                    -sy * out.placement.h / 2.0,
                );
                assert_close_ctx(after.0, before.0, &ctx);
                assert_close_ctx(after.1, before.1, &ctx);
            }
        }
    }

    #[test]
    fn shift_keeps_aspect_for_all_handles() {
        for handle in HandleKind::ALL {
            let out = resize(
                &placement(0.0, 0.0, 100.0, 40.0),
                &transform(0.0),
                handle,
                (25.0, 10.0),
                mods(true, false),
                true,
            );
            assert_close_ctx(
                out.placement.w / out.placement.h,
                100.0 / 40.0,
                &format!("{handle:?}"),
            );
        }
    }

    #[test]
    fn rotate_clockwise_and_counterclockwise() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        // Захват справа от центра, курсор ушёл вниз: по часовой → +90°
        // (конвенция шейдера: локальная точка (1,0) уходит в (0,1)).
        let out = rotate(
            &p,
            &transform(0.0),
            (200.0, 50.0),
            (100.0, 150.0),
            mods(false, false),
        );
        assert_close_ctx(out.transform.rotation, FRAC_PI_2, "clockwise");

        // Курсор ушёл вверх: против часовой → −90°.
        let out = rotate(
            &p,
            &transform(0.0),
            (200.0, 50.0),
            (100.0, -50.0),
            mods(false, false),
        );
        assert_close_ctx(out.transform.rotation, -FRAC_PI_2, "counterclockwise");

        // Дельта прибавляется к стартовому повороту.
        let out = rotate(
            &p,
            &transform(0.3),
            (200.0, 50.0),
            (100.0, 150.0),
            mods(false, false),
        );
        assert_close_ctx(out.transform.rotation, 0.3 + FRAC_PI_2, "from 0.3");
    }

    #[test]
    fn rotate_shift_snaps_to_15_degrees() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        // 0.3 + 90° = 107.19° → ближайший шаг 15° — 105° = 7·π/12.
        let out = rotate(
            &p,
            &transform(0.3),
            (200.0, 50.0),
            (100.0, 150.0),
            mods(true, false),
        );
        assert_close_ctx(out.transform.rotation, 7.0 * PI / 12.0, "snap to 105°");

        // Ровно 90° — кратно 15°, не меняется.
        let out = rotate(
            &p,
            &transform(0.0),
            (200.0, 50.0),
            (100.0, 150.0),
            mods(true, false),
        );
        assert_close_ctx(out.transform.rotation, FRAC_PI_2, "already multiple");
    }

    #[test]
    fn rotate_normalizes_to_minus_pi_pi() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        // Курсор на +0.6 рад по часовой от захвата: 3.0 + 0.6 = 3.6 > π.
        let current = (100.0 + 100.0 * 0.6_f64.cos(), 50.0 + 100.0 * 0.6_f64.sin());
        let out = rotate(
            &p,
            &transform(3.0),
            (200.0, 50.0),
            current,
            mods(false, false),
        );
        assert_close_ctx(out.transform.rotation, 3.6 - 2.0 * PI, "wrapped");
    }

    #[test]
    fn rotate_grab_at_center_is_noop() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        // Нулевой вектор захвата: угол не определён, поворот не меняется.
        let out = rotate(
            &p,
            &transform(0.7),
            (100.0, 50.0),
            (150.0, 90.0),
            mods(false, false),
        );
        assert_close_ctx(out.transform.rotation, 0.7, "grab at center");
    }

    #[test]
    fn rotate_rejects_nan() {
        let p = placement(100.0, 50.0, 40.0, 20.0);
        let t = Transform {
            opacity: 0.5,
            ..transform(0.3)
        };
        let out = rotate(&p, &t, (200.0, 50.0), (f64::NAN, 0.0), mods(false, false));
        assert_eq!(out.placement, p);
        assert_eq!(out.transform, t);
    }
}
