//! Чтение и запись config.json: атомарность, версионирование, миграции,
//! обработка битых файлов (CONFIG.md, «Правила записи»).
//!
//! Дебаунс записи (CONFIG.md) здесь намеренно отсутствует: он относится к
//! состоянию приложения, а не к чистому слою ввода-вывода.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{Config, MonitorId};
use crate::tiling::{InsertPolicy, WindowRule};

/// Текущая версия схемы config.json.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Предупреждение о нештатной загрузке (конфиг при этом возвращается всегда).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    /// config.json не парсился; загружена резервная копия config.bak.
    CorruptRecoveredFromBak,
    /// config.json и config.bak не парсились; загружены значения по умолчанию.
    CorruptLoadedDefaults,
}

/// Результат загрузки: сам конфиг плюс возможное предупреждение.
#[derive(Debug, Clone)]
pub struct ConfigLoad {
    pub config: Config,
    pub warning: Option<ConfigWarning>,
}

/// Секция `"tiling"` в config.json (docs/TILING_DESIGN.md, M9): от глобального
/// выключателя до биндов. Тайлинг меняет поведение ВСЕГО рабочего стола,
/// поэтому секция живёт в конфиге, а не в рантайме — пользователь должен
/// явно записать `"enabled": true`.
///
/// Обратная совместимость без бампа `CURRENT_SCHEMA_VERSION`: старые
/// config.json без секции читаются через `#[serde(default)]` на структуре —
/// тот же прецедент, что `Config.presets` (docs/M4_PREP_NOTES.md: «миграция
/// схемы не нужна (`#[serde(default)]`)»). Миграция v0→v1 секцию намеренно
/// НЕ добавляет: недостающее поле достраивает serde тем же механизмом, что
/// и для v1-конфигов без секции, — одна ветка обратной совместимости вместо
/// двух.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TilingConfig {
    /// Тайлинг выключен по умолчанию: это смена поведения всего рабочего
    /// стола, включать её должен пользователь осознанно.
    pub enabled: bool,
    /// Зазор между соседними плитками, px.
    pub gaps_in: i32,
    /// Зазор до края рабочей области, px.
    pub gaps_out: i32,
    /// Высота полосы табов у групп, px.
    pub tab_bar_h: i32,
    /// Политика вставки нового окна.
    pub insert_policy: InsertPolicy,
    /// Правила окон (float / ignore / workspace).
    pub rules: Vec<WindowRule>,
    /// Горячие клавиши тайлинга.
    pub bindings: Vec<TilingBinding>,
    /// Мониторы, на которых тайлинг работает. Пустой список - все.
    pub monitors: Vec<MonitorId>,
    /// Перехватывать Alt+Tab и показывать СВОЙ переключатель окон.
    ///
    /// Выключено по умолчанию, и это не осторожность ради осторожности:
    /// Alt+Tab - самая заученная комбинация в Windows, подменять её без
    /// явного согласия нельзя. Своя нужна ровно за одним: системный
    /// переключатель не умеет показывать группу окон одной карточкой, и
    /// научить его этому нечем (docs/TILING_DESIGN.md, решение Р3).
    pub own_alt_tab: bool,
}

impl Default for TilingConfig {
    fn default() -> Self {
        Self {
            // Выключен: включение — осознанный акт (см. доккомент поля).
            enabled: false,
            // 8 px — порядок величины, привычный по тайлингам: плитки
            // читаются как отдельные окна, но площадь почти не съедается.
            // Меньше — плитки сливаются в одно полотно, больше — экран
            // уходит в воздух.
            gaps_in: 8,
            gaps_out: 8,
            // 24 px: строка заголовка таба (~13 px шрифта + поля) влезает
            // без обрезки; заметно меньше — текст таб-бара режется.
            tab_bar_h: 24,
            // Dwindle — поведение Hyprland по умолчанию и самый
            // предсказуемый вариант для первого включения: новое окно делит
            // сфокусированную плитку, ничего не нужно настраивать руками
            // (в отличие от Manual).
            insert_policy: InsertPolicy::Dwindle,
            // Правила — личное дело пользователя; пустой набор означает
            // «тайлится всё» (docs/TILING_DESIGN.md §Р2).
            rules: Vec::new(),
            // Набор из R4_KEYBINDS.md §4.3. Каждая комбинация — Alt/Alt+Shift:
            // ни одна не входит в список зарезервированных Windows (§1.1)
            // и не конфликтует с глобальными хоткеями resticker
            // (Ctrl+Alt+S/H/M/T) — проверяется тестом
            // `default_bindings_avoid_windows_reserved_combos`.
            bindings: default_bindings(),
            // Пустой список — все мониторы (см. доккомент поля).
            monitors: Vec::new(),
            own_alt_tab: false,
        }
    }
}

