//! Адаптивные раскладки под минимальные размеры окон (вариант B, выбор
//! пользователя 2026-08-26).
//!
//! Пользователь выбирает раскладку из ленты миниатюр, но фиксированная
//! таблица [`crate::group_layout::presets_for`] не учитывает минимумы окон:
//! четыре колонки для набора окон с минимумами ~800 px по ширине не влезают
//! на экран 2560 px НИКОГДА — это арифметика, а не ошибка решателя, и окна
//! налезают друг на друга. Здесь раскладки строятся из минимумов: семь
//! семейств остаются узнаваемыми (колонки, ряды, главное+стопка вправо и
//! влево, главное+ряд сверху и снизу, сетка), но параметр структуры каждого
//! — сколько полос по каждой оси — подбирается под окна, а не берётся из
//! таблицы. «Колонки» для набора, которому тесно в четыре колонки, становятся
//! тремя — идея сохранена, а раскладка выполнима.
//!
//! Выполнимость проверяется [`crate::group_fit::layout_fits`] — тем же кодом,
//! что и при применении: вердикт генератора обязан совпадать с тем, что
//! потом произойдёт на экране, а этого не гарантирует ни одна собственная
//! проверка выполнимости.
//!
//! Крейт платформенно-чистый (CONTRIBUTING.md, «Правило зависимостей»):
//! только геометрия и юнит-тесты, никакого Win32.

use crate::group_fit::{LayoutFits, MinSize, layout_fits};
use crate::group_layout::{self, Preset, UnitRect};
use crate::model::Rect;

/// Допуск сравнения силуэтов, доли: две раскладки считаются одинаковыми,
/// если их слоты различаются меньше чем на это значение по каждой кромке.
/// Доли вычисляются из одного и того же деления, так что даже у разных
/// строителей они совпадают битово или почти; запас нужен на случай разной
/// последовательности арифметических операций.
const SHAPE_EPS: f64 = 1e-9;

/// Раскладки группы из `window_count` окон, подогнанные под их минимумы.
///
/// Семь семейств в фиксированном порядке (колонки, ряды, главное+стопка
/// вправо, главное+стопка влево, главное+ряд сверху, главное+ряд снизу,
/// сетка). У каждого семейства перебирается параметр структуры — от самого
/// близкого к исходной идее (колонка на окно, один ряд стопки) к самому
/// далёкому; берётся первый параметр, при котором раскладка выполнима под
/// `minimums` на `work_area` с зазором `gap_pct` процентов, — так семейство
/// остаётся максимально похожим на себя. Невыполнимые семейства выпадают,
/// дубликаты силуэтов удаляются (первым по порядку семейств), а если после
/// отсева осталось меньше трёх раскладок, в конец добавляются классические
/// из [`crate::group_layout::presets_for`] — лента не должна выглядеть
/// пустой, а невыполнимую раскладку пользователь вправе выбрать сознательно
/// (её пометят отдельно).
///
/// Когда минимумы неизвестны ВСЕ (ни одно приложение не ответило) —
/// возвращается классическая семёрка без изменений: поведение не должно
/// меняться там, где мы ничего не знаем об окнах. Вне диапазона 2..=8 —
/// пустой список, как и у таблицы. Вырожденные входы не паникуют.
///
/// Результат детерминирован: ни одного обхода хэш-таблиц, две одинаковых
/// входных задачи дают одинаковый ответ — иначе лента прыгала бы между
/// кадрами.
pub fn adaptive_presets(
    window_count: usize,
    work_area: Rect,
    gap_pct: u8,
    minimums: &[Option<MinSize>],
) -> Vec<Preset> {
    if !(2..=8).contains(&window_count) {
        return Vec::new();
    }
    if minimums.iter().all(|m| m.is_none()) {
        return group_layout::presets_for(window_count).to_vec();
    }
    let mut candidates: Vec<Preset> = Vec::new();
    for family in families(window_count) {
        let chosen = family.into_iter().find(|preset| {
            layout_fits(
                preset,
                work_area,
                gap_px_for(preset, work_area, gap_pct),
                minimums,
            ) == LayoutFits::Fits
        });
        if let Some(chosen) = chosen {
            candidates.push(chosen);
        }
    }
    let mut result: Vec<Preset> = Vec::new();
    for candidate in candidates {
        if !result.iter().any(|r| same_shape(r, &candidate)) {
            result.push(candidate);
        }
    }
    if result.len() < 3 {
        for classic in group_layout::presets_for(window_count) {
            if result.iter().any(|r| same_shape(r, classic)) {
                continue;
            }
            result.push(classic.clone());
        }
    }
    result
}

