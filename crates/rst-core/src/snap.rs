//! Геометрия перемещения стикера в режиме редактирования: магнит к краям,
//! центрам и углам монитора (ROADMAP.md M2 «Магнит к краям и центрам
//! монитора, отключается модификатором»), магнит к соседним стикерам (запрос
//! 2026-09-01: стикеры липнут друг к другу впритык и выравниваются) и
//! ограничение «минимум 10% видно с каждой стороны» (M2).
//!
//! Чистая логика: на вход — ось-выровненный bbox стикера и размер монитора
//! в DIP (ADR-010), на выход — смещение и прилипшие направляющие для
//! отрисовки линий-подсказок. Прилипание к углу получается автоматически:
//! одновременное срабатывание по обеим осям.

use crate::hittest::{DipRect, HandleKind, aabb};
use crate::model::Placement;

/// Настройки магнита.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapConfig {
    /// Магнит включён в настройках.
    pub enabled: bool,
    /// Радиус притягивания в DIP; прилипание при расстоянии `<= threshold`.
    /// Отрицательное значение или NaN эквивалентны отключению.
    pub threshold: f64,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 8.0,
        }
    }
}

/// Вертикальная направляющая монитора, к которой прилип стикер (ось X).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalLine {
    /// `x = 0`.
    Left,
    /// `x = monitor_w / 2`.
    Center,
    /// `x = monitor_w`.
    Right,
    /// Линия соседнего стикера ([`snap_move_with_peers`]): его левый край,
    /// центр или правый край — какая именно, понятно по координате в
    /// [`SnapResult`] (для отрисовки линии-подсказки она не нужна, важен сам
    /// факт «прилип к соседу»).
    Peer,
}

/// Горизонтальная направляющая монитора (ось Y).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalLine {
    /// `y = 0`.
    Top,
    /// `y = monitor_h / 2`.
    Center,
    /// `y = monitor_h`.
    Bottom,
    /// Линия соседнего стикера ([`snap_move_with_peers`]): его верхний край,
    /// центр или нижний край — как [`VerticalLine::Peer`], для отрисовки
    /// важна только координата, не вариант.
    Peer,
}

/// Результат магнита: куда сдвинуть стикер и какие направляющие показать.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SnapResult {
    /// Смещение по X, которое надо прибавить к позиции стикера.
    pub dx: f64,
    /// Смещение по Y.
    pub dy: f64,
    /// Прилипшая вертикальная направляющая и её координата X.
    pub vline: Option<(VerticalLine, f64)>,
    /// Прилипшая горизонтальная направляющая и её координата Y.
    pub hline: Option<(HorizontalLine, f64)>,
}

impl SnapResult {
    /// Прилипли хотя бы по одной оси?
    pub fn is_snapped(&self) -> bool {
        self.vline.is_some() || self.hline.is_some()
    }
}

/// Магнит при перемещении. `sticker` — текущий ось-выровненный bbox
/// стикера (для повёрнутого — через [`aabb`]), `monitor` — прямоугольник
/// монитора в тех же координатах (начало обычно в `(0, 0)`); всё в DIP.
/// `magnet_off` — снять магнит на этот жест. Какая именно клавиша это
/// делает, крейт не знает и знать не должен: `Alt` (запрос пользователя
/// 2026-09-01) и `Ctrl` при перемещении (ROADMAP.md M2) — решение
/// платформенного слоя, здесь только «да/нет».
///
/// На каждой оси кандидатами служат края и середина стикера, целями —
/// края и центр монитора; побеждает ближайшая пара в пределах порога.
/// Частный случай [`snap_move_with_peers`] без соседей.
pub fn snap_move(
    sticker: DipRect,
    monitor: DipRect,
    config: &SnapConfig,
    magnet_off: bool,
) -> SnapResult {
    snap_move_with_peers(sticker, monitor, &[], config, magnet_off)
}

