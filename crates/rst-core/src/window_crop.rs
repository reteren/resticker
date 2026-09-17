//! Геометрия выделения куска чужого окна.
//!
//! Этот модуль намеренно не знает ни о `HWND`, ни о выборе окна, ни о
//! мониторах, ни о DPI: платформенный слой сам выбирает окно и передаёт
//! только уже снятые физические координаты. Благодаря этому ошибку в
//! математике выделения можно поймать обычными тестами, не поднимая окна и
//! GPU.

use crate::model::{CropRect, Rect};

/// Точка курсора в экранных физических пикселях.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// Причина, по которой протяжку нельзя превратить в кусок окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropError {
    /// Начальная точка жеста не лежала в окне.
    DragOutsideWindow,
    /// Кусок меньше `CropRect::MIN_FRACTION` хотя бы по одной оси.
    DragTooSmall,
    /// У окна нет площади, поэтому доли и координаты не определены.
    DegenerateWindow,
}

/// Готовое выделение: доли для захвата и экранное место, куда его положить.
///
/// `crop` хранится в долях клиентской области источника и потому переживает
/// изменение размера окна. `screen_rect` — тот же кусок в физических
/// пикселях экрана, включая положение за пределами основного монитора. Это
/// ещё не `Placement`: вызывающий слой переводит `screen_rect` в DIP
/// относительно монитора и собирает `Placement` с нужным `monitor_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropSelection {
    pub crop: CropRect,
    pub screen_rect: Rect,
}

/// Перевести протяжку в `CropRect` и в экранное место будущего стикера.
///
/// Жест обязан начаться внутри окна. Конечная точка может уйти за любую
/// границу — это естественное движение мыши, поэтому она зажимается в окно;
/// `CropRect::from_corners` затем делает нормализацию направлений и проверку
/// минимального размера. Так обе величины строятся из одной и той же
/// геометрии и стикер появляется ровно под нарисованной рамкой.
pub fn select(
    start: ScreenPoint,
    end: ScreenPoint,
    window: Rect,
) -> Result<CropSelection, CropError> {
    if window.w == 0 || window.h == 0 {
        return Err(CropError::DegenerateWindow);
    }
    if !contains(&window, start) {
        return Err(CropError::DragOutsideWindow);
    }

    let left = i64::from(window.x);
    let top = i64::from(window.y);
    let right = left + i64::from(window.w);
    let bottom = top + i64::from(window.h);
    let clipped_end_x = i64::from(end.x).clamp(left, right);
    let clipped_end_y = i64::from(end.y).clamp(top, bottom);

    let start_x = (i64::from(start.x) - left) as f64 / f64::from(window.w);
    let start_y = (i64::from(start.y) - top) as f64 / f64::from(window.h);
    let end_x = (clipped_end_x - left) as f64 / f64::from(window.w);
    let end_y = (clipped_end_y - top) as f64 / f64::from(window.h);
    let crop =
        CropRect::from_corners(start_x, start_y, end_x, end_y).ok_or(CropError::DragTooSmall)?;

    let x0 = i64::from(start.x).min(clipped_end_x);
    let y0 = i64::from(start.y).min(clipped_end_y);
    let x1 = i64::from(start.x).max(clipped_end_x);
    let y1 = i64::from(start.y).max(clipped_end_y);
    Ok(CropSelection {
        crop,
        screen_rect: Rect {
            x: x0 as i32,
            y: y0 as i32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        },
    })
}

/// Удобный вариант для вызывающего слоя, которому нужны только доли окна.
pub fn crop_rect(
    start: ScreenPoint,
    end: ScreenPoint,
    window: Rect,
) -> Result<CropRect, CropError> {
    select(start, end, window).map(|selection| selection.crop)
}

fn contains(rect: &Rect, point: ScreenPoint) -> bool {
    let right = i64::from(rect.x) + i64::from(rect.w);
    let bottom = i64::from(rect.y) + i64::from(rect.h);
    i64::from(point.x) >= i64::from(rect.x)
        && i64::from(point.x) < right
        && i64::from(point.y) >= i64::from(rect.y)
        && i64::from(point.y) < bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: i32, y: i32) -> ScreenPoint {
        ScreenPoint { x, y }
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn all_four_drag_directions_produce_the_same_crop() {
        let window = rect(100, 200, 1000, 800);
        let expected = crop_rect(point(300, 400), point(700, 800), window).unwrap();
        for (start, end) in [
            (point(300, 400), point(700, 800)),
            (point(700, 400), point(300, 800)),
            (point(300, 800), point(700, 400)),
            (point(700, 800), point(300, 400)),
        ] {
            assert_eq!(crop_rect(start, end, window).unwrap(), expected);
        }
    }

    #[test]
    fn endpoint_outside_window_is_clamped_and_keeps_start_anchor() {
        let window = rect(100, 200, 1000, 800);
        let selected = select(point(300, 400), point(2_000, -500), window).unwrap();
        assert_eq!(
            selected.crop,
            CropRect {
                x: 0.2,
                y: 0.0,
                w: 0.8,
                h: 0.25
            }
        );
        assert_eq!(selected.screen_rect, rect(300, 200, 800, 200));
    }

    #[test]
    fn drag_start_uses_half_open_window_edges() {
        let window = rect(10, 20, 100, 50);
        assert!(crop_rect(point(10, 20), point(60, 50), window).is_ok());
        assert_eq!(
            crop_rect(point(110, 20), point(60, 50), window),
            Err(CropError::DragOutsideWindow)
        );
        assert_eq!(
            crop_rect(point(10, 70), point(60, 50), window),
            Err(CropError::DragOutsideWindow)
        );
    }

    #[test]
    fn negative_monitor_coordinates_stay_in_screen_space() {
        let window = rect(-1920, 100, 960, 800);
        let selected = select(point(-1800, 200), point(-1200, 700), window).unwrap();
        assert_eq!(
            selected.crop,
            CropRect {
                x: 0.125,
                y: 0.125,
                w: 0.625,
                h: 0.625
            }
        );
        assert_eq!(selected.screen_rect, rect(-1800, 200, 600, 500));
    }

    #[test]
    fn accidental_click_is_rejected_as_too_small() {
        let window = rect(0, 0, 1000, 1000);
        assert_eq!(
            crop_rect(point(500, 500), point(504, 700), window),
            Err(CropError::DragTooSmall)
        );
        assert_eq!(
            crop_rect(point(500, 500), point(500, 500), window),
            Err(CropError::DragTooSmall)
        );
    }

    #[test]
    fn start_outside_window_and_degenerate_window_have_distinct_errors() {
        assert_eq!(
            crop_rect(point(0, 0), point(100, 100), rect(10, 10, 0, 100)),
            Err(CropError::DegenerateWindow)
        );
        assert_eq!(
            crop_rect(point(0, 0), point(100, 100), rect(10, 10, 100, 100)),
            Err(CropError::DragOutsideWindow)
        );
    }
}
