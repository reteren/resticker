//! Модель переключателя окон (Alt+Tab) с поддержкой групп-табов
//! (M9, docs/TILING_DESIGN.md §Р3, docs/research/tiling/R6_GROUPS_ALTTAB_ANIM.md §1б).
//!
//! # Главное архитектурное решение: Собственный переключатель окон
//!
//! Системный Alt+Tab в Windows не имеет API для отображения группы окон как одной карточки.
//! Любые хаки (`WS_EX_TOOLWINDOW`, смена владельца через `GWLP_HWNDPARENT`) ломают
//! поведение окон, бьют по кнопкам на панели задач и конфликтуют с чужими процессами
//! (R6 §1б).
//!
//! Поэтому в resticker реализован **собственный переключатель**:
//! 1. Клавиатурный хук (`WH_KEYBOARD_LL`, `rst-win32::keyboard_guard`) перехватывает Alt+Tab.
//! 2. Модуль [`Switcher`] строит список карточек ([`Entry`]), где группа окон с табами
//!    ([`super::tree::ContainerLayout::Tabbed`] / [`super::tree::ContainerLayout::Stacked`])
//!    представлена **ровно одной карточкой**, а переключение ведёт на её активный таб.
//! 3. Отрисовка карточек с живыми превью и иконками производится на D3D11-оверлее
//!    (`resticker::tiling_ui`), полностью заменяя системный Alt+Tab.
//!
//! # Семантика порядка и выбора
//!
//! - **MRU-порядок (Most Recently Used)**: Окна из истории `mru` располагаются в начале
//!   списка в порядке их недавнего использования. Окна, отсутствующие в `mru`, добавляются
//!   следом в естественном порядке дерева воркспейсов.
//! - **Начальный выбор (Вторая запись)**: При открытии переключателя (`Switcher::open`)
//!   автоматически выбирается **вторая запись** (`selected = 1`), если в списке >= 2 окон.
//!   Это фундаментальное ожидание от Alt+Tab: пользователь хочет быстро переключиться на
//!   *предыдущее* окно, а не на текущее.
//! - **Окна скрытых воркспейсов**: В переключатель включаются окна со всех воркспейсов
//!   и мониторов. Это соответствует привычному поведению Windows (где Alt+Tab видит все окна)
//!   и позволяет мгновенно прыгать между воркспейсами по истории фокуса. При этом окна
//!   текущего воркспейса естественно оказываются вверху списка за счёт MRU.

use super::tree::{NodeId, NodeKind, Tree, WindowKey};
use super::workspace::Workspace;
use super::workspace::WorkspaceSet;

/// Одна карточка в переключателе окон.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Одиночное окно (плиточное или плавающее).
    Window(WindowKey),
    /// Группа окон (табы): показывается одной карточкой, переключение активирует `active`.
    Group {
        /// Активный таб группы (целевое окно при выборе карточки).
        active: WindowKey,
        /// Все окна-участники группы.
        members: Vec<WindowKey>,
    },
}

impl Entry {
    /// Получить целевое окно, которое будет активировано при выборе этой карточки.
    pub fn target_window(&self) -> WindowKey {
        match self {
            Self::Window(k) => *k,
            Self::Group { active, .. } => *active,
        }
    }

    /// Проверить, содержит ли эта карточка указанное окно (как одиночное или как член группы).
    pub fn contains(&self, key: WindowKey) -> bool {
        match self {
            Self::Window(k) => *k == key,
            Self::Group { members, .. } => members.contains(&key),
        }
    }
}

/// Состояние открытого сеанса переключателя окон Alt+Tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switcher {
    entries: Vec<Entry>,
    selected: usize,
}

impl Switcher {
    /// Собрать список карточек из воркспейсов с упорядочиванием по MRU.
    ///
    /// Окна из `mru` идут первыми (в порядке MRU), остальные — в порядке обхода дерева.
    /// Несуществующие окна из `mru` безопасно игнорируются.
    /// Если в списке >= 2 карточек, начальный выбор устанавливается на вторую запись (`selected = 1`).
    pub fn open(set: &WorkspaceSet, mru: &[WindowKey]) -> Self {
        let mut all_entries = Vec::new();

        // 1. Собираем все карточки со всех воркспейсов всех мониторов
        for m in &set.monitors {
            for ws in &m.workspaces {
                let ws_entries = collect_workspace_entries(ws);
                for entry in ws_entries {
                    all_entries.push(entry);
                }
            }
        }

        // 2. Упорядочиваем карточки по списку MRU
        let mut ordered = Vec::with_capacity(all_entries.len());
        let mut used = vec![false; all_entries.len()];

        // Сначала добавляем карточки, чьи окна встречаются в mru
        for &key in mru {
            for (i, entry) in all_entries.iter().enumerate() {
                if !used[i] && entry.contains(key) {
                    used[i] = true;
                    ordered.push(entry.clone());
                    break;
                }
            }
        }

        // Затем добавляем оставшиеся карточки в порядке их сбора из дерева
        for (i, entry) in all_entries.into_iter().enumerate() {
            if !used[i] {
                ordered.push(entry);
            }
        }

        // 3. Начальный выбор: ВТОРАЯ запись (индекс 1), если доступно >= 2 записей
        let selected = if ordered.len() >= 2 { 1 } else { 0 };

        Self {
            entries: ordered,
            selected,
        }
    }

