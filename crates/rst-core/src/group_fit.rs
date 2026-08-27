//! Подгонка слотов раскладки под минимальные размеры окон (живой репорт
//! 2026-08-26: «маленькие окна все время налазят друг на друга»).
//!
//! Приложения не становятся меньше собственного минимума (`WM_GETMINMAXINFO`,
//! `ptMinTrackSize`): если слот раскладки уже этого минимума, окно молча
//! остаётся крупнее слота и НАЛЕЗАЕТ НА СОСЕДА — на схеме слоты соприкасаются,
//! а на экране окна перекрываются. Здесь чистая функция: на вход
//! прямоугольники слотов, как их посчитал [`crate::group_layout::apply`],
//! рабочая область и минимум каждого окна; на выход — те же слоты,
//! растянутые и сдвинутые так, что каждый не меньше своего минимума и
//! никакие два не пересекаются. Если минимумы физически не влезают — честный
//! отказ с дефицитом, а не «решение» с перекрытиями.
//!
//! Ключевое наблюдение: все раскладки таблицы — разбиения прямоугольника
//! прямыми линиями, у соседних слотов ОБЩИЕ границы. Значит задача по каждой
//! оси независима: подвинуть вертикальные границы так, чтобы каждый слот был
//! достаточно широк, и горизонтальные — чтобы достаточно высок. Двигая общую
//! границу, мы меняем сразу все слоты по обе стороны от неё — и перекрытия
//! не возникают по построению (тесты всё равно проверяют каждую пару).
//!
//! Крейт платформенно-чистый (CONTRIBUTING.md, «Правило зависимостей»):
//! только геометрия и юнит-тесты, никакого Win32.

use crate::model::Rect;

/// Минимальный размер окна, которое поедет в слот (поле `ptMinTrackSize`
/// ответа на `WM_GETMINMAXINFO`).
///
/// `None` в списке минимумов — приложение не ответило, требований у слота
/// нет: окно ужмётся в любой слот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinSize {
    pub width: u32,
    pub height: u32,
}

/// Итог подгонки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitOutcome {
    /// Всё уложилось: те же по счёту слоты, каждый не меньше своего
    /// минимума, попарно не пересекаются, лежат внутри рабочей области.
    Placed(Vec<Rect>),
    /// Минимумы не влезают в рабочую область по сумме.
    ///
    /// `deficit_x`/`deficit_y` — сколько пикселей не хватает по каждой оси,
    /// если раздать минимумам всё место (включая зазоры, сжатые до одного
    /// пикселя на полосу); ноль — по этой оси минимумы влезают.
    Impossible { deficit_x: u32, deficit_y: u32 },
}

