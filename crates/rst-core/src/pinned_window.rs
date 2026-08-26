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

/// Где закреплённому окну разрешено показываться.
///
/// Двух состояний мало: «правил нет» и «список пуст» — разные вещи, и
/// именно их слияние ломало кнопку «Снять все» в выборе окон-хозяев (репорт
/// пользователя 2026-08-22). Снятие всех галочек давало пустой список,
/// пустой список читался как «ограничений нет», и панель тут же
/// перерисовывалась со всеми галочками на месте — кнопка выглядела
/// сломанной.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum HostFilter {
    /// Ограничений нет: обычное закрепление поверх всего
    /// ([`is_full_topmost`]) — состояние свежего пина.
    #[default]
    Anywhere,
    /// Показывать ТОЛЬКО поверх окон, подходящих под эти правила.
    ///
    /// Пустой список — валидное состояние «ни на одном окне»: окно остаётся
    /// свёрнутым, пока пользователь не вызовет его сам (Alt+Tab, панель
    /// задач) — [`host_action`] всегда уступает явному выбору человека,
    /// поэтому запереть окно этим состоянием нельзя.
    Only(Vec<OverlapRule>),
}

impl HostFilter {
    /// Правила для сопоставления; у [`HostFilter::Anywhere`] их нет —
    /// пустой срез тут значит «сопоставлять нечего», а не «нигде»: ветки
    /// различает [`Self::is_restricted`].
    pub fn rules(&self) -> &[OverlapRule] {
        match self {
            Self::Anywhere => &[],
            Self::Only(rules) => rules,
        }
    }

    /// Видимость окна ограничена списком хозяев.
    pub fn is_restricted(&self) -> bool {
        matches!(self, Self::Only(_))
    }

    /// Правила на изменение; правка списка сама по себе означает, что
    /// ограничение включено, поэтому [`HostFilter::Anywhere`] переходит в
    /// пустой [`HostFilter::Only`].
    pub fn rules_mut(&mut self) -> &mut Vec<OverlapRule> {
        if let Self::Anywhere = self {
            *self = Self::Only(Vec::new());
        }
        match self {
            Self::Only(rules) => rules,
            Self::Anywhere => unreachable!("переведено в Only строкой выше"),
        }
    }
}

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
    /// пользователя 2026-08-22). [`HostFilter::Anywhere`] — обычное
    /// закрепление поверх всего ([`is_full_topmost`]). В режиме
    /// [`HostFilter::Only`] окно живёт по [`host_action`]: активен хозяин —
    /// окно видно и лежит поверх него; активно что угодно другое (включая
    /// рабочий стол) — окно свёрнуто.
    ///
    /// Правило то же по форме, что у денй-листа и окклюдеров
    /// ([`OverlapRule`]): процесс ИЛИ шаблон заголовка. По клику в списке
    /// окон создаётся правило ПО ПРОЦЕССУ — оно переживает перезапуск
    /// приложения и смену заголовка (браузер меняет заголовок на каждой
    /// вкладке).
    pub hosts: HostFilter,
    /// С тех пор как окно свёрнуто, пользователь успел побывать не на
    /// хозяине — см. [`HostContext::away_from_hosts`]. Рантайм-состояние,
    /// ведёт его координатор на каждом снимке.
    pub away_from_hosts: bool,
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
            hosts: HostFilter::Anywhere,
            away_from_hosts: false,
            hidden_by_rules: false,
        }
    }
}

