//! Immediate-mode виджеты UI редактора (M2_UI_NOTES.md, раздел 8, пункт 1;
//! SPEC.md 3.6 «Тулбар стикера», 3.8 «Панель инструментов у курсора»).
//!
//! Модель работы: виджеты — удерживаемые (retained) состояния, которые (а)
//! отдают список примитивов [`Primitive`] к отрисовке и (б) отвечают на
//! точку хит-теста в DIP-координатах монитора. Преобразование примитивов в
//! спрайты — на вызывающем слое: `Fill` → [`crate::solid_sprite`], `Text` →
//! [`crate::text::rasterize`] + текстура (кэш по строке), `Icon` → текстура
//! из `assets/`. GPU здесь нет, всё покрыто юнит-тестами без устройства.
//!
//! Хит-тест использует ту же обратную аффинную математику, что и рамка
//! выделения ([`rst_core::hittest`]) — повёрнутые прямоугольники поддержаны
//! бесплатно, дублирования кода нет (M2_UI_NOTES §8, пункт 5).
//!
//! Клавиатурный фокус живёт внутри панели (не у Windows): поле ввода
//! перехватывает цифры, `Backspace` и `Esc` только пока сфокусировано;
//! `Ctrl`-комбинации до виджетов не доходят — их разбирает ядро
//! (M2_UI_NOTES §9 «Фокус клавиатуры»). Вставка `Ctrl+V` приходит методом
//! [`Widget::paste`] — чтение буфера обмена (rst-win32) делает вызывающий
//! слой, виджет фильтрует цифры сам.

use rst_core::hittest::to_local;
use rst_core::model::OverlapRule;

use crate::selection::Box2D;
use crate::text;

/// Идентификатор виджета (назначает вызывающий слой, константами).
pub type WidgetId = u32;

/// Точка в DIP-координатах монитора.
pub type Point = (f64, f64);

/// Точка внутри прямоугольника (с учётом его поворота), DIP.
///
/// Математика — [`rst_core::hittest::to_local`], как у хит-теста стикеров;
/// вырожденный (нулевой/отрицательный) прямоугольник не хитуется.
pub fn box_contains(r: &Box2D, pos: Point) -> bool {
    if !(r.w > 0.0 && r.h > 0.0) {
        return false;
    }
    let (lx, ly) = to_local(r.cx, r.cy, r.rotation, pos.0, pos.1);
    lx.abs() <= r.w / 2.0 && ly.abs() <= r.h / 2.0
}

/// Иконка кнопки (идентификация; текстуры из `assets/` — у вызывающего
/// слоя). Набор — по SPEC 3.6 (тулбар) и 3.8 (панель у курсора). `Hash` —
/// для кэша текстур по варианту иконки (`icon_rgba`, координатор).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// «Слои видимости» — панель выбора окон (SPEC 3.6, п. 3).
    Layers,
    /// «Глаз» — показать/скрыть стикер (SPEC 3.7).
    Eye,
    /// «Глаз закрытый» — стикер скрыт.
    EyeOff,
    /// Порядок «выше» (SPEC 3.6, п. 5).
    OrderUp,
    /// Порядок «ниже» (SPEC 3.6, п. 5).
    OrderDown,
    /// «Дублировать» (SPEC 3.6, п. 6).
    Duplicate,
    /// «Удалить» (SPEC 3.6, п. 7).
    Delete,
    /// «Загрузить файл» (SPEC 3.8).
    FileOpen,
    /// «Показать все стикеры» (SPEC 3.8).
    ShowAll,
    /// «Скрыть все стикеры» (SPEC 3.8).
    HideAll,
    /// «Сохранить пресет» (SPEC 3.8).
    PresetSave,
    /// «Загрузить пресет» (SPEC 3.8).
    PresetLoad,
    /// «Открыть настройки» (SPEC 3.8).
    Settings,
    /// «Выйти из режима редактирования» (SPEC 3.8).
    Exit,
    /// «Играть» — видео-стикер на паузе (M5b).
    Play,
    /// «Пауза» — видео-стикер играет (M5b).
    Pause,
    /// «Сбросить масштаб» — тулбар выделения, возвращает размер/поворот/
    /// отражения к натуральным (фидбэк пользователя 2026-08-09).
    ResetScale,
    /// «Замок» — индикатор interact-lock закреплённого окна (SPEC
    /// «закрепление окон», замок #2): окно видно, но не принимает ввод.
    Lock,
    /// «Замок открытый» — состояние «не заблокировано» в панели свойств
    /// закреплённого окна (переключатель пары Lock/LockOpen).
    LockOpen,
    /// «Плюс» — добавление правила соседства в панели свойств.
    Plus,
    /// «Кнопка-булавка» — бейдж «окно закреплено» в левом верхнем углу
    /// закреплённого окна ([`pin_indicator`]). Единственная растровая
    /// иконка: рисунок дал пользователь (2026-08-21), и перерисовывать его
    /// примитивами значило бы получить похожую, но другую булавку.
    Pinned,
}

impl Icon {
    /// Все варианты в порядке объявления — для предварительной генерации
    /// кэша иконок (текс-карта `HashMap<Icon, Texture>`, M2_WIRING_PLAN §3)
    /// и тестов генератора `icon_rgba`.
    pub const ALL: [Icon; 21] = [
        Icon::Layers,
        Icon::Eye,
        Icon::EyeOff,
        Icon::OrderUp,
        Icon::OrderDown,
        Icon::Duplicate,
        Icon::Delete,
        Icon::FileOpen,
        Icon::ShowAll,
        Icon::HideAll,
        Icon::PresetSave,
        Icon::PresetLoad,
        Icon::Settings,
        Icon::Exit,
        Icon::Play,
        Icon::Pause,
        Icon::ResetScale,
        Icon::Lock,
        Icon::LockOpen,
        Icon::Plus,
        Icon::Pinned,
    ];
}

/// Примитив отрисовки (геометрия DIP). В спрайты превращает вызывающий слой.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// Одноцветный прямоугольник (через solid-текстуру цвета `color`).
    Fill {
        rect: Box2D,
        color: [u8; 3],
        opacity: f64,
    },
    /// Иконка: текстурированный квадрат из `assets/`.
    Icon {
        rect: Box2D,
        icon: Icon,
        opacity: f64,
    },
    /// Произвольный RGBA-растр (иконка окна в панели выбора, M4 §6):
    /// `rgba` — straight-alpha пиксели `width`×`height` (первый аплоад
    /// текстуры), `key` — стабильный идентификатор для кэша текстур
    /// вызывающего слоя: примитивы с одинаковым `key` делят одну
    /// GPU-текстуру (окна одного exe), `rgba` при кэш-попадании не
    /// используется. В отличие от [`Icon`], данные произвольные — не из
    /// встроенных ассетов.
    Rgba {
        rect: Box2D,
        key: u64,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        opacity: f64,
    },
    /// Строка текста: растрируется [`crate::text::rasterize`] в текстуру;
    /// `rect` — область назначения (её размер совпадает с [`text::text_size`]).
    Text {
        rect: Box2D,
        text: String,
        color: [u8; 3],
        opacity: f64,
    },
}

impl Primitive {
    /// Сдвинуть примитив на `(dx, dy)` DIP.
    ///
    /// Нужен слою, который догоняет уже отрисованную панель до её актуальной
    /// позиции, не пересобирая виджеты (панель инструментов закреплённого
    /// окна едет вместе с окном каждый кадр — `overlay_manager`).
    pub fn translate(&mut self, dx: f64, dy: f64) {
        let rect = match self {
            Primitive::Fill { rect, .. }
            | Primitive::Icon { rect, .. }
            | Primitive::Rgba { rect, .. }
            | Primitive::Text { rect, .. } => rect,
        };
        rect.cx += dx;
        rect.cy += dy;
    }
}

/// Палитра и метрики UI (DIP). Значения подобраны под затемнение 50%
/// (SPEC 3.1): панель чуть светлее фона, акцент — для заполнения ползунка
/// и фокусной рамки.
pub mod theme {
    /// Фон панели/тулбара.
    pub const PANEL_BG: [u8; 3] = [0x2b, 0x2b, 0x30];
    /// Непрозрачность фона панели.
    pub const PANEL_BG_OPACITY: f64 = 0.92;
    /// Рамка панели.
    pub const PANEL_BORDER: [u8; 3] = [0x55, 0x55, 0x5e];
    /// Фон кнопки.
    pub const BUTTON_BG: [u8; 3] = [0x3a, 0x3a, 0x41];
    /// Фон кнопки под курсором.
    pub const BUTTON_BG_HOVER: [u8; 3] = [0x4a, 0x4a, 0x54];
    /// Фон кнопки зажатой.
    pub const BUTTON_BG_ARMED: [u8; 3] = [0x2a, 0x2a, 0x30];
    /// Дорожка ползунка.
    pub const SLIDER_TRACK: [u8; 3] = [0x55, 0x55, 0x5e];
    /// Заполненная часть и ручка ползунка (акцент).
    pub const SLIDER_FILL: [u8; 3] = [0x4f, 0x9c, 0xff];
    /// Фон поля ввода.
    pub const FIELD_BG: [u8; 3] = [0x20, 0x20, 0x24];
    /// Рамка поля ввода.
    pub const FIELD_BORDER: [u8; 3] = [0x55, 0x55, 0x5e];
    /// Рамка поля ввода в фокусе (акцент).
    pub const FIELD_BORDER_FOCUS: [u8; 3] = [0x4f, 0x9c, 0xff];
    /// Каретка.
    pub const CARET: [u8; 3] = [0xff, 0xff, 0xff];
    /// Текст.
    pub const TEXT: [u8; 3] = [0xf0, 0xf0, 0xf0];

    /// Сторона квадратной кнопки тулбара, DIP.
    pub const BUTTON_SIZE: f64 = 28.0;
    /// Внутренний отступ иконки в кнопке, DIP.
    pub const BUTTON_PAD: f64 = 4.0;
    /// Высота ползунка, DIP.
    pub const SLIDER_HEIGHT: f64 = 20.0;
    /// Сторона ручки ползунка, DIP.
    pub const SLIDER_KNOB: f64 = 12.0;
    /// Толщина дорожки ползунка, DIP.
    pub const SLIDER_TRACK_H: f64 = 2.0;
    /// Высота числового поля, DIP.
    pub const FIELD_HEIGHT: f64 = 22.0;
    /// Горизонтальный отступ текста в поле, DIP.
    pub const FIELD_PAD: f64 = 4.0;
    /// Сторона чекбокса панели выбора окон, DIP.
    pub const CHECKBOX_SIZE: f64 = 16.0;
    /// Ширина вертикальной полосы скролла, DIP (панели «Слои видимости»/
    /// список закрепления окон — тонкая, не съедает заметную часть и так
    /// узкого правого отступа панели).
    pub const SCROLLBAR_WIDTH: f64 = 3.0;
    /// Минимальная высота ручки скролла, DIP — при очень длинных списках
    /// `thumb_fraction` может выродиться в единицы DIP, ручка должна
    /// оставаться видимой и кликабельной на глаз.
    pub const SCROLLBAR_MIN_THUMB_H: f64 = 16.0;
    /// Сторона квадратного бейджа индикатора interact-lock закреплённого
    /// окна ([`lock_indicator`]), DIP.
    pub const LOCK_INDICATOR_SIZE: f64 = 16.0;
    /// Отступ бейджа индикатора от углов окна, DIP.
    pub const LOCK_INDICATOR_MARGIN: f64 = 4.0;
    /// Непрозрачность бейджа и иконки индикатора — полупрозрачный, чтобы
    /// не заслонять содержимое окна.
    pub const LOCK_INDICATOR_OPACITY: f64 = 0.85;
    /// Фон бейджа индикатора (тёмный — читается и на светлых окнах).
    pub const LOCK_INDICATOR_BG: [u8; 3] = [0x1a, 0x1a, 0x1e];

    /// Палитра окна настроек (Half-Life 2 / Source VGUI), перенесённая
    /// один-в-один из `crates/resticker/ui/styles.css` — те же значения
    /// переменных `--bg`, `--border-*`, `--btn-*`, `--check-*`, `--accent`.
    /// Применяется к панелям со стилем [`super::WidgetStyle::Settings`]
    /// (сейчас — панель инструментов закреплённого окна), чтобы оверлей и
    /// окно настроек читались как один продукт.
    ///
    /// Одно сознательное расхождение: в вебвью у панели есть
    /// `backdrop-filter: blur(4px)`, поэтому там фон живёт при
    /// непрозрачности 0.6. Оверлей размывать нечем (композитор рисует
    /// поверх произвольного окна), и при 0.6 без размытия панель читалась бы
    /// как грязное пятно — берём [`SETTINGS_BG_OPACITY`] повыше.
    pub mod settings {
        /// `--bg`: rgba(118, 118, 118, 0.6).
        pub const BG: [u8; 3] = [0x76, 0x76, 0x76];
        /// `--border-light` — светлая грань «поднятой» рамки (сверху слева).
        pub const BORDER_LIGHT: [u8; 3] = [0xbb, 0xba, 0xba];
        /// `--border-dark` — тёмная грань (снизу справа).
        pub const BORDER_DARK: [u8; 3] = [0x43, 0x43, 0x43];
        /// `--btn-bg` / `--btn-light` / `--btn-dark`.
        pub const BTN_BG: [u8; 3] = [0x7b, 0x7b, 0x7b];
        pub const BTN_BG_HOVER: [u8; 3] = [0x8c, 0x8c, 0x8c];
        pub const BTN_LIGHT: [u8; 3] = [0xbb, 0xbb, 0xbb];
        pub const BTN_DARK: [u8; 3] = [0x45, 0x45, 0x45];
        /// `--check-bg` / `--check-light` / `--check-dark`.
        pub const CHECK_BG: [u8; 3] = [0x55, 0x55, 0x55];
        pub const CHECK_BG_HOVER: [u8; 3] = [0x63, 0x63, 0x63];
        pub const CHECK_LIGHT: [u8; 3] = [0xba, 0xba, 0xba];
        pub const CHECK_DARK: [u8; 3] = [0x42, 0x43, 0x43];
        /// `--text`: белый, а не мягкий [`super::TEXT`] оверлея.
        pub const TEXT: [u8; 3] = [0xff, 0xff, 0xff];
        /// Толщина грани, DIP (в CSS — 1 px).
        pub const BEVEL: f64 = 1.0;
        /// Непрозрачность фона панели — см. доккомент модуля.
        pub const BG_OPACITY: f64 = 0.82;
        /// Непрозрачность иконки выключенного переключателя (в CSS
        /// неактивная вкладка живёт на `opacity: 0.5`).
        pub const ICON_OFF_OPACITY: f64 = 0.5;
    }
}

/// Оформление виджета: тёмная схема оверлея (по умолчанию) либо стилистика
/// окна настроек (Source VGUI, [`theme::settings`]).
///
/// Стиль выбирает СБОРЩИК панели, а не сам виджет: панель инструментов
/// закреплённого окна строится в стиле настроек целиком
/// ([`build_pinned_lock_panel`]), тулбар и остальные панели остаются
/// тёмными.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WidgetStyle {
    /// Тёмная схема оверлея ([`theme::PANEL_BG`] и соседи).
    #[default]
    Overlay,
    /// Стилистика окна настроек ([`theme::settings`]).
    Settings,
}

/// Грани «объёмной» рамки VGUI: светлая сверху и слева, тёмная снизу и
/// справа (`raised == true`) либо наоборот — «вдавленное» поле
/// (`raised == false`, так в настройках нарисованы поля ввода и чекбоксы).
/// Ровно те же четыре односторонние границы, что задаёт CSS настроек.
fn settings_bevel(out: &mut Vec<Primitive>, rect: Box2D, raised: bool, opacity: f64) {
    let (light, dark) = (theme::settings::BORDER_LIGHT, theme::settings::BORDER_DARK);
    let (top_left, bottom_right) = if raised { (light, dark) } else { (dark, light) };
    let t = theme::settings::BEVEL;
    let half_w = rect.w / 2.0;
    let half_h = rect.h / 2.0;
    let mut edge = |cx: f64, cy: f64, w: f64, h: f64, color: [u8; 3]| {
        out.push(Primitive::Fill {
            rect: Box2D {
                cx,
                cy,
                w: w.max(0.0),
                h: h.max(0.0),
                rotation: rect.rotation,
            },
            color,
            opacity,
        });
    };
    edge(rect.cx, rect.cy - half_h + t / 2.0, rect.w, t, top_left);
    edge(rect.cx - half_w + t / 2.0, rect.cy, t, rect.h, top_left);
    edge(rect.cx, rect.cy + half_h - t / 2.0, rect.w, t, bottom_right);
    edge(rect.cx + half_w - t / 2.0, rect.cy, t, rect.h, bottom_right);
}

