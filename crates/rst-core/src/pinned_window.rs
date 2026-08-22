//! Рантайм-состояние закреплённых окон (SPEC.md, «Закрепление окна»).
//!
//! В отличие от [`crate::model::Sticker`] `PinnedWindow` сознательно не
//! сериализуется: закрепление — чисто рантайм-механизм, в config.json его
//! нет и быть не должно («нельзя сохранить в пресет, всегда нужно
//! выставлять вручную при каждом запуске»). Единственная персистентная
//! часть фичи — `Settings.denylist` — живёт в [`crate::model`].

use crate::model::OverlapRule;

/// Максимальная доля монитора по каждой оси, которую окно может занимать
/// после закрепления (запрос пользователя 2026-08-19: «закреплённые окна не
/// могли быть больше чем 90% от размера монитора» — возврат клампа,
/// снятого 2026-08-18 при портировании модели PowerToys; теперь снова
/// применяется в `pin_window`, PowerToys-модель «никогда не двигать» для
/// самого topmost остаётся, кламп — только про размер).
const MONITOR_MAX_FRACTION: f64 = 0.9;

/// Закреплённое окно другого приложения: рантайм-состояние, не конфиг.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedWindow {
    /// Дескриптор окна (HWND) как обычное число: крейт платформенно-чистый
    /// (CONTRIBUTING.md, «Правило зависимостей»), конвертация в/из `HWND` —
    /// на границе `rst-win32`.
    pub hwnd: isize,
    /// Замок перемещения: пока включён, окно нельзя сдвинуть никаким
    /// способом (координатор делает реактивный snap-back).
    pub lock_move: bool,
    /// Замок взаимодействия: клики/клавиатура в окно не проходят
    /// (`EnableWindow(hwnd, FALSE)` + визуальный индикатор в координаторе).
    pub lock_interact: bool,
    /// Окна-хозяева: «показывать это окно ТОЛЬКО поверх них» (запрос
    /// пользователя 2026-08-22). Пока список пуст — обычное закрепление
    /// поверх всего ([`is_full_topmost`]). Как только в списке появляется
    /// правило, окно живёт по [`host_action`]: активен хозяин — окно видно
    /// и лежит поверх него; активно что угодно другое (включая рабочий
    /// стол) — окно свёрнуто.
    ///
    /// Правило то же по форме, что у денй-листа и окклюдеров
    /// ([`OverlapRule`]): процесс ИЛИ шаблон заголовка. По клику в списке
    /// окон создаётся правило ПО ПРОЦЕССУ — оно переживает перезапуск
    /// приложения и смену заголовка (браузер меняет заголовок на каждой
    /// вкладке).
    pub host_rules: Vec<OverlapRule>,
    /// Окно сейчас свёрнуто НАМИ, потому что ни один хозяин не активен
    /// ([`host_action`]). Отличает наше сокрытие от «пользователь свернул
    /// окно сам» — второе мы не оспариваем. Рантайм-флаг, как и всё
    /// остальное в этой структуре.
    pub hidden_by_rules: bool,
}

impl PinnedWindow {
    /// Новое закрепление: дефолт — full-topmost, без замков и без соседей.
    pub fn new(hwnd: isize) -> Self {
        Self {
            hwnd,
            lock_move: false,
            lock_interact: false,
            host_rules: Vec::new(),
            hidden_by_rules: false,
        }
    }
}

/// Full-topmost-режим: нет правил соседей — окно держится через
/// `WS_EX_TOPMOST`; непустой список — z-order-слот над соседями вместо
/// него. Вопрос «какой из двух режимов» решается одним этим предикатом.
pub fn is_full_topmost(pinned: &PinnedWindow) -> bool {
    pinned.host_rules.is_empty()
}