/// Full-topmost-режим: нет правил соседей — окно держится через
/// `WS_EX_TOPMOST`; непустой список — z-order-слот над соседями вместо
/// него. Вопрос «какой из двух режимов» решается одним этим предикатом.
pub fn is_full_topmost(pinned: &PinnedWindow) -> bool {
    !pinned.hosts.is_restricted()
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
    /// Где окну разрешено показываться ([`PinnedWindow::hosts`]).
    pub hosts: &'a HostFilter,
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
    /// С тех пор как окно оказалось свёрнутым, пользователь успел побывать
    /// НЕ на хозяине (другое приложение, рабочий стол).
    ///
    /// По этому признаку возвращается окно, свёрнутое пользователем вручную
    /// (запрос 2026-08-22: «свернул окно, потом снова переключился на
    /// программу, где оно закреплено, — оно появляется»). Простого «хозяин
    /// активен» тут мало: после нажатия «Свернуть» фокус падает как раз на
    /// хозяина, и окно всплывало бы обратно в ту же долю секунды — кнопку
    /// «Свернуть» стало бы невозможно нажать.
    ///
    /// Раньше здесь стоял фронт «фокус только что перешёл на хозяина», и он
    /// терялся: между сворачиванием и возвратом приходит несколько снимков
    /// (переключатель Alt+Tab, панель задач), любой из них съедал фронт, и
    /// окно не возвращалось (репорт пользователя). Состояние «успел уйти»
    /// от порядка и числа снимков не зависит.
    pub away_from_hosts: bool,
    /// Переднее окно лежит на ДРУГОМ мониторе, чем закреплённое.
    ///
    /// Переключение на соседнем экране не должно гасить окно на этом
    /// (запрос пользователя 2026-08-22: браузер с закреплённым Проводником
    /// на первом мониторе, Discord — на втором). Формально пользователь
    /// ушёл на постороннее окно, но на мониторе закреплённого окна ничего
    /// не изменилось: хозяин там как лежал, так и лежит, и убирать окно с
    /// экрана, на который человек даже не смотрел, — потеря информации без
    /// причины.
    ///
    /// `false`, когда монитор неизвестен (переднего окна нет — рабочий
    /// стол): неизвестность не повод отменять правило.
    pub foreground_elsewhere: bool,
    /// Пользователь прямо сейчас переключается между окнами средствами
    /// шелла (Alt+Tab, Win+Tab, меню Пуск, панель задач) — «активного
    /// приложения» в этот момент фактически нет, он ещё выбирает.
    pub shell_switching: bool,
}

/// Должно ли окно быть видно прямо сейчас: активен хозяин или само окно.
///
/// Отдельная функция, потому что этот же вопрос задаёт координатор, чтобы
/// вести [`HostContext::away_from_hosts`] — иначе признак «успел уйти»
/// считался бы по своей копии правил и разошёлся бы с решением.
pub fn host_is_active(ctx: &HostContext) -> bool {
    match ctx.hosts {
        HostFilter::Anywhere => true,
        HostFilter::Only(rules) => wants_visible(ctx, rules),
    }
}

