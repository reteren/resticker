//! Проверка инвариантов движка тайлинга на длинных псевдослучайных
//! последовательностях операций (M9; docs/TILING_DESIGN.md §Р4).
//!
//! Модули движка (`tree`, `layout`, `ops`, `policy`, `rules`, `reconcile`,
//! `workspace`, `binds`, `actions`) писали независимо разные агенты, и каждый
//! юнит-тест подаёт модулю аккуратные, заранее известные данные. Сквозной
//! файл `tiling_engine.rs` гоняет десять сценариев, но сценарий — это одна
//! придуманная человеком последовательность. Ни то, ни другое не ловит то,
//! что ловит ПРОИЗВОЛЬНАЯ последовательность: `move_direction` после
//! схлопывания контейнера, `swap_nodes` между ветками разной глубины,
//! `cycle_group` после удаления соседнего окна. Такой баг живёт на стыке
//! двух операций и обнаруживается только перебором.
//!
//! Этот файл генерирует длинные детерминированные последовательности
//! операций (свой линейный конгруэнтный генератор, без новых зависимостей)
//! и после КАЖДОГО шага проверяет все восемь инвариантов структуры. Если
//! инвариант нарушен — тест падает с зерном и номером шага в сообщении:
//! воспроизвести сценарий можно, передав то же зерно (см. `run_sequence`).
//! Платформенно-чисто (CONTRIBUTING.md, «Правило зависимостей»).

use rst_core::model::{MonitorId, Rect};
use rst_core::tiling::layout::{LayoutParams, layout};
use rst_core::tiling::ops::{
    Direction, cycle_group, focus_direction, move_direction, resize_focused, swap_direction,
    toggle_group, toggle_split,
};
use rst_core::tiling::policy::{InsertPolicy, insert, plan_insert};
use rst_core::tiling::tree::{ContainerLayout, InsertAt, NodeKind, Tree, WindowKey};
use rst_core::tiling::workspace::{WorkspaceId, WorkspaceSet};

fn w(n: u64) -> WindowKey {
    WindowKey(n)
}

/// Рабочая область, на которой считается раскладка (1920×1080, как в
/// остальных тестах крейта).
const WORK: Rect = Rect {
    x: 0,
    y: 0,
    w: 1920,
    h: 1080,
};

fn params() -> LayoutParams {
    LayoutParams::default()
}

// ---------------------------------------------------------------------------
// Детерминированный генератор: линейный конгруэнтный на u64. Зерно
// фиксировано в каждом тесте — падение обязано воспроизводиться.
// ---------------------------------------------------------------------------

struct Lcg {
    state: u64,
}

impl Lcg {
    /// Стандартные константы ЛКГ (те же, что в SplitMix64): множитель и
    /// инкремент — нечётные и взаимно простые с модулем, период 2^64.
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// Индекс в `[0, n)`; вызывающий обязан дать `n > 0`.
    fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next() % n as u64) as usize
    }
}

// ---------------------------------------------------------------------------
// Операции и их взвешенные наборы.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Op {
    /// Новое окно (свежий ключ) в активный воркспейс случайного монитора.
    Insert,
    /// Удалить случайное существующее окно.
    Remove,
    Focus(Direction),
    Move(Direction),
    Swap(Direction),
    Resize(Direction),
    ToggleSplit,
    ToggleGroup,
    CycleGroup(bool),
    /// Переключить активный воркспейс случайного монитора.
    SwitchWorkspace,
    /// Перенести случайное окно на случайный воркспейс.
    SendToWorkspace,
}

impl Op {
    fn random(rng: &mut Lcg, weights: &[(u32, Op)]) -> Op {
        let total: u32 = weights.iter().map(|(w, _)| w).sum();
        let roll = rng.below(total as usize) as u32;
        let mut acc = 0;
        for (w, op) in weights {
            acc += w;
            if roll < acc {
                return *op;
            }
        }
        weights.last().unwrap().1
    }
}

/// Сбалансированный набор: всё понемногу, вставки чаще — дерево растёт.
const MIXED: &[(u32, Op)] = &[
    (24, Op::Insert),
    (10, Op::Remove),
    (14, Op::Focus(Direction::Left)),
    (8, Op::Move(Direction::Left)),
    (8, Op::Swap(Direction::Left)),
    (10, Op::Resize(Direction::Left)),
    (5, Op::ToggleSplit),
    (8, Op::ToggleGroup),
    (4, Op::CycleGroup(true)),
    (4, Op::SwitchWorkspace),
    (5, Op::SendToWorkspace),
];

