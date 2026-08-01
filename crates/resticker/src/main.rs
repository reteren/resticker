//! resticker — точка входа.
//!
//! M0: логирование, конфиг, трей, окно настроек (Tauri, пустые вкладки),
//! автозапуск. Оверлей и остальные потоки — следующие вехи (ROADMAP.md).

mod logging;

use std::path::PathBuf;

use anyhow::Context;
use tauri::{Manager, WindowEvent};

use rst_win32::tray::{self, MenuItem, TrayEvent, TrayIcon};

const MENU_OPEN_SETTINGS: u32 = 1;
const MENU_TOGGLE_VISIBLE: u32 = 2;
const MENU_EXIT: u32 = 3;

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

    tauri::Builder::default()
        .manage(tray_icon)
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
                            // Стикеров ещё нет до M1 — переключателю нечего делать.
                            tracing::info!("показать/скрыть все: нет стикеров, no-op до M1");
                        }
                        TrayEvent::MenuItem(MENU_EXIT) => handle.exit(0),
                        TrayEvent::MenuItem(_) => {}
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
