//! Часы анимации стикера: какой кадр показывать и когда переключаться.
//!
//! Чисто runtime-состояние (M5a, §4 `docs/M5A_ANIMATION_DESIGN.md`): не
//! персистится, не зависит от GPU или окна, тестируется как арифметика
//! `Instant`/`Duration`. Часами владеет координатор (`overlay_manager.rs`),
//! который на каждый тик зовёт `advance` и планирует следующий тик на
//! `next_deadline`.
//!
//! Центральное требование — продвижение вперёд **модульной арифметикой** по
//! суммарной длительности цикла, а не наивным while-циклом по каждому
//! прошедшему кадру: после долгого сна процесса (пауза на годы) наивный
//! цикл прокрутил бы миллиарды итераций, здесь — одна-две.

use std::time::{Duration, Instant};

/// Горизонт для не-тикающих часов: не `Instant::MAX` (переполнение арифметики
/// где-то выше по стеку при вычислении таймаута), а заведомо далёкий, но
/// безопасный момент.
const NON_TICKING_HORIZON: Duration = Duration::from_secs(3600);

/// Часы анимации стикера: текущий кадр и момент его начала.
///
/// Инвариант: `frame_started_at` — момент начала показа кадра `frame_index`;
/// `advance` пересчитывает его при каждом изменении кадра, поэтому позиция
/// внутри цикла анимации считается как `elapsed % total_cycle`.
///
/// # Инварианты для `frame_delays`
///
/// Вызывающая сторона (координатор) обязана гарантировать **непустой**
/// список задержек кадров *до создания часов*: пустой список — программная
/// ошибка вызывающего кода, `advance`/`next_deadline` на нём паникуют с
/// понятным сообщением (паника здесь — зона ответственности часов, а не
/// защита на каждом уровне выше).
///
/// Список из одного кадра — легальный патологический случай (статичная
/// картинка, оформленная как анимация): такие часы никогда не тикают.
/// Нулевые задержки отдельных кадров допустимы и не зацикливают часы
/// (кадр с нулевой задержкой проскакивается: показать его нельзя, он
/// занимает 0 времени цикла).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationClock {
    /// Индекс текущего кадра в `frame_delays` координатора.
    pub frame_index: usize,
    /// Момент, когда текущий кадр начал показываться.
    pub frame_started_at: Instant,
}

