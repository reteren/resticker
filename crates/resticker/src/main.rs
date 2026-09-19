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
mod gap_panel;
mod group_manager;
mod group_strip;
mod groups;
mod i18n;
mod input_policy;
mod logging;
/// Чистый билдер примитивов предпросмотра разреза (M9, «Митоз окон», §4.3).
mod mitosis_overlay;
/// Бейдж номера монитора (T7).
mod monitor_badge;
mod overlay_manager;
mod preset_picker;
mod preset_strip;
mod toolbar;
/// Оффскрин-превью панелей в PNG — инструмент разработки оформления.
#[cfg(test)]
mod ui_preview;
mod window_crop_chrome;
mod window_crop_overlay;
mod window_pick_list;
mod window_picker;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};

use anyhow::Context;
use rst_core::model::{Config, Hotkeys, OverlapRule, Settings};
use tauri::{Emitter, Manager, WindowEvent};
use uuid::Uuid;

use overlay_manager::{CoordinatorRequest, OverlayCommand, OverlayHandle};
use rst_win32::tray::{TrayEvent, TrayIcon};
use tauri::window::{Effect, EffectsBuilder};

/// Метка окна меню трея.
const TRAY_MENU_LABEL: &str = "traymenu";

/// Создание, повторное открытие и уничтожение окна меню трея идут из разных
/// потоков (поток трея, таймер простоя). Под этим замком решение «окно ещё
/// нужно?» и само уничтожение не могут разойтись с открытием.
static TRAY_MENU_LIFECYCLE: Mutex<()> = Mutex::new(());

/// То же для окна настроек: трей, хоткей координатора и второй экземпляр
/// могут попросить его одновременно — создаётся ровно одно.
static SETTINGS_LIFECYCLE: Mutex<()> = Mutex::new(());

/// Где меню должно появиться и ждёт ли оно показа.
///
/// Точка приходит из потока трея, а размер — из webview уже после отрисовки
/// (число пресетов меняет высоту). Показать окно раньше, чем известны оба,
/// значит на мгновение показать пустую дыру в экране у курсора.
#[derive(Default)]
struct TrayMenuState {
    /// Экранная точка клика, физические пиксели.
    origin: Mutex<Option<(i32, i32)>>,
    /// `true` — меню просили открыть и оно ждёт размера от webview.
    pending: std::sync::atomic::AtomicBool,
    /// Счётчик поколения скрытия/открытия: инкрементируется при каждом скрытии/открытии,
    /// чтобы таймер простоя (60 с) уничтожал окно только если его не трогали.
    epoch: std::sync::atomic::AtomicU64,
}

/// Запланировать уничтожение окна меню трея через 60 с простоя.
///
/// Если за это время меню открыли снова — поколение `epoch` увеличивается,
/// и фоновый таймер ничего не делает. Если меню простаивало все 60 с скрытым —
/// окно уничтожается, освобождая память WebView2.
fn schedule_tray_menu_idle_cleanup<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let state = app.state::<TrayMenuState>();
    let current_epoch = state
        .epoch
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    let app_clone = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let _lifecycle = TRAY_MENU_LIFECYCLE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = app_clone.state::<TrayMenuState>();
        let untouched = state.epoch.load(std::sync::atomic::Ordering::SeqCst) == current_epoch;
        let waiting_to_show = state.pending.load(std::sync::atomic::Ordering::Acquire);
        if untouched && !waiting_to_show {
            if let Some(w) = app_clone.get_webview_window(TRAY_MENU_LABEL) {
                if !w.is_visible().unwrap_or(false) {
                    tracing::info!("меню трея: окно уничтожено по таймеру простоя 60с");
                    let _ = w.destroy();
                }
            }
        }
    });
}

