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
use rst_core::selection_set::SelectionSet;
use rst_core::snap::{self, SnapConfig};
use rst_core::transform_ops::{self, DragModifiers};
use rst_media::animation as media_animation;
use rst_media::paste;
use rst_render::{
    Box2D, Button, Checkbox, Device, Icon, Key, NumericField, Panel, PointerEvent, Primitive,
    RenderError, SelectionBox, Slider, Sprite, Texture, TextureAtlas, VideoTextures, WidgetId,
    WindowTarget, edit_overlay, marquee_visuals, rasterize, solid_sprite, theme,
};
use rst_video::VideoSource;
use rst_win32::clipboard::{self, ClipboardImage};
use rst_win32::file_dialog;
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::input::{
    Corner as Win32Corner, CursorShape, CursorZone, Handle as Win32Handle, InputEvent, Modifiers,
};
use rst_win32::monitors;
use rst_win32::overlay::{OverlayEvent, OverlayWindow};
use rst_win32::window_enum::{WindowInfo, WindowRect};
use rst_win32::window_tracker::{WindowEvent as TrackerWindowEvent, WindowTracker};
use uuid::Uuid;

use crate::{confirm_dialog, cursor_panel, toolbar, window_picker};

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

    fn draw(&self, sprites: &[Sprite]) -> Result<(), RenderError> {
        self.device.draw(&*self.target, sprites)
    }

    /// Как [`Self::draw`], но с маской перекрытия на спрайт (M4) — см.
    /// `Device::draw_masked`.
    fn draw_masked(
        &self,
        sprites: &[Sprite],
        masks: &[Option<&Texture>],
    ) -> Result<(), RenderError> {
        self.device.draw_masked(&*self.target, sprites, masks)
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
    edit_active: bool,
    hide_from_capture: bool,
) -> Option<MonitorState> {
    let (overlay, events) = match OverlayWindow::create_on_monitor(
        info.bounds_px,
        edit_hotkey,
        toggle_all_hotkey,
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

/// Кольцо поворота — зона за угловой ручкой (SPEC 3.3): начинается сразу за
/// квадратом ручки (проверяется раньше в `resolve_zone`, так что здесь нет
/// отдельной внутренней границы) и тянется до `ROTATE_RING_MAX_DIP` от её
/// центра.
const ROTATE_RING_MAX_DIP: f64 = 24.0;

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
    Shutdown,
}

/// Запрос координатора к главному потоку Tauri — обратный существующему
/// `OverlayCommand` канал (docs/M2_WIRING_PLAN.md, раздел 12): сообщения,
/// которым нужен Tauri/окно, а не чистый `Config`. Читается отдельной
/// задачей в `main::setup`, `Sender` держит координатор в `EditState`.
pub enum CoordinatorRequest {
    /// Открыть окно настроек (кнопка `BTN_SETTINGS` панели у курсора).
    OpenSettings,
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
    /// (docs/M2_WIRING_PLAN.md, раздел 8).
    fn start(&self) -> Option<&GestureStart> {
        match self {
            Gesture::Drag { start, .. } => Some(start),
            Gesture::Resize { start, .. } => Some(start),
            Gesture::Rotate { start, .. } => Some(start),
            Gesture::Marquee { .. } => None,
        }
    }
}

/// Минимальная протяжка (DIP), после которой клик по фону считается началом
/// марки, а не простым кликом со снятием выделения (docs/M2_WIRING_PLAN.md,
/// раздел 8).
const MARQUEE_THRESHOLD_DIP: f64 = 4.0;

/// Размер клетки шахматки скрытых стикеров, DIP (SPEC.md 3.7: «клетка 16×16
/// логических пикселей»).
const CHECKERBOARD_CELL_DIP: f64 = 16.0;

/// Зона под курсором в режиме редактирования (docs/M2_INTEGRATION_PLAN.md,
/// раздел 7): у выделенного стикера — кольцо поворота, ручки ресайза, тело;
/// иначе — любой видимый стикер под курсором или фон.
enum Zone {
    Background,
    StickerBody(Uuid),
    ResizeHandle(Uuid, HandleKind),
    Rotate(Uuid, CoreCorner),
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
    /// Установлен кликом по `TB_LAYERS` (`handle_toolbar_up`) — открытие
    /// нуждается в `window_snapshot`, которого нет в `handle_toolbar_up`
    /// (дизайн §5.2); фактическое открытие происходит в цикле `run()`,
    /// где снимок под рукой, сразу после обработки текущего сообщения.
    pending_open_picker: Option<Uuid>,
    /// Атлас только что добавленной анимации (M5a, docs/M5A_ANIMATION_DESIGN.md
    /// §5) — `add_sticker` собирает атлас (нужен `Renderer`, которого нет в
    /// `run()`) и кладёт его сюда вместо прямой записи в `animations`
    /// (локальная переменная `run()`, недоступная на глубине вызова); цикл
    /// `run()` забирает его в `animations` сразу после обработки текущего
    /// сообщения, тем же паттерном, что `pending_open_picker`.
    pending_animation: Option<(Uuid, TextureAtlas)>,
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

/// Анимация одного стикера (M5a, docs/M5A_ANIMATION_DESIGN.md §5): атлас
/// кадров на GPU + часы, решающие, какой кадр сейчас показывать. Чисто
/// runtime-состояние `run()` — не поле `EditState`/`Config`, тем же
/// паттерном, что `occluder_cache`/`window_snapshot`: переживает вход/выход
/// из режима редактирования, не персистится.
struct StickerAnimation {
    atlas: TextureAtlas,
    clock: AnimationClock,
}

impl StickerAnimation {
    /// Задержки кадров в порядке атласа — вход `AnimationClock::advance`/
    /// `next_deadline`.
    fn frame_delays(&self) -> Vec<Duration> {
        self.atlas.frames.iter().map(|f| f.delay).collect()
    }
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

fn run(
    config_path: PathBuf,
    mut cfg: Config,
    tx: Sender<OverlayMessage>,
    rx: Receiver<OverlayMessage>,
    coordinator_tx: Sender<CoordinatorRequest>,
) {
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
        let (edit_hotkey, this_toggle_all) = if info.id == primary_id {
            (Some(hotkey), toggle_all_hotkey)
        } else {
            (None, None)
        };
        if let Some(ms) = create_monitor_state(
            &device,
            &tx,
            info,
            edit_hotkey,
            this_toggle_all,
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
        pending_open_picker: None,
        pending_animation: None,
        pending_video: None,
        marquee: None,
        marquee_started: false,
        toolbar: None,
        cursor_panel: None,
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
            );
        } else {
            device_needs_recovery = true;
        }
    }

    for msg in rx {
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
                    media_animation::decode_animation(&new_path)
                        .ok()
                        .filter(|a| a.frames.len() >= 2)
                        .map_or(MediaType::Image, |_| MediaType::Animation)
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
                );
                // Клик-прозрачность снимается со ВСЕХ окон одновременно, не
                // только с того, что владеет хоткеем — иначе мышь на других
                // мониторах проваливалась бы сквозь режим редактирования
                // (M3_PREP_NOTES.md, раздел 3.5). `toggle_edit_mode` уже
                // применил её к окну-инициатору через `set_click_through`
                // (там же — забирает фокус); остальные — через
                // `set_interactive`, без повторного `SetForegroundWindow`,
                // иначе несколько окон боролись бы за фокус друг с другом.
                for (other_id, other_ms) in monitors_map.iter() {
                    if *other_id != monitor_id {
                        other_ms.overlay.set_interactive(edit.active);
                    }
                }
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
                ms.scale = dpi as f32 / 96.0;
                ms.target.set_dpi_scale(ms.scale);
                if let Err(e) = ms.target.resize(&device, ms.width, ms.height) {
                    tracing::error!(error = %e, "не удалось пересоздать цепочку рендера после смены DPI");
                }
                // `cursor_pos` — DIP от старого масштаба ЭТОГО монитора;
                // физическая позиция курсора смена DPI не меняет, поэтому
                // пересчёт — просто домножение на отношение масштабов, а не
                // ожидание следующего MouseMove (который без этого не
                // пересобрал бы панель вовсе — hover-ветка обновляет только
                // состояние существующей панели, docs/M3_STEP2_3_REVIEW.md,
                // пункт 2.2). Если курсор сейчас на другом мониторе, его
                // DIP уже в системе координат того, другого, монитора —
                // трогать не надо.
                if edit.cursor_monitor == monitor_id {
                    let ratio = old_scale / ms.scale;
                    edit.cursor_pos = (
                        edit.cursor_pos.0 * ratio as f64,
                        edit.cursor_pos.1 * ratio as f64,
                    );
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
            OverlayMessage::Event(_, OverlayEvent::HotkeyConflict(combo)) => {
                // Окно продолжает работать без входа в режим редактирования;
                // предупредить пользователя UI-уведомлением — отдельная
                // задача (нужен канал в Tauri/трей), пока — хотя бы в лог,
                // а не тихая потеря события.
                tracing::warn!(combo = %combo, "хоткей режима редактирования уже занят другим приложением");
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
                    let (edit_hotkey, this_toggle_all) = if info.id == new_primary_id {
                        (Some(hotkey), toggle_all_hotkey)
                    } else {
                        (None, None)
                    };
                    if let Some(ms) = create_monitor_state(
                        &device,
                        &tx,
                        info,
                        edit_hotkey,
                        this_toggle_all,
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
                );
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
                );
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
                occluder_cache = refresh_occlusion(&cfg, &monitor_bounds, &window_snapshot);
                // Открытая панель выбора окон показывает СТАРЫЙ снимок —
                // пересобрать её тем же новым снимком (M4_WINDOW_PICKER_DESIGN.md
                // §5): список окон должен обновляться в реальном времени
                // (SPEC 4.2), не только при переключении чекбоксов.
                rebuild_window_picker(&mut edit, &cfg, &window_snapshot, &monitor_geometry);
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
                for (id, anim) in animations.iter_mut() {
                    let Some(sticker) = cfg.stickers.iter().find(|s| s.id == *id) else {
                        continue;
                    };
                    if !sticker_should_tick(sticker, edit.active, &occluder_cache, &monitor_bounds)
                    {
                        continue;
                    }
                    let delays = anim.frame_delays();
                    if anim.clock.advance(now, &delays) {
                        let frame = anim.atlas.frames[anim.clock.frame_index];
                        if let Some((_, sprite)) = sprites.iter_mut().find(|(sid, _)| sid == id) {
                            sprite.uv_offset = frame.uv_offset;
                            sprite.uv_scale = frame.uv_scale;
                        }
                        need_redraw = true;
                    }
                }
                // Видео (M5b, docs/M5B_VIDEO_DESIGN.md §6): декодер сам держит
                // темп по PTS в своём потоке, координатор здесь только
                // выкачивает уже готовые кадры/звук неблокирующе. Кадры
                // копятся в очереди на 2-3 — берём ТОЛЬКО последний
                // (остальные устарели к моменту показа), а звук отдаём
                // микшеру целиком, по порядку, ни одной порции не пропуская.
                for (id, playback) in videos.iter_mut() {
                    let mut latest = None;
                    while let Some(frame) = playback.source.try_recv_frame() {
                        latest = Some(frame);
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
        // Атлас только что добавленной анимации (M5a §5, EditState::
        // pending_animation) — заводим часы здесь, где под рукой
        // `animations`; `add_sticker` не может сделать это сам, он не видит
        // локальную переменную цикла `run()`.
        if let Some((id, atlas)) = edit.pending_animation.take() {
            animations.insert(
                id,
                StickerAnimation {
                    atlas,
                    clock: AnimationClock::new(Instant::now()),
                },
            );
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
                        .then(|| anim.clock.next_deadline(&anim.frame_delays()))
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
        let next_tick_deadline = [next_anim_deadline, next_video_deadline]
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
        // Гейт хуков трекера — раз за итерацию, дёшево (см. комментарий у
        // объявления `last_mask_needed`); переключается только при реальном
        // изменении, не на каждой итерации подряд. Пока открыта панель
        // выбора окон, снимок обязан оставаться живым независимо от
        // `mask_needed(cfg)` (дизайн §5.1) — иначе если ВСЕ стикеры сейчас
        // `Always`, трекер спит, и список окон в панели не наполнится вовсе.
        let new_mask_needed = mask_needed(&cfg) || edit.window_picker.is_some();
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
                );
            } else {
                device_needs_recovery = true;
            }
        }
    }
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
    // Открытый модал не переживает выход из режима — как и незавершённый
    // жест выше, он относится к сеансу редактирования, а не к самому кадру.
    edit.confirm = None;
    edit.window_picker = None;
    edit.pending_open_picker = None;
    edit.pending_animation = None;
    edit.pending_video = None;
    edit.active = !edit.active;
    overlay.set_click_through(!edit.active);
    if !edit.active {
        edit.selection.clear();
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json при выходе из режима редактирования");
        }
    }
    rebuild_ui_panels(edit, cfg, monitor_geometry);
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
/// `Pasted` (оба ссылаются на файл на диске), нет у `Window` (M6, ещё не
/// реализован).
fn sticker_image_path(source: &StickerSource) -> Option<&Path> {
    match source {
        StickerSource::File { path, .. } => Some(path),
        StickerSource::Pasted { path } => Some(path),
        StickerSource::Window { .. } => None,
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
                            StickerAnimation {
                                atlas,
                                clock: AnimationClock::new(Instant::now()),
                            },
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "не удалось собрать атлас анимации — загружаю как статичное изображение");
                    }
                }
            }
            _ => {
                tracing::warn!(path = %path.display(), "анимация не декодировалась заново — загружаю как статичное изображение");
            }
        }
    }
    None
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
        PointerOwner::Toolbar | PointerOwner::CursorPanel | PointerOwner::WindowPicker
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

