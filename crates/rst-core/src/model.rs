//! Модель данных resticker (CONFIG.md, схема config.json).
//!
//! Все типы сериализуются в JSON точно в задокументированном виде;
//! неизвестные поля при чтении игнорируются (прямая совместимость),
//! недостающие — достраиваются дефолтами.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Стабильный идентификатор монитора (device interface path),
/// переживающий переподключение (ADR-010).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorId(pub String);

/// Глобальные настройки программы (SPEC.md, раздел 10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub autostart: bool,
    pub silent_start: bool,
    pub tray_icon: bool,
    pub hide_from_capture: bool,
    pub never_overlap_taskbar: bool,
    pub skip_delete_confirmation: bool,
    pub battery_fps_limit: u32,
    pub mute_invisible_stickers: bool,
    /// Смещение центра панели у курсора относительно курсора, DIP (SPEC 3.8:
    /// позиция панели запоминается между входами в режим редактирования).
    /// `None` — дефолтное смещение (12, 12).
    pub cursor_panel_offset: Option<(f64, f64)>,
    pub language: String,
    /// Онбординг первого запуска уже показан (ROADMAP.md M8, «первый
    /// запуск: короткий онбординг, показать хоткей») — тост с хоткеем
    /// входа в режим редактирования показывается ровно один раз, дальше
    /// координатор проверяет этот флаг и молчит.
    pub onboarding_shown: bool,
    /// Денй-лист закрепления (SPEC.md, «Закрепление окна»): хоткей-пин окна,
    /// чей процесс подпадает под правило, игнорируется, и такое окно скрыто
    /// из списка выбора в режиме редактирования. Дефолт — пустой список:
    /// старые `config.json` без этого поля получают `Vec::new()` через
    /// `#[serde(default)]` на структуре — та же обратная совместимость без
    /// миграции схемы, что `PlaybackSettings.paused`.
    pub denylist: Vec<OverlapRule>,
    /// Громкость звука закрепления окна хоткеем, проценты 0..=100 (запрос
    /// пользователя 2026-08-19). Независима от `AudioMixer`/`mute_all` —
    /// это UI-отклик на действие, не звук стикера, отдельный канал
    /// громкости (`rst_win32::sound::play_pin_sound`), не завязан на
    /// микшер видео-стикеров.
    pub pin_sound_volume: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: true,
            silent_start: true,
            tray_icon: true,
            hide_from_capture: false,
            never_overlap_taskbar: false,
            skip_delete_confirmation: false,
            battery_fps_limit: 30,
            mute_invisible_stickers: true,
            cursor_panel_offset: None,
            language: "ru".to_string(),
            onboarding_shown: false,
            denylist: Vec::new(),
            pin_sound_volume: 100,
        }
    }
}

/// Глобальные хоткеи; `None` — не назначен (SPEC.md, раздел 3.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub edit_mode: Option<String>,
    pub toggle_all_stickers: Option<String>,
    pub mute_all: Option<String>,
    pub pin_focused_window: Option<String>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            edit_mode: Some("Ctrl+Alt+S".to_string()),
            toggle_all_stickers: Some("Ctrl+Alt+H".to_string()),
            mute_all: Some("Ctrl+Alt+M".to_string()),
            pin_focused_window: Some("Ctrl+Alt+T".to_string()),
        }
    }
}

/// Прямоугольник в физических пикселях (границы монитора).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Запись о мониторе, который программа когда-либо видела (ADR-010, ADR-011).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorRecord {
    pub id: MonitorId,
    pub friendly_name: String,
    pub last_seen: DateTime<Utc>,
    pub last_bounds: Rect,
    pub last_scale: f64,
    pub is_primary: bool,
}

impl Default for MonitorRecord {
    fn default() -> Self {
        Self {
            id: MonitorId::default(),
            friendly_name: String::new(),
            last_seen: DateTime::<Utc>::UNIX_EPOCH,
            last_bounds: Rect::default(),
            last_scale: 1.0,
            is_primary: false,
        }
    }
}