/// Один бинд горячей клавиши тайлинга (docs/research/tiling/R4_KEYBINDS.md §4).
///
/// `combo` — строка в формате `HotkeyCombo::parse` (rst-win32, hotkey.rs:38),
/// например `Alt+Shift+H`; парсит и регистрирует координатор в
/// платформенном слое. В `rst-core` комбинация остаётся строкой намеренно:
/// парсер живёт в `rst-win32`, а здесь только конфиг.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TilingBinding {
    /// Комбинация в формате HotkeyCombo::parse, например Alt+Shift+H.
    pub combo: String,
    /// Что делать, snake_case: focus_direction, move_direction,
    /// swap_direction, resize, toggle_split, toggle_group, cycle_group,
    /// workspace, send_to_workspace, toggle_floating, toggle_fullscreen,
    /// close_window, enter_submap, leave_submap.
    pub action: String,
    /// Аргумент действия: left/right/up/down, номер воркспейса, имя submap.
    pub arg: Option<String>,
    /// Submap, в котором бинд активен. None - глобальный бинд.
    pub submap: Option<String>,
}

/// Бинды по умолчанию — таблица из R4_KEYBINDS.md §4.3.
///
/// Все комбинации на Alt/Alt+Shift (не Win, не Ctrl+Alt+Del): ни одна не
/// входит в список, который Windows забирает себе (§1.1: Win+L/Tab/стрелки/
/// D/E/R/I/S/A/N/1..9, Ctrl+Alt+Del, Ctrl+Shift+Esc). `Alt+Shift+Space` и
/// «голые» клавиши submaps потребуют расширения `HotkeyCombo::parse`
/// (R4 §4.2, задача T3) — конфиг к этому готов заранее: строка здесь не
/// валидируется, расширение парсера ничего в схеме не меняет.
fn default_bindings() -> Vec<TilingBinding> {
    fn b(combo: &str, action: &str, arg: Option<&str>) -> TilingBinding {
        TilingBinding {
            combo: combo.to_string(),
            action: action.to_string(),
            arg: arg.map(String::from),
            submap: None,
        }
    }
    fn m(combo: &str, action: &str, arg: Option<&str>, submap: &str) -> TilingBinding {
        TilingBinding {
            submap: Some(submap.to_string()),
            ..b(combo, action, arg)
        }
    }
    let mut out = Vec::new();
    // Направления — hjkl, как в i3, Hyprland и vim; стрелки пользователь
    // допишет сам, если они ему привычнее.
    for (key, dir) in [("H", "left"), ("J", "down"), ("K", "up"), ("L", "right")] {
        out.push(b(&format!("Alt+{key}"), "focus_direction", Some(dir)));
        out.push(b(&format!("Alt+Shift+{key}"), "move_direction", Some(dir)));
        // В модальном режиме те же клавиши без модификаторов меняют размер.
        // Выход из режима — Escape, он захардкожен в самой таблице биндов
        // (`rst_core::tiling::binds`) как аварийный: пользователь, застрявший
        // в режиме без выхода, теряет клавиатуру целиком.
        out.push(m(key, "resize", Some(dir), "resize"));
    }
    for n in 1..=9 {
        out.push(b(&format!("Alt+{n}"), "workspace", Some(&n.to_string())));
        out.push(b(
            &format!("Alt+Shift+{n}"),
            "send_to_workspace",
            Some(&n.to_string()),
        ));
    }
    out.push(b("Alt+E", "toggle_split", None));
    out.push(b("Alt+G", "toggle_group", None));
    out.push(b("Alt+F", "toggle_fullscreen", None));
    out.push(b("Alt+Shift+Space", "toggle_floating", None));
    out.push(b("Alt+Shift+Q", "close_window", None));
    out.push(b("Alt+R", "enter_submap", Some("resize")));
    out
}

