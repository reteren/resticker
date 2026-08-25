//! Подсистема тайлинга в координаторе (M9, docs/TILING_DESIGN.md §T2).
//!
//! Здесь сходятся три уже готовых слоя:
//!
//! * `rst_core::tiling` — дерево, раскладка, операции, правила. Чистая логика
//!   без Win32, 139 юнит-тестов.
//! * `rst_win32::window_enum` — снимок живых окон и фильтр [`is_tileable`].
//! * `rst_win32::tiling_apply` — отправка геометрии реальным окнам.
//!
//! Этот файл — их сшивка и ничего больше: он не считает геометрию и не
//! трогает Win32 напрямую. Ради этого он и вынесен из `overlay_manager.rs`
//! (14 с лишним тысяч строк): врезка в главный цикл — несколько строк, а вся
//! механика тайлинга живёт отдельно и тестируется отдельно.
//!
//! ## Как не войти в петлю
//!
//! Наша перестановка окна порождает `EVENT_OBJECT_LOCATIONCHANGE`, трекер
//! через 16 мс присылает новый снимок, координатор пересчитывает раскладку —
//! и если бы пересчёт снова отдавал команду тому же окну, цикл замкнулся бы
//! на 60 Гц. У проекта такой инцидент уже был (см. комментарий про вечный
//! snap-back в `window_pin.rs:119`). Здесь два предохранителя:
//!
//! 1. `reconcile` не выдаёт перестановку окну, которое уже стоит на месте с
//!    точностью до эпсилона. Это основной механизм, и он чистый.
//! 2. Окно, которому команда только что отдана, не получает следующую в
//!    течение [`SETTLE_MS`]. Нужен потому, что чужое приложение доезжает не
//!    мгновенно: пока оно перекладывает вёрстку, снимки показывают
//!    промежуточную геометрию, и без выдержки мы бы долбили его командами.
//!
//! `reconcile::EchoGuard` здесь СОЗНАТЕЛЬНО не используется: он отвечает на
//! вопрос «это окно двигаем мы или пользователь», а этот вопрос возникает
//! только вместе с move-lock'ом для плиток, то есть в T3. Честнее оставить
//! его неподключённым, чем натянуть на задачу, для которой он не нужен.

use std::collections::HashMap;

use rst_core::config::TilingBinding;
use rst_core::hittest::DipRect;
use rst_core::model::{MonitorId, Rect};
use rst_core::tiling::action::Action;
use rst_core::tiling::actions::{self, Outcome};
use rst_core::tiling::binds::{BindTable, Binding, KeyChord, Resolution};
use rst_core::tiling::layout::{self, LayoutParams, Placement};
use rst_core::tiling::policy::{self, InsertPolicy};
use rst_core::tiling::reconcile::{self, Observed, ReconcileParams};
use rst_core::tiling::rules::{WindowFacts, WindowRule};
use rst_core::tiling::switcher::{Entry, Switcher};
use rst_core::tiling::tile_id::{self, StickerTiles};
use rst_core::tiling::tree::WindowKey;
use rst_core::tiling::workspace::WorkspaceSet;
use uuid::Uuid;

use crate::tiling_ui;
use rst_render::{Box2D, Panel, Primitive};
use rst_win32::cloak;
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::keyboard_guard::Chord;
use rst_win32::monitors::MonitorInfo;
use rst_win32::thumb_cache::ThumbCache;
use rst_win32::tiling_apply::{self, TileTarget};
use rst_win32::window_enum::{self, WindowInfo, WindowRect};
use rst_win32::window_pin::{self, WindowPins};

/// Сколько миллисекунд окну даётся на то, чтобы доехать, прежде чем мы
/// отдадим ему следующую команду.
///
/// Взято с запасом относительно дебаунса трекера (16 мс) и типичного времени
/// перекладки тяжёлого приложения. Слишком мало — долбим окно командами и
/// видим дрожание; слишком много — раскладка вяло реагирует на закрытие окна.
const SETTLE_MS: u64 = 250;

/// Допустимое расхождение с целью, px.
///
/// Ноль недопустим: приложения округляют размеры сами (терминалы встают по
/// сетке символов, у многих окон есть минимальный размер), и точного
/// совпадения не будет никогда — с нулём мы бы переставляли такое окно вечно.
const EPSILON_PX: i32 = 2;

/// Сколько заходов подряд окно может не доехать, прежде чем мы признаем его
/// неуправляемым и перестанем трогать.
///
/// Так честно отсекаются elevated-окна (UIPI не пускает нас к ним вовсе,
/// window_pin.rs:855) и окна с минимальным размером больше плитки. Без этого
/// счётчика они бы получали команду на каждом снимке до конца сессии.
const MAX_FAILED_ATTEMPTS: u8 = 3;

/// Сколько окон переставляем за один заход.
const MAX_MOVES_PER_TICK: usize = 16;

/// Сколько снимков окон держим в кэше.
///
/// Переключатель показывает десятки карточек максимум, и лишний запас памяти
/// здесь ни к чему: снимок 144x80 RGBA — около 46 КБ.
const THUMB_CACHE_ENTRIES: usize = 32;

/// Сколько снимок считается свежим.
///
/// Содержимое окна меняется постоянно; трёх секунд достаточно, чтобы
/// повторные нажатия Tab не били по `PrintWindow`, и мало, чтобы показать
/// заведомо устаревшую картинку.
const THUMB_TTL_MS: u64 = 3_000;

/// Наибольшая сторона снимка: ровно область превью в карточке.
const THUMB_MAX_SIDE: u32 = 144;

/// Код клавиши Tab в Win32.
const VK_TAB: u32 = 0x09;

/// Через сколько бездействия модальный режим сам себя закрывает.
///
/// Режим забирает одиночные клавиши: пока он активен, `hjkl` не доходят до
/// приложений. Escape из него выводит и захардкожен как аварийный выход, но
/// про него надо ЗНАТЬ — а человек, случайно нажавший Alt+R и ушедший за
/// чаем, вернётся к клавиатуре, которая ведёт себя странно. Пятнадцати
/// секунд хватает, чтобы подумать над размером окна, и мало, чтобы забыть
/// (находка ревью 7).
const SUBMAP_IDLE_MS: u64 = 15_000;

/// Итог одного такта тайлинга — для журнала и для будущих слоёв.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TilingTick {
    /// Окнам отдана новая геометрия.
    pub moved: usize,
    /// Окна, которые должны быть спрятаны (фоновые табы групп, чужие
    /// воркспейсы). В T2 только считаются: скрытие через `DWMWA_CLOAKED` —
    /// это T4 вместе с watchdog'ом, без которого прятать окна нельзя.
    pub to_hide: Vec<usize>,
    /// Окна, которые должны быть показаны обратно.
    pub to_show: Vec<usize>,
    /// Окна, признанные неуправляемыми на этом такте.
    pub gave_up: Vec<usize>,
    /// Куда раскладка поставила стикеры: идентификатор, монитор и
    /// прямоугольник в DIP этого монитора.
    ///
    /// Применяет их координатор, а не подсистема: он единственный писатель
    /// `config.json` (докком `OverlayHandle`), и лезть в конфиг отсюда
    /// значило бы завести второго.
    pub sticker_places: Vec<(Uuid, MonitorId, Rect)>,
}

impl TilingTick {
    pub fn is_quiet(&self) -> bool {
        self.moved == 0
            && self.to_hide.is_empty()
            && self.to_show.is_empty()
            && self.gave_up.is_empty()
            && self.sticker_places.is_empty()
    }
}

