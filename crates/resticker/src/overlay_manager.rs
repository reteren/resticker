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
//! `Ctrl+Z`/`Ctrl+Shift+Z`/`Ctrl+Y`, `Ctrl+A`, `Delete`, `Ctrl+D`. `Ctrl+V`,
//! мультивыделение и тулбар — следующий срез (docs/M2_INTEGRATION_PLAN.md,
//! раздел 17, шаги 7–8). `Delete` пока без диалога подтверждения — удаляет
//! сразу (страхуется через `Ctrl+Z`); диалог — часть тулбара/панели.

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
use rst_render::{
    Box2D, Button, Panel, PointerEvent, Primitive, Renderer, SelectionBox, Sprite, Texture,
    edit_overlay, marquee_visuals, rasterize, solid_sprite, theme,
};
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::input::{
    Corner as Win32Corner, CursorShape, CursorZone, Handle as Win32Handle, InputEvent, Modifiers,
};
use rst_win32::overlay::{OverlayEvent, OverlayWindow};
use uuid::Uuid;

use crate::confirm_dialog;

/// Хоткей входа/выхода из режима редактирования по умолчанию (CONFIG.md),
/// если в конфиге он не задан или не парсится.
const DEFAULT_EDIT_HOTKEY: &str = "Ctrl+Alt+S";

