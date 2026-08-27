//! Проба C4 (репорт 2026-08-26 со скриншотом): «все пресеты, где окна не
//! одинакового размера, сломаны — маленькие окна налазят друг на друга».
//! Известная причина — собственный минимальный размер приложений
//! (`WM_GETMINMAXINFO`/`ptMinTrackSize`). Эта проба собирает НАСТОЯЩИЕ
//! минимумы реально открытых окон пользователя и прогоняет их через НАСТОЯЩУЮ
//! таблицу раскладок (`rst_core::group_layout`) на его рабочей области.
//!
//! Только чтение: окна не двигаются, не сворачиваются, не активируются.
//! Заголовок и MINMAXINFO читаются `SendMessageTimeoutW` с `SMTO_ABORTIFHUNG`
//! и таймаутом 300 мс — зависшее приложение не может повесить пробу.
//!
//! Запуск: cargo run --manifest-path spike/minmax_probe/Cargo.toml

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::PWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_TIMEOUT, HWND, LPARAM, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SendMessageTimeoutW, GWL_STYLE, MINMAXINFO, SMTO_ABORTIFHUNG,
    WM_GETMINMAXINFO, WM_GETTEXT, WS_THICKFRAME,
};

/// Чьи минимумы нам нужны: exe-имена приложений пользователя (репорт
/// 2026-08-26; скриншот: Nemora, Spotify, git-клиент, Upscayl). Регистр
/// файловой системы Windows не важен — сравнение без учёта регистра.
const INTERESTING_EXES: &[&str] = &[
    "spotify.exe",
    "opera.exe",
    "discord.exe",
    "obs64.exe",
    "explorer.exe",
    "claude.exe",
    "githubdesktop.exe",
    "upscayl.exe",
    "nemora.exe",
];

/// Таймаут кросс-поточного запроса (мс): 300 — верх разумного для живого
/// окна; `SMTO_ABORTIFHUNG` оборвёт сразу на зависшем.
const REQUEST_TIMEOUT_MS: u32 = 300;

/// Измерение одного живого окна.
#[derive(Debug, Clone)]
struct MeasuredWindow {
    exe: String,
    title: String,
    /// Есть ли рамка ресайза (`WS_THICKFRAME`) — только такие окна вообще
    /// участвуют в раскладке; фиксированные диалоги менять нельзя.
    resizable: bool,
    /// `ptMinTrackSize` из WM_GETMINMAXINFO: минимум в координатах
    /// `GetWindowRect` (включая невидимые поля ресайза).
    min_track: (i32, i32),
    /// Текущий прямоугольник окна (GetWindowRect) и его DWM-габариты.
    rect_gwr: (i32, i32, i32, i32),
    rect_dwm: (i32, i32, i32, i32),
    iconic: bool,
    /// Дельты «GetWindowRect минус DWM» по осям (невидимые поля ресайза).
    /// Проставляется пост-обработкой (`resolve_deltas`): у свёрнутого окна
    /// обе геометрии мусорные.
    delta: (i32, i32),
}

impl MeasuredWindow {
    /// Минимум, объявленный приложением, в DWM-координатах: `ptMinTrackSize`
    /// живёт в координатах GetWindowRect (включая невидимые поля ресайза), а
    /// раскладка работает в DWM — вычитаем дельту рамки. Дельту проставляет
    /// `resolve_deltas` (у свёрнутого окна и GWR, и DWM мусорные, поэтому
    /// дельта берётся у живого окна того же приложения).
    fn min_dwm(&self) -> (i32, i32) {
        (
            (self.min_track.0 - self.delta.0).max(0),
            (self.min_track.1 - self.delta.1).max(0),
        )
    }

    /// Текущий размер окна в DWM-координатах.
    fn dwm_size(&self) -> (i32, i32) {
        (
            self.rect_dwm.2 - self.rect_dwm.0,
            self.rect_dwm.3 - self.rect_dwm.1,
        )
    }
}

