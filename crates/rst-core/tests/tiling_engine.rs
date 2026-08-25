//! Сквозные сценарии тайлингового движка через ВЕСЬ стек сразу
//! (M9; docs/TILING_DESIGN.md §2 «Куда что ложится»).
//!
//! Модули движка — `tree`, `layout`, `ops`, `policy`, `rules`, `reconcile`,
//! `workspace` — писали независимо разные агенты, и юнит-тесты каждого модуля
//! видят соседей только через их же API, с идеальными данными, которые модуль
//! сам себе и приготовил. Это не ловит расхождения на стыках: policy может
//! строить дерево, которое layout раскладывает с щелью; workspace может решать
//! о видимости не так, как layout; reconcile может сходиться в юнит-тесте, но
//! разойтись на реальной раскладке. Этот файл гоняет пользовательские
//! сценарии так, как их гоняет координатор: вставка по политике → раскладка →
//! сведение → повторное сведение — и проверяет результат АРИФМЕТИКОЙ
//! (закрашенная площадь, пересечения, ширина плиток), а не на глаз.
//!
//! Платформенно-чисто (CONTRIBUTING.md, «Правило зависимостей»): ни строчки
//! Win32, окна — непрозрачные [`WindowKey`].

use rst_core::model::{MonitorId, Rect};
use rst_core::tiling::layout::{LayoutParams, Placement, layout, tab_bar_rect};
use rst_core::tiling::ops::{Direction, focus_direction, resize_focused, toggle_group};
use rst_core::tiling::policy::{InsertPolicy, insert, plan_insert};
use rst_core::tiling::reconcile::{EchoGuard, Observed, ReconcileParams, reconcile};
use rst_core::tiling::rules::{RuleAction, RuleMatch, WindowFacts, WindowRule, evaluate};
use rst_core::tiling::tree::{ContainerLayout, InsertAt, NodeId, NodeKind, Tree, WindowKey};
use rst_core::tiling::workspace::{WorkspaceId, WorkspaceSet};

// ---------------------------------------------------------------------------
// Помощники: окна, мониторы, вставка «как у координатора», арифметические
// проверки раскладки (покрытие без пересечений и без щелей сверх гэпов).
// ---------------------------------------------------------------------------

fn w(n: u64) -> WindowKey {
    WindowKey(n)
}

fn mon(s: &str) -> MonitorId {
    MonitorId(s.to_string())
}

fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

/// Экран 1920×1080 и параметры по умолчанию, на которых считаются все
/// ожидаемые площади ниже.
const WORK: Rect = Rect {
    x: 0,
    y: 0,
    w: 1920,
    h: 1080,
};

fn params() -> LayoutParams {
    LayoutParams {
        gaps_in: 10,
        gaps_out: 12,
        tab_bar_h: 24,
    }
}

/// Вставка нового окна по политике dwindle «как у координатора»: политике
/// отдаётся прямоугольник сфокусированной плитки из предыдущей раскладки
/// (именно по нему dwindle выбирает направление деления), затем план
/// исполняется над тем же деревом.
fn insert_dwindle(tree: &mut Tree, key: WindowKey) {
    let focused_rect = tree
        .focus()
        .and_then(|f| tree.get(f).and_then(|n| n.window()))
        .and_then(|fk| {
            layout(tree, WORK, &params())
                .into_iter()
                .find(|p| p.window == fk)
                .map(|p| p.rect)
        });
    let plan = plan_insert(tree, InsertPolicy::Dwindle, focused_rect);
    insert(tree, key, plan);
}

/// Распределение отрезка между `n` частями — та же арифметика, что в
/// `layout::split_span` (кумулятивные префиксные суммы с округлением).
/// Реплика нужна, чтобы ОЖИДАНИЕ в этом файле считалось независимо от
/// `layout`: если округление в движке сломается, тест увидит расхождение.
fn split_span_like(available: i32, ratios: &[f64], n: usize) -> Vec<i32> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![available.max(1)];
    }
    let min_px = 1;
    if available < n as i32 * min_px {
        return vec![min_px; n];
    }
    let mut norm = Vec::with_capacity(n);
    if ratios.len() == n {
        let sum: f64 = ratios.iter().filter(|r| r.is_finite() && **r > 0.0).sum();
        if sum.is_finite() && sum > 0.0 {
            norm.extend(ratios.iter().map(|&r| {
                if r.is_finite() && r > 0.0 {
                    r / sum
                } else {
                    0.0
                }
            }));
        }
    }
    if norm.len() != n {
        norm = vec![1.0 / n as f64; n];
    }
    let mut result = Vec::with_capacity(n);
    let mut prev = 0i32;
    let mut running = 0.0f64;
    for (i, &ratio) in norm.iter().enumerate() {
        let cum = if i == n - 1 {
            available
        } else {
            running += ratio;
            (available as f64 * running).round() as i32
        };
        result.push(cum - prev);
        prev = cum;
    }
    result
}

