//! Вычисление окклюдеров для маски перекрытия (M4, SPEC.md §4;
//! docs/M4_OCCLUDERS_DESIGN.md). Платформенно-чисто: окна приходят уже
//! переведёнными в [`OccluderCandidate`] координатором
//! (`rst_win32::window_enum::WindowInfo` этому крейту не видно, CONTRIBUTING.md
//! «Правило зависимостей»). Единственное знание о Win32 — задокументированная
//! строка-константа класса окна панели задач.

use uuid::Uuid;

use crate::model::{OverlapRule, Rect, VisibilityMode, WindowLocator};

/// Класс окна панели задач (SPEC.md §4.3, `never_overlap_taskbar`).
pub const TASKBAR_WINDOW_CLASS: &str = "Shell_TrayWnd";

/// Платформенно-чистое окно для матчинга правил видимости.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccluderCandidate {
    /// Полный путь к exe; `None` — `OpenProcess` не дал путь (защищённый
    /// процесс) — процесс-правила такому окну не соответствуют, окно
    /// консервативно считается окклюдером.
    pub exe_path: Option<String>,
    pub title: String,
    pub class: String,
}

/// Один общий набор окклюдеров (одна маска) на монитор
/// (docs/M4_OCCLUDERS_DESIGN.md §1/§7): стикеры с одинаковым эффективным
/// набором окон-окклюдеров делят одну маску вместо маски на весь экран —
/// иначе allow-list одного стикера просачивался бы в другой.
#[derive(Debug, Clone, PartialEq)]
pub struct OccluderSet {
    pub stickers: Vec<Uuid>,
    /// Прямоугольники в физических px, локальные для монитора (после
    /// [`clip_rect`]), порядок не важен.
    pub rects: Vec<Rect>,
}

/// Окно — окклюдер для стикера с правилом видимости `(mode, rules)`?
/// `never_overlap_taskbar` делает панель задач безусловным окклюдером даже
/// в allow-list (SPEC §4.3) — не относится к `Always` (тот вообще не
/// сэмплирует маску).
pub fn is_occluder(
    window: &OccluderCandidate,
    mode: VisibilityMode,
    rules: &[OverlapRule],
    never_overlap_taskbar: bool,
) -> bool {
    // `Always` — поверх всего по определению, панель задач его тоже не
    // перекрывает, даже с `never_overlap_taskbar` включённым.
    if mode == VisibilityMode::Always {
        return false;
    }
    if never_overlap_taskbar && window.class == TASKBAR_WINDOW_CLASS {
        return true;
    }
    match mode {
        VisibilityMode::Always => false,
        // «Только рабочий стол» и «прячется под любым окном» — идентичны
        // для маски (M4_PREP_NOTES §9, открытый вопрос сведения в модели).
        VisibilityMode::Desktop | VisibilityMode::NeverOverlap => true,
        VisibilityMode::OverlapAllowlist => !rules.iter().any(|r| rule_matches(r, window)),
    }
}

/// Правило матчит окно, если совпадает `process_name` ИЛИ `title_pattern`.
/// Пустое правило (оба `None`) не матчит ничего — окно остаётся окклюдером.
///
/// `pub`: панель выбора окон (docs/M4_WINDOW_PICKER_DESIGN.md §2.1) отвечает
/// «выбран ли чекбокс строки/процесса» ровно этим предикатом, которым маска
/// решает про окклюдера, — иначе панель и маска могли бы разойтись во мнениях.
pub fn rule_matches(rule: &OverlapRule, window: &OccluderCandidate) -> bool {
    let by_path = match (&rule.process_name, &window.exe_path) {
        (Some(rule_name), Some(exe_path)) => path_eq_ignore_case(rule_name, exe_path),
        // Правило по процессу, но путь окна неизвестен — не матчим
        // (консервативно: окно остаётся окклюдером).
        _ => false,
    };
    let by_title = match &rule.title_pattern {
        Some(pattern) => wildcard_match(pattern, &window.title),
        None => false,
    };
    by_path || by_title
}

/// Совпадает ли [`WindowLocator`] стикера-окна (ROADMAP.md M6, SPEC.md §5)
/// с живым окном: тот же предикат, что [`rule_matches`] для правил
/// видимости (M4) — `process_name` ИЛИ `title_pattern`, тем же
/// wildcard/регистронезависимым сравнением. Пустой локатор (оба `None`) не
/// матчит ничего: искать окно без единого критерия бессмысленно.
///
/// Используется координатором при старте (переустановка `hwnd`, «в конфиг
/// не пишется» — CONFIG.md, `source.kind == window`) и при добавлении
/// нового стикера-окна (построение локатора из выбранного окна для
/// последующего восстановления).
pub fn locator_matches(locator: &WindowLocator, window: &OccluderCandidate) -> bool {
    let by_path = match (&locator.process_name, &window.exe_path) {
        (Some(name), Some(exe_path)) => path_eq_ignore_case(name, exe_path),
        _ => false,
    };
    let by_title = match &locator.title_pattern {
        Some(pattern) => wildcard_match(pattern, &window.title),
        None => false,
    };
    by_path || by_title
}

