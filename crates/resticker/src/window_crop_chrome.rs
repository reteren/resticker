//! Обвязка живого куска окна: полоса управления и свернутый вид.
//!
//! Модуль намеренно не знает о состоянии, вводе или захвате. Координатор
//! передаёт геометрию и состояние наведения, получает примитивы и отдельно
//! решает, что делать с [`ChromeHit`]. Так десяток кусков не превращается в
//! постоянный ряд заголовков: полоса строится только при наведении.

#![allow(dead_code)]

use rst_core::hittest::DipRect;
use rst_render::{Box2D, Icon, Primitive, glass_panel, text_size, theme};

use crate::window_picker::truncate_to_width;

/// Размер полосы и квадратных кнопок, DIP. Общий токен гарантирует, что
/// область кнопки, её иконка и зона попадания имеют одну видимую геометрию.
pub const CHROME_HEIGHT: f64 = theme::BUTTON_SIZE;
/// Отступы/зазоры обвязки берутся из Dark Liquid Glass, чтобы она не стала
/// отдельным визуальным языком рядом с обычным тулбаром.
pub const CHROME_PAD: f64 = theme::BUTTON_PAD;
pub const CHROME_GAP: f64 = theme::BUTTON_PAD;

/// Геометрия видимой полосы управления над куском.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromeLayout {
    /// Исходный прямоугольник куска в DIP.
    pub piece: DipRect,
    /// Полоса поверх верхней кромки куска.
    pub bar: DipRect,
    /// Оставшаяся часть полосы, за которую можно тащить кусок.
    pub drag: DipRect,
    /// Кнопка «свернуть».
    pub minimize: DipRect,
    /// Кнопка «закрыть».
    pub close: DipRect,
}

/// Построить полосу над верхней кромкой куска.
///
/// Полоса намеренно **накладывается** на верхнюю часть содержимого, вместо
/// того чтобы занимать место над ним. Так кусок с `y == 0` не выталкивает
/// управление за границу монитора; цена — верхняя строка содержимого на миг
/// перекрыта только при наведении, что лучше постоянного сдвига геометрии.
/// Если кусок уже частично выше монитора, полоса дополнительно прижимается к
/// `monitor_top`, а не создаёт недоступную отрицательную область.
pub fn chrome_layout(piece: DipRect, monitor_top: f64, monitor_bottom: f64) -> ChromeLayout {
    let bar_h = CHROME_HEIGHT;
    let monitor_h = (monitor_bottom - monitor_top).max(0.0);
    let max_bar_y = (monitor_bottom - bar_h).max(monitor_top);
    let bar_y = (piece.y.max(monitor_top)).min(max_bar_y);

    // Узкий кусок получает минимальную ширину, чтобы две кнопки оставались
    // раздельными. Полоса может немного выступить вправо, но содержимое
    // самого куска не меняет размера и его координаты не ломаются.
    let min_bar_w = 2.0 * CHROME_PAD + 2.0 * theme::BUTTON_SIZE + CHROME_GAP;
    let bar_w = piece.w.max(min_bar_w);
    let right = piece.x + bar_w;
    let close = DipRect::new(
        right - CHROME_PAD - theme::BUTTON_SIZE,
        bar_y,
        theme::BUTTON_SIZE,
        bar_h,
    );
    let minimize = DipRect::new(
        close.x - CHROME_GAP - theme::BUTTON_SIZE,
        bar_y,
        theme::BUTTON_SIZE,
        bar_h,
    );
    let drag_right = (minimize.x - CHROME_GAP).max(piece.x + CHROME_PAD);

    ChromeLayout {
        piece,
        bar: DipRect::new(piece.x, bar_y, bar_w, bar_h.min(monitor_h.max(bar_h))),
        drag: DipRect::new(
            piece.x + CHROME_PAD,
            bar_y,
            (drag_right - piece.x - CHROME_PAD).max(0.0),
            bar_h,
        ),
        minimize,
        close,
    }
}

/// Семантика точки для обычного (развернутого) куска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeHit {
    /// Тащить кусок за полосу.
    Drag,
    /// Ужать в квадрат у края монитора.
    Minimize,
    /// Закрыть кусок.
    Close,
    /// Обычное тело куска; полоса невидима вне наведения, но тело остаётся
    /// отдельной зоной для координатора.
    Body,
    /// Точка не принадлежит куску или его обвязке.
    Outside,
}

fn contains(rect: DipRect, point: (f64, f64)) -> bool {
    rect.w > 0.0
        && rect.h > 0.0
        && point.0 >= rect.x
        && point.0 <= rect.x + rect.w
        && point.1 >= rect.y
        && point.1 <= rect.y + rect.h
}

