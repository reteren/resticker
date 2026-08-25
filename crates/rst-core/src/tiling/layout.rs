//! Вычисление геометрических координат окон из дерева раскладки
//! (docs/TILING_DESIGN.md §Р3, §Р4, §6).
//!
//! Чистая функция без побочных эффектов и без вызовов Win32: принимает
//! неизменяемое дерево [`Tree`], рабочую область [`Rect`] и параметры отступов
//! [`LayoutParams`], возвращает плоский список прямоугольников окон [`Placement`].
//!
//! # Алгоритм и правила
//!
//! 1. **Внешние отступы (`gaps_out`)**: рабочая область ужимается со всех четырёх
//!    сторон на `gaps_out`.
//! 2. **Сплиты (`SplitH` / `SplitV`)**: пространство делится между детьми согласно
//!    их долям `ratios`. Между $N$ детьми распределяется ровно $(N - 1)$ внутренних
//!    зазоров `gaps_in`.
//! 3. **Группы табов (`Tabbed` / `Stacked`)**: все дети получают один и тот же
//!    прямоугольник содержимого, ужатый сверху на высоту заголовка `tab_bar_h`.
//!    Видимым (`visible = true`) помечается только ребёнок с индексом `focused_child`.
//!    Остальные получают `visible = false` с корректным прямоугольником — координатор
//!    прячет их через DWM-cloak и мгновенно отображает при переключении.
//! 4. **Целочисленная точность (пиксельная сетка)**: распределение отрезков
//!    выполняется методом кумулятивных префиксных сумм с округлением, что
//!    математически исключает накопление ошибки и щели в 1px.

use serde::{Deserialize, Serialize};

use crate::model::Rect;
use crate::tiling::tree::{ContainerLayout, NodeId, NodeKind, Tree, WindowKey};

/// Минимальный допустимый размер плитки в физических пикселях.
/// Предотвращает вырождение геометрии при экстремальных зазорах.
pub const MIN_TILE_PX: u32 = 1;

/// Отступы и параметры геометрии раскладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutParams {
    /// Внутренний зазор между соседними плитками (в пикселях).
    pub gaps_in: i32,
    /// Внешний зазор от края рабочей области экрана до крайних плиток (в пикселях).
    pub gaps_out: i32,
    /// Высота полосы табов для групповых контейнеров `Tabbed`/`Stacked` (в пикселях).
    pub tab_bar_h: i32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            gaps_in: 8,
            gaps_out: 8,
            tab_bar_h: 24,
        }
    }
}

/// Результат размещения одного окна на экране.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// Идентификатор окна.
    pub window: WindowKey,
    /// Целевой прямоугольник окна в физических пикселях экрана.
    pub rect: Rect,
    /// Флаг видимости окна. `false` для неактивных вкладок в таб-группах
    /// (координатор скрывает такие окна через DWM-cloak).
    pub visible: bool,
}

/// Рассчитать прямоугольники всех окон дерева для заданной рабочей области.
///
/// Возвращает плоский список [`Placement`]. Для пустого дерева возвращается пустой вектор.
pub fn layout(tree: &Tree, work_area: Rect, params: &LayoutParams) -> Vec<Placement> {
    if tree.is_empty() {
        return Vec::new();
    }

    let gaps_out = params.gaps_out.max(0);
    let root_x = work_area.x + gaps_out;
    let root_y = work_area.y + gaps_out;
    let root_w = ((work_area.w as i32) - 2 * gaps_out).max(MIN_TILE_PX as i32) as u32;
    let root_h = ((work_area.h as i32) - 2 * gaps_out).max(MIN_TILE_PX as i32) as u32;

    let root_rect = Rect {
        x: root_x,
        y: root_y,
        w: root_w,
        h: root_h,
    };

    let mut placements = Vec::new();
    layout_node(tree, tree.root(), root_rect, true, params, &mut placements);
    placements
}

