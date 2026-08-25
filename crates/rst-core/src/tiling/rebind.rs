//! Перепривязка воркспейсов и окон при изменении набора мониторов
//! (docs/TILING_DESIGN.md §T4, ADR-010, ADR-011).
//!
//! # Главная задача модуля
//!
//! В resticker воркспейсы являются **помониторными** и привязаны к стабильному
//! идентификатору монитора [`MonitorId`] (device interface path, ADR-010).
//! Когда монитор отключается (физическое отключение кабеля, выключение питания,
//! переключение KVM):
//! 1. Windows DWM автоматически телепортирует все физические окна на оставшийся
//!    основной дисплей.
//! 2. Если тайлинг оставит окна в воркспейсах пропавшего монитора, они навсегда
//!    выпадут из поля зрения пользователя (останутся в `hidden_windows` или
//!    без геометрического тайлинга на живом экране).
//! 3. Модуль [`rebind`] строит и применяет план перепривязки ([`RebindPlan`]):
//!    эвакуирует окна с пропавшего монитора на оставшийся, сохраняя саму структуру
//!    воркспейсов пропавшего монитора для его последующего возвращения.
//!
//! # Архитектурные решения
//!
//! ## 1. Куда переносить окна пропавшего монитора?
//! Окна переносятся на первый доступный монитор из актуального списка `present`
//! (`present[0]`, который координатор передаёт как основной/активный дисплей),
//! в его текущий активный воркспейс. Это мгновенно возвращает окна в рабочее
//! пространство пользователя.
//!
//! ## 2. Раскладку пропавшего монитора СОХРАНИТЬ или растворить?
//! Сама структура воркспейсов [`super::workspace::MonitorWorkspaces`] пропавшего
//! монитора **СОХРАНЯЕТСЯ** в [`WorkspaceSet`]. Из неё извлекаются только живые
//! окна. Когда монитор подключается обратно (`restored`), его настроенные
//! воркспейсы (1..9, имена, история сплитов) мгновенно готовы к работе без
//! повторной инициализации.
//!
//! ## 3. Что делать, если пропали ВСЕ мониторы (сон, блокировка, DPMS)?
//! Если `present` пуст (`present.is_empty()`), планировщик возвращает пустой план.
//! Никакие окна не двигаются и данные не теряются. Это защищает от кратковременных
//! пропаданий перечисления дисплеев при уходе видеокарты в энергосберегающий режим.
//!
//! ## 4. Отсутствие дубликатов (Single-ownership)
//! Перенос окон выполняется через [`WorkspaceSet::move_window_to`], гарантирующий,
//! что окно удаляется из старого воркспейса перед вставкой в новый.
//!
//! ## 5. Связь с `monitor_loss.rs` и `monitor_rebind.rs`
//! Модуль оперирует тем же стабильным [`MonitorId`], что и подсистема стикеров
//! (`crates/rst-core/src/monitor_loss.rs`), и предназначен для вызова координатором
//! по событию `WM_DISPLAYCHANGE`.

use super::tree::WindowKey;
use super::workspace::{WorkspaceId, WorkspaceSet};
use crate::model::MonitorId;

/// План действий при изменении набора подключённых мониторов.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RebindPlan {
    /// Окна, которые необходимо перенести: `(окно, целевой_монитор)`.
    pub moves: Vec<(WindowKey, MonitorId)>,
    /// Идентификаторы мониторов, которые отключились и чьи воркспейсы стали осиротевшими.
    pub orphaned: Vec<MonitorId>,
    /// Идентификаторы мониторов, которые вернулись (снова подключены) и готовы к работе.
    pub restored: Vec<MonitorId>,
}

impl RebindPlan {
    /// Проверить, пуст ли план (никаких действий не требуется).
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.orphaned.is_empty() && self.restored.is_empty()
    }
}

/// Построить план перепривязки окон на основе актуального списка подключённых мониторов `present`.
///
/// Если `present` пуст (блокировка экрана / сон), возвращается пустой план во избежание
/// разрушения раскладки.
pub fn plan(set: &WorkspaceSet, present: &[MonitorId]) -> RebindPlan {
    if present.is_empty() {
        return RebindPlan::default();
    }

    let target_monitor = &present[0];
    let mut plan = RebindPlan::default();

    // 1. Ищем мониторы, которые есть в WorkspaceSet, но отсутствуют в present (осиротевшие)
    for m in &set.monitors {
        if !present.contains(&m.monitor) {
            plan.orphaned.push(m.monitor.clone());

            // Собираем все окна со всех воркспейсов осиротевшего монитора для переноса
            for ws in &m.workspaces {
                for win in ws.all_windows() {
                    plan.moves.push((win, target_monitor.clone()));
                }
            }
        }
    }

    // 2. Ищем мониторы, которые есть в present и уже присутствуют в WorkspaceSet (вернувшиеся/активные)
    for mon_id in present {
        if let Some(m) = set.get_monitor(mon_id) {
            // Если монитор был пуст (например, после эвакуации окон при прошлом отключении),
            // он помечается как восстановленный
            let has_windows = m.workspaces.iter().any(|ws| !ws.is_empty());
            if !has_windows {
                plan.restored.push(mon_id.clone());
            }
        }
    }

    plan
}

