//! Автомат потери монитора (SPEC.md, раздел 6.1; ADR-011;
//! docs/M3_PREP_NOTES.md, раздел 5.3): отслеживает пропажу мониторов по
//! снимкам перечисления и на каждый пропавший ведёт 20-секундный таймер —
//! скрытие стикеров, возврат «как было», миграция на текущий основной и
//! автовозврат из `origin`.
//!
//! Чистая логика: часы здесь не читаются — момент времени (`now`) в каждом
//! вызове передаётся извне, так что все четыре ветки тестируются таблицами
//! без сна. Состояние живёт в координаторе: он держит [`MonitorLossTracker`]
//! и кормит его свежим снимком мониторов при каждом изменении перечисления
//! (docs/M3_PREP_NOTES.md, раздел 2.3).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::migration::{migrate_to_monitor, return_from_origin};
use crate::model::{MonitorId, Origin, Placement, Rect, Sticker};

/// Сколько ждать возврата монитора до миграции стикеров (SPEC 6.1 п.1/п.3).
pub const LOSS_TIMEOUT: Duration = Duration::from_secs(20);

/// Снимок подключённого монитора для автомата (docs/M3_PREP_NOTES.md, §5.3):
/// стабильный id, границы в физических пикселях и флаг основного. Координатор
/// строит его из свежего перечисления (rst-win32::monitors → Config.monitors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorSnapshot {
    pub id: MonitorId,
    pub bounds_px: Rect,
    pub is_primary: bool,
}

/// Действие автомата потери монитора для координатора.
///
/// **Все варианты — системные события, а не жесты пользователя**
/// (docs/M3_PREP_NOTES.md, §5.3): координатор применяет их без снимка в
/// undo-историю — иначе `Ctrl+Z` воскрешал бы стикеры на физически
/// отсутствующем мониторе.
#[derive(Debug, Clone, PartialEq)]
pub enum LossAction {
    /// Скрыть стикер: его монитор пропал (SPEC 6.1 п.1). Видимость «до
    /// скрытия» автомат запоминает и вернёт в [`LossAction::RestoreVisibility`]
    /// или [`LossAction::Migrate`].
    HideSticker { sticker_id: Uuid },
    /// Вернуть стикеру видимость «как было»: монитор вернулся в течение
    /// таймера (п.2). `visible` — значение до скрытия.
    RestoreVisibility { sticker_id: Uuid, visible: bool },
    /// Мигрировать стикер на текущий основной: таймер истёк (п.3).
    /// `placement` — посчитанная геометрия с `monitor_id` приёмника,
    /// `origin` — «дом» для автовозврата (п.4), `visible` — как до скрытия.
    Migrate {
        sticker_id: Uuid,
        placement: Placement,
        origin: Origin,
        visible: bool,
    },
    /// Автовернуть стикер на «дом» из `origin`: монитор вернулся позже, а
    /// пользователь его не двигал (п.4). `rotation` — из `origin`; после
    /// применения координатор очищает `origin`.
    ReturnHome {
        sticker_id: Uuid,
        placement: Placement,
        rotation: f64,
    },
    /// Очистить `origin` стикера: пользователь отредактировал его `placement`
    /// (п.4) — автовозврат больше не должен трогать стикер.
    ClearOrigin { sticker_id: Uuid },
}

/// Состояние потери одного монитора.
#[derive(Debug)]
struct LossState {
    /// Момент обнаружения пропажи (`now` из того вызова — часы не читаем).
    lost_at: Instant,
    /// Последние известные границы монитора: «дом» для пропорциональной
    /// миграции (п.3).
    bounds_px: Rect,
    /// id стикеров монитора → их видимость до скрытия (п.1): она же
    /// восстанавливается при возврате (п.2) и переносится при миграции (п.3).
    visible_before: HashMap<Uuid, bool>,
}