/// Разрешить зону под курсором (docs/M2_INTEGRATION_PLAN.md, раздел 7):
/// порядок проверки — обратный порядку отрисовки. Ручки/кольцо поворота
/// доступны только для одиночного выделения (мультивыделение — следующий
/// срез).
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
            // Ручки ресайза — высший приоритет: их квадрат (сторона
            // HANDLE_SIZE_DIP) целиком накрывает свой угол, поэтому кольцо
            // поворота ниже начинается ровно за его границей без мёртвой
            // зоны и без квадратного «угла» на внутренней границе
            // (docs/M2_SLICE_REVIEW.md, пункт 3 — раньше внутренняя граница
            // считалась от тела стикера, а не от центра ручки).
            for (kind, rect) in sbox.handle_rects(rst_render::HANDLE_SIZE_DIP) {
                if point_in_box2d(&rect, dip_x, dip_y) {
                    return Zone::ResizeHandle(*id, kind);
                }
            }
            for corner in CoreCorner::ALL {
                let (hx, hy) = sbox.handle_center(corner.handle());
                let dist = (dip_x - hx).hypot(dip_y - hy);
                if dist <= ROTATE_RING_MAX_DIP {
                    return Zone::Rotate(*id, corner);
                }
            }
            if hittest::contains(&sticker.placement, &sticker.transform, dip_x, dip_y) {
                return Zone::StickerBody(*id);
            }
            return Zone::Background;
        }
    }
    match hit_sticker_at(cfg, monitor_id, dip_x, dip_y) {
        Some(id) => Zone::StickerBody(id),
        None => Zone::Background,
    }
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

