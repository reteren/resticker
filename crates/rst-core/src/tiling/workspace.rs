//! Воркспейсы тайлинга (docs/TILING_DESIGN.md §Р1).
//!
//! # Главное архитектурное решение: Свои помониторные воркспейсы через DWM Cloak
//!
//! В resticker воркспейсы реализованы **полностью независимо от системных
//! виртуальных рабочих столов Windows** (Virtual Desktops):
//! 1. Публичный API `IVirtualDesktopManager` не позволяет перечислять столы,
//!    а `MoveWindowToDesktop` на чужих окнах возвращает `E_ACCESSDENIED`.
//!    Приватный `IVirtualDesktopManagerInternal` ломается в каждом обновлении Win11.
//! 2. Системные столы Windows глобальны на все мониторы. Наша модель —
//!    **помониторная** (как в Hyprland / i3): у каждого монитора свой независимый
//!    набор воркспейсов ([`MonitorWorkspaces`]).
//! 3. Скрытие окон неактивных воркспейсов и неактивных табов групп вычисляется
//!    чисто в этом модуле ([`WorkspaceSet::hidden_windows`]), а физическое скрытие
//!    через `DwmSetWindowAttribute(DWMWA_CLOAKED)` выполняет координатор
//!    (без использования `SW_HIDE`, ломающего Electron, и `SW_MINIMIZE`,
//!    засоряющего панель задач).
//!
//! # Полноэкранный режим (Fullscreen / Monocle)
//! При разворачивании окна на весь воркспейс (`fullscreen = Some(key)`), все остальные
//! плиточные и плавающие окна этого воркспейса исключаются из [`WorkspaceSet::visible_windows`]
//! и попадают в [`WorkspaceSet::hidden_windows`]. Плавающие окна также скрываются,
//! поскольку монопольный полноэкранный режим требует полной концентрации и исключает
//! перекрытие развёрнутого приложения вспомогательными окнами.
//!
//! # Потеря и переподключение мониторов
//! Воркспейсы привязаны к стабильному идентификатору монитора [`MonitorId`]
//! (device interface path, ADR-010). При отключении монитора его воркспейсы
//! **не удаляются**, а сохраняются в [`WorkspaceSet`]. Автомат потери монитора
//! (`crates/rst-core/src/monitor_loss.rs`) и координатор управляют временным
//! скрытием или миграцией окон на основной монитор по истечении таймера (20 сек),
//! а при повторном подключении монитора его воркспейсы мгновенно готовы к работе.
//!
//! # Персистентность и Serde
//! Все структуры поддерживают сериализацию [`serde::Serialize`] и [`serde::Deserialize`].
//! Важное примечание: [`WindowKey`] хранит рантайм-хэндл окна (`HWND as u64`),
//! который невалиден после перезапуска системы. Восстановление раскладки после
//! рестарта — задача координатора (сопоставление правил `WindowRules` по exe/классу/заголовку),
//! а модель воркспейсов сохраняет чистую структуру дерева и идентификаторы столов.

use serde::{Deserialize, Serialize};

use super::tree::{InsertAt, NodeId, Tree, WindowKey};
use crate::model::MonitorId;

/// Идентификатор воркспейса (1..=9 и далее).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(pub u8);

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Один воркспейс: дерево плиточных окон, плавающие окна и полноэкранный режим.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: Option<String>,
    pub tree: Tree,
    /// Плавающие окна поверх плиток, порядок = z-порядок снизу вверх.
    pub floating: Vec<WindowKey>,
    /// Окно, развёрнутое на весь воркспейс (monocle/fullscreen).
    pub fullscreen: Option<WindowKey>,
}