/// Подогнать слоты под минимальные размеры окон.
///
/// `slots` — прямоугольники из [`crate::group_layout::apply`] (уже с зазором,
/// внутри `work_area`, попарно без пересечений); `minimums` — минимум окна
/// для каждого слота, допускается короче списка слотов (недостающие считаются
/// без требований) и длиннее (лишние игнорируются).
///
/// Результат — [`FitOutcome::Placed`] с теми же по счёту слотами, или
/// [`FitOutcome::Impossible`], если сумма минимумов по какой-то оси
/// превышает размер области: там раскладка без пересечений невозможна, и
/// притворяться, что она есть, хуже, чем честно сказать пользователю,
/// сколько места не хватает.
///
/// Вырожденные входы не паникуют: пустой список укладывается в пустой
/// список, нулевая рабочая область и слоты за её пределами (нарушение
/// контракта) возвращаются как есть — молча «исправлять» чужой прямоугольник
/// хуже, чем не трогать.
pub fn fit_slots(slots: &[Rect], work_area: Rect, minimums: &[Option<MinSize>]) -> FitOutcome {
    if slots.is_empty() || work_area.w == 0 || work_area.h == 0 {
        return FitOutcome::Placed(slots.to_vec());
    }
    let min_of = |i: usize| minimums.get(i).copied().flatten();
    let inside = |s: &Rect| {
        s.x >= work_area.x
            && s.y >= work_area.y
            && i64::from(s.x) + i64::from(s.w) <= i64::from(work_area.x) + i64::from(work_area.w)
            && i64::from(s.y) + i64::from(s.h) <= i64::from(work_area.y) + i64::from(work_area.h)
    };
    if !slots.iter().all(inside) {
        return FitOutcome::Placed(slots.to_vec());
    }
    // Минимумы уже выполнены — ничего не двигаем. Лишнее движение чужих
    // окон заметно, а пользы нет (тот же принцип «не дёргать зря», что в
    // `hotkey_decisions`); зазор, вычтенный в `apply`, при этом не тронут.
    if slots
        .iter()
        .enumerate()
        .all(|(i, s)| min_of(i).is_none_or(|m| s.w >= m.width && s.h >= m.height))
    {
        return FitOutcome::Placed(slots.to_vec());
    }
    // Слот нулевого размера с положительным минимумом некуда ужать: он не
    // занимает ни одной полосы, и требование к нему невыполнимо в принципе.
    let mut deficit_x = 0u32;
    let mut deficit_y = 0u32;
    for (i, s) in slots.iter().enumerate() {
        let Some(m) = min_of(i) else { continue };
        if s.w == 0 {
            deficit_x = deficit_x.saturating_add(m.width);
        }
        if s.h == 0 {
            deficit_y = deficit_y.saturating_add(m.height);
        }
    }
    if deficit_x > 0 || deficit_y > 0 {
        return FitOutcome::Impossible {
            deficit_x,
            deficit_y,
        };
    }
    let x_edges: Vec<(i32, i32)> = slots.iter().map(|s| (s.x, s.x + s.w as i32)).collect();
    let y_edges: Vec<(i32, i32)> = slots.iter().map(|s| (s.y, s.y + s.h as i32)).collect();
    let x_mins: Vec<Option<u32>> = (0..slots.len())
        .map(|i| min_of(i).map(|m| m.width))
        .collect();
    let y_mins: Vec<Option<u32>> = (0..slots.len())
        .map(|i| min_of(i).map(|m| m.height))
        .collect();
    let x_problem = axis_problem(
        &x_edges,
        &x_mins,
        work_area.x,
        work_area.x + work_area.w as i32,
    );
    let y_problem = axis_problem(
        &y_edges,
        &y_mins,
        work_area.y,
        work_area.y + work_area.h as i32,
    );
    // Обе оси считаются ДО возврата: дефицит по одной оси не должен скрывать
    // дефицит по другой — пользователю важно знать обе цифры сразу.
    let x_outcome = fit_axis(&x_problem.orig, &x_problem.spans, work_area.w);
    let y_outcome = fit_axis(&y_problem.orig, &y_problem.spans, work_area.h);
    let (x_widths, y_widths) = match (x_outcome, y_outcome) {
        (AxisOutcome::Fits(xw), AxisOutcome::Fits(yw)) => (xw, yw),
        (AxisOutcome::Overflow { deficit: dx }, AxisOutcome::Overflow { deficit: dy }) => {
            return FitOutcome::Impossible {
                deficit_x: dx,
                deficit_y: dy,
            };
        }
        (AxisOutcome::Overflow { deficit: dx }, _) => {
            return FitOutcome::Impossible {
                deficit_x: dx,
                deficit_y: 0,
            };
        }
        (_, AxisOutcome::Overflow { deficit: dy }) => {
            return FitOutcome::Impossible {
                deficit_x: 0,
                deficit_y: dy,
            };
        }
    };
    // Границы собираются из ширин полос слева направо, поэтому соседние
    // полосы стыкуются точно, и слоты не пересекаются по построению.
    let positions = |widths: &[u32], start: i32| -> Vec<i32> {
        let mut pos = Vec::with_capacity(widths.len() + 1);
        pos.push(start);
        for &w in widths {
            pos.push(*pos.last().expect("позиции всегда непусты") + w as i32);
        }
        pos
    };
    let x_pos = positions(&x_widths, work_area.x);
    let y_pos = positions(&y_widths, work_area.y);
    let fitted: Vec<Rect> = slots
        .iter()
        .enumerate()
        .map(
            |(i, _)| match (x_problem.intervals[i], y_problem.intervals[i]) {
                (Some((fx, lx)), Some((fy, ly))) => Rect {
                    x: x_pos[fx],
                    y: y_pos[fy],
                    w: (x_pos[lx + 1] - x_pos[fx]) as u32,
                    h: (y_pos[ly + 1] - y_pos[fy]) as u32,
                },
                // Слот, вырожденный в ноль по одной из осей (и без минимума по
                // ней — иначе был бы `Impossible` выше), остаётся как был:
                // интервала полос у него нет, и двигать его некуда.
                _ => slots[i],
            },
        )
        .collect();
    FitOutcome::Placed(fitted)
}

/// Вердикт ленты раскладок: подходит ли пресет под минимумы окон.
///
/// Простое значение без чисел намеренно: лента хранит его рядом с
/// миниатюрой и рисует пометку «не влезает», а дефицит ей показывать негде.
/// Подробности остаются в [`FitOutcome::Impossible`] для того, кто применяет
/// раскладку на экране.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutFits {
    /// Минимумы влезают: `apply` + `fit_slots` дадут раскладку, в которой
    /// каждое окно не меньше своего минимума и не налезает на соседа.
    Fits,
    /// Сумма минимумов больше рабочей области: как ни двигай общие границы,
    /// окна будут налезать друг на друга — эту раскладку лента должна
    /// показать помеченной, а не дать пользователю узнать о проблеме
    /// по результату.
    DoesNotFit,
}