/// Попросить окно меню пересобраться под текущее состояние и показаться.
///
/// Если окно ещё не создано, создаёт его WebviewWindowBuilder-ом на лету (холодный старт
/// замеряется в tracing). Само окно показывается после того, как webview сообщит свой
/// размер ([`tray_menu_ready`]).
fn show_tray_menu<R: tauri::Runtime>(app: &impl tauri::Manager<R>, x: i32, y: i32) {
    let _lifecycle = TRAY_MENU_LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let state = app.state::<TrayMenuState>();
    *state.origin.lock().unwrap_or_else(|e| e.into_inner()) = Some((x, y));
    state
        .pending
        .store(true, std::sync::atomic::Ordering::Release);
    // Инвалидируем любой ожидающий 60с-таймер очистки
    state
        .epoch
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let w = if let Some(w) = app.get_webview_window(TRAY_MENU_LABEL) {
        // Открытое меню по повторному клику закрывается — так ведёт себя и
        // системное, и любая кнопка-переключатель программы.
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
            state
                .pending
                .store(false, std::sync::atomic::Ordering::Release);
            schedule_tray_menu_idle_cleanup(app.app_handle());
            return;
        }
        w
    } else {
        let t0 = std::time::Instant::now();
        let effects = EffectsBuilder::new().effect(Effect::Acrylic).build();
        let builder = tauri::WebviewWindowBuilder::new(
            app,
            TRAY_MENU_LABEL,
            tauri::WebviewUrl::App("traymenu.html".into()),
        )
        .title("resticker menu")
        .inner_size(232.0, 240.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .theme(Some(tauri::Theme::Dark))
        .effects(effects)
        .visible(false);

        match builder.build() {
            Ok(w) => {
                let elapsed_ms = t0.elapsed().as_millis();
                tracing::info!(elapsed_ms, "холодное открытие меню трея");
                #[cfg(windows)]
                {
                    disable_browser_accelerators(&w);
                    if let Ok(hwnd) = w.hwnd() {
                        rst_win32::dwm::round_window_corners(hwnd.0 as isize);
                    }
                }
                w
            }
            Err(e) => {
                tracing::error!(error = %e, "не удалось создать окно меню трея");
                return;
            }
        }
    };

    // `emit`, а не `emit_to`: у окна настроек событие «показались» ходит
    // именно так и доходит до его `listen` (main.js). Адресный вариант с
    // меткой окна тихо не доезжал до webview — меню получало размер при
    // загрузке страницы и больше ни одного события.
    let _ = w.emit("tray-menu-shown", ());
    tracing::debug!(x, y, "меню трея: запрошен показ");

    // Страховка на случай, если webview промолчит (страница ещё грузится,
    // скрипт упал): через секунду показываем меню с тем размером,
    // который оно сообщило в прошлый раз или дефолтным. Иначе правый клик по иконке
    // выглядел бы как «ничего не произошло» — ровно то, чего мы избегаем.
    let app_for_fallback = app.app_handle().clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let state = app_for_fallback.state::<TrayMenuState>();
        if !state
            .pending
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        tracing::warn!("меню трея: webview не ответил, показываем прежним размером");
        let Some(w) = app_for_fallback.get_webview_window(TRAY_MENU_LABEL) else {
            return;
        };
        let size = w.outer_size().unwrap_or(tauri::PhysicalSize::new(232, 240));
        let origin = *state.origin.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((x, y)) = origin {
            let (px, py) = place_tray_menu(&w, x, y, size.width as i32, size.height as i32);
            let _ = w.set_position(tauri::PhysicalPosition::new(px, py));
        }
        let _ = w.show();
        let _ = w.set_focus();
    });
}

/// Webview отрисовал меню и сообщил его размер: ставим окно у курсора и
/// показываем.
///
/// Размер приходит в логических пикселях (CSS), позиция — в физических:
/// первое масштабирует Tauri сам, второе — экранная точка клика.
#[tauri::command]
fn tray_menu_ready(width: f64, height: f64, app: tauri::AppHandle) {
    let Some(w) = app.get_webview_window(TRAY_MENU_LABEL) else {
        return;
    };
    let _ = w.set_size(tauri::LogicalSize::new(width, height));
    let state = app.state::<TrayMenuState>();
    // Первый доклад приходит при загрузке страницы, когда меню никто не
    // просил: он только задаёт размер.
    if !state
        .pending
        .swap(false, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    let Some((x, y)) = *state.origin.lock().unwrap_or_else(|e| e.into_inner()) else {
        return;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    let (pw, ph) = (
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    );
    tracing::debug!(width, height, "меню трея: webview прислал размер");
    let (px, py) = place_tray_menu(&w, x, y, pw, ph);
    let _ = w.set_position(tauri::PhysicalPosition::new(px, py));
    let _ = w.show();
    let _ = w.set_focus();
    watch_focus_loss(&app, &w);
}

/// Закрывать меню, как только пользователь ушёл в другое окно.
///
/// Своим наблюдателем, а не событием окна: `WindowEvent::Focused` для этого
/// окна не приходит вовсе (замер 2026-09-17 — в обработчик падали только
/// `Moved`/`Resized`, а меню оставалось висеть после клика мимо него).
///
/// Правило двухступенчатое, и это не перестраховка. Windows отдаёт передний
/// план не всегда: клик по иконке трея такое право даёт, а вот меню,
/// показанное когда правом владеет чужое полноэкранное приложение, впереди не
/// окажется. Закрывать по «впереди не мы» в лоб значило бы, что в таком
/// случае меню мигнёт и исчезнет (ровно это и показал замер). Поэтому:
/// получили передний план — закрываемся по его потере; не получили — ждём,
/// пока пользователь не переключится на окно, отличное от того, что было
/// впереди в момент показа.
///
/// Поток живёт ровно пока открыто меню, опрос вдесятеро реже кадра.
fn watch_focus_loss<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let menu_hwnd = hwnd.0 as isize;
    let app = app.clone();
    std::thread::spawn(move || {
        let opened_over = rst_win32::window_pick::foreground_window();
        let mut was_ours = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let Some(w) = app.get_webview_window(TRAY_MENU_LABEL) else {
                return;
            };
            if !w.is_visible().unwrap_or(false) {
                return;
            }
            let fg = rst_win32::window_pick::foreground_window();
            if fg == menu_hwnd {
                was_ours = true;
                continue;
            }
            // Переключение «в никуда» (0) бывает на миг между окнами — это не
            // уход пользователя.
            if fg == 0 {
                continue;
            }
            if was_ours || fg != opened_over {
                let _ = w.hide();
                schedule_tray_menu_idle_cleanup(&app);
                return;
            }
        }
    });
}