/// Рекурсивный обход узла дерева для вычисления геометрии.
fn layout_node(
    tree: &Tree,
    node_id: NodeId,
    rect: Rect,
    visible: bool,
    params: &LayoutParams,
    out: &mut Vec<Placement>,
) {
    let Some(node) = tree.get(node_id) else {
        return;
    };

    match &node.kind {
        NodeKind::Window(window_key) => {
            out.push(Placement {
                window: *window_key,
                rect,
                visible,
            });
        }
        NodeKind::Container(container) => {
            let n = container.children.len();
            if n == 0 {
                return;
            }

            match container.layout {
                ContainerLayout::Tabbed | ContainerLayout::Stacked => {
                    let tab_h = params.tab_bar_h.max(0) as u32;
                    let content_h = rect.h.saturating_sub(tab_h).max(MIN_TILE_PX);
                    let content_y = rect.y + tab_h.min(rect.h) as i32;

                    let content_rect = Rect {
                        x: rect.x,
                        y: content_y,
                        w: rect.w,
                        h: content_h,
                    };

                    for (i, &child_id) in container.children.iter().enumerate() {
                        let child_visible = visible && (i == container.focused_child);
                        layout_node(tree, child_id, content_rect, child_visible, params, out);
                    }
                }
                ContainerLayout::SplitH => {
                    if n == 1 {
                        layout_node(tree, container.children[0], rect, visible, params, out);
                        return;
                    }

                    let gaps_in = fitting_gap(params.gaps_in, rect.w as i32, n);
                    let total_gaps = (n as i32 - 1) * gaps_in;
                    let available_w = (rect.w as i32) - total_gaps;
                    let widths = split_span(available_w, &container.ratios, n, MIN_TILE_PX as i32);

                    let mut cur_x = rect.x;
                    for (i, &child_id) in container.children.iter().enumerate() {
                        let w = widths[i] as u32;
                        let child_rect = Rect {
                            x: cur_x,
                            y: rect.y,
                            w,
                            h: rect.h,
                        };
                        layout_node(tree, child_id, child_rect, visible, params, out);
                        cur_x += widths[i] + gaps_in;
                    }
                }
                ContainerLayout::SplitV => {
                    if n == 1 {
                        layout_node(tree, container.children[0], rect, visible, params, out);
                        return;
                    }

                    let gaps_in = fitting_gap(params.gaps_in, rect.h as i32, n);
                    let total_gaps = (n as i32 - 1) * gaps_in;
                    let available_h = (rect.h as i32) - total_gaps;
                    let heights = split_span(available_h, &container.ratios, n, MIN_TILE_PX as i32);

                    let mut cur_y = rect.y;
                    for (i, &child_id) in container.children.iter().enumerate() {
                        let h = heights[i] as u32;
                        let child_rect = Rect {
                            x: rect.x,
                            y: cur_y,
                            w: rect.w,
                            h,
                        };
                        layout_node(tree, child_id, child_rect, visible, params, out);
                        cur_y += heights[i] + gaps_in;
                    }
                }
            }
        }
    }
}

/// Прямоугольник полосы табов контейнера-группы (для отрисовки на оверлее).
///
/// Возвращает `Some(Rect)`, если `container` существует в дереве, является группой
/// (`Tabbed` или `Stacked`) и содержит хотя бы одного ребёнка.
/// `None` — для сплитов, окон, несуществующих или пустых узлов.
pub fn tab_bar_rect(
    tree: &Tree,
    container: NodeId,
    work_area: Rect,
    params: &LayoutParams,
) -> Option<Rect> {
    let node = tree.get(container)?;
    let c = node.container()?;
    if c.children.is_empty() {
        return None;
    }
    if !matches!(c.layout, ContainerLayout::Tabbed | ContainerLayout::Stacked) {
        return None;
    }

    let gaps_out = params.gaps_out.max(0);
    let root_x = work_area.x + gaps_out;
    let root_y = work_area.y + gaps_out;
    let root_w = ((work_area.w as i32) - 2 * gaps_out).max(MIN_TILE_PX as i32) as u32;
    let root_h = ((work_area.h as i32) - 2 * gaps_out).max(MIN_TILE_PX as i32) as u32;

    let root_rect = Rect {
        x: root_x,
        y: root_y,
        w: root_w,
        h: root_h,
    };

    let container_rect = find_node_rect(tree, tree.root(), root_rect, container, params)?;
    let tab_h = params.tab_bar_h.max(0) as u32;
    Some(Rect {
        x: container_rect.x,
        y: container_rect.y,
        w: container_rect.w,
        h: tab_h.min(container_rect.h),
    })
}