impl Workspace {
    /// Создать пустой воркспейс с заданным ID.
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            name: None,
            tree: Tree::default(),
            floating: Vec::new(),
            fullscreen: None,
        }
    }

    /// Создать воркспейс с пользовательским именем.
    pub fn with_name(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: Some(name.into()),
            tree: Tree::default(),
            floating: Vec::new(),
            fullscreen: None,
        }
    }

    /// Проверить, пуст ли воркспейс (нет плиточных, плавающих и fullscreen-окон).
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty() && self.floating.is_empty() && self.fullscreen.is_none()
    }

    /// Все окна воркспейса (плиточные, плавающие и полноэкранное) без дубликатов.
    pub fn all_windows(&self) -> Vec<WindowKey> {
        let mut wins = self.tree.windows().collect::<Vec<_>>();
        for f in &self.floating {
            if !wins.contains(f) {
                wins.push(*f);
            }
        }
        if let Some(fs) = self.fullscreen {
            if !wins.contains(&fs) {
                wins.push(fs);
            }
        }
        wins
    }
}

/// Воркспейсы одного монитора.
///
/// `monitor` — стабильный идентификатор монитора ([`MonitorId`]), а не индекс.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorWorkspaces {
    pub monitor: MonitorId,
    pub workspaces: Vec<Workspace>,
    /// Индекс активного воркспейса в `workspaces`.
    pub active: usize,
}

impl MonitorWorkspaces {
    /// Создать монитор с начальным воркспейсом 1.
    pub fn new(monitor: MonitorId) -> Self {
        Self {
            monitor,
            workspaces: vec![Workspace::new(WorkspaceId(1))],
            active: 0,
        }
    }

    /// Ссылка на активный воркспейс монитора.
    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.get(self.active)
    }

    /// Мутабельная ссылка на активный воркспейс монитора.
    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.get_mut(self.active)
    }

    /// Найти воркспейс по ID.
    pub fn find_workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces.iter().find(|ws| ws.id == id)
    }

    /// Найти воркспейс по ID (мутабельно).
    pub fn find_workspace_mut(&mut self, id: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|ws| ws.id == id)
    }
}

/// Набор воркспейсов всей системы, сгруппированный по мониторам.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkspaceSet {
    pub monitors: Vec<MonitorWorkspaces>,
}

/// Точное местоположение окна в системе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub monitor: MonitorId,
    pub workspace: WorkspaceId,
    pub floating: bool,
}

impl WorkspaceSet {
    /// Создать пустой набор воркспейсов.
    pub fn new() -> Self {
        Self::default()
    }

    /// Гарантировать наличие структуры монитора в наборе.
    pub fn ensure_monitor(&mut self, monitor: &MonitorId) -> &mut MonitorWorkspaces {
        if let Some(idx) = self.monitors.iter().position(|m| m.monitor == *monitor) {
            &mut self.monitors[idx]
        } else {
            self.monitors.push(MonitorWorkspaces::new(monitor.clone()));
            self.monitors.last_mut().unwrap()
        }
    }

    /// Получить состояние воркспейсов монитора.
    pub fn get_monitor(&self, monitor: &MonitorId) -> Option<&MonitorWorkspaces> {
        self.monitors.iter().find(|m| m.monitor == *monitor)
    }

    /// Получить состояние воркспейсов монитора (мутабельно).
    pub fn get_monitor_mut(&mut self, monitor: &MonitorId) -> Option<&mut MonitorWorkspaces> {
        self.monitors.iter_mut().find(|m| m.monitor == *monitor)
    }

    /// Найти местоположение окна (монитор, воркспейс, плавающее/плиточное).
    pub fn find_window(&self, key: WindowKey) -> Option<Location> {
        for m in &self.monitors {
            for ws in &m.workspaces {
                if ws.floating.contains(&key) {
                    return Some(Location {
                        monitor: m.monitor.clone(),
                        workspace: ws.id,
                        floating: true,
                    });
                }
                if ws.tree.find_window(key).is_some() {
                    return Some(Location {
                        monitor: m.monitor.clone(),
                        workspace: ws.id,
                        floating: false,
                    });
                }
                if ws.fullscreen == Some(key) {
                    return Some(Location {
                        monitor: m.monitor.clone(),
                        workspace: ws.id,
                        floating: false,
                    });
                }
            }
        }
        None
    }

    /// Вставить окно в активный воркспейс монитора.
    ///
    /// Гарантирует инвариант: окно живёт ровно в одном месте (автоматически удаляется отовсюду перед вставкой).
    pub fn insert_window(
        &mut self,
        monitor: &MonitorId,
        key: WindowKey,
        at: InsertAt,
    ) -> Option<NodeId> {
        self.remove_window(key);
        let m = self.ensure_monitor(monitor);
        let ws = m.active_workspace_mut()?;
        Some(ws.tree.insert_window(key, at))
    }

