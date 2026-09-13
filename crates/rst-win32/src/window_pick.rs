//! Режим выбора окна-таргета закрепления (ROADMAP.md M6, первый срез):
//! определение целевого HWND под курсором по снимку кэша окон трекера.
//!
//! Снимок обязан быть кэшем [`crate::window_tracker::WindowTracker`] — тем
//! же, что уже кормит панель выбора окон M4 (docs/M4_WINDOW_PICKER_DESIGN.md
//! §5): окна в нём уже прошли фильтр `is_real_window`
//! ([`crate::window_enum`]), т.е. не-свои (`WS_EX_NOACTIVATE`), tool
//! и cloaked окна отсеяны на источнике. Здесь добавляется только то, что
//! кэш по определению не знает: собственные окна процесса (в режиме
//! редактирования оверлей снимает `WS_EX_NOACTIVATE` и попадает в кэш) и
//! свёрнутые окна (их `rect` от DWM — мусор, рисовать/пинить нечего).
//!
//! Подсветка и рамка под курсором — отдельная задача M6 в render-слое,
//! здесь только hit-test. Единицы: физические пиксели, как `WindowInfo::rect`
//! (M4_PREP_NOTES §2.1).

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::error::Win32Error;
use crate::window_enum::{WindowInfo, WindowRect};

/// Экранная точка в физических пикселях (пространство `WindowInfo::rect`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// Текущая позиция курсора в экранных физических пикселях.
pub fn cursor_position() -> Result<ScreenPoint, Win32Error> {
    let mut pt = POINT::default();
    // SAFETY: pt — валидный out-буфер под POINT.
    unsafe { GetCursorPos(&mut pt) }?;
    Ok(ScreenPoint { x: pt.x, y: pt.y })
}

/// Верхнее по z-order окно из `snapshot`, чей прямоугольник содержит
/// `point`; `None`, если курсор ни над одним окном кэша. Возвращает `hwnd`
/// как числовой ключ (окно может уже не существовать — проверить
/// [`crate::window_pin::WindowPins::is_pinned`]-стилем нельзя, см.
/// `window_enum::WindowInfo::hwnd`).
///
/// Фильтры сверх кэша трекера (см. шапку модуля): свёрнутые окна и
/// собственные окна процесса resticker.
pub fn window_at(snapshot: &[WindowInfo], point: ScreenPoint) -> Option<usize> {
    let own_pid = std::process::id();
    snapshot
        .iter()
        // Своё окно куска — полноценное окно, и закреплять его можно наравне
        // с чужими (запрос пользователя 2026-09-12: «окно вырезаное я мог
        // закреплять как обычное окно»). Остальные свои окна — оверлеи и
        // панели — по-прежнему не кандидаты: навести курсор на оверлей значит
        // навести его на то, что нарисовано поверх настоящего окна.
        .filter(|w| {
            !w.iconic
                && (w.pid != own_pid || w.class == crate::crop_window::WINDOW_CLASS)
                && rect_contains(&w.rect, point)
        })
        // z_order монотонно растёт сверху вниз (window_enum) — минимум и есть
        // верхнее окно; гонок между элементами одного снимка нет по построению.
        .min_by_key(|w| w.z_order)
        .map(|w| w.hwnd)
}

