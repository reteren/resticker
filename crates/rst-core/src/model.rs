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
    /// Потолок частоты перерисовок оверлея, кадр/с, — действует ТОЛЬКО пока
    /// машина работает от батареи (SPEC §10, «режим экономии на батарее»).
    ///
    /// От сети оверлей рисует по событиям и ничего не ждёт (ADR-006): на
    /// столе экономить нечего, а придержанный кадр читался бы как вялость
    /// программы. На батарее координатор разводит перерисовки во времени —
    /// кадр не выбрасывается, а откладывается до ближайшего разрешённого
    /// момента (`FpsLimiter` в `overlay_manager.rs`).
    ///
    /// До 2026-09-18 поле сохранялось, но не читалось НИКЕМ: ползунок в
    /// настройках двигался и ни на что не влиял.
    pub battery_fps_limit: u32,
    pub mute_invisible_stickers: bool,
    /// Смещение центра панели у курсора относительно курсора, DIP (SPEC 3.8:
    /// позиция панели запоминается между входами в режим редактирования).
    /// `None` — дефолтное смещение (12, 12).
    pub cursor_panel_offset: Option<(f64, f64)>,
    /// Язык интерфейса. Не используется с 2026-08-23: программа
    /// англоязычная целиком (`resticker/src/i18n.rs`, `ui/i18n.js`).
    /// Поле оставлено, чтобы существующие `config.json` со значением
    /// `"ru"` читались без миграции и чтобы возврат второго языка не
    /// требовал менять схему.
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
    /// Держать обводку на закреплённом окне всё время, пока оно закреплено
    /// (запрос пользователя 2026-08-22: «outline on selected window»). Без
    /// неё единственный постоянный признак закрепления — бейдж-булавка в
    /// углу, а сама рамка лишь мигает в момент закрепления. Дефолт `false`:
    /// рамка поверх ЧУЖОГО окна на всё время — заметное вмешательство в его
    /// интерфейс, включать её должен сам пользователь. Старые `config.json`
    /// без поля получают `false` через `#[serde(default)]` на структуре.
    pub outline_pinned_windows: bool,
    /// Отступ закреплённого окна внутри снап-зоны Windows, проценты 0..=35
    /// (запрос пользователя 2026-08-25). `0` — функция выключена, окно
    /// занимает свою половину/четверть/треть целиком, как его и положила
    /// Windows; иначе окно ужимается на этот процент по каждой стороне и
    /// встаёт по центру зоны, оставляя зазор со всех четырёх сторон.
    ///
    /// Живёт в настройках, а не в рантайме закрепления: значение выбирается
    /// раз и должно переживать перезапуск (в отличие от самого списка
    /// закреплённых окон, который сознательно не сериализуется — см.
    /// доккомент [`crate::pinned_window`]). Старые `config.json` без поля
    /// получают `0` через `#[serde(default)]` на структуре — миграция схемы
    /// не нужна, ровно как у `outline_pinned_windows`.
    ///
    /// Потолок 35% повторно применяется в
    /// [`crate::pinned_window::shrink_in_zone`]: файл конфига правят руками.
    pub snap_shrink_pct: u8,
    /// Применять отступ снап-зоны не только к закреплённым окнам, но и ко
    /// всем обычным (запрос пользователя 2026-08-25, галочка в подменю
    /// трея). Дефолт `false`: двигать чужие окна, которые пользователь нам
    /// не поручал, — заметное вмешательство, включать его должен он сам.
    ///
    /// Работает только вместе с ненулевым [`Settings::snap_shrink_pct`]:
    /// галочка задаёт ОБЛАСТЬ действия отступа, а не сам отступ.
    pub snap_shrink_all_windows: bool,
    /// Потолок «веса» окна для митоза (docs/M9_WINDOW_MITOSIS_DESIGN.md),
    /// мегабайты приватной памяти процесса. Процесс тяжелее — митоз
    /// отклоняется до единого движения окна.
    ///
    /// Смысл порога — не производительность resticker, а честность обещания:
    /// митоз ЗАПУСКАЕТ ВТОРОЙ ЭКЗЕМПЛЯР приложения, то есть в худшем случае
    /// удваивает его потребление памяти. Для приложения на четыре гигабайта
    /// это не «окно разделилось», а внезапная просадка всей системы, и
    /// пользователь связал бы её с чем угодно, кроме своего клика.
    ///
    /// Дефолт 4096 МБ — число из запроса пользователя 2026-09-01. Старые
    /// `config.json` без поля получают его через `#[serde(default)]` на
    /// структуре, миграция схемы не нужна (как у `snap_shrink_pct`).
    pub mitosis_max_memory_mb: u32,
    /// Приложения, для которых митоз запрещён: они открывают ровно одно
    /// окно и на запуск второго экземпляра лишь фокусируют существующее.
    /// Имена файлов exe без пути, сравнение без учёта регистра
    /// (`discord.exe`, а не `C:\...\Discord.exe`): путь меняется от
    /// обновления к обновлению, имя — нет.
    ///
    /// Зачем список, если отказ и так приходит по таймауту: без него окно
    /// ужимается СРАЗУ и стоит ужатым все секунды ожидания второго окна,
    /// которого не будет, — со стороны это выглядит как «программа просто
    /// порезала окно и ничего не открыла» (живой репорт пользователя
    /// 2026-09-01 про Discord и Spotify). Приложение из списка отсекается
    /// до единого движения окна.
    ///
    /// Список ПОПОЛНЯЕТСЯ САМ: два подряд отказа `NoSecondWindow` по одному
    /// и тому же exe заносят его сюда навсегда. Два, а не один, — потому
    /// что единичный отказ бывает и у нормального приложения (холодный
    /// старт с диска не уложился в таймаут), и запрещать по одной осечке
    /// значило бы отбирать функцию у приложений, которые на самом деле
    /// работают.
    ///
    /// Дефолт — `discord.exe` и `spotify.exe`: их назвал пользователь по
    /// живому опыту. Убрать любое приложение из списка можно, отредактировав
    /// `config.json`.
    pub mitosis_single_instance_apps: Vec<String>,
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
            language: "en".to_string(),
            onboarding_shown: false,
            denylist: Vec::new(),
            pin_sound_volume: 100,
            outline_pinned_windows: false,
            snap_shrink_pct: 0,
            snap_shrink_all_windows: false,
            mitosis_max_memory_mb: 4096,
            mitosis_single_instance_apps: default_single_instance_apps(),
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
    /// Меню редактирования групп (запрос пользователя 2026-08-25, T5):
    /// глобальный хоткей, открывающий менеджер групп поверх всего.
    /// `None` — не назначен.
    ///
    /// Дефолт `Ctrl+Alt+G` — в один ряд с остальными хоткеями программы
    /// (`Ctrl+Alt+S/H/M/T/U/F/C`) и с цифрами открытия групп.
    ///
    /// Весь набор дефолтов снят с рабочей настройки пользователя
    /// (2026-09-17, «сделай текущие бинды по умолчанию»): он пользуется
    /// программой каждый день, и его раскладка хоткеев — проверенная
    /// временем, в отличие от выбранной за столом.
    ///
    /// Один день здесь простоял `Ctrl+Shift+G`; `Ctrl+Shift` — вторая пара,
    /// которую Windows умеет отдавать переключателю раскладки (значение `2`
    /// в `HKCU\Keyboard Layout\Toggle`), так что возврат к `Ctrl+Alt`
    /// заодно уводит дефолт от этого риска. Разбор таких комбинаций написан
    /// (`rst_win32::hotkey_check`), но НИ ОТКУДА НЕ ВЫЗЫВАЕТСЯ —
    /// предупреждения пользователь пока не увидит.
    ///
    /// Изначально здесь стоял `Alt+Shift+G`, и это была ошибка, найденная
    /// вживую 2026-08-26. `Alt+Shift` — стандартное сочетание Windows для
    /// ПЕРЕКЛЮЧЕНИЯ РАСКЛАДКИ клавиатуры: когда установлено больше одной
    /// раскладки, а ключ `HKCU\Keyboard Layout\Toggle` не задан (то есть у
    /// большинства пользователей), система забирает эту пару себе. Хоткей
    /// при этом регистрируется без ошибки и комбинацию мы держим — но
    /// нажатие до слоя хоткеев доходит через раз, в зависимости от того, в
    /// каком порядке нажаты модификаторы. Отлаживать такое почти невозможно:
    /// «иногда работает» выглядит как случайный баг программы.
    pub edit_groups_menu: Option<String>,
    /// Удалить группу, которая сейчас открыта (T5). `None` — не назначен.
    ///
    /// Дефолт `Ctrl+Alt+Shift+G` — «меню редактирования групп» плюс
    /// Ctrl+Alt: те же клавиши, что открывают меню, но с добавленными
    /// модификаторами, потому что действие деструктивное — случайно
    /// задеть его труднее, чем «открыть меню», а запомнить надо только
    /// одну пару (Alt+Shift+G и Ctrl+Alt+Shift+G).
    pub delete_open_group: Option<String>,
    /// Открыть группу по её номеру цифровым хоткеем (T5): индекс 0 отвечает
    /// группе 1, индекс 8 — группе 9 (`MAX_GROUP_NUMBER`). `None` в ячейке —
    /// хоткей не назначен; вектор короче девяти читается как «остальные
    /// номера не назначены» (см. [`Hotkeys::open_group`]).
    ///
    /// Одно поле на девять хоткеев, а не девять полей `open_group_1..9`:
    /// настройки, регистрация и разбор `WM_HOTKEY` перебирают слоты одним
    /// циклом, а дефолт для старых config.json — один вектор, а не девять
    /// прибавлений к `Default`. Цена — позиционность: номер зашит в индекс,
    /// поэтому удалять ячейку из середины нельзя (съедет нумерация всех
    /// следующих групп), только ставить `null`; хвост короче девяти —
    /// просто «не назначен».
    pub open_group_by_number: Vec<Option<String>>,
    /// Открепить ВСЕ закреплённые окна разом (запрос пользователя
    /// 2026-08-26). `None` — не назначен.
    ///
    /// Дефолт `Ctrl+Alt+U` (unpin) — в один ряд с остальными хоткеями
    /// программы и мимо пары `Alt+Shift`, которую Windows отдаёт
    /// переключателю раскладки.
    ///
    /// Зачем отдельный хоткей, когда открепить можно по одному: закреплений
    /// бывает несколько, а разобрать их поштучно можно только войдя в режим
    /// редактирования и ткнув в каждое. Когда закреплённое окно мешает прямо
    /// сейчас, это слишком долго.
    pub unpin_all: Option<String>,
    /// Закрепить текущую (последнюю открытую) группу поверх всех окон —
    /// ПЕРЕКЛЮЧАТЕЛЬ: повторное нажатие открепляет (запрос пользователя
    /// 2026-08-26). `None` — не назначен.
    ///
    /// Дефолт `Ctrl+Alt+Shift+T` — в один ряд с остальными хоткеями
    /// программы и мимо пары `Alt+Shift`, которую Windows отдаёт
    /// переключателю раскладки (см. `edit_groups_menu`). От
    /// `pin_focused_window` (`Ctrl+Alt+T`) отличается добавленным Shift: оба
    /// хоткея про закрепление, но один закрепляет окно, другой — всю группу,
    /// и две комбинации рядом обязаны отличаться, чтобы не спутать их.
    pub pin_open_group: Option<String>,
    /// Режим резки окон — «митоз» (запрос пользователя 2026-09-01;
    /// docs/M9_WINDOW_MITOSIS_DESIGN.md). Переключатель: одно нажатие
    /// включает режим, повторное выключает; после состоявшегося разреза
    /// режим гаснет сам. `None` — не назначен.
    ///
    /// Дефолт `Ctrl+Alt+F` — в один ряд с остальными хоткеями программы
    /// (`Ctrl+Alt+S/H/M/T/G/U`) и мимо пары `Alt+Shift`, которую Windows
    /// отдаёт переключателю раскладки (см. `edit_groups_menu`).
    pub window_mitosis: Option<String>,
    /// Отделить кусок чужого окна и положить его отдельным стикером
    /// (запрос пользователя 2026-09-10; [`StickerSource::WindowCrop`]).
    /// Нажатие включает выделение области: экран притухает, наведение
    /// подсвечивает окно целиком, протяжка внутри него задаёт кусок.
    /// Повторное нажатие или `Esc` выходит без выделения. `None` — не
    /// назначен.
    ///
    /// Дефолт `Ctrl+Alt+C` («crop») — в один ряд с остальными хоткеями
    /// программы (`Ctrl+Alt+S/H/M/T/G/U/F`) и мимо пары `Alt+Shift`, которую
    /// Windows отдаёт переключателю раскладки (см. `edit_groups_menu`).
    pub window_crop: Option<String>,
}

