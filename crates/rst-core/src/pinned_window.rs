//! Рантайм-состояние закреплённых окон (SPEC.md, «Закрепление окна»).
//!
//! В отличие от [`crate::model::Sticker`] `PinnedWindow` сознательно не
//! сериализуется: закрепление — чисто рантайм-механизм, в config.json его
//! нет и быть не должно («нельзя сохранить в пресет, всегда нужно
//! выставлять вручную при каждом запуске»). Единственная персистентная
//! часть фичи — `Settings.denylist` — живёт в [`crate::model`].

use crate::model::OverlapRule;

/// Максимальная доля монитора по каждой оси, которую окно может занимать
/// после закрепления (SPEC.md: независимый кламп ширины и высоты).
const MONITOR_MAX_FRACTION: f64 = 0.9;

/// Закреплённое окно другого приложения: рантайм-состояние, не конфиг.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedWindow {
    /// Дескриптор окна (HWND) как обычное число: крейт платформенно-чистый
    /// (CONTRIBUTING.md, «Правило зависимостей»), конвертация в/из `HWND` —
    /// на границе `rst-win32`.
    pub hwnd: isize,
    /// Замок перемещения: пока включён, окно нельзя сдвинуть никаким
    /// способом (координатор делает реактивный snap-back).
    pub lock_move: bool,
    /// Замок взаимодействия: клики/клавиатура в окно не проходят
    /// (`EnableWindow(hwnd, FALSE)` + визуальный индикатор в координаторе).
    pub lock_interact: bool,
    /// Правила соседей по z-order (SPEC.md: «между какими окнами»). Пока
    /// список непуст, окно держится НАД совпавшими соседями вместо
    /// `WS_EX_TOPMOST` — два режима взаимоисключающие ([`is_full_topmost`]).
    pub neighbor_rules: Vec<OverlapRule>,
}

impl PinnedWindow {
    /// Новое закрепление: дефолт — full-topmost, без замков и без соседей.
    pub fn new(hwnd: isize) -> Self {
        Self {
            hwnd,
            lock_move: false,
            lock_interact: false,
            neighbor_rules: Vec::new(),
        }
    }
}

/// Full-topmost-режим: нет правил соседей — окно держится через
/// `WS_EX_TOPMOST`; непустой список — z-order-слот над соседями вместо
/// него. Вопрос «какой из двух режимов» решается одним этим предикатом.
pub fn is_full_topmost(pinned: &PinnedWindow) -> bool {
    pinned.neighbor_rules.is_empty()
}

/// Кламп размера закрепляемого окна до 90% монитора ПО КАЖДОЙ ОСИ
/// независимо (SPEC.md: окно уже fullscreen/больше монитора при
/// закреплении — ужать до лимита, без сохранения пропорций — это не
/// contain-fit-then-shrink из [`crate::sizing::initial_media_size`], у той
/// функции другое правило для другой фичи). Вырожденный вход (нулевой/
/// отрицательный размер) возвращается как есть — тот же защитный паттерн
/// без деления, что в `initial_media_size`.
pub fn clamp_to_monitor_max(w: f64, h: f64, monitor_w: f64, monitor_h: f64) -> (f64, f64) {
    if w <= 0.0 || h <= 0.0 || monitor_w <= 0.0 || monitor_h <= 0.0 {
        return (w, h);
    }
    (
        w.min(monitor_w * MONITOR_MAX_FRACTION),
        h.min(monitor_h * MONITOR_MAX_FRACTION),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pinned_window_defaults_full_topmost_unlocked() {
        let pinned = PinnedWindow::new(42);
        assert_eq!(pinned.hwnd, 42);
        assert!(!pinned.lock_move);
        assert!(!pinned.lock_interact);
        assert!(pinned.neighbor_rules.is_empty());
        assert!(is_full_topmost(&pinned));
    }

    #[test]
    fn neighbor_rules_switch_off_full_topmost() {
        let mut pinned = PinnedWindow::new(1);
        pinned.neighbor_rules.push(OverlapRule::default());
        assert!(!is_full_topmost(&pinned));
        pinned.neighbor_rules.clear();
        assert!(is_full_topmost(&pinned));
    }

    #[test]
    fn clamp_within_90_percent_is_unchanged() {
        assert_eq!(
            clamp_to_monitor_max(800.0, 600.0, 1920.0, 1080.0),
            (800.0, 600.0)
        );
    }

    #[test]
    fn clamp_exceeds_width_only_clamps_width_only() {
        assert_eq!(
            clamp_to_monitor_max(2000.0, 500.0, 1920.0, 1080.0),
            (1728.0, 500.0)
        );
    }

    #[test]
    fn clamp_exceeds_height_only_clamps_height_only() {
        assert_eq!(
            clamp_to_monitor_max(500.0, 1500.0, 1920.0, 1080.0),
            (500.0, 972.0)
        );
    }

    #[test]
    fn clamp_exceeds_both_clamps_each_axis_independently() {
        // 3000x2000 на 1920x1080: 90% = 1728x972, каждая ось своим
        // лимитом, пропорции НЕ сохраняются (в отличие от
        // initial_media_size).
        assert_eq!(
            clamp_to_monitor_max(3000.0, 2000.0, 1920.0, 1080.0),
            (1728.0, 972.0)
        );
    }

    #[test]
    fn clamp_exact_90_percent_boundary_is_unchanged() {
        assert_eq!(
            clamp_to_monitor_max(1728.0, 972.0, 1920.0, 1080.0),
            (1728.0, 972.0)
        );
    }

    #[test]
    fn clamp_degenerate_zero_monitor_does_not_panic() {
        assert_eq!(clamp_to_monitor_max(100.0, 100.0, 0.0, 0.0), (100.0, 100.0));
        assert_eq!(
            clamp_to_monitor_max(100.0, 100.0, -5.0, 1080.0),
            (100.0, 100.0)
        );
        assert_eq!(clamp_to_monitor_max(0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0));
    }
}