fn main() {
    // SAFETY: объявление DPI-контекста процесса; отказ не критичен (на
    // масштабе пользователя 100% ничего не меняет, но честность замера
    // требует единого контекста).
    let _ = unsafe {
        SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };

    let windows = enumerate();
    // Видимые изменяемые (с рамкой ресайза) окна интересных приложений —
    // ровно те, что умеют участвовать в раскладке и чья геометрия осмысленна.
    let interesting: Vec<MeasuredWindow> = windows
        .iter()
        .filter(|w| {
            w.resizable
                && INTERESTING_EXES.iter().any(|e| {
                    std::path::Path::new(&w.exe)
                        .file_name()
                        .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(e))
                })
        })
        .cloned()
        .collect();
    let interesting = resolve_deltas(interesting);
    // Главное окно каждого приложения: самое требовательное по минимумам
    // (у приложения бывает несколько top-level окон — Opera «картинка в
    // картинке», Claude второй экземпляр, Discord попауты).
    let apps = main_windows(&interesting);

    println!("=== 1. Минимумы приложений ===");
    println!(
        "{:<16} {:<30} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>4}",
        "app", "title", "rawW", "rawH", "minW", "minH", "dW", "dH", "min"
    );
    for w in &apps {
        let (mw, mh) = w.min_dwm();
        let title: String = w.title.chars().take(29).collect();
        let app = std::path::Path::new(&w.exe)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!(
            "{:<16} {:<30} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>4}",
            app,
            title,
            w.min_track.0,
            w.min_track.1,
            mw,
            mh,
            w.delta.0,
            w.delta.1,
            if w.iconic { "да" } else { "" }
        );
    }

    println!("\n=== 2. Раскладки на реальных минимумах ===");
    let work = work_area();
    println!(
        "Рабочая область: {}x{}",
        work.right - work.left,
        work.bottom - work.top
    );
    let work = to_core_rect(work);

    // Скриншот пользователя: четыре окна колонками.
    let screenshot = pick_by_name(
        &apps,
        &[
            "nemora.exe",
            "spotify.exe",
            "githubdesktop.exe",
            "upscayl.exe",
        ],
    );
    let fit = FitHarness { gap_pct: 4 };
    for n in [4usize, 3, 2] {
        println!("\n--- группы из {n} окон ---");
        for preset_idx in 0..7 {
            let slots = slots_for_preset(work, n, preset_idx, fit.gap_pct);
            if n == 4 {
                let r = fit.best_permutation(&screenshot, &slots);
                print_fit_row(preset_idx, &r, &screenshot, &slots);
            } else {
                // Все подмножества размера n из четвёрки скриншота.
                let subsets = subsets(&screenshot, n);
                let failing: Vec<String> = subsets
                    .iter()
                    .filter(|s| !fit.best_permutation(s, &slots).fits)
                    .map(|s| s.iter().map(app_name).collect::<Vec<_>>().join("+"))
                    .collect();
                if failing.is_empty() {
                    println!(
                        "пресет {preset_idx}: подходит всем {} подмножествам из {n} окон",
                        subsets.len()
                    );
                } else {
                    println!(
                        "пресет {preset_idx}: НЕ подходит {} из {} подмножеств из {n} окон: {}",
                        failing.len(),
                        subsets.len(),
                        failing.join("; ")
                    );
                }
            }
        }
    }

    println!("\n=== 3. Все сочетания из всех измеренных приложений (4 окна) ===");
    let all_combos = subsets(&apps, 4);
    for preset_idx in 0..7 {
        let slots = slots_for_preset(work, 4, preset_idx, fit.gap_pct);
        let ok = all_combos
            .iter()
            .filter(|c| fit.best_permutation(c, &slots).fits)
            .count();
        println!(
            "пресет {preset_idx}: влезает {} из {} сочетаний",
            ok,
            all_combos.len()
        );
    }

    println!("\n=== 4. Решатель rst_core::group_fit::fit_slots на моих числах ===");
    for n in [4usize, 3, 2] {
        println!("\n--- группы из {n} окон ---");
        for preset_idx in 0..7 {
            let slots = slots_for_preset(work, n, preset_idx, fit.gap_pct);
            let subsets = subsets(&screenshot, n);
            let mut best = SolverSummary::default();
            for subset in &subsets {
                let r = solver_best_permutation(subset, &slots, work);
                best.merge(&r);
                let names: String = subset.iter().map(app_name).collect::<Vec<_>>().join("+");
                match r {
                    SolverOutcome::Impossible {
                        deficit_x,
                        deficit_y,
                    } => println!(
                        "пресет {preset_idx}: НЕ укладывается {names}: не хватает {deficit_x} по ширине, {deficit_y} по высоте"
                    ),
                    SolverOutcome::Violation(msg) => {
                        println!("пресет {preset_idx}: {names}: НАРУШЕНИЕ ИНВАРИАНТОВ: {msg}")
                    }
                    SolverOutcome::Placed => {}
                }
            }
            println!(
                "пресет {preset_idx}: решатель укладывает {} из {} подмножеств из {n} окон",
                best.placed_count,
                subsets.len()
            );
        }
    }

    println!("\nготово");
}