impl Hotkeys {
    /// Сколько слотов под цифровые хоткеи групп — ровно числу номеров
    /// группы (`MAX_GROUP_NUMBER`). Одна константа на оба места: если
    /// количество номеров когда-нибудь изменится, слоты хоткеев поедут
    /// следом, а не разойдутся с нумерацией.
    pub const GROUP_OPEN_SLOTS: usize = MAX_GROUP_NUMBER as usize;

    /// Комбинация режима редактирования по умолчанию.
    ///
    /// Константа, а не литерал в [`Default`]: этот хоткей единственный
    /// обязательный (остальные снимаются кнопкой «Очистить» в настройках),
    /// и та же строка нужна как запасной вариант там, где в конфиге стоит
    /// `null` или неразбираемое значение — иначе пользователь остался бы
    /// без входа в режим редактирования вовсе. Два литерала в разных
    /// крейтах разошлись бы при первой же смене дефолта.
    pub const DEFAULT_EDIT_MODE: &'static str = "Ctrl+Alt+S";

    /// Хоткей открытия группы с номером `n` (1..=9). `None` — номер вне
    /// диапазона, ячейка пуста или вектор короче `n`. Индексация вектора
    /// напрямую вынуждала бы вызывающего помнить про поправку `n - 1` —
    /// здесь она одна на все места, где хоткеи групп читаются.
    pub fn open_group(&self, n: usize) -> Option<&str> {
        if n == 0 || n > Self::GROUP_OPEN_SLOTS {
            return None;
        }
        self.open_group_by_number.get(n - 1)?.as_deref()
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            edit_mode: Some(Self::DEFAULT_EDIT_MODE.to_string()),
            toggle_all_stickers: Some("Ctrl+Alt+H".to_string()),
            mute_all: Some("Ctrl+Alt+M".to_string()),
            pin_focused_window: Some("Ctrl+Alt+T".to_string()),
            edit_groups_menu: Some("Ctrl+Alt+G".to_string()),
            delete_open_group: Some("Ctrl+Alt+Shift+G".to_string()),
            open_group_by_number: default_open_group_by_number(),
            unpin_all: Some("Ctrl+Alt+U".to_string()),
            pin_open_group: Some("Ctrl+Alt+Shift+T".to_string()),
            window_mitosis: Some("Ctrl+Alt+F".to_string()),
            window_crop: Some("Ctrl+Alt+C".to_string()),
        }
    }
}

