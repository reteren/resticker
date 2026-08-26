//! Таблица готовых раскладок группы окон (режим редактирования — «лента
//! раскладок»).
//!
//! Пользователь собирает группу из 2..8 окон и выбирает из ленты, как эти
//! окна разложить на рабочей области экрана. Здесь живёт сама таблица
//! раскладок — в долях рабочей области, безразмерно, не зависит от разрешения
//! и DPI — и перевод раскладки в пиксели с зазором ([`apply`]).
//!
//! Крейт платформенно-чистый (CONTRIBUTING.md, «Правило зависимостей»):
//! только геометрия и юнит-тесты, никакого Win32.

use std::sync::OnceLock;

use crate::model::Rect;

/// Прямоугольник в долях рабочей области: `x`, `y` — от левого верхнего угла
/// области, `w`, `h` — доли её ширины/высоты. Все значения лежат в 0.0..=1.0.
///
/// Доли, а не пиксели, — раскладка не зависит от разрешения экрана и DPI;
/// в пиксели её переводит [`apply`] уже для конкретной рабочей области.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Одна раскладка группы: `slots[i]` — слот номер `i + 1`.
///
/// Порядок слотов — это порядок выбора окон пользователем: слот 1 (окно,
/// выбранное первым) в раскладках «главное + стопка» — главный, самый
/// большой. Поэтому вся таблица устроена так, что слот 1 не меньше любого
/// другого (тест `slot_one_is_never_smaller_than_its_neighbours`).
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub slots: Vec<UnitRect>,
}

/// Сокращение записи слотов: `rect!(0.0, 0.0, 0.5, 1.0)` вместо литерала —
/// строки таблицы раскладок остаются читаемыми колонками.
macro_rules! rect {
    ($x:expr, $y:expr, $w:expr, $h:expr) => {
        UnitRect {
            x: $x,
            y: $y,
            w: $w,
            h: $h,
        }
    };
}

/// Равные колонки: `n` слотов одинаковой ширины на всю высоту.
fn columns(n: usize) -> Preset {
    let w = 1.0 / n as f64;
    Preset {
        slots: (0..n).map(|i| rect!(i as f64 * w, 0.0, w, 1.0)).collect(),
    }
}

/// Равные ряды: `n` слотов одинаковой высоты на всю ширину.
fn rows(n: usize) -> Preset {
    let h = 1.0 / n as f64;
    Preset {
        slots: (0..n).map(|i| rect!(0.0, i as f64 * h, 1.0, h)).collect(),
    }
}

/// «Главное + стопка»: главный слот слева на всю высоту, остальные `n - 1` —
/// равными рядами в правой колонке.
fn stack_right(n: usize) -> Preset {
    let mut slots = Vec::with_capacity(n);
    slots.push(rect!(0.0, 0.0, 0.5, 1.0));
    let h = 1.0 / (n - 1) as f64;
    for i in 0..n - 1 {
        slots.push(rect!(0.5, i as f64 * h, 0.5, h));
    }
    Preset { slots }
}

/// Зеркально `stack_right`: равные ряды в левой колонке, главное справа.
///
/// Отдельная функция, а не «перевёрнутый» пресет: порядок слотов — порядок
/// выбора окон, и слот 1 обязан оставаться главным и тут — главное всегда
/// кладётся в `slots[0]`, независимо от того, слева оно или справа.
fn stack_left(n: usize) -> Preset {
    let mut slots = Vec::with_capacity(n);
    slots.push(rect!(0.5, 0.0, 0.5, 1.0));
    let h = 1.0 / (n - 1) as f64;
    for i in 0..n - 1 {
        slots.push(rect!(0.0, i as f64 * h, 0.5, h));
    }
    Preset { slots }
}

/// «Главное + ряд»: главный слот сверху на всю ширину (высоты `main_h`),
/// остальные `n - 1` — равными колонками в ряду снизу.
fn top_row(n: usize, main_h: f64) -> Preset {
    let mut slots = Vec::with_capacity(n);
    slots.push(rect!(0.0, 0.0, 1.0, main_h));
    let w = 1.0 / (n - 1) as f64;
    for i in 0..n - 1 {
        slots.push(rect!(i as f64 * w, main_h, w, 1.0 - main_h));
    }
    Preset { slots }
}