/// Примитивы индикатора interact-lock закреплённого окна (SPEC «закрепление
/// окон», замок #2): маленький тёмный бейдж с иконкой [`Icon::Lock`] в левом
/// верхнем углу прямоугольника окна. Окно видно целиком и выглядит обычно —
/// оно лишь не принимает ввод, поэтому лечение лёгкое, угловое: сплошное
/// затемнение или шахматка ([`crate::selection::checkerboard_tile`],
/// `HIDDEN_STICKER_CHECKERBOARD_OPACITY`) неправильно намекали бы, что окно
/// скрыто.
///
/// Возвращает примитивы в порядке «нижний — первым»: фон-бейдж, затем
/// иконка. Бейдж приводится к стороне окна — очень маленькие окна не дают
/// вырожденной геометрии.
pub fn lock_indicator(window_rect: Box2D) -> Vec<Primitive> {
    indicator_badge(window_rect, 1, Icon::Lock)
}

/// Примитивы бейджа «окно закреплено» (запрос пользователя 2026-08-21:
/// «небольшую иконку слева сверху на окно, что оно закреплено») — та же
/// угловая раскладка, что у [`lock_indicator`], слот 0 (крайний левый).
/// Рисуется для КАЖДОГО закреплённого окна, независимо от замков: пин —
/// состояние, которое иначе видно только по поведению окна.
pub fn pin_indicator(window_rect: Box2D) -> Vec<Primitive> {
    indicator_badge(window_rect, 0, Icon::Pinned)
}

/// Бейдж угловых индикаторов закреплённого окна: квадрат стороной
/// [`theme::LOCK_INDICATOR_SIZE`] в левом верхнем углу, `slot` — позиция в
/// ряду слева направо (0 — пин, 1 — замок). Бейдж приводится к стороне окна:
/// очень маленькие окна не дают вырожденной геометрии.
fn indicator_badge(window_rect: Box2D, slot: usize, icon: Icon) -> Vec<Primitive> {
    let size = theme::LOCK_INDICATOR_SIZE
        .min(window_rect.w)
        .min(window_rect.h)
        .max(0.0);
    let step = size + theme::LOCK_INDICATOR_MARGIN;
    let badge = Box2D {
        cx: window_rect.cx - window_rect.w / 2.0
            + theme::LOCK_INDICATOR_MARGIN
            + size / 2.0
            + slot as f64 * step,
        cy: window_rect.cy - window_rect.h / 2.0 + theme::LOCK_INDICATOR_MARGIN + size / 2.0,
        w: size,
        h: size,
        rotation: 0.0,
    };
    vec![
        Primitive::Fill {
            rect: badge,
            color: theme::LOCK_INDICATOR_BG,
            opacity: theme::LOCK_INDICATOR_OPACITY,
        },
        Primitive::Icon {
            rect: badge,
            icon,
            opacity: theme::LOCK_INDICATOR_OPACITY,
        },
    ]
}

/// Событие указателя в DIP-координатах монитора (перевод из `WM_MOUSE*` —
/// у вызывающего слоя).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEvent {
    /// Кнопка нажата.
    Down { pos: Point },
    /// Указатель перемещён (в т.ч. во время перетаскивания).
    Move { pos: Point },
    /// Кнопка отпущена.
    Up { pos: Point },
}

/// Клавиши, понятные виджетам (перевод из `WM_KEYDOWN` — у вызывающего
/// слоя; `Ctrl`-комбинации сюда не приходят — они хоткеи ядра, M2_UI_NOTES §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Цифра 0–9 (верхний ряд клавиатуры).
    Digit(u8),
    /// Печатный символ (перевод из `WM_CHAR` — у вызывающего слоя; нужен
    /// [`TextField`] для произвольных строк, `Digit` остаётся числовым
    /// полям).
    Char(char),
    Backspace,
    Enter,
    Escape,
    ArrowLeft,
    ArrowRight,
}

/// Результат обработки события панелью/виджетом.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventResult {
    /// Событие поглощено UI и не должно уходить в обработку сцены
    /// (клик по панели не снимает выделение со стикера).
    pub consumed: bool,
    /// Визуальное состояние изменилось — требуется перерисовка (ADR-006).
    pub redraw: bool,
}

/// Виджет immediate-mode UI: отдаёт примитивы и отвечает на хит-тест
/// (M2_UI_NOTES §8, пункт 1). Геометрия — абсолютная, в DIP монитора;
/// перемещение — [`Widget::set_bounds`] (панель/тулбар ездит за выделением).
pub trait Widget {
    /// Идентификатор, назначенный вызывающим слоем.
    fn id(&self) -> WidgetId;
    /// Границы виджета в DIP.
    fn bounds(&self) -> Box2D;
    /// Новые границы (перемещение панели, релайаут тулбара).
    fn set_bounds(&mut self, bounds: Box2D);
    /// Примитивы отрисовки в порядке «нижний — первым».
    fn draw(&self, out: &mut Vec<Primitive>);
    /// Точка внутри виджета (с учётом поворота границ).
    fn hit_test(&self, pos: Point) -> bool {
        box_contains(&self.bounds(), pos)
    }
    /// Хочет ли виджет клавиатурный фокус при клике (поле ввода — да).
    fn wants_focus(&self) -> bool {
        false
    }
    /// Удерживает ли виджет клавиатурный фокус прямо сейчас. Поле ввода
    /// отпускает фокус по `Enter`/`Esc` — панель тогда снимает роутинг.
    fn has_focus(&self) -> bool {
        false
    }
    /// Доступ к конкретному типу (опрос состояния ядром: `take_click` и т.п.).
    fn as_any(&self) -> &dyn std::any::Any;
    /// Доступ к конкретному типу, изменяемый.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    /// Курсор вошёл/вышел (hover); по умолчанию игнорируется.
    /// Возвращает `true`, если нужна перерисовка.
    fn set_hovered(&mut self, _hovered: bool) -> bool {
        false
    }
    /// Клавиатурный фокус потерян (клик вне виджета); по умолчанию ничего.
    fn on_blur(&mut self) {}
    /// Событие указателя (роутится панелью; при перетаскивании — виджету
    /// с захватом). Возвращает `true`, если нужна перерисовка.
    fn pointer_event(&mut self, _ev: PointerEvent) -> bool {
        false
    }
    /// Клавиша (роутится панелью сфокусированному виджету).
    fn key_event(&mut self, _key: Key) -> bool {
        false
    }
    /// Вставка текста из буфера (`Ctrl+V`); виджет фильтрует сам.
    fn paste(&mut self, _text: &str) -> bool {
        false
    }
}

/// Содержимое кнопки.
#[derive(Debug, Clone, PartialEq)]
pub enum ButtonContent {
    /// Иконка (SPEC 3.6: все кнопки тулбара).
    Icon(Icon),
    /// Текстовая подпись (шрифт — [`crate::text`]; набор глифов ограничен).
    Label(String),
}

/// Кнопка: клик = нажатие внутри + отпускание внутри. Само действие
/// выполняет ядро; виджет лишь фиксирует факт клика ([`Button::take_click`]).
pub struct Button {
    id: WidgetId,
    bounds: Box2D,
    content: ButtonContent,
    /// Оформление (см. [`WidgetStyle`]).
    style: WidgetStyle,
    hovered: bool,
    /// Нажата (указатель зажат внутри), клик ещё не свершился.
    armed: bool,
    clicked: bool,
}

impl Button {
    /// Та же кнопка в заданном оформлении (см. [`WidgetStyle`]).
    pub fn with_style(mut self, style: WidgetStyle) -> Self {
        self.style = style;
        self
    }

    /// Кнопка с содержимым в прямоугольнике `bounds` (DIP).
    pub fn new(id: WidgetId, bounds: Box2D, content: ButtonContent) -> Self {
        Self {
            id,
            bounds,
            content,
            style: WidgetStyle::default(),
            hovered: false,
            armed: false,
            clicked: false,
        }
    }

    /// Кнопка-иконка стандартного размера тулбара с центром в `(cx, cy)`.
    pub fn icon(id: WidgetId, cx: f64, cy: f64, icon: Icon) -> Self {
        let s = theme::BUTTON_SIZE;
        Self::new(
            id,
            Box2D {
                cx,
                cy,
                w: s,
                h: s,
                rotation: 0.0,
            },
            ButtonContent::Icon(icon),
        )
    }

    /// Был ли клик с прошлого опроса (флаг сбрасывается).
    pub fn take_click(&mut self) -> bool {
        std::mem::take(&mut self.clicked)
    }
}

