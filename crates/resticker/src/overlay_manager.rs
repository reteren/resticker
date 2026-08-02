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
use rst_render::{Box2D, Renderer, SelectionBox, Sprite, Texture, edit_overlay, solid_sprite};
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::input::{
    Corner as Win32Corner, CursorShape, CursorZone, Handle as Win32Handle, InputEvent, Modifiers,
};
use rst_win32::overlay::{OverlayEvent, OverlayWindow};
use uuid::Uuid;

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

/// Кольцо поворота — зона за угловой ручкой (SPEC 3.3): от `ROTATE_RING_MIN_DIP`
/// (сразу за телом стикера) до `ROTATE_RING_MAX_DIP` от центра ручки.
const ROTATE_RING_MIN_INFLATE_DIP: f64 = 6.0;
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
}

impl Gesture {
    fn start(&self) -> &GestureStart {
        match self {
            Gesture::Drag { start, .. } => start,
            Gesture::Resize { start, .. } => start,
            Gesture::Rotate { start, .. } => start,
        }
    }
}

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
    };

    redraw(
        &mut renderer,
        &sprites,
        &cfg,
        &edit,
        &white_tex,
        &black_tex,
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
                toggle_edit_mode(&overlay, &mut edit, &mut cfg, &config_path);
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
                );
            }
            OverlayMessage::Event(OverlayEvent::Key { .. }) => {}
            OverlayMessage::Event(OverlayEvent::Input(event)) if edit.active => {
                need_redraw = handle_input(
                    event,
                    scale,
                    &overlay,
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
    config_path: &Path,
) {
    edit.active = !edit.active;
    overlay.set_click_through(!edit.active);
    edit.gesture = None;
    if !edit.active {
        edit.selection.clear();
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json при выходе из режима редактирования");
        }
    }
}

/// Сохранить текущий `cfg` в историю undo перед мутирующим действием
/// (жест, удаление, дублирование) и очистить историю redo — новая ветка
/// истории (семантика как у `rst_core::undo::UndoStack::push`).
fn push_undo_snapshot(edit: &mut EditState, cfg: &Config) {
    if edit.undo_stack.len() == UNDO_CAPACITY {
        edit.undo_stack.remove(0);
    }
    edit.undo_stack.push(cfg.clone());
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
    edit.undo_stack.push(std::mem::replace(cfg, next));
    resync_sprites(renderer, cfg, sprites);
    edit.selection.prune(&cfg.stickers);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после повтора");
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
) -> bool {
    match vk {
        VK_ESCAPE => {
            toggle_edit_mode(overlay, edit, cfg, config_path);
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
            if edit.selection.is_empty() {
                return false;
            }
            push_undo_snapshot(edit, cfg);
            for id in edit.selection.ids().to_vec() {
                let _ = ops::delete(cfg, id);
            }
            edit.selection.prune(&cfg.stickers);
            resync_sprites(renderer, cfg, sprites);
            if let Err(e) = config::save(cfg, config_path) {
                tracing::warn!(error = %e, "не удалось сохранить config.json после удаления");
            }
            true
        }
        VK_D if modifiers.ctrl => {
            if edit.selection.is_empty() {
                return false;
            }
            push_undo_snapshot(edit, cfg);
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
            let outside_body = !hittest::contains_inflated(
                &sticker.placement,
                &sticker.transform,
                dip_x,
                dip_y,
                ROTATE_RING_MIN_INFLATE_DIP,
            );
            if outside_body {
                for corner in CoreCorner::ALL {
                    let (hx, hy) = sbox.handle_center(corner.handle());
                    let dist = (dip_x - hx).hypot(dip_y - hy);
                    if dist <= ROTATE_RING_MAX_DIP {
                        return Zone::Rotate(*id, corner);
                    }
                }
            }
            for (kind, rect) in sbox.handle_rects(rst_render::HANDLE_SIZE_DIP) {
                if point_in_box2d(&rect, dip_x, dip_y) {
                    return Zone::ResizeHandle(*id, kind);
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
    overlay_size: (u32, u32),
    cfg: &mut Config,
    config_path: &Path,
    sprites: &mut [(Uuid, Sprite)],
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
            let zone = resolve_zone(cfg, &edit.selection, dip_x, dip_y);
            match zone {
                Zone::Background => {
                    let before = edit.selection.ids().to_vec();
                    edit.selection.click(None);
                    before != edit.selection.ids()
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
                        push_undo_snapshot(edit, cfg);
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
                        push_undo_snapshot(edit, cfg);
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
                        push_undo_snapshot(edit, cfg);
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
            if !dragging {
                let zone = resolve_zone(cfg, &edit.selection, dip_x, dip_y);
                overlay.post_cursor_shape(cursor_shape_for_zone(&zone));
                return false;
            }
            apply_gesture(cfg, sprites, edit, (dip_x, dip_y), modifiers, monitor)
        }
        InputEvent::MouseUp { .. } => {
            if edit.gesture.take().is_some() {
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
            // раздел 6 — "CaptureLost -> отменить жест без push").
            match edit.gesture.take() {
                Some(gesture) => {
                    let start = gesture.start();
                    apply_transform(
                        cfg,
                        sprites,
                        start.id,
                        start.placement.clone(),
                        start.transform,
                    );
                    // Жест не завершился — снимок, сделанный на MouseDown,
                    // не понадобится (иначе Ctrl+Z отменял бы no-op).
                    edit.undo_stack.pop();
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
    width_px: u32,
    height_px: u32,
    scale: f32,
) {
    let mut frame: Vec<Sprite> = Vec::with_capacity(sprites.len() + 1 + 12);
    let monitor_id = MonitorId::default();

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