/// Левый верхний угол меню размера `w`×`h` (физические пиксели) для клика в
/// точке `x`,`y`.
///
/// Меню раскрывается ВВЕРХ И ВЛЕВО от курсора: иконка трея живёт в правом
/// нижнем углу, и любое другое направление упёрлось бы в край экрана. Если
/// монитор известен, результат прижимается к его границам — на верхнем
/// мониторе вертикальной пары меню иначе уезжало бы за верхний край.
fn place_tray_menu<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> (i32, i32) {
    /// Зазор между меню и курсором/краем экрана, физические пиксели.
    const GAP: i32 = 6;
    let (mut px, mut py) = (x - w - GAP, y - h - GAP);
    if let Ok(Some(monitor)) = window.monitor_from_point(f64::from(x), f64::from(y)) {
        let pos = monitor.position();
        let size = monitor.size();
        let (min_x, min_y) = (pos.x + GAP, pos.y + GAP);
        let max_x = pos.x + size.width as i32 - w - GAP;
        let max_y = pos.y + size.height as i32 - h - GAP;
        px = px.clamp(min_x.min(max_x), max_x.max(min_x));
        py = py.clamp(min_y.min(max_y), max_y.max(min_y));
    }
    (px, py)
}

/// Спрятать меню трея: клик по пункту, `Esc` или потеря фокуса.
#[tauri::command]
fn hide_tray_menu(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(TRAY_MENU_LABEL) {
        let _ = w.hide();
        schedule_tray_menu_idle_cleanup(&app);
    }
}

/// Пункт «Открыть настройки».
///
/// Асинхронная команда обязательно: окно теперь создаётся по требованию, а
/// `WebviewWindowBuilder::build` из синхронной команды на Windows зависает
/// намертво (доккомент `WebviewWindowBuilder::new`, tauri 2.11).
#[tauri::command]
async fn open_settings_window(app: tauri::AppHandle) {
    show_settings_window(&app);
}

/// Пункт «Режим редактирования» — дубль глобального хоткея на случай, когда
/// его перехватывает чужая программа.
#[tauri::command]
fn tray_toggle_edit_mode(overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::ToggleEditMode);
}

/// Пункт «Показать/скрыть все стикеры».
#[tauri::command]
fn tray_toggle_visible(overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::ToggleAllStickers);
}

/// Пункт «Зазор в снап-зоне»: панель с полем ввода поверх экрана.
#[tauri::command]
fn tray_open_gap_panel(overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::OpenGapPanel);
}

/// Пункт-переключатель «Все окна, не только закреплённые».
///
/// Без значения в аргументе: текущее состояние знает координатор (это его
/// `cfg`), и вторая копия флага здесь была бы вторым источником правды.
#[tauri::command]
fn tray_toggle_snap_gap_all(overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::ToggleSnapGapAllWindows);
}

/// Пункт «Выход».
#[tauri::command]
fn tray_exit(app: tauri::AppHandle) {
    app.exit(0);
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

/// Показать окно настроек: поверх всех окон и в фокусе.
///
/// Если окно ещё не создано, создаёт его WebviewWindowBuilder-ом на лету (холодный старт
/// замеряется в tracing). Окно уничтожается при закрытии, освобождая рантайм WebView2.
///
/// `set_always_on_top` обязателен: оверлей-окна и закреплённые окна живут с
/// `WS_EX_TOPMOST`, и обычное окно уходит под них — на прозрачном оверлее
/// это выглядит как «настройки открылись, но не нажимаются» (репорт
/// пользователя 2026-08-23).
fn show_settings_window<R: tauri::Runtime>(app: &impl tauri::Manager<R>) {
    let _lifecycle = SETTINGS_LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    let w = if let Some(w) = app.get_webview_window("settings") {
        let _ = w.set_always_on_top(true);
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("settings-shown", ());
        report_settings_rect(app, &w);
        w
    } else {
        let t0 = std::time::Instant::now();
        let effects = EffectsBuilder::new().effect(Effect::Acrylic).build();
        let builder = tauri::WebviewWindowBuilder::new(
            app,
            "settings",
            tauri::WebviewUrl::App("index.html".into()),
        )
        .title("resticker — settings")
        .inner_size(900.0, 600.0)
        .center()
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .theme(Some(tauri::Theme::Dark))
        .always_on_top(true)
        .effects(effects)
        .visible(false);

        match builder.build() {
            Ok(w) => {
                let elapsed_ms = t0.elapsed().as_millis();
                tracing::info!(elapsed_ms, "холодное открытие окна настроек");
                #[cfg(windows)]
                {
                    disable_browser_accelerators(&w);
                    if let Ok(hwnd) = w.hwnd() {
                        rst_win32::dwm::round_window_corners(hwnd.0 as isize);
                    }
                }
                let _ = w.show();
                let _ = w.set_focus();
                report_settings_rect(app, &w);
                w
            }
            Err(e) => {
                tracing::error!(error = %e, "не удалось создать окно настроек");
                return;
            }
        }
    };
    let _ = w;
}

/// Сообщить координатору прямоугольник окна настроек (физические пиксели
/// экрана) — по нему оверлей режима редактирования вырезает дыру, иначе в
/// окно нельзя тыкнуть (репорт пользователя 2026-08-23).
///
/// `None` шлём при скрытии окна; при показе, перемещении и изменении
/// размера — свежий прямоугольник.
fn report_settings_rect<R: tauri::Runtime>(
    app: &impl tauri::Manager<R>,
    window: &tauri::WebviewWindow<R>,
) {
    let rect = match (
        window.is_visible(),
        window.outer_position(),
        window.outer_size(),
    ) {
        (Ok(true), Ok(pos), Ok(size)) => Some((
            pos.x,
            pos.y,
            pos.x + size.width as i32,
            pos.y + size.height as i32,
        )),
        _ => None,
    };
    app.state::<OverlayHandle>()
        .send(OverlayCommand::SettingsWindowRect(rect));
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
            std::io::ErrorKind::NotFound => "Settings file not found.".to_string(),
            std::io::ErrorKind::PermissionDenied => {
                "No access to the settings file - check the permissions on the AppData folder."
                    .to_string()
            }
            _ => "Could not read the settings file.".to_string(),
        },
        rst_core::CoreError::Json(_) | rst_core::CoreError::Migrate(_) => {
            "Settings file is corrupted.".to_string()
        }
        rst_core::CoreError::UnknownSchemaVersion(_) => {
            "Settings file was written by a newer resticker - please update the app.".to_string()
        }
        rst_core::CoreError::NotAnObject => "Settings file is corrupted.".to_string(),
    }
}

