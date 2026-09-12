//! Пресеты (SPEC.md, раздел 11; ROADMAP.md M7): сохранение, переименование,
//! удаление и применение полной расстановки стикеров, а также экспорт/
//! импорт файла пресета.
//!
//! Операции над списком пресетов — чистые, как `ops` (только читают и
//! мутируют переданный `Config`, без ввода-вывода). Файловые операции —
//! по паттерну `config::save`/`load`: атомарная запись (tmp + rename),
//! `serde_json`, ошибки — через [`CoreError`].

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::CoreError;
use crate::model::{Config, Preset, StickerSource};

/// Ошибка операции с пресетами.
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    /// Пресет с данным id не найден в списке `cfg.presets`.
    #[error("no preset with id {0}")]
    PresetNotFound(Uuid),
    /// Ошибка ввода-вывода или JSON при экспорте/импорте файла пресета
    /// (те же варианты, что у `config`).
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Результат применения пресета (SPEC.md §11, «Загрузка с недостающими
/// элементами»): пресет применён без недоступных стикеров, их список —
/// здесь, для диалога недостающих элементов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPresetOutcome {
    /// Стикеры пресета, чей источник `File` отсутствует на диске:
    /// `(id стикера, путь)` в порядке следования в пресете.
    pub missing: Vec<(Uuid, PathBuf)>,
}

/// Снимок текущей расстановки в новый пресет (SPEC.md §11, «Сохранение и
/// загрузка полной расстановки»): глубокая копия всех стикеров `cfg`
/// (источники, позиции, размеры, повороты, прозрачность, порядок, правила
/// видимости, воспроизведение — всё переезжает как есть), свежий `id`.
///
/// Список пресетов не мутируется: вызывающий слой сам добавляет результат
/// в `cfg.presets` (и сохраняет через `config::save` — единый путь записи).
pub fn save_preset(cfg: &Config, name: String) -> Preset {
    Preset {
        id: Uuid::new_v4(),
        name,
        stickers: cfg.stickers.clone(),
    }
}

/// Переименовать пресет по id.
pub fn rename_preset(cfg: &mut Config, id: Uuid, new_name: String) -> Result<(), PresetError> {
    let preset = cfg
        .presets
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or(PresetError::PresetNotFound(id))?;
    preset.name = new_name;
    Ok(())
}

/// Удалить пресет из списка. Файлы источников стикеров не трогаются
/// (пресет — только записи конфига).
pub fn delete_preset(cfg: &mut Config, id: Uuid) -> Result<(), PresetError> {
    let i = cfg
        .presets
        .iter()
        .position(|p| p.id == id)
        .ok_or(PresetError::PresetNotFound(id))?;
    cfg.presets.remove(i);
    Ok(())
}

/// Применить пресет (SPEC.md §11): заменяет `cfg.stickers` на стикеры
/// пресета, но только те, чей источник доступен:
///
/// - `File { path, .. }` — только если `path.exists()` физически;
///   отсутствующие перечисляются в [`ApplyPresetOutcome::missing`]
///   (SPEC: диалог [Загрузить остальное] применяет пресет без них);
/// - `Pasted` — всегда (файл вставки лежит в профиле приложения и
///   пресетом не перемещается).
///
/// Сам пресет не модифицируется; несуществующий id — ошибка без мутации
/// конфига.
pub fn apply_preset(cfg: &mut Config, id: Uuid) -> Result<ApplyPresetOutcome, PresetError> {
    let stickers = cfg
        .presets
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.stickers.clone())
        .ok_or(PresetError::PresetNotFound(id))?;
    let mut applied = Vec::new();
    let mut missing = Vec::new();
    for sticker in stickers {
        match &sticker.source {
            StickerSource::File { path, .. } if !path.exists() => {
                missing.push((sticker.id, path.clone()));
            }
            // Кусок окна применяется всегда, даже если приложения сейчас нет:
            // «пропал» здесь означает удалённый файл, а окно — вещь временно
            // отсутствующая, и кусок обязан дождаться его возвращения
            // (решение пользователя 2026-09-10). Пока окна нет, координатор
            // держит кусок скрытым; список `missing` для этого не нужен.
            StickerSource::File { .. }
            | StickerSource::Pasted { .. }
            | StickerSource::WindowCrop { .. } => applied.push(sticker),
        }
    }
    cfg.stickers = applied;
    Ok(ApplyPresetOutcome { missing })
}