/// `process_name` в модели — короткое имя (CONFIG.md), но полный путь тоже
/// матчим на будущее (M4_PREP_NOTES §6.2: модель может расшириться до
/// полного пути, эта функция не изменится).
fn path_eq_ignore_case(rule_name: &str, exe_path: &str) -> bool {
    if rule_name.eq_ignore_ascii_case(exe_path) {
        return true;
    }
    let file_name = exe_path.rsplit(['\\', '/']).next().unwrap_or(exe_path);
    file_name.eq_ignore_ascii_case(rule_name)
}

/// `*`-подстановка (CONFIG.md, «Совпадение окон»): только `*`, произвольная
/// позиция, регистронезависимо. Классический двух-указательный glob с
/// backtracking (без `?`).
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<u8> = pattern.to_lowercase().into_bytes();
    let text: Vec<u8> = text.to_lowercase().into_bytes();
    wildcard_match_bytes(&pattern, &text)
}

fn wildcard_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut match_from = 0usize;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'*') {
            star = Some(p);
            match_from = t;
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            match_from += 1;
            t = match_from;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Пересечение прямоугольника окна (физические px виртуального десктопа) с
/// границами монитора (тоже виртуальный десктоп); `None` — не пересекаются.
/// Результат — в ЛОКАЛЬНЫХ координатах монитора (сдвинут на его origin), как
/// того требует координатное пространство маски (docs/M4_OCCLUDERS_DESIGN.md §4).
pub fn clip_rect(r: &Rect, bounds: &Rect) -> Option<Rect> {
    let x0 = r.x.max(bounds.x);
    let y0 = r.y.max(bounds.y);
    let x1 = (r.x.saturating_add(r.w as i32)).min(bounds.x.saturating_add(bounds.w as i32));
    let y1 = (r.y.saturating_add(r.h as i32)).min(bounds.y.saturating_add(bounds.h as i32));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Rect {
        x: x0 - bounds.x,
        y: y0 - bounds.y,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(exe: Option<&str>, title: &str, class: &str) -> OccluderCandidate {
        OccluderCandidate {
            exe_path: exe.map(String::from),
            title: title.to_string(),
            class: class.to_string(),
        }
    }

    fn rule(process: Option<&str>, title: Option<&str>) -> OverlapRule {
        OverlapRule {
            process_name: process.map(String::from),
            title_pattern: title.map(String::from),
        }
    }

    fn locator(process: Option<&str>, title: Option<&str>) -> WindowLocator {
        WindowLocator {
            process_name: process.map(String::from),
            title_pattern: title.map(String::from),
            ..Default::default()
        }
    }

    // --- locator_matches (M6, стикеры-окна) ---

    #[test]
    fn locator_matches_by_short_process_name() {
        let w = candidate(Some(r"C:\Program Files\obs-studio\obs64.exe"), "t", "c");
        assert!(locator_matches(&locator(Some("obs64.exe"), None), &w));
        assert!(!locator_matches(&locator(Some("other.exe"), None), &w));
    }

    #[test]
    fn locator_matches_by_title_wildcard() {
        let w = candidate(None, "OBS 30.1 - Профиль: Стрим", "c");
        assert!(locator_matches(&locator(None, Some("OBS *")), &w));
        assert!(!locator_matches(&locator(None, Some("Chrome *")), &w));
    }

    #[test]
    fn locator_empty_matches_nothing() {
        let w = candidate(Some(r"C:\a\b.exe"), "любой заголовок", "c");
        assert!(!locator_matches(&locator(None, None), &w));
    }

    #[test]
    fn locator_process_rule_no_exe_path_does_not_match() {
        let w = candidate(None, "t", "c");
        assert!(!locator_matches(&locator(Some("chrome.exe"), None), &w));
    }

    #[test]
    fn locator_matches_by_either_criterion() {
        // Правило по пути ИЛИ заголовку — как rule_matches: достаточно
        // совпадения хотя бы одного критерия.
        let w = candidate(Some(r"C:\a\notepad.exe"), "Wrong Title", "c");
        assert!(locator_matches(
            &locator(Some("notepad.exe"), Some("*Never*")),
            &w
        ));
    }

    // --- wildcard_match ---

    #[test]
    fn wildcard_exact_match() {
        assert!(wildcard_match("hello", "hello"));
        assert!(!wildcard_match("hello", "hellox"));
    }

    #[test]
    fn wildcard_star_at_start_middle_end() {
        assert!(wildcard_match("*.txt", "readme.txt"));
        assert!(wildcard_match("read*.txt", "readme.txt"));
        assert!(wildcard_match("readme*", "readme.txt"));
        assert!(!wildcard_match("*.txt", "readme.md"));
    }

    #[test]
    fn wildcard_multiple_stars() {
        assert!(wildcard_match("*a*b*", "xaxbx"));
        assert!(wildcard_match("**", "anything"));
    }

    #[test]
    fn wildcard_empty_pattern_and_text() {
        assert!(wildcard_match("", ""));
        assert!(!wildcard_match("", "x"));
        assert!(wildcard_match("*", ""));
    }

    #[test]
    fn wildcard_case_insensitive() {
        assert!(wildcard_match("Chrome*", "chrome.exe title"));
    }

    // --- is_occluder ---

    #[test]
    fn always_never_occludes() {
        let w = candidate(Some("chrome.exe"), "t", "c");
        assert!(!is_occluder(&w, VisibilityMode::Always, &[], false));
        assert!(!is_occluder(&w, VisibilityMode::Always, &[], true));
    }

    #[test]
    fn desktop_and_never_overlap_always_occlude() {
        let w = candidate(None, "t", "c");
        assert!(is_occluder(&w, VisibilityMode::Desktop, &[], false));
        assert!(is_occluder(&w, VisibilityMode::NeverOverlap, &[], false));
    }

    #[test]
    fn allowlist_empty_rules_is_desktop_equivalent() {
        let w = candidate(Some("chrome.exe"), "t", "c");
        assert!(is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &[],
            false
        ));
    }

    #[test]
    fn allowlist_matches_by_short_process_name() {
        let w = candidate(Some(r"C:\Program Files\Google\chrome.exe"), "t", "c");
        let rules = [rule(Some("chrome.exe"), None)];
        assert!(!is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            false
        ));
    }

    #[test]
    fn allowlist_matches_by_full_path() {
        let w = candidate(Some(r"C:\apps\foo.exe"), "t", "c");
        let rules = [rule(Some(r"C:\apps\foo.exe"), None)];
        assert!(!is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            false
        ));
    }

    #[test]
    fn allowlist_process_rule_no_exe_path_still_occludes() {
        let w = candidate(None, "t", "c");
        let rules = [rule(Some("chrome.exe"), None)];
        assert!(is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            false
        ));
    }

    #[test]
    fn allowlist_matches_by_title_wildcard() {
        let w = candidate(None, "Untitled - Notepad", "c");
        let rules = [rule(None, Some("*Notepad"))];
        assert!(!is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            false
        ));
    }

    #[test]
    fn allowlist_empty_rule_matches_nothing() {
        let w = candidate(Some("x.exe"), "t", "c");
        let rules = [rule(None, None)];
        assert!(is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            false
        ));
    }

    #[test]
    fn taskbar_forced_occluder_when_setting_on_even_if_allowlisted() {
        let w = candidate(None, "", TASKBAR_WINDOW_CLASS);
        let rules = [rule(None, Some("*"))];
        assert!(is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            true
        ));
    }

    #[test]
    fn taskbar_not_forced_when_setting_off() {
        let w = candidate(None, "", TASKBAR_WINDOW_CLASS);
        let rules = [rule(None, Some("*"))];
        assert!(!is_occluder(
            &w,
            VisibilityMode::OverlapAllowlist,
            &rules,
            false
        ));
    }

    #[test]
    fn taskbar_setting_does_not_override_always() {
        let w = candidate(None, "", TASKBAR_WINDOW_CLASS);
        assert!(!is_occluder(&w, VisibilityMode::Always, &[], true));
    }

    // --- clip_rect ---

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn clip_rect_full_overlap_inside_monitor() {
        let bounds = r(0, 0, 1920, 1080);
        let win = r(100, 100, 200, 200);
        assert_eq!(clip_rect(&win, &bounds), Some(r(100, 100, 200, 200)));
    }

    #[test]
    fn clip_rect_partial_overlap_clips_to_bounds() {
        let bounds = r(0, 0, 1920, 1080);
        let win = r(1800, 1000, 300, 300);
        assert_eq!(clip_rect(&win, &bounds), Some(r(1800, 1000, 120, 80)));
    }

    #[test]
    fn clip_rect_touching_edge_is_none() {
        let bounds = r(0, 0, 1920, 1080);
        let win = r(1920, 0, 100, 100);
        assert_eq!(clip_rect(&win, &bounds), None);
    }

    #[test]
    fn clip_rect_entirely_outside_is_none() {
        let bounds = r(0, 0, 1920, 1080);
        let win = r(5000, 5000, 100, 100);
        assert_eq!(clip_rect(&win, &bounds), None);
    }

    #[test]
    fn clip_rect_second_monitor_negative_origin_translates_to_local() {
        // Монитор слева от primary — отрицательный origin виртуального
        // десктопа (M3, ADR-010).
        let bounds = r(-1920, 0, 1920, 1080);
        let win = r(-1920, 100, 500, 400);
        assert_eq!(clip_rect(&win, &bounds), Some(r(0, 100, 500, 400)));
    }

    #[test]
    fn clip_rect_window_spans_two_monitors_clips_per_monitor() {
        let left = r(-1920, 0, 1920, 1080);
        let right = r(0, 0, 1920, 1080);
        let win = r(-100, 200, 400, 300);
        assert_eq!(clip_rect(&win, &left), Some(r(1820, 200, 100, 300)));
        assert_eq!(clip_rect(&win, &right), Some(r(0, 200, 300, 300)));
    }
}