/// Заменить `cfg.settings` целиком (вкладка «Общие») — координатор
/// остаётся единственным писателем `config.json` (доккомент
/// `OverlayCommand::UpdateSettings`).
#[tauri::command]
fn update_settings(settings: Settings, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::UpdateSettings(settings));
}

/// Заменить `cfg.hotkeys` целиком (вкладка «Управление») — новые комбинации
/// начинают работать сразу, без перезапуска (доккомент
/// `OverlayCommand::UpdateHotkeys`).
#[tauri::command]
fn update_hotkeys(hotkeys: Hotkeys, overlay: tauri::State<OverlayHandle>) {
    overlay.send(OverlayCommand::UpdateHotkeys(hotkeys));
}

fn parse_id(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|e| {
        tracing::warn!(error = %e, id, "parse_id: некорректный идентификатор от UI");
        "Internal error: malformed item identifier.".to_string()
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

/// Список открытых окон для пикера денй-листа (запрос пользователя
/// 2026-08-19: «хочу как окна выбирать только процессы для денай листа» —
/// раньше `process_name.exe` приходилось печатать руками). Читает то же
/// перечисление, что нативный пикер оверлея (`window_pick_list.rs`), но
/// напрямую с потока Tauri-команды — чтение без побочных эффектов, поход
/// через канал координатора не нужен. Схлопывает по процессу (одна строка
/// на процесс, а не на окно — денй-лист матчит по `process_name`, не по
/// конкретному HWND), берёт первый по z-order заголовок как подпись, само
/// resticker.exe из списка исключено (денй-лист на себя бессмыслен).
/// Возвращает `(process_name, title)` — пары вместо структуры: `resticker`
/// не тянет `serde` напрямую (сериализуется транзитивно через `tauri`),
/// кортеж примитивов не требует лишней зависимости в Cargo.toml.
#[tauri::command]
fn list_open_processes() -> Vec<(String, String)> {
    let self_exe = std::env::current_exe().ok();
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for win in rst_win32::window_enum::enumerate() {
        if win.title.trim().is_empty() || win.exe_path.as_os_str().is_empty() {
            continue;
        }
        if self_exe.as_deref() == Some(win.exe_path.as_path()) {
            continue;
        }
        let Some(process_name) = win
            .exe_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        if !seen.insert(process_name.clone()) {
            continue;
        }
        result.push((process_name, win.title));
    }
    result.sort_by_key(|a| a.0.to_lowercase());
    result
}

/// Отключить системные акселераторы браузера (Edge/WebView2) для окна настроек.
///
/// Зачем: по умолчанию WebView2 перехватывает встроенные акселераторы Edge
/// (`Alt+Shift+S` для Visual Search, `Ctrl+Shift+S` для Web Capture, `Ctrl+S`,
/// `F5`, `Ctrl+F` и др.) на уровне браузерного движка ДО того, как `keydown`
/// дойдёт до JavaScript в DOM. Из-за этого пользователь не мог назначить
/// `Alt+Shift+S` в поле хоткеев — событие просто не долетало до вебвью.
#[cfg(windows)]
fn disable_browser_accelerators<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    let _ = window.with_webview(|webview| {
        // SAFETY: прямое обращение к COM-интерфейсам WebView2 в соответствии
        // с контрактом WebView2 SDK.
        unsafe {
            use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
            use windows_core_61::Interface;

            let controller = webview.controller();
            if let Ok(core) = controller.CoreWebView2() {
                if let Ok(settings) = core.Settings() {
                    if let Ok(settings3) = settings.cast::<ICoreWebView2Settings3>() {
                        let _ = settings3.SetAreBrowserAcceleratorKeysEnabled(false);
                    }
                }
            }
        }
    });
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
            "Could not open File Explorer.".to_string()
        })
}

fn config_path() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("APPDATA").context("переменная APPDATA не задана")?;
    Ok(PathBuf::from(base).join("resticker").join("config.json"))
}