/// Перевести `RECT` из Win32 в прямоугольник модели rst-core.
fn to_core_rect(r: RECT) -> rst_core::model::Rect {
    rst_core::model::Rect {
        x: r.left,
        y: r.top,
        w: (r.right - r.left).max(0) as u32,
        h: (r.bottom - r.top).max(0) as u32,
    }
}

/// Слоты одного пресета с реальным зазором (та же формула, что
/// `GroupEditor::layout_targets` в groups.rs: зазор — процент от наименьшей
/// стороны самого маленького слота при нулевом зазоре).
fn slots_for_preset(
    work: rst_core::model::Rect,
    n: usize,
    preset_idx: usize,
    gap_pct: u8,
) -> Vec<rst_core::model::Rect> {
    let presets = rst_core::group_layout::presets_for(n);
    let bare = rst_core::group_layout::apply(&presets[preset_idx], work, 0);
    let smallest = bare.iter().map(|r| r.w.min(r.h)).min().unwrap_or(0);
    let gap = (u32::from(gap_pct) * smallest / 100) as i32;
    rst_core::group_layout::apply(&presets[preset_idx], work, gap)
}

/// Результат проверки одной раскладки: влезла ли (при лучшей перестановке)
/// и, если нет, кому и на сколько не хватает.
struct FitResult {
    fits: bool,
    /// Лучшая перестановка: `perm[slot]` — индекс окна в исходном списке.
    perm: Vec<usize>,
    /// Нехватки по слотам: `shortfalls[slot]` = (нехватка ширины, нехватка
    /// высоты) в лучшей перестановке; ноль — влезает.
    shortfalls: Vec<(i32, i32)>,
}

/// Проверка раскладки с учётом минимумов: существует ли перестановка окон,
/// при которой каждое окно влезает в свой слот целиком (и по ширине, и по
/// высоте). Слоты — DWM-координаты, минимумы переведены в DWM.
struct FitHarness {
    gap_pct: u8,
}

impl FitHarness {
    fn best_permutation(
        &self,
        windows: &[MeasuredWindow],
        slots: &[rst_core::model::Rect],
    ) -> FitResult {
        assert_eq!(windows.len(), slots.len());
        let n = windows.len();
        // Перебираем все перестановки (n <= 4, максимум 24) — точный ответ,
        // а не жадная эвристика.
        let mut perm: Vec<usize> = (0..n).collect();
        // Лучшая перестановка и её нехватки по слотам.
        type Best = (Vec<usize>, Vec<(i32, i32)>);
        let mut best: Option<Best> = None;
        loop {
            let shortfalls: Vec<(i32, i32)> = (0..n)
                .map(|slot| {
                    let w = &windows[perm[slot]];
                    let (mw, mh) = w.min_dwm();
                    let slot = slots[slot];
                    ((mw - slot.w as i32).max(0), (mh - slot.h as i32).max(0))
                })
                .collect();
            let total: i32 = shortfalls.iter().map(|(a, b)| a + b).sum();
            let replace = match &best {
                None => true,
                Some((_, prev)) => {
                    let prev_total: i32 = prev.iter().map(|(a, b)| a + b).sum();
                    total < prev_total
                }
            };
            if replace {
                best = Some((perm.clone(), shortfalls));
            }
            if !next_permutation(&mut perm) {
                break;
            }
        }
        let (perm, best) = best.unwrap();
        let fits = best.iter().all(|(a, b)| *a == 0 && *b == 0);
        FitResult {
            fits,
            // Если влезает — перестановка не важна; для отчёта оставляем
            // лучшую найденную (последнюю с минимальной суммой).
            perm,
            shortfalls: best,
        }
    }
}