/// Суммарная площадь плиток, которую ОБЯЗАНА дать раскладка, выведенная из
/// одной только структуры дерева: внутренняя область минус все внутренние
/// гэпы и полосы табов. Считается рекурсией, повторяющей геометрию `layout`,
/// но НЕ использующей его код, — это и есть независимое ожидание.
fn expected_covered_area(tree: &Tree, params: &LayoutParams) -> i64 {
    let gaps_out = params.gaps_out.max(0);
    let inner = r(
        gaps_out,
        gaps_out,
        ((WORK.w as i32) - 2 * gaps_out).max(1) as u32,
        ((WORK.h as i32) - 2 * gaps_out).max(1) as u32,
    );
    expected_area_node(tree, tree.root(), inner, params)
}

fn expected_area_node(tree: &Tree, id: NodeId, rect: Rect, params: &LayoutParams) -> i64 {
    let Some(node) = tree.get(id) else {
        return 0;
    };
    match &node.kind {
        NodeKind::Window(_) => rect.w as i64 * rect.h as i64,
        NodeKind::Container(c) => {
            let n = c.children.len();
            if n == 0 {
                return 0;
            }
            match c.layout {
                ContainerLayout::Tabbed | ContainerLayout::Stacked => {
                    // Полоса табов из площади окон уходит целиком.
                    let tab_h = params.tab_bar_h.max(0) as u32;
                    let content = r(
                        rect.x,
                        rect.y + tab_h.min(rect.h) as i32,
                        rect.w,
                        rect.h.saturating_sub(tab_h).max(1),
                    );
                    c.children
                        .iter()
                        .map(|ch| expected_area_node(tree, *ch, content, params))
                        .sum()
                }
                ContainerLayout::SplitH | ContainerLayout::SplitV => {
                    // Сумма площадей ДЕТЕЙ, без gap_area: гэпы плитками не
                    // покрываются, а если их прибавить, сумма по контейнерам
                    // телескопируется обратно в полную внутреннюю площадь.
                    let gaps_in = params.gaps_in.max(0);
                    let spans = match c.layout {
                        ContainerLayout::SplitH => {
                            let available = (rect.w as i32) - (n as i32 - 1) * gaps_in;
                            split_span_like(available, &c.ratios, n)
                        }
                        _ => {
                            let available = (rect.h as i32) - (n as i32 - 1) * gaps_in;
                            split_span_like(available, &c.ratios, n)
                        }
                    };
                    spans
                        .into_iter()
                        .zip(c.children.iter())
                        .map(|(span, ch)| match c.layout {
                            ContainerLayout::SplitH => {
                                let child_rect = r(rect.x, rect.y, span as u32, rect.h);
                                expected_area_node(tree, *ch, child_rect, params)
                            }
                            _ => {
                                let child_rect = r(rect.x, rect.y, rect.w, span as u32);
                                expected_area_node(tree, *ch, child_rect, params)
                            }
                        })
                        .sum::<i64>()
                }
            }
        }
    }
}

/// Закрасить плитки в сетку пикселей внутренней области; вернуть число
/// закрашенных клеток и флаг «какая-то клетка закрашена дважды».
fn rasterize(placements: &[Placement], inner: Rect) -> (i64, bool) {
    let w = inner.w as usize;
    let h = inner.h as usize;
    let mut grid = vec![0u8; w * h];
    let mut overlap = false;
    for p in placements {
        if !p.visible {
            continue;
        }
        let x0 = (p.rect.x - inner.x).max(0) as usize;
        let y0 = (p.rect.y - inner.y).max(0) as usize;
        let x1 = ((p.rect.x + p.rect.w as i32) - inner.x)
            .min(inner.w as i32)
            .max(0) as usize;
        let y1 = ((p.rect.y + p.rect.h as i32) - inner.y)
            .min(inner.h as i32)
            .max(0) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let cell = &mut grid[y * w + x];
                if *cell == 1 {
                    overlap = true;
                }
                *cell += 1;
            }
        }
    }
    (grid.iter().filter(|&&c| c > 0).count() as i64, overlap)
}