const PANIC_MARKER_HEADER: &str = "resticker-panic-marker-v1";
const PANIC_MARKER_FILE: &str = "panic.marker";

/// Данные, которые переживают `panic = "abort"`: после аварии процесс уже
/// не может отправить тост и не оставляет консоли, поэтому следующий старт
/// читает короткую метку рядом с журналом и объясняет пользователю исчезновение
/// резидентной программы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanicMarker {
    pub(crate) timestamp: String,
    pub(crate) version: String,
    pub(crate) thread: String,
    pub(crate) location: String,
    pub(crate) message: String,
    pub(crate) log_path: String,
}

impl PanicMarker {
    fn from_panic(
        timestamp: String,
        version: String,
        thread: &str,
        location: &str,
        message: &str,
        log_path: &Path,
    ) -> Self {
        Self {
            timestamp,
            version,
            thread: thread.to_string(),
            location: location.to_string(),
            message: message
                .lines()
                .next()
                .unwrap_or("panic without a message")
                .to_string(),
            log_path: log_path.to_string_lossy().into_owned(),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != PANIC_MARKER_HEADER {
            return None;
        }
        fn field<'a>(line: Option<&'a str>, name: &str) -> Option<&'a str> {
            line?.strip_prefix(name)?.strip_prefix('=')
        }
        let timestamp = field(lines.next(), "timestamp")?;
        let version = field(lines.next(), "version")?;
        let thread = field(lines.next(), "thread")?;
        let location = field(lines.next(), "location")?;
        let message = field(lines.next(), "message")?;
        let log_path = field(lines.next(), "log_path")?;
        if lines.next().is_some()
            || timestamp.is_empty()
            || version.is_empty()
            || thread.is_empty()
            || location.is_empty()
            || message.is_empty()
            || log_path.is_empty()
            || chrono::DateTime::parse_from_rfc3339(timestamp).is_err()
        {
            return None;
        }
        Some(Self {
            timestamp: timestamp.to_string(),
            version: version.to_string(),
            thread: thread.to_string(),
            location: location.to_string(),
            message: message.to_string(),
            log_path: log_path.to_string(),
        })
    }
}

/// Формат намеренно построчный: его можно прочитать даже после частичной
/// записи в момент аварии, а разбор отбрасывает чужие/повреждённые файлы
/// вместо показа мусора пользователю.
fn panic_marker_text(marker: &PanicMarker) -> String {
    format!(
        "{PANIC_MARKER_HEADER}\ntimestamp={}\nversion={}\nthread={}\nlocation={}\nmessage={}\nlog_path={}\n",
        marker.timestamp,
        marker.version,
        marker.thread,
        marker.location,
        marker.message,
        marker.log_path,
    )
}

fn panic_marker_path_for_log(log_path: &Path) -> PathBuf {
    log_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(PANIC_MARKER_FILE)
}

/// Запись после `tracing::error!` не должна сама паниковать: двойная паника
/// в аварийном хуке только скрыла бы исходную причину.
fn write_panic_marker(path: &Path, marker: &PanicMarker) {
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
    else {
        return;
    };
    let _ = file.write_all(panic_marker_text(marker).as_bytes());
    let _ = file.flush();
}

/// Прочитать метку и сразу собрать оба текста уведомления, но НЕ удалять
/// файл: тост и оверлей должны увидеть одну и ту же причину, а удаление
/// выполняется ровно один раз после успешного показа тоста.
pub(crate) fn panic_notification_from_marker(
    path: &Path,
) -> Option<(PanicMarker, (String, String))> {
    let Ok(text) = fs::read_to_string(path) else {
        return None;
    };
    let marker = PanicMarker::parse(&text)?;
    let notification = i18n::panic_notification(
        &marker.version,
        &marker.thread,
        &marker.location,
        &marker.message,
        &marker.log_path,
    );
    Some((marker, notification))
}

fn consume_panic_marker(
    path: &Path,
    notify: impl FnOnce(&PanicMarker, &(String, String)) -> bool,
) -> bool {
    let Some((marker, notification)) = panic_notification_from_marker(path) else {
        // Повреждённая метка не должна застрять и проверяться на каждом
        // старте; главное — не показывать её содержимое как достоверное.
        let _ = fs::remove_file(path);
        return false;
    };
    if notify(&marker, &notification) {
        fs::remove_file(path).is_ok()
    } else {
        false
    }
}