/// Семь семейств кандидатов: каждое — перебор параметра структуры от самого
/// близкого к исходной идее к самому далёкому.
fn families(n: usize) -> Vec<Vec<Preset>> {
    vec![
        // Колонки: от колонки на окно к одной колонке на все окна.
        (1..=n).rev().map(|c| group_layout::columns(n, c)).collect(),
        // Ряды: симметрично.
        (1..=n).rev().map(|r| group_layout::rows(n, r)).collect(),
        // Стопка вправо: от одной колонки стопки (исходная идея) к колонке
        // на окно — стопка становится шире, а колонки ниже.
        (1..n).map(|c| group_layout::stack_right(n, c)).collect(),
        // Стопка влево: зеркально.
        (1..n).map(|c| group_layout::stack_left(n, c)).collect(),
        // Главное + ряд сверху: от одного ряда снизу к ряду на окно.
        (1..n).map(|r| group_layout::top_row(n, 0.5, r)).collect(),
        // Главное + ряд снизу: зеркально.
        (1..n)
            .map(|r| group_layout::bottom_row(n, 0.5, r))
            .collect(),
        // Сетка: пары (ряды, колонки), от самой квадратной к вытянутой.
        grid_family(n),
    ]
}

/// Сетка: пары `(ряды, колонки)` с `rows * cols == n`, от самой квадратной
/// (близкие `rows` и `cols`) к самой вытянутой.
///
/// Сетка обязана вместить ровно `n` окон: пустая ячейка в ленте выглядела
/// бы как поломка, а окно без слота — как пропавшее. Для простого `n`
/// остаются только вырожденные пары — их отсеет дедупликация как дубли
/// колонок и рядов.
fn grid_family(n: usize) -> Vec<Preset> {
    let mut pairs: Vec<(usize, usize)> = (1..=n)
        .filter(|d| n % d == 0)
        .map(|rows| (rows, n / rows))
        .collect();
    pairs.sort_by(|a, b| {
        a.0.abs_diff(a.1)
            .cmp(&b.0.abs_diff(b.1))
            .then(a.0.cmp(&b.0))
    });
    pairs
        .iter()
        .map(|&(rows, cols)| group_layout::grid(rows, cols))
        .collect()
}

/// Раскладка «adaptive»: сочинить с нуля такую, чтобы влезло всё (запрос
/// пользователя 2026-08-26: «добавь новый тайлинг пресет который будет
/// называться adaptive и если ты его нажимаешь у тебя окна автоматически
/// ставятся чтобы у тебя все влезло»).
///
/// Рекурсивное разрезание (guillotine): прямоугольник режется прямой на две
/// части, окна делятся между частями двумя ПОДРЯД ИДУЩИМИ кусками `[lo..k)`
/// и `[k..hi)` — порядок слотов (порядок выбора окон пользователем)
/// сохраняется, — каждая часть режется дальше, пока в части не останется
/// одно окно. Такое разбиение по построению не даёт ни щелей, ни
/// перекрытий; при `n <= 8` полный перебор всех разрезаний и обеих осей
/// крошечный, поэтому эвристик нет — перебираются все варианты и из
/// выполнимых выбирается лучший.
///
/// Доля разреза на каждом шаге — не наугад: место делится пропорционально
/// потребностям частей, где потребность по оси разреза — сумма минимумов
/// окон части по этой оси (ширины для вертикального разреза, высоты для
/// горизонтального). Сумма, а не максимум, потому что только сумма даёт
/// ровно оптимальные доли в базовых случаях — «все колонками» (ширина
/// колонки пропорциональна сумме ширин её окон) и «все рядами» (аналогично
/// по высоте) — а для смешанных разбиений точность добирается перебором
/// всех разрезаний и выбором лучшего. Если потребностей нет (минимумы
/// неизвестны) — доля пропорциональна числу окон в частях.
///
/// Из всех выполнимых разрезаний выбирается лучшее по двум признакам:
/// сначала минимальная сумма отклонений пропорций слотов от пропорций
/// экрана — в долях единицы это `|w/h - 1|`, потому что слот с `w/h == 1`
/// имеет те же пропорции, что и экран, независимо от разрешения монитора
/// (окна не вытягиваются в длинные полоски); при равенстве — меньший
/// разброс площадей слотов (окна ближе по размеру). Выполнимость
/// проверяется [`crate::group_fit::layout_fits`] — тем же кодом, что и при
/// применении: вердикт обязан совпадать с тем, что произойдёт на экране.
///
/// Если выполнимого разрезания нет вовсе (для восьми окон на 2560×1392
/// так и есть) — `None`: ломаная раскладка молча не возвращается, карточку
/// пометят красным кольцом тем же механизмом, что и остальные невыполнимые.
/// Когда минимумы неизвестны ВСЕ — раскладка всё равно строится по числу
/// окон, просто без ограничений: пользователь нажал «adaptive» и должен
/// получить раскладку, а не пустоту. Результат детерминирован: ни одного
/// обхода хэш-таблиц, при равных метриках остаётся первый найденный
/// вариант — иначе лента прыгала бы между перерисовками.
pub fn adaptive_layout(
    window_count: usize,
    work_area: Rect,
    gap_pct: u8,
    minimums: &[Option<MinSize>],
) -> Option<Preset> {
    if !(2..=8).contains(&window_count) || work_area.w == 0 || work_area.h == 0 {
        return None;
    }
    let screen_w = f64::from(work_area.w);
    let screen_h = f64::from(work_area.h);
    let reqs = cut_reqs(window_count, minimums);
    let table = build_variants(window_count, minimums, &reqs, screen_w, screen_h);
    let mut best: Option<(f64, f64, Preset)> = None;
    for variant in &table[0][window_count] {
        let preset = Preset {
            slots: variant.slots.clone(),
        };
        // Метрика считается ДО дорогой проверки: layout_fits нужен только
        // кандидатам, которые претендуют на место лучшего. Отсечение
        // выполнимости в рекурсии — необходимое условие, и кандидат может
        // проскочить его невыполнимым — здесь он и отсеивается тем же
        // решателем, что и при применении: вердикт остаётся общим.
        let score = shape_score(&preset);
        let replace = match &best {
            None => true,
            Some((best_aspect, best_spread, _)) => {
                score.0 < *best_aspect - SHAPE_EPS
                    || ((score.0 - *best_aspect).abs() <= SHAPE_EPS && score.1 < *best_spread)
            }
        };
        if !replace {
            continue;
        }
        if layout_fits(
            &preset,
            work_area,
            gap_px_for(&preset, work_area, gap_pct),
            minimums,
        ) != LayoutFits::Fits
        {
            continue;
        }
        best = Some((score.0, score.1, preset));
    }
    best.map(|(_, _, preset)| preset)
}

