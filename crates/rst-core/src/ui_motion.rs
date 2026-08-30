//! Ядро расчёта движения UI: сглаживание переходов, фазы наведения и нажатия,
//! каскадная задержка появления списков и интерполяция цветов.
//!
//! Модуль платформенно-независим: здесь нет зависимостей от ОС, системных
//! таймеров или GPU. Время передаётся вызывающим кодом через параметр `dt_ms`.
//!
//! Референс кривых и длительностей — дизайн-система Dark Liquid Glass (§5
//! `docs/DESIGN_LIQUID_GLASS.md`) и Renarrator.

/// Длительность перехода наведения / ухода курсора (§5): 160 мс.
pub const HOVER_DURATION_MS: f64 = 160.0;
/// Длительность перехода нажатия кнопки (§5): 110 мс.
pub const PRESS_DURATION_MS: f64 = 110.0;
/// Длительность появления панели (§5): 240 мс.
pub const PANEL_DURATION_MS: f64 = 240.0;
/// Длительность появления меню трея (§5): 150 мс.
pub const TRAY_MENU_DURATION_MS: f64 = 150.0;
/// Длительность появления карточки в списке (§5): 300 мс.
pub const CARD_DURATION_MS: f64 = 300.0;

/// Задержка каскадного появления одного шага в миллисекундах (§5): +40 мс.
pub const STAGGER_STEP_MS: f64 = 40.0;
/// Максимальное число шагов каскада (§5): не более 6 шагов.
pub const STAGGER_MAX_STEPS: usize = 6;

/// Вычисляет координату кубической кривой Безье с контрольными точками
/// `p0 = 0.0`, `p1 = c1`, `p2 = c2`, `p3 = 1.0` в точке параметра `s`.
#[inline]
fn sample_curve(s: f64, c1: f64, c2: f64) -> f64 {
    // Коэффициенты полинома Безье третьей степени:
    // B(s) = 3*(1-s)^2*s*c1 + 3*(1-s)*s^2*c2 + s^3
    //      = s * (c + s * (b + s * a))
    let c = 3.0 * c1;
    let b = 3.0 * (c2 - c1) - c;
    let a = 1.0 - c - b;
    ((a * s + b) * s + c) * s
}

/// Вычисляет первую производную кубической кривой Безье по параметру `s`.
#[inline]
fn sample_curve_derivative(s: f64, c1: f64, c2: f64) -> f64 {
    let c = 3.0 * c1;
    let b = 3.0 * (c2 - c1) - c;
    let a = 1.0 - c - b;
    (3.0 * a * s + 2.0 * b) * s + c
}

/// Точное решение кубической кривой Безье `(x1, y1, x2, y2)` для времени `t`.
///
/// Находит параметр `s` такой, что `x(s) == t`, методом Ньютона-Рафсона
/// с автоматическим переходом на деление пополам (bisection) при плохой сходимости,
/// после чего вычисляет и возвращает `y(s)`.
pub fn solve_cubic_bezier(t: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    if t.is_nan() {
        return 0.0;
    }

    // Метод Ньютона-Рафсона: для гладких кривых Безье тайминга обычно
    // сходится за 3–5 итераций с машинной точностью.
    let mut s = t;
    let mut converged = false;
    for _ in 0..8 {
        let x = sample_curve(s, x1, x2);
        let dx = x - t;
        if dx.abs() < 1e-7 {
            converged = true;
            break;
        }
        let dxdt = sample_curve_derivative(s, x1, x2);
        if dxdt.abs() < 1e-6 {
            // Производная близка к нулю — выходим на bisection.
            break;
        }
        let next_s = s - dx / dxdt;
        if !(0.0..=1.0).contains(&next_s) {
            // Шаг Ньютона вылетел за пределы отрезка [0, 1] — выходим на bisection.
            break;
        }
        s = next_s;
    }

    // Запасное деление пополам (bisection) гарантирует монотонную сходимость.
    if !converged {
        let mut low = 0.0;
        let mut high = 1.0;
        s = t;
        for _ in 0..20 {
            let x = sample_curve(s, x1, x2);
            if (x - t).abs() < 1e-7 {
                break;
            }
            if x < t {
                low = s;
            } else {
                high = s;
            }
            s = (low + high) * 0.5;
        }
    }

    sample_curve(s, y1, y2).clamp(0.0, 1.0)
}

/// Кривая ease-out Renarrator: `cubic-bezier(0.22, 1, 0.36, 1)`.
///
/// Точное решение кубики по x методом Ньютона с запасным делением
/// пополам — приблизительная таблица не годится, разница видна на 160 мс.
pub fn ease_out(t: f64) -> f64 {
    solve_cubic_bezier(t, 0.22, 1.0, 0.36, 1.0)
}