fn rects_disjoint(a: &Rect, b: &Rect) -> bool {
    a.x + a.w as i32 <= b.x
        || b.x + b.w as i32 <= a.x
        || a.y + a.h as i32 <= b.y
        || b.y + b.h as i32 <= a.y
}

/// Главная арифметическая проверка сценариев 1–3: раскладка обязана
/// Первое: целиком лежать во внутренней области (не вылезать за внешние гэпы),
/// Второе: не пересекаться попарно; третье: покрывать ровно ту площадь, которую
/// диктует структура дерева, — значит, щелей сверх суммы гэпов нет.
fn assert_covers_without_overlap(tree: &Tree, placements: &[Placement]) {
    let gaps_out = params().gaps_out.max(0);
    let inner = r(
        gaps_out,
        gaps_out,
        ((WORK.w as i32) - 2 * gaps_out).max(1) as u32,
        ((WORK.h as i32) - 2 * gaps_out).max(1) as u32,
    );
    for p in placements {
        assert!(
            p.visible,
            "окно {} помечено невидимым без групп",
            p.window.0
        );
        assert!(
            p.rect.x >= inner.x
                && p.rect.y >= inner.y
                && p.rect.x + p.rect.w as i32 <= inner.x + inner.w as i32
                && p.rect.y + p.rect.h as i32 <= inner.y + inner.h as i32,
            "плитка {p:?} вылезла за внутреннюю область {inner:?}"
        );
    }
    for i in 0..placements.len() {
        for j in (i + 1)..placements.len() {
            assert!(
                rects_disjoint(&placements[i].rect, &placements[j].rect),
                "плитки {:?} и {:?} пересекаются",
                placements[i],
                placements[j]
            );
        }
    }
    let (covered, overlap) = rasterize(placements, inner);
    assert!(!overlap, "пересечение найдено по пиксельной сетке");
    let expected = expected_covered_area(tree, &params());
    assert_eq!(
        covered, expected,
        "покрыто {covered} px, а структура дерева требует {expected} px: \
         есть щель сверх суммы гэпов или плитка за границей"
    );
}

/// В дереве не осталось вырожденных контейнеров (ноль или один ребёнок) —
/// они растворились, как требует `Tree::remove_window`.
fn assert_no_degenerate_containers(tree: &Tree) {
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let Some(c) = tree.get(id).and_then(|n| n.container()) else {
            continue;
        };
        assert!(
            id == tree.root() || c.children.len() >= 2,
            "контейнер {id:?} вырожден: детей {}",
            c.children.len()
        );
        stack.extend(c.children.iter().copied());
    }
}

fn placements_of(placements: &[Placement], key: WindowKey) -> &Placement {
    placements
        .iter()
        .find(|p| p.window == key)
        .unwrap_or_else(|| panic!("окно {key:?} не участвует в раскладке"))
}

// ---------------------------------------------------------------------------
// Сценарии
// ---------------------------------------------------------------------------

/// Сценарий 1. Dwindle: четыре окна подряд — раскладка покрывает рабочую область
/// целиком: ни пересечений, ни щелей сверх суммы внутренних гэпов.
/// Ловит стык policy (построение дерева) ↔ layout (геометрия): если бы
/// политика строила дерево, которое раскладка «не умеет», площадь не сошлась.
#[test]
fn dwindle_layout_covers_work_area_without_overlap_or_extra_gaps() {
    let mut tree = Tree::new(ContainerLayout::SplitH);
    for n in 1..=4 {
        insert_dwindle(&mut tree, w(n));
    }
    let placements = layout(&tree, WORK, &params());
    assert_eq!(placements.len(), 4, "все четыре окна разложены");
    assert_covers_without_overlap(&tree, &placements);
}