/// Один вариант guillotine-разбиения: слоты в порядке окон (порядок выбора
/// сохраняется) и требование по осям — наименьший прямоугольник, в котором
/// эта структура может быть уложена решателем.
///
/// Требование выводится из требований половин: при вертикальном разрезе
/// ширины складываются (полосы слева и справа занимают место по очереди),
/// высоты берутся максимумом (обе колонки делят одну высоту); при
/// горизонтальном — наоборот. Это необходимое условие выполнимости
/// (`fit_slots` требует ещё минимум пиксель на полосу), поэтому отсечение
/// по нему никогда не отбрасывает выполнимый вариант — и тем самым не
/// меняет выбор формы.
struct Variant {
    slots: Vec<UnitRect>,
    req_w: u64,
    req_h: u64,
}

/// Минимальные требования диапазонов окон: наименьшая достижимая ширина и
/// высота и сумма площадей минимумов — по всем деревьям разрезания.
///
/// Считаются снизу вверх за O(n³): диапазонов всего порядка n², экспоненты
/// не остаётся вовсе. Используются на входе [`cut_variants`] для отсечения
/// веток, в которых диапазон не поместится НИКАКИМ деревом: это необходимое
/// условие, выполнимое дерево никогда не отсекается.
struct CutReqs {
    min_w: Vec<Vec<u64>>,
    min_h: Vec<Vec<u64>>,
    area: Vec<Vec<u64>>,
}

fn cut_reqs(n: usize, minimums: &[Option<MinSize>]) -> CutReqs {
    let mut min_w = vec![vec![0u64; n + 1]; n];
    let mut min_h = vec![vec![0u64; n + 1]; n];
    let mut area = vec![vec![0u64; n + 1]; n];
    for lo in 0..n {
        let m = minimums.get(lo).copied().flatten();
        min_w[lo][lo + 1] = u64::from(m.map(|m| m.width).unwrap_or(0));
        min_h[lo][lo + 1] = u64::from(m.map(|m| m.height).unwrap_or(0));
        area[lo][lo + 1] = min_w[lo][lo + 1] * min_h[lo][lo + 1];
    }
    for len in 2..=n {
        for lo in 0..=n - len {
            let hi = lo + len;
            let mut best_w = u64::MAX;
            let mut best_h = u64::MAX;
            for k in lo + 1..hi {
                let (lw, rw) = (min_w[lo][k], min_w[k][hi]);
                let (lh, rh) = (min_h[lo][k], min_h[k][hi]);
                // Вертикальный разрез: ширина складывается, высота — максимум;
                // горизонтальный — наоборот. Минимум по всем разрезам.
                best_w = best_w.min(lw + rw).min(lw.max(rw));
                best_h = best_h.min(lh.max(rh)).min(lh + rh);
            }
            min_w[lo][hi] = best_w;
            min_h[lo][hi] = best_h;
            // Площадь минимумов диапазона не зависит от разреза — это сумма
            // площадей его окон, считается одним сложением.
            area[lo][hi] = area[lo][hi - 1] + area[hi - 1][hi];
        }
    }
    CutReqs { min_w, min_h, area }
}

