//! Проба F3: независимая проверка раскладки «adaptive» (одиночная карточка
//! в ленте, сочиняющая раскладку под конкретные минимумы) на ЖИВЫХ окнах
//! пользователя.
//!
//! Делает, не веря никому на слово:
//! 1. Собирает минимумы всех открытых окон пользователя через
//!    `rst_win32::window_enum::min_window_size` (только чтение) и настоящую
//!    рабочую область главного монитора (SPI_GETWORKAREA).
//! 2. Считает, НАСКОЛЬКО «adaptive» сильнее семейств: для групп 2..8 окон —
//!    сколько раскладок вернули семейства (`adaptive_presets`) и сколько из
//!    них выполнимо, и нашла ли решение сама «adaptive». Разница — её
//!    оправдание.
//! 3. Для каждого решения «adaptive» независимо проверяет инварианты: число
//!    слотов, отсутствие пересечений (в долях и в пикселях после apply),
//!    лежание внутри области, выполнимость минимумов ПОСЛЕ fit_slots.
//! 4. Описывает форму словами для 4, 5 и 7 окон.
//! 5. Проверяет детерминизм: два вызова на одном входе дают побитово
//!    одинаковый ответ.
//!
//! Разведочный инструмент, в workspace не входит (spike исключён).

use std::io::Write;

use rst_core::group_fit::{self, FitOutcome, MinSize};
use rst_core::group_layout::{self, Preset, UnitRect};
use rst_core::group_shape::adaptive_presets;
use rst_core::model::Rect;
use rst_win32::window_enum::{enumerate, min_window_size};

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SPI_GETWORKAREA, SM_CXSCREEN, SM_CYSCREEN, SystemParametersInfoW,
};

/// Зазор пользователя (конфиг: gap_pct=4 во всех группах).
const GAP_PCT: u8 = 4;

fn gap_px_for(preset: &Preset, work_area: Rect) -> i32 {
    let bare = group_layout::apply(preset, work_area, 0);
    let smallest = bare.iter().map(|r| r.w.min(r.h)).min().unwrap_or(0);
    (u32::from(GAP_PCT) * smallest / 100) as i32
}

fn to_optional(mins: &[MinSize]) -> Vec<Option<MinSize>> {
    mins.iter().map(|m| Some(*m)).collect()
}

/// Независимая проверка инвариантов готовых слотов: число слотов, попарные
/// пересечения, лежание внутри рабочей области, каждый слот не меньше своего
/// минимума. Пусто — всё чисто.
fn check_slots(label: &str, slots: &[Rect], work_area: Rect, mins: &[MinSize]) -> Vec<String> {
    let mut violations = Vec::new();
    if slots.len() != mins.len() {
        violations.push(format!(
            "{label}: слотов {} при минимумах {}",
            slots.len(),
            mins.len()
        ));
    }
    let inside = |s: &Rect| {
        s.x >= work_area.x
            && s.y >= work_area.y
            && i64::from(s.x) + i64::from(s.w) <= i64::from(work_area.x) + i64::from(work_area.w)
            && i64::from(s.y) + i64::from(s.h) <= i64::from(work_area.y) + i64::from(work_area.h)
    };
    for (i, s) in slots.iter().enumerate() {
        if !inside(s) {
            violations.push(format!("{label}: слот {i} {s:?} вылез за рабочую область"));
        }
        if let Some(m) = mins.get(i) {
            if s.w < m.width || s.h < m.height {
                violations.push(format!(
                    "{label}: слот {i} {s:?} меньше минимума {}x{}",
                    m.width, m.height
                ));
            }
        }
    }
    for i in 0..slots.len() {
        for j in (i + 1)..slots.len() {
            let (a, b) = (&slots[i], &slots[j]);
            let overlap = i64::from(a.x) < i64::from(b.x) + i64::from(b.w)
                && i64::from(b.x) < i64::from(a.x) + i64::from(a.w)
                && i64::from(a.y) < i64::from(b.y) + i64::from(b.h)
                && i64::from(b.y) < i64::from(a.y) + i64::from(a.h);
            if overlap {
                violations.push(format!(
                    "{label}: слоты {i} и {j} пересекаются: {a:?} vs {b:?}"
                ));
            }
        }
    }
    violations
}