/// Кламп размера закрепляемого окна до 90% монитора ПО КАЖДОЙ ОСИ
/// независимо — окно уже fullscreen/больше монитора при закреплении, ужать
/// до лимита, без сохранения пропорций (это не contain-fit-then-shrink из
/// [`crate::sizing::initial_media_size`], у той функции другое правило для
/// другой фичи). Вырожденный вход (нулевой/отрицательный размер)
/// возвращается как есть — тот же защитный паттерн без деления, что в
/// `initial_media_size`.
pub fn clamp_to_monitor_max(w: f64, h: f64, monitor_w: f64, monitor_h: f64) -> (f64, f64) {
    if w <= 0.0 || h <= 0.0 || monitor_w <= 0.0 || monitor_h <= 0.0 {
        return (w, h);
    }
    (
        w.min(monitor_w * MONITOR_MAX_FRACTION),
        h.min(monitor_h * MONITOR_MAX_FRACTION),
    )
}

/// Магнит к кромкам монитора, DIP: ближе этого расстояния кромка окна
/// «прилипает» к кромке монитора (запрос пользователя 2026-08-21: «небольшой
/// магнит на границах экрана… чтобы я мог легко закреплённое окно поставить
/// чётко под угол экрана»). Величина подобрана как у snap-раскладок самой
/// Windows: заметно помогает попасть в угол, но не мешает поставить окно в
/// десятке пикселей от края намеренно.
pub const EDGE_SNAP_DIP: f64 = 12.0;

/// Прямоугольник в физических пикселях виртуального десктопа: `left`/`top`
/// включительно, `right`/`bottom` — исключительно (как `RECT` в Win32).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PxRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl PxRect {
    pub fn from_xywh(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }

    pub fn w(&self) -> f64 {
        self.right - self.left
    }

    pub fn h(&self) -> f64 {
        self.bottom - self.top
    }
}

/// ПЕРЕМЕЩЕНИЕ закреплённого окна в режиме редактирования: примагнитить
/// кромки к кромкам `monitor` и не дать увести окно за пределы `desktop`.
///
/// Размер НЕ меняется — это перемещение: если окно почему-то больше
/// десктопа по оси, по этой оси ограничение не применяется (иначе пришлось
/// бы его ужимать).
///
/// Почему удержание считается по `desktop` (объединяющему прямоугольнику
/// всех мониторов), а магнит — по `monitor` (тому, где идёт жест): удержание
/// решает задачу «окно нельзя утащить туда, откуда его не достать» (запрос
/// пользователя 2026-08-21), и оно не должно мешать перетащить окно на
/// СОСЕДНИЙ монитор; магнит же нужен именно у ближнего края.
pub fn snap_move(rect: PxRect, monitor: PxRect, desktop: PxRect, snap: f64) -> PxRect {
    let (w, h) = (rect.w(), rect.h());
    let mut left = rect.left;
    let mut top = rect.top;

    // Магнит: сначала левая/верхняя кромка, затем правая/нижняя — окно уже
    // ужато до 90% монитора, поэтому обе кромки одной оси одновременно в
    // зону магнита не попадают.
    if (left - monitor.left).abs() <= snap {
        left = monitor.left;
    } else if ((left + w) - monitor.right).abs() <= snap {
        left = monitor.right - w;
    }
    if (top - monitor.top).abs() <= snap {
        top = monitor.top;
    } else if ((top + h) - monitor.bottom).abs() <= snap {
        top = monitor.bottom - h;
    }

    // Удержание в границах десктопа.
    if w <= desktop.w() {
        left = left.clamp(desktop.left, desktop.right - w);
    }
    if h <= desktop.h() {
        top = top.clamp(desktop.top, desktop.bottom - h);
    }
    PxRect::from_xywh(left, top, w, h)
}