/// Все варианты guillotine-разбиения каждого непрерывного диапазона окон,
/// снизу вверх. `table[i][j]` — варианты диапазона `[i..j)`: слоты в порядке
/// окон (порядок выбора сохраняется) и требование по осям.
///
/// Слоты хранятся в НОРМИРОВАННЫХ долях диапазона (0..1 по обеим осям) и
/// масштабируются в долю родителя при сборке: так вариант диапазона не
/// зависит от прямоугольника, в который он попадёт, и каждый из ~n²
/// диапазонов считается РОВНО ОДИН раз, а не заново в каждом дереве —
/// экспоненты не остаётся вовсе. Порядок вариантов внутри диапазона тот же,
/// что в переборе сверху вниз (k по возрастанию, вертикальный разрез перед
/// горизонтальным, пары слева-направо), поэтому выбор «первого при равных
/// метриках» не меняется.
///
/// Отсечение выполнимости — арифметикой ДО дорогой проверки: диапазон,
/// чьё минимальное требование (по всем деревьям) больше экрана, и вариант,
/// чьё собственное требование больше экрана, не могут войти ни в одно
/// выполнимое полное дерево (в родителях требования только растут — суммы
/// и максимумы). Требование сравнивается с ЭКРАНОМ, а не с долями узла:
/// решатель двигает глобальные границы, и подпрямоугольники не
/// фиксированы. Дорогой [`crate::group_fit::layout_fits`] остаётся только у
/// итогового кандидата, чтобы вердикт совпадал с применением.
fn build_variants(
    n: usize,
    minimums: &[Option<MinSize>],
    reqs: &CutReqs,
    screen_w: f64,
    screen_h: f64,
) -> Vec<Vec<Vec<Variant>>> {
    let mut table: Vec<Vec<Vec<Variant>>> = (0..n)
        .map(|_| (0..=n).map(|_| Vec::new()).collect())
        .collect();
    for len in 1..=n {
        for lo in 0..=n - len {
            let hi = lo + len;
            // Необходимое условие: диапазону в любом подпрямоугольнике экрана
            // нужны хотя бы минимальные ширина и высота, а сумма площадей
            // минимумов не может превысить площадь экрана. Порог 1e-3 px —
            // страховка от округления долей.
            if reqs.min_w[lo][hi] as f64 > screen_w + 1e-3
                || reqs.min_h[lo][hi] as f64 > screen_h + 1e-3
                || reqs.area[lo][hi] as f64 > screen_w * screen_h + 1e-3
            {
                continue;
            }
            if len == 1 {
                let m = minimums.get(lo).copied().flatten();
                table[lo][hi] = vec![Variant {
                    slots: vec![UnitRect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    }],
                    req_w: u64::from(m.map(|m| m.width).unwrap_or(0)),
                    req_h: u64::from(m.map(|m| m.height).unwrap_or(0)),
                }];
                continue;
            }
            let mut out = Vec::new();
            let left_slice = &table[lo][(lo + 1)..hi];
            let right_iter = table[(lo + 1)..hi].iter().map(|row| &row[hi]);
            for (k, (left, right)) in (lo + 1..hi).zip(left_slice.iter().zip(right_iter)) {
                // Вертикальный разрез: ширина делится по сумме минимумов ширин.
                let left_need = need_x(lo, k, minimums);
                let right_need = need_x(k, hi, minimums);
                let frac = share(left_need, right_need, k - lo, hi - lo);
                if frac > 0.0 && frac < 1.0 {
                    for l in left {
                        for r in right {
                            let (req_w, req_h) = (l.req_w + r.req_w, l.req_h.max(r.req_h));
                            if req_w as f64 <= screen_w + 1e-3 && req_h as f64 <= screen_h + 1e-3 {
                                let mut slots = Vec::with_capacity(l.slots.len() + r.slots.len());
                                for s in &l.slots {
                                    slots.push(UnitRect {
                                        x: s.x * frac,
                                        y: s.y,
                                        w: s.w * frac,
                                        h: s.h,
                                    });
                                }
                                for s in &r.slots {
                                    slots.push(UnitRect {
                                        x: frac + s.x * (1.0 - frac),
                                        y: s.y,
                                        w: s.w * (1.0 - frac),
                                        h: s.h,
                                    });
                                }
                                out.push(Variant {
                                    slots,
                                    req_w,
                                    req_h,
                                });
                            }
                        }
                    }
                }
                // Горизонтальный разрез: высота делится по сумме минимумов высот.
                let top_need = need_y(lo, k, minimums);
                let bottom_need = need_y(k, hi, minimums);
                let frac = share(top_need, bottom_need, k - lo, hi - lo);
                if frac > 0.0 && frac < 1.0 {
                    for t in &table[lo][k] {
                        for b in &table[k][hi] {
                            let (req_w, req_h) = (t.req_w.max(b.req_w), t.req_h + b.req_h);
                            if req_w as f64 <= screen_w + 1e-3 && req_h as f64 <= screen_h + 1e-3 {
                                let mut slots = Vec::with_capacity(t.slots.len() + b.slots.len());
                                for s in &t.slots {
                                    slots.push(UnitRect {
                                        x: s.x,
                                        y: s.y * frac,
                                        w: s.w,
                                        h: s.h * frac,
                                    });
                                }
                                for s in &b.slots {
                                    slots.push(UnitRect {
                                        x: s.x,
                                        y: frac + s.y * (1.0 - frac),
                                        w: s.w,
                                        h: s.h * (1.0 - frac),
                                    });
                                }
                                out.push(Variant {
                                    slots,
                                    req_w,
                                    req_h,
                                });
                            }
                        }
                    }
                }
            }
            table[lo][hi] = out;
        }
    }
    table
}

/// Доля первой части в разрезе: потребности частей по оси, а если их нет
/// (минимумы неизвестны) — доля по числу окон.
fn share(left: u64, right: u64, left_count: usize, total_count: usize) -> f64 {
    if left + right > 0 {
        left as f64 / (left + right) as f64
    } else {
        left_count as f64 / total_count as f64
    }
}