/// Две раскладки — один силуэт? Слоты как множества прямоугольников с
/// допуском по каждой кромке (порядок слотов не важен).
fn same_shape(a: &Preset, b: &Preset) -> bool {
    const EPS: f64 = 1e-9;
    if a.slots.len() != b.slots.len() {
        return false;
    }
    let mut sa = a.slots.clone();
    let mut sb = b.slots.clone();
    let cmp = |x: &UnitRect, y: &UnitRect| {
        x.x
            .total_cmp(&y.x)
            .then(x.y.total_cmp(&y.y))
            .then(x.w.total_cmp(&y.w))
            .then(x.h.total_cmp(&y.h))
    };
    sa.sort_by(cmp);
    sb.sort_by(cmp);
    sa.iter().zip(&sb).all(|(x, y)| {
        (x.x - y.x).abs() <= EPS
            && (x.y - y.y).abs() <= EPS
            && (x.w - y.w).abs() <= EPS
            && (x.h - y.h).abs() <= EPS
    })
}

/// Словесное описание силуэта (для ленты пользователя).
fn describe_preset(p: &Preset) -> String {
    let n = p.slots.len();
    let main = &p.slots[0];
    let rest = &p.slots[1..];
    let main_full_height = main.y < 1e-9 && (main.h - 1.0).abs() < 1e-9;
    let main_full_width = main.x < 1e-9 && (main.w - 1.0).abs() < 1e-9;
    if main_full_height && (main.w - 0.5).abs() < 1e-9 {
        let side = if main.x < 1e-9 { "слева" } else { "справа" };
        let other = if main.x < 1e-9 { "справа" } else { "слева" };
        let mut cols_x: Vec<f64> = rest.iter().map(|s| s.x).collect();
        cols_x.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cols_x.dedup();
        let counts: Vec<String> = cols_x
            .iter()
            .map(|&cx| rest.iter().filter(|s| (s.x - cx).abs() < 1e-9).count().to_string())
            .collect();
        return format!(
            "главное {side} (половина ширины, вся высота), стопка {other} — колонки по {} окна",
            counts.join(" и ")
        );
    }
    if main_full_width && (main.h - 0.5).abs() < 1e-9 {
        let side = if main.y < 1e-9 { "сверху" } else { "снизу" };
        let other = if main.y < 1e-9 { "снизу" } else { "сверху" };
        let mut cols_x: Vec<f64> = rest.iter().map(|s| s.x).collect();
        cols_x.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cols_x.dedup();
        let r = cols_x.len();
        return format!("главное {side} (половина высоты, вся ширина), {other} — {r} окна в ряд");
    }
    let mut xs: Vec<f64> = p.slots.iter().flat_map(|s| [s.x, s.x + s.w]).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs.dedup();
    let mut ys: Vec<f64> = p.slots.iter().flat_map(|s| [s.y, s.y + s.h]).collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ys.dedup();
    let cols = xs.len() - 1;
    let rows = ys.len() - 1;
    let all_full_height = p.slots.iter().all(|s| s.y < 1e-9 && (s.h - 1.0).abs() < 1e-9);
    let all_full_width = p.slots.iter().all(|s| s.x < 1e-9 && (s.w - 1.0).abs() < 1e-9);
    if all_full_height {
        return format!("{cols} колонок равной ширины на всю высоту");
    }
    if all_full_width {
        return format!("{rows} рядов равной высоты на всю ширину");
    }
    // Рекурсивное разрезание «adaptive»: опишем структуру по-своему —
    // поддерево слева/справа/сверху/снизу. Общий случай: перечислим слоты
    // по позициям.
    format!("{cols} колонок × {rows} рядов, {n} слотов (не-регулярная)")
}

/// Есть ли в rst-core функция «adaptive» (одиночная раскладка): появление
/// файла-маркера в пробивной папке (кладётся соседней задачей) или по имени
/// в исходнике.
fn adaptive_function_present() -> bool {
    let src = std::fs::read_to_string("../../crates/rst-core/src/group_shape.rs").unwrap_or_default();
    src.contains("adaptive_layout") || src.contains("compose_layout")
}

