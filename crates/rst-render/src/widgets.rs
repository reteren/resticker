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
/// слоя). Набор — по SPEC 3.6 (тулбар) и 3.8 (панель у курсора).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl Icon {
    /// Все варианты в порядке объявления — для предварительной генерации
    /// кэша иконок (текс-карта `HashMap<Icon, Texture>`, M2_WIRING_PLAN §3)
    /// и тестов генератора `icon_rgba`.
    pub const ALL: [Icon; 14] = [
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
    /// Строка текста: растрируется [`crate::text::rasterize`] в текстуру;
    /// `rect` — область назначения (её размер совпадает с [`text::text_size`]).
    Text {
        rect: Box2D,
        text: String,
        color: [u8; 3],
        opacity: f64,
    },
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
    /// Цифра 0–9.
    Digit(u8),
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
    hovered: bool,
    /// Нажата (указатель зажат внутри), клик ещё не свершился.
    armed: bool,
    clicked: bool,
}

impl Button {
    /// Кнопка с содержимым в прямоугольнике `bounds` (DIP).
    pub fn new(id: WidgetId, bounds: Box2D, content: ButtonContent) -> Self {
        Self {
            id,
            bounds,
            content,
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
        let bg = if self.armed {
            theme::BUTTON_BG_ARMED
        } else if self.hovered {
            theme::BUTTON_BG_HOVER
        } else {
            theme::BUTTON_BG
        };
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: bg,
            opacity: 1.0,
        });
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
            ButtonContent::Label(label) => {
                out.push(Primitive::Text {
                    rect: content_rect,
                    text: label.clone(),
                    color: theme::TEXT,
                    opacity: 1.0,
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
            widgets: Vec::new(),
            focus: None,
            capture: None,
            hovered: None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    const ID_BTN: WidgetId = 1;
    const ID_SLIDER: WidgetId = 2;
    const ID_FIELD: WidgetId = 3;

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
}