/// Приложения, для которых митоз запрещён по умолчанию
/// ([`Settings::mitosis_single_instance_apps`]).
///
/// Ровно два и ровно те, что пользователь назвал по живому опыту
/// 2026-09-01. Добавлять сюда «очевидно одно-оконные» приложения по памяти
/// нельзя: ровно такое предположение про Проводник уже оказалось неверным
/// при замере (`docs/M9_MITOSIS_FIELD_NOTES.md` §3.2). Остальные приложения
/// список выучит сам.
fn default_single_instance_apps() -> Vec<String> {
    vec!["discord.exe".to_string(), "spotify.exe".to_string()]
}

/// Дефолтные хоткеи открытия групп 1..=9 — `Ctrl+Alt+<номер>`.
///
/// Отдельная функция, а не инлайн в `Default for Hotkeys`: та же логика
/// нужна здесь и в `Hotkeys::default()` — повторять цикл в двух местах
/// означало бы два места правки при смене шаблона.
fn default_open_group_by_number() -> Vec<Option<String>> {
    (1..=Hotkeys::GROUP_OPEN_SLOTS)
        .map(|n| Some(format!("Ctrl+Alt+{n}")))
        .collect()
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

    /// Новый стикер — живой кусок чужого окна (запрос пользователя
    /// 2026-09-10, [`StickerSource::WindowCrop`]).
    ///
    /// Отдельный конструктор по той же причине, что [`Sticker::new_pasted`]:
    /// источник собирается из двух частей (приметы окна + доли), и давать
    /// координатору собирать его вручную значило бы тащить туда `chrono` и
    /// `Uuid` ради одного места.
    pub fn new_window_crop(
        window: CropWindowKey,
        crop: CropRect,
        monitor_id: MonitorId,
        cx: f64,
        cy: f64,
        w: f64,
        h: f64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            source: StickerSource::WindowCrop {
                window,
                crop,
                // Новый кусок всегда развёрнут: человек только что провёл
                // рамку, и показать ему вместо содержимого иконку было бы
                // ответом не на то действие.
                minimized: false,
                // Обычное окно, а не «поверх всех»: поверх всего кусок
                // поднимает только сам человек булавкой на полосе.
                always_on_top: false,
            },
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
    /// Живой кусок чужого окна: прямоугольник внутри окна-источника,
    /// обновляющийся по мере его перерисовки (задача «оторвать кусок окна»,
    /// 2026-09-10). Не копия и не снимок — кадры приходит от
    /// Windows.Graphics.Capture ТОЛЬКО когда источник реально перерисовался
    /// (замер 2026-09-10: 3 с неподвижного окна — ноль кадров), поэтому
    /// обещание SPEC §13 «в полном покое программа не должна просыпаться
    /// вообще» не нарушается.
    ///
    /// Кликать по такому куску нельзя: это пиксели, а не окно. Пересылка
    /// ввода в источник проверена и отвергнута — Chromium/Electron не
    /// принимает `PostMessage`, а `SendInput` требует поднять окно и увести
    /// фокус (замер 2026-09-10).
    WindowCrop {
        /// Приметы окна-источника — те же, что у члена группы, и по той же
        /// причине: `HWND` не переживает перезапуск, а кусок обязан
        /// (`crate::group_match` опознаёт окно по exe + заголовку + классу).
        window: CropWindowKey,
        /// Какую часть окна показывать.
        crop: CropRect,
        /// Кусок свёрнут в иконку у ближайшего края экрана (решение
        /// пользователя 2026-09-11). Живёт в конфиге, а не в рантайме:
        /// свёрнутое состояние — это решение человека убрать кусок с глаз, и
        /// перезапуск программы не повод разворачивать всё обратно.
        ///
        /// Пока кусок свёрнут, захват его окна ОСТАНОВЛЕН: показывать нечего,
        /// а держать сессию Windows.Graphics.Capture ради иконки — это
        /// 2.4 МБ VRAM и пробуждения на каждую перерисовку чужого окна
        /// (замеры 2026-09-10) ни за что.
        ///
        /// `#[serde(default)]` — старые `config.json` без поля читаются как
        /// «развёрнут», миграция схемы не нужна.
        #[serde(default)]
        minimized: bool,
        /// Кусок держится поверх всех окон.
        ///
        /// По умолчанию `false` — и это принципиально: кусок должен быть
        /// обычным окном, которое уходит под другие окна, как любое другое
        /// (репорт пользователя 2026-09-12: «я не хочу чтобы вырезаные окна
        /// были алвейз он топ по умолчанию»). Сначала было наоборот, и куски
        /// накрывали собой в том числе панели самого resticker.
        ///
        /// Переключается булавкой на полосе куска и живёт в конфиге по той же
        /// причине, что [`Self::WindowCrop::minimized`]: это решение человека
        /// про конкретный кусок, и перезапуск программы не повод его отменять.
        ///
        /// `#[serde(default)]` — старые `config.json` без поля читаются как
        /// «не поверх всех», отдельная миграция схемы не нужна.
        #[serde(default)]
        always_on_top: bool,
    },
}

/// Приметы окна-источника живого куска. Повторяют [`GroupMember`] без
/// `place`: место окна к опознанию отношения не имеет. Переводится в
/// [`crate::group_match::MemberKey`] для сопоставления с живыми окнами —
/// механизм общий с группами, отдельного заводить не нужно.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropWindowKey {
    /// Полный путь к exe процесса-владельца — главная примета окна.
    pub exe_path: PathBuf,
    /// Заголовок на момент выделения куска. Сравнивается нестрого:
    /// заголовки меняются на лету, и опознание обязано это переживать.
    pub title: String,
    /// Класс окна: отличает главное окно приложения от вспомогательных,
    /// меняется куда реже заголовка.
    pub class: String,
}