/// Полная независимая проверка одного пресета: форма в долях, пиксели
/// после apply+fit_slots, каждый слот ≥ минимума.
fn verify_preset(p: &Preset, work_area: Rect, mins: &[MinSize]) -> (String, Vec<String>) {
    let name = describe_preset(p);
    let mut violations = Vec::new();
    for (i, s) in p.slots.iter().enumerate() {
        if s.x < -1e-9 || s.y < -1e-9 || s.x + s.w > 1.0 + 1e-9 || s.y + s.h > 1.0 + 1e-9 {
            violations.push(format!("слот {i} в долях вылез за единичный квадрат: {s:?}"));
        }
        if s.w <= 0.0 || s.h <= 0.0 {
            violations.push(format!("слот {i} схлопнулся в ноль: {s:?}"));
        }
    }
    for i in 0..p.slots.len() {
        for j in (i + 1)..p.slots.len() {
            let (a, b) = (&p.slots[i], &p.slots[j]);
            let ix = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
            let iy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
            if ix > 1e-9 && iy > 1e-9 {
                violations.push(format!("слоты {i} и {j} в долях налезают друг на друга"));
            }
        }
    }
    let gap = gap_px_for(p, work_area);
    let slots = group_layout::apply(p, work_area, gap);
    match group_fit::fit_slots(&slots, work_area, &to_optional(mins)) {
        FitOutcome::Placed(fitted) => {
            violations.extend(check_slots(&name, &fitted, work_area, mins));
        }
        FitOutcome::Impossible { deficit_x, deficit_y } => {
            violations.push(format!(
                "fit_slots отказал: не хватает {deficit_x} px по ширине, {deficit_y} по высоте"
            ));
        }
    }
    (name, violations)
}