fn next_permutation(a: &mut [usize]) -> bool {
    let n = a.len();
    if n < 2 {
        return false;
    }
    let mut i = n - 2;
    while a[i] >= a[i + 1] {
        if i == 0 {
            return false;
        }
        i -= 1;
    }
    let mut j = n - 1;
    while a[j] <= a[i] {
        j -= 1;
    }
    a.swap(i, j);
    a[i + 1..].reverse();
    true
}

/// Итог проверки решателя по одному подмножеству окон.
#[derive(Debug)]
enum SolverOutcome {
    /// `fit_slots` дал `Placed`, и инварианты сошлись (каждый слот ≥ минимума,
    /// попарно без пересечений, всё внутри рабочей области).
    Placed,
    /// `fit_slots` честно отказал с дефицитом.
    Impossible { deficit_x: u32, deficit_y: u32 },
    /// `fit_slots` вернул `Placed`, но инварианты НЕ сошлись — это расхождение
    /// решателя с его контрактом, ради которого всё и затевалось.
    Violation(String),
}

#[derive(Default)]
struct SolverSummary {
    placed_count: usize,
}

impl SolverSummary {
    fn merge(&mut self, r: &SolverOutcome) {
        if matches!(r, SolverOutcome::Placed) {
            self.placed_count += 1;
        }
    }
}

/// Прогнать `fit_slots` на всех перестановках окон по слотам и вернуть лучший
/// исход (Placed — если хоть одна перестановка уложилась). На каждом
/// `Placed` независимо проверяются инварианты результата: внутри рабочей
/// области, не меньше минимумов, попарно без пересечений — это и есть
/// «проверка глазами по числам» из постановки задачи.
fn solver_best_permutation(
    windows: &[MeasuredWindow],
    slots: &[rst_core::model::Rect],
    work_area: rst_core::model::Rect,
) -> SolverOutcome {
    assert_eq!(windows.len(), slots.len());
    let n = windows.len();
    let mut perm: Vec<usize> = (0..n).collect();
    let mut best: Option<SolverOutcome> = None;
    loop {
        let minimums: Vec<Option<rst_core::group_fit::MinSize>> = (0..n)
            .map(|slot| {
                let (mw, mh) = windows[perm[slot]].min_dwm();
                Some(rst_core::group_fit::MinSize {
                    width: mw.max(0) as u32,
                    height: mh.max(0) as u32,
                })
            })
            .collect();
        let outcome = match rst_core::group_fit::fit_slots(slots, work_area, &minimums) {
            rst_core::group_fit::FitOutcome::Placed(fitted) => {
                match verify_fit_invariants(&fitted, work_area, &minimums) {
                    Ok(()) => SolverOutcome::Placed,
                    Err(msg) => SolverOutcome::Violation(msg),
                }
            }
            rst_core::group_fit::FitOutcome::Impossible {
                deficit_x,
                deficit_y,
            } => SolverOutcome::Impossible {
                deficit_x,
                deficit_y,
            },
        };
        // Лучший исход: Placed > Violation > Impossible.
        let better = match (&best, &outcome) {
            (None, _) => true,
            (Some(SolverOutcome::Placed), _) => false,
            (Some(SolverOutcome::Violation(_)), SolverOutcome::Placed) => true,
            (Some(SolverOutcome::Violation(_)), SolverOutcome::Violation(_)) => false,
            (Some(SolverOutcome::Violation(_)), SolverOutcome::Impossible { .. }) => false,
            (Some(SolverOutcome::Impossible { .. }), _) => {
                !matches!(outcome, SolverOutcome::Impossible { .. })
            }
        };
        if better {
            best = Some(outcome);
        }
        if !next_permutation(&mut perm) {
            break;
        }
    }
    best.unwrap()
}