/// Состояние тайлинга. Живёт в координаторе рядом с остальным edit-state.
pub struct TilingState {
    enabled: bool,
    workspaces: WorkspaceSet,
    params: LayoutParams,
    policy: InsertPolicy,
    rules: Vec<WindowRule>,
    /// Мониторы, на которых тайлинг работает. Пустой список — все.
    monitors: Vec<MonitorId>,
    /// Окна, которым уже погашены системные переходы ([`tiling_apply::prepare`]).
    prepared: Vec<usize>,
    /// Когда окну в последний раз отдавали команду (мс монотонных часов).
    last_command: HashMap<usize, u64>,
    /// Куда окну приказали встать последней командой.
    ///
    /// Нужно, чтобы отличить «доехало» от «не доехало». Возврат
    /// `set_dwm_bounds` для этого не годится: он `true` даже когда система
    /// отказала по UIPI (window_pin.rs:737) — на этом и построен был
    /// неработавший счётчик неудач (находка ревью 2).
    pending: HashMap<usize, Rect>,
    /// Сколько заходов подряд окно не доехало.
    failed: HashMap<usize, u8>,
    /// Таблица биндов и автомат модальных режимов.
    binds: BindTable,
    /// Тайлинг уже получал снимок окон с момента включения.
    ///
    /// Нужно ровно для одной строчки в журнале. Аудит горячего пути нашёл,
    /// что такт тайлинга стоял не в той ветке цикла и не вызывался на
    /// события окон вовсе — снаружи это выглядело как «тайлинг включён, но
    /// ничего не делает», и молчал бы до первой жалобы. Одна запись при
    /// первом снимке делает такую поломку видимой сразу.
    saw_snapshot: bool,
    /// Кэш снимков окон для карточек переключателя.
    ///
    /// Снимать окно дорого (`PrintWindow` — синхронный поход в чужой
    /// процесс), а переключатель перерисовывается на каждое нажатие Tab.
    /// Кэш живёт здесь, а не в `tiling_ui`: билдеры панелей чистые и о
    /// Win32 не знают.
    thumbs: ThumbCache,
    /// Стикеры, живущие в раскладке наравне с окнами.
    ///
    /// Реестр нужен потому, что дерево хранит непрозрачный `u64`, а у
    /// стикера идентификатор — `Uuid` в 128 бит. Он же — причина, по которой
    /// стикеры вообще удалось поселить в раскладку, не трогая дерево:
    /// `tile_id` кодирует вид плитки старшим битом ключа
    /// (crates/rst-core/src/tiling/tile_id.rs).
    stickers: StickerTiles,
    /// Перехватывать Alt+Tab своим переключателем.
    own_alt_tab: bool,
    /// Открытый переключатель окон. `None` — закрыт.
    switcher: Option<Switcher>,
    /// Порядок «последнее использованное первым»: hwnd, свежие в начале.
    ///
    /// Без него переключатель показывал бы окна в порядке дерева, а от
    /// Alt+Tab ждут обратного — вернуться к тому, с чем работал только что.
    mru: Vec<usize>,
    /// Когда в модальном режиме последний раз что-то нажимали.
    /// `None` — режима нет.
    submap_active_ms: Option<u64>,
    /// Окна, которые СКРЫЛИ МЫ (чужие воркспейсы, фоновые табы групп).
    ///
    /// Отдельный список нужен из-за неочевидного следствия: скрытое окно
    /// пропадает из снимка трекера — `window_enum::is_real_window` считает
    /// скрытые окна ненастоящими. Без этого списка [`Self::sync_tree`]
    /// решил бы, что окно закрылось, выбросил бы его из дерева и никогда не
    /// показал обратно: оно осталось бы невидимым до перезапуска.
    cloaked: Vec<usize>,
    /// Последний разобранный снимок: окна под управлением и мониторы.
    ///
    /// Нужен действиям с клавиатуры. Смена фокуса или перестановка плитки
    /// внутри нашего дерева не порождает НИКАКОГО события Windows — трекер
    /// промолчит, и без кэша раскладка ждала бы постороннего события, чтобы
    /// показать результат нажатия. С кэшем действие применяется сразу.
    last_managed: Vec<ManagedWindow>,
    last_monitors: Vec<MonitorInfo>,
}

impl TilingState {
    pub fn new(
        enabled: bool,
        params: LayoutParams,
        policy: InsertPolicy,
        rules: Vec<WindowRule>,
        monitors: Vec<MonitorId>,
        bindings: Vec<Binding>,
        own_alt_tab: bool,
    ) -> Self {
        Self {
            enabled,
            workspaces: WorkspaceSet::new(),
            params,
            policy,
            rules,
            monitors,
            binds: BindTable::new(bindings),
            saw_snapshot: false,
            thumbs: ThumbCache::new(THUMB_CACHE_ENTRIES, THUMB_TTL_MS),
            stickers: StickerTiles::new(),
            own_alt_tab,
            switcher: None,
            mru: Vec::new(),
            submap_active_ms: None,
            cloaked: Vec::new(),
            prepared: Vec::new(),
            last_command: HashMap::new(),
            pending: HashMap::new(),
            failed: HashMap::new(),
            last_managed: Vec::new(),
            last_monitors: Vec::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Комбинации, которые клавиатурный страж обязан перехватывать.
    ///
    /// Отдаются ему один раз при включении: набор охватывает ВСЕ режимы
    /// сразу, потому что переставлять хук на каждый вход в модальный режим
    /// и медленно, и гоночно (см. `binds::BindTable::swallow_set`).
    pub fn swallow_set(&self) -> Vec<Chord> {
        let mut out: Vec<Chord> = self
            .binds
            .active_swallow_set()
            .into_iter()
            .map(|c| Chord {
                vk: c.vk,
                ctrl: c.ctrl,
                alt: c.alt,
                shift: c.shift,
                win: c.win,
            })
            .collect();
        if self.own_alt_tab {
            // Alt+Tab и Alt+Shift+Tab — не бинды из конфига, а встроенное
            // поведение переключателя, потому и добавляются здесь.
            out.push(Chord {
                vk: VK_TAB,
                alt: true,
                ..Default::default()
            });
            out.push(Chord {
                vk: VK_TAB,
                alt: true,
                shift: true,
                ..Default::default()
            });
        }
        out
    }

    /// Запомнить окно как самое свежее в порядке переключения.
    pub fn note_focus(&mut self, hwnd: usize) {
        if self.mru.first() == Some(&hwnd) {
            return;
        }
        self.mru.retain(|h| *h != hwnd);
        self.mru.insert(0, hwnd);
        // Список не должен расти бесконечно: окна закрываются, а мы про них
        // помним. Полусотни записей с запасом хватает любому Alt+Tab.
        self.mru.truncate(50);
    }

    /// Переключатель сейчас открыт?
    pub fn switcher_open(&self) -> bool {
        self.switcher.is_some()
    }

    /// Карточки открытого переключателя: заголовок, размер группы, выбрана ли.
    pub fn switcher_cards(&self) -> Vec<(String, usize, bool)> {
        let Some(sw) = &self.switcher else {
            return Vec::new();
        };
        let selected = sw.selected();
        sw.entries()
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let key = entry.target_window();
                let title = self
                    .last_managed
                    .iter()
                    .find(|m| m.hwnd as u64 == key.0)
                    .map(|m| m.title.clone())
                    .unwrap_or_default();
                let members = match entry {
                    Entry::Window(_) => 1,
                    Entry::Group { members, .. } => members.len(),
                };
                (title, members, i == selected)
            })
            .collect()
    }

