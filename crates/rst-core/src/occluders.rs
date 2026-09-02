//! Вычисление окклюдеров для маски перекрытия (M4, SPEC.md §4;
//! docs/M4_OCCLUDERS_DESIGN.md). Платформенно-чисто: окна приходят уже
//! переведёнными в [`OccluderCandidate`] координатором
//! (`rst_win32::window_enum::WindowInfo` этому крейту не видно, CONTRIBUTING.md
//! «Правило зависимостей»). Единственное знание о Win32 — задокументированная
//! строка-константа класса окна панели задач.

use uuid::Uuid;

use crate::model::{OverlapRule, Rect, VisibilityMode};

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
        // Зеркальное правило: окклюдеры — ровно перечисленные окна.
        VisibilityMode::OverlapDenylist => rules.iter().any(|r| rule_matches(r, window)),
    }
}

/// Правило матчит окно, если совпадает `process_name` ИЛИ `title_pattern`.
/// Пустое правило (оба `None`) не матчит ничего — окно остаётся окклюдером.
///
/// `pub`: панель выбора окон (docs/M4_WINDOW_PICKER_DESIGN.md §2.1) отвечает
/// «выбран ли чекбокс строки/процесса» ровно этим предикатом, которым маска
/// решает про окклюдера, — иначе панель и маска могли бы разойтись во мнениях.
pub fn rule_matches(rule: &OverlapRule, window: &OccluderCandidate) -> bool {
    rule_matches_strs(rule, window.exe_path.as_deref(), &window.title)
}

/// Правило матчит пару (process_name, title) — общая часть [`rule_matches`]
/// и [`is_denylisted`]: `process_name` ИЛИ `title_pattern`, тем же
/// wildcard/регистронезависимым сравнением. Пустое правило (оба `None`) не
/// матчит ничего.
fn rule_matches_strs(rule: &OverlapRule, process_name: Option<&str>, title: &str) -> bool {
    let by_path = match (&rule.process_name, process_name) {
        (Some(rule_name), Some(name)) => path_eq_ignore_case(rule_name, name),
        _ => false,
    };
    let by_title = match &rule.title_pattern {
        Some(pattern) => wildcard_match(pattern, title),
        None => false,
    };
    by_path || by_title
}

/// Окно попадает под денй-лист закрепления (SPEC.md, «Закрепление окна»),
/// если хотя бы одно правило матчит его процесс или заголовок — тот же
/// предикат, что [`rule_matches`], но над сырыми `(process_name, title)`
/// вместо [`OccluderCandidate`]: хоткей-пин и список выбора окна в режиме
/// редактирования получают от Win32 только эти два значения. Пустой
/// денй-лист не матчит ничего.
pub fn is_denylisted(
    process_name: Option<&str>,
    title: Option<&str>,
    denylist: &[OverlapRule],
) -> bool {
    any_rule_matches(process_name, title, denylist)
}