/// Сценарий 2. Master: master-плитка сверху, стек под ней — та же арифметическая
/// проверка. Ловит стык master-политики (корень: [master, SplitV-стек])
/// с раскладкой вложенных контейнеров.
#[test]
fn master_layout_covers_work_area_without_overlap_or_extra_gaps() {
    let mut tree = Tree::new(ContainerLayout::SplitH);
    for n in 1..=4 {
        let plan = plan_insert(&tree, InsertPolicy::Master, None);
        insert(&mut tree, w(n), plan);
    }
    let placements = layout(&tree, WORK, &params());
    assert_eq!(placements.len(), 4);
    assert_covers_without_overlap(&tree, &placements);
    // master-форма: первый ребёнок корня — окно, второй — SplitV-стек.
    let root = tree.root();
    assert!(tree.get(root).unwrap().container().unwrap().children.len() == 2);
}

/// Сценарий 3. Закрыли среднее окно (w2 из четырёх) — оставшиеся снова покрывают
/// всю область, а вырожденные контейнеры растворились. Ловит стык
/// `remove_window` (схлопывание) ↔ layout (пересчёт геометрии без дыры).
#[test]
fn closing_the_middle_window_restores_full_coverage_and_dissolves_degenerates() {
    let mut tree = Tree::new(ContainerLayout::SplitH);
    for n in 1..=4 {
        insert_dwindle(&mut tree, w(n));
    }
    assert!(tree.remove_window(w(2)).unwrap(), "w2 был в дереве");
    assert_no_degenerate_containers(&tree);

    let placements = layout(&tree, WORK, &params());
    assert_eq!(placements.len(), 3);
    assert_covers_without_overlap(&tree, &placements);
    assert_eq!(
        tree.windows().collect::<Vec<_>>(),
        vec![w(1), w(3), w(4)],
        "порядок листьев сохранился"
    );
}

/// Сценарий 4. Фокус прошёл по кругу всеми четырьмя направлениями и вернулся туда,
/// откуда начал. Дерево — 2×2 (корень SplitH: [SplitV-колонка, SplitV-колонка]);
/// круг: w3 →(Right)→ w2 →(Down)→ w4 →(Left)→ w3 →(Up)→ w1 →(Down)→ w3.
/// Ловит стык `ops::focus_direction` со структурой дерева: структурная
/// навигация обязана согласовываться с `focused_child` сплитов.
#[test]
fn focus_cycles_through_all_four_directions_and_returns_home() {
    let mut tree = Tree::new(ContainerLayout::SplitH);
    let a = tree.insert_window(w(1), InsertAt::Root);
    let b = tree.insert_window(w(2), InsertAt::Root);
    let left = tree.split_leaf(a, ContainerLayout::SplitV).unwrap();
    tree.insert_window(
        w(3),
        InsertAt::Into {
            parent: left,
            index: 1,
        },
    );
    let right = tree.split_leaf(b, ContainerLayout::SplitV).unwrap();
    tree.insert_window(
        w(4),
        InsertAt::Into {
            parent: right,
            index: 1,
        },
    );

    // Настраиваем «память» сплитов так, как её оставил бы реальный пользователь:
    // левая колонка помнит нижний таб, правая — верхний.
    tree.set_focus(tree.find_window(w(2)).unwrap()).unwrap();
    let start = tree.find_window(w(3)).unwrap();
    tree.set_focus(start).unwrap();

    assert!(focus_direction(&mut tree, Direction::Right), "w3 → w2");
    assert_eq!(
        tree.get(tree.focus().unwrap()).unwrap().window(),
        Some(w(2))
    );
    assert!(focus_direction(&mut tree, Direction::Down), "w2 → w4");
    assert_eq!(
        tree.get(tree.focus().unwrap()).unwrap().window(),
        Some(w(4))
    );
    assert!(focus_direction(&mut tree, Direction::Left), "w4 → w3");
    assert_eq!(
        tree.get(tree.focus().unwrap()).unwrap().window(),
        Some(w(3))
    );
    assert!(focus_direction(&mut tree, Direction::Up), "w3 → w1");
    assert_eq!(
        tree.get(tree.focus().unwrap()).unwrap().window(),
        Some(w(1))
    );
    assert!(
        focus_direction(&mut tree, Direction::Down),
        "w1 → w3 — круг замкнулся"
    );
    assert_eq!(
        tree.focus(),
        Some(start),
        "фокус вернулся туда, откуда начал"
    );

    // Край: от нижнего левого влево идти некуда.
    assert!(
        !focus_direction(&mut tree, Direction::Left),
        "граница не пускает"
    );
}

