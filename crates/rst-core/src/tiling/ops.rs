//! Операции над деревом тайлинга: фокус, перемещение, обмен, ресайз и группы
//! (docs/TILING_DESIGN.md §Р4).
//!
//! # Главное архитектурное решение: Структурная, а не геометрическая навигация
//!
//! Направление движения фокуса (`Left`, `Right`, `Up`, `Down`) определяется
//! **структурно по дереву контейнеров** (как в i3/sway), а не через опрос
//! экранных координат (AABB/геометрии):
//! 1. От сфокусированного листа мы поднимаемся вверх по цепочке предков, пока не
//!    найдём контейнер с соответствующей ориентацией (`SplitH` для `Left`/`Right`,
//!    `SplitV` для `Up`/`Down`), у которого есть соседний ребёнок в нужную сторону.
//! 2. Перейдя в соседнюю ветку, мы спускаемся вниз строго по `focused_child`
//!    каждого вложенного контейнера («возврат туда, где был» — классическое
//!    поведение i3).
//! 3. Группы (`Tabbed` и `Stacked`) намеренно **НЕ считаются сплитами**
//!    (`layout.is_split() == false`): сквозной фокус проходит мимо группы к её
//!    внешним соседям, а переключение между табами внутри группы осуществляется
//!    через [`cycle_group`].
//!
//! Преимущества структурного подхода:
//! - Модуль остаётся на 100% платформенно-чистым и детерминированно тестируемым
//!   без необходимости эмулировать физические экраны и DPI.
//! - Полностью исключаются геометрические неоднозначности при сложных вложенных
//!   раскладках.

use serde::{Deserialize, Serialize};

use super::tree::{ContainerLayout, InsertAt, NodeId, NodeKind, Tree, normalize};

/// Минимальная доля узла в контейнере при изменении размера (5%).
pub const MIN_CONTAINER_RATIO: f64 = 0.05;

/// Направление перемещения фокуса, окна или ресайза.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// Соответствующий тип сплит-раскладки для данного направления.
    pub fn matching_layout(self) -> ContainerLayout {
        match self {
            Self::Left | Self::Right => ContainerLayout::SplitH,
            Self::Up | Self::Down => ContainerLayout::SplitV,
        }
    }

    /// Движение идёт назад (влево/вверх) по списку детей?
    pub fn is_backward(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }

    /// Движение идёт вперёд (вправо/вниз) по списку детей?
    pub fn is_forward(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

/// Найти соседнюю ветку дерева в направлении `dir` от узла `start_leaf`.
fn find_neighbor_branch(tree: &Tree, start_leaf: NodeId, dir: Direction) -> Option<NodeId> {
    let matching_layout = dir.matching_layout();
    let mut curr = start_leaf;
    while let Some(parent) = tree.parent_of(curr) {
        let parent_node = tree.get(parent)?;
        let container = parent_node.container()?;
        if container.layout == matching_layout {
            let idx = tree.index_in_parent(curr)?;
            if dir.is_backward() && idx > 0 {
                return Some(container.children[idx - 1]);
            }
            if dir.is_forward() && idx + 1 < container.children.len() {
                return Some(container.children[idx + 1]);
            }
        }
        curr = parent;
    }
    None
}

/// Спуститься по `focused_child` каждого контейнера до целевого листа.
fn descend_to_leaf(tree: &Tree, mut curr: NodeId) -> Option<NodeId> {
    loop {
        let node = tree.get(curr)?;
        match &node.kind {
            NodeKind::Window(_) => return Some(curr),
            NodeKind::Container(c) => {
                if c.children.is_empty() {
                    return None;
                }
                let idx = c.focused_child.min(c.children.len().saturating_sub(1));
                curr = c.children[idx];
            }
        }
    }
}

/// Перевести фокус в направлении `dir`. Возвращает `false`, если двигаться некуда.
pub fn focus_direction(tree: &mut Tree, dir: Direction) -> bool {
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };
    let Some(target_branch) = find_neighbor_branch(tree, focus_leaf, dir) else {
        return false;
    };
    let Some(target_leaf) = descend_to_leaf(tree, target_branch) else {
        return false;
    };
    if target_leaf == focus_leaf {
        return false;
    }
    tree.set_focus(target_leaf).is_ok()
}

