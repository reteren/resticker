//! resticker — точка входа.
//!
//! M0: логирование, конфиг, трей, окно настроек (Tauri, пустые вкладки),
//! автозапуск. M1: оверлей-окно + рендерер на своём потоке
//! (`overlay_manager`), добавление стикера из настроек, восстановление
//! между запусками. Остальное — следующие вехи (ROADMAP.md).

// Без этого бинарник линкуется под CONSOLE-подсистему (дефолт для `fn main()`
// без GUI-фреймворка, который сам берёт это на себя) — каждый запуск
// открывал бы пустое консольное окно поверх трея/оверлея (найдено на живой
// машине пользователя: выглядело как «зависшая» программа). В debug-сборке
// подсистема остаётся консольной — `cargo run` показывает panic/eprintln
// напрямую, не только через файл лога.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod confirm_dialog;
mod cursor_panel;
mod i18n;
mod logging;
mod overlay_manager;
mod preset_picker;
mod toolbar;
mod window_pick_list;
mod window_picker;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use anyhow::Context;
use rst_core::model::{Config, Hotkeys, OverlapRule, Settings};
use tauri::{Emitter, Manager, WindowEvent};
use uuid::Uuid;

use overlay_manager::{CoordinatorRequest, OverlayCommand, OverlayHandle};
use rst_win32::tray::{self, MenuItem, TrayEvent, TrayIcon};

const MENU_OPEN_SETTINGS: u32 = 1;
const MENU_TOGGLE_VISIBLE: u32 = 2;
const MENU_EXIT: u32 = 3;
/// Первый id пункта меню трея под пресет (M7, «быстрое переключение из
/// трея» — ROADMAP.md). `WM_COMMAND` несёт id только в младшем слове
/// `wParam` (Win32-соглашение, `tray.rs::wndproc` берёт `wparam.0 & 0xffff`)
/// — 16 бит, поэтому пункт кодирует не сам `Uuid` пресета, а его индекс в
/// списке на момент последней пересборки меню; обратное соответствие —
/// `preset_ids` ниже.
const MENU_PRESET_BASE: u32 = 100;

/// Собрать пункты меню трея из текущего списка пресетов (M7, «быстрое
/// переключение из трея» — ROADMAP.md; SPEC.md §12: «пресеты (подменю)») —
/// постоянные пункты + вложенное подменю «Пресеты» (пусто — не добавляется).
/// Возвращает и сами пункты, и id-список пресетов в том же порядке, что и
/// пункты подменю: `preset_ids[i]` — это пресет пункта с
/// `id == MENU_PRESET_BASE + i` (см. `MENU_PRESET_BASE`).
/// `lang` — `cfg.settings.language`, захваченный один раз при старте
/// resticker (см. вызовы в `main()`): как и хоткеи, смена языка в окне
/// настроек применяется к меню трея после перезапуска, не мгновенно
/// (`crates/resticker/src/i18n.rs`, доккомент модуля).
fn build_tray_menu(presets: &[(Uuid, String)], lang: &str) -> (Vec<MenuItem>, Vec<Uuid>) {
    let mut items = vec![
        MenuItem::new(MENU_OPEN_SETTINGS, i18n::tray_open_settings(lang)),
        tray::separator(),
        MenuItem::new(MENU_TOGGLE_VISIBLE, i18n::tray_toggle_visible(lang)),
    ];
    let ids: Vec<Uuid> = presets.iter().map(|(id, _)| *id).collect();
    if !ids.is_empty() {
        let children = presets
            .iter()
            .enumerate()
            .map(|(i, (_, name))| MenuItem::new(MENU_PRESET_BASE + i as u32, name.clone()))
            .collect();
        items.push(tray::separator());
        items.push(MenuItem::submenu(
            i18n::tray_presets_submenu(lang),
            children,
        ));
    }
    items.push(tray::separator());
    items.push(MenuItem::new(MENU_EXIT, i18n::tray_exit(lang)));
    (items, ids)
}

/// Добавить стикер по пути, выбранному в диалоге настроек (M1).
#[tauri::command]
fn add_sticker(path: String, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::AddSticker(PathBuf::from(path)));
}

