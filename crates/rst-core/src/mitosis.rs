//! Геометрия «митоза» окон — чистая арифметика разреза окна пополам
//! (M9_WINDOW_MITOSIS_DESIGN.md, раздел 4.1).
//!
//! Всё, что решается до единого Win32-вызова, живёт здесь, а не в
//! координаторе: крейт платформенно-чистый (CONTRIBUTING.md, «Правило
//! зависимостей»), а решения «где резать» и «можно ли вообще резать»
//! обязаны быть переиспользуемыми и покрытыми юнит-тестами на любой ОС.
//! Win32-слой (`rst-win32/src/window_mitosis.rs`) отвечает только за замеры
//! памяти и запуск второго экземпляра — за то, что без Windows не проверить.

/// Максимальный отход линии разреза от центра окна, доля режущей оси.
///
/// Доля разреза живёт в `[0.5 - MAX_OFFSET_FRAC, 0.5 + MAX_OFFSET_FRAC]` —
/// то есть в `[0.25, 0.75]`. Зажим не косметический (запрос пользователя
/// 2026-09-01): «отрезать совсем чуть-чуть» запрещено и явно, и неявно —
/// окно в четверть ширины перестаёт быть читаемым, а митоз существует
/// именно ради пары одинаково полезных половин.
pub const MAX_OFFSET_FRAC: f64 = 0.25;

/// Минимальный размер половины по режущей оси, физические пиксели.
///
/// Половина меньше — отказ [`MitosisRefusal::TooSmall`]. Граница задаёт и
/// минимально осмысленное окно для митоза: при зажатой доле `0.25` режущая
/// ось должна быть не короче `4 * MIN_HALF_PX` ≈ 960 px, иначе две половины
/// — нечитаемые полоски, которые хуже одного нечитаемого окна.
pub const MIN_HALF_PX: i32 = 240;

/// Ось разреза: по какой стороне окна идёт линия и куда уезжает клон.
///
/// Две оси зеркальны, и удержать обе в одной паре веток дешевле, чем тащить
/// дублирующийся код по координатору: вся разница — какая координата
/// считается долей и как сдвигается вторая половина.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    /// Разрез вертикальной линией: половины слева/справа, оригинал — левый.
    Vertical,
    /// Разрез горизонтальной линией: половины сверху/снизу, оригинал — верхний.
    Horizontal,
}

impl SplitAxis {
    /// Противоположная ось: колесо мыши или `Tab` переключают разрез
    /// вертикаль ↔ горизонталь, и флип — ровно этот переход.
    pub fn flipped(self) -> Self {
        match self {
            Self::Vertical => Self::Horizontal,
            Self::Horizontal => Self::Vertical,
        }
    }
}

/// Прямоугольник в физических пикселях (`WindowInfo::rect`): `x`/`y` —
/// верхний левый угол, `w`/`h` — размер.
///
/// В отличие от `PxRect` из `pinned_window` координаты тут целые: это
/// границы окна из Win32 (`RECT`), а не дробные DIP монитора, и за
/// округление доли до пикселя отвечает [`split_rects`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Доля разреза вдоль оси по позиции курсора, зажатая в
/// `[0.5 - MAX_OFFSET_FRAC, 0.5 + MAX_OFFSET_FRAC]`.
///
/// Курсор вне прямоугольника (в том числе далеко за его краями)
/// зажимается к ближайшей границе диапазона: резать «за краем» окна
/// можно только по самой кромке — пользователь хочет треть/две трети,
/// а не 1% / 99%.
///
/// Вырожденный прямоугольник (`w` или `h` <= 0) возвращает 0.5 без
/// деления: делить на ноль или отрицательный размер нельзя, а 0.5 —
/// единственная доля, которую вырожденный прямоугольник возвращает
/// осмысленно. Дальше её всё равно отбракует [`preflight`] как
/// [`MitosisRefusal::TooSmall`].
pub fn split_fraction(rect: PixRect, axis: SplitAxis, cursor_x: i32, cursor_y: i32) -> f64 {
    if rect.w <= 0 || rect.h <= 0 {
        return 0.5;
    }
    let (pos, origin, size) = match axis {
        SplitAxis::Vertical => (cursor_x, rect.x, rect.w),
        SplitAxis::Horizontal => (cursor_y, rect.y, rect.h),
    };
    let frac = f64::from(pos - origin) / f64::from(size);
    frac.clamp(0.5 - MAX_OFFSET_FRAC, 0.5 + MAX_OFFSET_FRAC)
}

