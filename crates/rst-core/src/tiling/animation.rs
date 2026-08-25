//! Модель анимации перемещения и изменения размеров плиток
//! (M9, docs/TILING_DESIGN.md §Р5, docs/M5A_ANIMATION_DESIGN.md).
//!
//! # Главное архитектурное решение (docs/TILING_DESIGN.md §Р5)
//!
//! Пошагово двигать чужое окно ОС по кадрам (`SetWindowPos` на 60/120 FPS) **нельзя**:
//! тяжёлые приложения (веб-браузеры, Electron, IDE) не успевают перекладывать внутреннюю
//! вёрстку на каждый кадр, что приводит к разрывам, мерцанию и смазыванию изображения.
//!
//! **Решение resticker**:
//! 1. Окно мгновенно телепортируется в свою финальную позицию `to`.
//! 2. Статичный **снимок окна** плавно летит поверх старого места на оверлее
//!    (`rst-render`), скрывая стык и создавая бесшовную анимацию.
//! 3. Модуль [`animation`] реализует чистую математику интерполяции геометрии:
//!    кто куда летит и где находится в момент времени `t` (`now_ms`).
//!
//! # Ответы на архитектурные вопросы
//!
//! ## 1. Выбор длительности по умолчанию (200 мс)
//! Длительность 200 мс — эмпирический оптимум human-computer interaction
//! (Material Design / Apple HIG). Анимации короче 100 мс не считываются глазом
//! как плавное движение, а длиннее 300 мс начинают восприниматься пользователем
//! как искусственная задержка рабочего процесса.
//!
//! ## 2. Почему интерполируется прямоугольник целиком, а не только позиция?
//! В тайлинге разбиение и удаление соседних окон меняют не только экранные
//! координаты $(x, y)$, но и размеры плиток $(w, h)$. Плавный морфинг требует
//! одновременной интерполяции всех четырёх параметров [`Rect`].
//!
//! ## 3. Что происходит при перезапуске анимации на лету (Seamless retargeting)?
//! При повторном вызове [`Animations::start`] для уже летящего окна его текущее
//! положение в момент `now_ms` становится новой начальной точкой `from`. Это
//! предотвращает визуальные скачки и рывки при быстром изменении раскладок
//! (например, спам хоткеев сплита/ресайза).
//!
//! ## 4. Почему модель ничего не знает ни о снимках, ни об оверлее?
//! Крейт `rst-core` строго платформенно-чист (CONTRIBUTING.md). Он оперирует
//! только абстрактной геометрией во времени, позволяя 100% логики покрыть
//! детерминированными юнит-тестами без GPU и Win32.

use super::tree::WindowKey;
use crate::model::Rect;
use std::collections::HashMap;

/// Кривая скорости (кубическая кривая Безье, аналогично CSS `cubic-bezier` и Hyprland).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Easing {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Easing {
    /// Создать произвольную кубическую кривую Безье по двум контрольным точкам.
    pub fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self {
            x1: x1.clamp(0.0, 1.0),
            y1,
            x2: x2.clamp(0.0, 1.0),
            y2,
        }
    }

    /// Линейная интерполяция ($y = t$).
    pub fn linear() -> Self {
        Self::cubic_bezier(0.0, 0.0, 1.0, 1.0)
    }

    /// Плавное замедление к концу движения (`ease-out-cubic`, дефолт для оконных перелётов).
    pub fn ease_out_cubic() -> Self {
        Self::cubic_bezier(0.215, 0.61, 0.355, 1.0)
    }

    /// Значение прогресса анимации в момент $t \in [0, 1]$ (результат в диапазоне $0..=1$).
    pub fn sample(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }

        // Если кривая тривиально линейная
        if (self.x1 == self.y1) && (self.x2 == self.y2) && (self.x1 == 0.0 && self.x2 == 1.0) {
            return t;
        }

        // Численный поиск параметра s такого, что bezier_x(s) = t
        let s = self.solve_curve_x(t);
        self.sample_curve_y(s).clamp(0.0, 1.0)
    }

    fn sample_curve_x(&self, s: f64) -> f64 {
        // 3*(1-s)^2 * s * x1 + 3*(1-s)*s^2 * x2 + s^3
        let one_minus_s = 1.0 - s;
        3.0 * one_minus_s * one_minus_s * s * self.x1
            + 3.0 * one_minus_s * s * s * self.x2
            + s * s * s
    }

    fn sample_curve_y(&self, s: f64) -> f64 {
        let one_minus_s = 1.0 - s;
        3.0 * one_minus_s * one_minus_s * s * self.y1
            + 3.0 * one_minus_s * s * s * self.y2
            + s * s * s
    }

    fn sample_curve_derivative_x(&self, s: f64) -> f64 {
        let one_minus_s = 1.0 - s;
        3.0 * one_minus_s * one_minus_s * self.x1
            + 6.0 * one_minus_s * s * (self.x2 - self.x1)
            + 3.0 * s * s * (1.0 - self.x2)
    }

    fn solve_curve_x(&self, x: f64) -> f64 {
        // Метод Ньютона-Рафсона (до 8 итераций)
        let mut s = x;
        for _ in 0..8 {
            let current_x = self.sample_curve_x(s) - x;
            if current_x.abs() < 1e-7 {
                return s;
            }
            let d = self.sample_curve_derivative_x(s);
            if d.abs() < 1e-6 {
                break;
            }
            s -= current_x / d;
        }

        // Фоллбек на метод половинного деления (бисекция) при отсутствии сходимости
        let mut low = 0.0;
        let mut high = 1.0;
        let mut s = x;

        while low < high {
            let current_x = self.sample_curve_x(s);
            if (current_x - x).abs() < 1e-7 {
                return s;
            }
            if x > current_x {
                low = s;
            } else {
                high = s;
            }
            s = (high + low) * 0.5;
        }

        s
    }
}