/// Прямоугольник внутри окна-источника — в ДОЛЯХ его клиентской области,
/// не в пикселях (решение пользователя 2026-09-10).
///
/// Доли, потому что окно меняют в размере. В пикселях кусок остался бы
/// прежней полосой и при растянутом окне показывал бы не то место; в долях
/// он продолжает показывать ту же по смыслу область — растянули окно вдвое,
/// кусок показывает тот же список, просто крупнее.
///
/// `x`/`y` — левый верхний угол, `w`/`h` — размеры; все четыре в `0.0..=1.0`,
/// причём `x + w <= 1.0` и `y + h <= 1.0`. Конструктор это обеспечивает —
/// см. [`CropRect::new`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Default for CropRect {
    /// Всё окно целиком — нейтральный кусок, который ничего не отрезает.
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

impl CropRect {
    /// Наименьший кусок, который вообще имеет смысл показывать, в долях.
    /// Ниже этого протяжка считается случайным щелчком, а не выделением.
    pub const MIN_FRACTION: f64 = 0.005;

    /// Собрать по двум углам протяжки в долях окна. Порядок углов любой
    /// (протяжка влево-вверх нормализуется), выход всегда лежит в окне.
    /// `None` — протяжка вырожденная: по одной из осей меньше
    /// [`Self::MIN_FRACTION`], показывать нечего.
    pub fn from_corners(ax: f64, ay: f64, bx: f64, by: f64) -> Option<Self> {
        if ![ax, ay, bx, by].iter().all(|v| v.is_finite()) {
            return None;
        }
        let x0 = ax.min(bx).clamp(0.0, 1.0);
        let y0 = ay.min(by).clamp(0.0, 1.0);
        let x1 = ax.max(bx).clamp(0.0, 1.0);
        let y1 = ay.max(by).clamp(0.0, 1.0);
        let (w, h) = (x1 - x0, y1 - y0);
        if w < Self::MIN_FRACTION || h < Self::MIN_FRACTION {
            return None;
        }
        Some(Self { x: x0, y: y0, w, h })
    }