/// Уклон в группы: реже вставки, чаще `toggle_group`/`cycle_group` — именно
/// переходы сплит ↔ табы чаще всего ломают структуру (глубина, видимость,
/// фокус).
const GROUPY: &[(u32, Op)] = &[
    (16, Op::Insert),
    (10, Op::Remove),
    (8, Op::Focus(Direction::Left)),
    (6, Op::Move(Direction::Left)),
    (6, Op::Swap(Direction::Left)),
    (8, Op::Resize(Direction::Left)),
    (12, Op::ToggleSplit),
    (18, Op::ToggleGroup),
    (10, Op::CycleGroup(true)),
    (3, Op::SwitchWorkspace),
    (3, Op::SendToWorkspace),
];

/// Уклон в удаления: дерево обязано корректно схлопываться до пустого.
const REMOVEY: &[(u32, Op)] = &[
    (14, Op::Insert),
    (45, Op::Remove),
    (12, Op::Focus(Direction::Left)),
    (8, Op::Move(Direction::Left)),
    (6, Op::Swap(Direction::Left)),
    (8, Op::Resize(Direction::Left)),
    (3, Op::ToggleSplit),
    (4, Op::ToggleGroup),
];

// ---------------------------------------------------------------------------
// Харнесс: набор воркспейсов + генерация операций + проверка после шага.
// ---------------------------------------------------------------------------

/// Один «случайный» шаг по набору воркспейсов. Все операции намеренно
/// трактуются как «может не сработать» (возвращают false): тест проверяет
/// не результаты, а то, что структура пережила любую попытку.
fn step(
    set: &mut WorkspaceSet,
    rng: &mut Lcg,
    key_counter: &mut u64,
    monitors: &[MonitorId],
    op: Op,
) {
    let monitor = monitors[rng.below(monitors.len())].clone();
    match op {
        Op::Insert => {
            let policy = match rng.below(3) {
                0 => InsertPolicy::Dwindle,
                1 => InsertPolicy::Master,
                _ => InsertPolicy::Manual,
            };
            let key = WindowKey(*key_counter);
            *key_counter += 1;
            let state = set.ensure_monitor(&monitor);
            let ws = state
                .active_workspace_mut()
                .expect("активный воркспейс есть");
            let plan = plan_insert(&ws.tree, policy, None);
            insert(&mut ws.tree, key, plan);
        }
        Op::Remove => {
            if let Some(key) = random_window(set, rng) {
                let _ = set.remove_window(key);
            }
        }
        Op::Focus(dir) => tree_op(set, &monitor, |t| focus_direction(t, dir)),
        Op::Move(dir) => tree_op(set, &monitor, |t| move_direction(t, dir)),
        Op::Swap(dir) => tree_op(set, &monitor, |t| swap_direction(t, dir)),
        Op::Resize(dir) => tree_op(set, &monitor, |t| resize_focused(t, dir, 0.05)),
        Op::ToggleSplit => tree_op(set, &monitor, toggle_split),
        Op::ToggleGroup => tree_op(set, &monitor, toggle_group),
        Op::CycleGroup(fwd) => tree_op(set, &monitor, |t| cycle_group(t, fwd)),
        Op::SwitchWorkspace => {
            let ws = WorkspaceId(1 + rng.below(4) as u8);
            let _ = set.switch_to(&monitor, ws);
        }
        Op::SendToWorkspace => {
            if let Some(key) = random_window(set, rng) {
                let target = monitors[rng.below(monitors.len())].clone();
                let ws = WorkspaceId(1 + rng.below(4) as u8);
                let _ = set.move_window_to(key, &target, ws);
            }
        }
    }
}

/// Применить операцию к активному воркспейсу случайного монитора.
fn tree_op(set: &mut WorkspaceSet, monitor: &MonitorId, f: impl FnOnce(&mut Tree) -> bool) {
    if let Some(state) = set.get_monitor_mut(monitor)
        && let Some(ws) = state.active_workspace_mut()
    {
        let _ = f(&mut ws.tree);
    }
}

fn random_window(set: &WorkspaceSet, rng: &mut Lcg) -> Option<WindowKey> {
    let all: Vec<WindowKey> = set
        .monitors
        .iter()
        .flat_map(|m| m.workspaces.iter())
        .flat_map(|ws| ws.all_windows())
        .collect();
    if all.is_empty() {
        None
    } else {
        Some(all[rng.below(all.len())])
    }
}