/// Состояние одного летящего снимка плитки.
#[derive(Debug, Clone, PartialEq)]
pub struct FlyingTile {
    pub window: WindowKey,
    pub from: Rect,
    pub to: Rect,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub easing: Easing,
}

impl FlyingTile {
    /// Получить текущий интерполированный прямоугольник в момент времени `now_ms`.
    pub fn sample(&self, now_ms: u64) -> Rect {
        if self.duration_ms == 0 || now_ms <= self.start_ms {
            return self.from;
        }
        if now_ms >= self.start_ms.saturating_add(self.duration_ms) {
            return self.to;
        }

        let elapsed = now_ms.saturating_sub(self.start_ms) as f64;
        let progress = (elapsed / self.duration_ms as f64).clamp(0.0, 1.0);
        let alpha = self.easing.sample(progress);
        interpolate_rect(self.from, self.to, alpha)
    }

    /// Завершена ли анимация к моменту времени `now_ms`.
    pub fn is_finished(&self, now_ms: u64) -> bool {
        now_ms >= self.start_ms.saturating_add(self.duration_ms)
    }
}

/// Менеджер активных анимаций перемещения плиток.
#[derive(Debug, Clone, PartialEq)]
pub struct Animations {
    duration_ms: u64,
    easing: Easing,
    tiles: HashMap<WindowKey, FlyingTile>,
}

impl Animations {
    /// Создать менеджер анимаций с заданной длительностью и кривой скорости.
    pub fn new(duration_ms: u64, easing: Easing) -> Self {
        Self {
            duration_ms,
            easing,
            tiles: HashMap::new(),
        }
    }

    /// Начать перелёт окна из `from` в `to`.
    ///
    /// При повторном вызове для уже летящего окна анимация плавно продолжается
    /// из **текущей позиции** в момент `now_ms`, исключая скачки.
    pub fn start(&mut self, window: WindowKey, from: Rect, to: Rect, now_ms: u64) {
        let actual_from = if let Some(existing) = self.tiles.get(&window) {
            existing.sample(now_ms)
        } else {
            from
        };

        if actual_from == to {
            self.tiles.remove(&window);
            return;
        }

        self.tiles.insert(
            window,
            FlyingTile {
                window,
                from: actual_from,
                to,
                start_ms: now_ms,
                duration_ms: self.duration_ms,
                easing: self.easing,
            },
        );
    }

    /// Получить список текущих координат всех летящих снимков в момент `now_ms`.
    pub fn sample(&self, now_ms: u64) -> Vec<(WindowKey, Rect)> {
        let mut res = Vec::with_capacity(self.tiles.len());
        for tile in self.tiles.values() {
            res.push((tile.window, tile.sample(now_ms)));
        }
        res
    }

    /// Удалить завершившиеся анимации. Возвращает `true`, если ещё есть активные перелёты.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        self.tiles.retain(|_, tile| !tile.is_finished(now_ms));
        !self.tiles.is_empty()
    }

    /// Принудительно отменить анимацию для указанного окна.
    pub fn cancel(&mut self, window: WindowKey) {
        self.tiles.remove(&window);
    }

    /// Проверить, пуст ли список активных анимаций.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Ближайший дедлайн следующего кадра (мс).
    ///
    /// Координатор засыпает ровно до этого момента (ADR-006: запрет на холостые пробуждения).
    /// Возвращает `None`, если анимаций нет.
    pub fn next_deadline_ms(&self, now_ms: u64) -> Option<u64> {
        if self.tiles.is_empty() {
            return None;
        }

        // Интервал кадра для 120 FPS: ~8 мс
        const FRAME_INTERVAL_MS: u64 = 8;
        let next_frame = now_ms.saturating_add(FRAME_INTERVAL_MS);

        let mut min_end = u64::MAX;
        for tile in self.tiles.values() {
            let end = tile.start_ms.saturating_add(tile.duration_ms);
            if end > now_ms && end < min_end {
                min_end = end;
            }
        }

        if min_end == u64::MAX {
            Some(now_ms)
        } else {
            Some(next_frame.min(min_end))
        }
    }
}