/// Попадание точки в прямоугольник DWM-границ (полуоткрытые границы:
/// нижняя/правая кромка не принадлежит окну).
fn rect_contains(rect: &WindowRect, point: ScreenPoint) -> bool {
    point.x >= rect.x && point.x < rect.x + rect.w && point.y >= rect.y && point.y < rect.y + rect.h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(hwnd: usize, x: i32, y: i32, w: i32, h: i32, z: u32) -> WindowInfo {
        WindowInfo {
            hwnd,
            rect: WindowRect { x, y, w, h },
            z_order: z,
            ..Default::default()
        }
    }

    #[test]
    fn empty_snapshot_yields_none() {
        assert_eq!(window_at(&[], ScreenPoint { x: 0, y: 0 }), None);
    }

    #[test]
    fn negative_coordinates_second_monitor_left() {
        let snapshot = vec![info(1, -200, -100, 100, 80, 0)];
        assert_eq!(
            window_at(&snapshot, ScreenPoint { x: -150, y: -60 }),
            Some(1)
        );
        // Границы полуоткрытые: правая/нижняя кромка не принадлежит окну.
        assert_eq!(window_at(&snapshot, ScreenPoint { x: -100, y: -60 }), None);
        assert_eq!(window_at(&snapshot, ScreenPoint { x: -150, y: -20 }), None);
    }

    #[test]
    fn point_outside_rect_misses() {
        let snapshot = vec![info(1, 10, 20, 100, 50, 0)];
        for p in [
            ScreenPoint { x: 9, y: 30 },
            ScreenPoint { x: 110, y: 30 },
            ScreenPoint { x: 30, y: 70 },
        ] {
            assert_eq!(window_at(&snapshot, p), None, "мимо прямоугольника: {p:?}");
        }
    }

    #[test]
    fn overlapping_windows_topmost_wins() {
        let snapshot = vec![
            info(1, 0, 0, 200, 200, 5),
            info(2, 50, 50, 100, 100, 1),
            info(3, 10, 10, 50, 50, 3),
        ];
        let p = ScreenPoint { x: 60, y: 60 };
        // Точка внутри всех трёх — побеждает минимальный z_order (2).
        assert_eq!(window_at(&snapshot, p), Some(2));
        let p = ScreenPoint { x: 30, y: 30 };
        // Внутри 1 (z=5) и 3 (z=3) — побеждает 3.
        assert_eq!(window_at(&snapshot, p), Some(3));
    }

    #[test]
    fn iconic_window_is_skipped() {
        let mut w = info(1, 0, 0, 200, 200, 0);
        w.iconic = true;
        assert_eq!(window_at(&[w], ScreenPoint { x: 100, y: 100 }), None);
    }

    #[test]
    fn own_process_window_is_skipped() {
        let mut w = info(1, 0, 0, 200, 200, 0);
        w.pid = std::process::id();
        // Чужое окно ниже по z-order — попадание должно уйти ему.
        let snapshot = vec![w, info(2, 0, 0, 200, 200, 1)];
        assert_eq!(
            window_at(&snapshot, ScreenPoint { x: 100, y: 100 }),
            Some(2)
        );
    }

    #[test]
    fn own_crop_window_is_a_candidate_unlike_other_own_windows() {
        // Окно живого куска принадлежит нашему процессу, но это обычное окно
        // приложения, и закреплять его можно наравне с чужими (запрос
        // пользователя 2026-09-12). Остальные свои окна — оверлеи и панели —
        // кандидатами не становятся, иначе наведение на нарисованный поверх
        // окна оверлей выбирало бы оверлей.
        let mut crop = info(1, 0, 0, 200, 200, 0);
        crop.pid = std::process::id();
        crop.class = crate::crop_window::WINDOW_CLASS.to_string();
        let mut overlay = info(2, 0, 0, 200, 200, 1);
        overlay.pid = std::process::id();
        overlay.class = "resticker_overlay".to_string();
        let foreign = info(3, 0, 0, 200, 200, 2);
        let snapshot = vec![crop, overlay, foreign];
        assert_eq!(
            window_at(&snapshot, ScreenPoint { x: 100, y: 100 }),
            Some(1),
            "кусок выигрывает как верхнее окно, оверлей не участвует вовсе"
        );
    }

    #[test]
    fn stale_snapshot_dead_hwnd_still_returned_liveness_is_click_side_guard() {
        // Гонка «первый кадр после пробуждения трекера» (overlay_manager.rs,
        // run()/handle_input): BTN_ADD_WINDOW ставит `picking_window`, трекер
        // просыпается асинхронно, и один-два кадра `window_at` хит-тестит
        // СТАРЫЙ (до-wake) снимок. Окно из него могло уже умереть — но
        // `window_at` обязан вернуть его как есть: IsWindow на каждый
        // hit-test был бы кросс-процессным вызовом на рендер-цикле.
        // Живость проверяет сторона клика (`add_window_sticker`:
        // поиск по текущему снимку + `PinWindowGone` из `WindowPins::pin`).
        let snapshot = vec![info(0xDEAD_BEEF, 0, 0, 200, 200, 0)];
        assert_eq!(
            window_at(&snapshot, ScreenPoint { x: 100, y: 100 }),
            Some(0xDEAD_BEEF),
            "мёртвый hwnd из устаревшего снимка возвращается — liveness за вызывающим"
        );
    }

    #[test]
    fn cursor_position_is_on_some_screen() {
        // Интерактивная сессия предполагается (хоткеи уже тестируются так);
        // GetCursorPos падает только в экзотических контекстах без десктопа.
        let pt = cursor_position().expect("GetCursorPos должен работать");
        assert!(
            pt.x.abs() < 100_000 && pt.y.abs() < 100_000,
            "позиция курсора вне разумных экранных границ: {pt:?}"
        );
    }
}
