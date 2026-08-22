//! Владеет оверлей-окном, рендерером и живым списком спрайтов на одном
//! потоке (ADR-013). Запускается один раз при старте. Команды приходят по
//! одному объединённому каналу с двух сторон (docs/M2_INTEGRATION_PLAN.md,
//! раздел 1): «добавить стикер» — из обработчика Tauri, события мыши/
//! клавиатуры/хоткея — из потока оверлей-окна.
//!
//! M2 (срез 1): вход/выход из режима редактирования по хоткею, затемнение
//! 50%, клик по стикеру/фону выбирает/снимает выделение, рамка с ручками.
//! M2 (срез 2): перемещение, ресайз за ручки (`Shift`/`Alt`), поворот за
//! угловое кольцо (`Shift` — шаг 15°), магнит и ограничение видимости при
//! перетаскивании, курсор по зоне.
//! M2 (срез 3, этот файл): undo/redo (снимок всего `Config` — жест/удаление/
//! дублирование это оправдывает при ~25 стикерах, docs/M2_INTEGRATION_REVIEW.md,
//! раздел 3, а не `rst_core::undo::UndoStack`: тот держит `Box<dyn Command>`,
//! который не может владеть `&mut Config`, а снимок — проще и без Rc/RefCell),
//! `Ctrl+Z`/`Ctrl+Shift+Z`/`Ctrl+Y`, `Ctrl+A`, `Delete`, `Ctrl+D`.
//! M2 (срез 4, этот файл): марка мультивыделения протяжкой по фону, диалог
//! подтверждения удаления (`Delete`/кнопка тулбара — общий `begin_delete`),
//! `Ctrl+V` из буфера (картинка/файлы; вставка в поле числа пока не
//! подключена — блокируется тем же пробелом, что и клавиатура поля ниже).
//! M2 (срез 5/6, этот файл): тулбар и панель у курсора как immediate-mode
//! виджеты (`rst_render::widgets`), числовое поле принимает клавиатуру
//! (`Panel::key_event`, docs/M2_SLICE6_REVIEW.md, пункт 2.3), `BTN_SETTINGS`
//! уходит по `coordinator_tx` на поток Tauri (докс §12) — M2 закрыт
//! полностью (ROADMAP.md).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, RECT};

use rst_audio::{AudioMixer, AudioSource};
use rst_core::AnimationClock;
use rst_core::config;
use rst_core::hittest::{self, Corner as CoreCorner, DipRect, HandleKind};
use rst_core::model::{
    Config, Hotkeys, MediaType, MonitorId, OverlapRule, Placement, Rect, Settings, Sticker,
    StickerSource, Transform, VIDEO_EXTENSIONS, VisibilityMode,
};
use rst_core::monitor_loss::{LossAction, MonitorLossTracker, MonitorSnapshot};
use rst_core::monitor_rebind::{self, MonitorBounds};
use rst_core::occluders::{self, OccluderCandidate, OccluderSet};
use rst_core::ops;
use rst_core::pinned_window::{self, PinnedWindow};
use rst_core::presets;
use rst_core::selection_set::SelectionSet;
use rst_core::snap::{self, SnapConfig};
use rst_core::transform_ops::{self, DragModifiers};
use rst_media::animation as media_animation;
use rst_media::paste;
use rst_render::{
    PresentSync,
    Box2D, Button, Checkbox, Device, HIGHLIGHT_THICKNESS_DIP, HighlightKind, Icon,
    Key, Label, NumericField, Panel, PinnedRowField, PointerEvent, Primitive, RenderError,
    SelectionBox, Slider, Sprite, Texture, TextField, TextureAtlas, VideoTextures, WidgetId,
    WindowHighlight, WindowTarget, edit_overlay, lock_indicator, marquee_visuals, pin_indicator,
    pinned_row_id,
    rasterize, solid_sprite, theme,
};
use rst_video::VideoSource;
use rst_win32::Win32Error;
use rst_win32::clipboard::{self, ClipboardImage};
use rst_win32::file_dialog;
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::input::{CursorShape, CursorZone, Handle as Win32Handle, InputEvent, Modifiers};
use rst_win32::monitors;
use rst_win32::overlay::{OverlayEvent, OverlayWindow};
use rst_win32::window_enum::{WindowInfo, WindowRect};
use rst_win32::window_pin::{self as window_pin, WindowPins};
use rst_win32::window_tracker::{WindowEvent as TrackerWindowEvent, WindowTracker};
use uuid::Uuid;

use crate::{confirm_dialog, cursor_panel, preset_picker, toolbar, window_pick_list, window_picker};

/// Мост к `Device`/`WindowTarget` (M3 step 2 разделил `rst_render::Renderer`
/// на процесс-wide устройство и цель на монитор, M3_PREP_NOTES.md §4.2).
/// Держит ссылки, а не владеет: `device` общий на весь процесс, `target` —
/// цели ровно одного монитора из [`MonitorState`]; конструируется заново на
/// каждую точку диспетчеризации в `run()` (раздел 5 — per-monitor рантайм),
/// так что существующий M1/M2-код (`create_texture_from_rgba`/`load_image`/
/// `draw`) не менял сигнатур при переходе с владеющей версии на ссылочную.
/// DPI/масштаб и ресайз цепочки — не через эту обёртку: `run()` держит их
/// как поля [`MonitorState`] и вызывает `WindowTarget` напрямую, поскольку
/// они переживают конкретный `Renderer` (тот живёт только на один вызов
/// обработчика, а масштаб монитора — весь его жизненный цикл).
struct Renderer<'a> {
    device: &'a Device,
    target: &'a mut WindowTarget,
}

impl Renderer<'_> {
    fn create_texture_from_rgba(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Texture, RenderError> {
        self.device.create_texture_from_rgba(data, width, height)
    }

    fn load_image(&self, path: &Path) -> Result<Texture, RenderError> {
        self.device.load_image(path)
    }

    /// Собрать текстурный атлас анимации (M5a) — см. `Device::create_texture_atlas`.
    fn create_texture_atlas(
        &self,
        frames: &[(Vec<u8>, Duration)],
        frame_w: u32,
        frame_h: u32,
    ) -> Result<TextureAtlas, RenderError> {
        self.device.create_texture_atlas(frames, frame_w, frame_h)
    }

    fn draw(&self, sprites: &[Sprite], sync: PresentSync) -> Result<(), RenderError> {
        self.device.draw_with_sync(&*self.target, sprites, sync)
    }

    /// Как [`Self::draw`], но с маской перекрытия на спрайт (M4) — см.
    /// `Device::draw_masked`.
    fn draw_masked(
        &self,
        sprites: &[Sprite],
        masks: &[Option<&Texture>],
        sync: PresentSync,
    ) -> Result<(), RenderError> {
        self.device.draw_masked_with_sync(&*self.target, sprites, masks, sync)
    }
}

/// Рантайм одного монитора (M3 step 4, M3_PREP_NOTES.md раздел 5.2): своё
/// окно, своя цель рендера, свои живые размер/масштаб. `Device` — общий на
/// процесс и живёт отдельно в `run()`, не здесь (раздел 4.2 — текстуры
/// грузятся один раз и рисуются на любой цели). Ключ таблицы `run()`
/// (`HashMap<MonitorId, MonitorState>`) — `MonitorId` из перечисления
/// (`rst_win32::monitors::enumerate`), совпадает с `Placement::monitor_id`.
///
/// Порядок полей важен: Rust дропает поля структур в порядке объявления,
/// поэтому `target` (DComp-цепочка/RTV на этом HWND) объявлен раньше
/// `overlay` (само окно) — иначе на шатдауне окно уничтожилось бы
/// (`WM_CLOSE` + join потока, `OverlayWindow::drop`) раньше, чем
/// освободится DComp-цепочка, построенная на его HWND
/// (docs/M3_STEP4_REVIEW.md, пункт 2.3).
struct MonitorState {
    target: WindowTarget,
    overlay: OverlayWindow,
    /// Позиция окна в физических пикселях виртуального десктопа
    /// (`Rect::x`/`y` монитора на момент последнего `create_monitor_state`
    /// или шага 3.5 ветки `MonitorsChanged`). Кэшируется отдельно от
    /// `OverlayWindow`, потому что сравнение «сдвинулся ли монитор» в шаге
    /// 3.5 должно быть дешёвым и не идти через `GetWindowRect` каждый раз;
    /// без этого поля чистая перестановка монитора (тот же размер/DPI, но
    /// другие x/y) была физически неразличима от «монитор не менялся»
    /// (M3_STEP8_REVIEW.md, пункт 2.1).
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f32,
    /// `true`, если `recover_device` не смог пересоздать `target` этого
    /// монитора на новом устройстве (стал `stale` — указывает на уже
    /// уничтоженное устройство). `redraw_all` пропускает такие мониторы
    /// целиком: без этого флага `present` на устаревшей цели гарантированно
    /// возвращал бы `DeviceLost` на каждом кадре и превращал бы КАЖДОЕ
    /// следующее redraw-событие в полное повторное восстановление всех GPU-
    /// ресурсов (docs/M3_DEVICE_RECOVERY_REVIEW.md, пункт 2.1). Снимается
    /// следующим успешным `recover_device` для этого монитора. Если `broken`
    /// у ВСЕХ мониторов сразу (все цели не пересоздались на новом
    /// устройстве) — `redraw_all` не рисует ничего и возвращает `false`,
    /// новых попыток `recover_device` больше не будет (детект идёт от
    /// `present` здоровой цели, а её не осталось): тихий чёрный экран без
    /// восстановления, кроме перезапуска процесса (M3_STEP5_6_REVIEW.md,
    /// пункт 2.6). На практике маловероятно — устройство только что создано.
    broken: bool,
}

/// Окно + `WindowTarget` + поток-форвардер событий для одного монитора —
/// общий блок для старта и для hot-plug (ветка `MonitorsChanged`,
/// M3_HOTPLUG_DESIGN.md §2). Возвращает `None` (с логом) при неудаче — так
/// же, как раньше делал стартовый цикл напрямую. `edit_active` — если
/// монитор появляется, пока режим редактирования уже активен, его окно
/// должно сразу стать интерактивным (`set_interactive`), иначе мышь на нём
/// проваливалась бы сквозь режим, как и у остальных окон в этот момент
/// (M3_PREP_NOTES.md §3.5).
#[allow(clippy::too_many_arguments)]
fn create_monitor_state(
    device: &Device,
    tx: &Sender<OverlayMessage>,
    info: &monitors::MonitorInfo,
    edit_hotkey: Option<HotkeyCombo>,
    toggle_all_hotkey: Option<HotkeyCombo>,
    mute_all_hotkey: Option<HotkeyCombo>,
    pin_focused_hotkey: Option<HotkeyCombo>,
    edit_active: bool,
    hide_from_capture: bool,
) -> Option<MonitorState> {
    let (overlay, events) = match OverlayWindow::create_on_monitor(
        info.bounds_px,
        edit_hotkey,
        toggle_all_hotkey,
        mute_all_hotkey,
        pin_focused_hotkey,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, monitor = %info.id.0, "не удалось создать оверлей-окно монитора");
            return None;
        }
    };
    if edit_active {
        overlay.set_interactive(true);
    }
    apply_capture_affinity(&overlay, hide_from_capture, &info.id);
    let (width, height) = overlay.size();
    let mut target = match WindowTarget::new(device, overlay.hwnd(), width, height) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, monitor = %info.id.0, "не удалось создать цель рендера монитора");
            return None;
        }
    };
    let dpi = overlay.dpi();
    target.set_dpi_scale(dpi as f32 / 96.0);
    let scale = target.dpi_scale();

    // Мост «события окна → общий канал координатора», по одному на монитор —
    // каждый помечает свои события своим `MonitorId`, чтобы координатор не
    // спутал координаты/геометрию разных окон (M3_PREP_NOTES.md, раздел 3.4).
    let monitor_id = info.id.clone();
    let tx_for_events = tx.clone();
    thread::spawn(move || {
        for event in events {
            if tx_for_events
                .send(OverlayMessage::Event(monitor_id.clone(), event))
                .is_err()
            {
                break;
            }
        }
    });

    Some(MonitorState {
        overlay,
        target,
        x: info.bounds_px.x,
        y: info.bounds_px.y,
        width,
        height,
        scale,
        broken: false,
    })
}

/// Применить настройку «Скрывать от захвата экрана» (SPEC.md, раздел 8) к
/// окну монитора и залогировать, если `SetWindowDisplayAffinity` не
/// сработал вообще ИЛИ сработал, но `GetWindowDisplayAffinity` не
/// подтвердил применение (на отдельных сборках Windows 11 флаг применяется
/// нестабильно — SPEC явно требует проверять результат, а не считать
/// успех вызова достаточным доказательством).
fn apply_capture_affinity(overlay: &OverlayWindow, hide: bool, monitor_id: &MonitorId) {
    match overlay.set_capture_affinity(hide) {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                monitor = %monitor_id.0,
                hide,
                "SetWindowDisplayAffinity не подтверждён GetWindowDisplayAffinity — на этой сборке Windows настройка «скрывать от захвата экрана» может не работать"
            );
        }
        Err(e) => {
            tracing::warn!(monitor = %monitor_id.0, hide, error = %e, "не удалось применить SetWindowDisplayAffinity");
        }
    }
}

/// Изменилась ли геометрия монитора относительно закэшированной на
/// `MonitorState` — позиция, размер или масштаб (шаг 3.5 ветки
/// `MonitorsChanged`, M3_STEP8_REVIEW.md, пункт 2.1). Чистая функция,
/// вынесена отдельно от обработчика ради юнит-теста: сам `MonitorState`
/// держит живые Win32/GPU-хендлы (`OverlayWindow`/`WindowTarget`), которые
/// напрямую не протестировать.
fn monitor_geometry_changed(
    cached: (i32, i32, u32, u32, f32),
    bounds_px: Rect,
    new_scale: f32,
) -> bool {
    let (x, y, w, h, scale) = cached;
    x != bounds_px.x
        || y != bounds_px.y
        || w != bounds_px.w
        || h != bounds_px.h
        || (scale - new_scale).abs() >= f32::EPSILON
}

/// Снести состояние монитора (ветка `MonitorsChanged`, M3_HOTPLUG_DESIGN.md
/// §2, чек-лист сноса): удаляет `MonitorState` (порядок дропа полей —
/// `target` раньше `overlay`, уже безопасен, docs/M3_STEP4_REVIEW.md пункт
/// 2.3; поток-форвардер окна завершается сам, когда закрывается канал
/// событий уничтоженного окна) и чистит снимки-копии (`monitor_geometry`/
/// `monitor_bounds`). Дважды используется с разным смыслом `physically_gone`
/// (M3_STEP5_6_REVIEW.md, пункт 2.3): `true` — монитор реально пропал
/// (шаг 3), тогда, если режим редактирования активен, ещё снимает с
/// выделения стикеры этого монитора, переносит `cursor_monitor`/`cursor_pos`
/// на новый основной, если курсор был там же, отменяет незавершённый жест
/// сцены на этом мониторе так же, как `CaptureLost` (окно уже уничтожено —
/// `WM_CAPTURECHANGED` от него больше не придёт), и пересобирает панели;
/// `false` — монитор физически на месте, окно сносится только ради
/// перерегистрации хоткея на новом primary (шаг 1) — трогать выделение/
/// курсор/жест живого монитора ради этого не нужно. Стикеры пропавшего
/// монитора **не трогает** ни в одном случае — их прячет
/// `MonitorLossTracker` своим `LossAction::HideSticker` отдельно; снос окна и
/// скрытие стикеров — два разных эффекта одного события.
#[allow(clippy::too_many_arguments)]
fn teardown_monitor_state(
    monitors_map: &mut HashMap<MonitorId, MonitorState>,
    monitor_geometry: &mut HashMap<MonitorId, (u32, u32, f32)>,
    monitor_bounds: &mut HashMap<MonitorId, MonitorBounds>,
    id: &MonitorId,
    edit: &mut EditState,
    cfg: &mut Config,
    sprites: &mut [(Uuid, Sprite)],
    new_primary_id: &MonitorId,
    physically_gone: bool,
) {
    monitors_map.remove(id);
    monitor_geometry.remove(id);
    monitor_bounds.remove(id);

    if !physically_gone || !edit.active {
        return;
    }

    let dead_selected: Vec<Uuid> = edit
        .selection
        .ids()
        .iter()
        .copied()
        .filter(|sid| {
            cfg.stickers
                .iter()
                .any(|s| s.id == *sid && s.placement.monitor_id == *id)
        })
        .collect();
    for sid in dead_selected {
        edit.selection.deselect(sid);
    }

    // Захватить ДО переназначения ниже — иначе сравнение в ветке марки всегда
    // false (M3_STEP5_6_REVIEW.md, пункт 2.1): пока марка активна, курсор на
    // этом мониторе всегда здесь же (каждый `MouseMove` окна ставит
    // `cursor_monitor = monitor_id`, а марка живёт только на своём окне) —
    // сравнивать нужно с состоянием ДО переезда курсора на новый primary.
    let cursor_was_here = edit.cursor_monitor == *id;
    if cursor_was_here {
        edit.cursor_monitor = new_primary_id.clone();
        edit.cursor_pos = (0.0, 0.0);
    }

    let cancel_gesture = match &edit.gesture {
        Some(Gesture::Marquee { .. }) => cursor_was_here,
        Some(other) => other.start().is_some_and(|start| {
            cfg.stickers
                .iter()
                .any(|s| s.id == start.id && s.placement.monitor_id == *id)
        }),
        None => false,
    };
    if cancel_gesture {
        match edit.gesture.take() {
            Some(Gesture::Marquee { before, .. }) => {
                edit.selection.clear();
                for sid in before {
                    edit.selection.select(sid);
                }
                edit.marquee = None;
                edit.marquee_started = false;
            }
            Some(gesture) => {
                if let Some(start) = gesture.start() {
                    apply_transform(
                        cfg,
                        sprites,
                        start.id,
                        start.placement.clone(),
                        start.transform,
                    );
                }
                edit.marquee = None;
                edit.marquee_started = false;
                edit.pending_snapshot = None;
            }
            None => {}
        }
    }

    rebuild_ui_panels(edit, cfg, monitor_geometry);
}

/// Хоткей входа/выхода из режима редактирования по умолчанию (CONFIG.md),
/// если в конфиге он не задан или не парсится.
const DEFAULT_EDIT_HOTKEY: &str = "Ctrl+Alt+S";

/// Текст тоста первого запуска (ROADMAP.md M8) — `None`, если он уже был
/// показан (`Settings::onboarding_shown`). Чистая функция: логика решения
/// «показывать или нет» и текста сообщения тестируется без канала/GPU —
/// сама отправка и запись флага остаются в `run()`.
fn onboarding_notification(cfg: &Config) -> Option<(String, String)> {
    if cfg.settings.onboarding_shown {
        return None;
    }
    let hotkey_text = cfg
        .hotkeys
        .edit_mode
        .as_deref()
        .unwrap_or(DEFAULT_EDIT_HOTKEY);
    Some(crate::i18n::onboarding_notification(
        &cfg.settings.language,
        hotkey_text,
    ))
}

/// Виртуальный код `VK_ESCAPE` (docs.microsoft.com/Virtual-Key-Codes) — выход
/// из режима редактирования. Константа, а не зависимость от `windows`: этот
/// крейт не работает с Win32-типами напрямую (CONTRIBUTING.md).
const VK_ESCAPE: u32 = 0x1B;
const VK_BACK: u32 = 0x08;
const VK_RETURN: u32 = 0x0D;
const VK_LEFT: u32 = 0x25;
const VK_RIGHT: u32 = 0x27;
const VK_DELETE: u32 = 0x2E;
const VK_A: u32 = 0x41;
const VK_D: u32 = 0x44;
const VK_V: u32 = 0x56;
const VK_Y: u32 = 0x59;
const VK_Z: u32 = 0x5A;

/// Глубина истории undo/redo — снимков `Config` (см. заметку о снимках выше).
const UNDO_CAPACITY: usize = 100;

/// Минимальный отступ от угла рамки выделения (DIP), с которого начинается
/// зона поворота (фидбэк пользователя 2026-08-09, третий раунд): раньше
/// зона поворота была кольцом вокруг угла (`ROTATE_RING_MAX_DIP=24`,
/// срабатывало и ВНУТРИ рамки) — теперь поворот работает только СНАРУЖИ
/// рамки, начиная с этого отступа от ближайшего угла, и тянется на
/// бесконечную дистанцию (как в Photoshop — весь внешний периметр после
/// выделения объекта отдан повороту, ресайз — только на самих ручках).
const ROTATE_MIN_CORNER_GAP_DIP: f64 = 35.0;

/// Задержка перед показом тултипа кнопки — с момента, когда курсор навёлся
/// (фидбэк пользователя 2026-08-10: «через 0.2 секунды после того как
/// навёлся»).
const TOOLTIP_SHOW_DELAY: Duration = Duration::from_millis(200);
/// Длительность анимации плавного появления тултипа (0 → полная непрозрачность).
const TOOLTIP_FADE_DURATION: Duration = Duration::from_millis(300);
/// Шаг перепланирования кадра анимации появления тултипа — не «на каждый
/// возможный тик», а с разумным интервалом (тот же принцип «ноль
/// пробуждений в покое» ADR-006, что у анимации стикеров: пробуждаемся,
/// только пока тултип реально ещё проявляется).
const TOOLTIP_FADE_STEP: Duration = Duration::from_millis(16);
/// Зазор между кнопкой и тултипом, DIP.
const TOOLTIP_GAP_DIP: f64 = 8.0;
/// Внутренний отступ текста в тултипе, DIP.
const TOOLTIP_PAD_DIP: f64 = 5.0;

/// Наведённая кнопка тулбара/панели у курсора: с какого момента наведена
/// (для задержки показа и анимации появления), что показать и где —
/// вычисляется заново при каждой смене наведённого виджета
/// (`update_tooltip_hover`), не персистентно между наведениями.
struct TooltipState {
    text: &'static str,
    hover_started: Instant,
    /// Границы кнопки-источника (DIP, координаты монитора `monitor_id`) —
    /// тултип позиционируется относительно них.
    anchor: Box2D,
    monitor_id: MonitorId,
}

impl TooltipState {
    /// Прозрачность тултипа сейчас: 0 до истечения `TOOLTIP_SHOW_DELAY`,
    /// затем линейно 0→1 за `TOOLTIP_FADE_DURATION`.
    fn opacity(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.hover_started);
        let Some(fade_elapsed) = elapsed.checked_sub(TOOLTIP_SHOW_DELAY) else {
            return 0.0;
        };
        if fade_elapsed >= TOOLTIP_FADE_DURATION {
            1.0
        } else {
            fade_elapsed.as_secs_f64() / TOOLTIP_FADE_DURATION.as_secs_f64()
        }
    }

    /// Следующий момент, когда тултип нужно перерисовать — `None`, если
    /// анимация появления уже завершена (`opacity` дальше не меняется без
    /// смены наведённого виджета, которая приходит обычным `MouseMove`, не
    /// через таймер).
    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let elapsed = now.saturating_duration_since(self.hover_started);
        if elapsed < TOOLTIP_SHOW_DELAY {
            Some(self.hover_started + TOOLTIP_SHOW_DELAY)
        } else if elapsed < TOOLTIP_SHOW_DELAY + TOOLTIP_FADE_DURATION {
            Some(now + TOOLTIP_FADE_STEP)
        } else {
            None
        }
    }
}

/// Длительность пульса рамки при пин/анпин по хоткею (запрос пользователя
/// 2026-08-18, вдвое увеличено по запросу 2026-08-19: «в 2 раза длиннее в
/// плане проигрывания анимации»): 0 → 100% за 1 с, 100 → 0% за следующую
/// 1 с, итого 2 с.
/// Кадр сейчас должен идти вровень с закреплённым окном, которое тащит
/// пользователь (см. [`PIN_FOLLOW_STEP`]).
fn pin_follow_active(edit: &EditState) -> bool {
    edit.pinned_follow_until
        .is_some_and(|until| until > Instant::now())
}

/// Шаг «слежения» кадра за закреплённым окном, которое пользователь прямо
/// сейчас тащит или ресайзит его собственными средствами (репорт
/// пользователя 2026-08-21: рамка, бейдж и панель «отстают, как будто окно
/// в 60 кадрах, а обводка в 20»).
///
/// Почему нельзя обойтись событиями трекера: снимок приходит после дебаунса
/// 16 мс И полного перечисления окон, то есть заметно реже кадра — вся
/// графика поверх окна рисуется по последнему снимку и отстаёт. На время
/// живого жеста планировщик получает собственный дедлайн, кадр берёт живые
/// границы окна (`pinned_window_dip_placement` → `live_rect`) и идёт вровень
/// с ним. В покое дедлайна нет — «ноль пробуждений в покое» (ADR-006)
/// сохраняется.
const PIN_FOLLOW_STEP: Duration = Duration::from_millis(8);

/// Хвост слежения после отпускания кнопки: модальный цикл окна закрывается
/// чуть раньше, чем система досылает последнее перемещение, и без хвоста
/// последний кадр мог бы остаться на предпоследней позиции.
const PIN_FOLLOW_TAIL: Duration = Duration::from_millis(200);

/// Гистерезис принудительной геометрии закреплённого окна, физические px:
/// расхождения меньше него не исправляются. Нужен, потому что наша же
/// перестановка окна порождает новое событие трекера, а перевод между
/// DWM-габаритами и `SetWindowPos`-координатами округляется — без порога
/// окно дрожало бы на пиксель бесконечно.
const PINNED_GEOMETRY_EPS_PX: i32 = 2;

/// Как часто потолок размера вправе выводить окно из развёрнутого состояния
/// (см. `EditState::pinned_unmaximized_at`).
const PINNED_UNMAXIMIZE_COOLDOWN: Duration = Duration::from_secs(1);

/// Минимальная сторона закреплённого окна при ресайзе, DIP — только чтобы
/// кламп границ/потолка 90% не выродил окно в ноль; настоящий минимум всё
/// равно навязывает сама ОС.
const PINNED_MIN_SIZE_DIP: f64 = 40.0;

/// Прямоугольник монитора в физических пикселях виртуального десктопа.
fn monitor_px_rect(bounds: &MonitorBounds) -> pinned_window::PxRect {
    pinned_window::PxRect::from_xywh(
        bounds.bounds_px.x as f64,
        bounds.bounds_px.y as f64,
        bounds.bounds_px.w as f64,
        bounds.bounds_px.h as f64,
    )
}

/// Объединяющий прямоугольник всех мониторов — граница, за которую нельзя
/// утащить закреплённое окно в режиме редактирования (см.
/// [`pinned_window::snap_move`]: удержание по десктопу, а не по монитору,
/// чтобы перетаскивание на соседний монитор осталось возможным). Мониторов
/// нет вовсе (гонка переподключения) — отдаём вырожденный прямоугольник:
/// `snap_move` в этом случае просто ничего не ограничит.
fn desktop_px_rect(monitor_bounds: &HashMap<MonitorId, MonitorBounds>) -> pinned_window::PxRect {
    let mut acc: Option<pinned_window::PxRect> = None;
    for bounds in monitor_bounds.values() {
        let r = monitor_px_rect(bounds);
        acc = Some(match acc {
            None => r,
            Some(a) => pinned_window::PxRect {
                left: a.left.min(r.left),
                top: a.top.min(r.top),
                right: a.right.max(r.right),
                bottom: a.bottom.max(r.bottom),
            },
        });
    }
    acc.unwrap_or(pinned_window::PxRect {
        left: f64::NEG_INFINITY,
        top: f64::NEG_INFINITY,
        right: f64::INFINITY,
        bottom: f64::INFINITY,
    })
}

/// Непрозрачность постоянной обводки закреплённого окна (опция
/// «Обводка на закреплённом окне»). Заметно слабее пульса: пульс — это
/// разовый отклик на действие, а эта рамка висит всё время и не должна
/// перетягивать внимание с содержимого окна.
const PINNED_OUTLINE_OPACITY: f64 = 0.55;

/// Отступ панели инструментов закреплённого окна от его кромок, DIP
/// (панель рисуется ВНУТРИ окна — см. `rebuild_pinned_panel`).
const PINNED_PANEL_INSET: f64 = 8.0;

/// Раскладка пульса (запрос пользователя 2026-08-22): 0.25 с проявления,
/// 1 с на полной непрозрачности, 0.25 с угасания. Раньше это был
/// треугольник 1 с вверх / 1 с вниз — полной яркости рамка касалась ровно
/// на мгновение, и «подержать» её было нечем.
const PIN_FLASH_FADE_IN: Duration = Duration::from_millis(250);
const PIN_FLASH_HOLD: Duration = Duration::from_millis(1000);
const PIN_FLASH_FADE_OUT: Duration = Duration::from_millis(250);
const PIN_FLASH_DURATION: Duration = PIN_FLASH_FADE_IN
    .saturating_add(PIN_FLASH_HOLD)
    .saturating_add(PIN_FLASH_FADE_OUT);
/// Шаг перепланирования кадра пульса — тот же принцип «ноль пробуждений в
/// покое» (ADR-006), что у тултипа: 16 мс ≈ 60 Гц, дешевле некуда.
const PIN_FLASH_STEP: Duration = Duration::from_millis(16);
/// Цвет рамки пульса ПРИ ЗАКРЕПЛЕНИИ — акцент проекта: тот же `#3c9898`,
/// что `--accent` в настройках (НЕ синий `SLIDER_FILL` D3D-темы — тот для
/// другой семантики).
const PIN_FLASH_COLOR_PIN: [u8; 3] = [0x3c, 0x98, 0x98];
/// Цвет рамки пульса ПРИ ОТКРЕПЛЕНИИ (запрос пользователя 2026-08-19:
/// «обводка становится красного цвета, а не цвета cyan») — красный, чтобы
/// визуально отличаться от закрепления и читаться как «окно освобождено»,
/// не «окно занято».
const PIN_FLASH_COLOR_UNPIN: [u8; 3] = [0xd0, 0x3c, 0x3c];
/// Толщина рамки пульса — в 1.5 раза толще обычной рамки выделения
/// (`HIGHLIGHT_THICKNESS_DIP`, запрос пользователя 2026-08-19: «обводку в
/// 1.5 раза больше»); намеренно отдельная константа — амбарная рамка
/// выделения закреплённого окна в edit-mode (`HighlightKind::Pin`,
/// `pinned_selection`) толщину не меняет, только пульс пин/анпин.
const PIN_FLASH_THICKNESS_DIP: f64 = 1.5 * HIGHLIGHT_THICKNESS_DIP;

/// Какое действие запустило пульс — определяет цвет рамки
/// ([`PinFlash::color`]): закрепление — акцент проекта, открепление —
/// красный (запрос пользователя 2026-08-19).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PinFlashKind {
    Pin,
    Unpin,
}

/// Активный пульс рамки закрепляемого/открепляемого окна (Ctrl+Alt+R):
/// `hwnd` целевого окна + момент старта. Временное состояние, живёт ровно
/// `PIN_FLASH_DURATION` (истекает в цикле `run()`), рисуется ВСЕГДА — и в
/// обычном режиме, и в edit-mode: пин — фича нормального режима, пульс —
/// его визуальный отклик, редравы крутит планировщик анимаций тем же
/// дедлайном, что тултип/анимации стикеров (см. `next_deadline`).
struct PinFlash {
    hwnd: isize,
    started_at: Instant,
    kind: PinFlashKind,
}

impl PinFlash {
    fn new(hwnd: isize, kind: PinFlashKind) -> Self {
        Self {
            hwnd,
            started_at: Instant::now(),
            kind,
        }
    }

    /// Цвет рамки для текущего действия — см. [`PIN_FLASH_COLOR_PIN`]/
    /// [`PIN_FLASH_COLOR_UNPIN`].
    fn color(&self) -> [u8; 3] {
        match self.kind {
            PinFlashKind::Pin => PIN_FLASH_COLOR_PIN,
            PinFlashKind::Unpin => PIN_FLASH_COLOR_UNPIN,
        }
    }

    /// Прозрачность рамки сейчас: трапеция — проявление
    /// [`PIN_FLASH_FADE_IN`], удержание [`PIN_FLASH_HOLD`] на единице,
    /// угасание [`PIN_FLASH_FADE_OUT`] (0 после истечения).
    fn opacity(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed >= PIN_FLASH_DURATION {
            return 0.0;
        }
        if elapsed < PIN_FLASH_FADE_IN {
            return elapsed.as_secs_f64() / PIN_FLASH_FADE_IN.as_secs_f64();
        }
        let after_hold = PIN_FLASH_FADE_IN.saturating_add(PIN_FLASH_HOLD);
        if elapsed < after_hold {
            return 1.0;
        }
        1.0 - (elapsed - after_hold).as_secs_f64() / PIN_FLASH_FADE_OUT.as_secs_f64()
    }

    /// Следующий момент, когда пульс нужно перерисовать (амплитуда снова
    /// изменится) — `None`, когда анимация завершена и больше не меняется.
    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        (now.saturating_duration_since(self.started_at) < PIN_FLASH_DURATION)
            .then(|| now + PIN_FLASH_STEP)
    }

    /// Истёк ли пульс — пора вычищать из `EditState::pin_flashes`.
    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= PIN_FLASH_DURATION
    }
}

/// Длительность показа баннера предупреждений оверлея (конфликт хоткея и
/// т.п.) — короткоживущий, авто-dismiss; шаг перепланирования — тот же
/// `PIN_FLASH_STEP` (16 мс ≈ 60 Гц).
const BANNER_DURATION: Duration = Duration::from_secs(5);
/// Высота баннера, DIP.
const BANNER_HEIGHT: f64 = 28.0;
/// Внутренний отступ текста баннера, DIP.
const BANNER_PAD: f64 = 12.0;
/// Зазор баннера от верхнего края монитора, DIP.
const BANNER_TOP_GAP: f64 = 8.0;
/// `WidgetId` панели баннера (локальное пространство оверлея, свободное от
/// остальных панелей: PINNED_* — 300+, toolbar/picker/preset — свои базы).
const BANNER_PANEL_ID: WidgetId = 901;

/// Короткоживущий баннер-предупреждение, который resticker рисует САМ в
/// оверлее (решение координатора 2026-08-18: tray-баллун `Shell_NotifyIcon`
/// молча не рендерится на Windows 11 25H2 — доказано живым стендом —
/// поэтому критические предупреждения не должны полагаться на него одного;
/// баллун остаётся безвредным fallback для ОС, где он ещё работает).
/// Авто-dismiss через `BANNER_DURATION`, дедлайн — тот же паттерн, что у
/// [`PinFlash`]/тултипа.
struct BannerState {
    text: String,
    monitor_id: MonitorId,
    shown_at: Instant,
}

impl BannerState {
    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        (now.saturating_duration_since(self.shown_at) < BANNER_DURATION).then(|| now + PIN_FLASH_STEP)
    }

    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.shown_at) >= BANNER_DURATION
    }
}

/// Показать баннер предупреждения на мониторе `monitor_id` (обычно
/// primary — там живут глобальные хоткеи, там же их конфликты). Перезаписывает
/// предыдущий баннер (новое предупреждение важнее старого). Текст — уже
/// локализованный, как у `i18n::hotkey_conflict_notification`.
fn show_banner(edit: &mut EditState, monitor_id: &MonitorId, text: String) {
    edit.banner = Some(BannerState {
        text,
        monitor_id: monitor_id.clone(),
        shown_at: Instant::now(),
    });
}

pub enum OverlayCommand {
    AddSticker(PathBuf),
    /// Заменить `cfg.settings` целиком (окно настроек, вкладка «Общие») —
    /// координатор остаётся единственным писателем `config.json`
    /// (докком `add_sticker`/`OverlayHandle`): Tauri-поток не трогает диск
    /// напрямую, чтобы не гонять запись параллельно с координатором.
    UpdateSettings(Settings),
    /// Заменить `cfg.hotkeys` целиком (вкладка «Управление»). Применяется
    /// только к `config.json` — живая перерегистрация `RegisterHotKey` в
    /// этом срезе не реализована (хоткеи регистрируются один раз при
    /// создании окна монитора), эффект — после перезапуска resticker.
    UpdateHotkeys(Hotkeys),
    SetStickerEnabled(Uuid, bool),
    DeleteSticker(Uuid),
    ResetStickerPosition(Uuid),
    ResetStickerTransform(Uuid),
    RelinkSticker(Uuid, PathBuf),
    ResetAllStickers,
    DeleteAllStickers,
    /// «Показать/скрыть все стикеры» с главного потока Tauri (пункт меню
    /// трея) — та же логика, что у глобального хоткея
    /// [`OverlayEvent::ToggleAllStickers`] и `BTN_TOGGLE_ALL` панели у
    /// курсора, просто с другого источника (меню трея живёт вне оверлей-
    /// потока, `OverlayEvent` ему недоступен).
    ToggleAllStickers,
    /// Сохранить текущую расстановку стикеров как новый пресет (M7,
    /// SPEC.md §11) — снимок `cfg.stickers`, имя из настроек.
    SavePreset(String),
    /// Применить пресет по id: заменяет `cfg.stickers` на его стикеры
    /// (только существующие источники — `rst_core::presets::apply_preset`),
    /// пересобирает спрайты/анимации/видео заново. Недостающие элементы
    /// (см. `ApplyPresetOutcome::missing`) уходят обратно в Tauri через
    /// [`CoordinatorRequest::PresetMissingElements`].
    ApplyPreset(Uuid),
    RenamePreset(Uuid, String),
    DeletePreset(Uuid),
    /// Экспортировать пресет в отдельный JSON-файл по выбранному пути
    /// (диалог сохранения — на стороне Tauri, сюда приходит готовый путь).
    ExportPreset(Uuid, PathBuf),
    /// Импортировать пресет из файла (диалог открытия — на стороне Tauri) —
    /// добавляет в `cfg.presets` со свежим id (`presets::import_preset_from_file`).
    ImportPreset(PathBuf),
    /// Добавить правило в денй-лист закрепления (SPEC.md, «Закрепление
    /// окна») — вкладка «Денй-лист» окна настроек. Эффект денй-листа узкий:
    /// блокирует закрепление хоткеем (`rst_core::occluders::is_denylisted`)
    /// и прячет совпавшие окна из списка выбора — ни на что другое не влияет.
    AddDenylistRule(OverlapRule),
    /// Удалить правило денй-листа по индексу (у `OverlapRule` нет id — адрес
    /// строки и есть индекс, который видит UI; канал команд FIFO, поэтому
    /// индексы UI и координатора не расходятся).
    RemoveDenylistRule(usize),
    Shutdown,
}

/// Запрос координатора к главному потоку Tauri — обратный существующему
/// `OverlayCommand` канал (docs/M2_WIRING_PLAN.md, раздел 12): сообщения,
/// которым нужен Tauri/окно, а не чистый `Config`. Читается отдельной
/// задачей в `main::setup`, `Sender` держит координатор в `EditState`.
pub enum CoordinatorRequest {
    /// Открыть окно настроек (кнопка `BTN_SETTINGS` панели у курсора).
    OpenSettings,
    /// Применение пресета (M7) оставило часть стикеров неприменённой —
    /// их источник недоступен (`StickerSource::File`, путь не существует).
    /// Main.rs форвардит это событием Tauri (`preset-missing-elements`) в
    /// окно настроек; текст диалога — на стороне UI (CONFIG.md, «Загрузка
    /// с недостающими элементами»).
    PresetMissingElements(Vec<(Uuid, PathBuf)>),
    /// Показать баллонное уведомление трея (`TrayIcon::show_balloon`) —
    /// иконка трея живёт на главном потоке Tauri (`.manage()`), оверлей-
    /// поток её не видит, поэтому запрос идёт через тот же обратный канал,
    /// что `OpenSettings`. Используется для предупреждений, которые раньше
    /// уходили только в лог (`HotkeyConflict`, `PinAccessDenied`) — SPEC.md
    /// требует показать их пользователю, не только записать в файл лога.
    ShowNotification { title: String, body: String },
    /// Список пресетов изменился (сохранение/переименование/удаление/
    /// импорт — не `ApplyPreset`, он не трогает сам список). Main.rs
    /// пересобирает пункты меню трея (M7, «быстрое переключение из трея» —
    /// ROADMAP.md) из `(id, name)`, не полного `Preset` (имя — единственное,
    /// что нужно для пункта меню).
    PresetsChanged(Vec<(Uuid, String)>),
}

/// Сообщение объединённого канала координатора: команда от Tauri, событие от
/// потока оверлей-окна конкретного монитора (M3: несколько окон на процесс —
/// событие несёт `MonitorId`, чтобы координатор не спутал координаты/геометрию
/// одного монитора с другим, M3_PREP_NOTES.md §3.4), или периодический
/// «будильник» автомата потери монитора (M3_HOTPLUG_DESIGN.md §1).
enum OverlayMessage {
    Command(OverlayCommand),
    Event(MonitorId, OverlayEvent),
    Tick,
    /// Снимок кэша окон от `WindowTracker` (M4_WINDOW_TRACKER_DESIGN.md §6) —
    /// не привязан к монитору, форвардится тем же паттерном, что и per-monitor
    /// события: поток-мост копирует `WindowEvent` трекера в общий канал.
    Windows(TrackerWindowEvent),
    /// Будильник планировщика анимации (M5a, docs/M5A_ANIMATION_DESIGN.md §5):
    /// отправлен потоком-планировщиком, когда истёк ближайший дедлайн кадра
    /// хотя бы одной анимации. В отличие от `Tick` — переменный интервал, а
    /// не раз в секунду: планировщик спит ровно до дедлайна, который ему
    /// последним прислал координатор.
    AnimationTick,
}

/// Период тика автомата потери монитора (M3_HOTPLUG_DESIGN.md §1):
/// `monitor_loss::LOSS_TIMEOUT` (20 с) не может истечь сам по себе — цикл
/// координатора чисто событийный (ADR-006), периодических механизмов в
/// кодовой базе больше нет нигде — тик существует ровно для того, чтобы дать
/// таймеру шанс истечь без какого-либо внешнего события. Секундная точность
/// не нужна, важно лишь не ждать следующего `WM_DISPLAYCHANGE` неопределённо
/// долго.
///
/// Через сон (`Instant` монотонен и не движется, пока процесс не исполняется,
/// а тик — единственный источник промежуточных вызовов `on_monitor_snapshot`)
/// таймер фактически ставится на паузу: если монитор пропал прямо перед сном,
/// 20-секундный отсчёт стартует не с момента пропажи, а с первого снапшота
/// **после пробуждения** (`reenumerate_monitors` на `SystemResumed`) — время
/// сна в отсчёт не идёт. Врождённо событийной модели, не баг
/// (M3_SESSION_SLEEP_REVIEW.md, пункт 2.2).
const LOSS_TICK_PERIOD: Duration = Duration::from_secs(1);

/// Период опроса `VideoSource::try_recv_frame`/`try_recv_audio_samples` при
/// хотя бы одном играющем видео (M5b, docs/M5B_VIDEO_DESIGN.md §6) —
/// переиспользует планировщик анимации (`anim_deadline_tx`/`AnimationTick`,
/// M5a §5), просто как ещё один источник ближайшего дедлайна, а не отдельный
/// поток: декодер сам держит темп по PTS (`rst_video::decoder::Pacing`),
/// координатору не нужна точность лучше «часто достаточно, чтобы не
/// заметить» — 60 Гц с запасом покрывает типичную частоту кадров видео.
const VIDEO_POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Ручка для отправки команд оверлей-потоку; `Drop` останавливает поток.
pub struct OverlayHandle {
    tx: Sender<OverlayMessage>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayHandle {
    pub fn send(&self, cmd: OverlayCommand) {
        let _ = self.tx.send(OverlayMessage::Command(cmd));
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        let _ = self
            .tx
            .send(OverlayMessage::Command(OverlayCommand::Shutdown));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Запустить оверлей-окно + рендерер на отдельном потоке. `cfg` передаётся
/// по значению — существующие стикеры (сохранение/восстановление между
/// запусками, ROADMAP.md M1) загружаются сразу в первый кадр.
/// `coordinator_tx` — канал запросов координатора к главному потоку Tauri
/// (docs/M2_WIRING_PLAN.md, раздел 12): `Receiver` остаётся в `main` и
/// читается задачей из `setup`.
pub fn start(
    config_path: PathBuf,
    cfg: Config,
    coordinator_tx: Sender<CoordinatorRequest>,
) -> OverlayHandle {
    let (tx, rx) = mpsc::channel::<OverlayMessage>();
    let thread = thread::spawn({
        let tx = tx.clone();
        move || run(config_path, cfg, tx, rx, coordinator_tx)
    });
    OverlayHandle {
        tx,
        thread: Some(thread),
    }
}

/// Стартовый снимок стикера на момент `MouseDown` — жест всегда считается
/// от него (docs/M2_INTEGRATION_PLAN.md, раздел 8): не копится ошибка
/// округления, и `CaptureLost` может откатить незавершённый жест.
struct GestureStart {
    id: Uuid,
    placement: Placement,
    transform: Transform,
}

/// Активный жест редактирования. Захватывается в `MouseDown`, применяется в
/// `MouseMove`, завершается в `MouseUp` (или отменяется в `CaptureLost`).
enum Gesture {
    Drag {
        start: GestureStart,
        grab_dx: f64,
        grab_dy: f64,
    },
    Resize {
        start: GestureStart,
        handle: HandleKind,
        grab: (f64, f64),
    },
    Rotate {
        start: GestureStart,
        grab: (f64, f64),
    },
    /// Протяжка рамки мультивыделения по фону (SPEC 3.2). Не мутирует
    /// `Config` — только `edit.selection`/`edit.marquee` — поэтому не несёт
    /// `GestureStart` и не откатывается через `apply_transform`. `before` —
    /// выделение на момент `MouseDown` (до того, как `rubber_band` начал его
    /// менять) — нужно, чтобы `CaptureLost` мог вернуть выделение к тому, что
    /// было до марки, а не оставить «зависшим» на последнем `MouseMove`
    /// (docs/M2_SLICE4_REVIEW.md, пункт 3).
    Marquee {
        anchor: (f64, f64),
        before: Vec<Uuid>,
    },
}

impl Gesture {
    /// `None` для [`Gesture::Marquee`] — ей нечего откатывать в модели
    /// (докс M2_WIRING_PLAN.md, раздел 8).
    fn start(&self) -> Option<&GestureStart> {
        match self {
            Gesture::Drag { start, .. } => Some(start),
            Gesture::Resize { start, .. } => Some(start),
            Gesture::Rotate { start, .. } => Some(start),
            Gesture::Marquee { .. } => None,
        }
    }
}

/// Активный жест перемещения/ресайза выделенного закреплённого окна (SPEC
/// «Закрепление окна», пункт 9) — параллельно [`Gesture`] для стикеров:
/// `PinnedWindow` не хранит `Placement`/`Transform`, окно двигает/ресайзит
/// РЕАЛЬНЫЙ HWND через `WindowPins::move_resize`. `start_placement` — DIP-
/// геометрия окна на момент `MouseDown` ([`window_rect_to_placement`]),
/// локальная `monitor_id`: та же система координат, что у `GestureStart`
/// стикера, пересчитывается заново каждый `MouseMove`, не копится ошибка
/// округления. Поворота нет — окна ОС не вращаются.
enum PinnedGesture {
    Drag {
        hwnd: isize,
        monitor_id: MonitorId,
        start_placement: Placement,
        grab_dx: f64,
        grab_dy: f64,
    },
    Resize {
        hwnd: isize,
        monitor_id: MonitorId,
        start_placement: Placement,
        handle: HandleKind,
        grab: (f64, f64),
    },
}

/// Минимальная протяжка (DIP), после которой клик по фону считается началом
/// марки, а не простым кликом со снятием выделения (docs/M2_WIRING_PLAN.md,
/// раздел 8).
const MARQUEE_THRESHOLD_DIP: f64 = 4.0;

/// Размер клетки шахматки скрытых стикеров, DIP (SPEC.md 3.7: «клетка 16×16
/// логических пикселей»).
const CHECKERBOARD_CELL_DIP: f64 = 16.0;

/// Зона под курсором в режиме редактирования (docs/M2_INTEGRATION_PLAN.md,
/// раздел 7): у выделенного стикера — зона поворота (весь внешний периметр,
/// фидбэк пользователя 2026-08-09), ручки ресайза, тело; иначе — любой
/// видимый стикер под курсором или фон.
#[derive(Debug)]
enum Zone {
    Background,
    StickerBody(Uuid),
    ResizeHandle(Uuid, HandleKind),
    /// Угол в градусах (экранная конвенция) для курсора — направление от
    /// центра стикера к БЛИЖАЙШЕМУ углу его рамки в мировом пространстве, с
    /// учётом текущего поворота стикера (см. `nearest_corner_world_angle_deg`).
    Rotate(Uuid, i32),
}

/// Состояние режима редактирования (docs/M2_INTEGRATION_PLAN.md, раздел 2).
struct EditState {
    active: bool,
    selection: SelectionSet,
    gesture: Option<Gesture>,
    snap: SnapConfig,
    /// Снимки `Config` до последних `UNDO_CAPACITY` действий (см. заметку
    /// о снимках вместо `rst_core::undo::UndoStack` вверху файла).
    undo_stack: Vec<Config>,
    /// Снимки, отменённые через `Ctrl+Z` — доступны для `Ctrl+Y`/`Ctrl+Shift+Z`
    /// до следующего нового действия (стандартная семантика редакторов).
    redo_stack: Vec<Config>,
    /// `Config` на момент `MouseDown`, ещё не в `undo_stack`: жест кладёт
    /// снимок в историю только на `MouseUp`, и только если что-то реально
    /// изменилось — иначе клик без движения тратил бы шаг истории
    /// (docs/M2_SLICE_REVIEW.md, пункт 1).
    pending_snapshot: Option<Config>,
    /// Открытый модал подтверждения удаления (`begin_delete`); пока `Some`,
    /// модал блокирует и сцену, и историю (docs/M2_WIRING_PLAN.md, раздел 7).
    confirm: Option<ConfirmState>,
    /// Открытая панель выбора окон (M4, docs/M4_WINDOW_PICKER_DESIGN.md §1,
    /// §7.7): в отличие от `confirm`, НЕ модальна для мыши — клик мимо неё
    /// уходит в сцену как обычно (позволяет двигать стикер, не закрывая
    /// панель). Клавиатура блокируется, кроме `Esc`, — как у `confirm`
    /// (см. `handle_key`): панель редактирует ОДИН конкретный стикер, и
    /// хоткеи вроде `Delete`/`Ctrl+D`, сработавшие по текущему выделению
    /// параллельно с открытой панелью, были бы путающими.
    window_picker: Option<WindowPickerState>,
    /// Открытая панель быстрого переключения пресетов (M7, SPEC.md §3.8;
    /// `preset_picker.rs`) — в отличие от `window_picker`, модальна: пока
    /// открыта, блокирует и мышь (клик мимо закрывает без изменений), и
    /// клавиатуру, кроме `Esc` (см. `handle_key`) — она не редактирует
    /// конкретный стикер, а предлагает одноразовый выбор действия, ближе
    /// по семантике к `confirm`, чем к `window_picker`.
    preset_picker: Option<PresetPickerState>,
    /// Установлен кликом по `TB_LAYERS` (`handle_toolbar_up`) — открытие
    /// нуждается в `window_snapshot`, которого нет в `handle_toolbar_up`
    /// (дизайн §5.2); фактическое открытие происходит в цикле `run()`,
    /// где снимок под рукой, сразу после обработки текущего сообщения.
    pending_open_picker: Option<Uuid>,
    /// Установлен кликом по `BTN_ADD_WINDOW` (`handle_cursor_panel_up`) — тот
    /// же повод, что у `pending_open_picker`: открытие списка нуждается в
    /// `window_snapshot`, которого нет в `handle_cursor_panel_up`; несёт
    /// монитор, на котором открывать панель (кнопка нажата на нём же).
    pending_open_pick_list: Option<(MonitorId, PickListPurpose)>,
    /// Открытый список окон для закрепления как стикер (M6, SPEC.md §5.1;
    /// `window_pick_list.rs`) — установлен кликом по `BTN_ADD_WINDOW`, снят
    /// кликом по строке (закрепляет выбранное окно), кликом мимо панели или
    /// `Esc`. Модальна для мыши и клавиатуры, тем же паттерном, что
    /// `preset_picker` (список действий, а не редактор конкретного
    /// стикера): клик мимо панели закрывает её без изменений (докком
    /// `preset_picker.rs`).
    ///
    /// Заменил прежний режим «наведение+клик по реальному окну на экране»
    /// (`picking_window`/`pick_hover`, `WindowHighlight`): пользователь не
    /// видел, какое именно окно выбирает, и жаловался, что кнопка «выбирает
    /// какое-то текущее окно само» (фидбэк 2026-08-10) — явный список
    /// названий/иконок снимает вопрос «что именно я сейчас выбираю».
    window_pick_list: Option<WindowPickListState>,
    /// Закреплённые окна других приложений (редизайн пинов, SPEC.md
    /// «Закрепление окна»; `rst_core::pinned_window::PinnedWindow`) — ЧИСТО
    /// РАНТАЙМ-состояние: никогда не пишется в config.json, не
    /// восстанавливается на рестарте и не попадает в пресеты (в отличие от
    /// `Sticker`/`StickerSource::Window`). Наполняется кликом по строке
    /// списка выбора окна (`pin_window`, этот срез) и позже — хоткеем
    /// (задача проводки координатора). Правит содержимым (замки,
    /// соседские правила, открепление) и читает его координатор — по
    /// `hwnd` (обычное число: `isize`, тот же формат, что `WindowInfo::hwnd`
    /// после `as isize`).
    pinned_windows: Vec<PinnedWindow>,
    /// Выделенное закреплённое окно (клик по его прямоугольнику в
    /// edit-mode, SPEC «Закрепление окна», пункт 9) — `hwnd` как обычное
    /// число. Параллельно `selection` (стикеры): выделение либо на
    /// стикерах, либо на закреплённом окне, клик по одному снимает другое.
    /// `None` — ни одно закреплённое окно не выделено.
    pinned_selection: Option<isize>,
    /// Открытая панель свойств закреплённого окна (минимальная: только
    /// «Открепить» — замки/правила соседства скрыты из UI по решению
    /// пользователя 2026-08-18, код остался спящим; см. `rebuild_pinned_panel`)
    /// — есть, пока `pinned_selection` указывает на окно, панель
    /// пересобирается на каждое изменение (тот же паттерн, что
    /// `window_picker`).
    pinned_panel: Option<PinnedPanelState>,
    /// Активный жест перемещения/ресайза выделенного закреплённого окна
    /// (SPEC «Закрепление окна», пункт 9: «move/resize handles reusing the
    /// existing generic sticker resize-handle system») — параллельно
    /// `gesture` для стикеров: `PinnedWindow` не хранит `Placement`/
    /// `Transform`, поэтому жест правит РЕАЛЬНЫЙ HWND через
    /// `WindowPins::move_resize`, а не `apply_transform`/`cfg`. Захватывается
    /// в `MouseDown`, применяется в `MouseMove`, завершается в `MouseUp`/
    /// `CaptureLost` — тот же жизненный цикл, что у `gesture`.
    pinned_gesture: Option<PinnedGesture>,
    /// hwnd'ы соседско-слотовых окон, временно поднятых поверх всех, пока
    /// они держали фокус переднего плана (`surface_topmost_temporarily` —
    /// SPEC, пункт 4); `maintain_pinned_windows` возвращает их в слоты на
    /// потере фокуса. Рантайм-состояние движка закрепления, не конфиг.
    surfaced_pins: HashSet<isize>,
    /// Активные пульсы рамки при пин/анпин по хоткею Ctrl+Alt+R
    /// ([`PinFlash`]; запрос пользователя 2026-08-18) — временные состояния
    /// на 1 с: пуш в `pin_window`/`unpin_window` (оба направления, по
    /// подтверждению пользователя), истечение и чистка — в цикле `run()`
    /// (тот же паттерн транзиентного тайминга, что `tooltip`). Рисуются
    /// ВСЕГДА, вне зависимости от `active`: пин — фича нормального режима.
    pin_flashes: Vec<PinFlash>,
    /// Последняя геометрия каждого закреплённого окна (DWM-координаты, как в
    /// снимках трекера) — по ней [`enforce_pinned_geometry`] отличает «окно
    /// только что переехало» от «стоит на месте»: магнит к кромкам монитора
    /// должен срабатывать по факту перемещения, а не притягивать окно,
    /// которое пользователь сознательно поставил в паре пикселей от края и
    /// больше не трогает. Чисто рантайм, чистится вместе с откреплением.
    pinned_last_rects: HashMap<isize, WindowRect>,
    /// Момент, когда закреплённое окно последний раз было замечено в живом
    /// жесте (перетаскивание/ресайз силами самой ОС). Пока он свежее
    /// [`PIN_FOLLOW_TAIL`], планировщик держит кадровый дедлайн
    /// [`PIN_FOLLOW_STEP`] — рамка, бейдж и панель идут вровень с окном.
    pinned_follow_until: Option<Instant>,
    /// Когда потолок размера в последний раз выводил окно из развёрнутого
    /// состояния. Разворот — это тоже «расширение до 100%», и потолок его
    /// снимает, но приложение вправе развернуть себя обратно (или это
    /// сделает snap-раскладка Windows). Спорить с ним каждые 16 мс значит
    /// получить мигание, поэтому повторная попытка не раньше
    /// [`PINNED_UNMAXIMIZE_COOLDOWN`].
    pinned_unmaximized_at: HashMap<isize, Instant>,
    /// Активный баннер предупреждения оверлея ([`BannerState`]; решение
    /// координатора 2026-08-18 — заменяет невидимый на Win11 25H2
    /// tray-баллун для критических предупреждений). Рисуется в оверлее на
    /// своём мониторе, живёт `BANNER_DURATION`, чистится в цикле `run()`.
    banner: Option<BannerState>,
    /// Анимация только что добавленного стикера (M5a,
    /// docs/M5A_ANIMATION_DESIGN.md §5; потоковый вариант — ROADMAP.md M5a
    /// «потоковый режим») — `add_sticker` собирает атлас/открывает
    /// потоковый декодер (нужен `Renderer`/`Device`, которого нет в
    /// `run()`) и кладёт результат сюда вместо прямой записи в `animations`
    /// (локальная переменная `run()`, недоступная на глубине вызова); цикл
    /// `run()` забирает его в `animations` сразу после обработки текущего
    /// сообщения, тем же паттерном, что `pending_open_picker`.
    pending_animation: Option<(Uuid, PendingAnimation)>,
    /// Видео только что добавленного стикера (M5b, docs/M5B_VIDEO_DESIGN.md
    /// §6) — тот же паттерн, что `pending_animation`: `add_sticker` открывает
    /// `VideoSource`/`AudioSource` (нужен `Device`/`AudioMixer`, которых нет
    /// в его вызывающем коде на этой глубине) и кладёт готовый `VideoPlayback`
    /// сюда вместо прямой записи в `videos` (локальная переменная `run()`);
    /// цикл `run()` забирает его сразу после обработки текущего сообщения.
    pending_video: Option<(Uuid, VideoPlayback)>,
    /// Текущая рамка марки для отрисовки (`anchor_x, anchor_y, cur_x, cur_y`,
    /// DIP) — `None`, если марка не тянется в этот момент.
    marquee: Option<(f64, f64, f64, f64)>,
    /// Протяжка марки превысила порог (`MARQUEE_THRESHOLD_DIP`) — отличает
    /// «клик по фону» (снимает выделение на `MouseUp`) от настоящей марки.
    marquee_started: bool,
    /// Тулбар выделенного стикера — есть, когда `active && selection.len() ==
    /// 1 && marquee.is_none()` (docs/M2_WIRING_PLAN.md, раздел 4). Мультивыделение
    /// (`docs/M2_MULTISELECT_TOOLBAR_NOTES.md`) — следующий срез: билдер уже
    /// поддерживает `opacity: None`, но действия батчем сюда не подключены.
    toolbar: Option<Panel>,
    /// Панель у курсора — есть, пока `active` (раздел 4).
    cursor_panel: Option<Panel>,
    /// Тултип наведённой кнопки тулбара/панели у курсора (фидбэк
    /// пользователя 2026-08-10) — `None`, если курсор не над кнопкой с
    /// текстом подсказки.
    tooltip: Option<TooltipState>,
    /// Кто держит текущий указательный жест начиная с `MouseDown` (раздел 5).
    /// Модал сюда не входит — он перехватывается раньше отдельной веткой.
    pointer_owner: PointerOwner,
    /// `Config` на момент первого изменения ползунка прозрачности тулбара —
    /// аналог `pending_snapshot` для UI-жеста (не для жеста сцены), раздел 6.
    ui_pending_snapshot: Option<Config>,
    /// Последняя известная позиция курсора, DIP относительно `cursor_monitor`
    /// — источник позиции для `cursor_panel` при пересборках, не вызванных
    /// `MouseMove` (Ctrl+D, undo/redo и т.п., где курсор не двигался, но
    /// панель должна остаться там же). Обновляется на каждом `MouseMove`.
    cursor_pos: (f64, f64),
    /// Монитор, к которому относится `cursor_pos` (M3): какое окно последним
    /// прислало `MouseMove`. `redraw` рисует панель у курсора и марку только
    /// на этом мониторе — оба гарантированно принадлежат ровно одному окну
    /// на время жеста (мышь захвачена этим окном, M3_PREP_NOTES.md §3.4/3.5).
    cursor_monitor: MonitorId,
    /// Канал запросов координатора к главному потоку Tauri (`BTN_SETTINGS`,
    /// docs/M2_WIRING_PLAN.md, раздел 12). Здесь, а не отдельным параметром
    /// `handle_cursor_panel_up`, — чтобы обработчики UI могли слать запрос
    /// через `edit`.
    coordinator_tx: Sender<CoordinatorRequest>,
}

/// Кто получает события указателя после `MouseDown` (docs/M2_WIRING_PLAN.md,
/// раздел 5): решение фиксируется на `MouseDown` и не меняется до `MouseUp`/
/// `CaptureLost` — `dragging` больше не определяет маршрут (и ползунок, и
/// жест сцены держат Win32-захват одинаково).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointerOwner {
    None,
    Toolbar,
    CursorPanel,
    WindowPicker,
    /// Панель свойств закреплённого окна (SPEC «Закрепление окна», пункт 9)
    /// — тот же паттерн, что `WindowPicker`.
    PinnedPanel,
    Scene,
}

/// Открытый модал подтверждения удаления (docs/M2_WIRING_PLAN.md, раздел 6/7).
struct ConfirmState {
    /// Снимок `Config` на момент открытия модала — именно он уйдёт в undo по
    /// «Удалить», а не текущий `cfg` (выделение не меняется, пока модал
    /// открыт, но так инвариант проще и не зависит от этого факта).
    snapshot: Config,
    /// Id стикеров к удалению, зафиксированные на момент открытия.
    ids: Vec<Uuid>,
    panel: Panel,
    /// Монитор, вызвавший удаление (M3) — модал рисуется только в кадре
    /// этого монитора; остальные стикеры к удалению могут жить на других
    /// мониторах, это не меняет, где показывается сам диалог.
    monitor_id: MonitorId,
}

/// Открытая панель выбора окон (M4, docs/M4_WINDOW_PICKER_DESIGN.md §1):
/// правит `VisibilityRule` РОВНО ОДНОГО стикера (`sticker_id`). Панель
/// хранится собранной (не пересобирается на каждый кадр) — как `toolbar`/
/// `cursor_panel`/`ConfirmState.panel` — чтобы `Panel` могла держать
/// hover/armed-состояние своих чекбоксов между кадрами; `rebuild_window_picker`
/// перестраивает её заново при мутации `cfg` или новом снимке окон.
struct WindowPickerState {
    sticker_id: Uuid,
    panel: Panel,
    /// Сколько строк списка пропущено сверху (виртуализация, дизайн §7.4).
    scroll: usize,
    /// Монитор, на котором рисуется панель (тот же, что у тулбара, — панель
    /// открывается его кнопкой). Хит-тест и отрисовка — только на нём же,
    /// как у `toolbar`/`cursor_panel`/`confirm` (M3, docs/M3_STEP4_REVIEW.md,
    /// пункт 2.1).
    monitor_id: MonitorId,
}

/// Открытая панель быстрого переключения пресетов (M7, SPEC.md §3.8) —
/// в отличие от `WindowPickerState`, не привязана к стикеру и не
/// пересобирается на лету: список пресетов не может измениться, пока эта
/// модальная панель открыта (единственный способ её закрыть — клик по
/// строке или `Esc`, оба сразу же убирают `Some`).
struct PresetPickerState {
    panel: Panel,
    /// Монитор, на котором рисуется панель (тот же, что у панели у
    /// курсора, — открыта её кнопкой), как у `ConfirmState`/`WindowPickerState`.
    monitor_id: MonitorId,
}

/// Открытый список окон для закрепления (M6, `window_pick_list.rs`) — тот
/// же паттерн, что `PresetPickerState`: панель + монитор, на котором она
/// открыта. `scroll` — виртуализация списка (та же идея, что у
/// `WindowPickerState`, но список здесь читает `window_snapshot` напрямую
/// вместо `visibility`-состояния конкретного стикера, и панель
/// пересобирается заново на каждом новом снимке трекера, а не хранится
/// «замороженной» — список открытых окон живой, пока панель открыта).
struct WindowPickListState {
    panel: Panel,
    monitor_id: MonitorId,
    scroll: usize,
    /// Зачем список открыт — см. [`PickListPurpose`].
    purpose: PickListPurpose,
}

/// Зачем открыт список окон: он обслуживает два разных сценария, и клик по
/// строке значит в них разное.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickListPurpose {
    /// Закрепить выбранное окно (исходный сценарий, кнопка «Добавить окно»).
    PinWindow,
    /// Выбрать окно-ХОЗЯИНА для уже закреплённого окна `target`: клик
    /// добавляет правило «показывать только на нём» (запрос пользователя
    /// 2026-08-22).
    ChooseHost { target: isize },
}

/// Открытая панель свойств закреплённого окна (минимальная версия по
/// решению пользователя 2026-08-18 — только кнопка «Открепить»; см.
/// `rebuild_pinned_panel`) — панель + hwnd, чьи свойства она правит
/// (запись в `EditState::pinned_windows` ищется по нему), монитор. Скролл
/// списка правил сохранён в поле, но в минимальной версии всегда 0.
/// Пересобирается на каждый новый снимок трекера (окно могло переехать на
/// другой монитор, пока панель открыта) — тот же паттерн, что
/// `WindowPickerState`.
struct PinnedPanelState {
    hwnd: isize,
    /// Сколько правил «показывать только на этих окнах» было у окна на
    /// момент сборки панели: от этого зависит её высота, а покадровый догон
    /// панели за окном (`pinned_panel_catch_up`) считает ту же геометрию.
    host_rules: usize,
    panel: Panel,
    monitor_id: MonitorId,
    scroll: usize,
}

/// Кэш 1×1 текстур заливки и текстур растрированного текста для перевода
/// `Primitive` (immediate-mode виджетов) в `Sprite` (docs/M2_WIRING_PLAN.md,
/// раздел 2–3). Живёт на весь сеанс редактирования в `run()`, не в
/// `EditState` — это деталь рендера, а не состояние редактирования.
struct UiTextureCache {
    fills: HashMap<[u8; 3], Texture>,
    // Масштаб (`text_scale`/`size_px`) — часть ключа: после `WM_DPICHANGED`
    // те же текст/иконка растрируются в другой плотности, старый растр в
    // новом масштабе даёт размытие/неверный размер, а не просто устаревший
    // пиксель (docs/M3_STEP2_3_REVIEW.md, пункт 2.1). `fills` (1×1,
    // растягиваются как обычные спрайты) масштабонезависимы — не трогаем.
    texts: HashMap<(String, [u8; 3], u32), Texture>,
    icons: HashMap<(Icon, u32), Texture>,
    // Иконки окон (M4 §6): ключ — (стабильный key примитива, размер растра).
    // `rgba` примитива нужен только на первом аплоаде; размеры иконок окна
    // фиксированы на сессию (16×16 на 96 DPI), масштабонезависимы, как
    // `fills` — пересоздаются только при потере устройства (кэш целиком
    // пересоздаётся в `recover_device`).
    rgba_icons: HashMap<(u64, u32, u32), Texture>,
}

impl UiTextureCache {
    fn new() -> Self {
        Self {
            fills: HashMap::new(),
            texts: HashMap::new(),
            icons: HashMap::new(),
            rgba_icons: HashMap::new(),
        }
    }

    fn fill_texture(&mut self, renderer: &Renderer, color: [u8; 3]) -> Option<Texture> {
        if let Some(t) = self.fills.get(&color) {
            return Some(t.clone());
        }
        match renderer.create_texture_from_rgba(&[color[0], color[1], color[2], 0xff], 1, 1) {
            Ok(t) => {
                self.fills.insert(color, t.clone());
                Some(t)
            }
            Err(e) => {
                tracing::warn!(error = %e, "не удалось создать текстуру заливки UI");
                None
            }
        }
    }

    fn text_texture(
        &mut self,
        renderer: &Renderer,
        text: &str,
        color: [u8; 3],
        scale: u32,
    ) -> Option<Texture> {
        let key = (text.to_string(), color, scale);
        if let Some(t) = self.texts.get(&key) {
            return Some(t.clone());
        }
        let (rgba, w, h) = rasterize(text, color, scale);
        match renderer.create_texture_from_rgba(&rgba, w.max(1), h.max(1)) {
            Ok(t) => {
                self.texts.insert(key, t.clone());
                Some(t)
            }
            Err(e) => {
                tracing::warn!(error = %e, text, "не удалось создать текстуру текста UI");
                None
            }
        }
    }

    /// Текстура иконки `icon` — генерируется один раз (`rst_render::icon_rgba`)
    /// и кэшируется по варианту и `size_px` (масштаб — часть ключа: после
    /// смены DPI тот же вариант рисуется в другом физическом размере,
    /// docs/M3_STEP2_3_REVIEW.md, пункт 2.1).
    fn icon_texture(&mut self, renderer: &Renderer, icon: Icon, size_px: u32) -> Option<Texture> {
        let key = (icon, size_px);
        if let Some(t) = self.icons.get(&key) {
            return Some(t.clone());
        }
        let rgba = rst_render::icon_rgba(icon, size_px);
        match renderer.create_texture_from_rgba(&rgba, size_px, size_px) {
            Ok(t) => {
                self.icons.insert(key, t.clone());
                Some(t)
            }
            Err(e) => {
                tracing::warn!(error = %e, ?icon, "не удалось создать текстуру иконки UI");
                None
            }
        }
    }
    /// Текстура произвольного RGBA-растра (иконка окна в панели выбора,
    /// M4 §6): кэш по (key, width, height) — окна одного процесса делят
    /// один key и одну текстуру; `rgba` используется только на первом
    /// аплоаде. Ошибка фабрики не кэшируется (следующий кадр пробует
    /// снова), как у `icon_texture`.
    fn rgba_texture(
        &mut self,
        renderer: &Renderer,
        key: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<Texture> {
        let ckey = (key, width, height);
        if let Some(t) = self.rgba_icons.get(&ckey) {
            return Some(t.clone());
        }
        match renderer.create_texture_from_rgba(rgba, width, height) {
            Ok(t) => {
                self.rgba_icons.insert(ckey, t.clone());
                Some(t)
            }
            Err(e) => {
                tracing::warn!(error = %e, key, "не удалось создать текстуру иконки окна");
                None
            }
        }
    }
}

/// Перевести примитивы панели (`Panel::draw`) в спрайты кадра, используя кэш
/// текстур (docs/M2_WIRING_PLAN.md, раздел 3).
fn primitives_to_sprites(
    prims: &[Primitive],
    cache: &mut UiTextureCache,
    renderer: &Renderer,
    monitor_id: &MonitorId,
    text_scale: u32,
    out: &mut Vec<Sprite>,
) {
    for prim in prims {
        match prim {
            Primitive::Fill {
                rect,
                color,
                opacity,
            } => {
                if let Some(tex) = cache.fill_texture(renderer, *color) {
                    out.push(solid_sprite(&tex, monitor_id, rect, *opacity));
                }
            }
            Primitive::Text {
                rect,
                text,
                color,
                opacity,
            } => {
                if text.is_empty() {
                    continue;
                }
                if let Some(tex) = cache.text_texture(renderer, text, *color, text_scale) {
                    out.push(solid_sprite(&tex, monitor_id, rect, *opacity));
                }
            }
            Primitive::Icon {
                rect,
                icon,
                opacity,
            } => {
                // Иконки квадратные (theme::BUTTON_SIZE в DIP); растрируем в
                // физические пиксели тем же масштабом, что и текст.
                let size_px = (rect.w.max(rect.h) * f64::from(text_scale))
                    .round()
                    .max(1.0) as u32;
                if let Some(tex) = cache.icon_texture(renderer, *icon, size_px) {
                    out.push(solid_sprite(&tex, monitor_id, rect, *opacity));
                }
            }
            Primitive::Rgba {
                rect,
                key,
                width,
                height,
                rgba,
                opacity,
            } => {
                if let Some(tex) = cache.rgba_texture(renderer, *key, *width, *height, rgba) {
                    out.push(solid_sprite(&tex, monitor_id, rect, *opacity));
                }
            }
        }
    }
}

/// Анимация одного стикера (M5a, docs/M5A_ANIMATION_DESIGN.md §5). Чисто
/// runtime-состояние `run()` — не поле `EditState`/`Config`, тем же
/// паттерном, что `occluder_cache`/`window_snapshot`: переживает вход/выход
/// из режима редактирования, не персистится.
enum StickerAnimation {
    /// Обычный случай: атлас всех кадров на GPU + часы, решающие, какой
    /// кадр сейчас показывать — смена кадра это смена UV, декодирование не
    /// требуется на тике.
    Atlas {
        atlas: TextureAtlas,
        clock: AnimationClock,
        /// Задержки кадров в порядке атласа, посчитаны ОДИН раз при
        /// создании (ROADMAP.md M8, «профилирование, устранение горячих
        /// точек»): `next_deadline` дёргается на КАЖДОЕ сообщение
        /// координатора, а не только на `AnimationTick` (см.
        /// `next_tick_deadline` в `run()`) — пересборка `Vec<Duration>` из
        /// `atlas.frames` на каждый вызов означала аллокацию на каждое
        /// движение мыши для каждого анимированного стикера одновременно
        /// (найдено при ревью собственного кода этой же вехи).
        delays: Vec<Duration>,
    },
    /// Потоковый режим (ROADMAP.md M5a, «потоковый режим для очень длинных
    /// анимаций»): `decode_animation` отказал по порогу атласа
    /// (`TooManyFrames`/`TooLargeForAtlas`, rst_media::animation) — вместо
    /// атласа один переиспользуемый кадр-текстура, перезаливаемый декодером
    /// на каждый тик; `StreamingAnimation::next_frame` зацикливает сама, не
    /// требуя отслеживать конец анимации здесь.
    ///
    /// Часы устроены проще, чем `AnimationClock` атласного пути: без
    /// модульной арифметики «долгой паузы» (`AnimationClock::advance`) —
    /// там цена кадра это смена индекса, здесь это реальное декодирование,
    /// поэтому после долгой паузы (сон/блокировка) потоковая анимация
    /// просто продолжает с текущего кадра на ближайшем тике, а не пытается
    /// досчитать пропущенные кадры по времени.
    Streaming {
        source: media_animation::StreamingAnimation,
        texture: Texture,
        deadline: Instant,
    },
}

impl StickerAnimation {
    /// Завести атласную анимацию: считает `delays` из `atlas.frames` один
    /// раз здесь — единственное место, где это вообще должно происходить
    /// (см. доккомент поля `Atlas::delays`).
    fn from_atlas(atlas: TextureAtlas, clock: AnimationClock) -> Self {
        let delays = atlas.frames.iter().map(|f| f.delay).collect();
        Self::Atlas {
            atlas,
            clock,
            delays,
        }
    }

    /// Ближайший момент, когда эта анимация должна тикнуть снова — вход
    /// общего расчёта `next_tick_deadline` в `run()` (единая точка для
    /// атласного и потокового путей).
    fn next_deadline(&self) -> Instant {
        match self {
            Self::Atlas { clock, delays, .. } => clock.next_deadline(delays),
            Self::Streaming { deadline, .. } => *deadline,
        }
    }
}

/// Результат декодирования анимации нового стикера, ещё не заведённый в
/// `animations` (см. `EditState::pending_animation`) — `add_sticker` не
/// видит локальную переменную цикла `run()`, поэтому кладёт готовый атлас
/// или уже открытый потоковый декодер сюда, а `run()` заводит из него
/// `StickerAnimation` (с часами/дедлайном) сразу после обработки текущего
/// сообщения.
enum PendingAnimation {
    Atlas(TextureAtlas),
    Streaming {
        source: media_animation::StreamingAnimation,
        texture: Texture,
        first_delay: Duration,
    },
}

/// Видео одного стикера (M5b, docs/M5B_VIDEO_DESIGN.md §6): декодер-поток
/// (`rst_video::VideoSource`) + источник звука в общем микшере процесса.
/// Как и `StickerAnimation` — чисто runtime-состояние `run()`, не персистится
/// (переживает вход/выход из режима редактирования, восстанавливается заново
/// на старте/`resync_sprites`/`recover_device`).
///
/// `audio: None` — устройство вывода звука не открылось при старте
/// (`AudioMixer::new()` вернул `Err`, см. `run()`): видео всё равно должно
/// открываться и играть (пользовательское решение §0 — «никогда не
/// отказывать»), просто без звука. Декодер отбрасывает нечитаемые порции
/// звука сам (`try_send`, не блокирует), поэтому это безопасно.
struct VideoPlayback {
    source: VideoSource,
    audio: Option<AudioSource>,
}

/// Схлопнуть подряд идущие `MouseMove` одного монитора в очереди `rx`,
/// оставляя только последнее — защита от бага «стикер едет сам по себе
/// после того, как отпустил мышь» (найдено в этой сессии, живое
/// видео-подтверждение пользователя): канал `tx`/`rx` безлимитный, а
/// координатор синхронно перерисовывает все мониторы на КАЖДОЕ сообщение
/// (GPU-работа). Если поток окна успевает прислать реальные `WM_MOUSEMOVE`
/// быстрее, чем координатор их разбирает, канал копит очередь настоящих, но
/// устаревших позиций — и координатор потом честно доигрывает всю историю
/// уже ПОСЛЕ того, как пользователь физически остановился (`MouseUp` в той
/// же очереди, позже всех накопленных `MouseMove`). Внешне неотличимо от
/// самопроизвольного движения, хотя каждое сообщение по отдельности
/// абсолютно легитимно — разбираются они с опозданием, а не появляются
/// беспричинно. Стандартный приём против этого класса багов — тот же, что у
/// сырого Win32 `PeekMessage`/`PM_REMOVE` для `WM_MOUSEMOVE`: прыгать сразу
/// к последней доступной позиции, не проигрывая промежуточные.
///
/// `first` — уже полученное из `rx` сообщение. Если это `MouseMove`, дальше
/// неблокирующе (`try_recv`) вычитываются все НЕМЕДЛЕННО доступные
/// сообщения; те, что тоже `MouseMove` того же монитора, заменяют текущее
/// (более новая позиция вытесняет более старую); первое сообщение другого
/// типа (или другого монитора) прерывает дренаж и возвращается как
/// `leftover` — не теряется, а обрабатывается следующей итерацией цикла
/// координатора. Если `first` — не `MouseMove`, дренаж не выполняется
/// вовсе (только эта форма событий копится достаточно быстро, чтобы
/// формировать бэклог; остальные — редкие/дискретные).
fn coalesce_mouse_move(
    rx: &Receiver<OverlayMessage>,
    first: OverlayMessage,
) -> (OverlayMessage, Option<OverlayMessage>) {
    let OverlayMessage::Event(mid, OverlayEvent::Input(InputEvent::MouseMove { .. })) = &first
    else {
        return (first, None);
    };
    let mid = mid.clone();
    let mut latest = first;
    loop {
        match rx.try_recv() {
            Ok(next) => {
                let same_monitor_move = matches!(
                    &next,
                    OverlayMessage::Event(next_mid, OverlayEvent::Input(InputEvent::MouseMove { .. }))
                        if *next_mid == mid
                );
                if same_monitor_move {
                    latest = next;
                } else {
                    return (latest, Some(next));
                }
            }
            Err(_) => return (latest, None),
        }
    }
}

fn run(
    config_path: PathBuf,
    mut cfg: Config,
    tx: Sender<OverlayMessage>,
    rx: Receiver<OverlayMessage>,
    coordinator_tx: Sender<CoordinatorRequest>,
) {
    // Первый запуск (ROADMAP.md M8, «первый запуск: короткий онбординг,
    // показать хоткей») — один короткий тост с хоткеем входа в режим
    // редактирования, дальше `Settings::onboarding_shown` навсегда гасит
    // повтор. До основного цикла, но после того, как `cfg`/`coordinator_tx`
    // уже в скоупе — не зависит ни от монитора, ни от GPU-устройства ниже.
    if let Some((title, body)) = onboarding_notification(&cfg) {
        let _ = coordinator_tx.send(CoordinatorRequest::ShowNotification { title, body });
        cfg.settings.onboarding_shown = true;
        if let Err(e) = config::save(&cfg, &config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после показа онбординга");
        }
    }

    let hotkey = cfg
        .hotkeys
        .edit_mode
        .as_deref()
        .and_then(|s| HotkeyCombo::parse(s).ok())
        .or_else(|| HotkeyCombo::parse(DEFAULT_EDIT_HOTKEY).ok())
        .expect("DEFAULT_EDIT_HOTKEY — валидная комбинация");
    // В отличие от edit_hotkey, этот хоткей опционален: пустая/некорректная
    // настройка просто не регистрирует его (нет дефолта-фолбэка).
    let toggle_all_hotkey = cfg
        .hotkeys
        .toggle_all_stickers
        .as_deref()
        .and_then(|s| HotkeyCombo::parse(s).ok());
    // «Заглушить все стикеры» (M5d) — тот же опциональный паттерн, что и
    // toggle_all_hotkey выше: `AudioMixer::set_muted` уже существует, не
    // хватало только регистрации самого хоткея.
    let mute_all_hotkey = cfg
        .hotkeys
        .mute_all
        .as_deref()
        .and_then(|s| HotkeyCombo::parse(s).ok());
    // «Закрепить/открепить сфокусированное окно» (редизайн пинов,
    // `hotkeys.pin_focused_window`, дефолт «Ctrl+Alt+T») — тот же
    // опциональный паттерн, что у трёх предыдущих; на пустое/непарсящееся
    // значение регистрируется дефолт (дефолт задан в `Hotkeys::default()`).
    let pin_focused_hotkey = cfg
        .hotkeys
        .pin_focused_window
        .as_deref()
        .and_then(|s| HotkeyCombo::parse(s).ok())
        .or_else(|| HotkeyCombo::parse("Ctrl+Alt+T").ok())
        .expect("дефолтный пин-хоткей — валидная комбинация");

    // M3: окно на каждый подключённый монитор, а не один захардкоженный
    // основной (M3_PREP_NOTES.md, раздел 5). Перечисление — при старте;
    // переперечисление на `WM_DISPLAYCHANGE` (`OverlayEvent::MonitorsChanged`)
    // пока только логируется — динамическое добавление/снятие окон при
    // hot-plug и таймер ADR-011 (`monitor_loss::MonitorLossTracker`,
    // уже готов) — отдельный, следующий срез.
    let monitor_infos = match monitors::enumerate() {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => {
            tracing::error!("перечисление мониторов вернуло пустой список");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "не удалось перечислить мониторы");
            return;
        }
    };
    // `mut`: пересоздаётся веткой `MonitorsChanged`, если основной монитор
    // сменился (M3_HOTPLUG_DESIGN.md §2).
    let mut primary_id = monitor_infos
        .iter()
        .find(|m| m.is_primary)
        .unwrap_or(&monitor_infos[0])
        .id
        .clone();

    // `Device` — один на процесс (M3 step 2, ARCHITECTURE.md раздел 1):
    // текстуры (заливки, спрайты стикеров) грузятся здесь один раз и
    // рисуются на цели любого монитора без перезаливки на GPU.
    // `mut`: пересоздаётся целиком при потере устройства (`recover_device`,
    // ARCHITECTURE.md раздел 11) — не только на старте.
    let mut device = match Device::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать D3D11-устройство");
            return;
        }
    };

    // Заливки для рамки выделения (белая) и затемнения режима (чёрная) —
    // 1×1 текстуры, растягиваются рендерером как обычные спрайты; общие на
    // процесс, как и любая другая текстура на `device`. `mut` — пересоздаются
    // вместе с устройством при потере.
    let mut white_tex = match device.create_texture_from_rgba(&[0xff, 0xff, 0xff, 0xff], 1, 1) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать текстуру рамки выделения");
            return;
        }
    };
    let mut black_tex = match device.create_texture_from_rgba(&[0x00, 0x00, 0x00, 0xff], 1, 1) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать текстуру затемнения");
            return;
        }
    };

    // Восстановление между запусками: стикеры уже в cfg (загружены в main
    // через rst_core::config::load до вызова start()). Спрайт хранится
    // вместе с id стикера — жесты правят конкретный спрайт по id, порядок
    // отрисовки берётся из cfg.stickers (по `order`) в `redraw`, а
    // видимость на конкретном мониторе — фильтром по `placement.monitor_id`
    // (М3, там же).
    //
    // `animations` заведена уже здесь (а не позже, ближе к остальным
    // локальным переменным `run()`) — иначе анимированный стикер после
    // рестарта процесса навсегда завис бы статичным кадром 0: без записи
    // в `animations` часы для него просто никогда бы не завелись (найдено
    // независимым ревью сшивки).
    // Микшер звука (M5b, docs/M5B_VIDEO_DESIGN.md §4/§6) — один на процесс,
    // как и `Device`. Неудача (нет устройства вывода/оно занято) не фатальна
    // для всего оверлея — видео должно открываться и играть в любом случае
    // (пользовательское решение §0), просто без звука (см. доккомент
    // `VideoPlayback::audio`); поэтому `Option`, а не пробрасываем `Err` из
    // `run()` целиком, как для `Device`.
    let audio_mixer = match AudioMixer::new() {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "не удалось открыть аудиоустройство — видео будут играть без звука");
            None
        }
    };
    // Глобальный «заглушить все» (M5d, `OverlayEvent::ToggleMuteAll`) —
    // рантайм-состояние хоткея, не персистентное (в отличие от громкости
    // на стикер, конфиг ничего не хранит про этот тумблер).
    let mut audio_muted = false;

    // Закрепления окон (редизайн пинов, SPEC.md «Закрепление окна») —
    // маркер `WS_EX_TOPMOST` + window property, рантайм-механизм на весь
    // процесс рядом с `audio_mixer` (тот же паттерн: ресурс процесса, не
    // переживает конфиг — сами закрепления живут в
    // `EditState::pinned_windows`, `WindowPins` — их Win32-движок).
    // `unpin_all()` вызывается на выходе из `run()`, ниже, по гарантии
    // открепления.
    let mut window_pins = WindowPins::new();
    // Уборка после аварийного завершения прошлого запуска: маркер закрепления
    // живёт на ЧУЖОМ окне и переживает наш процесс, поэтому долгоживущие окна
    // (Проводник, Блокнот) могли накопить «вечные» маркеры — с ними окно
    // нельзя ни закрепить (ошибка «уже закреплено»), ни открепить (в книжке
    // его нет). Репорт пользователя 2026-08-21. Единственность процесса
    // гарантирует `single_instance`, так что чужих живых маркеров быть не
    // может — всё, что найдено, наш собственный мусор.
    {
        let startup_windows = rst_win32::window_enum::enumerate();
        let cleared = window_pins.clear_orphan_markers(&startup_windows);
        if cleared > 0 {
            tracing::info!(cleared, "снял осиротевшие маркеры закрепления прошлого запуска");
        }
    }

    let mut sprites: Vec<(Uuid, Sprite)> = Vec::new();
    let mut animations: HashMap<Uuid, StickerAnimation> = HashMap::new();
    let mut videos: HashMap<Uuid, VideoPlayback> = HashMap::new();
    for sticker in &cfg.stickers {
        if let Some((sprite, anim)) = load_sticker_sprite(&device, sticker) {
            sprites.push((sticker.id, sprite));
            animations.insert(sticker.id, anim);
        } else if let Some((sprite, playback)) =
            load_sticker_video(&device, sticker, audio_mixer.as_ref())
        {
            sprites.push((sticker.id, sprite));
            videos.insert(sticker.id, playback);
        } else if let Some(sprite) = load_static_sprite(&device, sticker) {
            sprites.push((sticker.id, sprite));
        }
    }

    // Окно + цель рендера на каждый монитор; глобальный хоткей режима
    // регистрирует ровно одно окно — основного монитора (M3_PREP_NOTES.md,
    // раздел 3.3), остальные создаются без него, чтобы не конфликтовать.
    let mut monitors_map: HashMap<MonitorId, MonitorState> = HashMap::new();
    for info in &monitor_infos {
        // Глобальные хоткеи — ровно одно окно на процесс (основного
        // монитора), остальные создаются без них, чтобы не конфликтовать.
        let (edit_hotkey, this_toggle_all, this_mute_all, this_pin_focused) =
            if info.id == primary_id {
                (
                    Some(hotkey),
                    toggle_all_hotkey,
                    mute_all_hotkey,
                    Some(pin_focused_hotkey),
                )
            } else {
                (None, None, None, None)
            };
        if let Some(ms) = create_monitor_state(
            &device,
            &tx,
            info,
            edit_hotkey,
            this_toggle_all,
            this_mute_all,
            this_pin_focused,
            false,
            cfg.settings.hide_from_capture,
        ) {
            monitors_map.insert(info.id.clone(), ms);
        }
    }

    if monitors_map.is_empty() {
        tracing::error!("не удалось создать ни одного оверлей-окна");
        return;
    }

    // Отдельный от `monitors_map` снимок геометрии: панели строятся по
    // геометрии своего «домашнего» монитора, который может отличаться от
    // монитора текущего события (тулбар — по монитору стикера, панель у
    // курсора — по `cursor_monitor`, docs/M3_STEP4_REVIEW.md, пункт 2.2).
    // `HashMap` не даёт держать `&mut MonitorState` одного монитора (через
    // `Renderer`) и `&monitors_map` для поиска геометрии другого одновременно
    // — отдельная лёгкая копия `(width, height, scale)` снимает конфликт.
    let mut monitor_geometry: HashMap<MonitorId, (u32, u32, f32)> = monitors_map
        .iter()
        .map(|(id, ms)| (id.clone(), (ms.width, ms.height, ms.scale)))
        .collect();

    // Границы мониторов в физических пикселях виртуального десктопа + масштаб
    // — вход для перепривязки стикера по центру bbox при перетаскивании между
    // мониторами (M3 step 7, `rst_core::monitor_rebind`). Позиция (`x`/`y`) не
    // меняется сменой DPI (только размер/масштаб монитора) — обновляется
    // только `w`/`h`/`scale` в ветке `DpiChanged`, отдельно от `monitors_map`
    // по той же причине, что и `monitor_geometry`.
    let mut monitor_bounds: HashMap<MonitorId, MonitorBounds> = monitor_infos
        .iter()
        .filter_map(|info| {
            let ms = monitors_map.get(&info.id)?;
            Some((
                info.id.clone(),
                MonitorBounds {
                    id: info.id.clone(),
                    bounds_px: info.bounds_px,
                    scale: ms.scale as f64,
                },
            ))
        })
        .collect();

    // Будильник автомата потери монитора — тот же паттерн «поток + сообщение
    // в общий канал», что и у пер-мониторных форвардеров выше (M3_HOTPLUG_
    // DESIGN.md §1); выходит сам, как только канал закрыт (координатор
    // завершился).
    let tick_tx = tx.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(LOSS_TICK_PERIOD);
            if tick_tx.send(OverlayMessage::Tick).is_err() {
                break;
            }
        }
    });

    // Планировщик анимации (M5a, docs/M5A_ANIMATION_DESIGN.md §5): тот же
    // паттерн «поток + канал в общую очередь», что и `Tick`-поток выше, но с
    // ПЕРЕМЕННЫМ интервалом — спит ровно до ближайшего дедлайна кадра
    // анимации, а не раз в секунду. `anim_deadline_tx` — отдельный канал
    // «координатор → планировщик»: координатор пересчитывает ближайший
    // дедлайн после каждого сообщения (см. конец цикла `for msg in rx`) и
    // присылает его сюда; `None` — анимаций, которым нужен тик, сейчас нет
    // (планировщик просто ждёт следующего обновления неопределённо долго).
    let (anim_deadline_tx, anim_deadline_rx) = mpsc::channel::<Option<Instant>>();
    let anim_tx = tx.clone();
    thread::spawn(move || {
        // Час без обновлений — не магическое ожидание конкретного события, а
        // периодическая самопроверка на случай гонки «дедлайн уже None,
        // но канал ещё не закрыт»; ничего не шлёт, если deadline реально None.
        const IDLE_POLL: Duration = Duration::from_secs(3600);
        let mut deadline: Option<Instant> = None;
        loop {
            let timeout =
                deadline.map_or(IDLE_POLL, |d| d.saturating_duration_since(Instant::now()));
            match anim_deadline_rx.recv_timeout(timeout) {
                Ok(new_deadline) => deadline = new_deadline,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if deadline.is_some() && anim_tx.send(OverlayMessage::AnimationTick).is_err() {
                        break;
                    }
                    // Дедлайн потреблён (сработал или был `IDLE_POLL`-заглушкой)
                    // — не тикать снова, пока координатор не пришлёт новый
                    // после обработки этого сообщения.
                    deadline = None;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // Трекер окон (M4_WINDOW_TRACKER_DESIGN.md §6): свой поток + канал, как у
    // OverlayWindow/тика — мост копирует его WindowEvent в общий канал.
    // Неудача запуска не фатальна для всего процесса (стикеры без слоёв
    // видимости продолжают работать как обычно) — просто не будет ни хуков,
    // ни маски перекрытия в этой сессии.
    let window_tracker = match WindowTracker::start() {
        Ok((tracker, tracker_rx)) => {
            let windows_tx = tx.clone();
            thread::spawn(move || {
                for ev in tracker_rx {
                    if windows_tx.send(OverlayMessage::Windows(ev)).is_err() {
                        break;
                    }
                }
            });
            Some(tracker)
        }
        Err(e) => {
            tracing::error!(error = %e, "не удалось запустить трекер окон — маска перекрытия недоступна в этой сессии");
            None
        }
    };
    // Гейт хуков трекера (ADR-005, M4_PREP_NOTES §6.4): пересчитывается раз
    // за итерацию цикла вместо разбрасывания вызова по всем ~20 местам,
    // мутирующим `cfg` — дёшево при типичном числе стикеров, и невозможно
    // забыть точку пересчёта.
    let mut last_mask_needed = mask_needed(&cfg);
    if let Some(tracker) = &window_tracker {
        tracker.set_mask_needed(last_mask_needed);
    }

    // `animations` уже заведена выше (сразу после старта, вместе со `sprites`)
    // — здесь только оставшееся runtime-состояние планировщика M5a.
    // Последний дедлайн, отправленный планировщику — чтобы не слать
    // одинаковое значение на каждой итерации цикла впустую.
    let mut last_anim_deadline: Option<Instant> = None;
    // Сессия Windows заблокирована (SPEC.md §9) — пока `true`, часы анимации
    // не тикают и планировщику не шлётся новый дедлайн: экран блокировки не
    // виден пользователю, анимировать нечего (тот же принцип, что «не
    // декодировать невидимое», ARCHITECTURE.md §4.3, просто на уровне всей
    // сессии, а не одного стикера).
    let mut session_locked = false;

    // Последний снимок окон трекера (M4_OCCLUDERS_DESIGN.md §1) — обычная
    // локальная переменная `run()`, не поле `EditState`: маска перекрытия не
    // относится к режиму редактирования и должна переживать вход/выход из
    // него. Пусто до первого `Windows(Changed)` — первый кадр рисуется без
    // окклюдеров (консервативно безопасно: ничего не прячется зря, самое
    // страшное — секундная вспышка стикера поверх окна до первого снимка).
    let mut window_snapshot: Vec<WindowInfo> = Vec::new();
    // Группы окклюдеров по монитору — топология (CPU, `Rect`), не GPU-текстуры
    // (M4_OCCLUDERS_DESIGN.md §1/§7): один шейдерный проход-маска на группу
    // стикеров с одинаковым эффективным правилом видимости, чтобы allow-list
    // одного стикера не просачивался в другой (M4_PREP_NOTES.md §4.2). Сами
    // GPU-текстуры масок строятся `redraw()` заново на каждый вызов — они
    // event-driven, как и весь рендер (ADR-006), пересоздавать их тут вместе с
    // топологией смысла нет (размер маски должен быть текущим размером цели
    // монитора на момент отрисовки, а не на момент пересчёта окклюдеров).
    // Начальный пересчёт с пустым снимком окон — до первого `Windows(Changed)`
    // группы для стикеров с `VisibilityMode != Always` уже существуют, но с
    // пустым `rects` (нет известных окклюдеров), что рисует их как обычно.
    let mut occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);

    // Автомат ADR-011 (SPEC 6.1) и его «эталонный» снимок мониторов — сверяет
    // с ним каждый `MonitorsChanged` (диффинг всегда против ЖИВОГО состояния,
    // не отдельной копии — WM_DISPLAYCHANGE рассылается всем окнам, дубликаты
    // событий идемпотентны сами по себе, M3_HOTPLUG_DESIGN.md §2) и служит
    // входом для `Tick`, где сам снимок не меняется — только даёт таймерам
    // шанс истечь.
    let mut loss_tracker = MonitorLossTracker::new();
    let mut last_snapshot: Vec<MonitorSnapshot> = monitor_infos.iter().map(core_snapshot).collect();
    // Обязательный старый прогон: без него первый реальный `MonitorsChanged`
    // сравнил бы новый снимок с ПУСТЫМ prev и принял бы мониторы, живые с
    // самого старта, за только что пропавшие (M3_HOTPLUG_DESIGN.md §2).
    // Действия здесь заведомо пусты — это только инициализация `prev_snapshot`.
    let _ = loss_tracker.on_monitor_snapshot(&last_snapshot, &cfg.stickers, Instant::now());

    let mut edit = EditState {
        active: false,
        selection: SelectionSet::new(),
        gesture: None,
        snap: SnapConfig::default(),
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
        pending_snapshot: None,
        confirm: None,
        window_picker: None,
        preset_picker: None,
        pending_open_picker: None,
        pending_open_pick_list: None,
        window_pick_list: None,
        pinned_windows: Vec::new(),
        pinned_selection: None,
        pinned_panel: None,
        pinned_gesture: None,
        surfaced_pins: HashSet::new(),
        pin_flashes: Vec::new(),
        pinned_last_rects: HashMap::new(),
        pinned_unmaximized_at: HashMap::new(),
        pinned_follow_until: None,
        banner: None,
        pending_animation: None,
        pending_video: None,
        marquee: None,
        marquee_started: false,
        toolbar: None,
        cursor_panel: None,
        tooltip: None,
        pointer_owner: PointerOwner::None,
        ui_pending_snapshot: None,
        cursor_pos: (0.0, 0.0),
        cursor_monitor: primary_id.clone(),
        coordinator_tx,
    };
    let mut ui_cache = UiTextureCache::new();

    // `true`, пока последняя попытка `recover_device` заканчивалась неудачей
    // (например, `Device::new()` не создался сразу после выхода из сна, пока
    // адаптер ещё «оседает», M3_SESSION_SLEEP_REVIEW.md, пункт 2.3): без
    // этого флага повтор ждал бы следующего события, выставляющего
    // `need_redraw`, — которого может не быть долго (ховер-движения его не
    // выставляют) — оверлей оставался бы чёрным. Ветка `Tick` ниже форсирует
    // `need_redraw`, пока флаг не снимется успешным восстановлением.
    let mut device_needs_recovery = false;

    if redraw_all(
        &device,
        &mut monitors_map,
        &sprites,
        &cfg,
        &edit,
        &white_tex,
        &black_tex,
        &mut ui_cache,
        &occluder_cache,
        &window_snapshot,
        &monitor_bounds,
    ) {
        if recover_device(
            &mut device,
            &mut monitors_map,
            &mut white_tex,
            &mut black_tex,
            &mut sprites,
            &cfg,
            &mut ui_cache,
            &mut animations,
            &mut videos,
            audio_mixer.as_ref(),
        ) {
            redraw_all(
                &device,
                &mut monitors_map,
                &sprites,
                &cfg,
                &edit,
                &white_tex,
                &black_tex,
                &mut ui_cache,
                &occluder_cache,
                &window_snapshot,
                &monitor_bounds,
            );
        } else {
            device_needs_recovery = true;
        }
    }

    // Канал `tx`/`rx` — безлимитный `mpsc::channel` (ADR не документировал
    // это как сознательное решение — просто так исторически сложилось).
    // Каждое сообщение здесь может вызвать полный синхронный `redraw_all`
    // по всем мониторам (GPU-работа, DirectComposition Commit). Если поток
    // окна успевает прислать реальные `WM_MOUSEMOVE` быстрее, чем
    // координатор успевает их разобрать и перерисовать (что тем более
    // вероятно на живом железе с несколькими мониторами, чем в
    // синтетических тестах на одном), канал накапливает очередь НАСТОЯЩИХ,
    // но уже устаревших позиций мыши — координатор потом честно доигрывает
    // её всю, кадр за кадром, ПОСЛЕ того, как пользователь физически
    // остановился и отпустил кнопку (её `MouseUp` тоже в этой же очереди,
    // позже всех накопленных `MouseMove`). Внешне это неотличимо от
    // «стикер продолжает ехать сам по себе» — хотя каждое отдельное
    // сообщение в очереди совершенно легитимно, проблема в том, что
    // разбираются они с опозданием в реальном времени, а не в том, что
    // они не должны были прийти. Ни один из предыдущих фиксов (проверка
    // физической кнопки, дедуп по координатам на уровне Win32-сообщения)
    // эту причину не ловит — там всё корректно уже В МОМЕНТ приёма
    // сообщения, отставание образуется на СТОРОНЕ ПОТРЕБИТЕЛЯ.
    //
    // Стандартный приём против этого класса багов (используется практически
    // в любом интерактивном drag поверх очереди сообщений, в т.ч. сам Win32
    // `PeekMessage`/`PM_REMOVE` в этом же цикле для сырых `WM_MOUSEMOVE`) —
    // схлопывать подряд идущие события движения мыши ОДНОГО монитора,
    // оставляя только последнее перед тем, как обрабатывать/рисовать: тогда
    // координатор всегда «прыгает» к актуальной позиции, а не проигрывает
    // историю. `pending` — место для одного «нескоалесцированного»
    // сообщения, вычитанного во время дренажа очереди, но не подошедшего
    // под условие коалесинга (переносится на следующую итерацию цикла).
    let mut pending: Option<OverlayMessage> = None;
    loop {
        let msg = match pending.take() {
            Some(m) => m,
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        let (msg, leftover) = coalesce_mouse_move(&rx, msg);
        pending = leftover;
        let mut need_redraw = false;
        match msg {
            OverlayMessage::Command(OverlayCommand::AddSticker(path)) => {
                // Команда от Tauri не несёт «текущий монитор» — добавляем на
                // основной, как и раньше в однооконном мире.
                if let Some(ms) = monitors_map.get_mut(&primary_id) {
                    let mut renderer = Renderer {
                        device: &device,
                        target: &mut ms.target,
                    };
                    add_sticker(
                        &ms.overlay,
                        &mut renderer,
                        &mut cfg,
                        &config_path,
                        &mut sprites,
                        &mut edit,
                        path,
                        false,
                        ms.scale,
                        &primary_id,
                        audio_mixer.as_ref(),
                    );
                }
                need_redraw = true;
            }
            // Окно настроек (Tauri): все команды ниже держат координатор
            // единственным писателем `config.json` (доккомент
            // `OverlayCommand`) — Tauri-поток только читает диск напрямую
            // и шлёт команды, применение и сохранение — всегда здесь.
            OverlayMessage::Command(OverlayCommand::UpdateSettings(settings)) => {
                let old_hide_from_capture = cfg.settings.hide_from_capture;
                cfg.settings = settings;
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после изменения общих настроек");
                }
                if cfg.settings.hide_from_capture != old_hide_from_capture {
                    for (monitor_id, ms) in monitors_map.iter() {
                        apply_capture_affinity(
                            &ms.overlay,
                            cfg.settings.hide_from_capture,
                            monitor_id,
                        );
                    }
                }
                // «Не перекрывать панель задач» и правила видимости читаются
                // заново при следующем пересчёте — но без реального события
                // от трекера окон его бы не было ещё долго; пересчитываем
                // сразу, тем же путём, что `Windows(Changed)`.
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::UpdateHotkeys(hotkeys)) => {
                cfg.hotkeys = hotkeys;
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после изменения хоткеев");
                }
            }
            OverlayMessage::Command(OverlayCommand::SetStickerEnabled(id, enabled)) => {
                ops::set_visible_many(&mut cfg, &[id], enabled);
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после включения/выключения стикера");
                }
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::DeleteSticker(id)) => {
                cleanup_pasted_file(&cfg, id);
                let _ = ops::delete(&mut cfg, id);
                edit.selection.prune(&cfg.stickers);
                if let Some(ms) = monitors_map.get_mut(&primary_id) {
                    let renderer = Renderer {
                        device: &device,
                        target: &mut ms.target,
                    };
                    resync_sprites(
                        &renderer,
                        &cfg,
                        &mut sprites,
                        &mut animations,
                        &mut videos,
                        audio_mixer.as_ref(),
                    );
                }
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после удаления стикера");
                }
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                rebuild_ui_panels(&mut edit, &cfg, &monitor_geometry);
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::ResetStickerPosition(id)) => {
                if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                    let monitor_id = sticker.placement.monitor_id.clone();
                    if let Some(&(w, h, scale)) = monitor_geometry.get(&monitor_id) {
                        let center = (w as f64 / scale as f64 / 2.0, h as f64 / scale as f64 / 2.0);
                        let _ = ops::reset_position(&mut cfg, id, center.0, center.1);
                        if let Some(placement) = cfg
                            .stickers
                            .iter()
                            .find(|s| s.id == id)
                            .map(|s| s.placement.clone())
                        {
                            if let Some((_, sprite)) =
                                sprites.iter_mut().find(|(sid, _)| *sid == id)
                            {
                                sprite.placement = placement;
                            }
                        }
                        if let Err(e) = config::save(&cfg, &config_path) {
                            tracing::warn!(error = %e, "не удалось сохранить config.json после сброса позиции стикера");
                        }
                        need_redraw = true;
                    }
                }
            }
            OverlayMessage::Command(OverlayCommand::ResetStickerTransform(id)) => {
                if let Some((natural_w, natural_h)) = sticker_natural_size(&sprites, id) {
                    let _ = ops::reset_transform_and_size(&mut cfg, id, natural_w, natural_h);
                    if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                        let (placement, transform) = (sticker.placement.clone(), sticker.transform);
                        if let Some((_, sprite)) = sprites.iter_mut().find(|(sid, _)| *sid == id) {
                            sprite.placement = placement;
                            sprite.transform = transform;
                        }
                    }
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после сброса размера/поворота стикера");
                    }
                    need_redraw = true;
                }
            }
            OverlayMessage::Command(OverlayCommand::RelinkSticker(id, new_path)) => {
                let is_video = new_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| {
                        VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(ext))
                    });
                let media_type = if is_video {
                    MediaType::Video
                } else {
                    // sniff_media_type, а не ручной decode_animation().ok():
                    // трактует TooManyFrames/TooLargeForAtlas как Animation
                    // тоже (потоковый режим, ROADMAP.md M5a) — иначе
                    // переуказание на очень длинную анимацию тихо давало бы
                    // Image (тот же баг, что был у add_sticker до потокового
                    // режима).
                    media_animation::sniff_media_type(&new_path)
                };
                if ops::relink_file(&mut cfg, id, new_path, media_type).is_ok() {
                    sprites.retain(|(sid, _)| *sid != id);
                    animations.remove(&id);
                    videos.remove(&id);
                    if let Some(ms) = monitors_map.get_mut(&primary_id) {
                        let renderer = Renderer {
                            device: &device,
                            target: &mut ms.target,
                        };
                        resync_sprites(
                            &renderer,
                            &cfg,
                            &mut sprites,
                            &mut animations,
                            &mut videos,
                            audio_mixer.as_ref(),
                        );
                    }
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после переуказания файла стикера");
                    }
                    need_redraw = true;
                }
            }
            OverlayMessage::Command(OverlayCommand::ResetAllStickers) => {
                let ids: Vec<Uuid> = cfg.stickers.iter().map(|s| s.id).collect();
                for id in ids {
                    if let Some((natural_w, natural_h)) = sticker_natural_size(&sprites, id) {
                        let _ = ops::reset_transform_and_size(&mut cfg, id, natural_w, natural_h);
                    }
                    let monitor_id = cfg
                        .stickers
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| s.placement.monitor_id.clone());
                    if let Some(monitor_id) = monitor_id {
                        if let Some(&(w, h, scale)) = monitor_geometry.get(&monitor_id) {
                            let _ = ops::reset_position(
                                &mut cfg,
                                id,
                                w as f64 / scale as f64 / 2.0,
                                h as f64 / scale as f64 / 2.0,
                            );
                        }
                    }
                }
                if let Some(ms) = monitors_map.get_mut(&primary_id) {
                    let renderer = Renderer {
                        device: &device,
                        target: &mut ms.target,
                    };
                    resync_sprites(
                        &renderer,
                        &cfg,
                        &mut sprites,
                        &mut animations,
                        &mut videos,
                        audio_mixer.as_ref(),
                    );
                }
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после сброса всех стикеров");
                }
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::DeleteAllStickers) => {
                for id in cfg.stickers.iter().map(|s| s.id).collect::<Vec<_>>() {
                    cleanup_pasted_file(&cfg, id);
                }
                cfg.stickers.clear();
                edit.selection.clear();
                sprites.clear();
                animations.clear();
                videos.clear();
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после удаления всех стикеров");
                }
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                rebuild_ui_panels(&mut edit, &cfg, &monitor_geometry);
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::ToggleAllStickers) => {
                // Тот же путь, что `OverlayEvent::ToggleAllStickers` (хоткей)
                // ниже — источник другой (меню трея, главный поток Tauri),
                // эффект и переисок панели у курсора идентичны.
                let before = cfg.clone();
                if converge_all_stickers_visibility(&mut cfg) {
                    commit_undo_snapshot(&mut edit, before);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после «показать/скрыть все» из трея");
                    }
                    if let Some(&(w, h, s)) = monitor_geometry.get(&edit.cursor_monitor) {
                        let screen = screen_dip_rect((w, h), s);
                        rebuild_cursor_panel(&mut edit, &cfg, &screen);
                    }
                    need_redraw = true;
                }
            }
            OverlayMessage::Command(OverlayCommand::SavePreset(name)) => {
                let preset = presets::save_preset(&cfg, name);
                cfg.presets.push(preset);
                if let Err(e) = config::save(&cfg, &config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после сохранения пресета");
                }
                notify_presets_changed(&cfg, &edit.coordinator_tx);
            }
            OverlayMessage::Command(OverlayCommand::ApplyPreset(id)) => {
                match presets::apply_preset(&mut cfg, id) {
                    Ok(outcome) => {
                        edit.selection.clear();
                        if let Some(ms) = monitors_map.get_mut(&primary_id) {
                            let renderer = Renderer {
                                device: &device,
                                target: &mut ms.target,
                            };
                            resync_sprites(
                                &renderer,
                                &cfg,
                                &mut sprites,
                                &mut animations,
                                &mut videos,
                                audio_mixer.as_ref(),
                            );
                        }
                        if let Err(e) = config::save(&cfg, &config_path) {
                            tracing::warn!(error = %e, "не удалось сохранить config.json после применения пресета");
                        }
                        occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                        rebuild_ui_panels(&mut edit, &cfg, &monitor_geometry);
                        if !outcome.missing.is_empty() {
                            let _ = edit
                                .coordinator_tx
                                .send(CoordinatorRequest::PresetMissingElements(outcome.missing));
                        }
                        need_redraw = true;
                    }
                    Err(e) => {
                        tracing::warn!(preset = %id, error = %e, "не удалось применить пресет")
                    }
                }
            }
            OverlayMessage::Command(OverlayCommand::RenamePreset(id, name)) => {
                match presets::rename_preset(&mut cfg, id, name) {
                    Ok(()) => {
                        if let Err(e) = config::save(&cfg, &config_path) {
                            tracing::warn!(error = %e, "не удалось сохранить config.json после переименования пресета");
                        }
                        notify_presets_changed(&cfg, &edit.coordinator_tx);
                    }
                    Err(e) => {
                        tracing::warn!(preset = %id, error = %e, "не удалось переименовать пресет")
                    }
                }
            }
            OverlayMessage::Command(OverlayCommand::DeletePreset(id)) => {
                match presets::delete_preset(&mut cfg, id) {
                    Ok(()) => {
                        if let Err(e) = config::save(&cfg, &config_path) {
                            tracing::warn!(error = %e, "не удалось сохранить config.json после удаления пресета");
                        }
                        notify_presets_changed(&cfg, &edit.coordinator_tx);
                    }
                    Err(e) => tracing::warn!(preset = %id, error = %e, "не удалось удалить пресет"),
                }
            }
            OverlayMessage::Command(OverlayCommand::ExportPreset(id, path)) => {
                if let Err(e) = presets::export_preset_to_file(&cfg, id, &path) {
                    tracing::warn!(preset = %id, path = %path.display(), error = %e, "не удалось экспортировать пресет");
                }
            }
            OverlayMessage::Command(OverlayCommand::ImportPreset(path)) => {
                match presets::import_preset_from_file(&mut cfg, &path) {
                    Ok(preset) => {
                        tracing::info!(preset = %preset.id, path = %path.display(), "пресет импортирован");
                        if let Err(e) = config::save(&cfg, &config_path) {
                            tracing::warn!(error = %e, "не удалось сохранить config.json после импорта пресета");
                        }
                        notify_presets_changed(&cfg, &edit.coordinator_tx);
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "не удалось импортировать пресет");
                    }
                }
            }
            OverlayMessage::Command(OverlayCommand::AddDenylistRule(rule)) => {
                if rule.process_name.is_none() && rule.title_pattern.is_none() {
                    // Пустое правило не матчит ничего
                    // (rst_core::occluders::rule_matches) — хранить его в
                    // списке бессмысленно, отбрасываем с логом.
                    tracing::warn!(
                        "denylist: пустое правило (ни process_name, ни title_pattern) — не добавлено"
                    );
                } else if cfg.settings.denylist.contains(&rule) {
                    tracing::info!(
                        ?rule,
                        "denylist: правило уже в списке — дубликат не добавлен"
                    );
                } else {
                    cfg.settings.denylist.push(rule);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после добавления правила денй-листа");
                    }
                }
            }
            OverlayMessage::Command(OverlayCommand::RemoveDenylistRule(index)) => {
                if index < cfg.settings.denylist.len() {
                    cfg.settings.denylist.remove(index);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после удаления правила денй-листа");
                    }
                } else {
                    tracing::warn!(
                        index,
                        len = cfg.settings.denylist.len(),
                        "denylist: удаление по индексу за пределами списка — пропускаем"
                    );
                }
            }
            OverlayMessage::Command(OverlayCommand::Shutdown) => break,
            OverlayMessage::Event(monitor_id, OverlayEvent::ToggleEditMode) => {
                let Some(ms) = monitors_map.get_mut(&monitor_id) else {
                    continue;
                };
                let renderer = Renderer {
                    device: &device,
                    target: &mut ms.target,
                };
                toggle_edit_mode(
                    &ms.overlay,
                    &mut edit,
                    &mut cfg,
                    &mut sprites,
                    &mut animations,
                    &mut videos,
                    audio_mixer.as_ref(),
                    &renderer,
                    &config_path,
                    &monitor_geometry,
                    &monitor_bounds,
                    &mut window_pins,
                    &window_snapshot,
                );
                // Клик-прозрачность/захват мыши синхронизируются со ВСЕХ
                // остальных мониторов — иначе мышь на них проваливалась бы
                // сквозь режим редактирования (M3_PREP_NOTES.md, раздел 3.5)
                // или залипала бы после выхода (см. доккомент
                // `sync_other_monitors_edit_mode`).
                sync_other_monitors_edit_mode(&monitors_map, &monitor_id, edit.active);
                need_redraw = true;
            }
            OverlayMessage::Event(_, OverlayEvent::ToggleAllStickers) => {
                // Глобальный хоткей работает независимо от режима
                // редактирования (M2b7) — та же логика, что у BTN_TOGGLE_ALL
                // на панели у курсора (docs/M2_WIRING_PLAN.md, раздел 6).
                let before = cfg.clone();
                if converge_all_stickers_visibility(&mut cfg) {
                    commit_undo_snapshot(&mut edit, before);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после «показать/скрыть все»");
                    }
                    // Панель у курсора живёт на `cursor_monitor`, а не
                    // обязательно на мониторе, приславшем этот хоткей (он
                    // зарегистрирован только на основном, M3, см.
                    // docs/M3_STEP4_REVIEW.md, пункт 2.2).
                    if let Some(&(w, h, s)) = monitor_geometry.get(&edit.cursor_monitor) {
                        let screen = screen_dip_rect((w, h), s);
                        rebuild_cursor_panel(&mut edit, &cfg, &screen);
                    }
                    need_redraw = true;
                }
            }
            OverlayMessage::Event(_, OverlayEvent::ToggleMuteAll) => {
                // Глобальный хоткей «заглушить все» (M5d, SPEC.md §7.1):
                // `AudioMixer::set_muted` уже существовал (используется
                // приглушением невидимых стикеров), не хватало только
                // подключения хоткея. Рантайм-тумблер, ничего не пишем в
                // config.json.
                audio_muted = !audio_muted;
                if let Some(mixer) = audio_mixer.as_ref() {
                    mixer.set_muted(audio_muted);
                }
            }
            OverlayMessage::Event(_, OverlayEvent::PinFocusedWindow) => {
                // Хоткей «закрепить/открепить сфокусированное окно»
                // (редизайн пинов, SPEC «Закрепление окна», пункт 1) —
                // работает независимо от режима редактирования.
                toggle_focused_pin(&mut edit, &cfg, &monitor_bounds, &mut window_pins);
                // `pinned_window_dip_placement`, который рисует пульс, читает
                // координаторский `window_snapshot` — он обновляется только
                // асинхронно, по следующему `Windows(Changed)` от трекера
                // (баг, живой репорт пользователя 2026-08-19: «свечение
                // перестало появляться при закреплении»). Без этой строки
                // первый(е) кадр(ы) пульса рисуются раньше, чем окно попадёт
                // в снимок — рамка не находит `placement` и просто не рисуется,
                // а к моменту, когда снимок наконец обновится, пульс уже
                // истёк или почти истёк. Разовое перечисление здесь — тот же
                // приём и та же цена, что у `pending_open_pick_list` выше и у
                // внутреннего `enumerate()` в `toggle_focused_pin`.
                window_snapshot = rst_win32::window_enum::enumerate();
                // Пульс рамки при пин/анпин стартовал в `pin_window`/
                // `unpin_window` — нужен немедленный редрав (первый кадр
                // пульса), дальше кадры крутит планировщик анимаций своим
                // дедлайном (см. `PinFlash::next_deadline`).
                need_redraw = true;
            }
            OverlayMessage::Event(monitor_id, OverlayEvent::DpiChanged { dpi, size }) => {
                // Окно уже переехало на рекомендованный прямоугольник
                // (rst_win32::overlay::handle_dpi_changed) — здесь досчитываем
                // рендер: пересоздать цепочку под новый размер, обновить
                // масштаб DIP→физика и геометрию тулбара/панели у курсора,
                // которая от него зависит (docs/M3_PREP_NOTES.md, раздел 3.6).
                let Some(ms) = monitors_map.get_mut(&monitor_id) else {
                    continue;
                };
                let old_scale = ms.scale;
                ms.width = size.0;
                ms.height = size.1;
                // Масштаб и (если курсор сейчас на этом мониторе) его новая
                // DIP-позиция — чистая арифметика, см. `dpi_change_scale_and_cursor`
                // (вынесена ради юнит-тестов на 100/150/200%, ROADMAP.md M3).
                // Если курсор на другом мониторе, его DIP уже в системе
                // координат ТОГО, другого, монитора — трогать не надо, поэтому
                // курсор пересчитывается тем же значением `old_scale`,
                // которое эта смена DPI и не меняла бы (ratio = 1.0), а сам
                // пересчёт применяется только под условием ниже.
                let (new_scale, new_cursor_pos) =
                    dpi_change_scale_and_cursor(old_scale, dpi, edit.cursor_pos);
                ms.scale = new_scale;
                ms.target.set_dpi_scale(ms.scale);
                if let Err(e) = ms.target.resize(&device, ms.width, ms.height) {
                    tracing::error!(error = %e, "не удалось пересоздать цепочку рендера после смены DPI");
                }
                // Пересчёт курсора применяется без ожидания следующего
                // MouseMove (который без этого не пересобрал бы панель
                // вовсе — hover-ветка обновляет только состояние
                // существующей панели, docs/M3_STEP2_3_REVIEW.md, пункт 2.2).
                if edit.cursor_monitor == monitor_id {
                    edit.cursor_pos = new_cursor_pos;
                }
                monitor_geometry.insert(monitor_id.clone(), (ms.width, ms.height, ms.scale));
                if let Some(bounds) = monitor_bounds.get_mut(&monitor_id) {
                    bounds.bounds_px.w = ms.width;
                    bounds.bounds_px.h = ms.height;
                    bounds.scale = ms.scale as f64;
                }
                // Найдено независимым ревью (2026-08-04): без пересчёта здесь
                // `occluder_cache` этого монитора остаётся в СТАРОМ
                // монитор-локальном пространстве физических px до следующего
                // `Windows(Changed)` — маска резалась бы не там (видимый
                // баг, не просто задержка), пока не придёт первое реальное
                // изменение окон после смены DPI.
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                rebuild_ui_panels(&mut edit, &cfg, &monitor_geometry);
                need_redraw = true;
            }
            OverlayMessage::Event(_, OverlayEvent::HotkeyConflict(name, combo)) => {
                // Окно продолжает работать без входа в режим редактирования;
                // тост трея (`ShowNotification`) — тот же канал, что M6 уже
                // использует для `PinAccessDenied` (общая инфраструктура,
                // не задача-заглушка «на потом»). `name` — какой именно из
                // трёх хоткеев конфликтует (раньше лог/тост всегда говорил
                // «режим редактирования», даже когда на самом деле не
                // регистрировался toggle_all/mute_all).
                tracing::warn!(?name, combo = %combo, "хоткей уже занят другим приложением");
                let (title, body) =
                    crate::i18n::hotkey_conflict_notification(&cfg.settings.language, name, &combo);
                let _ = edit
                    .coordinator_tx
                    .send(CoordinatorRequest::ShowNotification {
                        title,
                        body: body.clone(),
                    });
                // Баннер оверлея (решение координатора 2026-08-18): баллун
                // трея на Windows 11 25H2 молча не рендерится (доказано
                // живым стендом) — а баннер рисует сам resticker своим
                // render pipeline, его гарантированно видно. Показываем на
                // primary (там зарегистрированы глобальные хоткеи), текст —
                // тот же локализованный.
                show_banner(&mut edit, &primary_id, body);
                need_redraw = true;
            }
            OverlayMessage::Event(_, OverlayEvent::MonitorsChanged(new_infos)) => {
                // Диффинг — всегда против ЖИВОГО состояния (`monitors_map`),
                // не отдельной копии: WM_DISPLAYCHANGE рассылается КАЖДОМУ
                // живому окну, на одну смену топологии в канал приходит N
                // одинаковых событий; идемпотентный дифф по device interface
                // path делает дубликаты естественно безопасными
                // (M3_HOTPLUG_DESIGN.md §2).
                if new_infos.is_empty() {
                    tracing::warn!(
                        "WM_DISPLAYCHANGE: перечисление мониторов вернуло пустой список, игнорирую"
                    );
                    continue;
                }
                let new_snapshot: Vec<MonitorSnapshot> =
                    new_infos.iter().map(core_snapshot).collect();
                let new_primary_id = new_infos
                    .iter()
                    .find(|m| m.is_primary)
                    .unwrap_or(&new_infos[0])
                    .id
                    .clone();

                // 1. Смена владельца глобального хоткея — ПЕРВОЙ: снять
                // регистрацию со старого primary (если его окно ещё живо) до
                // того, как новое окно попробует свою — иначе RegisterHotKey
                // наткнётся на ещё занятую комбинацию и уйдёт в
                // HotkeyConflict вместо «переезда». `RegisteredHotkey` живёт
                // на pump-потоке своего окна и снимается только вместе с
                // ним — «перерегистрация» здесь означает пересоздание окна.
                if new_primary_id != primary_id {
                    // `physically_gone: false` — монитор ещё на месте, просто
                    // больше не primary; сносим окно только ради
                    // перерегистрации хоткея, edit-state живого монитора
                    // трогать не нужно (M3_STEP5_6_REVIEW.md, пункт 2.3).
                    let old_primary_still_connected = new_infos.iter().any(|m| m.id == primary_id);
                    teardown_monitor_state(
                        &mut monitors_map,
                        &mut monitor_geometry,
                        &mut monitor_bounds,
                        &primary_id,
                        &mut edit,
                        &mut cfg,
                        &mut sprites,
                        &new_primary_id,
                        !old_primary_still_connected,
                    );
                    // Старый primary мог остаться физически подключённым —
                    // просто больше не primary: тогда монитору нужно новое
                    // окно без хоткея, иначе он потеряет оверлей вовсе. Если
                    // он пропал совсем, ниже его не найдёт (не в new_infos) —
                    // снос уже случился, воссоздавать нечего.
                    if let Some(info) = new_infos.iter().find(|m| m.id == primary_id) {
                        if let Some(ms) = create_monitor_state(
                            &device,
                            &tx,
                            info,
                            None,
                            None,
                            None,
                            None,
                            edit.active,
                            cfg.settings.hide_from_capture,
                        ) {
                            monitors_map.insert(info.id.clone(), ms);
                        }
                    }
                    if let Some(info) = new_infos.iter().find(|m| m.id == new_primary_id) {
                        if let Some(ms) = create_monitor_state(
                            &device,
                            &tx,
                            info,
                            Some(hotkey),
                            toggle_all_hotkey,
                            mute_all_hotkey,
                            Some(pin_focused_hotkey),
                            edit.active,
                            cfg.settings.hide_from_capture,
                        ) {
                            monitors_map.insert(info.id.clone(), ms);
                        }
                    }
                }

                // 2. Прочие появившиеся мониторы → новое окно + цель +
                // форвардер. Хоткей достаётся тому, чей id == new_primary_id —
                // обычно это уже отфильтровано шагом 1 (contains_key), но
                // если создание окна нового primary там ПРОВАЛИЛОСЬ, эта
                // проверка даёт ему второй шанс с правильными хоткеями вместо
                // молчаливого создания без них (M3_STEP5_6_REVIEW.md, пункт
                // 2.4) — иначе глобальный хоткей остался бы незарегистрирован
                // до следующей смены primary.
                for info in &new_infos {
                    if monitors_map.contains_key(&info.id) {
                        continue;
                    }
                    let (edit_hotkey, this_toggle_all, this_mute_all, this_pin_focused) =
                        if info.id == new_primary_id {
                            (
                                Some(hotkey),
                                toggle_all_hotkey,
                                mute_all_hotkey,
                                Some(pin_focused_hotkey),
                            )
                        } else {
                            (None, None, None, None)
                        };
                    if let Some(ms) = create_monitor_state(
                        &device,
                        &tx,
                        info,
                        edit_hotkey,
                        this_toggle_all,
                        this_mute_all,
                        this_pin_focused,
                        edit.active,
                        cfg.settings.hide_from_capture,
                    ) {
                        monitors_map.insert(info.id.clone(), ms);
                    }
                }

                // 3. Пропавшие мониторы → снос (чек-лист в
                // teardown_monitor_state, включая edit-state:
                // `physically_gone: true`).
                let new_ids: HashSet<&MonitorId> = new_infos.iter().map(|m| &m.id).collect();
                let gone: Vec<MonitorId> = monitors_map
                    .keys()
                    .filter(|id| !new_ids.contains(id))
                    .cloned()
                    .collect();
                for id in gone {
                    teardown_monitor_state(
                        &mut monitors_map,
                        &mut monitor_geometry,
                        &mut monitor_bounds,
                        &id,
                        &mut edit,
                        &mut cfg,
                        &mut sprites,
                        &new_primary_id,
                        true,
                    );
                }

                // 3.5. Мониторы, оставшиеся подключёнными (в т.ч. только что
                // созданные шагами 1-2 — для них проверка ниже — идемпотентный
                // no-op, их размер уже точно совпадает с bounds_px), но
                // сменившие геометрию/DPI: подвинуть/растянуть окно и
                // пересоздать цепочку рендера под новый размер. Закрывает
                // ранее известный gap (M3_STEP5_6_REVIEW.md, пункт 2.5;
                // ROADMAP.md M3) — окно монитора, который никто не трогал в
                // шагах 1-3, раньше не реагировало на смену разрешения вовсе.
                // На не проверенном на смешанном DPI железе теоретически
                // возможен ping-pong с рекомендованным rect'ом WM_DPICHANGED,
                // если тот не совпадёт с `bounds_px` (M3_STEP8_REVIEW.md,
                // пункт 2.4) — не воспроизведено, не фикшено.
                for info in &new_infos {
                    let Some(ms) = monitors_map.get_mut(&info.id) else {
                        continue;
                    };
                    // `info.dpi` — из отдельного, свежего `enumerate()` для
                    // ЭТОГО монитора, а не живой DPI окна: `ms.overlay.dpi()`
                    // мог бы вернуть устаревшее значение, если WM_DISPLAYCHANGE
                    // придёт раньше WM_DPICHANGED (порядок не гарантирован) —
                    // `info.dpi` не зависит от того, добралось ли до окна
                    // отдельное DPI-уведомление (M3_STEP8_REVIEW.md, пункт 2.3).
                    let new_scale = info.dpi as f32 / 96.0;
                    let cached = (ms.x, ms.y, ms.width, ms.height, ms.scale);
                    if !monitor_geometry_changed(cached, info.bounds_px, new_scale) {
                        continue;
                    }
                    if !ms.overlay.set_bounds(info.bounds_px) {
                        // `ms.*` не обновлён — окно и `monitor_bounds`,
                        // который шаг 4 всё равно построит из `new_infos`,
                        // разойдутся до следующего WM_DISPLAYCHANGE
                        // (M3_STEP8_REVIEW.md, пункт 2.5): редкий системный
                        // сбой, не бесконечный цикл — просто «слепой» монитор
                        // до следующего триггера.
                        tracing::error!(
                            monitor = %info.id.0,
                            "не удалось переставить окно монитора после смены геометрии"
                        );
                        continue;
                    }
                    let old_scale = ms.scale;
                    ms.x = info.bounds_px.x;
                    ms.y = info.bounds_px.y;
                    ms.width = info.bounds_px.w;
                    ms.height = info.bounds_px.h;
                    ms.scale = new_scale;
                    ms.target.set_dpi_scale(ms.scale);
                    if let Err(e) = ms.target.resize(&device, ms.width, ms.height) {
                        tracing::error!(
                            error = %e,
                            monitor = %info.id.0,
                            "не удалось пересоздать цепочку рендера после смены геометрии монитора"
                        );
                    }
                    // Как в DpiChanged: физическая позиция курсора смена
                    // масштаба не меняет, только его представление в DIP.
                    if edit.cursor_monitor == info.id {
                        let ratio = old_scale / ms.scale;
                        edit.cursor_pos = (
                            edit.cursor_pos.0 * ratio as f64,
                            edit.cursor_pos.1 * ratio as f64,
                        );
                    }
                }

                // 4. Пересобрать снимки-копии из живого monitors_map — так
                // же, как на старте. Шаг 3.5 уже привёл `ms.width/height/
                // scale` в соответствие с `new_infos`, так что обе копии
                // здесь строятся из одного и того же снимка ОС и не
                // расходятся (бывший gap M3_STEP5_6_REVIEW.md, пункт 2.5 —
                // закрыт).
                monitor_geometry = monitors_map
                    .iter()
                    .map(|(id, ms)| (id.clone(), (ms.width, ms.height, ms.scale)))
                    .collect();
                monitor_bounds = new_infos
                    .iter()
                    .filter_map(|info| {
                        let ms = monitors_map.get(&info.id)?;
                        Some((
                            info.id.clone(),
                            MonitorBounds {
                                id: info.id.clone(),
                                bounds_px: info.bounds_px,
                                scale: ms.scale as f64,
                            },
                        ))
                    })
                    .collect();
                // Найдено независимым ревью (2026-08-04): без пересчёта
                // здесь `occluder_cache` остаётся в СТАРОМ монитор-локальном
                // пространстве физических px изменившихся мониторов до
                // следующего `Windows(Changed)` — маска резалась бы не там
                // (видимый баг, не просто задержка). `window_snapshot` сам
                // по себе сменой мониторов не устаревает — окна остаются
                // окнами, устарели только границы монитора, к которым их
                // клипает `occluder_rects_for`.
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);

                // 5. Новый «эталон» и прогон автомата потери монитора.
                last_snapshot = new_snapshot;
                let actions =
                    loss_tracker.on_monitor_snapshot(&last_snapshot, &cfg.stickers, Instant::now());
                if !actions.is_empty() {
                    apply_loss_actions(&mut cfg, &mut sprites, actions);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после миграции монитора");
                    }
                }
                // 6. Обновить `primary_id` — нужен `AddSticker` и фолбэкам.
                primary_id = new_primary_id;
                // Симметрично ветке DpiChanged (1059): geometry уже свежая
                // (шаг 4), панели должны это отразить сразу, а не ждать
                // несвязанный триггер — иначе тулбар ресайзнутого монитора
                // остаётся с прежней `screen_h`, а панель у курсора не
                // пере-клампится в новые границы (M3_STEP8_REVIEW.md, пункт
                // 2.2).
                rebuild_ui_panels(&mut edit, &cfg, &monitor_geometry);
                need_redraw = true;
            }
            OverlayMessage::Tick => {
                // Тик сам по себе снимок мониторов не меняет (диффить не
                // против чего) — одна из двух целей — дать шанс истечь
                // таймерам автомата потери монитора (M3_HOTPLUG_DESIGN.md
                // §1); редрав из-за автомата — только если он правда что-то
                // изменил.
                let actions =
                    loss_tracker.on_monitor_snapshot(&last_snapshot, &cfg.stickers, Instant::now());
                if !actions.is_empty() {
                    apply_loss_actions(&mut cfg, &mut sprites, actions);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после миграции монитора");
                    }
                    need_redraw = true;
                }
                // Вторая цель — ретрай восстановления устройства, если
                // предыдущая попытка (например, форсированная SystemResumed)
                // провалилась: раз в секунду форсируем ещё одну попытку
                // `redraw_all`/`recover_device`, пока адаптер не «осядет»
                // после сна (M3_SESSION_SLEEP_REVIEW.md, пункт 2.3) — без
                // этого чёрный оверлей мог бы простоять до следующего
                // несвязанного события.
                if device_needs_recovery {
                    need_redraw = true;
                }
            }
            OverlayMessage::Event(_, OverlayEvent::SessionLocked) => {
                // SPEC.md §9: на экране блокировки стикеры не видны — это
                // обеспечивает сама ОС (оверлеи принадлежат пользовательской
                // сессии), кода не требует. Приостановка декодирования видео/
                // звука из того же пункта была не наш случай до M5 — с M5a
                // анимация уже есть: пока сессия заблокирована, часы не
                // тикают и планировщику не шлётся новый дедлайн (см. низ
                // цикла), тем же принципом «не декодировать невидимое».
                tracing::info!("сессия Windows заблокирована");
                session_locked = true;
            }
            OverlayMessage::Event(_, OverlayEvent::SessionUnlocked) => {
                tracing::info!("сессия Windows разблокирована");
                session_locked = false;
                reenumerate_monitors(&tx, &primary_id, "разблокировки сессии");
            }
            OverlayMessage::Event(_, OverlayEvent::SystemSuspending) => {
                tracing::info!("система уходит в сон");
            }
            OverlayMessage::Event(_, OverlayEvent::SystemResumed) => {
                tracing::info!("система вышла из сна");
                // SPEC.md §9: при выходе из сна переинициализация D3D
                // обязательна — устройство могло быть потеряно. `redraw_all`/
                // `recover_device` уже умеют это обнаруживать и чинить, но
                // реактивно — только при следующей попытке `present`; здесь
                // просто гарантируем, что эта попытка случится немедленно, а
                // не будет ждать несвязанного события (может не быть долго,
                // если после сна ничего на экране не меняется). Редрав из-за
                // `need_redraw` случится в ЭТОЙ итерации, а `MonitorsChanged`
                // от `reenumerate_monitors` — только в следующей: если во сне
                // топология сменилась, этот кадр рисует по ещё старому
                // `monitors_map` (окно пропавшего монитора включительно).
                // Безвредно — либо тот `present` сам поймает потерю
                // устройства/укажет на несуществующий дисплей, либо кадр
                // просто не будет виден; дифф на следующей итерации снесёт
                // окно и перерисует (M3_SESSION_SLEEP_REVIEW.md, пункт 2.4).
                need_redraw = true;
                reenumerate_monitors(&tx, &primary_id, "выхода из сна");
            }
            OverlayMessage::Event(
                monitor_id,
                OverlayEvent::Key {
                    vk,
                    modifiers,
                    pressed: true,
                },
            ) if edit.active => {
                let Some(ms) = monitors_map.get_mut(&monitor_id) else {
                    continue;
                };
                let mut renderer = Renderer {
                    device: &device,
                    target: &mut ms.target,
                };
                let was_active = edit.active;
                need_redraw = handle_key(
                    vk,
                    modifiers,
                    &ms.overlay,
                    &mut renderer,
                    &mut cfg,
                    &config_path,
                    &mut sprites,
                    &mut edit,
                    (ms.width, ms.height),
                    ms.scale,
                    &monitor_id,
                    &monitor_geometry,
                    &monitor_bounds,
                    &window_snapshot,
                    &mut occluder_cache,
                    &mut animations,
                    &mut videos,
                    audio_mixer.as_ref(),
                    &mut window_pins,
                );
                // `Esc` внутри `handle_key` может дёрнуть `toggle_edit_mode`
                // напрямую (без доступа к `monitors_map`, см. доккомент
                // `sync_other_monitors_edit_mode`) — синхронизируем остальные
                // мониторы здесь, где `ms`/`renderer` уже не заняты картой.
                if edit.active != was_active {
                    sync_other_monitors_edit_mode(&monitors_map, &monitor_id, edit.active);
                }
            }
            OverlayMessage::Event(_, OverlayEvent::Key { .. }) => {}
            OverlayMessage::Event(monitor_id, OverlayEvent::Input(event)) if edit.active => {
                let Some(ms) = monitors_map.get_mut(&monitor_id) else {
                    continue;
                };
                let mut renderer = Renderer {
                    device: &device,
                    target: &mut ms.target,
                };
                let was_active = edit.active;
                need_redraw = handle_input(
                    event,
                    ms.scale,
                    &ms.overlay,
                    &mut renderer,
                    (ms.width, ms.height),
                    &mut cfg,
                    &config_path,
                    &mut sprites,
                    &mut edit,
                    &monitor_id,
                    &monitor_geometry,
                    &monitor_bounds,
                    &mut loss_tracker,
                    &window_snapshot,
                    &mut occluder_cache,
                    &mut animations,
                    &mut videos,
                    audio_mixer.as_ref(),
                    &mut window_pins,
                );
                // Кнопка «выйти» тулбара у курсора (BTN_EXIT) дёргает
                // `toggle_edit_mode` внутри `handle_input` напрямую (см.
                // доккомент `sync_other_monitors_edit_mode`) — синхронизируем
                // остальные мониторы здесь же.
                if edit.active != was_active {
                    sync_other_monitors_edit_mode(&monitors_map, &monitor_id, edit.active);
                }
            }
            OverlayMessage::Event(_, OverlayEvent::Input(_)) => {
                // Вне режима редактирования окно клик-прозрачно — эти
                // события приходить не должны, но игнорируем на всякий случай.
            }
            OverlayMessage::Windows(TrackerWindowEvent::Changed(windows)) => {
                // M4: снимок окон трекера — пересчитываем группы окклюдеров
                // по всем мониторам (M4_OCCLUDERS_DESIGN.md §1) и просим
                // перерисовать: старая маска могла прятать/показывать
                // стикер неверно относительно нового расположения окон.
                tracing::debug!(count = windows.len(), "снимок окон обновлён");
                window_snapshot = windows;
                // Редизайн пинов (SPEC «Закрепление окна»): рантайм-
                // обслуживание закреплённых окон — снос уничтоженных,
                // z-order-слоты/временный topmost по фокусу, move-lock
                // snap-back — до пересчёта окклюдеров (тот же порядок, что у
                // удалённой sync_window_stickers). Внутри стоит гейт на
                // edit-mode: пока редактирование активно, всё принуждение
                // молчит (пункт 5 спеки).
                if !edit.active {
                    maintain_pinned_windows(&mut edit, &window_snapshot, &mut window_pins);
                    enforce_pinned_geometry(&mut edit, &window_pins, &monitor_bounds);
                    // Закреплённое окно живёт в той же topmost-полосе, что и
                    // оверлей, и активация поднимает его НАД нами — бейдж
                    // «закреплено» и индикаторы замков уходят под окно
                    // (репорт пользователя 2026-08-21). Возвращаем оверлеи
                    // наверх, но только когда пин реально оказался выше:
                    // безусловный подъём означал бы z-order-войну с чужими
                    // topmost-приложениями.
                    if !edit.pinned_windows.is_empty() {
                        let pinned: Vec<usize> =
                            edit.pinned_windows.iter().map(|p| p.hwnd as usize).collect();
                        for state in monitors_map.values() {
                            state.overlay.raise_above_pinned(&pinned);
                        }
                    }
                }
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                // Открытая панель выбора окон показывает СТАРЫЙ снимок —
                // пересобрать её тем же новым снимком (M4_WINDOW_PICKER_DESIGN.md
                // §5): список окон должен обновляться в реальном времени
                // (SPEC 4.2), не только при переключении чекбоксов.
                rebuild_window_picker(&mut edit, &cfg, &window_snapshot, &monitor_geometry);
                // Список закрепления (M6) — та же логика: пока открыт,
                // должен отражать реально открытые окна, а не застывший
                // снимок на момент нажатия BTN_ADD_WINDOW.
                rebuild_window_pick_list(&mut edit, &cfg, &window_snapshot, &monitor_geometry);
                // Панель свойств закреплённого окна — та же логика: окно
                // могло переехать на другой монитор, пока панель открыта.
                rebuild_pinned_panel(&mut edit, &window_snapshot, &monitor_bounds);
                need_redraw = true;
            }
            OverlayMessage::AnimationTick => {
                // Планировщик разбудил нас на ближайший известный ему
                // дедлайн (M5a §5) — продвигаем часы всех анимаций, которым
                // сейчас положено тикать (видимых и не полностью
                // перекрытых, ARCHITECTURE.md §4.3); новый спрайт-кадр
                // пишем прямо в `sprites` — `redraw()` их уже читает
                // одинаково для статичных и анимированных стикеров (UV по
                // умолчанию — вся текстура).
                let now = Instant::now();
                // Пульсы рамки при пин/анпин (запрос пользователя
                // 2026-08-18) — пока хоть один жив, редрав нужен: прозрачность
                // меняется каждый кадр. Истёкшие (старше 1 с) вычищаем здесь
                // же — иначе копились бы вечно.
                if edit
                    .pin_flashes
                    .iter()
                    .any(|f| !f.expired(now))
                {
                    need_redraw = true;
                }
                edit.pin_flashes.retain(|f| !f.expired(now));
                // Закреплённое окно сейчас тащат/ресайзят силами ОС: кадр
                // обязан идти вровень с ним, а панель инструментов — ехать
                // вместе с окном (она рисуется ВНУТРИ него).
                if edit.pinned_follow_until.is_some_and(|until| until > now) {
                    rebuild_pinned_panel(&mut edit, &window_snapshot, &monitor_bounds);
                    need_redraw = true;
                    // Жест закончился — доводим окно до лимита и до кромки
                    // прямо здесь, не дожидаясь снимка трекера: после
                    // отпускания кнопки окно больше не двигается, а значит
                    // события `LOCATIONCHANGE` могут и не прийти вовсе, и
                    // потолок с магнитом остались бы неприменёнными.
                    let still_dragging = edit
                        .pinned_windows
                        .iter()
                        .any(|p| rst_win32::window_pin::is_user_dragging(p.hwnd as usize));
                    if !still_dragging && !edit.active {
                        enforce_pinned_geometry(&mut edit, &window_pins, &monitor_bounds);
                    }
                }
                // Баннер предупреждений (конфликт хоткея и т.п.) — тот же
                // паттерн: жив — редрав нужен, истёк — вычищаем.
                if edit
                    .banner
                    .as_ref()
                    .is_some_and(|b| !b.expired(now))
                {
                    need_redraw = true;
                }
                if edit.banner.as_ref().is_some_and(|b| b.expired(now)) {
                    edit.banner = None;
                }
                // Тултип ещё анимируется (задержка показа или плавное
                // появление, фидбэк пользователя 2026-08-10) — редрав нужен,
                // даже если ни одна анимация стикера/видео сейчас не тикает.
                if edit
                    .tooltip
                    .as_ref()
                    .is_some_and(|t| t.next_deadline(now).is_some())
                {
                    need_redraw = true;
                }
                for (id, anim) in animations.iter_mut() {
                    let Some(sticker) = cfg.stickers.iter().find(|s| s.id == *id) else {
                        continue;
                    };
                    if !sticker_should_tick(sticker, edit.active, &occluder_cache, &monitor_bounds)
                    {
                        continue;
                    }
                    match anim {
                        StickerAnimation::Atlas {
                            atlas,
                            clock,
                            delays,
                        } => {
                            if clock.advance(now, delays) {
                                let frame = atlas.frames[clock.frame_index];
                                if let Some((_, sprite)) =
                                    sprites.iter_mut().find(|(sid, _)| sid == id)
                                {
                                    sprite.uv_offset = frame.uv_offset;
                                    sprite.uv_scale = frame.uv_scale;
                                }
                                need_redraw = true;
                            }
                        }
                        StickerAnimation::Streaming {
                            source,
                            texture,
                            deadline,
                        } => {
                            if now >= *deadline {
                                match source.next_frame() {
                                    Ok(frame) => {
                                        if let Err(e) = device
                                            .update_streaming_animation_frame(texture, &frame.rgba)
                                        {
                                            tracing::warn!(error = %e, sticker = %id, "потоковая анимация: не удалось обновить кадр");
                                        } else {
                                            need_redraw = true;
                                        }
                                        *deadline = now + frame.delay;
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, sticker = %id, "потоковая анимация: ошибка декодирования кадра — тик остановлен на час");
                                        *deadline = now + Duration::from_secs(3600);
                                    }
                                }
                            }
                        }
                    }
                }
                // Видео (M5b, docs/M5B_VIDEO_DESIGN.md §6): декодер сам держит
                // темп по PTS в своём потоке, координатор здесь только
                // выкачивает уже готовые кадры/звук неблокирующе. Кадры
                // копятся в очереди на 2-3 — берём ТОЛЬКО последний
                // (остальные устарели к моменту показа), а звук отдаём
                // микшеру целиком, по порядку, ни одной порции не пропуская.
                for (id, playback) in videos.iter_mut() {
                    // Не выкачиваем кадры скрытого/полностью перекрытого
                    // видео из очереди декодера — тот же гейт, что у
                    // Atlas/Streaming-анимаций выше, чтобы кадр реально
                    // «стопался» на скрытии, а не менялся невидимо под
                    // шахматкой (фидбэк пользователя 2026-08-09). Очередь
                    // декодера ограничена (2-3 кадра), декодер сам применит
                    // бэкпрешер по PTS. Звук эта пауза не трогает — его
                    // включение/выключение отдельная логика (mute_invisible).
                    let should_tick = cfg
                        .stickers
                        .iter()
                        .find(|s| s.id == *id)
                        .is_some_and(|sticker| {
                            sticker_should_tick(sticker, edit.active, &occluder_cache, &monitor_bounds)
                        });
                    let mut latest = None;
                    if should_tick {
                        while let Some(frame) = playback.source.try_recv_frame() {
                            latest = Some(frame);
                        }
                    }
                    if let Some(frame) = latest {
                        if let Some((_, sprite)) = sprites.iter_mut().find(|(sid, _)| sid == id) {
                            if let Some(video) = &mut sprite.video {
                                if let Err(e) = device
                                    .update_video_textures(video, &frame.y, &frame.u, &frame.v)
                                {
                                    tracing::warn!(error = %e, sticker = %id, "не удалось обновить видеотекстуры");
                                } else {
                                    need_redraw = true;
                                }
                            }
                        }
                    }
                    if let Some(audio) = &playback.audio {
                        while let Some(chunk) = playback.source.try_recv_audio_samples() {
                            audio.push_samples(&chunk.samples);
                        }
                    }
                }
            }
        }
        // Анимация только что добавленного стикера (M5a §5, EditState::
        // pending_animation) — заводим часы здесь, где под рукой
        // `animations`; `add_sticker` не может сделать это сам, он не видит
        // локальную переменную цикла `run()`.
        if let Some((id, pending)) = edit.pending_animation.take() {
            let anim = match pending {
                PendingAnimation::Atlas(atlas) => {
                    StickerAnimation::from_atlas(atlas, AnimationClock::new(Instant::now()))
                }
                PendingAnimation::Streaming {
                    source,
                    texture,
                    first_delay,
                } => StickerAnimation::Streaming {
                    source,
                    texture,
                    deadline: Instant::now() + first_delay,
                },
            };
            animations.insert(id, anim);
        }
        // `VideoPlayback` только что добавленного видеостикера (M5b, то же
        // паттерн — см. доккомент `EditState::pending_video`).
        if let Some((id, playback)) = edit.pending_video.take() {
            videos.insert(id, playback);
        }
        // Стикер мог быть удалён (тулбар/`Delete`/undo-редо) — прунить раз
        // за итерацию, тем же дешёвым паттерном, что гейт `mask_needed`
        // ниже, а не в каждой из точек мутации `cfg.stickers` по отдельности.
        if !animations.is_empty() {
            animations.retain(|id, _| cfg.stickers.iter().any(|s| s.id == *id));
        }
        if !videos.is_empty() {
            videos.retain(|id, _| cfg.stickers.iter().any(|s| s.id == *id));
        }
        // Синхронизация Config → живые VideoSource/AudioSource (M5b) —
        // единая точка входа вместо разбросанных вызовов source.play()/
        // pause()/audio.set_volume() по местам мутации cfg (тулбар, undo/
        // redo, Esc-откат, CaptureLost): раньше откат cfg (Ctrl+Z после
        // паузы тулбаром, Esc после драга громкости) восстанавливал
        // playback.paused/volume в конфиге, но не толкал их в живые
        // источники — тулбар показывал одно состояние, видео/звук вели
        // себя по другому (независимое ревью сшивки, находка 1). Играть
        // должно ТОЛЬКО когда пользователь не поставил паузу, стикер сейчас
        // видим (то же «не декодировать невидимое», что у анимации,
        // `sticker_should_tick`) и сессия не заблокирована — иначе
        // перекрытый/скрытый стикер продолжал бы декодировать, играть звук
        // и держать координатор на `VIDEO_POLL_INTERVAL`-пробуждениях
        // впустую (то же ревью, находка 2).
        for (id, playback) in videos.iter_mut() {
            let Some(sticker) = cfg.stickers.iter().find(|s| s.id == *id) else {
                continue;
            };
            let should_play = !session_locked
                && !sticker.playback.paused
                && sticker_should_tick(sticker, edit.active, &occluder_cache, &monitor_bounds);
            let currently_playing = !playback.source.is_paused();
            if should_play && !currently_playing {
                playback.source.play();
            } else if !should_play && currently_playing {
                playback.source.pause();
            }
            if let Some(audio) = &playback.audio {
                audio.set_volume(sticker.playback.volume as f32);
            }
        }
        // Ближайший дедлайн среди анимаций, которым сейчас положено тикать —
        // пересчитывается после каждого сообщения (не только `AnimationTick`:
        // добавление/удаление стикера, скрытие/показ, любая мутация правила
        // видимости панелью/undo/redo — все меняют множество «тикающих»
        // анимаций или окклюдеры). Не шлём планировщику, пока сессия
        // заблокирована (см. `SessionLocked`) — и не шлём, если значение не
        // изменилось, чтобы не будить поток-планировщик впустую.
        //
        // Видео (M5b) участвует тем же дедлайном: пока играет хоть одно
        // видео, планировщику нужен ближайший тик не позже, чем через
        // `VIDEO_POLL_INTERVAL` — иначе `try_recv_frame` не вызывался бы
        // вовсе между несвязанными UI-событиями, и видео визуально
        // подвисало бы. На паузе у видео нет дедлайна вообще (новых кадров
        // не будет, опрашивать нечего) — тот же принцип «не тикать вникуда»,
        // что и у анимации.
        let next_anim_deadline = if session_locked {
            None
        } else {
            animations
                .iter()
                .filter_map(|(id, anim)| {
                    let sticker = cfg.stickers.iter().find(|s| s.id == *id)?;
                    sticker_should_tick(sticker, edit.active, &occluder_cache, &monitor_bounds)
                        .then(|| anim.next_deadline())
                })
                .min()
        };
        let next_video_deadline = if session_locked {
            None
        } else if videos.values().any(|v| !v.source.is_paused()) {
            Some(Instant::now() + VIDEO_POLL_INTERVAL)
        } else {
            None
        };
        // Тултип (фидбэк пользователя 2026-08-10): пока идёт задержка показа
        // или анимация появления, планировщику нужен дедлайн — тот же общий
        // канал `AnimationTick`, что у анимаций/видео (ADR-006: ноль
        // пробуждений в покое, дедлайн есть только пока реально что-то ещё
        // должно измениться).
        let next_tooltip_deadline = edit
            .tooltip
            .as_ref()
            .and_then(|t| t.next_deadline(Instant::now()));
        // Пульс рамки при пин/анпин (запрос пользователя 2026-08-18) — тот
        // же общий канал `AnimationTick`: пока пульс жив (1 с), планировщику
        // нужен дедлайн, иначе кадр не перерисуется сам по себе (в обычном
        // режиме без стикеров/видео «ноль пробуждений в покое», ADR-006).
        let next_flash_deadline = edit
            .pin_flashes
            .iter()
            .filter_map(|f| f.next_deadline(Instant::now()))
            .min();
        // Баннер предупреждений — пока жив, планировщику нужен дедлайн (тот
        // же канал `AnimationTick`, что у пульса/тултипа).
        let next_banner_deadline = edit
            .banner
            .as_ref()
            .and_then(|b| b.next_deadline(Instant::now()));
        // Слежение за закреплённым окном, которое пользователь тащит или
        // ресайзит сам: пока жест идёт (и короткий хвост после него),
        // планировщику нужен кадровый дедлайн — иначе графика поверх окна
        // обновлялась бы только по снимкам трекера и отставала (репорт
        // 2026-08-21).
        let now = Instant::now();
        if edit
            .pinned_windows
            .iter()
            .any(|p| rst_win32::window_pin::is_user_dragging(p.hwnd as usize))
        {
            edit.pinned_follow_until = Some(now + PIN_FOLLOW_TAIL);
        }
        let next_pin_follow_deadline = edit
            .pinned_follow_until
            .filter(|until| *until > now)
            .map(|_| now + PIN_FOLLOW_STEP);
        if next_pin_follow_deadline.is_none() {
            edit.pinned_follow_until = None;
        }
        let next_tick_deadline = [
            next_anim_deadline,
            next_video_deadline,
            next_tooltip_deadline,
            next_flash_deadline,
            next_banner_deadline,
            next_pin_follow_deadline,
        ]
            .into_iter()
            .flatten()
            .min();
        if next_tick_deadline != last_anim_deadline {
            last_anim_deadline = next_tick_deadline;
            let _ = anim_deadline_tx.send(next_tick_deadline);
        }
        // Открытие панели выбора окон отложено до этой точки — кликом по
        // `TB_LAYERS` в `handle_toolbar_up`, у которого нет `window_snapshot`
        // (дизайн §5.2); здесь, в цикле `run()`, снимок уже под рукой.
        if let Some(sticker_id) = edit.pending_open_picker.take() {
            open_window_picker(
                &mut edit,
                &cfg,
                &window_snapshot,
                &monitor_geometry,
                sticker_id,
            );
            need_redraw = true;
        }
        // Открытие списка окон для закрепления — тем же поводом отложено,
        // что открытие панели выбора окон выше (`BTN_ADD_WINDOW` в
        // `handle_cursor_panel_up` не имеет `window_snapshot`).
        //
        // Первый билдер получает СВЕЖЕЕ разовое перечисление, а не
        // `window_snapshot` координатора: гейт (`tracker_mask_needed`) будит
        // трекер только НИЖЕ по этому же тику, а сам асинхронный ответ
        // (`Windows(Changed)`) придёт следующей итерацией — до него
        // `window_snapshot` может быть тем, что осталось с прошлого раза,
        // когда трекер спал (пусто на чистом конфиге), и панель на кадр (а
        // если трекер почему-то не пришлёт свежий снимок сразу — то и
        // дольше) покажет не все реальные окна (живой репорт пользователя,
        // 2026-08-17: «программа видит только 2 окна» при открытых
        // десятках). `rebuild_window_pick_list` ниже по `Windows(Changed)`
        // и дальше продолжает читать живой кэш как обычно — разовое
        // перечисление нужно только для первого кадра.
        if let Some((monitor_id, purpose)) = edit.pending_open_pick_list.take() {
            let fresh_snapshot = rst_win32::window_enum::enumerate();
            open_window_pick_list(
                purpose,
                &mut edit,
                &cfg,
                &fresh_snapshot,
                &monitor_geometry,
                monitor_id,
            );
            need_redraw = true;
        }
        // Гейт хуков трекера — раз за итерацию, дёшево (см. комментарий у
        // объявления `last_mask_needed`); переключается только при реальном
        // изменении, не на каждой итерации подряд. Пока открыта панель
        // выбора окон, снимок обязан оставаться живым независимо от
        // `mask_needed(cfg)` (дизайн §5.1) — иначе если ВСЕ стикеры сейчас
        // `Always`, трекер спит, и список окон в панели не наполнится вовсе.
        let new_mask_needed = tracker_mask_needed(&cfg, &edit);
        if new_mask_needed != last_mask_needed {
            last_mask_needed = new_mask_needed;
            if let Some(tracker) = &window_tracker {
                tracker.set_mask_needed(last_mask_needed);
            }
        }
        if need_redraw
            && redraw_all(
                &device,
                &mut monitors_map,
                &sprites,
                &cfg,
                &edit,
                &white_tex,
                &black_tex,
                &mut ui_cache,
                &occluder_cache,
                &window_snapshot,
                &monitor_bounds,
            )
        {
            if recover_device(
                &mut device,
                &mut monitors_map,
                &mut white_tex,
                &mut black_tex,
                &mut sprites,
                &cfg,
                &mut ui_cache,
                &mut animations,
                &mut videos,
                audio_mixer.as_ref(),
            ) {
                device_needs_recovery = false;
                redraw_all(
                    &device,
                    &mut monitors_map,
                    &sprites,
                    &cfg,
                    &edit,
                    &white_tex,
                    &black_tex,
                    &mut ui_cache,
                    &occluder_cache,
                    &window_snapshot,
                    &monitor_bounds,
                );
            } else {
                device_needs_recovery = true;
            }
        }
    }
    // Гарантированное открепление стикеров-окон при выходе (ROADMAP.md M6,
    // SPEC.md §5): без этого WS_EX_TOPMOST на чужих окнах пережил бы процесс
    // resticker — цель ушла бы «навсегда поверх всего» до перезапуска той
    // программы. Аварийный выход (kill/crash) этим не покрыт по определению
    // (код после него не выполняется) — тот случай ROADMAP относит к
    // «известный предел», не к этой гарантии.
    window_pins.unpin_all();
    // monitors_map (Device+WindowTarget-обёртки внутри Renderer конструируются
    // временно и не переживают итерацию) и device освобождаются здесь;
    // MonitorState.target дропается раньше MonitorState.overlay в порядке
    // полей — тот же безопасный порядок «рендер раньше окна», что был у
    // одного монитора (COM/DComp раньше HWND).
}

#[allow(clippy::too_many_arguments)]
fn toggle_edit_mode(
    overlay: &OverlayWindow,
    edit: &mut EditState,
    cfg: &mut Config,
    sprites: &mut Vec<(Uuid, Sprite)>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    renderer: &Renderer,
    config_path: &Path,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_pins: &mut WindowPins,
    window_snapshot: &[WindowInfo],
) {
    // Выход во время незавершённого жеста откатывает его — так же, как
    // CaptureLost, а не коммитит середину перетаскивания (docs/M2_SLICE_REVIEW.md,
    // пункт 4: раньше это было асимметрично).
    if let Some(gesture) = edit.gesture.take() {
        if let Some(start) = gesture.start() {
            apply_transform(
                cfg,
                sprites,
                start.id,
                start.placement.clone(),
                start.transform,
            );
        }
    }
    // Тот же откат для незавершённого жеста закреплённого окна — реальный
    // HWND возвращается на стартовый rect (SPEC «Закрепление окна», пункт 9:
    // выход посреди драга/ресайза — та же семантика «CaptureLost -> отменить
    // жест без коммита», что у стикеров выше).
    if let Some(gesture) = edit.pinned_gesture.take() {
        let (hwnd, gesture_monitor, start_placement) = match gesture {
            PinnedGesture::Drag {
                hwnd,
                monitor_id,
                start_placement,
                ..
            } => (hwnd, monitor_id, start_placement),
            PinnedGesture::Resize {
                hwnd,
                monitor_id,
                start_placement,
                ..
            } => (hwnd, monitor_id, start_placement),
        };
        if let Some(bounds) = monitor_bounds.get(&gesture_monitor) {
            let (x, y, w, h) = placement_to_physical_rect(&start_placement, bounds);
            let _ = window_pins.move_resize(hwnd as usize, x, y, w, h);
        }
    }
    edit.pending_snapshot = None;
    edit.marquee = None;
    edit.marquee_started = false;
    // Незавершённый драг ползунка прозрачности — тот же принцип: откатить,
    // а не коммитить и не оставлять снимок висеть до случайного коммита на
    // следующем клике тулбара (docs/M2_SLICE6_REVIEW.md, пункт 2.1).
    if let Some(before) = edit.ui_pending_snapshot.take() {
        *cfg = before;
        resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
    }
    edit.pointer_owner = PointerOwner::None;
    // Панели/модалы сеанса редактирования не переживают переключение
    // режима — как и незавершённый жест выше, они относятся к сеансу, а не
    // к самому кадру (docs/M2_WIRING_PLAN.md, раздел 7). Выделено в
    // отдельную функцию, чтобы тест мог прогнать «путь выхода» без полного
    // `toggle_edit_mode` (тому нужны реальные `OverlayWindow`/`Renderer`/
    // `WindowPins`).
    let exiting = edit.active;
    reset_edit_mode_panels(edit, exiting);
    edit.active = !edit.active;
    overlay.set_click_through(!edit.active);
    if edit.active {
        // Вход в режим редактирования: ВСЁ пиновое принуждение встаёт
        // (SPEC «Закрепление окна», пункт 5) — замки снимаются реально, но
        // флаги конфигурации не трогаются; поднятые z-order-окна
        // возвращаются в слоты.
        suspend_pin_enforcement(edit, window_snapshot, window_pins);
    } else {
        // Выход: принуждение возобновляется ровно как сконфигурировано —
        // замки по сохранённым флагам (`set_move_lock(true)` заодно
        // переснимает эталонный rect: окно могли двигать в edit-mode),
        // z-order-слоты подхватит следующий `Windows(Changed)`.
        resume_pin_enforcement(edit, window_pins);
        // Хоткей выхода мог сработать, пока пользователь ещё держит кнопку
        // мыши (тянет ползунок/жест) — обычный цикл Down→Up, который снял бы
        // Win32-захват сам, тогда не наступает вовремя, и уже
        // клик-прозрачное окно продолжает монопольно получать всю мышь
        // системы до следующего физического отпускания кнопки (найдено при
        // разборе бага «HUD/мышь живут своей жизнью» — совместная сессия с
        // ботами-воркерами через Orca, 2026-08-08). Безопасно и когда
        // захвата нет вовсе.
        overlay.force_release_capture();
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json при выходе из режима редактирования");
        }
    }
    rebuild_ui_panels(edit, cfg, monitor_geometry);
}

/// Сбросить панели/модалы сеанса редактирования при переключении режима
/// (`toggle_edit_mode`). Часть сбрасывается на ЛЮБОЙ toggle (вход и выход) —
/// `confirm`, `window_picker`, список окон для закрепления `window_pick_list`
/// и панель пресетов `preset_picker` с их отложенными флагами открытия
/// (`pending_open_picker`/`pending_open_pick_list`), а также `tooltip`;
/// часть — только при выходе (`exiting`): выделение, `pinned_selection` и
/// панель свойств закреплённого окна `pinned_panel` (на входе они и так
/// пусты после предыдущего выхода).
///
/// Багфикс-раунд 2, задача D: `window_pick_list`/`pending_open_pick_list`/
/// `preset_picker` прежде здесь не сбрасывались — после выхода из режима
/// панель оставалась `Some(...)`, а `redraw` (в отличие от `toolbar`/
/// `cursor_panel`, которые `rebuild_ui_panels` гатит на `edit.active`) рисует
/// эти панели по одному только `Some(...)`, не гейтя на `edit.active`: панель
/// висела в центре экрана мёртвым клик-прозрачным UI — ровно «застрявшая
/// панель, с которой ничего не сделать» из репорта.
fn reset_edit_mode_panels(edit: &mut EditState, exiting: bool) {
    edit.confirm = None;
    edit.window_picker = None;
    edit.pending_open_picker = None;
    edit.pending_open_pick_list = None;
    edit.window_pick_list = None;
    edit.preset_picker = None;
    edit.tooltip = None;
    edit.pending_animation = None;
    edit.pending_video = None;
    if exiting {
        edit.selection.clear();
        edit.pinned_selection = None;
        edit.pinned_panel = None;
    }
}

/// Синхронизировать клик-прозрачность и Win32-захват мыши на ВСЕХ мониторах,
/// кроме `initiator_id`, после того как `toggle_edit_mode` сменил
/// `edit.active` — общая точка для каждого места, откуда можно выйти/войти в
/// режим редактирования (хоткей, `Esc`, кнопка «выйти» тулбара у курсора).
///
/// До этой функции синхронизацию делал только обработчик хоткея (единственное
/// место, где `toggle_edit_mode` мог быть вызван вместе с доступом к
/// `monitors_map` в одной точке) — `Esc` (`handle_key`) и кнопка «выйти»
/// (`handle_cursor_panel_up` → `BTN_EXIT`) вызывали `toggle_edit_mode`
/// напрямую и НЕ синхронизировали остальные мониторы: если пользователь
/// выходил из режима редактирования не хоткеем (а это как минимум так же
/// частый путь, если не чаще), не-главные мониторы оставались в интерактивном
/// (не клик-прозрачном) режиме навсегда — ровно баг «не могу нажать ЛКМ/ПКМ
/// на втором мониторе», о котором пользователь сообщил повторно 2026-08-09
/// уже ПОСЛЕ фикса, который синхронизировал только путь через хоткей.
/// Вызывающий код обязан сравнить `edit.active` до/после своего вызова
/// `toggle_edit_mode` (прямого или через `handle_key`/`handle_input`) и
/// позвать это здесь при реальном изменении — единой точки внутри самого
/// `toggle_edit_mode` не сделать: некоторые вызывающие держат `&mut
/// monitors_map`-производный `Renderer` (это боррош конфликтует с `&
/// monitors_map`, нужным для обхода остальных мониторов) на момент вызова.
fn sync_other_monitors_edit_mode(
    monitors_map: &HashMap<MonitorId, MonitorState>,
    initiator_id: &MonitorId,
    edit_active: bool,
) {
    for (other_id, other_ms) in monitors_map.iter() {
        if other_id != initiator_id {
            other_ms.overlay.set_interactive(edit_active);
            if !edit_active {
                other_ms.overlay.force_release_capture();
            }
        }
    }
}

/// Положить готовый снимок `Config` в историю undo и очистить историю redo —
/// новая ветка истории (семантика как у `rst_core::undo::UndoStack::push`).
/// Снимок берётся заранее (см. `pending_snapshot`), а не всегда «текущий
/// cfg», чтобы жест мог отложить решение до `MouseUp` (пункт 1 ревью).
fn commit_undo_snapshot(edit: &mut EditState, snapshot: Config) {
    if edit.undo_stack.len() == UNDO_CAPACITY {
        edit.undo_stack.remove(0);
    }
    edit.undo_stack.push(snapshot);
    edit.redo_stack.clear();
}

/// Привести `sprites` в соответствие с `cfg.stickers` после того, как `cfg`
/// целиком заменили (undo/redo): убрать спрайты стикеров, которых больше
/// нет, подгрузить текстуры для вернувшихся (undo удаления), синхронизировать
/// `placement`/`transform` для остальных (undo ресайза/поворота/перемещения).
fn resync_sprites(
    renderer: &Renderer,
    cfg: &Config,
    sprites: &mut Vec<(Uuid, Sprite)>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
) {
    sprites.retain(|(id, _)| cfg.stickers.iter().any(|s| s.id == *id));
    videos.retain(|id, _| cfg.stickers.iter().any(|s| s.id == *id));
    for sticker in &cfg.stickers {
        if let Some((_, sprite)) = sprites.iter_mut().find(|(id, _)| *id == sticker.id) {
            sprite.placement = sticker.placement.clone();
            sprite.transform = sticker.transform;
            continue;
        }
        // Стикер вернулся (undo удаления) или впервые появился (дублирование,
        // ops::duplicate) — `sprites` не содержит его. Анимация (M5a):
        // `load_sticker_sprite` сама решает статика это или атлас; на
        // `None` (не анимация/ошибка декода) — обычная статика через
        // `load_static_sprite`, той же логикой, что старт процесса и
        // `recover_device` (иначе именно этот путь молча терял анимацию —
        // найдено независимым ревью сшивки). Видео (M5b) — той же логикой,
        // отдельной веткой (взаимоисключающие `MediaType`, `load_sticker_video`
        // сама возвращает `None` для не-видео).
        if let Some((sprite, anim)) = load_sticker_sprite(renderer.device, sticker) {
            sprites.push((sticker.id, sprite));
            animations.insert(sticker.id, anim);
        } else if let Some((sprite, playback)) =
            load_sticker_video(renderer.device, sticker, audio_mixer)
        {
            sprites.push((sticker.id, sprite));
            videos.insert(sticker.id, playback);
        } else if let Some(sprite) = load_static_sprite(renderer.device, sticker) {
            sprites.push((sticker.id, sprite));
        }
    }
}

/// Путь к изображению стикера для загрузки текстуры: есть у `File` и
/// `Pasted` — оба ссылаются на файл на диске.
fn sticker_image_path(source: &StickerSource) -> Option<&Path> {
    match source {
        StickerSource::File { path, .. } => Some(path),
        StickerSource::Pasted { path } => Some(path),
    }
}

/// Натуральный размер уже загруженного спрайта стикера (окно настроек,
/// «сбросить размер и поворот», SPEC.md раздел 10): для видео (M5b) —
/// размеры плоскости Y (полное разрешение кадра), иначе — размеры обычной
/// текстуры/атласа (M5a/статика). Читает уже загруженный `Sprite`, а не
/// декодирует файл заново — дешёвый путь, у координатора текстура и так
/// уже на GPU. `None`, если стикер сейчас не материализован (спрайт не
/// загрузился — например, файл недоступен).
fn sticker_natural_size(sprites: &[(Uuid, Sprite)], id: Uuid) -> Option<(f64, f64)> {
    let (_, sprite) = sprites.iter().find(|(sid, _)| *sid == id)?;
    let (w, h) = match &sprite.video {
        Some(video) => (video.y.width() as f64, video.y.height() as f64),
        // `sprite.texture` для анимации (M5a) — весь текстурный атлас
        // (сетка кадров), не размер одного кадра; `uv_scale` — доля
        // атласа на кадр (1/columns, 1/rows), домножение даёт настоящий
        // натуральный размер. Для статики `uv_scale == [1.0, 1.0]`
        // (дефолт `Sprite::new`) — домножение безвредный no-op, той же
        // формулой не пришлось заводить отдельную ветку (найдено
        // независимым ревью: без домножения «сбросить размер и поворот»
        // растягивало анимированный стикер на N кадров по ширине).
        None => (
            sprite.texture.width() as f64 * sprite.uv_scale[0] as f64,
            sprite.texture.height() as f64 * sprite.uv_scale[1] as f64,
        ),
    };
    Some((w, h))
}

/// Загрузить спрайт стикера по текущему `cfg`-состоянию `sticker.source`:
/// статичная текстура или атлас анимации (M5a) — единая точка входа для
/// ЛЮБОГО места, материализующего спрайт из `Config` заново (старт
/// процесса, `resync_sprites` после undo/redo/дублирования/восстановления
/// удалённого, `recover_device` после потери D3D-устройства). Раньше эта
/// логика жила только внутри `recover_device` — из-за чего анимация
/// молча деградировала до статичного кадра 0 во всех остальных точках
/// (найдено независимым ревью сшивки, docs/M5A_ANIMATION_DESIGN.md §5).
/// `Some((_, Some(anim)))` — вызывающий код обязан завести запись в
/// `animations` для `sticker.id`; `Some((_, None))` — обычная статика.
fn load_sticker_sprite(device: &Device, sticker: &Sticker) -> Option<(Sprite, StickerAnimation)> {
    let path = sticker_image_path(&sticker.source)?;
    let is_animation = matches!(
        &sticker.source,
        StickerSource::File {
            media_type: MediaType::Animation,
            ..
        }
    );
    if is_animation {
        match media_animation::decode_animation(path) {
            Ok(anim) if anim.frames.len() >= 2 => {
                let frames: Vec<(Vec<u8>, Duration)> =
                    anim.frames.into_iter().map(|f| (f.rgba, f.delay)).collect();
                match device.create_texture_atlas(&frames, anim.width, anim.height) {
                    Ok(atlas) => {
                        let f0 = atlas.frames[0];
                        let sprite = Sprite::new(
                            atlas.texture.clone(),
                            sticker.placement.clone(),
                            sticker.transform,
                        )
                        .with_uv(f0.uv_offset, f0.uv_scale);
                        return Some((
                            sprite,
                            StickerAnimation::from_atlas(
                                atlas,
                                AnimationClock::new(Instant::now()),
                            ),
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "не удалось собрать атлас анимации — загружаю как статичное изображение");
                    }
                }
            }
            Err(
                media_animation::MediaError::TooManyFrames { .. }
                | media_animation::MediaError::TooLargeForAtlas { .. },
            ) => {
                if let Some((texture, source, first_delay)) = open_streaming_animation(device, path)
                {
                    let sprite = Sprite::new(
                        texture.clone(),
                        sticker.placement.clone(),
                        sticker.transform,
                    );
                    return Some((
                        sprite,
                        StickerAnimation::Streaming {
                            source,
                            texture,
                            deadline: Instant::now() + first_delay,
                        },
                    ));
                }
            }
            _ => {
                tracing::warn!(path = %path.display(), "анимация не декодировалась заново — загружаю как статичное изображение");
            }
        }
    }
    None
}

/// Открыть анимацию в потоковом режиме (ROADMAP.md M5a, «потоковый режим
/// для очень длинных анимаций» — `decode_animation` вернул `TooManyFrames`/
/// `TooLargeForAtlas`): декодирует только первый кадр и заводит для него
/// переиспользуемую GPU-текстуру; остальные кадры декодируются по одному на
/// каждый `AnimationTick` (см. `StickerAnimation::Streaming`). `None` — сам
/// потоковый декодер тоже не смог открыться/декодировать первый кадр
/// (битый файл) — вызывающий код в этом случае оставляет стикер без
/// спрайта, тем же принципом, что и остальные ошибки декода в этом файле.
fn open_streaming_animation(
    device: &Device,
    path: &Path,
) -> Option<(Texture, media_animation::StreamingAnimation, Duration)> {
    let mut source = match media_animation::StreamingAnimation::open(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "потоковая анимация: не удалось открыть");
            return None;
        }
    };
    let first = match source.next_frame() {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "потоковая анимация: не удалось декодировать первый кадр");
            return None;
        }
    };
    let texture = match device.create_streaming_animation_frame(
        &first.rgba,
        source.width(),
        source.height(),
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "потоковая анимация: не удалось создать текстуру кадра");
            return None;
        }
    };
    Some((texture, source, first.delay))
}

/// Пустые (чёрные) видеотекстуры нужного размера — до первого декодированного
/// кадра (декодер работает в своём потоке и не гарантирует кадр сразу же,
/// как `VideoSource::open*` вернулся, docs/M5B_VIDEO_DESIGN.md §6): спрайт
/// нужно чем-то залить уже сейчас, `VideoTick` заменит их первым же реальным
/// кадром. Нулевые Y/U/V после BT.709-конверсии в шейдере дают чёрный —
/// тот же приемлемый плейсхолдер, что и «чёрный фон» для альфа-видео (§0).
fn blank_video_textures(
    device: &Device,
    width: u32,
    height: u32,
) -> Result<VideoTextures, RenderError> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let y = vec![0u8; (width * height) as usize];
    let u = vec![0u8; (cw * ch) as usize];
    let v = vec![0u8; (cw * ch) as usize];
    device.create_video_textures(&y, &u, &v, width, height)
}

/// Открыть видеопоток стикера (M5b, docs/M5B_VIDEO_DESIGN.md §6): декодер
/// `rst-video` + источник звука общего микшера процесса, если он есть
/// (`mixer: None` — устройство вывода не открылось при старте, видео всё
/// равно играет, просто без звука, см. доккомент `VideoPlayback::audio`).
/// Открытие видео с альфа-каналом никогда не отклоняется — альфа-плоскость
/// декодер просто не читает (пользовательское решение §0).
fn load_sticker_video(
    device: &Device,
    sticker: &Sticker,
    mixer: Option<&AudioMixer>,
) -> Option<(Sprite, VideoPlayback)> {
    let path = sticker_image_path(&sticker.source)?;
    let is_video = matches!(
        &sticker.source,
        StickerSource::File {
            media_type: MediaType::Video,
            ..
        }
    );
    if !is_video {
        return None;
    }
    let opened = match mixer {
        Some(m) => VideoSource::open_with_audio_target(path, m.sample_rate(), m.channels()),
        None => VideoSource::open(path),
    };
    let source = match opened {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось открыть видео");
            return None;
        }
    };
    let (width, height) = source.dimensions();
    let textures = match blank_video_textures(device, width, height) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось создать видеотекстуры");
            return None;
        }
    };
    let audio = mixer.map(|m| {
        let a = m.add_source(sticker.id);
        a.set_volume(sticker.playback.volume as f32);
        a
    });
    if sticker.playback.paused {
        source.pause();
    } else {
        source.play();
    }
    let sprite = Sprite::new(
        textures.y.clone(),
        sticker.placement.clone(),
        sticker.transform,
    )
    .with_video(textures);
    Some((sprite, VideoPlayback { source, audio }))
}

/// Загрузить статичную текстуру стикера (без анимации) — общий хвост между
/// [`load_sticker_sprite`] на неудаче/не-анимации и всеми точками, которым
/// сама анимация не нужна.
fn load_static_sprite(device: &Device, sticker: &Sticker) -> Option<Sprite> {
    let path = sticker_image_path(&sticker.source)?;
    match device.load_image(path) {
        Ok(texture) => Some(Sprite::new(
            texture,
            sticker.placement.clone(),
            sticker.transform,
        )),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось загрузить изображение стикера");
            None
        }
    }
}

/// Удалить с диска материализованный файл вставленного стикера
/// (`StickerSource::Pasted`, SPEC 2.5: «удаление стикера... за исключением
/// материализованных вставок из буфера — они удаляются вместе со стикером»).
/// No-op для `File`/`Window` — их источник удалять нельзя. Ошибка удаления —
/// не паника: файл мог быть уже убран руками, оставлять его в этом случае
/// не хуже, чем сейчас.
fn cleanup_pasted_file(cfg: &Config, id: Uuid) {
    let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) else {
        return;
    };
    let StickerSource::Pasted { path } = &sticker.source else {
        return;
    };
    // `ops::duplicate` клонирует `source` целиком — копия вставленного
    // стикера ссылается на тот же файл, что и оригинал (docs/M2_SLICE5_REVIEW.md,
    // пункт 3.1). Удалять файл можно только когда удаляется последний
    // стикер, который на него ссылается — иначе удаление одного из пары
    // ломает другого.
    let still_referenced = cfg
        .stickers
        .iter()
        .any(|s| s.id != id && matches!(&s.source, StickerSource::Pasted { path: p } if p == path));
    if still_referenced {
        return;
    }
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!(path = %path.display(), error = %e, "не удалось удалить файл вставленного стикера");
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_undo(
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_snapshot: &[WindowInfo],
    occluder_cache: &mut HashMap<MonitorId, Vec<OccluderSet>>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
) -> bool {
    let Some(prev) = edit.undo_stack.pop() else {
        return false;
    };
    // Жест/UI-драг не могли быть активны здесь (Ctrl+Z игнорируется, пока
    // `edit.gesture.is_some()` или `pointer_owner` — панель, см. `handle_key`),
    // но снимаем защитно — docs/M2_SLICE_REVIEW.md, пункт 5;
    // docs/M2_SLICE6_REVIEW.md, пункт 2.1 (иначе повисший снимок ползунка
    // коммитится позже относительно уже откаченного cfg).
    edit.gesture = None;
    edit.pending_snapshot = None;
    edit.ui_pending_snapshot = None;
    edit.pointer_owner = PointerOwner::None;
    edit.redo_stack.push(std::mem::replace(cfg, prev));
    resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
    edit.selection.prune(&cfg.stickers);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после отмены");
    }
    // Снимок — это ВЕСЬ `Config`: откат мог поменять правило видимости любого
    // стикера (в т.ч. только что изменённое панелью выбора окон), не только
    // выделение/позиции. Без пересчёта здесь `occluder_cache` остался бы на
    // правилах ОТКАЧЕННОГО состояния (та же находка ревью, что и в
    // `handle_window_picker_up`, — см. MEMORY/BUGS.md vault'а).
    *occluder_cache = refresh_occlusion(cfg, monitor_bounds, window_snapshot);
    true
}

#[allow(clippy::too_many_arguments)]
fn perform_redo(
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_snapshot: &[WindowInfo],
    occluder_cache: &mut HashMap<MonitorId, Vec<OccluderSet>>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
) -> bool {
    let Some(next) = edit.redo_stack.pop() else {
        return false;
    };
    edit.gesture = None;
    edit.pending_snapshot = None;
    edit.ui_pending_snapshot = None;
    edit.pointer_owner = PointerOwner::None;
    if edit.undo_stack.len() == UNDO_CAPACITY {
        edit.undo_stack.remove(0);
    }
    edit.undo_stack.push(std::mem::replace(cfg, next));
    resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
    edit.selection.prune(&cfg.stickers);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после повтора");
    }
    // См. комментарий в `perform_undo` — то же самое касается повтора.
    *occluder_cache = refresh_occlusion(cfg, monitor_bounds, window_snapshot);
    true
}

/// Начать удаление выделенного: если подтверждение не подавлено
/// (`ops::should_confirm_delete`) — открыть модал (`edit.confirm`), иначе
/// удалить сразу тем же путём, что и раньше (docs/M2_WIRING_PLAN.md, раздел
/// 7). Общая точка входа для `Delete` и (позже) кнопки тулбара — диалог не
/// должен зависеть от того, чем вызван. Возвращает `false` только если
/// выделение пусто (нечего удалять — не открывать модал впустую).
#[allow(clippy::too_many_arguments)]
fn begin_delete(
    edit: &mut EditState,
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    center: (f64, f64),
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_id: &MonitorId,
) -> bool {
    if edit.selection.is_empty() {
        return false;
    }
    if ops::should_confirm_delete(cfg) {
        edit.confirm = Some(ConfirmState {
            snapshot: cfg.clone(),
            ids: edit.selection.ids().to_vec(),
            monitor_id: monitor_id.clone(),
            panel: confirm_dialog::build(edit.selection.ids().len() as u32, center),
        });
    } else {
        commit_undo_snapshot(edit, cfg.clone());
        for id in edit.selection.ids().to_vec() {
            cleanup_pasted_file(cfg, id);
            let _ = ops::delete(cfg, id);
        }
        edit.selection.prune(&cfg.stickers);
        resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после удаления");
        }
        rebuild_ui_panels(edit, cfg, monitor_geometry);
    }
    true
}

/// Клавиатурные команды режима редактирования (docs/M2_INTEGRATION_PLAN.md,
/// раздел 12): `Esc` — выход, `Ctrl+Z`/`Ctrl+Shift+Z`/`Ctrl+Y` — отмена/повтор,
/// `Ctrl+A` — выделить всё, `Delete` — удалить выделенное (без диалога
/// подтверждения — см. заметку в шапке файла), `Ctrl+D` — дублировать.
#[allow(clippy::too_many_arguments)]
fn handle_key(
    vk: u32,
    modifiers: Modifiers,
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    overlay_size: (u32, u32),
    scale: f32,
    monitor_id: &MonitorId,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_snapshot: &[WindowInfo],
    occluder_cache: &mut HashMap<MonitorId, Vec<OccluderSet>>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    window_pins: &mut WindowPins,
) -> bool {
    // История/удаление/дублирование во время активного жеста (сцены или
    // панели — ползунок прозрачности тоже держит указатель) мутировали бы
    // cfg из-под него — жест продолжал бы считать от своего старого
    // состояния поверх уже изменённого (docs/M2_SLICE_REVIEW.md, пункт 2;
    // docs/M2_SLICE6_REVIEW.md, пункт 2.4 — тот же класс бага для панели).
    // Esc — исключение: toggle_edit_mode сам корректно откатывает
    // незавершённый жест/драг перед выходом.
    let pointer_busy = matches!(
        edit.pointer_owner,
        PointerOwner::Toolbar
            | PointerOwner::CursorPanel
            | PointerOwner::WindowPicker
            | PointerOwner::PinnedPanel
    );
    if (edit.gesture.is_some() || pointer_busy) && vk != VK_ESCAPE {
        return false;
    }
    // Модал подтверждения блокирует всё, кроме своей отмены по Esc — как и
    // жест выше, но отдельной веткой: гостевой жест здесь всегда `None`
    // (модал открывается только когда жеста нет), поэтому порядок с
    // предыдущей проверкой не важен (docs/M2_WIRING_PLAN.md, раздел 10).
    if edit.confirm.is_some() {
        return if vk == VK_ESCAPE {
            edit.confirm = None;
            true
        } else {
            false
        };
    }
    // Панель быстрого переключения пресетов (M7) модальна для клавиатуры,
    // как и модал подтверждения выше, — тем же паттерном (единственный
    // выход — Esc, ничего не применяя).
    if edit.preset_picker.is_some() {
        return if vk == VK_ESCAPE {
            edit.preset_picker = None;
            true
        } else {
            false
        };
    }
    // Список окон для закрепления (M6, SPEC.md §5.1) блокирует клавиатуру
    // целиком, кроме `Esc` — тот же паттерн, что `preset_picker` выше
    // (список действий, а не редактор конкретного стикера).
    if edit.window_pick_list.is_some() {
        return if vk == VK_ESCAPE {
            edit.window_pick_list = None;
            true
        } else {
            false
        };
    }
    // Панель выбора окон не модальна для мыши (докс §7.7 — клик мимо неё
    // уходит в сцену), но клавиатуру блокирует, кроме `Esc`: панель правит
    // ОДИН конкретный стикер, и хоткеи вроде `Delete`/`Ctrl+D`, сработавшие
    // по текущему выделению параллельно с открытой панелью, были бы
    // путающими (тот же стикер мог бы исчезнуть/задублироваться прямо
    // из-под панели, которая его редактирует).
    if edit.window_picker.is_some() {
        return if vk == VK_ESCAPE {
            edit.window_picker = None;
            true
        } else {
            false
        };
    }
    // Панель свойств закреплённого окна (SPEC «Закрепление окна», пункт 9) —
    // тот же паттерн, что window_picker выше (модальна для клавиатуры), но
    // сама несёт текстовые поля правил соседства: ключ сперва пробуем в
    // саму панель (наберётся, если в фокусе поле), и только если она его не
    // взяла — `Esc` закрывает панель и снимает выделение окна.
    if edit.pinned_panel.is_some() {
        if let Some(key) = widget_key(vk, modifiers) {
            let consumed = edit
                .pinned_panel
                .as_mut()
                .map(|s| s.panel.key_event(key).consumed)
                .unwrap_or(false);
            if consumed {
                // Коммитим сразу по `Enter`, тем же паттерном, что
                // `NumericField` тулбара ниже — иначе значение висело бы до
                // случайного клика где-то ещё.
                if let Some(hwnd) = edit.pinned_panel.as_ref().map(|s| s.hwnd) {
                    let rule_count = edit
                        .pinned_windows
                        .iter()
                        .find(|p| p.hwnd == hwnd)
                        .map_or(0, |p| p.host_rules.len());
                    if sync_pinned_rule_text_fields(edit, hwnd, rule_count) {
                        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
                    }
                }
                return true;
            }
        }
        return if vk == VK_ESCAPE {
            edit.pinned_panel = None;
            edit.pinned_selection = None;
            true
        } else {
            false
        };
    }
    // Числовое поле тулбара: пока оно в фокусе, клавиатура — его, а не
    // хоткеи ядра ниже (в т.ч. `Enter`/`Esc`, которые иначе перехватил бы
    // `match`). Раньше `Panel::key_event` не вызывался вообще, поле было
    // полностью глухо к клавиатуре (docs/M2_SLICE6_REVIEW.md, пункт 2.3).
    // Без фокуса `key_event` возвращает `consumed: false` и здесь ничего не
    // происходит — обычные хоткеи идут дальше как раньше.
    if let Some(key) = widget_key(vk, modifiers) {
        let result = edit
            .toolbar
            .as_mut()
            .map(|p| p.key_event(key))
            .unwrap_or_default();
        if result.consumed {
            // Коммитим сразу по `Enter`, а не откладываем до следующего
            // `MouseUp` на тулбаре — иначе значение зависло бы до
            // случайного клика где-то ещё и закоммитилось бы не в том
            // месте истории (docs/M2_SLICE6_REVIEW.md, пункт 1.2).
            if let Some(value) = edit
                .toolbar
                .as_mut()
                .and_then(|p| p.widget_mut::<NumericField>(toolbar::TB_FIELD))
                .and_then(NumericField::take_submitted)
            {
                if let Some(id) = single_selected_id(&edit.selection) {
                    commit_undo_snapshot(edit, cfg.clone());
                    apply_opacity(cfg, sprites, id, value);
                    if let Some(slider) = edit
                        .toolbar
                        .as_mut()
                        .and_then(|p| p.widget_mut::<Slider>(toolbar::TB_SLIDER))
                    {
                        slider.set_value(value);
                    }
                    if let Err(e) = config::save(cfg, config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после изменения прозрачности");
                    }
                }
            }
            return result.redraw;
        }
    }
    match vk {
        VK_ESCAPE => {
            toggle_edit_mode(
                overlay,
                edit,
                cfg,
                sprites,
                animations,
                videos,
                audio_mixer,
                renderer,
                config_path,
                monitor_geometry,
                monitor_bounds,
                window_pins,
                window_snapshot,
            );
            true
        }
        VK_Z if modifiers.ctrl && modifiers.shift => {
            let did = perform_redo(
                renderer,
                cfg,
                config_path,
                sprites,
                edit,
                monitor_bounds,
                window_snapshot,
                occluder_cache,
                animations,
                videos,
                audio_mixer,
            );
            rebuild_ui_panels(edit, cfg, monitor_geometry);
            did
        }
        VK_Z if modifiers.ctrl => {
            let did = perform_undo(
                renderer,
                cfg,
                config_path,
                sprites,
                edit,
                monitor_bounds,
                window_snapshot,
                occluder_cache,
                animations,
                videos,
                audio_mixer,
            );
            rebuild_ui_panels(edit, cfg, monitor_geometry);
            did
        }
        VK_Y if modifiers.ctrl => {
            let did = perform_redo(
                renderer,
                cfg,
                config_path,
                sprites,
                edit,
                monitor_bounds,
                window_snapshot,
                occluder_cache,
                animations,
                videos,
                audio_mixer,
            );
            rebuild_ui_panels(edit, cfg, monitor_geometry);
            did
        }
        VK_A if modifiers.ctrl => {
            edit.selection.select_all(&cfg.stickers);
            rebuild_ui_panels(edit, cfg, monitor_geometry);
            true
        }
        VK_DELETE => {
            let center = selection_center_or_screen(edit, cfg, overlay_size, scale, monitor_id);
            begin_delete(
                edit,
                renderer,
                cfg,
                config_path,
                sprites,
                animations,
                videos,
                audio_mixer,
                center,
                monitor_geometry,
                monitor_id,
            )
        }
        VK_D if modifiers.ctrl => {
            if edit.selection.is_empty() {
                return false;
            }
            commit_undo_snapshot(edit, cfg.clone());
            let mut new_ids = Vec::new();
            for id in edit.selection.ids().to_vec() {
                if let Ok(new_id) = ops::duplicate(cfg, id) {
                    new_ids.push(new_id);
                }
            }
            resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
            edit.selection.clear();
            for id in new_ids {
                edit.selection.select(id);
            }
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после дублирования");
            }
            rebuild_ui_panels(edit, cfg, monitor_geometry);
            true
        }
        VK_V if modifiers.ctrl => paste_from_clipboard(
            overlay,
            renderer,
            cfg,
            config_path,
            sprites,
            edit,
            scale,
            monitor_id,
            audio_mixer,
        ),
        _ => false,
    }
}

/// Перевод кода `WM_KEYDOWN` в клавишу для виджетов (числовое поле тулбара).
/// `Ctrl`-комбинации сюда не доходят: `Ctrl+что-угодно` — хоткей ядра, а не
/// ввод в поле (M2_UI_NOTES §9), поэтому при `modifiers.ctrl` всегда `None`.
fn widget_key(vk: u32, modifiers: Modifiers) -> Option<Key> {
    if modifiers.ctrl {
        return None;
    }
    match vk {
        0x30..=0x39 => Some(Key::Digit((vk - 0x30) as u8)),
        VK_BACK => Some(Key::Backspace),
        VK_RETURN => Some(Key::Enter),
        VK_ESCAPE => Some(Key::Escape),
        VK_LEFT => Some(Key::ArrowLeft),
        VK_RIGHT => Some(Key::ArrowRight),
        _ => None,
    }
}

/// `Ctrl+V`: вставить изображение из буфера обмена как новый стикер
/// (docs/M2_WIRING_PLAN.md, раздел 9Б). Вариант «числовое поле в фокусе»
/// (раздел 9А, `Widget::paste`) сюда не попадает — `Ctrl`-комбинации не
/// доходят до виджетов (`widget_key` возвращает `None`), и это отдельный,
/// пока не подключённый срез. Отсутствие изображения в буфере — не ошибка,
/// просто `false` (нет перерисовки).
#[allow(clippy::too_many_arguments)]
fn paste_from_clipboard(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    scale: f32,
    monitor_id: &MonitorId,
    audio_mixer: Option<&AudioMixer>,
) -> bool {
    let image = match clipboard::read_image() {
        Ok(Some(image)) => image,
        Ok(None) => return false,
        Err(e) => {
            tracing::warn!(error = %e, "не удалось прочитать буфер обмена");
            return false;
        }
    };
    match image {
        ClipboardImage::Files(paths) => {
            let supported: Vec<_> = paths
                .into_iter()
                .filter(|p| clipboard::is_supported_image(p))
                .collect();
            if supported.is_empty() {
                return false;
            }
            // Один снимок на всю вставку — Ctrl+Z снимает её целиком, даже
            // если файлов несколько (докс раздел 9Б). Снимок берётся заранее,
            // но коммитится только если хоть один файл реально добавился —
            // иначе (все не декодировались) история получила бы пустой шаг
            // (docs/M2_SLICE4_REVIEW.md, пункт 7).
            let before = cfg.clone();
            let mut added = false;
            for path in supported {
                if add_sticker(
                    overlay,
                    renderer,
                    cfg,
                    config_path,
                    sprites,
                    edit,
                    path,
                    false,
                    scale,
                    monitor_id,
                    audio_mixer,
                ) {
                    added = true;
                }
            }
            if added {
                commit_undo_snapshot(edit, before);
            }
            added
        }
        png_or_bmp @ (ClipboardImage::Png(_) | ClipboardImage::Bmp(_)) => {
            let Some(target_dir) = config_path.parent() else {
                return false;
            };
            match paste::materialize(&png_or_bmp, target_dir) {
                Ok(path) => {
                    let before = cfg.clone();
                    if add_sticker(
                        overlay,
                        renderer,
                        cfg,
                        config_path,
                        sprites,
                        edit,
                        path.clone(),
                        true,
                        scale,
                        monitor_id,
                        audio_mixer,
                    ) {
                        commit_undo_snapshot(edit, before);
                        true
                    } else {
                        // add_sticker не смог декодировать только что
                        // материализованный файл — без стикера в конфиге
                        // чистить его больше некому (docs/M2_SLICE5_REVIEW.md,
                        // пункт 3.3): убрать сироту сразу.
                        if let Err(e) = std::fs::remove_file(&path) {
                            tracing::warn!(path = %path.display(), error = %e, "не удалось удалить файл после неудачной вставки");
                        }
                        false
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "не удалось материализовать вставленное изображение");
                    false
                }
            }
        }
    }
}

fn to_dip(pos: rst_win32::input::Point, scale: f32) -> (f64, f64) {
    (pos.x as f64 / scale as f64, pos.y as f64 / scale as f64)
}

/// Верхний (по `order`) видимый стикер под точкой `(dip_x, dip_y)`, если есть.
/// Верхний (по `order`) стикер под точкой `(dip_x, dip_y)`, если есть —
/// вызывается только в режиме редактирования, где скрытые стикеры рисуются
/// шахматкой и **полностью интерактивны** (SPEC.md 3.7: «можно двигать,
/// скейлить и вернуть кнопкой «глаз»»), поэтому `visible` здесь не
/// фильтруется — в отличие от `redraw`, где скрытые вне режима редактирования
/// не рисуются вообще.
fn hit_sticker_at(cfg: &Config, monitor_id: &MonitorId, dip_x: f64, dip_y: f64) -> Option<Uuid> {
    cfg.stickers
        .iter()
        .filter(|s| s.placement.monitor_id == *monitor_id)
        .filter(|s| hittest::contains(&s.placement, &s.transform, dip_x, dip_y))
        .max_by_key(|s| s.order)
        .map(|s| s.id)
}

fn point_in_box2d(r: &Box2D, x: f64, y: f64) -> bool {
    // Ручки ресайза не повёрнуты вместе с рамкой (selection.rs::handle_rects),
    // поэтому простое осевое сравнение корректно.
    (x - r.cx).abs() <= r.w / 2.0 && (y - r.cy).abs() <= r.h / 2.0
}

/// Направление от центра стикера к углу `corner` в ЛОКАЛЬНЫХ (неповёрнутых)
/// осях, экранная конвенция (по часовой = плюс, см. `transform_ops::rotate`
/// и доккомент `create_rotate_cursor` в rst-win32): чистая геометрия угла
/// осеориентированного прямоугольника, не подобранная константа — NW/NE/
/// SE/SW лежат на пересечении диагоналей ровно в этих направлениях
/// независимо от размера прямоугольника.
fn corner_local_angle_deg(corner: CoreCorner) -> f64 {
    match corner {
        CoreCorner::NorthWest => -135.0,
        CoreCorner::NorthEast => -45.0,
        CoreCorner::SouthEast => 45.0,
        CoreCorner::SouthWest => 135.0,
    }
}

/// Угол курсора поворота (градусы, целые — кэшируется по этому значению в
/// `rst-win32`): направление к БЛИЖАЙШЕМУ (по расстоянию до ручки) углу
/// рамки, повёрнутое вместе со стикером (фидбэк пользователя 2026-08-09,
/// третий раунд — «бери положение угла картинки в пространстве», а не
/// фиксированную константу на 4 угла: если стикер уже повёрнут, его углы
/// физически не там, где были бы у неповёрнутого прямоугольника). Изгиб
/// дуги курсора при этом смотрит туда же, куда «выпирает» реальный угол.
fn nearest_corner_cursor_angle_deg(sbox: &SelectionBox, rotation: f64, dip_x: f64, dip_y: f64) -> i32 {
    let nearest = CoreCorner::ALL
        .into_iter()
        .min_by(|a, b| {
            let da = {
                let (hx, hy) = sbox.handle_center(a.handle());
                (dip_x - hx).hypot(dip_y - hy)
            };
            let db = {
                let (hx, hy) = sbox.handle_center(b.handle());
                (dip_x - hx).hypot(dip_y - hy)
            };
            da.total_cmp(&db)
        })
        .expect("CoreCorner::ALL непусто");
    (corner_local_angle_deg(nearest) + rotation.to_degrees()).round() as i32
}

/// Разрешить зону под курсором (docs/M2_INTEGRATION_PLAN.md, раздел 7):
/// порядок проверки — обратный порядку отрисовки. Ручки — на самих себе;
/// поворот — СНАРУЖИ рамки на расстоянии от ближайшего угла не меньше
/// `ROTATE_MIN_CORNER_GAP_DIP`, без верхнего предела (весь внешний
/// периметр, как в Photoshop — фидбэк пользователя 2026-08-09, третий
/// раунд: старое кольцо вокруг угла срабатывало и ВНУТРИ рамки, и было
/// ограничено дистанцией). Ручки/поворот доступны только для одиночного
/// выделения (мультивыделение — следующий срез).
fn resolve_zone(
    cfg: &Config,
    selection: &SelectionSet,
    monitor_id: &MonitorId,
    dip_x: f64,
    dip_y: f64,
) -> Zone {
    if let [id] = selection.ids() {
        if let Some(sticker) = cfg
            .stickers
            .iter()
            .find(|s| s.id == *id && s.placement.monitor_id == *monitor_id)
        {
            let sbox = SelectionBox::new(&sticker.placement, &sticker.transform);
            // Ручки ресайза — высший приоритет, безусловно (и внутри, и
            // снаружи рамки — квадрат ручки обычно на самой границе).
            for (kind, rect) in sbox.handle_rects(rst_render::HANDLE_SIZE_DIP) {
                if point_in_box2d(&rect, dip_x, dip_y) {
                    return Zone::ResizeHandle(*id, kind);
                }
            }
            let inside = hittest::contains(&sticker.placement, &sticker.transform, dip_x, dip_y);
            if inside {
                return Zone::StickerBody(*id);
            }
            // Клик снаружи рамки выделенного стикера может лежать на ДРУГОМ
            // стикере — переключение выделения важнее зоны поворота: она
            // безгранична наружу (см. ниже), поэтому без этой проверки ни
            // один другой стикер на экране вообще не кликабелен, пока
            // текущий выделен (баг из репорта пользователя: «выбрал стикер —
            // не могу выбрать другой», 2026-08-10).
            if let Some(other_id) = hit_sticker_at(cfg, monitor_id, dip_x, dip_y) {
                if other_id != *id {
                    return Zone::StickerBody(other_id);
                }
            }
            let nearest_corner_dist = CoreCorner::ALL
                .into_iter()
                .map(|corner| {
                    let (hx, hy) = sbox.handle_center(corner.handle());
                    (dip_x - hx).hypot(dip_y - hy)
                })
                .fold(f64::INFINITY, f64::min);
            if nearest_corner_dist >= ROTATE_MIN_CORNER_GAP_DIP {
                let angle =
                    nearest_corner_cursor_angle_deg(&sbox, sticker.transform.rotation, dip_x, dip_y);
                return Zone::Rotate(*id, angle);
            }
            // Снаружи, но ближе ROTATE_MIN_CORNER_GAP_DIP к углу и не на
            // самой ручке — узкий буфер вокруг ручки, ни ресайз, ни поворот
            // (та же логика, что «дырка» между зонами в Photoshop).
            return Zone::Background;
        }
    }
    match hit_sticker_at(cfg, monitor_id, dip_x, dip_y) {
        Some(id) => Zone::StickerBody(id),
        None => Zone::Background,
    }
}

/// DIP-`Placement` закреплённого окна `hwnd` на мониторе, где оно сейчас
/// физически находится (SPEC «Закрепление окна», пункт 9: хит-тест/ручки
/// ресайза той же геометрией, что у стикеров, [`SelectionBox`]/
/// [`hittest::contains`]) — `None`, если окно исчезло из снимка, свёрнуто
/// (rect мусорный) или лежит вне известных мониторов; тот же принцип
/// «недостающий элемент — не паника», что у [`pin_window`]. Считается заново
/// на каждый кадр/клик — `PinnedWindow` намеренно не хранит геометрию.
///
/// ГЕОМЕТРИЯ БЕРЁТСЯ ЖИВОЙ ([`rst_win32::window_enum::live_rect`]), а снимок
/// трекера остаётся только фильтром «окно существует и не свёрнуто». Причина
/// (репорт 2026-08-21): снимок дебаунсится на 16 мс и приходит после полного
/// перечисления, поэтому во время перетаскивания окна рамка-пульс, бейдж
/// замка и панель инструментов рисовались по устаревшему прямоугольнику и
/// заметно «отлетали» от окна. Если DWM почему-то не отдал границы —
/// откатываемся к снимку, как было.
fn pinned_window_dip_placement(
    hwnd: isize,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) -> Option<(MonitorId, Placement)> {
    let win = window_snapshot
        .iter()
        .find(|w| w.hwnd == hwnd as usize && !w.iconic)?;
    let rect = rst_win32::window_enum::live_rect(hwnd as usize).unwrap_or(win.rect);
    let monitor_id = monitor_for_window_rect(&rect, monitor_bounds)?.clone();
    let bounds = monitor_bounds.get(&monitor_id)?;
    let placement = window_rect_to_placement(&rect, monitor_id.clone(), bounds);
    Some((monitor_id, placement))
}

/// Закреплённое окно под курсором на мониторе `monitor_id` (SPEC
/// «Закрепление окна», пункт 9) — от последнего добавленного к первому:
/// свежепин обычно визуально сверху, но явный z-order между несколькими
/// закреплениями друг относительно друга не моделируется — накладки редки.
fn hit_pinned_window_at(
    edit: &EditState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    monitor_id: &MonitorId,
    dip_x: f64,
    dip_y: f64,
) -> Option<isize> {
    edit.pinned_windows.iter().rev().find_map(|pinned| {
        let (win_monitor, placement) =
            pinned_window_dip_placement(pinned.hwnd, window_snapshot, monitor_bounds)?;
        if win_monitor != *monitor_id {
            return None;
        }
        hittest::contains(&placement, &Transform::default(), dip_x, dip_y).then_some(pinned.hwnd)
    })
}

/// Ручка ресайза выделенного закреплённого окна под курсором — та же
/// `SelectionBox`, что резолвит ручки стикеров ([`resolve_zone`]), только
/// геометрия из [`pinned_window_dip_placement`] вместо `Sticker::placement`.
/// `None`, если ничего не выделено, окно пропало из снимка/чужого монитора,
/// или курсор не на ручке.
fn resolve_pinned_resize_handle(
    edit: &EditState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    monitor_id: &MonitorId,
    dip_x: f64,
    dip_y: f64,
) -> Option<HandleKind> {
    let hwnd = edit.pinned_selection?;
    let (win_monitor, placement) =
        pinned_window_dip_placement(hwnd, window_snapshot, monitor_bounds)?;
    if win_monitor != *monitor_id {
        return None;
    }
    let sbox = SelectionBox::new(&placement, &Transform::default());
    for (kind, rect) in sbox.handle_rects(rst_render::HANDLE_SIZE_DIP) {
        if point_in_box2d(&rect, dip_x, dip_y) {
            return Some(kind);
        }
    }
    None
}

fn to_win32_handle(kind: HandleKind) -> Win32Handle {
    match kind {
        HandleKind::North => Win32Handle::North,
        HandleKind::NorthEast => Win32Handle::NorthEast,
        HandleKind::East => Win32Handle::East,
        HandleKind::SouthEast => Win32Handle::SouthEast,
        HandleKind::South => Win32Handle::South,
        HandleKind::SouthWest => Win32Handle::SouthWest,
        HandleKind::West => Win32Handle::West,
        HandleKind::NorthWest => Win32Handle::NorthWest,
    }
}

fn cursor_shape_for_zone(zone: &Zone) -> CursorShape {
    match zone {
        Zone::Background => CursorZone::Background.cursor_shape(),
        Zone::StickerBody(_) => CursorZone::StickerBody.cursor_shape(),
        Zone::ResizeHandle(_, kind) => {
            CursorZone::ResizeHandle(to_win32_handle(*kind)).cursor_shape()
        }
        Zone::Rotate(_, angle_deg) => CursorZone::RotateZone(*angle_deg).cursor_shape(),
    }
}

/// Текст тултипа кнопки тулбара выделения (фидбэк пользователя 2026-08-10) —
/// `None` для виджетов без подсказки (слайдер прозрачности, числовое поле —
/// не кнопки).
fn toolbar_tooltip_text(id: WidgetId) -> Option<&'static str> {
    match id {
        toolbar::TB_LAYERS => Some("Слои видимости"),
        toolbar::TB_EYE => Some("Показать/скрыть"),
        toolbar::TB_ORDER_UP => Some("Переместить выше"),
        toolbar::TB_ORDER_DOWN => Some("Переместить ниже"),
        toolbar::TB_DUPLICATE => Some("Дублировать"),
        toolbar::TB_RESET_SCALE => Some("Сбросить масштаб"),
        toolbar::TB_DELETE => Some("Удалить"),
        toolbar::TB_PLAY_PAUSE => Some("Играть/пауза"),
        _ => None,
    }
}

/// Текст тултипа кнопки панели у курсора (фидбэк пользователя 2026-08-10).
fn cursor_panel_tooltip_text(id: WidgetId) -> Option<&'static str> {
    match id {
        cursor_panel::BTN_LOAD_FILE => Some("Загрузить файл"),
        cursor_panel::BTN_ADD_WINDOW => Some("Закрепить окно"),
        cursor_panel::BTN_PRESETS => Some("Пресеты"),
        cursor_panel::BTN_TOGGLE_ALL => Some("Показать/скрыть все стикеры"),
        cursor_panel::BTN_SETTINGS => Some("Настройки"),
        cursor_panel::BTN_EXIT => Some("Выйти из режима редактирования"),
        _ => None,
    }
}

/// Текст тултипа панели свойств закреплённого окна (SPEC «Закрепление окна»,
/// пункт 9; фидбэк пользователя 2026-08-17 — «панель выглядит плохо, кнопки
/// без подсказок») — переключатели замков, «Открепить», «Добавить правило» и
/// элементы строк правил соседства ([`rst_render::decode_pinned_row_id`];
/// поля разъясняют, что за процесс/маску вводить — та же роль, что у
/// плейсхолдеров в `build_pinned_panel`).
fn pinned_panel_tooltip_text(id: WidgetId) -> Option<&'static str> {
    match id {
        rst_render::PINNED_CHECK_MOVE_LOCK => Some("Заблокировать перемещение окна"),
        rst_render::PINNED_CHECK_INTERACT_LOCK => Some("Заблокировать ввод в окно"),
        rst_render::PINNED_BTN_UNPIN => Some("Открепить окно"),
        rst_render::PINNED_BTN_ADD_RULE => Some("Добавить правило соседства"),
        _ => match rst_render::decode_pinned_row_id(id) {
            Some((_, PinnedRowField::Remove)) => Some("Удалить правило"),
            Some((_, PinnedRowField::ProcessName)) => Some("Имя процесса окна-соседа"),
            Some((_, PinnedRowField::TitlePattern)) => Some("Маска заголовка окна-соседа"),
            None => None,
        },
    }
}

/// Примитивы тултипа: фон (та же двухслойная заливка, что у `Panel::draw` —
/// PANEL_BORDER снаружи, PANEL_BG внутри, инсет 2 DIP) + текст, под кнопкой
/// (либо над ней, если снизу не хватает места — тот же приём, что
/// `toolbar_top`). `opacity` — множитель анимации появления (фидбэк
/// пользователя 2026-08-10); `[]`, если `opacity <= 0` (задержка ещё не
/// истекла) — не тратим текстуры на невидимое.
fn tooltip_primitives(tooltip: &TooltipState, screen_h: f64, opacity: f64) -> Vec<Primitive> {
    if opacity <= 0.0 {
        return Vec::new();
    }
    let (text_w, text_h) = rst_render::text_size(tooltip.text);
    let box_w = text_w + 2.0 * TOOLTIP_PAD_DIP;
    let box_h = text_h + 2.0 * TOOLTIP_PAD_DIP;
    let below_top = tooltip.anchor.cy + tooltip.anchor.h / 2.0 + TOOLTIP_GAP_DIP;
    let top = if below_top + box_h <= screen_h {
        below_top
    } else {
        tooltip.anchor.cy - tooltip.anchor.h / 2.0 - TOOLTIP_GAP_DIP - box_h
    };
    let cx = tooltip.anchor.cx;
    let cy = top + box_h / 2.0;
    let frame = Box2D {
        cx,
        cy,
        w: box_w,
        h: box_h,
        rotation: 0.0,
    };
    vec![
        Primitive::Fill {
            rect: frame,
            color: theme::PANEL_BORDER,
            opacity: theme::PANEL_BG_OPACITY * opacity,
        },
        Primitive::Fill {
            rect: Box2D {
                w: (frame.w - 2.0).max(0.0),
                h: (frame.h - 2.0).max(0.0),
                ..frame
            },
            color: theme::PANEL_BG,
            opacity: theme::PANEL_BG_OPACITY * opacity,
        },
        Primitive::Text {
            rect: Box2D {
                cx,
                cy,
                w: text_w,
                h: text_h,
                rotation: 0.0,
            },
            text: tooltip.text.to_string(),
            color: theme::TEXT,
            opacity,
        },
    ]
}

/// Записать `placement`/`transform` и в модель (`cfg.stickers`), и в спрайт
/// того же id — обе копии обязаны совпадать (модель — источник истины и то,
/// что сохраняется, спрайт — то, что рисуется).
fn apply_transform(
    cfg: &mut Config,
    sprites: &mut [(Uuid, Sprite)],
    id: Uuid,
    placement: Placement,
    transform: Transform,
) {
    if let Some(sticker) = cfg.stickers.iter_mut().find(|s| s.id == id) {
        sticker.placement = placement.clone();
        sticker.transform = transform;
    }
    if let Some((_, sprite)) = sprites.iter_mut().find(|(sid, _)| *sid == id) {
        sprite.placement = placement;
        sprite.transform = transform;
    }
}

/// Id единственного выделенного стикера — тулбар в этом срезе показывается
/// только для одиночного выделения (docs/M2_WIRING_PLAN.md, раздел 4;
/// мультивыделение — `docs/M2_MULTISELECT_TOOLBAR_NOTES.md`, следующий срез).
fn single_selected_id(selection: &SelectionSet) -> Option<Uuid> {
    match selection.ids() {
        [id] => Some(*id),
        _ => None,
    }
}

/// Монитор, на котором «живёт» тулбар — монитор единственного выделенного
/// стикера (тулбара нет, если выделение не одиночное). Общая точка для двух
/// решений, которые обязаны совпадать (M3, docs/M3_STEP4_REVIEW.md, пункт
/// 2.1): рисовать ли тулбар в кадре этого монитора (`redraw`) и адресован ли
/// клик тулбару (`handle_input`) — иначе клик с чужого монитора по локальным
/// DIP-координатам, совпавшим с тулбаром, «поглощался» бы невидимой там
/// панелью (вплоть до срабатывания её кнопок на чужом стикере).
fn toolbar_monitor<'a>(selection: &SelectionSet, cfg: &'a Config) -> Option<&'a MonitorId> {
    single_selected_id(selection)
        .and_then(|id| cfg.stickers.iter().find(|s| s.id == id))
        .map(|s| &s.placement.monitor_id)
}

/// Перечислить мониторы заново и, если получилось, отправить результат в
/// общий канал тем же событием, что и настоящий `WM_DISPLAYCHANGE` —
/// страховка на случай смены топологии/primary во время блокировки сессии
/// или сна без отдельного `WM_DISPLAYCHANGE` (допущение
/// M3_HOTPLUG_DESIGN.md §7: «Windows всегда шлёт его при смене раскладки
/// виртуального десктопа» — если на практике найдётся исключение, эта
/// страховка перечислит мониторы сама на `SessionUnlocked`/`SystemResumed`).
/// Идентификатор монитора в отправленном `OverlayMessage::Event` не имеет
/// значения — ветка `MonitorsChanged` его игнорирует (`_`), поэтому годится
/// текущий `primary_id`. Отправка, а не прямой вызов обработчика: событие
/// уйдёт в очередь и обработается на следующей итерации цикла `run()`, как и
/// любое другое сообщение (без реентрантности/рекурсии в текущий кадр match).
///
/// Каждое окно регистрируется на сессионные/энергоуведомления по отдельности,
/// поэтому на N мониторов это вызывается N раз подряд на одно системное
/// событие (N перечислений + N диффов + N полных `redraw_all`) — идемпотентно
/// (диффы против живого состояния схлопываются в no-op со второго раза), но
/// лишняя работа; при обычном числе мониторов (2-4) — миллисекунды, не
/// оптимизировано намеренно (M3_SESSION_SLEEP_REVIEW.md, пункт 2.1).
fn reenumerate_monitors(tx: &Sender<OverlayMessage>, primary_id: &MonitorId, reason: &str) {
    match monitors::enumerate() {
        Ok(infos) => {
            let _ = tx.send(OverlayMessage::Event(
                primary_id.clone(),
                OverlayEvent::MonitorsChanged(infos),
            ));
        }
        Err(e) => {
            tracing::warn!(error = %e, reason, "не удалось переперечислить мониторы");
        }
    }
}

/// Нужна ли трекеру окон живая маска перекрытия (ADR-005, M4_PREP_NOTES
/// §6.4): `false`, только если у ВСЕХ стикеров `VisibilityMode::Always` —
/// тогда окклюдеры в принципе не на что накладывать, и хуки не ставятся
/// вовсе (fast path). Пустой список стикеров — `false` (`any` на пустом
/// итераторе), тот же смысл: маска не на что переключать.
fn mask_needed(cfg: &Config) -> bool {
    cfg.stickers
        .iter()
        .any(|s| s.visibility.mode != VisibilityMode::Always)
}

/// Гейт хуков трекера окон (ADR-005, M4_PREP_NOTES §6.4): помимо
/// `mask_needed(cfg)` (окклюдеры) трекер обязан жить, пока открыт любой UI,
/// которому нужен живой `window_snapshot`, даже если ни у одного стикера нет
/// правила видимости, зависящего от окклюдеров, — иначе на чистом конфиге
/// (только что установленный resticker, все стикеры `Always` или их вообще
/// нет) трекер спит, и такой UI получает пустой/протухший снимок.
///
/// `window_picker` (M4, панель «какие окна видимы» у стикера) — известный
/// прежний случай. `window_pick_list` (M6, список окон для закрепления у
/// кнопки `BTN_ADD_WINDOW`) — тот же паттерн: без него список не
/// наполнялся бы НИКОГДА на чистом конфиге — трекер не просыпался,
/// снимок оставался пустым (найдено по репорту пользователя «кнопка
/// прикрепления окна вообще не работает», 2026-08-10).
///
/// `!edit.pinned_windows.is_empty()` (редизайн пинов) — тем же поводом:
/// пока хотя бы одно окно закреплено, `maintain_pinned_windows` каждый тик
/// нуждается в живом `window_snapshot` (снос уничтоженных таргетов,
/// z-order-слоты по правилам соседства, move-lock snap-back — доккомент
/// `maintain_pinned_windows`). Без этого условия трекер засыпал сразу же,
/// как закрывалась панель/список, которым он был разбужен для самого
/// пина, — уже закреплённое окно переставало обслуживаться тем же тиком
/// (живой репорт пользователя, 2026-08-17: хоткей `Ctrl+Alt+R` пина/отпина
/// вёл себя нестабильно на чистом конфиге — сам хоткей теперь берёт
/// собственное разовое перечисление ([`toggle_focused_pin`]), но
/// `window_snapshot` координатора всё равно обязан оставаться живым, пока
/// есть что обслуживать).
fn tracker_mask_needed(cfg: &Config, edit: &EditState) -> bool {
    mask_needed(cfg)
        || edit.window_picker.is_some()
        || edit.window_pick_list.is_some()
        || !edit.pinned_windows.is_empty()
}

/// Пересчитать группы окклюдеров по каждому монитору (M4_OCCLUDERS_DESIGN.md
/// §1/§7). Стикеры с одинаковым эффективным правилом видимости (`mode` +
/// `rules` — единственное, от чего зависит результат `occluders::is_occluder`
/// для конкретного окна) делят одну маску: изначальная идея из
/// M4_PREP_NOTES.md §4.2 — одна общая маска на весь монитор — ломала allow-
/// list, если у стикеров разные правила (окно из allow-list одного стикера
/// пряталось бы и под другим, для которого оно должно быть окклюдером).
/// Монитор без стикеров, которым вообще нужна маска, просто не попадает в
/// результат — `redraw` тогда не строит ни одной текстуры маски для него.
fn refresh_occlusion(
    cfg: &Config,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_snapshot: &[WindowInfo],
) -> HashMap<MonitorId, Vec<OccluderSet>> {
    let never_overlap_taskbar = cfg.settings.never_overlap_taskbar;
    let mut result: HashMap<MonitorId, Vec<OccluderSet>> = HashMap::new();
    for (monitor_id, bounds) in monitor_bounds {
        // Правило (mode, rules) каждой уже собранной группы этого монитора —
        // для группировки стикеров без Hash на `Vec<OverlapRule>` (сравниваем
        // напрямую через `==`; дёшево при типичном числе стикеров/групп).
        let mut signatures: Vec<(VisibilityMode, Vec<OverlapRule>)> = Vec::new();
        let mut groups: Vec<OccluderSet> = Vec::new();
        for sticker in cfg
            .stickers
            .iter()
            .filter(|s| s.placement.monitor_id == *monitor_id)
            .filter(|s| s.visibility.mode != VisibilityMode::Always)
        {
            let sig = (sticker.visibility.mode, sticker.visibility.rules.clone());
            let group_idx = match signatures.iter().position(|s| *s == sig) {
                Some(i) => i,
                None => {
                    let rects = occluder_rects_for(
                        sig.0,
                        &sig.1,
                        never_overlap_taskbar,
                        window_snapshot,
                        &bounds.bounds_px,
                    );
                    signatures.push(sig);
                    groups.push(OccluderSet {
                        stickers: Vec::new(),
                        rects,
                    });
                    groups.len() - 1
                }
            };
            groups[group_idx].stickers.push(sticker.id);
        }
        if !groups.is_empty() {
            result.insert(monitor_id.clone(), groups);
        }
    }
    result
}

/// Прямоугольники окон-окклюдеров для одной группы (physические px,
/// локальные для монитора — `occluders::clip_rect`). Свёрнутые окна не
/// участвуют: их прямоугольник мусорный (`WindowInfo::iconic`,
/// M4_PREP_NOTES.md §2.2) — они всё равно не показывают содержимого, под
/// которым стикеру имело бы смысл прятаться.
///
/// Каждый окклюдер режется `occluders::subtract_rects` по окнам, которые
/// физически ВЫШЕ него в z-order (`window_snapshot` уже в z-order сверху
/// вниз — `window_tracker.rs`, `Vec<WindowInfo>` кэша) — не только другими
/// окклюдерами, любым непрозрачным окном. Живой репорт пользователя: без
/// этого маска резала стикер по прямоугольнику окна, даже когда его
/// физически не видно (перекрыто чем-то ещё или пользователь давно
/// переключился на другое окно) — стикер не появлялся обратно, пока
/// окклюдер не закрывался/не двигался буквально. Теперь видимая площадь
/// окклюдера — это его rect минус то, что реально сверху, поэтому смена
/// переднего плана (`EVENT_SYSTEM_FOREGROUND`, см. `window_tracker.rs`)
/// сразу меняет и маску.
fn occluder_rects_for(
    mode: VisibilityMode,
    rules: &[OverlapRule],
    never_overlap_taskbar: bool,
    window_snapshot: &[WindowInfo],
    monitor_bounds_px: &Rect,
) -> Vec<Rect> {
    let visible: Vec<&WindowInfo> = window_snapshot.iter().filter(|w| !w.iconic).collect();
    let mut rects = Vec::new();
    for (i, w) in visible.iter().enumerate() {
        let candidate = OccluderCandidate {
            exe_path: window_exe_path(w),
            title: w.title.clone(),
            class: w.class.clone(),
        };
        if !occluders::is_occluder(&candidate, mode, rules, never_overlap_taskbar) {
            continue;
        }
        let Some(win_rect) = window_rect_to_core(&w.rect) else {
            continue;
        };
        // `visible[..i]` — всё, что стоит выше `w` в z-order (индекс 0 —
        // самое верхнее окно).
        let higher: Vec<Rect> = visible[..i]
            .iter()
            .filter_map(|hw| window_rect_to_core(&hw.rect))
            .collect();
        for piece in occluders::subtract_rects(win_rect, &higher) {
            if let Some(clipped) = occluders::clip_rect(&piece, monitor_bounds_px) {
                rects.push(clipped);
            }
        }
    }
    rects
}

/// `WindowInfo::exe_path` — пустой `PathBuf`, когда `OpenProcess` не дал путь
/// (защищённый процесс, window_enum.rs) — `occluders::is_occluder` ждёт в
/// этом случае `None` (правила по процессу консервативно не матчат).
fn window_exe_path(w: &WindowInfo) -> Option<String> {
    if w.exe_path.as_os_str().is_empty() {
        None
    } else {
        w.exe_path.to_str().map(str::to_owned)
    }
}

/// `WindowRect` (физические px, `i32` ширина/высота — DWM иногда отдаёт
/// вырожденные значения) → `rst_core::model::Rect`. `None` на невырожденных
/// отрицательных/нулевых размерах — таких окно не занимает экранного места,
/// окклюдером быть не может.
fn window_rect_to_core(r: &WindowRect) -> Option<Rect> {
    if r.w <= 0 || r.h <= 0 {
        return None;
    }
    Some(Rect {
        x: r.x,
        y: r.y,
        w: r.w as u32,
        h: r.h as u32,
    })
}


/// Радиус скругления маски перекрытия, физические px — должен совпадать с
/// радиусом в `mainMaskPS` (`crates/rst-render/src/shader.rs`). Дублируется
/// здесь константой, а не импортируется: `shader.rs` — HLSL-строка, у неё
/// нет Rust-API для этого числа.
const MASK_CORNER_RADIUS_PX: i32 = 8;

/// Целиком ли AABB стикера (DIP, `placement`/`rotation`) лежит внутри ОДНОГО
/// из `rects` (физические px, локальные для монитора — та же система
/// координат, что у AABB после перевода через `scale`, ROADMAP.md M4
/// «Отсечение полностью перекрытых стикеров»). Консервативно: `false`, если
/// покрытие достигается только объединением нескольких прямоугольников —
/// приемлемо для чистой оптимизации отрисовки (маска всё равно скрывает
/// стикер визуально), лишь бы не было ложноположительных срабатываний.
///
/// Каждый `rect` перед проверкой уменьшается на [`MASK_CORNER_RADIUS_PX`] со
/// всех сторон (`inset_for_mask_radius`): маска реально рисуется со
/// скруглёнными углами (SDF в `mainMaskPS`), поэтому содержание AABB в
/// ПОЛНОМ прямоугольнике-окклюдере не гарантирует покрытие — угловые дуги
/// (~8px от каждого угла) под маской не оказываются. Без этого уменьшения
/// стикер, попадающий в зону дуги, отсекался бы отсюда, хотя маска реально
/// показала бы его угол (найдено независимым ревью, 2026-08-04).
fn sticker_fully_covered(placement: &Placement, rotation: f64, scale: f32, rects: &[Rect]) -> bool {
    let aabb = hittest::aabb(placement, rotation);
    let px = Rect {
        x: (aabb.x * f64::from(scale)).round() as i32,
        y: (aabb.y * f64::from(scale)).round() as i32,
        w: (aabb.w * f64::from(scale)).round().max(0.0) as u32,
        h: (aabb.h * f64::from(scale)).round().max(0.0) as u32,
    };
    rects
        .iter()
        .filter_map(inset_for_mask_radius)
        .any(|r| rect_contains(&r, &px))
}

/// Видим ли стикер прямо сейчас (M5a/M5b, ARCHITECTURE.md §4.3 — не
/// декодировать невидимое): `false` для скрытого (`!visible`) или полностью
/// перекрытого маской стикера — тот же консервативный предикат, что
/// отсечение в `redraw()` (`sticker_fully_covered`), но не привязанный к
/// конкретному монитору/кадру рендера. В режиме редактирования маска
/// выключена (M4) — тикает всегда, пока видим. Общий вход для планировщика
/// анимации (M5a — продвигать часы) и синхронизации play/pause видео (M5b —
/// декодер должен играть звук и жечь CPU только пока есть смысл); был
/// специфичен только под анимацию (`animation_should_tick`), переименован
/// при добавлении второго вызывающего.
fn sticker_should_tick(
    sticker: &Sticker,
    edit_active: bool,
    occluder_cache: &HashMap<MonitorId, Vec<OccluderSet>>,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) -> bool {
    if !sticker.visible {
        return false;
    }
    if edit_active {
        return true;
    }
    let Some(groups) = occluder_cache.get(&sticker.placement.monitor_id) else {
        return true;
    };
    let Some(group) = groups.iter().find(|g| g.stickers.contains(&sticker.id)) else {
        return true;
    };
    if group.rects.is_empty() {
        return true;
    }
    let scale = monitor_bounds
        .get(&sticker.placement.monitor_id)
        .map_or(1.0, |b| b.scale) as f32;
    !sticker_fully_covered(
        &sticker.placement,
        sticker.transform.rotation,
        scale,
        &group.rects,
    )
}

/// `outer` уменьшенный на [`MASK_CORNER_RADIUS_PX`] со всех сторон — область,
/// где полное покрытие скруглённой маской гарантировано (см.
/// `sticker_fully_covered`). `None`, если `outer` меньше `2×radius` по
/// любой стороне (после уменьшения ничего не остаётся — прямоугольник
/// целиком в зоне угловых дуг, значит гарантированного покрытия для него нет
/// вообще).
fn inset_for_mask_radius(outer: &Rect) -> Option<Rect> {
    let inset2 = MASK_CORNER_RADIUS_PX * 2;
    if (outer.w as i32) <= inset2 || (outer.h as i32) <= inset2 {
        return None;
    }
    Some(Rect {
        x: outer.x + MASK_CORNER_RADIUS_PX,
        y: outer.y + MASK_CORNER_RADIUS_PX,
        w: outer.w - inset2 as u32,
        h: outer.h - inset2 as u32,
    })
}

/// `inner` целиком внутри `outer` (обе — физические px, общая система
/// координат).
fn rect_contains(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x.saturating_add(inner.w as i32) <= outer.x.saturating_add(outer.w as i32)
        && inner.y.saturating_add(inner.h as i32) <= outer.y.saturating_add(outer.h as i32)
}

/// `rst_win32::monitors::MonitorInfo` → `rst_core::monitor_loss::MonitorSnapshot`
/// (M3_HOTPLUG_DESIGN.md §2): тот же id/границы/флаг основного, без
/// платформенных полей (`friendly_name`/`dpi`), которые автомату не нужны.
fn core_snapshot(info: &monitors::MonitorInfo) -> MonitorSnapshot {
    MonitorSnapshot {
        id: info.id.clone(),
        bounds_px: info.bounds_px,
        is_primary: info.is_primary,
    }
}

/// Применить системные действия автомата потери монитора (ADR-011, SPEC 6.1)
/// к `cfg`/`sprites`. НЕ кладёт снимок в undo-историю — вызывающий код
/// (ветки `MonitorsChanged`/`Tick`) не трогает `edit.undo_stack`/`redo_stack`/
/// `pending_snapshot`: системная миграция пропавшего монитора не должна быть
/// отменяемой пользователем — `Ctrl+Z` иначе воскрешал бы стикеры на
/// физически отсутствующем мониторе (M3_PREP_NOTES.md §5.3).
fn apply_loss_actions(cfg: &mut Config, sprites: &mut [(Uuid, Sprite)], actions: Vec<LossAction>) {
    for action in actions {
        match action {
            LossAction::HideSticker { sticker_id } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.visible = false;
                }
            }
            LossAction::RestoreVisibility {
                sticker_id,
                visible,
            } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.visible = visible;
                }
            }
            LossAction::Migrate {
                sticker_id,
                placement,
                origin,
                visible,
            } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.placement = placement.clone();
                    s.origin = Some(origin);
                    s.visible = visible;
                }
                if let Some((_, sprite)) = sprites.iter_mut().find(|(id, _)| *id == sticker_id) {
                    sprite.placement = placement;
                }
            }
            LossAction::ReturnHome {
                sticker_id,
                placement,
                rotation,
            } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.placement = placement.clone();
                    s.transform.rotation = rotation;
                    s.origin = None;
                }
                if let Some((_, sprite)) = sprites.iter_mut().find(|(id, _)| *id == sticker_id) {
                    sprite.placement = placement;
                    sprite.transform.rotation = rotation;
                }
            }
            LossAction::ClearOrigin { sticker_id } => {
                if let Some(s) = cfg.stickers.iter_mut().find(|s| s.id == sticker_id) {
                    s.origin = None;
                }
            }
        }
    }
}

/// Применить прозрачность `percent` (0..=100, зеркалирует модель ползунка) к
/// стикеру `id`: конвертация в `0.0..=1.0` и запись через `apply_transform`
/// (позиция/поворот не меняются — только `transform.opacity`, раздел 6).
fn apply_opacity(cfg: &mut Config, sprites: &mut [(Uuid, Sprite)], id: Uuid, percent: u32) {
    let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) else {
        return;
    };
    let placement = sticker.placement.clone();
    let transform = Transform {
        opacity: f64::from(percent.min(100)) / 100.0,
        ..sticker.transform
    };
    apply_transform(cfg, sprites, id, placement, transform);
}

/// Пересчёт масштаба и позиции курсора при `OverlayEvent::DpiChanged`
/// (M3_PREP_NOTES.md §3.6) — чистая арифметика, вынесенная из ветки
/// `DpiChanged` в `run()`, где остаются только GPU-пересоздание цели
/// (`ms.target.resize`) и мутации `HashMap`, которым нужен реальный живой
/// монитор/устройство. Эта часть — нет, поэтому проверяется юнит-тестами на
/// синтетических переходах 100/150/200% (ROADMAP.md M3 «тест на смешанном
/// DPI 100/150/200» — `WM_DPICHANGED` несёт готовое значение DPI независимо
/// от того, откуда оно взялось физически: смена монитора или смена его
/// настройки, — поэтому симулировать значения так же достоверно, как
/// `rst_win32::overlay::handle_dpi_changed` уже делает для самого сообщения).
///
/// `cursor_pos` — DIP-курсор монитора, чья DPI сменилась (вызывающий код
/// зовёт эту функцию, только когда `edit.cursor_monitor == monitor_id`).
/// Курсор физически не двигался — только логический (DIP) масштаб этого
/// монитора, поэтому DIP-позиция пересчитывается пропорционально отношению
/// старого/нового масштаба (то же отношение, что сохраняет физический
/// (px) курсор неизменным: `dip * scale` — инвариант перехода).
fn dpi_change_scale_and_cursor(
    old_scale: f32,
    dpi: u32,
    cursor_pos: (f64, f64),
) -> (f32, (f64, f64)) {
    let new_scale = dpi as f32 / 96.0;
    let ratio = (old_scale / new_scale) as f64;
    (new_scale, (cursor_pos.0 * ratio, cursor_pos.1 * ratio))
}

/// Границы монитора в DIP — общий вход для позиционирования UI-панелей
/// (тулбар/панель у курсора).
fn screen_dip_rect(overlay_size: (u32, u32), scale: f32) -> DipRect {
    DipRect::new(
        0.0,
        0.0,
        overlay_size.0 as f64 / scale as f64,
        overlay_size.1 as f64 / scale as f64,
    )
}

/// Сдвинуть центр так, чтобы AABB стикера целиком помещался в `screen` (не
/// только центр, как `snap::clamp_min_visible`, который специально разрешает
/// уходить за край во время самого драга) — используется сразу после
/// перепривязки на другой монитор: без этого часть bbox остаётся физически
/// над монитором-источником, а хит-тест по `monitor_id` её не видит
/// (docs/M3_STEP7_REVIEW.md, пункт 2.2). Сжатия по осям независимы; для
/// стикера крупнее `screen` по одной из осей — лучшее доступное (прижат к
/// обеим границам разом не будет, но это вырожденный случай вне охвата
/// среза, как и у `clamp_min_visible`).
fn clamp_fully_within_monitor(placement: &Placement, rotation: f64, screen: &DipRect) -> Placement {
    let bounds = hittest::aabb(placement, rotation);
    let mut p = placement.clone();
    if bounds.x < screen.x {
        p.cx += screen.x - bounds.x;
    } else if bounds.x + bounds.w > screen.x + screen.w {
        p.cx -= (bounds.x + bounds.w) - (screen.x + screen.w);
    }
    if bounds.y < screen.y {
        p.cy += screen.y - bounds.y;
    } else if bounds.y + bounds.h > screen.y + screen.h {
        p.cy -= (bounds.y + bounds.h) - (screen.y + screen.h);
    }
    p
}

/// Пересобрать тулбар по текущему выделению (docs/M2_WIRING_PLAN.md,
/// раздел 4): есть, когда режим активен, выделен ровно один стикер и марка
/// не тянется; иначе — `None`. Билдер дёшев, но пересборка сбрасывает
/// hover-подсветку кнопок — приемлемо для этого среза (docs/M2_WIRING_PLAN.md
/// отмечает то же самое про пересборку по смене выделения).
fn rebuild_toolbar(edit: &mut EditState, cfg: &Config, screen_h: f64) {
    if !edit.active || edit.marquee.is_some() {
        edit.toolbar = None;
        return;
    }
    let Some(id) = single_selected_id(&edit.selection) else {
        edit.toolbar = None;
        return;
    };
    let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) else {
        edit.toolbar = None;
        return;
    };
    let bounds = hittest::aabb(&sticker.placement, sticker.transform.rotation);
    // Видео-виджеты тулбара (M5b) — источник состояния тот же Config, что и
    // у остальных полей тулбара: play/pause и громкость крутятся в
    // `sticker.playback`, координатор синхронизирует реальный
    // `rst_video::VideoSource`/`rst_audio::AudioSource` С НИМ (не наоборот),
    // поэтому здесь не нужна карта `videos` — она runtime-кэш, а не
    // источник истины.
    let is_video = matches!(
        &sticker.source,
        StickerSource::File {
            media_type: MediaType::Video,
            ..
        }
    );
    let video = is_video.then(|| toolbar::VideoToolbarState {
        paused: sticker.playback.paused,
        volume_pct: (sticker.playback.volume.clamp(0.0, 1.0) * 100.0).round() as u32,
    });
    let opacity = Some(sticker.transform.opacity);
    edit.toolbar = Some(toolbar::build_toolbar(&bounds, opacity, video, screen_h));
}

/// Пересобрать панель у курсора (раздел 4): есть, пока режим активен, на
/// позиции `edit.cursor_pos`. Пересобирается (не `translate`) на каждом
/// вызове — упрощение этого среза: `translate`-оптимизация из плана бережёт
/// hover при движении мыши поверх самой панели, здесь это не реализовано
/// (известный компромисс, а не забытый шаг).
fn rebuild_cursor_panel(edit: &mut EditState, cfg: &Config, screen: &DipRect) {
    if !edit.active {
        edit.cursor_panel = None;
        return;
    }
    let all_visible = cfg.stickers.iter().all(|s| s.visible);
    edit.cursor_panel = Some(cursor_panel::build_cursor_panel(
        edit.cursor_pos,
        screen,
        all_visible,
    ));
}

/// Пересобрать и тулбар, и панель у курсора вместе — единая точка вызова
/// после любого изменения, которое может повлиять хоть на одну из панелей
/// (смена выделения, undo/redo, видимость). Раздельные вызовы
/// `rebuild_toolbar`/`rebuild_cursor_panel` по разным подмножествам точек
/// разошлись и оставляли панель у курсора с устаревшей иконкой «показать/
/// скрыть все» после undo/redo и массовых операций
/// (docs/M2_SLICE6_REVIEW.md, пункты 2.5/2.6).
///
/// M3: каждая панель строится по геометрии **своего домашнего** монитора, а
/// не монитора, приславшего текущее событие — тулбар и панель у курсора
/// вполне могут жить на разных мониторах с разными DIP-размерами
/// (docs/M3_STEP4_REVIEW.md, пункт 2.2). `monitor_geometry` — снимок
/// `(width_px, height_px, scale)` по всем мониторам, отдельный от
/// `monitors_map`: без него пришлось бы одновременно держать `&mut
/// MonitorState` текущего монитора (внутри `Renderer`) и `&monitors_map` для
/// поиска геометрии другого монитора, а `HashMap` не даёт занять их
/// одновременно даже по разным ключам.
fn rebuild_ui_panels(
    edit: &mut EditState,
    cfg: &Config,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
) {
    match toolbar_monitor(&edit.selection, cfg).and_then(|id| monitor_geometry.get(id)) {
        Some(&(w, h, scale)) => {
            let screen = screen_dip_rect((w, h), scale);
            rebuild_toolbar(edit, cfg, screen.h);
        }
        None => edit.toolbar = None,
    }
    match monitor_geometry.get(&edit.cursor_monitor) {
        Some(&(w, h, scale)) => {
            let screen = screen_dip_rect((w, h), scale);
            rebuild_cursor_panel(edit, cfg, &screen);
        }
        None => edit.cursor_panel = None,
    }
    // Панель выбора окон редактирует ОДИН конкретный стикер (открыта его
    // кнопкой тулбара) — если выделение сменилось на другой стикер (или
    // снялось, или стало множественным), панель больше не отражает то, что
    // выбрано, и должна закрыться (M4_WINDOW_PICKER_DESIGN.md §1). Снимок
    // окон здесь не нужен — закрытие, а не пересборка содержимого.
    if let Some(state) = &edit.window_picker {
        if single_selected_id(&edit.selection) != Some(state.sticker_id) {
            edit.window_picker = None;
        }
    }
}

/// Открыть панель выбора окон для стикера `sticker_id` (клик по `TB_LAYERS`,
/// обработанный в `run()` через `pending_open_picker` — сам клик происходит
/// в `handle_toolbar_up`, у которого нет `window_snapshot`, дизайн §5.2).
/// Монитор панели — монитор тулбара (панель открыта его кнопкой); если
/// стикер уже не существует или его монитор пропал, панель просто не
/// откроется.
fn open_window_picker(
    edit: &mut EditState,
    cfg: &Config,
    window_snapshot: &[WindowInfo],
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    sticker_id: Uuid,
) {
    let Some(monitor_id) = toolbar_monitor(&edit.selection, cfg).cloned() else {
        return;
    };
    edit.window_picker = Some(WindowPickerState {
        sticker_id,
        // Плейсхолдер — `rebuild_window_picker` ниже строит настоящую
        // панель немедленно, до первой отрисовки.
        panel: Panel::new(
            window_picker::PICKER_PANEL_ID,
            Box2D {
                cx: 0.0,
                cy: 0.0,
                w: 1.0,
                h: 1.0,
                rotation: 0.0,
            },
        ),
        scroll: 0,
        monitor_id,
    });
    rebuild_window_picker(edit, cfg, window_snapshot, monitor_geometry);
}

/// Пересобрать панель выбора окон (мутация `cfg`, влияющая на правило
/// видимости редактируемого стикера, или новый снимок окон —
/// `OverlayMessage::Windows(Changed)`). Центрирована на экране своего
/// монитора, как модал подтверждения — точное позиционирование относительно
/// кнопки тулбара дизайн-доком не зафиксировано (M4_WINDOW_PICKER_DESIGN.md
/// §7.7 не решает вопрос до конца), а центр экрана всегда на виду и не
/// требует расчёта места под кнопкой на всех вариантах тулбара.
fn rebuild_window_picker(
    edit: &mut EditState,
    cfg: &Config,
    window_snapshot: &[WindowInfo],
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
) {
    let Some(state) = &mut edit.window_picker else {
        return;
    };
    let Some(sticker) = cfg.stickers.iter().find(|s| s.id == state.sticker_id) else {
        // Стикер удалён/пропал, пока панель была открыта.
        edit.window_picker = None;
        return;
    };
    let Some(&(w, h, scale)) = monitor_geometry.get(&state.monitor_id) else {
        edit.window_picker = None;
        return;
    };
    let visibility = sticker.visibility.clone();
    let screen = screen_dip_rect((w, h), scale);
    let frame = Box2D {
        cx: screen.w / 2.0,
        cy: screen.h / 2.0,
        w: window_picker::PICKER_WIDTH,
        h: window_picker::PICKER_HEIGHT,
        rotation: 0.0,
    };
    let mut result =
        window_picker::build_picker_panel(&visibility, window_snapshot, state.scroll, frame);
    // Снимок окон мог сжаться (окна закрылись) — скролл, валидный раньше,
    // теперь может указывать за конец списка и строить пустую страницу;
    // кламп и один повторный билд чинят это без падения (`build_picker_panel`
    // сам по себе не паникует на `scroll` за пределами `total_rows` — просто
    // не строит ни одной строки).
    if result.total_rows > 0 && state.scroll >= result.total_rows {
        state.scroll = result.total_rows - 1;
        result =
            window_picker::build_picker_panel(&visibility, window_snapshot, state.scroll, frame);
    } else if result.total_rows == 0 {
        state.scroll = 0;
    }
    state.panel = result.panel;
}

/// Открыть список окон для закрепления (M6, кнопка `BTN_ADD_WINDOW`,
/// `window_pick_list.rs`, фидбэк пользователя 2026-08-10) — центрирован на
/// экране монитора, где была нажата кнопка, тем же паттерном, что
/// `open_preset_picker`. Вызывается из цикла `run()` (`pending_open_pick_list`,
/// докком у объявления поля) — `handle_cursor_panel_up` не имеет
/// `window_snapshot`, которым список наполняется.
fn open_window_pick_list(
    purpose: PickListPurpose,
    edit: &mut EditState,
    cfg: &Config,
    window_snapshot: &[WindowInfo],
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_id: MonitorId,
) {
    if !monitor_geometry.contains_key(&monitor_id) {
        return;
    }
    edit.window_pick_list = Some(WindowPickListState {
        purpose,
        // Плейсхолдер — `rebuild_window_pick_list` ниже строит настоящую
        // панель немедленно, до первой отрисовки.
        panel: Panel::new(
            window_pick_list::PANEL_ID,
            Box2D {
                cx: 0.0,
                cy: 0.0,
                w: 1.0,
                h: 1.0,
                rotation: 0.0,
            },
        ),
        monitor_id,
        scroll: 0,
    });
    rebuild_window_pick_list(edit, cfg, window_snapshot, monitor_geometry);
}

/// Пересобрать список окон для закрепления (новый снимок трекера —
/// `OverlayMessage::Windows(Changed)` — список открытых окон живой, пока
/// панель открыта, в отличие от `window_picker`, который правит один
/// зафиксированный стикер). Центрирован на экране своего монитора, как
/// `rebuild_window_picker`/модал подтверждения.
fn rebuild_window_pick_list(
    edit: &mut EditState,
    cfg: &Config,
    window_snapshot: &[WindowInfo],
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
) {
    let Some(state) = &mut edit.window_pick_list else {
        return;
    };
    let Some(&(w, h, scale)) = monitor_geometry.get(&state.monitor_id) else {
        edit.window_pick_list = None;
        return;
    };
    let screen = screen_dip_rect((w, h), scale);
    // Денайлист + свёрнутые окна отфильтрованы ДО сортировки: строки
    // индексируются по этому же списку при декодировании клика
    // (`handle_window_pick_list_up`) — порядок и состав обязаны совпадать.
    let eligible = window_pick_list::eligible_snapshot(window_snapshot, &cfg.settings.denylist);
    let sorted = window_pick_list::sorted_snapshot(&eligible);
    let frame = Box2D {
        cx: screen.w / 2.0,
        cy: screen.h / 2.0,
        w: window_pick_list::WIDTH,
        h: window_pick_list::height(sorted.len()),
        rotation: 0.0,
    };
    let mut result = window_pick_list::build(&sorted, state.scroll, frame);
    // Снимок окон мог сжаться (окно закрылось, пока список был открыт) —
    // тот же кламп-и-перестрой, что `rebuild_window_picker`.
    if result.total_rows > 0 && state.scroll >= result.total_rows {
        state.scroll = result.total_rows - 1;
        result = window_pick_list::build(&sorted, state.scroll, frame);
    } else if result.total_rows == 0 {
        state.scroll = 0;
    }
    state.panel = result.panel;
}

/// Пересобрать панель свойств закреплённого окна (SPEC «Закрепление окна»,
/// пункт 9) — есть, пока `pinned_selection` указывает на живое окно этого
/// снимка (`None` — окно пропало/чужой монитор/открепилось, панель
/// закрывается тем же принципом «недостающий элемент — не паника», что у
/// остальных pinned-функций). Позиционируется под окном или над ним, если
/// снизу не хватает места (тот же приём, что `toolbar_top`), горизонтально
/// зажата в границы экрана монитора.
///
/// УРЕЗАННАЯ версия (решение пользователя 2026-08-18: «почему нет
/// функции порядка между окнами? …я никогда не просил этого, не знаю,
/// что это»): соседские правила скрыты из UI, но замки (`lock_move`/
/// `lock_interact`) остаются — панель строится через
/// `rst_render::build_pinned_lock_panel`. Полная панель с разделом правил
/// (`rst_render::build_pinned_panel`) больше не вызывается, но НЕ удалена —
/// код правил и их учёт (`resolve_topmost_neighbor`, строки панели)
/// остаются в репозитории в спящем виде: будущий раунд вернёт UI, вернув
/// этот вызов.
/// Где панель инструментов закреплённого окна должна стоять при данной
/// геометрии окна: внутри него, прижата к нижней кромке с отступом
/// [`PINNED_PANEL_INSET`] и центрирована по ширине; ширина ужимается под
/// узкие окна, но не ниже [`rst_render::PINNED_LOCK_PANEL_MIN_WIDTH`].
/// Общая точка правды для сборки панели и для её покадрового догона за
/// движущимся окном (`pinned_panel_catch_up`).
fn pinned_panel_frame(placement: &Placement, bounds: &MonitorBounds, hosts: usize) -> Box2D {
    let height = rst_render::pinned_lock_panel_height(hosts);
    let screen_w = bounds.bounds_px.w as f64 / bounds.scale;
    let screen_h = bounds.bounds_px.h as f64 / bounds.scale;
    let win_top = placement.cy - placement.h / 2.0;
    let win_bottom = placement.cy + placement.h / 2.0;
    let inset = PINNED_PANEL_INSET;
    let width = (placement.w - 2.0 * inset)
        .clamp(
            rst_render::PINNED_LOCK_PANEL_MIN_WIDTH,
            rst_render::PINNED_PANEL_WIDTH,
        )
        .min(screen_w);
    let top = (win_bottom - inset - height)
        .max(win_top + inset)
        .clamp(0.0, (screen_h - height).max(0.0));
    let left = (placement.cx - width / 2.0).clamp(0.0, (screen_w - width).max(0.0));
    Box2D {
        cx: left + width / 2.0,
        cy: top + height / 2.0,
        w: width,
        h: height,
        rotation: 0.0,
    }
}

/// Насколько сдвинуть уже отрисованную панель, чтобы она стояла там, где
/// положено ПРЯМО СЕЙЧАС. `None` — окно пропало или панель уже на месте.
fn pinned_panel_catch_up(
    state: &PinnedPanelState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) -> Option<(f64, f64)> {
    let (monitor_id, placement) =
        pinned_window_dip_placement(state.hwnd, window_snapshot, monitor_bounds)?;
    if monitor_id != state.monitor_id {
        return None; // окно переехало на другой монитор — ждём пересборки
    }
    let want = pinned_panel_frame(&placement, monitor_bounds.get(&monitor_id)?, state.host_rules);
    let have = state.panel.frame();
    let (dx, dy) = (want.cx - have.cx, want.cy - have.cy);
    (dx.abs() >= 0.5 || dy.abs() >= 0.5).then_some((dx, dy))
}

fn rebuild_pinned_panel(
    edit: &mut EditState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) {
    let Some(hwnd) = edit.pinned_selection else {
        edit.pinned_panel = None;
        return;
    };
    let Some(pinned) = edit.pinned_windows.iter().find(|p| p.hwnd == hwnd) else {
        edit.pinned_panel = None;
        return;
    };
    let Some((monitor_id, placement)) =
        pinned_window_dip_placement(hwnd, window_snapshot, monitor_bounds)
    else {
        edit.pinned_panel = None;
        return;
    };
    let bounds = &monitor_bounds[&monitor_id];
    let frame = pinned_panel_frame(&placement, bounds, pinned.host_rules.len());
    // Урезанная панель: замки + «Открепить», без раздела правил соседства
    // (см. доккомент `rst_render::build_pinned_lock_panel`).
    // Подписи строк «показывать только на этих окнах» — имена процессов
    // (правило создаётся по процессу, см. `add_host_rule`); правило по
    // заголовку, если оно когда-нибудь появится, показывается своим
    // шаблоном.
    let hosts: Vec<String> = pinned
        .host_rules
        .iter()
        .map(|rule| {
            rule.process_name
                .clone()
                .or_else(|| rule.title_pattern.clone())
                .unwrap_or_else(|| "—".to_string())
        })
        .collect();
    let panel = rst_render::build_pinned_lock_panel(
        pinned.lock_move,
        pinned.lock_interact,
        &hosts,
        frame,
    );
    edit.pinned_panel = Some(PinnedPanelState {
        host_rules: hosts.len(),
        hwnd,
        panel,
        monitor_id,
        scroll: 0,
    });
}

/// Держать геометрию закреплённых окон в рамках (репорты пользователя
/// 2026-08-21): потолок размера и магнит к кромкам монитора — теперь и для
/// ОБЫЧНЫХ действий пользователя, а не только для жеста внутри режима
/// редактирования ([`apply_pinned_gesture`]).
///
/// Что делает на каждом снимке трекера (вне режима редактирования):
/// 1. **Потолок 90% монитора по каждой оси** ([`pinned_window::clamp_to_monitor_max`]).
///    Применяется ВСЕГДА, в том числе прямо во время того, как пользователь
///    тянет рамку окна: иначе окно спокойно растягивается на весь экран, а
///    ужимается лишь при следующем случайном пересчёте — ровно то, на что
///    пожаловался пользователь. Развёрнутое окно обрабатывается тем же
///    путём: [`WindowPins::set_dwm_bounds`] выводит его из maximized
///    честным `SetWindowPlacement`.
/// 2. **Магнит к кромкам монитора** ([`pinned_window::snap_move`]) — только
///    когда окно ТОЛЬКО ЧТО переехало и жест уже закончился
///    ([`rst_win32::window_pin::is_user_dragging`]). Во время живого драга
///    магнит молчит, иначе мы дрались бы с рукой пользователя каждые 16 мс;
///    последний снимок приходит по `EVENT_SYSTEM_MOVESIZEEND` — он и
///    доводит окно до кромки.
///
/// Гистерезис [`PINNED_GEOMETRY_EPS_PX`] защищает от вечного цикла:
/// собственная перестановка окна порождает новый `LOCATIONCHANGE`, и без
/// порога округление между DWM-габаритами и `SetWindowPos`-координатами
/// гоняло бы окно туда-обратно на пиксель.
fn enforce_pinned_geometry(
    edit: &mut EditState,
    window_pins: &WindowPins,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) {
    let desktop = desktop_px_rect(monitor_bounds);
    let mut alive: HashSet<isize> = HashSet::new();
    let targets: Vec<isize> = edit.pinned_windows.iter().map(|p| p.hwnd).collect();
    for hwnd in targets {
        let Some(live) = rst_win32::window_enum::live_rect(hwnd as usize) else {
            continue; // окно скрыто/свёрнуто/умерло — геометрию не трогаем
        };
        alive.insert(hwnd);
        let Some(gesture_monitor) = monitor_for_window_rect(&live, monitor_bounds) else {
            continue;
        };
        let limit = monitor_px_rect(&monitor_bounds[gesture_monitor]);
        let max_size = pinned_window::clamp_to_monitor_max(
            f64::from(live.w),
            f64::from(live.h),
            limit.w(),
            limit.h(),
        );
        // ЖЕСТ ЕЩЁ ИДЁТ. Спорить с модальным циклом ресайза, который живёт
        // в процессе самого окна, покадрово нельзя — это тяга-перетяга с
        // рукой пользователя (дрожь, репорт 2026-08-17). Но и просто ждать
        // конца жеста мало: окно успевает вырасти за лимит, а потом прыгает
        // назад — «разширяется на небольшое время, а потом сразу
        // возвращается» (репорт 2026-08-21).
        //
        // Поэтому рост останавливается РОВНО НА ГРАНИЦЕ: как только размер
        // перевалил за лимит, мы заканчиваем сам жест
        // (`cancel_user_gesture` → `WM_CANCELMODE`) и тем же тиком ставим
        // предельный размер. Спорить дальше не с чем — цикла больше нет,
        // коррекция ровно одна. Пока окно в пределах лимита, жест не
        // трогается вовсе: перемещать и уменьшать окно можно свободно.
        if rst_win32::window_pin::is_user_dragging(hwnd as usize) {
            let over_limit = live.w as f64 > max_size.0 + f64::from(PINNED_GEOMETRY_EPS_PX)
                || live.h as f64 > max_size.1 + f64::from(PINNED_GEOMETRY_EPS_PX);
            if !over_limit {
                // Геометрию НЕ запоминаем: снимок после отпускания обязан
                // увидеть «окно переехало» и доработать магнитом.
                continue;
            }
            rst_win32::window_pin::cancel_user_gesture(hwnd as usize);
        }
        // Развёрнутое окно потолок обязан свернуть до лимита, но спорить с
        // приложением, которое разворачивает себя обратно, — значит мигать:
        // одна попытка в секунду (рекомендация воркера-исследователя
        // 2026-08-21, §8 maxsize.md).
        if rst_win32::window_pin::is_window_maximized(hwnd as usize) {
            let now = Instant::now();
            let fresh = edit
                .pinned_unmaximized_at
                .get(&hwnd)
                .is_some_and(|at| now.duration_since(*at) < PINNED_UNMAXIMIZE_COOLDOWN);
            if fresh {
                continue;
            }
            edit.pinned_unmaximized_at.insert(hwnd, now);
        }
        let Some(monitor_id) = monitor_for_window_rect(&live, monitor_bounds) else {
            continue;
        };
        let bounds = &monitor_bounds[monitor_id];
        let monitor = monitor_px_rect(bounds);

        let mut target = pinned_window::PxRect::from_xywh(
            live.x as f64,
            live.y as f64,
            live.w as f64,
            live.h as f64,
        );
        let (max_w, max_h) = pinned_window::clamp_to_monitor_max(
            target.w(),
            target.h(),
            monitor.w(),
            monitor.h(),
        );
        if max_w != target.w() || max_h != target.h() {
            target = pinned_window::PxRect::from_xywh(target.left, target.top, max_w, max_h);
        }

        let moved = edit
            .pinned_last_rects
            .get(&hwnd)
            .is_none_or(|last| !rects_within(last, &live, PINNED_GEOMETRY_EPS_PX));
        if moved {
            target = pinned_window::snap_move(
                target,
                monitor,
                desktop,
                pinned_window::EDGE_SNAP_DIP * bounds.scale,
            );
        }

        let applied = if px_rect_differs(&target, &live, PINNED_GEOMETRY_EPS_PX) {
            let win_hwnd = HWND(hwnd as *mut core::ffi::c_void);
            window_pins.set_dwm_bounds(
                win_hwnd,
                RECT {
                    left: target.left.round() as i32,
                    top: target.top.round() as i32,
                    right: target.right.round() as i32,
                    bottom: target.bottom.round() as i32,
                },
            )
        } else {
            false
        };
        // Запоминаем ту геометрию, которую окно должно иметь после нашего
        // вмешательства: иначе следующий снимок снова счёл бы окно
        // «только что переехавшим» и магнит зациклился бы сам на себе.
        edit.pinned_last_rects.insert(
            hwnd,
            if applied {
                WindowRect {
                    x: target.left.round() as i32,
                    y: target.top.round() as i32,
                    w: target.w().round() as i32,
                    h: target.h().round() as i32,
                }
            } else {
                live
            },
        );
    }
    edit.pinned_last_rects.retain(|hwnd, _| alive.contains(hwnd));
    edit.pinned_unmaximized_at
        .retain(|hwnd, _| alive.contains(hwnd));
}

/// Прямоугольники совпадают с точностью до `eps` по каждой кромке.
fn rects_within(a: &WindowRect, b: &WindowRect, eps: i32) -> bool {
    (a.x - b.x).abs() <= eps
        && (a.y - b.y).abs() <= eps
        && (a.w - b.w).abs() <= eps
        && (a.h - b.h).abs() <= eps
}

/// Цель расходится с фактической геометрией больше, чем на `eps`.
fn px_rect_differs(target: &pinned_window::PxRect, live: &WindowRect, eps: i32) -> bool {
    let eps = f64::from(eps);
    (target.left - f64::from(live.x)).abs() > eps
        || (target.top - f64::from(live.y)).abs() > eps
        || (target.w() - f64::from(live.w)).abs() > eps
        || (target.h() - f64::from(live.h)).abs() > eps
}

/// Опросить действия панели выбора окон после `Up` (M4_WINDOW_PICKER_DESIGN.md
/// §1, §4): «Выбрать все» и чекбоксы процессов — каждое переключение своим
/// шагом истории, как у тулбара. Чекбоксы окон всегда disabled (дизайн §7.6)
/// и здесь не опрашиваются — выбор только на уровне процесса.
#[allow(clippy::too_many_arguments)]
fn handle_window_picker_up(
    edit: &mut EditState,
    cfg: &mut Config,
    config_path: &Path,
    window_snapshot: &[WindowInfo],
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    occluder_cache: &mut HashMap<MonitorId, Vec<OccluderSet>>,
    pos: (f64, f64),
) -> bool {
    let Some(state) = &mut edit.window_picker else {
        return true;
    };
    state.panel.pointer_event(PointerEvent::Up { pos });
    let sticker_id = state.sticker_id;
    if !cfg.stickers.iter().any(|s| s.id == sticker_id) {
        // Стикер удалён, пока панель была открыта, — закрыть её.
        edit.window_picker = None;
        return true;
    }

    let select_all_clicked = edit
        .window_picker
        .as_mut()
        .and_then(|s| {
            s.panel
                .widget_mut::<Button>(window_picker::PICKER_BTN_SELECT_ALL)
        })
        .is_some_and(Button::take_click);
    if select_all_clicked {
        commit_undo_snapshot(edit, cfg.clone());
        let sticker = cfg
            .stickers
            .iter_mut()
            .find(|s| s.id == sticker_id)
            .expect("наличие стикера проверено выше");
        sticker.visibility = window_picker::toggle_select_all(&sticker.visibility, window_snapshot);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после «выбрать все» в панели выбора окон");
        }
        // Найдено независимым ревью (2026-08-05, см. MEMORY/BUGS.md
        // vault'а): без пересчёта здесь `occluder_cache` остаётся на
        // СТАРЫХ правилах видимости этого стикера до следующего
        // `Windows(Changed)`/`DpiChanged`/`MonitorsChanged` — маска не
        // отражала бы только что выбранные в панели окна, пока не придёт
        // не связанное с этим событие трекера.
        *occluder_cache = refresh_occlusion(cfg, monitor_bounds, window_snapshot);
        rebuild_window_picker(edit, cfg, window_snapshot, monitor_geometry);
        return true;
    }

    let desktop_only_clicked = edit
        .window_picker
        .as_mut()
        .and_then(|s| {
            s.panel
                .widget_mut::<Button>(window_picker::PICKER_BTN_DESKTOP_ONLY)
        })
        .is_some_and(Button::take_click);
    if desktop_only_clicked {
        commit_undo_snapshot(edit, cfg.clone());
        let sticker = cfg
            .stickers
            .iter_mut()
            .find(|s| s.id == sticker_id)
            .expect("наличие стикера проверено выше");
        sticker.visibility = window_picker::apply_desktop_only_preset(&sticker.visibility);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после пресета «только рабочий стол»");
        }
        // Тот же пересчёт, что и после «выбрать все» выше — тот же класс
        // бага (occluder_cache остаётся на старых правилах видимости до
        // следующего несвязанного события трекера).
        *occluder_cache = refresh_occlusion(cfg, monitor_bounds, window_snapshot);
        rebuild_window_picker(edit, cfg, window_snapshot, monitor_geometry);
        return true;
    }

    let groups = window_picker::group_by_process(window_snapshot);
    for (g, group) in groups.iter().enumerate() {
        let toggled = edit
            .window_picker
            .as_mut()
            .and_then(|s| {
                s.panel
                    .widget_mut::<Checkbox>(window_picker::PICKER_ROW_PROCESS_BASE + g as WidgetId)
            })
            .and_then(Checkbox::take_changed)
            .is_some();
        if !toggled {
            continue;
        }
        commit_undo_snapshot(edit, cfg.clone());
        let sticker = cfg
            .stickers
            .iter_mut()
            .find(|s| s.id == sticker_id)
            .expect("наличие стикера проверено выше");
        if let Some(new_rule) = window_picker::toggle_process_group(&sticker.visibility, group) {
            sticker.visibility = new_rule;
        }
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после переключения процесса в панели выбора окон");
        }
        // См. комментарий у «выбрать все» выше — тот же пересчёт нужен и
        // для точечного переключения одного процесса.
        *occluder_cache = refresh_occlusion(cfg, monitor_bounds, window_snapshot);
        rebuild_window_picker(edit, cfg, window_snapshot, monitor_geometry);
        return true;
    }
    true
}

/// Открыть панель быстрого переключения пресетов (M7, клик по
/// `cursor_panel::BTN_PRESETS` в `handle_cursor_panel_up` — только когда
/// `cfg.presets` непусто, вызывающий код это уже проверил). Центрирована на
/// экране своего монитора, как модал подтверждения/панель выбора окон —
/// точное позиционирование у кнопки не важнее простоты (та же логика, что
/// `open_window_picker`). В отличие от неё — не пересобирается впоследствии:
/// пока модальная панель открыта, ничто больше не может изменить
/// `cfg.presets` (`preset_picker.rs`, докком модуля).
fn open_preset_picker(
    edit: &mut EditState,
    cfg: &Config,
    monitor_id: &MonitorId,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
) {
    let Some(&(w, h, scale)) = monitor_geometry.get(monitor_id) else {
        return;
    };
    let screen = screen_dip_rect((w, h), scale);
    let frame = Box2D {
        cx: screen.w / 2.0,
        cy: screen.h / 2.0,
        w: preset_picker::WIDTH,
        h: preset_picker::height(cfg.presets.len()),
        rotation: 0.0,
    };
    edit.preset_picker = Some(PresetPickerState {
        panel: preset_picker::build(&cfg.presets, frame),
        monitor_id: monitor_id.clone(),
    });
}

/// Опросить клик по строке панели пресетов после `Up` (M7) — вызывается из
/// `handle_input` напрямую (не через `handle_cursor_panel_up`: нужны
/// `occluder_cache`/`window_snapshot`/`monitor_bounds`, которых у него нет,
/// но есть у `handle_input`), тем же паттерном, что
/// `OverlayCommand::ApplyPreset` в `run()` — тот же `presets::apply_preset`,
/// тот же пересчёт occluder-кэша и та же пересылка недостающих элементов.
/// Панель закрывается независимо от результата клика — единственная
/// строка, которую можно нажать, это применение конкретного пресета.
#[allow(clippy::too_many_arguments)]
fn handle_preset_picker_up(
    edit: &mut EditState,
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    occluder_cache: &mut HashMap<MonitorId, Vec<OccluderSet>>,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    pos: (f64, f64),
) -> bool {
    let Some(picker) = &mut edit.preset_picker else {
        return true;
    };
    picker.panel.pointer_event(PointerEvent::Up { pos });
    let clicked_id = cfg.presets.iter().enumerate().find_map(|(i, preset)| {
        edit.preset_picker
            .as_mut()
            .and_then(|s| {
                s.panel
                    .widget_mut::<Button>(preset_picker::ROW_BASE + i as WidgetId)
            })
            .is_some_and(Button::take_click)
            .then_some(preset.id)
    });
    edit.preset_picker = None;
    let Some(id) = clicked_id else {
        return true;
    };
    match presets::apply_preset(cfg, id) {
        Ok(outcome) => {
            edit.selection.clear();
            resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после применения пресета из панели у курсора");
            }
            *occluder_cache = refresh_occlusion(cfg, monitor_bounds, window_snapshot);
            rebuild_ui_panels(edit, cfg, monitor_geometry);
            if !outcome.missing.is_empty() {
                let _ = edit
                    .coordinator_tx
                    .send(CoordinatorRequest::PresetMissingElements(outcome.missing));
            }
        }
        Err(e) => {
            tracing::warn!(preset = %id, error = %e, "не удалось применить пресет из панели у курсора")
        }
    }
    true
}

/// Закрепить окно `hwnd` из списка выбора (редизайн пинов, SPEC.md
/// «Закрепление окна») — НОВЫЙ рантайм-путь вместо `add_window_sticker`:
/// никакого `Sticker` в config.json, никакого `config::save`. Только:
/// (1) `WindowPins::pin` — полный topmost одним `SetWindowPos(HWND_TOPMOST)`,
/// позиция НЕ трогается (порт механики PowerToys «Always On Top»: там пин
/// тоже одноразовый `SetWindowPos` без move — «pin = set WS_EX_TOPMOST,
/// unpin = clear it»);
/// (2) кламп размера до 90% монитора по каждой оси
/// ([`pinned_window::clamp_to_monitor_max`]) — окно уже fullscreen/больше
/// монитора при закреплении; снят был 2026-08-18 при портировании PowerToys
/// («зачем вообще что-то менять»), возвращён по прямому запросу пользователя
/// 2026-08-19 («закреплённые окна не могли быть больше чем 90% от размера
/// монитора»); только размер, позиция не трогается;
/// (3) запись в [`EditState::pinned_windows`] — единственный реестр
/// закреплённых окон, чисто рантайм;
/// (4) пуш флэша рамки ([`EditState::pin_flashes`]) — визуальный отклик
/// на пин/анпин по хоткею.
/// Общий вход для списка (этот срез) и хоткея (задача проводки) — тот же
/// путь, что заявлен SPEC: «клик в списке пинит так же, как хоткей».
///
/// Маркер `WindowPins::pin` — сам `hwnd`: у рантайм-пина нет `Uuid` стикера,
/// а маркер используется только для сравнения «свой/чужой» при
/// переиспользовании hwnd (уникальность hwnd среди живых окон достаточна).
///
/// Возвращает `true`, если окно реально закреплено и записано; `false` —
/// no-op (окно исчезло/вне мониторов/уже закреплено/`PinAccessDenied`) —
/// тем же принципом «недостающие элементы — не паника», что у
/// `apply_preset`. Уже закреплённое окно (маркер стоит, в т.ч. от аварийного
/// выхода прошлого запуска) НЕ перенимается в `pinned_windows` — повторный
/// клик просто no-op; тоггл «закрепить/открепить» по хоткею — зона задачи
/// проводки координатора.
fn pin_window(
    edit: &mut EditState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_pins: &mut WindowPins,
    hwnd: usize,
) -> bool {
    let Some(win) = window_snapshot.iter().find(|w| w.hwnd == hwnd) else {
        tracing::warn!("окно исчезло до клика по списку — закрепление пропущено");
        return false;
    };
    if edit.pinned_windows.iter().any(|p| p.hwnd == hwnd as isize) {
        tracing::warn!(hwnd, "окно уже в списке закреплённых — повторный пин пропущен");
        return false;
    }
    let Some(monitor_id) = monitor_for_window_rect(&win.rect, monitor_bounds) else {
        tracing::warn!(hwnd, "окно вне известной геометрии мониторов — закрепление пропущено");
        return false;
    };
    let bounds = &monitor_bounds[monitor_id];
    let result = match window_pins.pin(hwnd as u64, hwnd) {
        // Маркер на окне есть, а в книжке окна нет — осиротевшее закрепление
        // прошлого запуска (жёсткое завершение оставляет маркер на ЧУЖОМ
        // окне навсегда, см. `WindowPins::clear_orphan_markers`). Единственный
        // процесс гарантирован `single_instance`, значит спорить не с кем:
        // переннимаем окно, иначе пользователь упирается в «уже закреплено»
        // и не может ни закрепить, ни открепить его — репорт 2026-08-21 про
        // Проводник и Блокнот.
        Err(Win32Error::AlreadyPinned) => {
            tracing::info!(hwnd, "перенимаю осиротевшее закрепление прошлого запуска");
            window_pins.adopt(hwnd as u64, hwnd)
        }
        other => other,
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(hwnd, error = %e, "не удалось закрепить окно");
            // UIPI (SPEC.md §5.3): показать пользователю, не только в лог —
            // тот же принцип, что у старого add_window_sticker: за отказом
            // стоит явное действие («перезапустите от администратора»),
            // гонки момента клика (PinWindowGone/AlreadyPinned) тостом не
            // сопровождаем.
            if matches!(e, Win32Error::PinAccessDenied) {
                let _ = edit.coordinator_tx.send(CoordinatorRequest::ShowNotification {
                    title: "Не удалось закрепить окно".to_string(),
                    body: e.to_string(),
                });
            }
            return false;
        }
    }
    // Кламп 90%: только размер, позиция не трогается (окно fullscreen/
    // негабаритное при закреплении). Окно успело свернуться между кликом и
    // пином — rect от DWM мусорный, кламп пропускаем.
    if !win.iconic {
        let (clamped_w, clamped_h) = pinned_window::clamp_to_monitor_max(
            win.rect.w as f64,
            win.rect.h as f64,
            bounds.bounds_px.w as f64,
            bounds.bounds_px.h as f64,
        );
        if clamped_w != win.rect.w as f64 || clamped_h != win.rect.h as f64 {
            if let Err(e) = window_pins.move_resize(
                hwnd,
                win.rect.x,
                win.rect.y,
                clamped_w.round() as i32,
                clamped_h.round() as i32,
            ) {
                tracing::warn!(hwnd, error = %e, "кламп размера закреплённого окна не применился");
            }
        }
    }
    edit.pinned_windows.push(PinnedWindow::new(hwnd as isize));
    edit.pin_flashes.push(PinFlash::new(hwnd as isize, PinFlashKind::Pin));
    true
}

/// Верхний по z-order живой сосед, совпавший хотя бы с одним правилом
/// соседства (редизайн пинов, SPEC «Закрепление окна», пункт 3): правила
/// матчатся тем же предикатом, что окклюдеры ([`occluders::rule_matches`] —
/// процесс ИЛИ заголовок, `*`-wildcard); из совпавших берётся верхний по
/// z-order (снимок трекера идёт сверху вниз — первый совпавший и есть
/// верхний). Сам закреплённый таргет исключается (его собственные
/// process/title-признаки часто совпадают с его же правилами), свёрнутые
/// окна не участвуют (rect мусорный, «держаться над свёрнутым» бессмысленно).
/// `None` — ничего не совпало: слот деградирует до верха обычной полосы
/// (`enforce_slot(hwnd, None)`, см. `window_pin.rs`).
fn resolve_topmost_neighbor(
    snapshot: &[WindowInfo],
    hwnd: usize,
    rules: &[OverlapRule],
) -> Option<usize> {
    snapshot
        .iter()
        .filter(|w| !w.iconic && w.hwnd != hwnd)
        .find(|w| {
            let candidate = OccluderCandidate {
                exe_path: window_exe_path(w),
                title: w.title.clone(),
                class: w.class.clone(),
            };
            rules
                .iter()
                .any(|rule| occluders::rule_matches(rule, &candidate))
        })
        .map(|w| w.hwnd)
}

/// Рантайм-обслуживание закреплённых окон на свежем снимке трекера
/// (редизайн пинов, SPEC «Закрепление окна») — вызывается на каждый
/// `OverlayMessage::Windows(Changed)` и ТОЛЬКО вне режима редактирования
/// (гейт на вызывающем: в edit-mode всё пиновое принуждение стоит, SPEC
/// пункт 5; замки на входе в edit-mode уже сняты
/// `suspend_pin_enforcement`).
///
/// Делает три вещи:
/// (1) Снос уничтоженных таргетов: `WindowPins::handle_snapshot` →
///     `TargetDestroyed` — запись вычищается из `EditState::pinned_windows`,
///     снимаются выделение/панель свойств, если указывали на неё.
/// (2) Z-order-слот: для окна с непустыми `host_rules` сосед резолвится
///     [`resolve_topmost_neighbor`] и окно держится непосредственно над ним
///     ([`WindowPins::enforce_slot`]). Пока оно держит фокус переднего
///     плана ([`rst_win32::window_enum::foreground_hwnd`]) — временно
///     поднято поверх всех ([`WindowPins::surface_topmost_temporarily`],
///     пункт 4), на потере фокуса — [`WindowPins::restore_slot`] обратно в
///     слот. Full-topmost окна (пустые правила) обслуживания не требуют:
///     `WS_EX_TOPMOST` уже стоит с момента пина.
/// (3) Move-lock: для окна с `lock_move` фактический rect из снимка
///     скармливается в [`WindowPins::enforce_move_lock`] — snap-back при
///     расхождении; свёрнутое окно пропускается (rect от DWM мусорный).
/// (4) Topmost-backstop (порт механики PowerToys «Always On Top», задача 1):
///     для full-topmost пина (пустые соседские правила) — одноразовая
///     реактивная коррекция [`WindowPins::reassert_topmost_if_needed`]:
///     снял кто-то `WS_EX_TOPMOST` — вернуть тем же `SetWindowPos`, что и
///     пин. Никаких таймеров: `Windows(Changed)` и так приходит по смене
///     переднего плана (трекер классифицирует `EVENT_SYSTEM_FOREGROUND` как
///     полное перечисление) — та же модель, что у PowerToys. Окна
///     соседского слота НЕ бэкстопятся: им `WS_EX_TOPMOST` противопоказан
///     по построению (полоса topmost игнорирует относительный z-order),
///     бэкстоп поднял бы их из слота в топ.
///
/// `surfaced` — hwnd'ы, временно поднятые поверх — живёт в
/// `EditState::surfaced_pins` и поддерживается самой функцией (вставляется
/// при подъёме, вынимается при возврате в слот): источник истины о
/// «поднятом» состоянии между вызовами.
fn maintain_pinned_windows(
    edit: &mut EditState,
    window_snapshot: &[WindowInfo],
    window_pins: &mut WindowPins,
) {
    let foreground = rst_win32::window_enum::foreground_hwnd();

    for event in window_pins.handle_snapshot(window_snapshot) {
        let window_pin::PinEvent::TargetDestroyed { target } = event;
        edit.pinned_windows.retain(|p| p.hwnd != target as isize);
        edit.surfaced_pins.remove(&(target as isize));
        if edit.pinned_selection == Some(target as isize) {
            edit.pinned_selection = None;
        }
        if edit
            .pinned_panel
            .as_ref()
            .is_some_and(|p| p.hwnd == target as isize)
        {
            edit.pinned_panel = None;
        }
    }

    // Переднее окно как пара (процесс, заголовок) — вход решения
    // «показывать ли окно с правилами». Берём из того же снимка трекера,
    // которым живёт весь координатор; переднего окна может не быть вовсе
    // (рабочий стол) — тогда правила заведомо не выполнены.
    let foreground_info = foreground.and_then(|hwnd| {
        window_snapshot
            .iter()
            .find(|w| w.hwnd == hwnd)
            .map(|w| (w.exe_path.to_string_lossy().into_owned(), w.title.clone()))
    });

    for pinned in &mut edit.pinned_windows {
        let hwnd = pinned.hwnd as usize;
        let win_hwnd = HWND(hwnd as *mut core::ffi::c_void);
        if !pinned.host_rules.is_empty() {
            // «Показывать только на этих окнах» (запрос пользователя
            // 2026-08-22): видимость определяется ИДЕНТИЧНОСТЬЮ активного
            // окна, а не геометрией — перекрывает ли хозяин ту область, где
            // лежит закреплённое окно, значения не имеет.
            let action = pinned_window::host_action(&pinned_window::HostContext {
                rules: &pinned.host_rules,
                foreground_is_target: foreground == Some(hwnd),
                foreground_process: foreground_info.as_ref().map(|(exe, _)| exe.as_str()),
                foreground_title: foreground_info.as_ref().map(|(_, title)| title.as_str()),
                target_minimized: rst_win32::window_pin::is_window_minimized(hwnd),
                hidden_by_rules: pinned.hidden_by_rules,
            });
            match action {
                pinned_window::HostAction::Hide => {
                    if window_pins.hide_until_host(win_hwnd) {
                        pinned.hidden_by_rules = true;
                    }
                }
                pinned_window::HostAction::Show => {
                    window_pins.show_for_host(win_hwnd);
                    pinned.hidden_by_rules = false;
                }
                pinned_window::HostAction::None => {
                    // Окно видно и должно быть видно — держим его наверху
                    // тем же бэкстопом, что и обычные пины.
                    if !rst_win32::window_pin::is_window_minimized(hwnd) {
                        pinned.hidden_by_rules = false;
                        window_pins.reassert_topmost_if_needed(win_hwnd);
                    }
                }
            }
        } else {
            // Topmost-backstop (порт PowerToys «Always On Top», задача 1):
            // full-topmost пины — одноразовая реактивная коррекция снятого
            // WS_EX_TOPMOST (см. доккомент функции, пункт 4).
            window_pins.reassert_topmost_if_needed(win_hwnd);
        }
        if pinned.lock_move {
            if let Some(win) = window_snapshot.iter().find(|w| w.hwnd == hwnd && !w.iconic) {
                let rect = RECT {
                    left: win.rect.x,
                    top: win.rect.y,
                    right: win.rect.x + win.rect.w,
                    bottom: win.rect.y + win.rect.h,
                };
                window_pins.enforce_move_lock(win_hwnd, rect);
            }
        }
    }
}

/// Снять пиновое принуждение на входе в режим редактирования (SPEC
/// «Закрепление окна», пункт 5 — самое уточнённое место спеки): оба замка
/// реально выключаются механизмами задачи 2, НЕ трогая флаги
/// `PinnedWindow.lock_move`/`lock_interact` (пользовательская конфигурация
/// остаётся как была, на выходе из edit-mode она восстановится
/// [`resume_pin_enforcement`]); z-order-поднятия (`surfaced_pins`)
/// возвращаются в слоты — принуждение полностью стоит, пока редактирование
/// активно.
fn suspend_pin_enforcement(
    edit: &mut EditState,
    window_snapshot: &[WindowInfo],
    window_pins: &mut WindowPins,
) {
    for pinned in &edit.pinned_windows {
        let hwnd = HWND(pinned.hwnd as usize as *mut core::ffi::c_void);
        window_pins.set_move_lock(hwnd, false);
        window_pins.set_interact_lock(hwnd, false);
        if edit.surfaced_pins.remove(&pinned.hwnd) && !pinned.host_rules.is_empty() {
            let above = resolve_topmost_neighbor(
                window_snapshot,
                pinned.hwnd as usize,
                &pinned.host_rules,
            )
            .map(|n| HWND(n as *mut core::ffi::c_void));
            window_pins.restore_slot(hwnd, above);
        }
    }
}

/// Вернуть пиновое принуждение на выходе из режима редактирования —
/// зеркально [`suspend_pin_enforcement`]: замки применяются по СОХРАНЁННЫМ
/// флагам (`set_move_lock(true)` заодно переснимает эталонный rect —
/// окно могли двигать/ресайзить в edit-mode, SPEC пункт 9), z-order-слоты
/// продолжит обслуживать следующий снимок трекера (следующий
/// `maintain_pinned_windows`; фокус сменился при входе в edit-mode, поэтому
/// свежий `Windows(Changed)` придёт сразу).
fn resume_pin_enforcement(edit: &EditState, window_pins: &mut WindowPins) {
    for pinned in &edit.pinned_windows {
        let hwnd = HWND(pinned.hwnd as usize as *mut core::ffi::c_void);
        if pinned.lock_move {
            window_pins.set_move_lock(hwnd, true);
        }
        if pinned.lock_interact {
            window_pins.set_interact_lock(hwnd, true);
        }
    }
}

/// Полностью открепить окно (SPEC «Закрепление окна», пункт 9, «unpin»):
/// `WindowPins::unpin` снимает `WS_EX_TOPMOST`/маркер и заодно оба замка
/// (ввод возвращается), запись вычищается из `EditState::pinned_windows`,
/// выделение/панель свойств закрываются. Реальное окно НЕ закрывается и
/// фокус НЕ трогается — окно просто остаётся там, куда его положит обычный
/// z-order Windows после снятия topmost (SPEC: «no forced refocus»).
/// Пульс рамки — по подтверждению пользователя (2026-08-18) на ОБА
/// направления: пин и анпин.
fn unpin_window(edit: &mut EditState, window_pins: &mut WindowPins, hwnd: usize) {
    if let Err(e) = window_pins.unpin(hwnd) {
        tracing::warn!(hwnd, error = %e, "не удалось открепить окно");
    }
    edit.pin_flashes.push(PinFlash::new(hwnd as isize, PinFlashKind::Unpin));
    edit.pinned_windows.retain(|p| p.hwnd != hwnd as isize);
    if edit.pinned_selection == Some(hwnd as isize) {
        edit.pinned_selection = None;
    }
    if edit
        .pinned_panel
        .as_ref()
        .is_some_and(|p| p.hwnd == hwnd as isize)
    {
        edit.pinned_panel = None;
    }
}

/// Хоткей «закрепить/открепить сфокусированное окно» (SPEC «Закрепление
/// окна», пункт 1) — работает НЕЗАВИСИМО от режима редактирования, в
/// отличие от списка выбора (`handle_window_pick_list_up`), доступного
/// только в edit-mode. Идентификация — по HWND: повторное нажатие по тому
/// же `GetForegroundWindow`, что уже в `EditState::pinned_windows`,
/// открепляет его тем же путём, что кнопка «Открепить» на панели свойств
/// ([`unpin_window`]).
///
/// Иначе — денй-лист (`cfg.settings.denylist`) молча блокирует пин тем же
/// предикатом, что список выбора и маска окклюдеров
/// ([`occluders::is_denylisted`]); окно, исчезнувшее между хоткеем и этой
/// проверкой, тоже молча пропускается — тот же принцип «недостающие элементы
/// — не паника», что у [`pin_window`], которому в конце концов делегирует
/// сам пин (полный topmost, кламп 90% по каждой оси, запись в реестр —
/// SPEC пункт 1: «уже fullscreen/больше монитора — сразу ужать»).
///
/// Ветка пина НЕ читает кэш координатора (`window_snapshot`) — берёт
/// собственное разовое перечисление [`rst_win32::window_enum::enumerate`]
/// (живой репорт пользователя, 2026-08-17: «Ctrl+Alt+R не работает
/// вообще» — на чистом конфиге без стикеров с `VisibilityMode != Always`
/// и без открытых панелей `tracker_mask_needed` держит трекер спящим
/// неограниченно долго, `window_snapshot` координатора остаётся
/// `Vec::new()` с самого запуска, и хоткей молча не находил в нём НИ
/// ОДНОГО окна — включая только что сфокусированное). Хоткей — разовое
/// действие пользователя, а не непрерывный рендер-цикл: латентность
/// одного `enumerate()` (~25-30 окон, M4_WINDOW_TRACKER_DESIGN.md §1)
/// несравнима с ADR-005 (тот бюджет — про кадры, не про клик хоткея), и
/// результат не зависит от того, спит трекер или нет. Ветка отпина
/// (выше) от снимка не зависит вовсе — она читает только
/// `EditState::pinned_windows` и живой `foreground_hwnd()`.
fn toggle_focused_pin(
    edit: &mut EditState,
    cfg: &Config,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_pins: &mut WindowPins,
) {
    let Some(hwnd) = rst_win32::window_enum::foreground_hwnd() else {
        return;
    };
    if edit.pinned_windows.iter().any(|p| p.hwnd == hwnd as isize) {
        unpin_window(edit, window_pins, hwnd);
        // Звук на открепление хоткеем тоже (запрос пользователя 2026-08-19:
        // «когда я откреплял окно звук тоже проигрывался» — симметрично
        // пину ниже, тем же файлом/громкостью; кнопка «Открепить» на
        // панели свойств звук не проигрывает — тот же принцип «только
        // хоткей», что у пина).
        rst_win32::sound::play_pin_sound(cfg.settings.pin_sound_volume);
        return;
    }
    let fresh_snapshot = rst_win32::window_enum::enumerate();
    let Some(win) = fresh_snapshot.iter().find(|w| w.hwnd == hwnd) else {
        return;
    };
    if occluders::is_denylisted(
        window_exe_path(win).as_deref(),
        Some(&win.title),
        &cfg.settings.denylist,
    ) {
        return;
    }
    if pin_window(edit, &fresh_snapshot, monitor_bounds, window_pins, hwnd) {
        // Звук только на закрепление хоткеем (запрос пользователя
        // 2026-08-19: «когда ты закрепляешь биндом» — конкретно этот путь,
        // не клик по списку окон и не анпин-кнопка на панели); своя
        // громкость, не через AudioMixer стикеров (rst_win32::sound doc
        // comment).
        rst_win32::sound::play_pin_sound(cfg.settings.pin_sound_volume);
    }
}

/// Опросить клик по строке списка окон после `Up` (M6, `window_pick_list.rs`,
/// фидбэк пользователя 2026-08-10) — тем же паттерном, что
/// `handle_preset_picker_up`: строка декодируется обратно в `WindowInfo`
/// через тот же отсортированный снимок, что строил панель
/// (`window_pick_list::sorted_snapshot` — детерминированный порядок,
/// индекс строки не меняется между билдом и кликом в пределах одного
/// снимка). Панель закрывается независимо от результата клика (окно могло
/// исчезнуть между открытием списка и кликом — `pin_window` тогда
/// просто no-op, тем же принципом, что у `apply_preset`).
///
/// С редизайном пинов клик по строке больше НЕ создаёт стикер-окно в
/// конфиге — только рантайм-пин через [`pin_window`] с записью в
/// `EditState::pinned_windows` (см. доккомент поля). Свёрнутые и
/// денайлистовые окна в списке отсутствуют вовсе (фильтр
/// `window_pick_list::eligible_snapshot`, общий для билдера панели и
/// декодирования клика — индексы строк обязаны совпадать).
#[allow(clippy::too_many_arguments)]
fn handle_window_pick_list_up(
    edit: &mut EditState,
    cfg: &Config,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_pins: &mut WindowPins,
    pos: (f64, f64),
) -> bool {
    let Some(state) = &mut edit.window_pick_list else {
        return true;
    };
    state.panel.pointer_event(PointerEvent::Up { pos });
    let sorted = window_pick_list::sorted_snapshot(&window_pick_list::eligible_snapshot(
        window_snapshot,
        &cfg.settings.denylist,
    ));
    let clicked_hwnd = sorted.iter().enumerate().find_map(|(i, window)| {
        edit.window_pick_list
            .as_mut()
            .and_then(|s| {
                s.panel
                    .widget_mut::<Button>(window_pick_list::ROW_BASE + i as WidgetId)
            })
            .is_some_and(Button::take_click)
            .then_some(window.hwnd)
    });
    let purpose = edit
        .window_pick_list
        .as_ref()
        .map_or(PickListPurpose::PinWindow, |s| s.purpose);
    edit.window_pick_list = None;
    let Some(hwnd) = clicked_hwnd else {
        return true;
    };
    match purpose {
        PickListPurpose::PinWindow => {
            pin_window(edit, window_snapshot, monitor_bounds, window_pins, hwnd);
        }
        PickListPurpose::ChooseHost { target } => {
            add_host_rule(edit, window_snapshot, monitor_bounds, target, hwnd);
        }
    }
    true
}

/// Добавить окно `host` в список «показывать только на этих окнах» у
/// закреплённого окна `target` (клик по строке списка окон, запрос
/// пользователя 2026-08-22).
///
/// Правило создаётся ПО ПРОЦЕССУ, а не по заголовку: заголовок у браузера
/// меняется на каждой вкладке, а пользователь показал пальцем на
/// приложение. Имя короткое (`chrome.exe`) — так правило переживает и
/// перезапуск приложения, и его переустановку в другой каталог
/// (`occluders::any_rule_matches` сравнивает короткое имя с полным путём
/// снимка тем же способом, что денй-лист).
///
/// Повторный выбор того же приложения — no-op: одинаковые правила ничего не
/// добавляют, а список замусоривают.
fn add_host_rule(
    edit: &mut EditState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    target: isize,
    host: usize,
) {
    let Some(process) = window_snapshot
        .iter()
        .find(|w| w.hwnd == host)
        .and_then(|w| w.exe_path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
    else {
        tracing::warn!(host, "не удалось определить процесс окна-хозяина — правило не добавлено");
        return;
    };
    if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == target) {
        let already = pinned.host_rules.iter().any(|rule| {
            rule.process_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&process))
        });
        if !already {
            pinned.host_rules.push(OverlapRule {
                process_name: Some(process),
                title_pattern: None,
            });
        }
    }
    rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
}

/// Опросить `take_submitted()` текстовых полей правил соседства панели
/// свойств (process_name/title_pattern) и записать в `PinnedWindow` с этим
/// `hwnd` — общая часть [`handle_pinned_panel_up`] (опрос на `Up`) и
/// `handle_key` (немедленный коммит по `Enter`, тот же паттерн, что
/// `NumericField` тулбара). Пустая строка → `None` — пустое поле не должно
/// матчить пустым правилом (симметрично `occluders::rule_matches_strs`).
/// Возвращает `true`, если хоть одно поле реально изменилось.
fn sync_pinned_rule_text_fields(edit: &mut EditState, hwnd: isize, rule_count: usize) -> bool {
    let mut changed = false;
    for rule_index in 0..rule_count {
        for field in [PinnedRowField::ProcessName, PinnedRowField::TitlePattern] {
            let submitted = edit
                .pinned_panel
                .as_mut()
                .and_then(|s| s.panel.widget_mut::<TextField>(pinned_row_id(rule_index, field)))
                .and_then(TextField::take_submitted);
            let Some(text) = submitted else { continue };
            let value = (!text.is_empty()).then_some(text);
            if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == hwnd) {
                if let Some(rule) = pinned.host_rules.get_mut(rule_index) {
                    match field {
                        PinnedRowField::ProcessName => rule.process_name = value,
                        PinnedRowField::TitlePattern => rule.title_pattern = value,
                        PinnedRowField::Remove => {}
                    }
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Опросить действия панели свойств закреплённого окна после `Up` (SPEC
/// «Закрепление окна», пункт 9) — чекбоксы замков, кнопки добавления/
/// удаления правила соседства, текстовые поля правил (тот же дублирующий
/// опрос на `Up`, что у `NumericField` тулбара — на случай клика в другое
/// место панели без `Enter`) и кнопка «Открепить». Замки — рантайм-флаги
/// `PinnedWindow`, без `config::save`/undo (SPEC: «нельзя сохранить в
/// пресет, всегда нужно выставлять вручную»).
///
/// УРЕЗАННАЯ панель (решение пользователя 2026-08-18) содержит замки и
/// «Открепить» — ветки замков ниже срабатывают, ветки правил соседства
/// (виджетов с теми id в панели нет) — нет, но оставлены: будущий раунд,
/// вернувший полную панель, получит опрос действий обратно без изменений.
fn handle_pinned_panel_up(
    edit: &mut EditState,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_pins: &mut WindowPins,
    pos: (f64, f64),
) -> bool {
    let Some(state) = &mut edit.pinned_panel else {
        return true;
    };
    state.panel.pointer_event(PointerEvent::Up { pos });
    let hwnd = state.hwnd;
    if !edit.pinned_windows.iter().any(|p| p.hwnd == hwnd) {
        // Окно откреплено/уничтожено, пока панель была открыта.
        edit.pinned_panel = None;
        edit.pinned_selection = None;
        return true;
    }

    if edit
        .pinned_panel
        .as_mut()
        .and_then(|s| s.panel.widget_mut::<Button>(rst_render::PINNED_BTN_UNPIN))
        .is_some_and(Button::take_click)
    {
        unpin_window(edit, window_pins, hwnd as usize);
        return true;
    }

    // «Показывать только на…» — открыть список окон в режиме выбора хозяина.
    if edit
        .pinned_panel
        .as_mut()
        .and_then(|s| s.panel.widget_mut::<Button>(rst_render::PINNED_BTN_ADD_HOST))
        .is_some_and(Button::take_click)
    {
        let monitor_id = edit
            .pinned_panel
            .as_ref()
            .map(|s| s.monitor_id.clone())
            .unwrap_or_else(|| edit.cursor_monitor.clone());
        edit.pending_open_pick_list =
            Some((monitor_id, PickListPurpose::ChooseHost { target: hwnd }));
        return true;
    }

    // «×» в строке правила — убрать это окно-хозяина.
    let host_count = edit
        .pinned_windows
        .iter()
        .find(|p| p.hwnd == hwnd)
        .map_or(0, |p| p.host_rules.len());
    for index in 0..host_count.min(rst_render::PINNED_VISIBLE_HOSTS) {
        let clicked = edit
            .pinned_panel
            .as_mut()
            .and_then(|s| {
                s.panel
                    .widget_mut::<Button>(rst_render::PINNED_HOST_ROW_BASE + index as WidgetId)
            })
            .is_some_and(Button::take_click);
        if clicked {
            if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == hwnd) {
                if index < pinned.host_rules.len() {
                    pinned.host_rules.remove(index);
                }
                // Список опустел — окно снова обычный пин поверх всего; если
                // мы его прятали, вернуть обязаны мы же.
                if pinned.host_rules.is_empty() && pinned.hidden_by_rules {
                    window_pins.show_for_host(HWND(hwnd as *mut core::ffi::c_void));
                    pinned.hidden_by_rules = false;
                }
            }
            rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
            return true;
        }
    }

    let move_toggled = edit
        .pinned_panel
        .as_mut()
        .and_then(|s| s.panel.widget_mut::<Checkbox>(rst_render::PINNED_CHECK_MOVE_LOCK))
        .and_then(Checkbox::take_changed);
    if let Some(checked) = move_toggled {
        if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == hwnd) {
            pinned.lock_move = checked;
        }
        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
        return true;
    }

    let interact_toggled = edit
        .pinned_panel
        .as_mut()
        .and_then(|s| s.panel.widget_mut::<Checkbox>(rst_render::PINNED_CHECK_INTERACT_LOCK))
        .and_then(Checkbox::take_changed);
    if let Some(checked) = interact_toggled {
        if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == hwnd) {
            pinned.lock_interact = checked;
        }
        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
        return true;
    }

    if edit
        .pinned_panel
        .as_mut()
        .and_then(|s| s.panel.widget_mut::<Button>(rst_render::PINNED_BTN_ADD_RULE))
        .is_some_and(Button::take_click)
    {
        if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == hwnd) {
            pinned.host_rules.push(OverlapRule::default());
        }
        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
        return true;
    }

    let rule_count = edit
        .pinned_windows
        .iter()
        .find(|p| p.hwnd == hwnd)
        .map_or(0, |p| p.host_rules.len());
    for rule_index in 0..rule_count {
        let removed = edit
            .pinned_panel
            .as_mut()
            .and_then(|s| {
                s.panel
                    .widget_mut::<Button>(pinned_row_id(rule_index, PinnedRowField::Remove))
            })
            .is_some_and(Button::take_click);
        if !removed {
            continue;
        }
        if let Some(pinned) = edit.pinned_windows.iter_mut().find(|p| p.hwnd == hwnd) {
            if rule_index < pinned.host_rules.len() {
                pinned.host_rules.remove(rule_index);
            }
        }
        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
        return true;
    }

    if sync_pinned_rule_text_fields(edit, hwnd, rule_count) {
        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
    }
    true
}

/// Решить целевую видимость и применить её всем стикерам батчем (общая
/// логика хоткея `toggle_all_stickers` и кнопки `BTN_TOGGLE_ALL`,
/// docs/M2_WIRING_PLAN.md, раздел 6 «Панель у курсора»): сходится к
/// однородному состоянию — есть скрытый → показать всех, иначе скрыть всех.
/// Возвращает `true`, если хоть один стикер реально изменился (пустой список
/// стикеров или уже однородное состояние без реальной работы — `false`, шаг
/// истории не тратится).
fn converge_all_stickers_visibility(cfg: &mut Config) -> bool {
    if cfg.stickers.is_empty() {
        return false;
    }
    let target_visible = !cfg.stickers.iter().all(|s| s.visible);
    let ids: Vec<Uuid> = cfg
        .stickers
        .iter()
        .filter(|s| s.visible != target_visible)
        .map(|s| s.id)
        .collect();
    if ids.is_empty() {
        return false;
    }
    for id in ids {
        let _ = ops::toggle_visibility(cfg, id);
    }
    true
}

/// Центр текущего выделения (для позиционирования модала удаления) или
/// центр экрана, если выделение пусто/вырождено (общий вход для `VK_DELETE`
/// и `TB_DELETE`, docs/M2_WIRING_PLAN.md, раздел 7).
/// Центр выделения на мониторе `monitor_id` (для позиционирования модала
/// удаления, который всегда рисуется на этом мониторе) или центр его экрана,
/// если на нём нет выделенных стикеров. Union bbox по стикерам **другого**
/// монитора не годится — их координаты локальны для своего монитора
/// (ADR-010) и не образуют осмысленной точки в системе координат монитора
/// модала (M3, docs/M3_STEP4_REVIEW.md, пункт 2.4); стикеры вне
/// `monitor_id` просто не участвуют в объединении.
fn selection_center_or_screen(
    edit: &EditState,
    cfg: &Config,
    overlay_size: (u32, u32),
    scale: f32,
    monitor_id: &MonitorId,
) -> (f64, f64) {
    let same_monitor: Vec<Sticker> = cfg
        .stickers
        .iter()
        .filter(|s| s.placement.monitor_id == *monitor_id)
        .cloned()
        .collect();
    edit.selection
        .bounds(&same_monitor)
        .map(|r| (r.x + r.w / 2.0, r.y + r.h / 2.0))
        .unwrap_or((
            overlay_size.0 as f64 / 2.0 / scale as f64,
            overlay_size.1 as f64 / 2.0 / scale as f64,
        ))
}

/// Опросить действия тулбара после `Up` (одиночное выделение,
/// docs/M2_WIRING_PLAN.md, раздел 6): опрос ползунка/поля/пяти кнопок,
/// каждая — свой снимок undo. Мультивыделение — следующий срез (билдер уже
/// поддерживает `opacity: None`, но действия батчем сюда не подключены).
#[allow(clippy::too_many_arguments)]
fn handle_toolbar_up(
    edit: &mut EditState,
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    pos: (f64, f64),
    overlay_size: (u32, u32),
    scale: f32,
    monitor_id: &MonitorId,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
) -> bool {
    if let Some(panel) = &mut edit.toolbar {
        panel.pointer_event(PointerEvent::Up { pos });
    }
    let Some(id) = single_selected_id(&edit.selection) else {
        return true;
    };

    // Коммит живого opacity-жеста ползунка, если он был (раздел 6: «на
    // MouseUp... если cfg != before — commit, иначе отбросить»).
    if let Some(before) = edit.ui_pending_snapshot.take() {
        if before != *cfg {
            commit_undo_snapshot(edit, before);
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после изменения прозрачности");
            }
        }
    }

    if let Some(value) = edit
        .toolbar
        .as_mut()
        .and_then(|p| p.widget_mut::<NumericField>(toolbar::TB_FIELD))
        .and_then(NumericField::take_submitted)
    {
        commit_undo_snapshot(edit, cfg.clone());
        apply_opacity(cfg, sprites, id, value);
        if let Some(slider) = edit
            .toolbar
            .as_mut()
            .and_then(|p| p.widget_mut::<Slider>(toolbar::TB_SLIDER))
        {
            slider.set_value(value);
        }
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после изменения прозрачности");
        }
        rebuild_ui_panels(edit, cfg, monitor_geometry);
        return true;
    }

    let clicked = |edit: &mut EditState, id_widget: WidgetId| -> bool {
        edit.toolbar
            .as_mut()
            .and_then(|p| p.widget_mut::<Button>(id_widget))
            .is_some_and(Button::take_click)
    };

    if clicked(edit, toolbar::TB_LAYERS) {
        // Открытие нуждается в `window_snapshot`, которого здесь нет —
        // откладываем через флаг, обрабатываемый в цикле `run()` сразу после
        // текущего сообщения (дизайн §5.2). Повторный клик — переключатель:
        // если панель уже открыта (для любого стикера — в т.ч. этого же),
        // закрыть её сразу же, без похода в `run()`.
        if edit.window_picker.take().is_none() {
            edit.pending_open_picker = Some(id);
        }
        return true;
    }
    if clicked(edit, toolbar::TB_EYE) {
        commit_undo_snapshot(edit, cfg.clone());
        let _ = ops::toggle_visibility(cfg, id);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после переключения видимости");
        }
        rebuild_ui_panels(edit, cfg, monitor_geometry);
        return true;
    }
    if clicked(edit, toolbar::TB_ORDER_UP) {
        commit_undo_snapshot(edit, cfg.clone());
        let _ = ops::step_up(cfg, id);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после изменения порядка");
        }
        return true;
    }
    if clicked(edit, toolbar::TB_ORDER_DOWN) {
        commit_undo_snapshot(edit, cfg.clone());
        let _ = ops::step_down(cfg, id);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после изменения порядка");
        }
        return true;
    }
    if clicked(edit, toolbar::TB_DUPLICATE) {
        commit_undo_snapshot(edit, cfg.clone());
        if let Ok(new_id) = ops::duplicate(cfg, id) {
            resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
            edit.selection.click(Some(new_id));
        }
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после дублирования");
        }
        rebuild_ui_panels(edit, cfg, monitor_geometry);
        return true;
    }
    if clicked(edit, toolbar::TB_RESET_SCALE) {
        // То же действие, что `OverlayCommand::ResetStickerTransform` из
        // окна настроек (фидбэк пользователя 2026-08-09): натуральный
        // размер + сброс поворота/отражений, позиция и прозрачность не
        // трогаются. Молча ничего не делает, если натуральный размер
        // недоступен (стикер-окно без спрайта) — тот же контракт, что у
        // команды настроек.
        if let Some((natural_w, natural_h)) = sticker_natural_size(sprites, id) {
            commit_undo_snapshot(edit, cfg.clone());
            let _ = ops::reset_transform_and_size(cfg, id, natural_w, natural_h);
            if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                let (placement, transform) = (sticker.placement.clone(), sticker.transform);
                if let Some((_, sprite)) = sprites.iter_mut().find(|(sid, _)| *sid == id) {
                    sprite.placement = placement;
                    sprite.transform = transform;
                }
            }
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после сброса масштаба стикера");
            }
            rebuild_ui_panels(edit, cfg, monitor_geometry);
        }
        return true;
    }
    if clicked(edit, toolbar::TB_DELETE) {
        let center = selection_center_or_screen(edit, cfg, overlay_size, scale, monitor_id);
        return begin_delete(
            edit,
            renderer,
            cfg,
            config_path,
            sprites,
            animations,
            videos,
            audio_mixer,
            center,
            monitor_geometry,
            monitor_id,
        );
    }
    // Play/pause (M5b): иконка кнопки отражает действие (см. build_toolbar),
    // а состояние — `sticker.playback.paused`, единственный источник истины.
    // Живой `VideoSource` НЕ трогается здесь напрямую — единая
    // синхронизация cfg → живые источники в `run()` (после каждого
    // сообщения, тот же проход, что видимость/окклюдеры) применит его сама
    // на этой же итерации; раньше прямой вызов `source.play()/pause()`
    // отсюда был одним из нескольких мест, которые был обязаны знать о
    // живом источнике, и путь отката (Ctrl+Z) о нём не знал — тулбар и
    // реальное состояние расходились (независимое ревью сшивки).
    if clicked(edit, toolbar::TB_PLAY_PAUSE) {
        commit_undo_snapshot(edit, cfg.clone());
        if let Some(sticker) = cfg.stickers.iter_mut().find(|s| s.id == id) {
            sticker.playback.paused = !sticker.playback.paused;
        }
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после паузы/воспроизведения видео");
        }
        rebuild_ui_panels(edit, cfg, monitor_geometry);
        return true;
    }
    true
}

/// Опросить действия панели у курсора после `Up` (docs/M2_WIRING_PLAN.md,
/// раздел 6): `BTN_LOAD_FILE` вызывает системный диалог напрямую (COM,
/// `rst_win32::file_dialog` — не нужен round-trip на главный поток из §12,
/// диалог сам инициализирует COM на вызывающем потоке), `BTN_SETTINGS`
/// уходит через `coordinator_tx` — окно настроек живёт на потоке Tauri, а
/// не оверлея, показать/сфокусировать его отсюда напрямую нельзя.
#[allow(clippy::too_many_arguments)]
fn add_sticker_from_dialog(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    scale: f32,
    monitor_id: &MonitorId,
    audio_mixer: Option<&AudioMixer>,
) {
    match file_dialog::pick_media_file(overlay.hwnd()) {
        Ok(Some(path)) => {
            let before = cfg.clone();
            if add_sticker(
                overlay,
                renderer,
                cfg,
                config_path,
                sprites,
                edit,
                path,
                false,
                scale,
                monitor_id,
                audio_mixer,
            ) {
                commit_undo_snapshot(edit, before);
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "не удалось открыть системный диалог выбора файла");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_cursor_panel_up(
    edit: &mut EditState,
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    window_pins: &mut WindowPins,
    window_snapshot: &[WindowInfo],
    pos: (f64, f64),
    overlay_size: (u32, u32),
    scale: f32,
    monitor_id: &MonitorId,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) -> bool {
    if let Some(panel) = &mut edit.cursor_panel {
        panel.pointer_event(PointerEvent::Up { pos });
    }
    let clicked = |edit: &mut EditState, id_widget: WidgetId| -> bool {
        edit.cursor_panel
            .as_mut()
            .and_then(|p| p.widget_mut::<Button>(id_widget))
            .is_some_and(Button::take_click)
    };

    if clicked(edit, cursor_panel::BTN_LOAD_FILE) {
        add_sticker_from_dialog(
            overlay,
            renderer,
            cfg,
            config_path,
            sprites,
            edit,
            scale,
            monitor_id,
            audio_mixer,
        );
        let screen = screen_dip_rect(overlay_size, scale);
        rebuild_cursor_panel(edit, cfg, &screen);
        return true;
    }
    if clicked(edit, cursor_panel::BTN_ADD_WINDOW) {
        // M6 (SPEC.md §5.1, фидбэк пользователя 2026-08-10): открывает
        // список открытых окон для закрепления (`window_pick_list.rs`) —
        // отложено до цикла `run()` (`pending_open_pick_list`), у
        // `handle_cursor_panel_up` нет `window_snapshot`, которым список
        // наполняется (тот же повод, что у `pending_open_picker`/`TB_LAYERS`).
        edit.pending_open_pick_list = Some((monitor_id.clone(), PickListPurpose::PinWindow));
        return true;
    }
    if clicked(edit, cursor_panel::BTN_PRESETS) {
        // M7 (SPEC.md §3.8, ROADMAP.md «быстрое переключение… из панели
        // редактирования»): пустой список — открывать нечего, сообщаем
        // тостом (тот же канал, что HotkeyConflict/PinAccessDenied) вместо
        // пустой панели без единой строки.
        if cfg.presets.is_empty() {
            let (title, body) = crate::i18n::no_presets_notification(&cfg.settings.language);
            let _ = edit
                .coordinator_tx
                .send(CoordinatorRequest::ShowNotification { title, body });
        } else {
            open_preset_picker(edit, cfg, monitor_id, monitor_geometry);
        }
        return true;
    }
    if clicked(edit, cursor_panel::BTN_TOGGLE_ALL) {
        let before = cfg.clone();
        if converge_all_stickers_visibility(cfg) {
            commit_undo_snapshot(edit, before);
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после «показать/скрыть все»");
            }
        }
        let screen = screen_dip_rect(overlay_size, scale);
        rebuild_cursor_panel(edit, cfg, &screen);
        return true;
    }
    if clicked(edit, cursor_panel::BTN_SETTINGS) {
        // Round-trip к главному потоку Tauri (docs/M2_WIRING_PLAN.md, §12) —
        // окна настроек нет на оверлей-потоке, main::setup читает канал.
        let _ = edit.coordinator_tx.send(CoordinatorRequest::OpenSettings);
        return true;
    }
    if clicked(edit, cursor_panel::BTN_EXIT) {
        toggle_edit_mode(
            overlay,
            edit,
            cfg,
            sprites,
            animations,
            videos,
            audio_mixer,
            renderer,
            config_path,
            monitor_geometry,
            monitor_bounds,
            window_pins,
            window_snapshot,
        );
        return true;
    }
    true
}

/// Опросить ползунок прозрачности тулбара на каждом `MouseMove`, пока
/// перетаскивание держит его (`pointer_owner == Toolbar`): применяет живо,
/// первый снимок откладывается до `MouseUp` (docs/M2_WIRING_PLAN.md,
/// раздел 6 — «при первом изменении»). Числовое поле зеркалит новое значение.
fn poll_toolbar_opacity_live(
    edit: &mut EditState,
    cfg: &mut Config,
    sprites: &mut [(Uuid, Sprite)],
) {
    let Some(id) = single_selected_id(&edit.selection) else {
        return;
    };
    let value = {
        let Some(panel) = &mut edit.toolbar else {
            return;
        };
        let Some(slider) = panel.widget_mut::<Slider>(toolbar::TB_SLIDER) else {
            return;
        };
        let Some(v) = slider.take_changed() else {
            return;
        };
        v
    };
    if edit.ui_pending_snapshot.is_none() {
        edit.ui_pending_snapshot = Some(cfg.clone());
    }
    apply_opacity(cfg, sprites, id, value);
    if let Some(panel) = &mut edit.toolbar {
        if let Some(field) = panel.widget_mut::<NumericField>(toolbar::TB_FIELD) {
            field.set_value(value);
        }
    }
}

/// Опросить ползунок громкости тулбара (M5b) на каждом `MouseMove`, пока
/// перетаскивание держит его — тот же паттерн живого применения, что у
/// прозрачности (`poll_toolbar_opacity_live`): снимок для undo коммитит общий
/// код `handle_toolbar_up` на `MouseUp` (генерически, по `ui_pending_snapshot`,
/// не завязан на конкретный виджет); здесь — только `sticker.playback.volume`.
/// Реальный `AudioSource` НЕ трогается напрямую — единая синхронизация cfg →
/// живые источники в `run()` (после каждого сообщения, включая это
/// `MouseMove`) применит громкость на этой же итерации, до следующего
/// redraw — так же слышно сразу, но без второго места, которое обязано
/// помнить о живом источнике (независимое ревью сшивки, находка про откат
/// драга громкости через `Esc`, который применял cfg, но не звук).
fn poll_toolbar_volume_live(edit: &mut EditState, cfg: &mut Config) {
    let Some(id) = single_selected_id(&edit.selection) else {
        return;
    };
    let value = {
        let Some(panel) = &mut edit.toolbar else {
            return;
        };
        let Some(slider) = panel.widget_mut::<Slider>(toolbar::TB_VOLUME) else {
            return;
        };
        let Some(v) = slider.take_changed() else {
            return;
        };
        v
    };
    if edit.ui_pending_snapshot.is_none() {
        edit.ui_pending_snapshot = Some(cfg.clone());
    }
    let volume = f64::from(value.min(100)) / 100.0;
    if let Some(sticker) = cfg.stickers.iter_mut().find(|s| s.id == id) {
        sticker.playback.volume = volume;
    }
}

/// Обработать событие мыши в режиме редактирования. Возвращает `true`, если
/// нужна перерисовка (docs/M2_INTEGRATION_PLAN.md, раздел 6/8).
#[allow(clippy::too_many_arguments)]
fn handle_input(
    event: InputEvent,
    scale: f32,
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    overlay_size: (u32, u32),
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    monitor_id: &MonitorId,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    loss_tracker: &mut MonitorLossTracker,
    window_snapshot: &[WindowInfo],
    occluder_cache: &mut HashMap<MonitorId, Vec<OccluderSet>>,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
    window_pins: &mut WindowPins,
) -> bool {
    let monitor = DipRect::new(
        0.0,
        0.0,
        overlay_size.0 as f64 / scale as f64,
        overlay_size.1 as f64 / scale as f64,
    );
    match event {
        InputEvent::MouseDown { pos, modifiers } => {
            let (dip_x, dip_y) = to_dip(pos, scale);
            // Модал модален: пока открыт, клики в сцену не уходят вообще —
            // ни по кнопкам модала, ни мимо него (docs/M2_WIRING_PLAN.md,
            // раздел 5, п.1) — с ЛЮБОГО монитора, десктоп целиком заблокирован
            // модалом. Но в сам виджет клик роутится, только если он пришёл с
            // монитора модала — иначе локальные DIP-координаты чужого
            // монитора могли бы случайно совпасть с кнопкой «Удалить»
            // (docs/M3_STEP4_REVIEW.md, пункт 2.1).
            if let Some(confirm) = &mut edit.confirm {
                if confirm.monitor_id == *monitor_id {
                    confirm.panel.pointer_event(PointerEvent::Down {
                        pos: (dip_x, dip_y),
                    });
                }
                return true;
            }
            // Панель быстрого переключения пресетов (M7): в отличие от
            // модала подтверждения, клик мимо неё не «ничего не делает», а
            // закрывает её — обычная семантика попап-меню/дропдауна
            // (`preset_picker.rs`, докком модуля). Клик по чужому монитору
            // тоже закрывает: панель нарисована только на своём мониторе,
            // «мимо» неё оттуда — тоже клик мимо.
            if let Some(picker) = &mut edit.preset_picker {
                let hit = picker.monitor_id == *monitor_id
                    && picker
                        .panel
                        .pointer_event(PointerEvent::Down {
                            pos: (dip_x, dip_y),
                        })
                        .consumed;
                if !hit {
                    edit.preset_picker = None;
                }
                return true;
            }
            // Список окон для закрепления (M6, SPEC.md §5.1,
            // `window_pick_list.rs`) — тот же паттерн, что панель пресетов
            // выше: клик мимо панели (или по чужому монитору) закрывает её
            // без изменений, реальное закрепление — `handle_window_pick_list_up`
            // на `MouseUp`.
            if let Some(state) = &mut edit.window_pick_list {
                let hit = state.monitor_id == *monitor_id
                    && state
                        .panel
                        .pointer_event(PointerEvent::Down {
                            pos: (dip_x, dip_y),
                        })
                        .consumed;
                if !hit {
                    edit.window_pick_list = None;
                }
                return true;
            }
            // Панель выбора окон открыта из тулбара — она выше него по
            // z-order (docs/M4_WINDOW_PICKER_DESIGN.md §1), поэтому и в
            // хит-тесте раньше него. НЕ модальна: если клик мимо панели
            // (`consumed == false`), просто идём дальше к панели у
            // курсора/тулбару/сцене — в отличие от модала выше, ранний
            // `return` здесь только на реальном попадании (дизайн §7.7).
            if let Some(state) = &mut edit.window_picker {
                if state.monitor_id == *monitor_id
                    && state
                        .panel
                        .pointer_event(PointerEvent::Down {
                            pos: (dip_x, dip_y),
                        })
                        .consumed
                {
                    edit.pointer_owner = PointerOwner::WindowPicker;
                    return true;
                }
            }
            // Панель свойств закреплённого окна (SPEC «Закрепление окна»,
            // пункт 9) — тот же паттерн, что панель выбора окон выше: не
            // модальна для мыши, клик мимо неё идёт дальше к сцене (где
            // может попасть по ДРУГОМУ закреплённому окну или стикеру).
            if let Some(state) = &mut edit.pinned_panel {
                if state.monitor_id == *monitor_id
                    && state
                        .panel
                        .pointer_event(PointerEvent::Down {
                            pos: (dip_x, dip_y),
                        })
                        .consumed
                {
                    edit.pointer_owner = PointerOwner::PinnedPanel;
                    return true;
                }
            }
            // Приоритет top-down по z-order: панель у курсора выше тулбара
            // на экране (redraw рисует её позже), поэтому и в хит-тесте она
            // первая (docs/M2_WIRING_PLAN.md, раздел 5). Обе панели
            // хит-тестятся только на своём «домашнем» мониторе (M3,
            // docs/M3_STEP4_REVIEW.md, пункт 2.1) — иначе клик по пустому
            // месту монитора A, чьи локальные DIP-координаты совпали с
            // панелью на мониторе B, «поглощался» бы невидимой там панелью.
            if edit.cursor_monitor == *monitor_id {
                if let Some(panel) = &mut edit.cursor_panel {
                    if panel
                        .pointer_event(PointerEvent::Down {
                            pos: (dip_x, dip_y),
                        })
                        .consumed
                    {
                        edit.pointer_owner = PointerOwner::CursorPanel;
                        return true;
                    }
                }
            }
            if toolbar_monitor(&edit.selection, cfg) == Some(monitor_id) {
                if let Some(panel) = &mut edit.toolbar {
                    if panel
                        .pointer_event(PointerEvent::Down {
                            pos: (dip_x, dip_y),
                        })
                        .consumed
                    {
                        edit.pointer_owner = PointerOwner::Toolbar;
                        return true;
                    }
                }
            }
            edit.pointer_owner = PointerOwner::Scene;
            // Ручки ресайза выделенного закреплённого окна — приоритетнее
            // сцены стикеров (тот же приоритет «ручки прежде всего», что у
            // resolve_zone стикеров), проверяются ДО него: Zone стикеров
            // ничего не знает про закреплённые окна (редизайн пинов, SPEC
            // «Закрепление окна», пункт 9 — «move/resize handles reusing the
            // existing generic sticker resize-handle system»).
            if let Some(handle) = resolve_pinned_resize_handle(
                edit,
                window_snapshot,
                monitor_bounds,
                monitor_id,
                dip_x,
                dip_y,
            ) {
                let hwnd = edit
                    .pinned_selection
                    .expect("resolve_pinned_resize_handle требует активного pinned_selection");
                if let Some((win_monitor, start_placement)) =
                    pinned_window_dip_placement(hwnd, window_snapshot, monitor_bounds)
                {
                    edit.pinned_gesture = Some(PinnedGesture::Resize {
                        hwnd,
                        monitor_id: win_monitor,
                        start_placement,
                        handle,
                        grab: (dip_x, dip_y),
                    });
                }
                return false;
            }
            let zone = resolve_zone(cfg, &edit.selection, monitor_id, dip_x, dip_y);
            match zone {
                Zone::Background => {
                    // Клик по прямоугольнику закреплённого окна (SPEC
                    // «Закрепление окна», пункт 9): выделяет его и начинает
                    // перемещение — тем же жестом, что перетаскивание
                    // стикера, только правит РЕАЛЬНОЕ окно
                    // (`PinnedGesture::Drag`), не `cfg`.
                    if let Some(hwnd) = hit_pinned_window_at(
                        edit,
                        window_snapshot,
                        monitor_bounds,
                        monitor_id,
                        dip_x,
                        dip_y,
                    ) {
                        let selection_changed = edit.pinned_selection != Some(hwnd);
                        edit.pinned_selection = Some(hwnd);
                        edit.selection.clear();
                        if let Some((win_monitor, start_placement)) =
                            pinned_window_dip_placement(hwnd, window_snapshot, monitor_bounds)
                        {
                            edit.pinned_gesture = Some(PinnedGesture::Drag {
                                hwnd,
                                monitor_id: win_monitor,
                                grab_dx: dip_x - start_placement.cx,
                                grab_dy: dip_y - start_placement.cy,
                                start_placement,
                            });
                        }
                        if selection_changed {
                            rebuild_ui_panels(edit, cfg, monitor_geometry);
                        }
                        rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
                        return true;
                    }
                    // Настоящий клик по фону — снимает и выделение
                    // закреплённого окна (SPEC пункт 9: выделение либо на
                    // стикерах, либо на закреплённом окне, не оба сразу).
                    if edit.pinned_selection.take().is_some() {
                        edit.pinned_panel = None;
                    }
                    // Решение «клик или марка» откладывается до `MouseUp`/
                    // порога протяжки (docs/M2_WIRING_PLAN.md, раздел 8) —
                    // выделение здесь ещё не трогаем.
                    edit.gesture = Some(Gesture::Marquee {
                        anchor: (dip_x, dip_y),
                        before: edit.selection.ids().to_vec(),
                    });
                    edit.marquee_started = false;
                    false
                }
                Zone::StickerBody(id) => {
                    if edit.pinned_selection.take().is_some() {
                        edit.pinned_panel = None;
                    }
                    let before = edit.selection.ids().to_vec();
                    if modifiers.shift {
                        // Shift+клик строит мультивыделение по одному
                        // (SPEC 3.2) — жест перетаскивания здесь не
                        // начинаем: мульти-драг ещё не реализован
                        // (docs/M2_MULTISELECT_TOOLBAR_NOTES.md, раздел 4).
                        edit.selection.shift_click(id);
                        let changed = before != edit.selection.ids();
                        if changed {
                            rebuild_ui_panels(edit, cfg, monitor_geometry);
                        }
                        return changed;
                    }
                    edit.selection.click(Some(id));
                    let changed = before != edit.selection.ids();
                    if changed {
                        rebuild_ui_panels(edit, cfg, monitor_geometry);
                    }
                    if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                        let start = GestureStart {
                            id,
                            placement: sticker.placement.clone(),
                            transform: sticker.transform,
                        };
                        let grab_dx = dip_x - sticker.placement.cx;
                        let grab_dy = dip_y - sticker.placement.cy;
                        edit.pending_snapshot = Some(cfg.clone());
                        edit.gesture = Some(Gesture::Drag {
                            start,
                            grab_dx,
                            grab_dy,
                        });
                    }
                    changed
                }
                Zone::ResizeHandle(id, handle) => {
                    if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                        let start = GestureStart {
                            id,
                            placement: sticker.placement.clone(),
                            transform: sticker.transform,
                        };
                        edit.pending_snapshot = Some(cfg.clone());
                        edit.gesture = Some(Gesture::Resize {
                            start,
                            handle,
                            grab: (dip_x, dip_y),
                        });
                    }
                    false
                }
                Zone::Rotate(id, _angle_deg) => {
                    if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                        let start = GestureStart {
                            id,
                            placement: sticker.placement.clone(),
                            transform: sticker.transform,
                        };
                        edit.pending_snapshot = Some(cfg.clone());
                        edit.gesture = Some(Gesture::Rotate {
                            start,
                            grab: (dip_x, dip_y),
                        });
                    }
                    false
                }
            }
        }
        InputEvent::MouseMove {
            pos,
            modifiers,
            dragging,
        } => {
            let (dip_x, dip_y) = to_dip(pos, scale);
            edit.cursor_pos = (dip_x, dip_y);
            edit.cursor_monitor = monitor_id.clone();
            if dragging {
                // ВРЕМЕННАЯ диагностика бага «стикер дрейфует сам по себе» —
                // снять после того, как причина найдена по логу реального
                // запуска пользователя.
                tracing::info!(
                    px = pos.x,
                    py = pos.y,
                    dip_x,
                    dip_y,
                    ?edit.pointer_owner,
                    has_gesture = edit.gesture.is_some(),
                    "diag: coordinator MouseMove dragging=true"
                );
            }
            if let Some(confirm) = &mut edit.confirm {
                // Модал блокирует всю сцену на любом мониторе (см. MouseDown),
                // но hover-состояние его виджетов трогаем только своим
                // движением мыши (M3, docs/M3_STEP4_REVIEW.md, пункт 2.1).
                if confirm.monitor_id == *monitor_id {
                    confirm.panel.pointer_event(PointerEvent::Move {
                        pos: (dip_x, dip_y),
                    });
                }
                return true;
            }
            // Панель пресетов (M7): та же модальная блокировка сцены, что у
            // `confirm` выше — hover её строк живёт своим движением мыши.
            if let Some(picker) = &mut edit.preset_picker {
                if picker.monitor_id == *monitor_id {
                    picker.panel.pointer_event(PointerEvent::Move {
                        pos: (dip_x, dip_y),
                    });
                }
                return true;
            }
            // Список окон для закрепления (M6, `window_pick_list.rs`) — та
            // же модальная блокировка сцены, что у панели пресетов выше.
            if let Some(state) = &mut edit.window_pick_list {
                if state.monitor_id == *monitor_id {
                    state.panel.pointer_event(PointerEvent::Move {
                        pos: (dip_x, dip_y),
                    });
                }
                return true;
            }
            match edit.pointer_owner {
                PointerOwner::CursorPanel => {
                    if let Some(panel) = &mut edit.cursor_panel {
                        panel.pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        });
                    }
                    return true;
                }
                PointerOwner::Toolbar => {
                    if let Some(panel) = &mut edit.toolbar {
                        panel.pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        });
                    }
                    poll_toolbar_opacity_live(edit, cfg, sprites);
                    poll_toolbar_volume_live(edit, cfg);
                    return true;
                }
                PointerOwner::WindowPicker => {
                    if let Some(state) = &mut edit.window_picker {
                        state.panel.pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        });
                    }
                    return true;
                }
                PointerOwner::PinnedPanel => {
                    if let Some(state) = &mut edit.pinned_panel {
                        state.panel.pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        });
                    }
                    return true;
                }
                PointerOwner::Scene if dragging && edit.pinned_gesture.is_some() => {
                    let need_redraw = apply_pinned_gesture(
                        edit,
                        monitor_bounds,
                        window_pins,
                        modifiers,
                        (dip_x, dip_y),
                    );
                    return need_redraw;
                }
                PointerOwner::Scene if dragging => {
                    let need_redraw = apply_gesture(
                        cfg,
                        sprites,
                        edit,
                        (dip_x, dip_y),
                        modifiers,
                        monitor,
                        monitor_id,
                    );
                    if need_redraw {
                        // Драг/ресайз/поворот меняют placement/opacity живо —
                        // тулбар должен следовать за стикером в том же кадре
                        // (docs/M2_WIRING_PLAN.md, раздел 4: «едет за
                        // выделением»); панель у курсора раньше «замирала» на
                        // пред-драговой позиции до MouseUp
                        // (docs/M2_SLICE6_REVIEW.md, пункт 2.6) —
                        // `rebuild_ui_panels` чинит обе разом. Марка тоже
                        // проходит через эту ветку, но `rebuild_toolbar` сам
                        // держит его скрытым, пока `edit.marquee.is_some()` —
                        // вызов безопасен для обоих случаев.
                        rebuild_ui_panels(edit, cfg, monitor_geometry);
                    }
                    return need_redraw;
                }
                PointerOwner::Scene | PointerOwner::None => {}
            }
            // Hover: панели top-down для подсветки, затем зона сцены и курсор
            // (docs/M2_WIRING_PLAN.md, раздел 5, п.3). Пересборка вместо
            // `translate` — известное упрощение этого среза (см.
            // rebuild_cursor_panel).
            let mut need_redraw = false;
            if let Some(panel) = &mut edit.cursor_panel {
                need_redraw |= panel
                    .pointer_event(PointerEvent::Move {
                        pos: (dip_x, dip_y),
                    })
                    .redraw;
            }
            let toolbar_here = toolbar_monitor(&edit.selection, cfg) == Some(monitor_id);
            if toolbar_here {
                if let Some(panel) = &mut edit.toolbar {
                    need_redraw |= panel
                        .pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        })
                        .redraw;
                }
            }
            // Тултип: панель свойств закреплённого окна приоритетнее панели
            // у курсора и тулбара (тот же top-down порядок, что у подсветки
            // выше — панель свойств рисуется позже обеих в redraw) — на
            // практике панели не перекрываются, порядок имеет значение
            // только в редком краевом случае. Кнопки без текста подсказки
            // (слайдер/поле) тултип не получают. Пересобираем `TooltipState`
            // заново при каждой смене наведённого виджета — сбрасывает
            // задержку показа (фидбэк пользователя 2026-08-10: 0.2с задержка
            // + 0.3с плавное появление).
            let pinned_here = edit
                .pinned_panel
                .as_ref()
                .is_some_and(|s| s.monitor_id == *monitor_id);
            let new_tooltip = edit
                .pinned_panel
                .as_ref()
                .filter(|s| s.monitor_id == *monitor_id)
                .and_then(|s| s.panel.hovered_widget())
                .and_then(|(id, bounds)| {
                    pinned_panel_tooltip_text(id).map(|text| (bounds, text))
                })
                .or_else(|| {
                    edit.cursor_panel
                        .as_ref()
                        .and_then(Panel::hovered_widget)
                        .and_then(|(id, bounds)| {
                            cursor_panel_tooltip_text(id).map(|text| (bounds, text))
                        })
                })
                .or_else(|| {
                    toolbar_here
                        .then_some(edit.toolbar.as_ref())
                        .flatten()
                        .and_then(Panel::hovered_widget)
                        .and_then(|(id, bounds)| {
                            toolbar_tooltip_text(id).map(|text| (bounds, text))
                        })
                });
            match new_tooltip {
                Some((bounds, text)) => {
                    let same = edit.tooltip.as_ref().is_some_and(|t| {
                        t.text == text && t.anchor == bounds && t.monitor_id == *monitor_id
                    });
                    if !same {
                        edit.tooltip = Some(TooltipState {
                            text,
                            hover_started: Instant::now(),
                            anchor: bounds,
                            monitor_id: monitor_id.clone(),
                        });
                        need_redraw = true;
                    }
                }
                None => {
                    if edit.tooltip.take().is_some() {
                        need_redraw = true;
                    }
                }
            }
            let picker_here = edit
                .window_picker
                .as_ref()
                .is_some_and(|s| s.monitor_id == *monitor_id);
            if picker_here {
                if let Some(state) = &mut edit.window_picker {
                    need_redraw |= state
                        .panel
                        .pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        })
                        .redraw;
                }
            }
            // Панель свойств закреплённого окна — тот же паттерн hover, что
            // панель выбора окон выше (подсветка кнопок и тултипы
            // фидбэка 2026-08-17 требуют живого hover-состояния виджетов).
            if pinned_here {
                if let Some(state) = &mut edit.pinned_panel {
                    need_redraw |= state
                        .panel
                        .pointer_event(PointerEvent::Move {
                            pos: (dip_x, dip_y),
                        })
                        .redraw;
                }
            }
            let over_panel = edit
                .cursor_panel
                .as_ref()
                .is_some_and(|p| p.hit_test((dip_x, dip_y)))
                || (toolbar_here
                    && edit
                        .toolbar
                        .as_ref()
                        .is_some_and(|p| p.hit_test((dip_x, dip_y))))
                || (picker_here
                    && edit
                        .window_picker
                        .as_ref()
                        .is_some_and(|s| s.panel.hit_test((dip_x, dip_y))))
                || (pinned_here
                    && edit
                        .pinned_panel
                        .as_ref()
                        .is_some_and(|s| s.panel.hit_test((dip_x, dip_y))));
            if over_panel {
                overlay.post_cursor_shape(CursorShape::Arrow);
            } else {
                let zone = resolve_zone(cfg, &edit.selection, monitor_id, dip_x, dip_y);
                overlay.post_cursor_shape(cursor_shape_for_zone(&zone));
            }
            need_redraw
        }
        InputEvent::MouseUp { pos, .. } => {
            let (dip_x, dip_y) = to_dip(pos, scale);
            if edit.confirm.is_some() {
                // Забрать модал по значению — дальше нужен `&mut edit` для
                // `commit_undo_snapshot`, а он не может сосуществовать с
                // заимствованием `edit.confirm` (docs/M2_WIRING_PLAN.md,
                // раздел 6, таблица «Модал»).
                let mut confirm = edit.confirm.take().expect("проверено выше");
                // Модал блокирует весь десктоп (клик с любого монитора не
                // уходит в сцену), но в его кнопки клик роутится только со
                // своего монитора — иначе локальные DIP-координаты чужого
                // монитора могли бы случайно совпасть с «Удалить» и стереть
                // выделение (M3, docs/M3_STEP4_REVIEW.md, пункт 2.1). Клик с
                // чужого монитора — тот же путь, что и «мимо модала» ниже.
                if confirm.monitor_id != *monitor_id {
                    edit.confirm = Some(confirm);
                    return true;
                }
                confirm.panel.pointer_event(PointerEvent::Up {
                    pos: (dip_x, dip_y),
                });
                if confirm
                    .panel
                    .widget_mut::<Button>(confirm_dialog::ID_DELETE)
                    .expect("ID_DELETE собран в confirm_dialog::build")
                    .take_click()
                {
                    // Снимок взят при открытии модала — но если между
                    // открытием и «Удалить» пользователь успел щёлкнуть
                    // «Don't ask again», это единственная настройка, которая
                    // могла измениться внутри окна снимка. Без синхронизации
                    // `Ctrl+Z` откатил бы этот глобальный тумблер вместе с
                    // удалением (docs/M2_SLICE4_REVIEW.md, пункт 2) — settings
                    // не относится к тому, что отменяет история удаления.
                    let mut snapshot = confirm.snapshot;
                    snapshot.settings = cfg.settings.clone();
                    commit_undo_snapshot(edit, snapshot);
                    for id in &confirm.ids {
                        cleanup_pasted_file(cfg, *id);
                        let _ = ops::delete(cfg, *id);
                    }
                    edit.selection.prune(&cfg.stickers);
                    resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
                    if let Err(e) = config::save(cfg, config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после удаления через диалог");
                    }
                    rebuild_ui_panels(edit, cfg, monitor_geometry);
                } else if confirm
                    .panel
                    .widget_mut::<Button>(confirm_dialog::ID_DONT_ASK)
                    .expect("ID_DONT_ASK собран в confirm_dialog::build")
                    .take_click()
                {
                    // Тумблер не закрывает модал — пользователь ещё должен
                    // подтвердить (или отменить) само удаление (раздел 6).
                    ops::suppress_delete_confirmation(cfg);
                    if let Err(e) = config::save(cfg, config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после переключения подтверждения удаления");
                    }
                    edit.confirm = Some(confirm);
                } else if confirm
                    .panel
                    .widget_mut::<Button>(confirm_dialog::ID_CANCEL)
                    .expect("ID_CANCEL собран в confirm_dialog::build")
                    .take_click()
                {
                    // Явная отмена — закрыть без изменений (снимок не
                    // коммитился, отбрасываем вместе с `confirm`).
                } else {
                    // Клик мимо модала или по тексту сообщения — модал
                    // модален и должен остаться открытым
                    // (docs/M2_WIRING_PLAN.md, раздел 5: «клик мимо модала —
                    // ничего»; ранее это ошибочно закрывало диалог,
                    // docs/M2_SLICE4_REVIEW.md, пункт 6).
                    edit.confirm = Some(confirm);
                }
                return true;
            }
            if edit.preset_picker.is_some() {
                return handle_preset_picker_up(
                    edit,
                    renderer,
                    cfg,
                    config_path,
                    sprites,
                    animations,
                    videos,
                    audio_mixer,
                    window_snapshot,
                    monitor_bounds,
                    occluder_cache,
                    monitor_geometry,
                    (dip_x, dip_y),
                );
            }
            if edit.window_pick_list.is_some() {
                return handle_window_pick_list_up(
                    edit,
                    cfg,
                    window_snapshot,
                    monitor_bounds,
                    window_pins,
                    (dip_x, dip_y),
                );
            }
            match edit.pointer_owner {
                PointerOwner::CursorPanel => {
                    edit.pointer_owner = PointerOwner::None;
                    return handle_cursor_panel_up(
                        edit,
                        overlay,
                        renderer,
                        cfg,
                        config_path,
                        sprites,
                        animations,
                        videos,
                        audio_mixer,
                        window_pins,
                        window_snapshot,
                        (dip_x, dip_y),
                        overlay_size,
                        scale,
                        monitor_id,
                        monitor_geometry,
                        monitor_bounds,
                    );
                }
                PointerOwner::Toolbar => {
                    edit.pointer_owner = PointerOwner::None;
                    return handle_toolbar_up(
                        edit,
                        renderer,
                        cfg,
                        config_path,
                        sprites,
                        animations,
                        videos,
                        audio_mixer,
                        (dip_x, dip_y),
                        overlay_size,
                        scale,
                        monitor_id,
                        monitor_geometry,
                    );
                }
                PointerOwner::WindowPicker => {
                    edit.pointer_owner = PointerOwner::None;
                    return handle_window_picker_up(
                        edit,
                        cfg,
                        config_path,
                        window_snapshot,
                        monitor_geometry,
                        monitor_bounds,
                        occluder_cache,
                        (dip_x, dip_y),
                    );
                }
                PointerOwner::PinnedPanel => {
                    edit.pointer_owner = PointerOwner::None;
                    return handle_pinned_panel_up(
                        edit,
                        window_snapshot,
                        monitor_bounds,
                        window_pins,
                        (dip_x, dip_y),
                    );
                }
                PointerOwner::Scene if edit.pinned_gesture.is_some() => {
                    // Жест закреплённого окна не пишет `cfg` — коммитить в
                    // историю нечего (SPEC «Закрепление окна»: рантайм-
                    // состояние), просто отпускаем жест и обновляем панель
                    // свойств её новой позицией (окно уже подвинулось живьём
                    // через `WindowPins::move_resize`).
                    edit.pinned_gesture = None;
                    edit.pointer_owner = PointerOwner::None;
                    rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
                    return true;
                }
                PointerOwner::Scene | PointerOwner::None => {}
            }
            edit.pointer_owner = PointerOwner::None;
            if let Some(Gesture::Marquee { .. }) = &edit.gesture {
                let started = edit.marquee_started;
                edit.gesture = None;
                edit.marquee = None;
                edit.marquee_started = false;
                if !started {
                    // Клик по фону без протяжки — снять выделение только
                    // сейчас (docs/M2_WIRING_PLAN.md, раздел 8).
                    edit.selection.click(None);
                }
                rebuild_ui_panels(edit, cfg, monitor_geometry);
                return true;
            }
            // Перепривязка стикера к другому монитору по центру bbox (M3
            // step 7, «простой» вариант из M3_PREP_NOTES.md §5.4). И `Drag`,
            // и `Resize` без `Alt` (якорь неподвижен — центр смещается на
            // половину дельты, до 0.4·w за край источника) могут увести
            // центр стикера за границу монитора — исходная версия этого
            // блока ошибочно считала, что это бывает только у `Drag`
            // (docs/M3_STEP7_REVIEW.md, пункт 2.1). `Rotate` безопасен (центр
            // не двигается) и гейт по реальному движению ниже естественно
            // его пропускает — как и клик без единого движения, который
            // иначе мог бы молча перепривязать стикер и сжечь шаг истории,
            // если центр уже был за краем до этого клика (пункт 2.3).
            if let Some(start) = edit.gesture.as_ref().and_then(Gesture::start) {
                let id = start.id;
                let start_placement = start.placement.clone();
                let current = cfg
                    .stickers
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| (s.placement.clone(), s.transform.rotation));
                if let Some((placement, rotation)) = current {
                    if placement != start_placement {
                        if let Some(source) = monitor_bounds.get(monitor_id) {
                            // Порядок — по id, а не порядок обхода HashMap:
                            // rebind_monitor_by_center документирует тай-брейк
                            // «первый в списке побеждает» как ответственность
                            // вызывающего кода (docs/M3_STEP7_REVIEW.md, пункт
                            // 2.5) — без сортировки выбор между клонированными
                            // мониторами был бы недетерминирован.
                            let mut others: Vec<MonitorBounds> = monitor_bounds
                                .iter()
                                .filter(|(other_id, _)| *other_id != monitor_id)
                                .map(|(_, b)| b.clone())
                                .collect();
                            others.sort_by(|a, b| a.id.0.cmp(&b.id.0));
                            let rebound = monitor_rebind::rebind_monitor_by_center(
                                &placement, source, &others,
                            );
                            if rebound.monitor_id != placement.monitor_id {
                                // Дожать bbox целиком на новый монитор — без
                                // этого видимая часть стикера, оставшаяся
                                // физически над монитором-источником, была бы
                                // мертва для ввода: хит-тест фильтрует по
                                // monitor_id, а не по факту перекрытия
                                // (docs/M3_STEP7_REVIEW.md, пункт 2.2).
                                let rebound = match monitor_geometry.get(&rebound.monitor_id) {
                                    Some(&(w, h, s)) => clamp_fully_within_monitor(
                                        &rebound,
                                        rotation,
                                        &screen_dip_rect((w, h), s),
                                    ),
                                    None => rebound,
                                };
                                if let Some(sticker) = cfg.stickers.iter_mut().find(|s| s.id == id)
                                {
                                    sticker.placement = rebound.clone();
                                }
                                if let Some((_, sprite)) =
                                    sprites.iter_mut().find(|(sid, _)| *sid == id)
                                {
                                    sprite.placement = rebound;
                                }
                                // Тулбар стикера теперь на другом мониторе —
                                // переехать сразу, не дожидаясь несвязанного
                                // триггера пересборки.
                                rebuild_ui_panels(edit, cfg, monitor_geometry);
                            }
                        }
                    }
                }
            }
            let gesture_sticker_id = edit.gesture.as_ref().and_then(Gesture::start).map(|s| s.id);
            if edit.gesture.take().is_some() {
                // Снимок кладём в историю только сейчас, и только если жест
                // реально что-то изменил — клик без движения не тратит шаг
                // истории (docs/M2_SLICE_REVIEW.md, пункт 1). Перепривязка
                // монитора выше (если случилась) уже отражена в `cfg` — она
                // войдёт в тот же снимок, что и сам драг, а не отдельным
                // шагом истории.
                if let Some(before) = edit.pending_snapshot.take() {
                    if before != *cfg {
                        // Пользователь вручную подвинул/повернул стикер —
                        // если тот был смигрирован автоматом потери монитора,
                        // это осознанная правка поверх старого места: сбросить
                        // `origin`, чтобы будущий автовозврат не «воскресил»
                        // стикер обратно и не затёр ручную правку (SPEC 6.1,
                        // пункт 4). Rotate намеренно тоже сюда попадает —
                        // ReturnHome восстанавливает и origin.rotation.
                        //
                        // Известное ограничение (M3_STEP5_6_REVIEW.md, пункт
                        // 2.2): память трекера (`edited`) в undo не участвует.
                        // `Ctrl+Z` после этой правки вернёт `origin = Some`
                        // (из-за отката `cfg` к `before`), но трекер продолжит
                        // считать стикер уже отредактированным пользователем и
                        // не запустит автовозврат — стикер не теряется, но
                        // останется на месте миграции с «мёртвым» origin.
                        if let Some(id) = gesture_sticker_id {
                            if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == id) {
                                let actions = loss_tracker.on_user_edit(sticker);
                                if !actions.is_empty() {
                                    apply_loss_actions(cfg, sprites, actions);
                                }
                            }
                        }
                        commit_undo_snapshot(edit, before);
                    }
                }
                if let Err(e) = config::save(cfg, config_path) {
                    tracing::warn!(error = %e, "не удалось сохранить config.json после жеста редактирования");
                }
                true
            } else {
                false
            }
        }
        InputEvent::MouseWheel { notches } => {
            // Живой репорт пользователя: длинный список окон в панели «Слои
            // видимости»/списке закрепления окон обрезался без возможности
            // долистать — `scroll` был, но ничего не двигало его. Три строки
            // на «щелчок» колеса — типичный шаг ОС для списков. На нижний
            // край (за пределы `total_rows`) кламп делает сам
            // `rebuild_window_picker`/`rebuild_window_pick_list`, здесь
            // нужен только пол в 0 — `saturating_add_signed` даёт его
            // бесплатно.
            const ROWS_PER_NOTCH: isize = 3;
            let rows = (notches as isize).saturating_mul(-ROWS_PER_NOTCH);
            if let Some(state) = &mut edit.window_picker {
                state.scroll = state.scroll.saturating_add_signed(rows);
                rebuild_window_picker(edit, cfg, window_snapshot, monitor_geometry);
                true
            } else if let Some(state) = &mut edit.window_pick_list {
                state.scroll = state.scroll.saturating_add_signed(rows);
                rebuild_window_pick_list(edit, cfg, window_snapshot, monitor_geometry);
                true
            } else if let Some(state) = &mut edit.pinned_panel {
                state.scroll = state.scroll.saturating_add_signed(rows);
                rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
                true
            } else {
                false
            }
        }
        InputEvent::CaptureLost => {
            // Панель держала указатель (например, ползунок прозрачности) —
            // откатить модель к снимку до начала драга, а не отбросить его
            // молча: иначе `cfg`/`sprites` остаются на середине изменения
            // прозрачности, хотя ползунок его больше не отражает
            // (docs/M2_SLICE6_REVIEW.md, пункт 2.2). Заодно пересобрать сами
            // панели — `Panel`/`Slider` держат внутри `capture`/`dragging`,
            // которые `PointerEvent::Up`/`CaptureLost` в виджет не доходят;
            // самый безопасный способ снять их — построить панель заново, а
            // не синтезировать `Up`, который может засчитаться как клик.
            if matches!(
                edit.pointer_owner,
                PointerOwner::Toolbar | PointerOwner::CursorPanel
            ) {
                if let Some(before) = edit.ui_pending_snapshot.take() {
                    *cfg = before;
                    resync_sprites(renderer, cfg, sprites, animations, videos, audio_mixer);
                }
                edit.pointer_owner = PointerOwner::None;
                rebuild_ui_panels(edit, cfg, monitor_geometry);
                return true;
            }
            if edit.pointer_owner == PointerOwner::WindowPicker {
                // Панель выбора окон не ведёт живой драг (нет ползунков —
                // только чекбоксы/кнопка, коммит целиком на `Up`), откатывать
                // `cfg` нечего — только снять зависший armed/hover чекбокса
                // пересборкой (тот же приём, что выше для тулбара/панели у
                // курсора).
                edit.pointer_owner = PointerOwner::None;
                rebuild_window_picker(edit, cfg, window_snapshot, monitor_geometry);
                return true;
            }
            if edit.pointer_owner == PointerOwner::PinnedPanel {
                edit.pointer_owner = PointerOwner::None;
                rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
                return true;
            }
            if let Some(gesture) = edit.pinned_gesture.take() {
                // Отменить незавершённый жест закреплённого окна — вернуть
                // РЕАЛЬНОЕ окно на стартовый rect (тот же принцип «CaptureLost
                // -> отменить жест без коммита», что у стикеров ниже; тут
                // коммитить в `cfg` нечего — только что подвинутое живьём
                // через `move_resize` окно откатывается тем же примитивом).
                let (hwnd, gesture_monitor, start_placement) = match gesture {
                    PinnedGesture::Drag {
                        hwnd,
                        monitor_id,
                        start_placement,
                        ..
                    } => (hwnd, monitor_id, start_placement),
                    PinnedGesture::Resize {
                        hwnd,
                        monitor_id,
                        start_placement,
                        ..
                    } => (hwnd, monitor_id, start_placement),
                };
                if let Some(bounds) = monitor_bounds.get(&gesture_monitor) {
                    let (x, y, w, h) = placement_to_physical_rect(&start_placement, bounds);
                    let _ = window_pins.move_resize(hwnd as usize, x, y, w, h);
                }
                edit.pointer_owner = PointerOwner::None;
                rebuild_pinned_panel(edit, window_snapshot, monitor_bounds);
                return true;
            }
            edit.pointer_owner = PointerOwner::None;
            // Отменить незавершённый жест без сохранения: откатить модель и
            // спрайт к стартовому снимку (docs/M2_INTEGRATION_PLAN.md,
            // раздел 6 — "CaptureLost -> отменить жест без push"). У марки
            // (`Gesture::start() == None`) откатывать в модели нечего —
            // только сбросить визуал (docs/M2_WIRING_PLAN.md, раздел 8).
            match edit.gesture.take() {
                Some(Gesture::Marquee { before, .. }) => {
                    // Марка не мутирует Config, но rubber_band уже успел
                    // поменять edit.selection на каждом Move — вернуть его к
                    // тому, что было до марки, а не оставить «зависшим» на
                    // последнем частичном выделении (docs/M2_SLICE4_REVIEW.md,
                    // пункт 3).
                    edit.selection.clear();
                    for id in before {
                        edit.selection.select(id);
                    }
                    edit.marquee = None;
                    edit.marquee_started = false;
                    true
                }
                Some(gesture) => {
                    if let Some(start) = gesture.start() {
                        apply_transform(
                            cfg,
                            sprites,
                            start.id,
                            start.placement.clone(),
                            start.transform,
                        );
                    }
                    edit.marquee = None;
                    edit.marquee_started = false;
                    // Жест не завершился — отложенный снимок не понадобился,
                    // он ещё не попал в undo_stack (pending_snapshot).
                    edit.pending_snapshot = None;
                    true
                }
                None => false,
            }
        }
    }
}

/// Применить активный жест к текущей мировой точке курсора (DIP). Возвращает
/// `true`, если нужна перерисовка (жест активен и стикер найден).
#[allow(clippy::too_many_arguments)]
fn apply_gesture(
    cfg: &mut Config,
    sprites: &mut [(Uuid, Sprite)],
    edit: &mut EditState,
    (dip_x, dip_y): (f64, f64),
    modifiers: Modifiers,
    monitor: DipRect,
    monitor_id: &MonitorId,
) -> bool {
    if let Some(Gesture::Marquee { anchor, .. }) = &edit.gesture {
        let anchor = *anchor;
        let rect = DipRect::new(anchor.0, anchor.1, dip_x - anchor.0, dip_y - anchor.1);
        // Порог гейтит только переход «клик → марка»: once started, обновлять
        // безусловно — иначе сжатие рамки обратно ниже порога «замораживает»
        // и марку, и выделение на последней надпороговой позиции
        // (docs/M2_SLICE4_REVIEW.md, пункт 4).
        if edit.marquee_started
            || rect.w.abs() >= MARQUEE_THRESHOLD_DIP
            || rect.h.abs() >= MARQUEE_THRESHOLD_DIP
        {
            edit.marquee_started = true;
            edit.marquee = Some((anchor.0, anchor.1, dip_x, dip_y));
            // Марка тянется в локальных DIP одного монитора (мышь захвачена
            // его окном) — сравнивать рамку нужно только со стикерами того
            // же монитора, иначе совпадение координат с другим монитором
            // выделило бы чужой стикер (M3). Как и раньше, `rubber_band`
            // заменяет выделение целиком — стикеры других мониторов,
            // выделенные до начала этой марки, тоже снимутся: то же
            // поведение «замены», что и в однооконном мире, только теперь
            // применимое и к чужим мониторам.
            let same_monitor: Vec<Sticker> = cfg
                .stickers
                .iter()
                .filter(|s| s.placement.monitor_id == *monitor_id)
                .cloned()
                .collect();
            edit.selection.rubber_band(&same_monitor, &rect);
        }
        return true;
    }
    let Some(gesture) = &edit.gesture else {
        return false;
    };
    match gesture {
        Gesture::Drag {
            start,
            grab_dx,
            grab_dy,
        } => {
            let id = start.id;
            let rotation = start.transform.rotation;
            let mut placement = start.placement.clone();
            placement.cx = dip_x - grab_dx;
            placement.cy = dip_y - grab_dy;
            let snap_result =
                snap::snap_placement(&placement, rotation, monitor, &edit.snap, modifiers.ctrl);
            placement.cx += snap_result.dx;
            placement.cy += snap_result.dy;
            let placement = snap::clamp_min_visible(&placement, rotation, monitor);
            apply_transform(cfg, sprites, id, placement, start.transform);
            true
        }
        Gesture::Resize {
            start,
            handle,
            grab,
        } => {
            let id = start.id;
            let delta = (dip_x - grab.0, dip_y - grab.1);
            // Ctrl инвертирует блокировку пропорций в зависимости от типа
            // ручки (фидбэк пользователя 2026-08-09): угловые ручки по
            // умолчанию сохраняют пропорции, с Ctrl — деформируют; боковые
            // (по центру грани) — наоборот, по умолчанию тянут одну ось,
            // с Ctrl сохраняют пропорции. XOR ручки-угла с Ctrl даёт ровно
            // эту инверсию для обоих типов разом.
            let dm = DragModifiers {
                shift: handle.is_corner() != modifiers.ctrl,
                alt: modifiers.alt,
            };
            // Зеркалирование при протаскивании ручки через якорь не касается
            // видео (тот же фидбэк) — для видео размер просто не уходит
            // ниже минимума.
            let allow_mirror = !cfg.stickers.iter().any(|s| {
                s.id == id
                    && matches!(
                        &s.source,
                        StickerSource::File {
                            media_type: MediaType::Video,
                            ..
                        }
                    )
            });
            let result = transform_ops::resize(
                &start.placement,
                &start.transform,
                *handle,
                delta,
                dm,
                allow_mirror,
            );
            let placement =
                snap::clamp_min_visible(&result.placement, result.transform.rotation, monitor);
            apply_transform(cfg, sprites, id, placement, result.transform);
            true
        }
        Gesture::Rotate { start, grab } => {
            let id = start.id;
            let dm = DragModifiers {
                shift: modifiers.shift,
                alt: false,
            };
            let result = transform_ops::rotate(
                &start.placement,
                &start.transform,
                *grab,
                (dip_x, dip_y),
                dm,
            );
            apply_transform(cfg, sprites, id, result.placement, result.transform);
            true
        }
        Gesture::Marquee { .. } => unreachable!("обработано в раннем возврате выше"),
    }
}

/// Применить активный `edit.pinned_gesture` к текущей мировой точке курсора
/// (DIP) — параллельно [`apply_gesture`] для стикеров, но правит РЕАЛЬНОЕ
/// окно через `WindowPins::move_resize`, не `cfg` (SPEC «Закрепление окна»,
/// пункт 9). Ресайз переиспользует ту же математику ручек, что стикеры
/// ([`transform_ops::resize`]) с `Transform::default()` (окна ОС не
/// вращаются) и без зеркалирования (протаскивание ручки через якорь для
/// окна ОС не имеет смысла) — `snap::clamp_min_visible` не нужен, минимум
/// размера навязывает сама ОС (`WindowPins::move_resize`, доккомент модуля).
/// `false`, если жеста нет или его монитор с последнего кадра исчез (окно
/// продолжает двигаться там, где было, просто мы это не рисуем — тот же
/// принцип «недостающий элемент — не паника»).
fn apply_pinned_gesture(
    edit: &EditState,
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
    window_pins: &WindowPins,
    modifiers: Modifiers,
    (dip_x, dip_y): (f64, f64),
) -> bool {
    let Some(gesture) = &edit.pinned_gesture else {
        return false;
    };
    match gesture {
        PinnedGesture::Drag {
            hwnd,
            monitor_id,
            start_placement,
            grab_dx,
            grab_dy,
        } => {
            let Some(bounds) = monitor_bounds.get(monitor_id) else {
                return false;
            };
            let mut placement = start_placement.clone();
            placement.cx = dip_x - grab_dx;
            placement.cy = dip_y - grab_dy;
            let (x, y, w, h) = placement_to_physical_rect(&placement, bounds);
            let snapped = pinned_window::snap_move(
                pinned_window::PxRect::from_xywh(x as f64, y as f64, w as f64, h as f64),
                monitor_px_rect(bounds),
                desktop_px_rect(monitor_bounds),
                pinned_window::EDGE_SNAP_DIP * bounds.scale,
            );
            let _ = window_pins.move_resize(
                *hwnd as usize,
                snapped.left.round() as i32,
                snapped.top.round() as i32,
                snapped.w().round() as i32,
                snapped.h().round() as i32,
            );
            true
        }
        PinnedGesture::Resize {
            hwnd,
            monitor_id,
            start_placement,
            handle,
            grab,
        } => {
            let Some(bounds) = monitor_bounds.get(monitor_id) else {
                return false;
            };
            let delta = (dip_x - grab.0, dip_y - grab.1);
            let dm = DragModifiers {
                shift: handle.is_corner() != modifiers.ctrl,
                alt: modifiers.alt,
            };
            let result = transform_ops::resize(
                start_placement,
                &Transform::default(),
                *handle,
                delta,
                dm,
                false,
            );
            let (x, y, w, h) = placement_to_physical_rect(&result.placement, bounds);
            let snapped = pinned_window::snap_resize(
                pinned_window::PxRect::from_xywh(x as f64, y as f64, w as f64, h as f64),
                monitor_px_rect(bounds),
                pinned_window::EDGE_SNAP_DIP * bounds.scale,
                PINNED_MIN_SIZE_DIP * bounds.scale,
            );
            let _ = window_pins.move_resize(
                *hwnd as usize,
                snapped.left.round() as i32,
                snapped.top.round() as i32,
                snapped.w().round() as i32,
                snapped.h().round() as i32,
            );
            true
        }
    }
}

/// Собрать и отрисовать кадр (ADR-006 — только по событию): затемнение (если
/// активен режим редактирования), стикеры по `order` (снизу вверх), затем
/// рамка выделения поверх, тулбар/панель у курсора/модал.
///
/// M3: кадр ровно для одного монитора (`monitor_id`) — каждое окно получает
/// свой, отфильтрованный по `placement.monitor_id` (M3_PREP_NOTES.md, раздел
/// 5.2). Затемнение режима редактирования рисуется на **каждом** мониторе
/// безусловно (единый режим на весь десктоп, M3_PREP_NOTES.md §5.1);
/// тулбар/панель у курсора/марка/модал — только на том мониторе, к которому
/// они сейчас относятся (выделенный стикер, позиция курсора, монитор,
/// открывший диалог — соответственно), иначе один и тот же UI-элемент
/// нарисовался бы на всех окнах сразу.
///
/// Возвращает `true`, если устройство D3D потеряно (`RenderError::DeviceLost`
/// — сон, смена драйвера, TDR, ARCHITECTURE.md раздел 11): вызывающий код
/// должен остановить перебор остальных мониторов (то же мёртвое устройство)
/// и вызвать восстановление (`recover_device`), а не продолжать рисовать
/// кадры мёртвым устройством.
#[allow(clippy::too_many_arguments)]
fn redraw(
    renderer: &mut Renderer,
    sprites: &[(Uuid, Sprite)],
    cfg: &Config,
    edit: &EditState,
    white_tex: &Texture,
    black_tex: &Texture,
    ui_cache: &mut UiTextureCache,
    width_px: u32,
    height_px: u32,
    scale: f32,
    monitor_id: &MonitorId,
    occluders: Option<&[OccluderSet]>,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) -> bool {
    let mut frame: Vec<Sprite> = Vec::with_capacity(sprites.len() + 1 + 12);
    // (индекс в `frame`, индекс группы в `occluders`) для каждого видимого
    // стикера этого монитора, у которого `visibility.mode != Always` (M4).
    // Заполняется только в ветке `sticker.visible` ниже — единственное
    // место, где в `frame` попадает реальный спрайт стикера (шахматка
    // скрытого стикера и весь остальной UI никогда не маскируются, и то, и
    // то видно только при `edit.active`, а маски там всё равно выключены).
    let mut sticker_mask_slots: Vec<(usize, usize)> = Vec::new();
    // Растровый шрифт — целочисленный пиксельный масштаб; `scale` (DPI/96)
    // округляем, а не берём как есть (text::rasterize ждёт `u32`).
    let text_scale = scale.round().max(1.0) as u32;

    if edit.active {
        let w_dip = width_px as f64 / scale as f64;
        let h_dip = height_px as f64 / scale as f64;
        frame.push(solid_sprite(
            black_tex,
            monitor_id,
            &edit_overlay(w_dip, h_dip),
            rst_render::EDIT_OVERLAY_OPACITY,
        ));
    }

    // Порядок отрисовки стикеров — по `order` (больше — выше, CONFIG.md).
    // Только стикеры ЭТОГО монитора (M3) — чужие рисуются в своём кадре.
    // Скрытые рисуются только в режиме редактирования — чёрно-розовой
    // шахматкой по форме AABB вместо реального содержимого (SPEC.md 3.7):
    // стикер остаётся полностью интерактивным (см. `hit_sticker_at`), просто
    // не видно, что под ней.
    let mut order: Vec<&Sticker> = cfg
        .stickers
        .iter()
        .filter(|s| s.placement.monitor_id == *monitor_id)
        .filter(|s| s.visible || edit.active)
        .collect();
    order.sort_by_key(|s| s.order);
    for sticker in order {
        if sticker.visible {
            let group_idx = occluders
                .and_then(|groups| groups.iter().position(|g| g.stickers.contains(&sticker.id)));
            // Полностью перекрытый стикер не рисуем вовсе (ROADMAP.md M4,
            // «Отсечение полностью перекрытых стикеров из рендера») — маска
            // и так скрыла бы его визуально, отсечение только экономит
            // GPU-работу. Консервативная проверка: содержится ли AABB
            // стикера ЦЕЛИКОМ в ОДНОМ прямоугольнике-окклюдере группы — не
            // ловит покрытие объединением НЕСКОЛЬКИХ окклюдеров, но никогда
            // не отсекает стикер, который реально хоть немного виден
            // (никогда не даёт ложноположительный результат).
            let culled = !edit.active
                && group_idx.is_some_and(|idx| {
                    let rects = &occluders.expect("group_idx только из Some(occluders)")[idx].rects;
                    !rects.is_empty()
                        && sticker_fully_covered(
                            &sticker.placement,
                            sticker.transform.rotation,
                            scale,
                            rects,
                        )
                });
            if culled {
                continue;
            }
            if let Some((_, sprite)) = sprites.iter().find(|(id, _)| *id == sticker.id) {
                if let Some(group_idx) = group_idx {
                    sticker_mask_slots.push((frame.len(), group_idx));
                }
                frame.push(sprite.clone());
            }
            continue;
        }
        // Последний показанный (замороженный) кадр стикера — под шахматкой,
        // а не вместо неё: иначе несколько скрытых стикеров рядом неотличимы
        // друг от друга (фидбэк пользователя 2026-08-09). Кадр реально
        // «стопается» на скрытии — `sticker_should_tick`/видеопетля не
        // продвигают анимацию/видео невидимого стикера, см. там же.
        if let Some((_, sprite)) = sprites.iter().find(|(id, _)| *id == sticker.id) {
            frame.push(sprite.clone());
        }
        let bounds = hittest::aabb(&sticker.placement, sticker.transform.rotation);
        let cell_px = (CHECKERBOARD_CELL_DIP * f64::from(scale)).round().max(1.0) as u32;
        let w_px = (bounds.w * f64::from(scale)).round().max(1.0) as u32;
        let h_px = (bounds.h * f64::from(scale)).round().max(1.0) as u32;
        let rgba = rst_render::checkerboard_tile(cell_px, w_px, h_px);
        match renderer.create_texture_from_rgba(&rgba, w_px, h_px) {
            Ok(tex) => {
                let rect = Box2D {
                    cx: bounds.x + bounds.w / 2.0,
                    cy: bounds.y + bounds.h / 2.0,
                    w: bounds.w,
                    h: bounds.h,
                    rotation: 0.0,
                };
                // Полупрозрачно (HIDDEN_STICKER_CHECKERBOARD_OPACITY) поверх
                // замороженного кадра, вне зависимости от собственной opacity
                // стикера — так видно и что стикер скрыт, и какой именно.
                frame.push(solid_sprite(
                    &tex,
                    monitor_id,
                    &rect,
                    rst_render::HIDDEN_STICKER_CHECKERBOARD_OPACITY,
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "не удалось создать текстуру шахматки для скрытого стикера");
            }
        }
    }

    // Марка — под рамками выделения, только пока реально тянется (порог
    // протяжки, docs/M2_WIRING_PLAN.md, раздел 8/11) и только на мониторе,
    // где она сейчас тянется (M3: мышь захвачена этим окном на всё время
    // жеста, `edit.cursor_monitor` — тот же монитор всю дорогу).
    if let Some((ax, ay, cx, cy)) = edit.marquee {
        if edit.cursor_monitor == *monitor_id {
            let visuals = marquee_visuals((ax, ay), (cx, cy));
            if let Some(tex) = ui_cache.fill_texture(renderer, theme::SLIDER_FILL) {
                if let Some(fill_rect) = &visuals.fill {
                    frame.push(solid_sprite(
                        &tex,
                        monitor_id,
                        fill_rect,
                        rst_render::MARQUEE_FILL_OPACITY,
                    ));
                }
                for dash in &visuals.dashes {
                    frame.push(solid_sprite(
                        &tex,
                        monitor_id,
                        dash,
                        rst_render::MARQUEE_STROKE_OPACITY,
                    ));
                }
            }
        }
    }

    if edit.active {
        for id in edit.selection.ids() {
            let Some(sticker) = cfg
                .stickers
                .iter()
                .find(|s| s.id == *id && s.placement.monitor_id == *monitor_id)
            else {
                continue;
            };
            let selection_box = SelectionBox::new(&sticker.placement, &sticker.transform);
            for rect in selection_box.all_rects() {
                frame.push(solid_sprite(white_tex, monitor_id, &rect, 1.0));
            }
        }
        // Выделенное закреплённое окно (SPEC «Закрепление окна», пункт 9) —
        // янтарная рамка (`HighlightKind::Pin`, задача 3/6) вместо белой
        // рамки стикеров, чтобы визуально не путать «редактирую сторонее
        // окно» с «редактирую свой стикер»; ручки ресайза — та же белая
        // геометрия, что у стикеров ([`resolve_pinned_resize_handle`]).
        if let Some(hwnd) = edit.pinned_selection {
            if let Some((win_monitor, placement)) =
                pinned_window_dip_placement(hwnd, window_snapshot, monitor_bounds)
            {
                if win_monitor == *monitor_id {
                    // `dpi_scale: 1.0` — `placement` уже в DIP, не физических
                    // px: единственный способ переиспользовать
                    // `WindowHighlight` (который сам делит на dpi_scale) без
                    // повторного похода за физическим rect'ом.
                    let highlight = WindowHighlight::new(
                        placement.cx - placement.w / 2.0,
                        placement.cy - placement.h / 2.0,
                        placement.w,
                        placement.h,
                        1.0,
                    );
                    let prims = highlight.primitives(HighlightKind::Pin, HIGHLIGHT_THICKNESS_DIP);
                    primitives_to_sprites(
                        &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
                    );
                    let sbox = SelectionBox::new(&placement, &Transform::default());
                    for (_, rect) in sbox.handle_rects(rst_render::HANDLE_SIZE_DIP) {
                        frame.push(solid_sprite(white_tex, monitor_id, &rect, 1.0));
                    }
                }
            }
        }
    }
    // Угловые индикаторы закреплённого окна (SPEC «Закрепление окна», задача
    // 3/6) — рисуются ВСЕГДА, не только в `edit.active`: и сам пин, и замки
    // действуют ИМЕННО вне режима редактирования (`suspend_pin_enforcement`
    // снимает замки на время редактирования), а бейджи объясняют
    // пользователю, почему окно висит поверх всех и почему оно не двигается
    // или не реагирует на клики прямо сейчас.
    //
    // Слот 0 — булавка (окно закреплено, запрос пользователя 2026-08-21),
    // слот 1 — замок, если включён хотя бы один.
    for pinned in &edit.pinned_windows {
        let Some((win_monitor, placement)) =
            pinned_window_dip_placement(pinned.hwnd, window_snapshot, monitor_bounds)
        else {
            continue;
        };
        if win_monitor != *monitor_id {
            continue;
        }
        let rect = Box2D {
            cx: placement.cx,
            cy: placement.cy,
            w: placement.w,
            h: placement.h,
            rotation: 0.0,
        };
        let mut prims = Vec::new();
        // Постоянная обводка закреплённого окна — опция «Обводка на
        // закреплённом окне» (запрос пользователя 2026-08-22). По умолчанию
        // выключена: рамка поверх чужого окна всё время — сильное вмешательство
        // в чужой интерфейс, это осознанный выбор пользователя, а не дефолт.
        // Цвет и толщина — те же, что у пульса закрепления, чтобы «мигнуло
        // при закреплении» и «висит постоянно» читались как одно состояние.
        if cfg.settings.outline_pinned_windows {
            let highlight = WindowHighlight::new(
                rect.cx - rect.w / 2.0,
                rect.cy - rect.h / 2.0,
                rect.w,
                rect.h,
                1.0,
            );
            prims.extend(highlight.primitives_custom(
                PIN_FLASH_COLOR_PIN,
                PINNED_OUTLINE_OPACITY,
                PIN_FLASH_THICKNESS_DIP,
            ));
        }
        prims.extend(pin_indicator(rect));
        if pinned.lock_move || pinned.lock_interact {
            prims.extend(lock_indicator(rect));
        }
        primitives_to_sprites(&prims, ui_cache, renderer, monitor_id, text_scale, &mut frame);
    }

    // Пульс рамки при пин/анпин по хоткею (запрос пользователя 2026-08-18) —
    // рисуется ВСЕГДА, не только в `edit.active`: пин — фича нормального
    // режима, пульс — его визуальный отклик. Прозрачность — треугольник
    // 0→1 за 0.5 с, 1→0 за следующие 0.5 с; цвет — акцент проекта `#3c9898`
    // (тот же, что `--accent` настроек; `primitives_custom` — примитив
    // задачи 3/6). Редравы во время пульса держит планировщик анимаций
    // (`PinFlash::next_deadline`), кадр не зависит от `edit.active`.
    let flash_now = Instant::now();
    for flash in &edit.pin_flashes {
        if flash.expired(flash_now) {
            continue;
        }
        let Some((win_monitor, placement)) =
            pinned_window_dip_placement(flash.hwnd, window_snapshot, monitor_bounds)
        else {
            continue;
        };
        if win_monitor != *monitor_id {
            continue;
        }
        let highlight = WindowHighlight::new(
            placement.cx - placement.w / 2.0,
            placement.cy - placement.h / 2.0,
            placement.w,
            placement.h,
            1.0,
        );
        let prims = highlight.primitives_custom(
            flash.color(),
            flash.opacity(flash_now),
            PIN_FLASH_THICKNESS_DIP,
        );
        primitives_to_sprites(&prims, ui_cache, renderer, monitor_id, text_scale, &mut frame);
    }

    // Тулбар и панель у курсора — над рамками выделения, под модалом
    // (раздел 11). Тулбар следует за монитором выделенного стикера; панель
    // у курсора и марка — за `edit.cursor_monitor` (M3, см. выше).
    if let Some(toolbar) = &edit.toolbar {
        if toolbar_monitor(&edit.selection, cfg) == Some(monitor_id) {
            let mut prims = Vec::new();
            toolbar.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }
    if let Some(cursor_panel) = &edit.cursor_panel {
        if edit.cursor_monitor == *monitor_id {
            let mut prims = Vec::new();
            cursor_panel.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Тултип (фидбэк пользователя 2026-08-10) — над тулбаром/панелью у
    // курсора (та кнопка, к которой он относится, уже нарисована выше),
    // под панелью выбора окон/пресетов/модалом. `monitor_id` тултипа —
    // монитор кнопки-источника (`update_tooltip_hover`), не обязательно
    // совпадает с `edit.cursor_monitor` (тулбар может жить на другом
    // мониторе, чем текущая позиция курсора, M3).
    if let Some(tooltip) = &edit.tooltip {
        if tooltip.monitor_id == *monitor_id {
            let opacity = tooltip.opacity(Instant::now());
            let prims = tooltip_primitives(tooltip, height_px as f64 / scale as f64, opacity);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Панель выбора окон — открыта из тулбара, поверх него, но под модалом
    // (M4_WINDOW_PICKER_DESIGN.md §1), только на своём «домашнем» мониторе.
    if let Some(state) = &edit.window_picker {
        if state.monitor_id == *monitor_id {
            let mut prims = Vec::new();
            state.panel.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Панель свойств закреплённого окна (SPEC «Закрепление окна», пункт 9)
    // — тот же уровень z-order и тот же паттерн, что панель выбора окон
    // выше (не модальна, привязана к своему «домашнему» монитору).
    if let Some(state) = &edit.pinned_panel {
        if state.monitor_id == *monitor_id {
            let mut prims = Vec::new();
            state.panel.draw(&mut prims);
            // Панель живёт ВНУТРИ окна, а пересобирается по событиям — то
            // есть реже кадра. Рамка выделения и бейдж рисуются по ЖИВОЙ
            // геометрии каждый кадр и за окном поспевают, а панель отставала
            // и дёргалась относительно них (репорт 2026-08-21). Здесь
            // догоняем: считаем, где панель должна быть ПРЯМО СЕЙЧАС, и
            // сдвигаем уже отрисованные примитивы на разницу. Состояние
            // панели (позиции виджетов для попаданий) не трогаем — оно
            // догонит на ближайшей пересборке, а кликают по панели, когда
            // окно стоит.
            if let Some((dx, dy)) = pinned_panel_catch_up(state, window_snapshot, monitor_bounds)
            {
                for prim in &mut prims {
                    prim.translate(dx, dy);
                }
            }
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Панель пресетов (M7) — модальна, как и модал подтверждения ниже,
    // поэтому выше панели выбора окон (она не модальна), но под самим
    // модалом (оба одновременно не бывают открыты на практике — модал
    // открывается только при отсутствии жеста, панель пресетов сама
    // блокирует жест, — порядок здесь на случай неучтённой гонки).
    if let Some(state) = &edit.preset_picker {
        if state.monitor_id == *monitor_id {
            let mut prims = Vec::new();
            state.panel.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Список окон для закрепления (M6, window_pick_list.rs, фидбэк
    // пользователя 2026-08-10) — тот же уровень z-order, что панель
    // пресетов: модальный список действий, не редактор конкретного
    // стикера (в отличие от `window_picker` выше).
    if let Some(state) = &edit.window_pick_list {
        if state.monitor_id == *monitor_id {
            let mut prims = Vec::new();
            state.panel.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Модал подтверждения — самый верх (раздел 11), только на мониторе,
    // открывшем удаление (M3).
    if let Some(confirm) = &edit.confirm {
        if confirm.monitor_id == *monitor_id {
            let mut prims = Vec::new();
            confirm.panel.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Баннер предупреждений (решение координатора 2026-08-18: конфликт
    // хоткея и прочие критические сообщения — вместо невидимого на
    // Win11 25H2 tray-баллуна) — поверх всего, на своём мониторе, у
    // верхнего края по центру. Панель + надпись — те же виджеты, что у
    // остального UI; размер по тексту (`Label` сам считает ширину).
    if let Some(banner) = &edit.banner {
        if banner.monitor_id == *monitor_id {
            let screen_w = width_px as f64 / scale as f64;
            let (tw, th) = rst_render::text_size(&banner.text);
            let banner_frame = Box2D {
                cx: screen_w / 2.0,
                cy: BANNER_TOP_GAP + BANNER_HEIGHT / 2.0,
                w: tw + 2.0 * BANNER_PAD,
                h: BANNER_HEIGHT,
                rotation: 0.0,
            };
            let mut panel = Panel::new(BANNER_PANEL_ID, banner_frame);
            panel.add_widget(Label::new(
                0,
                banner_frame.cx - tw / 2.0,
                banner_frame.cy + (BANNER_HEIGHT - th) / 2.0,
                &banner.text,
            ));
            let mut prims = Vec::new();
            panel.draw(&mut prims);
            primitives_to_sprites(
                &prims, ui_cache, renderer, monitor_id, text_scale, &mut frame,
            );
        }
    }

    // Маска перекрытия выключена в режиме редактирования (M4_OCCLUDERS_
    // DESIGN.md §6: стикер должен оставаться полностью видимым и
    // интерактивным, пока его двигают/крутят, независимо от окон под ним) и
    // когда на этом мониторе нет ни одного видимого стикера с активной
    // группой окклюдеров — тогда обычный `draw()` не отличается от
    // `draw_masked()` с пустыми масками, но дешевле (не строит текстуры).
    // Пока пользователь тащит закреплённое окно, такт кадра задаёт ожидание
    // композиции ПЕРЕД сборкой (`redraw_all` → `wait_for_composition`), а не
    // `Present`: иначе геометрия, прочитанная до ожидания, показывается на
    // кадр позже и графика поверх окна отстаёт (замер 2026-08-21: около
    // 16.5 мс на 60 Гц). В остальное время такт задаёт сам `Present(1)`.
    let sync = if pin_follow_active(edit) {
        PresentSync::Immediate
    } else {
        PresentSync::VSync
    };
    let draw_result = if edit.active || sticker_mask_slots.is_empty() {
        renderer.draw(&frame, sync)
    } else {
        // Одна GPU-текстура маски на ГРУППУ окклюдеров (не на стикер) —
        // строится заново каждый вызов `redraw`, а не кэшируется вместе с
        // топологией (`occluder_cache` в `run()`): рендер event-driven
        // (ADR-006), `redraw` и так вызывается только на реальные события,
        // а свежий размер текстуры маски гарантированно совпадает с
        // текущим размером цели монитора (пересчитанный на смене DPI/
        // ресайза кэш топологии мог бы держать маску старого размера).
        let mut group_textures: HashMap<usize, Texture> = HashMap::new();
        let mut device_lost_building_mask = false;
        let mut mask_build_failed = false;
        if let Some(groups) = occluders {
            for &(_, group_idx) in &sticker_mask_slots {
                if group_textures.contains_key(&group_idx) {
                    continue;
                }
                let build = renderer
                    .device
                    .create_mask_texture(width_px, height_px)
                    .and_then(|tex| {
                        renderer.device.draw_mask(&tex, &groups[group_idx].rects)?;
                        Ok(tex)
                    });
                match build {
                    Ok(tex) => {
                        group_textures.insert(group_idx, tex);
                    }
                    Err(e) => {
                        let device_lost = matches!(e, RenderError::DeviceLost(_));
                        tracing::warn!(
                            error = %e,
                            device_lost,
                            "не удалось построить маску перекрытия для группы окклюдеров"
                        );
                        if device_lost {
                            device_lost_building_mask = true;
                            break;
                        }
                        mask_build_failed = true;
                    }
                }
            }
        }
        if device_lost_building_mask {
            return true;
        }
        // Отказ построить хотя бы одну маску — рисуем кадр целиком без масок
        // (стикеры этой группы на один кадр останутся полностью видимыми),
        // а не роняем весь кадр: та же «приемлемая деградация», что и у
        // прочих несмертельных ошибок текстур в этой функции.
        let mut masks: Vec<Option<&Texture>> = vec![None; frame.len()];
        if !mask_build_failed {
            for &(idx, group_idx) in &sticker_mask_slots {
                masks[idx] = group_textures.get(&group_idx);
            }
        }
        renderer.draw_masked(&frame, &masks, sync)
    };
    if let Err(e) = draw_result {
        let device_lost = matches!(e, RenderError::DeviceLost(_));
        tracing::warn!(error = %e, device_lost, "не удалось отрисовать кадр");
        return device_lost;
    }
    false
}

/// Отрисовать кадр на всех мониторах. Устройство общее — если оно потеряно
/// (`RenderError::DeviceLost`), это верно для всех целей сразу, поэтому при
/// первом же таком сигнале перебор останавливается (дорисовывать остальные
/// мониторы мёртвым устройством бессмысленно) и вызывающему сообщается, что
/// нужно восстановление (`recover_device`).
#[allow(clippy::too_many_arguments)]
fn redraw_all(
    device: &Device,
    monitors_map: &mut HashMap<MonitorId, MonitorState>,
    sprites: &[(Uuid, Sprite)],
    cfg: &Config,
    edit: &EditState,
    white_tex: &Texture,
    black_tex: &Texture,
    ui_cache: &mut UiTextureCache,
    occluder_cache: &HashMap<MonitorId, Vec<OccluderSet>>,
    window_snapshot: &[WindowInfo],
    monitor_bounds: &HashMap<MonitorId, MonitorBounds>,
) -> bool {
    // Якорь такта для кадров, идущих вровень с чужим окном: ждём границу
    // композиции ЗДЕСЬ, чтобы живые границы окна читались уже после неё —
    // см. `rst_win32::dwm::wait_for_composition` и `PresentSync::Immediate`.
    if pin_follow_active(edit) {
        rst_win32::dwm::wait_for_composition();
    }
    for (monitor_id, ms) in monitors_map.iter_mut() {
        // Устаревшая цель на уже уничтоженном устройстве, которую не
        // удалось пересоздать при последнем восстановлении — не рисуем: её
        // `present` гарантированно вернёт `DeviceLost` заново и превратит
        // каждое следующее redraw-событие в полное повторное восстановление
        // (docs/M3_DEVICE_RECOVERY_REVIEW.md, пункт 2.1). Ждёт следующего
        // успешного `recover_device` для этого монитора.
        if ms.broken {
            continue;
        }
        let mut renderer = Renderer {
            device,
            target: &mut ms.target,
        };
        let device_lost = redraw(
            &mut renderer,
            sprites,
            cfg,
            edit,
            white_tex,
            black_tex,
            ui_cache,
            ms.width,
            ms.height,
            ms.scale,
            monitor_id,
            occluder_cache.get(monitor_id).map(Vec::as_slice),
            window_snapshot,
            monitor_bounds,
        );
        if device_lost {
            return true;
        }
    }
    false
}

/// Восстановить D3D11-устройство и все его GPU-ресурсы после потери
/// (`RenderError::DeviceLost` — сон, смена драйвера, TDR; ARCHITECTURE.md
/// раздел 11): без этого оверлей молча оставался бы чёрным/замороженным
/// навсегда — устройство никогда не пересоздавалось, а `redraw` логировал бы
/// одну и ту же ошибку на каждый кадр. Пересоздаёт устройство, цель каждого
/// монитора на нём же (сохраняя её DPI-масштаб), заливки и текстуры всех
/// стикеров — старые GPU-ресурсы принадлежали уничтоженному устройству и уже
/// недействительны; UI-кэш растров тоже сбрасывается по той же причине.
/// Возвращает `false`, если пересоздать само устройство не удалось — тогда
/// рисовать больше нечем, и это уже логируется как ошибка внутри.
#[allow(clippy::too_many_arguments)]
fn recover_device(
    device: &mut Device,
    monitors_map: &mut HashMap<MonitorId, MonitorState>,
    white_tex: &mut Texture,
    black_tex: &mut Texture,
    sprites: &mut Vec<(Uuid, Sprite)>,
    cfg: &Config,
    ui_cache: &mut UiTextureCache,
    animations: &mut HashMap<Uuid, StickerAnimation>,
    videos: &mut HashMap<Uuid, VideoPlayback>,
    audio_mixer: Option<&AudioMixer>,
) -> bool {
    tracing::warn!("D3D-устройство потеряно — пересоздаю устройство и все GPU-ресурсы");
    let new_device = match Device::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "не удалось пересоздать D3D11-устройство после потери");
            return false;
        }
    };
    for (id, ms) in monitors_map.iter_mut() {
        match WindowTarget::new(&new_device, ms.overlay.hwnd(), ms.width, ms.height) {
            Ok(target) => {
                ms.target = target;
                ms.broken = false;
            }
            Err(e) => {
                // Оставляем старую (устаревшую) цель как есть — трогать её
                // незачем, `redraw_all` теперь пропускает монитор целиком по
                // флагу `broken`, не пытаясь рисовать в неё и не порождая
                // второй `DeviceLost` (docs/M3_DEVICE_RECOVERY_REVIEW.md,
                // пункт 2.1). Следующая потеря устройства (или ручной
                // перезапуск) — единственный способ снова попробовать.
                tracing::error!(error = %e, monitor = %id.0, "не удалось пересоздать цель рендера монитора после потери устройства — окно исключено из отрисовки");
                ms.broken = true;
                continue;
            }
        }
        ms.target.set_dpi_scale(ms.scale);
    }
    match new_device.create_texture_from_rgba(&[0xff, 0xff, 0xff, 0xff], 1, 1) {
        Ok(t) => *white_tex = t,
        Err(e) => tracing::error!(error = %e, "не удалось пересоздать текстуру рамки выделения"),
    }
    match new_device.create_texture_from_rgba(&[0x00, 0x00, 0x00, 0xff], 1, 1) {
        Ok(t) => *black_tex = t,
        Err(e) => tracing::error!(error = %e, "не удалось пересоздать текстуру затемнения"),
    }
    // Стикер, чья текстура здесь не перезагрузилась, просто отсутствует в
    // `sprites` и не рисуется — без явного ретрая, до ближайшего триггера
    // `resync_sprites` (undo/redo, жест, удаление, дублирование — он
    // пересоздаёт спрайты из cfg заново). Приемлемая деградация: путь и так
    // залогирован, а `resync_sprites` в этом сеансе обычно случается скоро
    // (docs/M3_DEVICE_RECOVERY_REVIEW.md, пункт 2.2).
    //
    // Анимации (M5a) пересобираются здесь целиком заново — `animations`
    // держал атлас на СТАРОМ (потерянном) устройстве, его `Texture` мертва
    // вместе с ним. Фаза анимации (текущий кадр) намеренно НЕ сохраняется
    // через потерю устройства — редкое событие (сброс GPU-драйвера),
    // рестарт с кадра 0 неотличим на глаз от короткого сбоя рендера.
    //
    // Видео (M5b) — иначе: декодер-поток (`VideoSource`) и звук
    // (`AudioSource`) живут независимо от D3D-устройства и потерю не
    // замечают вообще, `videos` НЕ чистится — только GPU-текстуры кадра
    // (`VideoTextures`) пересоздаются на новом устройстве, файл не
    // переоткрывается (в отличие от атласа анимации, у видео нет
    // фиксированного набора кадров, которые можно перезалить один раз;
    // переоткрытие потеряло бы текущую позицию воспроизведения без нужды).
    sprites.clear();
    animations.clear();
    videos.retain(|id, _| cfg.stickers.iter().any(|s| s.id == *id));
    for sticker in &cfg.stickers {
        if let Some((sprite, anim)) = load_sticker_sprite(&new_device, sticker) {
            sprites.push((sticker.id, sprite));
            animations.insert(sticker.id, anim);
        } else if let Some(playback) = videos.get(&sticker.id) {
            let (w, h) = playback.source.dimensions();
            match blank_video_textures(&new_device, w, h) {
                Ok(textures) => {
                    let sprite = Sprite::new(
                        textures.y.clone(),
                        sticker.placement.clone(),
                        sticker.transform,
                    )
                    .with_video(textures);
                    sprites.push((sticker.id, sprite));
                }
                Err(e) => {
                    tracing::error!(error = %e, sticker = %sticker.id, "не удалось пересоздать видеотекстуры после потери устройства");
                }
            }
        } else if let Some((sprite, playback)) =
            load_sticker_video(&new_device, sticker, audio_mixer)
        {
            sprites.push((sticker.id, sprite));
            videos.insert(sticker.id, playback);
        } else if let Some(sprite) = load_static_sprite(&new_device, sticker) {
            sprites.push((sticker.id, sprite));
        }
    }
    *ui_cache = UiTextureCache::new();
    *device = new_device;
    true
}

/// Монитор, физически содержащий центр `rect` (глобальные физические px,
/// как `WindowInfo::rect`) — какой монитор передать в
/// `window_rect_to_placement` при закреплении окна (M6, фидбэк
/// пользователя 2026-08-10): список окон (`window_pick_list.rs`) может
/// быть открыт на ДРУГОМ мониторе, чем окно, которое выбирает
/// пользователь, — в отличие от прежнего режима наведения (где курсор был
/// физически над целью), нельзя просто взять монитор панели. `None` —
/// окно не найдено ни в одном известном мониторе (редкий разрыв
/// многомониторной геометрии — окно на мониторе, который только что
/// отключили, снимок ещё не обновился).
fn monitor_for_window_rect<'a>(
    rect: &WindowRect,
    monitor_bounds: &'a HashMap<MonitorId, MonitorBounds>,
) -> Option<&'a MonitorId> {
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    monitor_bounds
        .values()
        .find(|b| {
            cx >= b.bounds_px.x
                && cx < b.bounds_px.x + b.bounds_px.w as i32
                && cy >= b.bounds_px.y
                && cy < b.bounds_px.y + b.bounds_px.h as i32
        })
        .map(|b| &b.id)
}

/// Экранный rect окна (физические px виртуального десктопа, как
/// `WindowInfo::rect`) → DIP-`Placement`, локальный монитору `monitor_id`
/// (SPEC.md «Закрепление окна»): выделение/хит-тест/ручки ресайза
/// закреплённого окна используют ту же геометрию (`SelectionBox`,
/// `hittest::contains`), что и стикеры, но `PinnedWindow` не хранит
/// `Placement` — она считается заново из живого снимка трекера на каждый
/// кадр/клик. Обратная операция — [`placement_to_physical_rect`].
fn window_rect_to_placement(
    rect: &WindowRect,
    monitor_id: MonitorId,
    bounds: &MonitorBounds,
) -> Placement {
    let scale = bounds.scale;
    let w = rect.w as f64 / scale;
    let h = rect.h as f64 / scale;
    Placement {
        monitor_id,
        cx: (rect.x - bounds.bounds_px.x) as f64 / scale + w / 2.0,
        cy: (rect.y - bounds.bounds_px.y) as f64 / scale + h / 2.0,
        w,
        h,
    }
}

/// Обратная операция к [`window_rect_to_placement`]: DIP-`Placement`
/// закреплённого окна → физический rect `(x, y, w, h)` для
/// `WindowPins::move_resize` (SPEC «Закрепление окна», перемещение/ресайз
/// через ручки). Округление до целого физического px — та же точность,
/// что у координат мыши.
fn placement_to_physical_rect(placement: &Placement, bounds: &MonitorBounds) -> (i32, i32, i32, i32) {
    let scale = bounds.scale;
    let w = (placement.w * scale).round() as i32;
    let h = (placement.h * scale).round() as i32;
    let x = ((placement.cx - placement.w / 2.0) * scale).round() as i32 + bounds.bounds_px.x;
    let y = ((placement.cy - placement.h / 2.0) * scale).round() as i32 + bounds.bounds_px.y;
    (x, y, w, h)
}

/// Сообщить main.rs, что `cfg.presets` изменился (для пересборки меню трея
/// — M7 «быстрое переключение из трея», ROADMAP.md). Вызывается после
/// успешных `SavePreset`/`RenamePreset`/`DeletePreset`/`ImportPreset` —
/// `ApplyPreset` сам список не меняет, поэтому его не трогает.
fn notify_presets_changed(cfg: &Config, coordinator_tx: &Sender<CoordinatorRequest>) {
    let list = cfg.presets.iter().map(|p| (p.id, p.name.clone())).collect();
    let _ = coordinator_tx.send(CoordinatorRequest::PresetsChanged(list));
}

/// Добавить стикер из файла на диске. `pasted` — источник
/// [`StickerSource::Pasted`] вместо [`StickerSource::File`] (материализованная
/// вставка из буфера, SPEC 2.1/2.5 — удаляется вместе с файлом,
/// `cleanup_pasted_file`); обычные файлы (диалог, `CF_HDROP`) — `false`.
/// Возвращает `true`, если стикер реально добавлен — вызывающий код решает
/// по этому флагу, стоит ли коммитить снимок undo (docs/M2_SLICE4_REVIEW.md,
/// пункт 7: неудачная загрузка не должна создавать пустой шаг истории).
#[allow(clippy::too_many_arguments)]
fn add_sticker(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    path: PathBuf,
    pasted: bool,
    scale: f32,
    monitor_id: &MonitorId,
    audio_mixer: Option<&AudioMixer>,
) -> bool {
    // Видео (M5b, docs/M5B_VIDEO_DESIGN.md §6) — отдельная ветка целиком, до
    // попытки декодировать как изображение/анимацию, и решается по
    // расширению (у видеоконтейнеров нет общего с изображениями декодера,
    // который мог бы просто вернуть `None`, как для анимации). Вставка из
    // буфера (`pasted`) никогда не несёт видео — `ClipboardImage` их не
    // возвращает, `CF_HDROP`-пути тоже отфильтрованы `is_supported_image`
    // раньше, чем дойти сюда.
    if !pasted {
        let is_video = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(ext)));
        if is_video {
            return add_video_sticker(
                overlay,
                renderer,
                cfg,
                config_path,
                sprites,
                edit,
                path,
                scale,
                monitor_id,
                audio_mixer,
            );
        }
    }
    // Анимация — только для обычных файлов (M5a, docs/M5A_ANIMATION_DESIGN.md
    // §5): вставка из буфера (`pasted`) материализует растровые данные
    // (CF_DIBV5) как одиночный PNG в `StickerSource::Pasted`, который вообще
    // не несёт `MediaType` — там анимации в принципе быть не может.
    // Вставка из буфера (`pasted`) никогда не несёт анимацию (см. коммент
    // выше) — decode_animation вообще не вызывается, `None` без похода на
    // диск повторно. Иначе — реальный результат decode_animation: `Ok` с
    // >= 2 кадрами — атлас; `TooManyFrames`/`TooLargeForAtlas` — потоковый
    // режим (ROADMAP.md M5a); остальное — статика.
    let animation = if pasted {
        None
    } else {
        Some(media_animation::decode_animation(&path))
    };

    let (texture, uv_offset, uv_scale, pending) = match animation {
        Some(Ok(anim)) if anim.frames.len() >= 2 => {
            let frames: Vec<(Vec<u8>, Duration)> =
                anim.frames.into_iter().map(|f| (f.rgba, f.delay)).collect();
            match renderer.create_texture_atlas(&frames, anim.width, anim.height) {
                Ok(atlas) => {
                    let f0 = atlas.frames[0];
                    (
                        atlas.texture.clone(),
                        f0.uv_offset,
                        f0.uv_scale,
                        Some(PendingAnimation::Atlas(atlas)),
                    )
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "не удалось собрать атлас анимации — загружаю как статичное изображение");
                    match load_static(renderer, &path) {
                        Some(t) => (t, [0.0, 0.0], [1.0, 1.0], None),
                        None => return false,
                    }
                }
            }
        }
        Some(Err(
            media_animation::MediaError::TooManyFrames { .. }
            | media_animation::MediaError::TooLargeForAtlas { .. },
        )) => match open_streaming_animation(renderer.device, &path) {
            Some((texture, source, first_delay)) => (
                texture.clone(),
                [0.0, 0.0],
                [1.0, 1.0],
                Some(PendingAnimation::Streaming {
                    source,
                    texture,
                    first_delay,
                }),
            ),
            None => match load_static(renderer, &path) {
                Some(t) => (t, [0.0, 0.0], [1.0, 1.0], None),
                None => return false,
            },
        },
        _ => match load_static(renderer, &path) {
            Some(t) => (t, [0.0, 0.0], [1.0, 1.0], None),
            None => return false,
        },
    };
    let media_type = if pending.is_some() {
        MediaType::Animation
    } else {
        MediaType::Image
    };
    // Для анимации (M5a) `texture` — весь атлас (сетка кадров), не размер
    // одного кадра; `uv_scale` — доля атласа на кадр, домножение даёт
    // настоящий натуральный размер (для статики `uv_scale == [1.0, 1.0]`,
    // no-op). Раньше стикер заводился растянутым на N кадров по ширине/
    // высоте — найдено независимым ревью сшивки окна настроек при
    // проверке `sticker_natural_size`, тот же баг был и здесь.
    let (w, h) = (
        texture.width() as f64 * uv_scale[0] as f64,
        texture.height() as f64 * uv_scale[1] as f64,
    );
    // `placement` — DIP, а `overlay.size()` — физические пиксели живого
    // GetWindowRect (M3): нужно делить на масштаб, иначе на не-100% DPI
    // центр вставки уходит от реального центра экрана
    // (docs/M3_STEP2_3_REVIEW.md, пункт 2.3).
    let (screen_w, screen_h) = overlay.size();
    let (center_x, center_y) = (
        screen_w as f64 / scale as f64 / 2.0,
        screen_h as f64 / scale as f64 / 2.0,
    );
    // M3: реальный device interface path монитора, откуда пришло добавление
    // (перетаскивание/вставка/диалог на конкретном окне) — `monitor_id`.
    let sticker = if pasted {
        Sticker::new_pasted(path, monitor_id.clone(), center_x, center_y, w, h)
    } else {
        Sticker::new_file(
            path,
            media_type,
            monitor_id.clone(),
            center_x,
            center_y,
            w,
            h,
        )
    };
    let sprite = Sprite::new(texture, sticker.placement.clone(), sticker.transform)
        .with_uv(uv_offset, uv_scale);
    let id = sticker.id;
    cfg.stickers.push(sticker);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после добавления стикера");
    }
    sprites.push((id, sprite));
    // Часы анимации заводятся в `run()` (там живёт `animations`, локальная
    // переменная цикла, недоступная на этой глубине вызова) — см. доккомент
    // `EditState::pending_animation`.
    if let Some(pending) = pending {
        edit.pending_animation = Some((id, pending));
    }
    true
}

/// Добавить видеостикер (M5b, docs/M5B_VIDEO_DESIGN.md §6) — открытие
/// `VideoSource`/добавление `AudioSource` идёт до создания `Sticker`
/// (нужны реальные размеры кадра для `placement`), тем же порядком, что и
/// декодирование картинки/анимации в `add_sticker`. Видео с альфа-каналом
/// никогда не отклоняется (§0) — сам декодер альфа-плоскость не читает.
/// `VideoPlayback` кладётся в `edit.pending_video`, а не напрямую в `videos`
/// (локальная переменная цикла `run()`, недоступная на этой глубине вызова)
/// — тот же паттерн, что `pending_animation`.
#[allow(clippy::too_many_arguments)]
fn add_video_sticker(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    path: PathBuf,
    scale: f32,
    monitor_id: &MonitorId,
    audio_mixer: Option<&AudioMixer>,
) -> bool {
    let opened = match audio_mixer {
        Some(m) => VideoSource::open_with_audio_target(&path, m.sample_rate(), m.channels()),
        None => VideoSource::open(&path),
    };
    let source = match opened {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось открыть видео");
            return false;
        }
    };
    let (w, h) = source.dimensions();
    let textures = match blank_video_textures(renderer.device, w, h) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось создать видеотекстуры");
            return false;
        }
    };
    let (screen_w, screen_h) = overlay.size();
    let (monitor_w, monitor_h) = (screen_w as f64 / scale as f64, screen_h as f64 / scale as f64);
    let (center_x, center_y) = (monitor_w / 2.0, monitor_h / 2.0);
    // Огромное видео (например портретный ролик выше монитора) иначе
    // вставилось бы в натуральном размере и вылезло бы за края экрана —
    // вписываем в разрешение монитора с запасом (rst_core::sizing).
    let (sticker_w, sticker_h) =
        rst_core::sizing::initial_media_size(w as f64, h as f64, monitor_w, monitor_h);
    let mut sticker = Sticker::new_file(
        path,
        MediaType::Video,
        monitor_id.clone(),
        center_x,
        center_y,
        sticker_w,
        sticker_h,
    );
    // По умолчанию звук выключен при добавлении видео — пользователь сам
    // включает громкость, если она нужна (иначе новый стикер сразу озвучен).
    sticker.playback.volume = 0.0;
    let id = sticker.id;
    let sprite = Sprite::new(
        textures.y.clone(),
        sticker.placement.clone(),
        sticker.transform,
    )
    .with_video(textures);
    // Новый стикер всегда со свежим `PlaybackSettings::default()` (играет),
    // кроме громкости — та обнулена явно чуть выше. `AudioSource` заводится
    // мьютексом со своим дефолтом (1.0, см. `rst_audio::mixer`), не читает
    // `sticker.playback` сам — громкость нужно применить явно, иначе звук
    // всё равно проиграется на полную.
    let audio = audio_mixer.map(|m| {
        let a = m.add_source(id);
        a.set_volume(0.0);
        a
    });
    cfg.stickers.push(sticker);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после добавления стикера");
    }
    sprites.push((id, sprite));
    edit.pending_video = Some((id, VideoPlayback { source, audio }));
    true
}

/// Загрузить статичное изображение, залогировав неудачу (общий хвост между
/// путём анимации, у которой не собрался атлас, и обычным путём — M5a).
fn load_static(renderer: &Renderer, path: &Path) -> Option<Texture> {
    match renderer.load_image(path) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось загрузить выбранное изображение");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::Widget as _;

    fn bounds(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    // --- coalesce_mouse_move: защита от бэклога безлимитного канала
    // (см. доккомент функции) ---

    fn move_msg(mid: &str, x: i32, y: i32) -> OverlayMessage {
        OverlayMessage::Event(
            MonitorId(mid.to_string()),
            OverlayEvent::Input(InputEvent::MouseMove {
                pos: rst_win32::input::Point { x, y },
                modifiers: Modifiers::default(),
                dragging: true,
            }),
        )
    }

    /// `OverlayMessage`/`OverlayCommand` не выводят `PartialEq` (утянуло бы
    /// его через `Settings`/`Hotkeys` и весь `Config` — не нужно нигде,
    /// кроме этих тестов) — сравниваем через паттерн-матчинг вместо
    /// `assert_eq!` на всё сообщение целиком.
    fn move_xy(msg: &OverlayMessage) -> Option<(i32, i32)> {
        match msg {
            OverlayMessage::Event(_, OverlayEvent::Input(InputEvent::MouseMove { pos, .. })) => {
                Some((pos.x, pos.y))
            }
            _ => None,
        }
    }

    fn move_monitor(msg: &OverlayMessage) -> Option<&str> {
        match msg {
            OverlayMessage::Event(mid, OverlayEvent::Input(InputEvent::MouseMove { .. })) => {
                Some(mid.0.as_str())
            }
            _ => None,
        }
    }

    #[test]
    fn coalesce_mouse_move_collapses_same_monitor_backlog_to_latest() {
        let (tx, rx) = mpsc::channel();
        tx.send(move_msg("main", 20, 20)).unwrap();
        tx.send(move_msg("main", 30, 30)).unwrap();
        tx.send(move_msg("main", 40, 40)).unwrap();
        let first = rx.recv().unwrap();
        let (coalesced, leftover) = coalesce_mouse_move(&rx, first);
        assert_eq!(
            move_xy(&coalesced),
            Some((40, 40)),
            "должна остаться только самая свежая позиция, а не первая из очереди"
        );
        assert!(leftover.is_none());
    }

    #[test]
    fn coalesce_mouse_move_leaves_non_move_message_as_leftover() {
        let (tx, rx) = mpsc::channel();
        tx.send(move_msg("main", 20, 20)).unwrap();
        tx.send(move_msg("main", 30, 30)).unwrap();
        tx.send(OverlayMessage::Tick).unwrap();
        let first = rx.recv().unwrap();
        let (coalesced, leftover) = coalesce_mouse_move(&rx, first);
        assert_eq!(move_xy(&coalesced), Some((30, 30)));
        assert!(
            matches!(leftover, Some(OverlayMessage::Tick)),
            "сообщение, прервавшее коалесинг, не должно теряться"
        );
    }

    #[test]
    fn coalesce_mouse_move_does_not_cross_monitors() {
        // MouseMove другого монитора не должен быть съеден коалесингом
        // текущего — иначе движение мыши на втором мониторе потерялось бы.
        let (tx, rx) = mpsc::channel();
        tx.send(move_msg("main", 20, 20)).unwrap();
        tx.send(move_msg("second", 99, 99)).unwrap();
        let first = rx.recv().unwrap();
        let (coalesced, leftover) = coalesce_mouse_move(&rx, first);
        assert_eq!(move_xy(&coalesced), Some((20, 20)));
        let leftover = leftover.expect("сообщение второго монитора не должно теряться");
        assert_eq!(move_monitor(&leftover), Some("second"));
        assert_eq!(move_xy(&leftover), Some((99, 99)));
    }

    #[test]
    fn coalesce_mouse_move_passes_through_non_move_first_message_untouched() {
        let (tx, rx) = mpsc::channel();
        // Даже если за Tick в очереди стоит MouseMove, коалесинг не должен
        // запускаться вовсе — дренаж выполняется только когда ПЕРВОЕ
        // сообщение само MouseMove (иначе Tick подавил бы реальный ход мыши).
        tx.send(move_msg("main", 20, 20)).unwrap();
        let (coalesced, leftover) = coalesce_mouse_move(&rx, OverlayMessage::Tick);
        assert!(matches!(coalesced, OverlayMessage::Tick));
        assert!(leftover.is_none());
        // MouseMove остаётся нетронутым в канале для следующей итерации.
        let still_queued = rx.recv().unwrap();
        assert_eq!(move_xy(&still_queued), Some((20, 20)));
    }

    #[test]
    fn coalesce_mouse_move_empty_queue_returns_first_unchanged() {
        let (_tx, rx) = mpsc::channel();
        let (coalesced, leftover) = coalesce_mouse_move(&rx, move_msg("main", 1, 1));
        assert_eq!(move_xy(&coalesced), Some((1, 1)));
        assert!(leftover.is_none());
    }

    #[test]
    fn monitor_geometry_changed_false_when_nothing_moved() {
        let cached = (0, 0, 1920, 1080, 1.0);
        assert!(!monitor_geometry_changed(
            cached,
            bounds(0, 0, 1920, 1080),
            1.0
        ));
    }

    #[test]
    fn monitor_geometry_changed_true_on_position_only() {
        // M3_STEP8_REVIEW.md, пункт 2.1: чистая перестановка монитора
        // (тот же размер и масштаб, другие x/y) должна считаться изменением.
        let cached = (0, 0, 1920, 1080, 1.0);
        assert!(monitor_geometry_changed(
            cached,
            bounds(1920, 0, 1920, 1080),
            1.0
        ));
    }

    #[test]
    fn monitor_geometry_changed_true_on_size_only() {
        let cached = (0, 0, 1920, 1080, 1.0);
        assert!(monitor_geometry_changed(
            cached,
            bounds(0, 0, 1280, 1024),
            1.0
        ));
    }

    #[test]
    fn monitor_geometry_changed_true_on_scale_only() {
        let cached = (0, 0, 1920, 1080, 1.0);
        assert!(monitor_geometry_changed(
            cached,
            bounds(0, 0, 1920, 1080),
            1.5
        ));
    }

    #[test]
    fn monitor_geometry_changed_false_on_negative_coords_matching() {
        // Неосновной монитор слева/сверху от primary — x/y отрицательные
        // (виртуальный десктоп), сравнение должно оставаться точным.
        let cached = (-1920, -200, 1920, 1080, 1.0);
        assert!(!monitor_geometry_changed(
            cached,
            bounds(-1920, -200, 1920, 1080),
            1.0
        ));
    }

    #[test]
    fn onboarding_notification_shown_on_fresh_config_with_default_hotkey() {
        let cfg = Config::default();
        let (title, body) = onboarding_notification(&cfg).expect("свежий конфиг ещё не показывал");
        assert_eq!(title, "resticker запущен");
        assert!(
            body.contains("Ctrl+Alt+S"),
            "тело должно содержать дефолтный хоткей: {body}"
        );
    }

    #[test]
    fn onboarding_notification_uses_configured_hotkey_when_customized() {
        let mut cfg = Config::default();
        cfg.hotkeys.edit_mode = Some("Ctrl+Shift+E".to_string());
        let (_, body) = onboarding_notification(&cfg).expect("ещё не показывали");
        assert!(
            body.contains("Ctrl+Shift+E"),
            "тело должно содержать настроенный хоткей, а не дефолт: {body}"
        );
    }

    #[test]
    fn onboarding_notification_falls_back_to_default_when_hotkey_unset() {
        let mut cfg = Config::default();
        cfg.hotkeys.edit_mode = None;
        let (_, body) = onboarding_notification(&cfg).expect("ещё не показывали");
        assert!(body.contains(DEFAULT_EDIT_HOTKEY));
    }

    #[test]
    fn onboarding_notification_none_once_already_shown() {
        let mut cfg = Config::default();
        cfg.settings.onboarding_shown = true;
        assert!(onboarding_notification(&cfg).is_none());
    }

    // --- M4: refresh_occlusion / occluder_rects_for / конвертеры ---

    fn monitor_id(s: &str) -> MonitorId {
        MonitorId(s.to_string())
    }

    fn sticker_with_visibility(
        monitor: &str,
        mode: VisibilityMode,
        rules: Vec<OverlapRule>,
    ) -> Sticker {
        Sticker {
            id: Uuid::new_v4(),
            placement: Placement {
                monitor_id: monitor_id(monitor),
                ..Default::default()
            },
            visibility: rst_core::model::VisibilityRule { mode, rules },
            ..Sticker::default()
        }
    }

    // --- resolve_zone / курсор поворота: зона СНАРУЖИ рамки, отступ от угла,
    // угол считается от реального направления угла в пространстве (фидбэк
    // пользователя 2026-08-09, третий раунд) ---

    /// Стикер 100×100 DIP с центром (200,200) без поворота — рамка выделения
    /// x∈[150,250], y∈[150,250].
    fn zone_test_sticker(rotation: f64) -> Sticker {
        Sticker {
            id: Uuid::new_v4(),
            placement: Placement {
                monitor_id: monitor_id("main"),
                cx: 200.0,
                cy: 200.0,
                w: 100.0,
                h: 100.0,
            },
            transform: Transform {
                rotation,
                ..Transform::default()
            },
            ..Sticker::default()
        }
    }

    fn zone_test_config(sticker: Sticker) -> (Config, SelectionSet, Uuid) {
        let id = sticker.id;
        let mut cfg = Config::default();
        cfg.stickers.push(sticker);
        let mut selection = SelectionSet::default();
        selection.click(Some(id));
        (cfg, selection, id)
    }

    // --- tracker_mask_needed: гейт хуков трекера окон должен оставаться
    // включённым, пока открыт режим прицела «добавить окно» (M6,
    // BTN_ADD_WINDOW), даже когда ни у одного стикера нет правила видимости,
    // зависящего от окклюдеров (репорт пользователя «кнопка прикрепления
    // окна вообще не работает», 2026-08-10) ---

    /// Минимальный `EditState` для теста гейта: все опциональные режимы
    /// выключены, `window_pick_list`/`window_picker` — единственное, что
    /// варьируют тесты этой секции.
    fn mask_gate_edit_state() -> EditState {
        let (coordinator_tx, _rx) = mpsc::channel();
        EditState {
            active: true,
            selection: SelectionSet::new(),
            gesture: None,
            snap: SnapConfig::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending_snapshot: None,
            confirm: None,
            window_picker: None,
            preset_picker: None,
            pending_open_picker: None,
            pending_open_pick_list: None,
            window_pick_list: None,
            pinned_windows: Vec::new(),
            pinned_selection: None,
            pinned_panel: None,
            pinned_gesture: None,
            surfaced_pins: HashSet::new(),
            pin_flashes: Vec::new(),
        pinned_last_rects: HashMap::new(),
        pinned_unmaximized_at: HashMap::new(),
        pinned_follow_until: None,
            banner: None,
            pending_animation: None,
            pending_video: None,
            marquee: None,
            marquee_started: false,
            toolbar: None,
            cursor_panel: None,
            tooltip: None,
            pointer_owner: PointerOwner::None,
            ui_pending_snapshot: None,
            cursor_pos: (0.0, 0.0),
            cursor_monitor: monitor_id("main"),
            coordinator_tx,
        }
    }

    #[test]
    fn tracker_mask_needed_false_on_clean_config_and_idle_edit() {
        // Ни одного стикера, никакой UI не открыт — трекеру спать.
        let cfg = Config::default();
        let edit = mask_gate_edit_state();
        assert!(!tracker_mask_needed(&cfg, &edit));
    }

    #[test]
    fn tracker_mask_needed_true_while_window_pick_list_open() {
        // Это ровно баг из репорта: `mask_needed(cfg)` одна не видит, что
        // список BTN_ADD_WINDOW нуждается в живом снимке — без
        // `window_pick_list` в гейте трекер не просыпается, и список
        // остаётся пустым независимо от реально открытых окон.
        let cfg = Config::default();
        let mut edit = mask_gate_edit_state();
        edit.window_pick_list = Some(WindowPickListState {
            purpose: PickListPurpose::PinWindow,
            panel: Panel::new(
                window_pick_list::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
            scroll: 0,
        });
        assert!(tracker_mask_needed(&cfg, &edit));
    }

    #[test]
    fn toggle_edit_mode_exit_path_clears_leaked_session_panels() {
        // Багфикс-раунд 2, задача D: `window_pick_list`/`pending_open_pick_list`/
        // `preset_picker` не сбрасывались при переключении режима (в отличие
        // от своих сестёр `window_picker`/`pending_open_picker`/`confirm`), а
        // `redraw` рисует эти панели по одному `Some(...)`, без гейта на
        // `edit.active` — после выхода из режима панель застревала в центре
        // экрана мёртвым клик-прозрачным UI («панель застряла, ничего с ней
        // не сделать»). Полный «путь выхода» `toggle_edit_mode` — это
        // `reset_edit_mode_panels(edit, exiting: true)`.
        let mut edit = mask_gate_edit_state();
        edit.pending_open_pick_list = Some((monitor_id("main"), PickListPurpose::PinWindow));
        edit.window_pick_list = Some(WindowPickListState {
            purpose: PickListPurpose::PinWindow,
            panel: Panel::new(
                window_pick_list::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
            scroll: 0,
        });
        edit.preset_picker = Some(PresetPickerState {
            panel: Panel::new(
                preset_picker::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
        });
        // Селект-состояние пинов — отдельная часть «пути выхода».
        edit.pinned_selection = Some(42);
        edit.pinned_panel = Some(PinnedPanelState {
            host_rules: 0,
            hwnd: 42,
            panel: Panel::new(
                window_pick_list::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
            scroll: 0,
        });
        edit.selection.select(Uuid::new_v4());

        reset_edit_mode_panels(&mut edit, true);

        assert!(
            edit.window_pick_list.is_none(),
            "список окон для закрепления должен закрыться на выходе из режима"
        );
        assert!(
            edit.pending_open_pick_list.is_none(),
            "отложенный флаг открытия списка должен сброситься на выходе из режима"
        );
        assert!(
            edit.preset_picker.is_none(),
            "панель пресетов должна закрыться на выходе из режима"
        );
        assert!(edit.pinned_selection.is_none());
        assert!(edit.pinned_panel.is_none());
        assert!(edit.selection.is_empty());
    }

    #[test]
    fn toggle_edit_mode_enter_path_preserves_pin_selection_but_clears_panels() {
        // «Вход» в режим (`exiting: false`) сбрасывает панели/модалы сеанса
        // (те же, что и на выходе, — они не переживают переключение режима),
        // но НЕ трогает селект-состояние пинов и выделение: это часть только
        // выхода. Защита от «слишком усердной» очистки — панели не должны
        // исчезать чаще, чем раньше.
        let mut edit = mask_gate_edit_state();
        edit.window_pick_list = Some(WindowPickListState {
            purpose: PickListPurpose::PinWindow,
            panel: Panel::new(
                window_pick_list::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
            scroll: 0,
        });
        edit.preset_picker = Some(PresetPickerState {
            panel: Panel::new(
                preset_picker::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
        });
        let selected = Uuid::new_v4();
        edit.selection.select(selected);
        edit.pinned_selection = Some(42);
        edit.pinned_panel = Some(PinnedPanelState {
            host_rules: 0,
            hwnd: 42,
            panel: Panel::new(
                window_pick_list::PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            monitor_id: monitor_id("main"),
            scroll: 0,
        });

        reset_edit_mode_panels(&mut edit, false);

        assert!(edit.window_pick_list.is_none());
        assert!(edit.preset_picker.is_none());
        assert!(
            edit.selection.contains(selected),
            "вход в режим не должен снимать выделение"
        );
        assert_eq!(edit.pinned_selection, Some(42));
        assert!(edit.pinned_panel.is_some());
    }

    #[test]
    fn tracker_mask_needed_true_while_window_picker_panel_open() {
        // Прежний случай (M4) — панель «какие окна видимы» у стикера должна
        // и дальше держать трекер живым независимо от нового условия.
        let cfg = Config::default();
        let mut edit = mask_gate_edit_state();
        edit.window_picker = Some(WindowPickerState {
            sticker_id: Uuid::new_v4(),
            panel: Panel::new(
                window_picker::PICKER_PANEL_ID,
                Box2D {
                    cx: 0.0,
                    cy: 0.0,
                    w: 1.0,
                    h: 1.0,
                    rotation: 0.0,
                },
            ),
            scroll: 0,
            monitor_id: monitor_id("main"),
        });
        assert!(tracker_mask_needed(&cfg, &edit));
    }

    #[test]
    fn tracker_mask_needed_true_when_sticker_has_non_always_visibility() {
        // Существующий путь через `mask_needed(cfg)` не должен сломаться
        // рефакторингом в `tracker_mask_needed`.
        let mut cfg = Config::default();
        let mut sticker = zone_test_sticker(0.0);
        sticker.visibility.mode = VisibilityMode::Desktop;
        cfg.stickers.push(sticker);
        let edit = mask_gate_edit_state();
        assert!(tracker_mask_needed(&cfg, &edit));
    }

    #[test]
    fn tracker_mask_needed_true_while_any_window_pinned() {
        // Живой репорт пользователя, 2026-08-17: на чистом конфиге (ни
        // одного стикера, ни одной открытой панели) трекер обязан оставаться
        // живым, пока есть хоть одно закреплённое окно — иначе
        // `maintain_pinned_windows` (снос уничтоженных таргетов, z-order-
        // слоты, move-lock) перестаёт получать свежий `window_snapshot`
        // сразу же, как закрывается панель/список, которым трекер был
        // разбужен для самого пина.
        let cfg = Config::default();
        let mut edit = mask_gate_edit_state();
        edit.pinned_windows.push(PinnedWindow::new(12345));
        assert!(tracker_mask_needed(&cfg, &edit));
    }

    // --- pin_window: пин-флоу через список выбора (SPEC.md «Закрепление
    // окна»). Снимок и hover берутся 1:1 из веток `handle_input` (MouseMove/
    // MouseDown) — здесь сквозной сценарий на реальном окне, без D3D-
    // пластика, тем же приёмом, что drag_sim_*.

    fn pin_flow_harness(
    ) -> (Config, PathBuf, Vec<WindowInfo>, HashMap<MonitorId, MonitorBounds>, DragSimWindow) {
        let wnd = DragSimWindow::create();
        let hwnd = wnd.0.0 as usize;
        let snapshot = vec![WindowInfo {
            hwnd,
            rect: WindowRect {
                x: 10,
                y: 20,
                w: 300,
                h: 200,
            },
            pid: std::process::id() + 1, // «чужое» окно — своё window_at отфильтрует
            exe_path: std::env::current_exe().expect("путь к exe теста"),
            z_order: 0,
            ..Default::default()
        }];
        let mut monitor_bounds = HashMap::new();
        monitor_bounds.insert(
            monitor_id("main"),
            MonitorBounds {
                id: monitor_id("main"),
                bounds_px: bounds(0, 0, 1920, 1080),
                scale: 1.0,
            },
        );
        let config_path = std::env::temp_dir().join(format!(
            "resticker_pin_flow_{}_{}.json",
            std::process::id(),
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_file(&config_path);
        (Config::default(), config_path, snapshot, monitor_bounds, wnd)
    }

    // --- pin_window: новый рантайм-пин (редизайн пинов, SPEC.md
    // «Закрепление окна») — сквозной сценарий на реальном окне, без
    // персистентного стикера.

    #[test]
    fn pin_window_pins_and_records_runtime_state_without_config() {
        let (cfg, config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        let pinned = pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd);

        assert!(pinned, "живое окно обязано закрепиться");
        assert!(window_pins.is_pinned(hwnd), "окно закреплено через WindowPins");
        assert_eq!(
            edit.pinned_windows,
            vec![PinnedWindow::new(hwnd as isize)],
            "запись в рантайм-реестре — дефолт: full topmost, без замков, без соседей"
        );
        assert_eq!(edit.pin_flashes.len(), 1, "пин запускает пульс рамки");
        assert!(cfg.stickers.is_empty(), "никакого Sticker в cfg.stickers");
        assert!(
            !config_path.exists(),
            "config::save не вызывался — ничего не персистится на диск"
        );
    }

    #[test]
    fn pin_window_second_click_same_window_is_noop() {
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        assert!(
            !pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd),
            "повторный клик по уже закреплённому — no-op"
        );
        assert_eq!(
            edit.pinned_windows.len(),
            1,
            "повторный клик не дублирует запись в реестре"
        );
    }

    #[test]
    fn pin_window_missing_from_snapshot_is_noop() {
        // Окно исчезло из СВЕЖЕГО снимка (закрыто между открытием списка и
        // кликом) — guard `find(|w| w.hwnd == hwnd)` возвращается до
        // каких-либо действий.
        let (_cfg, _config_path, mut snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        snapshot.retain(|w| w.hwnd != hwnd);
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(!pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        assert!(edit.pinned_windows.is_empty(), "реестр не тронут");
        assert!(
            !window_pins.is_pinned(hwnd),
            "окно живо, но пин не вызывался (снимок его не видит)"
        );
    }

    #[test]
    fn pin_window_destroyed_between_snapshot_and_click_is_noop() {
        // Настоящая гонка «окно исчезло между построением снимка списка и
        // кликом по нему»: снимок ещё содержит окно, `IsWindow` внутри
        // `pin` уже видит мёртвый hwnd → PinWindowGone.
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        drop(wnd);
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(!pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        assert!(
            edit.pinned_windows.is_empty(),
            "мёртвое окно не попадает в реестр"
        );
        assert!(!window_pins.is_pinned(hwnd));
    }

    #[test]
    fn pin_window_clamps_oversized_to_90_percent_of_monitor() {
        // Кламп размера вернули по запросу пользователя 2026-08-19
        // («закреплённые окна не могли быть больше чем 90% от размера
        // монитора»). «Fullscreen»-окно 1920×1080 на мониторе 1920×1080 →
        // кламп до 1728×972 (90% по каждой оси, независимо) тем же
        // move_resize, что использует драг стикера-окна; topmost (порт
        // PowerToys) не меняется — кламп только про размер.
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

        let (_cfg, _config_path, _snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let snapshot = vec![WindowInfo {
            hwnd,
            rect: WindowRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            pid: std::process::id() + 1,
            exe_path: std::env::current_exe().expect("путь к exe теста"),
            z_order: 0,
            ..Default::default()
        }];
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        assert!(
            window_pins.is_pinned(hwnd),
            "окно закреплено (WS_EX_TOPMOST + маркер)"
        );

        // SAFETY: чтение прямоугольника живого окна.
        let mut rect = RECT::default();
        unsafe { GetWindowRect(wnd.0, &mut rect) }.expect("GetWindowRect");
        assert_eq!(
            (rect.right - rect.left, rect.bottom - rect.top),
            (1728, 972),
            "окно ужато до 90% по каждой оси независимо"
        );
    }

    /// Потолок 90% обязан действовать и на ОБЫЧНЫЙ ресайз окна за рамку, а
    /// не только в момент закрепления (репорт пользователя 2026-08-21:
    /// «я всё ещё могу расширять окно»). Проверяем реактивный путь
    /// `enforce_pinned_geometry`: окно растянули мимо нас — следующий снимок
    /// трекера возвращает его в лимит.
    #[test]
    fn enforce_pinned_geometry_caps_window_grown_by_user() {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNA, SetWindowPos, ShowWindow,
        };

        let (_cfg, _config_path, _snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        // Принуждение геометрии работает по ЖИВОМУ прямоугольнику окна
        // (`live_rect`), а он есть только у видимого несвёрнутого окна —
        // тестовое окно по умолчанию скрыто.
        // SAFETY: ShowWindow безопасен для своего окна; SW_SHOWNA не забирает фокус.
        let _ = unsafe { ShowWindow(wnd.0, SW_SHOWNA) };
        let snapshot = vec![WindowInfo {
            hwnd,
            rect: WindowRect {
                x: 0,
                y: 0,
                w: 400,
                h: 300,
            },
            pid: std::process::id() + 1,
            exe_path: std::env::current_exe().expect("путь к exe теста"),
            z_order: 0,
            ..Default::default()
        }];
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();
        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));

        // Пользователь растянул окно на весь монитор мимо нас.
        // SAFETY: окно живо; флаги исключают активацию и смену z-order.
        unsafe {
            SetWindowPos(
                wnd.0,
                Some(HWND_TOP),
                0,
                0,
                1920,
                1080,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .expect("растянуть тестовое окно");

        enforce_pinned_geometry(&mut edit, &window_pins, &monitor_bounds);

        // Меряем ВИДИМЫЕ границы (DWM), а не `GetWindowRect`: последний
        // включает невидимые поля ресайза Win11 (около 7 px по бокам и
        // снизу), и лимит, посчитанный в этом пространстве, систематически
        // врал бы на размер рамки.
        let visible = rst_win32::window_enum::live_rect(hwnd).expect("живые границы окна");
        assert!(
            visible.w <= 1728 + PINNED_GEOMETRY_EPS_PX
                && visible.h <= 972 + PINNED_GEOMETRY_EPS_PX,
            "окно должно быть ужато до 90% монитора, а осталось {}×{}",
            visible.w,
            visible.h
        );
    }

    /// Магнит: окно, оставленное в нескольких пикселях от угла монитора,
    /// на следующем снимке встаёт вплотную (запрос пользователя 2026-08-21
    /// — «снап грид», работающий и вне режима редактирования).
    #[test]
    fn enforce_pinned_geometry_snaps_released_window_to_corner() {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNA, SetWindowPos, ShowWindow,
        };

        let (_cfg, _config_path, _snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        // См. соседний тест: без видимого окна `live_rect` ничего не отдаст.
        // SAFETY: ShowWindow безопасен для своего окна; SW_SHOWNA не забирает фокус.
        let _ = unsafe { ShowWindow(wnd.0, SW_SHOWNA) };
        let snapshot = vec![WindowInfo {
            hwnd,
            rect: WindowRect {
                x: 300,
                y: 300,
                w: 400,
                h: 300,
            },
            pid: std::process::id() + 1,
            exe_path: std::env::current_exe().expect("путь к exe теста"),
            z_order: 0,
            ..Default::default()
        }];
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();
        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        // Первый проход запоминает исходную геометрию.
        enforce_pinned_geometry(&mut edit, &window_pins, &monitor_bounds);

        // Пользователь перетащил окно почти в угол и отпустил.
        // SAFETY: окно живо; флаги исключают активацию и смену z-order.
        unsafe {
            SetWindowPos(wnd.0, Some(HWND_TOP), 5, 4, 400, 300, SWP_NOZORDER | SWP_NOACTIVATE)
        }
        .expect("подвинуть тестовое окно к углу");

        enforce_pinned_geometry(&mut edit, &window_pins, &monitor_bounds);

        // Магнит ставит вплотную ВИДИМУЮ кромку окна (DWM-границы) — именно
        // её видит пользователь; `GetWindowRect` показал бы «минус рамка».
        let visible = rst_win32::window_enum::live_rect(hwnd).expect("живые границы окна");
        assert!(
            visible.x.abs() <= PINNED_GEOMETRY_EPS_PX
                && visible.y.abs() <= PINNED_GEOMETRY_EPS_PX,
            "окно должно примагнититься в угол, а стоит в ({}, {})",
            visible.x,
            visible.y
        );
    }

    #[test]
    fn pin_window_within_limit_is_not_resized() {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

        let (_cfg, _config_path, _snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        // 300×200 на 1920×1080 — в пределах 90%, move_resize не нужен,
        // реальное окно не трогается.
        let snapshot = vec![WindowInfo {
            hwnd,
            rect: WindowRect {
                x: 10,
                y: 20,
                w: 300,
                h: 200,
            },
            pid: std::process::id() + 1,
            exe_path: std::env::current_exe().expect("путь к exe теста"),
            z_order: 0,
            ..Default::default()
        }];
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        // SAFETY: чтение прямоугольника живого окна.
        let mut before = RECT::default();
        unsafe { GetWindowRect(wnd.0, &mut before) }.expect("GetWindowRect");

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));

        // SAFETY: чтение прямоугольника живого окна.
        let mut after = RECT::default();
        unsafe { GetWindowRect(wnd.0, &mut after) }.expect("GetWindowRect");
        assert_eq!(after, before, "окно в пределах 90% — ресайза быть не должно");
    }

    #[test]
    fn unpin_window_pushes_flash() {
        // Пульс рамки — на ОБА направления по подтверждению пользователя
        // (2026-08-18): открепили — тоже мигаем.
        let (_cfg, _config_path, _snapshot, _monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        unpin_window(&mut edit, &mut window_pins, hwnd);

        assert_eq!(edit.pin_flashes.len(), 1, "анпин запускает пульс рамки");
        assert_eq!(edit.pin_flashes[0].hwnd, hwnd as isize);
    }

    #[test]
    fn pin_flash_trapezoid_opacity() {
        // Трапеция (запрос пользователя 2026-08-22): 0.25 с проявления,
        // 1 с на единице, 0.25 с угасания — всего 1.5 с.
        let base = Instant::now();
        let flash = PinFlash { hwnd: 1, started_at: base, kind: PinFlashKind::Pin };
        let at = |secs: f64| base + Duration::from_secs_f64(secs);
        assert!((flash.opacity(at(0.0)) - 0.0).abs() < 1e-9);
        assert!((flash.opacity(at(0.125)) - 0.5).abs() < 1e-9, "середина проявления");
        assert!((flash.opacity(at(0.25)) - 1.0).abs() < 1e-9, "полная яркость к 0.25 с");
        assert!((flash.opacity(at(0.8)) - 1.0).abs() < 1e-9, "держится всю секунду");
        assert!((flash.opacity(at(1.25)) - 1.0).abs() < 1e-9, "конец удержания");
        assert!((flash.opacity(at(1.375)) - 0.5).abs() < 1e-9, "середина угасания");
        assert_eq!(flash.opacity(at(1.5)), 0.0, "ровно в 1.5 с — уже 0");
        assert_eq!(flash.opacity(at(4.0)), 0.0);
        assert_eq!(PIN_FLASH_DURATION, Duration::from_millis(1500));
    }

    #[test]
    fn pin_flash_next_deadline_and_expiry() {
        let base = Instant::now();
        let flash = PinFlash { hwnd: 1, started_at: base, kind: PinFlashKind::Pin };
        assert!(
            flash.next_deadline(base).is_some(),
            "живой пульс держит планировщик на коротком шаге"
        );
        let after = base + PIN_FLASH_DURATION + Duration::from_millis(1);
        assert!(flash.expired(after), "истёк за пределами длительности");
        assert_eq!(flash.next_deadline(after), None, "дедлайна больше нет");
    }

    #[test]
    fn pin_flash_color_differs_by_kind() {
        // Запрос пользователя 2026-08-19: анпин красный, не cyan закрепления.
        let now = Instant::now();
        let pin = PinFlash { hwnd: 1, started_at: now, kind: PinFlashKind::Pin };
        let unpin = PinFlash { hwnd: 1, started_at: now, kind: PinFlashKind::Unpin };
        assert_eq!(pin.color(), PIN_FLASH_COLOR_PIN);
        assert_eq!(unpin.color(), PIN_FLASH_COLOR_UNPIN);
        assert_ne!(pin.color(), unpin.color());
    }

    #[test]
    fn banner_shows_with_monitor_and_expires_after_duration() {
        // Баннер оверлея (решение координатора 2026-08-18 — замена
        // невидимого на Win11 25H2 tray-баллуна): ставится на свой монитор,
        // живёт BANNER_DURATION, потом авто-dismiss через `expired`.
        let mut edit = mask_gate_edit_state();
        let text = "Комбинация Ctrl+Alt+R уже используется другим приложением".to_string();
        show_banner(&mut edit, &monitor_id("main"), text.clone());

        let banner = edit.banner.as_ref().expect("баннер установлен");
        assert_eq!(banner.text, text);
        assert_eq!(banner.monitor_id, monitor_id("main"));
        let base = banner.shown_at;
        assert!(banner.next_deadline(base).is_some(), "живой баннер держит дедлайн");
        let expired = base + BANNER_DURATION + Duration::from_millis(1);
        assert!(banner.expired(expired), "баннер истекает за BANNER_DURATION");
        assert_eq!(banner.next_deadline(expired), None);

        // Повторный показ перезаписывает предыдущий (новое важнее).
        show_banner(&mut edit, &monitor_id("main"), "другой конфликт".to_string());
        assert_eq!(edit.banner.as_ref().unwrap().text, "другой конфликт");
    }

    #[test]
    fn rebuild_pinned_panel_has_unpin_and_locks_no_rules() {
        // Панель свойств закреплённого окна урезана (решение пользователя
        // 2026-08-18): «Открепить» + замки — но без правил соседства.
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        edit.pinned_selection = Some(hwnd as isize);
        rebuild_pinned_panel(&mut edit, &snapshot, &monitor_bounds);

        let state = edit.pinned_panel.as_ref().expect("панель собрана");
        assert_eq!(state.hwnd, hwnd as isize);
        assert!(
            state
                .panel
                .widget::<Button>(rst_render::PINNED_BTN_UNPIN)
                .is_some(),
            "кнопка «Открепить» на месте — тот же id, что опрашивает handle_pinned_panel_up"
        );
        for present in [
            rst_render::PINNED_CHECK_MOVE_LOCK,
            rst_render::PINNED_CHECK_INTERACT_LOCK,
        ] {
            assert!(
                state.panel.widget::<Checkbox>(present).is_some(),
                "чекбокс замка {present} должен быть на панели"
            );
        }
        assert!(
            state
                .panel
                .widget::<Button>(rst_render::PINNED_BTN_ADD_RULE)
                .is_none(),
            "кнопка «Добавить правило» скрыта из UI (правила в спящем коде — \
             PINNED_LABEL_RULES/PINNED_SCROLLBAR_ID не публичны в rst_render, \
             непроверяемы отсюда, но не строятся в build_pinned_lock_panel)"
        );
    }

    /// Клик по окну в списке «Показывать только на…» добавляет правило по
    /// ПРОЦЕССУ этого окна (запрос пользователя 2026-08-22), повторный
    /// выбор того же приложения ничего не дублирует, а «×» правило убирает.
    #[test]
    fn host_rules_add_dedupe_and_remove() {
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();
        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        edit.pinned_selection = Some(hwnd as isize);

        // «Хозяин» — окно другого процесса в том же снимке.
        let host_hwnd = hwnd + 1;
        let mut snapshot = snapshot.clone();
        snapshot.push(WindowInfo {
            hwnd: host_hwnd,
            rect: WindowRect { x: 0, y: 0, w: 800, h: 600 },
            pid: std::process::id() + 2,
            exe_path: PathBuf::from(r"C:\Program Files\Google\chrome.exe"),
            title: "Пример — Chrome".to_string(),
            z_order: 1,
            ..Default::default()
        });

        add_host_rule(&mut edit, &snapshot, &monitor_bounds, hwnd as isize, host_hwnd);
        let rules = &edit.pinned_windows[0].host_rules;
        assert_eq!(rules.len(), 1, "правило добавлено");
        assert_eq!(
            rules[0].process_name.as_deref(),
            Some("chrome.exe"),
            "правило по короткому имени процесса — переживает перезапуск приложения"
        );
        assert!(rules[0].title_pattern.is_none(), "заголовок не фиксируем");

        add_host_rule(&mut edit, &snapshot, &monitor_bounds, hwnd as isize, host_hwnd);
        assert_eq!(
            edit.pinned_windows[0].host_rules.len(),
            1,
            "повторный выбор того же приложения ничего не добавляет"
        );

        // Панель показывает строку правила и кнопку её удаления.
        rebuild_pinned_panel(&mut edit, &snapshot, &monitor_bounds);
        let state = edit.pinned_panel.as_ref().expect("панель собрана");
        assert!(
            state
                .panel
                .widget::<Button>(rst_render::PINNED_BTN_ADD_HOST)
                .is_some(),
            "кнопка «Показывать только на…» на месте"
        );
        assert!(
            state
                .panel
                .widget::<Button>(rst_render::PINNED_HOST_ROW_BASE)
                .is_some(),
            "строка правила с кнопкой удаления на месте"
        );

        // Клик по «×» убирает правило.
        let pos = edit
            .pinned_panel
            .as_ref()
            .and_then(|s| s.panel.widget::<Button>(rst_render::PINNED_HOST_ROW_BASE))
            .map(|b| {
                let r = b.bounds();
                (r.cx, r.cy)
            })
            .expect("кнопка удаления");
        if let Some(state) = edit.pinned_panel.as_mut() {
            state.panel.pointer_event(PointerEvent::Down { pos });
        }
        handle_pinned_panel_up(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, pos);
        assert!(
            edit.pinned_windows[0].host_rules.is_empty(),
            "«×» убирает правило"
        );
    }

    /// Панель инструментов должна лежать ВНУТРИ прямоугольника окна
    /// (решение пользователя 2026-08-21) — раньше она висела под окном
    /// снаружи.
    #[test]
    fn pinned_panel_sits_inside_window_rect() {
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        edit.pinned_selection = Some(hwnd as isize);
        rebuild_pinned_panel(&mut edit, &snapshot, &monitor_bounds);

        let state = edit.pinned_panel.as_ref().expect("панель собрана");
        let (_, placement) =
            pinned_window_dip_placement(hwnd as isize, &snapshot, &monitor_bounds)
                .expect("геометрия закреплённого окна");
        let frame = state.panel.frame();
        let win_left = placement.cx - placement.w / 2.0;
        let win_right = placement.cx + placement.w / 2.0;
        let win_bottom = placement.cy + placement.h / 2.0;
        assert!(
            frame.w <= placement.w,
            "панель шире окна: панель {frame:?}, окно {placement:?}"
        );
        assert!(
            frame.cx - frame.w / 2.0 >= win_left - 0.5
                && frame.cx + frame.w / 2.0 <= win_right + 0.5,
            "панель вышла за боковые кромки окна: панель {frame:?}, окно {placement:?}"
        );
        assert!(
            frame.cy + frame.h / 2.0 <= win_bottom + 0.5,
            "панель вылезла ниже окна: панель {frame:?}, окно {placement:?}"
        );
        assert!(
            frame.cy - frame.h / 2.0 >= placement.cy - placement.h / 2.0 - 0.5,
            "панель вылезла выше окна: панель {frame:?}, окно {placement:?}"
        );
    }

    #[test]
    fn handle_pinned_panel_up_toggles_move_lock() {
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        edit.pinned_selection = Some(hwnd as isize);
        rebuild_pinned_panel(&mut edit, &snapshot, &monitor_bounds);

        let state = edit.pinned_panel.as_mut().expect("панель собрана");
        let pos = state
            .panel
            .widget::<Checkbox>(rst_render::PINNED_CHECK_MOVE_LOCK)
            .expect("чекбокс move-lock на панели")
            .bounds();
        let pos = (pos.cx, pos.cy);
        state.panel.pointer_event(PointerEvent::Down { pos });

        assert!(handle_pinned_panel_up(
            &mut edit,
            &snapshot,
            &monitor_bounds,
            &mut window_pins,
            pos,
        ));

        let pinned = edit
            .pinned_windows
            .iter()
            .find(|p| p.hwnd == hwnd as isize)
            .expect("окно остаётся в реестре");
        assert!(pinned.lock_move, "клик по чекбоксу включает move-lock");
        assert!(!pinned.lock_interact, "interact-lock не тронут");
        assert!(edit.pinned_panel.is_some(), "панель пересобрана, а не закрыта");
    }

    #[test]
    fn handle_pinned_panel_up_toggles_interact_lock() {
        let (_cfg, _config_path, snapshot, monitor_bounds, wnd) = pin_flow_harness();
        let hwnd = wnd.0.0 as usize;
        let mut edit = mask_gate_edit_state();
        let mut window_pins = WindowPins::new();

        assert!(pin_window(&mut edit, &snapshot, &monitor_bounds, &mut window_pins, hwnd));
        edit.pinned_selection = Some(hwnd as isize);
        rebuild_pinned_panel(&mut edit, &snapshot, &monitor_bounds);

        let state = edit.pinned_panel.as_mut().expect("панель собрана");
        let pos = state
            .panel
            .widget::<Checkbox>(rst_render::PINNED_CHECK_INTERACT_LOCK)
            .expect("чекбокс interact-lock на панели")
            .bounds();
        let pos = (pos.cx, pos.cy);
        state.panel.pointer_event(PointerEvent::Down { pos });

        assert!(handle_pinned_panel_up(
            &mut edit,
            &snapshot,
            &monitor_bounds,
            &mut window_pins,
            pos,
        ));

        let pinned = edit
            .pinned_windows
            .iter()
            .find(|p| p.hwnd == hwnd as isize)
            .expect("окно остаётся в реестре");
        assert!(pinned.lock_interact, "клик по чекбоксу включает interact-lock");
        assert!(!pinned.lock_move, "move-lock не тронут");
        assert!(edit.pinned_panel.is_some(), "панель пересобрана, а не закрыта");
    }

    #[test]
    fn corner_local_angle_deg_matches_axis_aligned_geometry() {
        // Чистая геометрия направления к углу неповёрнутого прямоугольника
        // в экранных осях (Y вниз) — не подобранная константа.
        assert_eq!(corner_local_angle_deg(CoreCorner::NorthWest), -135.0);
        assert_eq!(corner_local_angle_deg(CoreCorner::NorthEast), -45.0);
        assert_eq!(corner_local_angle_deg(CoreCorner::SouthEast), 45.0);
        assert_eq!(corner_local_angle_deg(CoreCorner::SouthWest), 135.0);
    }

    #[test]
    fn resolve_zone_inside_box_is_sticker_body() {
        let (cfg, selection, id) = zone_test_config(zone_test_sticker(0.0));
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), 200.0, 200.0);
        assert!(matches!(zone, Zone::StickerBody(zid) if zid == id));
    }

    #[test]
    fn resolve_zone_on_handle_is_resize_regardless_of_inside_outside() {
        let (cfg, selection, id) = zone_test_config(zone_test_sticker(0.0));
        // Точно на NW-ручке (150,150) — угол рамки.
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), 150.0, 150.0);
        assert!(matches!(
            zone,
            Zone::ResizeHandle(zid, HandleKind::NorthWest) if zid == id
        ));
    }

    #[test]
    fn resolve_zone_outside_but_close_to_corner_is_dead_zone_not_rotate() {
        // (140,140): снаружи рамки (x<150 и y<150), но всего ~14 DIP от угла
        // (150,150) — меньше ROTATE_MIN_CORNER_GAP_DIP=35, значит НЕ поворот
        // (фидбэк пользователя: раньше здесь СРАБАТЫВАЛО кольцо поворота
        // даже так близко к рамке — теперь явный буфер).
        let (cfg, selection, _id) = zone_test_config(zone_test_sticker(0.0));
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), 140.0, 140.0);
        assert!(matches!(zone, Zone::Background), "должна быть мёртвая зона, не поворот");
    }

    #[test]
    fn resolve_zone_outside_past_gap_near_corner_is_rotate_with_local_angle() {
        // (120,120): снаружи, ~42 DIP от угла (150,150) — за отступом,
        // ближайший угол NW, без поворота стикера угол курсора = -135°.
        let (cfg, selection, id) = zone_test_config(zone_test_sticker(0.0));
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), 120.0, 120.0);
        let Zone::Rotate(zid, angle) = zone else {
            panic!("ожидался Zone::Rotate, получено другое");
        };
        assert_eq!(zid, id);
        assert_eq!(angle, -135);
    }

    #[test]
    fn resolve_zone_rotate_has_no_upper_distance_bound() {
        // Старое кольцо было ограничено ROTATE_RING_MAX_DIP=24 сверху —
        // очень далёкая точка снаружи ловилась бы в Background. Теперь
        // поворот действует на бесконечную дистанцию (фидбэк пользователя).
        let (cfg, selection, id) = zone_test_config(zone_test_sticker(0.0));
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), -5000.0, -5000.0);
        assert!(matches!(zone, Zone::Rotate(zid, _) if zid == id));
    }

    #[test]
    fn resolve_zone_captures_whole_perimeter_not_just_near_corners() {
        // Точка снаружи прямо над серединой верхней грани (не рядом ни с
        // одним углом конкретно) — тоже поворот, а не «мёртвая зона» (фидбэк
        // пользователя: «он вообще захватывает всю зону редакции»).
        let (cfg, selection, id) = zone_test_config(zone_test_sticker(0.0));
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), 200.0, 50.0);
        assert!(matches!(zone, Zone::Rotate(zid, _) if zid == id));
    }

    #[test]
    fn resolve_zone_rotate_angle_follows_sticker_rotation() {
        // Стикер повёрнут на 90° (по часовой, экранная конвенция) — угол,
        // который был NW у неповёрнутого прямоугольника, теперь физически
        // там, где раньше был NE. Находим его РЕАЛЬНОЕ мировое положение
        // через ту же SelectionBox, что использует resolve_zone, и берём
        // точку далеко за ним по той же радиальной линии от центра.
        let sticker = zone_test_sticker(std::f64::consts::FRAC_PI_2);
        let sbox = SelectionBox::new(&sticker.placement, &sticker.transform);
        let (hx, hy) = sbox.handle_center(CoreCorner::NorthWest.handle());
        let (cx, cy) = (sticker.placement.cx, sticker.placement.cy);
        // Точка вдвое дальше от центра по той же линии центр→угол —
        // гарантированно снаружи рамки и дальше ROTATE_MIN_CORNER_GAP_DIP.
        let (px, py) = (cx + (hx - cx) * 3.0, cy + (hy - cy) * 3.0);
        let (cfg, selection, id) = zone_test_config(sticker);
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), px, py);
        let Zone::Rotate(zid, angle) = zone else {
            panic!("ожидался Zone::Rotate, получено другое");
        };
        assert_eq!(zid, id);
        // Локальный угол NW (-135°) + поворот стикера (90°) = -45°.
        assert_eq!(angle, -45);
    }

    #[test]
    fn resolve_zone_click_on_other_sticker_selects_it_instead_of_rotating_selected() {
        // Регрессия: зона поворота выделенного стикера безгранична наружу
        // (см. `resolve_zone_rotate_has_no_upper_distance_bound` выше) — без
        // явного приоритета клика по ДРУГОМУ стикеру она перехватывала бы
        // клик по нему целиком, и переключить выделение на другой стикер
        // было бы физически невозможно (репорт пользователя, 2026-08-10).
        let a = zone_test_sticker(0.0); // центр (200,200), 100×100
        let a_id = a.id;
        let mut b = zone_test_sticker(0.0);
        b.id = Uuid::new_v4();
        b.placement.cx = 600.0;
        b.placement.cy = 600.0;
        let b_id = b.id;
        let mut cfg = Config::default();
        cfg.stickers.push(a);
        cfg.stickers.push(b);
        let mut selection = SelectionSet::default();
        selection.click(Some(a_id));
        // (600,600) — центр B, далеко за пределами зоны поворота A (которая
        // формально безгранична, но клик должен уйти к B, не к повороту A).
        let zone = resolve_zone(&cfg, &selection, &monitor_id("main"), 600.0, 600.0);
        assert!(
            matches!(zone, Zone::StickerBody(zid) if zid == b_id),
            "клик по стикеру B должен выбрать B, получено: {zone:?}"
        );
    }

    // --- Тултипы кнопок (фидбэк пользователя 2026-08-10): задержка 0.2с +
    // плавное появление 0.3с ---

    fn test_tooltip(hover_started: Instant) -> TooltipState {
        TooltipState {
            text: "Удалить",
            hover_started,
            anchor: Box2D {
                cx: 100.0,
                cy: 100.0,
                w: 28.0,
                h: 28.0,
                rotation: 0.0,
            },
            monitor_id: monitor_id("main"),
        }
    }

    #[test]
    fn tooltip_opacity_zero_before_show_delay() {
        let tooltip = test_tooltip(Instant::now());
        assert_eq!(tooltip.opacity(Instant::now()), 0.0);
        // 0.1с меньше задержки в 0.2с — всё ещё невидим.
        let now = tooltip.hover_started + Duration::from_millis(100);
        assert_eq!(tooltip.opacity(now), 0.0);
    }

    #[test]
    fn tooltip_opacity_ramps_linearly_during_fade() {
        let tooltip = test_tooltip(Instant::now());
        // Ровно на границе задержки — начало анимации, opacity=0.
        let start = tooltip.hover_started + TOOLTIP_SHOW_DELAY;
        assert_eq!(tooltip.opacity(start), 0.0);
        // На середине анимации появления — примерно половина.
        let mid = start + TOOLTIP_FADE_DURATION / 2;
        let mid_opacity = tooltip.opacity(mid);
        assert!(
            (mid_opacity - 0.5).abs() < 0.05,
            "ожидалась ~0.5 на середине анимации, получено {mid_opacity}"
        );
    }

    #[test]
    fn tooltip_opacity_full_after_fade_completes() {
        let tooltip = test_tooltip(Instant::now());
        let after = tooltip.hover_started + TOOLTIP_SHOW_DELAY + TOOLTIP_FADE_DURATION;
        assert_eq!(tooltip.opacity(after), 1.0);
        // Далеко за завершением анимации — остаётся 1.0, не растёт бесконечно.
        let far_after = after + Duration::from_secs(60);
        assert_eq!(tooltip.opacity(far_after), 1.0);
    }

    #[test]
    fn tooltip_next_deadline_targets_show_delay_end_before_fade() {
        let tooltip = test_tooltip(Instant::now());
        let deadline = tooltip
            .next_deadline(Instant::now())
            .expect("до истечения задержки дедлайн обязан быть");
        assert_eq!(deadline, tooltip.hover_started + TOOLTIP_SHOW_DELAY);
    }

    #[test]
    fn tooltip_next_deadline_none_after_fade_completes() {
        let tooltip = test_tooltip(Instant::now());
        let after = tooltip.hover_started + TOOLTIP_SHOW_DELAY + TOOLTIP_FADE_DURATION;
        assert!(
            tooltip.next_deadline(after).is_none(),
            "после завершения анимации дедлайн больше не нужен — opacity стабилен"
        );
    }

    #[test]
    fn toolbar_tooltip_text_covers_all_buttons_and_skips_non_buttons() {
        for id in [
            toolbar::TB_LAYERS,
            toolbar::TB_EYE,
            toolbar::TB_ORDER_UP,
            toolbar::TB_ORDER_DOWN,
            toolbar::TB_DUPLICATE,
            toolbar::TB_RESET_SCALE,
            toolbar::TB_DELETE,
            toolbar::TB_PLAY_PAUSE,
        ] {
            assert!(
                toolbar_tooltip_text(id).is_some(),
                "у кнопки {id} должен быть текст тултипа"
            );
        }
        // Слайдер/поле/громкость — не кнопки, тултипа не имеют.
        for id in [toolbar::TB_SLIDER, toolbar::TB_FIELD, toolbar::TB_VOLUME] {
            assert_eq!(toolbar_tooltip_text(id), None);
        }
    }

    #[test]
    fn cursor_panel_tooltip_text_covers_all_buttons() {
        for id in [
            cursor_panel::BTN_LOAD_FILE,
            cursor_panel::BTN_ADD_WINDOW,
            cursor_panel::BTN_PRESETS,
            cursor_panel::BTN_TOGGLE_ALL,
            cursor_panel::BTN_SETTINGS,
            cursor_panel::BTN_EXIT,
        ] {
            assert!(
                cursor_panel_tooltip_text(id).is_some(),
                "у кнопки {id} должен быть текст тултипа"
            );
        }
    }

    #[test]
    fn pinned_panel_tooltip_text_covers_buttons_rows_and_skips_others() {
        for id in [
            rst_render::PINNED_CHECK_MOVE_LOCK,
            rst_render::PINNED_CHECK_INTERACT_LOCK,
            rst_render::PINNED_BTN_UNPIN,
            rst_render::PINNED_BTN_ADD_RULE,
        ] {
            assert!(
                pinned_panel_tooltip_text(id).is_some(),
                "у кнопки {id} должен быть текст тултипа"
            );
        }
        // Каждое поле каждой строки правил — свой текст подсказки.
        for (row, field) in [
            (0usize, PinnedRowField::Remove),
            (0, PinnedRowField::ProcessName),
            (0, PinnedRowField::TitlePattern),
            (7, PinnedRowField::Remove),
            (7, PinnedRowField::ProcessName),
            (7, PinnedRowField::TitlePattern),
        ] {
            assert!(
                pinned_panel_tooltip_text(rst_render::pinned_row_id(row, field)).is_some(),
                "у элемента строки {row} ({field:?}) должен быть текст тултипа"
            );
        }
        // Сам панель и несвязанные id — без подсказки.
        for id in [rst_render::PINNED_PANEL_ID, cursor_panel::BTN_SETTINGS, 1] {
            assert_eq!(pinned_panel_tooltip_text(id), None);
        }
    }

    #[test]
    fn tooltip_primitives_empty_when_invisible() {
        let tooltip = test_tooltip(Instant::now());
        assert!(tooltip_primitives(&tooltip, 1080.0, 0.0).is_empty());
    }

    #[test]
    fn tooltip_primitives_positions_below_anchor_when_room() {
        let tooltip = test_tooltip(Instant::now());
        let prims = tooltip_primitives(&tooltip, 1080.0, 1.0);
        assert_eq!(prims.len(), 3, "два Fill (рамка+фон) + один Text");
        let Primitive::Fill { rect, opacity, .. } = prims[0] else {
            panic!("первый примитив — фон-рамка");
        };
        assert_eq!(opacity, theme::PANEL_BG_OPACITY, "полная анимация — полная непрозрачность фона");
        // Верх тултипа строго ниже низа кнопки (anchor.cy + h/2 = 114) плюс
        // зазор TOOLTIP_GAP_DIP.
        let anchor_bottom = tooltip.anchor.cy + tooltip.anchor.h / 2.0;
        assert!(
            rect.cy - rect.h / 2.0 >= anchor_bottom,
            "тултип должен быть ниже кнопки, когда снизу есть место"
        );
    }

    #[test]
    fn tooltip_primitives_flips_above_anchor_when_no_room_below() {
        // Кнопка у самого низа маленького экрана — снизу места нет, тултип
        // должен уйти НАД кнопкой (тот же приём, что toolbar_top).
        let mut tooltip = test_tooltip(Instant::now());
        tooltip.anchor.cy = 195.0; // низ кнопки на y=209, экран высотой 210
        let prims = tooltip_primitives(&tooltip, 210.0, 1.0);
        let Primitive::Fill { rect, .. } = prims[0] else {
            panic!("первый примитив — фон-рамка");
        };
        let anchor_top = tooltip.anchor.cy - tooltip.anchor.h / 2.0;
        assert!(
            rect.cy + rect.h / 2.0 <= anchor_top,
            "тултип должен уйти над кнопкой, когда снизу не хватает места"
        );
    }

    fn window(
        exe: Option<&str>,
        title: &str,
        class: &str,
        rect: WindowRect,
        iconic: bool,
    ) -> WindowInfo {
        WindowInfo {
            exe_path: exe.map(std::path::PathBuf::from).unwrap_or_default(),
            title: title.to_string(),
            class: class.to_string(),
            rect,
            iconic,
            ..Default::default()
        }
    }

    fn single_monitor_bounds(id: &str, bounds_px: Rect) -> HashMap<MonitorId, MonitorBounds> {
        let mid = monitor_id(id);
        HashMap::from([(
            mid.clone(),
            MonitorBounds {
                id: mid,
                bounds_px,
                scale: 1.0,
            },
        )])
    }

    #[test]
    fn refresh_occlusion_groups_stickers_by_visibility_signature() {
        // Два стикера с одинаковым (mode, rules) делят одну маску, третий с
        // другими rules — свою (M4_PREP_NOTES.md §4.2 — иначе allow-list
        // одного стикера просачивался бы в другой).
        let rule_a = OverlapRule {
            process_name: Some("chrome.exe".to_string()),
            title_pattern: None,
        };
        let s1 =
            sticker_with_visibility("M1", VisibilityMode::OverlapAllowlist, vec![rule_a.clone()]);
        let s2 = sticker_with_visibility("M1", VisibilityMode::OverlapAllowlist, vec![rule_a]);
        let s3 = sticker_with_visibility("M1", VisibilityMode::NeverOverlap, vec![]);
        let cfg = Config {
            stickers: vec![s1.clone(), s2.clone(), s3.clone()],
            ..Config::default()
        };
        let bounds = single_monitor_bounds("M1", bounds(0, 0, 1920, 1080));
        let result = refresh_occlusion(&cfg, &bounds, &[]);
        let groups = result.get(&monitor_id("M1")).expect("группы для M1");
        assert_eq!(groups.len(), 2, "два разных правила — две группы");
        let group_of = |id: uuid::Uuid| {
            groups
                .iter()
                .position(|g| g.stickers.contains(&id))
                .expect("стикер должен быть в какой-то группе")
        };
        assert_eq!(
            group_of(s1.id),
            group_of(s2.id),
            "одинаковое правило — одна группа"
        );
        assert_ne!(
            group_of(s1.id),
            group_of(s3.id),
            "разное правило — разные группы"
        );
    }

    #[test]
    fn refresh_occlusion_skips_always_visibility_stickers() {
        let always = sticker_with_visibility("M1", VisibilityMode::Always, vec![]);
        let cfg = Config {
            stickers: vec![always],
            ..Config::default()
        };
        let bounds = single_monitor_bounds("M1", bounds(0, 0, 1920, 1080));
        let result = refresh_occlusion(&cfg, &bounds, &[]);
        assert!(
            !result.contains_key(&monitor_id("M1")),
            "монитор без стикеров, которым нужна маска, не должен попадать в результат"
        );
    }

    #[test]
    fn refresh_occlusion_empty_window_snapshot_yields_group_with_empty_rects() {
        // Топология (группы) существует независимо от того, есть ли уже
        // известные окна — до первого Windows(Changed) стикер просто ничего
        // не прячет (безопасный дефолт).
        let s = sticker_with_visibility("M1", VisibilityMode::NeverOverlap, vec![]);
        let cfg = Config {
            stickers: vec![s.clone()],
            ..Config::default()
        };
        let bounds = single_monitor_bounds("M1", bounds(0, 0, 1920, 1080));
        let result = refresh_occlusion(&cfg, &bounds, &[]);
        let groups = result.get(&monitor_id("M1")).expect("группа для M1");
        assert_eq!(groups.len(), 1);
        assert!(groups[0].rects.is_empty());
        assert_eq!(groups[0].stickers, vec![s.id]);
    }

    #[test]
    fn occluder_rects_for_skips_iconic_windows() {
        let windows = vec![window(
            None,
            "t",
            "c",
            WindowRect {
                x: 10,
                y: 10,
                w: 100,
                h: 100,
            },
            true,
        )];
        let rects = occluder_rects_for(
            VisibilityMode::NeverOverlap,
            &[],
            false,
            &windows,
            &bounds(0, 0, 1920, 1080),
        );
        assert!(
            rects.is_empty(),
            "свёрнутое окно не должно давать оклюдер-прямоугольник"
        );
    }

    #[test]
    fn occluder_rects_for_clips_to_monitor_bounds() {
        let windows = vec![window(
            None,
            "t",
            "c",
            WindowRect {
                x: 1800,
                y: 1000,
                w: 300,
                h: 300,
            },
            false,
        )];
        let rects = occluder_rects_for(
            VisibilityMode::NeverOverlap,
            &[],
            false,
            &windows,
            &bounds(0, 0, 1920, 1080),
        );
        assert_eq!(rects, vec![bounds(1800, 1000, 120, 80)]);
    }

    #[test]
    fn occluder_rects_for_allowlisted_window_is_excluded() {
        let windows = vec![window(
            Some("chrome.exe"),
            "t",
            "c",
            WindowRect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            false,
        )];
        let rule = OverlapRule {
            process_name: Some("chrome.exe".to_string()),
            title_pattern: None,
        };
        let rects = occluder_rects_for(
            VisibilityMode::OverlapAllowlist,
            &[rule],
            false,
            &windows,
            &bounds(0, 0, 1920, 1080),
        );
        assert!(rects.is_empty());
    }

    /// Регрессия на живой репорт пользователя: окклюдер, частично закрытый
    /// ДРУГИМ окном, стоящим выше него в z-order (`windows[0]` — самое
    /// верхнее, порядок кэша трекера), даёт вырезанный прямоугольник, а не
    /// исходный целиком — иначе стикер оставался спрятанным даже там, где
    /// окклюдер физически не виден. Allow-list с правилом `notepad.exe`:
    /// `notepad` — «разрешённое» окно (сверху, `is_occluder` false, само по
    /// себе рект в вывод не даёт), `chrome` не подходит под правило —
    /// окклюдер (сравнение с `allowlist_matches_by_short_process_name`/
    /// `allowlist_process_rule_no_exe_path_still_occludes` в occluders.rs).
    #[test]
    fn occluder_rects_for_subtracts_windows_above_in_z_order() {
        let on_top = window(
            Some("notepad.exe"),
            "поверх",
            "c",
            WindowRect {
                x: 0,
                y: 0,
                w: 50,
                h: 100,
            },
            false,
        );
        let occluder = window(
            Some("chrome.exe"),
            "t",
            "c",
            WindowRect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            false,
        );
        // z-order сверху вниз: `on_top` первым (топовое окно), `occluder` —
        // ниже (window_tracker.rs: «Кэш: Vec<WindowInfo> (порядок = z-order)»).
        let windows = vec![on_top, occluder];
        let rule = OverlapRule {
            process_name: Some("notepad.exe".to_string()),
            title_pattern: None,
        };
        let rects = occluder_rects_for(
            VisibilityMode::OverlapAllowlist,
            &[rule],
            false,
            &windows,
            &bounds(0, 0, 1920, 1080),
        );
        let total_area: i64 = rects.iter().map(|r| r.w as i64 * r.h as i64).sum();
        assert_eq!(
            total_area,
            100 * 100 - 50 * 100,
            "видимая площадь окклюдера — его rect минус то, что реально сверху"
        );
        for r in &rects {
            assert!(r.x >= 50, "куски не должны заходить в область on_top: {r:?}");
        }
    }

    /// Окно, разрешённое allow-list'ом (само не окклюдер), но стоящее выше
    /// окклюдера в z-order и полностью его закрывающее, всё равно должно
    /// вычитать его площадь — маска реагирует на «что реально видно», а не
    /// только на другие окклюдеры (тот самый случай из репорта:
    /// переключились на постороннее/разрешённое окно — оно теперь сверху и
    /// закрывает исключённое окно, стикер обязан появиться под ним).
    #[test]
    fn occluder_rects_for_subtracts_non_matching_window_above_too() {
        let allowed_on_top = window(
            Some("notepad.exe"),
            "разрешённое",
            "c",
            WindowRect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            false,
        );
        let occluder = window(
            Some("chrome.exe"),
            "t",
            "c",
            WindowRect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            false,
        );
        let windows = vec![allowed_on_top, occluder];
        let rule = OverlapRule {
            process_name: Some("notepad.exe".to_string()),
            title_pattern: None,
        };
        let rects = occluder_rects_for(
            VisibilityMode::OverlapAllowlist,
            &[rule],
            false,
            &windows,
            &bounds(0, 0, 1920, 1080),
        );
        assert!(
            rects.is_empty(),
            "полностью закрытый сверху окклюдер не должен давать видимую площадь, а закрывающее окно само не окклюдер"
        );
    }

    #[test]
    fn window_exe_path_empty_pathbuf_is_none() {
        let w = window(None, "t", "c", WindowRect::default(), false);
        assert_eq!(window_exe_path(&w), None);
    }

    #[test]
    fn window_exe_path_nonempty_is_some() {
        let w = window(Some("chrome.exe"), "t", "c", WindowRect::default(), false);
        assert_eq!(window_exe_path(&w), Some("chrome.exe".to_string()));
    }

    #[test]
    fn window_rect_to_core_rejects_zero_or_negative_size() {
        assert_eq!(
            window_rect_to_core(&WindowRect {
                x: 0,
                y: 0,
                w: 0,
                h: 10
            }),
            None
        );
        assert_eq!(
            window_rect_to_core(&WindowRect {
                x: 0,
                y: 0,
                w: 10,
                h: -5
            }),
            None
        );
    }

    #[test]
    fn window_rect_to_core_converts_valid_rect() {
        assert_eq!(
            window_rect_to_core(&WindowRect {
                x: -10,
                y: 20,
                w: 100,
                h: 200
            }),
            Some(bounds(-10, 20, 100, 200))
        );
    }

    // --- M3: смена DPI (M3_PREP_NOTES.md §3.6) — синтетические переходы
    // 100/150/200%, без реального смешанного DPI-железа (см. докком
    // `dpi_change_scale_and_cursor`) ---

    #[test]
    fn dpi_change_scale_matches_standard_percents() {
        // 96/144/192 — стандартные Windows-точки 100/150/200% (dpi/96.0).
        assert_eq!(dpi_change_scale_and_cursor(1.0, 96, (0.0, 0.0)).0, 1.0);
        assert_eq!(dpi_change_scale_and_cursor(1.0, 144, (0.0, 0.0)).0, 1.5);
        assert_eq!(dpi_change_scale_and_cursor(1.0, 192, (0.0, 0.0)).0, 2.0);
    }

    /// Отношение `old_scale/new_scale` считается в f32 (см. докком
    /// `dpi_change_scale_and_cursor`) — для «некруглых» отношений вроде
    /// 1.0/1.5 это не бит-в-бит точное число, поэтому сравнение физической
    /// позиции ниже — с допуском, а не `assert_eq!` (стандартная практика
    /// для float-арифметики, не признак бага в самой функции).
    fn assert_close(a: f64, b: f64, msg: &str) {
        assert!((a - b).abs() < 1e-4, "{msg}: {a} vs {b}");
    }

    #[test]
    fn dpi_change_cursor_preserves_physical_position_100_to_150() {
        // 300 DIP при 100% = 300 физических px; при 150% та же физическая
        // точка — 200 DIP (300 / 1.5).
        let (scale, cursor) = dpi_change_scale_and_cursor(1.0, 144, (300.0, 150.0));
        assert_eq!(scale, 1.5);
        assert_close(cursor.0, 200.0, "x");
        assert_close(cursor.1, 100.0, "y");
    }

    #[test]
    fn dpi_change_cursor_preserves_physical_position_150_to_200() {
        // 150 DIP при 150% = 225 физических px; при 200% та же точка —
        // 112.5 DIP (225 / 2.0).
        let (scale, cursor) = dpi_change_scale_and_cursor(1.5, 192, (150.0, 300.0));
        assert_eq!(scale, 2.0);
        assert_close(cursor.0, 112.5, "x");
        assert_close(cursor.1, 225.0, "y");
    }

    #[test]
    fn dpi_change_cursor_preserves_physical_position_200_to_100() {
        // Обратный переход (монитор передвинули со 200% экрана на 100%) —
        // та же инвариантная физическая точка, ratio > 1 на этот раз.
        let (scale, cursor) = dpi_change_scale_and_cursor(2.0, 96, (100.0, 50.0));
        assert_eq!(scale, 1.0);
        assert_close(cursor.0, 200.0, "x");
        assert_close(cursor.1, 100.0, "y");
    }

    #[test]
    fn dpi_change_same_dpi_is_a_no_op_for_cursor() {
        // Дубль WM_DISPLAYCHANGE/WM_DPICHANGED на тот же DPI (случается —
        // M3_HOTPLUG_DESIGN.md §2 про идемпотентность дублей) не должен
        // сдвигать курсор ни на суб-пиксель.
        let (scale, cursor) = dpi_change_scale_and_cursor(1.5, 144, (77.0, 33.0));
        assert_eq!(scale, 1.5);
        assert_close(cursor.0, 77.0, "x");
        assert_close(cursor.1, 33.0, "y");
    }

    #[test]
    fn dpi_change_chain_100_150_200_preserves_original_physical_position() {
        // «Смешанный DPI» сквозь три перехода подряд (100% → 150% → 200%,
        // как если бы окно переезжало по мониторам с разным DPI, или один
        // монитор трижды сменил настройку) — физическая (px) позиция курсора
        // должна остаться неизменной на каждом шаге, а не только на первом.
        let start_dip = (960.0, 540.0);
        let physical = start_dip.0 * 1.0;

        let (scale_150, cursor_150) = dpi_change_scale_and_cursor(1.0, 144, start_dip);
        assert_close(
            cursor_150.0 * scale_150 as f64,
            physical,
            "физика после 150%",
        );

        let (scale_200, cursor_200) = dpi_change_scale_and_cursor(scale_150, 192, cursor_150);
        assert_close(
            cursor_200.0 * scale_200 as f64,
            physical,
            "физика после 200%",
        );

        let (scale_100, cursor_100) = dpi_change_scale_and_cursor(scale_200, 96, cursor_200);
        assert_eq!(scale_100, 1.0);
        // Полный круг 100→150→200→100 возвращает исходную DIP-позицию.
        assert_close(cursor_100.0, start_dip.0, "x после полного круга");
        assert_close(cursor_100.1, start_dip.1, "y после полного круга");
    }

    // --- Геометрия закреплённых окон (SPEC.md «Закрепление окна»):
    // физический rect окна <-> DIP-`Placement`, для хит-теста/ручек ресайза
    // и обратно для `WindowPins::move_resize`.

    fn monitor_bounds_at(x: i32, y: i32, w: u32, h: u32, scale: f64) -> MonitorBounds {
        MonitorBounds {
            id: MonitorId("test-monitor".to_string()),
            bounds_px: bounds(x, y, w, h),
            scale,
        }
    }

    #[test]
    fn window_rect_to_placement_scale_one_no_origin() {
        let mon = monitor_bounds_at(0, 0, 1920, 1080, 1.0);
        let p = window_rect_to_placement(
            &WindowRect {
                x: 100,
                y: 50,
                w: 400,
                h: 300,
            },
            MonitorId("m".to_string()),
            &mon,
        );
        assert_eq!(p.cx, 300.0, "cx = x + w/2");
        assert_eq!(p.cy, 200.0, "cy = y + h/2");
        assert_eq!(p.w, 400.0);
        assert_eq!(p.h, 300.0);
        assert_eq!(p.monitor_id, MonitorId("m".to_string()));
    }

    #[test]
    fn window_rect_to_placement_subtracts_monitor_origin() {
        // Второй монитор правее основного: начало (1920, 0) в физике.
        let mon = monitor_bounds_at(1920, 0, 1920, 1080, 1.0);
        let p = window_rect_to_placement(
            &WindowRect {
                x: 1920 + 100,
                y: 50,
                w: 200,
                h: 100,
            },
            MonitorId("m".to_string()),
            &mon,
        );
        // Локальные DIP-координаты монитора — без сдвига на его начало.
        assert_eq!(p.cx, 100.0 + 100.0);
        assert_eq!(p.cy, 100.0);
    }

    #[test]
    fn window_rect_to_placement_divides_by_scale() {
        // 200% DPI: физические пиксели вдвое больше DIP.
        let mon = monitor_bounds_at(0, 0, 3840, 2160, 2.0);
        let p = window_rect_to_placement(
            &WindowRect {
                x: 200,
                y: 100,
                w: 800,
                h: 400,
            },
            MonitorId("m".to_string()),
            &mon,
        );
        assert_eq!(p.w, 400.0, "физические 800 / scale 2.0 = 400 DIP");
        assert_eq!(p.h, 200.0);
        assert_eq!(p.cx, 100.0 + 200.0);
        assert_eq!(p.cy, 50.0 + 100.0);
    }

    #[test]
    fn placement_to_physical_rect_is_inverse_of_window_rect_to_placement() {
        for (mon, rect) in [
            (
                monitor_bounds_at(0, 0, 1920, 1080, 1.0),
                WindowRect {
                    x: 100,
                    y: 50,
                    w: 400,
                    h: 300,
                },
            ),
            (
                monitor_bounds_at(1920, 0, 1920, 1080, 1.0),
                WindowRect {
                    x: 2020,
                    y: 50,
                    w: 200,
                    h: 100,
                },
            ),
            (
                monitor_bounds_at(0, 0, 3840, 2160, 2.0),
                WindowRect {
                    x: 201,
                    y: 101,
                    w: 801,
                    h: 401,
                },
            ),
        ] {
            let placement = window_rect_to_placement(&rect, MonitorId("m".to_string()), &mon);
            let (x, y, w, h) = placement_to_physical_rect(&placement, &mon);
            // Округление до целого физического px — допустимая погрешность
            // 1 px в любую сторону (та же точность, что у координат мыши).
            assert!((x - rect.x).abs() <= 1, "{rect:?}: x {x} vs {}", rect.x);
            assert!((y - rect.y).abs() <= 1, "{rect:?}: y {y} vs {}", rect.y);
            assert!((w - rect.w).abs() <= 1, "{rect:?}: w {w} vs {}", rect.w);
            assert!((h - rect.h).abs() <= 1, "{rect:?}: h {h} vs {}", rect.h);
        }
    }

    // --- M4: отсечение полностью перекрытых стикеров ---

    #[test]
    fn rect_contains_fully_inside() {
        assert!(rect_contains(
            &bounds(0, 0, 200, 200),
            &bounds(50, 50, 50, 50)
        ));
    }

    #[test]
    fn rect_contains_exact_match_is_contained() {
        assert!(rect_contains(
            &bounds(0, 0, 100, 100),
            &bounds(0, 0, 100, 100)
        ));
    }

    #[test]
    fn rect_contains_partial_overlap_is_not_contained() {
        assert!(!rect_contains(
            &bounds(0, 0, 100, 100),
            &bounds(50, 50, 100, 100)
        ));
    }

    #[test]
    fn rect_contains_disjoint_is_not_contained() {
        assert!(!rect_contains(
            &bounds(0, 0, 100, 100),
            &bounds(200, 200, 50, 50)
        ));
    }

    #[test]
    fn sticker_fully_covered_true_when_aabb_inside_one_occluder() {
        let placement = Placement {
            monitor_id: monitor_id("M1"),
            cx: 100.0,
            cy: 100.0,
            w: 40.0,
            h: 40.0,
        };
        // AABB без поворота (физические px при scale=1.0): (80,80,40,40).
        let occluder = bounds(0, 0, 500, 500);
        assert!(sticker_fully_covered(&placement, 0.0, 1.0, &[occluder]));
    }

    #[test]
    fn sticker_fully_covered_false_when_only_partially_covered() {
        let placement = Placement {
            monitor_id: monitor_id("M1"),
            cx: 100.0,
            cy: 100.0,
            w: 40.0,
            h: 40.0,
        };
        // Оклюдер накрывает только левую половину AABB (80,80,40,40).
        let occluder = bounds(0, 0, 100, 500);
        assert!(!sticker_fully_covered(&placement, 0.0, 1.0, &[occluder]));
    }

    #[test]
    fn sticker_fully_covered_false_when_covered_only_by_union_of_two() {
        // Консервативная проверка: покрытие ДВУМЯ прямоугольниками вместе не
        // считается — ни один из них по отдельности не содержит AABB целиком.
        let placement = Placement {
            monitor_id: monitor_id("M1"),
            cx: 100.0,
            cy: 100.0,
            w: 40.0,
            h: 40.0,
        };
        let left_half = bounds(0, 0, 100, 500);
        let right_half = bounds(100, 0, 100, 500);
        assert!(!sticker_fully_covered(
            &placement,
            0.0,
            1.0,
            &[left_half, right_half]
        ));
    }

    #[test]
    fn sticker_fully_covered_scales_aabb_by_monitor_scale() {
        let placement = Placement {
            monitor_id: monitor_id("M1"),
            cx: 100.0,
            cy: 100.0,
            w: 40.0,
            h: 40.0,
        };
        // При scale=2.0 AABB в физических px — (160,160,80,80); прямоугольник,
        // достаточный только для scale=1.0, больше не накрывает его целиком.
        let occluder_for_scale_1 = bounds(0, 0, 200, 200);
        assert!(!sticker_fully_covered(
            &placement,
            0.0,
            2.0,
            &[occluder_for_scale_1]
        ));
        let occluder_for_scale_2 = bounds(0, 0, 500, 500);
        assert!(sticker_fully_covered(
            &placement,
            0.0,
            2.0,
            &[occluder_for_scale_2]
        ));
    }

    #[test]
    fn sticker_fully_covered_false_for_sticker_tucked_in_rounded_corner() {
        // Найдено независимым ревью (2026-08-04): маска реально скруглена
        // (mainMaskPS, радиус MASK_CORNER_RADIUS_PX) — стикер, чей AABB
        // целиком внутри ПОЛНОГО прямоугольника-окклюдера, но лежит в зоне
        // угловой дуги, реально остаётся хоть немного видимым под маской.
        // AABB (2,2,8,8) — целиком в (0,0,200,200), но целиком и в зоне
        // дуги верхнего левого угла (уменьшенный на 8px прямоугольник
        // начинается только с (8,8)).
        let placement = Placement {
            monitor_id: monitor_id("M1"),
            cx: 6.0,
            cy: 6.0,
            w: 8.0,
            h: 8.0,
        };
        let occluder = bounds(0, 0, 200, 200);
        assert!(!sticker_fully_covered(&placement, 0.0, 1.0, &[occluder]));
    }

    #[test]
    fn inset_for_mask_radius_shrinks_by_radius_on_each_side() {
        assert_eq!(
            inset_for_mask_radius(&bounds(10, 20, 100, 100)),
            Some(bounds(18, 28, 84, 84))
        );
    }

    #[test]
    fn inset_for_mask_radius_none_when_too_small() {
        assert_eq!(inset_for_mask_radius(&bounds(0, 0, 16, 100)), None);
        assert_eq!(inset_for_mask_radius(&bounds(0, 0, 100, 16)), None);
    }

    // --- M2: симуляция бага «стикер едет сам по себе» — дубликаты/coalesced
    // WM_MOUSEMOVE с ТЕМИ ЖЕ координатами при живом Drag-жесте. Прогон через
    // реальный `MouseCapture` (rst_win32::input, реальные коды сообщений
    // Win32) и реальный `apply_gesture` — ровно тот вызов, что делает
    // `handle_input` в ветке `PointerOwner::Scene if dragging`
    // (handle_input → apply_gesture, строчка выше), с реальными
    // `snap_placement`/`clamp_min_visible`. Вопрос: даёт ли дубликат
    // координат ненулевую дельту (дрейф) из-за магнита/клампа? ---

    use rst_win32::input::MouseCapture;
    use windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS;
    use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW, WS_OVERLAPPED,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };
    use windows::core::w;

    /// Скрытое окно тест-потока: `SetCapture` в `MouseCapture` требует живое
    /// окно того же потока (инвариант `MouseCapture`, input.rs) — тот же
    /// паттерн, что в тестах rst-win32.
    struct DragSimWindow(HWND);

    impl DragSimWindow {
        fn create() -> Self {
            // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
            let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(drag_sim_wndproc),
                hInstance: hinstance.into(),
                lpszClassName: w!("resticker_drag_sim_test"),
                ..Default::default()
            };
            // SAFETY: wc заполнена корректно. Класс процесс-wide: повторная
            // регистрация (параллельные тесты) — не ошибка.
            if unsafe { RegisterClassExW(&wc) } == 0 {
                // SAFETY: осмысленна сразу после провалившегося вызова.
                let err = unsafe { GetLastError() };
                assert_eq!(err, ERROR_CLASS_ALREADY_EXISTS);
            }
            // SAFETY: аргументы — валидные константы и зарегистрированный
            // класс; окно скрытое, принадлежит текущему потоку.
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("resticker_drag_sim_test"),
                    w!("test"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    100,
                    100,
                    None,
                    None,
                    Some(hinstance.into()),
                    None,
                )
            }
            .expect("создание тестового окна");
            Self(hwnd)
        }
    }

    impl Drop for DragSimWindow {
        fn drop(&mut self) {
            // SAFETY: окно создано этим же потоком выше.
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    unsafe extern "system" fn drag_sim_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        // SAFETY: делегирование системному обработчику.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// Кодировка координат Win32: знаковые 16-битные поля x/y в lParam.
    fn drag_sim_lparam(x: i16, y: i16) -> LPARAM {
        LPARAM((((y as u16 as u32) << 16) | (x as u16 as u32)) as isize)
    }

    /// Стикер 300x200 с центром (500, 400) на мониторе «main» 1920x1080
    /// (DIP = px при scale 1.0) + готовый `EditState` режима редактирования.
    fn drag_sim_harness() -> (Config, EditState, DragSimWindow, MouseCapture) {
        let mut cfg = Config::default();
        cfg.stickers.push(Sticker {
            id: Uuid::new_v4(),
            placement: Placement {
                monitor_id: monitor_id("main"),
                cx: 500.0,
                cy: 400.0,
                w: 300.0,
                h: 200.0,
            },
            ..Default::default()
        });
        let (coordinator_tx, _rx) = mpsc::channel();
        let edit = EditState {
            active: true,
            selection: SelectionSet::new(),
            gesture: None,
            snap: SnapConfig::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending_snapshot: None,
            confirm: None,
            window_picker: None,
            preset_picker: None,
            pending_open_picker: None,
            pending_open_pick_list: None,
            window_pick_list: None,
            pinned_windows: Vec::new(),
            pinned_selection: None,
            pinned_panel: None,
            pinned_gesture: None,
            surfaced_pins: HashSet::new(),
            pin_flashes: Vec::new(),
        pinned_last_rects: HashMap::new(),
        pinned_unmaximized_at: HashMap::new(),
        pinned_follow_until: None,
            banner: None,
            pending_animation: None,
            pending_video: None,
            marquee: None,
            marquee_started: false,
            toolbar: None,
            cursor_panel: None,
            tooltip: None,
            pointer_owner: PointerOwner::None,
            ui_pending_snapshot: None,
            cursor_pos: (0.0, 0.0),
            cursor_monitor: monitor_id("main"),
            coordinator_tx,
        };
        let wnd = DragSimWindow::create();
        let cap = MouseCapture::new(wnd.0);
        (cfg, edit, wnd, cap)
    }

    /// WM_LBUTTONDOWN через реальное ядро `MouseCapture` (тестируемая версия
    /// с инъекцией состояния кнопки: `handle_message_checked(..., true)` —
    /// в проде `handle_message` читает его через `GetAsyncKeyState`) + реальный
    /// старт Gesture::Drag (эти строки `handle_input` выполняет в ветке
    /// `Zone::StickerBody`): click + захват grab-офсета.
    fn drag_sim_down(cap: &mut MouseCapture, cfg: &mut Config, edit: &mut EditState, x: i16, y: i16) {
        let ev = cap
            .handle_message_checked(WM_LBUTTONDOWN, WPARAM(0), drag_sim_lparam(x, y), true)
            .expect("MouseDown");
        let InputEvent::MouseDown { pos, .. } = ev else {
            panic!("ожидался MouseDown, пришло: {ev:?}");
        };
        let (dip_x, dip_y) = to_dip(pos, 1.0);
        let mid = monitor_id("main");
        let zone = resolve_zone(cfg, &edit.selection, &mid, dip_x, dip_y);
        let Zone::StickerBody(id) = zone else {
            panic!("клик мимо стикера");
        };
        edit.selection.click(Some(id));
        let sticker = cfg
            .stickers
            .iter()
            .find(|s| s.id == id)
            .expect("стикер в конфиге");
        edit.gesture = Some(Gesture::Drag {
            start: GestureStart {
                id,
                placement: sticker.placement.clone(),
                transform: sticker.transform,
            },
            grab_dx: dip_x - sticker.placement.cx,
            grab_dy: dip_y - sticker.placement.cy,
        });
        edit.pointer_owner = PointerOwner::Scene;
    }

    /// WM_MOUSEMOVE через реальное ядро `MouseCapture` + реальный
    /// `apply_gesture` — точно тот же вызов, что в `handle_input` (ветка
    /// dragging). `left_button_down` — физическое состояние ЛКМ, инъекция
    /// вместо `GetAsyncKeyState` из `handle_message` (прод: кнопка зажата,
    /// пока пользователь тянет).
    fn drag_sim_move(
        cap: &mut MouseCapture,
        cfg: &mut Config,
        edit: &mut EditState,
        x: i16,
        y: i16,
        ctrl: bool,
        left_button_down: bool,
    ) {
        let Some(ev) =
            cap.handle_message_checked(WM_MOUSEMOVE, WPARAM(0), drag_sim_lparam(x, y), left_button_down)
        else {
            // Дедуп точных дубликатов (input.rs:226-244): повторный
            // WM_MOUSEMOVE с бит-в-бит теми же координатами отсекается ещё
            // на входе — до apply_gesture доезжает только несовпадающий.
            return;
        };
        let InputEvent::MouseMove { pos, .. } = ev else {
            panic!("ожидался MouseMove для WM_MOUSEMOVE=0x{WM_MOUSEMOVE:x}, пришло: {ev:?}");
        };
        let (dip_x, dip_y) = to_dip(pos, 1.0);
        let mid = monitor_id("main");
        let monitor = DipRect::new(0.0, 0.0, 1920.0, 1080.0);
        let mods = Modifiers {
            shift: false,
            ctrl,
            alt: false,
        };
        apply_gesture(cfg, &mut [], edit, (dip_x, dip_y), mods, monitor, &mid);
    }

    fn drag_sim_center(cfg: &Config) -> (f64, f64) {
        (cfg.stickers[0].placement.cx, cfg.stickers[0].placement.cy)
    }

    #[test]
    fn duplicate_coalesced_moves_do_not_drift_sticker() {
        let (mut cfg, mut edit, _wnd, mut cap) = drag_sim_harness();
        drag_sim_down(&mut cap, &mut cfg, &mut edit, 500, 400);
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 560, 420, false, true);
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 620, 430, false, true);
        let last = drag_sim_center(&cfg);
        // 3000 дубликатов WM_MOUSEMOVE с ТЕМИ ЖЕ координатами (coalescing,
        // автоповторы, «призрачные» сообщения): 24 c × 60 fps × ~2 события.
        // Кнопка физически зажата всю дорогу — как в реальном драге.
        for i in 0..3000 {
            drag_sim_move(&mut cap, &mut cfg, &mut edit, 620, 430, false, true);
            assert_eq!(
                drag_sim_center(&cfg),
                last,
                "дубликат WM_MOUSEMOVE (одинаковые координаты) сдвинул стикер на итерации {i}"
            );
        }
    }

    #[test]
    fn duplicate_moves_with_snap_engaged_are_stable() {
        let (mut cfg, mut edit, _wnd, mut cap) = drag_sim_harness();
        drag_sim_down(&mut cap, &mut cfg, &mut edit, 500, 400);
        // Стикер 300px шириной, центр 158 → левый край 8px от направляющей
        // x=0 — ровно на пороге магнита (8 DIP): магнит должен притянуть к 150.
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 158, 400, false, true);
        let snapped = drag_sim_center(&cfg);
        // Контроль: с Ctrl магнит отключён (`snap_move` возвращает 0), центр
        // остаётся 200 — значит выше магнит реально сработал. Точка взята
        // далеко от направляющих И от предыдущей (иначе дедуп input.rs:226
        // съел бы контрольный move как точный дубликат).
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 200, 400, true, true);
        let unsnapped = drag_sim_center(&cfg);
        assert_ne!(
            snapped, unsnapped,
            "магнит должен был притянуть на пороге: {snapped:?} vs {unsnapped:?}"
        );
        // Возврат к snap и 2000 дубликатов — позиция обязана быть бит-в-бит
        // стабильной (иначе магнит «дёргал» бы стикер на каждом дубликате).
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 158, 400, false, true);
        assert_eq!(drag_sim_center(&cfg), snapped);
        for i in 0..2000 {
            drag_sim_move(&mut cap, &mut cfg, &mut edit, 158, 400, false, true);
            assert_eq!(
                drag_sim_center(&cfg),
                snapped,
                "магнит залип/осциллирует на дубликате {i}"
            );
        }
    }

    #[test]
    fn duplicate_moves_under_clamp_are_stable() {
        let (mut cfg, mut edit, _wnd, mut cap) = drag_sim_harness();
        drag_sim_down(&mut cap, &mut cfg, &mut edit, 500, 400);
        // Захваченная мышь может уходить за границы монитора (Win32 шлёт
        // координаты вне клиентской области). Стикер 300px, центр 2100 →
        // правый край 2250 > 1920: `clamp_min_visible` зажимает (магнит
        // выключен Ctrl — чистый кламп, без притягивания).
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 2100, 400, true, true);
        let clamped = drag_sim_center(&cfg);
        assert!(
            clamped.0 < 2100.0,
            "clamp_min_visible должен был зажать центр: {clamped:?}"
        );
        for i in 0..2000 {
            drag_sim_move(&mut cap, &mut cfg, &mut edit, 2100, 400, true, true);
            assert_eq!(
                drag_sim_center(&cfg),
                clamped,
                "кламп дёргает стикер на дубликате {i}"
            );
        }
    }

    #[test]
    fn released_button_terminates_drag_via_getasynckeystate_guard() {
        // Защита из input.rs::handle_message_checked: WM_MOUSEMOVE во время
        // драга при физически отжатой ЛКМ обрабатывается как WM_LBUTTONUP
        // (input.rs:201-205) — жест обязан завершиться, а не «ехать сам».
        let (mut cfg, mut edit, _wnd, mut cap) = drag_sim_harness();
        drag_sim_down(&mut cap, &mut cfg, &mut edit, 500, 400);
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 620, 430, false, true);
        let ev = cap
            .handle_message_checked(WM_MOUSEMOVE, WPARAM(0), drag_sim_lparam(620, 430), false)
            .expect("движение при отжатой кнопке");
        assert!(
            matches!(ev, InputEvent::MouseUp { .. }),
            "WM_MOUSEMOVE при отжатой кнопке должен превратиться в MouseUp: {ev:?}"
        );
        assert!(!cap.is_captured());
        // 1000 последующих движений (кнопка всё ещё отжата) — захват снят,
        // драг не возобновляется.
        for _ in 0..1000 {
            let ev = cap
                .handle_message_checked(WM_MOUSEMOVE, WPARAM(0), drag_sim_lparam(620, 430), false)
                .expect("move без захвата");
            assert!(!matches!(ev, InputEvent::MouseDown { .. } | InputEvent::MouseUp { .. }));
        }
    }

    #[test]
    fn subpixel_noise_while_button_held_moves_sticker() {
        // Документированный в input.rs:218-222 остаточный механизм: дедуп
        // срабатывает только на БИТ-В-БИТ равные координаты; «микро-джиттер»
        // сенсора между спровоцированными пересылками (±1px к той же позиции)
        // при зажатой кнопке даёт ненулевую дельту — самоподдерживающаяся
        // петля «стикер едет сам при неподвижном курсоре». Тест фиксирует
        // этот факт, чтобы отличать его от чистых дубликатов.
        let (mut cfg, mut edit, _wnd, mut cap) = drag_sim_harness();
        drag_sim_down(&mut cap, &mut cfg, &mut edit, 500, 400);
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 620, 430, false, true);
        let before = drag_sim_center(&cfg);
        // Точный дубликат — дедуп, дельты нет.
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 620, 430, false, true);
        assert_eq!(drag_sim_center(&cfg), before, "точный дубликат не двигает");
        // Джиттер ±1px вокруг той же позиции — дельта есть.
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 621, 430, false, true);
        let after = drag_sim_center(&cfg);
        assert_ne!(before, after, "джиттер-пересылка должна дать дельту");
    }

    #[test]
    fn duplicate_moves_after_mouse_up_do_not_restart_drag() {
        let (mut cfg, mut edit, _wnd, mut cap) = drag_sim_harness();
        drag_sim_down(&mut cap, &mut cfg, &mut edit, 500, 400);
        drag_sim_move(&mut cap, &mut cfg, &mut edit, 620, 430, false, true);
        // WM_LBUTTONUP — жест завершается (captured=false в автомате,
        // ReleaseCapture), последующие дубликаты move идут с dragging=false.
        let up = cap
            .handle_message_checked(WM_LBUTTONUP, WPARAM(0), drag_sim_lparam(620, 430), false)
            .expect("MouseUp");
        assert!(matches!(up, InputEvent::MouseUp { .. }));
        for _ in 0..500 {
            let ev = cap
                .handle_message_checked(
                    WM_MOUSEMOVE,
                    WPARAM(0),
                    drag_sim_lparam(620, 430),
                    false,
                )
                .expect("move после up");
            let InputEvent::MouseMove { dragging, .. } = ev else {
                panic!("ожидался MouseMove");
            };
            assert!(!dragging, "после MouseUp движение не должно быть драгом");
        }
        assert!(
            edit.gesture.is_some(),
            "жест в EditState снимается только обработчиком MouseUp в handle_input"
        );
    }
}
