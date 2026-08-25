//! Политики вставки: куда в дереве появляется новое окно (docs/TILING_DESIGN.md §Р4).
//!
//! Три поведения, которые пользователь ждёт от тайлинга, — dwindle (Hyprland),
//! master-stack и ручное управление (i3) — живут поверх ОДНОЙ модели дерева
//! контейнеров и отличаются только ответом на вопрос «куда положить новое
//! окно»: [`plan_insert`] строит план, [`insert`] исполняет его. Это и есть
//! причина, по которой выбрана модель i3 (docs/TILING_DESIGN.md §Р4):
//! политика — слой над деревом, а не отдельная модель раскладки; табы и
//! группы остаются обычными контейнерами при любой политике.
//!
//! План — либо «в существующий контейнер» ([`InsertAt`], его понимает само
//! дерево), либо «сначала разветвить лист» ([`InsertPlan::SplitFocused`]):
//! dwindle делит сфокусированную плитку пополам, master материализует
//! контейнер стека. План несёт [`NodeId`], валидный только внутри того
//! дерева, для которого он построен, — поэтому планы не хранятся дольше
//! одного шага: координатор строит план и сразу исполняет его над тем же
//! деревом.

use serde::{Deserialize, Serialize};

use crate::model::Rect;

use super::tree::{ContainerLayout, InsertAt, NodeId, Tree, WindowKey};

/// Какая политика решает, куда встанет новое окно.
///
/// `Serialize`/`Deserialize` нужны конфигу (`TilingConfig::insert_policy`,
/// `crate::config`); `snake_case` — тот же формат, что у `RuleAction`
/// (`crate::tiling::rules`): в config.json пишется `"dwindle"`, не `"Dwindle"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertPolicy {
    /// Каждое новое окно делит пополам сфокусированную плитку (dwindle).
    Dwindle,
    /// Первое окно — master (первый ребёнок корня), остальные копятся в
    /// стеке (второй ребёнок корня, контейнер `SplitV`). Новое окно всегда
    /// идёт в стек, независимо от того, что в фокусе: master-раскладка не
    /// даёт фокусу перехватывать новые окна.
    ///
    /// Удаление master — операция `ops`, здесь не реализуется. Кто станет
    /// новым master: первый ребёнок стека. После `remove_window` корню
    /// остаётся один ребёнок — сам стек, и `ops` должен извлечь его первое
    /// окно на место master (root.children[0]), ужав стек до оставшихся
    /// окон; пустой стек при этом растворится сам (`collapse_if_degenerate`).
    Master,
    /// Новое окно встаёт соседом сфокусированного в том же контейнере
    /// (ручное управление, i3).
    Manual,
}

/// План вставки: либо просто в существующий контейнер, либо сначала
/// разветвить сфокусированный лист.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertPlan {
    /// Вставить без разветвления — [`InsertAt`] понятен самому дереву.
    At(InsertAt),
    /// Обернуть лист `leaf` в новый контейнер `layout` и вставить окно в него
    /// вторым ребёнком (первый — `leaf`, он делится пополам).
    SplitFocused {
        leaf: NodeId,
        layout: ContainerLayout,
    },
}

/// Куда встанет новое окно при политике `policy`.
///
/// `focused_rect` — прямоугольник сфокусированной плитки, если он известен.
/// Нужен только [`InsertPolicy::Dwindle`]: направление деления выбирается по
/// соотношению сторон (широкую плитку делим вертикальной линией, высокую —
/// горизонтальной). `None` — падаем на чередование по глубине дерева.
pub fn plan_insert(tree: &Tree, policy: InsertPolicy, focused_rect: Option<Rect>) -> InsertPlan {
    match policy {
        InsertPolicy::Dwindle => plan_dwindle(tree, focused_rect),
        InsertPolicy::Master => plan_master(tree),
        InsertPolicy::Manual => plan_manual(tree),
    }
}

/// Выполнить план и вернуть id нового листа.
///
/// При [`InsertPlan::SplitFocused`] — [`Tree::split_leaf`], затем вставка
/// окна в получившийся контейнер вторым ребёнком. Фокус после вставки стоит
/// на НОВОМ окне — это делает сам `Tree::insert_window`.
///
/// План обязан происходить из [`plan_insert`] над ЭТИМ ЖЕ деревом:
/// `SplitFocused` несёт `NodeId` живого листа, иначе `split_leaf` ошибётся.
pub fn insert(tree: &mut Tree, key: WindowKey, plan: InsertPlan) -> NodeId {
    match plan {
        InsertPlan::At(at) => tree.insert_window(key, at),
        InsertPlan::SplitFocused { leaf, layout } => {
            let container = tree
                .split_leaf(leaf, layout)
                .expect("план построен plan_insert по живому листу того же дерева");
            tree.insert_window(
                key,
                InsertAt::Into {
                    parent: container,
                    index: 1,
                },
            )
        }
    }
}

