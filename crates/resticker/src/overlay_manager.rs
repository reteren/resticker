//! Владеет оверлей-окном, рендерером и живым списком спрайтов на одном
//! потоке (ADR-013). Запускается один раз при старте. Команды приходят по
//! одному объединённому каналу с двух сторон (docs/M2_INTEGRATION_PLAN.md,
//! раздел 1): «добавить стикер» — из обработчика Tauri, события мыши/
//! клавиатуры/хоткея — из потока оверлей-окна.
//!
//! M2 (срез 1): вход/выход из режима редактирования по хоткею, затемнение
//! 50%, клик по стикеру/фону выбирает/снимает выделение, рамка с ручками
//! рисуется для выделенного стикера, `Esc` выходит из режима. Перетаскивание/
//! ресайз/поворот, undo/redo, буфер обмена и тулбар — следующий срез
//! (docs/M2_INTEGRATION_PLAN.md, раздел 17, шаги 5–8).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rst_core::config;
use rst_core::hittest;
use rst_core::model::{Config, MediaType, MonitorId, Sticker, StickerSource};
use rst_core::selection_set::SelectionSet;
use rst_render::{Renderer, Sprite, edit_overlay, solid_sprite};
use rst_win32::hotkey::HotkeyCombo;
use rst_win32::input::InputEvent;
use rst_win32::overlay::{OverlayEvent, OverlayWindow};
use uuid::Uuid;

/// Хоткей входа/выхода из режима редактирования по умолчанию (CONFIG.md),
/// если в конфиге он не задан или не парсится.
const DEFAULT_EDIT_HOTKEY: &str = "Ctrl+Alt+S";

/// Виртуальный код `VK_ESCAPE` (docs.microsoft.com/Virtual-Key-Codes) — выход
/// из режима редактирования. Константа, а не зависимость от `windows`: этот
/// крейт не работает с Win32-типами напрямую (CONTRIBUTING.md).
const VK_ESCAPE: u32 = 0x1B;

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

/// Состояние режима редактирования (docs/M2_INTEGRATION_PLAN.md, раздел 2).
/// Жесты (перетаскивание/ресайз/поворот) и undo — следующий срез.
struct EditState {
    active: bool,
    selection: SelectionSet,
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
    // через rst_core::config::load до вызова start()).
    let mut sprites: Vec<Sprite> = Vec::new();
    for sticker in &cfg.stickers {
        if let StickerSource::File { path, .. } = &sticker.source {
            match renderer.load_image(path) {
                Ok(texture) => {
                    sprites.push(Sprite::new(
                        texture,
                        sticker.placement.clone(),
                        sticker.transform,
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
            OverlayMessage::Command(OverlayCommand::Shutdown) => break,
            OverlayMessage::Event(OverlayEvent::ToggleEditMode) => {
                toggle_edit_mode(&overlay, &mut edit, &mut cfg, &config_path);
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
            OverlayMessage::Event(OverlayEvent::Key {
                vk: VK_ESCAPE,
                pressed: true,
                ..
            }) if edit.active => {
                toggle_edit_mode(&overlay, &mut edit, &mut cfg, &config_path);
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
            OverlayMessage::Event(OverlayEvent::Key { .. }) => {}
            OverlayMessage::Event(OverlayEvent::Input(event)) if edit.active => {
                if handle_input(event, scale, &cfg, &mut edit) {
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
            OverlayMessage::Event(OverlayEvent::Input(_)) => {
                // Вне режима редактирования окно клик-прозрачно — эти
                // события приходить не должны, но игнорируем на всякий случай.
            }
        }
    }
    // renderer и overlay освобождаются здесь в обратном порядке объявления:
    // сначала renderer (COM/DComp), затем overlay (окно) — корректный порядок.
}

fn toggle_edit_mode(
    overlay: &OverlayWindow,
    edit: &mut EditState,
    cfg: &mut Config,
    config_path: &std::path::Path,
) {
    edit.active = !edit.active;
    overlay.set_click_through(!edit.active);
    if !edit.active {
        edit.selection.clear();
        if let Err(e) = config::save(cfg, config_path) {
            tracing::warn!(error = %e, "не удалось сохранить config.json при выходе из режима редактирования");
        }
    }
}

/// Обработать событие мыши в режиме редактирования. Возвращает `true`, если
/// нужна перерисовка. Перетаскивание/ресайз/поворот — следующий срез
/// (docs/M2_INTEGRATION_PLAN.md, разделы 6, 8); здесь — только выбор/снятие
/// выделения кликом (§6 «MouseDown»/«StickerBody»/«Background»).
fn handle_input(event: InputEvent, scale: f32, cfg: &Config, edit: &mut EditState) -> bool {
    match event {
        InputEvent::MouseDown { pos, .. } => {
            let dip_x = pos.x as f64 / scale as f64;
            let dip_y = pos.y as f64 / scale as f64;
            let hit = hit_sticker_at(cfg, dip_x, dip_y);
            let before = edit.selection.ids().to_vec();
            edit.selection.click(hit);
            before != edit.selection.ids()
        }
        InputEvent::MouseMove { .. } | InputEvent::MouseUp { .. } | InputEvent::CaptureLost => {
            // Жесты (перетаскивание/ресайз/поворот) — следующий срез.
            false
        }
    }
}

/// Верхний (по `order`) видимый стикер под точкой `(dip_x, dip_y)`, если есть.
fn hit_sticker_at(cfg: &Config, dip_x: f64, dip_y: f64) -> Option<Uuid> {
    cfg.stickers
        .iter()
        .filter(|s| s.visible && hittest::contains(&s.placement, &s.transform, dip_x, dip_y))
        .max_by_key(|s| s.order)
        .map(|s| s.id)
}

/// Собрать и отрисовать кадр (ADR-006 — только по событию): затемнение (если
/// активен режим редактирования), стикеры, затем рамка выделения поверх
/// (docs/M2_INTEGRATION_PLAN.md, раздел 11 — тулбар и панель добавятся во
/// втором срезе M2).
#[allow(clippy::too_many_arguments)]
fn redraw(
    renderer: &mut Renderer,
    sprites: &[Sprite],
    cfg: &Config,
    edit: &EditState,
    white_tex: &rst_render::Texture,
    black_tex: &rst_render::Texture,
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

    frame.extend_from_slice(sprites);

    if edit.active {
        for id in edit.selection.ids() {
            let Some(sticker) = cfg.stickers.iter().find(|s| s.id == *id) else {
                continue;
            };
            let selection_box =
                rst_render::SelectionBox::new(&sticker.placement, &sticker.transform);
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
    config_path: &std::path::Path,
    sprites: &mut Vec<Sprite>,
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
    cfg.stickers.push(sticker);
    if let Err(e) = config::save(cfg, config_path) {
        tracing::warn!(error = %e, "не удалось сохранить config.json после добавления стикера");
    }
    sprites.push(sprite);
}