/// Прочитать текущий `config.json` для окна настроек. Читается прямо с
/// диска (не запрашивается у координатора) — координатор сохраняет его
/// сразу после каждой своей мутации `cfg` (add_sticker/drag/undo и т.д.),
/// поэтому свежий файл на диске и есть текущее состояние; отдельный
/// query-канал в координатор не нужен ради этого чтения (README M2_WIRING).
#[tauri::command]
fn get_config(config_path: tauri::State<PathBuf>) -> Result<Config, String> {
    rst_core::config::load(&config_path)
        .map(|loaded| loaded.config)
        .map_err(|e| {
            tracing::warn!(error = %e, "get_config: не удалось загрузить config.json");
            friendly_config_error(&e)
        })
}

/// Понятный пользователю текст ошибки конфига вместо кода/сырого текста
/// (ROADMAP.md M8, «понятные тексты ошибок вместо кодов»): `CoreError`
/// оборачивает `std::io::Error`/`serde_json::Error`, чей `Display` — это
/// текст ОС/парсера ("The system cannot find the file specified. (os error
/// 2)", "expected value at line 3 column 1") — годится для лога
/// (`tracing::warn!`), но не для показа в окне настроек. Полный `e`
/// логируется рядом в каждом вызывающем коде — эта функция только для
/// текста, который видит пользователь.
fn friendly_config_error(e: &rst_core::CoreError) -> String {
    match e {
        rst_core::CoreError::Io(io) => match io.kind() {
            std::io::ErrorKind::NotFound => "Файл настроек не найден.".to_string(),
            std::io::ErrorKind::PermissionDenied => {
                "Нет доступа к файлу настроек — проверьте права на папку AppData.".to_string()
            }
            _ => "Не удалось прочитать файл настроек.".to_string(),
        },
        rst_core::CoreError::Json(_) | rst_core::CoreError::Migrate(_) => {
            "Файл настроек повреждён.".to_string()
        }
        rst_core::CoreError::UnknownSchemaVersion(_) => {
            "Файл настроек создан более новой версией resticker — обновите программу.".to_string()
        }
        rst_core::CoreError::NotAnObject => "Файл настроек повреждён.".to_string(),
    }
}

/// Заменить `cfg.settings` целиком (вкладка «Общие») — координатор
/// остаётся единственным писателем `config.json` (доккомент
/// `OverlayCommand::UpdateSettings`).
#[tauri::command]
fn update_settings(settings: Settings, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::UpdateSettings(settings));
}

/// Заменить `cfg.hotkeys` целиком (вкладка «Управление») — применяется
/// после перезапуска resticker (доккомент `OverlayCommand::UpdateHotkeys`).
#[tauri::command]
fn update_hotkeys(hotkeys: Hotkeys, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::UpdateHotkeys(hotkeys));
}

fn parse_id(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|e| {
        tracing::warn!(error = %e, id, "parse_id: некорректный идентификатор от UI");
        "Внутренняя ошибка: некорректный идентификатор элемента.".to_string()
    })
}

#[tauri::command]
fn set_sticker_enabled(
    id: String,
    enabled: bool,
    overlay: tauri::State<OverlayHandle>,
) -> Result<(), String> {
    overlay.send(OverlayCommand::SetStickerEnabled(parse_id(&id)?, enabled));
    Ok(())
}

#[tauri::command]
fn delete_sticker(id: String, overlay: tauri::State<OverlayHandle>) -> Result<(), String> {
    overlay.send(OverlayCommand::DeleteSticker(parse_id(&id)?));
    Ok(())
}

#[tauri::command]
fn reset_sticker_position(id: String, overlay: tauri::State<OverlayHandle>) -> Result<(), String> {
    overlay.send(OverlayCommand::ResetStickerPosition(parse_id(&id)?));
    Ok(())
}

#[tauri::command]
fn reset_sticker_transform(id: String, overlay: tauri::State<OverlayHandle>) -> Result<(), String> {
    overlay.send(OverlayCommand::ResetStickerTransform(parse_id(&id)?));
    Ok(())
}

#[tauri::command]
fn relink_sticker(
    id: String,
    path: String,
    overlay: tauri::State<OverlayHandle>,
) -> Result<(), String> {
    overlay.send(OverlayCommand::RelinkSticker(
        parse_id(&id)?,
        PathBuf::from(path),
    ));
    Ok(())
}

#[tauri::command]
fn reset_all_stickers(overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::ResetAllStickers);
}

#[tauri::command]
fn delete_all_stickers(overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::DeleteAllStickers);
}