/// Комбинация из тех, что Windows не отдаст ни одному приложению
/// (docs/research/tiling/R4_KEYBINDS.md §1.1): `Win+…` целиком (Win+L,
/// Win+Tab, Win+стрелки, Win+D, Win+E/R/I/S/A/N/1..9 — оболочка регистрирует
/// их при входе в сессию, до нас), плюс аппаратные `Ctrl+Alt+Del` и
/// `Ctrl+Shift+Esc`, которые перехватывает ядро ещё до очереди сообщений.
///
/// Проверка строковая и регистронезависимая: `rst-core` платформенно-чист,
/// на этапе конфига у нас только строка [`TilingBinding::combo`]. Зовёт
/// координатор при регистрации биндов, чтобы честно сказать пользователю
/// «эту комбинацию Windows не отдаст», и тесты умолчаний.
pub fn is_windows_reserved(combo: &str) -> bool {
    // Пробелы вокруг токенов игнорируем, как и HotkeyCombo::parse
    // (rst-win32, hotkey.rs:48) — "Win + Tab" и "Win+Tab" одно и то же.
    let c: String = combo
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    c.starts_with("win+") || matches!(c.as_str(), "ctrl+alt+del" | "ctrl+shift+esc")
}

/// Загрузить конфиг (CONFIG.md, «Правила записи»):
/// отсутствующий файл → дефолты (первый запуск); старая схема → config.bak
/// перед миграцией; битый файл → config.corrupt.\<timestamp\>.json, затем
/// попытка config.bak, затем дефолты с предупреждением. Конфиг новее текущей
/// схемы — ошибка, файл не трогаем.
pub fn load(path: &Path) -> Result<ConfigLoad, CoreError> {
    if !path.exists() {
        return Ok(ConfigLoad {
            config: Config::default(),
            warning: None,
        });
    }
    match try_load(path) {
        Ok(config) => Ok(ConfigLoad {
            config,
            warning: None,
        }),
        Err(CoreError::UnknownSchemaVersion(v)) => Err(CoreError::UnknownSchemaVersion(v)),
        Err(_) => {
            // Битый основной файл: сохраняем его для разбора и пробуем .bak.
            let _ = fs::rename(path, corrupt_path(path));
            let bak = bak_path(path);
            if bak.exists() {
                if let Ok(config) = try_load(&bak) {
                    return Ok(ConfigLoad {
                        config,
                        warning: Some(ConfigWarning::CorruptRecoveredFromBak),
                    });
                }
            }
            Ok(ConfigLoad {
                config: Config::default(),
                warning: Some(ConfigWarning::CorruptLoadedDefaults),
            })
        }
    }
}

/// Прочитать и разобрать файл, при необходимости применив миграции.
fn try_load(path: &Path) -> Result<Config, CoreError> {
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if version > CURRENT_SCHEMA_VERSION as u64 {
        return Err(CoreError::UnknownSchemaVersion(version));
    }
    let mut value = if version < CURRENT_SCHEMA_VERSION as u64 {
        // CONFIG.md: перед первой миграцией сохраняется config.bak.
        let _ = fs::copy(path, bak_path(path));
        migrate(value)?
    } else {
        value
    };
    // Редизайн пинов: старые записи `"kind": "window"` (закреплённые окна)
    // больше не существуют в модели и не мигрируются никуда — вычищаем их ДО
    // десериализации, чтобы один мёртвый вариант не уронил загрузку ВСЕГО
    // конфига (see `strip_legacy_window_stickers`).
    strip_legacy_window_stickers(&mut value);
    serde_json::from_value(value).map_err(CoreError::Migrate)
}

/// Вычистить записи с `"kind": "window"` (удалённый вариант `StickerSource`)
/// из `stickers` и из каждого пресета — старые закреплённые окна были
/// персистентными стикерами, редизайн пинов сделал закрепление чисто
/// рантайм-состоянием (SPEC.md, «Закрепление окна»: «нельзя сохранить в
/// пресет, всегда нужно выставлять вручную»). Решение: НЕ ронять весь
/// config.json/файл пресета из-за одного мёртвого поля — записи молча
/// отбрасываются, остальной конфиг грузится как обычно.
pub fn strip_legacy_window_stickers(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(stickers)) = obj.get_mut("stickers") {
        stickers.retain(|s| !is_legacy_window_sticker(s));
    }
    if let Some(Value::Array(presets)) = obj.get_mut("presets") {
        for preset in presets {
            if let Some(Value::Array(stickers)) = preset.get_mut("stickers") {
                stickers.retain(|s| !is_legacy_window_sticker(s));
            }
        }
    }
}