    /// Втащить прямоугольник в границы окна, не меняя его размера, пока
    /// это возможно. Нужно на загрузке конфига: файл мог быть отредактирован
    /// руками, а кусок за краем окна означал бы пустую или битую текстуру.
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        if ![self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
        {
            return d;
        }
        let w = self.w.clamp(Self::MIN_FRACTION, 1.0);
        let h = self.h.clamp(Self::MIN_FRACTION, 1.0);
        Self {
            x: self.x.clamp(0.0, 1.0 - w),
            y: self.y.clamp(0.0, 1.0 - h),
            w,
            h,
        }
    }

    /// Соотношение сторон куска при данном размере окна-источника в
    /// пикселях — исходная пропорция стикера в момент выделения.
    /// `None` — окно вырожденное (нулевая сторона).
    pub fn aspect(&self, window_w: u32, window_h: u32) -> Option<f64> {
        let (pw, ph) = (self.w * f64::from(window_w), self.h * f64::from(window_h));
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        Some(pw / ph)
    }

    /// Перевести в пиксели окна-источника — то, что уходит в `D3D11_BOX`
    /// при вырезании куска из кадра захвата. Результат всегда лежит внутри
    /// `window_w × window_h` и не вырожден по осям (минимум 1 пиксель),
    /// иначе `CopySubresourceRegion` получил бы пустой бокс.
    /// `None` — окно вырожденное, вырезать не из чего.
    pub fn to_pixels(&self, window_w: u32, window_h: u32) -> Option<(u32, u32, u32, u32)> {
        if window_w == 0 || window_h == 0 {
            return None;
        }
        let r = self.sanitized();
        let fw = f64::from(window_w);
        let fh = f64::from(window_h);
        let x = (r.x * fw).round().clamp(0.0, fw - 1.0) as u32;
        let y = (r.y * fh).round().clamp(0.0, fh - 1.0) as u32;
        let w = ((r.w * fw).round() as u32).clamp(1, window_w - x);
        let h = ((r.h * fh).round() as u32).clamp(1, window_h - y);
        Some((x, y, w, h))
    }
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
///
/// Список намеренно шире пяти самых частых расширений (репорт пользователя
/// 2026-08-22: «кучу типов видео я не могу добавить»): файл, который
/// FFmpeg прекрасно открывает, отсеивался ещё на выборе файла — по
/// расширению, не по содержимому. Открыть заведомо чужой файл дешевле, чем
/// не дать добавить свой: неподдерживаемое содержимое честно отвалится с
/// сообщением от декодера.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "webm", "mkv", "mov", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "mts",
    "3gp", "ogv",
];

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisibilityRule {
    pub mode: VisibilityMode,
    /// Используется при режимах never_overlap / overlap_allowlist.
    pub rules: Vec<OverlapRule>,
    /// Видим ли стикер, когда на мониторе нет окон.
    ///
    /// Отдельное поле нужно, потому что allow-list окон отвечает только на
    /// вопрос «над какими окнами оставить стикер», а чистый рабочий стол —
    /// отдельное состояние. Для старого конфига отсутствие поля означает
    /// прежнее поведение: на рабочем столе стикер виден.
    #[serde(default = "default_desktop_visibility")]
    pub desktop: bool,
}

