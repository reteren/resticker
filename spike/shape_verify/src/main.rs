//! Проба E3: независимая проверка генератора раскладок (`group_shape`) на
//! ЖИВЫХ минимумах окон пользователя.
//!
//! Делает три вещи, не веря никому на слово:
//! 1. Собирает минимумы всех открытых окон пользователя через
//!    `rst_win32::window_enum::min_window_size` (только чтение: запрос
//!    WM_GETMINMAXINFO, ничего не двигается и не закрывается) и настоящую
//!    рабочую область главного монитора (SPI_GETWORKAREA — она меньше
//!    монитора на панель задач: 2560x1392 против 2560x1440).
//! 2. Прогоняет генератор `rst_core::group_shape::adaptive_presets` на этих
//!    числах для групп 2..8 окон и НЕЗАВИСИМО проверяет инварианты каждого
//!    возвращённого пресета: число слотов, отсутствие пересечений (в долях и
//!    в пикселях после `apply`), лежание внутри области, выполнимость
//!    минимумов ПОСЛЕ `fit_slots` (генератор мог проверять выполнимость одним
//!    кодом, а вернуть другое). Плюс собственный поиск дублей силуэтов.
//! 3. Сравнивает со старой классической семёркой `presets_for`: сколько из
//!    неё выполнимо — цифра выигрыша для пользователя.
//! 4. Вырожденное: все минимумы неизвестны — генератор обязан вернуть ровно
//!    классическую семёрку.
//!
//! Разведочный инструмент, в workspace не входит (spike исключён).

use std::io::Write;

use rst_core::group_fit::{self, FitOutcome, MinSize};
use rst_core::group_layout::{self, Preset, UnitRect};
use rst_core::group_shape::adaptive_presets;
use rst_core::model::Rect;
use rst_win32::window_enum::{enumerate, is_resizable, min_window_size};

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

/// Собственная (независимая от генератора) проверка инвариантов готовых
/// слотов: число слотов, попарные пересечения, лежание внутри рабочей
/// области, каждый слот не меньше своего минимума. Пусто — всё чисто.
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

/// Две раскладки — один силуэт? Своя проверка: слоты как множества
/// прямоугольников с допуском по каждой кромке (порядок слотов не важен).
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

/// Словесное описание силуэта пресета (для ленты пользователя).
///
/// Сначала ищется «главное» (слот 1 крупнее остальных и занимает грань или
/// край), затем чистые колонки/ряды, затем равномерная сетка; остальное —
/// общий счётчик колонок и рядов.
fn describe_preset(p: &Preset) -> String {
    let n = p.slots.len();
    let main = &p.slots[0];
    let rest = &p.slots[1..];
    let main_full_height = main.y < 1e-9 && (main.h - 1.0).abs() < 1e-9;
    let main_full_width = main.x < 1e-9 && (main.w - 1.0).abs() < 1e-9;
    // «Главное + стопка»: главное в половину ширины на всю высоту, остальные
    // колонками с другой стороны (сколько окон в каждой колонке).
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
    // «Главное + ряд»: главное в половину высоты на всю ширину, остальные
    // в один ряд с другой стороны.
    if main_full_width && (main.h - 0.5).abs() < 1e-9 {
        let side = if main.y < 1e-9 { "сверху" } else { "снизу" };
        let other = if main.y < 1e-9 { "снизу" } else { "сверху" };
        let mut cols_x: Vec<f64> = rest.iter().map(|s| s.x).collect();
        cols_x.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cols_x.dedup();
        let r = cols_x.len();
        return format!("главное {side} (половина высоты, вся ширина), {other} — {r} окна в ряд");
    }
    // Полоса-структуры без главного: колонки, ряды, сетка.
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
    // Равномерная сетка.
    let cell_w = 1.0 / cols as f64;
    let cell_h = 1.0 / rows as f64;
    let uniform = p.slots.iter().all(|s| {
        let cx = (s.w / cell_w).round() as usize;
        let cy = (s.h / cell_h).round() as usize;
        (s.w - cx as f64 * cell_w).abs() < 1e-6 && (s.h - cy as f64 * cell_h).abs() < 1e-6
    });
    if uniform && (cols > 1 || rows > 1) && cols * rows == n {
        return format!("сетка {rows}×{cols} ({n} равных окон)");
    }
    format!("{cols} колонок × {rows} рядов, {n} слотов")
}