    /// Окна карточек переключателя, в том же порядке, что и карточки.
    pub fn switcher_targets(&self) -> Vec<usize> {
        self.switcher
            .as_ref()
            .map(|sw| {
                sw.entries()
                    .iter()
                    .map(|e| e.target_window().0 as usize)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Нажали Alt+Tab. `true` — нажатие наше, дальше его разбирать не надо.
    pub fn switcher_key(&mut self, chord: Chord) -> bool {
        if !self.own_alt_tab || chord.vk != VK_TAB || !chord.alt {
            return false;
        }
        match &mut self.switcher {
            // Уже открыт — крутим. Shift меняет направление, как и у
            // системного переключателя.
            Some(sw) => sw.step(!chord.shift),
            None => {
                let mru: Vec<WindowKey> = self.mru.iter().map(|h| WindowKey(*h as u64)).collect();
                let sw = Switcher::open(&self.workspaces, &mru);
                // Пустой переключатель не показываем, но нажатие всё равно
                // считаем своим: иначе при пустой раскладке Alt+Tab ушёл бы
                // системному, и одна клавиша вела бы себя двумя способами.
                if !sw.is_empty() {
                    self.switcher = Some(sw);
                }
            }
        }
        true
    }

    /// Alt отпущен: закрыть переключатель и отдать окно для фокуса.
    pub fn close_switcher(&mut self) -> Option<usize> {
        let sw = self.switcher.take()?;
        let key = sw.close()?;
        let hwnd = key.0 as usize;
        self.note_focus(hwnd);
        Some(hwnd)
    }

    /// Разобрать нажатие: вернуть действие, если сработал бинд.
    ///
    /// Вход и выход из модального режима сюда не доходят — их съедает сама
    /// таблица и возвращает `ModeChanged`; координатору остаётся только
    /// показать индикатор режима (это T5).
    /// Закрыть модальный режим, если в нём давно ничего не нажимали.
    ///
    /// `true` — режим закрыт, и координатор обязан обновить набор перехвата
    /// у хука и пересобрать индикаторы.
    pub fn expire_submap(&mut self, now_ms: u64) -> bool {
        let Some(since) = self.submap_active_ms else {
            return false;
        };
        if now_ms.saturating_sub(since) < SUBMAP_IDLE_MS {
            return false;
        }
        self.binds.reset();
        self.submap_active_ms = None;
        tracing::debug!("модальный режим тайлинга закрыт по бездействию");
        true
    }

    pub fn resolve_key(&mut self, chord: Chord, now_ms: u64) -> KeyOutcome {
        // Любое нажатие в режиме продлевает его: таймаут считается от
        // бездействия, а не от входа.
        let resolved = self.binds.resolve(KeyChord::new(
            chord.vk,
            chord.ctrl,
            chord.alt,
            chord.shift,
            chord.win,
        ));
        match resolved {
            Resolution::Fire(action) => {
                if self.submap_active_ms.is_some() {
                    self.submap_active_ms = Some(now_ms);
                }
                KeyOutcome {
                    action: Some(action),
                    mode_changed: false,
                }
            }
            Resolution::ModeChanged { submap } => {
                tracing::debug!(?submap, "режим биндов тайлинга");
                self.submap_active_ms = submap.as_ref().map(|_| now_ms);
                KeyOutcome {
                    action: None,
                    mode_changed: true,
                }
            }
            Resolution::PassThrough => KeyOutcome::default(),
        }
    }

    /// Включить/выключить тайлинг.
    ///
    /// Выключение возвращает окнам штатное поведение (системные анимации) и
    /// забывает раскладку, но НЕ разгоняет окна обратно: куда «обратно» —
    /// неизвестно, а гадать хуже, чем оставить как есть.
    pub fn set_enabled(&mut self, enabled: bool, pins: &WindowPins) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            // Показать всё, что мы прятали, — ПЕРВЫМ делом. Оставить окно
            // скрытым после выключения тайлинга значит потерять его для
            // пользователя: скрытого окна нет ни на экране, ни в Task View.
            for hwnd in self.cloaked.drain(..) {
                cloak::uncloak(hwnd);
            }
            for hwnd in self.prepared.drain(..) {
                tiling_apply::release(pins, hwnd);
            }
            self.workspaces = WorkspaceSet::new();
            self.saw_snapshot = false;
            self.last_command.clear();
            self.pending.clear();
            self.failed.clear();
            self.last_managed.clear();
            self.last_monitors.clear();
        }
    }

    /// Один такт: снимок окон пришёл — привести раскладку в соответствие.
    ///
    /// `now_ms` — монотонные часы координатора, те же, что у остальных
    /// таймеров цикла.
    pub fn on_snapshot(
        &mut self,
        windows: &[WindowInfo],
        monitors: &[MonitorInfo],
        pins: &WindowPins,
        now_ms: u64,
    ) -> TilingTick {
        if !self.enabled || monitors.is_empty() {
            return TilingTick::default();
        }

        let managed = self.managed_windows(windows, monitors);
        if !self.saw_snapshot {
            self.saw_snapshot = true;
            tracing::info!(
                windows = windows.len(),
                managed = managed.len(),
                monitors = monitors.len(),
                "тайлинг получил первый снимок окон"
            );
        }
        self.sync_tree(&managed, pins);
        self.last_managed = managed;
        self.last_monitors = monitors.to_vec();
        self.relayout(pins, now_ms)
    }

    /// Выполнить действие тайлинга (пришло с горячей клавиши).
    ///
    /// Монитор действия — тот, где сейчас сфокусированное окно: воркспейсы у
    /// нас помониторные (docs/TILING_DESIGN.md §Р1), и «переключить
    /// воркспейс» без указания монитора бессмысленно.
    pub fn dispatch(&mut self, action: &Action, pins: &WindowPins, now_ms: u64) -> TilingTick {
        if !self.enabled {
            return TilingTick::default();
        }
        let Some(monitor) = self.focused_monitor() else {
            return TilingTick::default();
        };
        match actions::apply(&mut self.workspaces, &monitor, action) {
            Outcome::NoChange => TilingTick::default(),
            Outcome::Relayout => self.relayout(pins, now_ms),
            Outcome::FocusWindow(key) => {
                // Фокус в нашем дереве и фокус в системе — разные вещи:
                // без этого вызова пользователь увидел бы, что рамка
                // «переехала», а печатает он по-прежнему в старое окно.
                let hwnd = key.0 as usize;
                if !tiling_apply::focus(hwnd) {
                    tracing::debug!(hwnd, "система не отдала фокус окну");
                }
                self.relayout(pins, now_ms)
            }
            Outcome::CloseWindow(key) => {
                tiling_apply::close(key.0 as usize);
                // Дерево не трогаем: окно вправе не закрыться (несохранённый
                // документ). Оно уйдёт из раскладки, когда трекер увидит,
                // что его больше нет.
                TilingTick::default()
            }
        }
    }

    /// Монитор сфокусированного окна; если фокуса нет — первый известный.
    fn focused_monitor(&self) -> Option<MonitorId> {
        for m in &self.last_monitors {
            if let Some(state) = self.workspaces.get_monitor(&m.id)
                && let Some(ws) = state.active_workspace()
                && ws.tree.focus().is_some()
            {
                return Some(m.id.clone());
            }
        }
        self.last_monitors.first().map(|m| m.id.clone())
    }

    /// Пересчитать раскладку прямо сейчас, по последнему снимку.
    ///
    /// Нужен, когда состав раскладки изменился не от событий Windows, а от
    /// действия пользователя над стикером: события окон при этом не будет
    /// вовсе, и без принудительного пересчёта плитка появилась бы только
    /// после постороннего события.
    pub fn relayout_now(&mut self, pins: &WindowPins, now_ms: u64) -> TilingTick {
        if !self.enabled {
            return TilingTick::default();
        }
        self.relayout(pins, now_ms)
    }

    /// Посчитать раскладку по кэшированному снимку и отправить её окнам.
    fn relayout(&mut self, pins: &WindowPins, now_ms: u64) -> TilingTick {
        let managed = std::mem::take(&mut self.last_managed);
        let monitors = std::mem::take(&mut self.last_monitors);
        let tick = self.relayout_with(&managed, &monitors, pins, now_ms);
        self.last_managed = managed;
        self.last_monitors = monitors;
        tick
    }

    fn relayout_with(
        &mut self,
        managed: &[ManagedWindow],
        monitors: &[MonitorInfo],
        pins: &WindowPins,
        now_ms: u64,
    ) -> TilingTick {
        let mut tick = TilingTick::default();
        self.judge_pending(managed, now_ms, &mut tick, pins);
        let mut targets: Vec<TileTarget> = Vec::new();

        for monitor in self.active_monitors(monitors) {
            let Some(state) = self.workspaces.get_monitor(&monitor.id) else {
                continue;
            };
            let Some(ws) = state.active_workspace() else {
                continue;
            };
            let all = layout::layout(&ws.tree, monitor.work_area_px, &self.params);
            // Стикеры не окна Windows: им не нужны ни сведение, ни команды
            // через Win32 — достаточно записать новое место в модель.
            let (sticker_slots, placements): (Vec<_>, Vec<_>) =
                all.into_iter().partition(|p| tile_id::is_sticker(p.window));
            for slot in &sticker_slots {
                if !slot.visible {
                    continue;
                }
                if let Some(uuid) = self.stickers.get_uuid(slot.window) {
                    tick.sticker_places.push((
                        uuid,
                        monitor.id.clone(),
                        to_dip_rect(slot.rect, monitor, (monitor.dpi as f64 / 96.0).max(0.1)),
                    ));
                }
            }
            let observed = observed_for(&placements, managed);
            let visible_now: Vec<WindowKey> = observed.iter().map(|o| o.window).collect();
            let plan = reconcile::reconcile(
                &observed,
                &placements,
                &visible_now,
                &ReconcileParams {
                    epsilon_px: EPSILON_PX,
                    max_moves: MAX_MOVES_PER_TICK,
                },
            );

            // hide/show из плана НЕ берём: `reconcile` видит только активный
            // воркспейс этого монитора, а прятать надо и окна чужих
            // воркспейсов — их в `placements` нет вовсе. Источник истины по
            // видимости один, и он ниже, после цикла.

            for mv in &plan.moves {
                let hwnd = mv.window.0 as usize;
                if self.is_settling(hwnd, now_ms) {
                    continue;
                }
                // Пользователь тащит это окно прямо сейчас — не драться с
                // его рукой. Проверка точечная: в Win32-слое вопрос задаётся
                // про конкретное окно (window_pin.rs:2107), общего «идёт ли
                // где-то драг» там нет.
                if window_pin::is_user_dragging(hwnd) {
                    continue;
                }
                if self.failed.get(&hwnd).copied().unwrap_or(0) >= MAX_FAILED_ATTEMPTS {
                    continue;
                }
                targets.push(TileTarget {
                    hwnd,
                    rect: to_window_rect(mv.to),
                });
                self.pending.insert(hwnd, mv.to);
            }
        }

        // Кто должен быть виден — решают воркспейсы целиком: активный
        // воркспейс каждого монитора плюс активные табы групп; всё
        // остальное прячется. Раньше этот шаг брался из плана сведения и
        // покрывал только фоновые табы, поэтому переключение воркспейса
        // меняло дерево, а окна оставались на экране (второе ревью,
        // наблюдение 6).
        let (show, hide) = self.desired_visibility();
        tick.to_show = show;
        tick.to_hide = hide;
        self.apply_visibility(&mut tick);

        if targets.is_empty() {
            return tick;
        }

        let report = tiling_apply::apply(pins, &targets);
        for hwnd in &report.attempted {
            self.last_command.insert(*hwnd, now_ms);
        }
        // `report.refused` — только отказ на самом вызове. Судить по нему о
        // неуправляемости нельзя: главный класс отказов (UIPI) возвращает
        // успех. Настоящий вердикт выносит `judge_pending` по факту —
        // встало окно на место или нет.
        for hwnd in &report.refused {
            self.pending.remove(hwnd);
            tracing::debug!(hwnd, "система отклонила перестановку окна");
        }
        for hwnd in &report.dead {
            self.forget(*hwnd);
        }
        tick.moved = report.attempted.len();
        tick
    }

    /// Собрать индикаторы тайлинга под текущее состояние.
    ///
    /// Геометрия окон живёт в физических пикселях виртуального десктопа, а
    /// оверлей рисует в DIP относительно СВОЕГО монитора — отсюда перевод в
    /// [`to_dip`]. Ошибиться здесь легко и заметно: рамка уехала бы на
    /// величину смещения монитора, а на мониторе с другим масштабом ещё и
    /// не совпала бы по размеру.
    pub fn build_overlay(&mut self, monitors: &[MonitorInfo], now_ms: u64) -> TilingOverlay {
        let mut overlay = TilingOverlay::default();
        if !self.enabled {
            return overlay;
        }
        let focused_monitor = self.focused_monitor();
        for monitor in self.active_monitors(monitors) {
            let Some(state) = self.workspaces.get_monitor(&monitor.id) else {
                continue;
            };
            let scale = (monitor.dpi as f64 / 96.0).max(0.1);
            // Панели живут в рабочей области, а не в границах монитора:
            // бар воркспейсов у нижнего края экрана иначе оказался бы ПОД
            // панелью задач, то есть невидимым (второе ревью, подозрение 5).
            // Координаты — относительно левого верхнего угла монитора, в той
            // же системе, в которую переводит `to_dip`.
            let screen = DipRect {
                x: (monitor.work_area_px.x - monitor.bounds_px.x) as f64 / scale,
                y: (monitor.work_area_px.y - monitor.bounds_px.y) as f64 / scale,
                w: monitor.work_area_px.w as f64 / scale,
                h: monitor.work_area_px.h as f64 / scale,
            };

            // Рамка — только у сфокусированной плитки: обводить каждое окно
            // значило бы перечертить весь экран рамками и потерять смысл
            // индикатора.
            if let Some(ws) = state.active_workspace()
                && let Some(focus) = ws.tree.focus()
                && let Some(key) = ws.tree.get(focus).and_then(|n| n.window())
                && let Some(win) = self.last_managed.iter().find(|m| m.hwnd as u64 == key.0)
            {
                let prims = tiling_ui::active_border(to_dip(win.rect, monitor, scale), true);
                if !prims.is_empty() {
                    overlay.borders.push((monitor.id.clone(), prims));
                }
            }

            let bars: Vec<(u8, bool, bool)> = state
                .workspaces
                .iter()
                .enumerate()
                .map(|(i, ws)| (ws.id.0, i == state.active, !ws.is_empty()))
                .collect();
            if !bars.is_empty() {
                overlay
                    .bars
                    .push((monitor.id.clone(), tiling_ui::workspace_bar(&bars, &screen)));
            }

            // Полосы табов: без них группа неотличима от одиночного окна —
            // пользователь видит одно окно и не знает, что за ним ещё три.
            if let Some(ws) = state.active_workspace() {
                for container in tabbed_containers(&ws.tree) {
                    let Some(bar_px) = layout::tab_bar_rect(
                        &ws.tree,
                        container,
                        monitor.work_area_px,
                        &self.params,
                    ) else {
                        continue;
                    };
                    let tabs: Vec<tiling_ui::TabEntry> = ws
                        .tree
                        .children_of(container)
                        .iter()
                        .enumerate()
                        .filter_map(|(i, child)| {
                            let key = ws.tree.get(*child).and_then(|n| n.window())?;
                            let active = ws
                                .tree
                                .get(container)
                                .and_then(|n| n.container())
                                .is_some_and(|c| c.focused_child == i);
                            let title = self
                                .last_managed
                                .iter()
                                .find(|m| m.hwnd as u64 == key.0)
                                .map(|m| m.title.clone())
                                .unwrap_or_default();
                            Some(tiling_ui::TabEntry { title, active })
                        })
                        .collect();
                    if tabs.is_empty() {
                        continue;
                    }
                    let bar = to_dip_rect(bar_px, monitor, scale);
                    overlay.bars.push((
                        monitor.id.clone(),
                        tiling_ui::group_tabs(bar, self.params.tab_bar_h as f64 / scale, &tabs),
                    ));
                }
            }

            // Переключатель окон — поверх всего и только на мониторе с
            // фокусом: он один на программу, и дублировать его на каждом
            // экране значило бы заставить пользователя искать, где именно
            // сейчас выделение.
            if self.switcher.is_some() && focused_monitor.as_ref() == Some(&monitor.id) {
                let cards: Vec<tiling_ui::SwitcherCard> = self
                    .switcher_cards()
                    .into_iter()
                    .map(|(title, members, selected)| tiling_ui::SwitcherCard {
                        title,
                        members,
                        selected,
                    })
                    .collect();
                if !cards.is_empty() {
                    overlay.bars.push((
                        monitor.id.clone(),
                        tiling_ui::switcher_panel(&cards, &screen),
                    ));
                    let slots = tiling_ui::switcher_preview_rects(cards.len(), &screen);
                    let targets = self.switcher_targets();
                    for (slot, hwnd) in slots.into_iter().zip(targets) {
                        // Снимка может не быть: окно защищено, отдаёт чёрный
                        // кадр или ещё не отрисовано. Тогда карточка просто
                        // остаётся с заголовком — это лучше чёрного
                        // прямоугольника.
                        let Some(thumb) = self.thumbs.get(hwnd, THUMB_MAX_SIDE, now_ms) else {
                            continue;
                        };
                        overlay.images.push((
                            monitor.id.clone(),
                            Primitive::Rgba {
                                rect: fit_inside(slot, thumb.width, thumb.height),
                                // Ключ кэша текстур — хэндл окна: пока окно
                                // то же, GPU-текстура переиспользуется.
                                key: hwnd as u64,
                                width: thumb.width,
                                height: thumb.height,
                                rgba: thumb.rgba.clone(),
                                opacity: 1.0,
                            },
                        ));
                    }
                }
            }

            if let Some(name) = self.binds.submap()
                && focused_monitor.as_ref() == Some(&monitor.id)
            {
                overlay.submap = Some((
                    monitor.id.clone(),
                    tiling_ui::submap_indicator(name, &screen),
                ));
            }
        }
        overlay
    }

    /// Подвести итог по командам, у которых истекла выдержка.
    ///
    /// Окно либо доехало (сбрасываем счётчик неудач), либо нет
    /// (увеличиваем). Набрав [`MAX_FAILED_ATTEMPTS`], окно признаётся
    /// неуправляемым и УХОДИТ ИЗ ДЕРЕВА: elevated-окно, оставленное в
    /// раскладке, держало бы пустую плитку и получало команды до конца
    /// сессии (находки ревью 2 и 3).
    fn judge_pending(
        &mut self,
        managed: &[ManagedWindow],
        now_ms: u64,
        tick: &mut TilingTick,
        pins: &WindowPins,
    ) {
        let due: Vec<(usize, Rect)> = self
            .pending
            .iter()
            .filter(|(hwnd, _)| !self.is_settling(**hwnd, now_ms))
            .map(|(hwnd, rect)| (*hwnd, *rect))
            .collect();

        for (hwnd, target) in due {
            // Окно тащит пользователь: оно и не должно стоять там, куда мы
            // его отправили. Судить сейчас — значит через три четверти
            // секунды драга объявить обычное окно неуправляемым и выбросить
            // из раскладки навсегда (второе ревью, находка 2).
            if window_pin::is_user_dragging(hwnd) {
                self.pending.remove(&hwnd);
                continue;
            }
            let Some(window) = managed.iter().find(|m| m.hwnd == hwnd) else {
                // Окна нет в снимке — либо закрылось, либо мы его спрятали.
                // Судить не о чем.
                self.pending.remove(&hwnd);
                continue;
            };
            self.pending.remove(&hwnd);
            if arrived(to_core_rect(window.rect), target) {
                self.failed.remove(&hwnd);
                continue;
            }
            let counter = self.failed.entry(hwnd).or_insert(0);
            *counter = counter.saturating_add(1);
            if *counter >= MAX_FAILED_ATTEMPTS {
                tracing::info!(
                    hwnd,
                    "окно не встаёт в плитку — считаю неуправляемым (elevated или свой минимальный размер)"
                );
                tick.gave_up.push(hwnd);
                self.workspaces.remove_window(WindowKey(hwnd as u64));
                self.last_command.remove(&hwnd);
                self.prepared.retain(|h| *h != hwnd);
                tiling_apply::release(pins, hwnd);
            }
        }
    }

    /// Поселить стикер в раскладку монитора.
    ///
    /// Возвращает `false`, если стикер уже там: повторное добавление создало
    /// бы вторую плитку под тот же стикер, и он метался бы между ними.
    pub fn add_sticker(&mut self, uuid: Uuid, monitor: &MonitorId) -> bool {
        if self.stickers.contains_uuid(uuid) {
            return false;
        }
        let key = self.stickers.get_or_register(uuid);
        let state = self.workspaces.ensure_monitor(monitor);
        let plan = state
            .active_workspace()
            .map(|ws| policy::plan_insert(&ws.tree, self.policy, None));
        if let Some(plan) = plan
            && let Some(ws) = state.active_workspace_mut()
        {
            policy::insert(&mut ws.tree, key, plan);
            return true;
        }
        // Вставить не вышло — реестр не должен помнить несуществующую плитку.
        self.stickers.unregister(uuid);
        false
    }

    /// Убрать стикер из раскладки. Сам стикер остаётся жить, просто уже не
    /// в плитке — где он был, там и остаётся.
    pub fn remove_sticker(&mut self, uuid: Uuid) -> bool {
        let Some(key) = self.stickers.get_key(uuid) else {
            return false;
        };
        self.workspaces.remove_window(key);
        self.stickers.unregister(uuid);
        true
    }

    /// Стикер сейчас в раскладке?
    pub fn has_sticker(&self, uuid: Uuid) -> bool {
        self.stickers.contains_uuid(uuid)
    }

    /// Кто должен быть виден, а кто спрятан прямо сейчас.
    ///
    /// Источник истины — воркспейсы целиком, а не раскладка одного монитора:
    /// в раскладке активного воркспейса окон чужих воркспейсов нет вовсе, и
    /// пока видимость считалась из неё, переключение воркспейса меняло
    /// дерево, но окна оставались на экране.
    fn desired_visibility(&self) -> (Vec<usize>, Vec<usize>) {
        // Стикеры отсеиваются: их видимость — дело модели стикеров, а
        // `DWMWA_CLOAK` применим только к настоящим окнам Windows. Отправить
        // ключ стикера в `cloak` значило бы позвать Win32 с числом, которое
        // хэндлом не является.
        let show = self
            .workspaces
            .visible_windows()
            .into_iter()
            .filter(|k| !tile_id::is_sticker(*k))
            .map(|k| k.0 as usize)
            .collect();
        let hide = self
            .workspaces
            .hidden_windows()
            .into_iter()
            .filter(|k| !tile_id::is_sticker(*k))
            .map(|k| k.0 as usize)
            .collect();
        (show, hide)
    }

    /// Спрятать и показать окна по плану раскладки.
    ///
    /// Показываем ДО того, как прячем: если оба списка непусты (переключение
    /// воркспейса), обратный порядок оставил бы экран на мгновение пустым.
    fn apply_visibility(&mut self, tick: &mut TilingTick) {
        tick.to_show.retain(|hwnd| {
            if !self.cloaked.contains(hwnd) {
                // Окно и так видно — показывать нечего.
                return false;
            }
            let shown = cloak::uncloak(*hwnd);
            if shown {
                self.cloaked.retain(|h| h != hwnd);
            }
            shown
        });
        tick.to_hide.retain(|hwnd| {
            if self.cloaked.contains(hwnd) {
                return false;
            }
            let hidden = cloak::cloak(*hwnd);
            if hidden {
                self.cloaked.push(*hwnd);
            } else {
                // Не спрятали (UIPI, окно умерло) — не беда: окно просто
                // останется видимым. Хуже было бы записать его в скрытые и
                // потом «показывать» то, что и так видно.
                tracing::debug!(hwnd, "окно не удалось спрятать");
            }
            hidden
        });
    }

    /// Окно закрылось или ушло из-под управления — забыть о нём всё.
    fn forget(&mut self, hwnd: usize) {
        self.workspaces.remove_window(WindowKey(hwnd as u64));
        self.last_command.remove(&hwnd);
        self.failed.remove(&hwnd);
        self.pending.remove(&hwnd);
        self.prepared.retain(|h| *h != hwnd);
        // Окно уходит из-под управления — оно обязано быть видимым.
        if self.cloaked.contains(&hwnd) {
            cloak::uncloak(hwnd);
            self.cloaked.retain(|h| *h != hwnd);
        }
    }

    /// Окну только что отдали команду — пусть доедет.
    fn is_settling(&self, hwnd: usize, now_ms: u64) -> bool {
        self.last_command
            .get(&hwnd)
            .is_some_and(|sent| now_ms.saturating_sub(*sent) < SETTLE_MS)
    }

    /// Мониторы, на которых тайлинг работает.
    fn active_monitors<'a>(&self, monitors: &'a [MonitorInfo]) -> Vec<&'a MonitorInfo> {
        monitors
            .iter()
            .filter(|m| self.monitors.is_empty() || self.monitors.contains(&m.id))
            .collect()
    }

    /// Окна, которыми тайлинг вправе распоряжаться, с монитором каждого.
    fn managed_windows(
        &self,
        windows: &[WindowInfo],
        monitors: &[MonitorInfo],
    ) -> Vec<ManagedWindow> {
        let active: Vec<&MonitorInfo> = self.active_monitors(monitors);
        windows
            .iter()
            .filter(|w| window_enum::is_tileable(w))
            .filter(|w| {
                let outcome = rst_core::tiling::rules::evaluate(&self.rules, &facts_of(w));
                outcome.tiled && !outcome.ignored
            })
            // Окно, уже признанное неуправляемым, обратно в дерево не берём:
            // иначе следующий же снимок вернул бы его и всё началось бы
            // заново.
            .filter(|w| self.failed.get(&w.hwnd).copied().unwrap_or(0) < MAX_FAILED_ATTEMPTS)
            .filter_map(|w| {
                monitor_of(w.rect, &active).map(|m| ManagedWindow {
                    hwnd: w.hwnd,
                    rect: w.rect,
                    monitor: m.id.clone(),
                    title: w.title.clone(),
                })
            })
            .collect()
    }

    /// Привести дерево в соответствие со снимком: новые окна вставить,
    /// исчезнувшие — убрать.
    fn sync_tree(&mut self, managed: &[ManagedWindow], pins: &WindowPins) {
        let known: Vec<usize> = self
            .workspaces
            .monitors
            .iter()
            .flat_map(|m| m.workspaces.iter())
            .flat_map(|w| w.all_windows())
            .map(|k| k.0 as usize)
            .collect();

        for hwnd in known {
            // Скрытое НАМИ окно в снимке не появляется по построению (см.
            // комментарий к полю `cloaked`) — это не значит, что оно
            // закрылось.
            if self.cloaked.contains(&hwnd) {
                continue;
            }
            // Стикера в снимке окон нет и быть не может: он не окно Windows.
            // Без этой проверки первый же снимок вышвыривал бы стикеры из
            // раскладки — ровно та ловушка, что была со скрытыми окнами.
            if tile_id::is_sticker(WindowKey(hwnd as u64)) {
                continue;
            }
            if !managed.iter().any(|m| m.hwnd == hwnd) {
                self.forget(hwnd);
                tiling_apply::release(pins, hwnd);
            }
        }

        for window in managed {
            let key = WindowKey(window.hwnd as u64);
            if self.workspaces.find_window(key).is_some() {
                continue;
            }
            let state = self.workspaces.ensure_monitor(&window.monitor);
            let plan = state
                .active_workspace()
                .map(|ws| policy::plan_insert(&ws.tree, self.policy, None));
            if let Some(plan) = plan
                && let Some(ws) = state.active_workspace_mut()
            {
                policy::insert(&mut ws.tree, key, plan);
            }
            if !self.prepared.contains(&window.hwnd) {
                tiling_apply::prepare(pins, window.hwnd);
                self.prepared.push(window.hwnd);
            }
        }
    }
}