fn main() {
    println!("=== F3: проверка раскладки «adaptive» на живых минимумах ===");
    let _ = std::io::stdout().flush();

    // --- 1. Живые минимумы (только чтение) ---
    println!("\n--- 1. Живые минимумы открытых окон ---");
    // SAFETY: GetSystemMetrics — чтение параметров системы.
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let mut wa = RECT::default();
    // SAFETY: SystemParametersInfoW(SPI_GETWORKAREA) — рабочая область
    // главного монитора; wa — валидный буфер под RECT.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut wa as *mut RECT).cast()),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    let work_area = if ok.is_ok() {
        Rect {
            x: wa.left,
            y: wa.top,
            w: (wa.right - wa.left) as u32,
            h: (wa.bottom - wa.top) as u32,
        }
    } else {
        Rect {
            x: 0,
            y: 0,
            w: screen_w as u32,
            h: screen_h as u32,
        }
    };
    println!(
        "  монитор: {screen_w}x{screen_h}; рабочая область (SPI_GETWORKAREA): {}x{} (x={}, y={})",
        work_area.w, work_area.h, work_area.x, work_area.y
    );

    let windows = enumerate();
    let mut with_min: Vec<(String, MinSize)> = Vec::new();
    for w in &windows {
        let min = min_window_size(w.hwnd);
        let desc = format!(
            "{} [{}] pid={}",
            if w.title.is_empty() { "(без заголовка)" } else { &w.title },
            w.class,
            w.pid
        );
        match min {
            Some(m) if m.w > 0 && m.h > 0 => {
                with_min.push((
                    desc.clone(),
                    MinSize {
                        width: m.w as u32,
                        height: m.h as u32,
                    },
                ));
                println!("  0x{:08X} {:<58} МИН {}x{}", w.hwnd, desc, m.w, m.h);
            }
            Some(_) => {
                println!("  0x{:08X} {:<58} мин 0x0 (минимум не объявлен)", w.hwnd, desc);
            }
            None => {
                println!("  0x{:08X} {:<58} нет ответа/зависло", w.hwnd, desc);
            }
        }
    }
    println!(
        "  итого окон с известным ненулевым минимумом: {} (порядок — z-order перечисления)",
        with_min.len()
    );

    // --- 2. Семейства vs adaptive ---
    println!("\n--- 2. Семейства vs «adaptive» (зазор {GAP_PCT}%) ---");
    println!(
        "  функция «adaptive» в group_shape.rs: {}",
        if adaptive_function_present() {
            "ЕСТЬ"
        } else {
            "пока нет — жду соседнюю задачу"
        }
    );
    for n in 2..=8usize {
        if with_min.len() < n {
            println!("  {n} окна: не хватает окон с минимумами (есть {})", with_min.len());
            continue;
        }
        let mins: Vec<MinSize> = with_min[..n].iter().map(|(_, m)| *m).collect();
        println!(
            "  === {n} окна: {} ===",
            with_min[..n]
                .iter()
                .map(|(_, m)| format!("{}x{}", m.width, m.height))
                .collect::<Vec<_>>()
                .join(", ")
        );
        // Семейства: сколько вернули и сколько выполнимо.
        let presets = adaptive_presets(n, work_area, GAP_PCT, &to_optional(&mins));
        let mut feasible = 0;
        let mut feasible_names = Vec::new();
        for p in &presets {
            let (name, violations) = verify_preset(p, work_area, &mins);
            if violations.is_empty() {
                feasible += 1;
                feasible_names.push(name);
            }
        }
println!(
        "  семейства: вернули {} раскладок, выполнимо {feasible}",
        presets.len()
        );
        for (i, name) in feasible_names.iter().enumerate() {
            println!("      выполнима #{i}: {name}");
        }
    }

    // --- 3..5. adaptive-функция: инварианты, формы, детерминизм ---
    println!("\n--- 3-5. Раскладка «adaptive» ---");
    let mut adaptive_found = 0usize;
    for n in 2..=8usize {
        if with_min.len() < n {
            continue;
        }
        let mins: Vec<MinSize> = with_min[..n].iter().map(|(_, m)| *m).collect();
        println!(
            "  === {n} окна: {} ===",
            with_min[..n]
                .iter()
                .map(|(_, m)| format!("{}x{}", m.width, m.height))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let opt = rst_core::group_shape::adaptive_layout(n, work_area, GAP_PCT, &to_optional(&mins));
        match &opt {
            None => {
                println!(
                    "  adaptive: НЕ НАШЛА раскладку (минимумы не влезают ни в одно guillotine-разрезание)"
                );
            }
            Some(p) => {
                adaptive_found += 1;
                let (name, violations) = verify_preset(p, work_area, &mins);
                if std::env::var("E3_DEBUG").is_ok() {
                    for (i, s) in p.slots.iter().enumerate() {
                        println!("      DEBUG слот {i}: {s:?}");
                    }
                }
                if violations.is_empty() {
                    println!("  adaptive: НАШЛА — {name} — инварианты чистые");
                } else {
                    println!("  adaptive: НАШЛА — {name} — НАРУШЕНИЯ:");
                    for v in &violations {
                        println!("      {v}");
                    }
                }
                // Детерминизм: второй вызов обязан дать побитово тот же ответ.
                let again =
                    rst_core::group_shape::adaptive_layout(n, work_area, GAP_PCT, &to_optional(&mins));
                let same = again.as_ref().map(|q| q.slots == p.slots).unwrap_or(false);
                println!(
                    "  детерминизм: {}",
                    if same {
                        "побитово одинаковый (2 вызова)"
                    } else {
                        "ОТЛИЧАЕТСЯ МЕЖДУ ВЫЗОВАМИ"
                    }
                );
                // Совпадает ли силуэт с каким-то из семейств (информация).
                let presets = adaptive_presets(n, work_area, GAP_PCT, &to_optional(&mins));
                let dup = presets.iter().any(|f| same_shape(f, p));
                if dup {
                    println!("  (силуэт совпадает с одним из семейств)");
                }
            }
        }
    }
    println!(
        "\n  adaptive нашла решение в {adaptive_found} из 7 случаев (n = 2..8)"
    );

    println!("\nГотово.");
    let _ = std::io::stdout().flush();
}