/// Сводка по одному пресету: словесный силуэт и независимая проверка.
fn verify_preset(p: &Preset, work_area: Rect, mins: &[MinSize]) -> (String, Vec<String>) {
    let name = describe_preset(p);
    let mut violations = Vec::new();
    let debug = std::env::var("E3_DEBUG").is_ok();
    if debug {
        println!("      ПРЕСЕТ #{:?}: {:?}", p.slots.len(), p.slots);
        println!("      ОПИСАНИЕ: {name}");
    }
    // 1. Форма в долях: слоты в единичном квадрате, без нахлёста.
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
    if debug {
        for (i, s) in p.slots.iter().enumerate() {
            println!("      DEBUG доля {i}: {s:?} (мин {}x{})", mins[i].width, mins[i].height);
        }
    }
    // 2. Пиксели: apply → fit_slots → каждый слот не меньше минимума.
    let gap = gap_px_for(p, work_area);
    let slots = group_layout::apply(p, work_area, gap);
    match group_fit::fit_slots(&slots, work_area, &to_optional(mins)) {
        FitOutcome::Placed(fitted) => {
            if debug {
                for (i, r) in fitted.iter().enumerate() {
                    println!(
                        "      DEBUG fit {i}: {}x{} (мин {}x{})",
                        r.w, r.h, mins[i].width, mins[i].height
                    );
                }
            }
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
    println!("=== E3: проверка генератора раскладок на живых минимумах ===");
    let _ = std::io::stdout().flush();

    // --- 1. Живые минимумы окон пользователя (только чтение) ---
    println!("\n--- 1. Живые минимумы открытых окон ---");
    // SAFETY: GetSystemMetrics — чтение параметров системы.
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let mut wa = RECT::default();
// SAFETY: SystemParametersInfoW(SPI_GETWORKAREA) — чтение рабочей области
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
    let mut with_min: Vec<(String, MinSize, bool)> = Vec::new();
    for w in &windows {
        let min = min_window_size(w.hwnd);
        let resizable = is_resizable(w.hwnd);
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
                    resizable,
                ));
                println!(
                    "  0x{:08X} {:<58} МИН {}x{}  ресайз={resizable}",
                    w.hwnd,
                    desc,
                    m.w,
                    m.h
                );
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

    // --- 2. Генератор: формы, дубли, инварианты ---
    println!("\n--- 2. Генератор adaptive_presets (зазор {GAP_PCT}%) ---");
    for n in 2..=8usize {
        if with_min.len() < n {
            println!(
                "  {n} окна: не хватает окон с минимумами (есть {}), пропущено",
                with_min.len()
            );
            continue;
        }
let mins: Vec<MinSize> = with_min[..n].iter().map(|(_, m, _)| *m).collect();
        println!(
            "  === {n} окна: {} ... ===",
            with_min[..n]
                .iter()
                .map(|(_, m, _)| format!("{}x{}", m.width, m.height))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let presets = adaptive_presets(n, work_area, GAP_PCT, &to_optional(&mins));
        let classic = group_layout::presets_for(n);
        println!("  генератор вернул: {} раскладок", presets.len());
        // Дубли силуэтов — независимая проверка (same_shape по множествам слотов
    // с допуском 1e-9 по каждой кромке, порядок слотов не важен).
    let mut duplicates = 0;
    for i in 0..presets.len() {
        for j in (i + 1)..presets.len() {
            if same_shape(&presets[i], &presets[j]) {
                duplicates += 1;
                println!("  ДЕФЕКТ: дубликаты силуэтов #{i} и #{j}");
            }
        }
    }
        if duplicates == 0 {
            println!("  дублей силуэтов: нет");
        }
        for (i, p) in presets.iter().enumerate() {
            let (name, violations) = verify_preset(p, work_area, &mins);
            let origin = if classic.iter().any(|c| same_shape(c, p)) {
                "классика (дозаполнение ленты)"
            } else {
                "кандидат генератора"
            };
            if violations.is_empty() {
                println!("  #{i} [{origin}]: {name} — инварианты чистые");
            } else {
                println!("  #{i} [{origin}]: {name} — НАРУШЕНИЯ:");
                for v in &violations {
                    println!("      {v}");
                }
            }
        }
    }

    // --- 3. Старая семёрка: сколько выполнимо ---
    println!("\n--- 3. Классическая семёрка presets_for: сколько выполнимо ---");
    for n in 2..=8usize {
        if with_min.len() < n {
            println!("  {n} окна: не хватает окон с минимумами");
            continue;
        }
let mins: Vec<MinSize> = with_min[..n].iter().map(|(_, m, _)| *m).collect();
        let mut feasible = 0;
        for p in group_layout::presets_for(n) {
            let gap = gap_px_for(p, work_area);
            let slots = group_layout::apply(p, work_area, gap);
            let placed = group_fit::fit_slots(&slots, work_area, &to_optional(&mins));
            if let FitOutcome::Placed(fitted) = &placed {
                if check_slots("", fitted, work_area, &mins).is_empty() {
                    feasible += 1;
                }
            }
        }
        println!("  {n} окна: выполнимо {feasible} из 7");
    }

    // --- 4. Вырожденное: все минимумы неизвестны ---
    println!("\n--- 4. Вырожденное: все минимумы неизвестны ---");
    let mut all_ok = true;
    for n in 2..=8usize {
        let unknown: Vec<Option<MinSize>> = (0..n).map(|_| None).collect();
        let got = adaptive_presets(n, work_area, GAP_PCT, &unknown);
        let classic = group_layout::presets_for(n);
        let shapes_match = got.len() == classic.len()
            && got.iter().zip(classic).all(|(g, c)| same_shape(g, c));
        if shapes_match {
            println!("  {n} окна: ровно классическая семёрка без изменений");
        } else {
            all_ok = false;
            println!(
                "  {n} окна: НЕ совпало (вернул {}, ожидали {})",
                got.len(),
                classic.len()
            );
        }
    }
    println!("  итог вырожденного случая: {}", if all_ok { "ОК" } else { "ДЕФЕКТ" });

    println!("\nГотово.");
    let _ = std::io::stdout().flush();
}

