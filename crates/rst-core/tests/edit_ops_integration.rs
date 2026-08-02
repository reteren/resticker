//! Интеграционные сценарии редактирования (M2): комбинации операций из
//! `ops`, `selection_set` и `snap`, которые их per-module юнит-тесты по
//! отдельности не покрывают. Только данные и чистые функции, без окон/GPU
//! (CONTRIBUTING.md, «Правило зависимостей»).
//!
//! Проверяемые связки:
//! 1. `ops::delete` выделенного стикера + `SelectionSet::prune` при соседних
//!    (и совпадающих) значениях `order` у выживших;
//! 2. `ops::duplicate` у края экрана + `snap::clamp_min_visible` на позиции
//!    копии — составление ограничения видимости из офсета дубликата;
//! 3. `select_all` → `rubber_band` с пустым списком стикеров.

use rst_core::hittest::DipRect;
use rst_core::model::{Config, MediaType, MonitorId, Sticker};
use rst_core::ops;
use rst_core::selection_set::SelectionSet;
use rst_core::snap::{self, MIN_VISIBLE_FRACTION};
use std::f64::consts::FRAC_PI_2;

/// Экран (DIP) — как в тестах overlay_manager: 1920×1080.
const SCREEN: DipRect = DipRect::new(0.0, 0.0, 1920.0, 1080.0);

fn sticker(order: i64, cx: f64, cy: f64, w: f64, h: f64) -> Sticker {
    let mut s = Sticker::new_file(
        "C:\\pics\\test.png".into(),
        MediaType::Image,
        MonitorId::default(),
        cx,
        cy,
        w,
        h,
    );
    s.order = order;
    s
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-9,
        "ожидалось {expected}, получено {actual}"
    );
}

/// Удаление выделенного стикера, затем `prune`: мёртвый id вычищается,
/// порядок выживших сохраняется, зей-операция над соседними order (0 и 2
/// после удаления середины) нормализует и работает.
#[test]
fn delete_selected_then_prune_keeps_survivors_and_orders_usable() {
    let mut cfg = Config {
        stickers: vec![
            sticker(0, 100.0, 100.0, 50.0, 50.0),
            sticker(1, 300.0, 100.0, 50.0, 50.0),
            sticker(2, 500.0, 100.0, 50.0, 50.0),
        ],
        ..Default::default()
    };
    let a = cfg.stickers[0].id;
    let b = cfg.stickers[1].id;
    let c = cfg.stickers[2].id;

    let mut sel = SelectionSet::new();
    sel.select_all(&cfg.stickers);

    // ops::delete не нормализует order (CONFIG.md — при сохранении): у
    // выживших остаются соседние значения 0 и 2 с пропуском.
    ops::delete(&mut cfg, b).unwrap();
    sel.prune(&cfg.stickers);

    assert_eq!(
        sel.ids(),
        &[a, c],
        "мёртвый id вычищен, порядок выделения сохранён"
    );
    assert_eq!(sel.selected_stickers(&cfg.stickers).len(), 2);
    let bounds = sel.bounds(&cfg.stickers).expect("bbox выделения жив");
    assert_close(bounds.x, 75.0);
    assert_close(bounds.w, 450.0);

    // С пропуском в order зей-операция работает: c опустился ниже a.
    ops::step_down(&mut cfg, c).unwrap();
    let a_order = cfg.stickers.iter().find(|s| s.id == a).unwrap().order;
    let c_order = cfg.stickers.iter().find(|s| s.id == c).unwrap().order;
    assert!(c_order < a_order, "c ниже a после step_down");
}