/// Применить план перепривязки к состоянию воркспейсов.
///
/// Возвращает `true`, если состояние изменилось (были перенесены окна или обновлены мониторы).
pub fn apply(set: &mut WorkspaceSet, plan: &RebindPlan) -> bool {
    if plan.is_empty() {
        return false;
    }

    let mut changed = false;

    // Гарантируем наличие восстановленных мониторов в наборе
    for mon_id in &plan.restored {
        set.ensure_monitor(mon_id);
    }

    // Выполняем перенос окон на целевые мониторы
    for (win_key, target_mon) in &plan.moves {
        // Определяем активный воркспейс на целевом мониторе
        let target_ws_id = set
            .get_monitor(target_mon)
            .and_then(|m| m.active_workspace())
            .map(|ws| ws.id)
            .unwrap_or(WorkspaceId(1));

        if set.move_window_to(*win_key, target_mon, target_ws_id) {
            changed = true;
        }
    }

    changed || !plan.restored.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::tree::{ContainerLayout, InsertAt};

    fn mon(s: &str) -> MonitorId {
        MonitorId(s.to_string())
    }

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    #[test]
    fn empty_workspace_set_produces_empty_plan() {
        let set = WorkspaceSet::new();
        let p = plan(&set, std::slice::from_ref(&mon("MON1")));
        assert!(p.moves.is_empty());
        assert!(p.orphaned.is_empty());
        assert!(p.restored.is_empty());
        assert!(p.is_empty());
    }

    #[test]
    fn empty_present_monitors_produces_empty_plan_and_preserves_all_data() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        let p = plan(&set, &[]);
        assert!(p.is_empty());
        assert!(!apply(&mut set, &p));

        // Данные не тронуты
        let loc = set.find_window(w(1)).unwrap();
        assert_eq!(loc.monitor, m1);
    }

    #[test]
    fn single_monitor_with_no_changes_produces_empty_moves() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        set.insert_window(&m1, w(1), InsertAt::Root);

        let p = plan(&set, std::slice::from_ref(&m1));
        assert!(p.moves.is_empty());
        assert!(p.orphaned.is_empty());
        assert!(p.restored.is_empty());
    }

    #[test]
    fn disconnected_monitor_moves_all_tiled_windows_to_remaining_monitor() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);
        set.insert_window(&m2, w(3), InsertAt::Root);

        // MON2 отключился, остался только MON1
        let p = plan(&set, std::slice::from_ref(&m1));
        assert_eq!(p.orphaned, vec![m2.clone()]);
        assert_eq!(p.moves, vec![(w(2), m1.clone()), (w(3), m1.clone())]);

        assert!(apply(&mut set, &p));

        // Все окна теперь на MON1
        assert_eq!(set.find_window(w(2)).unwrap().monitor, m1);
        assert_eq!(set.find_window(w(3)).unwrap().monitor, m1);
        assert_eq!(set.visible_windows(), vec![w(1), w(2), w(3)]);

        // Воркспейс MON2 остался в наборе, но пуст
        let m2_ws = set.get_monitor(&m2).unwrap().active_workspace().unwrap();
        assert!(m2_ws.is_empty());
    }

    #[test]
    fn disconnected_monitor_moves_floating_and_fullscreen_windows_too() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);

        // На MON2: w(2) в плитке, w(3) плавающее, w(4) в fullscreen
        set.insert_window(&m2, w(2), InsertAt::Root);
        set.insert_window(&m2, w(3), InsertAt::Root);
        set.toggle_floating(w(3));
        set.insert_window(&m2, w(4), InsertAt::Root);
        set.set_fullscreen(w(4), true);

        let p = plan(&set, std::slice::from_ref(&m1));
        assert_eq!(p.orphaned, vec![m2.clone()]);
        assert!(p.moves.contains(&(w(2), m1.clone())));
        assert!(p.moves.contains(&(w(3), m1.clone())));
        assert!(p.moves.contains(&(w(4), m1.clone())));

        assert!(apply(&mut set, &p));

        assert_eq!(set.find_window(w(2)).unwrap().monitor, m1);
        assert_eq!(set.find_window(w(3)).unwrap().monitor, m1);
        assert_eq!(set.find_window(w(4)).unwrap().monitor, m1);
    }

    #[test]
    fn disconnected_monitor_multiple_workspaces_evacuates_all_windows() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);

        // На MON2 окно w(2) на воркспейсе 1, а w(3) на воркспейсе 2
        set.insert_window(&m2, w(2), InsertAt::Root);
        set.switch_to(&m2, WorkspaceId(2));
        set.insert_window(&m2, w(3), InsertAt::Root);

        let p = plan(&set, std::slice::from_ref(&m1));
        assert_eq!(p.orphaned, vec![m2.clone()]);
        assert_eq!(p.moves.len(), 2);

        assert!(apply(&mut set, &p));
        assert_eq!(set.find_window(w(2)).unwrap().monitor, m1);
        assert_eq!(set.find_window(w(3)).unwrap().monitor, m1);
    }

    #[test]
    fn reconnected_monitor_is_reported_as_restored() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);

        // 1. Отключаем MON2 и применяем эвакуацию
        let p1 = plan(&set, std::slice::from_ref(&m1));
        apply(&mut set, &p1);

        // 2. MON2 возвращается
        let p2 = plan(&set, &[m1.clone(), m2.clone()]);
        assert!(p2.orphaned.is_empty());
        assert!(p2.moves.is_empty());
        assert_eq!(p2.restored, vec![m2.clone()]);

        assert!(apply(&mut set, &p2));
        assert!(set.get_monitor(&m2).is_some());
    }

    #[test]
    fn window_does_not_duplicate_after_rebind_apply() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);

        let p = plan(&set, std::slice::from_ref(&m1));
        apply(&mut set, &p);

        // Окно w(2) должно существовать ровно один раз
        let count_w2 = set
            .monitors
            .iter()
            .flat_map(|m| &m.workspaces)
            .flat_map(|ws| ws.all_windows())
            .filter(|&k| k == w(2))
            .count();
        assert_eq!(count_w2, 1);
    }

    #[test]
    fn find_window_finds_window_before_and_after_rebind() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);

        // До перепривязки w(2) находится на MON2
        let loc_before = set.find_window(w(2)).unwrap();
        assert_eq!(loc_before.monitor, m2);

        let p = plan(&set, std::slice::from_ref(&m1));
        apply(&mut set, &p);

        // После перепривязки w(2) находится на MON1
        let loc_after = set.find_window(w(2)).unwrap();
        assert_eq!(loc_after.monitor, m1);
    }

    #[test]
    fn orphaned_monitor_workspaces_are_preserved_in_workspace_set() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);
        set.switch_to(&m2, WorkspaceId(3));

        let p = plan(&set, std::slice::from_ref(&m1));
        apply(&mut set, &p);

        // Структура воркспейсов MON2 не удалена из памяти
        let m2_entry = set.get_monitor(&m2).expect("монитор MON2 сохранён");
        assert_eq!(m2_entry.workspaces.len(), 2);
        assert_eq!(m2_entry.active_workspace().unwrap().id, WorkspaceId(3));
    }

    #[test]
    fn unknown_new_monitor_in_present_does_not_break_planning() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m_new = mon("MON_NEW");
        set.insert_window(&m1, w(1), InsertAt::Root);

        let p = plan(&set, &[m1.clone(), m_new.clone()]);
        assert!(p.orphaned.is_empty());
        assert!(p.moves.is_empty());

        // Применение плана не падает и добавляет новый монитор
        apply(&mut set, &p);
    }

    #[test]
    fn multiple_monitors_disconnected_simultaneously_all_evacuate_to_primary() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        let m3 = mon("MON3");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);
        set.insert_window(&m3, w(3), InsertAt::Root);

        // MON2 и MON3 отключились одновременно
        let p = plan(&set, std::slice::from_ref(&m1));
        assert_eq!(p.orphaned.len(), 2);
        assert_eq!(p.moves.len(), 2);

        apply(&mut set, &p);
        assert_eq!(set.visible_windows(), vec![w(1), w(2), w(3)]);
    }

    #[test]
    fn apply_on_empty_plan_returns_false() {
        let mut set = WorkspaceSet::new();
        let empty_plan = RebindPlan::default();
        assert!(!apply(&mut set, &empty_plan));
    }

    #[test]
    fn rebind_preserves_floating_status_of_moved_windows() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);
        set.insert_window(&m2, w(2), InsertAt::Root);
        set.toggle_floating(w(2));

        let p = plan(&set, std::slice::from_ref(&m1));
        apply(&mut set, &p);

        let loc = set.find_window(w(2)).unwrap();
        assert_eq!(loc.monitor, m1);
        assert!(loc.floating, "окно сохранило плавающий статус");
    }

    #[test]
    fn inactive_tabs_in_groups_are_also_evacuated() {
        let mut set = WorkspaceSet::new();
        let m1 = mon("MON1");
        let m2 = mon("MON2");
        set.insert_window(&m1, w(1), InsertAt::Root);

        // На MON2 группа с двумя табами: w(2) активен, w(3) скрыт
        let w2_node = set.insert_window(&m2, w(2), InsertAt::Root).unwrap();
        {
            let m2_mut = set.get_monitor_mut(&m2).unwrap();
            let ws = m2_mut.active_workspace_mut().unwrap();
            let group = ws
                .tree
                .split_leaf(w2_node, ContainerLayout::Tabbed)
                .unwrap();
            ws.tree.insert_window(
                w(3),
                InsertAt::Into {
                    parent: group,
                    index: 1,
                },
            );
        }

        let p = plan(&set, std::slice::from_ref(&m1));
        assert_eq!(p.moves.len(), 2);
        assert!(p.moves.contains(&(w(2), m1.clone())));
        assert!(p.moves.contains(&(w(3), m1.clone())));

        apply(&mut set, &p);
        assert_eq!(set.find_window(w(2)).unwrap().monitor, m1);
        assert_eq!(set.find_window(w(3)).unwrap().monitor, m1);
    }
}