/// Стикер — визуальный объект поверх рабочего стола (SPEC.md, раздел 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sticker {
    pub id: Uuid,
    pub enabled: bool,
    /// Видимость по кнопке «глаз» (в отличие от `enabled` — наличие в списке).
    pub visible: bool,
    /// Порядок отрисовки: больше — выше (CONFIG.md, «order»).
    pub order: i64,
    pub created_at: DateTime<Utc>,
    pub source: StickerSource,
    pub placement: Placement,
    pub transform: Transform,
    pub visibility: VisibilityRule,
    pub playback: PlaybackSettings,
    /// Заполняется только при вынужденной миграции с пропавшего монитора (ADR-011).
    pub origin: Option<Origin>,
}

impl Default for Sticker {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            enabled: true,
            visible: true,
            order: 0,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            source: StickerSource::default(),
            placement: Placement::default(),
            transform: Transform::default(),
            visibility: VisibilityRule::default(),
            playback: PlaybackSettings::default(),
            origin: None,
        }
    }
}

impl Sticker {
    /// Новый стикер из файла, центрированный в точке `(cx, cy)` с размером
    /// `(w, h)` (DIP), обычно — центр основного монитора и нативный размер
    /// изображения (ROADMAP.md M1: «появление в центре основного монитора»).
    /// `order` нормализуется при следующем `config::save` — здесь неважен.
    pub fn new_file(
        path: std::path::PathBuf,
        media_type: MediaType,
        monitor_id: MonitorId,
        cx: f64,
        cy: f64,
        w: f64,
        h: f64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            source: StickerSource::File { path, media_type },
            placement: Placement {
                monitor_id,
                cx,
                cy,
                w,
                h,
            },
            ..Self::default()
        }
    }

    /// Новый стикер из изображения, материализованного из буфера обмена в
    /// `pasted/<uuid>.png` (SPEC 2.1/2.5, `Ctrl+V`). В отличие от
    /// [`Sticker::new_file`] источник — [`StickerSource::Pasted`]: удаление
    /// такого стикера обязано удалить и файл (координатор), не только запись
    /// в конфиге.
    pub fn new_pasted(
        path: std::path::PathBuf,
        monitor_id: MonitorId,
        cx: f64,
        cy: f64,
        w: f64,
        h: f64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            source: StickerSource::Pasted { path },
            placement: Placement {
                monitor_id,
                cx,
                cy,
                w,
                h,
            },
            ..Self::default()
        }
    }
}
/// Источник стикера (SPEC.md, раздел 2.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StickerSource {
    /// Обычный файл; хранится путь к оригиналу, не копия.
    File {
        path: PathBuf,
        media_type: MediaType,
    },
    /// Вставка из буфера, материализованная в pasted/<uuid>.png (SPEC 2.1).
    Pasted { path: PathBuf },
}

impl Default for StickerSource {
    fn default() -> Self {
        Self::File {
            path: PathBuf::new(),
            media_type: MediaType::Image,
        }
    }
}

/// Тип медиа в файле (SPEC.md, раздел 1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    #[default]
    Image,
    Animation,
    Video,
}

/// Расширения файлов, которые `rst-video`/FFmpeg открывает как видео (M5b) —
/// общий список между диалогом выбора файла (`rst_win32::file_dialog`) и
/// определением `MediaType` при добавлении стикера (`add_sticker`), чтобы
/// они не разошлись.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mkv", "mov", "avi"];

/// Размещение стикера: логические (DIP) координаты центра относительно
/// левого верхнего угла своего монитора (ADR-010, CONFIG.md «placement»).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    pub monitor_id: MonitorId,
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
}

/// Геометрическая и цветовая трансформация (SPEC.md, раздел 1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub rotation: f64,
    pub opacity: f64,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            rotation: 0.0,
            opacity: 1.0,
            flip_h: false,
            flip_v: false,
        }
    }
}