/// Сценарий 5. Ресайз изменил доли, и раскладка отразила это ТОЧНО: плитка, которой
/// добавили долю, стала шире на ожидаемую величину с точностью до округления
/// кумулятивных сумм. Ловит стык `ops::resize_focused` ↔ `layout::split_span`:
/// если бы ресайз и раскладка считали доли по-разному, ширина не сошлась бы.
#[test]
fn resize_grows_the_focused_tile_by_exactly_the_expected_width() {
    let mut tree = Tree::default(); // корень SplitH, два окна пополам
    tree.insert_window(w(1), InsertAt::Root);
    tree.insert_window(w(2), InsertAt::Root);
    tree.set_focus(tree.find_window(w(1)).unwrap()).unwrap();

    let gaps_out = params().gaps_out;
    let gaps_in = params().gaps_in;
    let inner_w = (WORK.w as i32) - 2 * gaps_out;
    let available = inner_w - gaps_in;

    let before = layout(&tree, WORK, &params());
    let w1_before = placements_of(&before, w(1)).rect.w;
    assert_eq!(
        w1_before,
        (available as f64 * 0.5).round() as u32,
        "исходно ровно половина"
    );

    assert!(
        resize_focused(&mut tree, Direction::Right, 0.1),
        "ресайз применился"
    );

    let after = layout(&tree, WORK, &params());
    let w1_after = placements_of(&after, w(1));
    let w2_after = placements_of(&after, w(2));
    let expected_w1 = (available as f64 * 0.6).round() as u32;
    assert_eq!(
        w1_after.rect.w, expected_w1,
        "доля 0.6 даёт ровно эту ширину по кумулятивным суммам"
    );
    assert_eq!(
        w1_after.rect.w as i64 - w1_before as i64,
        expected_w1 as i64 - (available as f64 * 0.5).round() as i64,
        "прирост ширины — ровно разница округлённых долей"
    );
    // Сосед получил остаток, стык без щели и без нахлёста.
    assert_eq!(w2_after.rect.w as i32, available - expected_w1 as i32);
    assert_eq!(
        w1_after.rect.x + w1_after.rect.w as i32 + gaps_in,
        w2_after.rect.x,
        "между плитками ровно один внутренний гэп"
    );
}

/// Сценарий 6. Сгруппировали два окна в табы: оба получили ОДИН прямоугольник
/// содержимого (высота уменьшена на tab_bar_h), видимым помечен ровно один,
/// а полоса табов отдана бару. Ловит стык `ops::toggle_group` ↔ `layout`
/// и `tab_bar_rect`: единая модель «группа — контейнер» во всех трёх местах.
#[test]
fn tabbed_group_gives_both_windows_one_rect_and_marks_one_visible() {
    let mut tree = Tree::default();
    tree.insert_window(w(1), InsertAt::Root);
    tree.insert_window(w(2), InsertAt::Root); // фокус на w2

    assert!(toggle_group(&mut tree), "корень стал Tabbed");

    let placements = layout(&tree, WORK, &params());
    assert_eq!(placements.len(), 2);
    let p1 = placements_of(&placements, w(1));
    let p2 = placements_of(&placements, w(2));
    assert_eq!(p1.rect, p2.rect, "оба таба — один прямоугольник");
    let visible = placements.iter().filter(|p| p.visible).collect::<Vec<_>>();
    assert_eq!(visible.len(), 1, "видим ровно один таб");
    assert_eq!(visible[0].window, w(2), "активен сфокусированный таб");
    let inner_h = (WORK.h as i32) - 2 * params().gaps_out;
    assert_eq!(
        p1.rect.h as i32,
        inner_h - params().tab_bar_h,
        "высота снята на полосу табов"
    );
    let bar = tab_bar_rect(&tree, tree.root(), WORK, &params()).expect("у группы есть таб-бар");
    assert_eq!(
        bar,
        r(
            params().gaps_out,
            params().gaps_out,
            (WORK.w as i32 - 2 * params().gaps_out) as u32,
            params().tab_bar_h as u32
        ),
        "полоса табов — ровно верхняя кромка внутренней области"
    );
}