/// Влезает ли раскладка `preset` под минимумы окон.
///
/// Высокоуровневая обёртка для ленты миниатюр: на вход — раскладка из
/// [`crate::group_layout::presets_for`] (не готовые слоты), рабочая область,
/// зазор в пикселях и минимумы по слотам. Слоты считаются внутри
/// [`crate::group_layout::apply`] — тем же кодом, что и при применении:
/// считать их здесь заново значило бы повторить в ленте кусок математики
/// применения и рано или поздно разойтись с ней.
///
/// Зазор принимается уже в пикселях, а не процентом. Правило «процент от
/// наименьшего слота при нулевом зазоре» живёт в одном месте
/// (`GroupsState::layout_targets`), и вердикт обязан проверять РОВНО то,
/// что попадёт на экран. Продублируй перевод процента здесь — лента и
/// применение возьмут зазор из разных мест, и при первом же изменении
/// правила вердикт разойдётся с реальностью.
///
/// Когда минимумы неизвестны все (ни одно приложение не ответило) —
/// [`LayoutFits::Fits`]: обвинять раскладку без доказательств нельзя,
/// окно без известного минимума ужмётся в любой слот по факту.
/// Несовпадение числа слотов и числа минимумов не паникует: минимумы
/// читаются по индексу, лишние игнорируются, недостающие считаются
/// неизвестными (тот же контракт, что у [`fit_slots`]).
pub fn layout_fits(
    preset: &crate::group_layout::Preset,
    work_area: Rect,
    gap_px: i32,
    minimums: &[Option<MinSize>],
) -> LayoutFits {
    let slots = crate::group_layout::apply(preset, work_area, gap_px);
    match fit_slots(&slots, work_area, minimums) {
        FitOutcome::Placed(_) => LayoutFits::Fits,
        FitOutcome::Impossible { .. } => LayoutFits::DoesNotFit,
    }
}

/// Одномерный срез задачи: полосы между границами слотов и требования.
struct AxisProblem {
    /// Исходные ширины полос — эталон пропорций при распределении остатка.
    orig: Vec<u32>,
    /// Для каждого слота его интервал полос `[first, last]`; `None` — слот
    /// нулевого размера по этой оси (полос не занимает).
    intervals: Vec<Option<(usize, usize)>>,
    /// Требования слотов: интервал полос и минимум по оси.
    spans: Vec<(usize, usize, u32)>,
}

/// По одной оси: кромки слотов и их минимумы → полосы и требования.
///
/// Полоса — интервал между соседними различными границами; любой слот — это
/// подряд идущие полосы (его левая и правая кромки — две из границ), поэтому
/// его минимум — требование к СУММЕ ширин его полос. Края рабочей области
/// добавляются в границы принудительно: при зазоре слоты не достают до краёв,
/// и полосы-щели должны быть частью задачи, а не пропасть.
fn axis_problem(
    edges: &[(i32, i32)],
    mins: &[Option<u32>],
    area_start: i32,
    area_end: i32,
) -> AxisProblem {
    let mut bounds = Vec::with_capacity(edges.len() * 2 + 2);
    bounds.push(area_start);
    for &(a, b) in edges {
        bounds.push(a);
        bounds.push(b);
    }
    bounds.push(area_end);
    bounds.sort_unstable();
    bounds.dedup();
    let index_of = |v: i32| {
        bounds
            .binary_search(&v)
            .expect("кромка слота — одна из границ")
    };
    let orig: Vec<u32> = bounds.windows(2).map(|w| (w[1] - w[0]) as u32).collect();
    let intervals: Vec<Option<(usize, usize)>> = edges
        .iter()
        .map(|&(a, b)| (a < b).then(|| (index_of(a), index_of(b) - 1)))
        .collect();
    let spans: Vec<(usize, usize, u32)> = edges
        .iter()
        .zip(mins)
        .filter_map(|(&(a, b), min)| {
            if a == b {
                return None;
            }
            Some((index_of(a), index_of(b) - 1, (*min)?))
        })
        .collect();
    AxisProblem {
        orig,
        intervals,
        spans,
    }
}

/// Результат одномерной задачи.
enum AxisOutcome {
    /// Ширины полос, удовлетворяющие всем требованиям и дающие в сумме
    /// размер области.
    Fits(Vec<u32>),
    /// Минимумы не влезают: не хватает `deficit` пикселей.
    Overflow { deficit: u32 },
}