fn plan_dwindle(tree: &Tree, focused_rect: Option<Rect>) -> InsertPlan {
    let Some(leaf) = tree.focus() else {
        // Фокуса нет — дерево пустое (инвариант Tree), разветвлять нечего.
        return InsertPlan::At(InsertAt::Root);
    };
    let layout = match focused_rect {
        // Широкую плитку делим вертикальной линией (дети в ряд — SplitH),
        // высокую — горизонтальной (SplitV). Квадрат честно уходит в SplitV:
        // «иначе» из спеки.
        Some(rect) if rect.w > rect.h => ContainerLayout::SplitH,
        Some(_) => ContainerLayout::SplitV,
        // Без геометрии — чередование по чётности глубины: корень по
        // умолчанию SplitH, поэтому лист нечётной глубины делится SplitV,
        // чётной — SplitH, и вложенные сплиты не сливаются в один ряд.
        None => {
            let depth = tree.ancestors(leaf).len();
            if depth % 2 == 0 {
                ContainerLayout::SplitH
            } else {
                ContainerLayout::SplitV
            }
        }
    };
    InsertPlan::SplitFocused { leaf, layout }
}

fn plan_master(tree: &Tree) -> InsertPlan {
    let children = tree.children_of(tree.root());
    if children.len() <= 1 {
        // 0 окон: первое окно становится master (первый ребёнок корня).
        // 1 окно: второе открывает стек-регион — второй ребёнок корня; сам
        // SplitV-контейнер стека материализуется третьим окном (см. ниже).
        return InsertPlan::At(InsertAt::Root);
    }
    let second = children[1];
    if let Some(stack) = tree.get(second).and_then(|n| n.container()) {
        // Стек уже контейнер: окно идёт в конец стека, независимо от фокуса
        // (в т.ч. когда в фокусе master).
        return InsertPlan::At(InsertAt::Into {
            parent: second,
            index: stack.children.len(),
        });
    }
    if children.len() == 2 {
        // Стек-регион пока одно окно: обернуть его в SplitV-контейнер —
        // «второе окно создаёт стек», третье уже уйдёт внутрь него, а не
        // соседом master на уровне корня.
        return InsertPlan::SplitFocused {
            leaf: second,
            layout: ContainerLayout::SplitV,
        };
    }
    // Деградировавшее дерево (корень не в master-форме — например, три
    // окна подряд в корне после Manual): конец корня — дальше от master,
    // чем любой из существующих детей.
    InsertPlan::At(InsertAt::Root)
}