/// Независимая проверка инвариантов результата решателя.
fn verify_fit_invariants(
    fitted: &[rst_core::model::Rect],
    area: rst_core::model::Rect,
    minimums: &[Option<rst_core::group_fit::MinSize>],
) -> Result<(), String> {
    for (i, s) in fitted.iter().enumerate() {
        if s.x < area.x || s.y < area.y {
            return Err(format!("слот {i} вылез за левый/верхний край: {s:?}"));
        }
        if i64::from(s.x) + i64::from(s.w) > i64::from(area.x) + i64::from(area.w)
            || i64::from(s.y) + i64::from(s.h) > i64::from(area.y) + i64::from(area.h)
        {
            return Err(format!("слот {i} вылез за область: {s:?}"));
        }
        if let Some(m) = minimums.get(i).copied().flatten() {
            if s.w < m.width || s.h < m.height {
                return Err(format!(
                    "слот {i} меньше минимума: {}x{} против {m:?}",
                    s.w, s.h
                ));
            }
        }
    }
    for (i, a) in fitted.iter().enumerate() {
        for (j, b) in fitted.iter().enumerate().skip(i + 1) {
            let ix = (i64::from(a.x) + i64::from(a.w)).min(i64::from(b.x) + i64::from(b.w))
                - i64::from(a.x).max(i64::from(b.x));
            let iy = (i64::from(a.y) + i64::from(a.h)).min(i64::from(b.y) + i64::from(b.h))
                - i64::from(a.y).max(i64::from(b.y));
            if ix > 0 && iy > 0 {
                return Err(format!("слоты {i} и {j} налезают: {a:?} vs {b:?}"));
            }
        }
    }
    Ok(())
}

/// Печать строки «влезает / не влезает» для четырёх окон скриншота: по
/// каждому окну — его минимум и нехватка в лучшей перестановке (окно
/// привязано к СВОЕМУ слоту через перестановку, а не к порядку списка).
fn print_fit_row(
    preset_idx: usize,
    r: &FitResult,
    windows: &[MeasuredWindow],
    slots: &[rst_core::model::Rect],
) {
    let status = if r.fits {
        "ВЛЕЗАЕТ"
    } else {
        "НЕ ВЛЕЗАЕТ"
    };
    println!("пресет {preset_idx}: {status}");
    for (wi, w) in windows.iter().enumerate() {
        let slot = r.perm.iter().position(|p| *p == wi).unwrap();
        let (mw, mh) = w.min_dwm();
        let (sw, sh) = (slots[slot].w, slots[slot].h);
        let (dw, dh) = r.shortfalls[slot];
        let mut missing = Vec::new();
        if dw > 0 {
            missing.push(format!("{dw} по ширине"));
        }
        if dh > 0 {
            missing.push(format!("{dh} по высоте"));
        }
        let need = if r.fits {
            format!("min {mw}x{mh} в слот {sw}x{sh}")
        } else {
            format!(
                "min {mw}x{mh} в слот {sw}x{sh}: не хватает {}",
                if missing.is_empty() {
                    "0 (нехватка у соседа)".to_string()
                } else {
                    missing.join(", ")
                }
            )
        };
        println!("    {:>14} {need}", app_name(w));
    }
}

/// Подмножества размера `k` (перебор по битовой маске).
fn subsets(items: &[MeasuredWindow], k: usize) -> Vec<Vec<MeasuredWindow>> {
    let n = items.len();
    let mut out = Vec::new();
    for mask in 0u32..(1u32 << n) {
        if mask.count_ones() as usize != k {
            continue;
        }
        out.push(
            (0..n)
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| items[i].clone())
                .collect(),
        );
    }
    out
}

// ---------- измерение ----------

fn enumerate() -> Vec<MeasuredWindow> {
    let mut out = Vec::new();
    // SAFETY: колбэк живёт внутри вызова, указатель на `out` валиден.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&raw mut out as *mut _ as isize));
    }
    out
}