fn default_desktop_visibility() -> bool {
    true
}

impl Default for VisibilityRule {
    fn default() -> Self {
        Self {
            mode: VisibilityMode::Always,
            rules: Vec::new(),
            desktop: true,
        }
    }
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
    /// Виден над всеми окнами, КРОМЕ перечисленных.
    ///
    /// Зеркало [`Self::OverlapAllowlist`] и единственный способ выразить
    /// «везде, кроме этого приложения» так, чтобы правило пережило появление
    /// новых окон. Через список разрешений то же самое выражается только
    /// материализацией снимка запущенных процессов — и любое окно, открытое
    /// позже (тем более после перезагрузки компьютера), в такой список уже не
    /// попадает: стикер молча уходит под него (репорт пользователя
    /// 2026-09-02).
    OverlapDenylist,
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
    /// Показывать таймлайн перемотки ВНЕ режима редактирования (запрос
    /// пользователя 2026-08-22). В самом режиме редактирования таймлайн
    /// показывается всегда — там он часть редактирования; здесь речь про
    /// обычную работу, где стикер обычно должен оставаться картинкой без
    /// элементов управления. Включённый флаг не делает полосу постоянной:
    /// она всплывает при наведении курсора на стикер и уходит, когда
    /// курсор ушёл, — как в нормальных плеерах.
    ///
    /// Настройка стикера, а не глобальная: у одного видео перемотка нужна
    /// постоянно, у другого — никогда.
    pub show_timeline: bool,
    /// Звук выключен (запрос пользователя 2026-09-06: «включить/выключить
    /// звук» — отдельное действие тулбара, кнопка-динамик). Отдельное поле,
    /// а не `volume = 0`: выключение обязано быть обратимым в один клик,
    /// с возвратом к прежнему уровню, а не к какому-нибудь «по умолчанию».
    /// Старые `config.json` без поля получают `false` через `#[serde(default)]`
    /// на структуре — отдельная миграция схемы не нужна.
    pub muted: bool,
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
            show_timeline: false,
            muted: false,
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
/// Группа окон (запрос пользователя 2026-08-25).
///
/// Набирается по хоткею в меню редактирования групп и открывается по
/// `Ctrl+Alt+<номер>`: окна встают на свои сохранённые места и поднимаются
/// над остальными. Чужие окна при этом не трогаются — группа всплывает
/// поверх, а не подменяет собой рабочий стол.
///
/// В отличие от закрепления ([`crate::pinned_window`], сознательно
/// рантайм-механизм) группа ЖИВЁТ В КОНФИГЕ и обязана пережить перезагрузку.
/// Поэтому член группы хранит не `HWND` — после перезапуска он другой, — а
/// приметы окна, по которым его потом опознают.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowGroup {
    pub id: Uuid,
    /// Номер для хоткея `Ctrl+Alt+<номер>`, 1..=9.
    ///
    /// Отдельно от порядка в `Vec`: удаление группы 2 не должно
    /// переназначать хоткеи всех следующих групп — открывалась группа 3 по
    /// тройке, пусть по тройке и открывается.
    pub number: u8,
    /// Имя. По умолчанию — номер строкой («1»), переименовывается в
    /// менеджере групп.
    pub name: String,
    /// Члены в порядке набора. Индекс + 1 — это номер слота в раскладке
    /// тайлинга: пользователь выбирает окна по очереди, и первое выбранное
    /// попадает в слот 1 (в раскладках «главное плюс стопка» — в главное).
    pub members: Vec<GroupMember>,
    /// Зазор между окнами этой группы, проценты 0..=35.
    ///
    /// СВОЙ, не общий с `Settings::snap_shrink_pct`: тот отвечает за окна,
    /// которые Windows положила в снап-зону, и менять его в трее не должно
    /// перекладывать давно собранную группу (прямое требование
    /// пользователя). Одно число в двух местах означало бы ровно это.
    pub gap_pct: u8,
}

impl Default for WindowGroup {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            number: 1,
            name: String::new(),
            members: Vec::new(),
            gap_pct: 0,
        }
    }
}

/// Одно окно внутри группы.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupMember {
    /// Путь к exe процесса-владельца — главная примета окна.
    pub exe_path: PathBuf,
    /// Заголовок на момент добавления. Заголовки меняются на лету (браузер,
    /// редактор пишут туда имя документа), поэтому сравнение при опознании
    /// нестрогое — см. `crate::group_match`.
    pub title: String,
    /// Класс окна: у одного процесса это отличает главное окно от
    /// вспомогательных, а меняется он куда реже заголовка.
    pub class: String,
    /// Где окно стоит. `None` — место ещё не запоминалось (группу только
    /// что собрали и раскладку не применяли).
    pub place: Option<GroupPlace>,
}