/// Хотя бы одно правило матчит окно `(process_name, title)` — общий предикат
/// над сырой парой значений, которую отдаёт Win32.
///
/// Отдельное имя рядом с [`is_denylisted`] не дублирование, а разделение
/// смыслов: денй-лист ЗАПРЕЩАЕТ закрепление, а те же по форме правила
/// «показывать только на этих окнах» ([`crate::pinned_window::host_action`])
/// наоборот РАЗРЕШАЮТ показ. Одна реализация, два читаемых вызова.
pub fn any_rule_matches(
    process_name: Option<&str>,
    title: Option<&str>,
    rules: &[OverlapRule],
) -> bool {
    rules
        .iter()
        .any(|rule| rule_matches_strs(rule, process_name, title.unwrap_or("")))
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

/// `base` минус объединение `holes` — список непересекающихся прямоугольников,
/// покрывающих ровно ту часть `base`, которую не закрывает ни один из `holes`
/// (живой репорт пользователя: маска резала стикер по прямоугольнику окна,
/// даже когда это окно физически не видно — перекрыто чем-то другим или
/// просто не в фокусе; `occluder_rects_for` в `overlay_manager.rs` вызывает
/// это для каждого окна-окклюдера с `holes` = окна, стоящие выше него в
/// z-order, чтобы прятать стикер только там, где окклюдер реально виден).
/// Каждая дыра режет накопленные куски на «бублик» из до 4 полос (верх/низ/
/// лево/право вокруг пересечения) — классический алгоритм вычитания
/// прямоугольников, без пересечений и без дыр между кусками.
pub fn subtract_rects(base: Rect, holes: &[Rect]) -> Vec<Rect> {
    let mut pieces = vec![base];
    for hole in holes {
        if pieces.is_empty() {
            break;
        }
        pieces = pieces
            .into_iter()
            .flat_map(|p| subtract_one(p, hole))
            .collect();
    }
    pieces
}

/// `r` минус `hole` — до 4 непересекающихся прямоугольников. Пустой
/// `Vec` — `hole` полностью покрывает `r`.
fn subtract_one(r: Rect, hole: &Rect) -> Vec<Rect> {
    let (rx0, ry0) = (r.x, r.y);
    let (rx1, ry1) = (
        r.x.saturating_add(r.w as i32),
        r.y.saturating_add(r.h as i32),
    );
    let (hx0, hy0) = (hole.x, hole.y);
    let (hx1, hy1) = (
        hole.x.saturating_add(hole.w as i32),
        hole.y.saturating_add(hole.h as i32),
    );

    let ix0 = rx0.max(hx0);
    let iy0 = ry0.max(hy0);
    let ix1 = rx1.min(hx1);
    let iy1 = ry1.min(hy1);
    if ix1 <= ix0 || iy1 <= iy0 {
        return vec![r]; // не пересекаются — r остаётся целиком
    }

    let mut out = Vec::with_capacity(4);
    // Верхняя полоса: вся ширина r, от верха r до верха пересечения.
    if iy0 > ry0 {
        out.push(Rect {
            x: rx0,
            y: ry0,
            w: r.w,
            h: (iy0 - ry0) as u32,
        });
    }
    // Нижняя полоса: вся ширина r, от низа пересечения до низа r.
    if iy1 < ry1 {
        out.push(Rect {
            x: rx0,
            y: iy1,
            w: r.w,
            h: (ry1 - iy1) as u32,
        });
    }
    // Левая и правая полосы — только в вертикальной полосе пересечения
    // [iy0, iy1), чтобы не задваивать углы с верхней/нижней полосой.
    if ix0 > rx0 {
        out.push(Rect {
            x: rx0,
            y: iy0,
            w: (ix0 - rx0) as u32,
            h: (iy1 - iy0) as u32,
        });
    }
    if ix1 < rx1 {
        out.push(Rect {
            x: ix1,
            y: iy0,
            w: (rx1 - ix1) as u32,
            h: (iy1 - iy0) as u32,
        });
    }
    out
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

    /// «Все, кроме»: окклюдеры — ровно перечисленные окна, всё остальное
    /// стикер перекрывает, включая приложения, о которых правило ничего не
    /// знает (в этом весь смысл — оно и не должно их знать).
    #[test]
    fn denylist_occludes_only_the_listed_windows() {
        let listed = candidate(Some("chrome.exe"), "t", "c");
        let other = candidate(Some("notepad.exe"), "t", "c");
        let rules = [OverlapRule {
            process_name: Some("chrome.exe".to_string()),
            title_pattern: None,
        }];
        assert!(is_occluder(
            &listed,
            VisibilityMode::OverlapDenylist,
            &rules,
            false
        ));
        assert!(!is_occluder(
            &other,
            VisibilityMode::OverlapDenylist,
            &rules,
            false
        ));
    }

    /// Пустой список исключений — тождественно `Always`: никто не окклюдер.
    #[test]
    fn denylist_without_rules_occludes_nothing() {
        let w = candidate(Some("chrome.exe"), "t", "c");
        assert!(!is_occluder(
            &w,
            VisibilityMode::OverlapDenylist,
            &[],
            false
        ));
    }

    /// Глобальная галочка «никогда не перекрывать панель задач» сильнее
    /// списка исключений — как и в allow-list (SPEC §4.3).
    #[test]
    fn denylist_still_respects_never_overlap_taskbar() {
        let taskbar = candidate(Some("explorer.exe"), "", TASKBAR_WINDOW_CLASS);
        assert!(is_occluder(
            &taskbar,
            VisibilityMode::OverlapDenylist,
            &[],
            true
        ));
        assert!(!is_occluder(
            &taskbar,
            VisibilityMode::OverlapDenylist,
            &[],
            false
        ));
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

    // --- is_denylisted (денй-лист закрепления окон) ---

    #[test]
    fn denylist_matches_by_process_name_only() {
        let rules = [rule(Some("chrome.exe"), None)];
        assert!(is_denylisted(Some("chrome.exe"), Some("t"), &rules));
        assert!(!is_denylisted(Some("other.exe"), Some("t"), &rules));
    }

    #[test]
    fn denylist_matches_by_title_wildcard() {
        let rules = [rule(None, Some("*YouTube*"))];
        assert!(is_denylisted(None, Some("YouTube - Chrome"), &rules));
        assert!(!is_denylisted(None, Some("Безымянный - Notepad"), &rules));
    }

    #[test]
    fn denylist_empty_never_matches() {
        assert!(!is_denylisted(Some("chrome.exe"), Some("t"), &[]));
        assert!(!is_denylisted(None, None, &[]));
    }

    #[test]
    fn denylist_non_matching_process_does_not_match() {
        let rules = [rule(Some("obs64.exe"), Some("OBS *"))];
        assert!(!is_denylisted(Some("chrome.exe"), Some("Chrome"), &rules));
    }

    #[test]
    fn denylist_matches_any_rule_not_just_first() {
        // Матчит ТРЕТЬЕ правило — первые два не подходят.
        let rules = [
            rule(Some("no1.exe"), Some("No Match *")),
            rule(Some("no2.exe"), Some("Also No *")),
            rule(Some("target.exe"), None),
        ];
        assert!(is_denylisted(Some("target.exe"), Some("t"), &rules));

        // И наоборот: заголовочное правило позади процессных.
        let rules = [
            rule(Some("no1.exe"), None),
            rule(None, Some("*Target Title*")),
        ];
        assert!(is_denylisted(
            Some("x.exe"),
            Some("My Target Title"),
            &rules
        ));
    }

    #[test]
    fn denylist_process_rule_without_process_name_does_not_match() {
        let rules = [rule(Some("chrome.exe"), None)];
        assert!(!is_denylisted(None, Some("t"), &rules));
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

    // --- subtract_rects (окклюдер виден только там, где его не закрывает
    // окно выше по z-order — живой репорт пользователя) ---

    fn area(rects: &[Rect]) -> i64 {
        rects.iter().map(|r| r.w as i64 * r.h as i64).sum()
    }

    /// Ни один из полученных кусков не пересекается ни с одной дырой —
    /// инвариант, который должен держаться независимо от геометрии.
    fn no_piece_overlaps_any_hole(pieces: &[Rect], holes: &[Rect]) -> bool {
        pieces.iter().all(|p| {
            holes.iter().all(|h| {
                let ix0 = p.x.max(h.x);
                let iy0 = p.y.max(h.y);
                let ix1 = (p.x + p.w as i32).min(h.x + h.w as i32);
                let iy1 = (p.y + p.h as i32).min(h.y + h.h as i32);
                ix1 <= ix0 || iy1 <= iy0
            })
        })
    }

    #[test]
    fn subtract_rects_no_overlap_keeps_base_whole() {
        let base = r(0, 0, 100, 100);
        let hole = r(200, 200, 50, 50);
        assert_eq!(subtract_rects(base, &[hole]), vec![base]);
    }

    #[test]
    fn subtract_rects_hole_fully_covers_base_yields_empty() {
        let base = r(10, 10, 50, 50);
        let hole = r(0, 0, 1000, 1000);
        assert_eq!(subtract_rects(base, &[hole]), Vec::<Rect>::new());
    }

    #[test]
    fn subtract_rects_center_hole_leaves_four_strips_of_correct_total_area() {
        // Окклюдер 100x100 в (0,0); окно выше по z-order — центральный
        // квадрат 20x20 — видимая площадь окклюдера теряет ровно этот кусок.
        let base = r(0, 0, 100, 100);
        let hole = r(40, 40, 20, 20);
        let pieces = subtract_rects(base, &[hole]);
        assert_eq!(area(&pieces), 100 * 100 - 20 * 20);
        assert!(no_piece_overlaps_any_hole(&pieces, &[hole]));
    }

    #[test]
    fn subtract_rects_edge_hole_leaves_two_strips() {
        // Дыра прижата к левому краю на всю высоту — левая/правая полосы
        // вырождаются в одну (левой полосы нет вовсе, только правая), но
        // верх/низ по-прежнему валидны (в данном случае дыра высотой во
        // весь r — значит и верх/низ вырождаются тоже, остаётся одна
        // полоса справа).
        let base = r(0, 0, 100, 100);
        let hole = r(0, 0, 30, 100);
        assert_eq!(subtract_rects(base, &[hole]), vec![r(30, 0, 70, 100)]);
    }

    #[test]
    fn subtract_rects_multiple_holes_accumulate() {
        // Живой сценарий: окклюдер частично закрыт ДВУМЯ окнами, стоящими
        // выше него в z-order (например, два маленьких окна поверх одного
        // большого).
        let base = r(0, 0, 100, 100);
        let holes = [r(0, 0, 30, 30), r(70, 70, 30, 30)];
        let pieces = subtract_rects(base, &holes);
        assert_eq!(area(&pieces), 100 * 100 - 30 * 30 - 30 * 30);
        assert!(no_piece_overlaps_any_hole(&pieces, &holes));
    }

    #[test]
    fn subtract_rects_disjoint_pieces_never_overlap_each_other() {
        let base = r(0, 0, 100, 100);
        let holes = [r(20, 0, 10, 100), r(60, 0, 10, 100)];
        let pieces = subtract_rects(base, &holes);
        for i in 0..pieces.len() {
            for j in (i + 1)..pieces.len() {
                let a = pieces[i];
                let b = pieces[j];
                let ix0 = a.x.max(b.x);
                let iy0 = a.y.max(b.y);
                let ix1 = (a.x + a.w as i32).min(b.x + b.w as i32);
                let iy1 = (a.y + a.h as i32).min(b.y + b.h as i32);
                assert!(ix1 <= ix0 || iy1 <= iy0, "куски {a:?} и {b:?} пересекаются");
            }
        }
    }
}