impl AnimationClock {
    /// Создаёт часы на первом кадре (`frame_index = 0`), начавшемся в `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            frame_index: 0,
            frame_started_at: now,
        }
    }

    /// Продвигает `frame_index` вперёд по мере необходимости, учитывая
    /// per-frame `frame_delays`.
    ///
    /// Возвращает `true`, если `frame_index` реально изменился (координатору
    /// нужен redraw), `false` — если показываемый кадр не поменялся.
    ///
    /// # Долгая пауза
    ///
    /// Если процесс был заблокирован/усыплён надолго (сон системы, тяжёлая
    /// пауза), позиция вычисляется модульной арифметикой по суммарной
    /// длительности цикла (`elapsed % total_cycle`), после чего сканируется
    /// не более одного прохода по списку кадров — а не while-цикл по каждому
    /// прошедшему кадру. Пауза в годы даёт две-три итерации, а не миллиарды.
    ///
    /// # Panics
    ///
    /// Паникует на пустом `frame_delays` (инвариант вызывающей стороны,
    /// см. документацию структуры).
    pub fn advance(&mut self, now: Instant, frame_delays: &[Duration]) -> bool {
        let total = total_cycle(frame_delays);
        // Один кадр либо вырожденный цикл из нулевых задержек: тикать некуда.
        if frame_delays.len() == 1 || total == Duration::ZERO {
            return false;
        }

        let elapsed = now.saturating_duration_since(self.frame_started_at);
        // Позиция внутри цикла: `elapsed % total` не зависит от числа полных
        // циклов, прошедших за паузу (`Rem for Duration` не стабилизирован —
        // считаем по наносекундам; остаток меньше цикла, реальные задержки
        // кадров — миллисекунды, так что усечение в u64 безопасно).
        let rem_nanos = elapsed.as_nanos() % total.as_nanos();
        let mut rem = Duration::from_nanos(
            u64::try_from(rem_nanos)
                .expect("AnimationClock: суммарная длительность цикла превышает ~584 года"),
        );
        let mut k = self.frame_index;
        loop {
            let delay = frame_delays[k];
            if delay > rem {
                // Ещё не время уходить с кадра `k`; `rem` — сколько уже
                // простояли на кадре `k` в ЭТОМ повторении цикла.
                break;
            }
            if !delay.is_zero() {
                rem -= delay;
            }
            k = (k + 1) % frame_delays.len();
        }

        let changed = k != self.frame_index;
        self.frame_index = k;
        // Синхронизируем `frame_started_at` на «сейчас минус сколько уже
        // простояли на кадре `k`» ВСЕГДА, даже если кадр не изменился
        // (`changed == false`): иначе, приземлившись после долгой паузы на
        // ТОТ ЖЕ кадр, что и до паузы, старый якорь остаётся от давнего
        // цикла — `next_deadline` на нём считает от него и уходит в далёкое
        // прошлое (планировщик получил бы уже истёкший дедлайн и слал бы
        // `AnimationTick` в бесконечном цикле без единой смены кадра — баг
        // найден независимым ревью M5a, воспроизведён отдельной симуляцией).
        // Для «изменился» кейса это то же самое значение, что раньше давало
        // `frame_started_at += consumed` — расходится только когда `elapsed`
        // пересекает границу полного цикла (реальный сценарий паузы).
        self.frame_started_at = now - rem;
        changed
    }

    /// Ближайший момент, когда `advance` может снова изменить кадр.
    ///
    /// Для анимации из N > 1 кадров — конец текущего кадра:
    /// `frame_started_at + frame_delays[frame_index]`. Монотонен при вызовах
    /// подряд без `advance` между ними (чистая функция от `&self`).
    ///
    /// Для не-тикающих часов (один кадр либо все задержки нулевые) —
    /// далёкий горизонт `frame_started_at + 3600s`, а не `Instant::MAX`.
    ///
    /// # Panics
    ///
    /// Паникует на пустом `frame_delays` (инвариант вызывающей стороны,
    /// см. документацию структуры).
    pub fn next_deadline(&self, frame_delays: &[Duration]) -> Instant {
        let total = total_cycle(frame_delays);
        if frame_delays.len() == 1 || total == Duration::ZERO {
            return self.frame_started_at + NON_TICKING_HORIZON;
        }
        self.frame_started_at + frame_delays[self.frame_index]
    }
}