impl Widget for Button {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let settings = self.style == WidgetStyle::Settings;
        let bg = match (settings, self.armed, self.hovered) {
            (true, _, true) => theme::settings::BTN_BG_HOVER,
            (true, _, _) => theme::settings::BTN_BG,
            (false, true, _) => theme::BUTTON_BG_ARMED,
            (false, _, true) => theme::BUTTON_BG_HOVER,
            (false, _, _) => theme::BUTTON_BG,
        };
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: bg,
            opacity: 1.0,
        });
        if settings {
            // Нажатая кнопка в VGUI «проваливается» — грани меняются местами
            // (`.button:active` в styles.css).
            settings_bevel(out, self.bounds, !self.armed, 1.0);
        }
        let pad = theme::BUTTON_PAD;
        let content_rect = Box2D {
            w: (self.bounds.w - 2.0 * pad).max(0.0),
            h: (self.bounds.h - 2.0 * pad).max(0.0),
            ..self.bounds
        };
        match &self.content {
            ButtonContent::Icon(icon) => {
                out.push(Primitive::Icon {
                    rect: content_rect,
                    icon: *icon,
                    opacity: 1.0,
                });
            }
            ButtonContent::Label(label) if !label.is_empty() => {
                // Текстура текста растрируется в натуральном размере
                // (`text::text_size`) — растянуть её на весь `content_rect`
                // (обычно шире надписи, особенно у строк списков вроде
                // `preset_picker`/`window_pick_list`) значило бы смазать
                // глиф по горизонтали: конвейер спрайтов (`solid_sprite`)
                // маппит текстуру на `rect` 1:1, без сохранения пропорций.
                // Центрируем натуральный размер внутри содержимого кнопки —
                // тот же приём, что `Label`/`TextField`/`RowLabel` уже
                // применяют для нерастянутого текста.
                let (tw, th) = text::text_size(label);
                out.push(Primitive::Text {
                    rect: Box2D {
                        cx: content_rect.cx,
                        cy: content_rect.cy,
                        w: tw.min(content_rect.w),
                        h: th.min(content_rect.h),
                        rotation: content_rect.rotation,
                    },
                    text: label.clone(),
                    color: if settings {
                        theme::settings::TEXT
                    } else {
                        theme::TEXT
                    },
                    opacity: 1.0,
                });
            }
            // Пустая подпись — фон-кнопка без своего текста (`window_pick_list`:
            // строка = эта кнопка под hover/hit-test + отдельный неинтерактивный
            // виджет с иконкой и текстом поверх неё). Пустой `Primitive::Text`
            // не несёт содержимого — не эмитим его вовсе.
            ButtonContent::Label(_) => {}
        }
    }

    fn set_hovered(&mut self, hovered: bool) -> bool {
        std::mem::replace(&mut self.hovered, hovered) != hovered
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        match ev {
            PointerEvent::Down { pos } => {
                if self.hit_test(pos) {
                    self.armed = true;
                    return true;
                }
                false
            }
            PointerEvent::Up { pos } => {
                if !self.armed {
                    return false;
                }
                self.armed = false;
                if self.hit_test(pos) {
                    self.clicked = true;
                }
                true
            }
            PointerEvent::Move { .. } => false,
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Ползунок значения (прозрачность): горизонтальная дорожка с ручкой.
/// Диапазон настраивается (SPEC 3.6 — 1–100; значение целое).
pub struct Slider {
    id: WidgetId,
    bounds: Box2D,
    min: u32,
    max: u32,
    value: u32,
    /// Ручка перетаскивается указателем (захват удерживается панелью).
    dragging: bool,
    changed: bool,
}

impl Slider {
    /// Ползунок диапазона `min..=max` с текущим значением `value`
    /// (приводится к диапазону). Паника при `min >= max`.
    pub fn new(id: WidgetId, bounds: Box2D, min: u32, max: u32, value: u32) -> Self {
        assert!(min < max, "пустой диапазон ползунка");
        Self {
            id,
            bounds,
            min,
            max,
            value: value.clamp(min, max),
            dragging: false,
            changed: false,
        }
    }

    /// Ползунок прозрачности 0–100 с центром в `(cx, cy)` и шириной `w`.
    pub fn opacity(id: WidgetId, cx: f64, cy: f64, w: f64) -> Self {
        Self::new(
            id,
            Box2D {
                cx,
                cy,
                w,
                h: theme::SLIDER_HEIGHT,
                rotation: 0.0,
            },
            0,
            100,
            100,
        )
    }

    /// Текущее значение.
    pub fn value(&self) -> u32 {
        self.value
    }

    /// Установить значение извне (синхронизация с числовым полем).
    pub fn set_value(&mut self, value: u32) {
        self.value = value.clamp(self.min, self.max);
    }

    /// Новое значение с прошлого опроса (флаг сбрасывается).
    pub fn take_changed(&mut self) -> Option<u32> {
        if self.changed {
            self.changed = false;
            Some(self.value)
        } else {
            None
        }
    }

    /// Диапазон дорожки по X, доступный центру ручки (ручка не вылезает
    /// за края виджета).
    fn track_range(&self) -> (f64, f64) {
        let half = theme::SLIDER_KNOB / 2.0;
        (
            self.bounds.cx - self.bounds.w / 2.0 + half,
            self.bounds.cx + self.bounds.w / 2.0 - half,
        )
    }

    /// Позиция X центра ручки для значения `value`.
    fn value_to_x(&self, value: u32) -> f64 {
        let (x0, x1) = self.track_range();
        let t = f64::from(value - self.min) / f64::from(self.max - self.min);
        x0 + t * (x1 - x0)
    }

    /// Значение по позиции X указателя: округление к ближайшему, отсечение
    /// к диапазону (увод указателя за край не ломает перетаскивание).
    fn x_to_value(&self, x: f64) -> u32 {
        let (x0, x1) = self.track_range();
        if x1 <= x0 {
            return self.min;
        }
        let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
        let v = (t * f64::from(self.max - self.min)).round() as i64 + i64::from(self.min);
        v.clamp(i64::from(self.min), i64::from(self.max)) as u32
    }

    /// Установить значение по X; флаг изменения — только при реальной смене.
    fn set_from_x(&mut self, x: f64) -> bool {
        let v = self.x_to_value(x);
        if v == self.value {
            return false;
        }
        self.value = v;
        self.changed = true;
        true
    }
}

impl Widget for Slider {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let (x0, x1) = self.track_range();
        let cy = self.bounds.cy;
        // Дорожка на всю ширину хода ручки.
        out.push(Primitive::Fill {
            rect: Box2D {
                cx: (x0 + x1) / 2.0,
                cy,
                w: x1 - x0,
                h: theme::SLIDER_TRACK_H,
                rotation: 0.0,
            },
            color: theme::SLIDER_TRACK,
            opacity: 1.0,
        });
        // Заполненная часть слева от ручки.
        let kx = self.value_to_x(self.value);
        if kx > x0 {
            out.push(Primitive::Fill {
                rect: Box2D {
                    cx: (x0 + kx) / 2.0,
                    cy,
                    w: kx - x0,
                    h: theme::SLIDER_TRACK_H,
                    rotation: 0.0,
                },
                color: theme::SLIDER_FILL,
                opacity: 1.0,
            });
        }
        // Ручка.
        out.push(Primitive::Fill {
            rect: Box2D {
                cx: kx,
                cy,
                w: theme::SLIDER_KNOB,
                h: theme::SLIDER_KNOB,
                rotation: 0.0,
            },
            color: theme::SLIDER_FILL,
            opacity: 1.0,
        });
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        match ev {
            // Клик в любую точку виджета ставит значение и начинает drag.
            PointerEvent::Down { pos } => {
                self.dragging = true;
                self.set_from_x(pos.0)
            }
            PointerEvent::Move { pos } => self.dragging && self.set_from_x(pos.0),
            PointerEvent::Up { .. } => std::mem::replace(&mut self.dragging, false),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Вертикальный индикатор прокрутки списка (панель «Слои видимости»/список
/// закрепления окон, M4/M6) — живой репорт пользователя: длинный список
/// окон обрезался без единого визуального намёка, что его можно листать
/// (сам скролл к тому же не был подключён к колесу мыши, см. `InputEvent::
/// MouseWheel`). Только отрисовка — колесо мыши двигает `scroll` на
/// вызывающем слое, виджет лишь визуализирует текущее положение; не
/// перетаскиваемый (ручка мыши не нужна списку такого размера, дорожка/
/// ручка достаточно, чтобы было видно «тут можно листать»).
pub struct ScrollBar {
    id: WidgetId,
    bounds: Box2D,
    /// Доля видимой части списка от общего числа строк, `(0.0, 1.0]`.
    thumb_fraction: f64,
    /// Положение верха ручки как доля высоты дорожки, `[0.0, 1.0 -
    /// thumb_fraction]`.
    thumb_offset: f64,
}

impl ScrollBar {
    /// `visible_rows`/`total_rows` — то же, что даёт билдер панели
    /// (`PickerPanel::total_rows`/аналог); `scroll` — текущая позиция (в
    /// строках, как у панели). `total_rows <= visible_rows` даёт ручку во
    /// всю дорожку (скроллить нечего) — вызывающий слой обычно вообще не
    /// добавляет виджет в этом случае, но `ScrollBar` не паникует.
    pub fn new(
        id: WidgetId,
        bounds: Box2D,
        visible_rows: usize,
        total_rows: usize,
        scroll: usize,
    ) -> Self {
        let total_rows = total_rows.max(1);
        let thumb_fraction = (visible_rows as f64 / total_rows as f64).min(1.0);
        let max_scroll = total_rows.saturating_sub(1).max(1);
        let thumb_offset = if total_rows <= visible_rows {
            0.0
        } else {
            (scroll.min(max_scroll) as f64 / max_scroll as f64) * (1.0 - thumb_fraction)
        };
        Self {
            id,
            bounds,
            thumb_fraction,
            thumb_offset,
        }
    }
}

impl Widget for ScrollBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        // Дорожка — на всю высоту, полупрозрачная; ручка — акцентным
        // цветом поверх (тот же принцип, что у `Slider::opacity`/`SLIDER_FILL`).
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: theme::SLIDER_TRACK,
            opacity: 0.5,
        });
        let top = self.bounds.cy - self.bounds.h / 2.0;
        let thumb_h = (self.bounds.h * self.thumb_fraction).max(theme::SCROLLBAR_MIN_THUMB_H);
        let thumb_top = top + self.bounds.h * self.thumb_offset;
        out.push(Primitive::Fill {
            rect: Box2D {
                cx: self.bounds.cx,
                cy: thumb_top + thumb_h / 2.0,
                w: self.bounds.w,
                h: thumb_h,
                rotation: 0.0,
            },
            color: theme::SLIDER_FILL,
            opacity: 0.9,
        });
    }

    /// Не интерактивна (доккомент структуры) — клики/hover сквозь неё к
    /// содержимому под ней, панель не отдаёт ей события указателя.
    fn hit_test(&self, _pos: Point) -> bool {
        false
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Числовое поле прозрачности (SPEC 3.6, п. 2; M2_UI_NOTES §8, пункт 4):
/// только цифры, `Backspace`, `Enter` — принять, `Esc` — отменить,
/// `Ctrl+V` — только цифры, каретка рисованная.
///
/// Текст в буфере — всегда ASCII-цифры, поэтому позиция каретки считается
/// и в символах, и в байтах. Пока поле не в фокусе, текст зеркалит
/// `value` (синхронизация с ползунком — [`NumericField::set_value`]).
pub struct NumericField {
    id: WidgetId,
    bounds: Box2D,
    min: u32,
    max: u32,
    max_len: usize,
    value: u32,
    /// Редактируемый текст (в фокусе); вне фокуса == value.to_string().
    text: String,
    /// Каретка: индекс символа 0..=len (курсор ПЕРЕД ним).
    caret: usize,
    focused: bool,
    /// Текст на момент получения фокуса — для отмены по `Esc`/потере фокуса.
    original: String,
    submitted: Option<u32>,
    cancelled: bool,
}

impl NumericField {
    /// Поле диапазона `min..=max`. `max_len` ограничивает ввод (для 1–100
    /// достаточно трёх символов). Паника при `min >= max`.
    pub fn new(
        id: WidgetId,
        bounds: Box2D,
        min: u32,
        max: u32,
        max_len: usize,
        value: u32,
    ) -> Self {
        assert!(min < max, "пустой диапазон числового поля");
        let value = value.clamp(min, max);
        let text = value.to_string();
        Self {
            id,
            bounds,
            min,
            max,
            max_len,
            value,
            caret: text.len(),
            original: text.clone(),
            text,
            focused: false,
            submitted: None,
            cancelled: false,
        }
    }

    /// Поле прозрачности 1–100 (SPEC 3.6) с центром в `(cx, cy)`, шириной `w`.
    pub fn opacity(id: WidgetId, cx: f64, cy: f64, w: f64) -> Self {
        Self::new(
            id,
            Box2D {
                cx,
                cy,
                w,
                h: theme::FIELD_HEIGHT,
                rotation: 0.0,
            },
            1,
            100,
            3,
            100,
        )
    }

    /// Текущее принятое значение.
    pub fn value(&self) -> u32 {
        self.value
    }

    /// Установить значение извне (синхронизация с ползунком). В фокусе
    /// редактируемый текст не трогаем — пользователь печатает.
    pub fn set_value(&mut self, value: u32) {
        self.value = value.clamp(self.min, self.max);
        if !self.focused {
            self.text = self.value.to_string();
            self.caret = self.text.len();
        }
    }

    /// Принятое по `Enter` значение с прошлого опроса (сбрасывается).
    pub fn take_submitted(&mut self) -> Option<u32> {
        self.submitted.take()
    }

    /// Была ли отмена по `Esc` с прошлого опроса (сбрасывается). Ядру:
    /// этот `Esc` поглощён полем и не должен выходить из режима редактирования.
    pub fn take_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.cancelled)
    }

    /// Левая координата текста внутри поля.
    fn text_origin_x(&self) -> f64 {
        self.bounds.cx - self.bounds.w / 2.0 + theme::FIELD_PAD
    }

    /// Позиция каретки по X клика: ближайший разрыв между символами.
    fn caret_from_x(&self, x: f64) -> usize {
        let rel = x - self.text_origin_x();
        let mut best = 0;
        let mut best_dist = f64::INFINITY;
        for i in 0..=self.text.len() {
            let d = (text::width_up_to(&self.text, i) - rel).abs();
            if d < best_dist {
                best_dist = d;
                best = i;
            }
        }
        best
    }

    /// Вставить цифру в каретку (с учётом `max_len`).
    fn insert_digit(&mut self, digit: u8) -> bool {
        if self.text.len() >= self.max_len {
            return false;
        }
        self.text.insert(self.caret, char::from(b'0' + digit));
        self.caret += 1;
        true
    }

    /// Принять: пустой текст — как отмена; иначе привести к диапазону
    /// (SPEC 3.6: «значения вне 1–100 отсекаются») и отпустить фокус.
    fn submit(&mut self) {
        if self.text.is_empty() {
            self.cancel();
            return;
        }
        // В тексте только ASCII-цифры и длина ограничена — parse безопасен.
        let v = self
            .text
            .parse::<u32>()
            .unwrap_or(self.min)
            .clamp(self.min, self.max);
        self.value = v;
        self.text = v.to_string();
        self.caret = self.text.len();
        self.focused = false;
        self.submitted = Some(v);
    }

    /// Отменить: вернуть текст к исходному и отпустить фокус.
    fn cancel(&mut self) {
        self.text.clone_from(&self.original);
        self.caret = self.text.len();
        self.focused = false;
        self.cancelled = true;
    }
}

impl Widget for NumericField {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        // Рамка (в фокусе — акцентная) и фон с отступом в 1 DIP.
        let border = if self.focused {
            theme::FIELD_BORDER_FOCUS
        } else {
            theme::FIELD_BORDER
        };
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: border,
            opacity: 1.0,
        });
        out.push(Primitive::Fill {
            rect: Box2D {
                w: (self.bounds.w - 2.0).max(0.0),
                h: (self.bounds.h - 2.0).max(0.0),
                ..self.bounds
            },
            color: theme::FIELD_BG,
            opacity: 1.0,
        });
        // Текст: левый край + отступ, по вертикали — по центру поля.
        if !self.text.is_empty() {
            let (tw, th) = text::text_size(&self.text);
            out.push(Primitive::Text {
                rect: Box2D {
                    cx: self.text_origin_x() + tw / 2.0,
                    cy: self.bounds.cy,
                    w: tw,
                    h: th,
                    rotation: 0.0,
                },
                text: self.text.clone(),
                color: theme::TEXT,
                opacity: 1.0,
            });
        }
        // Рисованная каретка (1 DIP шириной, чуть выше строки).
        if self.focused {
            let cx = self.text_origin_x() + text::width_up_to(&self.text, self.caret);
            out.push(Primitive::Fill {
                rect: Box2D {
                    cx: cx + 0.5,
                    cy: self.bounds.cy,
                    w: 1.0,
                    h: text::LINE_HEIGHT + 2.0,
                    rotation: 0.0,
                },
                color: theme::CARET,
                opacity: 1.0,
            });
        }
    }

    fn wants_focus(&self) -> bool {
        true
    }

    fn has_focus(&self) -> bool {
        self.focused
    }

    fn on_blur(&mut self) {
        if self.focused {
            self.cancel();
        }
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        let PointerEvent::Down { pos } = ev else {
            return false;
        };
        if !self.hit_test(pos) {
            return false;
        }
        if !self.focused {
            self.focused = true;
            self.original.clone_from(&self.text);
        }
        self.caret = self.caret_from_x(pos.0);
        true
    }

    fn key_event(&mut self, key: Key) -> bool {
        if !self.focused {
            return false;
        }
        match key {
            Key::Digit(d) => self.insert_digit(d),
            Key::Char(_) => false,
            Key::Backspace => {
                if self.caret == 0 {
                    return false;
                }
                self.text.remove(self.caret - 1);
                self.caret -= 1;
                true
            }
            Key::ArrowLeft => {
                let new = self.caret.saturating_sub(1);
                std::mem::replace(&mut self.caret, new) != new
            }
            Key::ArrowRight => {
                let new = (self.caret + 1).min(self.text.len());
                std::mem::replace(&mut self.caret, new) != new
            }
            Key::Enter => {
                self.submit();
                true
            }
            Key::Escape => {
                self.cancel();
                true
            }
        }
    }

    fn paste(&mut self, text: &str) -> bool {
        if !self.focused {
            return false;
        }
        let mut changed = false;
        for c in text.chars().filter(|c| c.is_ascii_digit()) {
            if self.text.len() >= self.max_len {
                break;
            }
            self.text.insert(self.caret, c);
            self.caret += 1;
            changed = true;
        }
        changed
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Текстовое поле произвольных строк (SPEC «закрепление окон» — поля
/// `process_name`/`title_pattern` редактора правил соседства): свободный
/// ввод через [`Key::Char`], `Backspace`/стрелки, `Enter` — принять,
/// `Esc` — отменить, `Ctrl+V` — вставка с фильтрацией управляющих символов.
/// Каретка рисованная; позиция считается в символах (`char`), как у
/// [`NumericField`] (для произвольных строк — в т.ч. кириллических —
/// счёт по байтам разрезал бы символы пополам).
///
/// Пока поле не в фокусе, текст зеркалит принятое значение
/// ([`TextField::set_text`]); пустой текст рисуется плейсхолдером
/// приглушённым, если он задан.
pub struct TextField {
    id: WidgetId,
    bounds: Box2D,
    max_len: usize,
    /// Текст: принятое значение; в фокусе — редактируемый буфер.
    text: String,
    /// Каретка: индекс символа 0..=len (курсор ПЕРЕД ним).
    caret: usize,
    focused: bool,
    /// Текст на момент получения фокуса — для отмены по `Esc`/потере фокуса.
    original: String,
    submitted: Option<String>,
    cancelled: bool,
    placeholder: Option<String>,
}

impl TextField {
    /// Поле с начальным текстом `text` (обрезается до `max_len` символов).
    pub fn new(id: WidgetId, bounds: Box2D, text: &str, max_len: usize) -> Self {
        let text: String = text.chars().take(max_len).collect();
        Self {
            id,
            bounds,
            max_len,
            caret: text.chars().count(),
            original: text.clone(),
            text,
            focused: false,
            submitted: None,
            cancelled: false,
            placeholder: None,
        }
    }

    /// Поле с плейсхолдером `placeholder` (рисуется приглушённым, пока
    /// текст пуст).
    pub fn with_placeholder(
        id: WidgetId,
        bounds: Box2D,
        text: &str,
        max_len: usize,
        placeholder: &str,
    ) -> Self {
        let mut field = Self::new(id, bounds, text, max_len);
        field.placeholder = Some(placeholder.to_string());
        field
    }

    /// Текущий принятый текст.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Установить текст извне (пересборка панели из `rules` — состояние
    /// полей живёт в `cfg`, а не в панели). В фокусе редактируемый текст
    /// не трогаем — пользователь печатает.
    pub fn set_text(&mut self, text: &str) {
        if self.focused {
            return;
        }
        self.text = text.chars().take(self.max_len).collect();
        self.caret = self.text.chars().count();
    }

    /// Принятый по `Enter` текст с прошлого опроса (сбрасывается).
    pub fn take_submitted(&mut self) -> Option<String> {
        self.submitted.take()
    }

    /// Была ли отмена по `Esc` с прошлого опроса (сбрасывается). Ядру:
    /// этот `Esc` поглощён полем и не должен выходить из режима редактирования.
    pub fn take_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.cancelled)
    }

    /// Левая координата текста внутри поля.
    fn text_origin_x(&self) -> f64 {
        self.bounds.cx - self.bounds.w / 2.0 + theme::FIELD_PAD
    }

    /// Позиция каретки по X клика: ближайший разрыв между символами.
    fn caret_from_x(&self, x: f64) -> usize {
        let rel = x - self.text_origin_x();
        let mut best = 0;
        let mut best_dist = f64::INFINITY;
        for i in 0..=self.text.chars().count() {
            let d = (text::width_up_to(&self.text, i) - rel).abs();
            if d < best_dist {
                best_dist = d;
                best = i;
            }
        }
        best
    }

    /// Вставить символ в каретку (управляющие символы отбрасываются).
    fn insert_char(&mut self, c: char) -> bool {
        if c.is_control() || self.text.chars().count() >= self.max_len {
            return false;
        }
        let at = self
            .text
            .char_indices()
            .nth(self.caret)
            .map_or(self.text.len(), |(i, _)| i);
        self.text.insert(at, c);
        self.caret += 1;
        true
    }

    /// Принять текст и отпустить фокус.
    fn submit(&mut self) {
        self.caret = self.text.chars().count();
        self.focused = false;
        self.submitted = Some(self.text.clone());
    }

    /// Отменить: вернуть текст к исходному и отпустить фокус.
    fn cancel(&mut self) {
        self.text.clone_from(&self.original);
        self.caret = self.text.chars().count();
        self.focused = false;
        self.cancelled = true;
    }
}