    /// Список всех карточек в переключателе.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Индекс текущей выбранной карточки.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Шаг вперёд (`forward = true`, Tab) или назад (`forward = false`, Shift+Tab) по кругу.
    pub fn step(&mut self, forward: bool) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len();
        if forward {
            self.selected = (self.selected + 1) % len;
        } else {
            self.selected = (self.selected + len - 1) % len;
        }
    }

    /// Закрыть переключатель, вернув выбранное целевое окно (`None`, если переключатель пуст).
    pub fn close(self) -> Option<WindowKey> {
        if self.entries.is_empty() {
            None
        } else {
            Some(self.entries[self.selected].target_window())
        }
    }

    /// Проверить, пуст ли переключатель.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Собрать карточки окон и групп из одного воркспейса.
fn collect_workspace_entries(ws: &Workspace) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut grouped_windows = Vec::new();

    // 1. Обходим дерево тайлинга (сплиты и группы)
    collect_tree_entries(&ws.tree, ws.tree.root(), &mut entries, &mut grouped_windows);

    // 2. Добавляем плавающие окна
    for &f in &ws.floating {
        if !grouped_windows.contains(&f) && !entries.iter().any(|e| e.contains(f)) {
            entries.push(Entry::Window(f));
        }
    }

    entries
}

/// Рекурсивно обойти дерево и собрать карточки (группы оборачиваются в `Entry::Group`).
fn collect_tree_entries(
    tree: &Tree,
    node_id: NodeId,
    out: &mut Vec<Entry>,
    grouped_windows: &mut Vec<WindowKey>,
) {
    let Some(node) = tree.get(node_id) else {
        return;
    };
    match &node.kind {
        NodeKind::Window(key) => {
            if !grouped_windows.contains(key) {
                out.push(Entry::Window(*key));
            }
        }
        NodeKind::Container(c) => {
            if !c.layout.is_split() {
                // Контейнер-группа (Tabbed или Stacked): представляется одной карточкой
                let mut members = Vec::new();
                collect_leaves_under(tree, node_id, &mut members);
                if !members.is_empty() {
                    let active = descend_active_leaf(tree, node_id).unwrap_or(members[0]);
                    for &m in &members {
                        if !grouped_windows.contains(&m) {
                            grouped_windows.push(m);
                        }
                    }
                    out.push(Entry::Group { active, members });
                }
            } else {
                // Обычный сплит-контейнер: рекурсивно обходим детей
                for &child in &c.children {
                    collect_tree_entries(tree, child, out, grouped_windows);
                }
            }
        }
    }
}

/// Рекурсивно собрать все листья окон под узлом дерева.
fn collect_leaves_under(tree: &Tree, root: NodeId, out: &mut Vec<WindowKey>) {
    let Some(node) = tree.get(root) else {
        return;
    };
    match &node.kind {
        NodeKind::Window(key) => {
            if !out.contains(key) {
                out.push(*key);
            }
        }
        NodeKind::Container(c) => {
            for &child in &c.children {
                collect_leaves_under(tree, child, out);
            }
        }
    }
}