/// Две половины: `.0` — оригинал (левая/верхняя), `.1` — клон
/// (правая/нижняя).
///
/// Доля — `f64`, пиксели — целые, и округление обязано не терять пиксель:
/// сумма размеров половин по режущей оси равна размеру исходного окна
/// ВСЕГДА, потому что вторая половина считается как `total - первая`,
/// а не через собственную долю. Иначе между двумя соседними окнами
/// разъезжался бы зазор в пиксель — дыра или перекрытие.
///
/// Дробная доля зажимается в `[0, 1]`: вызывающий слой питает функцию из
/// [`split_fraction`], но защита должна пережить любой вход (например
/// `NaN`, который иначе дал бы мусорный размер), а вырожденный результат
/// всё равно догонит [`preflight`] и откажет как [`MitosisRefusal::TooSmall`].
pub fn split_rects(rect: PixRect, axis: SplitAxis, fraction: f64) -> (PixRect, PixRect) {
    let frac = fraction.clamp(0.0, 1.0);
    match axis {
        SplitAxis::Vertical => {
            let first_w = (frac * f64::from(rect.w)).round() as i32;
            let second_w = rect.w - first_w;
            (
                PixRect {
                    x: rect.x,
                    y: rect.y,
                    w: first_w,
                    h: rect.h,
                },
                PixRect {
                    x: rect.x + first_w,
                    y: rect.y,
                    w: second_w,
                    h: rect.h,
                },
            )
        }
        SplitAxis::Horizontal => {
            let first_h = (frac * f64::from(rect.h)).round() as i32;
            let second_h = rect.h - first_h;
            (
                PixRect {
                    x: rect.x,
                    y: rect.y,
                    w: rect.w,
                    h: first_h,
                },
                PixRect {
                    x: rect.x,
                    y: rect.y + first_h,
                    w: rect.w,
                    h: second_h,
                },
            )
        }
    }
}

/// Почему митоз отказал — причина достаточно серьёзная, чтобы прервать весь
/// жест, показать баннер и выйти из режима резки.
///
/// Три варианта — про размер и память — проверяются ещё до запуска
/// процесса ([`preflight`]); остальные три приходят из Win32-слоя после
/// старта. Откат при этом всегда одинаковый: оригинал возвращается на
/// место, как бы ни отказало.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MitosisRefusal {
    /// Приватная память процесса превысила потолок настройки
    /// `mitosis_max_memory_mb` (дефолт 4096 МБ): клонировать гиганта —
    /// значит уронить и его, и себя.
    TooHeavy { bytes: u64, limit_bytes: u64 },
    /// Процесс тяжелее свободной физической памяти системы: запуск второго
    /// экземпляра гарантированно уходит в своп или в OOM.
    NotEnoughMemory { bytes: u64, available_bytes: u64 },
    /// Половина по режущей оси меньше [`MIN_HALF_PX`]: резать нечего —
    /// две нечитаемые полоски хуже одного нечитаемого окна.
    TooSmall { half_px: i32, min_px: i32 },
    /// У окна не читается путь к exe (защищённый процесс): второй
    /// экземпляр не запустить, а без него митоз не имеет смысла.
    NoExePath,
    /// Процесс не завёлся — отказ уже ПОСЛЕ ужатия оригинала, поэтому
    /// обязателен откат.
    SpawnFailed,
    /// Второй экземпляр не показал НОВОГО окна за отведённое время:
    /// приложение просто сфокусировало существующее окно. Заранее это не
    /// определить — ни по exe, ни по процессу — только по факту, поэтому
    /// отказ приходит уже ПОСЛЕ ужатия и требует отката.
    ///
    /// Отказ обучающий: два подряд по одному и тому же exe заносят
    /// приложение в `settings.mitosis_single_instance_apps`, и дальше оно
    /// отсекается заранее ([`MitosisRefusal::SingleInstanceApp`]) — окно
    /// больше не ужимается впустую.
    NoSecondWindow,
    /// Приложение числится в списке одно-оконных
    /// (`settings.mitosis_single_instance_apps`): второе окно оно не
    /// откроет никогда, и резать его бессмысленно.
    ///
    /// Отличается от [`MitosisRefusal::NoSecondWindow`] тем, КОГДА
    /// случается: этот отказ приходит ДО единого движения окна, а не через
    /// таймаут ожидания. Ради этого список и существует — репорт
    /// пользователя 2026-09-01: Discord и Spotify «просто режутся», потому
    /// что окно ужимается и стоит ужатым всё время ожидания второго окна,
    /// которого не будет.
    SingleInstanceApp,
}