impl Widget for TextField {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let border = if self.focused {
            theme::FIELD_BORDER_FOCUS
        } else {
            theme::FIELD_BORDER
        };
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: border,
            opacity: 1.0,
        });
        out.push(Primitive::Fill {
            rect: Box2D {
                w: (self.bounds.w - 2.0).max(0.0),
                h: (self.bounds.h - 2.0).max(0.0),
                ..self.bounds
            },
            color: theme::FIELD_BG,
            opacity: 1.0,
        });
        // Текст: левый край + отступ, по вертикали — по центру поля. Пустое
        // поле рисует плейсхолдер приглушённым (если задан).
        let shown = if self.text.is_empty() {
            self.placeholder.clone().unwrap_or_default()
        } else {
            self.text.clone()
        };
        if !shown.is_empty() {
            let (tw, th) = text::text_size(&shown);
            out.push(Primitive::Text {
                rect: Box2D {
                    cx: self.text_origin_x() + tw / 2.0,
                    cy: self.bounds.cy,
                    w: tw,
                    h: th,
                    rotation: 0.0,
                },
                text: shown,
                color: theme::TEXT,
                opacity: if self.text.is_empty() { 0.45 } else { 1.0 },
            });
        }
        // Рисованная каретка (1 DIP шириной, чуть выше строки).
        if self.focused {
            let cx = self.text_origin_x() + text::width_up_to(&self.text, self.caret);
            out.push(Primitive::Fill {
                rect: Box2D {
                    cx: cx + 0.5,
                    cy: self.bounds.cy,
                    w: 1.0,
                    h: text::LINE_HEIGHT + 2.0,
                    rotation: 0.0,
                },
                color: theme::CARET,
                opacity: 1.0,
            });
        }
    }

    fn wants_focus(&self) -> bool {
        true
    }

    fn has_focus(&self) -> bool {
        self.focused
    }

    fn on_blur(&mut self) {
        if self.focused {
            self.cancel();
        }
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        let PointerEvent::Down { pos } = ev else {
            return false;
        };
        if !self.hit_test(pos) {
            return false;
        }
        if !self.focused {
            self.focused = true;
            self.original.clone_from(&self.text);
        }
        self.caret = self.caret_from_x(pos.0);
        true
    }

    fn key_event(&mut self, key: Key) -> bool {
        if !self.focused {
            return false;
        }
        match key {
            Key::Char(c) => self.insert_char(c),
            Key::Digit(d) => self.insert_char(char::from(b'0' + d)),
            Key::Backspace => {
                if self.caret == 0 {
                    return false;
                }
                let at = self
                    .text
                    .char_indices()
                    .nth(self.caret - 1)
                    .map_or(0, |(i, _)| i);
                self.text.remove(at);
                self.caret -= 1;
                true
            }
            Key::ArrowLeft => {
                let new = self.caret.saturating_sub(1);
                std::mem::replace(&mut self.caret, new) != new
            }
            Key::ArrowRight => {
                let new = (self.caret + 1).min(self.text.chars().count());
                std::mem::replace(&mut self.caret, new) != new
            }
            Key::Enter => {
                self.submit();
                true
            }
            Key::Escape => {
                self.cancel();
                true
            }
        }
    }

    fn paste(&mut self, text: &str) -> bool {
        if !self.focused {
            return false;
        }
        let mut changed = false;
        for c in text.chars().filter(|c| !c.is_control()) {
            if !self.insert_char(c) {
                break;
            }
            changed = true;
        }
        changed
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Чекбокс панели выбора окон (docs/M4_WINDOW_PICKER_DESIGN.md, §7.1 п. 2
/// и §7.3): квадрат с галочкой, рисуется примитивами `Fill` — глифов «☐»/«☑»
/// в битовом шрифте нет (RST_RENDER_AUDIT 2.6). Состояние удерживается
/// виджетом; ядро забирает переключение [`Checkbox::take_changed`] и пишет
/// его в `cfg` тем же путём, что кнопки тулбара.
///
/// Режим `icon_toggle` ([`Checkbox::icon_toggle`], панель свойств
/// закреплённого окна) рисует состояние иконкой замка [`Icon::Lock`] /
/// [`Icon::LockOpen`] на фоне кнопки вместо квадрата с галочкой — семантика
/// (checked/`take_changed`) та же, меняется только вид.
pub struct Checkbox {
    id: WidgetId,
    bounds: Box2D,
    checked: bool,
    /// Оформление (см. [`WidgetStyle`]).
    style: WidgetStyle,
    /// Недоступен (строки, которые нельзя выразить правилом, панель задач
    /// при `never_overlap_taskbar` — §2.3, §7.8): рисуется приглушённым,
    /// хит-тест отключён.
    disabled: bool,
    hovered: bool,
    /// Нажат (указатель зажат внутри), переключение ещё не свершилось.
    armed: bool,
    changed: bool,
    /// Рисовать состояние иконкой замка на фоне кнопки (вместо квадрата
    /// с галочкой) — панель свойств закреплённого окна.
    icon_toggle: bool,
}

impl Checkbox {
    /// Чекбокс с состоянием `checked` в прямоугольнике `bounds` (DIP).
    pub fn new(id: WidgetId, bounds: Box2D, checked: bool) -> Self {
        Self {
            id,
            bounds,
            checked,
            style: WidgetStyle::default(),
            disabled: false,
            hovered: false,
            armed: false,
            changed: false,
            icon_toggle: false,
        }
    }

    /// Тот же чекбокс в заданном оформлении (см. [`WidgetStyle`]).
    pub fn with_style(mut self, style: WidgetStyle) -> Self {
        self.style = style;
        self
    }

    /// Чекбокс стандартного размера (`theme::CHECKBOX_SIZE`) с центром
    /// в `(cx, cy)`.
    pub fn standard(id: WidgetId, cx: f64, cy: f64, checked: bool) -> Self {
        Self::new(
            id,
            Box2D {
                cx,
                cy,
                w: theme::CHECKBOX_SIZE,
                h: theme::CHECKBOX_SIZE,
                rotation: 0.0,
            },
            checked,
        )
    }

    /// Переключатель в виде кнопки с иконкой замка (панель свойств
    /// закреплённого окна): сторона [`theme::BUTTON_SIZE`], состояние
    /// рисуется [`Icon::Lock`] (заблокировано) / [`Icon::LockOpen`]
    /// (свободно). Клик/переключение — как у обычного чекбокса.
    pub fn icon_toggle(id: WidgetId, cx: f64, cy: f64, checked: bool) -> Self {
        let mut cb = Self::new(
            id,
            Box2D {
                cx,
                cy,
                w: theme::BUTTON_SIZE,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            checked,
        );
        cb.icon_toggle = true;
        cb
    }

    /// Текущее состояние.
    pub fn checked(&self) -> bool {
        self.checked
    }

    /// Установить состояние извне (пересборка панели из `rules` — §2.1:
    /// состояние чекбоксов живёт в `cfg`, а не в панели).
    pub fn set_checked(&mut self, checked: bool) {
        self.checked = checked;
    }

    /// Заблокировать/разблокировать (disabled не хитуется и не переключается).
    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
    }

    /// Переключение с прошлого опроса: новое состояние (флаг сбрасывается).
    pub fn take_changed(&mut self) -> Option<bool> {
        if self.changed {
            self.changed = false;
            Some(self.checked)
        } else {
            None
        }
    }
}

impl Widget for Checkbox {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn hit_test(&self, pos: Point) -> bool {
        !self.disabled && box_contains(&self.bounds, pos)
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let opacity = if self.disabled { 0.45 } else { 1.0 };
        if self.icon_toggle {
            // Кнопка с иконкой замка: фон как у [`Button`], состояние —
            // иконка Lock (заблокировано) / LockOpen (свободно).
            let settings = self.style == WidgetStyle::Settings;
            let bg = match (settings, self.armed, self.hovered) {
                (true, _, true) => theme::settings::CHECK_BG_HOVER,
                (true, _, _) => theme::settings::CHECK_BG,
                (false, true, _) => theme::BUTTON_BG_ARMED,
                (false, _, true) => theme::BUTTON_BG_HOVER,
                (false, _, _) => theme::BUTTON_BG,
            };
            out.push(Primitive::Fill {
                rect: self.bounds,
                color: bg,
                opacity,
            });
            if settings {
                // Переключатель — «вдавленное» поле, как `.checkmark` в
                // настройках; нажатие временно поднимает его.
                settings_bevel(out, self.bounds, self.armed, opacity);
            }
            let pad = theme::BUTTON_PAD;
            // Иконка нужного состояния уже несёт смысл; выключенный замок
            // дополнительно приглушается — тот же приём, что у неактивной
            // вкладки настроек (`opacity: 0.5`).
            let icon_opacity = if settings && !self.checked {
                opacity * theme::settings::ICON_OFF_OPACITY
            } else {
                opacity
            };
            out.push(Primitive::Icon {
                rect: Box2D {
                    w: (self.bounds.w - 2.0 * pad).max(0.0),
                    h: (self.bounds.h - 2.0 * pad).max(0.0),
                    ..self.bounds
                },
                icon: if self.checked { Icon::Lock } else { Icon::LockOpen },
                opacity: icon_opacity,
            });
            return;
        }
        let border = if !self.disabled && (self.armed || self.hovered) {
            theme::FIELD_BORDER_FOCUS
        } else {
            theme::FIELD_BORDER
        };
        let bg = if self.armed {
            theme::BUTTON_BG_ARMED
        } else {
            theme::FIELD_BG
        };
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: border,
            opacity,
        });
        out.push(Primitive::Fill {
            rect: Box2D {
                w: (self.bounds.w - 2.0).max(0.0),
                h: (self.bounds.h - 2.0).max(0.0),
                ..self.bounds
            },
            color: bg,
            opacity,
        });
        // Галочка — два наклонных штриха «✓» (плечо и хвост), геометрия
        // в долях полустороны от центра, y — вниз.
        if self.checked {
            let s = self.bounds.w / 2.0;
            let thickness = (self.bounds.w * 0.15).max(1.5);
            let segments = [
                ((-0.35, 0.00), (-0.15, 0.22)),
                ((-0.15, 0.22), (0.40, -0.24)),
            ];
            for ((x0, y0), (x1, y1)) in segments {
                let dx = (x1 - x0) * s;
                let dy = (y1 - y0) * s;
                out.push(Primitive::Fill {
                    rect: Box2D {
                        cx: self.bounds.cx + (x0 + x1) / 2.0 * s,
                        cy: self.bounds.cy + (y0 + y1) / 2.0 * s,
                        w: dx.hypot(dy),
                        h: thickness,
                        rotation: dy.atan2(dx),
                    },
                    color: theme::SLIDER_FILL,
                    opacity,
                });
            }
        }
    }

    fn set_hovered(&mut self, hovered: bool) -> bool {
        std::mem::replace(&mut self.hovered, hovered) != hovered
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        match ev {
            PointerEvent::Down { pos } => {
                if !self.disabled && self.hit_test(pos) {
                    self.armed = true;
                    return true;
                }
                false
            }
            PointerEvent::Up { pos } => {
                if !self.armed {
                    return false;
                }
                self.armed = false;
                if self.hit_test(pos) {
                    self.checked = !self.checked;
                    self.changed = true;
                }
                true
            }
            PointerEvent::Move { .. } => false,
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Неинтерактивная текстовая надпись (подписи чекбоксов и заголовки секций
/// панели свойств закреплённого окна): рисует [`Primitive::Text`] по левому
/// краю; хит-теста нет — клики сквозь неё (аналог `RowLabel` панели выбора
/// окон, но без слота иконки).
pub struct Label {
    id: WidgetId,
    text_rect: Box2D,
    text: String,
    /// Приглушённая (заголовок секции) — рисуется полупрозрачной.
    dim: bool,
}

impl Label {
    /// Надпись `text` с левым краем `left` и центром строки `cy` (DIP);
    /// прямоугольник считается по [`text::text_size`].
    pub fn new(id: WidgetId, left: f64, cy: f64, text: &str) -> Self {
        let (tw, th) = text::text_size(text);
        Self {
            id,
            text_rect: Box2D {
                cx: left + tw / 2.0,
                cy,
                w: tw,
                h: th,
                rotation: 0.0,
            },
            text: text.to_string(),
            dim: false,
        }
    }

    /// Приглушить (заголовок секции) — рисуется полупрозрачным.
    pub fn set_dim(&mut self, dim: bool) {
        self.dim = dim;
    }
}

impl Widget for Label {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.text_rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.text_rect = bounds;
    }

    /// Не интерактивна — клики/hover сквозь неё, панель не отдаёт ей
    /// события указателя.
    fn hit_test(&self, _pos: Point) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Text {
            rect: self.text_rect,
            text: self.text.clone(),
            color: theme::TEXT,
            opacity: if self.dim { 0.5 } else { 1.0 },
        });
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Панель — контейнер виджетов с фоном (SPEC 3.6 «Тулбар», 3.8 «Панель у
/// курсора»). Порядок отрисовки = порядок добавления (нижний — первым);
/// хит-тест — в обратном порядке: верхний виджет выигрывает (M2_UI_NOTES §8).
///
/// Панель владеет двумя состояниями роутинга:
/// - захват указателя: после `Down` по виджету все `Move`/`Up` идут ему,
///   даже если курсор ушёл за границы (перетаскивание ручки ползунка);
/// - клавиатурный фокус: клик по виджету с `wants_focus` отдаёт ему клавиши
///   и вставку; клик вне его снимает фокус (через [`Widget::on_blur`]).
pub struct Panel {
    id: WidgetId,
    frame: Box2D,
    /// Оформление панели и её фона (виджетам стиль назначается отдельно —
    /// см. [`WidgetStyle`]).
    style: WidgetStyle,
    /// Виджеты в порядке отрисовки: первый — нижний.
    widgets: Vec<Box<dyn Widget>>,
    focus: Option<usize>,
    capture: Option<usize>,
    hovered: Option<usize>,
}

impl Panel {
    /// Пустая панель с фоном в прямоугольнике `frame` (DIP).
    pub fn new(id: WidgetId, frame: Box2D) -> Self {
        Self {
            id,
            frame,
            style: WidgetStyle::default(),
            widgets: Vec::new(),
            focus: None,
            capture: None,
            hovered: None,
        }
    }

    /// Панель в заданном оформлении (см. [`WidgetStyle`]).
    pub fn with_style(mut self, style: WidgetStyle) -> Self {
        self.style = style;
        self
    }

    /// Добавить виджет поверх уже добавленных.
    pub fn add_widget(&mut self, widget: impl Widget + 'static) {
        self.widgets.push(Box::new(widget));
    }

    /// Идентификатор панели.
    pub fn id(&self) -> WidgetId {
        self.id
    }

    /// Границы панели (фон), DIP.
    pub fn frame(&self) -> Box2D {
        self.frame
    }

    /// Идентификатор сфокусированного виджета, если есть.
    pub fn focused_widget(&self) -> Option<WidgetId> {
        self.focus.map(|i| self.widgets[i].id())
    }

    /// Идентификатор и границы (DIP) наведённого виджета, если есть — для
    /// тултипов (позиционируются относительно кнопки-источника).
    pub fn hovered_widget(&self) -> Option<(WidgetId, Box2D)> {
        self.hovered
            .map(|i| (self.widgets[i].id(), self.widgets[i].bounds()))
    }

    /// Доступ к виджету по идентификатору (опрос состояния ядром).
    pub fn widget<W: 'static>(&self, id: WidgetId) -> Option<&W> {
        self.widgets
            .iter()
            .find(|w| w.id() == id)?
            .as_any()
            .downcast_ref()
    }

    /// Изменяемый доступ к виджету по идентификатору.
    pub fn widget_mut<W: 'static>(&mut self, id: WidgetId) -> Option<&mut W> {
        self.widgets
            .iter_mut()
            .find(|w| w.id() == id)?
            .as_any_mut()
            .downcast_mut()
    }

    /// Сдвинуть панель со всеми виджетами (тулбар едет за выделением,
    /// панель 3.8 перетаскивается — SPEC 3.8 «панель перетаскиваемая»).
    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.frame.cx += dx;
        self.frame.cy += dy;
        for w in &mut self.widgets {
            let mut b = w.bounds();
            b.cx += dx;
            b.cy += dy;
            w.set_bounds(b);
        }
    }

    /// Точка внутри панели (фон или любой виджет).
    pub fn hit_test(&self, pos: Point) -> bool {
        box_contains(&self.frame, pos) || self.widgets.iter().any(|w| w.hit_test(pos))
    }

    /// Примитивы отрисовки: фон панели, затем виджеты в порядке добавления.
    pub fn draw(&self, out: &mut Vec<Primitive>) {
        if self.style == WidgetStyle::Settings {
            // Полупрозрачный серый прямоугольник с объёмной рамкой — модалка
            // окна настроек (`.wrapper` в styles.css).
            out.push(Primitive::Fill {
                rect: self.frame,
                color: theme::settings::BG,
                opacity: theme::settings::BG_OPACITY,
            });
            settings_bevel(out, self.frame, true, theme::settings::BG_OPACITY);
            for w in &self.widgets {
                w.draw(out);
            }
            return;
        }
        out.push(Primitive::Fill {
            rect: self.frame,
            color: theme::PANEL_BORDER,
            opacity: theme::PANEL_BG_OPACITY,
        });
        out.push(Primitive::Fill {
            rect: Box2D {
                w: (self.frame.w - 2.0).max(0.0),
                h: (self.frame.h - 2.0).max(0.0),
                ..self.frame
            },
            color: theme::PANEL_BG,
            opacity: theme::PANEL_BG_OPACITY,
        });
        for w in &self.widgets {
            w.draw(out);
        }
    }

    /// Снять клавиатурный фокус (с `on_blur` виджета). Возвращает `true`,
    /// если фокус был — рамка поля изменится, нужна перерисовка.
    fn blur_focus(&mut self) -> bool {
        if let Some(i) = self.focus.take() {
            self.widgets[i].on_blur();
            return true;
        }
        false
    }

    /// Индекс верхнего виджета под точкой (обратный порядок отрисовки).
    fn top_at(&self, pos: Point) -> Option<usize> {
        self.widgets.iter().rposition(|w| w.hit_test(pos))
    }

    /// Событие указателя: роутинг с захватом и клавиатурным фокусом.
    pub fn pointer_event(&mut self, ev: PointerEvent) -> EventResult {
        match ev {
            PointerEvent::Down { pos } => {
                if let Some(i) = self.top_at(pos) {
                    self.capture = Some(i);
                    let mut redraw = if self.widgets[i].wants_focus() {
                        if self.focus == Some(i) {
                            false
                        } else {
                            self.blur_focus();
                            self.focus = Some(i);
                            true
                        }
                    } else {
                        self.blur_focus()
                    };
                    redraw |= self.widgets[i].pointer_event(ev);
                    return EventResult {
                        consumed: true,
                        redraw,
                    };
                }
                if box_contains(&self.frame, pos) {
                    // Клик по фону панели: снять фокус, событие не уходит в сцену.
                    return EventResult {
                        consumed: true,
                        redraw: self.blur_focus(),
                    };
                }
                EventResult::default()
            }
            PointerEvent::Move { pos } => {
                if let Some(i) = self.capture {
                    return EventResult {
                        consumed: true,
                        redraw: self.widgets[i].pointer_event(ev),
                    };
                }
                let new_hover = self.top_at(pos);
                let mut redraw = false;
                if new_hover != self.hovered {
                    if let Some(old) = self.hovered {
                        redraw |= self.widgets[old].set_hovered(false);
                    }
                    if let Some(new) = new_hover {
                        redraw |= self.widgets[new].set_hovered(true);
                    }
                    self.hovered = new_hover;
                }
                EventResult {
                    consumed: new_hover.is_some() || box_contains(&self.frame, pos),
                    redraw,
                }
            }
            PointerEvent::Up { pos } => {
                if let Some(i) = self.capture.take() {
                    return EventResult {
                        consumed: true,
                        redraw: self.widgets[i].pointer_event(ev),
                    };
                }
                EventResult {
                    consumed: self.hit_test(pos),
                    redraw: false,
                }
            }
        }
    }

    /// Клавиша: уходит сфокусированному виджету; без фокуса панель клавиши
    /// не потребляет (они хоткеи ядра — M2_UI_NOTES §9).
    pub fn key_event(&mut self, key: Key) -> EventResult {
        let Some(i) = self.focus else {
            return EventResult::default();
        };
        let redraw = self.widgets[i].key_event(key);
        // Поле могло отпустить фокус само (Enter/Esc) — снимаем роутинг.
        if !self.widgets[i].has_focus() {
            self.focus = None;
        }
        EventResult {
            consumed: true,
            redraw,
        }
    }

    /// Вставка из буфера: сфокусированному виджету (фильтрация — его дело).
    pub fn paste(&mut self, text: &str) -> EventResult {
        let Some(i) = self.focus else {
            return EventResult::default();
        };
        EventResult {
            consumed: true,
            redraw: self.widgets[i].paste(text),
        }
    }
}

// ---------------------------------------------------------------------------
// Панель свойств закреплённого окна (SPEC «закрепление окон», задача 3/6):
// переключатели замков, редактор правил соседства, кнопка «Открепить»
// ---------------------------------------------------------------------------

/// Идентификатор панели свойств закреплённого окна. Диапазон 300+: тулбар
/// 0-8, панель у курсора 100+, панель выбора окон 200-204.
pub const PINNED_PANEL_ID: WidgetId = 300;
/// Переключатель «запретить перемещение» (move-lock, SPEC «закрепление окон»
/// #5.1) — кнопка-иконка замка ([`Checkbox::icon_toggle`]).
pub const PINNED_CHECK_MOVE_LOCK: WidgetId = 301;
/// Переключатель «запретить ввод» (interact-lock, SPEC «закрепление окон»
/// #5.2) — кнопка-иконка замка.
pub const PINNED_CHECK_INTERACT_LOCK: WidgetId = 302;
/// Кнопка «Открепить» — единственное действие удаления закреплённого окна
/// (SPEC #9: открепление не закрывает окно).
pub const PINNED_BTN_UNPIN: WidgetId = 303;
/// Кнопка «Добавить правило» в шапке списка соседства.
pub const PINNED_BTN_ADD_RULE: WidgetId = 304;
/// Подпись переключателя move-lock (не интерактивна).
const PINNED_LABEL_MOVE_LOCK: WidgetId = 306;
/// Подпись переключателя interact-lock (не интерактивна).
const PINNED_LABEL_INTERACT_LOCK: WidgetId = 307;
/// Заголовок секции правил соседства (не интерактивен).
const PINNED_LABEL_RULES: WidgetId = 308;
/// Полоса скролла списка правил (не интерактивна, см. [`ScrollBar`]).
const PINNED_SCROLLBAR_ID: WidgetId = 305;
/// Разделитель секции замков и правил соседства (не интерактивен).
const PINNED_DIVIDER_RULES: WidgetId = 309;

/// Ширина панели, DIP.
pub const PINNED_PANEL_WIDTH: f64 = 320.0;

/// Кнопка «Показывать только на…»: открывает список окон, чтобы выбрать
/// окно-хозяина (запрос пользователя 2026-08-22).
pub const PINNED_BTN_ADD_HOST: WidgetId = 340;
/// Первая строка списка окон-хозяев; `PINNED_HOST_ROW_BASE + индекс` —
/// кнопка «убрать это правило».
pub const PINNED_HOST_ROW_BASE: WidgetId = 350;
/// Надпись строки правила (неинтерактивная) — тот же приём разделения
/// «кнопка + надпись», что в списках выбора окон.
const PINNED_HOST_LABEL_BASE: WidgetId = 380;
/// Заголовок секции правил.
const PINNED_LABEL_HOSTS: WidgetId = 339;
/// Сколько строк правил панель показывает без прокрутки: правил обычно
/// одно-два, а панель живёт ВНУТРИ окна и не должна разрастаться.
pub const PINNED_VISIBLE_HOSTS: usize = 4;

/// Высота панели инструментов закреплённого окна при `hosts` правилах.
/// Секция правил появляется целиком (заголовок + строки + кнопка), поэтому
/// высота считается здесь, а не берётся константой.
pub fn pinned_lock_panel_height(hosts: usize) -> f64 {
    let rows = hosts.min(PINNED_VISIBLE_HOSTS) as f64;
    PINNED_LOCK_PANEL_HEIGHT
        + PINNED_GAP
        + PINNED_SECTION_ROW_H // заголовок секции
        + rows * PINNED_SECTION_ROW_H
        + PINNED_SECTION_ROW_H // кнопка «Показывать только на…»
}

/// Минимальная ширина панели инструментов закреплённого окна, DIP.
/// Панель рисуется ВНУТРИ окна (`overlay_manager::rebuild_pinned_panel`) и
/// поэтому ужимается под узкие окна — но не ниже этой границы: слева от
/// подписи стоят переключатель ([`theme::BUTTON_SIZE`]) и отступы
/// ([`PINNED_PAD`] + [`PINNED_GAP`]), а самой длинной подписи
/// («Блокировать перемещение») нужно около 150 DIP.
pub const PINNED_LOCK_PANEL_MIN_WIDTH: f64 = 210.0;
/// Высота строки-секции (переключатель/шапка/кнопка), DIP — кнопка
/// [`theme::BUTTON_SIZE`] с воздухом.
pub const PINNED_SECTION_ROW_H: f64 = theme::BUTTON_SIZE + 4.0;
/// Высота строки правила, DIP — поле [`theme::FIELD_HEIGHT`] с воздухом.
pub const PINNED_RULE_ROW_H: f64 = theme::FIELD_HEIGHT + 6.0;
/// Сколько строк правил видно без скролла (виртуализация, как
/// `PICKER_VISIBLE_ROWS` панели выбора окон).
pub const PINNED_VISIBLE_RULES: usize = 4;
/// Внутренний отступ панели, DIP.
pub const PINNED_PAD: f64 = 6.0;
/// Зазор между элементами, DIP.
pub const PINNED_GAP: f64 = 8.0;
/// Полная высота панели, DIP: отступы + две строки переключателей + шапка
/// списка + видимые правила + строка кнопки «Открепить».
pub const PINNED_PANEL_HEIGHT: f64 = 2.0 * PINNED_PAD
    + 3.0 * PINNED_SECTION_ROW_H
    + PINNED_VISIBLE_RULES as f64 * PINNED_RULE_ROW_H
    + theme::BUTTON_SIZE;
/// Высота УРЕЗАННОЙ панели ([`build_pinned_lock_panel`]), DIP: отступы + две
/// строки переключателей замков + зазор + строка кнопки «Открепить» — без
/// раздела правил соседства (тот остаётся только в [`build_pinned_panel`]).
/// Больше [`PINNED_PANEL_ID`]-минимума вызывающего слоя (`PINNED_MINIMAL_
/// PANEL_HEIGHT` в `overlay_manager`, только «Открепить»), меньше
/// [`PINNED_PANEL_HEIGHT`] (тот резервирует место под список правил).
pub const PINNED_LOCK_PANEL_HEIGHT: f64 =
    2.0 * PINNED_PAD + 2.0 * PINNED_SECTION_ROW_H + PINNED_GAP + theme::BUTTON_SIZE;

/// Поле строки правила соседства — декодируется из `WidgetId` строки
/// ([`decode_pinned_row_id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum PinnedRowField {
    /// Кнопка удаления правила.
    Remove,
    /// Поле `process_name`.
    ProcessName,
    /// Поле `title_pattern` (маска с `*`).
    TitlePattern,
}