/// Экспорт пресета в файл (CONFIG.md, «Экспорт и импорт»): JSON одного
/// пресета, атомарная запись по паттерну `config::save` (tmp-файл рядом →
/// flush + fsync → rename поверх).
///
/// **Персональные данные:** внутри файла — абсолютные пути к файлам
/// пользователя и имена процессов (CONFIG.md). UI ОБЯЗАН показать
/// предупреждение перед экспортом (SPEC.md §11; CONFIG.md) — сам код
/// не предупреждает.
pub fn export_preset_to_file(cfg: &Config, id: Uuid, path: &Path) -> Result<(), PresetError> {
    let preset = cfg
        .presets
        .iter()
        .find(|p| p.id == id)
        .ok_or(PresetError::PresetNotFound(id))?;
    write_preset_file(preset, path)
}

/// Импорт пресета из файла (CONFIG.md, «Экспорт и импорт»): читает JSON
/// одного пресета и добавляет его в `cfg.presets`. Импорт всегда
/// присваивает пресету новый `id` — файл пресета это переносимое описание
/// расстановки, а не ссылка на запись чужого конфига: повторный импорт
/// того же файла не конфликтует с существующими пресетами. Имя и стикеры
/// сохраняются как в файле. Возвращает добавленный пресет.
pub fn import_preset_from_file(cfg: &mut Config, path: &Path) -> Result<Preset, PresetError> {
    let raw = fs::read_to_string(path).map_err(CoreError::Io)?;
    let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(CoreError::Json)?;
    // Старый файл пресета мог нести закреплённые окна (`"kind": "window"`) —
    // вариант удалён редизайном пинов, вычищаем ДО десериализации (см.
    // `config::strip_legacy_window_stickers`), иначе пресет не импортировался
    // бы целиком.
    crate::config::strip_legacy_window_stickers(&mut value);
    let mut preset: Preset = serde_json::from_value(value).map_err(CoreError::Json)?;
    preset.id = Uuid::new_v4();
    cfg.presets.push(preset.clone());
    Ok(preset)
}

/// Атомарная запись файла пресета (тот же приём, что `config::save`):
/// tmp-файл в той же папке → flush + fsync → rename поверх (на Windows —
/// `MoveFileExW` с `MOVEFILE_REPLACE_EXISTING`).
fn write_preset_file(preset: &Preset, path: &Path) -> Result<(), PresetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CoreError::Io)?;
    }
    let json = serde_json::to_string_pretty(preset).map_err(CoreError::Json)?;
    let tmp = tmp_path(path);
    {
        let mut file = fs::File::create(&tmp).map_err(CoreError::Io)?;
        file.write_all(json.as_bytes()).map_err(CoreError::Io)?;
        file.sync_all().map_err(CoreError::Io)?; // FlushFileBuffers
    }
    fs::rename(&tmp, path).map_err(CoreError::Io)?;
    Ok(())
}