/// Поменять сфокусированное окно местами с соседом в направлении `dir`.
pub fn swap_direction(tree: &mut Tree, dir: Direction) -> bool {
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };
    let Some(target_branch) = find_neighbor_branch(tree, focus_leaf, dir) else {
        return false;
    };
    let Some(target_leaf) = descend_to_leaf(tree, target_branch) else {
        return false;
    };
    if target_leaf == focus_leaf {
        return false;
    }
    if tree.swap_nodes(focus_leaf, target_leaf).is_ok() {
        let _ = tree.set_focus(focus_leaf);
        true
    } else {
        false
    }
}

/// Переставить сфокусированное окно в направлении `dir` (внутри дерева).
///
/// Если у окна есть сосед в текущем контейнере, оно перемещается (меняется местами с ним).
/// Если окно упирается в край своего контейнера или ориентация контейнера не совпадает
/// с направлением движения, окно поднимается на уровень выше (выходит в родительский контейнер).
pub fn move_direction(tree: &mut Tree, dir: Direction) -> bool {
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };
    let Some(parent) = tree.parent_of(focus_leaf) else {
        return false;
    };
    let Some(parent_container) = tree.get(parent).and_then(|n| n.container()) else {
        return false;
    };
    let Some(idx) = tree.index_in_parent(focus_leaf) else {
        return false;
    };

    // Случай 1: Перемещение внутри текущего контейнера (если сплит совпадает и есть куда двигаться)
    if parent_container.layout == dir.matching_layout() {
        if dir.is_backward() && idx > 0 {
            let sibling = parent_container.children[idx - 1];
            if tree.swap_nodes(focus_leaf, sibling).is_ok() {
                let _ = tree.set_focus(focus_leaf);
                return true;
            }
        } else if dir.is_forward() && idx + 1 < parent_container.children.len() {
            let sibling = parent_container.children[idx + 1];
            if tree.swap_nodes(focus_leaf, sibling).is_ok() {
                let _ = tree.set_focus(focus_leaf);
                return true;
            }
        }
    }

    // Случай 2: Окно упёрлось в край контейнера или ориентация не совпадает — выход на уровень выше
    let Some(grandparent) = tree.parent_of(parent) else {
        // Мы в корневом контейнере и дальше выходить некуда
        return false;
    };
    let Some(parent_idx) = tree.index_in_parent(parent) else {
        return false;
    };
    let Some(key) = tree.get(focus_leaf).and_then(|n| n.window()) else {
        return false;
    };

    // Целевая позиция в контейнере дедушки: до или после родителя
    let insert_index = if dir.is_backward() {
        parent_idx
    } else {
        parent_idx + 1
    };

    // Удаляем окно из текущего контейнера (это также схлопнет вырожденный родитель, если там останется 1 узел)
    if tree.remove_window(key).is_err() {
        return false;
    }

    // Вставляем окно в контейнер дедушки
    tree.insert_window(
        key,
        InsertAt::Into {
            parent: grandparent,
            index: insert_index,
        },
    );
    true
}