/// Фаза перехода 0..1 с целью и длительностью.
///
/// Управляет плавным переходом контролов при наведении курсора, нажатии,
/// показе/скрытии панелей и т. д. Значение `value` меняется линейно во
/// времени, а метод `eased()` возвращает сглаженное значение через кривую `ease_out`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phase {
    /// Текущее линейное положение 0.0..1.0.
    value: f64,
    /// Текущая цель: 1.0 (активно/включено) или 0.0 (покой/выключено).
    target: f64,
    /// Длительность полного перехода от 0 до 1 в миллисекундах.
    duration_ms: f64,
}

impl Phase {
    /// Создаёт фазу с заданной длительностью перехода в миллисекундах.
    ///
    /// Начальное значение и цель равны 0.0 (покой).
    pub fn new(duration_ms: f64) -> Self {
        Self {
            value: 0.0,
            target: 0.0,
            duration_ms: duration_ms.max(0.0),
        }
    }

    /// Устанавливает целевое состояние: `true` -> 1.0, `false` -> 0.0.
    ///
    /// Смена цели на полпути не сбрасывает текущее значение `value`, поэтому
    /// переход разворачивается плавно без скачков.
    pub fn set_target(&mut self, target_on: bool) {
        self.target = if target_on { 1.0 } else { 0.0 };
    }

    /// Продвинуть на `dt_ms` мс. Возвращает `true`, если анимация ещё движется
    /// (вызывающий обязан запланировать следующий кадр, иначе анимация замрёт на середине).
    ///
    /// Возвращает `false`, когда текущее значение достигло цели.
    pub fn advance(&mut self, dt_ms: f64) -> bool {
        if !self.is_animating() {
            return false;
        }
        if dt_ms <= 0.0 {
            return self.is_animating();
        }
        if self.duration_ms <= 0.0 {
            self.value = self.target;
            return false;
        }

        let step = dt_ms / self.duration_ms;
        if self.target > self.value {
            self.value = (self.value + step).min(self.target);
        } else {
            self.value = (self.value - step).max(self.target);
        }

        self.is_animating()
    }

    /// Сглаженное значение 0..1 через `ease_out` — то, чем красят и масштабируют.
    pub fn eased(&self) -> f64 {
        ease_out(self.value)
    }

    /// Проверяет, находится ли фаза в процессе движения к цели.
    pub fn is_animating(&self) -> bool {
        (self.value - self.target).abs() > 1e-9
    }

    /// Мгновенно доехать до цели (сброс при пересборке панели или начальной инициализации).
    pub fn snap(&mut self) {
        self.value = self.target;
    }

    /// Текущее линейное значение фазы [0.0, 1.0].
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Текущая цель фазы (0.0 или 1.0).
    pub fn target(&self) -> f64 {
        self.target
    }

    /// Длительность перехода в миллисекундах.
    pub fn duration_ms(&self) -> f64 {
        self.duration_ms
    }

    /// Обновляет длительность перехода.
    pub fn set_duration_ms(&mut self, duration_ms: f64) {
        self.duration_ms = duration_ms.max(0.0);
    }

    /// Принудительно задаёт линейное значение фазы (с ограничением в [0.0, 1.0]).
    pub fn set_value(&mut self, value: f64) {
        self.value = value.clamp(0.0, 1.0);
    }
}

impl Default for Phase {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// Каскад появления: задержка элемента с номером `index` (§5, +40 мс на шаг,
/// не более 6 шагов).
pub fn stagger_delay_ms(index: usize) -> f64 {
    (index.min(STAGGER_MAX_STEPS) as f64) * STAGGER_STEP_MS
}

/// Линейная интерполяция между числами `a` и `b` по параметру `t`.
#[inline]
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Линейная интерполяция RGB-цвета для перекраски по фазе.
///
/// На границах `t <= 0.0` и `t >= 1.0` возвращает в точности `a` и `b` соответственно.
pub fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    [
        lerp(a[0] as f64, b[0] as f64, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[1] as f64, b[1] as f64, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[2] as f64, b[2] as f64, t).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Линейная интерполяция RGBA-цвета для перекраски по фазе.
pub fn lerp_rgba(a: [u8; 4], b: [u8; 4], t: f64) -> [u8; 4] {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    [
        lerp(a[0] as f64, b[0] as f64, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[1] as f64, b[1] as f64, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[2] as f64, b[2] as f64, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[3] as f64, b[3] as f64, t).round().clamp(0.0, 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_boundary_values() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-0.5), 0.0);
        assert_eq!(ease_out(1.5), 1.0);
        assert_eq!(ease_out(f64::NAN), 0.0);
    }

    #[test]
    fn ease_out_monotonicity() {
        let steps = 1000;
        let mut prev = 0.0;
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let val = ease_out(t);
            assert!(
                val >= prev,
                "Кривая ease_out должна быть монотонной: при t={t} val={val} < prev={prev}"
            );
            assert!(
                (0.0..=1.0).contains(&val),
                "Значение ease_out должно быть в пределах [0, 1]: t={t}, val={val}"
            );
            prev = val;
        }
    }