/// Линейная интерполяция между двумя прямоугольниками по коэффициенту `alpha` $\in [0, 1]$.
fn interpolate_rect(from: Rect, to: Rect, alpha: f64) -> Rect {
    let alpha = alpha.clamp(0.0, 1.0);
    let x = from.x as f64 + (to.x as f64 - from.x as f64) * alpha;
    let y = from.y as f64 + (to.y as f64 - from.y as f64) * alpha;
    let w = from.w as f64 + (to.w as f64 - from.w as f64) * alpha;
    let h = from.h as f64 + (to.h as f64 - from.h as f64) * alpha;

    Rect {
        x: x.round() as i32,
        y: y.round() as i32,
        w: (w.round().max(0.0)) as u32,
        h: (h.round().max(0.0)) as u32,
    }
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

    #[test]
    fn easing_linear_matches_t_exactly() {
        let easing = Easing::linear();
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            let val = easing.sample(t);
            assert!((val - t).abs() < 1e-6, "t = {t}, val = {val}");
        }
    }

    #[test]
    fn easing_ease_out_cubic_starts_at_zero_ends_at_one_and_is_monotonic() {
        let easing = Easing::ease_out_cubic();
        assert_eq!(easing.sample(0.0), 0.0);
        assert_eq!(easing.sample(1.0), 1.0);

        let mut prev = 0.0;
        for i in 1..=100 {
            let t = i as f64 / 100.0;
            let val = easing.sample(t);
            assert!(
                val >= prev,
                "кривая должна быть монотонной: prev={prev}, val={val}"
            );
            prev = val;
        }
    }

    #[test]
    fn cubic_bezier_custom_curve_boundary_values() {
        let custom = Easing::cubic_bezier(0.4, 0.0, 0.2, 1.0);
        assert_eq!(custom.sample(0.0), 0.0);
        assert_eq!(custom.sample(1.0), 1.0);
    }

    #[test]
    fn easing_clamps_out_of_bounds_t() {
        let easing = Easing::ease_out_cubic();
        assert_eq!(easing.sample(-10.0), 0.0);
        assert_eq!(easing.sample(10.0), 1.0);
    }

    #[test]
    fn flying_tile_at_start_equals_from_at_end_equals_to() {
        let tile = FlyingTile {
            window: w(1),
            from: r(100, 200, 300, 400),
            to: r(500, 600, 700, 800),
            start_ms: 1000,
            duration_ms: 200,
            easing: Easing::linear(),
        };

        assert_eq!(tile.sample(1000), r(100, 200, 300, 400));
        assert_eq!(tile.sample(1200), r(500, 600, 700, 800));
        assert_eq!(tile.sample(1300), r(500, 600, 700, 800));
    }

    #[test]
    fn flying_tile_interpolates_position_and_dimensions() {
        let tile = FlyingTile {
            window: w(1),
            from: r(0, 0, 100, 200),
            to: r(100, 200, 300, 400),
            start_ms: 0,
            duration_ms: 100,
            easing: Easing::linear(),
        };

        let mid = tile.sample(50);
        assert_eq!(mid, r(50, 100, 200, 300));
    }

    #[test]
    fn zero_duration_animation_does_not_divide_by_zero_and_completes_immediately() {
        let tile = FlyingTile {
            window: w(1),
            from: r(0, 0, 100, 100),
            to: r(200, 200, 200, 200),
            start_ms: 1000,
            duration_ms: 0,
            easing: Easing::linear(),
        };

        assert_eq!(tile.sample(1000), r(0, 0, 100, 100));
        assert!(tile.is_finished(1000));
    }

    #[test]
    fn start_with_same_from_and_to_clears_animation() {
        let mut anims = Animations::new(200, Easing::linear());
        anims.start(w(1), r(10, 10, 100, 100), r(10, 10, 100, 100), 1000);
        assert!(anims.is_empty());
    }

    #[test]
    fn restart_mid_flight_continues_from_current_position_without_jumping() {
        let mut anims = Animations::new(200, Easing::linear());
        // Запуск из 0 в 200 на 200мс (в момент 100мс будет на x=100)
        anims.start(w(1), r(0, 0, 100, 100), r(200, 0, 100, 100), 1000);

        // На 1100 мс окно находится на x=100
        let samples = anims.sample(1100);
        assert_eq!(samples[0].1, r(100, 0, 100, 100));

        // В этот момент меняем цель на x=400
        anims.start(w(1), r(0, 0, 100, 100), r(400, 0, 100, 100), 1100);

        // Проверяем, что в момент 1100 новая анимация начинается ровно из x=100, а не прыгнула в 0!
        let new_samples = anims.sample(1100);
        assert_eq!(new_samples[0].1, r(100, 0, 100, 100));
    }

    #[test]
    fn tick_removes_finished_animations_and_returns_remaining_status() {
        let mut anims = Animations::new(200, Easing::linear());
        anims.start(w(1), r(0, 0, 100, 100), r(200, 0, 100, 100), 1000);

        assert!(anims.tick(1100), "на 1100 мс анимация ещё идёт");
        assert!(!anims.is_empty());

        assert!(!anims.tick(1200), "на 1200 мс анимация завершилась");
        assert!(anims.is_empty());
    }

    #[test]
    fn cancel_removes_specific_window_animation() {
        let mut anims = Animations::new(200, Easing::linear());
        anims.start(w(1), r(0, 0, 100, 100), r(200, 0, 100, 100), 1000);
        anims.start(w(2), r(0, 0, 100, 100), r(300, 0, 100, 100), 1000);

        anims.cancel(w(1));
        assert_eq!(anims.sample(1050).len(), 1);
        assert_eq!(anims.sample(1050)[0].0, w(2));
    }

    #[test]
    fn next_deadline_ms_is_none_when_empty() {
        let anims = Animations::new(200, Easing::linear());
        assert_eq!(anims.next_deadline_ms(1000), None);
    }

    #[test]
    fn next_deadline_ms_schedules_next_frame_up_to_animation_end() {
        let mut anims = Animations::new(200, Easing::linear());
        anims.start(w(1), r(0, 0, 100, 100), r(200, 0, 100, 100), 1000);

        // На 1000 мс следующий дедлайн — 1008 мс (1000 + 8)
        assert_eq!(anims.next_deadline_ms(1000), Some(1008));

        // На 1195 мс следующий шаг 1203 ограничивается дедлайном окончания 1200 мс
        assert_eq!(anims.next_deadline_ms(1195), Some(1200));
    }

    #[test]
    fn multiple_windows_animate_simultaneously_and_independently() {
        let mut anims = Animations::new(200, Easing::linear());
        anims.start(w(1), r(0, 0, 100, 100), r(200, 0, 100, 100), 1000);
        anims.start(w(2), r(0, 0, 100, 100), r(0, 200, 100, 100), 1100);

        // На 1150 мс: w(1) на 75% пути (150/200), w(2) на 25% пути (50/200)
        let samples = anims.sample(1150);
        let s1 = samples.iter().find(|(k, _)| *k == w(1)).unwrap().1;
        let s2 = samples.iter().find(|(k, _)| *k == w(2)).unwrap().1;

        assert_eq!(s1, r(150, 0, 100, 100));
        assert_eq!(s2, r(0, 50, 100, 100));
    }

    #[test]
    fn sample_returns_all_active_flying_rectangles() {
        let mut anims = Animations::new(200, Easing::linear());
        anims.start(w(1), r(0, 0, 50, 50), r(100, 100, 50, 50), 0);
        let samples = anims.sample(0);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0], (w(1), r(0, 0, 50, 50)));
    }

    #[test]
    fn is_empty_reports_correct_state() {
        let mut anims = Animations::new(200, Easing::linear());
        assert!(anims.is_empty());
        anims.start(w(1), r(0, 0, 50, 50), r(100, 100, 50, 50), 0);
        assert!(!anims.is_empty());
        anims.cancel(w(1));
        assert!(anims.is_empty());
    }

    #[test]
    fn easing_ease_out_cubic_is_faster_at_start_than_linear() {
        let ease = Easing::ease_out_cubic();
        let lin = Easing::linear();
        // В первой половине времени ease_out проходит больше расстояния, чем linear
        assert!(ease.sample(0.3) > lin.sample(0.3));
    }

    #[test]
    fn start_time_in_future_holds_from_rect() {
        let tile = FlyingTile {
            window: w(1),
            from: r(10, 10, 10, 10),
            to: r(50, 50, 50, 50),
            start_ms: 2000,
            duration_ms: 200,
            easing: Easing::linear(),
        };

        // Время до начала анимации
        assert_eq!(tile.sample(1000), r(10, 10, 10, 10));
    }
}