/// Решить одномерную задачу: ширины полос, при которых каждый интервал
/// достигает своего минимума, а сумма равна размеру области.
///
/// Минимальное выполнимое решение считается как длиннейший путь. Каждое
/// требование «сумма полос интервала ≥ m» — это ограничение `x[b+1] ≥ x[a] + m`
/// на позиции границ; все рёбра идут от меньшего индекса к большему, и обход
/// слева направо даёт наименьшие позиции, при которых всё выполнимо (любое
/// решение обязано быть не меньше этих). Это и есть «нижняя граница» каждой
/// полосы: раздать её — первый шаг алгоритма из постановки. Если даже
/// нижние границы не влезают — честный дефицит.
///
/// Остаток до размера области распределяется по полосам пропорционально
/// ИСХОДНЫМ ширинам: так пропорции раскладки искажаются меньше всего при
/// разумной простоте. Целочисленный остаток от деления раздаётся по пикселю
/// полосам с самой большой дробной долей, иначе сумма полос не сойдётся
/// с областью.
///
/// Каждая полоса получает минимум 1 px: соседние границы не слипаются, и
/// окно без известного минимума не схлопывается в ноль, когда соседи
/// требуют всё место. Цена — полосы-щели зазора занимают хотя бы по
/// пикселю; на реальных размерах это ничтожно.
fn fit_axis(orig: &[u32], spans: &[(usize, usize, u32)], extent: u32) -> AxisOutcome {
    if spans
        .iter()
        .all(|&(a, b, m)| orig[a..=b].iter().map(|&w| u64::from(w)).sum::<u64>() >= u64::from(m))
    {
        // По этой оси всё уже выполнено — не дёргаем: оси с выполненными
        // минимумами не должны сдвинуться из-за пересчёта другой оси.
        return AxisOutcome::Fits(orig.to_vec());
    }
    let n = orig.len();
    // x[k] — минимальная позиция границы после k полос, от начала оси.
    let mut x = vec![0i64; n + 1];
    for k in 0..n {
        let mut need = x[k] + 1;
        for &(a, b, m) in spans {
            if b == k {
                need = need.max(x[a] + i64::from(m));
            }
        }
        x[k + 1] = need;
    }
    let total = x[n];
    if total > i64::from(extent) {
        return AxisOutcome::Overflow {
            deficit: (total - i64::from(extent)).min(i64::from(u32::MAX)) as u32,
        };
    }
    let rest = i64::from(extent) - total;
    let total_orig: i64 = orig.iter().map(|&w| i64::from(w)).sum();
    let mut widths: Vec<u32> = Vec::with_capacity(n);
    let mut fractions: Vec<(i64, usize)> = Vec::with_capacity(n);
    let mut handed = 0i64;
    for k in 0..n {
        let base = (x[k + 1] - x[k]) as u32;
        let (extra, fraction) = if total_orig == 0 {
            (0, 0)
        } else {
            let num = rest * i64::from(orig[k]);
            (num / total_orig, num % total_orig)
        };
        widths.push(base + extra as u32);
        handed += extra;
        fractions.push((fraction, k));
    }
    let mut leftover = rest - handed;
    fractions.sort_by_key(|&(fraction, _)| std::cmp::Reverse(fraction));
    for (_, k) in fractions {
        if leftover == 0 {
            break;
        }
        widths[k] += 1;
        leftover -= 1;
    }
    debug_assert_eq!(leftover, 0);
    debug_assert_eq!(
        widths.iter().map(|&w| u64::from(w)).sum::<u64>(),
        u64::from(extent)
    );
    AxisOutcome::Fits(widths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group_layout::{apply, presets_for};

    /// Условный монитор пользователя: 2560×1440 минус панель задач.
    fn work_area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        }
    }

    /// Площадь пересечения двух слотов: 0 — слоты не налезают друг на друга.
    fn overlap(a: Rect, b: Rect) -> i64 {
        let ix = (i64::from(a.x) + i64::from(a.w)).min(i64::from(b.x) + i64::from(b.w))
            - i64::from(a.x).max(i64::from(b.x));
        let iy = (i64::from(a.y) + i64::from(a.h)).min(i64::from(b.y) + i64::from(b.h))
            - i64::from(a.y).max(i64::from(b.y));
        ix.max(0) * iy.max(0)
    }

    fn min(w: u32, h: u32) -> Option<MinSize> {
        Some(MinSize {
            width: w,
            height: h,
        })
    }

    fn uniform_minimums(count: usize, w: u32, h: u32) -> Vec<Option<MinSize>> {
        (0..count).map(|_| min(w, h)).collect()
    }

    fn alternating_minimums(
        count: usize,
        w1: u32,
        h1: u32,
        w2: u32,
        h2: u32,
    ) -> Vec<Option<MinSize>> {
        (0..count)
            .map(|i| if i % 2 == 0 { min(w1, h1) } else { min(w2, h2) })
            .collect()
    }

    fn with_unknown_minimums(count: usize, w: u32, h: u32) -> Vec<Option<MinSize>> {
        (0..count)
            .map(|i| if i % 3 == 2 { None } else { min(w, h) })
            .collect()
    }

    /// Разложить и потребовать `Placed`: `Impossible` здесь — провал теста
    /// с указанием дефицита, иначе не видно, что именно не влезло.
    fn fit_placed(slots: &[Rect], area: Rect, minimums: &[Option<MinSize>]) -> Vec<Rect> {
        match fit_slots(slots, area, minimums) {
            FitOutcome::Placed(fitted) => fitted,
            FitOutcome::Impossible {
                deficit_x,
                deficit_y,
            } => panic!("ожидалось Placed, получено Impossible ({deficit_x}, {deficit_y})"),
        }
    }

    /// Инварианты результата: внутри области, не меньше минимумов, попарно
    /// без пересечений. Это то, что обязан гарантировать `fit_slots`, когда
    /// минимумы влезают.
    fn assert_fit_invariants(slots: &[Rect], area: Rect, minimums: &[Option<MinSize>]) {
        for (i, s) in slots.iter().enumerate() {
            assert!(
                s.x >= area.x && s.y >= area.y,
                "слот {i}: вылез за левый край"
            );
            assert!(
                i64::from(s.x) + i64::from(s.w) <= i64::from(area.x) + i64::from(area.w),
                "слот {i}: вылез за правый край"
            );
            assert!(
                i64::from(s.y) + i64::from(s.h) <= i64::from(area.y) + i64::from(area.h),
                "слот {i}: вылез за нижний край"
            );
            if let Some(m) = minimums.get(i).copied().flatten() {
                assert!(
                    s.w >= m.width,
                    "слот {i}: ширина {} меньше минимума {}",
                    s.w,
                    m.width
                );
                assert!(
                    s.h >= m.height,
                    "слот {i}: высота {} меньше минимума {}",
                    s.h,
                    m.height
                );
            }
        }
        for (i, a) in slots.iter().enumerate() {
            for (j, b) in slots.iter().enumerate().skip(i + 1) {
                assert_eq!(
                    overlap(*a, *b),
                    0,
                    "слоты {i} и {j} налезают друг на друга: {a:?} vs {b:?}"
                );
            }
        }
    }

    /// Минимумы, уже выполненные на исходной раскладке, не двигают ни одного
    /// слота: подгонка — исправление перекрытий, а не перестановка окон.
    #[test]
    fn met_minimums_leave_every_layout_untouched() {
        for count in 2..=8 {
            for preset in presets_for(count) {
                let slots = apply(preset, work_area(), 0);
                let minimums = uniform_minimums(count, 300, 175);
                assert_eq!(
                    fit_slots(&slots, work_area(), &minimums),
                    FitOutcome::Placed(slots.clone()),
                    "count={count}: выполненные минимумы не должны двигать слоты"
                );
            }
        }
    }

    /// Исчерпывающая проверка на всех 49 раскладках: если минимумы влезают,
    /// результат обязан быть внутри рабочей области, каждый слот не меньше
    /// своего минимума, и никакие два не пересекаются. Три набора минимумов
    /// (одинаковые, чередующиеся, с неизвестными) на двух рабочих областях —
    /// мониторе пользователя и мониторе слева с отрицательным origin.
    /// Счётчик пар фиксируется числом, как в `group_layout`.
    #[test]
    fn every_layout_fits_minimums_without_overlap() {
        let mut pair_checks = 0usize;
        for (area, unit) in [
            (
                Rect {
                    x: 0,
                    y: 0,
                    w: 2560,
                    h: 1400,
                },
                (300u32, 150u32),
            ),
            (
                Rect {
                    x: -1920,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
                (200u32, 110u32),
            ),
        ] {
            let (w, h) = unit;
            for count in 2..=8 {
                for preset in presets_for(count) {
                    for minimums in [
                        uniform_minimums(count, w, h),
                        alternating_minimums(count, w, h, w / 2, h + 20),
                        with_unknown_minimums(count, w, h),
                    ] {
                        let slots = apply(preset, area, 0);
                        let fitted = fit_placed(&slots, area, &minimums);
                        assert_eq!(fitted.len(), count);
                        assert_fit_invariants(&fitted, area, &minimums);
                        pair_checks += count * (count - 1) / 2;
                    }
                }
            }
        }
        // Пар на пресет — 588 (сумма count*(count-1)/2 для 2..=8 по семи
        // пресетам), всего 2 области × 3 набора × 588.
        assert_eq!(pair_checks, 2 * 3 * 588, "полнота перебора");
    }

    /// Живой репорт 2026-08-26: четыре окна колонками на 2560×1400, минимумы
    /// по замерам (Discord 816, Spotify 600, OBS 1018, Upscayl ~700). Сумма
    /// 3134 px > 2560 — физически не влезает, и честный ответ — `Impossible`
    /// с точным дефицитом, а не раскладка с перекрытиями.
    #[test]
    fn four_columns_with_realistic_minimums_report_exact_deficit() {
        let slots = apply(&presets_for(4)[4], work_area(), 0);
        let minimums = [min(816, 600), min(600, 700), min(1018, 600), min(700, 600)];
        assert_eq!(
            fit_slots(&slots, work_area(), &minimums),
            FitOutcome::Impossible {
                deficit_x: 3134 - 2560,
                deficit_y: 0,
            }
        );
    }

    /// Сумма минимумов больше области по обеим осям — дефицит по обеим:
    /// сетка 2×2 с минимумами 1600×800 требует 2×1600 = 3200 при ширине 2560
    /// (не хватает 640) и 2×800 = 1600 при высоте 1400 (не хватает 200).
    #[test]
    fn grid_that_cannot_fit_reports_deficit_on_both_axes() {
        let slots = apply(&presets_for(4)[6], work_area(), 0);
        let minimums = uniform_minimums(4, 1600, 800);
        assert_eq!(
            fit_slots(&slots, work_area(), &minimums),
            FitOutcome::Impossible {
                deficit_x: 640,
                deficit_y: 200,
            }
        );
    }

    /// «Главное + стопка»: правые окна выше своего слота (минимум 600 px при
    /// слоте 466 px) — подгонка растягивает правую колонку, минимумы
    /// соблюдены, пересечений нет.
    #[test]
    fn stack_with_tall_windows_fits_by_growing_the_side_stack() {
        let slots = apply(&presets_for(3)[0], work_area(), 0);
        let minimums = [min(400, 400), min(400, 600), min(400, 600)];
        let fitted = fit_placed(&slots, work_area(), &minimums);
        assert_fit_invariants(&fitted, work_area(), &minimums);
        assert!(
            fitted[1].h >= 600 && fitted[2].h >= 600,
            "правые окна обязаны стать выше минимума"
        );
    }

    /// Колонки, где часть окон уже минимума (700 при слоте 640), часть —
    /// меньше (500): пересчёт даёт все минимумы и никаких пересечений.
    #[test]
    fn columns_with_oversized_windows_are_redistributed_without_overlap() {
        let slots = apply(&presets_for(4)[4], work_area(), 0);
        let minimums = [min(700, 500), min(500, 700), min(700, 500), min(500, 700)];
        let fitted = fit_placed(&slots, work_area(), &minimums);
        assert_fit_invariants(&fitted, work_area(), &minimums);
    }

    /// Зазор, вычтенный в `apply`, подгонка не ломает: выполненные минимумы
    /// оставляют слоты с зазором нетронутыми; минимумы больше слотов дают
    /// пересчёт без пересечений; минимумы, не влезающие даже при сжатии
    /// зазора до пикселя на полосу, — честный отказ.
    #[test]
    fn gap_slots_are_fit_without_overlap_and_met_minimums_do_not_move_them() {
        let area = work_area();
        // С зазором 16 слоты колонок 620 px: минимум 620 уже выполнен.
        let slots = apply(&presets_for(4)[4], area, 16);
        let met = uniform_minimums(4, 620, 500);
        assert_eq!(
            fit_slots(&slots, area, &met),
            FitOutcome::Placed(slots.clone()),
            "выполненные минимумы не должны трогать слоты с зазором"
        );
        // Минимум 640 > 620: пересчёт, зазор сжимается, но пересечений нет.
        let tight = uniform_minimums(4, 630, 500);
        let fitted = fit_placed(&slots, area, &tight);
        assert_fit_invariants(&fitted, area, &tight);
        // 4 × 640 = 2560, но между слотами и от краёв стоят щели зазора —
        // 9 полос, каждой нужен минимум 1 px: не хватает ровно 5 px.
        let over = uniform_minimums(4, 640, 500);
        assert_eq!(
            fit_slots(&slots, area, &over),
            FitOutcome::Impossible {
                deficit_x: 5,
                deficit_y: 0,
            }
        );
    }

    /// Окно без известного минимума не исчезает, когда соседи требуют всё
    /// место: полоса ему сохраняется (минимум полосы — 1 px), а не
    /// схлопывается в ноль.
    #[test]
    fn slot_without_minimum_survives_when_neighbours_grow() {
        let slots = apply(&presets_for(3)[4], work_area(), 0);
        let minimums = [min(900, 900), None, min(900, 900)];
        let fitted = fit_placed(&slots, work_area(), &minimums);
        assert_fit_invariants(&fitted, work_area(), &minimums);
        assert!(fitted[1].w >= 1, "средний слот не должен исчезнуть");
    }

    /// Один слот (редкий вход, но контракт допускает): минимум больше слота —
    /// слот растягивается до минимума, свободное место остаётся свободным
    /// (слот не расползается на всю область — это не его полосы), паники нет.
    #[test]
    fn single_slot_grows_to_meet_its_minimum() {
        let slots = vec![Rect {
            x: 0,
            y: 0,
            w: 640,
            h: 480,
        }];
        let minimums = [min(1000, 600)];
        let fitted = fit_placed(&slots, work_area(), &minimums);
        assert_fit_invariants(&fitted, work_area(), &minimums);
        assert!(
            fitted[0].w >= 1000 && fitted[0].h >= 600,
            "слот обязан вырасти до минимума, стало {}x{}",
            fitted[0].w,
            fitted[0].h
        );
    }

    /// Минимумы, переданные не на все слоты, не паникуют: недостающие слои
    /// считаются без требований, лишние игнорируются.
    #[test]
    fn shorter_and_longer_minimum_lists_are_tolerated() {
        let slots = apply(&presets_for(4)[4], work_area(), 0);
        let short = [min(700, 500), min(700, 500)];
        let fitted = fit_placed(&slots, work_area(), &short);
        assert_fit_invariants(&fitted, work_area(), &short);
        let long: Vec<Option<MinSize>> = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    min(700, 500)
                } else {
                    min(500, 700)
                }
            })
            .collect();
        let fitted = fit_placed(&slots, work_area(), &long);
        assert_fit_invariants(&fitted, work_area(), &long);
    }

    /// Слот нулевого размера с положительным минимумом некуда ужать —
    /// честный отказ с дефицитом, а не паника.
    #[test]
    fn zero_sized_slot_with_minimum_is_impossible() {
        let slots = vec![Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }];
        assert_eq!(
            fit_slots(&slots, work_area(), &[min(100, 200)]),
            FitOutcome::Impossible {
                deficit_x: 100,
                deficit_y: 200,
            }
        );
    }

    /// Вырожденные входы не паникуют: пустой список укладывается в пустой
    /// список, нулевая рабочая область возвращает слоты как есть, слот за
    /// пределами области (нарушение контракта) — тоже.
    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert_eq!(fit_slots(&[], work_area(), &[]), FitOutcome::Placed(vec![]));
        let zero_area = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        let slots = vec![Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }];
        assert_eq!(
            fit_slots(&slots, zero_area, &[min(100, 100)]),
            FitOutcome::Placed(slots.clone())
        );
        let outside = vec![Rect {
            x: 5000,
            y: 5000,
            w: 100,
            h: 100,
        }];
        assert_eq!(
            fit_slots(&outside, work_area(), &[min(100, 100)]),
            FitOutcome::Placed(outside)
        );
    }

    /// Реальные числа пользователя 2026-08-26: монитор 2560×1440 минус
    /// панель задач, четыре окна (Discord 816×508, Spotify 800×600,
    /// Steam 1010×600, Nemora 700×500). Из семи раскладок на четыре окна
    /// влезают три: «главное + ряд» сверху и снизу (пересчётом — нижним
    /// рядовым слотам хватает высоты, главному ширины) и сетка 2×2 (сразу);
    /// стопки не влезают по высоте (600+600+500 > 1400), колонки — по
    /// ширине (816+800+1010+700 > 2560), ряды — тоже по высоте
    /// (508+600+600+500 > 1400).
    #[test]
    fn four_user_windows_fit_three_layouts_out_of_seven() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        let minimums = [
            min(816, 508),  // Discord
            min(800, 600),  // Spotify
            min(1010, 600), // Steam
            min(700, 500),  // Nemora
        ];
        let verdicts: Vec<LayoutFits> = presets_for(4)
            .iter()
            .map(|p| layout_fits(p, area, 0, &minimums))
            .collect();
        assert_eq!(
            verdicts,
            vec![
                LayoutFits::DoesNotFit, // стопка справа
                LayoutFits::DoesNotFit, // стопка слева
                LayoutFits::Fits,       // главное + ряд сверху
                LayoutFits::Fits,       // главное + ряд снизу
                LayoutFits::DoesNotFit, // колонки
                LayoutFits::DoesNotFit, // ряды
                LayoutFits::Fits,       // сетка 2×2
            ]
        );
        assert_eq!(
            verdicts.iter().filter(|v| **v == LayoutFits::Fits).count(),
            3
        );
    }

    /// Все минимумы неизвестны — вердикт «влезает» на всех 49 раскладках:
    /// обвинять раскладку без доказательств нельзя, окно без известного
    /// минимума ужмётся в любой слот по факту.
    #[test]
    fn unknown_minimums_fit_every_layout() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        for count in 2..=8 {
            for preset in presets_for(count) {
                let unknown: Vec<Option<MinSize>> = (0..count).map(|_| None).collect();
                assert_eq!(
                    layout_fits(preset, area, 0, &unknown),
                    LayoutFits::Fits,
                    "count={count}: без минимумов раскладка не может не влезть"
                );
            }
        }
    }

    /// Число слотов и число минимумов не совпадает — вердикт всё равно
    /// даётся без паники: лишние минимумы игнорируются, недостающие
    /// считаются неизвестными.
    #[test]
    fn slot_count_mismatch_does_not_panic() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        let columns = &presets_for(4)[4];
        // Два минимума на четыре колонки: 816+800 = 1616 ≤ 2560 влезает,
        // остальные колонки без требований.
        let two = [min(816, 508), min(800, 600)];
        assert_eq!(
            layout_fits(columns, area, 0, &two),
            LayoutFits::Fits,
            "недостающие минимумы не должны ломать вердикт"
        );
        // Восемь минимумов на четыре колонки: берутся первые четыре
        // (700+701+702+703 = 2806 > 2560) — не влезает.
        let eight: Vec<Option<MinSize>> = (0..8).map(|i| min(700 + i as u32, 500)).collect();
        assert_eq!(
            layout_fits(columns, area, 0, &eight),
            LayoutFits::DoesNotFit,
            "лишние минимумы игнорируются, первые решают"
        );
    }

    /// Зазор честно учитывается: вердикт проверяет ровно то, что будет
    /// применено. Минимумы, влезающие впритык без зазора, с зазором могут
    /// не влезть — лента обязана пометить раскладку до того, как пользователь
    /// выберет.
    #[test]
    fn gap_px_shrinks_what_fits() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        let columns = &presets_for(4)[4];
        // 620 ≤ 640: влезает и без зазора, и с зазором 16 (слоты 620).
        let relaxed = uniform_minimums(4, 620, 500);
        assert_eq!(layout_fits(columns, area, 0, &relaxed), LayoutFits::Fits);
        assert_eq!(layout_fits(columns, area, 16, &relaxed), LayoutFits::Fits);
        // 640 = ровно 2560/4: без зазора влезает впритык, с зазором 16
        // места уже не хватает (щели зазора съедают пиксели).
        let exact = uniform_minimums(4, 640, 500);
        assert_eq!(layout_fits(columns, area, 0, &exact), LayoutFits::Fits);
        assert_eq!(
            layout_fits(columns, area, 16, &exact),
            LayoutFits::DoesNotFit,
            "зазор обязан участвовать в вердикте"
        );
    }

    /// Вердикт совпадает с тем, что скажет прямое применение: `layout_fits`
    /// зовёт те же `apply` и `fit_slots`, что и тот, кто раскладывает окна,
    /// — обёртка не может разойтись с реальностью.
    #[test]
    fn verdict_matches_direct_fit_slots_on_every_layout() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        for count in 2..=8 {
            for preset in presets_for(count) {
                let minimums = alternating_minimums(count, 700, 500, 300, 600);
                let direct = matches!(
                    fit_slots(&apply(preset, area, 8), area, &minimums),
                    FitOutcome::Placed(_)
                );
                assert_eq!(
                    layout_fits(preset, area, 8, &minimums) == LayoutFits::Fits,
                    direct,
                    "count={count}: обёртка разошлась с прямым применением"
                );
            }
        }
    }

    /// Пустой пресет (сломанная таблица) не паникует и даёт вердикт
    /// «влезает»: раскладки нет, обвинять нечего.
    #[test]
    fn empty_preset_fits_vacuously() {
        let empty = crate::group_layout::Preset { slots: Vec::new() };
        assert_eq!(
            layout_fits(&empty, work_area(), 0, &[min(800, 600)]),
            LayoutFits::Fits
        );
    }
}