/// Сценарий 7. Полный цикл сведения — тест на петлю из docs/TILING_DESIGN.md §3:
/// посчитали раскладку → окна стоят не там → reconcile выдал перестановки →
/// «применили» (наблюдаемые = целевые) → второй reconcile обязан выдать
/// ПУСТОЙ план, иначе координатор зациклится на 60 Гц. Ловит стык
/// `layout` ↔ `reconcile` ↔ `EchoGuard` целиком.
#[test]
fn reconcile_converges_after_one_application_breaking_the_feedback_loop() {
    let mut tree = Tree::new(ContainerLayout::SplitH);
    for n in 1..=3 {
        insert_dwindle(&mut tree, w(n));
    }
    let target = layout(&tree, WORK, &params());
    assert_eq!(target.len(), 3);

    // Наблюдаемое состояние: все три окна стоят в углу — «не там».
    let observed: Vec<Observed> = target
        .iter()
        .map(|p| Observed {
            window: p.window,
            rect: r(0, 0, 10, 10),
        })
        .collect();
    let visible_now: Vec<WindowKey> = target.iter().map(|p| p.window).collect();
    let p = ReconcileParams::default();

    let plan = reconcile(&observed, &target, &visible_now, &p);
    assert_eq!(plan.moves.len(), 3, "все три окна требуют перестановки");
    assert!(
        plan.show.is_empty() && plan.hide.is_empty(),
        "видимость не менялась"
    );

    // Координатор применил план и записал эхо своих перестановок.
    let mut guard = EchoGuard::new(500, p.epsilon_px);
    guard.record(&plan.moves, 1000);

    // Система подтвердила: каждое окно встало ровно в цель.
    let applied: Vec<Observed> = target
        .iter()
        .map(|p| Observed {
            window: p.window,
            rect: p.rect,
        })
        .collect();
    for m in &plan.moves {
        assert!(
            guard.is_echo(m.window, m.to, 1016),
            "событие от Windows распознано как наше эхо"
        );
    }

    // ПОВТОРНОЕ СВЕДЕНИЕ НЕ ДАЁТ НИ ОДНОЙ ПЕРЕСТАНОВКИ — петля разорвана.
    let next = reconcile(&applied, &target, &visible_now, &p);
    assert!(
        next.moves.is_empty(),
        "второй reconcile выдал перестановки: петля!"
    );
    assert!(next.show.is_empty() && next.hide.is_empty());
}

/// Сценарий 8. Правило float исключило окно из тайлинга, а остальные разложились так,
/// будто его и не было (полное покрытие без дыры). Ловит стык `rules`
/// (решение о судьбе окна) ↔ `policy`/`layout` (игнорирование исключённого).
#[test]
fn float_rule_excludes_window_and_remaining_tiles_cover_the_area() {
    let rules = [WindowRule {
        matcher: RuleMatch {
            exe: Some("calc.exe".to_string()),
            ..RuleMatch::default()
        },
        action: RuleAction::Float,
    }];
    let calc = WindowFacts {
        exe_path: Some(r"C:\Windows\System32\calc.exe".to_string()),
        title: "Калькулятор".to_string(),
        class: "CalcFrame".to_string(),
    };
    let notepad = WindowFacts {
        exe_path: Some(r"C:\Windows\System32\notepad.exe".to_string()),
        title: "Безымянный — Блокнот".to_string(),
        class: "Notepad".to_string(),
    };
    assert!(!evaluate(&rules, &calc).tiled, "calc исключён правилом");
    assert!(
        evaluate(&rules, &notepad).tiled,
        "notepad тайлится по умолчанию"
    );

    // Координаторский цикл: правило решает судьбу окна ДО вставки в дерево.
    let mut tree = Tree::new(ContainerLayout::SplitH);
    for (key, facts) in [
        (w(1), &notepad),
        (w(2), &calc),
        (w(3), &notepad),
        (w(4), &notepad),
    ] {
        if evaluate(&rules, facts).tiled {
            insert_dwindle(&mut tree, key);
        }
    }

    let placements = layout(&tree, WORK, &params());
    assert_eq!(
        placements.len(),
        3,
        "исключённое окно не участвует в раскладке"
    );
    assert!(placements.iter().all(|p| p.window != w(2)));
    assert!(tree.find_window(w(2)).is_none(), "calc даже не в дереве");
    assert_covers_without_overlap(&tree, &placements);
}