/// РЕСАЙЗ закреплённого окна в режиме редактирования: магнит кромок к
/// кромкам `monitor`, запрет вылезать за `monitor` и потолок размера —
/// [`MONITOR_MAX_FRACTION`] от монитора по каждой оси (запрос пользователя
/// 2026-08-21: «запретить расширять закреплённые окна больше чем на 90% от
/// размера монитора» — тот же лимит, что применяется при самом закреплении
/// в [`clamp_to_monitor_max`], теперь и во время ресайза).
///
/// В отличие от [`snap_move`] границей служит МОНИТОР, а не десктоп:
/// растянуть окно на два монитора значит перестать понимать, от какого
/// монитора считать 90 процентов.
///
/// Кромка, которую пользователь не тянет, остаётся на месте: обрезается
/// всегда та сторона, которая вышла за предел. `min_size` не даёт
/// выродиться в ноль при клампе.
pub fn snap_resize(rect: PxRect, monitor: PxRect, snap: f64, min_size: f64) -> PxRect {
    let mut left = rect.left;
    let mut top = rect.top;
    let mut right = rect.right;
    let mut bottom = rect.bottom;

    if (left - monitor.left).abs() <= snap {
        left = monitor.left;
    }
    if (top - monitor.top).abs() <= snap {
        top = monitor.top;
    }
    if (right - monitor.right).abs() <= snap {
        right = monitor.right;
    }
    if (bottom - monitor.bottom).abs() <= snap {
        bottom = monitor.bottom;
    }

    left = left.max(monitor.left);
    top = top.max(monitor.top);
    right = right.min(monitor.right);
    bottom = bottom.min(monitor.bottom);

    let max_w = monitor.w() * MONITOR_MAX_FRACTION;
    let max_h = monitor.h() * MONITOR_MAX_FRACTION;
    if right - left > max_w {
        // Какую кромку двигать: ту, что дальше от исходной — то есть ту,
        // которую пользователь и тянул наружу.
        if (left - rect.left).abs() <= (right - rect.right).abs() {
            right = left + max_w;
        } else {
            left = right - max_w;
        }
    }
    if bottom - top > max_h {
        if (top - rect.top).abs() <= (bottom - rect.bottom).abs() {
            bottom = top + max_h;
        } else {
            top = bottom - max_h;
        }
    }

    if right - left < min_size {
        right = left + min_size;
    }
    if bottom - top < min_size {
        bottom = top + min_size;
    }
    PxRect {
        left,
        top,
        right,
        bottom,
    }
}

/// Что сделать с закреплённым окном, у которого заданы окна-хозяева.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    /// Ничего: состояние окна уже соответствует правилам, либо правил нет,
    /// либо окно свёрнуто самим пользователем — его выбор не оспаривается.
    None,
    /// Развернуть обратно (окно пряталось НАМИ, а сейчас должно быть видно).
    Show,
    /// Свернуть: активного хозяина нет.
    Hide,
}

/// Всё, что нужно знать о моменте, чтобы решить судьбу окна с правилами.
#[derive(Debug, Clone, Copy)]
pub struct HostContext<'a> {
    /// Правила «показывать только на этих окнах» ([`PinnedWindow::host_rules`]).
    pub rules: &'a [OverlapRule],
    /// Переднее окно — это САМО закреплённое окно (пользователь вызвал его
    /// через Alt+Tab или панель задач).
    pub foreground_is_target: bool,
    /// Процесс переднего окна (короткое имя или путь) — `None`, если
    /// переднего окна нет вовсе (рабочий стол) или его не удалось прочитать.
    pub foreground_process: Option<&'a str>,
    /// Заголовок переднего окна.
    pub foreground_title: Option<&'a str>,
    /// Закреплённое окно сейчас свёрнуто.
    pub target_minimized: bool,
    /// Свернули его МЫ по этим правилам (а не пользователь руками).
    pub hidden_by_rules: bool,
}