/// Сумма минимумов ширин окон части — потребность части по ширине.
fn need_x(lo: usize, hi: usize, minimums: &[Option<MinSize>]) -> u64 {
    (lo..hi)
        .map(|i| {
            u64::from(
                minimums
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|m| m.width)
                    .unwrap_or(0),
            )
        })
        .sum()
}

/// Сумма минимумов высот окон части — потребность части по высоте.
fn need_y(lo: usize, hi: usize, minimums: &[Option<MinSize>]) -> u64 {
    (lo..hi)
        .map(|i| {
            u64::from(
                minimums
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|m| m.height)
                    .unwrap_or(0),
            )
        })
        .sum()
}

/// Оценка формы: (сумма отклонений пропорций от экрана, разброс площадей).
///
/// В долях единицы слот имеет те же пропорции, что и экран, когда `w/h == 1`
/// — метрика не зависит от разрешения монитора. Чем меньше сумма, тем
/// ближе окна к форме экрана; разброс площадей — вторая ступень, чтобы при
/// равных пропорциях окна были похожи по размеру.
fn shape_score(preset: &Preset) -> (f64, f64) {
    let mut aspect = 0.0;
    let mut min_area: f64 = f64::INFINITY;
    let mut max_area: f64 = 0.0;
    for s in &preset.slots {
        aspect += (s.w / s.h - 1.0).abs();
        let area = s.w * s.h;
        min_area = min_area.min(area);
        max_area = max_area.max(area);
    }
    (aspect, max_area - min_area)
}

/// Зазор в пикселях тем же правилом, что и при применении раскладки
/// (`GroupsState::layout_targets`): процент от наименьшей стороны самого
/// тесного слота при нулевом зазоре.
///
/// Правило повторяется ВНУТРИ генератора намеренно, а не принимается
/// готовым в пикселях: зазор зависит от кандидата (наименьший слот у каждого
/// пресета свой), и передавать его снаружи значило бы требовать от
/// вызывающего повторить то же правило по каждому кандидату. Одно место —
/// одинаково для всех семейств, вердикты сравнимы между собой.
fn gap_px_for(preset: &Preset, work_area: Rect, gap_pct: u8) -> i32 {
    let bare = group_layout::apply(preset, work_area, 0);
    let smallest = bare.iter().map(|r| r.w.min(r.h)).min().unwrap_or(0);
    (u32::from(gap_pct) * smallest / 100) as i32
}

/// Две раскладки одинаковы, если их слоты совпадают как множества с
/// точностью до [`SHAPE_EPS`].
///
/// Порядок слотов намеренно не сравнивается: силуэт в ленте от него не
/// зависит, а «колонки» и «сетка» при вырождении дают одни и те же
/// прямоугольники в разном порядке — это одна раскладка, а не две.
fn same_shape(a: &Preset, b: &Preset) -> bool {
    if a.slots.len() != b.slots.len() {
        return false;
    }
    let mut sa = a.slots.clone();
    let mut sb = b.slots.clone();
    let cmp = |x: &UnitRect, y: &UnitRect| {
        x.x.total_cmp(&y.x)
            .then(x.y.total_cmp(&y.y))
            .then(x.w.total_cmp(&y.w))
            .then(x.h.total_cmp(&y.h))
    };
    sa.sort_by(cmp);
    sb.sort_by(cmp);
    sa.iter().zip(&sb).all(|(x, y)| {
        (x.x - y.x).abs() <= SHAPE_EPS
            && (x.y - y.y).abs() <= SHAPE_EPS
            && (x.w - y.w).abs() <= SHAPE_EPS
            && (x.h - y.h).abs() <= SHAPE_EPS
    })
}

#[cfg(test)]
mod bench_guard {
    use super::*;
    use crate::model::Rect;
    use std::time::Instant;