    /// Убрать окно отовсюду, где оно есть. Возвращает `true`, если окно было найдено и удалено.
    pub fn remove_window(&mut self, key: WindowKey) -> bool {
        let mut removed = false;
        for m in &mut self.monitors {
            for ws in &mut m.workspaces {
                if ws.fullscreen == Some(key) {
                    ws.fullscreen = None;
                    removed = true;
                }
                if let Some(pos) = ws.floating.iter().position(|k| *k == key) {
                    ws.floating.remove(pos);
                    removed = true;
                }
                if ws.tree.remove_window(key).unwrap_or(false) {
                    removed = true;
                }
            }
        }
        removed
    }

    /// Переключить активный воркспейс монитора.
    ///
    /// Если воркспейса с таким `id` ещё нет на мониторе, он создаётся автоматически.
    pub fn switch_to(&mut self, monitor: &MonitorId, ws: WorkspaceId) -> bool {
        let m = self.ensure_monitor(monitor);
        if let Some(pos) = m.workspaces.iter().position(|w| w.id == ws) {
            m.active = pos;
            true
        } else {
            m.workspaces.push(Workspace::new(ws));
            m.active = m.workspaces.len() - 1;
            true
        }
    }

    /// Перенести окно на другой воркспейс (возможно, другого монитора).
    ///
    /// Окно сохраняет статус плавающего / плиточного. При переносе старое местоположение очищается.
    pub fn move_window_to(&mut self, key: WindowKey, monitor: &MonitorId, ws: WorkspaceId) -> bool {
        let Some(loc) = self.find_window(key) else {
            return false;
        };
        let was_floating = loc.floating;

        self.remove_window(key);

        let m = self.ensure_monitor(monitor);
        let target_ws = if let Some(target) = m.workspaces.iter_mut().find(|w| w.id == ws) {
            target
        } else {
            m.workspaces.push(Workspace::new(ws));
            m.workspaces.last_mut().unwrap()
        };

        if was_floating {
            target_ws.floating.push(key);
        } else {
            target_ws.tree.insert_window(key, InsertAt::Root);
        }
        true
    }

    /// Переключить окно между плавающим и плиточным режимом.
    pub fn toggle_floating(&mut self, key: WindowKey) -> bool {
        let Some(loc) = self.find_window(key) else {
            return false;
        };
        let Some(m) = self.get_monitor_mut(&loc.monitor) else {
            return false;
        };
        let Some(ws) = m.find_workspace_mut(loc.workspace) else {
            return false;
        };

        if loc.floating {
            // Переводим из плавающего в плиточное дерево
            ws.floating.retain(|k| *k != key);
            if ws.fullscreen == Some(key) {
                ws.fullscreen = None;
            }
            ws.tree.insert_window(key, InsertAt::Root);
            true
        } else {
            // Переводим из дерева в плавающий список
            let _ = ws.tree.remove_window(key);
            if ws.fullscreen == Some(key) {
                ws.fullscreen = None;
            }
            ws.floating.push(key);
            true
        }
    }

    /// Установить или снять полноэкранный режим для окна.
    pub fn set_fullscreen(&mut self, key: WindowKey, fullscreen: bool) -> bool {
        let Some(loc) = self.find_window(key) else {
            return false;
        };
        let Some(m) = self.get_monitor_mut(&loc.monitor) else {
            return false;
        };
        let Some(ws) = m.find_workspace_mut(loc.workspace) else {
            return false;
        };

        if fullscreen {
            ws.fullscreen = Some(key);
        } else if ws.fullscreen == Some(key) {
            ws.fullscreen = None;
        }
        true
    }