/// База идентификаторов строк правил: `PINNED_ROW_BASE + (индекс_правила
/// << PINNED_ROW_FIELD_BITS) + смещение_поля`. Индекс правила — позиция в
/// `Vec<OverlapRule>` (правил соседства единицы), смещение — вариант
/// [`PinnedRowField`]. Кодируется РЕАЛЬНЫЙ индекс (не видимый): вызывающий
/// слой декодирует id по [`decode_pinned_row_id`] независимо от скролла.
pub const PINNED_ROW_BASE: WidgetId = 0x10_0000;
/// Битов сдвига индекса правила в кодировке `WidgetId`.
pub const PINNED_ROW_FIELD_BITS: u32 = 2;

/// `WidgetId` элемента строки `rule_index` (позиция в списке правил).
pub fn pinned_row_id(rule_index: usize, field: PinnedRowField) -> WidgetId {
    PINNED_ROW_BASE + ((rule_index as WidgetId) << PINNED_ROW_FIELD_BITS) + field as WidgetId
}

/// Разобрать `WidgetId` строки обратно в `(индекс_правила, поле)`; для
/// идентификаторов, не принадлежащих строкам правил, — `None`.
pub fn decode_pinned_row_id(id: WidgetId) -> Option<(usize, PinnedRowField)> {
    if id < PINNED_ROW_BASE {
        return None;
    }
    let raw = id - PINNED_ROW_BASE;
    let field = match raw & ((1 << PINNED_ROW_FIELD_BITS) - 1) {
        0 => PinnedRowField::Remove,
        1 => PinnedRowField::ProcessName,
        2 => PinnedRowField::TitlePattern,
        _ => return None,
    };
    Some(((raw >> PINNED_ROW_FIELD_BITS) as usize, field))
}

/// Тонкая неинтерактивная разделительная линия между секциями панели
/// свойств закреплённого окна — визуально отделяет переключатели замков от
/// списка правил соседства (тот же неинтерактивный статус, что у [`Label`]:
/// хит-теста нет, клики сквозь неё).
struct Divider {
    id: WidgetId,
    rect: Box2D,
}

impl Divider {
    fn new(id: WidgetId, cx: f64, cy: f64, w: f64) -> Self {
        Self {
            id,
            rect: Box2D {
                cx,
                cy,
                w,
                h: 1.0,
                rotation: 0.0,
            },
        }
    }
}

impl Widget for Divider {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.rect = bounds;
    }

    fn hit_test(&self, _pos: Point) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Fill {
            rect: self.rect,
            color: theme::PANEL_BORDER,
            opacity: 0.6,
        });
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Результат [`build_pinned_panel`]: панель + число строк правил для клампа
/// скролла вызывающим слоем (скролл валиден в `0..=total_rows - 1`, как
/// `PickerPanel::total_rows` панели выбора окон).
pub struct PinnedPanel {
    /// Собранная панель (замки + видимый срез правил + «Открепить»).
    pub panel: Panel,
    /// Всего строк правил.
    pub total_rows: usize,
}