fn main() -> anyhow::Result<()> {
    // Ограничиваем рантайм Tokio для Tauri 2 рабочими потоками вместо числа ядер CPU
    // (обычно 16-32), чтобы устранить 16 простаивающих фоновых потоков.
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("инициализация tokio runtime")?;
    tauri::async_runtime::set(tokio_rt.handle().clone());
    let _tokio_rt = tokio_rt;

    let log_path = logging::init()?;
    tracing::info!(path = %log_path.display(), "логирование инициализировано");
    let panic_marker_file = panic_marker_path_for_log(&log_path);
    let panic_marker_file_for_hook = panic_marker_file.clone();

    // `panic = "abort"` (Cargo.toml, release-профиль) — паника на ЛЮБОМ
    // потоке мгновенно валит весь процесс, а GUI-подсистема (windows_subsystem
    // = "windows" выше) означает, что stderr никуда не подключён: без этого
    // хука паника просто исчезает — процесс молча пропадает из списка
    // процессов, ни единой строки в логе, ни записи в Windows Error Reporting
    // (найдено вживую 2026-08-18: resticker тихо умер, конфиг/сборка были в
    // порядке, причина осталась бы неизвестной без этого). Хук ставится ДО
    // спавна остальных потоков (окна оверлея на каждый монитор, трей,
    // хоткей-поток), чтобы покрыть панику где угодно, не только в main.
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "неизвестно".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "паника без сообщения".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            thread = %std::thread::current().name().unwrap_or("<unnamed>"),
            location = %location,
            message = %message,
            backtrace = %backtrace,
            "ПАНИКА — процесс сейчас завершится (panic = \"abort\")"
        );
        let marker = PanicMarker::from_panic(
            chrono::Utc::now().to_rfc3339(),
            env!("CARGO_PKG_VERSION").to_string(),
            std::thread::current().name().unwrap_or("<unnamed>"),
            &location,
            &message,
            &log_path,
        );
        write_panic_marker(&panic_marker_file_for_hook, &marker);
    }));

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
    let mut cfg = loaded.config;
    // Группы окон живут в конфиге (чтобы пережить перезапуск программы), но
    // обязаны исчезать при перезагрузке компьютера — запрос пользователя
    // 2026-08-27. Различает одно от другого отметка сеанса загрузки, и
    // ставится она ЗДЕСЬ, до первой записи конфига ниже: тогда та же запись и
    // унесёт на диск уже вычищенные группы.
    match rst_core::boot_session::adopt_boot_session(
        &mut cfg,
        &rst_win32::boot_session::current_stamp(),
    ) {
        rst_core::boot_session::GroupsOnStart::Kept => {
            tracing::info!(
                groups = cfg.groups.len(),
                "тот же сеанс загрузки — группы сохранены"
            );
        }
        rst_core::boot_session::GroupsOnStart::Forgotten(0) => {}
        rst_core::boot_session::GroupsOnStart::Forgotten(n) => {
            tracing::info!(forgotten = n, "новый сеанс загрузки — группы забыты");
        }
    }
    // ROADMAP.md M0: "читает и пишет конфиг" — гарантируем файл на диске
    // сразу при старте (первый запуск создаёт config.json с дефолтами).
    rst_core::config::save(&cfg, &cfg_path).context("сохранение config.json")?;

    // config.json — источник истины для UI-настройки автозапуска; реестр
    // синхронизируется с ним при каждом старте.
    let exe = std::env::current_exe().context("current_exe")?;
    if let Err(e) = rst_win32::autostart::set_enabled(cfg.settings.autostart, &exe) {
        tracing::warn!(error = %e, "не удалось синхронизировать автозапуск");
    }

    let (tray_icon, tray_rx) = TrayIcon::new("resticker").context("инициализация иконки трея")?;

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
    let panic_marker_file_for_setup = panic_marker_file.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tray_icon)
        .manage(TrayMenuState::default())
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
            list_open_processes,
            tray_menu_ready,
            hide_tray_menu,
            open_settings_window,
            tray_toggle_edit_mode,
            tray_toggle_visible,
            tray_open_gap_panel,
            tray_toggle_snap_gap_all,
            tray_exit,
        ])
        .setup(move |app| {
            // Прошлый запуск завершился паникой — сказать об этом человеку.
            // Двумя каналами сразу: тост трея и баннер оверлея. Баллуны
            // Windows 11 25H2 молча не рендерятся (замер 2026-08-18), а
            // баннер рисует сам resticker своим конвейером — его видно
            // гарантированно. Метка удаляется в любом случае: канал
            // координатора жив всё время работы программы, и повторять
            // сообщение при каждом запуске было бы хуже, чем пропустить его
            // один раз.
            let _ = consume_panic_marker(&panic_marker_file_for_setup, |_marker, notification| {
                let (title, body) = notification;
                if app.state::<TrayIcon>().show_balloon(title, body).is_err() {
                    tracing::warn!(
                        "баллун трея не показался — о падении сообщит только баннер оверлея"
                    );
                }
                app.state::<OverlayHandle>()
                    .send(OverlayCommand::ShowBanner(body.clone()));
                true
            });

            if !silent_start {
                show_settings_window(app);
            }

            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for event in tray_rx {
                    match event {
                        // Левый клик по иконке — сразу настройки, как и было.
                        TrayEvent::Activate => show_settings_window(&handle),
                        // Правый клик — своё окно меню у курсора. Пункты
                        // больше не приходят сюда номерами: их нажимает
                        // webview и зовёт обычные команды Tauri.
                        TrayEvent::ContextMenu { x, y } => {
                            show_tray_menu(&handle, x, y);
                        }
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
                            show_settings_window(&coordinator_handle);
                        }
                        // M7: применение пресета оставило часть стикеров
                        // неприменённой (SPEC.md §11) — форвардим списком
                        // события фронтенду окна настроек, если оно открыто;
                        // если окно закрыто — уведомляем через баллун и баннер,
                        // чтобы событие не пропало молча.
                        CoordinatorRequest::PresetMissingElements(missing) => {
                            let payload = serde_json::json!({ "missing": missing });
                            // Окно, которое есть, но спрятано или свёрнуто,
                            // сообщение не покажет — тогда баллун и баннер.
                            let has_settings = coordinator_handle
                                .get_webview_window("settings")
                                .is_some_and(|w| {
                                    w.is_visible().unwrap_or(false)
                                        && !w.is_minimized().unwrap_or(false)
                                });
                            let delivered = if has_settings {
                                coordinator_handle
                                    .emit_to("settings", "preset-missing-elements", payload)
                                    .is_ok()
                            } else {
                                false
                            };
                            if !delivered {
                                let lines = missing
                                    .iter()
                                    .map(|(_, path)| format!("• {}", path.display()))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let body = format!("Пресет применён частично:\n{lines}");
                                if let Err(e) = coordinator_handle
                                    .state::<TrayIcon>()
                                    .show_balloon("resticker — пресет", &body)
                                {
                                    tracing::warn!(error = %e, "не удалось показать баллун трея для недостающих элементов пресета");
                                }
                                coordinator_handle
                                    .state::<OverlayHandle>()
                                    .send(OverlayCommand::ShowBanner(body));
                            }
                        }
                        CoordinatorRequest::ShowNotification { title, body } => {
                            if let Err(e) =
                                coordinator_handle.state::<TrayIcon>().show_balloon(&title, &body)
                            {
                                tracing::warn!(error = %e, "не удалось показать баллон-уведомление трея");
                            }
                        }
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Меню трея закрывается, как только фокус ушёл: меню, пережившее
            // клик мимо себя, и есть то, чем раздражало системное. Слушаем
            // здесь, а не в webview: событие фокуса до страницы не доходило
            // (замер 2026-09-17 — меню оставалось на экране), а окну оно
            // приходит всегда.
            if window.label() == TRAY_MENU_LABEL {
                if let WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                    schedule_tray_menu_idle_cleanup(window.app_handle());
                }
                return;
            }

            // Окно настроек: уничтожается при закрытии, освобождая память WebView2.
            // Оверлей режима редактирования вырезает в себе прямоугольник
            // окна настроек — значит, обязан знать про каждое его движение и исчезновение.
            if window.label() == "settings" {
                if matches!(
                    event,
                    WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::Focused(_)
                ) {
                    if let Some(w) = window.get_webview_window("settings") {
                        report_settings_rect(window.app_handle(), &w);
                    }
                }
                if let WindowEvent::CloseRequested { .. } = event {
                    let _ = window.set_always_on_top(false);
                    window
                        .state::<OverlayHandle>()
                        .send(OverlayCommand::SettingsWindowRect(None));
                }
                if let WindowEvent::Destroyed = event {
                    window
                        .state::<OverlayHandle>()
                        .send(OverlayCommand::SettingsWindowRect(None));
                }
            }
        })
        .build(tauri::generate_context!())
        .context("запуск приложения Tauri")?
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if !should_exit_on_request(code) {
                    api.prevent_exit();
                }
            }
        });

    Ok(())
}