/// Окно под управлением тайлинга вместе с монитором, на котором оно живёт.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedWindow {
    hwnd: usize,
    rect: WindowRect,
    monitor: MonitorId,
    /// Заголовок окна — его показывает полоса табов группы.
    title: String,
}

/// Наблюдаемое положение окон, о которых говорит раскладка.
///
/// Только те, что есть и в раскладке, и в снимке: окно из раскладки, которого
/// нет в снимке, `reconcile` и так пропустит, но собирать его сюда незачем.
fn observed_for(placements: &[Placement], managed: &[ManagedWindow]) -> Vec<Observed> {
    placements
        .iter()
        .filter_map(|p| {
            managed
                .iter()
                .find(|m| m.hwnd as u64 == p.window.0)
                .map(|m| Observed {
                    window: p.window,
                    rect: to_core_rect(m.rect),
                })
        })
        .collect()
}

/// Монитор, на котором живёт окно: тот, чьи границы содержат его ЦЕНТР.
///
/// Не левый верхний угол: окно, чей угол чуть заехал на соседний монитор,
/// принадлежит тому, где его основная площадь, — иначе плитка прыгала бы
/// между мониторами от одного пикселя. Ни один монитор не подошёл (окно за
/// пределами всех, такое бывает у свёрнутых и у окон на отключённом
/// мониторе) — `None`, и окно просто не участвует в раскладке.
fn monitor_of<'a>(rect: WindowRect, monitors: &[&'a MonitorInfo]) -> Option<&'a MonitorInfo> {
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    monitors
        .iter()
        .find(|m| contains(m.bounds_px, cx, cy))
        .copied()
}

