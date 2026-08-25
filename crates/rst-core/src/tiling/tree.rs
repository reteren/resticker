//! Дерево контейнеров: узлы, вставка, удаление, схлопывание, фокус
//! (docs/TILING_DESIGN.md §Р4).
//!
//! Арена (`Vec<Option<Node>>` + список свободных слотов), а не `Rc<RefCell<_>>`:
//! дерево сериализуется в конфиг как есть, копируется дёшево и не требует
//! владения по ссылкам, а операции над ним (`ops`) — обычные функции, которые
//! легко тестировать.
//!
//! Геометрию это дерево не считает — это дело [`super::layout`]. Здесь только
//! структура и инварианты.

use serde::{Deserialize, Serialize};

/// Ключ окна, непрозрачный для этого крейта.
///
/// Координатор кладёт сюда `HWND as u64`; крейту всё равно, что внутри, —
/// важно лишь, что ключ уникален и сравним. Так `rst-core` остаётся
/// платформенно-чистым.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WindowKey(pub u64);

/// Индекс узла в арене. Валиден только внутри своего [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

/// Как контейнер раскладывает своих детей.
///
/// `Tabbed`/`Stacked` — это и есть «группы» из требований пользователя: дети
/// занимают ОДИН прямоугольник, виден только сфокусированный
/// (docs/TILING_DESIGN.md §Р3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerLayout {
    /// Дети в ряд слева направо.
    SplitH,
    /// Дети в столбец сверху вниз.
    SplitV,
    /// Один прямоугольник на всех, полоса табов сверху.
    Tabbed,
    /// Один прямоугольник на всех, заголовки стопкой.
    Stacked,
}

impl ContainerLayout {
    /// Контейнер показывает всех детей сразу (а не одного активного)?
    ///
    /// Разделение важно и для геометрии, и для скрытия окон: у `Tabbed`/
    /// `Stacked` неактивные дети физически прячутся (cloak), у сплитов — нет.
    pub fn is_split(self) -> bool {
        matches!(self, Self::SplitH | Self::SplitV)
    }
}

/// Контейнер — внутренний узел дерева.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub layout: ContainerLayout,
    /// Дети в порядке раскладки.
    pub children: Vec<NodeId>,
    /// Доли детей, параллельно `children`, в сумме 1.0.
    ///
    /// Хранятся и для `Tabbed`/`Stacked`, где геометрию не задают: иначе
    /// переключение группы обратно в сплит теряло бы пропорции, которые
    /// пользователь настраивал руками.
    pub ratios: Vec<f64>,
    /// Индекс активного ребёнка в `children`.
    ///
    /// Для табов — видимый таб; для сплитов — куда вернуть фокус, когда он
    /// приходит в этот контейнер «снаружи» (поведение i3).
    pub focused_child: usize,
}

/// Узел дерева: либо окно (лист), либо контейнер.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Window(WindowKey),
    Container(Container),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// `None` только у корня.
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
}

impl Node {
    pub fn window(&self) -> Option<WindowKey> {
        match &self.kind {
            NodeKind::Window(w) => Some(*w),
            NodeKind::Container(_) => None,
        }
    }

    pub fn container(&self) -> Option<&Container> {
        match &self.kind {
            NodeKind::Container(c) => Some(c),
            NodeKind::Window(_) => None,
        }
    }

    pub fn container_mut(&mut self) -> Option<&mut Container> {
        match &mut self.kind {
            NodeKind::Container(c) => Some(c),
            NodeKind::Window(_) => None,
        }
    }
}

/// Куда вставить новое окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertAt {
    /// Последним ребёнком корня. Пустое дерево или «просто добавь куда-нибудь».
    Root,
    /// Сразу после узла `sibling`, в его же контейнере. Обычный случай:
    /// новое окно появляется рядом с текущим фокусом.
    After(NodeId),
    /// В контейнер `parent` на позицию `index` (насыщается длиной).
    Into { parent: NodeId, index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("узла нет в дереве")]
    NoSuchNode,
    #[error("узел не контейнер")]
    NotAContainer,
    #[error("узел не окно")]
    NotAWindow,
    #[error("корень удалить нельзя")]
    CannotRemoveRoot,
}