/// Автомат потери монитора (docs/M3_PREP_NOTES.md, раздел 5.3).
///
/// Состояние — [`HashMap`] из `MonitorId` в [`LossState`]: по одному на
/// пропавший монитор. Обнаружение пропажи/возврата — по переходам между
/// последовательными снимками; мониторы, отсутствующие уже в первом снимке,
/// «пропавшими» не считаются (автомат видит только переходы).
#[derive(Debug, Default)]
pub struct MonitorLossTracker {
    losses: HashMap<MonitorId, LossState>,
    prev_snapshot: HashMap<MonitorId, MonitorSnapshot>,
    /// Стикеры, чей `placement` пользователь правил после миграции
    /// ([`Self::on_user_edit`]): автовозврат (п.4) их не трогает, даже если
    /// `origin` по какой-то причине ещё не очищен.
    edited: HashSet<Uuid>,
}

impl MonitorLossTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Прогнать автомат по свежему снимку подключённых мониторов
    /// (полный список присутствующих, с границами и флагом основного).
    /// Возвращает системные действия для применения к конфигу:
    ///
    /// - монитор, бывший в предыдущем снимке и отсутствующий в текущем, —
    ///   потерян (п.1): его стикеры скрываются, запускается таймер
    ///   [`LOSS_TIMEOUT`];
    /// - монитор вернулся в течение таймера (п.2) — видимость стикеров
    ///   возвращается «как было»;
    /// - таймер истёк (п.3) — стикеры мигрируют на текущий основной через
    ///   [`crate::migration::migrate_to_monitor`]; без основного в снимке
    ///   миграция откладывается, состояние потери сохраняется;
    /// - монитор вернулся позже (п.4) — стикеры с `origin` на него
    ///   автоматически возвращаются домой
    ///   ([`crate::migration::return_from_origin`]), если пользователь их не
    ///   двигал (origin не очищен правкой и стикер не в
    ///   [`Self::on_user_edit`]-памяти).
    ///
    /// `now` — момент прогона: часы внутри не читаются, поэтому тесты
    /// двигают время аргументом. Автовозврат перевыдаётся на каждом прогоне,
    /// пока координатор не применит [`LossAction::ReturnHome`] (и не очистит
    /// `origin`).
    pub fn on_monitor_snapshot(
        &mut self,
        snapshot: &[MonitorSnapshot],
        stickers: &[Sticker],
        now: Instant,
    ) -> Vec<LossAction> {
        let mut actions = Vec::new();
        let current: HashMap<MonitorId, &MonitorSnapshot> =
            snapshot.iter().map(|m| (m.id.clone(), m)).collect();

        // (a) Новые потери: монитор был в прошлом снимке, сейчас отсутствует.
        let lost: Vec<(MonitorId, Rect)> = self
            .prev_snapshot
            .values()
            .filter_map(|prev| {
                (!current.contains_key(&prev.id)).then_some((prev.id.clone(), prev.bounds_px))
            })
            .collect();
        for (monitor_id, bounds_px) in lost {
            let mut visible_before = HashMap::new();
            for sticker in stickers {
                if sticker.placement.monitor_id == monitor_id {
                    visible_before.insert(sticker.id, sticker.visible);
                    actions.push(LossAction::HideSticker {
                        sticker_id: sticker.id,
                    });
                }
            }
            self.losses.insert(
                monitor_id,
                LossState {
                    lost_at: now,
                    bounds_px,
                    visible_before,
                },
            );
        }

        // (b) Возврат в течение таймера: видимость «как было» — только для
        // существующих стикеров (удалённые за время пропажи не трогаем).
        let returned: Vec<MonitorId> = current
            .keys()
            .filter(|id| self.losses.contains_key(*id))
            .cloned()
            .collect();
        for monitor_id in returned {
            let Some(state) = self.losses.remove(&monitor_id) else {
                continue;
            };
            for sticker in stickers {
                let Some(visible) = state.visible_before.get(&sticker.id) else {
                    continue;
                };
                actions.push(LossAction::RestoreVisibility {
                    sticker_id: sticker.id,
                    visible: *visible,
                });
            }
        }

        // (c) Истёкшие таймеры: миграция на текущий основной. Без основного
        // в снимке мигрировать некуда — состояние потери сохраняется и
        // дождётся прогона, в котором основной появится.
        let expired: Vec<MonitorId> = self
            .losses
            .iter()
            .filter(|(_, s)| now.saturating_duration_since(s.lost_at) >= LOSS_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for monitor_id in expired {
            let Some(primary) = current.values().find(|m| m.is_primary) else {
                break;
            };
            let Some(state) = self.losses.remove(&monitor_id) else {
                continue;
            };
            for sticker in stickers {
                if sticker.placement.monitor_id != monitor_id {
                    continue;
                }
                let (mut placement, origin) = migrate_to_monitor(
                    &sticker.placement,
                    &sticker.transform,
                    &state.bounds_px,
                    &primary.bounds_px,
                );
                placement.monitor_id = primary.id.clone();
                // Стикер, добавленный на пропавший монитор уже после скрытия,
                // в visible_before отсутствует — он не скрывался, оставляем
                // видимым.
                let visible = state
                    .visible_before
                    .get(&sticker.id)
                    .copied()
                    .unwrap_or(true);
                actions.push(LossAction::Migrate {
                    sticker_id: sticker.id,
                    placement,
                    origin,
                    visible,
                });
            }
        }

        // (d) Автовозврат (п.4): монитор вернулся позже, стикер с `origin` на
        // него и пользователь его не двигал (origin не очищен правкой и id
        // нет в `edited`). Основной сигнал «не двигал» — очистка origin;
        // `migration::should_auto_return` консервативна при масштабированной
        // миграции и как ворота неприменима (см. её документацию).
        for sticker in stickers {
            let Some(origin) = &sticker.origin else {
                continue;
            };
            if self.edited.contains(&sticker.id) {
                continue;
            }
            if sticker.placement.monitor_id == origin.monitor_id {
                // Уже дома; origin — остаточный (координатор не очистил).
                continue;
            }
            if !current.contains_key(&origin.monitor_id) {
                continue;
            }
            actions.push(LossAction::ReturnHome {
                sticker_id: sticker.id,
                placement: return_from_origin(origin),
                rotation: origin.rotation,
            });
        }

        self.prev_snapshot = current.into_iter().map(|(id, m)| (id, m.clone())).collect();
        actions
    }

    /// Монитор в состоянии потери (таймер ещё идёт): координатор, например,
    /// уничтожает его оверлей-окно (docs/M3_PREP_NOTES.md, §5.3).
    pub fn is_monitor_lost(&self, monitor_id: &MonitorId) -> bool {
        self.losses.contains_key(monitor_id)
    }

    /// Сообщить о пользовательской правке `placement` стикера (жест или
    /// тулбар, там же, где применяются `transform_ops`): `origin` стикера
    /// подлежит очистке (SPEC 6.1 п.4) — отдельный триггер, не таймер и не
    /// снимок. Правка без `origin` ничего не порождает; сам факт правки
    /// запоминается на случай, если координатор очистку не применит.
    pub fn on_user_edit(&mut self, sticker: &Sticker) -> Vec<LossAction> {
        let mut actions = Vec::new();
        if sticker.origin.is_some() {
            actions.push(LossAction::ClearOrigin {
                sticker_id: sticker.id,
            });
        }
        self.edited.insert(sticker.id);
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Placement;

    fn mon(id: &str, x: i32, y: i32, w: u32, h: u32, is_primary: bool) -> MonitorSnapshot {
        MonitorSnapshot {
            id: MonitorId(id.into()),
            bounds_px: Rect { x, y, w, h },
            is_primary,
        }
    }

    fn sticker(id: u32, monitor: &str, visible: bool) -> Sticker {
        Sticker {
            id: Uuid::from_u128(id as u128),
            visible,
            placement: Placement {
                monitor_id: MonitorId(monitor.into()),
                cx: 100.0,
                cy: 200.0,
                w: 50.0,
                h: 30.0,
            },
            ..Sticker::default()
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Разогнать автомат до состояния «S пропал, стикер скрыт»: возвращает
    /// трекер, момент потери и «дом» стикера.
    fn loss_setup(
        s: &Sticker,
    ) -> (
        MonitorLossTracker,
        Instant,
        MonitorSnapshot,
        MonitorSnapshot,
    ) {
        let primary = mon("P", 0, 0, 3840, 2160, true);
        let secondary = mon("S", 0, 0, 1920, 1080, false);
        let mut t = MonitorLossTracker::new();
        let t0 = Instant::now();
        let _ = t.on_monitor_snapshot(
            &[primary.clone(), secondary.clone()],
            std::slice::from_ref(s),
            t0,
        );
        let _ = t.on_monitor_snapshot(
            std::slice::from_ref(&primary),
            std::slice::from_ref(s),
            t0 + secs(1),
        );
        (t, t0, primary, secondary)
    }

    /// Извлечь единственную миграцию из действий.
    fn expect_migrate(actions: Vec<LossAction>) -> LossAction {
        let mut iter = actions.into_iter();
        let action = iter.next().expect("ожидалась миграция");
        assert!(iter.next().is_none(), "действие должно быть ровно одно");
        action
    }

    #[test]
    fn newly_absent_monitor_hides_its_stickers_and_starts_timer() {
        // Ветка (a): скрываются ВСЕ стикеры пропавшего монитора (видимость
        // запоминается «как была»), стикеры других мониторов не трогаются.
        let primary = mon("P", 0, 0, 1920, 1080, true);
        let secondary = mon("S", 1920, 0, 1920, 1080, false);
        let s_primary = sticker(1, "P", true);
        let s_secondary = sticker(2, "S", true);
        let s_secondary_hidden = sticker(3, "S", false);

        let mut t = MonitorLossTracker::new();
        let t0 = Instant::now();
        let all = [
            s_primary.clone(),
            s_secondary.clone(),
            s_secondary_hidden.clone(),
        ];
        let _ = t.on_monitor_snapshot(&[primary.clone(), secondary.clone()], &all, t0);

        let actions = t.on_monitor_snapshot(std::slice::from_ref(&primary), &all, t0 + secs(1));
        assert_eq!(
            actions,
            vec![
                LossAction::HideSticker {
                    sticker_id: s_secondary.id
                },
                LossAction::HideSticker {
                    sticker_id: s_secondary_hidden.id
                },
            ]
        );
        assert!(t.is_monitor_lost(&MonitorId("S".into())), "таймер идёт");
        assert!(
            !t.is_monitor_lost(&MonitorId("P".into())),
            "оставшийся монитор не в потере"
        );
    }

    #[test]
    fn return_within_timeout_restores_visibility_as_was() {
        // Ветка (b), таблица: видимость до пропажи → видимость после
        // возврата («без изменений», SPEC 6.1 п.2).
        let cases = [(1u32, true), (2, false), (3, true)];
        for (id, visible_before) in cases {
            let s = sticker(id, "S", visible_before);
            let (mut t, t0, primary, secondary) = loss_setup(&s);

            let actions = t.on_monitor_snapshot(
                &[primary, secondary],
                std::slice::from_ref(&s),
                t0 + secs(10),
            );
            assert_eq!(
                actions,
                vec![LossAction::RestoreVisibility {
                    sticker_id: s.id,
                    visible: visible_before,
                }],
                "id={id}, visible_before={visible_before}"
            );
            assert!(
                !t.is_monitor_lost(&MonitorId("S".into())),
                "таймер снят после возврата"
            );
        }
    }

    #[test]
    fn expired_timer_migrates_to_current_primary() {
        // Ветка (c), таблица: видимость до пропажи → видимость после
        // миграции; геометрия — пропорциональный перенос на текущий основной.
        let cases = [(1u32, true), (2, false)];
        for (id, visible_before) in cases {
            let home = sticker(id, "S", visible_before);
            let (mut t, t0, _primary, _secondary) = loss_setup(&home);

            let actions = t.on_monitor_snapshot(
                &[mon("P", 0, 0, 3840, 2160, true)],
                std::slice::from_ref(&home),
                t0 + secs(21),
            );
            match expect_migrate(actions) {
                LossAction::Migrate {
                    sticker_id,
                    placement,
                    origin,
                    visible,
                } => {
                    assert_eq!(sticker_id, home.id);
                    // Приёмник — текущий основной; «дом» сохранён для
                    // автовозврата.
                    assert_eq!(placement.monitor_id, MonitorId("P".into()));
                    assert_eq!(origin.monitor_id, MonitorId("S".into()));
                    assert_eq!(origin.cx, home.placement.cx);
                    assert_eq!(origin.cy, home.placement.cy);
                    // Пропорциональное масштабирование 1920x1080 → 3840x2160
                    // (×2), правило границ внутри монитора.
                    assert_eq!(placement.cx, 200.0);
                    assert_eq!(placement.cy, 400.0);
                    assert_eq!(placement.w, 100.0);
                    assert_eq!(placement.h, 60.0);
                    assert_eq!(visible, visible_before, "видимость — как до пропажи");
                }
                other => panic!("ожидалась Migrate, получено: {other:?}"),
            }
            assert!(
                !t.is_monitor_lost(&MonitorId("S".into())),
                "состояние снято после миграции"
            );
        }
    }

    #[test]
    fn timeout_expires_at_exactly_20_seconds() {
        // Граница таймера: за наносекунду до 20 с — ещё не истёк, ровно
        // на 20-й секунде — миграция.
        let s = sticker(1, "S", true);
        let (mut t, t0, _primary, _secondary) = loss_setup(&s);
        let lost_at = t0 + secs(1);
        let present = [mon("P", 0, 0, 3840, 2160, true)];

        let actions = t.on_monitor_snapshot(
            &present,
            std::slice::from_ref(&s),
            lost_at + secs(20) - Duration::from_nanos(1),
        );
        assert!(actions.is_empty(), "до 20 с миграции нет");
        assert!(t.is_monitor_lost(&MonitorId("S".into())));

        let actions = t.on_monitor_snapshot(&present, std::slice::from_ref(&s), lost_at + secs(20));
        assert!(
            matches!(&actions[..], [LossAction::Migrate { .. }]),
            "ровно на 20-й секунде: {actions:?}"
        );
    }

    #[test]
    fn expired_timer_without_primary_defers_migration() {
        // Ветка (c), край: основного в снимке нет — мигрировать некуда,
        // состояние потери сохраняется и срабатывает при появлении основного.
        let s = sticker(1, "S", true);
        let (mut t, t0, _primary, _secondary) = loss_setup(&s);

        let actions = t.on_monitor_snapshot(&[], std::slice::from_ref(&s), t0 + secs(21));
        assert!(actions.is_empty());
        assert!(t.is_monitor_lost(&MonitorId("S".into())));

        let actions = t.on_monitor_snapshot(
            &[mon("P", 0, 0, 3840, 2160, true)],
            std::slice::from_ref(&s),
            t0 + secs(22),
        );
        assert!(
            matches!(&actions[..], [LossAction::Migrate { sticker_id, .. }] if *sticker_id == s.id),
            "{actions:?}"
        );
    }

    #[test]
    fn late_return_auto_returns_untouched_stickers_home() {
        // Ветка (d): полный цикл п.1–п.4 — пропажа, миграция, поздний
        // возврат, автовозврат домой из origin.
        let home = sticker(1, "S", true);
        let (mut t, t0, primary, secondary) = loss_setup(&home);
        let actions = t.on_monitor_snapshot(
            std::slice::from_ref(&primary),
            std::slice::from_ref(&home),
            t0 + secs(21),
        );
        let action = expect_migrate(actions);
        let LossAction::Migrate {
            placement, origin, ..
        } = action
        else {
            panic!("ожидалась Migrate, получено: {action:?}")
        };

        // Координатор применил миграцию: стикер на основном, origin — «дом».
        let mut moved = home.clone();
        moved.placement = placement;
        moved.origin = Some(origin);

        // Через час монитор вернулся; стикер не двигали — автовозврат.
        let actions = t.on_monitor_snapshot(
            &[primary, secondary],
            std::slice::from_ref(&moved),
            t0 + secs(3600),
        );
        assert_eq!(
            actions,
            vec![LossAction::ReturnHome {
                sticker_id: moved.id,
                placement: home.placement,
                rotation: home.transform.rotation,
            }]
        );
    }

    #[test]
    fn user_edit_clears_origin_and_prevents_auto_return() {
        // Ветка (d), часть 2: правка placement очищает origin (отдельный
        // триггер, не таймер) — автовозврат после возврата монитора не
        // срабатывает.
        let home = sticker(1, "S", true);
        let (mut t, t0, primary, secondary) = loss_setup(&home);
        let actions = t.on_monitor_snapshot(
            std::slice::from_ref(&primary),
            std::slice::from_ref(&home),
            t0 + secs(21),
        );
        let action = expect_migrate(actions);
        let LossAction::Migrate {
            placement, origin, ..
        } = action
        else {
            panic!("ожидалась Migrate, получено: {action:?}")
        };
        let mut moved = home.clone();
        moved.placement = placement;
        moved.origin = Some(origin);

        // Пользователь двигает стикер на основном: ClearOrigin + запоминание.
        let actions = t.on_user_edit(&moved);
        assert_eq!(
            actions,
            vec![LossAction::ClearOrigin {
                sticker_id: moved.id
            }]
        );
        moved.origin = None; // координатор применил очистку

        // Монитор вернулся позже — автовозврата нет, стикер остаётся на
        // основном.
        let actions = t.on_monitor_snapshot(
            &[primary, secondary],
            std::slice::from_ref(&moved),
            t0 + secs(3600),
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn user_edit_is_remembered_even_if_clear_not_applied() {
        // Страховка п.4: координатор «забыл» применить ClearOrigin — сам
        // факт правки (edited-память) не даёт автовозврату сработать.
        let home = sticker(1, "S", true);
        let (mut t, t0, primary, secondary) = loss_setup(&home);
        let actions = t.on_monitor_snapshot(
            std::slice::from_ref(&primary),
            std::slice::from_ref(&home),
            t0 + secs(21),
        );
        let action = expect_migrate(actions);
        let LossAction::Migrate {
            placement, origin, ..
        } = action
        else {
            panic!("ожидалась Migrate, получено: {action:?}")
        };
        let mut moved = home.clone();
        moved.placement = placement;
        moved.origin = Some(origin);

        let _ = t.on_user_edit(&moved); // origin НЕ очищаем — только факт правки

        let actions = t.on_monitor_snapshot(
            &[primary, secondary],
            std::slice::from_ref(&moved),
            t0 + secs(3600),
        );
        assert!(actions.is_empty(), "edited-память блокирует автовозврат");
    }

    #[test]
    fn user_edit_without_origin_is_noop() {
        let mut t = MonitorLossTracker::new();
        let s = sticker(1, "P", true);
        assert!(t.on_user_edit(&s).is_empty(), "без origin очищать нечего");
    }

    #[test]
    fn first_snapshot_does_not_report_losses() {
        // Автомат видит только переходы: монитор, отсутствующий уже в первом
        // снимке, «пропавшим» не считается.
        let s = sticker(1, "S", true);
        let mut t = MonitorLossTracker::new();
        let actions = t.on_monitor_snapshot(
            &[mon("P", 0, 0, 1920, 1080, true)],
            std::slice::from_ref(&s),
            Instant::now(),
        );
        assert!(actions.is_empty());
        assert!(!t.is_monitor_lost(&MonitorId("S".into())));
    }

    #[test]
    fn deleted_sticker_is_not_restored_or_migrated() {
        // Стикер удалили, пока монитор пропадал: возврат и миграция его не
        // трогают (действия ссылаются только на существующие стикеры).
        let s = sticker(1, "S", true);
        let (mut t, t0, primary, secondary) = loss_setup(&s);

        let actions = t.on_monitor_snapshot(&[primary.clone(), secondary], &[], t0 + secs(5));
        assert!(actions.is_empty(), "возврат без стикера: {actions:?}");

        let actions = t.on_monitor_snapshot(&[primary], &[], t0 + secs(25));
        assert!(actions.is_empty(), "миграция без стикера: {actions:?}");
    }
}