/// M7 (SPEC.md §11): сохранить текущую расстановку стикеров как новый
/// пресет.
#[tauri::command]
fn save_preset(name: String, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::SavePreset(name));
}

/// Применить пресет. Недостающие элементы (SPEC §11, «Загрузка с
/// недостающими элементами») координатор пришлёт отдельно, событием
/// `preset-missing-elements` в окно настроек (см. `CoordinatorRequest`
/// обработчик в `main`) — этот вызов ничего не возвращает синхронно,
/// координатор — единственный писатель `cfg`, а Tauri-поток не может
/// дождаться результата без отдельного request/reply канала, которого
/// здесь нет (тот же принцип, что у всех остальных мутирующих команд).
#[tauri::command]
fn apply_preset(id: String, overlay: tauri::State<OverlayHandle>) -> Result<(), String> {
    overlay.send(OverlayCommand::ApplyPreset(parse_id(&id)?));
    Ok(())
}

#[tauri::command]
fn rename_preset(
    id: String,
    name: String,
    overlay: tauri::State<OverlayHandle>,
) -> Result<(), String> {
    overlay.send(OverlayCommand::RenamePreset(parse_id(&id)?, name));
    Ok(())
}

#[tauri::command]
fn delete_preset(id: String, overlay: tauri::State<OverlayHandle>) -> Result<(), String> {
    overlay.send(OverlayCommand::DeletePreset(parse_id(&id)?));
    Ok(())
}

/// `path` — уже выбранный пользователем путь (диалог сохранения — на
/// стороне JS, `tauri_plugin_dialog`, тот же паттерн, что у остальных
/// путевых команд этого файла). Предупреждение про персональные данные в
/// файле (SPEC §11) — обязанность UI-слоя ДО вызова этой команды, здесь не
/// дублируется.
#[tauri::command]
fn export_preset(
    id: String,
    path: String,
    overlay: tauri::State<OverlayHandle>,
) -> Result<(), String> {
    overlay.send(OverlayCommand::ExportPreset(
        parse_id(&id)?,
        PathBuf::from(path),
    ));
    Ok(())
}

#[tauri::command]
fn import_preset(path: String, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::ImportPreset(PathBuf::from(path)));
}

/// Вкладка «Денй-лист» (SPEC.md, «Закрепление окна»): добавить правило —
/// совпавшие окна нельзя закрепить хоткеем и не видно в списке выбора.
/// `process_name` обязателен (UI не даёт отправить без него), `title_pattern`
/// опционален ('*' — подстановка, CONFIG.md «Совпадение окон»). Пробельные
/// строки нормализуются в `None`; правило «оба `None`» координатор не
/// добавит (такое не матчит ничего) — см. обработчик `AddDenylistRule`.
#[tauri::command]
fn add_denylist_rule(
    process_name: String,
    title_pattern: Option<String>,
    overlay: tauri::State<OverlayHandle>,
) {
    overlay.send(OverlayCommand::AddDenylistRule(OverlapRule {
        process_name: trim_non_empty(process_name),
        title_pattern: title_pattern.and_then(trim_non_empty),
    }));
}

/// Удалить правило денй-листа по индексу строки списка (у `OverlapRule` нет
/// id — адрес строки и есть её индекс; тот же принцип, что у пунктов
/// подменю пресетов в трее: индекс вместо идентификатора).
#[tauri::command]
fn remove_denylist_rule(index: usize, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::RemoveDenylistRule(index));
}

/// Строка после trim; пустая/пробельная — `None` (поля-критерии правила
/// денй-листа не хранят пустые значения: `""` матчил бы заголовок `""`).
fn trim_non_empty(s: String) -> Option<String> {
    let trimmed = s.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Показать файл в проводнике (SPEC.md, раздел 10 — «показать в
/// проводнике»). Не трогает `cfg`/координатор — чисто читающее действие
/// над файловой системой, `explorer.exe` сам подсвечивает файл в открытом
/// окне папки.
#[tauri::command]
fn reveal_in_explorer(path: String) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            tracing::warn!(error = %e, path, "reveal_in_explorer: не удалось запустить проводник");
            "Не удалось открыть проводник.".to_string()
        })
}

fn config_path() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("APPDATA").context("переменная APPDATA не задана")?;
    Ok(PathBuf::from(base).join("resticker").join("config.json"))
}