// SAFETY: конвенция EnumWindows; `data` — &mut Vec из `enumerate`.
unsafe extern "system" fn enum_proc(hwnd: HWND, data: LPARAM) -> windows::core::BOOL {
    let out = unsafe { &mut *(data.0 as *mut Vec<MeasuredWindow>) };
    if let Some(w) = collect(hwnd) {
        out.push(w);
    }
    windows::core::BOOL::from(true)
}

fn collect(hwnd: HWND) -> Option<MeasuredWindow> {
    // SAFETY: чтение состояния живого окна; мёртвый хэндл безопасен.
    unsafe {
        // Невидимые окна (скрытые в трей, служебные) пропускаем: их
        // геометрия мусорная, в раскладке они не участвуют.
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let exe = process_exe(hwnd);
        if exe.as_os_str().is_empty() {
            return None;
        }
        let title = window_title(hwnd);
        let iconic = IsIconic(hwnd).as_bool();
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let resizable = style & WS_THICKFRAME.0 != 0;
        let (min_track, _) = minmax_info(hwnd);
        let mut gwr = RECT::default();
        if GetWindowRect(hwnd, &mut gwr).is_err() {
            return None;
        }
        let dwm = dwm_bounds(hwnd);
        Some(MeasuredWindow {
            exe: exe.to_string_lossy().into_owned(),
            title,
            resizable,
            min_track,
            rect_gwr: (gwr.left, gwr.top, gwr.right, gwr.bottom),
            rect_dwm: (dwm.left, dwm.top, dwm.right, dwm.bottom),
            iconic,
            delta: (0, 0),
        })
    }
}

/// Собственная дельта «GetWindowRect минус DWM» окна по осям.
fn own_delta(w: &MeasuredWindow) -> (i32, i32) {
    (
        (w.rect_gwr.2 - w.rect_gwr.0) - (w.rect_dwm.2 - w.rect_dwm.0),
        (w.rect_gwr.3 - w.rect_gwr.1) - (w.rect_dwm.3 - w.rect_dwm.1),
    )
}

/// Проставить дельты рамки всем окнам. У свёрнутого окна и GWR, и DWM
/// мусорные — дельта берётся у живого (не свёрнутого, не полноэкранного)
/// окна того же приложения; полноэкранный режим даёт свою дельту (у Opera
/// 16x16), не подходящую обычному состоянию. Если живого окна того же
/// приложения нет — консервативный дефолт (14, 7): невидимые поля ресайза
/// Windows 11, замеренные на классических окнах (переоценка минимума на
/// ≤14 px безопасна для вывода «влезает»).
fn resolve_deltas(windows: Vec<MeasuredWindow>) -> Vec<MeasuredWindow> {
    let work = work_area();
    let fullscreen_area = (work.right - work.left) as i64 * (work.bottom - work.top) as i64;
    let mut out = Vec::with_capacity(windows.len());
    for i in 0..windows.len() {
        let mut w = windows[i].clone();
        w.delta = if w.iconic {
            let candidates: Vec<&MeasuredWindow> = windows
                .iter()
                .filter(|s| !s.iconic && s.exe == w.exe)
                .filter(|s| {
                    let (sw, sh) = s.dwm_size();
                    (sw as i64 * sh as i64) < fullscreen_area * 95 / 100
                })
                .collect();
            match candidates.iter().max_by_key(|s| {
                let (sw, sh) = s.dwm_size();
                sw as i64 * sh as i64
            }) {
                Some(s) => own_delta(s),
                None => (14, 7),
            }
        } else {
            own_delta(&w)
        };
        out.push(w);
    }
    out
}

/// Главные окна приложений: по одному на exe, самое требовательное по
/// минимумам (площадь `ptMinTrackSize` максимальна — это и есть главное
/// окно: у Opera минимум вкладки 660x310 против 284x160 у «картинки в
/// картинке»).
fn main_windows(windows: &[MeasuredWindow]) -> Vec<MeasuredWindow> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for w in windows {
        let name = std::path::Path::new(&w.exe)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        if seen.contains(&name) {
            continue;
        }
        let best = windows
            .iter()
            .filter(|s| {
                std::path::Path::new(&s.exe)
                    .file_name()
                    .is_some_and(|f| f.to_string_lossy() == name)
            })
            .max_by_key(|s| s.min_track.0 as i64 * s.min_track.1 as i64)
            .cloned()
            .expect("хотя бы одно окно приложения есть");
        out.push(best);
        seen.push(name);
    }
    out
}