/// Магнит при перемещении с учётом соседних стикеров. `sticker` — текущий
/// ось-выровненный bbox, `monitor` — прямоугольник монитора, `peers` — bbox
/// ДРУГИХ стикеров в тех же DIP-координатах (перетаскиваемого среди них
/// быть не должно — это забота вызывающего); всё в DIP. `magnet_off` —
/// временно отключить магнит.
///
/// На каждой оси кандидатами служат края и середина стикера, целями —
/// края и центр монитора ПЛЮС края и центры каждого соседа; побеждает
/// ближайшая пара в пределах порога. Этого достаточно, чтобы оба нужных
/// поведения получились сами собой:
/// - ПРИЛИПАНИЕ ВПРИТЫК: правый край перетаскиваемого встаёт ровно на левый
///   край соседа (и наоборот, а по Y — верх на низ и низ на верх) — щели и
///   нахлёста между стикерами не остаётся;
/// - ВЫРАВНИВАНИЕ: верхние края двух стикеров (или центры, или нижние края)
///   становятся на одну линию.
///
/// По три направляющие на соседа — ровно как на монитор: у стикера три
/// характерные точки (лево/центр/право), и каждой нужна своя цель на соседе;
/// совпадение точки стикера с любой линией соседа даёт осмысленное
/// выравнивание, а линий больше трёх не бывает — любая другая дублирует одну
/// из трёх. Направляющие соседей идут ПОСЛЕ мониторных: при равных
/// расстояниях (ровно на пороге) побеждает монитор — рамка экрана
/// приоритетнее соседа.
pub fn snap_move_with_peers(
    sticker: DipRect,
    monitor: DipRect,
    peers: &[DipRect],
    config: &SnapConfig,
    magnet_off: bool,
) -> SnapResult {
    let mut result = SnapResult::default();
    if magnet_off || !config.enabled || config.threshold < 0.0 || config.threshold.is_nan() {
        return result;
    }
    // Два вектора на вызов — по одному на ось, каждый собран в один проход
    // (`collect_guides`): на горячем пути перетаскивания лишних аллокаций
    // не нужно, а три направляющие монитора + три на соседа в одном буфере
    // сохраняют порядок «монитор раньше соседей».
    let x_guides = collect_guides(monitor, peers, triple_x);
    let y_guides = collect_guides(monitor, peers, triple_y);
    let (dx, guide) = snap_axis(triple_x(sticker), &x_guides, config.threshold);
    result.dx = dx;
    result.vline = guide.map(|(i, x)| (vertical_guide(i), x));
    let (dy, guide) = snap_axis(triple_y(sticker), &y_guides, config.threshold);
    result.dy = dy;
    result.hline = guide.map(|(i, y)| (horizontal_guide(i), y));
    result
}

/// Магнит по [`Placement`] с учётом поворота: bbox вычисляется через
/// [`aabb`], результат (смещение) применяется к `cx`/`cy`. Частный случай
/// [`snap_placement_with_peers`] без соседей.
pub fn snap_placement(
    placement: &Placement,
    rotation: f64,
    monitor: DipRect,
    config: &SnapConfig,
    magnet_off: bool,
) -> SnapResult {
    snap_placement_with_peers(placement, rotation, monitor, &[], config, magnet_off)
}

/// То же, что [`snap_placement`], но с учётом соседних стикеров — делегирует
/// [`snap_move_with_peers`] на bbox повёрнутого стикера.
pub fn snap_placement_with_peers(
    placement: &Placement,
    rotation: f64,
    monitor: DipRect,
    peers: &[DipRect],
    config: &SnapConfig,
    magnet_off: bool,
) -> SnapResult {
    snap_move_with_peers(
        aabb(placement, rotation),
        monitor,
        peers,
        config,
        magnet_off,
    )
}

/// Кромки прямоугольника, которые тянет ручка ресайза.
///
/// Ручка двигает одну кромку по каждой оси или ни одной: угловая — по обеим,
/// боковая — только по своей. Магниту важно ровно это: подтягивать надо
/// кромку, которая едет за пальцем, а не противоположную ей неподвижную.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResizeEdges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl ResizeEdges {
    /// Какие кромки двигает эта ручка.
    pub fn of(handle: HandleKind) -> Self {
        let (left, right) = match handle {
            HandleKind::NorthWest | HandleKind::West | HandleKind::SouthWest => (true, false),
            HandleKind::NorthEast | HandleKind::East | HandleKind::SouthEast => (false, true),
            HandleKind::North | HandleKind::South => (false, false),
        };
        let (top, bottom) = match handle {
            HandleKind::NorthWest | HandleKind::North | HandleKind::NorthEast => (true, false),
            HandleKind::SouthWest | HandleKind::South | HandleKind::SouthEast => (false, true),
            HandleKind::East | HandleKind::West => (false, false),
        };
        Self {
            left,
            right,
            top,
            bottom,
        }
    }
}

/// Магнит при ИЗМЕНЕНИИ РАЗМЕРА: на сколько сдвинуть движущиеся кромки
/// `rect`, чтобы они сели на ближайшие направляющие (края и центр монитора,
/// края и центры соседних стикеров). Возвращает поправку по каждой оси; ноль
/// — прилипать не к чему.
///
/// Почему поправка, а не готовый прямоугольник: вызывающий слой считает
/// размер не сам, а через `transform_ops::resize`, которая знает про
/// блокировку пропорций, зеркалирование и минимальный размер. Подсунуть ей
/// исправленное смещение пальца — единственный способ получить магнит, не
/// продублировав всю эту логику и не разойдясь с ней. При заблокированных
/// пропорциях кромка сядет на направляющую не идеально точно (вторую сторону
/// задаст соотношение) — и это правильнее, чем сломанные пропорции.
///
/// `disabled` — магнит выключен на этот жест (зажат модификатор).
pub fn snap_resize_delta(
    rect: DipRect,
    monitor: DipRect,
    peers: &[DipRect],
    edges: ResizeEdges,
    config: &SnapConfig,
    disabled: bool,
) -> (f64, f64) {
    if disabled || !config.enabled || config.threshold < 0.0 || config.threshold.is_nan() {
        return (0.0, 0.0);
    }
    let dx = edge_delta(
        moving_edge(rect.x, rect.x + rect.w, edges.left, edges.right),
        &collect_guides(monitor, peers, triple_x),
        config.threshold,
    );
    let dy = edge_delta(
        moving_edge(rect.y, rect.y + rect.h, edges.top, edges.bottom),
        &collect_guides(monitor, peers, triple_y),
        config.threshold,
    );
    (dx, dy)
}