/// Суммарная длительность полного цикла анимации.
///
/// Паникует на пустом списке: непустой `frame_delays` — инвариант
/// вызывающей стороны, который она обязана гарантировать до создания часов.
fn total_cycle(frame_delays: &[Duration]) -> Duration {
    assert!(
        !frame_delays.is_empty(),
        "AnimationClock: frame_delays пуст — программная ошибка вызывающей \
         стороны: непустой список задержек кадров должен быть гарантирован \
         до создания часов"
    );
    frame_delays.iter().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// 100 лет в секундах: наивный while-цикл по кадрам 100ms на такой паузе
    /// прокрутил бы ~3e10 итераций (минуты в debug) — тест доказательно
    /// проверяет, что продвижение модульное.
    const YEARS_100_SECS: u64 = 100 * 365 * 24 * 60 * 60;

    fn delays(ms: &[u64]) -> Vec<Duration> {
        ms.iter().map(|&m| Duration::from_nanos(m * MS)).collect()
    }

    #[test]
    fn advances_one_frame_at_frame_boundary() {
        let t0 = Instant::now();
        let frames = delays(&[100, 100]);
        let mut clock = AnimationClock::new(t0);

        assert!(
            !clock.advance(t0 + Duration::from_millis(50), &frames),
            "до границы — без смены"
        );
        assert_eq!(clock.frame_index, 0);

        assert!(
            clock.advance(t0 + Duration::from_millis(100), &frames),
            "ровно на границе"
        );
        assert_eq!(clock.frame_index, 1);
        assert_eq!(clock.frame_started_at, t0 + Duration::from_millis(100));

        assert!(
            !clock.advance(t0 + Duration::from_millis(100), &frames),
            "повторный тик в тот же момент"
        );
        assert_eq!(clock.frame_index, 1);
    }

    #[test]
    fn catch_up_after_long_pause_lands_on_correct_frame() {
        let t0 = Instant::now();
        let frames = delays(&[100, 100, 100]);
        let mut clock = AnimationClock::new(t0);
        assert!(clock.advance(t0 + Duration::from_millis(100), &frames));

        // Пауза в 100 лет + 250ms: за это время прошло целое число циклов
        // по 300ms плюс 150ms от начала кадра 1. Модульная арифметика даёт
        // кадр 2 (время [200, 300) внутри цикла от t0), наивный цикл —
        // 3e10 итераций. Прошедшие целиком циклы не накапливаются в
        // `frame_started_at` — он остаётся в пределах одного цикла от `now`.
        let now = t0 + Duration::from_secs(YEARS_100_SECS) + Duration::from_millis(250);
        assert!(clock.advance(now, &frames));
        assert_eq!(clock.frame_index, 2);
        assert_eq!(
            clock.frame_started_at,
            now - Duration::from_millis(50),
            "кадр 2 занимает [200, 300) внутри ЭТОГО повторения цикла; `now` на \
             50ms глубже в нём — якорь обязан быть недавним (`now`-относительным), \
             а не датированным исходным t0 (иначе next_deadline после паузы \
             уходит в прошлое)"
        );
        assert!(
            clock.next_deadline(&frames) > now,
            "дедлайн после догонялок обязан быть в будущем относительно `now`"
        );
    }

    #[test]
    fn catch_up_after_exact_multiple_of_cycle_stays_on_frame() {
        let t0 = Instant::now();
        let frames = delays(&[100, 100, 100]);
        let mut clock = AnimationClock::new(t0);
        assert!(clock.advance(t0 + Duration::from_millis(100), &frames));
        assert_eq!(clock.frame_index, 1);

        // Ровно целое число циклов (100 лет % 300ms == 0) — снова начало
        // того же кадра 1: `frame_index` не меняется (`advance` возвращает
        // `false`), но `frame_started_at` обязан пересинхронизироваться на
        // `now` (мы буквально только что вошли в кадр 1 в ЭТОМ повторении
        // цикла) — а не остаться на исходном t0+100ms из давнего прошлого.
        let now = t0 + Duration::from_secs(YEARS_100_SECS) + Duration::from_millis(100);
        assert!(!clock.advance(now, &frames));
        assert_eq!(clock.frame_index, 1);
        assert_eq!(clock.frame_started_at, now);
        assert!(
            clock.next_deadline(&frames) > now,
            "дедлайн после догонялок обязан быть в будущем относительно `now`"
        );
    }

    /// Регрессия на баг, найденный независимым ревью M5a: пауза, после
    /// которой часы приземляются на ТОТ ЖЕ кадр, что и до паузы (самый частый
    /// случай догонялок — не только точное кратное циклу), не должна
    /// оставлять `next_deadline` в прошлом. Старая реализация обновляла
    /// `frame_started_at` только в ветке «кадр изменился» — здесь кадр НЕ
    /// меняется, и без фикса `next_deadline` осталась бы датирована исходным
    /// `t0`, планировщик получил бы уже истёкший дедлайн и слал бы
    /// `AnimationTick` бесконечно, ни разу не сменив кадр (busy-loop).
    #[test]
    fn next_deadline_after_landing_on_same_frame_is_never_in_the_past() {
        let t0 = Instant::now();
        let frames = delays(&[100, 100, 100]);
        let mut clock = AnimationClock::new(t0);
        assert!(clock.advance(t0 + Duration::from_millis(100), &frames));
        assert_eq!(clock.frame_index, 1);

        // 100 лет (кратно 300ms по построению теста выше) + 30ms — рем
        // внутри кадра 1 (delay 100ms), не ноль: обычный случай «всё ещё на
        // том же кадре», не только патологический ровно-кратный.
        let now = t0 + Duration::from_secs(YEARS_100_SECS) + Duration::from_millis(130);
        assert!(
            !clock.advance(now, &frames),
            "кадр не меняется — мы всё ещё в его окне [100,200) этого повторения цикла"
        );
        assert_eq!(clock.frame_index, 1);
        assert!(
            clock.next_deadline(&frames) > now,
            "без фикса дедлайн был бы датирован t0 — на ~100 лет в прошлом от `now`"
        );
    }

    #[test]
    fn single_frame_list_never_ticks() {
        let t0 = Instant::now();
        let frames = delays(&[100]);
        let mut clock = AnimationClock::new(t0);

        assert!(!clock.advance(t0 + Duration::from_secs(1), &frames));
        assert!(!clock.advance(t0 + Duration::from_secs(3600), &frames));
        assert_eq!(clock.frame_index, 0);
        assert_eq!(clock.frame_started_at, t0);

        let deadline = clock.next_deadline(&frames);
        assert_eq!(
            deadline,
            t0 + Duration::from_secs(3600),
            "далёкий горизонт, не Instant::MAX"
        );
        assert_eq!(
            deadline,
            clock.next_deadline(&frames),
            "стабилен при повторах"
        );
    }

    #[test]
    fn wraps_around_from_last_frame_to_first() {
        let t0 = Instant::now();
        let frames = delays(&[100, 100, 100]);
        let mut clock = AnimationClock::new(t0);

        assert!(clock.advance(t0 + Duration::from_millis(100), &frames));
        assert!(clock.advance(t0 + Duration::from_millis(200), &frames));
        assert_eq!(clock.frame_index, 2);

        assert!(clock.advance(t0 + Duration::from_millis(300), &frames));
        assert_eq!(
            clock.frame_index, 0,
            "последний кадр перематывается на первый"
        );
        assert_eq!(clock.frame_started_at, t0 + Duration::from_millis(300));
    }

    #[test]
    fn next_deadline_is_monotonic_without_advance() {
        let t0 = Instant::now();
        let frames = delays(&[100, 100, 100]);
        let mut clock = AnimationClock::new(t0);

        let first = clock.next_deadline(&frames);
        assert_eq!(first, t0 + Duration::from_millis(100));
        assert_eq!(
            first,
            clock.next_deadline(&frames),
            "повторные вызовы без advance"
        );

        assert!(clock.advance(t0 + Duration::from_millis(100), &frames));
        let second = clock.next_deadline(&frames);
        assert_eq!(
            second,
            t0 + Duration::from_millis(200),
            "конец нового текущего кадра"
        );
        assert!(second >= first);
        assert_eq!(second, clock.next_deadline(&frames));
    }

    #[test]
    fn zero_delay_frames_are_skipped() {
        let t0 = Instant::now();

        // Нулевой первый кадр: цикл начинается сразу со второго.
        let mut clock = AnimationClock::new(t0);
        assert!(clock.advance(t0 + Duration::from_millis(50), &delays(&[0, 100])));
        assert_eq!(clock.frame_index, 1);
        assert_eq!(clock.frame_started_at, t0);

        // Нулевой кадр в середине: за ним сразу идёт следующий.
        let mut clock = AnimationClock::new(t0);
        let frames = delays(&[100, 0, 100]);
        assert!(clock.advance(t0 + Duration::from_millis(100), &frames));
        assert_eq!(clock.frame_index, 2);
        assert_eq!(clock.frame_started_at, t0 + Duration::from_millis(100));
    }

    #[test]
    #[should_panic(expected = "AnimationClock")]
    fn empty_frame_delays_panics_in_advance() {
        let mut clock = AnimationClock::new(Instant::now());
        let _ = clock.advance(Instant::now(), &[]);
    }

    #[test]
    #[should_panic(expected = "AnimationClock")]
    fn empty_frame_delays_panics_in_next_deadline() {
        let clock = AnimationClock::new(Instant::now());
        let _ = clock.next_deadline(&[]);
    }
}
