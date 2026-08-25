//! Исполнение действий тайлинга [`Action`] над состоянием воркспейсов [`WorkspaceSet`]
//! (M9, docs/TILING_DESIGN.md §T3).
//!
//! # Роль модуля в архитектуре
//!
//! Данный модуль является **маршрутизатором действий** (action dispatcher):
//! 1. Получает абстрактное действие [`Action`], порождённое таблицей горячих клавиш
//!    (`binds`) или IPC-командой.
//! 2. Определяет целевой монитор и активный воркспейс ([`MonitorWorkspaces::active_workspace`]).
//! 3. Вызывает низкоуровневые операции над деревом [`super::ops`] или методы набора
//!    воркспейсов [`super::workspace::WorkspaceSet`].
//! 4. Возвращает [`Outcome`], на основании которого координатор Win32 принимает решение
//!    о пересчёте геометрии оверлея (`Relayout`), переводе системного фокуса
//!    через `SetForegroundWindow` (`FocusWindow`) или посылке сообщения закрытия
//!    окна `WM_CLOSE` (`CloseWindow`).
//!
//! # Важные архитектурные решения
//!
//! ## 1. Разделение логического и системного фокуса в Windows
//! В отличие от Wayland-композитора, где фокус ввода и геометрия окна неразрывно
//! связаны внутри одного процесса, в Windows фокус в логическом дереве тайлинга
//! (`Tree::focus`) не переводит системный фокус автоматически. Поэтому при успехе
//! [`Action::FocusDirection`] возвращается [`Outcome::FocusWindow(key)`], обязывающий
//! координатор вызвать платформенный `SetForegroundWindow`.
//!
//! ## 2. Асинхронное закрытие окон (WM_CLOSE)
//! При получении [`Action::CloseWindow`] окно **не удаляется** из дерева немедленно.
//! Координатор отправляет процессу окна `WM_CLOSE`. Приложение может иметь несохранённые
//! данные и показать модальный диалог «Сохранить изменения?» или вовсе отменить закрытие.
//! Окно будет удалено из дерева тайлинга трекером (`WindowTracker`) только после реального
//! уничтожения его `HWND` (событие `EVENT_OBJECT_DESTROY`).
//!
//! ## 3. Режимы Submap
//! Действия [`Action::EnterSubmap`] и [`Action::LeaveSubmap`] управляют FSM таблицы
//! биндов и перехватываются слоем `binds.rs`. До воркспейсов они штатно не доходят,
//! но для исчерпывающего сопоставления шаблонов возвращают [`Outcome::NoChange`].

use super::action::Action;
use super::ops;
use super::tree::WindowKey;
use super::workspace::{Workspace, WorkspaceId, WorkspaceSet};
use crate::model::MonitorId;

/// Результат исполнения действия над воркспейсами.
///
/// Координатор использует `Outcome`, чтобы решить, нужен ли пересчёт раскладки
/// и требуется ли выполнить платформенные Win32-вызовы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Состояние не изменилось (упор в границу, отсутствие фокуса, пустой воркспейс и т.д.).
    NoChange,
    /// Дерево тайлинга или активный воркспейс изменились — требуется пересчёт геометрии.
    Relayout,
    /// Фокус в дереве перешёл на другое окно — требуется пересчёт И вызов `SetForegroundWindow`.
    FocusWindow(WindowKey),
    /// Координатор обязан отправить `WM_CLOSE` данному окну (дерево тайлинга не мутируется).
    CloseWindow(WindowKey),
}

/// Найти сфокусированное окно на воркспейсе (fullscreen -> дерево тайлинга -> плавающие).
fn focused_window(ws: &Workspace) -> Option<WindowKey> {
    if let Some(fs) = ws.fullscreen {
        return Some(fs);
    }
    if let Some(focus_leaf) = ws.tree.focus() {
        if let Some(key) = ws.tree.get(focus_leaf).and_then(|n| n.window()) {
            return Some(key);
        }
    }
    ws.floating.last().copied()
}