/// Найти геометрию целевого узла в дереве.
fn find_node_rect(
    tree: &Tree,
    cur_id: NodeId,
    cur_rect: Rect,
    target_id: NodeId,
    params: &LayoutParams,
) -> Option<Rect> {
    if cur_id == target_id {
        return Some(cur_rect);
    }

    let node = tree.get(cur_id)?;
    let container = node.container()?;
    let n = container.children.len();
    if n == 0 {
        return None;
    }

    match container.layout {
        ContainerLayout::Tabbed | ContainerLayout::Stacked => {
            let tab_h = params.tab_bar_h.max(0) as u32;
            let content_h = cur_rect.h.saturating_sub(tab_h).max(MIN_TILE_PX);
            let content_y = cur_rect.y + tab_h.min(cur_rect.h) as i32;

            let content_rect = Rect {
                x: cur_rect.x,
                y: content_y,
                w: cur_rect.w,
                h: content_h,
            };

            for &child_id in &container.children {
                if let Some(r) = find_node_rect(tree, child_id, content_rect, target_id, params) {
                    return Some(r);
                }
            }
        }
        ContainerLayout::SplitH => {
            if n == 1 {
                return find_node_rect(tree, container.children[0], cur_rect, target_id, params);
            }

            let gaps_in = params.gaps_in.max(0);
            let total_gaps = (n as i32 - 1) * gaps_in;
            let available_w = (cur_rect.w as i32) - total_gaps;
            let widths = split_span(available_w, &container.ratios, n, MIN_TILE_PX as i32);

            let mut cur_x = cur_rect.x;
            for (i, &child_id) in container.children.iter().enumerate() {
                let w = widths[i] as u32;
                let child_rect = Rect {
                    x: cur_x,
                    y: cur_rect.y,
                    w,
                    h: cur_rect.h,
                };
                if let Some(r) = find_node_rect(tree, child_id, child_rect, target_id, params) {
                    return Some(r);
                }
                cur_x += widths[i] + gaps_in;
            }
        }
        ContainerLayout::SplitV => {
            if n == 1 {
                return find_node_rect(tree, container.children[0], cur_rect, target_id, params);
            }

            let gaps_in = params.gaps_in.max(0);
            let total_gaps = (n as i32 - 1) * gaps_in;
            let available_h = (cur_rect.h as i32) - total_gaps;
            let heights = split_span(available_h, &container.ratios, n, MIN_TILE_PX as i32);

            let mut cur_y = cur_rect.y;
            for (i, &child_id) in container.children.iter().enumerate() {
                let h = heights[i] as u32;
                let child_rect = Rect {
                    x: cur_rect.x,
                    y: cur_y,
                    w: cur_rect.w,
                    h,
                };
                if let Some(r) = find_node_rect(tree, child_id, child_rect, target_id, params) {
                    return Some(r);
                }
                cur_y += heights[i] + gaps_in;
            }
        }
    }

    None
}

/// Распределить отрезок `available_span` между `n` частями в соответствии с `ratios`.
///
/// Гарантии алгоритма:
/// 1. При `available_span >= n * min_px` сумма всех частей В ТОЧНОСТИ равна `available_span`
///    (исключается щель в 1px из-за погрешностей деления).
/// 2. Каждая часть имеет размер не меньше `min_px`.
/// 3. Округление остатков монотонно распределяется через кумулятивные префиксные суммы.
fn split_span(available_span: i32, ratios: &[f64], n: usize, min_px: i32) -> Vec<i32> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        // Раздуть единственную плитку до min_px нельзя: в контейнере
        // шириной 0 она вылезла бы наружу. Пустая плитка невидима, а
        // переполнение — видимый наезд на соседа.
        return vec![available_span.max(0)];
    }

    // Места не хватает даже на минимальные плитки.
    //
    // Раньше здесь каждому ребёнку выдавался гарантированный `min_px`, и
    // сумма выданного превышала ширину контейнера — плитки наезжали на
    // соседний контейнер. Нашёл фаззер инвариантов
    // (`crates/rst-core/tests/tiling_invariants.rs`,
    // `narrow_container_children_overflow_into_sibling`): обычные тесты
    // такого не видели, потому что для этого нужна доля порядка процента
    // при узком экране.
    //
    // Правильный размен: лучше плитка нулевой ширины, чем плитка, залезшая
    // на чужую территорию. Первая невидима, вторая портит всю раскладку.
    if available_span < n as i32 * min_px {
        return spread_evenly(available_span.max(0), n);
    }

    // Подготавливаем нормализованные доли
    let mut normalized_ratios = Vec::with_capacity(n);
    if ratios.len() == n {
        let sum: f64 = ratios.iter().filter(|r| r.is_finite() && **r > 0.0).sum();
        if sum.is_finite() && sum > 0.0 {
            for &r in ratios {
                let val = if r.is_finite() && r > 0.0 {
                    r / sum
                } else {
                    0.0
                };
                normalized_ratios.push(val);
            }
        }
    }
    if normalized_ratios.len() != n {
        normalized_ratios.clear();
        normalized_ratios.resize(n, 1.0 / n as f64);
    }

    // Метод кумулятивных префиксных сумм с округлением:
    // C_k = round(available_span * sum_{i=0..=k}(ratio_i))
    // span_k = C_k - C_{k-1}
    // Это математически гарантирует sum(span_k) == available_span.
    let mut result = Vec::with_capacity(n);
    let mut prev_cum = 0i32;
    let mut running_ratio = 0.0f64;

    for (i, &ratio) in normalized_ratios.iter().enumerate() {
        let cur_cum = if i == n - 1 {
            available_span
        } else {
            running_ratio += ratio;
            (available_span as f64 * running_ratio).round() as i32
        };
        let span = cur_cum - prev_cum;
        result.push(span);
        prev_cum = cur_cum;
    }

    // Защита от min_px для экстремальных пропорций (например, 0.999 / 0.001)
    let has_undersized = result.iter().any(|&s| s < min_px);
    if has_undersized {
        let mut deficit = 0i32;
        for s in result.iter_mut() {
            if *s < min_px {
                deficit += min_px - *s;
                *s = min_px;
            }
        }
        while deficit > 0 {
            if let Some(max_elem) = result
                .iter_mut()
                .filter(|s| **s > min_px)
                .max_by_key(|s| **s)
            {
                *max_elem -= 1;
                deficit -= 1;
            } else {
                break;
            }
        }
    }

    // Последний рубеж, общий для всех веток выше: сумма НИКОГДА не должна
    // превышать доступную длину. Компенсация min_px может выйти из цикла с
    // неоплаченным дефицитом (если отнимать уже не у кого) — и тогда без
    // этой проверки плитки снова вылезли бы за контейнер.
    clamp_to_span(&mut result, available_span);

    result
}

