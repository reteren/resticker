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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rst_core::config;
use rst_core::hittest::{self, Corner as CoreCorner, DipRect, HandleKind};
use rst_core::model::{Config, MediaType, MonitorId, Placement, Sticker, StickerSource, Transform};
use rst_core::ops;
use rst_core::selection_set::SelectionSet;
use rst_core::snap::{self, SnapConfig};
use rst_core::transform_ops::{self, DragModifiers};
use rst_media::paste;
use rst_render::{
    Box2D, Button, Icon, Key, NumericField, Panel, PointerEvent, Primitive, Renderer, SelectionBox,
    Slider, Sprite, Texture, WidgetId, edit_overlay, marquee_visuals, rasterize, solid_sprite,
    theme,
};
use rst_win32::clipboard::{self, ClipboardImage};
use rst_win32::file_dialog;
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::input::{
    Corner as Win32Corner, CursorShape, CursorZone, Handle as Win32Handle, InputEvent, Modifiers,
};
use rst_win32::overlay::{OverlayEvent, OverlayWindow};
use uuid::Uuid;

use crate::{confirm_dialog, cursor_panel, toolbar};

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

/// Сообщение объединённого канала координатора: команда от Tauri или
/// событие от потока оверлей-окна.
enum OverlayMessage {
    Command(OverlayCommand),
    Event(OverlayEvent),
}

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
    /// Последняя известная позиция курсора, DIP — источник позиции для
    /// `cursor_panel` при пересборках, не вызванных `MouseMove` (Ctrl+D,
    /// undo/redo и т.п., где курсор не двигался, но панель должна остаться
    /// там же). Обновляется на каждом `MouseMove`.
    cursor_pos: (f64, f64),
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
}

/// Кэш 1×1 текстур заливки и текстур растрированного текста для перевода
/// `Primitive` (immediate-mode виджетов) в `Sprite` (docs/M2_WIRING_PLAN.md,
/// раздел 2–3). Живёт на весь сеанс редактирования в `run()`, не в
/// `EditState` — это деталь рендера, а не состояние редактирования.
struct UiTextureCache {
    fills: HashMap<[u8; 3], Texture>,
    texts: HashMap<(String, [u8; 3]), Texture>,
    icons: HashMap<Icon, Texture>,
}