/// Сценарий 9. Два монитора: раскладки независимы (окна одного не попадают в другую),
/// переключение воркспейса на одном мониторе не трогает другой. Ловит стык
/// `workspace` (помониторная модель) ↔ `layout` (раскладка по рабочей области).
#[test]
fn monitors_have_independent_layouts_and_workspaces() {
    let m1 = mon("MONITOR_1");
    let m2 = mon("MONITOR_2");
    let wa2 = r(0, 0, 2560, 1440);

    let mut set = WorkspaceSet::new();
    set.insert_window(&m1, w(1), InsertAt::Root);
    set.insert_window(&m1, w(2), InsertAt::Root);
    set.insert_window(&m2, w(3), InsertAt::Root);

    let p1 = layout(
        &set.get_monitor(&m1)
            .unwrap()
            .active_workspace()
            .unwrap()
            .tree,
        WORK,
        &params(),
    );
    let p2 = layout(
        &set.get_monitor(&m2)
            .unwrap()
            .active_workspace()
            .unwrap()
            .tree,
        wa2,
        &params(),
    );
    assert_eq!(
        p1.iter().map(|p| p.window).collect::<Vec<_>>(),
        vec![w(1), w(2)],
        "MON1 раскладывает только свои окна"
    );
    assert_eq!(p2.iter().map(|p| p.window).collect::<Vec<_>>(), vec![w(3)]);
    // Окна не утекают между мониторами ни в одну сторону.
    assert!(p1.iter().all(|p| p.window != w(3)));
    assert!(p2.iter().all(|p| p.window != w(1) && p.window != w(2)));

    // Переключение воркспейса на MON1 и вставка нового окна.
    set.switch_to(&m1, WorkspaceId(2));
    set.insert_window(&m1, w(4), InsertAt::Root);
    assert_eq!(
        set.visible_windows(),
        vec![w(4), w(3)],
        "MON2 не задет переключением MON1"
    );

    let p2_after = layout(
        &set.get_monitor(&m2)
            .unwrap()
            .active_workspace()
            .unwrap()
            .tree,
        wa2,
        &params(),
    );
    assert_eq!(
        p2_after, p2,
        "раскладка MON2 не изменилась от чужого переключения"
    );
}

/// Сценарий 10. Окно переехало на другой воркспейс: исчезло из visible_windows,
/// появилось в hidden, а после переключения снова видно. Ловит стык
/// `workspace::move_window_to` ↔ `visible/hidden_windows` — те самые списки,
/// по которым координатор клоакирует окна (docs/TILING_DESIGN.md §Р1).
#[test]
fn moving_window_to_another_workspace_hides_it_until_switched_to() {
    let m1 = mon("MONITOR_1");
    let mut set = WorkspaceSet::new();
    set.insert_window(&m1, w(1), InsertAt::Root);
    set.insert_window(&m1, w(2), InsertAt::Root);
    set.insert_window(&m1, w(3), InsertAt::Root);
    assert_eq!(set.visible_windows(), vec![w(1), w(2), w(3)]);

    assert!(
        set.move_window_to(w(2), &m1, WorkspaceId(2)),
        "перенос выполнен"
    );
    assert_eq!(
        set.visible_windows(),
        vec![w(1), w(3)],
        "на активном воркспейсе w2 больше не виден"
    );
    assert_eq!(set.hidden_windows(), vec![w(2)], "w2 ушёл в cloak-список");
    assert_eq!(set.find_window(w(2)).unwrap().workspace, WorkspaceId(2));

    assert!(set.switch_to(&m1, WorkspaceId(2)));
    assert_eq!(
        set.visible_windows(),
        vec![w(2)],
        "после переключения w2 виден"
    );
    assert_eq!(
        set.hidden_windows(),
        vec![w(1), w(3)],
        "а прежние окна — в cloak"
    );
    let ws1_tree = &set
        .get_monitor(&m1)
        .unwrap()
        .find_workspace(WorkspaceId(1))
        .unwrap()
        .tree;
    assert_eq!(
        ws1_tree.windows().collect::<Vec<_>>(),
        vec![w(1), w(3)],
        "на старом воркспейсе дубликатов не осталось"
    );
}