/// Собрать панель свойств закреплённого окна (SPEC «закрепление окон» #9):
/// два переключателя замков ([`Checkbox::icon_toggle`] — иконка [`Icon::Lock`]
/// / [`Icon::LockOpen`] вместо квадрата с галочкой; `move_locked`/
/// `interact_locked` — состояние живёт в рантайм-модели закрепления, не в
/// панели), список правил соседства (по паре [`TextField`] на [`OverlapRule`],
/// кнопки добавления/удаления — `PINNED_BTN_ADD_RULE`/`PinnedRowField::Remove`)
/// и кнопка «Открепить». `scroll` — сколько строк правил пропустить сверху
/// (виртуализация: строятся только строки
/// `[scroll, scroll + PINNED_VISIBLE_RULES)`, `Panel` не трогается); `frame` —
/// рамка панели (типовой размер — [`PINNED_PANEL_WIDTH`]×
/// [`PINNED_PANEL_HEIGHT`]).
///
/// Оформление (фидбэк пользователя 2026-08-17, «панель выглядит плохо»):
/// секции разделены линией [`Divider`], заголовок «Соседние окна» приглушён
/// и поясняет, что ниже — правила соседства; кнопка «Добавить правило» —
/// иконка [`Icon::Plus`], поля правил имеют разговорные плейсхолдеры
/// («процесс chrome.exe» / «маска заголовка окна»), чтобы назначение строк
/// читалось без подсказки. Подписи/тултипы кнопок — у вызывающего слоя
/// (`overlay_manager::pinned_panel_tooltip_text`).
///
/// Правый край контента всегда резервирует колонку под [`ScrollBar`] —
/// ширина полей не скачет от наличия скролла (тот же приём, что в
/// `build_picker_panel` панели выбора окон).
pub fn build_pinned_panel(
    rules: &[OverlapRule],
    move_locked: bool,
    interact_locked: bool,
    scroll: usize,
    frame: Box2D,
) -> PinnedPanel {
    let mut panel = Panel::new(PINNED_PANEL_ID, frame);
    let left = frame.cx - frame.w / 2.0 + PINNED_PAD;
    let top = frame.cy - frame.h / 2.0;
    let bottom = frame.cy + frame.h / 2.0;
    let toggle_cx = left + theme::BUTTON_SIZE / 2.0;
    let label_left = toggle_cx + theme::BUTTON_SIZE / 2.0 + PINNED_GAP;

    // Секция замков: два переключателя-иконки с подписями.
    let cy_move = top + PINNED_PAD + PINNED_SECTION_ROW_H / 2.0;
    panel.add_widget(Checkbox::icon_toggle(
        PINNED_CHECK_MOVE_LOCK,
        toggle_cx,
        cy_move,
        move_locked,
    ));
    panel.add_widget(Label::new(
        PINNED_LABEL_MOVE_LOCK,
        label_left,
        cy_move,
        "Блокировать перемещение",
    ));
    let cy_interact = cy_move + PINNED_SECTION_ROW_H;
    panel.add_widget(Checkbox::icon_toggle(
        PINNED_CHECK_INTERACT_LOCK,
        toggle_cx,
        cy_interact,
        interact_locked,
    ));
    panel.add_widget(Label::new(
        PINNED_LABEL_INTERACT_LOCK,
        label_left,
        cy_interact,
        "Блокировать клики",
    ));

    // Разделитель секций: замки отделены от правил соседства тонкой линией.
    let cy_divider = cy_interact + PINNED_SECTION_ROW_H / 2.0;
    let right_edge = frame.cx + frame.w / 2.0 - PINNED_PAD - theme::SCROLLBAR_WIDTH - PINNED_GAP;
    panel.add_widget(Divider::new(
        PINNED_DIVIDER_RULES,
        (left + right_edge) / 2.0,
        cy_divider,
        right_edge - left,
    ));

    // Шапка списка: приглушённый заголовок + кнопка-иконка «+».
    let cy_header = cy_interact + PINNED_SECTION_ROW_H;
    let mut rules_label = Label::new(PINNED_LABEL_RULES, left, cy_header, "Соседние окна");
    rules_label.set_dim(true);
    panel.add_widget(rules_label);
    panel.add_widget(Button::icon(
        PINNED_BTN_ADD_RULE,
        right_edge - theme::BUTTON_SIZE / 2.0,
        cy_header,
        Icon::Plus,
    ));

    // Строки правил: одна пара полей + кнопка удаления на правило. Строятся
    // только видимые строки (виртуализация); обход — по реальным индексам.
    let list_top = top + PINNED_PAD + 3.0 * PINNED_SECTION_ROW_H;
    let fields_left = left + theme::BUTTON_SIZE + PINNED_GAP;
    let field_w = (right_edge - fields_left - PINNED_GAP) / 2.0;
    for (rule_index, rule) in rules.iter().enumerate() {
        let visible = rule_index as isize - scroll as isize;
        if !(0..PINNED_VISIBLE_RULES as isize).contains(&visible) {
            continue;
        }
        let cy = list_top + visible as f64 * PINNED_RULE_ROW_H + PINNED_RULE_ROW_H / 2.0;
        panel.add_widget(Button::icon(
            pinned_row_id(rule_index, PinnedRowField::Remove),
            left + theme::BUTTON_SIZE / 2.0,
            cy,
            Icon::Delete,
        ));
        for (field, value, placeholder) in [
            (
                PinnedRowField::ProcessName,
                &rule.process_name,
                "процесс chrome.exe",
            ),
            (
                PinnedRowField::TitlePattern,
                &rule.title_pattern,
                "маска заголовка окна",
            ),
        ] {
            let cx = match field {
                PinnedRowField::ProcessName => fields_left + field_w / 2.0,
                _ => fields_left + field_w + PINNED_GAP + field_w / 2.0,
            };
            panel.add_widget(TextField::with_placeholder(
                pinned_row_id(rule_index, field),
                Box2D {
                    cx,
                    cy,
                    w: field_w,
                    h: theme::FIELD_HEIGHT,
                    rotation: 0.0,
                },
                value.as_deref().unwrap_or(""),
                64,
                placeholder,
            ));
        }
    }

    // Полоса скролла — только когда правил больше видимых строк (тот же
    // приём, что в `build_picker_panel`: скролл колесом — у вызывающего
    // слоя, виджет лишь визуализирует положение).
    if rules.len() > PINNED_VISIBLE_RULES {
        let list_h = PINNED_VISIBLE_RULES as f64 * PINNED_RULE_ROW_H;
        panel.add_widget(ScrollBar::new(
            PINNED_SCROLLBAR_ID,
            Box2D {
                cx: right_edge + PINNED_GAP + theme::SCROLLBAR_WIDTH / 2.0,
                cy: list_top + list_h / 2.0,
                w: theme::SCROLLBAR_WIDTH,
                h: list_h,
                rotation: 0.0,
            },
            PINNED_VISIBLE_RULES,
            rules.len(),
            scroll,
        ));
    }

    // Кнопка «Открепить» — внизу, на всю ширину контента.
    panel.add_widget(Button::new(
        PINNED_BTN_UNPIN,
        Box2D {
            cx: frame.cx,
            cy: bottom - PINNED_PAD - theme::BUTTON_SIZE / 2.0,
            w: (frame.w - 2.0 * PINNED_PAD).max(0.0),
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label("Открепить".to_string()),
    ));

    PinnedPanel {
        panel,
        total_rows: rules.len(),
    }
}

/// Собрать УРЕЗАННУЮ панель свойств закреплённого окна: только секция замков
/// (переключатели [`PINNED_CHECK_MOVE_LOCK`]/[`PINNED_CHECK_INTERACT_LOCK`],
/// та же пара [`Checkbox::icon_toggle`]/[`Icon::Lock`]/[`Icon::LockOpen`], что
/// в [`build_pinned_panel`]) и кнопка «Открепить» ([`PINNED_BTN_UNPIN`]) —
/// без раздела правил соседства/z-order. Тот раздел решением пользователя
/// 2026-08-18 убран из UI и намеренно НЕ воспроизведён здесь; полная версия
/// с правилами остаётся нетронутой в [`build_pinned_panel`] для будущего
/// возврата.
///
/// Использует ТЕ ЖЕ id виджетов, что и [`build_pinned_panel`]
/// (`PINNED_CHECK_MOVE_LOCK`/`PINNED_CHECK_INTERACT_LOCK`/`PINNED_BTN_UNPIN`),
/// поэтому `overlay_manager::handle_pinned_panel_up` опрашивает эту панель
/// без изменений — виджетов с id раздела правил (`PINNED_LABEL_RULES`,
/// `PINNED_BTN_ADD_RULE`, `PINNED_SCROLLBAR_ID`) в результате нет, их ветки
/// просто не сработают. `frame` — рамка панели (типовой размер —
/// [`PINNED_PANEL_WIDTH`]×[`PINNED_LOCK_PANEL_HEIGHT`]).
pub fn build_pinned_lock_panel(
    move_locked: bool,
    interact_locked: bool,
    hosts: &[String],
    frame: Box2D,
) -> Panel {
    let mut panel = Panel::new(PINNED_PANEL_ID, frame).with_style(WidgetStyle::Settings);
    let left = frame.cx - frame.w / 2.0 + PINNED_PAD;
    let top = frame.cy - frame.h / 2.0;
    let bottom = frame.cy + frame.h / 2.0;
    let toggle_cx = left + theme::BUTTON_SIZE / 2.0;
    let label_left = toggle_cx + theme::BUTTON_SIZE / 2.0 + PINNED_GAP;

    // Секция замков: два переключателя-иконки с подписями (см.
    // `build_pinned_panel` — идентичная раскладка).
    let cy_move = top + PINNED_PAD + PINNED_SECTION_ROW_H / 2.0;
    panel.add_widget(
        Checkbox::icon_toggle(PINNED_CHECK_MOVE_LOCK, toggle_cx, cy_move, move_locked)
            .with_style(WidgetStyle::Settings),
    );
    panel.add_widget(Label::new(
        PINNED_LABEL_MOVE_LOCK,
        label_left,
        cy_move,
        "Блокировать перемещение",
    ));
    let cy_interact = cy_move + PINNED_SECTION_ROW_H;
    panel.add_widget(
        Checkbox::icon_toggle(
            PINNED_CHECK_INTERACT_LOCK,
            toggle_cx,
            cy_interact,
            interact_locked,
        )
        .with_style(WidgetStyle::Settings),
    );
    panel.add_widget(Label::new(
        PINNED_LABEL_INTERACT_LOCK,
        label_left,
        cy_interact,
        "Блокировать клики",
    ));

    // Секция «Показывать только на этих окнах» (запрос пользователя
    // 2026-08-22): пока список пуст, окно закреплено поверх всего; как
    // только в нём появляется окно-хозяин, закреплённое окно видно только
    // когда этот хозяин активен.
    let mut cy = cy_interact + PINNED_SECTION_ROW_H + PINNED_GAP;
    panel.add_widget(Label::new(
        PINNED_LABEL_HOSTS,
        left,
        cy,
        "Показывать только на окнах:",
    ));
    for (i, host) in hosts.iter().take(PINNED_VISIBLE_HOSTS).enumerate() {
        cy += PINNED_SECTION_ROW_H;
        let remove_cx = frame.cx + frame.w / 2.0 - PINNED_PAD - theme::BUTTON_SIZE / 2.0;
        panel.add_widget(
            Button::new(
                PINNED_HOST_ROW_BASE + i as WidgetId,
                Box2D {
                    cx: remove_cx,
                    cy,
                    w: theme::BUTTON_SIZE,
                    h: theme::BUTTON_SIZE,
                    rotation: 0.0,
                },
                ButtonContent::Label("×".to_string()),
            )
            .with_style(WidgetStyle::Settings),
        );
        panel.add_widget(Label::new(
            PINNED_HOST_LABEL_BASE + i as WidgetId,
            left,
            cy,
            host,
        ));
    }
    cy += PINNED_SECTION_ROW_H;
    panel.add_widget(
        Button::new(
            PINNED_BTN_ADD_HOST,
            Box2D {
                cx: frame.cx,
                cy,
                w: (frame.w - 2.0 * PINNED_PAD).max(0.0),
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label("Показывать только на…".to_string()),
        )
        .with_style(WidgetStyle::Settings),
    );

    // Кнопка «Открепить» — внизу, на всю ширину контента (см.
    // `build_pinned_panel` — идентичная раскладка).
    panel.add_widget(
        Button::new(
            PINNED_BTN_UNPIN,
            Box2D {
                cx: frame.cx,
                cy: bottom - PINNED_PAD - theme::BUTTON_SIZE / 2.0,
                w: (frame.w - 2.0 * PINNED_PAD).max(0.0),
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label("Открепить".to_string()),
        )
        .with_style(WidgetStyle::Settings),
    );

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    const ID_BTN: WidgetId = 1;
    const ID_SLIDER: WidgetId = 2;
    const ID_FIELD: WidgetId = 3;
    const ID_CHECK: WidgetId = 4;

    fn rect(cx: f64, cy: f64, w: f64, h: f64) -> Box2D {
        Box2D {
            cx,
            cy,
            w,
            h,
            rotation: 0.0,
        }
    }

    #[test]
    fn contains_axis_aligned_and_degenerate() {
        let r = rect(100.0, 50.0, 40.0, 20.0);
        assert!(box_contains(&r, (100.0, 50.0)));
        assert!(box_contains(&r, (120.0, 60.0)), "граница — попадание");
        assert!(!box_contains(&r, (120.1, 50.0)));
        assert!(
            !box_contains(&rect(0.0, 0.0, 0.0, 10.0), (0.0, 0.0)),
            "вырожденный"
        );
    }

    #[test]
    fn contains_rotated_uses_hittest_math() {
        // Поворот на 90°: длинная ось вертикальна (rst_core::hittest).
        let mut r = rect(200.0, 100.0, 100.0, 40.0);
        r.rotation = FRAC_PI_2;
        assert!(box_contains(&r, (200.0, 149.0)));
        assert!(!box_contains(&r, (221.0, 100.0)));
    }

    #[test]
    fn button_click_inside() {
        let mut b = Button::icon(ID_BTN, 50.0, 50.0, Icon::Delete);
        assert!(b.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) }));
        assert!(b.pointer_event(PointerEvent::Up { pos: (50.0, 50.0) }));
        assert!(b.take_click());
        assert!(!b.take_click(), "клик одноразовый");
    }

    #[test]
    fn button_press_inside_release_outside_no_click() {
        let mut b = Button::icon(ID_BTN, 50.0, 50.0, Icon::Delete);
        b.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) });
        b.pointer_event(PointerEvent::Up {
            pos: (200.0, 200.0),
        });
        assert!(!b.take_click());
    }

    #[test]
    fn button_hover_redraw_only_on_change() {
        let mut b = Button::icon(ID_BTN, 50.0, 50.0, Icon::Delete);
        assert!(b.set_hovered(true));
        assert!(!b.set_hovered(true));
        assert!(b.set_hovered(false));
    }

    #[test]
    fn button_draw_background_then_icon() {
        let b = Button::icon(ID_BTN, 50.0, 50.0, Icon::Eye);
        let mut out = Vec::new();
        b.draw(&mut out);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Primitive::Fill { .. }));
        assert!(matches!(
            out[1],
            Primitive::Icon {
                icon: Icon::Eye,
                ..
            }
        ));
    }

    /// Регрессия: `Text`-примитив кнопки-надписи не должен растягиваться на
    /// всю ширину кнопки (конвейер спрайтов маппит текстуру текста на `rect`
    /// 1:1 без сохранения пропорций — широкий `rect` при короткой надписи
    /// смазывал бы глиф по горизонтали). Ширина текстового прямоугольника
    /// обязана совпадать с натуральным размером надписи, не с шириной кнопки.
    #[test]
    fn button_label_text_rect_matches_natural_text_size_not_button_width() {
        let b = Button::new(
            ID_BTN,
            rect(50.0, 50.0, 300.0, theme::BUTTON_SIZE),
            ButtonContent::Label("ok".to_string()),
        );
        let mut out = Vec::new();
        b.draw(&mut out);
        let Primitive::Text { rect: text_rect, .. } = out[1] else {
            panic!("второй примитив — Text")
        };
        let (tw, th) = text::text_size("ok");
        assert_eq!(text_rect.w, tw, "ширина текста — натуральная, не 300");
        assert_eq!(text_rect.h, th);
        assert!(
            text_rect.w < 300.0,
            "надпись короче кнопки — растяжения быть не должно"
        );
    }

    /// Ползунок 0–100, центр (100, 50), ширина 112: ход ручки x ∈ [50, 150].
    fn slider() -> Slider {
        Slider::opacity(ID_SLIDER, 100.0, 50.0, 112.0)
    }

    #[test]
    fn slider_click_sets_value() {
        let mut s = slider();
        s.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        assert_eq!(s.value(), 50);
        assert_eq!(s.take_changed(), Some(50));
        assert_eq!(s.take_changed(), None);
    }

    #[test]
    fn slider_drag_clamps_to_range() {
        let mut s = slider();
        s.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        s.pointer_event(PointerEvent::Move { pos: (500.0, 50.0) });
        assert_eq!(s.value(), 100);
        s.pointer_event(PointerEvent::Move {
            pos: (-500.0, 50.0),
        });
        assert_eq!(s.value(), 0);
        s.pointer_event(PointerEvent::Up {
            pos: (-500.0, 50.0),
        });
        assert_eq!(s.value(), 0);
    }

    #[test]
    fn slider_custom_range() {
        let mut s = Slider::new(ID_SLIDER, rect(100.0, 50.0, 112.0, 20.0), 1, 100, 50);
        s.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) });
        assert_eq!(s.value(), 1, "левый край дорожки — минимум");
        s.pointer_event(PointerEvent::Move { pos: (150.0, 50.0) });
        assert_eq!(s.value(), 100, "правый край — максимум");
    }

    #[test]
    fn slider_move_without_drag_ignored() {
        let mut s = slider();
        assert!(!s.pointer_event(PointerEvent::Move { pos: (0.0, 50.0) }));
        assert_eq!(s.value(), 100);
    }

    #[test]
    fn slider_draw_primitives() {
        let mut s = slider();
        let mut out = Vec::new();
        s.draw(&mut out);
        assert_eq!(out.len(), 3, "дорожка + заполнение + ручка");
        s.set_value(0);
        out.clear();
        s.draw(&mut out);
        assert_eq!(out.len(), 2, "при нуле заполненной части нет");
    }

    // --- ScrollBar ---

    const ID_SCROLLBAR: WidgetId = 900;

    fn scrollbar_bounds() -> Box2D {
        Box2D {
            cx: 100.0,
            cy: 100.0,
            w: theme::SCROLLBAR_WIDTH,
            h: 200.0,
            rotation: 0.0,
        }
    }

    #[test]
    fn scrollbar_fits_content_gives_full_track_thumb() {
        let sb = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 10, 0);
        assert_eq!(sb.thumb_fraction, 1.0);
        assert_eq!(sb.thumb_offset, 0.0);
    }

    #[test]
    fn scrollbar_at_top_thumb_at_top() {
        let sb = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 30, 0);
        assert_eq!(sb.thumb_offset, 0.0);
        assert!((sb.thumb_fraction - 10.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn scrollbar_scrolled_to_max_thumb_at_bottom() {
        // total_rows=30, максимально валидный scroll — 29 (доккомент
        // `WindowPickerState`/`rebuild_window_picker`: «валиден в 0..=total_rows-1»).
        let sb = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 30, 29);
        assert!(
            (sb.thumb_offset - (1.0 - 10.0 / 30.0)).abs() < 1e-9,
            "ручка у самого низа дорожки: {}",
            sb.thumb_offset
        );
    }

    #[test]
    fn scrollbar_halfway_thumb_at_midpoint() {
        // total_rows=21 -> max_scroll=20, scroll=10 -> ровно середина.
        let sb = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 21, 10);
        let expected = 0.5 * (1.0 - 10.0 / 21.0);
        assert!((sb.thumb_offset - expected).abs() < 1e-9);
    }

    #[test]
    fn scrollbar_scroll_beyond_total_rows_clamps_like_max() {
        let over = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 30, 9999);
        let at_max = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 30, 29);
        assert_eq!(over.thumb_offset, at_max.thumb_offset);
    }

    #[test]
    fn scrollbar_draws_track_and_thumb() {
        let sb = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 30, 0);
        let mut out = Vec::new();
        sb.draw(&mut out);
        assert_eq!(out.len(), 2, "дорожка + ручка");
    }

    #[test]
    fn scrollbar_never_intercepts_pointer() {
        let sb = ScrollBar::new(ID_SCROLLBAR, scrollbar_bounds(), 10, 30, 5);
        assert!(!sb.hit_test((100.0, 100.0)), "не интерактивна");
    }

    /// Поле 1–100, центр (100, 50), ширина 48: левый край 76, текст с x = 80.
    fn field(value: u32) -> NumericField {
        let mut f = NumericField::opacity(ID_FIELD, 100.0, 50.0, 48.0);
        f.set_value(value);
        f
    }

    #[test]
    fn field_digits_append_and_clamp_on_enter() {
        let mut f = field(50);
        assert!(f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) }));
        assert!(f.has_focus());
        assert!(f.key_event(Key::Digit(5)), "\"50\" -> \"505\"");
        assert!(!f.key_event(Key::Digit(5)), "max_len = 3");
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted(), Some(100), "505 приведён к максимуму");
        assert!(!f.has_focus(), "Enter отпускает фокус");
    }

    #[test]
    fn field_caret_from_click_x() {
        let mut f = field(50);
        // Клик в левый край текста (x = 80) — каретка 0.
        f.pointer_event(PointerEvent::Down { pos: (80.0, 50.0) });
        f.key_event(Key::Digit(4));
        f.key_event(Key::Enter);
        assert_eq!(
            f.take_submitted(),
            Some(100),
            "\"450\" приведён к максимуму"
        );
    }

    #[test]
    fn field_backspace_arrows_and_min_clamp() {
        let mut f = field(50);
        // x = 98: rel = 18 от левого края текста — каретка за вторым символом.
        f.pointer_event(PointerEvent::Down { pos: (98.0, 50.0) });
        assert!(f.key_event(Key::ArrowLeft));
        assert!(f.key_event(Key::Backspace), "удалена пятёрка");
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted(), Some(1), "0 приведён к минимуму");
    }

    #[test]
    fn field_escape_reverts_to_original() {
        let mut f = field(50);
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Digit(9));
        f.key_event(Key::Escape);
        assert!(f.take_cancelled());
        assert_eq!(f.take_submitted(), None);
        assert!(!f.has_focus());
        // Текст вернулся к исходному «50».
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted(), Some(50));
    }

    #[test]
    fn field_blur_reverts_like_escape() {
        let mut f = field(50);
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Digit(9));
        f.on_blur();
        assert!(!f.has_focus());
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted(), Some(50));
    }

    #[test]
    fn field_paste_digits_only() {
        let mut f = field(50);
        assert!(!f.paste("12"), "вне фокуса вставка игнорируется");
        f.pointer_event(PointerEvent::Down { pos: (98.0, 50.0) });
        assert!(f.paste("a1b2c3"), "цифры вставлены, буквы отфильтрованы");
        f.key_event(Key::Enter);
        assert_eq!(
            f.take_submitted(),
            Some(100),
            "\"501\" приведён к максимуму"
        );
    }

    #[test]
    fn field_draw_caret_only_when_focused() {
        let mut f = field(50);
        let mut out = Vec::new();
        f.draw(&mut out);
        assert_eq!(out.len(), 3, "рамка + фон + текст");
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        out.clear();
        f.draw(&mut out);
        assert_eq!(out.len(), 4, "+ рисованная каретка");
        let Primitive::Fill { rect, .. } = out[3] else {
            panic!("каретка — Fill")
        };
        assert!(rect.w < 1.1 && rect.h > 8.0);
    }

    /// Чекбокс 16×16 с центром в (50, 50).
    fn checkbox(checked: bool) -> Checkbox {
        Checkbox::standard(ID_CHECK, 50.0, 50.0, checked)
    }

    #[test]
    fn checkbox_click_toggles_and_take_changed() {
        let mut c = checkbox(false);
        assert!(!c.checked());
        assert!(c.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) }));
        assert!(c.pointer_event(PointerEvent::Up { pos: (50.0, 50.0) }));
        assert_eq!(c.take_changed(), Some(true));
        assert_eq!(c.take_changed(), None, "переключение одноразовое");
        c.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) });
        c.pointer_event(PointerEvent::Up { pos: (50.0, 50.0) });
        assert_eq!(c.take_changed(), Some(false), "обратное переключение");
    }

    #[test]
    fn checkbox_press_inside_release_outside_no_toggle() {
        let mut c = checkbox(false);
        c.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) });
        c.pointer_event(PointerEvent::Up {
            pos: (200.0, 200.0),
        });
        assert_eq!(c.take_changed(), None);
        assert!(!c.checked());
    }

    #[test]
    fn checkbox_disabled_no_hit_test_and_no_toggle() {
        let mut c = checkbox(true);
        c.set_disabled(true);
        assert!(!c.hit_test((50.0, 50.0)), "disabled не хитуется");
        assert!(!c.pointer_event(PointerEvent::Down { pos: (50.0, 50.0) }));
        assert!(!c.pointer_event(PointerEvent::Up { pos: (50.0, 50.0) }));
        assert_eq!(c.take_changed(), None);
        assert!(c.checked(), "состояние не тронуто");
    }

    #[test]
    fn checkbox_set_checked_syncs_without_event() {
        let mut c = checkbox(false);
        c.set_checked(true);
        assert!(c.checked());
        assert_eq!(c.take_changed(), None, "внешняя синхронизация не событие");
    }

    #[test]
    fn checkbox_draw_primitives() {
        let mut c = checkbox(false);
        let mut out = Vec::new();
        c.draw(&mut out);
        assert_eq!(out.len(), 2, "рамка + фон");
        c.set_checked(true);
        out.clear();
        c.draw(&mut out);
        assert_eq!(out.len(), 4, "рамка + фон + два штриха галочки");
        let Primitive::Fill { rect, .. } = out[2] else {
            panic!("штрих галочки — Fill")
        };
        assert!(rect.rotation.abs() > 0.1, "штрих наклонён");
    }

    #[test]
    fn checkbox_hover_redraw_only_on_change() {
        let mut c = checkbox(false);
        assert!(c.set_hovered(true));
        assert!(!c.set_hovered(true));
        assert!(c.set_hovered(false));
    }

    #[test]
    fn panel_disabled_checkbox_click_lands_on_frame() {
        let mut p = Panel::new(0, rect(100.0, 100.0, 200.0, 100.0));
        let mut c = Checkbox::standard(ID_CHECK, 100.0, 100.0, true);
        c.set_disabled(true);
        p.add_widget(c);
        let r = p.pointer_event(PointerEvent::Down {
            pos: (100.0, 100.0),
        });
        assert!(r.consumed, "клик поглощён фоном панели");
        assert_eq!(
            p.widget_mut::<Checkbox>(ID_CHECK).unwrap().take_changed(),
            None,
            "disabled-виджет не получил событие"
        );
    }

    /// Панель (100, 100) 200×100 с двумя перекрывающимися кнопками:
    /// нижняя в (90, 100), верхняя в (110, 100); перекрытие x ∈ [96, 104].
    fn panel_with_two_buttons() -> Panel {
        let mut p = Panel::new(0, rect(100.0, 100.0, 200.0, 100.0));
        p.add_widget(Button::icon(ID_BTN, 90.0, 100.0, Icon::Eye));
        p.add_widget(Button::icon(ID_BTN + 10, 110.0, 100.0, Icon::Delete));
        p
    }

    #[test]
    fn panel_hit_top_widget_wins() {
        let mut p = panel_with_two_buttons();
        p.pointer_event(PointerEvent::Down {
            pos: (100.0, 100.0),
        });
        p.pointer_event(PointerEvent::Up {
            pos: (100.0, 100.0),
        });
        assert!(
            p.widget_mut::<Button>(ID_BTN + 10).unwrap().take_click(),
            "верхняя кнопка"
        );
        assert!(
            !p.widget_mut::<Button>(ID_BTN).unwrap().take_click(),
            "нижняя не получила"
        );
    }

    #[test]
    fn panel_click_frame_consumed_but_not_action() {
        let mut p = panel_with_two_buttons();
        let r = p.pointer_event(PointerEvent::Down {
            pos: (195.0, 145.0),
        });
        assert!(r.consumed, "клик по фону панели поглощён");
        assert!(
            !p.key_event(Key::Digit(5)).consumed,
            "без фокуса клавиши — ядру"
        );
    }

    #[test]
    fn panel_outside_click_not_consumed() {
        let mut p = panel_with_two_buttons();
        let r = p.pointer_event(PointerEvent::Down {
            pos: (500.0, 500.0),
        });
        assert_eq!(r, EventResult::default());
    }

    #[test]
    fn panel_focus_routes_keys_and_releases_on_enter() {
        let mut p = Panel::new(0, rect(100.0, 100.0, 200.0, 100.0));
        p.add_widget(NumericField::opacity(ID_FIELD, 100.0, 100.0, 48.0));
        p.pointer_event(PointerEvent::Down {
            pos: (100.0, 100.0),
        });
        assert_eq!(p.focused_widget(), Some(ID_FIELD));
        assert!(
            p.key_event(Key::Digit(5)).consumed,
            "цифра перехвачена полем"
        );
        p.key_event(Key::Enter);
        assert_eq!(p.focused_widget(), None, "Enter отпустил фокус");
        assert!(!p.key_event(Key::Digit(5)).consumed);
        assert_eq!(
            p.widget_mut::<NumericField>(ID_FIELD)
                .unwrap()
                .take_submitted(),
            Some(100)
        );
    }

    #[test]
    fn panel_blur_on_frame_click_reverts_field() {
        let mut p = Panel::new(0, rect(100.0, 100.0, 200.0, 100.0));
        p.add_widget(NumericField::opacity(ID_FIELD, 100.0, 100.0, 48.0));
        p.pointer_event(PointerEvent::Down {
            pos: (100.0, 100.0),
        });
        p.key_event(Key::Backspace);
        p.key_event(Key::Backspace);
        p.pointer_event(PointerEvent::Down {
            pos: (195.0, 145.0),
        });
        assert_eq!(p.focused_widget(), None);
        // Поле отменилось до «100»: повторный фокус + Enter.
        p.pointer_event(PointerEvent::Down {
            pos: (100.0, 100.0),
        });
        p.key_event(Key::Enter);
        assert_eq!(
            p.widget_mut::<NumericField>(ID_FIELD)
                .unwrap()
                .take_submitted(),
            Some(100)
        );
    }

    #[test]
    fn panel_capture_keeps_dragging_outside() {
        let mut p = Panel::new(0, rect(100.0, 100.0, 200.0, 100.0));
        p.add_widget(Slider::opacity(ID_SLIDER, 100.0, 100.0, 112.0));
        p.pointer_event(PointerEvent::Down {
            pos: (100.0, 100.0),
        });
        let r = p.pointer_event(PointerEvent::Move {
            pos: (1000.0, 500.0),
        });
        assert!(r.consumed, "захват: событие у ползунка даже за пределами");
        assert_eq!(p.widget::<Slider>(ID_SLIDER).unwrap().value(), 100);
        p.pointer_event(PointerEvent::Up {
            pos: (1000.0, 500.0),
        });
    }

    #[test]
    fn panel_hover_redraws_on_change() {
        let mut p = panel_with_two_buttons();
        assert!(
            p.pointer_event(PointerEvent::Move { pos: (90.0, 100.0) })
                .redraw,
            "hover вошёл"
        );
        assert!(
            p.pointer_event(PointerEvent::Move {
                pos: (500.0, 500.0)
            })
            .redraw,
            "hover вышел"
        );
    }

    #[test]
    fn panel_translate_moves_frame_and_widgets() {
        let mut p = panel_with_two_buttons();
        p.translate(10.0, 20.0);
        assert!(p.hit_test((209.0, 169.0)));
        assert!(!p.hit_test((5.0, 55.0)));
        p.pointer_event(PointerEvent::Down {
            pos: (120.0, 120.0),
        });
        p.pointer_event(PointerEvent::Up {
            pos: (120.0, 120.0),
        });
        assert!(
            p.widget_mut::<Button>(ID_BTN + 10).unwrap().take_click(),
            "кнопка переехала"
        );
    }

    #[test]
    fn panel_draw_background_then_widgets_in_order() {
        let p = panel_with_two_buttons();
        let mut out = Vec::new();
        p.draw(&mut out);
        assert_eq!(out.len(), 2 + 2 + 2, "фон ×2 + две кнопки ×2");
        assert!(matches!(out[0], Primitive::Fill { .. }));
        assert!(
            matches!(out[2], Primitive::Fill { .. }),
            "первая кнопка ниже второй"
        );
    }

    #[test]
    fn panel_widget_lookup_by_id_and_type() {
        let p = panel_with_two_buttons();
        assert!(p.widget::<Button>(ID_BTN).is_some());
        assert!(p.widget::<Slider>(ID_BTN).is_none(), "тип не совпал");
        assert!(p.widget::<Button>(999).is_none(), "id не найден");
    }

    // --- Индикатор interact-lock (SPEC «закрепление окон») ---

    /// Бейдж-булавка занимает крайний левый слот верхнего угла, замок —
    /// следующий за ним (иначе они рисовались бы друг поверх друга).
    #[test]
    fn pin_badge_takes_first_slot_and_lock_follows() {
        let win = rect(200.0, 150.0, 100.0, 60.0);
        let pin = pin_indicator(win);
        let lock = lock_indicator(win);
        let (Primitive::Fill { rect: pin_rect, .. }, Primitive::Fill { rect: lock_rect, .. }) =
            (&pin[0], &lock[0])
        else {
            panic!("первый примитив каждого бейджа — Fill");
        };
        assert_eq!(
            pin_rect.cx,
            200.0 - 50.0 + theme::LOCK_INDICATOR_MARGIN + theme::LOCK_INDICATOR_SIZE / 2.0,
            "булавка прижата к левому верхнему углу окна"
        );
        assert_eq!(pin_rect.cy, lock_rect.cy, "бейджи в одном ряду");
        assert_eq!(
            lock_rect.cx - pin_rect.cx,
            theme::LOCK_INDICATOR_SIZE + theme::LOCK_INDICATOR_MARGIN,
            "замок стоит следующим слотом, без наложения"
        );
        assert!(matches!(pin[1], Primitive::Icon { icon: Icon::Pinned, .. }));
    }

    #[test]
    fn lock_indicator_badge_sits_in_top_left_corner() {
        let win = rect(200.0, 150.0, 100.0, 60.0);
        let prims = lock_indicator(win);
        assert_eq!(prims.len(), 2, "фон-бейдж + иконка");
        let badge = Box2D {
            cx: 200.0 - 50.0
                + theme::LOCK_INDICATOR_MARGIN
                + theme::LOCK_INDICATOR_SIZE / 2.0
                // слот 1: слева от замка стоит булавка «окно закреплено»
                + theme::LOCK_INDICATOR_SIZE
                + theme::LOCK_INDICATOR_MARGIN,
            cy: 150.0 - 30.0 + theme::LOCK_INDICATOR_MARGIN + theme::LOCK_INDICATOR_SIZE / 2.0,
            w: theme::LOCK_INDICATOR_SIZE,
            h: theme::LOCK_INDICATOR_SIZE,
            rotation: 0.0,
        };
        match &prims[0] {
            Primitive::Fill {
                rect,
                color,
                opacity,
            } => {
                assert_eq!(*rect, badge);
                assert_eq!(*color, theme::LOCK_INDICATOR_BG);
                assert_eq!(*opacity, theme::LOCK_INDICATOR_OPACITY);
            }
            other => panic!("первый примитив — Fill, не {other:?}"),
        }
        match &prims[1] {
            Primitive::Icon {
                rect,
                icon,
                opacity,
            } => {
                assert_eq!(*rect, badge);
                assert_eq!(*icon, Icon::Lock);
                assert_eq!(*opacity, theme::LOCK_INDICATOR_OPACITY);
            }
            other => panic!("второй примитив — Icon, не {other:?}"),
        }
    }

    #[test]
    fn lock_indicator_light_treatment_not_full_coverage() {
        let win = rect(0.0, 0.0, 400.0, 300.0);
        let prims = lock_indicator(win);
        assert_eq!(prims.len(), 2, "только бейдж + иконка, окно не заливается");
        for p in &prims {
            let rect = match p {
                Primitive::Fill { rect, .. } | Primitive::Icon { rect, .. } => *rect,
                other => panic!("только Fill/Icon, не {other:?}"),
            };
            let area = rect.w * rect.h;
            assert!(
                area < win.w * win.h / 2.0,
                "бейдж в углу, не на всё окно: {area} >= {}",
                win.w * win.h / 2.0
            );
        }
    }

    #[test]
    fn lock_indicator_clamps_to_tiny_windows() {
        let win = rect(0.0, 0.0, 8.0, 200.0);
        let prims = lock_indicator(win);
        let Primitive::Fill { rect, .. } = &prims[0] else {
            panic!("бейдж — Fill")
        };
        assert_eq!(rect.w, 8.0, "бейдж не больше окна по ширине");
        assert_eq!(rect.h, 8.0);
    }

    // --- TextField ---

    const ID_TEXT: WidgetId = 5;

    fn text_field(text: &str) -> TextField {
        TextField::with_placeholder(
            ID_TEXT,
            rect(100.0, 50.0, 120.0, theme::FIELD_HEIGHT),
            text,
            12,
            "процесс",
        )
    }

    #[test]
    fn text_field_chars_submit_and_take() {
        let mut f = text_field("chrome");
        assert!(f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) }));
        assert!(f.has_focus());
        assert!(f.key_event(Key::Char('_')));
        assert!(f.key_event(Key::Char('1')));
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("chrome_1"));
        assert_eq!(f.take_submitted(), None, "событие одноразовое");
        assert!(!f.has_focus(), "Enter отпускает фокус");
    }

    #[test]
    fn text_field_digit_key_also_types() {
        let mut f = text_field("");
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        assert!(f.key_event(Key::Digit(7)));
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("7"));
    }

    #[test]
    fn text_field_max_len_limits_input() {
        let mut f = TextField::new(ID_TEXT, rect(0.0, 0.0, 60.0, 22.0), "", 3);
        f.pointer_event(PointerEvent::Down { pos: (0.0, 0.0) });
        assert!(f.key_event(Key::Char('a')));
        assert!(f.key_event(Key::Char('b')));
        assert!(f.key_event(Key::Char('c')));
        assert!(!f.key_event(Key::Char('d')), "max_len = 3");
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("abc"));
    }

    #[test]
    fn text_field_backspace_arrows_and_caret_from_click() {
        let mut f = text_field("abcd");
        // Поле: левый край 40, текст с x = 44. Клик в x = 47 — каретка перед
        // первым символом (rel = 3, ровно середина «разрыва» перед 'a').
        f.pointer_event(PointerEvent::Down { pos: (47.0, 50.0) });
        assert!(f.key_event(Key::ArrowRight));
        assert!(f.key_event(Key::Backspace), "удалён первый символ");
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("bcd"));
    }

    #[test]
    fn text_field_caret_is_char_based_for_cyrillic() {
        let mut f = text_field("окна");
        // Клик в правый край поля (x = 159) — каретка в конец, за кириллицу.
        f.pointer_event(PointerEvent::Down { pos: (159.0, 50.0) });
        assert!(f.key_event(Key::Char('!')));
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("окна!"));
    }

    #[test]
    fn text_field_escape_reverts_to_original() {
        let mut f = text_field("chrome");
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Char('x'));
        f.key_event(Key::Escape);
        assert!(f.take_cancelled());
        assert_eq!(f.take_submitted(), None);
        assert!(!f.has_focus());
        assert_eq!(f.text(), "chrome", "текст вернулся к исходному");
    }

    #[test]
    fn text_field_blur_reverts_like_escape() {
        let mut f = text_field("chrome");
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Char('x'));
        f.on_blur();
        assert!(!f.has_focus());
        assert_eq!(f.text(), "chrome");
    }

    #[test]
    fn text_field_paste_filters_control_chars() {
        let mut f = text_field("");
        assert!(!f.paste("ab"), "вне фокуса вставка игнорируется");
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        assert!(f.paste("a\nb\tc"), "управляющие символы отфильтрованы");
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("abc"));
    }

    #[test]
    fn text_field_set_text_syncs_without_event() {
        let mut f = text_field("old");
        f.set_text("new value");
        assert_eq!(f.text(), "new value");
        assert_eq!(f.take_submitted(), None, "внешняя синхронизация не событие");
        // В фокусе set_text не трогает редактируемый буфер.
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        f.key_event(Key::Char('!'));
        f.set_text("ignored");
        f.key_event(Key::Enter);
        assert_eq!(f.take_submitted().as_deref(), Some("new value!"));
    }

    #[test]
    fn text_field_draw_placeholder_when_empty() {
        let f = text_field("");
        let mut out = Vec::new();
        f.draw(&mut out);
        assert_eq!(out.len(), 3, "рамка + фон + плейсхолдер");
        let Primitive::Text { text, opacity, .. } = &out[2] else {
            panic!("третий примитив — Text")
        };
        assert_eq!(text, "процесс");
        assert!(*opacity < 1.0, "плейсхолдер приглушён");
    }

    #[test]
    fn text_field_draw_caret_only_when_focused() {
        let mut f = text_field("text");
        let mut out = Vec::new();
        f.draw(&mut out);
        assert_eq!(out.len(), 3, "рамка + фон + текст");
        f.pointer_event(PointerEvent::Down { pos: (100.0, 50.0) });
        out.clear();
        f.draw(&mut out);
        assert_eq!(out.len(), 4, "+ рисованная каретка");
        let Primitive::Fill { rect, .. } = out[3] else {
            panic!("каретка — Fill")
        };
        assert!(rect.w < 1.1 && rect.h > 8.0);
    }

    // --- Label ---

    const ID_LABEL: WidgetId = 6;

    #[test]
    fn label_draws_text_and_never_hit() {
        let l = Label::new(ID_LABEL, 50.0, 30.0, "Правила");
        let mut out = Vec::new();
        l.draw(&mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            Primitive::Text { text, opacity, .. } if text == "Правила" && *opacity == 1.0
        ));
        assert!(!l.hit_test((50.0, 30.0)), "надпись не интерактивна");
    }

    #[test]
    fn label_geometry_from_left_edge_and_dim() {
        let mut l = Label::new(ID_LABEL, 50.0, 30.0, "ok");
        let (tw, th) = text::text_size("ok");
        assert_eq!(
            l.bounds(),
            Box2D {
                cx: 50.0 + tw / 2.0,
                cy: 30.0,
                w: tw,
                h: th,
                rotation: 0.0
            }
        );
        l.set_dim(true);
        let mut out = Vec::new();
        l.draw(&mut out);
        let Primitive::Text { opacity, .. } = &out[0] else {
            panic!("Text")
        };
        assert_eq!(*opacity, 0.5, "приглушённая подпись");
    }

    // --- Панель свойств закреплённого окна ---

    fn pinned_frame() -> Box2D {
        rect(200.0, 200.0, PINNED_PANEL_WIDTH, PINNED_PANEL_HEIGHT)
    }

    fn pinned_lock_frame() -> Box2D {
        rect(200.0, 200.0, PINNED_PANEL_WIDTH, PINNED_LOCK_PANEL_HEIGHT)
    }

    fn rule(process: &str, title: &str) -> OverlapRule {
        OverlapRule {
            process_name: Some(process.to_string()),
            title_pattern: Some(title.to_string()),
        }
    }

    #[test]
    fn pinned_row_id_roundtrip() {
        for (i, field) in [
            (0usize, PinnedRowField::Remove),
            (0, PinnedRowField::ProcessName),
            (0, PinnedRowField::TitlePattern),
            (7, PinnedRowField::ProcessName),
            (4095, PinnedRowField::TitlePattern),
        ] {
            let id = pinned_row_id(i, field);
            assert_eq!(decode_pinned_row_id(id), Some((i, field)));
        }
    }

    #[test]
    fn pinned_row_id_decode_rejects_panel_ids() {
        for id in [PINNED_PANEL_ID, PINNED_CHECK_MOVE_LOCK, PINNED_BTN_UNPIN, 0] {
            assert_eq!(decode_pinned_row_id(id), None, "id {id}");
        }
    }

    #[test]
    fn pinned_panel_seeds_locks_and_unpin_click() {
        let mut p = build_pinned_panel(&[], true, false, 0, pinned_frame()).panel;
        assert!(p.widget::<Checkbox>(PINNED_CHECK_MOVE_LOCK).unwrap().checked());
        assert!(!p
            .widget::<Checkbox>(PINNED_CHECK_INTERACT_LOCK)
            .unwrap()
            .checked());
        // Кнопка «Открепить» — внизу по центру панели.
        let unpin_b = p.widget::<Button>(PINNED_BTN_UNPIN).unwrap().bounds();
        let click = (unpin_b.cx, unpin_b.cy);
        p.pointer_event(PointerEvent::Down { pos: click });
        p.pointer_event(PointerEvent::Up { pos: click });
        assert!(
            p.widget_mut::<Button>(PINNED_BTN_UNPIN).unwrap().take_click(),
            "«Открепить» кликабельна"
        );
    }

    #[test]
    fn pinned_panel_seeds_rule_fields_from_overlap_rule() {
        let p = build_pinned_panel(&[rule("chrome", "нет*")], false, false, 0, pinned_frame())
            .panel;
        let process = p
            .widget::<TextField>(pinned_row_id(0, PinnedRowField::ProcessName))
            .unwrap();
        assert_eq!(process.text(), "chrome");
        let title = p
            .widget::<TextField>(pinned_row_id(0, PinnedRowField::TitlePattern))
            .unwrap();
        assert_eq!(title.text(), "нет*");
        // Пустые поля правила: плейсхолдеры подставлены, текст пуст.
        let p = build_pinned_panel(
            &[OverlapRule {
                process_name: None,
                title_pattern: None,
            }],
            false,
            false,
            0,
            pinned_frame(),
        )
        .panel;
        assert_eq!(
            p.widget::<TextField>(pinned_row_id(0, PinnedRowField::ProcessName))
                .unwrap()
                .text(),
            ""
        );
    }

    #[test]
    fn pinned_panel_remove_button_click_identifiable_by_id() {
        let mut p =
            build_pinned_panel(&[rule("a", "b"), rule("c", "d")], false, false, 0, pinned_frame())
                .panel;
        let id = pinned_row_id(1, PinnedRowField::Remove);
        let b = p.widget::<Button>(id).unwrap().bounds();
        p.pointer_event(PointerEvent::Down { pos: (b.cx, b.cy) });
        p.pointer_event(PointerEvent::Up { pos: (b.cx, b.cy) });
        assert!(
            p.widget_mut::<Button>(id).unwrap().take_click(),
            "клик по «удалить правило» второй строки"
        );
    }

    #[test]
    fn pinned_panel_scrollbar_only_when_rules_overflow() {
        let p = build_pinned_panel(
            &(0..PINNED_VISIBLE_RULES).map(|_| rule("p", "t")).collect::<Vec<_>>(),
            false,
            false,
            0,
            pinned_frame(),
        );
        assert_eq!(p.total_rows, PINNED_VISIBLE_RULES);
        assert!(
            p.panel.widget::<ScrollBar>(PINNED_SCROLLBAR_ID).is_none(),
            "ровно видимое число правил — скролла нет"
        );
        let p = build_pinned_panel(
            &(0..PINNED_VISIBLE_RULES + 3).map(|_| rule("p", "t")).collect::<Vec<_>>(),
            false,
            false,
            0,
            pinned_frame(),
        );
        assert_eq!(p.total_rows, PINNED_VISIBLE_RULES + 3);
        assert!(
            p.panel.widget::<ScrollBar>(PINNED_SCROLLBAR_ID).is_some(),
            "правил больше видимых строк — скролл есть"
        );
    }

    #[test]
    fn pinned_panel_virtualizes_rows_by_scroll() {
        let rules: Vec<OverlapRule> = (0..10)
            .map(|i| rule(&format!("p{i}"), &format!("t{i}")))
            .collect();
        let p = build_pinned_panel(&rules, false, false, 2, pinned_frame());
        assert_eq!(p.total_rows, 10);
        // Строка 1 (невидимая) не собрана, строка 2 (первая видимая) — есть.
        assert!(
            p.panel
                .widget::<TextField>(pinned_row_id(1, PinnedRowField::ProcessName))
                .is_none()
        );
        assert_eq!(
            p.panel
                .widget::<TextField>(pinned_row_id(2, PinnedRowField::ProcessName))
                .unwrap()
                .text(),
            "p2"
        );
        // Последняя видимая — индекс 2 + 4 - 1 = 5.
        assert_eq!(
            p.panel
                .widget::<TextField>(pinned_row_id(5, PinnedRowField::TitlePattern))
                .unwrap()
                .text(),
            "t5"
        );
        assert!(
            p.panel
                .widget::<TextField>(pinned_row_id(6, PinnedRowField::ProcessName))
                .is_none(),
            "строка 6 уже за окном скролла"
        );
    }

    #[test]
    fn pinned_panel_add_rule_button_click() {
        let mut p = build_pinned_panel(&[], false, false, 0, pinned_frame()).panel;
        let b = p.widget::<Button>(PINNED_BTN_ADD_RULE).unwrap().bounds();
        p.pointer_event(PointerEvent::Down { pos: (b.cx, b.cy) });
        p.pointer_event(PointerEvent::Up { pos: (b.cx, b.cy) });
        assert!(p
            .widget_mut::<Button>(PINNED_BTN_ADD_RULE)
            .unwrap()
            .take_click());
    }

    /// Иконка-переключатель замка рисует иконку [`Icon::Lock`] в состоянии
    /// «заблокировано» и [`Icon::LockOpen`] в «свободно» — состояние панели
    /// читается пиктограммой, а не квадратом с галочкой.
    #[test]
    fn pinned_panel_lock_toggles_draw_lock_icons() {
        for (id, checked, expect) in [
            (PINNED_CHECK_MOVE_LOCK, true, Icon::Lock),
            (PINNED_CHECK_MOVE_LOCK, false, Icon::LockOpen),
            (PINNED_CHECK_INTERACT_LOCK, true, Icon::Lock),
            (PINNED_CHECK_INTERACT_LOCK, false, Icon::LockOpen),
        ] {
            let p = build_pinned_panel(&[], checked, checked, 0, pinned_frame()).panel;
            let mut prims = Vec::new();
            p.widget::<Checkbox>(id).unwrap().draw(&mut prims);
            let mut seen_icon = false;
            for prim in prims {
                if let Primitive::Icon { icon, .. } = prim {
                    seen_icon = true;
                    assert_eq!(icon, expect, "id {id} checked {checked}");
                }
            }
            assert!(seen_icon, "id {id} checked {checked}: иконка не нарисована");
        }
    }

    /// Переключатель замка — кнопка [`theme::BUTTON_SIZE`] (не маленький
    /// квадрат чекбокса): клик/переключение по центру кнопки работает.
    #[test]
    fn pinned_panel_lock_toggle_clicks_like_checkbox() {
        let mut p = build_pinned_panel(&[], false, false, 0, pinned_frame()).panel;
        let b = p.widget::<Checkbox>(PINNED_CHECK_MOVE_LOCK).unwrap().bounds();
        assert_eq!(b.w, theme::BUTTON_SIZE, "иконка-кнопка размера тулбара");
        p.pointer_event(PointerEvent::Down { pos: (b.cx, b.cy) });
        p.pointer_event(PointerEvent::Up { pos: (b.cx, b.cy) });
        assert_eq!(
            p.widget_mut::<Checkbox>(PINNED_CHECK_MOVE_LOCK)
                .unwrap()
                .take_changed(),
            Some(true),
            "клик по иконке-замку переключает move-lock"
        );
    }

    /// Разделитель секций собран и не интерактивен (клики сквозь него —
    /// как у [`Label`]).
    #[test]
    fn pinned_panel_has_non_interactive_rules_divider() {
        let p = build_pinned_panel(&[], false, false, 0, pinned_frame()).panel;
        let d = p.widget::<Divider>(PINNED_DIVIDER_RULES).unwrap();
        assert!(
            !d.hit_test((d.bounds().cx, d.bounds().cy)),
            "разделитель не потребляет клики"
        );
        assert!(d.bounds().w > 0.0, "разделитель тянется через контент");
    }

    /// Плейсхолдеры полей правил поясняют назначение полей и влезают
    /// в ширину поля (текст не клиппится конвейером примитивов — длинный
    /// плейсхолдер иначе рисовался бы за рамкой поля).
    #[test]
    fn pinned_panel_rule_placeholders_fit_field_width() {
        let rules = [OverlapRule {
            process_name: None,
            title_pattern: None,
        }];
        let p = build_pinned_panel(&rules, false, false, 0, pinned_frame()).panel;
        let process = p
            .widget::<TextField>(pinned_row_id(0, PinnedRowField::ProcessName))
            .unwrap();
        let avail = process.bounds().w - 2.0 * theme::FIELD_PAD;
        for label in ["процесс chrome.exe", "маска заголовка окна"] {
            let (tw, _) = text::text_size(label);
            assert!(
                tw <= avail,
                "плейсхолдер {label:?} ({tw} DIP) шире поля ({avail} DIP)"
            );
        }
    }

    // --- Урезанная панель свойств закреплённого окна (build_pinned_lock_panel) ---

    /// Та же иконка-семантика, что у `pinned_panel_lock_toggles_draw_lock_icons`,
    /// но для урезанной панели: `Icon::Lock` в состоянии «заблокировано»,
    /// `Icon::LockOpen` в «свободно».
    #[test]
    fn pinned_lock_panel_toggles_draw_lock_icons() {
        for (id, checked, expect) in [
            (PINNED_CHECK_MOVE_LOCK, true, Icon::Lock),
            (PINNED_CHECK_MOVE_LOCK, false, Icon::LockOpen),
            (PINNED_CHECK_INTERACT_LOCK, true, Icon::Lock),
            (PINNED_CHECK_INTERACT_LOCK, false, Icon::LockOpen),
        ] {
            let p = build_pinned_lock_panel(checked, checked, &[], pinned_lock_frame());
            let mut prims = Vec::new();
            p.widget::<Checkbox>(id).unwrap().draw(&mut prims);
            let mut seen_icon = false;
            for prim in prims {
                if let Primitive::Icon { icon, .. } = prim {
                    seen_icon = true;
                    assert_eq!(icon, expect, "id {id} checked {checked}");
                }
            }
            assert!(seen_icon, "id {id} checked {checked}: иконка не нарисована");
        }
    }

    /// Клик по переключателю замка урезанной панели работает как обычный
    /// `Checkbox` (тот же контракт, что `pinned_panel_lock_toggle_clicks_like_checkbox`).
    #[test]
    fn pinned_lock_panel_toggle_clicks_like_checkbox() {
        let mut p = build_pinned_lock_panel(false, false, &[], pinned_lock_frame());
        let b = p.widget::<Checkbox>(PINNED_CHECK_MOVE_LOCK).unwrap().bounds();
        assert_eq!(b.w, theme::BUTTON_SIZE, "иконка-кнопка размера тулбара");
        p.pointer_event(PointerEvent::Down { pos: (b.cx, b.cy) });
        p.pointer_event(PointerEvent::Up { pos: (b.cx, b.cy) });
        assert_eq!(
            p.widget_mut::<Checkbox>(PINNED_CHECK_MOVE_LOCK)
                .unwrap()
                .take_changed(),
            Some(true),
            "клик по иконке-замку переключает move-lock"
        );
    }

    /// Кнопка «Открепить» присутствует и растянута на всю ширину контента
    /// (та же раскладка, что у полной панели).
    #[test]
    fn pinned_lock_panel_has_unpin_button() {
        let frame = pinned_lock_frame();
        let p = build_pinned_lock_panel(false, false, &[], frame);
        let b = p.widget::<Button>(PINNED_BTN_UNPIN).unwrap().bounds();
        assert_eq!(
            b.w,
            (frame.w - 2.0 * PINNED_PAD).max(0.0),
            "«Открепить» растянута на ширину контента"
        );
        assert!(
            b.cy > frame.cy,
            "«Открепить» в нижней части урезанной панели"
        );
    }

    /// Раздел правил соседства/z-order отсутствует в урезанной панели —
    /// решение пользователя 2026-08-18 держит его вне UI, `build_pinned_panel`
    /// (с разделом) остаётся нетронутым для будущего возврата.
    #[test]
    fn pinned_lock_panel_omits_neighbor_rule_widgets() {
        let p = build_pinned_lock_panel(false, false, &[], pinned_lock_frame());
        assert!(
            p.widget::<Label>(PINNED_LABEL_RULES).is_none(),
            "заголовок «Соседние окна» не должен строиться"
        );
        assert!(
            p.widget::<Button>(PINNED_BTN_ADD_RULE).is_none(),
            "кнопка «Добавить правило» не должна строиться"
        );
        assert!(
            p.widget::<ScrollBar>(PINNED_SCROLLBAR_ID).is_none(),
            "полоса скролла списка правил не должна строиться"
        );
    }
}