/// Зеркально `top_row`: равные колонки в ряду сверху, главное снизу.
///
/// Как и в `stack_left`, главное кладётся в `slots[0]`, а не в последний
/// слот, — слот 1 получает окно, выбранное первым, и оно должно быть
/// главным. Высота главного — `main_h`, как и в `top_row`: оба пресета
/// одной пары зеркальны по вертикали.
fn bottom_row(n: usize, main_h: f64) -> Preset {
    let mut slots = Vec::with_capacity(n);
    slots.push(rect!(0.0, 1.0 - main_h, 1.0, main_h));
    let w = 1.0 / (n - 1) as f64;
    for i in 0..n - 1 {
        slots.push(rect!(i as f64 * w, 0.0, w, 1.0 - main_h));
    }
    Preset { slots }
}

/// Сетка `rows × cols` одинаковых ячеек.
fn grid(rows: usize, cols: usize) -> Preset {
    let mut slots = Vec::with_capacity(rows * cols);
    let (cw, ch) = (1.0 / cols as f64, 1.0 / rows as f64);
    for r in 0..rows {
        for c in 0..cols {
            slots.push(rect!(c as f64 * cw, r as f64 * ch, cw, ch));
        }
    }
    Preset { slots }
}

/// «Кирпич»: `top` колонок сверху и `bottom` снизу (для семи окон — 3 + 4).
///
/// Размеры ячеек в рядах разные, поэтому глаз читает раскладку как «кирпич»,
/// а не как сетку, — лента получает ещё один осмысленный силуэт.
fn brick(top: usize, bottom: usize) -> Preset {
    let mut slots = Vec::with_capacity(top + bottom);
    for c in 0..top {
        slots.push(rect!(c as f64 / top as f64, 0.0, 1.0 / top as f64, 0.5));
    }
    for c in 0..bottom {
        slots.push(rect!(
            c as f64 / bottom as f64,
            0.5,
            1.0 / bottom as f64,
            0.5
        ));
    }
    Preset { slots }
}

/// «Главное по центру» для восьми окон: главное в центральной колонке,
/// слева и справа — по три окна на всю высоту, снизу — панель.
///
/// Почему не кольцо 3×3: вокруг центрального слот в сетке 3×3 помещается
/// восемь окон — на одно больше, чем в группе из восьми. Свободный угол
/// выглядел бы как поломка, поэтому центр обстраивается колоннами и панелью
/// (1 + 3 + 3 + 1 = 8) — читается как «главное в центре», а не как сетка.
fn main_center_eight() -> Preset {
    let third = 1.0 / 3.0;
    Preset {
        slots: vec![
            rect!(0.25, 0.0, 0.5, 0.875),
            rect!(0.0, 0.0, 0.25, third),
            rect!(0.0, third, 0.25, third),
            rect!(0.0, 2.0 * third, 0.25, third),
            rect!(0.75, 0.0, 0.25, third),
            rect!(0.75, third, 0.25, third),
            rect!(0.75, 2.0 * third, 0.25, third),
            rect!(0.25, 0.875, 0.5, 0.125),
        ],
    }
}

/// Таблица раскладок для 2..8 окон, индекс — `count - 2`.
///
/// Ленивая инициализация ([`OnceLock`]) — не ради экономии, а потому что
/// `Vec` в `Preset::slots` нельзя собрать в `const`-выражении (аллокации в
/// const-контексте запрещены). Первое обращение строит все раскладки, дальше
/// таблица не меняется.
static TABLE: OnceLock<[[Preset; 7]; 7]> = OnceLock::new();