fn plan_manual(tree: &Tree) -> InsertPlan {
    match tree.focus() {
        Some(focus) => InsertPlan::At(InsertAt::After(focus)),
        None => InsertPlan::At(InsertAt::Root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    fn rect(w: u32, h: u32) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    fn insert_planned(
        tree: &mut Tree,
        n: u64,
        policy: InsertPolicy,
        focused_rect: Option<Rect>,
    ) -> NodeId {
        let plan = plan_insert(tree, policy, focused_rect);
        insert(tree, w(n), plan)
    }

    fn layout_of(tree: &Tree, id: NodeId) -> ContainerLayout {
        tree.get(id).unwrap().container().unwrap().layout
    }

    fn ratios(tree: &Tree, id: NodeId) -> Vec<f64> {
        tree.get(id).unwrap().container().unwrap().ratios.clone()
    }

    #[test]
    fn dwindle_inserts_into_empty_tree_at_root() {
        let mut t = Tree::default();
        let leaf = insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        assert_eq!(t.children_of(t.root()), &[leaf]);
        assert_eq!(t.focus(), Some(leaf));
    }

    #[test]
    fn master_inserts_into_empty_tree_at_root() {
        let mut t = Tree::default();
        let leaf = insert_planned(&mut t, 1, InsertPolicy::Master, None);
        assert_eq!(t.children_of(t.root()), &[leaf]);
        assert_eq!(t.focus(), Some(leaf));
    }

    #[test]
    fn manual_inserts_into_empty_tree_at_root() {
        let mut t = Tree::default();
        let leaf = insert_planned(&mut t, 1, InsertPolicy::Manual, None);
        assert_eq!(t.children_of(t.root()), &[leaf]);
        assert_eq!(t.focus(), Some(leaf));
    }

    #[test]
    fn dwindle_splits_wide_tile_with_vertical_divider() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        let plan = plan_insert(&t, InsertPolicy::Dwindle, Some(rect(800, 600)));
        assert_eq!(
            plan,
            InsertPlan::SplitFocused {
                leaf: a,
                layout: ContainerLayout::SplitH,
            },
            "широкая плитка делится вертикальной линией"
        );
        let b = insert(&mut t, w(2), plan);
        let cont = t.parent_of(a).unwrap();
        assert_eq!(t.children_of(cont), &[a, b]);
        assert_eq!(layout_of(&t, cont), ContainerLayout::SplitH);
    }

    #[test]
    fn dwindle_splits_tall_tile_with_horizontal_divider() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        let plan = plan_insert(&t, InsertPolicy::Dwindle, Some(rect(400, 600)));
        assert_eq!(
            plan,
            InsertPlan::SplitFocused {
                leaf: a,
                layout: ContainerLayout::SplitV,
            },
            "высокая плитка делится горизонтальной линией"
        );
        let b = insert(&mut t, w(2), plan);
        assert_eq!(
            layout_of(&t, t.parent_of(b).unwrap()),
            ContainerLayout::SplitV
        );
    }

    #[test]
    fn dwindle_square_tile_falls_back_to_vertical_divider() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        let plan = plan_insert(&t, InsertPolicy::Dwindle, Some(rect(600, 600)));
        assert_eq!(
            plan,
            InsertPlan::SplitFocused {
                leaf: a,
                layout: ContainerLayout::SplitV,
            },
            "«иначе» из спеки: не-широкая плитка делится горизонтальной линией"
        );
    }

    #[test]
    fn dwindle_without_rect_alternates_by_leaf_depth() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        let b = insert_planned(&mut t, 2, InsertPolicy::Dwindle, None);
        let _c = insert_planned(&mut t, 3, InsertPolicy::Dwindle, None);
        // Глубина a при разветвлении была 1 → SplitV; глубина b = 2 → SplitH.
        // Уровни чередуются, вложенность не сливается в один ряд.
        assert_eq!(
            layout_of(&t, t.parent_of(a).unwrap()),
            ContainerLayout::SplitV
        );
        assert_eq!(
            layout_of(&t, t.parent_of(b).unwrap()),
            ContainerLayout::SplitH
        );
        assert_ne!(t.parent_of(a), t.parent_of(b), "вложенность, а не один ряд");
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(2), w(3)]);
    }

    #[test]
    fn dwindle_insert_halves_the_focused_leaf() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        let b = insert_planned(&mut t, 2, InsertPolicy::Dwindle, Some(rect(800, 600)));
        let cont = t.parent_of(a).unwrap();
        assert_eq!(t.children_of(cont), &[a, b]);
        assert_eq!(
            t.children_of(t.root()),
            &[cont],
            "контейнер встал на место листа"
        );
        assert_eq!(t.focus(), Some(b), "фокус на новом окне");
    }

    #[test]
    fn dwindle_plan_on_empty_tree_ignores_rect() {
        let t = Tree::default();
        assert_eq!(
            plan_insert(&t, InsertPolicy::Dwindle, Some(rect(10, 10))),
            InsertPlan::At(InsertAt::Root),
            "пустому дереву нечего делить"
        );
    }

    #[test]
    fn master_first_window_becomes_master() {
        let mut t = Tree::default();
        let m = insert_planned(&mut t, 1, InsertPolicy::Master, None);
        assert_eq!(t.children_of(t.root()), &[m]);
        assert_eq!(
            t.index_in_parent(m),
            Some(0),
            "master — первый ребёнок корня"
        );
    }

    #[test]
    fn master_second_window_opens_the_stack_region() {
        let mut t = Tree::default();
        let m = insert_planned(&mut t, 1, InsertPolicy::Master, None);
        let s = insert_planned(&mut t, 2, InsertPolicy::Master, None);
        assert_eq!(
            t.children_of(t.root()),
            &[m, s],
            "стек-регион — второй ребёнок корня"
        );
        assert_eq!(t.parent_of(m), Some(t.root()));
    }

    #[test]
    fn master_third_window_goes_into_stack_not_next_to_master() {
        let mut t = Tree::default();
        let m = insert_planned(&mut t, 1, InsertPolicy::Master, None);
        let s2 = insert_planned(&mut t, 2, InsertPolicy::Master, None);
        let s3 = insert_planned(&mut t, 3, InsertPolicy::Master, None);
        let stack = t.parent_of(s3).unwrap();
        assert_ne!(
            stack,
            t.root(),
            "третье окно не сосед master на уровне корня"
        );
        assert_eq!(
            t.children_of(t.root()),
            &[m, stack],
            "корень: [master, стек]"
        );
        assert_eq!(layout_of(&t, stack), ContainerLayout::SplitV);
        assert_eq!(
            t.children_of(stack),
            &[s2, s3],
            "стек растёт из второго окна"
        );
        assert_eq!(
            t.parent_of(m),
            Some(t.root()),
            "master не тронут разветвлением"
        );
    }

    #[test]
    fn master_focus_on_master_still_inserts_into_stack() {
        let mut t = Tree::default();
        let m = insert_planned(&mut t, 1, InsertPolicy::Master, None);
        let s2 = insert_planned(&mut t, 2, InsertPolicy::Master, None);
        let s3 = insert_planned(&mut t, 3, InsertPolicy::Master, None);
        t.set_focus(m).unwrap();
        let s4 = insert_planned(&mut t, 4, InsertPolicy::Master, None);
        let stack = t.parent_of(s3).unwrap();
        assert_eq!(
            t.parent_of(s4),
            Some(stack),
            "фокус на master не перехватывает окно"
        );
        assert_eq!(t.children_of(stack), &[s2, s3, s4], "в конец стека");
    }

    #[test]
    fn master_plan_degrades_to_root_for_deviated_roots() {
        let mut t = Tree::default();
        for i in 1..=3 {
            insert_planned(&mut t, i, InsertPolicy::Manual, None);
        }
        assert_eq!(
            plan_insert(&t, InsertPolicy::Master, None),
            InsertPlan::At(InsertAt::Root),
            "три окна в корне — не master-форма, но вставка не ломается"
        );
    }

    #[test]
    fn manual_inserts_next_to_focused_window() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Manual, None);
        insert_planned(&mut t, 2, InsertPolicy::Manual, None);
        t.set_focus(a).unwrap();
        let c = insert_planned(&mut t, 3, InsertPolicy::Manual, None);
        assert_eq!(
            t.index_in_parent(c),
            Some(1),
            "сразу после сфокусированного"
        );
        assert_eq!(t.parent_of(c), t.parent_of(a), "в том же контейнере");
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(3), w(2)]);
    }

    #[test]
    fn insert_sets_focus_to_the_new_window() {
        let mut t = Tree::default();
        let a = insert_planned(&mut t, 1, InsertPolicy::Dwindle, Some(rect(800, 600)));
        let b = insert_planned(&mut t, 2, InsertPolicy::Dwindle, Some(rect(500, 900)));
        assert_eq!(t.focus(), Some(b));
        assert_eq!(t.get(b).unwrap().window(), Some(w(2)));
        assert_ne!(t.focus(), Some(a));
        let stack = t.parent_of(t.focus().unwrap()).unwrap();
        assert_eq!(
            t.get(stack).unwrap().container().unwrap().focused_child,
            1,
            "focused_child по пути к корню указывает на новое окно"
        );
    }

    #[test]
    fn ratios_sum_to_one_in_every_container_after_mixed_inserts() {
        let mut t = Tree::default();
        insert_planned(&mut t, 1, InsertPolicy::Dwindle, None);
        insert_planned(&mut t, 2, InsertPolicy::Dwindle, Some(rect(800, 600)));
        insert_planned(&mut t, 3, InsertPolicy::Master, None);
        insert_planned(&mut t, 4, InsertPolicy::Manual, None);
        let mut ids = vec![t.root()];
        while let Some(id) = ids.pop() {
            let Some(c) = t.get(id).and_then(|n| n.container()) else {
                continue;
            };
            let sum: f64 = c.ratios.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-9,
                "контейнер {id:?}: сумма долей {sum}"
            );
            assert_eq!(c.ratios.len(), c.children.len());
            ids.extend(c.children.iter().copied());
        }
        let _ = ratios(&t, t.root());
    }
}