/// Запись стикера — удалённый вариант `"kind": "window"`? Тег варианта живёт
/// внутри поля `source` (внутренняя тегированность `StickerSource`), поэтому
/// смотрим `source.kind`, а не верхний уровень записи.
fn is_legacy_window_sticker(sticker: &Value) -> bool {
    sticker
        .get("source")
        .and_then(|source| source.get("kind"))
        .and_then(Value::as_str)
        == Some("window")
}

/// Миграции схемы — чистые функции vN → vN+1 (CONFIG.md, «Миграции»).
pub fn migrate(mut value: Value) -> Result<Value, CoreError> {
    let mut version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if version > CURRENT_SCHEMA_VERSION as u64 {
        return Err(CoreError::UnknownSchemaVersion(version));
    }
    while version < CURRENT_SCHEMA_VERSION as u64 {
        value = match version {
            0 => migrate_v0_to_v1(value)?,
            _ => return Err(CoreError::UnknownSchemaVersion(version)),
        };
        version += 1;
    }
    Ok(value)
}

/// Заглушка v0 → v1: v0 — «конфиг без номера версии». Существующие значения
/// сохраняются, недостающие секции достраиваются дефолтами.
fn migrate_v0_to_v1(mut value: Value) -> Result<Value, CoreError> {
    let Some(obj) = value.as_object_mut() else {
        return Err(CoreError::NotAnObject);
    };
    obj.insert("schema_version".to_string(), Value::from(1));
    let defaults = serde_json::to_value(Config::default()).map_err(CoreError::Migrate)?;
    for key in ["settings", "hotkeys", "monitors", "stickers", "presets"] {
        obj.entry(key).or_insert_with(|| defaults[key].clone());
    }
    Ok(value)
}
/// Атомарная запись (CONFIG.md, «Атомарность обязательна»):
/// tmp-файл в той же папке → flush + fsync → rename поверх целевого.
/// Перед сериализацией порядок стикеров нормализуется к 0..N.
pub fn save(config: &Config, path: &Path) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut config = config.clone();
    normalize_orders(&mut config);
    let json = serde_json::to_string_pretty(&config).map_err(CoreError::Migrate)?;
    let tmp = tmp_path(path);
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?; // FlushFileBuffers
    }
    // На Windows std::fs::rename использует MoveFileExW с
    // MOVEFILE_REPLACE_EXISTING, то есть замена целевого файла атомарна.
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Нормализация порядка отрисовки к последовательности 0..N при сохранении
/// (CONFIG.md, «order»). Относительный порядок при равных значениях
/// сохраняется (сортировка стабильная).
fn normalize_orders(config: &mut Config) {
    let mut order: Vec<usize> = (0..config.stickers.len()).collect();
    order.sort_by_key(|&i| config.stickers[i].order);
    for (new_order, idx) in order.into_iter().enumerate() {
        config.stickers[idx].order = new_order as i64;
    }
}

/// tmp-файл рядом с целевым: config.json.tmp.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Резервная копия рядом с целевым: config.bak.
fn bak_path(path: &Path) -> PathBuf {
    path.with_file_name("config.bak")
}