/// Изменить долю сфокусированного узла в его контейнере на `delta` (в долях, например 0.05).
///
/// Находит ближайший контейнер-предок с соответствующей ориентацией (`SplitH` для `Left`/`Right`,
/// `SplitV` для `Up`/`Down`). Забирает долю у соседа и отдаёт фокусу (или наоборот при отрицательной дельте),
/// сохраняя сумму долей равной 1.0. Не позволяет доле опускаться ниже [`MIN_CONTAINER_RATIO`].
pub fn resize_focused(tree: &mut Tree, dir: Direction, delta: f64) -> bool {
    if !delta.is_finite() || delta.abs() < 1e-9 {
        return false;
    }
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };

    let matching_layout = dir.matching_layout();

    // Ищем ближайшего предка с подходящей сплит-ориентацией и >1 детей
    let mut curr = focus_leaf;
    let mut target_container = None;
    let mut child_idx_in_container = 0;

    while let Some(parent) = tree.parent_of(curr) {
        if let Some(c) = tree.get(parent).and_then(|n| n.container()) {
            if c.layout == matching_layout && c.children.len() > 1 {
                if let Some(idx) = tree.index_in_parent(curr) {
                    target_container = Some(parent);
                    child_idx_in_container = idx;
                    break;
                }
            }
        }
        curr = parent;
    }

    let Some(container_id) = target_container else {
        return false;
    };

    let Some(c) = tree.get_mut(container_id).and_then(|n| n.container_mut()) else {
        return false;
    };

    let len = c.children.len();
    if len <= 1 {
        return false;
    }

    // Определяем соседа, с которым перераспределяется доля
    let neighbor_idx = if dir.is_forward() {
        if child_idx_in_container + 1 < len {
            child_idx_in_container + 1
        } else if child_idx_in_container > 0 {
            child_idx_in_container - 1
        } else {
            return false;
        }
    } else if child_idx_in_container > 0 {
        child_idx_in_container - 1
    } else if child_idx_in_container + 1 < len {
        child_idx_in_container + 1
    } else {
        return false;
    };

    let cur_focus_ratio = c.ratios[child_idx_in_container];
    let cur_neighbor_ratio = c.ratios[neighbor_idx];

    // Ограничиваем дельту минимальным порогом MIN_CONTAINER_RATIO
    let mut effective_delta = delta;
    if cur_focus_ratio + effective_delta < MIN_CONTAINER_RATIO {
        effective_delta = MIN_CONTAINER_RATIO - cur_focus_ratio;
    }
    if cur_neighbor_ratio - effective_delta < MIN_CONTAINER_RATIO {
        effective_delta = cur_neighbor_ratio - MIN_CONTAINER_RATIO;
    }

    if effective_delta.abs() < 1e-9 {
        return false;
    }

    c.ratios[child_idx_in_container] += effective_delta;
    c.ratios[neighbor_idx] -= effective_delta;
    normalize(&mut c.ratios);
    true
}

/// Сменить ориентацию контейнера, в котором лежит фокус: `SplitH` <-> `SplitV`.
///
/// Если контейнер был группой (`Tabbed`/`Stacked`), переводит его в `SplitH`.
pub fn toggle_split(tree: &mut Tree) -> bool {
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };
    let Some(parent) = tree.parent_of(focus_leaf) else {
        return false;
    };
    let Some(c) = tree.get_mut(parent).and_then(|n| n.container_mut()) else {
        return false;
    };

    c.layout = match c.layout {
        ContainerLayout::SplitH => ContainerLayout::SplitV,
        ContainerLayout::SplitV => ContainerLayout::SplitH,
        ContainerLayout::Tabbed | ContainerLayout::Stacked => ContainerLayout::SplitH,
    };
    true
}

/// Превратить контейнер фокуса в группу (`Tabbed`) и обратно в сплит (`SplitH`).
///
/// При возврате из группы в сплит устанавливается `SplitH` (дефолт i3/sway),
/// при этом сохранённые доли `ratios` остаются без изменений.
pub fn toggle_group(tree: &mut Tree) -> bool {
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };
    let Some(parent) = tree.parent_of(focus_leaf) else {
        return false;
    };
    let Some(c) = tree.get_mut(parent).and_then(|n| n.container_mut()) else {
        return false;
    };

    c.layout = match c.layout {
        ContainerLayout::Tabbed | ContainerLayout::Stacked => ContainerLayout::SplitH,
        ContainerLayout::SplitH | ContainerLayout::SplitV => ContainerLayout::Tabbed,
    };
    true
}