fn contains(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.w as i32 && y < r.y + r.h as i32
}

/// Окно встало туда, куда приказали, с точностью до эпсилона.
///
/// Эпсилон обязателен: приложения округляют размеры сами — терминалы встают
/// по сетке символов, у многих окон есть свой минимальный размер. С нулевым
/// допуском «доехавшим» не считалось бы почти ничего.
fn arrived(observed: Rect, target: Rect) -> bool {
    (observed.x - target.x).abs() <= EPSILON_PX
        && (observed.y - target.y).abs() <= EPSILON_PX
        && (observed.w as i32 - target.w as i32).abs() <= EPSILON_PX
        && (observed.h as i32 - target.h as i32).abs() <= EPSILON_PX
}

/// Вписать картинку в слот, сохранив её пропорции.
///
/// Без этого снимок вертикального окна растянулся бы на всю ширину карточки
/// и стал бы неузнаваемым — а узнаваемость и есть единственный смысл превью.
fn fit_inside(slot: Box2D, img_w: u32, img_h: u32) -> Box2D {
    if img_w == 0 || img_h == 0 || slot.w <= 0.0 || slot.h <= 0.0 {
        return slot;
    }
    let scale = (slot.w / img_w as f64).min(slot.h / img_h as f64);
    Box2D {
        cx: slot.cx,
        cy: slot.cy,
        w: img_w as f64 * scale,
        h: img_h as f64 * scale,
        rotation: 0.0,
    }
}