/// Дерево одного воркспейса.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    nodes: Vec<Option<Node>>,
    /// Освободившиеся слоты арены — переиспользуются при вставке.
    free: Vec<NodeId>,
    root: NodeId,
    /// Сфокусированный ЛИСТ. `None` — в дереве нет окон.
    focus: Option<NodeId>,
}

impl Default for Tree {
    fn default() -> Self {
        Self::new(ContainerLayout::SplitH)
    }
}

impl Tree {
    /// Пустое дерево: один корневой контейнер без детей.
    pub fn new(root_layout: ContainerLayout) -> Self {
        let root = Node {
            parent: None,
            kind: NodeKind::Container(Container {
                layout: root_layout,
                children: Vec::new(),
                ratios: Vec::new(),
                focused_child: 0,
            }),
        };
        Self {
            nodes: vec![Some(root)],
            free: Vec::new(),
            root: NodeId(0),
            focus: None,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn focus(&self) -> Option<NodeId> {
        self.focus
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0 as usize).and_then(|s| s.as_ref())
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id.0 as usize).and_then(|s| s.as_mut())
    }

    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.get(id).and_then(|n| n.parent)
    }

    pub fn children_of(&self, id: NodeId) -> &[NodeId] {
        self.get(id)
            .and_then(|n| n.container())
            .map_or(&[], |c| c.children.as_slice())
    }

    /// Позиция узла среди детей его родителя.
    pub fn index_in_parent(&self, id: NodeId) -> Option<usize> {
        let parent = self.parent_of(id)?;
        self.children_of(parent).iter().position(|c| *c == id)
    }

    /// В дереве нет ни одного окна.
    pub fn is_empty(&self) -> bool {
        self.windows().next().is_none()
    }

    /// Все листья-окна в порядке обхода слева направо.
    pub fn leaves(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.collect_leaves(self.root, &mut out);
        out
    }

    fn collect_leaves(&self, id: NodeId, out: &mut Vec<NodeId>) {
        match self.get(id).map(|n| &n.kind) {
            Some(NodeKind::Window(_)) => out.push(id),
            Some(NodeKind::Container(c)) => {
                for child in &c.children {
                    self.collect_leaves(*child, out);
                }
            }
            None => {}
        }
    }

    /// Все окна дерева в том же порядке, что и [`Self::leaves`].
    pub fn windows(&self) -> impl Iterator<Item = WindowKey> + '_ {
        self.leaves()
            .into_iter()
            .filter_map(move |id| self.get(id).and_then(|n| n.window()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Лист с этим окном.
    pub fn find_window(&self, key: WindowKey) -> Option<NodeId> {
        self.leaves()
            .into_iter()
            .find(|id| self.get(*id).and_then(|n| n.window()) == Some(key))
    }

    /// Цепочка предков от родителя `id` до корня.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = self.parent_of(id);
        while let Some(p) = cur {
            out.push(p);
            cur = self.parent_of(p);
        }
        out
    }

    /// Вставить окно. Возвращает id нового листа; фокус переходит на него.
    ///
    /// Вставка в лист (`After` на листе) кладёт окно СОСЕДОМ в контейнер этого
    /// листа — само по себе дерево не разветвляется. Разветвление (обернуть
    /// лист в новый контейнер) — решение политики вставки, см.
    /// [`Self::split_leaf`] и `policy`.
    pub fn insert_window(&mut self, key: WindowKey, at: InsertAt) -> NodeId {
        let (parent, index) = match at {
            InsertAt::Root => (self.root, self.children_of(self.root).len()),
            InsertAt::After(sibling) => match self.parent_of(sibling) {
                Some(p) => (p, self.index_in_parent(sibling).map_or(0, |i| i + 1)),
                // `sibling` — корень: единственное осмысленное место внутри.
                None => (self.root, self.children_of(self.root).len()),
            },
            InsertAt::Into { parent, index } => {
                let len = self.children_of(parent).len();
                (parent, index.min(len))
            }
        };

        let leaf = self.alloc(Node {
            parent: Some(parent),
            kind: NodeKind::Window(key),
        });
        self.attach(parent, leaf, index);
        self.focus = Some(leaf);
        self.sync_focus_path(leaf);
        leaf
    }

    /// Обернуть лист в новый контейнер и вернуть id этого контейнера.
    ///
    /// Нужно политикам вставки (dwindle делит лист пополам) и операции
    /// «сгруппировать»: контейнер встаёт на место листа, лист становится его
    /// единственным ребёнком с долей 1.0.
    pub fn split_leaf(
        &mut self,
        leaf: NodeId,
        layout: ContainerLayout,
    ) -> Result<NodeId, TreeError> {
        let parent = self.parent_of(leaf).ok_or(TreeError::CannotRemoveRoot)?;
        let index = self.index_in_parent(leaf).ok_or(TreeError::NoSuchNode)?;
        let ratio = self
            .get(parent)
            .and_then(|n| n.container())
            .and_then(|c| c.ratios.get(index).copied())
            .unwrap_or(1.0);

        let container = self.alloc(Node {
            parent: Some(parent),
            kind: NodeKind::Container(Container {
                layout,
                children: vec![leaf],
                ratios: vec![1.0],
                focused_child: 0,
            }),
        });

        // Контейнер занимает место листа с ТОЙ ЖЕ долей — иначе соседи
        // дёрнулись бы при обычном разветвлении.
        if let Some(c) = self.get_mut(parent).and_then(|n| n.container_mut()) {
            c.children[index] = container;
            c.ratios[index] = ratio;
        }
        if let Some(n) = self.get_mut(leaf) {
            n.parent = Some(container);
        }
        Ok(container)
    }

    /// Удалить лист с окном. `Ok(false)` — такого окна в дереве не было.
    ///
    /// После удаления контейнер, у которого остался ОДИН ребёнок,
    /// растворяется в родителе (поведение i3): иначе дерево копило бы
    /// вырожденные узлы, а `toggle_split` начал бы менять слой, которого
    /// пользователь не видит.
    pub fn remove_window(&mut self, key: WindowKey) -> Result<bool, TreeError> {
        let Some(leaf) = self.find_window(key) else {
            return Ok(false);
        };
        self.remove_node(leaf)?;
        Ok(true)
    }

    /// Удалить узел (лист или поддерево) вместе с потомками.
    pub fn remove_node(&mut self, id: NodeId) -> Result<(), TreeError> {
        if id == self.root {
            return Err(TreeError::CannotRemoveRoot);
        }
        let parent = self.parent_of(id).ok_or(TreeError::NoSuchNode)?;
        let index = self.index_in_parent(id).ok_or(TreeError::NoSuchNode)?;

        self.detach(parent, index);
        self.free_subtree(id);
        self.collapse_if_degenerate(parent);
        self.repair_focus();
        Ok(())
    }

    /// Поменять местами два узла вместе с поддеревьями (операция swap).
    ///
    /// Доли остаются за ПОЗИЦИЯМИ, а не за узлами: пользователь меняет местами
    /// содержимое плиток, а не их размеры.
    pub fn swap_nodes(&mut self, a: NodeId, b: NodeId) -> Result<(), TreeError> {
        if a == b {
            return Ok(());
        }
        if a == self.root || b == self.root {
            return Err(TreeError::CannotRemoveRoot);
        }
        // Обмен предка с потомком развалил бы дерево в цикл.
        if self.ancestors(a).contains(&b) || self.ancestors(b).contains(&a) {
            return Err(TreeError::NoSuchNode);
        }
        let (pa, ia) = (
            self.parent_of(a).ok_or(TreeError::NoSuchNode)?,
            self.index_in_parent(a).ok_or(TreeError::NoSuchNode)?,
        );
        let (pb, ib) = (
            self.parent_of(b).ok_or(TreeError::NoSuchNode)?,
            self.index_in_parent(b).ok_or(TreeError::NoSuchNode)?,
        );

        if let Some(c) = self.get_mut(pa).and_then(|n| n.container_mut()) {
            c.children[ia] = b;
        }
        if let Some(c) = self.get_mut(pb).and_then(|n| n.container_mut()) {
            c.children[ib] = a;
        }
        if let Some(n) = self.get_mut(a) {
            n.parent = Some(pb);
        }
        if let Some(n) = self.get_mut(b) {
            n.parent = Some(pa);
        }
        Ok(())
    }

    /// Сфокусировать лист и обновить `focused_child` у всех его предков.
    pub fn set_focus(&mut self, leaf: NodeId) -> Result<(), TreeError> {
        if self.get(leaf).is_none() {
            return Err(TreeError::NoSuchNode);
        }
        self.focus = Some(leaf);
        self.sync_focus_path(leaf);
        Ok(())
    }

    /// Проставить `focused_child` по пути от `leaf` к корню.
    fn sync_focus_path(&mut self, leaf: NodeId) {
        let mut child = leaf;
        while let Some(parent) = self.parent_of(child) {
            if let Some(idx) = self.children_of(parent).iter().position(|c| *c == child)
                && let Some(c) = self.get_mut(parent).and_then(|n| n.container_mut())
            {
                c.focused_child = idx;
            }
            child = parent;
        }
    }

    /// Фокус мог указывать на удалённый узел — увести его на ближайший живой лист.
    fn repair_focus(&mut self) {
        let alive = self.focus.is_some_and(|f| self.get(f).is_some());
        if alive {
            return;
        }
        self.focus = self.leaves().first().copied();
        if let Some(f) = self.focus {
            self.sync_focus_path(f);
        }
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        if let Some(id) = self.free.pop() {
            self.nodes[id.0 as usize] = Some(node);
            return id;
        }
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Some(node));
        id
    }

    /// Вставить уже созданный узел ребёнком, пересчитав доли.
    fn attach(&mut self, parent: NodeId, child: NodeId, index: usize) {
        let Some(c) = self.get_mut(parent).and_then(|n| n.container_mut()) else {
            return;
        };
        let n_after = c.children.len() + 1;
        let share = 1.0 / n_after as f64;
        // Старые доли ужимаются пропорционально, новая берёт равную часть —
        // так вставка не перекраивает пропорции, настроенные пользователем.
        for r in &mut c.ratios {
            *r *= 1.0 - share;
        }
        c.children.insert(index, child);
        c.ratios.insert(index, share);
        c.focused_child = index;
        normalize(&mut c.ratios);
    }

    fn detach(&mut self, parent: NodeId, index: usize) {
        let Some(c) = self.get_mut(parent).and_then(|n| n.container_mut()) else {
            return;
        };
        c.children.remove(index);
        c.ratios.remove(index);
        normalize(&mut c.ratios);
        c.focused_child = c.focused_child.min(c.children.len().saturating_sub(1));
    }

    /// Контейнер с одним ребёнком (и не корень) заменяется этим ребёнком.
    fn collapse_if_degenerate(&mut self, container: NodeId) {
        if container == self.root {
            return;
        }
        let children = self.children_of(container).to_vec();
        let Some(parent) = self.parent_of(container) else {
            return;
        };
        let Some(index) = self.index_in_parent(container) else {
            return;
        };

        match children.len() {
            // Пустой контейнер бессмыслен — убираем целиком и проверяем деда.
            0 => {
                self.detach(parent, index);
                self.nodes[container.0 as usize] = None;
                self.free.push(container);
                self.collapse_if_degenerate(parent);
            }
            1 => {
                let only = children[0];
                if let Some(c) = self.get_mut(parent).and_then(|n| n.container_mut()) {
                    c.children[index] = only;
                }
                if let Some(n) = self.get_mut(only) {
                    n.parent = Some(parent);
                }
                self.nodes[container.0 as usize] = None;
                self.free.push(container);
            }
            _ => {}
        }
    }

    fn free_subtree(&mut self, id: NodeId) {
        let children = self.children_of(id).to_vec();
        for child in children {
            self.free_subtree(child);
        }
        if self.nodes.get(id.0 as usize).is_some_and(|s| s.is_some()) {
            self.nodes[id.0 as usize] = None;
            self.free.push(id);
        }
    }
}