/// Зазор, который реально влезает в контейнер.
///
/// Зазор — украшение, содержимое — суть: если запрошенный `gaps_in` не
/// оставляет каждой плитке даже минимального размера, ужимаем зазор, а не
/// плитки. Иначе получалось либо переполнение контейнера (плитки наезжали на
/// соседний — нашёл фаззер инвариантов), либо плитки нулевого размера при
/// внешне безобидной настройке гэпов.
///
/// Пользователь, поставивший гэп больше экрана, увидит уменьшенный зазор — и
/// это единственное осмысленное поведение: показать ему пустой экран было бы
/// хуже.
fn fitting_gap(requested: i32, container_span: i32, n: usize) -> i32 {
    let requested = requested.max(0);
    if n <= 1 {
        return requested;
    }
    let slots = n as i32 - 1;
    // Сколько места останется под зазоры, если каждой плитке дать минимум.
    let room = container_span - n as i32 * MIN_TILE_PX as i32;
    if room <= 0 {
        return 0;
    }
    requested.min(room / slots)
}

/// Раздать `span` поровну между `n` частями, распределив остаток по первым.
fn spread_evenly(span: i32, n: usize) -> Vec<i32> {
    let base = span / n as i32;
    let mut out = vec![base; n];
    let mut rest = span - base * n as i32;
    for slot in out.iter_mut() {
        if rest <= 0 {
            break;
        }
        *slot += 1;
        rest -= 1;
    }
    out
}

