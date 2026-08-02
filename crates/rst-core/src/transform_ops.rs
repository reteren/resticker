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
/// - ручку можно протащить через якорь: отрицательный размер превращается
///   в переключение `flip_h`/`flip_v` (зеркалирование, SPEC 3.3), размеры
///   в `placement` остаются положительными.
pub fn resize(
    placement: &Placement,
    transform: &Transform,
    handle: HandleKind,
    delta: (f64, f64),
    modifiers: DragModifiers,
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

    // Знаковые размеры: зеркалирование ещё не свёрнуто в flip-флаги.
    new_w = clamp_min_abs(new_w, MIN_SIZE_DIP);
    new_h = clamp_min_abs(new_h, MIN_SIZE_DIP);

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
    if ![gx, gy, px, py, transform.rotation].iter().all(|v| v.is_finite()) {
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
    if v < 0.0 { v.min(-min_abs) } else { v.max(min_abs) }
}
// __CHUNK3__