/// Всё, что решается до единого Win32-вызова: размер половин и вес процесса.
///
/// Три независимые проверки в строгом порядке: сначала геометрия
/// ([`MitosisRefusal::TooSmall`]), затем потолок веса
/// ([`MitosisRefusal::TooHeavy`]), затем дефицит свободной памяти
/// ([`MitosisRefusal::NotEnoughMemory`]). Геометрия первой, потому что она
/// бесплатна и не зависит от замеров, которые могут не удаться; две
/// память-проверки — по возрастанию «вины» приложения: превышение
/// собственного потолка настройки — про само приложение, а конкуренция за
/// память с остальной системой — про окружение.
///
/// `memory_bytes`/`available_bytes` — `None`, когда замерить не удалось
/// (закрытый процесс, сбой `GlobalMemoryStatusEx`): неизвестный вес не
/// повод отказывать — пропускаем проверку и доверяемся Win32-слою дальше.
pub fn preflight(
    rect: PixRect,
    axis: SplitAxis,
    fraction: f64,
    memory_bytes: Option<u64>,
    limit_bytes: u64,
    available_bytes: Option<u64>,
) -> Result<(), MitosisRefusal> {
    let (half0, half1) = split_rects(rect, axis, fraction);
    let half_px = match axis {
        SplitAxis::Vertical => half0.w.min(half1.w),
        SplitAxis::Horizontal => half0.h.min(half1.h),
    };
    if half_px < MIN_HALF_PX {
        return Err(MitosisRefusal::TooSmall {
            half_px,
            min_px: MIN_HALF_PX,
        });
    }
    if let Some(bytes) = memory_bytes {
        if bytes > limit_bytes {
            return Err(MitosisRefusal::TooHeavy { bytes, limit_bytes });
        }
        if let Some(available) = available_bytes {
            if bytes > available {
                return Err(MitosisRefusal::NotEnoughMemory {
                    bytes,
                    available_bytes: available,
                });
            }
        }
    }
    Ok(())
}

/// Насколько сильно новая ось должна выигрывать у текущей, чтобы
/// переключение состоялось (доля размера окна).
pub const AXIS_SWITCH_MARGIN_FRAC: f64 = 0.05;