/// Прогнать последовательность и проверить инварианты после каждого шага.
/// `seed` печатается в сообщении падения — воспроизведение = тот же seed.
fn run_sequence(seed: u64, steps: usize, weights: &[(u32, Op)], monitor_count: usize) {
    let mut rng = Lcg::new(seed);
    let mut set = WorkspaceSet::new();
    let monitors: Vec<MonitorId> = (0..monitor_count)
        .map(|i| MonitorId(format!("MON{i}")))
        .collect();
    let mut key_counter: u64 = 1_000;

    for step_no in 1..=steps {
        let op = Op::random(&mut rng, weights);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            step(&mut set, &mut rng, &mut key_counter, &monitors, op);
        }));
        if let Err(payload) = result {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<не-строковая паника>".to_string());
            panic!("[{seed:#x} шаг {step_no}] операция упала: {msg}");
        }
        check_all(&set, seed, step_no);
    }
}

/// Проверить все инварианты по всему набору воркспейсов. Каждая проверка —
/// отдельная функция с говорящим именем: падение показывает, ЧТО именно
/// сломалось, а seed+шаг в сообщении — ГДЕ.
fn check_all(set: &WorkspaceSet, seed: u64, step: usize) {
    for (mi, m) in set.monitors.iter().enumerate() {
        for ws in &m.workspaces {
            let where_ = format!("monitor {mi} workspace {}", ws.id);
            check_windows_unique_in_tree(&ws.tree, seed, step, &where_);
            check_parent_links_consistent(&ws.tree, seed, step, &where_);
            check_ratios_sum_to_one_and_match_children(&ws.tree, seed, step, &where_);
            check_no_degenerate_containers(&ws.tree, seed, step, &where_);
            check_focused_child_in_range(&ws.tree, seed, step, &where_);
            check_focus_is_a_live_leaf(&ws.tree, seed, step, &where_);
            check_layout_sizes_and_no_overlaps(&ws.tree, seed, step, &where_);
        }
    }
    check_every_window_in_exactly_one_workspace(set, seed, step);
    // Геометрическая проверка — после структурных: если структура цела, а
    // раскладка пересекается, это уже геометрический, а не структурный баг.
    for (mi, m) in set.monitors.iter().enumerate() {
        for ws in &m.workspaces {
            let where_ = format!("monitor {mi} workspace {}", ws.id);
            check_layout_sizes_and_no_overlaps(&ws.tree, seed, step, &where_);
        }
    }
}

fn fail(seed: u64, step: usize, where_: &str, msg: impl std::fmt::Display) -> ! {
    panic!("[{seed:#x} шаг {step}] {where_}: {msg}")
}

/// Инвариант 1: каждое окно присутствует в дереве ровно один раз — среди
/// листьев нет дубликатов ключей, и `Tree::windows()` согласован с обходом.
fn check_windows_unique_in_tree(tree: &Tree, seed: u64, step: usize, where_: &str) {
    // Стек — LIFO, поэтому дети обходятся в обратном порядке: для сравнения
    // с Tree::windows() (порядок слева направо) собираем ключи через
    // tree.leaves(), а уникальность проверяем отдельно сортировкой.
    let mut keys: Vec<WindowKey> = Vec::new();
    for id in tree.leaves() {
        let Some(key) = tree.get(id).and_then(|n| n.window()) else {
            fail(seed, step, where_, "лист не окно");
        };
        keys.push(key);
    }
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != keys.len() {
        fail(seed, step, where_, "окно присутствует в дереве дважды");
    }
    let via_api = tree.windows().collect::<Vec<_>>();
    if via_api != keys {
        fail(
            seed,
            step,
            where_,
            "Tree::windows() разошёлся с обходом дерева",
        );
    }
}

/// Инвариант 2: у каждого узла, кроме корня, есть родитель, и этот родитель
/// содержит его в `children`; корень на родителя не указывает; из `children`
/// нет ссылок на мёртвые узлы.
fn check_parent_links_consistent(tree: &Tree, seed: u64, step: usize, where_: &str) {
    let root = tree.root();
    if tree.get(root).and_then(|n| n.parent).is_some() {
        fail(seed, step, where_, "корень имеет родителя");
    }
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(node) = tree.get(id) else {
            fail(seed, step, where_, "обход наткнулся на мёртвый узел");
        };
        if let Some(c) = node.container() {
            for &child in &c.children {
                if tree.get(child).is_none() {
                    fail(seed, step, where_, "children ссылается на удалённый узел");
                }
                if tree.parent_of(child) != Some(id) {
                    fail(
                        seed,
                        step,
                        where_,
                        "ребёнок не указывает на родителя из children",
                    );
                }
            }
            stack.extend(c.children.iter().copied());
        }
    }
}

