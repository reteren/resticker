//! Слой сведения (reconciliation) желаемой раскладки с наблюдаемым состоянием окон
//! (docs/TILING_DESIGN.md §3 «Три ловушки, которые убьют реализацию», Ловушки 1 и 2).
//!
//! # Назначение и защита от петли
//!
//! Координатор не должен перемещать окна вслепую на каждый чих трекера.
//! Если послать окну `SetWindowPos`, Windows через 16 мс присылает событие
//! `EVENT_OBJECT_LOCATIONCHANGE` (`rst_win32::window_tracker`), координатор видит новую
//! геометрию и, при наивной логике, снова вызвал бы перестановку — это классическая
//! 60-герцовая петля обратной связи (ранее случавшаяся в `window_pin.rs:119`).
//!
//! Данный модуль решает две ключевые задачи:
//! 1. **Дифф с допуском (`reconcile`)**: вычисляет минимально необходимый набор
//!    перестановок (`Move`) и изменений видимости (`show`/`hide`), полностью игнорируя
//!    расхождения в пределах `epsilon_px` (окна со встроенным шагом сетки, терминалы,
//!    минимальные размеры).
//! 2. **Подавление эха (`EchoGuard`)**: запоминает выданные координаты и гасит
//!    входящие системные события от собственных перемещений, позволяя отличить наше
//!    эхо от реального перетаскивания окна пользователем (защита от ложного snap-back).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::Rect;
use crate::tiling::layout::Placement;
use crate::tiling::tree::WindowKey;

/// Где окно находится СЕЙЧАС по данным снимка координатора (`window_tracker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub window: WindowKey,
    pub rect: Rect,
}

/// Одно запланированное перемещение / изменение размера окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub window: WindowKey,
    pub from: Rect,
    pub to: Rect,
}

/// Параметры сведения раскладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileParams {
    /// Расхождение в пикселях по любой из 4 координат (x, y, w, h), которое
    /// считается совпадением.
    ///
    /// Ноль недопустим на практике: чужие приложения сами клампят размеры
    /// (например, терминалы с шагом символьной сетки, окна с `WM_GETMINMAXINFO`),
    /// и строгого попиксельного совпадения может не быть никогда.
    pub epsilon_px: i32,
    /// Максимальное число окон, переставляемых за один цикл.
    /// Защита от лавины одновременных тяжелых Win32-вызовов `SetWindowPos`.
    pub max_moves: usize,
}

impl Default for ReconcileParams {
    fn default() -> Self {
        Self {
            epsilon_px: 2,
            max_moves: 8,
        }
    }
}

/// План действий координатора по итогам сведения желаемого с действительным.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Список окон, требующих физического перемещения / ресайза.
    pub moves: Vec<Move>,
    /// Окна, которые требуется отобразить (стали активным табом группы).
    pub show: Vec<WindowKey>,
    /// Окна, которые требуется скрыть (ушли в фон группы) — координатор прячет их через DWM-cloak.
    pub hide: Vec<WindowKey>,
}