impl UiTextureCache {
    fn new() -> Self {
        Self {
            fills: HashMap::new(),
            texts: HashMap::new(),
            icons: HashMap::new(),
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
        let key = (text.to_string(), color);
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
    /// и кэшируется по варианту; `size_px` берётся из первого запроса
    /// (все иконки квадратные и одного размера — `theme::BUTTON_SIZE`).
    fn icon_texture(&mut self, renderer: &Renderer, icon: Icon, size_px: u32) -> Option<Texture> {
        if let Some(t) = self.icons.get(&icon) {
            return Some(t.clone());
        }
        let rgba = rst_render::icon_rgba(icon, size_px);
        match renderer.create_texture_from_rgba(&rgba, size_px, size_px) {
            Ok(t) => {
                self.icons.insert(icon, t.clone());
                Some(t)
            }
            Err(e) => {
                tracing::warn!(error = %e, ?icon, "не удалось создать текстуру иконки UI");
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

    let (overlay, events) = match OverlayWindow::create(hotkey, toggle_all_hotkey) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать оверлей-окно");
            return;
        }
    };
    // Мост «события окна → общий канал координатора» — один поток-цикл
    // читает и команды Tauri, и события мыши/клавиатуры/хоткея
    // (docs/M2_INTEGRATION_PLAN.md, раздел 1).
    thread::spawn(move || {
        for event in events {
            if tx.send(OverlayMessage::Event(event)).is_err() {
                break;
            }
        }
    });

    let (width, height) = overlay.size();
    let mut renderer = match Renderer::new(overlay.hwnd(), width, height) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать рендерер");
            return;
        }
    };
    let dpi = overlay.dpi();
    renderer.set_dpi_scale(dpi as f32 / 96.0);
    let scale = renderer.dpi_scale();

    // Заливки для рамки выделения (белая) и затемнения режима (чёрная) —
    // 1×1 текстуры, растягиваются рендерером как обычные спрайты.
    let white_tex = match renderer.create_texture_from_rgba(&[0xff, 0xff, 0xff, 0xff], 1, 1) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать текстуру рамки выделения");
            return;
        }
    };
    let black_tex = match renderer.create_texture_from_rgba(&[0x00, 0x00, 0x00, 0xff], 1, 1) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать текстуру затемнения");
            return;
        }
    };

    // Восстановление между запусками: стикеры уже в cfg (загружены в main
    // через rst_core::config::load до вызова start()). Спрайт хранится
    // вместе с id стикера — жесты правят конкретный спрайт по id, порядок
    // отрисовки берётся из cfg.stickers (по `order`) в `redraw`.
    let mut sprites: Vec<(Uuid, Sprite)> = Vec::new();
    for sticker in &cfg.stickers {
        if let Some(path) = sticker_image_path(&sticker.source) {
            match renderer.load_image(path) {
                Ok(texture) => {
                    sprites.push((
                        sticker.id,
                        Sprite::new(texture, sticker.placement.clone(), sticker.transform),
                    ));
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "не удалось загрузить стикер при старте");
                }
            }
        }
    }

    let mut edit = EditState {
        active: false,
        selection: SelectionSet::new(),
        gesture: None,
        snap: SnapConfig::default(),
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
        pending_snapshot: None,
        confirm: None,
        marquee: None,
        marquee_started: false,
        toolbar: None,
        cursor_panel: None,
        pointer_owner: PointerOwner::None,
        ui_pending_snapshot: None,
        cursor_pos: (0.0, 0.0),
        coordinator_tx,
    };
    let mut ui_cache = UiTextureCache::new();

    redraw(
        &mut renderer,
        &sprites,
        &cfg,
        &edit,
        &white_tex,
        &black_tex,
        &mut ui_cache,
        width,
        height,
        scale,
    );

    for msg in rx {
        let mut need_redraw = false;
        match msg {
            OverlayMessage::Command(OverlayCommand::AddSticker(path)) => {
                add_sticker(
                    &overlay,
                    &mut renderer,
                    &mut cfg,
                    &config_path,
                    &mut sprites,
                    path,
                    false,
                );
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::Shutdown) => break,
            OverlayMessage::Event(OverlayEvent::ToggleEditMode) => {
                toggle_edit_mode(
                    &overlay,
                    &mut edit,
                    &mut cfg,
                    &mut sprites,
                    &renderer,
                    &config_path,
                    (width, height),
                    scale,
                );
                need_redraw = true;
            }
            OverlayMessage::Event(OverlayEvent::ToggleAllStickers) => {
                // Глобальный хоткей работает независимо от режима
                // редактирования (M2b7) — та же логика, что у BTN_TOGGLE_ALL
                // на панели у курсора (docs/M2_WIRING_PLAN.md, раздел 6).
                let before = cfg.clone();
                if converge_all_stickers_visibility(&mut cfg) {
                    commit_undo_snapshot(&mut edit, before);
                    if let Err(e) = config::save(&cfg, &config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после «показать/скрыть все»");
                    }
                    let screen = screen_dip_rect((width, height), scale);
                    rebuild_cursor_panel(&mut edit, &cfg, &screen);
                    need_redraw = true;
                }
            }
            OverlayMessage::Event(OverlayEvent::HotkeyConflict(combo)) => {
                // Окно продолжает работать без входа в режим редактирования;
                // предупредить пользователя UI-уведомлением — отдельная
                // задача (нужен канал в Tauri/трей), пока — хотя бы в лог,
                // а не тихая потеря события.
                tracing::warn!(combo = %combo, "хоткей режима редактирования уже занят другим приложением");
            }
            OverlayMessage::Event(OverlayEvent::Key {
                vk,
                modifiers,
                pressed: true,
            }) if edit.active => {
                need_redraw = handle_key(
                    vk,
                    modifiers,
                    &overlay,
                    &mut renderer,
                    &mut cfg,
                    &config_path,
                    &mut sprites,
                    &mut edit,
                    (width, height),
                    scale,
                );
            }
            OverlayMessage::Event(OverlayEvent::Key { .. }) => {}
            OverlayMessage::Event(OverlayEvent::Input(event)) if edit.active => {
                need_redraw = handle_input(
                    event,
                    scale,
                    &overlay,
                    &mut renderer,
                    (width, height),
                    &mut cfg,
                    &config_path,
                    &mut sprites,
                    &mut edit,
                );
            }
            OverlayMessage::Event(OverlayEvent::Input(_)) => {
                // Вне режима редактирования окно клик-прозрачно — эти
                // события приходить не должны, но игнорируем на всякий случай.
            }
        }
        if need_redraw {
            redraw(
                &mut renderer,
                &sprites,
                &cfg,
                &edit,
                &white_tex,
                &black_tex,
                &mut ui_cache,
                width,
                height,
                scale,
            );
        }
    }
    // renderer и overlay освобождаются здесь в обратном порядке объявления:
    // сначала renderer (COM/DComp), затем overlay (окно) — корректный порядок.
}

