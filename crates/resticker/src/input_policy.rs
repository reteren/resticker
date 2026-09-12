//! Чистое решение о том, какие области каждого монитора принимают мышь.
//!
//! Здесь нет состояния окон и побочных эффектов: координатор передаёт полный
//! снимок состояния, а получает независимое решение для каждого монитора.

#![allow(dead_code)]

/// Номер монитора в снимке координатора — индекс, а не
/// `rst_core::model::MonitorId`.
///
/// Отдельный тип с отдельным именем намеренно: в этом же крейте живёт
/// настоящий `MonitorId(String)`, и два разных типа под одним именем — это
/// ловушка, в которой однажды перепутают один с другим. Координатор
/// переводит свои мониторы в номера сам.
///
/// Номера должны быть уникальны в одном вызове [`resolve_input_policies`],
/// иначе невозможно однозначно выбрать владельца панели или инициатора
/// режима.
pub type MonitorIndex = u32;

/// Прямоугольник `(x, y, width, height)` в физических пикселях.
pub type Rect = (i32, i32, i32, i32);

/// Как окну оверлея ловить мышь.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPolicy {
    /// Кликопрозрачно целиком: мышь идёт сквозь окно, захвата нет.
    Transparent,
    /// Интерактивно целиком. `take_focus` — забрать фокус (только инициатор).
    Interactive { take_focus: bool },
    /// Ловить мышь только в этих прямоугольниках, остальное — сквозь.
    /// Захват мыши в этом режиме запрещён.
    HitRects(Vec<Rect>),
    /// Ловить мышь всем окном без фокуса, захват разрешён — для полосы
    /// перемотки видео.
    ///
    /// Не `HitRects` с прямоугольником полосы: ползунок тащат, а
    /// перетаскиванию нужен захват, запрещённый в `HitRects`. Без него
    /// нажатие и отпускание пошли бы разными путями, флаг «тащат» мог бы не
    /// сброситься, и окно осталось бы интерактивным на весь монитор — мышь на
    /// нём была бы мертва. Режим воспроизводит прежнее рабочее поведение
    /// полосы (`set_hover_click_target(true)`) бит в бит.
    HoverTarget,
}

/// Прямоугольники живых кусков на одном мониторе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInput {
    /// Уникальный идентификатор монитора.
    pub id: MonitorIndex,
    /// Области кусков в клиентских координатах и физических пикселях.
    pub piece_rects: Vec<Rect>,
}

/// Полоса перемотки, которая может быть добавлена к областям своего монитора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineInput {
    /// Монитор, на котором находится полоса.
    pub monitor: MonitorIndex,
    /// Прямоугольник полосы в клиентских координатах.
    pub rect: Rect,
    /// Курсор находится над полосой в текущем снимке.
    pub under_cursor: bool,
}

/// Полный снимок входных данных, необходимый для выбора политики.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputState {
    /// Активен обычный режим редактирования.
    pub editing: bool,
    /// Идёт полноэкранная резка окна или выделение куска.
    pub fullscreen: bool,
    /// Инициатор режима, которому можно передать фокус.
    pub initiator: Option<MonitorIndex>,
    /// Монитор, чья панель сейчас открыта.
    pub panel_monitor: Option<MonitorIndex>,
    /// Полоса под курсором (или полоса, которую удерживает перетаскивание).
    pub timeline: Option<TimelineInput>,
    /// Перетаскивание удерживает область полосы даже после ухода курсора.
    pub timeline_dragging: bool,
    /// Сессия заблокирована: ввод не должен переживать блокировку.
    pub session_locked: bool,
    /// Система уходит в сон: ввод не должен переживать переход состояния.
    pub system_suspending: bool,
    /// Геометрия hole-настройки не является владельцем ввода и намеренно
    /// не влияет на политику.
    pub settings_rect: Option<Rect>,
}

/// Выход политики с привязкой к монитору.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorPolicy {
    /// Идентификатор монитора из входного снимка.
    pub monitor: MonitorIndex,
    /// Решение для окна этого монитора.
    pub policy: InputPolicy,
}