    /// Сочинение раскладки зовётся на каждой пересборке ленты — по клику,
    /// по прокрутке. Полный перебор разрезаний при восьми окнах обязан
    /// оставаться дешевле кадра, иначе лента начнёт заикаться.
    #[test]
    // В отладочной сборке измерять время бессмысленно: она медленнее в разы,
    // и порог пришлось бы задрать так, что он перестал бы что-либо ловить.
    #[cfg_attr(debug_assertions, ignore)]
    fn composing_a_layout_for_eight_windows_is_cheaper_than_a_frame() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1392,
        };
        let minimums: Vec<Option<MinSize>> = (0..8)
            .map(|i| {
                Some(MinSize {
                    width: 300 + i * 40,
                    height: 200 + i * 30,
                })
            })
            .collect();
        let start = Instant::now();
        let _ = adaptive_layout(8, work, 4, &minimums);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 16,
            "сочинение раскладки на восемь окон заняло {elapsed:?}, а бюджет кадра — 16 мс"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group_layout::presets_for;

    /// Рабочая область пользователя: 2560×1440 минус панель задач.
    fn user_area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1392,
        }
    }

    fn min(w: u32, h: u32) -> Option<MinSize> {
        Some(MinSize {
            width: w,
            height: h,
        })
    }

    fn uniform(count: usize, w: u32, h: u32) -> Vec<Option<MinSize>> {
        (0..count).map(|_| min(w, h)).collect()
    }

    /// Площадь пересечения двух слотов в долях.
    fn overlap(a: UnitRect, b: UnitRect) -> f64 {
        let ix = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let iy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        ix.max(0.0) * iy.max(0.0)
    }

    /// Допуск нахлёста в долях: меньше доли пикселя на любом реальном
    /// мониторе. Соседние слоты считают общую границу умножением
    /// (`(k + 1) * w` у следующего против `k * w + w` у предыдущего), и эти
    /// два выражения могут разойтись на ульпиду — в пикселях расхождение
    /// съедает округление в [`crate::group_layout::apply`], на экране
    /// нахлёста нет.
    const OVERLAP_EPS: f64 = 1e-9;

    /// Инварианты результата: ровно `n` слотов, все строго внутри единичного
    /// квадрата, попарно без нахлёста — окну без места взяться неоткуда.
    fn assert_well_formed(presets: &[Preset], n: usize) {
        for preset in presets {
            assert_eq!(preset.slots.len(), n, "слотов не {n}");
            for s in &preset.slots {
                assert!(s.x >= -1e-9 && s.y >= -1e-9, "слот вылез за левый край");
                assert!(
                    s.x + s.w <= 1.0 + 1e-9 && s.y + s.h <= 1.0 + 1e-9,
                    "слот вылез за правый край"
                );
                assert!(s.w > 0.0 && s.h > 0.0, "слот схлопнулся в ноль");
            }
            for (i, a) in preset.slots.iter().enumerate() {
                for b in preset.slots.iter().skip(i + 1) {
                    assert!(
                        overlap(*a, *b) <= OVERLAP_EPS,
                        "нахлёст слотов {i}: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }

    /// Настоящий набор пользователя 2026-08-26 на его же мониторе
    /// (2560×1392, зазор 4%): GitHub Desktop 946×653, Spotify 786×593,
    /// Nemora 686×493, Upscayl 586×493. Четыре колонки невозможны
    /// (946+786+686+586 = 3004 > 2560), три ряда невозможны по высоте
    /// (653+493+493 = 1639 > 1392), поэтому генератор возвращает шесть
    /// раскладок: колонки в три (2+1+1), ряды в два (2+2), стопки в две
    /// колонки, главное+ряд в один ряд — а сетка 2×2 выпадает как точный
    /// дубль рядов 2×2 (один силуэт не должен встречаться дважды).
    #[test]
    fn user_windows_get_six_families_with_fitted_structure() {
        let area = user_area();
        let minimums = [
            min(946, 653), // GitHub Desktop
            min(786, 593), // Spotify
            min(686, 493), // Nemora
            min(586, 493), // Upscayl
        ];
        let result = adaptive_presets(4, area, 4, &minimums);
        let expected = vec![
            group_layout::columns(4, 3),
            group_layout::rows(4, 2),
            group_layout::stack_right(4, 2),
            group_layout::stack_left(4, 2),
            group_layout::top_row(4, 0.5, 1),
            group_layout::bottom_row(4, 0.5, 1),
        ];
        assert_eq!(result.len(), 6, "семь семейств, но сетка 2×2 — дубль рядов");
        for (i, (got, want)) in result.iter().zip(&expected).enumerate() {
            assert!(same_shape(got, want), "семейство {i}: {got:?} != {want:?}");
        }
        for preset in &result {
            assert_eq!(
                layout_fits(preset, area, gap_px_for(preset, area, 4), &minimums),
                LayoutFits::Fits,
                "возвращённая раскладка обязана быть выполнимой"
            );
        }
    }

    /// Детерминизм: два вызова с одинаковыми входами дают побайтово
    /// одинаковый ответ — лента не должна прыгать между кадрами.
    #[test]
    fn same_inputs_produce_same_result() {
        let area = user_area();
        let minimums = uniform(6, 700, 500);
        assert_eq!(
            adaptive_presets(6, area, 5, &minimums),
            adaptive_presets(6, area, 5, &minimums)
        );
    }

    /// Дедупликация: для двух окон «колонки» и «ряды» — один и тот же
    /// силуэт (половины), в ленте он остаётся один; выпавшие семейства не
    /// оставляют ленту пустой — недублирующие классические раскладки
    /// добавляются в конец.
    #[test]
    fn duplicate_shapes_appear_once_and_classics_fill_the_rest() {
        let area = user_area();
        let minimums = uniform(2, 400, 400);
        let result = adaptive_presets(2, area, 4, &minimums);
        assert!(result.len() >= 3, "лента не должна быть пустой");
        for (i, a) in result.iter().enumerate() {
            for b in result.iter().skip(i + 1) {
                assert!(!same_shape(a, b), "дубль силуэта в ленте");
            }
        }
    }

    /// Все минимумы неизвестны — классическая семёрка без изменений:
    /// поведение не должно меняться там, где мы ничего не знаем об окнах.
    #[test]
    fn unknown_minimums_return_classic_seven() {
        let area = user_area();
        for count in 2..=8 {
            let unknown: Vec<Option<MinSize>> = (0..count).map(|_| None).collect();
            assert_eq!(
                adaptive_presets(count, area, 5, &unknown),
                presets_for(count).to_vec(),
                "count={count}"
            );
        }
    }

    /// Каждая возвращённая раскладка — ровно на `window_count` слотов, слоты
    /// в единичном квадрате и попарно без пересечений — при любом числе окон
    /// и нескольких наборах минимумов, включая несимметричные.
    #[test]
    fn every_result_is_well_formed_for_all_counts() {
        let area = user_area();
        for count in 2..=8 {
            let mixed: Vec<Option<MinSize>> = (0..count)
                .map(|i| {
                    if i % 2 == 0 {
                        min(900, 400)
                    } else {
                        min(400, 900)
                    }
                })
                .collect();
            for minimums in [uniform(count, 700, 500), uniform(count, 300, 900), mixed] {
                let result = adaptive_presets(count, area, 4, &minimums);
                assert!(!result.is_empty(), "count={count}: лента пуста");
                assert_well_formed(&result, count);
            }
        }
    }

    /// Минимумы, которые не влезают ни в одну структуру (все окна
    /// 2000×2000), — ни одно семейство не даёт кандидата, и лента получает
    /// классическую семёрку: невыполнимые раскладки пометят отдельно, но
    /// пользователь вправе выбрать их сознательно.
    #[test]
    fn when_nothing_fits_classic_layouts_are_returned() {
        let area = user_area();
        let huge = uniform(4, 2000, 2000);
        let result = adaptive_presets(4, area, 4, &huge);
        assert_eq!(result.len(), presets_for(4).len());
        for (i, p) in result.iter().enumerate() {
            assert!(
                same_shape(p, &presets_for(4)[i]),
                "порядок классики сохранён"
            );
        }
    }

    /// Вырожденные входы не паникуют: ноль, одно и девять окон дают пустую
    /// ленту (как и таблица), нулевая рабочая область — без паники, пустые
    /// минимумы — классическая семёрка.
    #[test]
    fn degenerate_inputs_do_not_panic() {
        let area = user_area();
        assert!(adaptive_presets(0, area, 4, &[]).is_empty());
        assert!(adaptive_presets(1, area, 4, &[min(100, 100)]).is_empty());
        assert!(adaptive_presets(9, area, 4, &uniform(9, 500, 500)).is_empty());
        let zero = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        assert!(!adaptive_presets(4, zero, 4, &uniform(4, 800, 600)).is_empty());
        assert_eq!(adaptive_presets(4, area, 4, &[]), presets_for(4).to_vec());
    }

    /// Проверить результат `adaptive_layout`: обязательные инварианты
    /// (ровно `n` слотов, в единичном квадрате, без нахлёста) и — главное —
    /// выполнимость тем же решателем, что и при применении.
    fn assert_adaptive_fits(
        preset: &Preset,
        area: Rect,
        gap_pct: u8,
        minimums: &[Option<MinSize>],
    ) {
        assert_well_formed(std::slice::from_ref(preset), minimums.len());
        assert_eq!(
            layout_fits(preset, area, gap_px_for(preset, area, gap_pct), minimums),
            LayoutFits::Fits,
            "adaptive обязан сочинить выполнимую раскладку: {preset:?}"
        );
    }

    /// Четыре окна пользователя (GitHub Desktop 946×653, Spotify 786×593,
    /// Nemora 686×493, Upscayl 586×493) на 2560×1392: «adaptive» сочиняет
    /// два ряда — верхний из двух окон (первое шире), нижний из двух
    /// (первое шире), верхний ряд выше нижнего. Все минимумы соблюдены.
    #[test]
    fn adaptive_layout_fits_user_four_windows() {
        let area = user_area();
        let minimums = [min(946, 653), min(786, 593), min(686, 493), min(586, 493)];
        let preset = adaptive_layout(4, area, 4, &minimums).expect("четыре окна влезают");
        assert_adaptive_fits(&preset, area, 4, &minimums);
        assert_eq!(preset.slots.len(), 4);
        // Верхний ряд выше нижнего (653+593 > 493+493), первое окно каждого
        // ряда шире второго (946 > 786, 686 > 586) — пропорции сохраняют
        // порядок окон.
        assert!(preset.slots[0].h > preset.slots[2].h);
        assert_eq!(preset.slots[0].y, preset.slots[1].y);
        assert_eq!(preset.slots[2].y, preset.slots[3].y);
        assert!(preset.slots[0].w > preset.slots[1].w);
        assert!(preset.slots[2].w > preset.slots[3].w);
    }

    /// Пять окон пользователя (те же плюс Steam 1010×600): «adaptive»
    /// сочиняет два ряда — верхний из двух окон, нижний из трёх, и Steam
    /// (последнее по порядку) в нижнем ряду самый широкий.
    #[test]
    fn adaptive_layout_fits_user_five_windows() {
        let area = user_area();
        let minimums = [
            min(946, 653),
            min(786, 593),
            min(686, 493),
            min(586, 493),
            min(1010, 600),
        ];
        let preset = adaptive_layout(5, area, 4, &minimums).expect("пять окон влезают");
        assert_adaptive_fits(&preset, area, 4, &minimums);
        assert_eq!(preset.slots.len(), 5);
        // Два ряда: первые два окна — в верхнем, остальные три — в нижнем.
        assert_eq!(preset.slots[0].y, preset.slots[1].y);
        assert_eq!(preset.slots[2].y, preset.slots[3].y);
        assert_eq!(preset.slots[3].y, preset.slots[4].y);
        assert!(preset.slots[0].y + preset.slots[0].h <= preset.slots[2].y + 1e-9);
        // Steam — последнее окно, и оно шире своих соседей по ряду.
        assert!(preset.slots[4].w > preset.slots[3].w);
    }

    /// Семь окон пользователя (те же пять плюс Discord 816×508 и Opera
    /// 660×310) на 2560×1392: выполнимого разрезания НЕТ — сумма минимумов
    /// ширин 5490 px и высот 3650 px при экране 2560×1392 не оставляют
    /// варианта, и точный перебор всех ~8,5 тысяч разрезаний с точным
    /// решателем это подтверждает: возвращается None, а не ломаная
    /// раскладка.
    #[test]
    fn adaptive_layout_returns_none_when_seven_windows_cannot_fit() {
        let area = user_area();
        let minimums = [
            min(946, 653),
            min(786, 593),
            min(686, 493),
            min(586, 493),
            min(1010, 600),
            min(816, 508),
            min(660, 310),
        ];
        assert_eq!(adaptive_layout(7, area, 4, &minimums), None);
    }

    /// Восемь окон на 2560×1392 не влезают ни в какую гуильотину — None:
    /// окну без места не дают ломаный слот, карточку пометят красным кольцом
    /// тем же механизмом, что и остальные невыполнимые.
    #[test]
    fn adaptive_layout_returns_none_for_eight_windows() {
        let area = user_area();
        let minimums = [
            min(946, 653),
            min(786, 593),
            min(686, 493),
            min(586, 493),
            min(1010, 600),
            min(816, 508),
            min(660, 310),
            min(1018, 400),
        ];
        assert_eq!(adaptive_layout(8, area, 4, &minimums), None);
    }

    /// Все минимумы неизвестны — раскладка всё равно строится по числу
    /// окон (пользователь нажал «adaptive» и должен получить раскладку),
    /// просто без ограничений: результат Some для любого числа окон и
    /// корректной формы.
    #[test]
    fn adaptive_layout_with_unknown_minimums_still_builds() {
        let area = user_area();
        for count in 2..=8 {
            let unknown: Vec<Option<MinSize>> = (0..count).map(|_| None).collect();
            let preset = adaptive_layout(count, area, 4, &unknown)
                .unwrap_or_else(|| panic!("count={count}: adaptive без минимумов обязан строить"));
            assert_well_formed(std::slice::from_ref(&preset), count);
        }
    }

    /// Детерминизм: два вызова с одинаковыми входами дают побайтово
    /// одинаковый ответ — лента не должна прыгать между перерисовками.
    #[test]
    fn adaptive_layout_is_deterministic() {
        let area = user_area();
        let minimums = uniform(6, 700, 500);
        assert_eq!(
            adaptive_layout(6, area, 5, &minimums),
            adaptive_layout(6, area, 5, &minimums)
        );
    }

    /// Инварианты при любом числе окон и разных наборах минимумов: если
    /// результат Some — он корректной формы и выполним, если None — так и
    /// задумано (для восьми окон берётся один набор: перебор там самый
    /// дорогой).
    #[test]
    fn adaptive_layout_invariants_for_many_sets() {
        let area = user_area();
        for count in 2..=7 {
            let mixed: Vec<Option<MinSize>> = (0..count)
                .map(|i| {
                    if i % 2 == 0 {
                        min(900, 400)
                    } else {
                        min(400, 900)
                    }
                })
                .collect();
            for minimums in [uniform(count, 700, 500), uniform(count, 300, 900), mixed] {
                if let Some(preset) = adaptive_layout(count, area, 4, &minimums) {
                    assert_adaptive_fits(&preset, area, 4, &minimums);
                }
            }
        }
        let eight = uniform(8, 500, 400);
        if let Some(preset) = adaptive_layout(8, area, 4, &eight) {
            assert_adaptive_fits(&preset, area, 4, &eight);
        }
    }

    /// Вырожденные входы не паникуют: ноль и одно окно — None (группы из
    /// одного окна не бывает), нулевая рабочая область — без паники.
    #[test]
    fn adaptive_layout_degenerate_inputs_do_not_panic() {
        let area = user_area();
        assert_eq!(adaptive_layout(0, area, 4, &[]), None);
        assert_eq!(adaptive_layout(1, area, 4, &[min(100, 100)]), None);
        assert_eq!(adaptive_layout(9, area, 4, &uniform(9, 500, 500)), None);
        let zero = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        let _ = adaptive_layout(4, zero, 4, &uniform(4, 800, 600));
    }
}