fn main() -> anyhow::Result<()> {
    let log_path = logging::init()?;
    tracing::info!(path = %log_path.display(), "логирование инициализировано");

    // Второй экземпляр (автозапуск + ручной запуск, повторный клик по
    // ярлыку) поднял бы второй набор WS_EX_TOPMOST оверлей-окон на тех же
    // мониторах — оба получают реальные события мыши/клавиатуры вперемешку
    // (rst_win32::single_instance, доккомент модуля). Тихо выходим, не
    // трогая конфиг/трей/оверлей уже работающего экземпляра.
    let _single_instance: Option<rst_win32::single_instance::SingleInstance> =
        match rst_win32::single_instance::acquire() {
            rst_win32::single_instance::SingleInstanceResult::Acquired(guard) => Some(guard),
            rst_win32::single_instance::SingleInstanceResult::AlreadyRunning => {
                tracing::warn!("resticker уже запущен в этом сеансе — выходим");
                return Ok(());
            }
            rst_win32::single_instance::SingleInstanceResult::Error => {
                tracing::warn!(
                    "не удалось проверить единственность экземпляра (CreateMutex не сработал) — продолжаем запуск как есть"
                );
                None
            }
        };

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

    let tray_lang = cfg.settings.language.clone();
    let initial_presets: Vec<(Uuid, String)> =
        cfg.presets.iter().map(|p| (p.id, p.name.clone())).collect();
    let (initial_menu, initial_preset_ids) = build_tray_menu(&initial_presets, &tray_lang);
    let (tray_icon, tray_rx) =
        TrayIcon::new("resticker", initial_menu).context("инициализация иконки трея")?;
    // Индекс пункта меню → id пресета (см. `MENU_PRESET_BASE`) — общий между
    // потоком трея (читает при клике) и потоком координатора (пишет при
    // `CoordinatorRequest::PresetsChanged`).
    let preset_ids = Arc::new(Mutex::new(initial_preset_ids));

    let silent_start = cfg.settings.silent_start;
    // Для окна настроек (`get_config`) — читает диск напрямую, не через
    // координатор, поэтому нужен свой клон пути ДО того, как `cfg_path`
    // уйдёт во владение `overlay_manager::start`.
    let cfg_path_for_settings = cfg_path.clone();
    // Обратный канал координатор → main (docs/M2_WIRING_PLAN.md, раздел 12):
    // `Sender` уходит в координатор (оверлей-поток), `Receiver` читает поток
    // из `setup` ниже.
    let (coordinator_tx, coordinator_rx) = mpsc::channel::<CoordinatorRequest>();
    let overlay_handle = overlay_manager::start(cfg_path, cfg, coordinator_tx);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tray_icon)
        .manage(overlay_handle)
        .manage(cfg_path_for_settings)
        .invoke_handler(tauri::generate_handler![
            add_sticker,
            get_config,
            update_settings,
            update_hotkeys,
            set_sticker_enabled,
            delete_sticker,
            reset_sticker_position,
            reset_sticker_transform,
            relink_sticker,
            reset_all_stickers,
            delete_all_stickers,
            reveal_in_explorer,
            save_preset,
            apply_preset,
            rename_preset,
            delete_preset,
            export_preset,
            import_preset,
            add_denylist_rule,
            remove_denylist_rule,
        ])
        .setup(move |app| {
            if !silent_start {
                if let Some(w) = app.get_webview_window("settings") {
                    let _ = w.show();
                }
            }

            let handle = app.handle().clone();
            let preset_ids_for_tray = Arc::clone(&preset_ids);
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
                            handle
                                .state::<OverlayHandle>()
                                .send(OverlayCommand::ToggleAllStickers);
                        }
                        TrayEvent::MenuItem(MENU_EXIT) => handle.exit(0),
                        // M7: клик по пункту пресета в меню трея — id несёт
                        // только индекс в списке на момент последней
                        // пересборки меню (`build_tray_menu`), сам `Uuid`
                        // ищем в общем с координатор-потоком `preset_ids`.
                        TrayEvent::MenuItem(id) if id >= MENU_PRESET_BASE => {
                            let target = {
                                let ids = preset_ids_for_tray
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                ids.get((id - MENU_PRESET_BASE) as usize).copied()
                            };
                            if let Some(preset_id) = target {
                                handle
                                    .state::<OverlayHandle>()
                                    .send(OverlayCommand::ApplyPreset(preset_id));
                            }
                        }
                        TrayEvent::MenuItem(_) => {}
                    }
                }
            });

            // Запросы координатора, которым нужен Tauri (docs/M2_WIRING_PLAN.md,
            // раздел 12): окна живут на главном потоке, оверлей-поток их
            // трогать не может. Обработка — та же, что у пункта трея.
            let coordinator_handle = app.handle().clone();
            let preset_ids_for_coordinator = Arc::clone(&preset_ids);
            let tray_lang_for_coordinator = tray_lang.clone();
            std::thread::spawn(move || {
                for request in coordinator_rx {
                    match request {
                        CoordinatorRequest::OpenSettings => {
                            if let Some(w) = coordinator_handle.get_webview_window("settings") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        // M7: применение пресета оставило часть стикеров
                        // неприменённой (SPEC.md §11) — форвардим списком
                        // события фронтенду окна настроек; текст диалога —
                        // ответственность JS (main.js уже слушает
                        // 'preset-missing-elements').
                        CoordinatorRequest::PresetMissingElements(missing) => {
                            // Фронтенд (main.js) ждёт объект `{missing: [...]}`,
                            // не голый массив — `event.payload.missing`.
                            let payload = serde_json::json!({ "missing": missing });
                            let _ =
                                coordinator_handle.emit_to("settings", "preset-missing-elements", payload);
                        }
                        CoordinatorRequest::ShowNotification { title, body } => {
                            if let Err(e) =
                                coordinator_handle.state::<TrayIcon>().show_balloon(&title, &body)
                            {
                                tracing::warn!(error = %e, "не удалось показать баллон-уведомление трея");
                            }
                        }
                        // M7: список пресетов изменился — пересобираем меню
                        // трея целиком (`TrayIcon::set_menu`) и обновляем
                        // общий с потоком трея id→Uuid список.
                        CoordinatorRequest::PresetsChanged(list) => {
                            let (items, ids) = build_tray_menu(&list, &tray_lang_for_coordinator);
                            *preset_ids_for_coordinator
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()) = ids;
                            coordinator_handle.state::<TrayIcon>().set_menu(items);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ROADMAP.md M8 «понятные тексты ошибок вместо кодов»: текст, который
    /// увидит пользователь в окне настроек, не должен содержать сырые
    /// сообщения `std::io::Error`/`serde_json::Error` (пути к файлам,
    /// "os error N", позиции JSON-парсера и т.п.).
    #[test]
    fn friendly_config_error_hides_raw_io_and_json_text() {
        let not_found = rst_core::CoreError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(
            friendly_config_error(&not_found),
            "Файл настроек не найден."
        );

        let denied =
            rst_core::CoreError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(
            friendly_config_error(&denied),
            "Нет доступа к файлу настроек — проверьте права на папку AppData."
        );

        let other_io = rst_core::CoreError::Io(std::io::Error::other("что-то сломалось на диске"));
        let msg = friendly_config_error(&other_io);
        assert_eq!(msg, "Не удалось прочитать файл настроек.");
        assert!(
            !msg.contains("что-то сломалось"),
            "сырой текст io::Error не должен просочиться в UI-сообщение"
        );

        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let msg = friendly_config_error(&rst_core::CoreError::Json(json_err));
        assert_eq!(msg, "Файл настроек повреждён.");

        let msg = friendly_config_error(&rst_core::CoreError::UnknownSchemaVersion(99));
        assert_eq!(
            msg,
            "Файл настроек создан более новой версией resticker — обновите программу."
        );

        let msg = friendly_config_error(&rst_core::CoreError::NotAnObject);
        assert_eq!(msg, "Файл настроек повреждён.");
    }

    #[test]
    fn parse_id_rejects_garbage_with_friendly_text_not_raw_parse_error() {
        let err = parse_id("not-a-uuid").unwrap_err();
        assert_eq!(
            err,
            "Внутренняя ошибка: некорректный идентификатор элемента."
        );
    }

    #[test]
    fn parse_id_accepts_valid_uuid() {
        let id = Uuid::new_v4();
        assert_eq!(parse_id(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn trim_non_empty_drops_empty_and_whitespace() {
        assert_eq!(trim_non_empty("chrome.exe".to_string()).as_deref(), Some("chrome.exe"));
        assert_eq!(trim_non_empty("  chrome.exe  ".to_string()).as_deref(), Some("chrome.exe"));
        assert_eq!(trim_non_empty("".to_string()), None);
        assert_eq!(trim_non_empty("   ".to_string()), None);
    }
}