/// Правило видимости стикера (SPEC.md, раздел 7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisibilityRule {
    pub mode: VisibilityMode,
    /// Используется при режимах never_overlap / overlap_allowlist.
    pub rules: Vec<OverlapRule>,
}

/// Режимы видимости (SPEC.md, раздел 7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityMode {
    /// Всегда поверх всего.
    #[default]
    Always,
    /// Только на рабочем столе (прячется при любых окнах на мониторе).
    Desktop,
    /// Прячется при перекрытии любым окном.
    NeverOverlap,
    /// Прячется только под окнами из списка разрешений.
    OverlapAllowlist,
}

/// Одно правило перекрытия: process_name и/или title_pattern.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlapRule {
    pub process_name: Option<String>,
    pub title_pattern: Option<String>,
}

/// Настройки воспроизведения для video/GIF (SPEC.md, раздел 8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackSettings {
    pub volume: f64,
    pub speed: f64,
    pub loop_mode: LoopMode,
    pub audio_track: Option<u32>,
    /// Некоторым стикерам нужен звук даже когда они невидимы (SPEC 8.3).
    pub override_mute_when_invisible: bool,
    /// Воспроизведение на паузе (M5b) — не применимо к статичным картинкам,
    /// но живёт здесь, а не в отдельном поле `Sticker`: play/pause — часть
    /// того же UI-жеста, что остальные настройки воспроизведения. Дефолт
    /// `false`: автовоспроизведение при добавлении. Старые `config.json`
    /// без этого поля получают `false` через `#[serde(default)]` на
    /// структуре — отдельная миграция схемы не нужна.
    pub paused: bool,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            speed: 1.0,
            loop_mode: LoopMode::default(),
            audio_track: None,
            override_mute_when_invisible: false,
            paused: false,
        }
    }
}

/// Режим цикла (SPEC.md, раздел 8.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
    #[default]
    Loop,
    Once,
    HoldLastFrame,
}

/// «Дом» стикера, вынужденно мигрировавшего с пропавшего монитора (ADR-011).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub monitor_id: MonitorId,
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub rotation: f64,
    pub migrated_at: DateTime<Utc>,
}

/// Пресет — сохранённая полная расстановка стикеров (SPEC.md, раздел 11;
/// ROADMAP.md M7): только расстановка, без глобальных настроек и хоткеев
/// (CONFIG.md, «Формат пресета»). Список живёт в `Config.presets`; файл
/// пресета — та же сериализация (экспорт/импорт, `presets`).
///
/// `id` — идентификатор записи в списке пресетов, не часть «расстановки»:
/// импорт файла пресета всегда создаёт новый id (`presets::import_preset_from_file`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    pub id: Uuid,
    pub name: String,
    pub stickers: Vec<Sticker>,
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            stickers: Vec::new(),
        }
    }
}

/// Корень config.json (CONFIG.md, «Схема config.json»).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub settings: Settings,
    pub hotkeys: Hotkeys,
    pub monitors: Vec<MonitorRecord>,
    pub stickers: Vec<Sticker>,
    /// Сохранённые пресеты расстановки (SPEC.md, раздел 11).
    ///
    /// Обратная совместимость без бампа `CURRENT_SCHEMA_VERSION`: старые
    /// config.json без этого поля читаются через `#[serde(default)]` на
    /// структуре — тот же прецедент, что `PlaybackSettings.paused`
    /// (docs/M4_PREP_NOTES.md: «миграция схемы не нужна (`#[serde(default)]`)»).
    pub presets: Vec<Preset>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: crate::config::CURRENT_SCHEMA_VERSION,
            settings: Settings::default(),
            hotkeys: Hotkeys::default(),
            monitors: Vec::new(),
            stickers: Vec::new(),
            presets: Vec::new(),
        }
    }
}
