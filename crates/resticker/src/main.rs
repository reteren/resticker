//! resticker — точка входа.
//!
//! M0: логирование, конфиг, трей, окно настроек (Tauri, пустые вкладки),
//! автозапуск. M1: оверлей-окно + рендерер на своём потоке
//! (`overlay_manager`), добавление стикера из настроек, восстановление
//! между запусками. Остальное — следующие вехи (ROADMAP.md).

mod confirm_dialog;
mod cursor_panel;
mod logging;
mod overlay_manager;
mod toolbar;
mod window_picker;

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Context;
use tauri::{Manager, WindowEvent};

use overlay_manager::{CoordinatorRequest, OverlayCommand, OverlayHandle};
use rst_win32::tray::{self, MenuItem, TrayEvent, TrayIcon};

const MENU_OPEN_SETTINGS: u32 = 1;
const MENU_TOGGLE_VISIBLE: u32 = 2;
const MENU_EXIT: u32 = 3;

/// Добавить стикер по пути, выбранному в диалоге настроек (M1).
#[tauri::command]
fn add_sticker(path: String, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::AddSticker(PathBuf::from(path)));
}

fn config_path() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("APPDATA").context("переменная APPDATA не задана")?;
    Ok(PathBuf::from(base).join("resticker").join("config.json"))
}

fn main() -> anyhow::Result<()> {
    let log_path = logging::init()?;
    tracing::info!(path = %log_path.display(), "логирование инициализировано");

    let cfg_path = config_path()?;
    let loaded = rst_core::config::load(&cfg_path).context("загрузка config.json")?;
    if let Some(warning) = &loaded.warning {
        tracing::warn!(?warning, "config.json загружен нештатно");
    }
    let cfg = loaded.config;
    // ROADMAP.md M0: "читает и пишет конфиг" — гарантируем файл на диске
    // сразу при старте (первый запуск создаёт config.json с дефолтами).
    rst_core::config::save(&cfg, &cfg_path).context("сохранение config.json")?;

    // config.json — источник истины для UI-настройки автозапуска; реестр
    // синхронизируется с ним при каждом старте.
    let exe = std::env::current_exe().context("current_exe")?;
    if let Err(e) = rst_win32::autostart::set_enabled(cfg.settings.autostart, &exe) {
        tracing::warn!(error = %e, "не удалось синхронизировать автозапуск");
    }

    let (tray_icon, tray_rx) = TrayIcon::new(
        "resticker",
        vec![
            MenuItem {
                id: MENU_OPEN_SETTINGS,
                label: "Открыть настройки".into(),
            },
            tray::separator(),
            MenuItem {
                id: MENU_TOGGLE_VISIBLE,
                label: "Показать/скрыть все стикеры".into(),
            },
            tray::separator(),
            MenuItem {
                id: MENU_EXIT,
                label: "Выход".into(),
            },
        ],
    )
    .context("инициализация иконки трея")?;

    let silent_start = cfg.settings.silent_start;
    // Обратный канал координатор → main (docs/M2_WIRING_PLAN.md, раздел 12):
    // `Sender` уходит в координатор (оверлей-поток), `Receiver` читает поток
    // из `setup` ниже.
    let (coordinator_tx, coordinator_rx) = mpsc::channel::<CoordinatorRequest>();
    let overlay_handle = overlay_manager::start(cfg_path, cfg, coordinator_tx);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tray_icon)
        .manage(overlay_handle)
        .invoke_handler(tauri::generate_handler![add_sticker])
        .setup(move |app| {
            if !silent_start {
                if let Some(w) = app.get_webview_window("settings") {
                    let _ = w.show();
                }
            }

            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for event in tray_rx {
                    match event {
                        TrayEvent::MenuItem(MENU_OPEN_SETTINGS) | TrayEvent::Activate => {
                            if let Some(w) = handle.get_webview_window("settings") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        TrayEvent::MenuItem(MENU_TOGGLE_VISIBLE) => {
                            // Массовый переключатель видимости — M2 (тулбар/состояние
                            // редактирования); пункт меню уже есть, поведение — позже.
                            tracing::info!("показать/скрыть все: пока no-op, реализация в M2");
                        }
                        TrayEvent::MenuItem(MENU_EXIT) => handle.exit(0),
                        TrayEvent::MenuItem(_) => {}
                    }
                }
            });

            // Запросы координатора, которым нужен Tauri (docs/M2_WIRING_PLAN.md,
            // раздел 12): окна живут на главном потоке, оверлей-поток их
            // трогать не может. Обработка — та же, что у пункта трея.
            let coordinator_handle = app.handle().clone();
            std::thread::spawn(move || {
                for request in coordinator_rx {
                    match request {
                        CoordinatorRequest::OpenSettings => {
                            if let Some(w) = coordinator_handle.get_webview_window("settings") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Закрытие окна настроек прячет его, а не завершает процесс —
            // приложение живёт в трее (SPEC.md, раздел 12).
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .context("запуск приложения Tauri")?;

    Ok(())
}