/// tmp-файл рядом с целевым: `<name>.json.tmp`.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaType, Sticker};

    fn file_sticker(id: u128, path: &str, order: i64) -> Sticker {
        Sticker {
            id: Uuid::from_u128(id),
            order,
            source: StickerSource::File {
                path: PathBuf::from(path),
                media_type: MediaType::Image,
            },
            ..Default::default()
        }
    }

    fn pasted_sticker(id: u128, order: i64) -> Sticker {
        Sticker {
            id: Uuid::from_u128(id),
            order,
            source: StickerSource::Pasted {
                path: PathBuf::from("pasted/x.png"),
            },
            ..Default::default()
        }
    }

    fn preset(id: u128, name: &str, stickers: Vec<Sticker>) -> Preset {
        Preset {
            id: Uuid::from_u128(id),
            name: name.to_string(),
            stickers,
        }
    }

    fn cfg_with(presets: Vec<Preset>) -> Config {
        Config {
            presets,
            ..Default::default()
        }
    }

    /// Ошибка — ровно `PresetNotFound(id)` (PresetError не PartialEq:
    /// обёрнутый CoreError тоже нет, ср. `matches!` в config_io.rs).
    fn assert_not_found<E: std::fmt::Debug>(result: Result<E, PresetError>, id: u128) {
        assert!(
            matches!(result, Err(PresetError::PresetNotFound(got)) if got == Uuid::from_u128(id)),
            "ожидался PresetNotFound({id}), получено: {result:?}"
        );
    }

    // --- сохранение снимка (SPEC §11) ---

    #[test]
    fn save_preset_snapshots_stickers_deeply() {
        let mut cfg = Config {
            stickers: vec![
                file_sticker(1, "C:\\pics\\cat.png", 0),
                pasted_sticker(2, 1),
            ],
            ..Default::default()
        };
        let snapshot = save_preset(&cfg, "Стрим".to_string());
        assert_ne!(snapshot.id, Uuid::nil());
        assert_eq!(snapshot.name, "Стрим");
        assert_eq!(snapshot.stickers, cfg.stickers);
        // Глубокая копия: правка конфига после снимка не влияет на пресет.
        cfg.stickers[0].placement.cx = 999.0;
        cfg.stickers.pop();
        assert_eq!(snapshot.stickers.len(), 2);
        assert_eq!(snapshot.stickers[0].placement.cx, 0.0);
    }

    #[test]
    fn save_preset_generates_unique_ids() {
        let cfg = Config::default();
        let a = save_preset(&cfg, "A".to_string());
        let b = save_preset(&cfg, "B".to_string());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn save_preset_does_not_touch_presets_list() {
        let cfg = cfg_with(vec![preset(7, "Существующий", vec![])]);
        let before = cfg.clone();
        let _ = save_preset(&cfg, "Новый".to_string());
        assert_eq!(cfg, before, "снимок не мутирует конфиг");
    }

    // --- переименование / удаление ---

    #[test]
    fn rename_preset_updates_name_by_id() {
        let mut cfg = cfg_with(vec![
            preset(1, "Старое имя", vec![]),
            preset(2, "Другой", vec![]),
        ]);
        rename_preset(&mut cfg, Uuid::from_u128(1), "Новое имя".to_string()).unwrap();
        assert_eq!(cfg.presets[0].name, "Новое имя");
        assert_eq!(cfg.presets[1].name, "Другой", "прочие пресеты не тронуты");
    }

    #[test]
    fn rename_preset_unknown_id_errors() {
        let mut cfg = cfg_with(vec![preset(1, "A", vec![])]);
        let before = cfg.clone();
        assert_not_found(
            rename_preset(&mut cfg, Uuid::from_u128(99), "X".to_string()),
            99,
        );
        assert_eq!(cfg, before, "ошибка без мутации");
    }

    #[test]
    fn delete_preset_removes_only_target() {
        let mut cfg = cfg_with(vec![
            preset(1, "A", vec![]),
            preset(2, "B", vec![]),
            preset(3, "C", vec![]),
        ]);
        delete_preset(&mut cfg, Uuid::from_u128(2)).unwrap();
        let ids: Vec<Uuid> = cfg.presets.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![Uuid::from_u128(1), Uuid::from_u128(3)]);
    }

    #[test]
    fn delete_preset_unknown_id_errors() {
        let mut cfg = cfg_with(vec![preset(1, "A", vec![])]);
        let before = cfg.clone();
        assert_not_found(delete_preset(&mut cfg, Uuid::from_u128(99)), 99);
        assert_eq!(cfg, before);
    }

    // --- применение (SPEC §11, «Загрузка с недостающими элементами») ---

    #[test]
    fn apply_preset_replaces_stickers_with_present_ones_only() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("cat.png");
        std::fs::write(&existing, b"x").unwrap();

        let mut cfg = cfg_with(vec![preset(
            1,
            "Расстановка",
            vec![
                file_sticker(10, existing.to_str().unwrap(), 0),
                file_sticker(11, "W:\\gone\\missing.png", 1),
                pasted_sticker(12, 2),
            ],
        )]);
        cfg.stickers = vec![file_sticker(99, "W:\\старое.png", 0)];

        let outcome = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        assert_eq!(
            outcome.missing,
            vec![(Uuid::from_u128(11), PathBuf::from("W:\\gone\\missing.png"))]
        );
        assert_eq!(
            cfg.stickers,
            vec![
                file_sticker(10, existing.to_str().unwrap(), 0),
                pasted_sticker(12, 2),
            ],
            "применены существующие + Pasted, отсутствующий пропущен"
        );
    }

    #[test]
    fn apply_preset_pasted_always_present_even_if_file_gone() {
        // Pasted считается присутствующим всегда: файл вставки живёт внутри
        // профиля приложения и не перемещается пресетом.
        let mut cfg = cfg_with(vec![preset(1, "P", vec![pasted_sticker(10, 0)])]);
        let outcome = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        assert!(outcome.missing.is_empty());
        assert_eq!(cfg.stickers, vec![pasted_sticker(10, 0)]);
    }

    #[test]
    fn apply_preset_all_missing_applies_nothing() {
        let mut cfg = cfg_with(vec![preset(
            1,
            "Всё пропало",
            vec![
                file_sticker(10, "W:\\a\\1.png", 0),
                file_sticker(11, "W:\\a\\2.png", 1),
            ],
        )]);
        cfg.stickers = vec![file_sticker(99, "W:\\b\\old.png", 0)];

        let outcome = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        assert_eq!(outcome.missing.len(), 2);
        assert!(cfg.stickers.is_empty(), "замена на пустой набор");
    }

    #[test]
    fn apply_preset_empty_preset_clears_stickers() {
        let mut cfg = cfg_with(vec![preset(1, "Пусто", vec![])]);
        cfg.stickers = vec![file_sticker(99, "W:\\x.png", 0)];
        let outcome = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        assert!(outcome.missing.is_empty());
        assert!(cfg.stickers.is_empty());
    }

    #[test]
    fn apply_preset_unknown_id_errors_without_mutation() {
        let mut cfg = cfg_with(vec![preset(1, "A", vec![pasted_sticker(10, 0)])]);
        cfg.stickers = vec![file_sticker(99, "W:\\x.png", 0)];
        let before = cfg.clone();
        assert_not_found(apply_preset(&mut cfg, Uuid::from_u128(99)), 99);
        assert_eq!(cfg, before);
    }

    #[test]
    fn apply_preset_does_not_modify_preset() {
        let mut cfg = cfg_with(vec![preset(1, "A", vec![pasted_sticker(10, 0)])]);
        let before = cfg.presets.clone();
        let _ = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        assert_eq!(
            cfg.presets, before,
            "пресет остаётся для повторного применения"
        );
    }

    #[test]
    fn apply_preset_is_repeatable() {
        // Применение одного пресета дважды идемпотентно по результату.
        let mut cfg = cfg_with(vec![preset(1, "A", vec![pasted_sticker(10, 0)])]);
        let first = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        let second = apply_preset(&mut cfg, Uuid::from_u128(1)).unwrap();
        assert_eq!(first, second);
        assert_eq!(cfg.stickers, vec![pasted_sticker(10, 0)]);
    }

    // --- экспорт/импорт файла (CONFIG.md, «Экспорт и импорт») ---

    #[test]
    fn export_then_import_roundtrip_preserves_name_and_stickers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        let original = preset(
            1,
            "Стрим",
            vec![
                file_sticker(10, "C:\\pics\\cat.png", 0),
                pasted_sticker(11, 1),
            ],
        );
        let cfg = cfg_with(vec![original.clone()]);

        export_preset_to_file(&cfg, Uuid::from_u128(1), &path).unwrap();
        let mut target = Config::default();
        let imported = import_preset_from_file(&mut target, &path).unwrap();

        assert_eq!(imported.name, "Стрим");
        assert_eq!(imported.stickers, original.stickers);
        assert_eq!(target.presets, vec![imported.clone()]);
    }

    #[test]
    fn import_always_assigns_fresh_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        let cfg = cfg_with(vec![preset(1, "A", vec![pasted_sticker(10, 0)])]);
        export_preset_to_file(&cfg, Uuid::from_u128(1), &path).unwrap();

        let mut target = Config::default();
        let first = import_preset_from_file(&mut target, &path).unwrap();
        let second = import_preset_from_file(&mut target, &path).unwrap();
        assert_ne!(first.id, Uuid::from_u128(1), "не наследует id файла");
        assert_ne!(first.id, second.id, "повторный импорт не конфликтует");
        assert_eq!(target.presets.len(), 2);
    }

    #[test]
    fn export_writes_pretty_json_with_expected_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        let cfg = cfg_with(vec![preset(1, "Рабочий стол", vec![pasted_sticker(10, 0)])]);
        export_preset_to_file(&cfg, Uuid::from_u128(1), &path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"name\": \"Рабочий стол\""));
        assert!(raw.contains("\"kind\": \"pasted\""));
        let parsed: Preset = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.id, Uuid::from_u128(1));
    }

    #[test]
    fn export_unknown_id_errors_without_creating_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        let cfg = cfg_with(vec![preset(1, "A", vec![])]);
        assert_not_found(export_preset_to_file(&cfg, Uuid::from_u128(99), &path), 99);
        assert!(!path.exists());
    }

    #[test]
    fn export_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        let cfg = cfg_with(vec![preset(1, "A", vec![pasted_sticker(10, 0)])]);
        export_preset_to_file(&cfg, Uuid::from_u128(1), &path).unwrap();
        let leftover = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!leftover, "атомарная запись не оставляет tmp");
    }

    #[test]
    fn import_missing_file_is_io_error() {
        let mut cfg = Config::default();
        let err =
            import_preset_from_file(&mut cfg, Path::new("W:\\нет_такого\\p.json")).unwrap_err();
        assert!(
            matches!(err, PresetError::Core(CoreError::Io(_))),
            "ожидалась Io-ошибка, получено: {err}"
        );
        assert!(cfg.presets.is_empty());
    }

    #[test]
    fn import_invalid_json_is_json_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        std::fs::write(&path, "это не json {{{").unwrap();
        let mut cfg = Config::default();
        let err = import_preset_from_file(&mut cfg, &path).unwrap_err();
        assert!(
            matches!(err, PresetError::Core(CoreError::Json(_))),
            "ожидалась Json-ошибка, получено: {err}"
        );
        assert!(cfg.presets.is_empty());
    }

    #[test]
    fn import_tolerates_minimal_preset_file() {
        // `#[serde(default)]` на Preset: файл с одним именем парсится —
        // недостающие id/stickers достраиваются дефолтами (та же философия
        // прямого чтения, что у config.json), id заменяется свежим.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset.json");
        std::fs::write(&path, r#"{ "name": "черновик" }"#).unwrap();
        let mut cfg = Config::default();
        let imported = import_preset_from_file(&mut cfg, &path).unwrap();
        assert_eq!(imported.name, "черновик");
        assert!(imported.stickers.is_empty());
        assert_ne!(imported.id, Uuid::nil(), "id не наследуется из файла");
        assert_eq!(cfg.presets, vec![imported]);
    }

    // --- обратная совместимость (без бампа схемы) ---

    #[test]
    fn old_config_without_presets_key_loads_with_empty_list() {
        let cfg: Config = serde_json::from_str(
            r#"{ "schema_version": 1, "stickers": [], "settings": { "language": "en" } }"#,
        )
        .unwrap();
        assert!(
            cfg.presets.is_empty(),
            "старый config.json читается без ошибок"
        );
        assert_eq!(cfg.settings.language, "en");
    }

    #[test]
    fn config_with_presets_roundtrips_through_json() {
        let cfg = cfg_with(vec![preset(
            1,
            "Стрим",
            vec![file_sticker(10, "C:\\pics\\cat.png", 0)],
        )]);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn v0_fixture_migration_adds_empty_presets() {
        let raw = r#"{ "settings": { "autostart": false }, "stickers": [] }"#;
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let migrated = crate::config::migrate(value).unwrap();
        let cfg: Config = serde_json::from_value(migrated).unwrap();
        assert_eq!(cfg.schema_version, 1);
        assert!(
            cfg.presets.is_empty(),
            "миграция v0 достраивает presets дефолтом"
        );
    }
}