/// Следующий (`forward = true`) или предыдущий (`forward = false`) таб внутри ближайшей группы-предка.
///
/// Переключает `focused_child` группы по кругу и переводит фокус на соответствующий лист.
pub fn cycle_group(tree: &mut Tree, forward: bool) -> bool {
    let Some(focus_leaf) = tree.focus() else {
        return false;
    };

    // Ищем ближайшего предка-группу (Tabbed или Stacked)
    let mut curr = focus_leaf;
    let mut group_container = None;
    while let Some(parent) = tree.parent_of(curr) {
        if let Some(c) = tree.get(parent).and_then(|n| n.container()) {
            if !c.layout.is_split() {
                group_container = Some(parent);
                break;
            }
        }
        curr = parent;
    }

    let Some(group_id) = group_container else {
        return false;
    };

    let Some(c) = tree.get_mut(group_id).and_then(|n| n.container_mut()) else {
        return false;
    };

    let len = c.children.len();
    if len <= 1 {
        return false;
    }

    let next_idx = if forward {
        (c.focused_child + 1) % len
    } else {
        (c.focused_child + len - 1) % len
    };
    c.focused_child = next_idx;
    let target_child = c.children[next_idx];

    let Some(target_leaf) = descend_to_leaf(tree, target_child) else {
        return false;
    };
    tree.set_focus(target_leaf).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::tree::WindowKey;

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    fn ratios(tree: &Tree, id: NodeId) -> Vec<f64> {
        tree.get(id)
            .and_then(|n| n.container())
            .map(|c| c.ratios.clone())
            .unwrap_or_default()
    }

    fn focused_window(tree: &Tree) -> Option<WindowKey> {
        tree.focus()
            .and_then(|f| tree.get(f))
            .and_then(|n| n.window())
    }

    #[test]
    fn focus_direction_in_four_directions_works() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(w1, ContainerLayout::SplitV).unwrap();
        let _w2 = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        let _w3 = t.insert_window(w(3), InsertAt::Root);

        // Дерево: SplitH [ SplitV [ w1, w2 ], w3 ]
        // Сейчас фокус на w3
        assert_eq!(focused_window(&t), Some(w(3)));

        // Движение влево -> переход в SplitV к активному ребёнку (w2)
        assert!(focus_direction(&mut t, Direction::Left));
        assert_eq!(focused_window(&t), Some(w(2)));

        // Движение вверх -> w1
        assert!(focus_direction(&mut t, Direction::Up));
        assert_eq!(focused_window(&t), Some(w(1)));

        // Движение вниз -> w2
        assert!(focus_direction(&mut t, Direction::Down));
        assert_eq!(focused_window(&t), Some(w(2)));

        // Движение вправо -> w3
        assert!(focus_direction(&mut t, Direction::Right));
        assert_eq!(focused_window(&t), Some(w(3)));
    }

    #[test]
    fn focus_hitting_boundary_returns_false() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);

        // Фокус на w(2) (правый край)
        assert!(!focus_direction(&mut t, Direction::Right));
        assert!(!focus_direction(&mut t, Direction::Up));
        assert!(!focus_direction(&mut t, Direction::Down));

        // Фокус на w(1) (левый край)
        assert!(focus_direction(&mut t, Direction::Left));
        assert_eq!(focused_window(&t), Some(w(1)));
        assert!(!focus_direction(&mut t, Direction::Left));
    }

    #[test]
    fn focus_through_nested_container_preserves_focused_child() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(w1, ContainerLayout::SplitV).unwrap();
        let _w2 = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        let _w3 = t.insert_window(w(3), InsertAt::Root);

        // Переводим фокус на w(1) внутри SplitV
        t.set_focus(w1).unwrap();
        assert_eq!(focused_window(&t), Some(w(1)));

        // Уходим вправо на w(3)
        assert!(focus_direction(&mut t, Direction::Right));
        assert_eq!(focused_window(&t), Some(w(3)));

        // Возвращаемся влево: должны попасть в w(1), т.к. он был focused_child
        assert!(focus_direction(&mut t, Direction::Left));
        assert_eq!(focused_window(&t), Some(w(1)));
    }

    #[test]
    fn focus_through_group_skips_internal_tabs() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let group = t.split_leaf(w1, ContainerLayout::Tabbed).unwrap();
        let w2 = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: group,
                index: 1,
            },
        );
        let _w3 = t.insert_window(w(3), InsertAt::Root);

        // Ставим фокус на w(2) внутри табов
        t.set_focus(w2).unwrap();
        assert_eq!(focused_window(&t), Some(w(2)));

        // Фокус вправо выходит из табов к w(3)
        assert!(focus_direction(&mut t, Direction::Right));
        assert_eq!(focused_window(&t), Some(w(3)));

        // Фокус влево возвращается к активному табу w(2)
        assert!(focus_direction(&mut t, Direction::Left));
        assert_eq!(focused_window(&t), Some(w(2)));

        // Вверх/вниз внутри табов не двигаются через focus_direction
        assert!(!focus_direction(&mut t, Direction::Up));
        assert!(!focus_direction(&mut t, Direction::Down));
    }

    #[test]
    fn focus_on_empty_or_single_window_returns_false() {
        let mut empty = Tree::default();
        assert!(!focus_direction(&mut empty, Direction::Left));

        let mut single = Tree::default();
        single.insert_window(w(1), InsertAt::Root);
        assert!(!focus_direction(&mut single, Direction::Left));
        assert!(!focus_direction(&mut single, Direction::Right));
    }

    #[test]
    fn move_within_container_swaps_with_sibling() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        t.insert_window(w(1), InsertAt::Root);
        let w2 = t.insert_window(w(2), InsertAt::Root);
        t.insert_window(w(3), InsertAt::Root);

        // Фокус на w(2)
        t.set_focus(w2).unwrap();
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(2), w(3)]);

        // Двигаем w(2) влево
        assert!(move_direction(&mut t, Direction::Left));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(2), w(1), w(3)]);
        assert_eq!(focused_window(&t), Some(w(2)));

        // Двигаем w(2) вправо дважды
        assert!(move_direction(&mut t, Direction::Right));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(2), w(3)]);
        assert!(move_direction(&mut t, Direction::Right));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(3), w(2)]);
    }

    #[test]
    fn move_hitting_boundary_climbs_out_of_container() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(w1, ContainerLayout::SplitV).unwrap();
        let w2 = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        let _w3 = t.insert_window(w(3), InsertAt::Root);

        // Дерево: SplitH [ SplitV [ w1, w2 ], w3 ]
        // Фокус на w(2)
        t.set_focus(w2).unwrap();

        // Двигаем w(2) вправо: в SplitV вправо двигаться нельзя -> w(2) поднимается в SplitH после cont
        assert!(move_direction(&mut t, Direction::Right));
        // SplitV содержал только w(1), поэтому схлопнулся
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(2), w(3)]);
        assert_eq!(t.children_of(t.root()).len(), 3);
        assert_eq!(focused_window(&t), Some(w(2)));
    }

    #[test]
    fn move_hitting_root_boundary_returns_false() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);

        t.set_focus(w1).unwrap();
        assert!(!move_direction(&mut t, Direction::Left));
        assert!(!move_direction(&mut t, Direction::Up));
        assert!(!move_direction(&mut t, Direction::Down));
    }

    #[test]
    fn move_on_empty_tree_returns_false() {
        let mut t = Tree::default();
        assert!(!move_direction(&mut t, Direction::Left));
    }

    #[test]
    fn swap_direction_exchanges_positions_and_updates_focus() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let _w2 = t.insert_window(w(2), InsertAt::Root);

        t.set_focus(w1).unwrap();
        assert!(swap_direction(&mut t, Direction::Right));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(2), w(1)]);
        assert_eq!(focused_window(&t), Some(w(1)));

        // Упор в край
        assert!(!swap_direction(&mut t, Direction::Right));
    }

    #[test]
    fn swap_direction_on_empty_tree_returns_false() {
        let mut t = Tree::default();
        assert!(!swap_direction(&mut t, Direction::Right));
    }

    #[test]
    fn resize_preserves_sum_of_ratios() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);
        t.insert_window(w(3), InsertAt::Root);

        t.set_focus(w1).unwrap();
        assert!(resize_focused(&mut t, Direction::Right, 0.1));

        let r = ratios(&t, t.root());
        assert_eq!(r.len(), 3);
        let sum: f64 = r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "сумма долей = {sum}");
        assert!(r[0] > 0.33, "первое окно выросло");
    }

    #[test]
    fn resize_hits_minimum_and_clamps() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);

        t.set_focus(w1).unwrap();
        // Пытаемся ужать w1 на 0.6 при начальной доле 0.5 (упрётся в MIN_CONTAINER_RATIO = 0.05)
        assert!(resize_focused(&mut t, Direction::Right, -0.6));

        let r = ratios(&t, t.root());
        assert!(
            (r[0] - MIN_CONTAINER_RATIO).abs() < 1e-9,
            "доля w1 = {}",
            r[0]
        );
        let sum: f64 = r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn resize_at_minimum_returns_false() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);

        t.set_focus(w1).unwrap();
        assert!(resize_focused(&mut t, Direction::Right, -0.6));
        // Повторное сжатие невозможно
        assert!(!resize_focused(&mut t, Direction::Right, -0.1));
    }

    #[test]
    fn resize_through_nested_vertical_parent_finds_horizontal_ancestor() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(w1, ContainerLayout::SplitV).unwrap();
        let _w2 = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        let _w3 = t.insert_window(w(3), InsertAt::Root);

        // w1 внутри SplitV. Ресайз вправо должен найти корневой SplitH и изменить долю cont
        t.set_focus(w1).unwrap();
        assert!(resize_focused(&mut t, Direction::Right, 0.1));

        let root_r = ratios(&t, t.root());
        assert!(root_r[0] > 0.5, "контейнер SplitV вырос по горизонтали");
        let sum: f64 = root_r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn resize_on_empty_or_single_window_returns_false() {
        let mut empty = Tree::default();
        assert!(!resize_focused(&mut empty, Direction::Right, 0.1));

        let mut single = Tree::default();
        single.insert_window(w(1), InsertAt::Root);
        assert!(!resize_focused(&mut single, Direction::Right, 0.1));
    }

    #[test]
    fn toggle_split_switches_between_splith_and_splitv() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        t.insert_window(w(1), InsertAt::Root);

        assert!(toggle_split(&mut t));
        assert_eq!(
            t.get(t.root()).unwrap().container().unwrap().layout,
            ContainerLayout::SplitV
        );

        assert!(toggle_split(&mut t));
        assert_eq!(
            t.get(t.root()).unwrap().container().unwrap().layout,
            ContainerLayout::SplitH
        );
    }

    #[test]
    fn toggle_split_on_empty_tree_returns_false() {
        let mut t = Tree::default();
        assert!(!toggle_split(&mut t));
    }

    #[test]
    fn toggle_group_converts_split_to_tabbed_and_back() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        t.insert_window(w(1), InsertAt::Root);

        assert!(toggle_group(&mut t));
        assert_eq!(
            t.get(t.root()).unwrap().container().unwrap().layout,
            ContainerLayout::Tabbed
        );

        assert!(toggle_group(&mut t));
        assert_eq!(
            t.get(t.root()).unwrap().container().unwrap().layout,
            ContainerLayout::SplitH
        );
    }

    #[test]
    fn toggle_group_on_empty_tree_returns_false() {
        let mut t = Tree::default();
        assert!(!toggle_group(&mut t));
    }

    #[test]
    fn cycle_group_cycles_forward_and_backward() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        let w1 = t.insert_window(w(1), InsertAt::Root);
        let group = t.split_leaf(w1, ContainerLayout::Tabbed).unwrap();
        let _w2 = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: group,
                index: 1,
            },
        );
        let _w3 = t.insert_window(
            w(3),
            InsertAt::Into {
                parent: group,
                index: 2,
            },
        );

        t.set_focus(w1).unwrap();
        assert_eq!(focused_window(&t), Some(w(1)));

        // Цикл вперёд: 1 -> 2 -> 3 -> 1
        assert!(cycle_group(&mut t, true));
        assert_eq!(focused_window(&t), Some(w(2)));
        assert!(cycle_group(&mut t, true));
        assert_eq!(focused_window(&t), Some(w(3)));
        assert!(cycle_group(&mut t, true));
        assert_eq!(focused_window(&t), Some(w(1)));

        // Цикл назад: 1 -> 3 -> 2
        assert!(cycle_group(&mut t, false));
        assert_eq!(focused_window(&t), Some(w(3)));
        assert!(cycle_group(&mut t, false));
        assert_eq!(focused_window(&t), Some(w(2)));
    }

    #[test]
    fn cycle_group_outside_group_returns_false() {
        let mut t = Tree::new(ContainerLayout::SplitH);
        t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);

        assert!(!cycle_group(&mut t, true));
    }

    #[test]
    fn cycle_group_on_empty_tree_returns_false() {
        let mut t = Tree::default();
        assert!(!cycle_group(&mut t, true));
    }
}