#[allow(clippy::too_many_arguments)]
fn toggle_edit_mode(
    overlay: &OverlayWindow,
    edit: &mut EditState,
    cfg: &mut Config,
    sprites: &mut Vec<(Uuid, Sprite)>,
    renderer: &Renderer,
    config_path: &Path,
    overlay_size: (u32, u32),
    scale: f32,
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
        resync_sprites(renderer, cfg, sprites);
    }
    edit.pointer_owner = PointerOwner::None;
    // Открытый модал не переживает выход из режима — как и незавершённый
    // жест выше, он относится к сеансу редактирования, а не к самому кадру.
    edit.confirm = None;
    edit.active = !edit.active;
    overlay.set_click_through(!edit.active);
    if !edit.active {
        edit.selection.clear();
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json при выходе из режима редактирования");
        }
    }
    rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
fn resync_sprites(renderer: &Renderer, cfg: &Config, sprites: &mut Vec<(Uuid, Sprite)>) {
    sprites.retain(|(id, _)| cfg.stickers.iter().any(|s| s.id == *id));
    for sticker in &cfg.stickers {
        if let Some((_, sprite)) = sprites.iter_mut().find(|(id, _)| *id == sticker.id) {
            sprite.placement = sticker.placement.clone();
            sprite.transform = sticker.transform;
            continue;
        }
        if let Some(path) = sticker_image_path(&sticker.source) {
            match renderer.load_image(path) {
                Ok(texture) => sprites.push((
                    sticker.id,
                    Sprite::new(texture, sticker.placement.clone(), sticker.transform),
                )),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "не удалось загрузить стикер после undo/redo");
                }
            }
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

fn perform_undo(
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
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
    resync_sprites(renderer, cfg, sprites);
    edit.selection.prune(&cfg.stickers);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после отмены");
    }
    true
}