    /// Список окон, которые ДОЛЖНЫ быть видимы прямо сейчас на всех активных мониторах.
    ///
    /// Включает видимые плиточные окна активных воркспейсов (с учётом табов) и плавающие окна,
    /// либо только полноэкранное окно при включённом fullscreen.
    pub fn visible_windows(&self) -> Vec<WindowKey> {
        let mut visible = Vec::new();
        for m in &self.monitors {
            let Some(active_ws) = m.active_workspace() else {
                continue;
            };

            if let Some(fs_key) = active_ws.fullscreen {
                visible.push(fs_key);
                continue;
            }

            // Добавляем видимые плиточные окна
            for leaf_id in active_ws.tree.leaves() {
                if is_leaf_visible_in_tree(&active_ws.tree, leaf_id) {
                    if let Some(key) = active_ws.tree.get(leaf_id).and_then(|n| n.window()) {
                        visible.push(key);
                    }
                }
            }

            // Добавляем плавающие окна активного воркспейса
            for &f in &active_ws.floating {
                if !visible.contains(&f) {
                    visible.push(f);
                }
            }
        }
        visible
    }

    /// Список окон, которые ДОЛЖНЫ быть скрыты координатором через `DWMWA_CLOAKED`.
    ///
    /// Включает окна неактивных воркспейсов, неактивные табы групп и окна, скрытые полноэкранным режимом.
    pub fn hidden_windows(&self) -> Vec<WindowKey> {
        let visible = self.visible_windows();
        let mut hidden = Vec::new();

        for m in &self.monitors {
            for ws in &m.workspaces {
                for win in ws.all_windows() {
                    if !visible.contains(&win) && !hidden.contains(&win) {
                        hidden.push(win);
                    }
                }
            }
        }
        hidden
    }
}