/// Внутренняя часть: пустой список хозяев не совпадает ни с чем — окно
/// показывается только по явному вызову пользователем.
fn wants_visible(ctx: &HostContext, rules: &[OverlapRule]) -> bool {
    ctx.foreground_is_target
        || crate::occluders::any_rule_matches(ctx.foreground_process, ctx.foreground_title, rules)
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
/// * **Возвращаем и то, что свернул пользователь, — но только если он
///   успел уйти с хозяев и вернуться** (`away_from_hosts`, запрос
///   2026-08-22). Пока пользователь не уходил, свёрнутое руками окно не
///   трогаем: иначе свернуть его при активном хозяине было бы невозможно.
/// * **Пользователь всегда главнее правил.** Как только он сам вызвал окно
///   (`foreground_is_target`), оно показывается, даже если по правилам
///   должно быть скрыто; спрячется снова, когда он уйдёт на другое окно.
pub fn host_action(ctx: &HostContext) -> HostAction {
    let HostFilter::Only(rules) = ctx.hosts else {
        return HostAction::None; // обычное закрепление поверх всего
    };
    if ctx.shell_switching {
        // Идёт Alt+Tab/Win+Tab/меню Пуск: активен переключатель шелла, а не
        // приложение. Свернуть или развернуть окно сейчас — значит менять
        // список прямо под рукой пользователя; переключатель от этого
        // ломается целиком (критический репорт 2026-08-22). Ждём, пока
        // выбор закончится: следующий же снимок решит по настоящему окну.
        return HostAction::None;
    }
    let wanted_visible = wants_visible(ctx, rules);
    if wanted_visible {
        // Своё сокрытие разворачиваем всегда; сворачивание пользователем —
        // когда он успел уйти с хозяев и вернуться (см. `away_from_hosts`).
        if ctx.target_minimized && (ctx.hidden_by_rules || ctx.away_from_hosts) {
            HostAction::Show
        } else {
            HostAction::None
        }
    } else if ctx.foreground_elsewhere {
        // Ушли на другой монитор — этого экрана переключение не касается
        // (см. `HostContext::foreground_elsewhere`).
        HostAction::None
    } else if ctx.target_minimized {
        HostAction::None // уже убрано — неважно, кем
    } else {
        HostAction::Hide
    }
}

/// Наибольший отступ, на который можно ужать окно внутри его снап-зоны,
/// проценты (запрос пользователя 2026-08-25: «поставить процентное
/// соотношение ДО 35%»). Потолок не косметический: зона половины экрана,
/// ужатая сильнее, перестаёт вмещать полезное содержимое, а смысл функции
/// — зазор вокруг окна, а не превращение половины экрана в марку.
pub const SNAP_SHRINK_MAX_PCT: u8 = 35;

/// Шаг процента в меню трея: 0, 5, …, 35 — восемь пунктов.
///
/// Меню трея не имеет ползунка (это `HMENU`, пункты дискретны), поэтому шаг
/// задаёт и сам набор пунктов, и цену промаха: 5% на половине FullHD — это
/// 48 px, заметно, но не грубо.
pub const SNAP_SHRINK_STEP_PCT: u8 = 5;

/// Допуск сопоставления окна со снап-зоной, физические пиксели.
///
/// Windows делит рабочую область пополам с округлением (1921 → 960 + 961),
/// а у части окон DWM-границы отличаются от расчётных на пиксель-другой из-за
/// собственных ограничений минимального размера. Допуск заведомо меньше
/// половины расстояния между соседними кандидатами (треть экрана против
/// половины — сотни пикселей), поэтому перепутать зоны он не может.
pub const SNAP_ZONE_EPS_PX: f64 = 6.0;

/// Наименьшая сторона окна после ужатия, физические пиксели: страховка от
/// вырожденной зоны (монитор-«полоска», рабочая область в пару десятков
/// пикселей). При штатных размерах не срабатывает никогда.
const SNAP_SHRINK_MIN_PX: f64 = 120.0;

/// Все зоны, в которые Windows кладёт окно при снапе рабочей области `work`.
///
/// Публичного способа спросить «это окно в снапе?» у Windows нет: снап — это
/// поведение оболочки, а не свойство окна, и `GetWindowPlacement` о нём
/// молчит. Поэтому зона распознаётся по геометрии — совпадением границ с
/// одним из кандидатов ниже.
///
/// Состав повторяет раскладки Windows 10/11: половины по обеим осям,
/// четверти и колонки-трети (Snap Layouts на широком мониторе, включая
/// «две трети + треть»). Рабочая область целиком в список НЕ входит:
/// это развёрнутое окно, у него своё обращение в координаторе
/// (`is_window_maximized`), и спорить с ним отступом значило бы драться с
/// самим `ShowWindow(SW_MAXIMIZE)`.
pub fn snap_zones(work: PxRect) -> Vec<PxRect> {
    let (x, y, w, h) = (work.left, work.top, work.w(), work.h());
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let (hw, hh) = (w / 2.0, h / 2.0);
    let third = w / 3.0;
    vec![
        // Половины.
        PxRect::from_xywh(x, y, hw, h),
        PxRect::from_xywh(x + hw, y, w - hw, h),
        PxRect::from_xywh(x, y, w, hh),
        PxRect::from_xywh(x, y + hh, w, h - hh),
        // Четверти.
        PxRect::from_xywh(x, y, hw, hh),
        PxRect::from_xywh(x + hw, y, w - hw, hh),
        PxRect::from_xywh(x, y + hh, hw, h - hh),
        PxRect::from_xywh(x + hw, y + hh, w - hw, h - hh),
        // Колонки-трети.
        PxRect::from_xywh(x, y, third, h),
        PxRect::from_xywh(x + third, y, third, h),
        PxRect::from_xywh(x + 2.0 * third, y, w - 2.0 * third, h),
        // Две трети слева и справа.
        PxRect::from_xywh(x, y, 2.0 * third, h),
        PxRect::from_xywh(x + third, y, w - third, h),
    ]
}

/// Снап-зона, которую занимает `rect`, — или `None`, если окно стоит само
/// по себе.
///
/// Совпадать обязаны все четыре кромки: окно ровно в половину ширины, но
/// сдвинутое по вертикали, снапом не является, и трогать его нельзя.
pub fn snap_zone_of(rect: PxRect, work: PxRect, eps: f64) -> Option<PxRect> {
    snap_zones(work)
        .into_iter()
        .find(|zone| edges_within(rect, *zone, eps))
}

/// Ужать зону на `pct` процентов ПО КАЖДОЙ СТОРОНЕ, оставив окно по центру
/// зоны: зазор появляется со всех четырёх сторон одинаковый.
///
/// Проценты режутся о [`SNAP_SHRINK_MAX_PCT`] здесь, а не только в UI:
/// значение приходит из config.json, который пользователь правит руками.
pub fn shrink_in_zone(zone: PxRect, pct: u8) -> PxRect {
    let k = 1.0 - f64::from(pct.min(SNAP_SHRINK_MAX_PCT)) / 100.0;
    let w = (zone.w() * k).max(SNAP_SHRINK_MIN_PX.min(zone.w()));
    let h = (zone.h() * k).max(SNAP_SHRINK_MIN_PX.min(zone.h()));
    PxRect::from_xywh(
        zone.left + (zone.w() - w) / 2.0,
        zone.top + (zone.h() - h) / 2.0,
        w,
        h,
    )
}

/// `rect` — это результат нашего же ужатия зоны `zone`?
///
/// Нужно, чтобы отступ пережил собственное применение. Ужатое окно больше не
/// совпадает ни с одной снап-зоной, и без этой проверки следующий же снимок
/// счёл бы его «просто окном» и забыл зону — отступ действовал бы ровно один
/// кадр, а смена процента вообще не доходила бы до окна.
///
/// Признак — центр в центре зоны и рамка внутри зоны. Конкретный процент не
/// проверяется намеренно: тогда смена значения в меню перестала бы узнавать
/// окно, ужатое предыдущим значением.
pub fn is_shrunk_in_zone(rect: PxRect, zone: PxRect, eps: f64) -> bool {
    let centered = ((rect.left + rect.right) - (zone.left + zone.right)).abs() <= 2.0 * eps
        && ((rect.top + rect.bottom) - (zone.top + zone.bottom)).abs() <= 2.0 * eps;
    let inside = rect.left >= zone.left - eps
        && rect.top >= zone.top - eps
        && rect.right <= zone.right + eps
        && rect.bottom <= zone.bottom + eps;
    centered && inside
}

/// Все четыре кромки совпадают с точностью до `eps`.
fn edges_within(a: PxRect, b: PxRect, eps: f64) -> bool {
    (a.left - b.left).abs() <= eps
        && (a.top - b.top).abs() <= eps
        && (a.right - b.right).abs() <= eps
        && (a.bottom - b.bottom).abs() <= eps
}

#[cfg(test)]
mod tests {

    /// Рабочая область условного FullHD с панелью задач снизу.
    fn work() -> PxRect {
        PxRect::from_xywh(0.0, 0.0, 1920.0, 1032.0)
    }

    #[test]
    fn snap_zone_recognises_left_half() {
        let left = PxRect::from_xywh(0.0, 0.0, 960.0, 1032.0);
        assert_eq!(
            snap_zone_of(left, work(), SNAP_ZONE_EPS_PX),
            Some(left),
            "левая половина обязана распознаваться"
        );
    }

    #[test]
    fn snap_zone_tolerates_rounding_of_odd_width() {
        // Windows делит 1921 как 960 + 961: окно правой половины начинается
        // не в 960.5, а в 960 или 961 — допуск обязан это пережить.
        let work = PxRect::from_xywh(0.0, 0.0, 1921.0, 1032.0);
        let right = PxRect::from_xywh(961.0, 0.0, 960.0, 1032.0);
        assert!(
            snap_zone_of(right, work, SNAP_ZONE_EPS_PX).is_some(),
            "округление половины не должно ломать распознавание"
        );
    }

    #[test]
    fn snap_zone_recognises_quarter_and_third() {
        let quarter = PxRect::from_xywh(960.0, 516.0, 960.0, 516.0);
        assert!(snap_zone_of(quarter, work(), SNAP_ZONE_EPS_PX).is_some());
        let third = PxRect::from_xywh(640.0, 0.0, 640.0, 1032.0);
        assert!(snap_zone_of(third, work(), SNAP_ZONE_EPS_PX).is_some());
    }

    #[test]
    fn snap_zone_rejects_free_window() {
        // Ширина как у половины, но окно сдвинуто по вертикали — не снап.
        let free = PxRect::from_xywh(0.0, 120.0, 960.0, 700.0);
        assert_eq!(snap_zone_of(free, work(), SNAP_ZONE_EPS_PX), None);
    }

    #[test]
    fn snap_zone_ignores_whole_work_area() {
        // Развёрнутое окно — не снап-зона: им занимается отдельная ветка
        // координатора, отступ туда лезть не должен.
        assert_eq!(snap_zone_of(work(), work(), SNAP_ZONE_EPS_PX), None);
    }

    #[test]
    fn shrink_keeps_window_centred_in_zone() {
        let zone = PxRect::from_xywh(0.0, 0.0, 1000.0, 800.0);
        let got = shrink_in_zone(zone, 20);
        assert_eq!(got.w(), 800.0);
        assert_eq!(got.h(), 640.0);
        // Зазор одинаковый со всех сторон.
        assert_eq!(got.left - zone.left, zone.right - got.right);
        assert_eq!(got.top - zone.top, zone.bottom - got.bottom);
    }

    #[test]
    fn shrink_is_capped_at_max_pct() {
        let zone = PxRect::from_xywh(0.0, 0.0, 1000.0, 1000.0);
        // Значение из руками правленого config.json потолок обязан срезать.
        assert_eq!(
            shrink_in_zone(zone, 90),
            shrink_in_zone(zone, SNAP_SHRINK_MAX_PCT)
        );
    }

    #[test]
    fn shrink_by_zero_returns_the_zone() {
        let zone = PxRect::from_xywh(10.0, 20.0, 900.0, 700.0);
        assert_eq!(shrink_in_zone(zone, 0), zone);
    }

    #[test]
    fn shrunk_window_is_recognised_as_belonging_to_its_zone() {
        let zone = PxRect::from_xywh(0.0, 0.0, 960.0, 1032.0);
        for pct in [5, 15, 35] {
            let shrunk = shrink_in_zone(zone, pct);
            assert!(
                is_shrunk_in_zone(shrunk, zone, SNAP_ZONE_EPS_PX),
                "ужатое на {pct}% окно обязано узнаваться в своей зоне"
            );
        }
    }

    #[test]
    fn window_dragged_out_of_zone_is_not_recognised() {
        let zone = PxRect::from_xywh(0.0, 0.0, 960.0, 1032.0);
        let shrunk = shrink_in_zone(zone, 20);
        // Пользователь утащил окно вправо: центр уехал — зона больше не наша.
        let moved = PxRect::from_xywh(shrunk.left + 300.0, shrunk.top, shrunk.w(), shrunk.h());
        assert!(!is_shrunk_in_zone(moved, zone, SNAP_ZONE_EPS_PX));
    }

    #[test]
    fn shrink_never_collapses_a_degenerate_zone() {
        // Вырожденная рабочая область (гонка переподключения монитора):
        // окно обязано остаться не шире зоны и не выродиться в ноль.
        let zone = PxRect::from_xywh(0.0, 0.0, 40.0, 30.0);
        let got = shrink_in_zone(zone, SNAP_SHRINK_MAX_PCT);
        assert_eq!((got.w(), got.h()), (40.0, 30.0));
    }
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

    fn only(processes: &[&str]) -> HostFilter {
        HostFilter::Only(processes.iter().map(|p| rule_for(p)).collect())
    }

    fn ctx<'a>(hosts: &'a HostFilter, fg: Option<&'a str>) -> HostContext<'a> {
        HostContext {
            hosts,
            foreground_is_target: false,
            foreground_process: fg,
            foreground_title: None,
            target_minimized: false,
            hidden_by_rules: false,
            away_from_hosts: false,
            foreground_elsewhere: false,
            shell_switching: false,
        }
    }

    /// Пока пользователь переключается через Alt+Tab, окно не трогаем
    /// вовсе: активен переключатель шелла, а не приложение (критический
    /// репорт 2026-08-22 — иначе ломается сам Alt+Tab).
    #[test]
    fn host_action_freezes_while_shell_is_switching() {
        let rules = only(&["chrome.exe"]);
        let mut c = ctx(&rules, Some("notepad.exe"));
        c.shell_switching = true;
        assert_eq!(
            host_action(&c),
            HostAction::None,
            "не прячем во время Alt+Tab"
        );

        // И не разворачиваем: список переключателя не должен меняться.
        c.foreground_process = Some("chrome.exe");
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::None, "и не показываем");

        // Переключение закончилось — решение принимается как обычно.
        c.shell_switching = false;
        assert_eq!(host_action(&c), HostAction::Show);
    }

    /// Пока активно системное всплывающее меню снап-раскладок Windows 11 (Snap Layouts),
    /// закреплённые окна не должны прятаться или менять z-order (репорт 2026-08-26).
    #[test]
    fn host_action_freezes_during_snap_layouts_flyout() {
        let rules = only(&["explorer.exe"]);
        let mut c = ctx(&rules, Some("explorer.exe"));
        c.shell_switching = true;
        assert_eq!(
            host_action(&c),
            HostAction::None,
            "не трогаем окно при показе Snap Layouts"
        );

        let mut c2 = ctx(&rules, Some("notepad.exe"));
        c2.shell_switching = true;
        assert_eq!(
            host_action(&c2),
            HostAction::None,
            "не скрываем окно при показе Snap Layouts"
        );
    }

    /// Без правил фича молчит: обычное закрепление ведёт себя как раньше.
    #[test]
    fn host_action_is_silent_without_rules() {
        let anywhere = HostFilter::Anywhere;
        assert_eq!(
            host_action(&ctx(&anywhere, Some("chrome.exe"))),
            HostAction::None
        );
    }

    /// Снятые ВСЕ галочки — это «ни на одном окне», а не «ограничений нет»
    /// (репорт 2026-08-22: кнопка «Снять все» выглядела неработающей именно
    /// потому, что пустой список читался как отсутствие правил). Окно при
    /// этом не заперто: пользователь вызывает его сам и видит.
    #[test]
    fn empty_host_list_hides_everywhere_but_yields_to_the_user() {
        let nowhere = HostFilter::Only(Vec::new());
        assert_eq!(
            host_action(&ctx(&nowhere, Some("chrome.exe"))),
            HostAction::Hide
        );
        assert_eq!(host_action(&ctx(&nowhere, None)), HostAction::Hide);

        let mut c = ctx(&nowhere, Some("chrome.exe"));
        c.foreground_is_target = true;
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(
            host_action(&c),
            HostAction::Show,
            "вызванное вручную — показываем"
        );
    }

    /// Активен хозяин — окно должно быть видно; активно что-то другое —
    /// свёрнуто. Ровно пример пользователя: Проводник на браузере.
    #[test]
    fn host_action_follows_foreground_window() {
        let rules = only(&["chrome.exe"]);
        // Хозяин активен, окно уже видно — трогать нечего.
        assert_eq!(
            host_action(&ctx(&rules, Some("chrome.exe"))),
            HostAction::None
        );
        // Активен чужой процесс — прячем.
        assert_eq!(host_action(&ctx(&rules, Some("vlc.exe"))), HostAction::Hide);
        // Рабочий стол (переднего окна нет) — тоже прячем.
        assert_eq!(host_action(&ctx(&rules, None)), HostAction::Hide);
    }

    /// Хозяин снова активен — возвращаем окно, которое прятали сами.
    #[test]
    fn host_action_restores_only_what_it_hid() {
        let rules = only(&["chrome.exe"]);
        let mut c = ctx(&rules, Some("chrome.exe"));
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::Show);

        // То же окно, свёрнутое ПОЛЬЗОВАТЕЛЕМ, при уже активном хозяине:
        // не спорим с ним, иначе кнопку «Свернуть» было бы не нажать.
        c.hidden_by_rules = false;
        assert_eq!(host_action(&c), HostAction::None);
    }

    /// Свернул окно руками, ушёл на другое приложение, вернулся на
    /// хозяина — окно возвращается (запрос пользователя 2026-08-22).
    #[test]
    fn host_action_restores_user_minimized_window_after_leaving_hosts() {
        let rules = only(&["chrome.exe"]);
        let mut c = ctx(&rules, Some("chrome.exe"));
        c.target_minimized = true;
        c.hidden_by_rules = false;

        // Сразу после сворачивания фокус падает на того же хозяина —
        // возвращать нельзя, иначе кнопка «Свернуть» не работает.
        c.away_from_hosts = false;
        assert_eq!(host_action(&c), HostAction::None);

        // Пользователь успел уйти на постороннее окно и вернулся.
        c.away_from_hosts = true;
        assert_eq!(host_action(&c), HostAction::Show);
    }

    /// Признак «активен хозяин» — тот же, по которому принимается решение:
    /// координатор ведёт им состояние ухода.
    #[test]
    fn host_is_active_matches_decision() {
        let rules = only(&["chrome.exe"]);
        assert!(host_is_active(&ctx(&rules, Some("chrome.exe"))));
        assert!(!host_is_active(&ctx(&rules, Some("notepad.exe"))));
        assert!(!host_is_active(&ctx(&rules, None)));

        // Само окно на переднем плане — тоже «активен хозяин»: пользователь
        // вызвал его сам.
        let mut c = ctx(&rules, Some("notepad.exe"));
        c.foreground_is_target = true;
        assert!(host_is_active(&c));

        // Без ограничений вопрос не стоит.
        let anywhere = HostFilter::Anywhere;
        assert!(host_is_active(&ctx(&anywhere, Some("vlc.exe"))));
    }

    /// Пользователь сам вызвал окно (Alt+Tab, панель задач) — показываем,
    /// даже если по правилам его быть не должно.
    #[test]
    fn host_action_yields_to_explicit_user_choice() {
        let rules = only(&["chrome.exe"]);
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
        let rules = only(&["chrome.exe"]);
        let c = ctx(&rules, Some("notepad.exe"));
        assert_eq!(host_action(&c), HostAction::Hide);
    }

    /// Постороннее окно на ДРУГОМ мониторе окно не гасит (запрос
    /// пользователя 2026-08-22): на экране закреплённого окна ничего не
    /// изменилось.
    #[test]
    fn host_action_ignores_switches_on_another_monitor() {
        let rules = only(&["chrome.exe"]);
        let mut c = ctx(&rules, Some("discord.exe"));
        c.foreground_elsewhere = true;
        assert_eq!(host_action(&c), HostAction::None, "не гасим соседний экран");

        // Тот же Discord, но на мониторе закреплённого окна — прячем.
        c.foreground_elsewhere = false;
        assert_eq!(host_action(&c), HostAction::Hide);
    }

    /// Хозяин на другом мониторе — окно всё равно показываем: ограничение
    /// касается только сокрытия, иначе окно нельзя было бы вернуть.
    #[test]
    fn host_action_shows_for_a_host_on_another_monitor() {
        let rules = only(&["chrome.exe"]);
        let mut c = ctx(&rules, Some("chrome.exe"));
        c.foreground_elsewhere = true;
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::Show);
    }

    /// Уже свёрнутое окно повторно не сворачиваем — иначе на каждый снимок
    /// летела бы лишняя команда чужому окну.
    #[test]
    fn host_action_does_not_hide_twice() {
        let rules = only(&["chrome.exe"]);
        let mut c = ctx(&rules, Some("notepad.exe"));
        c.target_minimized = true;
        c.hidden_by_rules = true;
        assert_eq!(host_action(&c), HostAction::None);
    }

    /// Несколько правил: достаточно совпадения с любым.
    #[test]
    fn host_action_accepts_any_of_several_rules() {
        let rules = only(&["chrome.exe", "firefox.exe"]);
        assert_eq!(
            host_action(&ctx(&rules, Some("firefox.exe"))),
            HostAction::None
        );
        assert_eq!(host_action(&ctx(&rules, Some("vlc.exe"))), HostAction::Hide);
    }

    /// Правило по заголовку с маской работает так же, как в денй-листе.
    #[test]
    fn host_action_matches_title_pattern() {
        let rules = HostFilter::Only(vec![OverlapRule {
            process_name: None,
            title_pattern: Some("*YouTube*".to_string()),
        }]);
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
        let rules = only(&["chrome.exe"]);
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
        assert!(!pinned.away_from_hosts);
        assert!(!pinned.hosts.is_restricted());
        assert!(is_full_topmost(&pinned));
    }

    #[test]
    fn host_rules_switch_off_full_topmost() {
        let mut pinned = PinnedWindow::new(1);
        pinned.hosts.rules_mut().push(OverlapRule::default());
        assert!(!is_full_topmost(&pinned));
        // Пустой список — всё ещё ограничение («ни на одном окне»), а не его
        // отсутствие: обратно в full-topmost возвращает только `Anywhere`.
        pinned.hosts.rules_mut().clear();
        assert!(!is_full_topmost(&pinned));
        pinned.hosts = HostFilter::Anywhere;
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