/// Координата движущейся кромки по оси; `None` — по этой оси ручка ничего
/// не двигает (боковая ручка).
fn moving_edge(low: f64, high: f64, low_moves: bool, high_moves: bool) -> Option<f64> {
    match (low_moves, high_moves) {
        (true, false) => Some(low),
        (false, true) => Some(high),
        // Обе кромки сразу не двигает ни одна ручка; «ни одной» — боковая
        // ручка по своей поперечной оси.
        _ => None,
    }
}

/// Ближайшая направляющая к кромке в пределах порога — как смещение.
fn edge_delta(edge: Option<f64>, guides: &[f64], threshold: f64) -> f64 {
    let Some(edge) = edge else {
        return 0.0;
    };
    guides
        .iter()
        .map(|g| g - edge)
        .filter(|d| d.abs() <= threshold)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(0.0)
}

/// Одномерный магнит: ближайшая пара «точка стикера — направляющая»
/// в пределах `threshold`. Возвращает смещение и `(индекс направляющей,
/// её координату)`. При равном расстоянии побеждает более ранняя пара
/// (порядок: сначала направляющие, внутри — точки стикера).
///
/// Индекс направляющей из [`snap_move_with_peers`]: 0/1/2 — линии монитора
/// (Left/Center/Right), всё, что дальше, — линии соседей (`Peer`). Возвращаем
/// именно индекс, а не вариант enum: [`snap_axis`] не знает, по какой оси его
/// зовут, а enum для X и Y разные; раскладывает индекс в вариант вызывающий
/// ([`vertical_guide`]/[`horizontal_guide`]).
fn snap_axis(points: [f64; 3], guides: &[f64], threshold: f64) -> (f64, Option<(usize, f64)>) {
    // (индекс, координата направляющей, смещение, |смещение|)
    let mut best: Option<(usize, f64, f64, f64)> = None;
    for (gi, &g) in guides.iter().enumerate() {
        for &p in &points {
            let delta = g - p;
            let dist = delta.abs();
            if dist <= threshold && best.as_ref().is_none_or(|&(_, _, _, d)| dist < d) {
                best = Some((gi, g, delta, dist));
            }
        }
    }
    match best {
        Some((gi, g, delta, _)) => (delta, Some((gi, g))),
        None => (0.0, None),
    }
}

/// Направляющие одной оси в одном `Vec`: три линии монитора, затем по три на
/// каждого соседа (значения `triple`). Ёмкость известна заранее, сбор — один
/// проход: на горячем пути перетаскивания не строим вектор по кускам.
fn collect_guides(
    monitor: DipRect,
    peers: &[DipRect],
    triple: fn(DipRect) -> [f64; 3],
) -> Vec<f64> {
    let mut guides = Vec::with_capacity(3 + peers.len() * 3);
    guides.extend(triple(monitor));
    for &peer in peers {
        guides.extend(triple(peer));
    }
    guides
}

/// Три характерные координаты прямоугольника по оси X: левый край, центр,
/// правый край.
fn triple_x(rect: DipRect) -> [f64; 3] {
    [rect.x, rect.x + rect.w / 2.0, rect.x + rect.w]
}

/// Три характерные координаты прямоугольника по оси Y: верх, центр, низ.
fn triple_y(rect: DipRect) -> [f64; 3] {
    [rect.y, rect.y + rect.h / 2.0, rect.y + rect.h]
}

/// Вариант вертикальной направляющей по индексу из [`snap_axis`]. Явный match
/// вместо арифметики по индексу в вызывающем коде: связь «первые три индекса —
/// монитор, дальше — соседи» видна сразу и не ломается при расширении.
fn vertical_guide(i: usize) -> VerticalLine {
    match i {
        0 => VerticalLine::Left,
        1 => VerticalLine::Center,
        2 => VerticalLine::Right,
        _ => VerticalLine::Peer,
    }
}

/// То же для горизонтальной оси ([`vertical_guide`]).
fn horizontal_guide(i: usize) -> HorizontalLine {
    match i {
        0 => HorizontalLine::Top,
        1 => HorizontalLine::Center,
        2 => HorizontalLine::Bottom,
        _ => HorizontalLine::Peer,
    }
}

/// Минимальная доля bbox стикера, которая обязана оставаться видимой
/// с каждой стороны монитора (SPEC.md, раздел 1).
pub const MIN_VISIBLE_FRACTION: f64 = 0.1;