/// Проверить, активно ли окно в дереве (не скрыто ли оно неактивным табом группы).
fn is_leaf_visible_in_tree(tree: &Tree, leaf_id: NodeId) -> bool {
    let mut child = leaf_id;
    while let Some(parent) = tree.parent_of(child) {
        if let Some(c) = tree.get(parent).and_then(|n| n.container()) {
            if !c.layout.is_split() {
                // Tabbed или Stacked
                let child_idx = c.children.iter().position(|&x| x == child);
                if child_idx != Some(c.focused_child) {
                    return false;
                }
            }
        }
        child = parent;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::tree::ContainerLayout;
    use super::*;

    fn mon(s: &str) -> MonitorId {
        MonitorId(s.to_string())
    }

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    #[test]
    fn empty_workspace_set_has_no_windows_and_empty_visible_hidden() {
        let set = WorkspaceSet::new();
        assert!(set.visible_windows().is_empty());
        assert!(set.hidden_windows().is_empty());
        assert_eq!(set.find_window(w(1)), None);
    }

    #[test]
    fn insert_window_into_active_workspace_places_window_in_tree() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let node = set.insert_window(&m1, w(10), InsertAt::Root).unwrap();
        assert_eq!(node, NodeId(1));

        assert_eq!(set.visible_windows(), vec![w(10)]);
        assert!(set.hidden_windows().is_empty());

        let loc = set.find_window(w(10)).expect("окно найдено");
        assert_eq!(loc.monitor, m1);
        assert_eq!(loc.workspace, WorkspaceId(1));
        assert!(!loc.floating);
    }

    #[test]
    fn find_window_locates_tiled_and_floating_windows() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.toggle_floating(w(2));

        let loc1 = set.find_window(w(1)).unwrap();
        assert_eq!(loc1.workspace, WorkspaceId(1));
        assert!(!loc1.floating);

        let loc2 = set.find_window(w(2)).unwrap();
        assert_eq!(loc2.workspace, WorkspaceId(1));
        assert!(loc2.floating);
    }

    #[test]
    fn find_window_returns_none_for_missing_window() {
        let mut set = WorkspaceSet::new();
        set.insert_window(&mon("MON1"), w(1), InsertAt::Root);
        assert_eq!(set.find_window(w(99)), None);
    }

    #[test]
    fn switch_workspace_changes_visible_and_hidden_windows() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);

        // Переключаемся на воркспейс 2 и добавляем w(3)
        assert!(set.switch_to(&m1, WorkspaceId(2)));
        set.insert_window(&m1, w(3), InsertAt::Root);

        assert_eq!(set.visible_windows(), vec![w(3)]);
        assert_eq!(set.hidden_windows(), vec![w(1), w(2)]);

        // Возвращаемся на воркспейс 1
        assert!(set.switch_to(&m1, WorkspaceId(1)));
        assert_eq!(set.visible_windows(), vec![w(1), w(2)]);
        assert_eq!(set.hidden_windows(), vec![w(3)]);
    }

    #[test]
    fn move_window_to_same_monitor_different_workspace_leaves_no_duplicates() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);

        // Переносим w(2) на воркспейс 2
        assert!(set.move_window_to(w(2), &m1, WorkspaceId(2)));

        let loc = set.find_window(w(2)).unwrap();
        assert_eq!(loc.workspace, WorkspaceId(2));

        // На активном воркспейсе 1 осталось только w(1)
        assert_eq!(set.visible_windows(), vec![w(1)]);
        assert_eq!(set.hidden_windows(), vec![w(2)]);
    }

    #[test]
    fn move_window_to_another_monitor_transfers_window_cleanly() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);

        // Переносим w(1) с MON1 на MON2 (воркспейс 1)
        assert!(set.move_window_to(w(1), &m2, WorkspaceId(1)));

        let loc = set.find_window(w(1)).unwrap();
        assert_eq!(loc.monitor, m2);
        assert_eq!(loc.workspace, WorkspaceId(1));

        // На MON1 пусто, на MON2 оба окна
        let m1_ws = set.get_monitor(&m1).unwrap().active_workspace().unwrap();
        assert!(m1_ws.tree.is_empty());
        let m2_ws = set.get_monitor(&m2).unwrap().active_workspace().unwrap();
        assert_eq!(m2_ws.tree.windows().collect::<Vec<_>>(), vec![w(2), w(1)]);
    }

    #[test]
    fn move_window_to_nonexistent_workspace_creates_it() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        assert!(set.move_window_to(w(1), &m1, WorkspaceId(9)));
        let loc = set.find_window(w(1)).unwrap();
        assert_eq!(loc.workspace, WorkspaceId(9));
    }

    #[test]
    fn toggle_floating_converts_tiled_to_floating_and_back() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        // Плиточное -> плавающее
        assert!(set.toggle_floating(w(1)));
        let loc1 = set.find_window(w(1)).unwrap();
        assert!(loc1.floating);
        assert_eq!(set.visible_windows(), vec![w(1)]);

        // Плавающее -> плиточное
        assert!(set.toggle_floating(w(1)));
        let loc2 = set.find_window(w(1)).unwrap();
        assert!(!loc2.floating);
        assert_eq!(set.visible_windows(), vec![w(1)]);
    }

    #[test]
    fn toggle_floating_missing_window_returns_false() {
        let mut set = WorkspaceSet::new();
        assert!(!set.toggle_floating(w(404)));
    }

    #[test]
    fn inactive_tab_in_group_is_hidden_while_active_tab_is_visible() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        // Превращаем w(1) в группу Tabbed и добавляем w(2)
        {
            let m = set.get_monitor_mut(&m1).unwrap();
            let ws = m.active_workspace_mut().unwrap();
            let w1_node = ws.tree.find_window(w(1)).unwrap();
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
            // Делаем w(1) активным табом
            ws.tree.set_focus(w1_node).unwrap();
        }

        // w(1) активен в группе -> visible, w(2) неактивен -> hidden
        assert_eq!(set.visible_windows(), vec![w(1)]);
        assert_eq!(set.hidden_windows(), vec![w(2)]);
    }

    #[test]
    fn nested_groups_correctly_filter_hidden_tabs() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        {
            let m = set.get_monitor_mut(&m1).unwrap();
            let ws = m.active_workspace_mut().unwrap();
            let w1_node = ws.tree.find_window(w(1)).unwrap();
            let group_outer = ws
                .tree
                .split_leaf(w1_node, ContainerLayout::Tabbed)
                .unwrap();
            let w2_node = ws.tree.insert_window(
                w(2),
                InsertAt::Into {
                    parent: group_outer,
                    index: 1,
                },
            );

            // Оборачиваем таб w2 в SplitV и добавляем w3
            let split_inner = ws
                .tree
                .split_leaf(w2_node, ContainerLayout::SplitV)
                .unwrap();
            ws.tree.insert_window(
                w(3),
                InsertAt::Into {
                    parent: split_inner,
                    index: 1,
                },
            );

            // Активный таб внешней группы — w1
            ws.tree.set_focus(w1_node).unwrap();
        }

        // w1 виден, а w2 и w3 внутри неактивной ветки внешней группы спрятаны
        assert_eq!(set.visible_windows(), vec![w(1)]);
        assert_eq!(set.hidden_windows(), vec![w(2), w(3)]);
    }

    #[test]
    fn fullscreen_window_hides_all_other_tiled_and_floating_windows_on_workspace() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.insert_window(&m1, w(3), InsertAt::Root);
        set.toggle_floating(w(3));

        // До fullscreen видны все 3 окна
        assert_eq!(set.visible_windows(), vec![w(1), w(2), w(3)]);

        // Включаем fullscreen на w(1)
        assert!(set.set_fullscreen(w(1), true));
        assert_eq!(set.visible_windows(), vec![w(1)]);
        assert_eq!(set.hidden_windows(), vec![w(2), w(3)]);

        // Выключаем fullscreen
        assert!(set.set_fullscreen(w(1), false));
        assert_eq!(set.visible_windows(), vec![w(1), w(2), w(3)]);
        assert!(set.hidden_windows().is_empty());
    }

    #[test]
    fn fullscreen_on_inactive_workspace_is_hidden() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.set_fullscreen(w(1), true);

        // Переключаемся на воркспейс 2
        set.switch_to(&m1, WorkspaceId(2));
        set.insert_window(&m1, w(2), InsertAt::Root);

        assert_eq!(set.visible_windows(), vec![w(2)]);
        assert_eq!(set.hidden_windows(), vec![w(1)]);
    }

    #[test]
    fn remove_window_removes_from_tree_floating_and_fullscreen() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.toggle_floating(w(2));
        set.set_fullscreen(w(1), true);

        assert!(set.remove_window(w(1)));
        assert_eq!(set.find_window(w(1)), None);
        assert_eq!(
            set.get_monitor(&m1)
                .unwrap()
                .active_workspace()
                .unwrap()
                .fullscreen,
            None
        );

        assert!(set.remove_window(w(2)));
        assert_eq!(set.find_window(w(2)), None);
        assert!(set.visible_windows().is_empty());
    }

    #[test]
    fn remove_window_nonexistent_returns_false() {
        let mut set = WorkspaceSet::new();
        assert!(!set.remove_window(w(999)));
    }

    #[test]
    fn reinserting_existing_window_moves_it_without_duplicates() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);

        // Повторная вставка того же окна на другой монитор
        set.insert_window(&m2, w(1), InsertAt::Root);

        let loc = set.find_window(w(1)).unwrap();
        assert_eq!(loc.monitor, m2);
        assert!(
            set.get_monitor(&m1)
                .unwrap()
                .active_workspace()
                .unwrap()
                .tree
                .is_empty()
        );
    }

    #[test]
    fn monitor_reconnection_preserves_workspaces_by_stable_monitor_id() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("DISPLAY_PRIMARY_PCI_1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.switch_to(&m1, WorkspaceId(3));
        set.insert_window(&m1, w(3), InsertAt::Root);

        // Симуляция обращения к монитору после "отключения" и повторного обнаружения
        let m_state = set.get_monitor(&m1).expect("состояние монитора сохранено");
        assert_eq!(m_state.workspaces.len(), 2);
        assert_eq!(m_state.active_workspace().unwrap().id, WorkspaceId(3));
    }

    #[test]
    fn workspace_set_serde_roundtrip() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m1, w(2), InsertAt::Root);
        set.toggle_floating(w(2));
        set.switch_to(&m1, WorkspaceId(2));
        set.insert_window(&m1, w(3), InsertAt::Root);

        let json = serde_json::to_string(&set).unwrap();
        let restored: WorkspaceSet = serde_json::from_str(&json).unwrap();
        assert_eq!(set, restored);
    }
}