/// Удаление всех выделенных стикеров + `prune`: и конфиг, и выделение, и
/// bbox пусты — без паники и висячих id.
#[test]
fn delete_all_selected_then_prune_empties_everything() {
    let mut cfg = Config {
        stickers: vec![
            sticker(0, 100.0, 100.0, 40.0, 20.0),
            sticker(1, 300.0, 100.0, 40.0, 20.0),
        ],
        ..Default::default()
    };
    let mut sel = SelectionSet::new();
    sel.select_all(&cfg.stickers);

    let ids: Vec<_> = cfg.stickers.iter().map(|s| s.id).collect();
    for id in ids {
        ops::delete(&mut cfg, id).unwrap();
    }
    sel.prune(&cfg.stickers);

    assert!(cfg.stickers.is_empty());
    assert!(sel.is_empty());
    assert_eq!(sel.bounds(&cfg.stickers), None);
    assert!(sel.selected_stickers(&cfg.stickers).is_empty());
}

/// Удаление одного из пары с совпадающим order (конфиг без нормализации) +
/// `prune`: зей-операция над выжившим нормализует и работает.
#[test]
fn delete_one_of_tied_order_pair_then_zop_works() {
    let mut cfg = Config {
        stickers: vec![
            sticker(5, 100.0, 100.0, 40.0, 20.0),
            sticker(5, 300.0, 100.0, 40.0, 20.0),
        ],
        ..Default::default()
    };
    let a = cfg.stickers[0].id;
    let b = cfg.stickers[1].id;

    let mut sel = SelectionSet::new();
    sel.select_all(&cfg.stickers);
    ops::delete(&mut cfg, a).unwrap();
    sel.prune(&cfg.stickers);
    assert_eq!(sel.ids(), &[b]);

    ops::step_down(&mut cfg, b).unwrap(); // единственный стикер — no-op
    assert_eq!(cfg.stickers.len(), 1);
    assert_eq!(cfg.stickers[0].order, 0, "step_down нормализовал order");
}

/// Дубликат у правого нижнего края: `duplicate` сдвигает копию на +16 и
/// уводит её за экран; `clamp_min_visible` (тот же путь, что overlay_manager
/// применяет к drag/resize) возвращает копию в кадр — видно ровно 10%.
#[test]
fn duplicate_near_bottom_right_then_clamp_keeps_10_percent_visible() {
    let mut cfg = Config {
        stickers: vec![sticker(0, 1955.0, 1095.0, 100.0, 50.0)],
        ..Default::default()
    };
    let src = cfg.stickers[0].id;

    // Оригинал на пределе, но в рамках: уход вправо 85 = 85% ширины (< 90%),
    // вниз 40 = 80% высоты — clamp над оригиналом был бы no-op.
    let orig = snap::clamp_min_visible(&cfg.stickers[0].placement, 0.0, SCREEN);
    assert_close(orig.cx, 1955.0);
    assert_close(orig.cy, 1095.0);

    let clone_id = ops::duplicate(&mut cfg, src).unwrap();
    let clone = cfg.stickers.iter().find(|s| s.id == clone_id).unwrap();
    assert_close(clone.placement.cx, 1955.0 + ops::DUPLICATE_OFFSET);
    assert_close(clone.placement.cy, 1095.0 + ops::DUPLICATE_OFFSET);

    // Клон за краем: ограничение видимости возвращает его в кадр — уход
    // ровно 90% (видно 10%), и по вертикали тоже.
    let clamped = snap::clamp_min_visible(&clone.placement, clone.transform.rotation, SCREEN);
    assert_close(clamped.cx, 1960.0);
    assert_close(clamped.cy, 1100.0);
    assert_close(
        clamped.cx + 50.0 - SCREEN.w,
        (1.0 - MIN_VISIBLE_FRACTION) * 100.0,
    );

    // Повторное ограничение идемпотентно; оригинал не пострадал.
    let again = snap::clamp_min_visible(&clamped, clone.transform.rotation, SCREEN);
    assert_close(again.cx, clamped.cx);
    assert_close(again.cy, clamped.cy);
    assert_close(
        cfg.stickers
            .iter()
            .find(|s| s.id == src)
            .unwrap()
            .placement
            .cx,
        1955.0,
    );
}