/// Выбрать главные окна по имени exe.
fn pick_by_name(apps: &[MeasuredWindow], names: &[&str]) -> Vec<MeasuredWindow> {
    names
        .iter()
        .filter_map(|name| {
            apps.iter()
                .find(|w| {
                    std::path::Path::new(&w.exe)
                        .file_name()
                        .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name))
                })
                .cloned()
        })
        .collect()
}

/// Имя exe без расширения — для читаемых строк отчёта.
fn app_name(w: &MeasuredWindow) -> String {
    std::path::Path::new(&w.exe)
        .file_name()
        .map(|f| f.to_string_lossy().trim_end_matches(".exe").to_string())
        .unwrap_or_default()
}

/// `ptMinTrackSize` окна через `SendMessageTimeoutW` с таймаутом: зависшее
/// окно не вешает пробу. Первый элемент пары — (ширина, высота) минимума в
/// координатах GetWindowRect; второй — ответило ли окно.
///
/// Возврат WM_GETMINMAXINFO у DefWindowProc всегда 0, поэтому успех
/// отличается от таймаута не возвратом, а кодом ошибки: SMTO_ABORTIFHUNG
/// ставит ERROR_TIMEOUT ровно на оборванном ожидании. Перед вызовом код
/// сбрасывается, чтобы не поймать чужую устаревшую ошибку.
fn minmax_info(hwnd: HWND) -> ((i32, i32), bool) {
    let mut info = MINMAXINFO::default();
    // SAFETY: info — валидный буфер; таймаут + SMTO_ABORTIFHUNG ограничивают
    // ожидание на чужом зависшем потоке.
    unsafe { SetLastError(windows::Win32::Foundation::WIN32_ERROR(0)) };
    let _sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM(&raw mut info as *mut _ as isize),
            SMTO_ABORTIFHUNG,
            REQUEST_TIMEOUT_MS,
            None,
        )
    };
    // SAFETY: код ошибки текущего потока, прочитан сразу после вызова.
    let err = unsafe { GetLastError().0 };
    if err == ERROR_TIMEOUT.0 {
        return ((0, 0), false);
    }
    ((info.ptMinTrackSize.x, info.ptMinTrackSize.y), true)
}

/// Заголовок окна (кросс-поточный, с таймаутом — тот же приём, что в
/// window_enum.rs).
fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let mut copied = 0usize;
    // SAFETY: buf — буфер под копию, SendMessageTimeoutW ограничен таймаутом.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETTEXT,
            WPARAM(buf.len()),
            LPARAM(buf.as_mut_ptr() as isize),
            SMTO_ABORTIFHUNG,
            REQUEST_TIMEOUT_MS,
            Some(&raw mut copied),
        )
    };
    if sent.0 == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied.min(buf.len())])
}

fn process_exe(hwnd: HWND) -> PathBuf {
    let mut pid = 0u32;
    // SAFETY: pid — валидный out-параметр.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return PathBuf::new();
    }
    // SAFETY: хэндл закрывается ниже в любом случае.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    let Ok(process) = process else {
        return PathBuf::new();
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: process — свежий хэндл; buf/len — валидный выход.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    // SAFETY: хэндл наш и больше не нужен.
    unsafe {
        let _ = CloseHandle(process);
    }
    if ok {
        PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        PathBuf::new()
    }
}

fn dwm_bounds(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: rect — валидный буфер; атрибут окна — чтение.
    let _ = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
    };
    rect
}

/// Рабочая область основного монитора.
fn work_area() -> RECT {
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi заполнен; MONITOR_DEFAULTTOPRIMARY — основной монитор.
    let mon = unsafe { MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY) };
    let _ = unsafe { GetMonitorInfoW(mon, &mut mi) };
    mi.rcWork
}