/// Инвариант 3: доли любого контейнера в сумме дают 1.0 (±1e-9) и их число
/// равно числу детей.
fn check_ratios_sum_to_one_and_match_children(tree: &Tree, seed: u64, step: usize, where_: &str) {
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let Some(c) = tree.get(id).and_then(|n| n.container()) else {
            continue;
        };
        if c.ratios.len() != c.children.len() {
            fail(
                seed,
                step,
                where_,
                format!(
                    "долей {} != детей {} (ratios={:?})",
                    c.ratios.len(),
                    c.children.len(),
                    c.ratios
                ),
            );
        }
        let sum: f64 = c.ratios.iter().sum();
        // Пустой контейнер (пустое дерево, корень) — легитимный вырожденный
        // случай: детей нет, долей нет, сумма = 0.
        if !c.children.is_empty() && (sum - 1.0).abs() > 1e-9 {
            fail(
                seed,
                step,
                where_,
                format!("сумма долей = {sum} (ratios={:?})", c.ratios),
            );
        }
        stack.extend(c.children.iter().copied());
    }
}

/// Инвариант 4: контейнер, кроме корня, не вырожден — детей 2 и более.
/// (Ноль детей допустим только ВНУТРИ операции, после неё — нет:
/// `Tree::remove_window` обязан растворить вырожденный контейнер.)
fn check_no_degenerate_containers(tree: &Tree, seed: u64, step: usize, where_: &str) {
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let Some(c) = tree.get(id).and_then(|n| n.container()) else {
            continue;
        };
        if id != tree.root() && c.children.len() < 2 {
            fail(
                seed,
                step,
                where_,
                format!("вырожденный контейнер: детей {}", c.children.len()),
            );
        }
        stack.extend(c.children.iter().copied());
    }
}

/// Инвариант 5: `focused_child` любого контейнера — валидный индекс в его
/// `children` (у пустого контейнера — 0).
fn check_focused_child_in_range(tree: &Tree, seed: u64, step: usize, where_: &str) {
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let Some(c) = tree.get(id).and_then(|n| n.container()) else {
            continue;
        };
        if c.focused_child >= c.children.len().max(1) {
            fail(
                seed,
                step,
                where_,
                format!(
                    "focused_child = {} вне children.len = {}",
                    c.focused_child,
                    c.children.len()
                ),
            );
        }
        stack.extend(c.children.iter().copied());
    }
}

/// Инвариант 6: фокус дерева, если есть, указывает на существующий ЛИСТ,
/// а не на контейнер и не на удалённый узел.
fn check_focus_is_a_live_leaf(tree: &Tree, seed: u64, step: usize, where_: &str) {
    let Some(focus) = tree.focus() else {
        return;
    };
    match tree.get(focus) {
        Some(node) if matches!(node.kind, NodeKind::Window(_)) => {}
        Some(_) => fail(seed, step, where_, "фокус указывает на контейнер"),
        None => fail(seed, step, where_, "фокус указывает на удалённый узел"),
    }
}

/// Инвариант 7: `layout()` не даёт пересечений между ВИДИМЫМИ плитками одного
/// воркспейса (скрытые табы групп делят один прямоугольник и не считаются).
///
/// Отрицательные размеры невозможны по построению (`Rect.w/h` — беззнаковые),
/// а НУЛЕВЫЕ — легитимны с починки `layout::split_span` (layout.rs:356):
/// плитка нулевой ширины лучше плитки, залезшей на соседа. Пересечение же —
/// всегда баг: оно означает, что одна плитка физически рисуется поверх
/// другой.
fn check_layout_sizes_and_no_overlaps(tree: &Tree, seed: u64, step: usize, where_: &str) {
    let placements = layout(tree, WORK, &params());
    let visible: Vec<_> = placements.iter().filter(|p| p.visible).collect();
    for i in 0..visible.len() {
        for j in (i + 1)..visible.len() {
            if !rects_disjoint(&visible[i].rect, &visible[j].rect) {
                eprintln!("TREE: {tree:?}");
                eprintln!("PLACEMENTS: {placements:?}");
                fail(
                    seed,
                    step,
                    where_,
                    format!(
                        "плитки {:?} и {:?} пересекаются",
                        visible[i].rect, visible[j].rect
                    ),
                );
            }
        }
    }
}