/// Виртуальный код `VK_ESCAPE` (docs.microsoft.com/Virtual-Key-Codes) — выход
/// из режима редактирования. Константа, а не зависимость от `windows`: этот
/// крейт не работает с Win32-типами напрямую (CONTRIBUTING.md).
const VK_ESCAPE: u32 = 0x1B;
const VK_DELETE: u32 = 0x2E;
const VK_A: u32 = 0x41;
const VK_D: u32 = 0x44;
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
pub fn start(config_path: PathBuf, cfg: Config) -> OverlayHandle {
    let (tx, rx) = mpsc::channel::<OverlayMessage>();
    let thread = thread::spawn({
        let tx = tx.clone();
        move || run(config_path, cfg, tx, rx)
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
    /// `GestureStart` и не откатывается через `apply_transform`.
    Marquee { anchor: (f64, f64) },
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
}

impl UiTextureCache {
    fn new() -> Self {
        Self {
            fills: HashMap::new(),
            texts: HashMap::new(),
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
}

/// Перевести примитивы панели (`Panel::draw`) в спрайты кадра, используя кэш
/// текстур (docs/M2_WIRING_PLAN.md, раздел 3). Иконок-ассетов в этом срезе
/// нет — плейсхолдер заливкой `theme::BUTTON_BG` (раздел 14).
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
            Primitive::Icon { rect, opacity, .. } => {
                if let Some(tex) = cache.fill_texture(renderer, theme::BUTTON_BG) {
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
) {
    let hotkey = cfg
        .hotkeys
        .edit_mode
        .as_deref()
        .and_then(|s| HotkeyCombo::parse(s).ok())
        .or_else(|| HotkeyCombo::parse(DEFAULT_EDIT_HOTKEY).ok())
        .expect("DEFAULT_EDIT_HOTKEY — валидная комбинация");

    let (overlay, events) = match OverlayWindow::create(hotkey) {
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
        if let StickerSource::File { path, .. } = &sticker.source {
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
                );
                need_redraw = true;
            }
            OverlayMessage::Command(OverlayCommand::Shutdown) => break,
            OverlayMessage::Event(OverlayEvent::ToggleEditMode) => {
                toggle_edit_mode(&overlay, &mut edit, &mut cfg, &mut sprites, &config_path);
                need_redraw = true;
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
                    &renderer,
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
                    &renderer,
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

fn toggle_edit_mode(
    overlay: &OverlayWindow,
    edit: &mut EditState,
    cfg: &mut Config,
    sprites: &mut [(Uuid, Sprite)],
    config_path: &Path,
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
        if let StickerSource::File { path, .. } = &sticker.source {
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
    // Жест не мог быть активен здесь (Ctrl+Z игнорируется, пока
    // `edit.gesture.is_some()`, см. `handle_key`), но снимаем защитно —
    // docs/M2_SLICE_REVIEW.md, пункт 5.
    edit.gesture = None;
    edit.pending_snapshot = None;
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
            let _ = ops::delete(cfg, id);
        }
        edit.selection.prune(&cfg.stickers);
        resync_sprites(renderer, cfg, sprites);
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json после удаления");
        }
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
    renderer: &Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    edit: &mut EditState,
    overlay_size: (u32, u32),
    scale: f32,
) -> bool {
    // История/удаление/дублирование во время активного жеста мутировали бы
    // cfg из-под него — жест продолжал бы считать от своего старого
    // GestureStart поверх уже изменённого состояния (docs/M2_SLICE_REVIEW.md,
    // пункт 2). Esc — исключение: toggle_edit_mode сам корректно откатывает
    // незавершённый жест перед выходом.
    if edit.gesture.is_some() && vk != VK_ESCAPE {
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
    match vk {
        VK_ESCAPE => {
            toggle_edit_mode(overlay, edit, cfg, sprites, config_path);
            true
        }
        VK_Z if modifiers.ctrl && modifiers.shift => {
            perform_redo(renderer, cfg, config_path, sprites, edit)
        }
        VK_Z if modifiers.ctrl => perform_undo(renderer, cfg, config_path, sprites, edit),
        VK_Y if modifiers.ctrl => perform_redo(renderer, cfg, config_path, sprites, edit),
        VK_A if modifiers.ctrl => {
            edit.selection.select_all(&cfg.stickers);
            true
        }
        VK_DELETE => {
            let center = edit
                .selection
                .bounds(&cfg.stickers)
                .map(|r| (r.x + r.w / 2.0, r.y + r.h / 2.0))
                .unwrap_or((
                    overlay_size.0 as f64 / 2.0 / scale as f64,
                    overlay_size.1 as f64 / 2.0 / scale as f64,
                ));
            begin_delete(edit, renderer, cfg, config_path, sprites, center)
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
            true
        }
        _ => false,
    }
}

fn to_dip(pos: rst_win32::input::Point, scale: f32) -> (f64, f64) {
    (pos.x as f64 / scale as f64, pos.y as f64 / scale as f64)
}

/// Верхний (по `order`) видимый стикер под точкой `(dip_x, dip_y)`, если есть.
fn hit_sticker_at(cfg: &Config, dip_x: f64, dip_y: f64) -> Option<Uuid> {
    cfg.stickers
        .iter()
        .filter(|s| s.visible && hittest::contains(&s.placement, &s.transform, dip_x, dip_y))
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

/// Обработать событие мыши в режиме редактирования. Возвращает `true`, если
/// нужна перерисовка (docs/M2_INTEGRATION_PLAN.md, раздел 6/8).
#[allow(clippy::too_many_arguments)]
fn handle_input(
    event: InputEvent,
    scale: f32,
    overlay: &OverlayWindow,
    renderer: &Renderer,
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
        InputEvent::MouseDown { pos, .. } => {
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
            let zone = resolve_zone(cfg, &edit.selection, dip_x, dip_y);
            match zone {
                Zone::Background => {
                    // Решение «клик или марка» откладывается до `MouseUp`/
                    // порога протяжки (docs/M2_WIRING_PLAN.md, раздел 8) —
                    // выделение здесь ещё не трогаем.
                    edit.gesture = Some(Gesture::Marquee {
                        anchor: (dip_x, dip_y),
                    });
                    edit.marquee_started = false;
                    false
                }
                Zone::StickerBody(id) => {
                    let before = edit.selection.ids().to_vec();
                    edit.selection.click(Some(id));
                    let changed = before != edit.selection.ids();
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
            if let Some(confirm) = &mut edit.confirm {
                confirm.panel.pointer_event(PointerEvent::Move {
                    pos: (dip_x, dip_y),
                });
                return true;
            }
            if !dragging {
                let zone = resolve_zone(cfg, &edit.selection, dip_x, dip_y);
                overlay.post_cursor_shape(cursor_shape_for_zone(&zone));
                return false;
            }
            apply_gesture(cfg, sprites, edit, (dip_x, dip_y), modifiers, monitor)
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
                    commit_undo_snapshot(edit, confirm.snapshot);
                    for id in &confirm.ids {
                        let _ = ops::delete(cfg, *id);
                    }
                    edit.selection.prune(&cfg.stickers);
                    resync_sprites(renderer, cfg, sprites);
                    if let Err(e) = config::save(cfg, config_path) {
                        tracing::warn!(error = %e, "не удалось сохранить config.json после удаления через диалог");
                    }
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
                }
                // `ID_CANCEL`, клик по сообщению или мимо модала: закрыть без
                // изменений — снимок не коммитился, отбрасываем вместе с `confirm`.
                return true;
            }
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
            // Отменить незавершённый жест без сохранения: откатить модель и
            // спрайт к стартовому снимку (docs/M2_INTEGRATION_PLAN.md,
            // раздел 6 — "CaptureLost -> отменить жест без push"). У марки
            // (`Gesture::start() == None`) откатывать в модели нечего —
            // только сбросить визуал (docs/M2_WIRING_PLAN.md, раздел 8).
            match edit.gesture.take() {
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
    if let Some(Gesture::Marquee { anchor }) = &edit.gesture {
        let anchor = *anchor;
        let rect = DipRect::new(anchor.0, anchor.1, dip_x - anchor.0, dip_y - anchor.1);
        if rect.w.abs() >= MARQUEE_THRESHOLD_DIP || rect.h.abs() >= MARQUEE_THRESHOLD_DIP {
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

    // Порядок отрисовки стикеров — по `order` (больше — выше, CONFIG.md), а
    // не по порядку загрузки: важно, как только доступен UI z-order (M2,
    // следующий срез — тулбар).
    let mut order: Vec<&Sticker> = cfg.stickers.iter().filter(|s| s.visible).collect();
    order.sort_by_key(|s| s.order);
    for sticker in order {
        if let Some((_, sprite)) = sprites.iter().find(|(id, _)| *id == sticker.id) {
            frame.push(sprite.clone());
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

fn add_sticker(
    overlay: &OverlayWindow,
    renderer: &mut Renderer,
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut Vec<(Uuid, Sprite)>,
    path: PathBuf,
) {
    let texture = match renderer.load_image(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "не удалось загрузить выбранное изображение");
            return;
        }
    };
    let (w, h) = (texture.width(), texture.height());
    let (screen_w, screen_h) = overlay.size();
    // M3: реальный device interface path монитора; M1 — один монитор, заглушка.
    let sticker = Sticker::new_file(
        path,
        MediaType::Image,
        MonitorId::default(),
        screen_w as f64 / 2.0,
        screen_h as f64 / 2.0,
        w as f64,
        h as f64,
    );
    let sprite = Sprite::new(texture, sticker.placement.clone(), sticker.transform);
    let id = sticker.id;
    cfg.stickers.push(sticker);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после добавления стикера");
    }
    sprites.push((id, sprite));
}