/// Все контейнеры дерева, которые являются группами с табами.
fn tabbed_containers(tree: &rst_core::tiling::Tree) -> Vec<rst_core::tiling::NodeId> {
    fn walk(
        tree: &rst_core::tiling::Tree,
        id: rst_core::tiling::NodeId,
        out: &mut Vec<rst_core::tiling::NodeId>,
    ) {
        let Some(node) = tree.get(id) else {
            return;
        };
        if let Some(c) = node.container() {
            if !c.layout.is_split() {
                out.push(id);
            }
            for child in &c.children {
                walk(tree, *child, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, tree.root(), &mut out);
    out
}

/// Произвольный прямоугольник (физические px десктопа) → DIP этого монитора.
fn to_dip_rect(r: Rect, monitor: &MonitorInfo, scale: f64) -> Rect {
    to_dip(
        WindowRect {
            x: r.x,
            y: r.y,
            w: r.w as i32,
            h: r.h as i32,
        },
        monitor,
        scale,
    )
}

/// Прямоугольник окна (физические px виртуального десктопа) → DIP этого
/// монитора.
fn to_dip(r: WindowRect, monitor: &MonitorInfo, scale: f64) -> Rect {
    let x = (r.x - monitor.bounds_px.x) as f64 / scale;
    let y = (r.y - monitor.bounds_px.y) as f64 / scale;
    Rect {
        x: x.round() as i32,
        y: y.round() as i32,
        w: (r.w as f64 / scale).round().max(0.0) as u32,
        h: (r.h as f64 / scale).round().max(0.0) as u32,
    }
}

fn to_core_rect(r: WindowRect) -> Rect {
    Rect {
        x: r.x,
        y: r.y,
        w: r.w.max(0) as u32,
        h: r.h.max(0) as u32,
    }
}

fn to_window_rect(r: Rect) -> WindowRect {
    WindowRect {
        x: r.x,
        y: r.y,
        w: r.w as i32,
        h: r.h as i32,
    }
}

fn facts_of(w: &WindowInfo) -> WindowFacts {
    WindowFacts {
        exe_path: Some(w.exe_path.to_string_lossy().into_owned()).filter(|s| !s.is_empty()),
        title: w.title.clone(),
        class: w.class.clone(),
    }
}

/// Что делать координатору после разбора нажатия.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyOutcome {
    /// Сработавшее действие.
    pub action: Option<Action>,
    /// Модальный режим сменился - координатор ОБЯЗАН обновить набор
    /// перехвата у клавиатурного хука. Без этого одиночные клавиши режима
    /// продолжали бы глотаться после выхода из него.
    pub mode_changed: bool,
}

/// Готовые к отрисовке индикаторы тайлинга на один кадр.
///
/// Собираются координатором при изменении состояния и просто рисуются в
/// `redraw` — по тому же принципу, что и остальные панели проекта: в кадре
/// не должно быть логики.
///
/// Без `Debug`: `Panel` его не реализует, а печатать сюда всё равно нечего —
/// это готовая графика, а не состояние.
#[derive(Default)]
pub struct TilingOverlay {
    /// Рамки плиток: монитор → примитивы.
    borders: Vec<(MonitorId, Vec<Primitive>)>,
    /// Бар воркспейсов на каждом тайлящемся мониторе.
    bars: Vec<(MonitorId, Panel)>,
    /// Плашка модального режима. Один раз, на мониторе с фокусом: режим
    /// один на всю программу, и дублировать его на каждом экране незачем.
    submap: Option<(MonitorId, Panel)>,
    /// Растровые картинки поверх панелей: снимки окон в карточках
    /// переключателя. Отдельно от панелей, потому что рисуются ПОСЛЕ них.
    images: Vec<(MonitorId, Primitive)>,
}

impl TilingOverlay {
    /// Выложить примитивы этого монитора.
    pub fn draw(&self, monitor: &MonitorId, out: &mut Vec<Primitive>) {
        for (id, prims) in &self.borders {
            if id == monitor {
                out.extend(prims.iter().cloned());
            }
        }
        for (id, panel) in &self.bars {
            if id == monitor {
                panel.draw(out);
            }
        }
        if let Some((id, panel)) = &self.submap
            && id == monitor
        {
            panel.draw(out);
        }
        // Картинки последними: снимок окна ложится поверх своей карточки.
        for (id, prim) in &self.images {
            if id == monitor {
                out.push(prim.clone());
            }
        }
    }
}

/// Собрать таблицу биндов из конфига.
///
/// Непонятная строка не роняет конфиг и не отменяет остальные бинды: её
/// пропускают с предупреждением в журнал. Причина простая — этот файл правит
/// человек руками, опечатка в нём норма, а «программа не запустилась из-за
/// одной строчки» — не норма. Тот же принцип уже принят в разборе действий
/// (`rst_core::tiling::action::Action::parse`).
///
/// Комбинации, которые Windows забирает себе, тоже отбрасываются: молча
/// принять их значило бы отдать пользователю клавишу, которая никогда не
/// сработает, без единого слова о том, почему.
pub fn bindings_from_config(configured: &[TilingBinding]) -> Vec<Binding> {
    let mut out = Vec::new();
    for entry in configured {
        let combo = match HotkeyCombo::parse_binding(&entry.combo) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(combo = %entry.combo, error = %e, "бинд тайлинга пропущен");
                continue;
            }
        };
        if combo.is_reserved_by_windows() {
            tracing::warn!(
                combo = %entry.combo,
                "бинд тайлинга пропущен: комбинацию забирает себе Windows"
            );
            continue;
        }
        let action = match Action::parse(&entry.action, entry.arg.as_deref()) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    combo = %entry.combo,
                    action = %entry.action,
                    error = %e,
                    "бинд тайлинга пропущен"
                );
                continue;
            }
        };
        out.push(Binding {
            chord: KeyChord::new(combo.vk, combo.ctrl, combo.alt, combo.shift, combo.win),
            action,
            submap: entry.submap.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_core::tiling::ops::Direction;

    fn monitor(id: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id.to_string()),
            friendly_name: id.to_string(),
            bounds_px: Rect { x, y, w, h },
            // Рабочая область на 40 px ниже — как будто снизу панель задач.
            work_area_px: Rect {
                x,
                y,
                w,
                h: h.saturating_sub(40),
            },
            dpi: 96,
            is_primary: x == 0 && y == 0,
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> WindowRect {
        WindowRect { x, y, w, h }
    }

    fn cfg_bind(combo: &str, action: &str, arg: Option<&str>) -> TilingBinding {
        TilingBinding {
            combo: combo.to_string(),
            action: action.to_string(),
            arg: arg.map(|a| a.to_string()),
            submap: None,
        }
    }

    #[test]
    fn a_good_binding_survives_the_config_round_trip() {
        let out = bindings_from_config(&[cfg_bind("Alt+H", "focus_direction", Some("left"))]);
        assert_eq!(out.len(), 1);
        assert!(out[0].chord.alt && !out[0].chord.ctrl);
        assert_eq!(out[0].action, Action::FocusDirection(Direction::Left));
    }

    #[test]
    fn a_typo_in_the_config_skips_one_binding_and_keeps_the_rest() {
        // Конфиг правит человек: опечатка не должна стоить ему всех биндов.
        let out = bindings_from_config(&[
            cfg_bind("Alt+H", "focus_direction", Some("left")),
            cfg_bind("Alt+Ы", "focus_direction", Some("left")),
            cfg_bind("Alt+J", "do_a_barrel_roll", None),
            cfg_bind("Alt+K", "focus_direction", Some("up")),
        ]);
        assert_eq!(out.len(), 2, "выжили только корректные строки");
    }

    #[test]
    fn a_combination_windows_keeps_for_itself_is_dropped() {
        // Иначе пользователь получил бы клавишу, которая молча никогда
        // не сработает.
        let out = bindings_from_config(&[cfg_bind("Win+L", "close_window", None)]);
        assert!(out.is_empty());
    }

    fn state() -> TilingState {
        TilingState::new(
            true,
            LayoutParams::default(),
            InsertPolicy::Dwindle,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        )
    }

    fn tab(alt: bool, shift: bool) -> Chord {
        Chord {
            vk: VK_TAB,
            alt,
            shift,
            ..Default::default()
        }
    }

    #[test]
    fn alt_tab_is_ignored_while_the_own_switcher_is_off() {
        // Alt+Tab — самая заученная комбинация в Windows; подменять её
        // молча нельзя, только по явной настройке.
        let mut st = state();
        assert!(!st.switcher_key(tab(true, false)));
        assert!(!st.switcher_open());
    }

    #[test]
    fn the_swallow_set_gains_alt_tab_only_when_asked() {
        let off = state();
        assert!(!off.swallow_set().iter().any(|c| c.vk == VK_TAB));

        let mut on = state();
        on.own_alt_tab = true;
        let set = on.swallow_set();
        assert!(set.contains(&tab(true, false)), "Alt+Tab");
        assert!(set.contains(&tab(true, true)), "Alt+Shift+Tab");
    }

    #[test]
    fn a_plain_tab_is_never_ours() {
        // Без Alt это обычный Tab: отобрать его у приложений — сломать ввод.
        let mut st = state();
        st.own_alt_tab = true;
        assert!(!st.switcher_key(tab(false, false)));
    }

    #[test]
    fn an_empty_layout_swallows_alt_tab_without_showing_anything() {
        // Нажатие считаем своим даже когда показывать нечего: иначе одна и
        // та же клавиша вела бы себя двумя разными способами.
        let mut st = state();
        st.own_alt_tab = true;
        assert!(st.switcher_key(tab(true, false)));
        assert!(!st.switcher_open());
    }

    #[test]
    fn closing_a_switcher_that_was_never_opened_gives_nothing() {
        let mut st = state();
        assert!(st.close_switcher().is_none());
    }

    #[test]
    fn the_focus_order_puts_the_newest_window_first() {
        let mut st = state();
        st.note_focus(1);
        st.note_focus(2);
        st.note_focus(1);
        assert_eq!(st.mru, vec![1, 2], "повтор поднимает, а не дублирует");
    }

    #[test]
    fn the_focus_order_does_not_grow_without_bound() {
        let mut st = state();
        for hwnd in 0..200 {
            st.note_focus(hwnd);
        }
        assert!(st.mru.len() <= 50);
        assert_eq!(st.mru.first().copied(), Some(199));
    }

    fn managed(hwnd: usize, r: WindowRect) -> ManagedWindow {
        ManagedWindow {
            hwnd,
            rect: r,
            monitor: MonitorId("A".to_string()),
            title: String::new(),
        }
    }

    #[test]
    fn a_window_that_arrived_clears_its_failure_count() {
        let mut st = state();
        st.failed.insert(7, 1);
        st.pending.insert(
            7,
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
        );
        let pins = WindowPins::new();
        let mut tick = TilingTick::default();
        // Окно стоит на месте с точностью до эпсилона.
        st.judge_pending(
            &[managed(7, rect(1, 0, 100, 100))],
            10_000,
            &mut tick,
            &pins,
        );
        assert!(st.failed.is_empty(), "доехало — счётчик обнуляется");
        assert!(st.pending.is_empty());
    }

    #[test]
    fn a_window_that_ignored_the_command_is_counted_as_failed() {
        let mut st = state();
        st.pending.insert(
            7,
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
        );
        let pins = WindowPins::new();
        let mut tick = TilingTick::default();
        st.judge_pending(
            &[managed(7, rect(500, 500, 100, 100))],
            10_000,
            &mut tick,
            &pins,
        );
        assert_eq!(st.failed.get(&7).copied(), Some(1));
    }

    #[test]
    fn a_window_is_given_up_on_after_the_attempt_limit() {
        // Так честно отсекаются elevated-окна: система молча отказывает,
        // а вызов при этом рапортует успех (находка ревью 2).
        let mut st = state();
        let pins = WindowPins::new();
        let far_away = rect(500, 500, 100, 100);
        let target = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let mut tick = TilingTick::default();
        for _ in 0..MAX_FAILED_ATTEMPTS {
            st.pending.insert(7, target);
            st.judge_pending(&[managed(7, far_away)], 10_000, &mut tick, &pins);
        }
        assert_eq!(tick.gave_up, vec![7]);
    }

    #[test]
    fn a_window_still_settling_is_not_judged_yet() {
        // Судить до истечения выдержки значило бы записать в неуправляемые
        // любое приложение, которое перекладывает вёрстку медленнее нас.
        let mut st = state();
        st.last_command.insert(7, 1_000);
        st.pending.insert(
            7,
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
        );
        let pins = WindowPins::new();
        let mut tick = TilingTick::default();
        st.judge_pending(
            &[managed(7, rect(500, 500, 100, 100))],
            1_050,
            &mut tick,
            &pins,
        );
        assert!(st.failed.is_empty());
        assert!(st.pending.contains_key(&7), "вердикт отложен, а не вынесен");
    }

    #[test]
    fn a_sticker_can_live_in_the_layout_next_to_windows() {
        // Ради этого тайлинг и живёт в resticker: ни komorebi, ни GlazeWM
        // так не умеют — у них в раскладке только окна Windows.
        let mut st = state();
        let mon = MonitorId("A".to_string());
        let sticker = Uuid::from_u128(7);
        assert!(st.add_sticker(sticker, &mon));
        assert!(st.has_sticker(sticker));
        assert!(
            !st.add_sticker(sticker, &mon),
            "второй раз тот же стикер — вторая плитка под него, он метался бы между ними"
        );
    }

    #[test]
    fn removing_a_sticker_takes_it_out_of_the_layout() {
        let mut st = state();
        let mon = MonitorId("A".to_string());
        let sticker = Uuid::from_u128(7);
        st.add_sticker(sticker, &mon);
        assert!(st.remove_sticker(sticker));
        assert!(!st.has_sticker(sticker));
        assert!(
            !st.remove_sticker(sticker),
            "повторное удаление — не ошибка"
        );
    }

    #[test]
    fn a_sticker_is_never_offered_to_the_window_hiding_layer() {
        // Ключ стикера — не хэндл окна. Отправить его в cloak значило бы
        // позвать Win32 с числом, которое окном не является.
        let mut st = state();
        let mon = MonitorId("A".to_string());
        st.add_sticker(Uuid::from_u128(7), &mon);
        let (show, hide) = st.desired_visibility();
        assert!(
            show.is_empty(),
            "стикер не прячется и не показывается через Win32"
        );
        assert!(hide.is_empty());
    }

    #[test]
    fn a_sticker_survives_a_snapshot_that_does_not_mention_it() {
        // Стикера нет и не может быть в снимке окон. Без защиты первый же
        // снимок вышвырнул бы его из раскладки — та же ловушка, что была со
        // скрытыми окнами.
        let mut st = state();
        let mon = MonitorId("A".to_string());
        let sticker = Uuid::from_u128(7);
        st.add_sticker(sticker, &mon);
        let pins = WindowPins::new();
        st.sync_tree(&[], &pins);
        assert!(st.has_sticker(sticker), "стикер выжил пустой снимок окон");
    }

    #[test]
    fn switching_a_workspace_marks_the_old_windows_for_hiding() {
        // Регрессия: пока видимость считалась из раскладки одного монитора,
        // окна чужих воркспейсов в неё не попадали, и переключение
        // воркспейса меняло дерево, оставляя окна на экране.
        use rst_core::tiling::InsertAt;
        use rst_core::tiling::workspace::WorkspaceId;

        let mut st = state();
        let mon = MonitorId("A".to_string());
        st.workspaces.ensure_monitor(&mon);
        st.workspaces
            .insert_window(&mon, WindowKey(1), InsertAt::Root);

        let (show, hide) = st.desired_visibility();
        assert!(show.contains(&1), "окно активного воркспейса видно");
        assert!(hide.is_empty());

        // Переезжаем на пустой воркспейс: окно первого обязано уйти в скрытые.
        assert!(st.workspaces.switch_to(&mon, WorkspaceId(2)));
        let (show, hide) = st.desired_visibility();
        assert!(hide.contains(&1), "окно чужого воркспейса надо спрятать");
        assert!(!show.contains(&1));
    }

    #[test]
    fn an_empty_layout_asks_to_hide_nothing() {
        let st = state();
        let (show, hide) = st.desired_visibility();
        assert!(show.is_empty());
        assert!(hide.is_empty());
    }

    #[test]
    fn showing_a_window_we_never_hid_is_a_no_op() {
        // Иначе каждый такт дёргал бы DWM на окна, которые и так видны.
        let mut st = state();
        let mut tick = TilingTick {
            to_show: vec![42],
            ..Default::default()
        };
        st.apply_visibility(&mut tick);
        assert!(tick.to_show.is_empty());
    }

    #[test]
    fn hiding_a_dead_window_does_not_add_it_to_the_hidden_list() {
        // Иначе мы бы вечно «показывали» окно, которого нет, и держали бы
        // его в списке скрытых до конца сессии.
        let mut st = state();
        let mut tick = TilingTick {
            to_hide: vec![0xDEAD_BEEF],
            ..Default::default()
        };
        st.apply_visibility(&mut tick);
        assert!(tick.to_hide.is_empty());
        assert!(st.cloaked.is_empty());
    }

    #[test]
    fn a_hidden_window_is_not_treated_as_closed() {
        // Ключевая ловушка T4: скрытое окно пропадает из снимка трекера,
        // и без этой защиты подсистема сочла бы его закрытым и никогда не
        // показала обратно.
        let mut st = state();
        st.cloaked.push(77);
        let pins = WindowPins::new();
        st.sync_tree(&[], &pins);
        assert!(
            st.cloaked.contains(&77),
            "скрытое окно не должно быть забыто по пустому снимку"
        );
    }

    #[test]
    fn disabling_tiling_clears_the_hidden_list() {
        // Оставить окно скрытым после выключения — значит потерять его.
        let mut st = state();
        st.cloaked.push(0xDEAD_BEEF);
        let pins = WindowPins::new();
        st.set_enabled(false, &pins);
        assert!(st.cloaked.is_empty());
    }

    #[test]
    fn a_window_belongs_to_the_monitor_holding_its_centre() {
        let primary = monitor("A", 0, 0, 1920, 1080);
        let second = monitor("B", 1920, 0, 1920, 1080);
        let all = vec![&primary, &second];

        let left = monitor_of(rect(100, 100, 800, 600), &all).unwrap();
        assert_eq!(left.id.0, "A");
        let right = monitor_of(rect(2000, 100, 800, 600), &all).unwrap();
        assert_eq!(right.id.0, "B");
    }

    #[test]
    fn a_window_straddling_the_seam_goes_where_most_of_it_is() {
        let primary = monitor("A", 0, 0, 1920, 1080);
        let second = monitor("B", 1920, 0, 1920, 1080);
        let all = vec![&primary, &second];

        // Левый край на первом мониторе, но центр — уже на втором.
        let m = monitor_of(rect(1800, 100, 800, 600), &all).unwrap();
        assert_eq!(m.id.0, "B", "решает центр, а не угол");
    }

    #[test]
    fn a_monitor_left_of_the_primary_has_negative_coordinates() {
        let primary = monitor("A", 0, 0, 1920, 1080);
        let left = monitor("B", -1920, 0, 1920, 1080);
        let all = vec![&primary, &left];

        let m = monitor_of(rect(-1800, 100, 800, 600), &all).unwrap();
        assert_eq!(m.id.0, "B");
    }

    #[test]
    fn a_window_outside_every_monitor_belongs_to_none() {
        let primary = monitor("A", 0, 0, 1920, 1080);
        let all = vec![&primary];
        assert!(monitor_of(rect(5000, 5000, 100, 100), &all).is_none());
    }

    #[test]
    fn an_empty_monitor_list_yields_no_owner() {
        assert!(monitor_of(rect(0, 0, 100, 100), &[]).is_none());
    }

    #[test]
    fn the_monitor_filter_defaults_to_all_monitors() {
        let state = state();
        let a = monitor("A", 0, 0, 1920, 1080);
        let b = monitor("B", 1920, 0, 1920, 1080);
        let all = vec![a, b];
        assert_eq!(state.active_monitors(&all).len(), 2);
    }

    #[test]
    fn an_explicit_monitor_list_narrows_the_set() {
        let state = TilingState::new(
            true,
            LayoutParams::default(),
            InsertPolicy::Dwindle,
            Vec::new(),
            vec![MonitorId("B".to_string())],
            Vec::new(),
            false,
        );
        let a = monitor("A", 0, 0, 1920, 1080);
        let b = monitor("B", 1920, 0, 1920, 1080);
        let all = vec![a, b];
        let active = state.active_monitors(&all);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id.0, "B");
    }

    #[test]
    fn a_window_that_was_just_commanded_is_left_to_settle() {
        let mut state = state();
        state.last_command.insert(42, 1_000);
        assert!(state.is_settling(42, 1_000 + SETTLE_MS - 1));
        assert!(!state.is_settling(42, 1_000 + SETTLE_MS));
    }

    #[test]
    fn a_window_never_commanded_is_not_settling() {
        let state = TilingState::new(
            true,
            LayoutParams::default(),
            InsertPolicy::Dwindle,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        );
        assert!(!state.is_settling(42, 10_000));
    }

    #[test]
    fn disabled_tiling_does_nothing_on_a_snapshot() {
        let mut state = TilingState::new(
            false,
            LayoutParams::default(),
            InsertPolicy::Dwindle,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        );
        let pins = WindowPins::new();
        let tick = state.on_snapshot(&[], &[monitor("A", 0, 0, 1920, 1080)], &pins, 0);
        assert!(tick.is_quiet());
    }

    #[test]
    fn without_monitors_there_is_nothing_to_tile() {
        let mut state = TilingState::new(
            true,
            LayoutParams::default(),
            InsertPolicy::Dwindle,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        );
        let pins = WindowPins::new();
        assert!(state.on_snapshot(&[], &[], &pins, 0).is_quiet());
    }

    #[test]
    fn window_rect_survives_the_round_trip_through_core_rect() {
        let original = rect(-100, 50, 800, 600);
        assert_eq!(to_window_rect(to_core_rect(original)), original);
    }

    #[test]
    fn a_negative_size_does_not_wrap_around_on_conversion() {
        // Свёрнутые окна дают мусорную геометрию; она не должна превращаться
        // в гигантское беззнаковое число.
        let core = to_core_rect(rect(0, 0, -10, -10));
        assert_eq!(core.w, 0);
        assert_eq!(core.h, 0);
    }

    #[test]
    fn observed_only_covers_windows_present_in_both_views() {
        let placements = vec![
            Placement {
                window: WindowKey(1),
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                },
                visible: true,
            },
            Placement {
                window: WindowKey(2),
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                },
                visible: true,
            },
        ];
        let managed = vec![ManagedWindow {
            hwnd: 1,
            rect: rect(10, 10, 90, 90),
            monitor: MonitorId("A".to_string()),
            title: String::new(),
        }];
        let observed = observed_for(&placements, &managed);
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].window, WindowKey(1));
        assert_eq!(observed[0].rect.x, 10, "берётся НАБЛЮДАЕМАЯ геометрия");
    }
}