/// Разрешить ли Tauri завершить процесс по `RunEvent::ExitRequested`.
///
/// Окна настроек и меню теперь уничтожаются, когда не нужны, и закрытие
/// последнего из них Tauri по умолчанию считает выходом из программы
/// (`code == None`). Программа живёт в трее и оверлее, а не в окнах, поэтому
/// такой выход отменяется; явный выход (`AppHandle::exit`, пункт «Выход»)
/// приходит с кодом и проходит.
fn should_exit_on_request(code: Option<i32>) -> bool {
    code.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_last_window_does_not_exit_but_explicit_exit_does() {
        assert!(!should_exit_on_request(None));
        assert!(should_exit_on_request(Some(0)));
        assert!(should_exit_on_request(Some(1)));
    }

    /// ROADMAP.md M8 «понятные тексты ошибок вместо кодов»: текст, который
    /// увидит пользователь в окне настроек, не должен содержать сырые
    /// сообщения `std::io::Error`/`serde_json::Error` (пути к файлам,
    /// "os error N", позиции JSON-парсера и т.п.).
    #[test]
    fn friendly_config_error_hides_raw_io_and_json_text() {
        let not_found = rst_core::CoreError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(
            friendly_config_error(&not_found),
            "Settings file not found."
        );

        let denied =
            rst_core::CoreError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(
            friendly_config_error(&denied),
            "No access to the settings file - check the permissions on the AppData folder."
        );

        let other_io = rst_core::CoreError::Io(std::io::Error::other("что-то сломалось на диске"));
        let msg = friendly_config_error(&other_io);
        assert_eq!(msg, "Could not read the settings file.");
        assert!(
            !msg.contains("что-то сломалось"),
            "сырой текст io::Error не должен просочиться в UI-сообщение"
        );

        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let msg = friendly_config_error(&rst_core::CoreError::Json(json_err));
        assert_eq!(msg, "Settings file is corrupted.");

        let msg = friendly_config_error(&rst_core::CoreError::UnknownSchemaVersion(99));
        assert_eq!(
            msg,
            "Settings file was written by a newer resticker - please update the app."
        );

        let msg = friendly_config_error(&rst_core::CoreError::NotAnObject);
        assert_eq!(msg, "Settings file is corrupted.");
    }

    #[test]
    fn parse_id_rejects_garbage_with_friendly_text_not_raw_parse_error() {
        let err = parse_id("not-a-uuid").unwrap_err();
        assert_eq!(err, "Internal error: malformed item identifier.");
    }

    #[test]
    fn parse_id_accepts_valid_uuid() {
        let id = Uuid::new_v4();
        assert_eq!(parse_id(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn trim_non_empty_drops_empty_and_whitespace() {
        assert_eq!(
            trim_non_empty("chrome.exe".to_string()).as_deref(),
            Some("chrome.exe")
        );
        assert_eq!(
            trim_non_empty("  chrome.exe  ".to_string()).as_deref(),
            Some("chrome.exe")
        );
        assert_eq!(trim_non_empty("".to_string()), None);
        assert_eq!(trim_non_empty("   ".to_string()), None);
    }

    fn sample_panic_marker() -> PanicMarker {
        PanicMarker {
            timestamp: "2026-09-08T15:00:00+00:00".to_string(),
            version: "0.5.0".to_string(),
            thread: "overlay-1".to_string(),
            location: "crates/resticker/src/main.rs:42:7".to_string(),
            message: "device lost".to_string(),
            log_path: r"C:\Users\me\AppData\Local\resticker\logs\resticker.log".to_string(),
        }
    }

    fn temp_marker_path() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("resticker-panic-marker-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).expect("создать временный каталог для метки");
        let path = dir.join(PANIC_MARKER_FILE);
        (dir, path)
    }

    #[test]
    fn panic_marker_text_round_trips_deterministically() {
        let marker = sample_panic_marker();
        let text = panic_marker_text(&marker);
        assert!(
            text.starts_with("resticker-panic-marker-v1\ntimestamp=2026-09-08T15:00:00+00:00\n")
        );
        assert_eq!(PanicMarker::parse(&text), Some(marker));

        let panic = PanicMarker::from_panic(
            "2026-09-08T15:00:00+00:00".to_string(),
            "0.5.0".to_string(),
            "overlay-1",
            "main.rs:42:7",
            "first line\nsecond line",
            Path::new(r"C:\logs\resticker.log"),
        );
        assert_eq!(panic.message, "first line");
    }

    #[test]
    fn panic_notification_from_marker_reads_ready_text_without_consuming() {
        let (dir, path) = temp_marker_path();
        let marker = sample_panic_marker();
        std::fs::write(&path, panic_marker_text(&marker)).expect("записать метку");
        let (found, (title, body)) =
            panic_notification_from_marker(&path).expect("валидная метка читается");
        assert_eq!(found, marker);
        assert_eq!(title, "resticker closed unexpectedly");
        assert!(body.contains("version 0.5.0"));
        assert!(path.exists(), "чтение текста не должно удалять метку");
        std::fs::remove_dir_all(dir).expect("убрать временный каталог");
    }

    #[test]
    fn missing_panic_marker_does_not_notify() {
        let (dir, path) = temp_marker_path();
        let mut called = false;
        assert!(!consume_panic_marker(&path, |_, _| {
            called = true;
            true
        }));
        assert!(!called);
        std::fs::remove_dir_all(dir).expect("убрать временный каталог");
    }

    #[test]
    fn malformed_or_foreign_panic_marker_is_ignored_without_garbage() {
        let (dir, path) = temp_marker_path();
        for text in [
            "",
            "foreign-app-marker\nmessage=not ours\n",
            "resticker-panic-marker-v1\nversion=\nmessage=garbage\n",
        ] {
            std::fs::write(&path, text).expect("записать тестовую метку");
            let mut called = false;
            assert!(!consume_panic_marker(&path, |_, _| {
                called = true;
                true
            }));
            assert!(!called);
            assert!(!path.exists(), "битая метка удаляется после проверки");
        }
        std::fs::remove_dir_all(dir).expect("убрать временный каталог");
    }

    #[test]
    fn panic_marker_is_removed_after_notification() {
        let (dir, path) = temp_marker_path();
        let marker = sample_panic_marker();
        std::fs::write(&path, panic_marker_text(&marker)).expect("записать метку");
        let mut shown = false;
        assert!(consume_panic_marker(&path, |_, (title, body)| {
            shown = title == "resticker closed unexpectedly"
                && body.contains("version 0.5.0")
                && body.contains("resticker.log");
            shown
        }));
        assert!(shown);
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).expect("убрать временный каталог");
    }

    #[test]
    fn tray_menu_state_defaults_and_epoch_increment() {
        let state = TrayMenuState::default();
        assert_eq!(state.epoch.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(!state.pending.load(std::sync::atomic::Ordering::SeqCst));
        assert!(state.origin.lock().unwrap().is_none());

        let next = state
            .epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        assert_eq!(next, 1);
        assert_eq!(state.epoch.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn preset_missing_notification_formats_paths() {
        let missing = [
            (Uuid::new_v4(), PathBuf::from(r"C:\stickers\cat.png")),
            (Uuid::new_v4(), PathBuf::from(r"C:\stickers\dog.gif")),
        ];
        let lines = missing
            .iter()
            .map(|(_, path)| format!("• {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        let body = format!("Пресет применён частично:\n{lines}");
        assert!(body.contains("cat.png"));
        assert!(body.contains("dog.gif"));
    }
}