fn to_win32_corner(corner: CoreCorner) -> Win32Corner {
    match corner {
        CoreCorner::NorthWest => Win32Corner::NorthWest,
        CoreCorner::NorthEast => Win32Corner::NorthEast,
        CoreCorner::SouthEast => Win32Corner::SouthEast,
        CoreCorner::SouthWest => Win32Corner::SouthWest,
    }
}

fn cursor_shape_for_zone(zone: &Zone) -> CursorShape {
    match zone {
        Zone::Background => CursorZone::Background.cursor_shape(),
        Zone::StickerBody(_) => CursorZone::StickerBody.cursor_shape(),
        Zone::ResizeHandle(_, kind) => {
            CursorZone::ResizeHandle(to_win32_handle(*kind)).cursor_shape()
        }
        Zone::Rotate(_, corner) => CursorZone::RotateZone(to_win32_corner(*corner)).cursor_shape(),
    }
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
fn occluder_rects_for(
    mode: VisibilityMode,
    rules: &[OverlapRule],
    never_overlap_taskbar: bool,
    window_snapshot: &[WindowInfo],
    monitor_bounds_px: &Rect,
) -> Vec<Rect> {
    window_snapshot
        .iter()
        .filter(|w| !w.iconic)
        .filter(|w| {
            let candidate = OccluderCandidate {
                exe_path: window_exe_path(w),
                title: w.title.clone(),
                class: w.class.clone(),
            };
            occluders::is_occluder(&candidate, mode, rules, never_overlap_taskbar)
        })
        .filter_map(|w| {
            let win_rect = window_rect_to_core(&w.rect)?;
            occluders::clip_rect(&win_rect, monitor_bounds_px)
        })
        .collect()
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
    edit.toolbar = Some(toolbar::build_toolbar(
        &bounds,
        Some(sticker.transform.opacity),
        video,
        screen_h,
    ));
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
    pos: (f64, f64),
    overlay_size: (u32, u32),
    scale: f32,
    monitor_id: &MonitorId,
    monitor_geometry: &HashMap<MonitorId, (u32, u32, f32)>,
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
            let zone = resolve_zone(cfg, &edit.selection, monitor_id, dip_x, dip_y);
            match zone {
                Zone::Background => {
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
                Zone::Rotate(id, _corner) => {
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
                        (dip_x, dip_y),
                        overlay_size,
                        scale,
                        monitor_id,
                        monitor_geometry,
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
            let dm = DragModifiers {
                shift: modifiers.shift,
                alt: modifiers.alt,
            };
            let result =
                transform_ops::resize(&start.placement, &start.transform, *handle, delta, dm);
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
                // Полная непрозрачность независимо от собственной opacity
                // стикера — шахматка должна быть чётко видна, а не выцветать
                // вместе со скрытым содержимым под ней.
                frame.push(solid_sprite(&tex, monitor_id, &rect, 1.0));
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

    // Маска перекрытия выключена в режиме редактирования (M4_OCCLUDERS_
    // DESIGN.md §6: стикер должен оставаться полностью видимым и
    // интерактивным, пока его двигают/крутят, независимо от окон под ним) и
    // когда на этом мониторе нет ни одного видимого стикера с активной
    // группой окклюдеров — тогда обычный `draw()` не отличается от
    // `draw_masked()` с пустыми масками, но дешевле (не строит текстуры).
    let draw_result = if edit.active || sticker_mask_slots.is_empty() {
        renderer.draw(&frame)
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
        renderer.draw_masked(&frame, &masks)
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
) -> bool {
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
    let animation = if pasted {
        None
    } else {
        media_animation::decode_animation(&path)
            .ok()
            .filter(|a| a.frames.len() >= 2)
    };

    let (texture, uv_offset, uv_scale, atlas) = if let Some(anim) = animation {
        let frames: Vec<(Vec<u8>, Duration)> =
            anim.frames.into_iter().map(|f| (f.rgba, f.delay)).collect();
        match renderer.create_texture_atlas(&frames, anim.width, anim.height) {
            Ok(atlas) => {
                let f0 = atlas.frames[0];
                (
                    atlas.texture.clone(),
                    f0.uv_offset,
                    f0.uv_scale,
                    Some(atlas),
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
    } else {
        match load_static(renderer, &path) {
            Some(t) => (t, [0.0, 0.0], [1.0, 1.0], None),
            None => return false,
        }
    };
    let media_type = if atlas.is_some() {
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
    if let Some(atlas) = atlas {
        edit.pending_animation = Some((id, atlas));
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
    let (center_x, center_y) = (
        screen_w as f64 / scale as f64 / 2.0,
        screen_h as f64 / scale as f64 / 2.0,
    );
    let sticker = Sticker::new_file(
        path,
        MediaType::Video,
        monitor_id.clone(),
        center_x,
        center_y,
        w as f64,
        h as f64,
    );
    let id = sticker.id;
    let sprite = Sprite::new(
        textures.y.clone(),
        sticker.placement.clone(),
        sticker.transform,
    )
    .with_video(textures);
    // Новый стикер всегда со свежим `PlaybackSettings::default()` (играет,
    // громкость 1.0) — то же стартовое состояние, что и у только что
    // созданного `AudioSource`/`VideoSource`, явно применять нечего.
    let audio = audio_mixer.map(|m| m.add_source(id));
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

    fn bounds(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
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
}