/// Сравнить желаемую раскладку с наблюдаемой и вернуть минимальный план действий.
///
/// # Правила сведения:
///
/// 1. Окна с расхождением геометрии `<= epsilon_px` не включаются в `moves` (главная
///    защита от циклической петли перекладок).
/// 2. Окно, присутствующее в `target`, но отсутствующее в `observed`, в `moves` не попадает:
///    координатор ещё не получил его физический прямоугольник от системы (окно может
///    быть только что создано или свёрнуто). Оно будет обработано следующим снимком.
/// 3. Окна из `observed`, которых нет в `target`, не трогаются (плавающие, сторонние окна).
/// 4. Изменения видимости (`show`/`hide`) формируются строго по разнице между `Placement::visible`
///    и текущим срезом `visible_now`.
/// 5. Если число требуемых перемещений превышает `max_moves`, первыми выбираются окна с
///    **наибольшим расстоянием** между текущим и целевым положением (наиболее заметные
///    глазу ошибки исправляются в первую очередь, мелкие подгонки откладываются).
pub fn reconcile(
    observed: &[Observed],
    target: &[Placement],
    visible_now: &[WindowKey],
    params: &ReconcileParams,
) -> Plan {
    let observed_map: HashMap<WindowKey, Rect> =
        observed.iter().map(|o| (o.window, o.rect)).collect();
    let visible_set: HashSet<WindowKey> = visible_now.iter().copied().collect();

    let mut candidate_moves = Vec::new();
    let mut show = Vec::new();
    let mut hide = Vec::new();

    let mut seen_show = HashSet::new();
    let mut seen_hide = HashSet::new();

    for placement in target {
        // Проверяем изменение видимости
        if placement.visible {
            if !visible_set.contains(&placement.window) && seen_show.insert(placement.window) {
                show.push(placement.window);
            }
        } else if visible_set.contains(&placement.window) && seen_hide.insert(placement.window) {
            hide.push(placement.window);
        }

        // Проверяем необходимость перемещения
        if let Some(&current_rect) = observed_map.get(&placement.window) {
            if !rects_match(current_rect, placement.rect, params.epsilon_px) {
                let dist = distance(&current_rect, &placement.rect);
                candidate_moves.push((
                    dist,
                    Move {
                        window: placement.window,
                        from: current_rect,
                        to: placement.rect,
                    },
                ));
            }
        }
    }

    // Если перемещений больше лимита, сортируем по убыванию расстояния:
    // окна, уехавшие дальше всего, переставляются первыми.
    if candidate_moves.len() > params.max_moves {
        candidate_moves.sort_by_key(|m| std::cmp::Reverse(m.0));
    }

    let moves: Vec<Move> = candidate_moves
        .into_iter()
        .take(params.max_moves)
        .map(|(_, m)| m)
        .collect();

    Plan { moves, show, hide }
}

/// Память о собственных перестановках координатора.
///
/// Предотвращает ложное срабатывание move-lock snap-back'а и исключает повторную
/// реакцию на системные эхо-события `EVENT_OBJECT_LOCATIONCHANGE`.
#[derive(Debug, Clone)]
pub struct EchoGuard {
    entries: HashMap<WindowKey, EchoEntry>,
    ttl_ms: u64,
    epsilon_px: i32,
}

#[derive(Debug, Clone, Copy)]
struct EchoEntry {
    target_rect: Rect,
    recorded_at: u64,
}

impl EchoGuard {
    /// Создать новый страж эха с заданным временем жизни записей (мс) и допуском (пкс).
    pub fn new(ttl_ms: u64, epsilon_px: i32) -> Self {
        Self {
            entries: HashMap::new(),
            ttl_ms,
            epsilon_px: epsilon_px.max(0),
        }
    }

    /// Запомнить выданные перестановки в момент времени `now_ms`.
    ///
    /// Повторные записи по тому же окну перезаписывают предыдущую цель и обновляют метку времени.
    pub fn record(&mut self, moves: &[Move], now_ms: u64) {
        for m in moves {
            self.entries.insert(
                m.window,
                EchoEntry {
                    target_rect: m.to,
                    recorded_at: now_ms,
                },
            );
        }
    }

    /// Проверить, является ли входящее изменение геометрии окна нашим собственным эхом.
    ///
    /// Возвращает `true`, если для окна есть живая запись по TTL и фактический прямоугольник
    /// совпадает с целевым в пределах `epsilon_px`.
    ///
    /// # Однократное гашение (Consume on Match)
    ///
    /// При успешном совпадении запись **удаляется** из памяти. Это принципиально:
    /// один вызов `SetWindowPos` порождает ровно одно ожидаемое эхо от системы.
    /// Если то же окно сдвинется повторно (например, пользователь начал перетаскивание
    /// в ту же точку), это уже не наше эхо, а новое внешнее воздействие.
    pub fn is_echo(&mut self, window: WindowKey, rect: Rect, now_ms: u64) -> bool {
        let Some(entry) = self.entries.get(&window).copied() else {
            return false;
        };

        // Запись протухла по TTL
        if now_ms.saturating_sub(entry.recorded_at) > self.ttl_ms {
            self.entries.remove(&window);
            return false;
        }

        // Совпадает ли геометрия
        if rects_match(rect, entry.target_rect, self.epsilon_px) {
            // Гасим запись — эхо подтверждено и закрыто
            self.entries.remove(&window);
            true
        } else {
            // Геометрия не совпала — окно сдвинуто пользователем или сторонним процессом
            false
        }
    }