/// Место окна: монитор плюс прямоугольник в его логических пикселях.
///
/// DIP относительно СВОЕГО монитора, а не физические пиксели виртуального
/// десктопа, — та же система координат, что у [`Placement`] стикеров и по
/// той же причине: смена масштаба или перестановка мониторов не должна
/// увозить окно неизвестно куда.
///
/// Левый верхний угол, а не центр (в отличие от `Placement`): у окна нет
/// поворота, и «левый-верхний плюс размер» — ровно то, что просит Win32.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupPlace {
    pub monitor_id: MonitorId,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Наибольшее число окон в одной группе.
///
/// Ограничение не техническое, а смысловое: таблица раскладок тайлинга
/// заведена ровно на 2..=8 окон (`crate::group_layout`), и группа из
/// девяти окон осталась бы без раскладки.
pub const MAX_GROUP_MEMBERS: usize = 8;

/// Наименьшее число окон в группе: группа из одного окна — это просто окно.
pub const MIN_GROUP_MEMBERS: usize = 2;

/// Наибольший номер группы — по числу цифровых хоткеев `Ctrl+Alt+1..9`.
pub const MAX_GROUP_NUMBER: u8 = 9;

impl WindowGroup {
    /// Свободный номер для новой группы: наименьший незанятый из 1..=9.
    ///
    /// Наименьший свободный, а не «последний плюс один»: после удаления
    /// группы 2 следующая новая обязана занять именно двойку, иначе номера
    /// расползаются, а хоткеев всего девять.
    ///
    /// `None` — все девять заняты.
    pub fn next_number(existing: &[WindowGroup]) -> Option<u8> {
        (1..=MAX_GROUP_NUMBER).find(|n| !existing.iter().any(|g| g.number == *n))
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
    /// Группы окон (запрос пользователя 2026-08-25, см. [`WindowGroup`]).
    ///
    /// Обратная совместимость без бампа `CURRENT_SCHEMA_VERSION`: старые
    /// config.json без этого поля читаются через `#[serde(default)]` на
    /// структуре — тот же приём, что у `presets`.
    pub groups: Vec<WindowGroup>,
    /// Сеанс загрузки Windows, в котором этот конфиг писался
    /// ([`crate::boot_session`]).
    ///
    /// Только ради групп: они обязаны пережить перезапуск программы и обязаны
    /// НЕ пережить перезагрузку компьютера (запрос пользователя 2026-08-27), а
    /// отличить одно от другого можно только по сеансу.
    ///
    /// `None` — конфиг написан сборкой до этой возможности; при старте
    /// считается чужим сеансом.
    pub boot_stamp: Option<crate::boot_session::BootStamp>,
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
            groups: Vec::new(),
            boot_stamp: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pin_open_group_hotkey_is_ctrl_alt_shift_t() {
        assert_eq!(
            Hotkeys::default().pin_open_group.as_deref(),
            Some("Ctrl+Alt+Shift+T")
        );
    }

    #[test]
    fn config_without_pin_open_group_field_reads_with_default() {
        // Старый config.json без нового поля обязан читаться без миграции
        // схемы: недостающее поле достраивается дефолтом из
        // `Hotkeys::default()` через `#[serde(default)]` на структуре — тот
        // же прецедент, что `pin_focused_window` и `unpin_all`.
        let raw = r#"{"schema_version":1,"hotkeys":{"edit_mode":"Ctrl+Alt+S"}}"#;
        let cfg: Config = serde_json::from_str(raw).expect("старый конфиг обязан читаться");
        assert_eq!(cfg.hotkeys.edit_mode.as_deref(), Some("Ctrl+Alt+S"));
        assert_eq!(
            cfg.hotkeys.pin_open_group.as_deref(),
            Some("Ctrl+Alt+Shift+T"),
            "отсутствующий хоткей достраивается дефолтом"
        );
    }

    // --- Живой кусок окна (StickerSource::WindowCrop, 2026-09-10) ---

    #[test]
    fn crop_from_corners_normalizes_any_drag_direction() {
        let a = CropRect::from_corners(0.2, 0.3, 0.6, 0.8).unwrap();
        let b = CropRect::from_corners(0.6, 0.8, 0.2, 0.3).unwrap();
        assert_eq!(a, b, "протяжка влево-вверх даёт тот же прямоугольник");
        assert_eq!((a.x, a.y), (0.2, 0.3));
        assert!((a.w - 0.4).abs() < 1e-9 && (a.h - 0.5).abs() < 1e-9);
    }

    #[test]
    fn crop_from_corners_rejects_accidental_click() {
        assert!(
            CropRect::from_corners(0.5, 0.5, 0.5004, 0.9).is_none(),
            "полоса тоньше MIN_FRACTION — это щелчок, а не выделение"
        );
        assert!(CropRect::from_corners(0.5, 0.5, 0.5, 0.5).is_none());
        assert!(CropRect::from_corners(f64::NAN, 0.0, 1.0, 1.0).is_none());
    }

    #[test]
    fn crop_from_corners_clamps_drag_outside_window() {
        let r = CropRect::from_corners(-0.4, -0.2, 1.7, 1.3).unwrap();
        assert_eq!(
            (r.x, r.y, r.w, r.h),
            (0.0, 0.0, 1.0, 1.0),
            "протяжка за край окна упирается в его границы"
        );
    }

    #[test]
    fn crop_sanitized_pulls_hand_edited_config_back_inside() {
        let r = CropRect {
            x: 0.9,
            y: 0.95,
            w: 0.5,
            h: 0.4,
        }
        .sanitized();
        assert!(r.x + r.w <= 1.0 + 1e-9 && r.y + r.h <= 1.0 + 1e-9);
        assert!(
            (r.w - 0.5).abs() < 1e-9,
            "размер сохраняется, сдвигается угол"
        );
        assert_eq!(
            CropRect {
                x: f64::NAN,
                y: 0.0,
                w: 0.5,
                h: 0.5
            }
            .sanitized(),
            CropRect::default(),
            "битые числа из конфига дают всё окно, а не панику"
        );
    }

    #[test]
    fn crop_to_pixels_matches_window_size() {
        let r = CropRect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        };
        assert_eq!(r.to_pixels(1000, 800), Some((250, 400, 500, 200)));
        assert_eq!(
            r.to_pixels(0, 800),
            None,
            "вырожденное окно — вырезать не из чего"
        );
    }