/// Ось разреза по положению курсора внутри окна, с гистерезисом.
///
/// Правило: пользователь показывает курсором ту сторону окна, КУДА УПРЁТСЯ
/// линия разреза. Курсор у левого или правого края — линия горизонтальная
/// (она и упирается концами в левый и правый края), курсор у верхнего или
/// нижнего — вертикальная. Схема пользователя 2026-09-01: боковые зоны —
/// горизонтальный разрез, верхняя и нижняя — вертикальный.
///
/// Первая версия делала ровно наоборот («ушёл вправо — вертикальный
/// разрез»), и это оказалось неверной моделью: пользователь ставил курсор
/// точно в центр верхнего края и получал горизонтальный разрез, хотя
/// показывал на вертикальную линию. Ошибка была в том, что курсор считали
/// указателем на СМЕЩЕНИЕ линии, а он указывает на саму линию.
///
/// Заметьте, что позиция разреза при этом берётся по ДРУГОЙ координате
/// ([`split_fraction`] по режущей оси): у левого края ось горизонтальная, а
/// высоту линии задаёт `cursor_y` — то есть вдоль края можно вести курсор и
/// двигать линию. Это и есть цельный жест: подвёл к стороне — выбрал ось,
/// поехал вдоль неё — выбрал место.
///
/// Прежнее переключение колесом и `Tab` этой функцией ЗАМЕНЕНО и удалено:
/// два способа задать одно и то же расходились бы между собой — курсор
/// говорил бы одно, колесо другое.
///
/// Смена оси не должна происходить на каждом дрожании мыши: на диагонали
/// окна отклонения по осям почти равны, и один пиксель туда-сюда
/// перекидывал бы предпросмотр каждый кадр. Поэтому ось меняется на
/// противоположную ТОЛЬКО когда новая выигрывает у текущей больше чем на
/// [`AXIS_SWITCH_MARGIN_FRAC`] — это и есть гистерезис: войти в зону новой
/// оси нужно заметно дальше, чем из неё выйти.
///
/// Отклонение нормируется на размер окна по своей оси: сравниваются доли,
/// а не пиксели. Иначе в неквадратном окне (например 2000x400) любое
/// вертикальное движение в пикселях всегда побеждало бы горизонтальное.
/// Геометрически правило делит окно диагоналями на четыре треугольника:
/// левый и правый дают горизонтальный разрез, верхний и нижний —
/// вертикальный.
///
/// Вырожденный прямоугольник (`w` или `h` <= 0) возвращает `current` без
/// деления: делить на ноль нельзя, а резать вырожденное окно всё равно
/// нечего — [`preflight`] откажет как [`MitosisRefusal::TooSmall`].
pub fn axis_for_cursor(
    rect: PixRect,
    cursor_x: i32,
    cursor_y: i32,
    current: SplitAxis,
) -> SplitAxis {
    if rect.w <= 0 || rect.h <= 0 {
        return current;
    }
    let dx = (f64::from(cursor_x - rect.x) / f64::from(rect.w) - 0.5).abs();
    let dy = (f64::from(cursor_y - rect.y) / f64::from(rect.h) - 0.5).abs();
    match current {
        // Ушёл вбок сильнее, чем вверх-вниз, — показывает на левый/правый
        // край, куда упирается ГОРИЗОНТАЛЬНАЯ линия.
        SplitAxis::Vertical if dx > dy + AXIS_SWITCH_MARGIN_FRAC => SplitAxis::Horizontal,
        SplitAxis::Horizontal if dy > dx + AXIS_SWITCH_MARGIN_FRAC => SplitAxis::Vertical,
        _ => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PixRect {
        PixRect { x, y, w, h }
    }

    /// Доля зажимается к 0.25, когда курсор у левого/верхнего края окна.
    #[test]
    fn fraction_clamps_to_min() {
        let r = rect(100, 200, 1000, 800);
        assert_eq!(split_fraction(r, SplitAxis::Vertical, 100, 500), 0.25);
        assert_eq!(split_fraction(r, SplitAxis::Horizontal, 500, 200), 0.25);
    }

    /// Доля зажимается к 0.75, когда курсор у правого/нижнего края.
    #[test]
    fn fraction_clamps_to_max() {
        let r = rect(100, 200, 1000, 800);
        assert_eq!(split_fraction(r, SplitAxis::Vertical, 1100, 500), 0.75);
        assert_eq!(split_fraction(r, SplitAxis::Horizontal, 500, 1000), 0.75);
    }

    /// Курсор вне прямоугольника зажимается к ближайшей границе диапазона,
    /// а не экстраполирует долю за пределы `[0.25, 0.75]`.
    #[test]
    fn fraction_clamps_cursor_outside() {
        let r = rect(100, 200, 1000, 800);
        assert_eq!(split_fraction(r, SplitAxis::Vertical, -500, 500), 0.25);
        assert_eq!(split_fraction(r, SplitAxis::Vertical, 5000, 500), 0.75);
        assert_eq!(split_fraction(r, SplitAxis::Horizontal, 500, -500), 0.25);
        assert_eq!(split_fraction(r, SplitAxis::Horizontal, 500, 5000), 0.75);
    }

    /// Вырожденный прямоугольник даёт 0.5 без деления на ноль.
    #[test]
    fn fraction_on_degenerate_rect_is_midpoint() {
        let zero_w = rect(0, 0, 0, 800);
        assert_eq!(split_fraction(zero_w, SplitAxis::Vertical, 500, 400), 0.5);
        let negative_h = rect(0, 0, 1000, -5);
        assert_eq!(
            split_fraction(negative_h, SplitAxis::Horizontal, 500, 400),
            0.5
        );
    }

    /// Сумма размеров половин по режущей оси равна размеру исходника — на
    /// обеих осях и для нечётных размеров, где округление обязано отдать
    /// лишний пиксель одной из половин, а не потерять его.
    #[test]
    fn split_preserves_size_round_trip() {
        for w in [0, 1, 2, 3, 100, 960, 961, 1921, 2559] {
            let r = rect(10, 20, w, 800);
            let (a, b) = split_rects(r, SplitAxis::Vertical, 0.5);
            assert_eq!(a.w + b.w, w, "сумма ширин для {w}");
            assert_eq!(a.x + a.w, b.x, "половины соседствуют при ширине {w}");
        }
        for h in [0, 1, 2, 3, 100, 960, 961, 1921, 2559] {
            let r = rect(10, 20, 800, h);
            let (a, b) = split_rects(r, SplitAxis::Horizontal, 0.5);
            assert_eq!(a.h + b.h, h, "сумма высот для {h}");
            assert_eq!(a.y + a.h, b.y, "половины соседствуют при высоте {h}");
        }
    }

    /// Доля вне `[0, 1]` не ломает инвариант суммы: вызывающий слой питает
    /// функцию из [`split_fraction`], но защита переживает любой вход.
    #[test]
    fn split_tolerates_out_of_range_fraction() {
        let r = rect(0, 0, 1000, 1000);
        for f in [-1.0, 2.0, f64::NAN] {
            let (a, b) = split_rects(r, SplitAxis::Vertical, f);
            assert_eq!(a.w + b.w, 1000, "сумма ширин при доле {f}");
        }
    }

    /// Митоз вертикаль ↔ горизонталь переключается флипом.
    #[test]
    fn axis_flips() {
        assert_eq!(SplitAxis::Vertical.flipped(), SplitAxis::Horizontal);
        assert_eq!(SplitAxis::Horizontal.flipped(), SplitAxis::Vertical);
    }

    /// Геометрия проверяется РАНЬШЕ памяти: маленькое окно отказывается,
    /// даже если вес в порядке.
    #[test]
    fn preflight_rejects_too_small_before_memory() {
        let r = rect(0, 0, 300, 800);
        let err =
            preflight(r, SplitAxis::Vertical, 0.5, Some(100), 4096, Some(100_000)).unwrap_err();
        assert!(
            matches!(err, MitosisRefusal::TooSmall { .. }),
            "ожидали TooSmall, получили {err:?}"
        );
    }

    /// Каждый префлайтовый отказ — по отдельности, плюс успешный проход.
    #[test]
    fn preflight_each_refusal() {
        let r = rect(0, 0, 1000, 800);

        // TooHeavy: вес выше потолка настройки.
        let err = preflight(
            r,
            SplitAxis::Vertical,
            0.5,
            Some(5000),
            4096,
            Some(1_000_000),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                MitosisRefusal::TooHeavy {
                    bytes: 5000,
                    limit_bytes: 4096
                }
            ),
            "{err:?}"
        );

        // NotEnoughMemory: свободной памяти меньше веса процесса.
        let err =
            preflight(r, SplitAxis::Vertical, 0.5, Some(5000), 10_000, Some(4000)).unwrap_err();
        assert!(
            matches!(
                err,
                MitosisRefusal::NotEnoughMemory {
                    bytes: 5000,
                    available_bytes: 4000
                }
            ),
            "{err:?}"
        );

        // Ok: вес в пределах и потолка, и свободной памяти.
        assert!(
            preflight(
                r,
                SplitAxis::Vertical,
                0.5,
                Some(1000),
                4096,
                Some(1_000_000)
            )
            .is_ok()
        );
    }

    /// Неизвестный вес (`None`) пропускает обе проверки памяти, а не
    /// отказывает: неизвестность не повод мешать пользователю.
    #[test]
    fn preflight_skips_unknown_memory() {
        let r = rect(0, 0, 1000, 800);
        assert!(preflight(r, SplitAxis::Vertical, 0.5, None, 1, Some(1)).is_ok());
        assert!(preflight(r, SplitAxis::Vertical, 0.5, Some(500), 4096, None).is_ok());
        let err = preflight(r, SplitAxis::Vertical, 0.5, Some(5000), 4096, None).unwrap_err();
        assert!(matches!(err, MitosisRefusal::TooHeavy { .. }), "{err:?}");
    }

    /// TooSmall считается по режущей оси и для горизонтали.
    #[test]
    fn preflight_too_small_horizontal() {
        let r = rect(0, 0, 800, 300);
        let err = preflight(r, SplitAxis::Horizontal, 0.5, None, 4096, None).unwrap_err();
        assert!(
            matches!(
                err,
                MitosisRefusal::TooSmall {
                    half_px: 150,
                    min_px: 240
                }
            ),
            "{err:?}"
        );
    }

    /// Вырожденный прямоугольник отбраковывается как TooSmall, а не падает.
    #[test]
    fn preflight_rejects_degenerate_rect() {
        let r = rect(0, 0, 0, 800);
        let err = preflight(r, SplitAxis::Vertical, 0.5, Some(1), 4096, Some(2)).unwrap_err();
        assert!(
            matches!(err, MitosisRefusal::TooSmall { half_px: 0, .. }),
            "{err:?}"
        );
    }

    /// Курсор у левого/правого края — запрос вертикального разреза:
    /// отклонение по x максимально (0.5) и всегда побеждает y.
    #[test]
    fn axis_cursor_left_and_right_edge_is_horizontal() {
        // Схема пользователя 2026-09-01: боковые зоны — горизонтальный
        // разрез. Курсор показывает на край, В КОТОРЫЙ упрётся линия, а не
        // на сторону, куда она сдвинется.
        let r = rect(100, 200, 1000, 800);
        let y = 500;
        assert_eq!(
            axis_for_cursor(r, 100, y, SplitAxis::Horizontal),
            SplitAxis::Horizontal
        );
        assert_eq!(
            axis_for_cursor(r, 100, y, SplitAxis::Vertical),
            SplitAxis::Horizontal
        );
        assert_eq!(
            axis_for_cursor(r, 1100, y, SplitAxis::Horizontal),
            SplitAxis::Horizontal
        );
        assert_eq!(
            axis_for_cursor(r, 1100, y, SplitAxis::Vertical),
            SplitAxis::Horizontal
        );
    }

    /// Курсор у верхнего/нижнего края — запрос ВЕРТИКАЛЬНОГО разреза.
    ///
    /// Ровно тот случай, который пользователь и поймал скриншотом: курсор
    /// точно по центру верхнего края окна обязан давать вертикальную линию.
    #[test]
    fn axis_cursor_top_and_bottom_edge_is_vertical() {
        let r = rect(100, 200, 1000, 800);
        let x = 500;
        assert_eq!(
            axis_for_cursor(r, x, 200, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
        assert_eq!(
            axis_for_cursor(r, x, 200, SplitAxis::Horizontal),
            SplitAxis::Vertical
        );
        assert_eq!(
            axis_for_cursor(r, x, 1000, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
        assert_eq!(
            axis_for_cursor(r, x, 1000, SplitAxis::Horizontal),
            SplitAxis::Vertical
        );
    }

    /// Ровно в центре отклонений нет ни по одной оси — текущая ось не
    /// меняется: пользователь ещё не выбрал сторону, спорить не с чем.
    #[test]
    fn axis_cursor_at_center_keeps_current() {
        let r = rect(100, 200, 1000, 800);
        assert_eq!(
            axis_for_cursor(r, 600, 600, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
        assert_eq!(
            axis_for_cursor(r, 600, 600, SplitAxis::Horizontal),
            SplitAxis::Horizontal
        );
    }

    /// Гистерезис: чуть за диагональю (выигрыш меньше поля 0.05) ось НЕ
    /// переключается, заметно за ней — переключается. Без этого дрожание
    /// мыши в один пиксель мигало бы предпросмотром на каждом кадре.
    #[test]
    fn axis_hysteresis_suppresses_small_lead() {
        let r = rect(0, 0, 1000, 1000);
        // dx = 0.31, dy = 0.30: горизонталь лидирует, но выигрыш 0.01
        // меньше поля — вертикаль остаётся.
        assert_eq!(
            axis_for_cursor(r, 190, 200, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
        // dx = 0.40, dy = 0.30: выигрыш 0.10 больше поля — ось меняется.
        assert_eq!(
            axis_for_cursor(r, 100, 200, SplitAxis::Vertical),
            SplitAxis::Horizontal
        );
    }

    /// Вырожденный прямоугольник возвращает текущую ось без деления.
    #[test]
    fn axis_degenerate_rect_keeps_current() {
        let zero_w = rect(0, 0, 0, 800);
        assert_eq!(
            axis_for_cursor(zero_w, 500, 400, SplitAxis::Horizontal),
            SplitAxis::Horizontal
        );
        let negative_h = rect(0, 0, 1000, -5);
        assert_eq!(
            axis_for_cursor(negative_h, 500, 400, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
    }

    /// Нормировка по размеру окна, а не по пикселям: в окне 2000x400 точка
    /// на 300 px правее центра — это 0.15 ширины, а 150 px ниже центра —
    /// 0.375 высоты. Побеждает отклонение по Y, то есть курсор у нижнего
    /// края — а значит ВЕРТИКАЛЬНЫЙ разрез, хотя в пикселях уехали дальше
    /// по x.
    #[test]
    fn axis_normalises_by_size_not_pixels() {
        let r = rect(0, 0, 2000, 400);
        assert_eq!(
            axis_for_cursor(r, 1300, 350, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
        assert_eq!(
            axis_for_cursor(r, 1300, 350, SplitAxis::Horizontal),
            SplitAxis::Vertical
        );
    }

    /// Курсор вне окна обрабатывается той же формулой: отклонение просто
    /// больше 0.5, и ось выбирается как у самого края.
    #[test]
    fn axis_cursor_outside_behaves_like_edge() {
        let r = rect(100, 200, 1000, 800);
        assert_eq!(
            axis_for_cursor(r, -9999, 500, SplitAxis::Horizontal),
            SplitAxis::Horizontal
        );
        assert_eq!(
            axis_for_cursor(r, 99_999, 500, SplitAxis::Vertical),
            SplitAxis::Horizontal
        );
        assert_eq!(
            axis_for_cursor(r, 500, -9999, SplitAxis::Vertical),
            SplitAxis::Vertical
        );
        assert_eq!(
            axis_for_cursor(r, 500, 99_999, SplitAxis::Horizontal),
            SplitAxis::Vertical
        );
    }
}