    /// Удалить все записи, чей срок жизни истек к моменту `now_ms`.
    pub fn prune(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        self.entries
            .retain(|_, entry| now_ms.saturating_sub(entry.recorded_at) <= ttl);
    }

    /// Число активных ожидающих записей.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Пуст ли список ожидаемых эхо-событий.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Проверка совпадения прямоугольников с допуском `epsilon`.
pub(crate) fn rects_match(a: Rect, b: Rect, epsilon: i32) -> bool {
    let eps = epsilon.max(0);
    (a.x - b.x).abs() <= eps
        && (a.y - b.y).abs() <= eps
        && ((a.w as i32) - (b.w as i32)).abs() <= eps
        && ((a.h as i32) - (b.h as i32)).abs() <= eps
}

/// Манхэттенское расстояние расхождения геометрии между двумя прямоугольниками (в пикселях).
pub(crate) fn distance(a: &Rect, b: &Rect) -> i64 {
    let dx = (a.x - b.x).abs() as i64;
    let dy = (a.y - b.y).abs() as i64;
    let dw = ((a.w as i32) - (b.w as i32)).abs() as i64;
    let dh = ((a.h as i32) - (b.h as i32)).abs() as i64;
    dx + dy + dw + dh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn default_params() -> ReconcileParams {
        ReconcileParams {
            epsilon_px: 2,
            max_moves: 8,
        }
    }

    #[test]
    fn rect_match_within_epsilon_produces_no_move() {
        let obs = [Observed {
            window: w(1),
            rect: r(10, 20, 800, 600),
        }];
        // Расхождение на 1px и 2px (при epsilon = 2) считается совпадением
        let target = [Placement {
            window: w(1),
            rect: r(11, 22, 799, 598),
            visible: true,
        }];
        let plan = reconcile(&obs, &target, &[w(1)], &default_params());
        assert!(
            plan.moves.is_empty(),
            "в пределах эпсилона перестановка не генерируется"
        );
    }

    #[test]
    fn rect_mismatch_exceeding_epsilon_produces_move() {
        let obs = [Observed {
            window: w(1),
            rect: r(10, 20, 800, 600),
        }];
        let target = [Placement {
            window: w(1),
            rect: r(50, 20, 800, 600),
            visible: true,
        }];
        let plan = reconcile(&obs, &target, &[w(1)], &default_params());
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(
            plan.moves[0],
            Move {
                window: w(1),
                from: r(10, 20, 800, 600),
                to: r(50, 20, 800, 600),
            }
        );
    }

    #[test]
    fn exact_epsilon_difference_is_treated_as_match() {
        let obs = [Observed {
            window: w(1),
            rect: r(100, 100, 500, 500),
        }];
        let target = [Placement {
            window: w(1),
            rect: r(102, 100, 500, 500), // разница ровно 2px при epsilon = 2
            visible: true,
        }];
        let plan = reconcile(&obs, &target, &[w(1)], &default_params());
        assert!(plan.moves.is_empty());
    }

    #[test]
    fn epsilon_plus_one_difference_produces_move() {
        let obs = [Observed {
            window: w(1),
            rect: r(100, 100, 500, 500),
        }];
        let target = [Placement {
            window: w(1),
            rect: r(103, 100, 500, 500), // разница 3px при epsilon = 2
            visible: true,
        }];
        let plan = reconcile(&obs, &target, &[w(1)], &default_params());
        assert_eq!(plan.moves.len(), 1);
    }

    #[test]
    fn window_only_in_target_is_ignored_by_moves() {
        // Окно W(2) есть в целевой раскладке, но ещё не появилось в observed снимке
        let obs = [Observed {
            window: w(1),
            rect: r(0, 0, 500, 500),
        }];
        let target = [
            Placement {
                window: w(1),
                rect: r(0, 0, 500, 500),
                visible: true,
            },
            Placement {
                window: w(2),
                rect: r(500, 0, 500, 500),
                visible: true,
            },
        ];
        let plan = reconcile(&obs, &target, &[w(1)], &default_params());
        assert!(
            plan.moves.is_empty(),
            "окна без observed rect не должны попадать в moves"
        );
        assert_eq!(
            plan.show,
            vec![w(2)],
            "но новое окно должно быть включено в show"
        );
    }

    #[test]
    fn window_only_in_observed_is_ignored() {
        // Окно W(99) есть в observed (стороннее/плавающее), но не в target
        let obs = [
            Observed {
                window: w(1),
                rect: r(0, 0, 500, 500),
            },
            Observed {
                window: w(99),
                rect: r(100, 100, 200, 200),
            },
        ];
        let target = [Placement {
            window: w(1),
            rect: r(0, 0, 500, 500),
            visible: true,
        }];
        let plan = reconcile(&obs, &target, &[w(1), w(99)], &default_params());
        assert!(plan.moves.is_empty());
        assert!(plan.show.is_empty());
        assert!(plan.hide.is_empty());
    }

    #[test]
    fn show_and_hide_accurately_reflect_visibility_delta() {
        let obs = [
            Observed {
                window: w(1),
                rect: r(0, 0, 500, 500),
            },
            Observed {
                window: w(2),
                rect: r(0, 0, 500, 500),
            },
        ];
        // W(1) должен стать скрытым, W(2) — видимым
        let target = [
            Placement {
                window: w(1),
                rect: r(0, 0, 500, 500),
                visible: false,
            },
            Placement {
                window: w(2),
                rect: r(0, 0, 500, 500),
                visible: true,
            },
        ];
        // Сейчас видно W(1)
        let visible_now = [w(1)];

        let plan = reconcile(&obs, &target, &visible_now, &default_params());
        assert_eq!(plan.show, vec![w(2)]);
        assert_eq!(plan.hide, vec![w(1)]);
    }

    #[test]
    fn already_visible_window_is_not_in_show() {
        let obs = [Observed {
            window: w(1),
            rect: r(0, 0, 500, 500),
        }];
        let target = [Placement {
            window: w(1),
            rect: r(0, 0, 500, 500),
            visible: true,
        }];
        let visible_now = [w(1)]; // уже видно

        let plan = reconcile(&obs, &target, &visible_now, &default_params());
        assert!(plan.show.is_empty());
        assert!(plan.hide.is_empty());
    }

    #[test]
    fn already_hidden_window_is_not_in_hide() {
        let obs = [Observed {
            window: w(1),
            rect: r(0, 0, 500, 500),
        }];
        let target = [Placement {
            window: w(1),
            rect: r(0, 0, 500, 500),
            visible: false,
        }];
        let visible_now: [WindowKey; 0] = []; // уже не видно

        let plan = reconcile(&obs, &target, &visible_now, &default_params());
        assert!(plan.show.is_empty());
        assert!(plan.hide.is_empty());
    }

    #[test]
    fn max_moves_limits_count_and_prioritizes_largest_distance() {
        let obs = [
            Observed {
                window: w(1),
                rect: r(0, 0, 100, 100), // сдвиг: 10px
            },
            Observed {
                window: w(2),
                rect: r(0, 0, 100, 100), // сдвиг: 500px
            },
            Observed {
                window: w(3),
                rect: r(0, 0, 100, 100), // сдвиг: 100px
            },
        ];
        let target = [
            Placement {
                window: w(1),
                rect: r(10, 0, 100, 100),
                visible: true,
            },
            Placement {
                window: w(2),
                rect: r(500, 0, 100, 100),
                visible: true,
            },
            Placement {
                window: w(3),
                rect: r(100, 0, 100, 100),
                visible: true,
            },
        ];

        let params = ReconcileParams {
            epsilon_px: 2,
            max_moves: 2, // берем только 2 из 3
        };

        let plan = reconcile(&obs, &target, &[w(1), w(2), w(3)], &params);
        assert_eq!(plan.moves.len(), 2);
        assert_eq!(
            plan.moves[0].window,
            w(2),
            "первым должен идти наибольший сдвиг (500px)"
        );
        assert_eq!(
            plan.moves[1].window,
            w(3),
            "вторым должен идти средний сдвиг (100px)"
        );
    }

    #[test]
    fn echo_guard_recognizes_matching_echo() {
        let mut guard = EchoGuard::new(500, 2);
        let moves = [Move {
            window: w(1),
            from: r(0, 0, 500, 500),
            to: r(500, 0, 500, 500),
        }];
        guard.record(&moves, 1000);

        // Приходит событие с целевой геометрией в момент 1100 мс (в пределах TTL)
        assert!(guard.is_echo(w(1), r(500, 0, 500, 500), 1100));
    }

    #[test]
    fn echo_guard_rejects_expired_entry_by_ttl() {
        let mut guard = EchoGuard::new(500, 2);
        let moves = [Move {
            window: w(1),
            from: r(0, 0, 500, 500),
            to: r(500, 0, 500, 500),
        }];
        guard.record(&moves, 1000);

        // Приходит событие через 600 мс (TTL 500 мс истек)
        assert!(!guard.is_echo(w(1), r(500, 0, 500, 500), 1600));
        assert!(guard.is_empty(), "протухшая запись должна быть удалена");
    }

    #[test]
    fn echo_guard_rejects_echo_with_different_geometry() {
        let mut guard = EchoGuard::new(500, 2);
        let moves = [Move {
            window: w(1),
            from: r(0, 0, 500, 500),
            to: r(500, 0, 500, 500),
        }];
        guard.record(&moves, 1000);

        // Пользователь перетащил окно в другое место (200, 300)
        assert!(!guard.is_echo(w(1), r(200, 300, 500, 500), 1100));
        assert_eq!(guard.len(), 1, "несовпавшая запись сохраняется до TTL");
    }

    #[test]
    fn echo_guard_extinguishes_entry_after_first_match() {
        let mut guard = EchoGuard::new(500, 2);
        let moves = [Move {
            window: w(1),
            from: r(0, 0, 500, 500),
            to: r(500, 0, 500, 500),
        }];
        guard.record(&moves, 1000);

        // Первый вызов гасит запись
        assert!(guard.is_echo(w(1), r(500, 0, 500, 500), 1100));
        // Второй вызов с той же геометрией уже не эхо
        assert!(!guard.is_echo(w(1), r(500, 0, 500, 500), 1105));
    }

    #[test]
    fn echo_guard_prune_removes_expired_entries() {
        let mut guard = EchoGuard::new(500, 2);
        guard.record(
            &[
                Move {
                    window: w(1),
                    from: r(0, 0, 100, 100),
                    to: r(10, 0, 100, 100),
                },
                Move {
                    window: w(2),
                    from: r(0, 0, 100, 100),
                    to: r(20, 0, 100, 100),
                },
            ],
            1000,
        );

        guard.record(
            &[Move {
                window: w(3),
                from: r(0, 0, 100, 100),
                to: r(30, 0, 100, 100),
            }],
            1400,
        );

        assert_eq!(guard.len(), 3);

        // В момент 1550: w(1) и w(2) протухли (1000 + 500 = 1500 < 1550), w(3) жив (1400 + 500 = 1900)
        guard.prune(1550);
        assert_eq!(guard.len(), 1);
        assert!(guard.is_echo(w(3), r(30, 0, 100, 100), 1550));
    }

    #[test]
    fn echo_guard_record_overwrites_existing_window_entry() {
        let mut guard = EchoGuard::new(500, 2);
        guard.record(
            &[Move {
                window: w(1),
                from: r(0, 0, 100, 100),
                to: r(10, 0, 100, 100),
            }],
            1000,
        );

        guard.record(
            &[Move {
                window: w(1),
                from: r(10, 0, 100, 100),
                to: r(200, 0, 100, 100),
            }],
            1200,
        );

        assert_eq!(guard.len(), 1);
        // Старая цель (10, 0) больше не эхо
        assert!(!guard.is_echo(w(1), r(10, 0, 100, 100), 1250));
        // Новая цель (200, 0) признается эхом
        assert!(guard.is_echo(w(1), r(200, 0, 100, 100), 1250));
    }

    #[test]
    fn echo_guard_empty_and_len_methods() {
        let mut guard = EchoGuard::new(500, 2);
        assert!(guard.is_empty());
        assert_eq!(guard.len(), 0);

        guard.record(
            &[Move {
                window: w(1),
                from: r(0, 0, 100, 100),
                to: r(50, 0, 100, 100),
            }],
            100,
        );
        assert!(!guard.is_empty());
        assert_eq!(guard.len(), 1);
    }

    #[test]
    fn full_reconciliation_feedback_loop_scenario() {
        // 1. Исходное состояние: окно W(1) не на своем месте
        let initial_obs = [Observed {
            window: w(1),
            rect: r(0, 0, 1920, 1080),
        }];
        let target = [Placement {
            window: w(1),
            rect: r(10, 10, 950, 1060),
            visible: true,
        }];

        let params = default_params();
        let mut echo_guard = EchoGuard::new(500, params.epsilon_px);

        // 2. Сведение: получаем план перестановки
        let plan = reconcile(&initial_obs, &target, &[w(1)], &params);
        assert_eq!(plan.moves.len(), 1);

        // 3. Координатор отправляет SetWindowPos и записывает эхо
        echo_guard.record(&plan.moves, 1000);

        // 4. Через 16 мс трекер ловит EVENT_OBJECT_LOCATIONCHANGE с новой геометрией
        let incoming_rect = r(10, 10, 950, 1060);
        let is_echo = echo_guard.is_echo(w(1), incoming_rect, 1016);
        assert!(
            is_echo,
            "событие от Windows распознано как наше собственное эхо"
        );

        // 5. Следующий снимок содержит новую геометрию
        let next_obs = [Observed {
            window: w(1),
            rect: incoming_rect,
        }];
        let next_plan = reconcile(&next_obs, &target, &[w(1)], &params);

        // 6. ПОВТОРНЫЙ RECONCILE НЕ ДАЕТ ПЕРЕСТАНОВОК — ПЕТЛЯ РАЗОРВАНА!
        assert!(next_plan.moves.is_empty(), "петля обратной связи разорвана");
        assert!(next_plan.show.is_empty());
        assert!(next_plan.hide.is_empty());
    }

    #[test]
    fn distance_calculation_ordering_is_correct() {
        let r0 = r(0, 0, 100, 100);
        let r1 = r(10, 0, 100, 100); // dist = 10
        let r2 = r(0, 0, 150, 100); // dist = 50
        let r3 = r(100, 100, 200, 200); // dist = 100 + 100 + 100 + 100 = 400

        assert_eq!(distance(&r0, &r1), 10);
        assert_eq!(distance(&r0, &r2), 50);
        assert_eq!(distance(&r0, &r3), 400);
        assert!(distance(&r0, &r1) < distance(&r0, &r2));
        assert!(distance(&r0, &r2) < distance(&r0, &r3));
    }

    #[test]
    fn reconcile_with_empty_inputs_produces_empty_plan() {
        let plan = reconcile(&[], &[], &[], &default_params());
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn plan_and_params_serde_roundtrip() {
        let params = default_params();
        let json_params = serde_json::to_string(&params).unwrap();
        let back_params: ReconcileParams = serde_json::from_str(&json_params).unwrap();
        assert_eq!(back_params, params);

        let plan = Plan {
            moves: vec![Move {
                window: w(1),
                from: r(0, 0, 100, 100),
                to: r(50, 50, 200, 200),
            }],
            show: vec![w(2)],
            hide: vec![w(3)],
        };
        let json_plan = serde_json::to_string(&plan).unwrap();
        let back_plan: Plan = serde_json::from_str(&json_plan).unwrap();
        assert_eq!(back_plan, plan);
    }
}