/// Решить, как каждое окно оверлея принимает мышь в текущей итерации.
///
/// Приоритеты намеренно проверяются сверху вниз. Блокировка и сон обязаны
/// снять ввод везде, чтобы захват не пережил системное состояние. Режимы,
/// которым нужен полный поток событий, получают интерактивное окно целиком;
/// панель получает единственного владельца; только после этого можно безопасно
/// объединять независимые области полосы и кусков в один список.
pub fn resolve_input_policies(monitors: &[MonitorInput], state: &InputState) -> Vec<MonitorPolicy> {
    let focus_index = state
        .initiator
        .and_then(|initiator| monitors.iter().position(|monitor| monitor.id == initiator));
    let panel_index = state
        .panel_monitor
        .and_then(|panel| monitors.iter().position(|monitor| monitor.id == panel));

    monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let policy = if state.session_locked || state.system_suspending {
                // Блокировка/сон имеют абсолютный приоритет: иначе старое
                // состояние захвата может остаться активным на всём столе.
                InputPolicy::Transparent
            } else if state.editing || state.fullscreen {
                // Редактор и полноэкранная резка требуют событий за пределами
                // отдельных областей; только инициатор получает фокус.
                InputPolicy::Interactive {
                    take_focus: focus_index == Some(index),
                }
            } else if state.panel_monitor.is_some() {
                if panel_index == Some(index) {
                    // Панель — единственный интерактивный владелец, поэтому
                    // два окна не будут одновременно бороться за фокус.
                    InputPolicy::Interactive { take_focus: true }
                } else {
                    // Открытая панель имеет приоритет над timeline и кусками
                    // на всех остальных мониторах.
                    InputPolicy::Transparent
                }
            } else if state.timeline.is_some_and(|timeline| {
                (timeline.under_cursor || state.timeline_dragging)
                    && timeline.monitor == monitor.id
                    && is_non_empty(timeline.rect)
            }) {
                // Полоса перемотки выше областей кусков: пока её трогают,
                // окну нужно ловить мышь целиком и держать захват ради
                // перетаскивания ползунка. Куски на этом мониторе в это время
                // тоже ловят мышь — окно интерактивно целиком, и клики по их
                // полосе разберёт координатор.
                InputPolicy::HoverTarget
            } else {
                let rects: Vec<Rect> = monitor
                    .piece_rects
                    .iter()
                    .copied()
                    .filter(|rect| is_non_empty(*rect))
                    .collect();

                if rects.is_empty() {
                    // Пустой список нельзя выдавать как HitRects: он должен
                    // оставлять окно полностью прозрачным.
                    InputPolicy::Transparent
                } else {
                    InputPolicy::HitRects(rects)
                }
            };

            MonitorPolicy {
                monitor: monitor.id,
                policy,
            }
        })
        .collect()
}