/// Семь вариантов на каждое количество окон — см. `presets_for`.
///
/// Варианты сознательно РАЗНЫЕ, а не семь видов сетки (требование ленты):
/// половины по обеим осям, «главное + стопка» слева и справа, «главное +
/// ряд» сверху и снизу, равные колонки, равные ряды, сетка, главное по
/// центру. У больших количеств часть вариантов естественно вырождается в
/// сетку — это нормально, важно, чтобы лента предлагала выбор.
fn build_table() -> [[Preset; 7]; 7] {
    [
        [
            // 2 окна: половины, «главное + сосед» в обе стороны по обеим осям,
            // главное по центру с панелью снизу.
            Preset {
                slots: vec![rect!(0.0, 0.0, 0.5, 1.0), rect!(0.5, 0.0, 0.5, 1.0)],
            },
            Preset {
                slots: vec![rect!(0.0, 0.0, 1.0, 0.5), rect!(0.0, 0.5, 1.0, 0.5)],
            },
            Preset {
                slots: vec![
                    rect!(0.0, 0.0, 2.0 / 3.0, 1.0),
                    rect!(2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
                ],
            },
            Preset {
                slots: vec![
                    rect!(1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0),
                    rect!(0.0, 0.0, 1.0 / 3.0, 1.0),
                ],
            },
            Preset {
                slots: vec![
                    rect!(0.0, 0.0, 1.0, 2.0 / 3.0),
                    rect!(0.0, 2.0 / 3.0, 1.0, 1.0 / 3.0),
                ],
            },
            Preset {
                slots: vec![
                    rect!(0.0, 1.0 / 3.0, 1.0, 2.0 / 3.0),
                    rect!(0.0, 0.0, 1.0, 1.0 / 3.0),
                ],
            },
            Preset {
                slots: vec![rect!(0.25, 0.0, 0.5, 0.7), rect!(0.0, 0.7, 1.0, 0.3)],
            },
        ],
        [
            // 3 окна: «главное + стопка» и «главное + ряд» в обе стороны,
            // колонки, ряды, главное по центру с боковинами.
            stack_right(3),
            stack_left(3),
            top_row(3, 2.0 / 3.0),
            bottom_row(3, 2.0 / 3.0),
            columns(3),
            rows(3),
            Preset {
                slots: vec![
                    rect!(0.25, 0.125, 0.5, 0.75),
                    rect!(0.0, 0.0, 0.25, 1.0),
                    rect!(0.75, 0.0, 0.25, 1.0),
                ],
            },
        ],
        [
            // 4 окна: те же «главное + …», сетка 2×2, колонки, ряды.
            stack_right(4),
            stack_left(4),
            top_row(4, 2.0 / 3.0),
            bottom_row(4, 2.0 / 3.0),
            columns(4),
            rows(4),
            grid(2, 2),
        ],
        [
            // 5 окон: «главное + …», колонки, ряды, главное по центру
            // с четырьмя углами.
            stack_right(5),
            stack_left(5),
            top_row(5, 0.5),
            bottom_row(5, 0.5),
            columns(5),
            rows(5),
            Preset {
                slots: vec![
                    rect!(0.3, 0.25, 0.4, 0.5),
                    rect!(0.0, 0.0, 0.3, 0.25),
                    rect!(0.7, 0.0, 0.3, 0.25),
                    rect!(0.0, 0.75, 0.3, 0.25),
                    rect!(0.7, 0.75, 0.3, 0.25),
                ],
            },
        ],
        [
            // 6 окон: «главное + …», колонки, ряды, сетка 2×3.
            stack_right(6),
            stack_left(6),
            top_row(6, 0.5),
            bottom_row(6, 0.5),
            columns(6),
            rows(6),
            grid(2, 3),
        ],
        [
            // 7 окон: «главное + …», колонки, ряды, «кирпич» 3+4.
            stack_right(7),
            stack_left(7),
            top_row(7, 0.5),
            bottom_row(7, 0.5),
            columns(7),
            rows(7),
            brick(3, 4),
        ],
        [
            // 8 окон: «главное + …», колонки, ряды, главное по центру.
            stack_right(8),
            stack_left(8),
            top_row(8, 1.0 / 3.0),
            bottom_row(8, 1.0 / 3.0),
            columns(8),
            rows(8),
            main_center_eight(),
        ],
    ]
}

/// Раскладки для группы из `window_count` окон.
///
/// Для 2..=8 — по семь разных вариантов (см. `build_table`), вне диапазона —
/// пустой срез: группы меньше двух окон не бывает, а больше восьми лента не
/// рисует. Вызывающий слой может не проверять диапазон заранее.
pub fn presets_for(window_count: usize) -> &'static [Preset] {
    if !(2..=8).contains(&window_count) {
        return &[];
    }
    &TABLE.get_or_init(build_table)[window_count - 2]
}