    #[test]
    fn crop_to_pixels_never_leaves_the_frame() {
        // Правый нижний угол при мелком окне — самый опасный случай:
        // D3D11_BOX за пределами текстуры уронил бы копирование кадра.
        let r = CropRect {
            x: 0.99,
            y: 0.99,
            w: 0.01,
            h: 0.01,
        };
        let (x, y, w, h) = r.to_pixels(7, 5).unwrap();
        assert!(
            x + w <= 7 && y + h <= 5,
            "бокс {x},{y} {w}x{h} вышел за кадр 7x5"
        );
        assert!(w >= 1 && h >= 1, "бокс не может быть пустым");
    }

    #[test]
    fn crop_resize_keeps_the_same_part_of_the_window() {
        // Решение пользователя 2026-09-10: окно растянули — кусок
        // показывает ту же по смыслу область, просто крупнее.
        let r = CropRect {
            x: 0.0,
            y: 0.0,
            w: 0.3,
            h: 1.0,
        };
        assert_eq!(r.to_pixels(1000, 800).unwrap().2, 300);
        assert_eq!(r.to_pixels(1600, 800).unwrap().2, 480);
    }

    #[test]
    fn crop_aspect_follows_window_shape() {
        let r = CropRect {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        assert_eq!(r.aspect(1000, 500), Some(2.0));
        assert_eq!(r.aspect(0, 500), None);
    }

    #[test]
    fn window_crop_source_survives_config_roundtrip() {
        let src = StickerSource::WindowCrop {
            window: CropWindowKey {
                exe_path: std::path::PathBuf::from(r"C:\app\app.exe"),
                title: "Чат".to_string(),
                class: "Chrome_WidgetWin_1".to_string(),
            },
            crop: CropRect {
                x: 0.1,
                y: 0.2,
                w: 0.3,
                h: 0.4,
            },
            minimized: true,
            always_on_top: true,
        };
        let json = serde_json::to_string(&src).expect("сериализация");
        assert!(
            json.contains("window_crop"),
            "тег вида источника в конфиге: {json}"
        );
        let back: StickerSource = serde_json::from_str(&json).expect("разбор");
        assert_eq!(back, src, "кусок обязан пережить перезапуск");
        assert!(
            json.contains("minimized"),
            "свёрнутость пишется в конфиг: перезапуск не повод разворачивать всё обратно"
        );
        assert!(
            json.contains("always_on_top"),
            "булавка пишется в конфиг: включённая обязана пережить перезапуск: {json}"
        );
    }

    #[test]
    fn window_crop_reads_old_config_without_always_on_top() {
        // Куски, созданные до появления булавки, держались поверх всех окон
        // безусловно. Читать их как `always_on_top: true` было бы сохранением
        // поведения, от которого пользователь прямо отказался (2026-09-12), —
        // поэтому отсутствие поля означает обычное окно.
        let legacy = r#"{"kind":"window_crop","window":{"exe_path":"C:\\app\\app.exe","title":"Чат","class":"Cls"},"crop":{"x":0.0,"y":0.0,"w":1.0,"h":1.0},"minimized":true}"#;
        let src: StickerSource = serde_json::from_str(legacy).expect("старый конфиг читается");
        assert!(matches!(
            src,
            StickerSource::WindowCrop {
                minimized: true,
                always_on_top: false,
                ..
            }
        ));
    }

    #[test]
    fn window_crop_reads_old_config_without_minimized() {
        // Старые `config.json` писались до появления свёрнутости. Они обязаны
        // читаться как «развёрнут», а не отказом чтения: миграция схемы ради
        // одного булева поля — это риск потерять весь конфиг пользователя.
        let legacy = r#"{"kind":"window_crop","window":{"exe_path":"C:\\app\\app.exe","title":"Чат","class":"Cls"},"crop":{"x":0.0,"y":0.0,"w":1.0,"h":1.0}}"#;
        let src: StickerSource = serde_json::from_str(legacy).expect("старый конфиг читается");
        assert!(matches!(
            src,
            StickerSource::WindowCrop {
                minimized: false,
                ..
            }
        ));
    }
}
