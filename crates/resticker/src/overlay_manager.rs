//! Владеет оверлей-окном, рендерером и живым списком спрайтов на одном
//! потоке (ADR-013). Запускается один раз при старте; команда «добавить
//! стикер» приходит через канал из обработчика Tauri.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rst_core::config;
use rst_core::model::{Config, MediaType, MonitorId, Sticker, StickerSource};
use rst_render::{Renderer, Sprite};
use rst_win32::overlay::OverlayWindow;

pub enum OverlayCommand {
    AddSticker(PathBuf),
    Shutdown,
}

/// Ручка для отправки команд оверлей-потоку; `Drop` останавливает поток.
pub struct OverlayHandle {
    tx: Sender<OverlayCommand>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayHandle {
    pub fn send(&self, cmd: OverlayCommand) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(OverlayCommand::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Запустить оверлей-окно + рендерер на отдельном потоке. `cfg` передаётся
/// по значению — существующие стикеры (сохранение/восстановление между
/// запусками, ROADMAP.md M1) загружаются сразу в первый кадр.
pub fn start(config_path: PathBuf, cfg: Config) -> OverlayHandle {
    let (tx, rx) = mpsc::channel::<OverlayCommand>();
    let thread = thread::spawn(move || run(config_path, cfg, rx));
    OverlayHandle {
        tx,
        thread: Some(thread),
    }
}

fn run(config_path: PathBuf, mut cfg: Config, rx: Receiver<OverlayCommand>) {
    let overlay = match OverlayWindow::create() {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать оверлей-окно");
            return;
        }
    };
    let (width, height) = overlay.size();
    let mut renderer = match Renderer::new(overlay.hwnd(), width, height) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "не удалось создать рендерер");
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
    if let Err(e) = renderer.draw(&sprites) {
        tracing::warn!(error = %e, "не удалось отрисовать стартовый кадр");
    }

    for cmd in rx {
        match cmd {
            OverlayCommand::AddSticker(path) => {
                add_sticker(
                    &overlay,
                    &mut renderer,
                    &mut cfg,
                    &config_path,
                    &mut sprites,
                    path,
                );
            }
            OverlayCommand::Shutdown => break,
        }
    }
    // renderer и overlay освобождаются здесь в обратном порядке объявления:
    // сначала renderer (COM/DComp), затем overlay (окно) — корректный порядок.
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
    if let Err(e) = renderer.draw(sprites) {
        tracing::warn!(error = %e, "не удалось отрисовать кадр после добавления стикера");
    }
}