/// Применить действие [`Action`] к активному воркспейсу монитора `monitor`.
///
/// Все операции тайлинга и воркспейсов являются помониторными (docs/TILING_DESIGN.md §Р1).
/// Если монитор не найден или на нём нет активного воркспейса, возвращается [`Outcome::NoChange`].
pub fn apply(set: &mut WorkspaceSet, monitor: &MonitorId, action: &Action) -> Outcome {
    match action {
        Action::EnterSubmap(_) | Action::LeaveSubmap => {
            // Модальные режимы биндов (submap) перехватываются и обрабатываются
            // конечным автоматом в слое биндов (binds.rs).
            Outcome::NoChange
        }

        Action::Workspace(n) => {
            let target_ws = WorkspaceId(*n);
            let Some(m) = set.get_monitor(monitor) else {
                return Outcome::NoChange;
            };
            if let Some(active_ws) = m.active_workspace() {
                if active_ws.id == target_ws {
                    // Уже находимся на запрошенном воркспейсе
                    return Outcome::NoChange;
                }
            }
            if set.switch_to(monitor, target_ws) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::SendToWorkspace(n) => {
            let target_ws = WorkspaceId(*n);
            let Some(m) = set.get_monitor(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace() else {
                return Outcome::NoChange;
            };
            if active_ws.id == target_ws {
                return Outcome::NoChange;
            }
            let Some(focus_key) = focused_window(active_ws) else {
                return Outcome::NoChange;
            };
            if set.move_window_to(focus_key, monitor, target_ws) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::ToggleFloating => {
            let Some(m) = set.get_monitor(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace() else {
                return Outcome::NoChange;
            };
            let Some(focus_key) = focused_window(active_ws) else {
                return Outcome::NoChange;
            };
            if set.toggle_floating(focus_key) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::ToggleFullscreen => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if active_ws.fullscreen.is_some() {
                active_ws.fullscreen = None;
                Outcome::Relayout
            } else if let Some(focus_key) = focused_window(active_ws) {
                active_ws.fullscreen = Some(focus_key);
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::CloseWindow => {
            let Some(m) = set.get_monitor(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace() else {
                return Outcome::NoChange;
            };
            if let Some(focus_key) = focused_window(active_ws) {
                Outcome::CloseWindow(focus_key)
            } else {
                Outcome::NoChange
            }
        }

        Action::FocusDirection(dir) => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::focus_direction(&mut active_ws.tree, *dir) {
                if let Some(key) = active_ws
                    .tree
                    .focus()
                    .and_then(|f| active_ws.tree.get(f))
                    .and_then(|n| n.window())
                {
                    Outcome::FocusWindow(key)
                } else {
                    Outcome::Relayout
                }
            } else {
                Outcome::NoChange
            }
        }

        Action::MoveDirection(dir) => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::move_direction(&mut active_ws.tree, *dir) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::SwapDirection(dir) => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::swap_direction(&mut active_ws.tree, *dir) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::Resize { dir, delta } => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::resize_focused(&mut active_ws.tree, *dir, *delta) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::ToggleSplit => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::toggle_split(&mut active_ws.tree) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::ToggleGroup => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::toggle_group(&mut active_ws.tree) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }

        Action::CycleGroup { forward } => {
            let Some(m) = set.get_monitor_mut(monitor) else {
                return Outcome::NoChange;
            };
            let Some(active_ws) = m.active_workspace_mut() else {
                return Outcome::NoChange;
            };
            if ops::cycle_group(&mut active_ws.tree, *forward) {
                Outcome::Relayout
            } else {
                Outcome::NoChange
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::ops::Direction;
    use crate::tiling::tree::{ContainerLayout, InsertAt};

    fn mon(s: &str) -> MonitorId {
        MonitorId(s.to_string())
    }

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    #[test]
    fn missing_monitor_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("NONEXISTENT");
        assert_eq!(
            apply(&mut set, &m, &Action::FocusDirection(Direction::Left)),
            Outcome::NoChange
        );
        assert_eq!(
            apply(&mut set, &m, &Action::Workspace(2)),
            Outcome::NoChange
        );
        assert_eq!(apply(&mut set, &m, &Action::CloseWindow), Outcome::NoChange);
    }

    #[test]
    fn empty_workspace_directional_ops_return_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.ensure_monitor(&m);

        assert_eq!(
            apply(&mut set, &m, &Action::FocusDirection(Direction::Left)),
            Outcome::NoChange
        );
        assert_eq!(
            apply(&mut set, &m, &Action::MoveDirection(Direction::Right)),
            Outcome::NoChange
        );
        assert_eq!(
            apply(&mut set, &m, &Action::SwapDirection(Direction::Up)),
            Outcome::NoChange
        );
        assert_eq!(
            apply(
                &mut set,
                &m,
                &Action::Resize {
                    dir: Direction::Left,
                    delta: 0.05
                }
            ),
            Outcome::NoChange
        );
    }

    #[test]
    fn focus_direction_successful_returns_focus_window_with_target_key() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        set.insert_window(&m, w(2), InsertAt::Root);

        // Сейчас фокус на w(2). Движение влево должно вернуть FocusWindow(w(1))
        let res = apply(&mut set, &m, &Action::FocusDirection(Direction::Left));
        assert_eq!(res, Outcome::FocusWindow(w(1)));

        // Движение вправо возвращает FocusWindow(w(2))
        let res = apply(&mut set, &m, &Action::FocusDirection(Direction::Right));
        assert_eq!(res, Outcome::FocusWindow(w(2)));
    }

    #[test]
    fn focus_direction_hitting_boundary_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        set.insert_window(&m, w(2), InsertAt::Root);

        // Фокус на w(2) (правый край) -> вправо идти некуда
        let res = apply(&mut set, &m, &Action::FocusDirection(Direction::Right));
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn move_direction_reorders_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        set.insert_window(&m, w(2), InsertAt::Root);

        // Двигаем w(2) влево
        let res = apply(&mut set, &m, &Action::MoveDirection(Direction::Left));
        assert_eq!(res, Outcome::Relayout);

        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws.tree.windows().collect::<Vec<_>>(), vec![w(2), w(1)]);
    }

    #[test]
    fn move_direction_blocked_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::MoveDirection(Direction::Left));
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn swap_direction_swaps_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        let _w2_node = set.insert_window(&m, w(2), InsertAt::Root);

        // Фокус на w(2), меняем влево с w(1)
        let res = apply(&mut set, &m, &Action::SwapDirection(Direction::Left));
        assert_eq!(res, Outcome::Relayout);

        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws.tree.windows().collect::<Vec<_>>(), vec![w(2), w(1)]);
    }

    #[test]
    fn swap_direction_blocked_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::SwapDirection(Direction::Right));
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn resize_resizes_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        set.insert_window(&m, w(2), InsertAt::Root);

        let res = apply(
            &mut set,
            &m,
            &Action::Resize {
                dir: Direction::Left,
                delta: 0.1,
            },
        );
        assert_eq!(res, Outcome::Relayout);
    }

    #[test]
    fn resize_on_single_window_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(
            &mut set,
            &m,
            &Action::Resize {
                dir: Direction::Right,
                delta: 0.1,
            },
        );
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn toggle_split_switches_orientation_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::ToggleSplit);
        assert_eq!(res, Outcome::Relayout);

        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        let root = ws.tree.root();
        assert_eq!(
            ws.tree.get(root).unwrap().container().unwrap().layout,
            ContainerLayout::SplitV
        );
    }

    #[test]
    fn toggle_group_converts_to_tabbed_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::ToggleGroup);
        assert_eq!(res, Outcome::Relayout);

        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        let root = ws.tree.root();
        assert_eq!(
            ws.tree.get(root).unwrap().container().unwrap().layout,
            ContainerLayout::Tabbed
        );
    }

    #[test]
    fn cycle_group_cycles_tab_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        let w1_node = set.insert_window(&m, w(1), InsertAt::Root).unwrap();

        {
            let m_ref = set.get_monitor_mut(&m).unwrap();
            let ws = m_ref.active_workspace_mut().unwrap();
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

        let res = apply(&mut set, &m, &Action::CycleGroup { forward: true });
        assert_eq!(res, Outcome::Relayout);
    }

    #[test]
    fn cycle_group_outside_group_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::CycleGroup { forward: true });
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn toggle_floating_toggles_tiled_to_floating_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        // Плитка -> плавающее
        let res = apply(&mut set, &m, &Action::ToggleFloating);
        assert_eq!(res, Outcome::Relayout);
        let loc = set.find_window(w(1)).unwrap();
        assert!(loc.floating);

        // Плавающее -> плитка
        let res = apply(&mut set, &m, &Action::ToggleFloating);
        assert_eq!(res, Outcome::Relayout);
        let loc = set.find_window(w(1)).unwrap();
        assert!(!loc.floating);
    }

    #[test]
    fn toggle_floating_on_empty_workspace_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.ensure_monitor(&m);

        assert_eq!(
            apply(&mut set, &m, &Action::ToggleFloating),
            Outcome::NoChange
        );
    }

    #[test]
    fn toggle_fullscreen_toggles_on_and_off_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        // Включаем fullscreen
        let res = apply(&mut set, &m, &Action::ToggleFullscreen);
        assert_eq!(res, Outcome::Relayout);
        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws.fullscreen, Some(w(1)));

        // Выключаем fullscreen
        let res = apply(&mut set, &m, &Action::ToggleFullscreen);
        assert_eq!(res, Outcome::Relayout);
        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws.fullscreen, None);
    }

    #[test]
    fn toggle_fullscreen_on_empty_workspace_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.ensure_monitor(&m);

        assert_eq!(
            apply(&mut set, &m, &Action::ToggleFullscreen),
            Outcome::NoChange
        );
    }

    #[test]
    fn close_window_returns_close_window_and_preserves_tree_structure() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        set.insert_window(&m, w(2), InsertAt::Root);

        // Фокус на w(2). Закрытие должно вернуть CloseWindow(w(2)), не удаляя окно из дерева сразу
        let res = apply(&mut set, &m, &Action::CloseWindow);
        assert_eq!(res, Outcome::CloseWindow(w(2)));

        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws.tree.windows().collect::<Vec<_>>(), vec![w(1), w(2)]);
    }

    #[test]
    fn close_window_on_empty_workspace_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.ensure_monitor(&m);

        assert_eq!(apply(&mut set, &m, &Action::CloseWindow), Outcome::NoChange);
    }

    #[test]
    fn workspace_switch_to_new_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::Workspace(2));
        assert_eq!(res, Outcome::Relayout);

        let ws = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws.id, WorkspaceId(2));
    }

    #[test]
    fn workspace_switch_to_current_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        // Сейчас на воркспейсе 1
        let res = apply(&mut set, &m, &Action::Workspace(1));
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn send_to_workspace_moves_window_and_returns_relayout() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);
        set.insert_window(&m, w(2), InsertAt::Root);

        // Фокус на w(2), отправляем на воркспейс 3
        let res = apply(&mut set, &m, &Action::SendToWorkspace(3));
        assert_eq!(res, Outcome::Relayout);

        let loc = set.find_window(w(2)).unwrap();
        assert_eq!(loc.workspace, WorkspaceId(3));

        let ws1 = set.get_monitor(&m).unwrap().active_workspace().unwrap();
        assert_eq!(ws1.tree.windows().collect::<Vec<_>>(), vec![w(1)]);
    }

    #[test]
    fn send_to_workspace_on_empty_workspace_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.ensure_monitor(&m);

        let res = apply(&mut set, &m, &Action::SendToWorkspace(2));
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn send_to_workspace_to_current_workspace_returns_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        let res = apply(&mut set, &m, &Action::SendToWorkspace(1));
        assert_eq!(res, Outcome::NoChange);
    }

    #[test]
    fn submap_actions_return_no_change() {
        let mut set = WorkspaceSet::new();
        let m = mon("MON1");
        set.insert_window(&m, w(1), InsertAt::Root);

        assert_eq!(
            apply(&mut set, &m, &Action::EnterSubmap("resize".to_string())),
            Outcome::NoChange
        );
        assert_eq!(apply(&mut set, &m, &Action::LeaveSubmap), Outcome::NoChange);
    }
}