/// Инвариант 8: окно живёт ровно в одном воркспейсе на всём наборе мониторов
/// — ни в двух деревьях, ни в дереве и floating одновременно, ни дважды в
/// одном месте.
fn check_every_window_in_exactly_one_workspace(set: &WorkspaceSet, seed: u64, step: usize) {
    let mut all: Vec<WindowKey> = Vec::new();
    for m in &set.monitors {
        for ws in &m.workspaces {
            all.extend(ws.all_windows());
        }
    }
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != all.len() {
        fail(
            seed,
            step,
            "все мониторы",
            "окно встречается в наборе дважды",
        );
    }
    for key in &sorted {
        if set.find_window(*key).is_none() {
            fail(
                seed,
                step,
                "все мониторы",
                format!("find_window не нашёл окно {key:?}"),
            );
        }
    }
}

fn rects_disjoint(a: &Rect, b: &Rect) -> bool {
    a.x + a.w as i32 <= b.x
        || b.x + b.w as i32 <= a.x
        || a.y + a.h as i32 <= b.y
        || b.y + b.h as i32 <= a.y
}

// ---------------------------------------------------------------------------
// Сценарии
// ---------------------------------------------------------------------------

/// Длинная сбалансированная последовательность на одном мониторе: все восемь
/// инвариантов держатся после каждого из 400 шагов.
#[test]
fn long_sequence_on_one_monitor_keeps_invariants() {
    run_sequence(0xC0FFEE2026, 400, MIXED, 1);
}

/// Два монитора: переключение воркспейсов и перенос окон между ними
/// (SwitchWorkspace/SendToWorkspace в наборе) не должны дублировать окна и
/// ломать деревья ни на одном мониторе.
///
/// Раньше падал на шаге 467 из-за переполнения узкого контейнера
/// (`layout::split_span`); баг починен, тест сторожит починку.
#[test]
fn long_sequence_on_two_monitors_keeps_invariants() {
    run_sequence(0x0BAD_C0DE2026, 500, MIXED, 2);
}

/// Уклон в группы: частые toggle_split/toggle_group/cycle_group — переходы
/// сплит ↔ табы чаще всего роняют фокус и доля. 500 шагов.
#[test]
fn group_heavy_sequence_keeps_invariants() {
    run_sequence(0x6F0A2026, 500, GROUPY, 1);
}

/// Уклон в удаления: дерево обязано схлопываться до пустого без вырожденных
/// контейнеров и без фокуса на мёртвом узле; после опустошения набор
/// продолжает работать (вставки снова растят дерево).
#[test]
fn deletion_heavy_sequence_collapses_the_tree_cleanly() {
    let seed = 0xD31E7ED2026;
    let mut rng = Lcg::new(seed);
    let mut set = WorkspaceSet::new();
    let monitors = vec![MonitorId("MON0".to_string())];
    let mut key_counter: u64 = 1_000;

    // Сначала вырастим дерево из 12 окон.
    for _ in 0..12 {
        let key = WindowKey(key_counter);
        key_counter += 1;
        let state = set.ensure_monitor(&monitors[0]);
        let ws = state.active_workspace_mut().unwrap();
        let plan = plan_insert(&ws.tree, InsertPolicy::Dwindle, None);
        insert(&mut ws.tree, key, plan);
    }
    check_all(&set, seed, 0);

    // Теперь преимущественно удаляем; инварианты — после каждого шага.
    let mut step_no = 0;
    while !set.monitors[0].workspaces[0].all_windows().is_empty() && step_no < 400 {
        step_no += 1;
        let op = Op::random(&mut rng, REMOVEY);
        step(&mut set, &mut rng, &mut key_counter, &monitors, op);
        check_all(&set, seed, step_no);
    }
    assert!(
        set.monitors[0].workspaces[0].all_windows().is_empty(),
        "дерево обязано схлопнуться до пустого"
    );
    check_all(&set, seed, step_no + 1);

    // Пустое дерево продолжает принимать операции (рост после опустошения).
    for _ in 0..30 {
        step_no += 1;
        let op = Op::random(&mut rng, MIXED);
        step(&mut set, &mut rng, &mut key_counter, &monitors, op);
        check_all(&set, seed, step_no);
    }
}