    #[test]
    fn ease_out_looks_like_ease_out_fast_start_slow_finish() {
        // На середине времени (t = 0.5) кривая ease-out Renarrator cubic-bezier(0.22, 1, 0.36, 1)
        // проходит заметно больше половины пути (y > 0.85).
        let mid = ease_out(0.5);
        assert!(
            mid > 0.85,
            "На t=0.5 пройдено заметно больше половины пути (факт: {mid})"
        );
    }

    #[test]
    fn phase_reaches_target_in_exact_duration_and_no_overshoot() {
        let mut phase = Phase::new(160.0);
        phase.set_target(true);

        assert!(phase.is_animating());
        assert_eq!(phase.value(), 0.0);

        // Половина пути: 80 мс из 160 мс
        let moving = phase.advance(80.0);
        assert!(moving, "На 80 мс анимация должна продолжаться");
        assert!((phase.value() - 0.5).abs() < 1e-9);

        // Вторая половина пути: ещё 80 мс
        let moving = phase.advance(80.0);
        assert!(
            !moving,
            "На 160 мс цель достигнута, advance возвращает false"
        );
        assert_eq!(phase.value(), 1.0);
        assert!(!phase.is_animating());

        // Огромный шаг dt не даёт перелёта выше 1.0
        let mut phase2 = Phase::new(160.0);
        phase2.set_target(true);
        let moving = phase2.advance(10_000.0);
        assert!(!moving);
        assert_eq!(phase2.value(), 1.0);
    }

    #[test]
    fn phase_direction_change_midway_has_no_jump() {
        let mut phase = Phase::new(160.0);
        phase.set_target(true);
        phase.advance(80.0);

        let val_before = phase.value();
        let eased_before = phase.eased();
        assert!((val_before - 0.5).abs() < 1e-9);

        // Разворот цели назад
        phase.set_target(false);
        assert_eq!(
            phase.value(),
            val_before,
            "Смена цели не должна вызывать скачка значения"
        );
        assert_eq!(
            phase.eased(),
            eased_before,
            "Сглаженное значение непрерывно при смене цели"
        );

        // Движение назад
        let moving = phase.advance(40.0);
        assert!(moving);
        assert!((phase.value() - 0.25).abs() < 1e-9);

        let moving = phase.advance(40.0);
        assert!(!moving);
        assert_eq!(phase.value(), 0.0);
    }

    #[test]
    fn phase_snap_immediately_reaches_target() {
        let mut phase = Phase::new(160.0);
        phase.set_target(true);
        assert!(phase.is_animating());

        phase.snap();
        assert_eq!(phase.value(), 1.0);
        assert!(!phase.is_animating());
        assert_eq!(phase.eased(), 1.0);
    }

    #[test]
    fn stagger_delay_saturates_at_step_six() {
        assert_eq!(stagger_delay_ms(0), 0.0);
        assert_eq!(stagger_delay_ms(1), 40.0);
        assert_eq!(stagger_delay_ms(2), 80.0);
        assert_eq!(stagger_delay_ms(3), 120.0);
        assert_eq!(stagger_delay_ms(4), 160.0);
        assert_eq!(stagger_delay_ms(5), 200.0);
        assert_eq!(stagger_delay_ms(6), 240.0);
        // Начиная с 6-го шага задержка насыщается и больше не растёт (§5)
        assert_eq!(stagger_delay_ms(7), 240.0);
        assert_eq!(stagger_delay_ms(100), 240.0);
    }

    #[test]
    fn lerp_rgb_boundaries_and_interpolation() {
        let c1 = [10, 20, 30];
        let c2 = [210, 120, 130];

        assert_eq!(lerp_rgb(c1, c2, 0.0), c1);
        assert_eq!(lerp_rgb(c1, c2, -0.5), c1);
        assert_eq!(lerp_rgb(c1, c2, 1.0), c2);
        assert_eq!(lerp_rgb(c1, c2, 1.5), c2);

        // Середина
        let mid = lerp_rgb(c1, c2, 0.5);
        assert_eq!(mid, [110, 70, 80]);
    }
}