/// Срезать превышение с самых больших частей, не уводя их ниже нуля.
///
/// Именно с больших: отнимать у маленькой плитки заметнее, чем у крупной.
fn clamp_to_span(parts: &mut [i32], span: i32) {
    let span = span.max(0);
    let mut total: i32 = parts.iter().sum();
    while total > span {
        let Some(biggest) = parts.iter_mut().filter(|p| **p > 0).max_by_key(|p| **p) else {
            break;
        };
        *biggest -= 1;
        total -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::tree::{ContainerLayout, InsertAt, Tree, WindowKey};

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    fn params(gaps_in: i32, gaps_out: i32, tab_bar_h: i32) -> LayoutParams {
        LayoutParams {
            gaps_in,
            gaps_out,
            tab_bar_h,
        }
    }

    fn default_work_area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }

    #[test]
    fn single_window_occupies_entire_work_area_minus_outer_gaps() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);

        let p = params(10, 20, 30);
        let placements = layout(&tree, default_work_area(), &p);

        assert_eq!(placements.len(), 1);
        assert_eq!(
            placements[0],
            Placement {
                window: w(1),
                rect: Rect {
                    x: 20,
                    y: 20,
                    w: 1920 - 40,
                    h: 1080 - 40,
                },
                visible: true,
            }
        );
    }

    #[test]
    fn empty_tree_produces_empty_placements() {
        let tree = Tree::new(ContainerLayout::SplitH);
        let p = params(8, 8, 24);
        let placements = layout(&tree, default_work_area(), &p);
        assert!(placements.is_empty());
    }

    #[test]
    fn large_outer_gaps_do_not_produce_negative_dimensions() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);

        let small_work_area = Rect {
            x: 100,
            y: 100,
            w: 50,
            h: 50,
        };
        let p = params(10, 100, 20); // gaps_out (100) больше половины размера (50)
        let placements = layout(&tree, small_work_area, &p);

        assert_eq!(placements.len(), 1);
        assert!(placements[0].rect.w >= MIN_TILE_PX);
        assert!(placements[0].rect.h >= MIN_TILE_PX);
        assert_eq!(placements[0].rect.x, 200);
        assert_eq!(placements[0].rect.y, 200);
    }

    #[test]
    fn gaps_too_large_for_the_screen_shrink_instead_of_squashing_tiles() {
        // Раньше этот тест требовал, чтобы КАЖДАЯ плитка ужалась ровно до
        // MIN_TILE_PX, а зазоры остались запрошенными. Это и было ошибкой:
        // 5 плиток по 1px плюс 4 зазора по 20px не помещаются в 40px, и
        // плитки вылезали за контейнер, наезжая на соседний (нашёл фаззер
        // инвариантов `narrow_container_children_overflow_into_sibling`).
        //
        // Верное поведение: зазор — украшение, содержимое — суть. Ужимается
        // зазор, а плитки остаются видимыми и внутри контейнера.
        let mut tree = Tree::new(ContainerLayout::SplitH);
        for i in 1..=5 {
            tree.insert_window(w(i), InsertAt::Root);
        }

        let small_work_area = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: 100,
        };
        let p = params(20, 0, 20); // 4 зазора по 20px = 80px при ширине 40px
        let placements = layout(&tree, small_work_area, &p);

        assert_eq!(placements.len(), 5);
        for placement in &placements {
            assert!(
                placement.rect.w >= MIN_TILE_PX,
                "плитка не должна исчезать: {placement:?}"
            );
            assert_eq!(placement.rect.h, 100);
            assert!(placement.visible);
        }

        // Главное: всё уместилось в рабочую область и ничего не пересекается.
        let right_edge = placements
            .iter()
            .map(|p| p.rect.x + p.rect.w as i32)
            .max()
            .unwrap();
        assert!(
            right_edge <= small_work_area.w as i32,
            "плитки вылезли за экран: правый край {right_edge}"
        );
        let mut sorted: Vec<&Placement> = placements.iter().collect();
        sorted.sort_by_key(|p| p.rect.x);
        for pair in sorted.windows(2) {
            let (left, right) = (pair[0], pair[1]);
            assert!(
                left.rect.x + left.rect.w as i32 <= right.rect.x,
                "плитки пересеклись: {left:?} и {right:?}"
            );
        }
    }

    #[test]
    fn nested_containers_geometry_covers_parent_bounds() {
        // Дерево: Root SplitH
        //   - Left: W1 (0.5)
        //   - Right: SplitV (0.5)
        //       - Top: W2 (0.5)
        //       - Bottom: SplitH (0.5)
        //           - W3 (0.5)
        //           - W4 (0.5)
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);
        let w2 = tree.insert_window(w(2), InsertAt::Root);
        let split_v = tree.split_leaf(w2, ContainerLayout::SplitV).unwrap();
        let w3 = tree.insert_window(
            w(3),
            InsertAt::Into {
                parent: split_v,
                index: 1,
            },
        );
        let split_h = tree.split_leaf(w3, ContainerLayout::SplitH).unwrap();
        tree.insert_window(
            w(4),
            InsertAt::Into {
                parent: split_h,
                index: 1,
            },
        );

        let work_area = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 600,
        };
        let p = params(0, 0, 0); // без зазоров для чистой проверки покрытия
        let placements = layout(&tree, work_area, &p);

        assert_eq!(placements.len(), 4);

        let p1 = placements.iter().find(|p| p.window == w(1)).unwrap();
        let p2 = placements.iter().find(|p| p.window == w(2)).unwrap();
        let p3 = placements.iter().find(|p| p.window == w(3)).unwrap();
        let p4 = placements.iter().find(|p| p.window == w(4)).unwrap();

        assert_eq!(
            p1.rect,
            Rect {
                x: 0,
                y: 0,
                w: 500,
                h: 600
            }
        );
        assert_eq!(
            p2.rect,
            Rect {
                x: 500,
                y: 0,
                w: 500,
                h: 300
            }
        );
        assert_eq!(
            p3.rect,
            Rect {
                x: 500,
                y: 300,
                w: 250,
                h: 300
            }
        );
        assert_eq!(
            p4.rect,
            Rect {
                x: 750,
                y: 300,
                w: 250,
                h: 300
            }
        );
    }

    #[test]
    fn unequal_ratios_distribute_space_proportionally_after_gaps() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);
        tree.insert_window(w(2), InsertAt::Root);

        // Выставляем пропорции 0.7 / 0.3
        if let Some(c) = tree.get_mut(tree.root()).and_then(|n| n.container_mut()) {
            c.ratios = vec![0.7, 0.3];
        }

        let work_area = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 500,
        };
        let p = params(100, 0, 0);
        // Доступная ширина = 1000 - 100 (gap) = 900
        // W1 = 900 * 0.7 = 630
        // W2 = 900 * 0.3 = 270
        let placements = layout(&tree, work_area, &p);

        assert_eq!(placements.len(), 2);
        assert_eq!(
            placements[0].rect,
            Rect {
                x: 0,
                y: 0,
                w: 630,
                h: 500,
            }
        );
        assert_eq!(
            placements[1].rect,
            Rect {
                x: 730, // 630 + 100
                y: 0,
                w: 270,
                h: 500,
            }
        );
        assert_eq!(
            placements[0].rect.w + 100 + placements[1].rect.w,
            work_area.w,
            "ширины с зазором покрывают ровно всю ширину экрана"
        );
    }

    #[test]
    fn tabbed_inside_split_v_offsets_height_only_for_tabbed_group() {
        let mut tree = Tree::new(ContainerLayout::SplitV);
        tree.insert_window(w(1), InsertAt::Root);
        let w2 = tree.insert_window(w(2), InsertAt::Root);
        let tab_group = tree.split_leaf(w2, ContainerLayout::Tabbed).unwrap();
        let w3 = tree.insert_window(
            w(3),
            InsertAt::Into {
                parent: tab_group,
                index: 1,
            },
        );

        // Активным табом делаем w3
        tree.set_focus(w3).unwrap();

        let work_area = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 1000,
        };
        let p = params(0, 0, 40);
        let placements = layout(&tree, work_area, &p);

        assert_eq!(placements.len(), 3);

        let p1 = placements.iter().find(|p| p.window == w(1)).unwrap();
        let p2 = placements.iter().find(|p| p.window == w(2)).unwrap();
        let p3 = placements.iter().find(|p| p.window == w(3)).unwrap();

        // Верхний сплит W1: без смещения таб-бара
        assert_eq!(
            p1.rect,
            Rect {
                x: 0,
                y: 0,
                w: 1000,
                h: 500
            }
        );
        assert!(p1.visible);

        // Нижний сплит Tabbed [W2, W3]: высота 500 - 40 = 460, y = 500 + 40 = 540
        assert_eq!(
            p2.rect,
            Rect {
                x: 0,
                y: 540,
                w: 1000,
                h: 460
            }
        );
        assert_eq!(
            p3.rect,
            Rect {
                x: 0,
                y: 540,
                w: 1000,
                h: 460
            }
        );

        // W2 не в фокусе -> false, W3 в фокусе -> true
        assert!(!p2.visible);
        assert!(p3.visible);
    }

    #[test]
    fn rounding_remainder_is_distributed_without_one_pixel_gap() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);
        tree.insert_window(w(2), InsertAt::Root);
        tree.insert_window(w(3), InsertAt::Root);

        // 1000px делится на 3 части (333.333...)
        let work_area = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 600,
        };
        let p = params(0, 0, 0);
        let placements = layout(&tree, work_area, &p);

        assert_eq!(placements.len(), 3);
        let total_w: u32 = placements.iter().map(|p| p.rect.w).sum();
        assert_eq!(total_w, 1000, "сумма ширин должна быть ровно 1000px");

        assert_eq!(
            placements[0].rect,
            Rect {
                x: 0,
                y: 0,
                w: 333,
                h: 600
            }
        );
        assert_eq!(
            placements[1].rect,
            Rect {
                x: 333,
                y: 0,
                w: 334,
                h: 600
            }
        );
        assert_eq!(
            placements[2].rect,
            Rect {
                x: 667,
                y: 0,
                w: 333,
                h: 600
            }
        );
    }

    #[test]
    fn stacked_container_hides_unfocused_children_and_shares_rect() {
        let mut tree = Tree::new(ContainerLayout::Stacked);
        tree.insert_window(w(1), InsertAt::Root);
        let w2 = tree.insert_window(w(2), InsertAt::Root);
        tree.insert_window(w(3), InsertAt::Root);

        tree.set_focus(w2).unwrap();

        let work_area = Rect {
            x: 10,
            y: 20,
            w: 800,
            h: 600,
        };
        let p = params(0, 0, 30);
        let placements = layout(&tree, work_area, &p);

        assert_eq!(placements.len(), 3);

        let shared_rect = Rect {
            x: 10,
            y: 50, // 20 + 30
            w: 800,
            h: 570, // 600 - 30
        };

        for placement in &placements {
            assert_eq!(placement.rect, shared_rect);
        }

        let p1 = placements.iter().find(|p| p.window == w(1)).unwrap();
        let p2 = placements.iter().find(|p| p.window == w(2)).unwrap();
        let p3 = placements.iter().find(|p| p.window == w(3)).unwrap();

        assert!(!p1.visible);
        assert!(p2.visible);
        assert!(!p3.visible);
    }

    #[test]
    fn tab_bar_rect_computes_header_strip_for_tabbed_container() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);
        let w2 = tree.insert_window(w(2), InsertAt::Root);
        let tab_group = tree.split_leaf(w2, ContainerLayout::Tabbed).unwrap();
        tree.insert_window(
            w(3),
            InsertAt::Into {
                parent: tab_group,
                index: 1,
            },
        );

        let work_area = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 500,
        };
        let p = params(0, 0, 28);

        let bar = tab_bar_rect(&tree, tab_group, work_area, &p)
            .expect("полоса табов должна существовать");
        assert_eq!(
            bar,
            Rect {
                x: 500,
                y: 0,
                w: 500,
                h: 28,
            }
        );
    }

    #[test]
    fn tab_bar_rect_returns_none_for_split_container_or_window() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        let w1 = tree.insert_window(w(1), InsertAt::Root);
        let p = params(8, 8, 24);

        assert_eq!(
            tab_bar_rect(&tree, tree.root(), default_work_area(), &p),
            None
        );
        assert_eq!(tab_bar_rect(&tree, w1, default_work_area(), &p), None);
    }

    #[test]
    fn tab_bar_rect_returns_none_for_nonexistent_node() {
        let tree = Tree::new(ContainerLayout::Tabbed);
        let p = params(8, 8, 24);
        assert_eq!(
            tab_bar_rect(&tree, NodeId(9999), default_work_area(), &p),
            None
        );
    }

    #[test]
    fn tab_bar_rect_returns_none_when_container_is_empty() {
        let tree = Tree::new(ContainerLayout::Tabbed);
        let p = params(8, 8, 24);
        assert_eq!(
            tab_bar_rect(&tree, tree.root(), default_work_area(), &p),
            None
        );
    }

    #[test]
    fn switching_tree_focus_updates_tab_placement_visibility() {
        let mut tree = Tree::new(ContainerLayout::Tabbed);
        let w1 = tree.insert_window(w(1), InsertAt::Root);
        let _w2 = tree.insert_window(w(2), InsertAt::Root);

        let p = params(0, 0, 20);

        // По умолчанию активен w2 (последняя вставка)
        let pl = layout(&tree, default_work_area(), &p);
        assert!(!pl.iter().find(|p| p.window == w(1)).unwrap().visible);
        assert!(pl.iter().find(|p| p.window == w(2)).unwrap().visible);

        // Переключаем фокус на w1
        tree.set_focus(w1).unwrap();
        let pl = layout(&tree, default_work_area(), &p);
        assert!(pl.iter().find(|p| p.window == w(1)).unwrap().visible);
        assert!(!pl.iter().find(|p| p.window == w(2)).unwrap().visible);
    }

    #[test]
    fn hidden_tab_group_propagates_invisibility_to_all_its_children() {
        // Внешний Tabbed [W1, Вложенный SplitV [W2, W3]]
        let mut tree = Tree::new(ContainerLayout::Tabbed);
        let w1 = tree.insert_window(w(1), InsertAt::Root);
        let w2 = tree.insert_window(w(2), InsertAt::Root);
        let split_v = tree.split_leaf(w2, ContainerLayout::SplitV).unwrap();
        tree.insert_window(
            w(3),
            InsertAt::Into {
                parent: split_v,
                index: 1,
            },
        );

        // Фокус на W1 -> Вложенный split_v неактивен целиком
        tree.set_focus(w1).unwrap();

        let p = params(0, 0, 20);
        let placements = layout(&tree, default_work_area(), &p);

        assert_eq!(placements.len(), 3);
        assert!(
            placements
                .iter()
                .find(|p| p.window == w(1))
                .unwrap()
                .visible
        );
        assert!(
            !placements
                .iter()
                .find(|p| p.window == w(2))
                .unwrap()
                .visible
        );
        assert!(
            !placements
                .iter()
                .find(|p| p.window == w(3))
                .unwrap()
                .visible
        );
    }

    #[test]
    fn zero_gaps_and_zero_tab_bar_height_are_handled_cleanly() {
        let mut tree = Tree::new(ContainerLayout::Tabbed);
        tree.insert_window(w(1), InsertAt::Root);

        let p = params(0, 0, 0);
        let placements = layout(&tree, default_work_area(), &p);

        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].rect, default_work_area());
        assert!(placements[0].visible);
    }

    #[test]
    fn three_way_split_h_remainder_exact_coverage() {
        let mut tree = Tree::new(ContainerLayout::SplitH);
        tree.insert_window(w(1), InsertAt::Root);
        tree.insert_window(w(2), InsertAt::Root);
        tree.insert_window(w(3), InsertAt::Root);

        let work_area = Rect {
            x: 10,
            y: 20,
            w: 1920,
            h: 1080,
        };
        let p = params(15, 25, 0);

        let placements = layout(&tree, work_area, &p);
        assert_eq!(placements.len(), 3);

        // Проверяем непрерывность: p0.right + gap == p1.left, p1.right + gap == p2.left
        let p0 = &placements[0];
        let p1 = &placements[1];
        let p2 = &placements[2];

        assert_eq!(p0.rect.x, 10 + 25);
        assert_eq!(p0.rect.x + p0.rect.w as i32 + 15, p1.rect.x);
        assert_eq!(p1.rect.x + p1.rect.w as i32 + 15, p2.rect.x);
        assert_eq!(p2.rect.x + p2.rect.w as i32, 10 + 1920 - 25);
    }

    #[test]
    fn three_way_split_v_remainder_exact_coverage() {
        let mut tree = Tree::new(ContainerLayout::SplitV);
        tree.insert_window(w(1), InsertAt::Root);
        tree.insert_window(w(2), InsertAt::Root);
        tree.insert_window(w(3), InsertAt::Root);

        let work_area = Rect {
            x: 0,
            y: 0,
            w: 800,
            h: 1000,
        };
        let p = params(12, 10, 0);

        let placements = layout(&tree, work_area, &p);
        assert_eq!(placements.len(), 3);

        let p0 = &placements[0];
        let p1 = &placements[1];
        let p2 = &placements[2];

        assert_eq!(p0.rect.y, 10);
        assert_eq!(p0.rect.y + p0.rect.h as i32 + 12, p1.rect.y);
        assert_eq!(p1.rect.y + p1.rect.h as i32 + 12, p2.rect.y);
        assert_eq!(p2.rect.y + p2.rect.h as i32, 1000 - 10);
    }

    #[test]
    fn layout_params_and_placement_serde_roundtrip() {
        let lp = params(10, 15, 25);
        let json = serde_json::to_string(&lp).unwrap();
        let back: LayoutParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back, lp);

        let pl = Placement {
            window: w(42),
            rect: Rect {
                x: 10,
                y: 20,
                w: 300,
                h: 400,
            },
            visible: true,
        };
        let json_pl = serde_json::to_string(&pl).unwrap();
        let back_pl: Placement = serde_json::from_str(&json_pl).unwrap();
        assert_eq!(back_pl, pl);
    }

    #[test]
    fn tab_bar_rect_for_stacked_container() {
        let mut tree = Tree::new(ContainerLayout::Stacked);
        tree.insert_window(w(1), InsertAt::Root);
        tree.insert_window(w(2), InsertAt::Root);

        let work_area = Rect {
            x: 100,
            y: 100,
            w: 800,
            h: 600,
        };
        let p = params(0, 10, 32);

        let bar = tab_bar_rect(&tree, tree.root(), work_area, &p)
            .expect("полоса табов должна существовать");
        assert_eq!(
            bar,
            Rect {
                x: 110,
                y: 110,
                w: 780,
                h: 32,
            }
        );
    }
}