fn perform_redo(
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
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
    resync_sprites(renderer, cfg, sprites);
    edit.selection.prune(&cfg.stickers);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после повтора");
    }
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
    center: (f64, f64),
    overlay_size: (u32, u32),
    scale: f32,
) -> bool {
    if edit.selection.is_empty() {
        return false;
    }
    if ops::should_confirm_delete(cfg) {
        edit.confirm = Some(ConfirmState {
            snapshot: cfg.clone(),
            ids: edit.selection.ids().to_vec(),
            panel: confirm_dialog::build(edit.selection.ids().len() as u32, center),
        });
    } else {
        commit_undo_snapshot(edit, cfg.clone());
        for id in edit.selection.ids().to_vec() {
            cleanup_pasted_file(cfg, id);
            let _ = ops::delete(cfg, id);
        }
        edit.selection.prune(&cfg.stickers);
        resync_sprites(renderer, cfg, sprites);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после удаления");
        }
        rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
        PointerOwner::Toolbar | PointerOwner::CursorPanel
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
                renderer,
                config_path,
                overlay_size,
                scale,
            );
            true
        }
        VK_Z if modifiers.ctrl && modifiers.shift => {
            let did = perform_redo(renderer, cfg, config_path, sprites, edit);
            rebuild_ui_panels(edit, cfg, overlay_size, scale);
            did
        }
        VK_Z if modifiers.ctrl => {
            let did = perform_undo(renderer, cfg, config_path, sprites, edit);
            rebuild_ui_panels(edit, cfg, overlay_size, scale);
            did
        }
        VK_Y if modifiers.ctrl => {
            let did = perform_redo(renderer, cfg, config_path, sprites, edit);
            rebuild_ui_panels(edit, cfg, overlay_size, scale);
            did
        }
        VK_A if modifiers.ctrl => {
            edit.selection.select_all(&cfg.stickers);
            rebuild_ui_panels(edit, cfg, overlay_size, scale);
            true
        }
        VK_DELETE => {
            let center = selection_center_or_screen(edit, cfg, overlay_size, scale);
            begin_delete(
                edit,
                renderer,
                cfg,
                config_path,
                sprites,
                center,
                overlay_size,
                scale,
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
            resync_sprites(renderer, cfg, sprites);
            edit.selection.clear();
            for id in new_ids {
                edit.selection.select(id);
            }
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после дублирования");
            }
            rebuild_ui_panels(edit, cfg, overlay_size, scale);
            true
        }
        VK_V if modifiers.ctrl => {
            paste_from_clipboard(overlay, renderer, cfg, config_path, sprites, edit)
        }
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
fn paste_from_clipboard(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
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
                if add_sticker(overlay, renderer, cfg, config_path, sprites, path, false) {
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
                        path.clone(),
                        true,
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
fn hit_sticker_at(cfg: &Config, dip_x: f64, dip_y: f64) -> Option<Uuid> {
    cfg.stickers
        .iter()
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
fn resolve_zone(cfg: &Config, selection: &SelectionSet, dip_x: f64, dip_y: f64) -> Zone {
    if let [id] = selection.ids() {
        if let Some(sticker) = cfg.stickers.iter().find(|s| s.id == *id) {
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
    match hit_sticker_at(cfg, dip_x, dip_y) {
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
    edit.toolbar = Some(toolbar::build_toolbar(
        &bounds,
        Some(sticker.transform.opacity),
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
fn rebuild_ui_panels(edit: &mut EditState, cfg: &Config, overlay_size: (u32, u32), scale: f32) {
    let screen = screen_dip_rect(overlay_size, scale);
    rebuild_toolbar(edit, cfg, screen.h);
    rebuild_cursor_panel(edit, cfg, &screen);
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
fn selection_center_or_screen(
    edit: &EditState,
    cfg: &Config,
    overlay_size: (u32, u32),
    scale: f32,
) -> (f64, f64) {
    edit.selection
        .bounds(&cfg.stickers)
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
    pos: (f64, f64),
    overlay_size: (u32, u32),
    scale: f32,
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
        rebuild_ui_panels(edit, cfg, overlay_size, scale);
        return true;
    }

    let clicked = |edit: &mut EditState, id_widget: WidgetId| -> bool {
        edit.toolbar
            .as_mut()
            .and_then(|p| p.widget_mut::<Button>(id_widget))
            .is_some_and(Button::take_click)
    };

    if clicked(edit, toolbar::TB_EYE) {
        commit_undo_snapshot(edit, cfg.clone());
        let _ = ops::toggle_visibility(cfg, id);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после переключения видимости");
        }
        rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
            resync_sprites(renderer, cfg, sprites);
            edit.selection.click(Some(new_id));
        }
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после дублирования");
        }
        rebuild_ui_panels(edit, cfg, overlay_size, scale);
        return true;
    }
    if clicked(edit, toolbar::TB_DELETE) {
        let center = selection_center_or_screen(edit, cfg, overlay_size, scale);
        return begin_delete(
            edit,
            renderer,
            cfg,
            config_path,
            sprites,
            center,
            overlay_size,
            scale,
        );
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
) {
    match file_dialog::pick_image_file(overlay.hwnd()) {
        Ok(Some(path)) => {
            let before = cfg.clone();
            if add_sticker(overlay, renderer, cfg, config_path, sprites, path, false) {
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
    pos: (f64, f64),
    overlay_size: (u32, u32),
    scale: f32,
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
        add_sticker_from_dialog(overlay, renderer, cfg, config_path, sprites, edit);
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
            renderer,
            config_path,
            overlay_size,
            scale,
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
            // раздел 5, п.1).
            if let Some(confirm) = &mut edit.confirm {
                confirm.panel.pointer_event(PointerEvent::Down {
                    pos: (dip_x, dip_y),
                });
                return true;
            }
            // Приоритет top-down по z-order: панель у курсора выше тулбара
            // на экране (redraw рисует её позже), поэтому и в хит-тесте она
            // первая (docs/M2_WIRING_PLAN.md, раздел 5).
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
            edit.pointer_owner = PointerOwner::Scene;
            let zone = resolve_zone(cfg, &edit.selection, dip_x, dip_y);
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
                            rebuild_ui_panels(edit, cfg, overlay_size, scale);
                        }
                        return changed;
                    }
                    edit.selection.click(Some(id));
                    let changed = before != edit.selection.ids();
                    if changed {
                        rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
            if let Some(confirm) = &mut edit.confirm {
                confirm.panel.pointer_event(PointerEvent::Move {
                    pos: (dip_x, dip_y),
                });
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
                    return true;
                }
                PointerOwner::Scene if dragging => {
                    let need_redraw =
                        apply_gesture(cfg, sprites, edit, (dip_x, dip_y), modifiers, monitor);
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
                        rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
            if let Some(panel) = &mut edit.toolbar {
                need_redraw |= panel
                    .pointer_event(PointerEvent::Move {
                        pos: (dip_x, dip_y),
                    })
                    .redraw;
            }
            let over_panel = edit
                .cursor_panel
                .as_ref()
                .is_some_and(|p| p.hit_test((dip_x, dip_y)))
                || edit
                    .toolbar
                    .as_ref()
                    .is_some_and(|p| p.hit_test((dip_x, dip_y)));
            if over_panel {
                overlay.post_cursor_shape(CursorShape::Arrow);
            } else {
                let zone = resolve_zone(cfg, &edit.selection, dip_x, dip_y);
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
                    resync_sprites(renderer, cfg, sprites);
                    if let Err(e) = config::save(cfg, config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после удаления через диалог");
                    }
                    rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
                        (dip_x, dip_y),
                        overlay_size,
                        scale,
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
                        (dip_x, dip_y),
                        overlay_size,
                        scale,
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
                rebuild_ui_panels(edit, cfg, overlay_size, scale);
                return true;
            }
            if edit.gesture.take().is_some() {
                // Снимок кладём в историю только сейчас, и только если жест
                // реально что-то изменил — клик без движения не тратит шаг
                // истории (docs/M2_SLICE_REVIEW.md, пункт 1).
                if let Some(before) = edit.pending_snapshot.take() {
                    if before != *cfg {
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
                    resync_sprites(renderer, cfg, sprites);
                }
                edit.pointer_owner = PointerOwner::None;
                rebuild_ui_panels(edit, cfg, overlay_size, scale);
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
            edit.selection.rubber_band(&cfg.stickers, &rect);
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
/// рамка выделения поверх (тулбар и панель у курсора — следующий срез,
/// docs/M2_INTEGRATION_PLAN.md, раздел 11).
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
) {
    let mut frame: Vec<Sprite> = Vec::with_capacity(sprites.len() + 1 + 12);
    let monitor_id = MonitorId::default();
    // Растровый шрифт — целочисленный пиксельный масштаб; `scale` (DPI/96)
    // округляем, а не берём как есть (text::rasterize ждёт `u32`).
    let text_scale = scale.round().max(1.0) as u32;

    if edit.active {
        let w_dip = width_px as f64 / scale as f64;
        let h_dip = height_px as f64 / scale as f64;
        frame.push(solid_sprite(
            black_tex,
            &monitor_id,
            &edit_overlay(w_dip, h_dip),
            rst_render::EDIT_OVERLAY_OPACITY,
        ));
    }

    // Порядок отрисовки стикеров — по `order` (больше — выше, CONFIG.md).
    // Скрытые рисуются только в режиме редактирования — чёрно-розовой
    // шахматкой по форме AABB вместо реального содержимого (SPEC.md 3.7):
    // стикер остаётся полностью интерактивным (см. `hit_sticker_at`), просто
    // не видно, что под ней.
    let mut order: Vec<&Sticker> = cfg
        .stickers
        .iter()
        .filter(|s| s.visible || edit.active)
        .collect();
    order.sort_by_key(|s| s.order);
    for sticker in order {
        if sticker.visible {
            if let Some((_, sprite)) = sprites.iter().find(|(id, _)| *id == sticker.id) {
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
                frame.push(solid_sprite(&tex, &monitor_id, &rect, 1.0));
            }
            Err(e) => {
                tracing::warn!(error = %e, "не удалось создать текстуру шахматки для скрытого стикера");
            }
        }
    }

    // Марка — под рамками выделения, только пока реально тянется (порог
    // протяжки, docs/M2_WIRING_PLAN.md, раздел 8/11).
    if let Some((ax, ay, cx, cy)) = edit.marquee {
        let visuals = marquee_visuals((ax, ay), (cx, cy));
        if let Some(tex) = ui_cache.fill_texture(renderer, theme::SLIDER_FILL) {
            if let Some(fill_rect) = &visuals.fill {
                frame.push(solid_sprite(
                    &tex,
                    &monitor_id,
                    fill_rect,
                    rst_render::MARQUEE_FILL_OPACITY,
                ));
            }
            for dash in &visuals.dashes {
                frame.push(solid_sprite(
                    &tex,
                    &monitor_id,
                    dash,
                    rst_render::MARQUEE_STROKE_OPACITY,
                ));
            }
        }
    }

    if edit.active {
        for id in edit.selection.ids() {
            let Some(sticker) = cfg.stickers.iter().find(|s| s.id == *id) else {
                continue;
            };
            let selection_box = SelectionBox::new(&sticker.placement, &sticker.transform);
            for rect in selection_box.all_rects() {
                frame.push(solid_sprite(white_tex, &monitor_id, &rect, 1.0));
            }
        }
    }

    // Тулбар и панель у курсора — над рамками выделения, под модалом
    // (раздел 11).
    if let Some(toolbar) = &edit.toolbar {
        let mut prims = Vec::new();
        toolbar.draw(&mut prims);
        primitives_to_sprites(
            &prims,
            ui_cache,
            renderer,
            &monitor_id,
            text_scale,
            &mut frame,
        );
    }
    if let Some(cursor_panel) = &edit.cursor_panel {
        let mut prims = Vec::new();
        cursor_panel.draw(&mut prims);
        primitives_to_sprites(
            &prims,
            ui_cache,
            renderer,
            &monitor_id,
            text_scale,
            &mut frame,
        );
    }

    // Модал подтверждения — самый верх (раздел 11).
    if let Some(confirm) = &edit.confirm {
        let mut prims = Vec::new();
        confirm.panel.draw(&mut prims);
        primitives_to_sprites(
            &prims,
            ui_cache,
            renderer,
            &monitor_id,
            text_scale,
            &mut frame,
        );
    }

    if let Err(e) = renderer.draw(&frame) {
        tracing::warn!(error = %e, "не удалось отрисовать кадр");
    }
}

/// Добавить стикер из файла на диске. `pasted` — источник
/// [`StickerSource::Pasted`] вместо [`StickerSource::File`] (материализованная
/// вставка из буфера, SPEC 2.1/2.5 — удаляется вместе с файлом,
/// `cleanup_pasted_file`); обычные файлы (диалог, `CF_HDROP`) — `false`.
/// Возвращает `true`, если стикер реально добавлен — вызывающий код решает
/// по этому флагу, стоит ли коммитить снимок undo (docs/M2_SLICE4_REVIEW.md,
/// пункт 7: неудачная загрузка не должна создавать пустой шаг истории).
fn add_sticker(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    path: PathBuf,
    pasted: bool,
) -> bool {
    let texture = match renderer.load_image(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось загрузить выбранное изображение");
            return false;
        }
    };
    let (w, h) = (texture.width(), texture.height());
    let (screen_w, screen_h) = overlay.size();
    // M3: реальный device interface path монитора; M1 — один монитор, заглушка.
    let sticker = if pasted {
        Sticker::new_pasted(
            path,
            MonitorId::default(),
            screen_w as f64 / 2.0,
            screen_h as f64 / 2.0,
            w as f64,
            h as f64,
        )
    } else {
        Sticker::new_file(
            path,
            MediaType::Image,
            MonitorId::default(),
            screen_w as f64 / 2.0,
            screen_h as f64 / 2.0,
            w as f64,
            h as f64,
        )
    };
    let sprite = Sprite::new(texture, sticker.placement.clone(), sticker.transform);
    let id = sticker.id;
    cfg.stickers.push(sticker);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после добавления стикера");
    }
    sprites.push((id, sprite));
    true
}