/// Имя для битого файла: config.corrupt.<timestamp>.json.
fn corrupt_path(path: &Path) -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.corrupt.{ts}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Config, MonitorId};
    use crate::tiling::{InsertPolicy, RuleAction, RuleMatch, WindowRule};

    #[test]
    fn config_without_tiling_section_loads_with_defaults() {
        // Старый config.json (схема 1) без секции "tiling" обязан читаться
        // штатным путём load() и давать секцию с умолчаниями (тайлинг
        // выключен) — та же обратная совместимость, что у `presets`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "settings": {},
                "hotkeys": {},
                "monitors": [],
                "stickers": [],
                "presets": []
            }"#,
        )
        .unwrap();
        let loaded = load(&path).expect("старый конфиг обязан читаться");
        assert_eq!(loaded.warning, None);
        assert_eq!(loaded.config.tiling, TilingConfig::default());
        assert!(!loaded.config.tiling.enabled);
    }

    #[test]
    fn v0_config_without_tiling_migrates_and_loads() {
        // v0 — «конфиг без schema_version». Миграция достраивает секции
        // дефолтами; tiling туда намеренно не добавляется — недостающее
        // поле достраивает serde тем же механизмом, что и для v1
        // (см. доккомент TilingConfig).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{ "settings": {}, "hotkeys": {}, "stickers": [] }"#,
        )
        .unwrap();
        let loaded = load(&path).expect("v0 конфиг обязан читаться");
        assert_eq!(loaded.config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.config.tiling, TilingConfig::default());
    }

    #[test]
    fn config_with_tiling_section_overrides_defaults() {
        // Секция реально читается, а не только достраивается дефолтами:
        // каждое поле из JSON попадает в модель.
        let raw = r#"{
            "schema_version": 1,
            "tiling": {
                "enabled": true,
                "gaps_in": 12,
                "gaps_out": 16,
                "tab_bar_h": 30,
                "insert_policy": "master",
                "bindings": [{ "combo": "Alt+Q", "action": "close_window" }],
                "monitors": ["\\\\?\\DISPLAY#A"]
            }
        }"#;
        let value: Value = serde_json::from_str(raw).unwrap();
        let value = migrate(value).unwrap();
        let cfg: Config = serde_json::from_value(value).unwrap();
        assert!(cfg.tiling.enabled);
        assert_eq!(cfg.tiling.gaps_in, 12);
        assert_eq!(cfg.tiling.gaps_out, 16);
        assert_eq!(cfg.tiling.tab_bar_h, 30);
        assert_eq!(cfg.tiling.insert_policy, InsertPolicy::Master);
        assert_eq!(cfg.tiling.bindings.len(), 1);
        assert_eq!(
            cfg.tiling.monitors,
            vec![MonitorId("\\\\?\\DISPLAY#A".into())]
        );
        assert!(
            cfg.tiling.rules.is_empty(),
            "секция без rules — пустые rules (serde-default)"
        );
    }

    #[test]
    fn tiling_disabled_by_default() {
        assert!(!TilingConfig::default().enabled);
        assert!(
            !Config::default().tiling.enabled,
            "корень конфига тоже выключен"
        );
    }

    #[test]
    fn tiling_config_roundtrip_through_json() {
        let cfg = TilingConfig {
            enabled: true,
            gaps_in: 12,
            gaps_out: 16,
            tab_bar_h: 30,
            insert_policy: InsertPolicy::Manual,
            rules: vec![WindowRule {
                matcher: RuleMatch::default(),
                action: RuleAction::Float,
            }],
            bindings: vec![TilingBinding {
                combo: "Alt+Q".into(),
                action: "close_window".into(),
                arg: None,
                submap: None,
            }],
            monitors: vec![MonitorId("M1".into())],
            own_alt_tab: true,
        };
        let json = serde_json::to_value(&cfg).unwrap();
        let back: TilingConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn full_config_roundtrip_with_tiling() {
        let mut cfg = Config::default();
        cfg.tiling.enabled = true;
        cfg.tiling.gaps_in = 10;
        cfg.tiling.bindings = default_bindings();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn default_bindings_avoid_windows_reserved_combos() {
        for b in TilingConfig::default().bindings {
            assert!(
                !is_windows_reserved(&b.combo),
                "{} зарезервирована Windows — из умолчаний вон (R4 §1.1)",
                b.combo
            );
        }
    }

    #[test]
    fn windows_reserved_combos_are_detected() {
        for combo in [
            "Win+L",
            "Win+Tab",
            "Win+Left",
            "Win+D",
            "Win+E",
            "Win+1",
            "Win+9",
            "CTRL+ALT+DEL",
            "Ctrl+Shift+Esc",
            "win+l",
            " Win + Tab ",
        ] {
            assert!(is_windows_reserved(combo), "{combo} не опознан как занятый");
        }
    }

    #[test]
    fn tiling_combos_are_not_windows_reserved() {
        for combo in ["Alt+H", "Alt+Shift+Space", "Alt+R", "Ctrl+Alt+S"] {
            assert!(!is_windows_reserved(combo), "{combo} ложно зарезервирован");
        }
    }

    #[test]
    fn default_bindings_are_well_formed_hotkeys() {
        // Формат HotkeyCombo::parse (rst-win32, hotkey.rs:38): модификаторы +
        // клавиша.
        //
        // Голая клавиша без модификатора допустима РОВНО в модальном режиме
        // (submap) — там она и нужна: нажал Alt+R, дальше hjkl меняют размер.
        // Глобальный бинд без модификатора был бы тяжёлым багом: он проглотил
        // бы обычную клавишу у всей системы, и пользователь не смог бы ею
        // печатать. Разбирает такие комбинации отдельный конструктор
        // `HotkeyCombo::parse_binding`, тогда как `parse` их по-прежнему
        // отвергает (hotkey.rs:66).
        const MODS: [&str; 4] = ["ctrl", "alt", "shift", "win"];
        for b in TilingConfig::default().bindings {
            let tokens: Vec<&str> = b.combo.split('+').map(str::trim).collect();
            let (mods, key) = tokens.split_at(tokens.len() - 1);
            assert!(!key[0].is_empty(), "{}: клавиша пуста", b.combo);
            assert!(
                mods.iter()
                    .all(|m| MODS.contains(&m.to_ascii_lowercase().as_str())),
                "{}: неизвестный модификатор",
                b.combo
            );
            if mods.is_empty() {
                assert!(
                    b.submap.is_some(),
                    "{}: голая клавиша вне модального режима проглотила бы её у всей системы",
                    b.combo
                );
            }
        }
    }

    #[test]
    fn empty_monitors_default_to_all_monitors() {
        // Семантика поля: пустой список = тайлинг на всех мониторах
        // (решает координатор). Умолчание пустое — новый пользователь не
        // найдёт свои мониторы «отключёнными».
        assert!(TilingConfig::default().monitors.is_empty());
    }

    #[test]
    fn unknown_action_does_not_break_config_parsing() {
        // action — String, а не enum: конфиг правит человек руками, и
        // опечатка в действии не должна ронять ВЕСЬ конфиг (тогда не
        // загрузились бы и стикеры). Неизвестное значение доживает до
        // координатора, который покажет предупреждение точечно — ни молча,
        // ни фатально. Enum-вариант такого выбора не оставляет.
        let raw = r#"{
            "schema_version": 1,
            "tiling": {
                "bindings": [
                    { "combo": "Alt+X", "action": "make_it_rain", "arg": "left" },
                    { "combo": "Alt+Q", "action": "close_window" }
                ]
            }
        }"#;
        let value: Value = serde_json::from_str(raw).unwrap();
        let value = migrate(value).unwrap();
        let cfg: Config = serde_json::from_value(value).unwrap();
        assert_eq!(cfg.tiling.bindings[0].action, "make_it_rain");
        assert_eq!(cfg.tiling.bindings[0].arg.as_deref(), Some("left"));
        assert_eq!(cfg.tiling.bindings[1].action, "close_window");
    }

    #[test]
    fn binding_without_arg_and_submap_is_valid() {
        // Строки R4 §4.3 опускают arg/submap — отсутствующие поля
        // достраиваются дефолтами (#[serde(default)] на структуре).
        let b: TilingBinding =
            serde_json::from_str(r#"{ "combo": "Alt+F", "action": "toggle_fullscreen" }"#).unwrap();
        assert_eq!(b.combo, "Alt+F");
        assert_eq!(b.arg, None);
        assert_eq!(b.submap, None);
    }

    #[test]
    fn insert_policy_serializes_as_snake_case() {
        // Тот же формат, что у RuleAction: в config.json "dwindle"/"master"/
        // "manual", а не имена вариантов Rust.
        assert_eq!(
            serde_json::to_value(InsertPolicy::Dwindle).unwrap(),
            "dwindle"
        );
        assert_eq!(
            serde_json::from_value::<InsertPolicy>(serde_json::json!("master")).unwrap(),
            InsertPolicy::Master
        );
        assert_eq!(
            serde_json::from_value::<InsertPolicy>(serde_json::json!("manual")).unwrap(),
            InsertPolicy::Manual
        );
    }

    #[test]
    fn default_gaps_and_tab_bar_height_are_sane() {
        let d = TilingConfig::default();
        assert_eq!(d.gaps_in, 8, "зазор между плитками");
        assert_eq!(d.gaps_out, 8, "зазор до краёв рабочей области");
        assert_eq!(d.tab_bar_h, 24, "высота таб-бара");
        assert_eq!(d.insert_policy, InsertPolicy::Dwindle);
    }
}