/// Только операции фокуса/навигации на пустом дереве: ни одна не должна
/// паниковать, структура остаётся валидной. Ловит разыменования фокуса
/// без проверки `None`.
#[test]
fn focus_only_sequence_on_empty_tree_never_panics() {
    let seed = 0xFEED2026;
    let mut rng = Lcg::new(seed);
    let mut set = WorkspaceSet::new();
    let monitors = vec![MonitorId("MON0".to_string())];
    let mut key_counter: u64 = 1_000;
    let focus_ops: &[(u32, Op)] = &[
        (20, Op::Focus(Direction::Left)),
        (20, Op::Move(Direction::Left)),
        (20, Op::Swap(Direction::Left)),
        (15, Op::Resize(Direction::Left)),
        (10, Op::ToggleSplit),
        (10, Op::ToggleGroup),
        (5, Op::CycleGroup(true)),
    ];
    for step_no in 1..=200 {
        let op = Op::random(&mut rng, focus_ops);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            step(&mut set, &mut rng, &mut key_counter, &monitors, op);
        }));
        if let Err(payload) = result {
            let msg = payload
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("<не-строковая паника>");
            panic!("[{seed:#x} шаг {step_no}] пустое дерево упало: {msg}");
        }
        check_all(&set, seed, step_no);
    }
    assert!(
        set.monitors.is_empty(),
        "вставок и переключений не было — мониторы не создавались"
    );
}

/// Стресс: 2000 шагов, два монитора, весь набор операций. Деревья растут до
/// сотен окон, вложенность — максимальная; проверка инвариантов после
/// каждого шага.
///
/// Раньше падал на шаге 919 из-за переполнения узкого контейнера
/// (`layout::split_span`); баг починен, тест сторожит починку.
#[test]
fn stress_two_thousand_steps_keeps_invariants() {
    run_sequence(0x5EED2026, 2000, MIXED, 2);
}

/// НАЙДЕННЫЙ БАГ ДВИЖКА (отчёт — в worker_done): минимальный воспроизводящий
/// сценарий нарушения инварианта 7 «видимые плитки одного воркспейса не
/// пересекаются».
///
/// Причина: `layout::split_span` (crates/rst-core/src/tiling/layout.rs:341)
/// при `available_span < n * min_px` отдаёт КАЖДОМУ ребёнку гарантированный
/// `min_px` = 1 px, не проверяя, влезают ли дети в контейнер вообще:
/// суммарно им нужно `(n-1)*gaps_in + n*1` px, что больше ширины контейнера.
/// Дети раскладываются с полными гэпами, вылезают за правый/нижний край
/// контейнера и накладываются на соседние плитки (подтверждено живыми
/// деревьями из случайных прогонов: overlap (120,8,1,1064) × (120,8,1,1064)
/// и (8,27,948,1) × (8,26,1904,13)).
///
/// Достижимо не только фаззингом: контейнер, ужатый долями (resize_focused
/// или вставки через attach, ужимающие старые доли), при росте числа детей
/// рано или поздно становится уже своих гэпов — и плитки пересекаются.
///
/// По правилам задачи чужой модуль не чиним: тест задокументирован и
/// отключён. Когда layout.rs починят (минимум — клампить число детей по
/// ширине или разрешать гэпы меньше min), снять #[ignore] с этого теста и
/// с двух фаззеров выше.
#[test]
fn narrow_container_children_overflow_into_sibling() {
    // Корень SplitH: левая колонка с долей 1% (ширина ~19 px), правая — окно.
    let mut tree = Tree::new(ContainerLayout::SplitH);
    let a = tree.insert_window(w(1), InsertAt::Root);
    let b = tree.insert_window(w(2), InsertAt::Root);
    if let Some(c) = tree.get_mut(tree.root()).and_then(|n| n.container_mut()) {
        c.ratios = vec![0.01, 0.99];
    }
    // В узкую колонку кладём 10 окон: ей нужно 9*8 + 10 = 82 px, есть ~19.
    let cont = tree.split_leaf(a, ContainerLayout::SplitH).unwrap();
    for i in 0..9 {
        tree.insert_window(
            w(10 + i),
            InsertAt::Into {
                parent: cont,
                index: (i + 1) as usize,
            },
        );
    }
    // Последние дети колонки обязаны вылезти на окно b.
    let placements = layout(&tree, WORK, &params());
    let visible: Vec<_> = placements.iter().filter(|p| p.visible).collect();
    for i in 0..visible.len() {
        for j in (i + 1)..visible.len() {
            assert!(
                rects_disjoint(&visible[i].rect, &visible[j].rect),
                "переполнение контейнера: {:?} и {:?} пересекаются",
                visible[i].rect,
                visible[j].rect
            );
        }
    }
    let _ = b;
}