/// Перевести раскладку в пиксели рабочей области `work_area`.
///
/// `gap_px` — зазор между окнами и от краёв рабочей области, вычитается
/// симметрично: каждый зазор делится пополам между своими сторонами, поэтому
/// и от края, и между соседями получается ровно `gap_px` (тест
/// `gap_is_split_exactly_between_neighbours_and_edges`).
///
/// Почему границы считаются в долях и округляются ОДИН раз в конце, а не
/// размеры по отдельности: у соседних слотов общая граница. Округли каждый
/// размер отдельно (`round(w_frac * W)`), и общая граница разъехалась бы на
/// пиксель — между окнами появилась бы щель или они налезли бы друг на друга.
/// Здесь же общая граница — одно и то же `f64`, и округляется одинаково для
/// обоих соседей: при нулевом зазоре слоты стыкуются точно.
///
/// Вырожденный вход не паникует и не даёт отрицательных размеров: нулевая
/// рабочая область (монитор-полоска, гонка переподключения) даёт нулевые
/// слоты, зазор больше половины слота схлопывает слот в нулевой размер.
pub fn apply(preset: &Preset, work_area: Rect, gap_px: i32) -> Vec<Rect> {
    let gap = gap_px.max(0) as f64;
    let half = gap / 2.0;
    let (min_x, max_x) = (
        work_area.x as f64,
        work_area.x as f64 + f64::from(work_area.w),
    );
    let (min_y, max_y) = (
        work_area.y as f64,
        work_area.y as f64 + f64::from(work_area.h),
    );
    if work_area.w == 0 || work_area.h == 0 {
        return preset
            .slots
            .iter()
            .map(|_| Rect {
                x: work_area.x,
                y: work_area.y,
                w: 0,
                h: 0,
            })
            .collect();
    }
    // Внутренняя рамка: рабочая область, ужатая на ползазора с каждой стороны
    // (полный зазор по каждой оси). Второй ползазора каждый слот отнимает от
    // своих кромок — в сумме и от края, и между соседями выходит ровно
    // `gap_px`, и все кромки считаются от одной и той же рамки.
    let inner_w = (max_x - min_x - gap).max(0.0);
    let inner_h = (max_y - min_y - gap).max(0.0);
    let origin_x = min_x + half;
    let origin_y = min_y + half;
    preset
        .slots
        .iter()
        .map(|s| {
            // Кромки в пикселях: границы слота внутри рамки, отодвинутые на
            // ползазора к соседям. Общие границы соседей — одно и то же
            // значение, округляется один раз и для левого, и для правого.
            let left = (origin_x + s.x * inner_w + half).round();
            let right = (origin_x + (s.x + s.w) * inner_w - half).round();
            let top = (origin_y + s.y * inner_h + half).round();
            let bottom = (origin_y + (s.y + s.h) * inner_h - half).round();
            // Кламп в рабочую область и схлопывание вырожденных кромок в ноль:
            // `right` поджимается к `left`, размеры не становятся
            // отрицательными.
            let x = left.clamp(min_x, max_x);
            let right = right.clamp(x, max_x);
            let y = top.clamp(min_y, max_y);
            let bottom = bottom.clamp(y, max_y);
            Rect {
                x: x as i32,
                y: y as i32,
                w: (right - x) as u32,
                h: (bottom - y) as u32,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Условный FullHD с панелью задач снизу (как в `pinned_window`).
    fn work_area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1032,
        }
    }

    fn area(r: Rect) -> i64 {
        i64::from(r.w) * i64::from(r.h)
    }

    /// Площадь пересечения двух слотов: 0 — слоты не налезают друг на друга.
    fn overlap(a: Rect, b: Rect) -> i64 {
        let ix = (i64::from(a.x) + i64::from(a.w)).min(i64::from(b.x) + i64::from(b.w))
            - i64::from(a.x).max(i64::from(b.x));
        let iy = (i64::from(a.y) + i64::from(a.h)).min(i64::from(b.y) + i64::from(b.h))
            - i64::from(a.y).max(i64::from(b.y));
        ix.max(0) * iy.max(0)
    }

    /// Доля рабочей области, покрытая слотами: 1.0 — раскладка без свободного
    /// места (половины, сетки, «главное + стопка»), меньше — свободное место
    /// вокруг главного по центру.
    fn covered_fraction(preset: &Preset) -> f64 {
        preset.slots.iter().map(|s| s.w * s.h).sum()
    }

    /// При нулевом зазоре слоты стыкуются без швов и нахлёстов: общая граница
    /// соседей — одно и то же значение и округляется одинаково для обоих.
    /// Полные раскладки покрывают рабочую область точно; раскладки со
    /// свободным местом вокруг главного — покрывают меньше и не выходят за
    /// область.
    #[test]
    fn zero_gap_tiles_work_area_without_seams_or_overlaps() {
        let full = i64::from(work_area().w) * i64::from(work_area().h);
        for count in 2..=8 {
            for preset in presets_for(count) {
                let slots = apply(preset, work_area(), 0);
                assert_eq!(slots.len(), count);
                let total: i64 = slots.iter().map(|s| area(*s)).sum();
                if (covered_fraction(preset) - 1.0).abs() < 1e-9 {
                    assert_eq!(
                        total, full,
                        "count={count}: полная раскладка обязана покрыть область точно"
                    );
                } else {
                    assert!(
                        total < full,
                        "count={count}: свободное место вокруг главного не должно быть занято"
                    );
                }
                for (i, a) in slots.iter().enumerate() {
                    for b in slots.iter().skip(i + 1) {
                        assert_eq!(overlap(*a, *b), 0, "count={count}: нахлёст слотов {i}");
                    }
                }
            }
        }
    }

    /// Первое выбранное окно (слот 1) — главное: в раскладках «главное +
    /// стопка» и «главное + ряд» оно крупнее всех остальных, и ни в одной
    /// раскладке таблицы слот 1 не меньше любого другого. Это смысловое
    /// требование ленты: слот 1 получает окно, выбранное первым.
    #[test]
    fn slot_one_is_never_smaller_than_its_neighbours() {
        for count in 2..=8 {
            for preset in presets_for(count) {
                let first = preset.slots[0].w * preset.slots[0].h;
                for (i, slot) in preset.slots.iter().enumerate().skip(1) {
                    assert!(
                        first >= slot.w * slot.h,
                        "count={count}: слот 1 меньше слота {}",
                        i + 1
                    );
                }
            }
        }
    }

    /// Ни один слот не выходит за рабочую область — ни при нулевом зазоре,
    /// ни при зазоре: кромки считаются от краёв области, и округление не
    /// может вытолкнуть окно наружу.
    #[test]
    fn no_slot_leaves_the_work_area() {
        for gap in [0, 1, 7, 64] {
            for count in 2..=8 {
                for preset in presets_for(count) {
                    for slot in apply(preset, work_area(), gap) {
                        assert!(slot.x >= 0 && slot.y >= 0, "gap={gap}");
                        assert!(i64::from(slot.x) + i64::from(slot.w) <= 1920, "gap={gap}");
                        assert!(i64::from(slot.y) + i64::from(slot.h) <= 1032, "gap={gap}");
                    }
                }
            }
        }
    }

    /// Нулевая рабочая область (монитор-полоска, гонка переподключения) не
    /// паникует: слоты вырождаются в нулевой размер, а не в отрицательный.
    #[test]
    fn degenerate_work_area_yields_zero_sized_slots_without_panicking() {
        for preset in presets_for(2).iter().chain(presets_for(8)) {
            let slots = apply(
                preset,
                Rect {
                    x: 3,
                    y: -5,
                    w: 0,
                    h: 0,
                },
                8,
            );
            assert!(slots.iter().all(|s| s.w == 0 && s.h == 0));
            let slots = apply(
                preset,
                Rect {
                    x: 3,
                    y: -5,
                    w: 0,
                    h: 100,
                },
                8,
            );
            assert!(slots.iter().all(|s| s.w == 0));
            let slots = apply(
                preset,
                Rect {
                    x: 3,
                    y: -5,
                    w: 100,
                    h: 0,
                },
                8,
            );
            assert!(slots.iter().all(|s| s.h == 0));
        }
    }

    /// Группы меньше двух окон не бывает, больше восьми лента не рисует —
    /// вне диапазона 2..=8 срез пустой.
    #[test]
    fn presets_for_outside_two_to_eight_is_empty() {
        for count in [0, 1, 9, 16, 100] {
            assert!(presets_for(count).is_empty(), "count={count}");
        }
    }

    /// На каждое количество окон — ровно семь РАЗНЫХ раскладок (семь
    /// одинаковых сеток не годятся: лента должна предлагать выбор), каждая —
    /// ровно с `count` слотами.
    #[test]
    fn each_count_offers_seven_distinct_layouts_of_the_right_size() {
        for count in 2..=8 {
            let presets = presets_for(count);
            assert_eq!(presets.len(), 7, "count={count}");
            for (i, preset) in presets.iter().enumerate() {
                assert_eq!(preset.slots.len(), count, "count={count}, пресет {i}");
                for other in presets.iter().skip(i + 1) {
                    assert_ne!(preset, other, "count={count}: дубль раскладки {i}");
                }
            }
        }
    }

    /// Все слоты таблицы лежат внутри единичного квадрата — таблицу правят
    /// руками, и такая проверка ловит опечатку в долях на этапе теста.
    #[test]
    fn every_slot_stays_inside_the_unit_square() {
        for count in 2..=8 {
            for (i, preset) in presets_for(count).iter().enumerate() {
                for slot in &preset.slots {
                    assert!(slot.w > 0.0 && slot.h > 0.0, "count={count}, пресет {i}");
                    assert!(slot.x >= -1e-9 && slot.y >= -1e-9, "count={count}");
                    assert!(slot.x + slot.w <= 1.0 + 1e-9, "count={count}");
                    assert!(slot.y + slot.h <= 1.0 + 1e-9, "count={count}");
                }
            }
        }
    }

    /// Зазор делится пополам между сторонами: от края рабочей области и между
    /// соседними окнами получается ровно `gap_px` — в том числе при нечётном
    /// зазоре, где половинки не целые.
    #[test]
    fn gap_is_split_exactly_between_neighbours_and_edges() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 800,
        };
        for gap in [1, 7, 8, 50] {
            let slots = apply(&presets_for(2)[0], work, gap);
            assert_eq!(slots[0].x, gap, "левый край");
            assert_eq!(slots[0].y, gap, "верхний край");
            assert_eq!(
                i64::from(slots[1].x) + i64::from(slots[1].w),
                i64::from(1000 - gap),
                "правый край"
            );
            assert_eq!(
                i64::from(slots[0].h),
                i64::from(800 - 2 * gap),
                "вертикальный размер без внутреннего зазора"
            );
            let between = i64::from(slots[1].x) - (i64::from(slots[0].x) + i64::from(slots[0].w));
            assert_eq!(between, i64::from(gap), "зазор между окнами");
        }
    }

    /// Отрицательный зазор — как нулевой (кламп на входе), а зазор больше
    /// половины слота схлопывает слот в нулевой размер, а не в отрицательный.
    #[test]
    fn negative_or_oversized_gap_never_yields_negative_sizes() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let columns = &presets_for(4)[4];
        assert_eq!(
            apply(columns, work, -8),
            apply(columns, work, 0),
            "отрицательный зазор не должен ничего двигать"
        );
        // 25px-колонки: зазор 50 уже больше половины слота.
        let slots = apply(columns, work, 50);
        for slot in slots {
            assert!(
                slot.w <= 25,
                "слот обязан схлопнуться, а не остаться или стать отрицательным"
            );
        }
        for preset in presets_for(2) {
            let slots = apply(preset, work, 200);
            for slot in slots {
                assert_eq!((slot.w, slot.h), (0, 0), "зазор больше области: всё в ноль");
            }
        }
    }

    /// Пустой пресет (сломанная таблица, ручная правка) применяется в пустой
    /// список без паники.
    #[test]
    fn empty_preset_applies_to_empty_list() {
        let empty = Preset { slots: Vec::new() };
        assert!(apply(&empty, work_area(), 8).is_empty());
    }

    /// Реальные разрешения для исчерпывающей проверки: монитор пользователя
    /// минус панель задач (2560x1400, из жалобы 2026-08-26), FullHD,
    /// маленький ноутбук, ультраширокий, плюс второй монитор слева с
    /// отрицательным origin (ADR-010) — кромки считаются от origin области,
    /// и проверка обязана покрыть отрицательные координаты.
    const REAL_AREAS: [Rect; 5] = [
        Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        },
        Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        },
        Rect {
            x: 0,
            y: 0,
            w: 1366,
            h: 768,
        },
        Rect {
            x: 0,
            y: 0,
            w: 3440,
            h: 1440,
        },
        Rect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080,
        },
    ];

    /// Зазор в пикселях ровно как это делает вызывающий код
    /// (`resticker/src/groups.rs`, `layout_targets` — читать можно, править
    /// нельзя): процент от наименьшей стороны слота при нулевом зазоре,
    /// целочисленное умножение в `u32`. Только такая копия формулы честно
    /// проверяет то, что реально попадёт на экран пользователя.
    fn realistic_gap(preset: &Preset, area: Rect, pct: u8) -> i32 {
        let bare = apply(preset, area, 0);
        let smallest = bare.iter().map(|r| r.w.min(r.h)).min().unwrap_or(0);
        (u32::from(pct) * smallest / 100) as i32
    }

    /// Исчерпывающая проверка нахлёста: КАЖДАЯ раскладка каждого размера на
    /// КАЖДОЙ рабочей области при КАЖДОМ реальном зазоре (0, 1, 4 — реальный
    /// зазор пользователя, 10, 35 процентов). Два окна на одном слоте —
    /// жалоба пользователя 2026-08-26 («2 окна полетели на один слот»);
    /// этот тест доказывает, что таблица и `apply` не дают нахлёста ни в
    /// одном сочетании. Счётчик проверок фиксируется числом: если циклы
    /// молча перестанут выполняться, тест не «позеленеет» впустую.
    #[test]
    fn slots_never_overlap_for_any_work_area_and_realistic_gap() {
        let mut pair_checks = 0usize;
        let mut apply_calls = 0usize;
        for area in REAL_AREAS {
            for count in 2..=8 {
                for preset in presets_for(count) {
                    for pct in [0u8, 1, 4, 10, 35] {
                        let gap = realistic_gap(preset, area, pct);
                        let slots = apply(preset, area, gap);
                        apply_calls += 1;
                        assert_eq!(
                            slots.len(),
                            count,
                            "area={area:?}, count={count}, pct={pct}: слотов не {count}"
                        );
                        for (i, a) in slots.iter().enumerate() {
                            for (j, b) in slots.iter().enumerate().skip(i + 1) {
                                pair_checks += 1;
                                assert_eq!(
                                    overlap(*a, *b),
                                    0,
                                    "area={area:?}, count={count}, pct={pct}%: слоты {i} и {j} налезают друг на друга: {a:?} vs {b:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(apply_calls, 5 * 7 * 7 * 5, "полнота перебора");
        // Пар на пресет: count*(count-1)/2 для 2..=8, сумма по пресетам —
        // 588 пар на каждую область×зазор, всего 5×5×588.
        assert_eq!(pair_checks, 5 * 5 * 588, "полнота пар");
    }

    /// Вырожденный случай: очень маленькая рабочая область и зазор от
    /// реальных 35% до заведомо нереальных (больше самой области). Слот
    /// обязан схлопнуться в нулевой размер, а не вывернуться наизнанку, и —
    /// главное — два разных слота не могут получить ОДИНАКОВЫЙ НЕНУЛЕВОЙ
    /// прямоугольник: это и есть «окно на окне» из жалобы. Совпадение
    /// нулевых слотов в одной точке допускается сознательно: окно нулевого
    /// размера невидимо и ничего не заслоняет, а при зазоре, большем
    /// размера области, развести два ненулевых окна с зазором между ними
    /// физически невозможно.
    #[test]
    fn degenerate_area_with_big_gap_never_duplicates_nonzero_slots() {
        let tiny_areas = [
            Rect {
                x: 0,
                y: 0,
                w: 300,
                h: 200,
            },
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 50,
            },
            Rect {
                x: 0,
                y: 0,
                w: 40,
                h: 30,
            },
        ];
        for area in tiny_areas {
            for count in 2..=8 {
                for preset in presets_for(count) {
                    // 0 и 1 — реальные; 20 и больше — уже схлопывают слоты
                    // на этих областях, вплоть до «зазор больше области».
                    for gap in [0, 1, 20, 60, 150, 9999] {
                        let slots = apply(preset, area, gap);
                        for (i, a) in slots.iter().enumerate() {
                            for (j, b) in slots.iter().enumerate().skip(i + 1) {
                                let both_zero = a.w == 0 && a.h == 0 && b.w == 0 && b.h == 0;
                                assert!(
                                    both_zero || a != b,
                                    "area={area:?}, count={count}, gap={gap}: слоты {i} и {j} совпали по позиции и размеру: {a:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Реальный зазор (0..35% от наименьшей стороны слота) не может
    /// схлопнуть слот в ноль даже на самой маленькой рабочей области:
    /// зазор меньше самого маленького слота, поэтому каждый слот обязан
    /// остаться строго положительным. Это гарантирует, что «окно на окне»
    /// из жалобы пользователя невозможно и в вырожденном по размерам
    /// сценарии — схлопывание случается только при заведомо нереальных
    /// зазорах (тест `degenerate_area_with_big_gap_never_duplicates_nonzero_slots`).
    #[test]
    fn realistic_gap_never_collapses_a_slot_on_a_tiny_area() {
        let tiny_areas = [
            Rect {
                x: 0,
                y: 0,
                w: 300,
                h: 200,
            },
            Rect {
                x: 0,
                y: 0,
                w: 640,
                h: 360,
            },
            Rect {
                x: 0,
                y: 0,
                w: 1366,
                h: 200,
            },
        ];
        for area in tiny_areas {
            for count in 2..=8 {
                for preset in presets_for(count) {
                    for pct in [0u8, 1, 4, 10, 35] {
                        let gap = realistic_gap(preset, area, pct);
                        let slots = apply(preset, area, gap);
                        assert!(
                            slots.iter().all(|s| s.w > 0 && s.h > 0),
                            "area={area:?}, count={count}, pct={pct}%: слот схлопнулся в ноль"
                        );
                    }
                }
            }
        }
    }

    /// Ни один слот не выходит за рабочую область ни на одном реальном
    /// разрешении, ни при каком зазоре — в том числе экстремальном, где
    /// слоты схлопываются: схлопнутый слот обязан остаться на краю области,
    /// а не «уехать» за неё. Дополняет `no_slot_leaves_the_work_area`
    /// (та проверяет одну область) несколькими областями, включая монитор
    /// слева с отрицательным origin.
    #[test]
    fn no_slot_leaves_any_work_area_at_any_gap() {
        let mut areas = REAL_AREAS.to_vec();
        areas.push(Rect {
            x: 0,
            y: 0,
            w: 40,
            h: 30,
        });
        for area in areas {
            let (max_x, max_y) = (
                i64::from(area.x) + i64::from(area.w),
                i64::from(area.y) + i64::from(area.h),
            );
            for count in 2..=8 {
                for preset in presets_for(count) {
                    for gap in [0, 1, 64, 4096] {
                        for slot in apply(preset, area, gap) {
                            assert!(
                                i64::from(slot.x) >= i64::from(area.x)
                                    && i64::from(slot.y) >= i64::from(area.y)
                                    && i64::from(slot.x) + i64::from(slot.w) <= max_x
                                    && i64::from(slot.y) + i64::from(slot.h) <= max_y,
                                "area={area:?}, count={count}, gap={gap}: слот {slot:?} вышел за область"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Пресеты всегда ровно на то число окон, для которого запрошены:
    /// лишний слот оставил бы окно без места (жалоба «вылетают не все
    /// окна»), недостающий — два окна на одном слоте. Проверка намеренно
    /// дублирует `each_count_offers_seven_distinct_layouts_of_the_right_size`,
    /// чтобы держать размерность слотов отдельным явным утверждением.
    #[test]
    fn presets_for_every_count_have_exactly_that_many_slots() {
        for count in 2..=8 {
            for (i, preset) in presets_for(count).iter().enumerate() {
                assert_eq!(preset.slots.len(), count, "count={count}, пресет {i}");
            }
        }
    }
}