/// Дубликат с поворотом: `clamp_min_visible` использует AABB (40x100 у
/// стикера 100x40 на 90°) — по горизонтали лимит ухода 90% от 40.
#[test]
fn duplicate_clamp_uses_rotated_aabb() {
    let mut s = sticker(0, 1930.0, 1090.0, 100.0, 40.0);
    s.transform.rotation = FRAC_PI_2;
    let rotation = s.transform.rotation;
    let mut cfg = Config {
        stickers: vec![s],
        ..Default::default()
    };
    let src = cfg.stickers[0].id;

    let orig = snap::clamp_min_visible(&cfg.stickers[0].placement, rotation, SCREEN);
    assert_close(orig.cx, 1930.0);

    let clone_id = ops::duplicate(&mut cfg, src).unwrap();
    let clone = cfg.stickers.iter().find(|s| s.id == clone_id).unwrap();
    let clamped = snap::clamp_min_visible(&clone.placement, clone.transform.rotation, SCREEN);

    // max_x = 1920 - 0.1*40 = 1916 → cx = 1916 + 20 = 1936.
    assert_close(clamped.cx, 1936.0);
    // По вертикали (высокая ось 100) копия ещё в кадре — офсет сохранён.
    assert_close(clamped.cy, 1090.0 + ops::DUPLICATE_OFFSET);
}

/// `Ctrl+A` по непустому списку, затем протяжка рамкой по пустому списку
/// (все стикеры удалены): выделение просто снимается.
#[test]
fn select_all_then_rubber_band_empty_list_clears() {
    let stickers = vec![
        sticker(0, 100.0, 100.0, 40.0, 20.0),
        sticker(1, 300.0, 100.0, 40.0, 20.0),
    ];
    let mut sel = SelectionSet::new();
    sel.select_all(&stickers);
    assert_eq!(sel.len(), 2);

    let empty: Vec<Sticker> = Vec::new();
    sel.rubber_band(&empty, &DipRect::new(50.0, 50.0, 400.0, 200.0));
    assert!(sel.is_empty());
    assert!(sel.selected_stickers(&empty).is_empty());
    assert_eq!(sel.bounds(&empty), None);
}

/// `select_all` по пустому списку и `rubber_band` по пустому списку (в том
/// числе с вырожденной рамкой) — выделение остаётся пустым.
#[test]
fn select_all_empty_and_rubber_band_empty_stay_empty() {
    let mut sel = SelectionSet::new();
    sel.select_all(&[]);
    assert!(sel.is_empty());

    sel.rubber_band(&[], &DipRect::new(0.0, 0.0, 0.0, 0.0));
    assert!(sel.is_empty());
    sel.rubber_band(&[], &DipRect::new(100.0, 100.0, 50.0, 50.0));
    assert!(sel.is_empty());
}

/// Удаление части выделения + `prune`, затем новая протяжка рамкой: рамка
/// заменяет выделение целиком, по выжившему списку.
#[test]
fn delete_partial_selection_then_rubber_band_replaces() {
    let mut cfg = Config {
        stickers: vec![
            sticker(0, 100.0, 100.0, 40.0, 20.0), // AABB x=80..120, y=90..110
            sticker(1, 300.0, 100.0, 40.0, 20.0), // AABB x=280..320
            sticker(2, 500.0, 100.0, 40.0, 20.0), // AABB x=480..520
        ],
        ..Default::default()
    };
    let a = cfg.stickers[0].id;
    let b = cfg.stickers[1].id;

    let mut sel = SelectionSet::new();
    sel.select_all(&cfg.stickers);
    ops::delete(&mut cfg, a).unwrap();
    sel.prune(&cfg.stickers);
    assert_eq!(sel.len(), 2);

    // Рамка пересекает только b (x=250..310 ∩ 280..320) — выделение заменено.
    let rect = DipRect::new(250.0, 80.0, 60.0, 40.0);
    sel.rubber_band(&cfg.stickers, &rect);
    assert_eq!(sel.ids(), &[b]);
}