/// Спуститься по `focused_child` до активного листа группы.
fn descend_active_leaf(tree: &Tree, mut curr: NodeId) -> Option<WindowKey> {
    loop {
        let node = tree.get(curr)?;
        match &node.kind {
            NodeKind::Window(key) => return Some(*key),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonitorId;
    use crate::tiling::tree::{ContainerLayout, InsertAt};
    use crate::tiling::workspace::WorkspaceId;

    fn mon(s: &str) -> MonitorId {
        MonitorId(s.to_string())
    }

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    #[test]
    fn empty_workspace_set_produces_empty_switcher_and_close_returns_none() {
        let set = WorkspaceSet::new();
        let mut sw = Switcher::open(&set, &[]);
        assert!(sw.is_empty());
        assert_eq!(sw.entries(), &[]);
        assert_eq!(sw.selected(), 0);

        sw.step(true);
        sw.step(false);
        assert_eq!(sw.close(), None);
    }

    #[test]
    fn single_window_switcher_selects_first_entry_and_step_does_not_panic() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        let mut sw = Switcher::open(&set, &[w(1)]);
        assert!(!sw.is_empty());
        assert_eq!(sw.entries().len(), 1);
        assert_eq!(sw.selected(), 0);

        sw.step(true);
        assert_eq!(sw.selected(), 0);
        sw.step(false);
        assert_eq!(sw.selected(), 0);

        assert_eq!(sw.close(), Some(w(1)));
    }

    #[test]
    fn first_press_with_multiple_windows_selects_second_entry() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.insert_window(&m1, w(3), InsertAt::Root);

        // MRU: w(3) текущее активное, w(2) предыдущее, w(1) давнее
        let sw = Switcher::open(&set, &[w(3), w(2), w(1)]);
        assert_eq!(sw.entries().len(), 3);
        // Второе нажатие в MRU — индекс 1, то есть w(2)
        assert_eq!(sw.selected(), 1);
        assert_eq!(sw.close(), Some(w(2)));
    }

    #[test]
    fn mru_order_places_most_recent_windows_first() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.insert_window(&m1, w(3), InsertAt::Root);

        // Задаём порядок MRU: [w(2), w(3), w(1)]
        let sw = Switcher::open(&set, &[w(2), w(3), w(1)]);
        assert_eq!(
            sw.entries(),
            &[
                Entry::Window(w(2)),
                Entry::Window(w(3)),
                Entry::Window(w(1))
            ]
        );
    }

    #[test]
    fn dead_window_in_mru_is_skipped() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);

        // w(999) уже не существует в воркспейсах
        let sw = Switcher::open(&set, &[w(999), w(2), w(1)]);
        assert_eq!(sw.entries(), &[Entry::Window(w(2)), Entry::Window(w(1))]);
        assert_eq!(sw.selected(), 1);
        assert_eq!(sw.close(), Some(w(1)));
    }

    #[test]
    fn remaining_windows_not_in_mru_are_appended_in_tree_order() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.insert_window(&m1, w(3), InsertAt::Root);

        // В MRU указано только w(3)
        let sw = Switcher::open(&set, &[w(3)]);
        assert_eq!(
            sw.entries(),
            &[
                Entry::Window(w(3)),
                Entry::Window(w(1)),
                Entry::Window(w(2))
            ]
        );
    }

    #[test]
    fn group_with_multiple_tabs_is_represented_as_single_entry() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let w1_node = set.insert_window(&m1, w(1), InsertAt::Root).unwrap();

        // Превращаем w(1) в группу Tabbed с табами w(1), w(2), w(3)
        {
            let m1_mut = set.get_monitor_mut(&m1).unwrap();
            let ws = m1_mut.active_workspace_mut().unwrap();
            let group = ws
                .tree
                .split_leaf(w1_node, ContainerLayout::Tabbed)
                .unwrap();
            ws.tree.insert_window(
                w(2),
                InsertAt::Into {
                    parent: group,
                    index: 1,
                },
            );
            ws.tree.insert_window(
                w(3),
                InsertAt::Into {
                    parent: group,
                    index: 2,
                },
            );
            // Активный таб — w(2)
            ws.tree
                .set_focus(ws.tree.find_window(w(2)).unwrap())
                .unwrap();
        }

        let sw = Switcher::open(&set, &[]);
        assert_eq!(sw.entries().len(), 1);
        assert_eq!(
            sw.entries()[0],
            Entry::Group {
                active: w(2),
                members: vec![w(1), w(2), w(3)],
            }
        );
    }

    #[test]
    fn selecting_group_returns_active_tab_window_key() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let w1_node = set.insert_window(&m1, w(1), InsertAt::Root).unwrap();
        set.insert_window(&m1, w(4), InsertAt::Root);

        {
            let m1_mut = set.get_monitor_mut(&m1).unwrap();
            let ws = m1_mut.active_workspace_mut().unwrap();
            let group = ws
                .tree
                .split_leaf(w1_node, ContainerLayout::Tabbed)
                .unwrap();
            ws.tree.insert_window(
                w(2),
                InsertAt::Into {
                    parent: group,
                    index: 1,
                },
            );
            ws.tree
                .set_focus(ws.tree.find_window(w(1)).unwrap())
                .unwrap();
        }

        // Карточки: Group { active: w(1), members: [w(1), w(2)] } и Window(w(4))
        // MRU: [w(4), w(1)] -> при открытии выбран индекс 1 (Group)
        let sw = Switcher::open(&set, &[w(4), w(1)]);
        assert_eq!(sw.selected(), 1);
        assert_eq!(sw.close(), Some(w(1)));
    }

    #[test]
    fn mru_match_on_any_group_member_places_group_entry_at_that_position() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let w1_node = set.insert_window(&m1, w(1), InsertAt::Root).unwrap();
        set.insert_window(&m1, w(3), InsertAt::Root);

        {
            let m1_mut = set.get_monitor_mut(&m1).unwrap();
            let ws = m1_mut.active_workspace_mut().unwrap();
            let group = ws
                .tree
                .split_leaf(w1_node, ContainerLayout::Tabbed)
                .unwrap();
            ws.tree.insert_window(
                w(2),
                InsertAt::Into {
                    parent: group,
                    index: 1,
                },
            );
        }

        // В MRU упоминается неактивный таб w(2) группы
        let sw = Switcher::open(&set, &[w(2), w(3)]);
        assert_eq!(sw.entries().len(), 2);
        assert!(matches!(sw.entries()[0], Entry::Group { .. }));
        assert_eq!(sw.entries()[1], Entry::Window(w(3)));
    }

    #[test]
    fn step_forward_and_backward_cycles_cleanly() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.insert_window(&m1, w(3), InsertAt::Root);

        let mut sw = Switcher::open(&set, &[w(1), w(2), w(3)]);
        // Начальный: индекс 1 (w(2))
        assert_eq!(sw.selected(), 1);

        // Вперёд: 1 -> 2 -> 0 -> 1
        sw.step(true);
        assert_eq!(sw.selected(), 2);
        sw.step(true);
        assert_eq!(sw.selected(), 0);
        sw.step(true);
        assert_eq!(sw.selected(), 1);

        // Назад: 1 -> 0 -> 2 -> 1
        sw.step(false);
        assert_eq!(sw.selected(), 0);
        sw.step(false);
        assert_eq!(sw.selected(), 2);
        sw.step(false);
        assert_eq!(sw.selected(), 1);
    }

    #[test]
    fn step_backward_from_start_wraps_to_end() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);

        let mut sw = Switcher::open(&set, &[w(1), w(2)]);
        // Ставим на 0
        sw.step(false); // 1 -> 0
        assert_eq!(sw.selected(), 0);
        sw.step(false); // 0 -> 1
        assert_eq!(sw.selected(), 1);
    }

    #[test]
    fn floating_windows_are_included_in_switcher_entries() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.toggle_floating(w(2));

        let sw = Switcher::open(&set, &[w(2), w(1)]);
        assert_eq!(sw.entries(), &[Entry::Window(w(2)), Entry::Window(w(1))]);
    }

    #[test]
    fn fullscreen_window_is_included_in_switcher() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.set_fullscreen(w(1), true);

        let sw = Switcher::open(&set, &[w(1), w(2)]);
        assert_eq!(sw.entries(), &[Entry::Window(w(1)), Entry::Window(w(2))]);
    }

    #[test]
    fn windows_across_multiple_workspaces_and_monitors_are_all_collected() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.switch_to(&m1, WorkspaceId(2));
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.insert_window(&m2, w(3), InsertAt::Root);

        let sw = Switcher::open(&set, &[w(3), w(2), w(1)]);
        assert_eq!(
            sw.entries(),
            &[
                Entry::Window(w(3)),
                Entry::Window(w(2)),
                Entry::Window(w(1))
            ]
        );
    }

    #[test]
    fn entry_methods_target_window_and_contains_work_correctly() {
        let single = Entry::Window(w(10));
        assert_eq!(single.target_window(), w(10));
        assert!(single.contains(w(10)));
        assert!(!single.contains(w(20)));

        let group = Entry::Group {
            active: w(1),
            members: vec![w(1), w(2), w(3)],
        };
        assert_eq!(group.target_window(), w(1));
        assert!(group.contains(w(1)));
        assert!(group.contains(w(2)));
        assert!(group.contains(w(3)));
        assert!(!group.contains(w(4)));
    }

    #[test]
    fn close_consumes_switcher_and_returns_correct_target() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(10), InsertAt::Root);
        set.insert_window(&m1, w(20), InsertAt::Root);

        let sw = Switcher::open(&set, &[w(10), w(20)]);
        // Изначально выбран индекс 1 (w(20))
        assert_eq!(sw.close(), Some(w(20)));
    }
}