/// Нулевые или отрицательные размеры не описывают область, которую можно
/// безопасно принимать: отбрасываем их до выбора между HitRects и Transparent.
fn is_non_empty((_, _, width, height): Rect) -> bool {
    width > 0 && height > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: MonitorIndex, piece_rects: &[(i32, i32, i32, i32)]) -> MonitorInput {
        MonitorInput {
            id,
            piece_rects: piece_rects.to_vec(),
        }
    }

    fn policies(monitors: &[MonitorInput], state: InputState) -> Vec<MonitorPolicy> {
        resolve_input_policies(monitors, &state)
    }

    fn policy(result: &[MonitorPolicy], id: MonitorIndex) -> &InputPolicy {
        &result
            .iter()
            .find(|entry| entry.monitor == id)
            .expect("test monitor must have a policy")
            .policy
    }

    #[test]
    fn lock_wins_over_every_other_source_of_input() {
        let result = policies(
            &[monitor(1, &[(1, 2, 3, 4)]), monitor(2, &[])],
            InputState {
                editing: true,
                fullscreen: true,
                initiator: Some(1),
                panel_monitor: Some(2),
                timeline: Some(TimelineInput {
                    monitor: 1,
                    rect: (10, 10, 20, 20),
                    under_cursor: true,
                }),
                timeline_dragging: true,
                session_locked: true,
                system_suspending: false,
                settings_rect: Some((0, 0, 1, 1)),
            },
        );

        assert_eq!(policy(&result, 1), &InputPolicy::Transparent);
        assert_eq!(policy(&result, 2), &InputPolicy::Transparent);
    }

    #[test]
    fn suspending_wins_even_when_session_is_not_locked() {
        let result = policies(
            &[monitor(1, &[(1, 2, 3, 4)])],
            InputState {
                editing: true,
                system_suspending: true,
                initiator: Some(1),
                ..InputState::default()
            },
        );

        assert_eq!(policy(&result, 1), &InputPolicy::Transparent);
    }

    #[test]
    fn editing_beats_piece_rects_and_focuses_only_the_initiator() {
        let result = policies(
            &[monitor(1, &[(1, 2, 3, 4)]), monitor(2, &[(5, 6, 7, 8)])],
            InputState {
                editing: true,
                initiator: Some(2),
                ..InputState::default()
            },
        );

        assert_eq!(
            policy(&result, 1),
            &InputPolicy::Interactive { take_focus: false }
        );
        assert_eq!(
            policy(&result, 2),
            &InputPolicy::Interactive { take_focus: true }
        );
    }

    #[test]
    fn fullscreen_mode_beats_piece_rects_and_focuses_initiator() {
        let result = policies(
            &[monitor(1, &[(1, 2, 3, 4)]), monitor(2, &[])],
            InputState {
                fullscreen: true,
                initiator: Some(1),
                ..InputState::default()
            },
        );

        assert_eq!(
            policy(&result, 1),
            &InputPolicy::Interactive { take_focus: true }
        );
        assert_eq!(
            policy(&result, 2),
            &InputPolicy::Interactive { take_focus: false }
        );
    }

    #[test]
    fn open_panel_beats_timeline_and_has_one_focus_owner() {
        let result = policies(
            &[monitor(1, &[]), monitor(2, &[])],
            InputState {
                panel_monitor: Some(2),
                timeline: Some(TimelineInput {
                    monitor: 1,
                    rect: (10, 10, 20, 20),
                    under_cursor: true,
                }),
                ..InputState::default()
            },
        );

        assert_eq!(policy(&result, 1), &InputPolicy::Transparent);
        assert_eq!(
            policy(&result, 2),
            &InputPolicy::Interactive { take_focus: true }
        );
        assert_eq!(
            result
                .iter()
                .filter(|entry| matches!(
                    entry.policy,
                    InputPolicy::Interactive { take_focus: true }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn active_timeline_takes_its_monitor_whole_and_leaves_others_alone() {
        // Полоса под курсором — окно её монитора ловит мышь целиком
        // (`HoverTarget`), с захватом ради перетаскивания ползунка. Куски на
        // том же мониторе при этом тоже ловят мышь: окно интерактивно
        // целиком. Соседний монитор полоса не трогает — там свои области.
        let result = policies(
            &[monitor(1, &[(1, 2, 3, 4)]), monitor(2, &[(5, 6, 7, 8)])],
            InputState {
                timeline: Some(TimelineInput {
                    monitor: 1,
                    rect: (10, 11, 12, 13),
                    under_cursor: true,
                }),
                ..InputState::default()
            },
        );

        assert_eq!(policy(&result, 1), &InputPolicy::HoverTarget);
        assert_eq!(
            policy(&result, 2),
            &InputPolicy::HitRects(vec![(5, 6, 7, 8)])
        );
    }

    #[test]
    fn removing_timeline_does_not_remove_piece_rects() {
        let result = policies(
            &[monitor(1, &[(1, 2, 3, 4)])],
            InputState {
                timeline: None,
                ..InputState::default()
            },
        );

        assert_eq!(
            policy(&result, 1),
            &InputPolicy::HitRects(vec![(1, 2, 3, 4)])
        );
    }

    #[test]
    fn timeline_drag_keeps_its_area_after_cursor_leaves() {
        let result = policies(
            &[monitor(1, &[])],
            InputState {
                timeline: Some(TimelineInput {
                    monitor: 1,
                    rect: (10, 11, 12, 13),
                    under_cursor: false,
                }),
                timeline_dragging: true,
                ..InputState::default()
            },
        );

        // Курсор уже ушёл с полосы, но ползунок ещё тащат: захват обязан
        // держаться до отпускания. Уход в `HitRects` посреди жеста отнял бы
        // захват, и отпускание могло бы не прийти вовсе.
        assert_eq!(policy(&result, 1), &InputPolicy::HoverTarget);
    }

    #[test]
    fn no_areas_and_empty_rects_are_transparent() {
        let result = policies(
            &[monitor(1, &[(0, 0, 0, 10), (0, 0, 10, 0)]), monitor(2, &[])],
            InputState {
                settings_rect: Some((1, 2, 3, 4)),
                ..InputState::default()
            },
        );

        assert_eq!(policy(&result, 1), &InputPolicy::Transparent);
        assert_eq!(policy(&result, 2), &InputPolicy::Transparent);
    }

    #[test]
    fn settings_rect_does_not_become_an_input_owner() {
        let monitors = [monitor(1, &[])];
        let without_settings = policies(&monitors, InputState::default());
        let with_settings = policies(
            &monitors,
            InputState {
                settings_rect: Some((1, 2, 30, 40)),
                ..InputState::default()
            },
        );

        assert_eq!(without_settings, with_settings);
        assert_eq!(policy(&with_settings, 1), &InputPolicy::Transparent);
    }
}