/// Привести доли к сумме 1.0. Пустой или вырожденный набор — равные доли.
pub(crate) fn normalize(ratios: &mut [f64]) {
    if ratios.is_empty() {
        return;
    }
    let sum: f64 = ratios.iter().filter(|r| r.is_finite() && **r > 0.0).sum();
    if !sum.is_finite() || sum <= 0.0 {
        let equal = 1.0 / ratios.len() as f64;
        ratios.fill(equal);
        return;
    }
    for r in ratios.iter_mut() {
        if !r.is_finite() || *r <= 0.0 {
            *r = 0.0;
        }
        *r /= sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    fn ratios(tree: &Tree, id: NodeId) -> Vec<f64> {
        tree.get(id)
            .and_then(|n| n.container())
            .map(|c| c.ratios.clone())
            .unwrap_or_default()
    }

    #[test]
    fn new_tree_has_empty_root_container() {
        let t = Tree::new(ContainerLayout::SplitH);
        assert!(t.is_empty());
        assert!(t.focus().is_none());
        assert!(t.children_of(t.root()).is_empty());
        assert!(t.get(t.root()).unwrap().container().is_some());
    }

    #[test]
    fn first_insert_becomes_focus() {
        let mut t = Tree::default();
        let leaf = t.insert_window(w(1), InsertAt::Root);
        assert_eq!(t.focus(), Some(leaf));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1)]);
    }

    #[test]
    fn insert_after_puts_window_next_to_sibling() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(3), InsertAt::Root);
        t.insert_window(w(2), InsertAt::After(a));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1), w(2), w(3)]);
    }

    #[test]
    fn ratios_stay_normalized_after_inserts() {
        let mut t = Tree::default();
        for i in 0..5 {
            t.insert_window(w(i), InsertAt::Root);
        }
        let r = ratios(&t, t.root());
        assert_eq!(r.len(), 5);
        let sum: f64 = r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "сумма долей = {sum}");
        for share in r {
            assert!((share - 0.2).abs() < 1e-9, "равные вставки — равные доли");
        }
    }

    #[test]
    fn removing_window_renormalizes_ratios() {
        let mut t = Tree::default();
        for i in 0..4 {
            t.insert_window(w(i), InsertAt::Root);
        }
        assert!(t.remove_window(w(2)).unwrap());
        let r = ratios(&t, t.root());
        assert_eq!(r.len(), 3);
        let sum: f64 = r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn removing_unknown_window_is_not_an_error() {
        let mut t = Tree::default();
        t.insert_window(w(1), InsertAt::Root);
        assert!(!t.remove_window(w(42)).unwrap());
        assert_eq!(t.windows().count(), 1);
    }

    #[test]
    fn last_window_leaves_empty_root() {
        let mut t = Tree::default();
        t.insert_window(w(1), InsertAt::Root);
        assert!(t.remove_window(w(1)).unwrap());
        assert!(t.is_empty());
        assert!(t.focus().is_none());
        assert_eq!(t.root(), NodeId(0), "корень переживает опустошение");
    }

    #[test]
    fn split_leaf_wraps_it_in_a_container() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(a, ContainerLayout::SplitV).unwrap();
        assert_eq!(t.parent_of(a), Some(cont));
        assert_eq!(t.parent_of(cont), Some(t.root()));
        assert_eq!(t.children_of(t.root()), &[cont]);
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(1)]);
    }

    #[test]
    fn split_keeps_the_share_of_the_leaf_it_replaced() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        t.insert_window(w(2), InsertAt::Root);
        let before = ratios(&t, t.root());
        t.split_leaf(a, ContainerLayout::SplitV).unwrap();
        assert_eq!(ratios(&t, t.root()), before, "соседи не должны дёргаться");
    }

    #[test]
    fn container_with_one_child_dissolves_on_removal() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(a, ContainerLayout::SplitV).unwrap();
        let b = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        assert_eq!(t.parent_of(b), Some(cont));

        assert!(t.remove_window(w(2)).unwrap());
        // Контейнер остался бы вырожденным — он растворяется, лист поднимается.
        assert_eq!(t.parent_of(a), Some(t.root()));
        assert!(t.get(cont).is_none(), "вырожденный контейнер удалён");
    }

    #[test]
    fn focus_moves_to_a_live_leaf_when_the_focused_one_dies() {
        let mut t = Tree::default();
        t.insert_window(w(1), InsertAt::Root);
        let b = t.insert_window(w(2), InsertAt::Root);
        assert_eq!(t.focus(), Some(b));
        assert!(t.remove_window(w(2)).unwrap());
        let focus = t.focus().expect("фокус обязан переехать, а не исчезнуть");
        assert_eq!(t.get(focus).unwrap().window(), Some(w(1)));
    }

    #[test]
    fn focused_child_is_synced_along_the_path() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(a, ContainerLayout::Tabbed).unwrap();
        let b = t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        t.set_focus(a).unwrap();

        let c = t.get(cont).unwrap().container().unwrap();
        assert_eq!(c.focused_child, 0, "активный таб — сфокусированный");
        t.set_focus(b).unwrap();
        let c = t.get(cont).unwrap().container().unwrap();
        assert_eq!(c.focused_child, 1);
    }

    #[test]
    fn swap_exchanges_positions_but_not_shares() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        let b = t.insert_window(w(2), InsertAt::Root);
        if let Some(c) = t.get_mut(t.root()).and_then(|n| n.container_mut()) {
            c.ratios = vec![0.7, 0.3];
        }
        t.swap_nodes(a, b).unwrap();
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(2), w(1)]);
        assert_eq!(ratios(&t, t.root()), vec![0.7, 0.3], "доли за позициями");
    }

    #[test]
    fn swap_refuses_ancestor_and_descendant() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(a, ContainerLayout::SplitV).unwrap();
        assert!(
            t.swap_nodes(cont, a).is_err(),
            "иначе дерево замкнулось бы в цикл"
        );
    }

    #[test]
    fn root_cannot_be_removed() {
        let mut t = Tree::default();
        assert_eq!(t.remove_node(t.root()), Err(TreeError::CannotRemoveRoot));
    }

    #[test]
    fn freed_slots_are_reused_without_aliasing() {
        let mut t = Tree::default();
        t.insert_window(w(1), InsertAt::Root);
        assert!(t.remove_window(w(1)).unwrap());
        let b = t.insert_window(w(2), InsertAt::Root);
        assert_eq!(t.get(b).unwrap().window(), Some(w(2)));
        assert_eq!(t.windows().collect::<Vec<_>>(), vec![w(2)]);
    }

    #[test]
    fn normalize_handles_a_degenerate_set() {
        let mut r = vec![0.0, -1.0, f64::NAN];
        normalize(&mut r);
        let sum: f64 = r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "равные доли вместо мусора");
    }

    #[test]
    fn tree_survives_a_serde_roundtrip() {
        let mut t = Tree::default();
        let a = t.insert_window(w(1), InsertAt::Root);
        let cont = t.split_leaf(a, ContainerLayout::Tabbed).unwrap();
        t.insert_window(
            w(2),
            InsertAt::Into {
                parent: cont,
                index: 1,
            },
        );
        let json = serde_json::to_string(&t).unwrap();
        let back: Tree = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