/// Решение по видимости закреплённого окна с правилами
/// (запрос пользователя 2026-08-22: «показывать только на определённых
/// окнах»).
///
/// Семантика намеренно НЕ геометрическая, в отличие от масок видимости
/// стикеров ([`crate::occluders`]): перекрывает ли окно-хозяин ту область,
/// где лежит закреплённое окно, значения не имеет. Важно ровно одно — какое
/// окно сейчас активно.
///
/// Два правила, которые делают поведение предсказуемым:
/// * **Возвращаем только то, что прятали сами.** Если пользователь свернул
///   окно руками, оно останется свёрнутым, даже когда хозяин активен, —
///   иначе приложение спорило бы с человеком, а он бы не понимал, почему
///   окно всё время всплывает.
/// * **Пользователь всегда главнее правил.** Как только он сам вызвал окно
///   (`foreground_is_target`), оно показывается, даже если по правилам
///   должно быть скрыто; спрячется снова, когда он уйдёт на другое окно.
pub fn host_action(ctx: &HostContext) -> HostAction {
    if ctx.rules.is_empty() {
        return HostAction::None; // обычное закрепление поверх всего
    }
    let wanted_visible = ctx.foreground_is_target
        || crate::occluders::any_rule_matches(
            ctx.foreground_process,
            ctx.foreground_title,
            ctx.rules,
        );
    if wanted_visible {
        // Разворачиваем ТОЛЬКО своё сокрытие: свёрнутое пользователем окно
        // не трогаем.
        if ctx.hidden_by_rules && ctx.target_minimized {
            HostAction::Show
        } else {
            HostAction::None
        }
    } else if ctx.target_minimized {
        HostAction::None // уже убрано — неважно, кем
    } else {
        HostAction::Hide
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    fn mon() -> PxRect {
        PxRect::from_xywh(0.0, 0.0, 1920.0, 1080.0)
    }

    /// Магнит: окно в нескольких пикселях от угла встаёт ровно в угол.
    #[test]
    fn snap_move_pulls_window_into_corner() {
        let out = snap_move(
            PxRect::from_xywh(7.0, 5.0, 800.0, 600.0),
            mon(),
            mon(),
            EDGE_SNAP_DIP,
        );
        assert_eq!(out, PxRect::from_xywh(0.0, 0.0, 800.0, 600.0));

        let out = snap_move(
            PxRect::from_xywh(1920.0 - 800.0 - 6.0, 1080.0 - 600.0 - 3.0, 800.0, 600.0),
            mon(),
            mon(),
            EDGE_SNAP_DIP,
        );
        assert_eq!(
            out,
            PxRect::from_xywh(1920.0 - 800.0, 1080.0 - 600.0, 800.0, 600.0)
        );
    }

    /// За пределами зоны магнита позиция не меняется — намеренный отступ
    /// от края сохраняется.
    #[test]
    fn snap_move_leaves_deliberate_gap_alone() {
        let start = PxRect::from_xywh(40.0, 33.0, 800.0, 600.0);
        assert_eq!(snap_move(start, mon(), mon(), EDGE_SNAP_DIP), start);
    }

    /// Окно нельзя утащить за пределы десктопа (репорт 2026-08-21: «могу
    /// утащить окно далеко за границы монитора… и тогда я его достать не
    /// смогу»). Размер при этом не трогается.
    #[test]
    fn snap_move_keeps_window_on_desktop() {
        let out = snap_move(
            PxRect::from_xywh(-700.0, -400.0, 800.0, 600.0),
            mon(),
            mon(),
            EDGE_SNAP_DIP,
        );
        assert_eq!(out, PxRect::from_xywh(0.0, 0.0, 800.0, 600.0));

        let out = snap_move(
            PxRect::from_xywh(3000.0, 2000.0, 800.0, 600.0),
            mon(),
            mon(),
            EDGE_SNAP_DIP,
        );
        assert_eq!(
            out,
            PxRect::from_xywh(1920.0 - 800.0, 1080.0 - 600.0, 800.0, 600.0)
        );
    }

    /// Второй монитор справа: удержание считается по десктопу, поэтому
    /// перетащить окно на соседний монитор по-прежнему можно.
    #[test]
    fn snap_move_allows_moving_to_neighbour_monitor() {
        let desktop = PxRect::from_xywh(0.0, 0.0, 3840.0, 1080.0);
        let start = PxRect::from_xywh(2400.0, 200.0, 800.0, 600.0);
        assert_eq!(snap_move(start, mon(), desktop, EDGE_SNAP_DIP), start);
    }

    /// Ресайз не даёт перевалить за 90% монитора по каждой оси.
    #[test]
    fn snap_resize_caps_at_ninety_percent() {
        let out = snap_resize(
            PxRect::from_xywh(0.0, 0.0, 1900.0, 1070.0),
            mon(),
            0.0,
            10.0,
        );
        assert_eq!(out.w(), 1920.0 * 0.9);
        assert_eq!(out.h(), 1080.0 * 0.9);
    }

    /// Тянут левую кромку наружу — режется именно она, правая стоит.
    #[test]
    fn snap_resize_trims_the_dragged_edge() {
        let out = snap_resize(
            PxRect {
                left: -400.0,
                top: 100.0,
                right: 1900.0,
                bottom: 700.0,
            },
            mon(),
            0.0,
            10.0,
        );
        assert_eq!(out.right, 1900.0, "правая кромка не должна дёргаться");
        assert_eq!(out.left, 1900.0 - 1920.0 * 0.9);
    }

    /// Магнит работает и на ресайзе — кромку легко посадить точно на край.
    #[test]
    fn snap_resize_snaps_edges_to_monitor() {
        let out = snap_resize(
            PxRect {
                left: 5.0,
                top: 300.0,
                right: 1200.0,
                bottom: 1074.0,
            },
            mon(),
            EDGE_SNAP_DIP,
            10.0,
        );
        assert_eq!(out.left, 0.0);
        assert_eq!(out.bottom, 1080.0);
        assert_eq!(out.right, 1200.0, "дальняя кромка не двигается");
    }


    fn rule_for(process: &str) -> OverlapRule {
        OverlapRule {
            process_name: Some(process.to_string()),
            title_pattern: None,
        }
    }

    fn ctx<'a>(rules: &'a [OverlapRule], fg: Option<&'a str>) -> HostContext<'a> {
        HostContext {
            rules,
            foreground_is_target: false,
            foreground_process: fg,
            foreground_title: None,
            target_minimized: false,
            hidden_by_rules: false,
        }
    }

    /// Без правил фича молчит: обычное закрепление ведёт себя как раньше.
    #[test]
    fn host_action_is_silent_without_rules() {
        let empty: [OverlapRule; 0] = [];
        assert_eq!(host_action(&ctx(&empty, Some("chrome.exe"))), HostAction::None);
    }

    /// Активен хозяин — окно должно быть видно; активно что-то другое —
    /// свёрнуто. Ровно пример пользователя: Проводник на браузере.
    #[test]
    fn host_action_follows_foreground_window() {
        let rules = [rule_for("chrome.exe")];
        // Хозяин активен, окно уже видно — трогать нечего.
        assert_eq!(host_action(&ctx(&rules, Some("chrome.exe"))), HostAction::None);
        // Активен чужой процесс — прячем.
        assert_eq!(host_action(&ctx(&rules, Some("vlc.exe"))), HostAction::Hide);
        // Рабочий стол (переднего окна нет) — тоже прячем.
        assert_eq!(host_action(&ctx(&rules, None)), HostAction::Hide);
    }

    /// Хозяин снова активен — возвращаем окно, которое прятали сами.
    #[test]
    fn host_action_restores_only_what_it_hid() {
        let rules = [rule_for("chrome.exe")];
        let mut c = ctx(&rules, Some("chrome.exe"));
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::Show);

        // То же окно, но свёрнутое ПОЛЬЗОВАТЕЛЕМ: не спорим с ним.
        c.hidden_by_rules = false;
        assert_eq!(host_action(&c), HostAction::None);
    }

    /// Пользователь сам вызвал окно (Alt+Tab, панель задач) — показываем,
    /// даже если по правилам его быть не должно.
    #[test]
    fn host_action_yields_to_explicit_user_choice() {
        let rules = [rule_for("chrome.exe")];
        let mut c = ctx(&rules, Some("explorer.exe"));
        c.foreground_is_target = true;
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::Show);
        // И не прячем, пока он на нём.
        c.target_minimized = false;
        c.hidden_by_rules = false;
        assert_eq!(host_action(&c), HostAction::None);
    }

    /// Ушёл с окна на постороннее — прячем снова.
    #[test]
    fn host_action_hides_again_after_user_leaves() {
        let rules = [rule_for("chrome.exe")];
        let c = ctx(&rules, Some("notepad.exe"));
        assert_eq!(host_action(&c), HostAction::Hide);
    }

    /// Уже свёрнутое окно повторно не сворачиваем — иначе на каждый снимок
    /// летела бы лишняя команда чужому окну.
    #[test]
    fn host_action_does_not_hide_twice() {
        let rules = [rule_for("chrome.exe")];
        let mut c = ctx(&rules, Some("notepad.exe"));
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::None);
    }

    /// Несколько правил: достаточно совпадения с любым.
    #[test]
    fn host_action_accepts_any_of_several_rules() {
        let rules = [rule_for("chrome.exe"), rule_for("firefox.exe")];
        assert_eq!(host_action(&ctx(&rules, Some("firefox.exe"))), HostAction::None);
        assert_eq!(host_action(&ctx(&rules, Some("vlc.exe"))), HostAction::Hide);
    }

    /// Правило по заголовку с маской работает так же, как в денй-листе.
    #[test]
    fn host_action_matches_title_pattern() {
        let rules = [OverlapRule {
            process_name: None,
            title_pattern: Some("*YouTube*".to_string()),
        }];
        let mut c = ctx(&rules, Some("chrome.exe"));
        c.foreground_title = Some("Видео — YouTube — Chrome");
        assert_eq!(host_action(&c), HostAction::None);
        c.foreground_title = Some("Почта — Chrome");
        assert_eq!(host_action(&c), HostAction::Hide);
    }

    /// Полный путь к exe в снимке против короткого имени в правиле —
    /// сопоставление то же, что у денй-листа (без этого правило, созданное
    /// кликом по списку окон, не срабатывало бы вовсе).
    #[test]
    fn host_action_matches_full_exe_path() {
        let rules = [rule_for("chrome.exe")];
        assert_eq!(
            host_action(&ctx(&rules, Some(r"C:\Program Files\Google\chrome.exe"))),
            HostAction::None
        );
    }

    #[test]
    fn new_pinned_window_defaults_full_topmost_unlocked() {
        let pinned = PinnedWindow::new(42);
        assert_eq!(pinned.hwnd, 42);
        assert!(!pinned.lock_move);
        assert!(!pinned.lock_interact);
        assert!(pinned.host_rules.is_empty());
        assert!(is_full_topmost(&pinned));
    }

    #[test]
    fn host_rules_switch_off_full_topmost() {
        let mut pinned = PinnedWindow::new(1);
        pinned.host_rules.push(OverlapRule::default());
        assert!(!is_full_topmost(&pinned));
        pinned.host_rules.clear();
        assert!(is_full_topmost(&pinned));
    }

    #[test]
    fn clamp_within_90_percent_is_unchanged() {
        assert_eq!(
            clamp_to_monitor_max(800.0, 600.0, 1920.0, 1080.0),
            (800.0, 600.0)
        );
    }

    #[test]
    fn clamp_exceeds_width_only_clamps_width_only() {
        assert_eq!(
            clamp_to_monitor_max(2000.0, 500.0, 1920.0, 1080.0),
            (1728.0, 500.0)
        );
    }

    #[test]
    fn clamp_exceeds_height_only_clamps_height_only() {
        assert_eq!(
            clamp_to_monitor_max(500.0, 1500.0, 1920.0, 1080.0),
            (500.0, 972.0)
        );
    }

    #[test]
    fn clamp_exceeds_both_clamps_each_axis_independently() {
        // 3000x2000 на 1920x1080: 90% = 1728x972, каждая ось своим
        // лимитом, пропорции НЕ сохраняются (в отличие от
        // initial_media_size).
        assert_eq!(
            clamp_to_monitor_max(3000.0, 2000.0, 1920.0, 1080.0),
            (1728.0, 972.0)
        );
    }

    #[test]
    fn clamp_exact_90_percent_boundary_is_unchanged() {
        assert_eq!(
            clamp_to_monitor_max(1728.0, 972.0, 1920.0, 1080.0),
            (1728.0, 972.0)
        );
    }

    #[test]
    fn clamp_degenerate_zero_monitor_does_not_panic() {
        assert_eq!(clamp_to_monitor_max(100.0, 100.0, 0.0, 0.0), (100.0, 100.0));
        assert_eq!(
            clamp_to_monitor_max(100.0, 100.0, -5.0, 1080.0),
            (100.0, 100.0)
        );
        assert_eq!(clamp_to_monitor_max(0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0));
    }
}