/// Ограничение «минимум 10% видно с каждой стороны» (ROADMAP.md M2):
/// возвращает копию `placement` со скорректированным центром, при которой
/// ось-выровненный bbox стикера (с учётом поворота, [`crate::hittest::aabb`])
/// уходит за каждую сторону `monitor` не более чем на
/// `1 - MIN_VISIBLE_FRACTION` своей ширины/высоты.
///
/// Позиция, уже удовлетворяющая ограничению, не меняется. Стикер
/// с дегенеративным размером (нулевым, отрицательным или NaN) или монитор
/// нулевого размера возвращаются как есть — ограничение для них не определено.
pub fn clamp_min_visible(placement: &Placement, rotation: f64, monitor: DipRect) -> Placement {
    let bbox = aabb(placement, rotation);
    if !(bbox.w > 0.0 && bbox.h > 0.0 && monitor.w > 0.0 && monitor.h > 0.0) {
        return placement.clone();
    }
    let min_x = monitor.x - (1.0 - MIN_VISIBLE_FRACTION) * bbox.w;
    let max_x = monitor.x + monitor.w - MIN_VISIBLE_FRACTION * bbox.w;
    let min_y = monitor.y - (1.0 - MIN_VISIBLE_FRACTION) * bbox.h;
    let max_y = monitor.y + monitor.h - MIN_VISIBLE_FRACTION * bbox.h;
    let dx = bbox.x.clamp(min_x, max_x) - bbox.x;
    let dy = bbox.y.clamp(min_y, max_y) - bbox.y;
    let mut clamped = placement.clone();
    clamped.cx += dx;
    clamped.cy += dy;
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    const MONITOR: DipRect = DipRect::new(0.0, 0.0, 1920.0, 1080.0);

    fn snap(left: f64, top: f64, w: f64, h: f64, cfg: &SnapConfig, ctrl: bool) -> SnapResult {
        snap_move(DipRect::new(left, top, w, h), MONITOR, cfg, ctrl)
    }

    /// Магнит с соседями: bbox перетаскиваемого, монитор и порог по умолчанию.
    fn snap_peers(sticker: DipRect, peers: &[DipRect], ctrl: bool) -> SnapResult {
        snap_move_with_peers(sticker, MONITOR, peers, &SnapConfig::default(), ctrl)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "ожидалось {expected}, получено {actual}"
        );
    }

    /// Табличные тесты (CONTRIBUTING.md, «Правило границ и магнит»).
    /// Стикер 100x50; порог по умолчанию 8 DIP.
    #[test]
    fn snap_move_table() {
        struct Case {
            name: &'static str,
            left: f64,
            top: f64,
            dx: f64,
            dy: f64,
            vline: Option<VerticalLine>,
            hline: Option<HorizontalLine>,
        }
        let cases = [
            Case {
                name: "далеко от направляющих",
                left: 500.0,
                top: 400.0,
                dx: 0.0,
                dy: 0.0,
                vline: None,
                hline: None,
            },
            Case {
                name: "левый край в пределах порога",
                left: 5.0,
                top: 400.0,
                dx: -5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Left),
                hline: None,
            },
            Case {
                name: "правый край к правому краю",
                left: 1815.0,
                top: 400.0,
                dx: 5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Right),
                hline: None,
            },
            Case {
                name: "центр к центру по X",
                left: 905.0,
                top: 400.0,
                dx: 5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Center),
                hline: None,
            },
            Case {
                name: "верх в пределах порога",
                left: 500.0,
                top: 3.0,
                dx: 0.0,
                dy: -3.0,
                vline: None,
                hline: Some(HorizontalLine::Top),
            },
            Case {
                name: "низ к низу",
                left: 500.0,
                top: 1026.0,
                dx: 0.0,
                dy: 4.0,
                vline: None,
                hline: Some(HorizontalLine::Bottom),
            },
            Case {
                name: "середина к центру по Y",
                left: 500.0,
                top: 512.0,
                dx: 0.0,
                dy: 3.0,
                vline: None,
                hline: Some(HorizontalLine::Center),
            },
            Case {
                name: "угол: лево + верх одновременно",
                left: 6.0,
                top: -7.0,
                dx: -6.0,
                dy: 7.0,
                vline: Some(VerticalLine::Left),
                hline: Some(HorizontalLine::Top),
            },
            Case {
                name: "точно на пороге — прилипает (включительно)",
                left: 8.0,
                top: 400.0,
                dx: -8.0,
                dy: 0.0,
                vline: Some(VerticalLine::Left),
                hline: None,
            },
            Case {
                name: "за порогом — не прилипает",
                left: 9.0,
                top: 400.0,
                dx: 0.0,
                dy: 0.0,
                vline: None,
                hline: None,
            },
            Case {
                name: "центр стикера к краю монитора",
                left: -45.0,
                top: 400.0,
                dx: -5.0,
                dy: 0.0,
                vline: Some(VerticalLine::Left),
                hline: None,
            },
            Case {
                name: "уже выровнен: смещение 0, направляющая есть",
                left: 910.0,
                top: 400.0,
                dx: 0.0,
                dy: 0.0,
                vline: Some(VerticalLine::Center),
                hline: None,
            },
        ];
        for c in &cases {
            let r = snap(c.left, c.top, 100.0, 50.0, &SnapConfig::default(), false);
            assert_close(r.dx, c.dx);
            assert_close(r.dy, c.dy);
            assert_eq!(r.vline.map(|(g, _)| g), c.vline, "{}: vline", c.name);
            assert_eq!(r.hline.map(|(g, _)| g), c.hline, "{}: hline", c.name);
            assert_eq!(
                r.is_snapped(),
                c.vline.is_some() || c.hline.is_some(),
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn guide_coordinates_reported() {
        let r = snap(905.0, 400.0, 100.0, 50.0, &SnapConfig::default(), false);
        let Some((VerticalLine::Center, x)) = r.vline else {
            panic!("ожидался центр")
        };
        assert_close(x, 960.0);
        let r = snap(5.0, 400.0, 100.0, 50.0, &SnapConfig::default(), false);
        let Some((VerticalLine::Left, x)) = r.vline else {
            panic!("ожидался левый край")
        };
        assert_close(x, 0.0);
    }

    #[test]
    fn ctrl_disables_snap() {
        let r = snap(5.0, 3.0, 100.0, 50.0, &SnapConfig::default(), true);
        assert!(!r.is_snapped());
        assert_close(r.dx, 0.0);
        assert_close(r.dy, 0.0);
    }

    #[test]
    fn disabled_config_disables_snap() {
        let cfg = SnapConfig {
            enabled: false,
            ..SnapConfig::default()
        };
        let r = snap(5.0, 3.0, 100.0, 50.0, &cfg, false);
        assert!(!r.is_snapped());
    }

    #[test]
    fn invalid_threshold_disables_snap() {
        for threshold in [-1.0, f64::NAN] {
            let cfg = SnapConfig {
                enabled: true,
                threshold,
            };
            let r = snap(5.0, 3.0, 100.0, 50.0, &cfg, false);
            assert!(!r.is_snapped(), "threshold={threshold}");
        }
    }

    #[test]
    fn axis_nearest_pair_wins() {
        // Точка 7.0 ближе к 0 (7), точка 17.0 ближе к 20 (3): побеждает 17->20.
        let (delta, guide) = snap_axis([7.0, 17.0, 27.0], &[0.0, 20.0, 40.0], 8.0);
        assert_close(delta, 3.0);
        assert_eq!(guide.map(|(i, _)| i), Some(1));
    }

    #[test]
    fn axis_tie_prefers_earlier_guide() {
        // Оба расстояния равны 10 и на пороге: побеждает первая направляющая.
        let (delta, guide) = snap_axis([10.0, 10.0, 10.0], &[0.0, 20.0, 40.0], 10.0);
        assert_close(delta, -10.0);
        assert_eq!(guide.map(|(i, _)| i), Some(0));
    }

    #[test]
    fn axis_nan_points_do_not_snap() {
        let (_, guide) = snap_axis([f64::NAN; 3], &[0.0, 20.0, 40.0], 8.0);
        assert_eq!(guide, None);
    }

    #[test]
    fn peer_tight_snap_all_four_sides() {
        let sticker = DipRect::new(500.0, 400.0, 100.0, 50.0);
        // Правый край перетаскиваемого встаёт ровно на левый край соседа —
        // щели и нахлёста нет: смещение ровно равно зазору в 8 DIP.
        let r = snap_peers(sticker, &[DipRect::new(608.0, 460.0, 100.0, 50.0)], false);
        assert_close(r.dx, 8.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Peer));
        assert_close(r.dy, 0.0);
        assert_eq!(r.hline, None);
        // Левый край перетаскиваемого встаёт на правый край соседа.
        let r = snap_peers(sticker, &[DipRect::new(392.0, 460.0, 100.0, 50.0)], false);
        assert_close(r.dx, -8.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Peer));
        assert_close(r.dy, 0.0);
        assert_eq!(r.hline, None);
        // Верх перетаскиваемого встаёт на низ соседа (сосед выше).
        let r = snap_peers(sticker, &[DipRect::new(520.0, 342.0, 100.0, 50.0)], false);
        assert_close(r.dy, -8.0);
        assert_eq!(r.hline.map(|(g, _)| g), Some(HorizontalLine::Peer));
        assert_close(r.dx, 0.0);
        assert_eq!(r.vline, None);
        // Низ перетаскиваемого встаёт на верх соседа (сосед ниже).
        let r = snap_peers(sticker, &[DipRect::new(520.0, 458.0, 100.0, 50.0)], false);
        assert_close(r.dy, 8.0);
        assert_eq!(r.hline.map(|(g, _)| g), Some(HorizontalLine::Peer));
        assert_close(r.dx, 0.0);
        assert_eq!(r.vline, None);
    }

    #[test]
    fn peer_edges_and_centers_align() {
        let sticker = DipRect::new(500.0, 400.0, 100.0, 50.0);
        // Верхние края на одну линию (при равном расстоянии побеждает более
        // ранняя пара — верх к верху, а не центр к центру).
        let r = snap_peers(sticker, &[DipRect::new(700.0, 405.0, 100.0, 50.0)], false);
        assert_close(r.dy, 5.0);
        assert_eq!(r.hline.map(|(g, _)| g), Some(HorizontalLine::Peer));
        // Левые края на одну линию.
        let r = snap_peers(sticker, &[DipRect::new(505.0, 500.0, 100.0, 50.0)], false);
        assert_close(r.dx, 5.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Peer));
        // Центры по X на одну линию.
        let r = snap_peers(sticker, &[DipRect::new(507.0, 460.0, 100.0, 50.0)], false);
        assert_close(r.dx, 7.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Peer));
        // Центры по Y на одну линию.
        let r = snap_peers(sticker, &[DipRect::new(520.0, 407.0, 100.0, 50.0)], false);
        assert_close(r.dy, 7.0);
        assert_eq!(r.hline.map(|(g, _)| g), Some(HorizontalLine::Peer));
    }

    #[test]
    fn nearest_peer_wins() {
        let sticker = DipRect::new(500.0, 400.0, 100.0, 50.0);
        // Два соседа справа: правый край (600) до P2 (606) — 6 DIP, до P1
        // (608) — 8 DIP; побеждает ближайший, смещение ровно 6, а не 8.
        let peers = [
            DipRect::new(608.0, 460.0, 100.0, 50.0),
            DipRect::new(606.0, 460.0, 100.0, 50.0),
        ];
        let r = snap_peers(sticker, &peers, false);
        assert_close(r.dx, 6.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Peer));
        assert_close(r.vline.unwrap().1, 606.0);
    }

    #[test]
    fn peer_beyond_threshold_ignored() {
        let sticker = DipRect::new(500.0, 400.0, 100.0, 50.0);
        // Зазор 20 DIP больше порога 8 — сосед не влияет, как будто его нет.
        let peers = [DipRect::new(620.0, 460.0, 100.0, 50.0)];
        let r = snap_peers(sticker, &peers, false);
        assert!(!r.is_snapped());
        assert_close(r.dx, 0.0);
        assert_close(r.dy, 0.0);
    }

    #[test]
    fn ctrl_disables_peer_snap() {
        let sticker = DipRect::new(500.0, 400.0, 100.0, 50.0);
        let peers = [DipRect::new(608.0, 460.0, 100.0, 50.0)];
        let r = snap_peers(sticker, &peers, true);
        assert!(!r.is_snapped());
        assert_close(r.dx, 0.0);
        assert_close(r.dy, 0.0);
    }

    #[test]
    fn empty_peers_matches_snap_move() {
        // Новый API с пустым срезом соседей обязан давать РОВНО тот же
        // результат, что и старый `snap_move` (он на него и делегирует) — и
        // с Ctrl, и без, на позициях, покрывающих и срабатывание, и тишину.
        let positions = [
            DipRect::new(5.0, 400.0, 100.0, 50.0),
            DipRect::new(905.0, 512.0, 100.0, 50.0),
            DipRect::new(1815.0, 1026.0, 100.0, 50.0),
            DipRect::new(500.0, 400.0, 100.0, 50.0),
            DipRect::new(-45.0, -7.0, 100.0, 50.0),
        ];
        let cfg = SnapConfig::default();
        for &s in &positions {
            for ctrl in [false, true] {
                let old = snap_move(s, MONITOR, &cfg, ctrl);
                let new = snap_move_with_peers(s, MONITOR, &[], &cfg, ctrl);
                assert_eq!(old, new, "sticker={s:?} ctrl={ctrl}");
            }
        }
    }

    #[test]
    fn snap_placement_with_peers_snaps_to_peer() {
        // placement cx/cy = (550, 425) даёт bbox (500, 400, 100, 50) без
        // поворота — правый край 600 к левому краю соседа 608, зазор 8.
        let p = placement_at(550.0, 425.0, 100.0, 50.0);
        let peers = [DipRect::new(608.0, 460.0, 100.0, 50.0)];
        let r = snap_placement_with_peers(&p, 0.0, MONITOR, &peers, &SnapConfig::default(), false);
        assert_close(r.dx, 8.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Peer));
    }

    #[test]
    fn sticker_wider_than_monitor_snaps_center_aligned() {
        // Центр уже на центре: смещение 0, но направляющая активна.
        let r = snap(-40.0, 0.0, 2000.0, 50.0, &SnapConfig::default(), false);
        assert_close(r.dx, 0.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Center));
        assert_eq!(r.hline.map(|(g, _)| g), Some(HorizontalLine::Top));
    }

    #[test]
    fn snap_placement_respects_rotation() {
        let p = Placement {
            cx: 25.0,
            cy: 540.0,
            w: 100.0,
            h: 40.0,
            ..Placement::default()
        };
        let cfg = SnapConfig::default();
        // Без поворота: левый край bbox на -25 — до края монитора 25 DIP,
        // дальше порога, магнит по X молчит.
        let r = snap_placement(&p, 0.0, MONITOR, &cfg, false);
        assert_close(r.dx, 0.0);
        assert_eq!(r.vline.map(|(g, _)| g), None);
        // Поворот на 90°: bbox 40x100, левый край на 5 — в пределах порога.
        let r = snap_placement(&p, FRAC_PI_2, MONITOR, &cfg, false);
        assert_close(r.dx, -5.0);
        assert_eq!(r.vline.map(|(g, _)| g), Some(VerticalLine::Left));
    }

    fn placement_at(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        Placement {
            cx,
            cy,
            w,
            h,
            ..Placement::default()
        }
    }

    fn clamped(cx: f64, cy: f64, w: f64, h: f64) -> Placement {
        clamp_min_visible(&placement_at(cx, cy, w, h), 0.0, MONITOR)
    }

    #[test]
    fn inside_monitor_is_unchanged() {
        let p = clamped(960.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, 960.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn partially_out_within_90_percent_is_unchanged() {
        // bbox 100x50 уходит влево на 80% ширины — допустимо.
        let p = clamped(-30.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, -30.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn far_left_clamps_to_10_percent_visible() {
        let p = clamped(-5000.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, -40.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn far_right_clamps_to_10_percent_visible() {
        let p = clamped(5000.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, 1960.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn far_top_clamps_to_10_percent_visible() {
        let p = clamped(960.0, -5000.0, 100.0, 50.0);
        assert_close(p.cx, 960.0);
        assert_close(p.cy, -20.0);
    }

    #[test]
    fn far_bottom_clamps_to_10_percent_visible() {
        let p = clamped(960.0, 5000.0, 100.0, 50.0);
        assert_close(p.cx, 960.0);
        assert_close(p.cy, 1100.0);
    }

    #[test]
    fn corner_clamps_both_axes() {
        let p = clamped(-5000.0, -5000.0, 100.0, 50.0);
        assert_close(p.cx, -40.0);
        assert_close(p.cy, -20.0);
    }

    #[test]
    fn exactly_at_ten_percent_is_unchanged() {
        // bbox.x = -90 ровно: слева видно ровно 10% ширины.
        let p = clamped(-40.0, 540.0, 100.0, 50.0);
        assert_close(p.cx, -40.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn rotation_uses_aabb_dimensions() {
        // 100x40 при повороте 90°: bbox 40x100 — ограничение по ширине 40.
        let p = clamp_min_visible(
            &placement_at(-5000.0, 540.0, 100.0, 40.0),
            FRAC_PI_2,
            MONITOR,
        );
        assert_close(p.cx, -16.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn offset_monitor_clamps_relatively() {
        let m = DipRect::new(100.0, 200.0, 1920.0, 1080.0);
        let p = clamp_min_visible(&placement_at(-5000.0, -5000.0, 100.0, 50.0), 0.0, m);
        assert_close(p.cx, 60.0);
        assert_close(p.cy, 180.0);
    }

    #[test]
    fn sticker_wider_than_monitor_still_clamps() {
        // bbox 2000x50 в мониторе 1920: уход влево ограничен 90% ширины.
        let p = clamped(-5000.0, 540.0, 2000.0, 50.0);
        assert_close(p.cx, -800.0);
        assert_close(p.cy, 540.0);
    }

    #[test]
    fn degenerate_sticker_is_unchanged() {
        for (w, h) in [(0.0, 50.0), (-10.0, 50.0), (100.0, 0.0)] {
            let p = clamp_min_visible(&placement_at(-5000.0, -5000.0, w, h), 0.0, MONITOR);
            assert_close(p.cx, -5000.0);
            assert_close(p.cy, -5000.0);
        }
    }

    #[test]
    fn nan_size_is_unchanged() {
        let p = clamp_min_visible(
            &placement_at(-5000.0, -5000.0, f64::NAN, 50.0),
            0.0,
            MONITOR,
        );
        assert_close(p.cx, -5000.0);
        assert_close(p.cy, -5000.0);
    }

    #[test]
    fn zero_size_monitor_is_unchanged() {
        let zero = DipRect::new(0.0, 0.0, 0.0, 0.0);
        let p = clamp_min_visible(&placement_at(-5000.0, -5000.0, 100.0, 50.0), 0.0, zero);
        assert_close(p.cx, -5000.0);
        assert_close(p.cy, -5000.0);
    }
    // --- Магнит при ресайзе (запрос пользователя 2026-09-01) ---

    fn free_snap() -> SnapConfig {
        SnapConfig {
            enabled: true,
            threshold: 8.0,
        }
    }

    /// Каждая ручка двигает ровно те кромки, за которые её и тянут.
    #[test]
    fn resize_edges_match_the_handle() {
        let e = ResizeEdges::of(HandleKind::NorthWest);
        assert!(e.left && e.top && !e.right && !e.bottom);
        let e = ResizeEdges::of(HandleKind::SouthEast);
        assert!(e.right && e.bottom && !e.left && !e.top);
        // Боковая ручка по поперечной оси не двигает ничего — и магнит по
        // этой оси обязан молчать, иначе он тянул бы неподвижную кромку.
        let e = ResizeEdges::of(HandleKind::East);
        assert!(e.right && !e.left && !e.top && !e.bottom);
        let e = ResizeEdges::of(HandleKind::North);
        assert!(e.top && !e.bottom && !e.left && !e.right);
    }

    /// Правая кромка садится на левый край соседа — окна встают впритык.
    #[test]
    fn resize_snaps_the_dragged_edge_to_a_peer() {
        let rect = DipRect::new(100.0, 100.0, 195.0, 100.0); // правый край 295
        let peer = DipRect::new(300.0, 400.0, 100.0, 100.0); // левый край 300
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let (dx, dy) = snap_resize_delta(
            rect,
            monitor,
            &[peer],
            ResizeEdges::of(HandleKind::East),
            &free_snap(),
            false,
        );
        assert_eq!(dx, 5.0, "правая кромка должна доехать до 300");
        assert_eq!(dy, 0.0, "боковая ручка по вертикали ничего не двигает");
    }

    /// Тянут левую кромку — подтягивается она, а не правая.
    #[test]
    fn resize_snaps_the_left_edge_when_it_is_the_one_dragged() {
        let rect = DipRect::new(303.0, 100.0, 200.0, 100.0);
        let peer = DipRect::new(100.0, 100.0, 200.0, 100.0); // правый край 300
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let (dx, _) = snap_resize_delta(
            rect,
            monitor,
            &[peer],
            ResizeEdges::of(HandleKind::West),
            &free_snap(),
            false,
        );
        assert_eq!(dx, -3.0, "левая кромка должна доехать до 300");
    }

    /// Угловая ручка подтягивает обе свои кромки разом.
    #[test]
    fn a_corner_handle_snaps_both_axes() {
        let rect = DipRect::new(100.0, 100.0, 196.0, 96.0); // правый 296, низ 196
        let peer = DipRect::new(300.0, 200.0, 100.0, 100.0); // левый 300, верх 200
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let (dx, dy) = snap_resize_delta(
            rect,
            monitor,
            &[peer],
            ResizeEdges::of(HandleKind::SouthEast),
            &free_snap(),
            false,
        );
        assert_eq!((dx, dy), (4.0, 4.0));
    }

    /// Направляющие монитора работают так же, как соседи.
    #[test]
    fn resize_snaps_to_the_monitor_edge() {
        let rect = DipRect::new(100.0, 100.0, 1815.0, 100.0); // правый край 1915
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let (dx, _) = snap_resize_delta(
            rect,
            monitor,
            &[],
            ResizeEdges::of(HandleKind::East),
            &free_snap(),
            false,
        );
        assert_eq!(dx, 5.0);
    }

    /// Дальше порога магнит молчит: 8 DIP — это 8, а не «примерно».
    #[test]
    fn resize_ignores_a_guide_beyond_the_threshold() {
        let rect = DipRect::new(100.0, 100.0, 180.0, 100.0); // правый край 280
        let peer = DipRect::new(300.0, 400.0, 100.0, 100.0);
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let (dx, _) = snap_resize_delta(
            rect,
            monitor,
            &[peer],
            ResizeEdges::of(HandleKind::East),
            &free_snap(),
            false,
        );
        assert_eq!(dx, 0.0);
    }

    /// Зажатый модификатор выключает магнит целиком — как и при перемещении.
    #[test]
    fn resize_magnet_is_off_when_disabled() {
        let rect = DipRect::new(100.0, 100.0, 195.0, 100.0);
        let peer = DipRect::new(300.0, 400.0, 100.0, 100.0);
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        assert_eq!(
            snap_resize_delta(
                rect,
                monitor,
                &[peer],
                ResizeEdges::of(HandleKind::East),
                &free_snap(),
                true,
            ),
            (0.0, 0.0)
        );
        // И выключенная в настройках магнитность — тоже.
        let off = SnapConfig {
            enabled: false,
            threshold: 8.0,
        };
        assert_eq!(
            snap_resize_delta(
                rect,
                monitor,
                &[peer],
                ResizeEdges::of(HandleKind::East),
                &off,
                false,
            ),
            (0.0, 0.0)
        );
    }

    /// Из нескольких направляющих побеждает ближайшая.
    #[test]
    fn resize_picks_the_nearest_guide() {
        let rect = DipRect::new(100.0, 100.0, 197.0, 100.0); // правый край 297
        let near = DipRect::new(299.0, 400.0, 50.0, 50.0); // левый край 299
        let far = DipRect::new(303.0, 400.0, 50.0, 50.0); // левый край 303
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let (dx, _) = snap_resize_delta(
            rect,
            monitor,
            &[far, near],
            ResizeEdges::of(HandleKind::East),
            &free_snap(),
            false,
        );
        assert_eq!(dx, 2.0, "ближе направляющая 299, а не 303");
    }
}