/// Определить действие под точкой. Кнопки проверяются прежде полосы и тела,
/// потому что полоса лежит поверх верхней кромки куска.
pub fn hit_test(layout: &ChromeLayout, point: (f64, f64)) -> ChromeHit {
    if contains(layout.close, point) {
        ChromeHit::Close
    } else if contains(layout.minimize, point) {
        ChromeHit::Minimize
    } else if contains(layout.drag, point) {
        ChromeHit::Drag
    } else if contains(layout.piece, point) {
        ChromeHit::Body
    } else {
        ChromeHit::Outside
    }
}

/// Построить примитивы обвязки. Пока `hovered == false`, возвращается пустой
/// список: решение пользователя о скрытии полосы вне наведения выполняется на
/// границе билдеров, а не маскируется нулевой прозрачностью в рендере.
///
/// Для имени приложения пустая строка означает «без подписи». Кнопка
/// `OrderDown` — единственная существующая пиктограмма со смыслом ужатия, а
/// `Delete` — существующая пиктограмма удаления; новые иконки намеренно не
/// добавляются в общий проверяемый набор.
pub fn chrome_primitives(layout: &ChromeLayout, app_name: &str, hovered: bool) -> Vec<Primitive> {
    if !hovered {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(4);
    glass_panel(
        &mut out,
        Box2D::from_top_left(layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h),
        theme::RADIUS_TIGHT,
        1.0,
    );
    out.push(Primitive::Icon {
        rect: Box2D::from_top_left(
            layout.minimize.x,
            layout.minimize.y,
            layout.minimize.w,
            layout.minimize.h,
        ),
        icon: Icon::OrderDown,
        opacity: theme::TEXT_OPACITY,
    });
    out.push(Primitive::Icon {
        rect: Box2D::from_top_left(
            layout.close.x,
            layout.close.y,
            layout.close.w,
            layout.close.h,
        ),
        icon: Icon::Delete,
        opacity: theme::TEXT_OPACITY,
    });

    if !app_name.is_empty() {
        let text_x = layout.bar.x + CHROME_PAD;
        let text_right = layout.minimize.x - CHROME_GAP;
        let available = text_right - text_x;
        if available > 0.0 {
            // Примитивы текста сами себя не клипуют, поэтому длинное имя
            // обрезается до свободного места, а не налезает на кнопки.
            let text = truncate_to_width(app_name, available);
            let (text_w, text_h) = text_size(&text);
            out.push(Primitive::Text {
                rect: Box2D::from_top_left(
                    text_x,
                    layout.bar.y + (layout.bar.h - text_h) / 2.0,
                    text_w,
                    text_h,
                ),
                text,
                color: theme::TEXT,
                opacity: theme::TEXT_OPACITY,
            });
        }
    }
    out
}

/// Грань монитора, к которой прикреплён свернутый квадрат.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Выбрать ближайшую грань по центру куска. При равенстве используется
/// стабильный порядок Left → Right → Top → Bottom: для угла это не зависит от
/// дробной погрешности, а координатор получает предсказуемое место иконки.
pub fn nearest_edge(piece: DipRect, monitor: DipRect) -> ScreenEdge {
    let cx = piece.x + piece.w / 2.0;
    let cy = piece.y + piece.h / 2.0;
    let distances = [
        (ScreenEdge::Left, (cx - monitor.x).abs()),
        (ScreenEdge::Right, (monitor.x + monitor.w - cx).abs()),
        (ScreenEdge::Top, (cy - monitor.y).abs()),
        (ScreenEdge::Bottom, (monitor.y + monitor.h - cy).abs()),
    ];
    distances
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(ScreenEdge::Left, |(edge, _)| edge)
}

fn overlaps(a: DipRect, b: DipRect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

fn first_free_along_edge(
    edge: ScreenEdge,
    piece: DipRect,
    monitor: DipRect,
    occupied: &[DipRect],
    side: f64,
) -> Option<DipRect> {
    let margin = CHROME_PAD;
    let gap = CHROME_GAP;
    let along_min = match edge {
        ScreenEdge::Left | ScreenEdge::Right => monitor.y + margin,
        ScreenEdge::Top | ScreenEdge::Bottom => monitor.x + margin,
    };
    let along_max = match edge {
        ScreenEdge::Left | ScreenEdge::Right => monitor.y + monitor.h - margin - side,
        ScreenEdge::Top | ScreenEdge::Bottom => monitor.x + monitor.w - margin - side,
    };
    let desired = match edge {
        ScreenEdge::Left | ScreenEdge::Right => piece.y + piece.h / 2.0 - side / 2.0,
        ScreenEdge::Top | ScreenEdge::Bottom => piece.x + piece.w / 2.0 - side / 2.0,
    }
    .clamp(along_min, along_max.max(along_min));
    let step = side + gap;
    let slots = ((along_max - along_min).max(0.0) / step).floor() as usize + 1;

    // Сначала пробуем ближайшие слоты к проекции куска, затем любые
    // оставшиеся. Поэтому иконка сохраняет связь с исходным куском, но не
    // налезает на ранее занятые места у той же грани.
    let mut candidates = Vec::with_capacity(slots);
    for i in 0..slots {
        let delta = i as f64 * step;
        candidates.push((desired + delta).min(along_max.max(along_min)));
        if i != 0 {
            candidates.push((desired - delta).max(along_min));
        }
    }
    for along in candidates {
        let rect = match edge {
            ScreenEdge::Left => DipRect::new(monitor.x + margin, along, side, side),
            ScreenEdge::Right => {
                DipRect::new(monitor.x + monitor.w - margin - side, along, side, side)
            }
            ScreenEdge::Top => DipRect::new(along, monitor.y + margin, side, side),
            ScreenEdge::Bottom => {
                DipRect::new(along, monitor.y + monitor.h - margin - side, side, side)
            }
        };
        if occupied.iter().all(|other| !overlaps(rect, *other)) {
            return Some(rect);
        }
    }

    // Нет свободного слота: лучше честный отказ, чем прямоугольник,
    // налезающий на уже показанную иконку.
    None
}

/// Вычислить квадрат свернутого куска у ближайшей грани.
///
/// `occupied` — уже занятые квадраты (обычно предыдущие свернутые куски).
/// Следующее место сдвигается вдоль той же грани на `BUTTON_SIZE + GAP`,
/// поэтому квадраты не накладываются друг на друга. `None` возвращается для
/// вырожденного монитора или когда у выбранной грани не осталось свободного
/// слота — честный отказ безопаснее наложения иконок.
pub fn collapsed_icon_rect(
    piece: DipRect,
    monitor: DipRect,
    occupied: &[DipRect],
) -> Option<DipRect> {
    if !(monitor.w > 0.0 && monitor.h > 0.0) {
        return None;
    }
    let side = theme::BUTTON_SIZE.min(monitor.w).min(monitor.h);
    // `is_finite` отдельно от сравнения: NaN обязан отсеиваться, а
    // `side <= 0.0` для NaN ложно.
    if !side.is_finite() || side <= 0.0 {
        return None;
    }
    first_free_along_edge(nearest_edge(piece, monitor), piece, monitor, occupied, side)
}

/// Примитив свернутого вида: компактный квадрат с существующей пиктограммой
/// выхода/возврата; сам координатор трактует попадание по квадрату как
/// «развернуть».
pub fn collapsed_primitives(rect: DipRect) -> Vec<Primitive> {
    vec![Primitive::Icon {
        rect: Box2D::from_top_left(rect.x, rect.y, rect.w, rect.h),
        icon: Icon::Exit,
        opacity: theme::TEXT_OPACITY,
    }]
}

/// Единственное действие свернутого квадрата — вернуть кусок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapsedHit {
    Restore,
    Outside,
}

pub fn hit_test_collapsed(rect: DipRect, point: (f64, f64)) -> CollapsedHit {
    if contains(rect, point) {
        CollapsedHit::Restore
    } else {
        CollapsedHit::Outside
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> DipRect {
        DipRect::new(x, y, w, h)
    }

    #[test]
    fn bar_overlays_piece_at_top_edge() {
        let layout = chrome_layout(rect(100.0, 0.0, 400.0, 300.0), 0.0, 1080.0);
        assert_eq!(layout.bar.y, 0.0);
        assert_eq!(layout.bar.h, CHROME_HEIGHT);
        assert!(layout.bar.y >= 0.0);
        assert!(layout.bar.y + layout.bar.h <= 1080.0);
    }

    #[test]
    fn bar_clamps_partially_offscreen_top() {
        let layout = chrome_layout(rect(100.0, -40.0, 400.0, 300.0), 0.0, 1080.0);
        assert_eq!(layout.bar.y, 0.0);
    }

    #[test]
    fn hit_test_covers_every_zone_and_boundaries() {
        let layout = chrome_layout(rect(100.0, 100.0, 400.0, 300.0), 0.0, 1080.0);
        assert_eq!(
            hit_test(&layout, (layout.minimize.x, layout.minimize.y)),
            ChromeHit::Minimize
        );
        assert_eq!(
            hit_test(
                &layout,
                (
                    layout.close.x + layout.close.w,
                    layout.close.y + layout.close.h
                )
            ),
            ChromeHit::Close
        );
        assert_eq!(
            hit_test(
                &layout,
                (layout.drag.x + 1.0, layout.drag.y + layout.drag.h / 2.0)
            ),
            ChromeHit::Drag
        );
        assert_eq!(
            hit_test(
                &layout,
                (
                    layout.piece.x + layout.piece.w / 2.0,
                    layout.piece.y + layout.piece.h / 2.0
                )
            ),
            ChromeHit::Body
        );
        assert_eq!(
            hit_test(&layout, (layout.piece.x - 1.0, layout.piece.y - 1.0)),
            ChromeHit::Outside
        );
    }

    #[test]
    fn primitives_hide_bar_without_hover_and_omit_empty_title() {
        let layout = chrome_layout(rect(100.0, 100.0, 400.0, 300.0), 0.0, 1080.0);
        assert!(chrome_primitives(&layout, "Source", false).is_empty());
        let prims = chrome_primitives(&layout, "", true);
        assert_eq!(prims.len(), 3, "glass + two buttons, no empty title");
        assert!(!prims.iter().any(|p| matches!(p, Primitive::Text { .. })));
    }

    #[test]
    fn primitives_include_source_title_when_it_fits() {
        let layout = chrome_layout(rect(100.0, 100.0, 600.0, 300.0), 0.0, 1080.0);
        let prims = chrome_primitives(&layout, "Chrome", true);
        assert!(
            prims
                .iter()
                .any(|p| matches!(p, Primitive::Text { text, .. } if text == "Chrome"))
        );
    }

    #[test]
    fn long_source_title_is_truncated_before_buttons() {
        let layout = chrome_layout(rect(100.0, 100.0, 400.0, 300.0), 0.0, 1080.0);
        let prims = chrome_primitives(
            &layout,
            "A very very very very very very very long source application title",
            true,
        );
        let Some(Primitive::Text { text, rect, .. }) =
            prims.iter().find(|p| matches!(p, Primitive::Text { .. }))
        else {
            panic!("длинное имя должно получить усечённую подпись");
        };
        assert!(text.ends_with("..."));
        assert!(rect.cx + rect.w / 2.0 <= layout.minimize.x - CHROME_GAP);
    }

    #[test]
    fn nearest_edge_handles_corner_and_middle_deterministically() {
        let monitor = rect(0.0, 0.0, 1000.0, 800.0);
        assert_eq!(
            nearest_edge(rect(20.0, 20.0, 100.0, 100.0), monitor),
            ScreenEdge::Left
        );
        assert_eq!(
            nearest_edge(rect(450.0, 350.0, 100.0, 100.0), monitor),
            ScreenEdge::Top
        );
        assert_eq!(
            nearest_edge(rect(870.0, 300.0, 100.0, 100.0), monitor),
            ScreenEdge::Right
        );
    }

    #[test]
    fn collapsed_icons_at_same_edge_do_not_overlap() {
        let monitor = rect(0.0, 0.0, 1000.0, 800.0);
        let piece = rect(20.0, 100.0, 100.0, 100.0);
        let first = collapsed_icon_rect(piece, monitor, &[]).expect("first slot");
        let second = collapsed_icon_rect(piece, monitor, &[first]).expect("second slot");
        let third = collapsed_icon_rect(piece, monitor, &[first, second]).expect("third slot");
        assert!(!overlaps(first, second));
        assert!(!overlaps(first, third));
        assert!(!overlaps(second, third));
        assert_eq!(first.w, first.h);
        assert_eq!(second.w, second.h);
    }

    #[test]
    fn collapsed_icon_stays_inside_monitor_and_hits_restore() {
        let monitor = rect(100.0, 50.0, 1000.0, 700.0);
        let icon = collapsed_icon_rect(rect(500.0, 100.0, 100.0, 100.0), monitor, &[]).unwrap();
        assert!(icon.x >= monitor.x && icon.y >= monitor.y);
        assert!(icon.x + icon.w <= monitor.x + monitor.w);
        assert!(icon.y + icon.h <= monitor.y + monitor.h);
        assert_eq!(
            hit_test_collapsed(icon, (icon.x + icon.w / 2.0, icon.y + icon.h / 2.0)),
            CollapsedHit::Restore
        );
        assert_eq!(
            hit_test_collapsed(icon, (icon.x - 1.0, icon.y - 1.0)),
            CollapsedHit::Outside
        );
    }

    #[test]
    fn collapsed_primitive_is_one_existing_icon() {
        let prims = collapsed_primitives(rect(10.0, 20.0, theme::BUTTON_SIZE, theme::BUTTON_SIZE));
        assert!(matches!(
            prims.as_slice(),
            [Primitive::Icon {
                icon: Icon::Exit,
                ..
            }]
        ));
    }
}
