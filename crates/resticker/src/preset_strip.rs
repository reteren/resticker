//! Лента раскладок тайлинга (T10) — верхняя часть меню редактирования
//! групп: горизонтальная лента миниатюр-схем, по одной на каждую раскладку.
//!
//! Каждая миниатюра — это экран, разбитый на прямоугольные слоты, и в
//! каждом слоте нарисована его цифра. Главное правило читается прямо с
//! ленты: окно, отмеченное первым в ленте карточек (`group_strip`), попадёт
//! в слот 1. Справа от ленты — поле величины зазора между окнами (проценты,
//! `NumericField::snap_gap`).
//!
//! Невлезающая раскладка помечается красным кольцом на миниатюре: у окна
//! есть собственный минимальный размер (`WM_GETMINMAXINFO`), и слот меньше
//! него окно молча игнорирует, оставаясь крупнее и накрывая соседа (живой
//! репорт 2026-08-26: «маленькие окна все время налазят друг на друга»).
//! Вердикты считает НЕ лента — это чистая геометрия в rst-core, которую
//! применяет координатор; сюда приходит готовый срез [`ThumbFit`], и лента
//! только рисует. Кольцо совещательное: выбрать такую раскладку можно —
//! пользователь вправе получить перекрытие сознательно.
//!
//! Чистый строитель, как `preset_picker`/`group_strip`: на вход — ГОТОВЫЙ
//! срез раскладок ([`group_layout::Preset`]), индекс выбранной, прямоугольник
//! экрана в DIP и текущий зазор; на выход — [`Panel`] и геометрия для
//! координатора ([`StripBuild`]). Никакого Win32 и никакого состояния.
//!
//! Лента НЕ выводит раскладки из числа отмеченных окон: с переходом на
//! генератор раскладок набор вариантов считается по минимальным размерам
//! окон (сколько полос по каждой оси влезает в рабочую область), и это не
//! функция одного лишь числа окон. Лента рисует ровно то, что ей дали, —
//! откуда срез взялся, её не касается.
//!
//! Первая раскладка среза может быть «adaptive» — сочинена под конкретный
//! набор окон (их минимумы и рабочую область), а не взята из семейства.
//! Такая карточка подписывается словом `adaptive` ПОД миниатюрой: силуэт
//! слотов занимает всю карточку, и текст поверх него спрятал бы ровно то,
//! что пользователь сравнивает. Признак — отдельный флаг в [`build`],
//! индексы он не двигает: выбранная раскладка, вердикт и id кликов
//! привязаны к индексу в срезе как обычно, флаг лишь рисует подпись.
//!
//! Клики и перетаскивание: слоты — обычные кнопки [`Button`], клик по
//! любому месту миниатюры выбирает раскладку (кнопка-подложка ловит клики
//! по свободным углам раскладок «главное по центру»). Прямоугольники
//! слотов дополнительно отдаются наружу ([`StripBuild::slots`]) — по ним
//! координатор хитует перетаскивание окна из ленты карточек в конкретный
//! слот, это отдельный канал, не через кнопки.
//!
//! Ужатие вместо прокрутки (в отличие от ленты карточек `group_strip`):
//! раскладок немного (от нуля до семи), и пользователь обязан видеть все
//! варианты сразу — выбор раскладки это сравнение силуэтов, а прокрутка
//! скрыла бы половину выбора; размер миниатюр считается по ФАКТИЧЕСКОМУ
//! числу раскладок в срезе, а не по потолку. Миниатюры квадратные, а не в
//! пропорции экрана: схемы читаются по форме слотов, и высота квадрата
//! использует место эффективнее, чем низкая полоска 16:9.
//!
//! Модуль пока не подключён к координатору (подключение — отдельная задача
//! координатора, в `main.rs` он добавит `mod preset_strip;` сам), поэтому
//! `#![allow(dead_code)]` — тот же приём, что у `group_strip.rs`; атрибут
//! снять при подключении.

#![allow(dead_code)]

use rst_core::group_layout::{Preset, UnitRect};
use rst_core::hittest::DipRect;
use rst_render::{
    Box2D, Button, ButtonContent, Label, NumericField, Panel, Primitive, Widget, WidgetId,
    WidgetStyle, text_size, theme,
};

/// Идентификатор панели ленты. Диапазон 800+ свободен: лента карточек 600+,
/// панель зазора 700+, баннер и бейджи мониторов 900+.
pub const STRIP_PANEL_ID: WidgetId = 800;
/// Кнопка-подложка миниатюры: `THUMB_BASE + индекс раскладки` — клик по
/// пустому месту миниатюры выбирает раскладку (тот же контракт
/// декодирования индекса из id, что `preset_picker::ROW_BASE`).
pub const THUMB_BASE: WidgetId = 801;
/// Кнопка слота: `SLOT_BASE + preset * MAX_SLOTS + slot`. Координатор
/// декодирует `preset = (id - SLOT_BASE) / MAX_SLOTS`,
/// `slot = (id - SLOT_BASE) % MAX_SLOTS`; `slot` считается с нуля, как
/// индекс в `Preset::slots` (слот номер `slot + 1`).
pub const SLOT_BASE: WidgetId = 810;
/// Шаг кодирования id слота — максимальное число слотов в группе.
pub const MAX_SLOTS: usize = 8;

/// Поле величины зазора (`NumericField`, проценты 0..=35). Текущее значение
/// читается `panel.widget::<NumericField>(GAP_FIELD_ID).value()`. Стоит выше
/// потолка диапазона слотов (`SLOT_BASE + 7 * MAX_SLOTS`): иначе id поля
/// совпал бы с id слота, и `Panel::widget` находит ПЕРВЫЙ виджет с id —
/// слот, а не поле.
pub const GAP_FIELD_ID: WidgetId = 880;
/// Флаг для id подложки миниатюры — не декодируется (тот же приём, что
/// `group_strip::LABEL_FLAG`: интерактив миниатюры — кнопка под тем же
/// индексом, оформление поверх — неинтерактивный виджет).
const BACKDROP_FLAG: WidgetId = 0x4000_0000;
/// Флаг для id красного кольца невлезающей раскладки — не декодируется и
/// отличен от [`BACKDROP_FLAG`]: у кольца и подложки общая родительская
/// кнопка, и совпади флаги — второй виджет с тем же id молча потерялся бы
/// в `Panel::widget`.
const RING_FLAG: WidgetId = 0x4000_0002;
/// Отступ красного кольца внутрь миниатюры, когда раскладка одновременно
/// ВЫБРАНА и не влезает: акцентное кольцо выбора стоит на кромке, и красное
/// на той же кромке легло бы ровно поверх него, съев выбор. Две рамки
/// читаются только когда вторая сдвинута внутрь.
const WARN_RING_INSET: f64 = 2.0;
/// Подпись поля зазора (нативный UI — английский, как весь UI оверлея).
/// Подпись поля зазора. Знак процента стоит В ПОДПИСИ, а не в самом поле:
/// поле числовое и принимает только цифры, а без единицы измерения «5»
/// рядом с раскладками читается как «пятая раскладка» (запрос пользователя
/// 2026-08-25).
const GAP_LABEL: &str = "Gap %";
/// Идентификатор подписи поля зазора (не интерактивна).
const ID_GAP_LABEL: WidgetId = 881;
/// Подпись первой раскладки, сочинённой под конкретный набор окон
/// (запрос пользователя 2026-08-27: «добавь новый тайлинг пресет который
/// будет называться adaptive»). Латиницей, как просил пользователь, а не
/// переводом: слово в ленте должно совпадать со словом, которое он
/// нажимал и будет искать.
const ADAPTIVE_LABEL: &str = "adaptive";
/// Идентификатор подписи «adaptive» (не интерактивна). Тот же принцип, что
/// у [`ID_GAP_LABEL`]: id вне диапазона слотов, иначе `Panel::widget` по
/// совпавшему id нашёл бы слот, а не подпись.
const ID_ADAPTIVE_LABEL: WidgetId = 882;
/// Зазор между нижней кромкой миниатюры и подписью под ней, DIP.
const ADAPTIVE_CAPTION_GAP: f64 = 4.0;

/// Сторона миниатюры при полном размере, DIP.
pub const THUMB_SIZE: f64 = 72.0;
/// Зазор между миниатюрами, DIP.
const THUMB_GAP: f64 = 8.0;
/// Зазор между слотами внутри миниатюры, DIP — между светлыми слотами
/// просвечивает тёмная подложка, и зазор читается как разделитель «окон».
const SLOT_GAP: f64 = 1.0;
/// Внутренний отступ панели, DIP.
const PAD: f64 = 12.0;
/// Зазор между лентой миниатюр и блоком поля зазора, DIP.
const STRIP_GAP: f64 = 14.0;
/// Зазор между подписью и полем зазора, DIP.
const LABEL_GAP: f64 = 8.0;
/// Отступ панели от верхнего края экрана, DIP.
const TOP_GAP: f64 = 16.0;
/// Ширина поля зазора, DIP.
const FIELD_W: f64 = 56.0;

/// Прямоугольник слота в экранных DIP: `preset` — индекс раскладки,
/// `slot` — индекс в `Preset::slots` (слот номер `slot + 1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotTarget {
    pub preset: usize,
    pub slot: usize,
    pub rect: Box2D,
}

/// Результат [`build`]: панель + геометрия, по которой координатор
/// понимает клики и перетаскивание.
pub struct StripBuild {
    pub panel: Panel,
    /// Прямоугольники миниатюр в экранных DIP; `thumbs[i]` — раскладка `i`.
    pub thumbs: Vec<Box2D>,
    /// Прямоугольники всех слотов всех раскладок — для хит-теста
    /// перетаскивания окна в слот (и для подсветки под курсором, если
    /// координатор захочет).
    pub slots: Vec<SlotTarget>,
}

/// Вердикт «влезает ли раскладка в минимальные размеры окон группы».
///
/// Считает его НЕ лента: это чистая геометрия — слоты против
/// `ptMinTrackSize` окон, — она живёт в rst-core (`group_fit::fit_slots`) и
/// применяется координатором; сюда вердикты приходят готовым срезом,
/// параллельным списку показанных раскладок (см. [`build`]).
///
/// Вердикт СОВЕЩАТЕЛЬНЫЙ: выбрать невлезающую раскладку можно —
/// пользователь вправе получить перекрытие сознательно, а лента лишь
/// показывает, что его ждёт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbFit {
    /// Все известные минимумы влезают в свои слоты.
    Fits,
    /// Хотя бы одно окно группы не влезает в свой слот: раскладка даст
    /// перекрытие, если применить её как есть.
    Overflows,
}

/// Сторона миниатюры, при которой лента вместе с блоком поля зазора
/// влезает в экран `screen` по ширине: `count` миниатюр ужимаются
/// равномерно, список никогда не режется — все варианты обязаны быть
/// видны сразу, а размер считается по ФАКТИЧЕСКОМУ числу раскладок в
/// срезе, а не по потолку (срез может быть короче семи). Ноль раскладок —
/// нулевые миниатюры: рисовать нечего, а деление на ноль незачем.
/// Вырожденный экран даёт 0 — нулевые миниатюры, без паники.
fn thumb_size(screen: &DipRect, count: usize) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let count = count as f64;
    let fixed = 2.0 * PAD
        + (count - 1.0) * THUMB_GAP
        + STRIP_GAP
        + LABEL_GAP
        + text_size(GAP_LABEL).0
        + FIELD_W;
    ((screen.w - fixed) / count).clamp(0.0, THUMB_SIZE)
}

/// Слот `unit` в долях миниатюры → прямоугольник в DIP с зазором `gap`
/// между слотами и от краёв. Математика та же, что у
/// `group_layout::apply`: каждый слот отступает от своих кромок на ползазора,
/// общая граница соседей считается один раз и не разъезжается — слоты
/// стыкуются без щелей и нахлёстов.
fn unit_to_box(unit: UnitRect, thumb: Box2D, gap: f64) -> Box2D {
    let half = gap / 2.0;
    let left = thumb.cx - thumb.w / 2.0 + half;
    let top = thumb.cy - thumb.h / 2.0 + half;
    let inner_w = (thumb.w - gap).max(0.0);
    let inner_h = (thumb.h - gap).max(0.0);
    let x0 = left + unit.x * inner_w + half;
    let x1 = left + (unit.x + unit.w) * inner_w - half;
    let y0 = top + unit.y * inner_h + half;
    let y1 = top + (unit.y + unit.h) * inner_h - half;
    Box2D {
        cx: (x0 + x1) / 2.0,
        cy: (y0 + y1) / 2.0,
        w: (x1 - x0).max(0.0),
        h: (y1 - y0).max(0.0),
        rotation: 0.0,
    }
}

/// Четыре тонкие полосы accent-рамки выбранной миниатюры — та же геометрия,
/// что `group_strip::picked_outline_edges` (рамка читается как состояние,
/// а не как второй контур).
fn outline_edges(rect: Box2D) -> [Box2D; 4] {
    let t = theme::settings::BEVEL;
    let half_w = rect.w / 2.0;
    let half_h = rect.h / 2.0;
    let edge = |cx: f64, cy: f64, w: f64, h: f64| Box2D {
        cx,
        cy,
        w,
        h,
        rotation: 0.0,
    };
    [
        edge(rect.cx, rect.cy - half_h + t / 2.0, rect.w, t),
        edge(rect.cx, rect.cy + half_h - t / 2.0, rect.w, t),
        edge(rect.cx - half_w + t / 2.0, rect.cy, t, rect.h),
        edge(rect.cx + half_w - t / 2.0, rect.cy, t, rect.h),
    ]
}

/// Собрать ленту раскладок в верхней части экрана `screen` (DIP).
///
/// `presets` — ГОТОВЫЙ срез раскладок: миниатюры рисуются по нему один в
/// один. Лента не выводит раскладки из числа отмеченных окон и не знает,
/// откуда срез взялся — его считает координатор по минимальным размерам
/// окон (генератор поверх `group_fit`), и число слотов в каждой раскладке
/// это число отмеченных окон: номера на миниатюре берутся из
/// `Preset::slots`, а не домысливаются. ПУСТОЙ срез — раскладок нет вовсе:
/// лента без миниатюр, панель сжимается до блока поля зазора, и
/// пользователь видит только подпись «Gap %» с полем — пустого «обрубка»
/// во всю миниатюру не остаётся.
///
/// `adaptive_first` — первая раскладка среза «adaptive»: сочинена под
/// конкретный набор окон, а не из семейства, и подписана словом
/// [`ADAPTIVE_LABEL`] ПОД миниатюрой (силуэт слотов занимает всю карточку,
/// текст поверх него спрятал бы то, что пользователь сравнивает; подпись
/// растягивает ленту на строку, но та остаётся внутри экрана). Флаг НЕ
/// двигает индексы: выбранная раскладка, вердикт и id кликов привязаны к
/// индексу в срезе как обычно — флаг лишь рисует подпись на карточке 0.
/// Без флага лента выглядит ровно как сегодня.
///
/// `selected` — индекс выбранной раскладки (accent-рамка); `gap_pct` —
/// текущая величина зазора в процентах, подставляется в поле.
///
/// `fit` — вердикты, параллельные срезу `presets`: `fit[i]` решает судьбу
/// миниатюры `i`. ПУСТОЙ срез — «вердиктов нет» (минимумы неизвестны,
/// координатор ещё не посчитал), ничего не помечается; срез короче списка
/// — недостающие раскладки считаются влезающими, длиннее — лишние вердикты
/// не на что вешать.
pub fn build(
    presets: &[Preset],
    adaptive_first: bool,
    selected: Option<usize>,
    screen: &DipRect,
    gap_pct: u32,
    fit: &[ThumbFit],
) -> StripBuild {
    let thumb = thumb_size(screen, presets.len());
    // Подпись «adaptive» под первой карточкой: силуэт слотов занимает всю
    // карточку, и текст поверх него спрятал бы то, что пользователь
    // сравнивает; поэтому подпись живёт ПОД миниатюрой и растягивает
    // панель на строку. На вырожденном экране (миниатюра нулевая) подписи
    // негде быть — она пропадает вместе с карточкой.
    let has_caption = adaptive_first && !presets.is_empty() && thumb > 0.0;
    let caption_h = if has_caption {
        ADAPTIVE_CAPTION_GAP + text_size(ADAPTIVE_LABEL).1
    } else {
        0.0
    };
    // Ширина — по ФАКТИЧЕСКОМУ числу миниатюр, а не по потолку: набор
    // короче семи (или пустой) оставлял бы за собой полосу пустой панели
    // во весь экран.
    let shown = presets.len().max(1) as f64;
    let strip_w = shown * thumb + (shown - 1.0) * THUMB_GAP;
    let gap_block = STRIP_GAP + LABEL_GAP + text_size(GAP_LABEL).0 + FIELD_W;
    let w = 2.0 * PAD + strip_w + gap_block;
    let h = 2.0 * PAD + thumb + caption_h;
    let frame = Box2D {
        cx: screen.x + screen.w / 2.0,
        cy: screen.y + TOP_GAP + h / 2.0,
        w,
        h,
        rotation: 0.0,
    };
    let mut panel = Panel::new(STRIP_PANEL_ID, frame)
        .with_style(WidgetStyle::Settings)
        .with_corner_radius(theme::settings::CORNER_RADIUS);

    let mut thumbs = Vec::new();
    let mut slots = Vec::new();
    let left = frame.cx - frame.w / 2.0 + PAD;
    for (i, preset) in presets.iter().enumerate() {
        let thumb_frame = Box2D {
            cx: left + thumb / 2.0 + i as f64 * (thumb + THUMB_GAP),
            cy: frame.cy,
            w: thumb,
            h: thumb,
            rotation: 0.0,
        };
        // Клик по пустому месту миниатюры (раскладки «главное по центру»
        // оставляют свободные углы) тоже выбирает раскладку: кнопка без
        // своей подписи под неинтерактивным оформлением — тот же приём, что
        // карточки `group_strip`.
        panel.add_widget(
            Button::new(
                THUMB_BASE + i as WidgetId,
                thumb_frame,
                ButtonContent::Label(String::new()),
            )
            .with_style(WidgetStyle::Settings),
        );
        panel.add_widget(ThumbBackdrop {
            id: THUMB_BASE + i as WidgetId + BACKDROP_FLAG,
            frame: thumb_frame,
            selected: selected == Some(i),
        });
        for (j, unit) in preset.slots.iter().enumerate() {
            let slot_rect = unit_to_box(*unit, thumb_frame, SLOT_GAP);
            panel.add_widget(
                Button::new(
                    SLOT_BASE + (i * MAX_SLOTS + j) as WidgetId,
                    slot_rect,
                    ButtonContent::Label((j + 1).to_string()),
                )
                .with_style(WidgetStyle::Settings),
            );
            slots.push(SlotTarget {
                preset: i,
                slot: j,
                rect: slot_rect,
            });
        }
        if fit.get(i) == Some(&ThumbFit::Overflows) {
            // Кольцо — ПОВЕРХ слотов, а не под ними (в отличие от подложки
            // и акцентной рамки): слоты прилегают к кромке миниатюры, и
            // кольцо, нарисованное раньше них, было бы почти целиком
            // закрыто. Клику оно не мешает — hit_test ложь.
            panel.add_widget(OverflowRing {
                id: THUMB_BASE + i as WidgetId + RING_FLAG,
                frame: thumb_frame,
                inset: if selected == Some(i) {
                    WARN_RING_INSET
                } else {
                    0.0
                },
            });
        }
        thumbs.push(thumb_frame);
    }

    if has_caption {
        // Подпись по центру под первой (adaptive) карточкой. `Label`
        // неинтерактивен (hit_test ложь) — кликам и перетаскиванию по
        // миниатюре не мешает, хотя и лежит внутри панели.
        let first = &thumbs[0];
        let (tw, th) = text_size(ADAPTIVE_LABEL);
        panel.add_widget(Label::new(
            ID_ADAPTIVE_LABEL,
            first.cx - tw / 2.0,
            first.cy + first.h / 2.0 + ADAPTIVE_CAPTION_GAP + th / 2.0,
            ADAPTIVE_LABEL,
        ));
    }

    // Поле величины зазора с подписью — справа от ленты, по центру панели
    // по вертикали (поле ниже миниатюр, но панель не растёт: высоту задают
    // миниатюры).
    let label_w = text_size(GAP_LABEL).0;
    let field_cx = frame.cx + frame.w / 2.0 - PAD - FIELD_W / 2.0;
    panel.add_widget(Label::new(
        ID_GAP_LABEL,
        field_cx - FIELD_W / 2.0 - LABEL_GAP - label_w,
        frame.cy,
        GAP_LABEL,
    ));
    let mut field = NumericField::snap_gap(GAP_FIELD_ID, field_cx, frame.cy, FIELD_W)
        .with_style(WidgetStyle::Settings);
    field.set_value(gap_pct);
    panel.add_widget(field);

    StripBuild {
        panel,
        thumbs,
        slots,
    }
}

/// Оформление миниатюры: тёмная подложка «экрана» и accent-рамка выбранной
/// раскладки. Неинтерактивна — клики и hover ловит кнопка-подложка под ней
/// (тот же приём разделения «фон/интерактив» и «контент», что `CardContent`
/// в `group_strip`).
struct ThumbBackdrop {
    id: WidgetId,
    frame: Box2D,
    selected: bool,
}

impl Widget for ThumbBackdrop {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.frame
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.frame = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        // Подложка темнее фона панели — светлые слоты-кнопки читаются на ней
        // как окна, а зазор между ними виден как тонкая тёмная сетка.
        out.push(Primitive::Fill {
            rect: self.frame,
            color: theme::settings::INPUT_BG,
            opacity: 1.0,
        });
        if self.selected {
            for edge in outline_edges(self.frame) {
                out.push(Primitive::Fill {
                    rect: edge,
                    color: theme::settings::ACCENT,
                    opacity: 1.0,
                });
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Красное кольцо невлезающей раскладки: та же геометрия, что у акцентной
/// рамки выбора ([`ThumbBackdrop`]), но в неинтерактивном слое ПОВЕРХ
/// слотов — силуэт остаётся читаемым, клик по миниатюре не блокируется, и
/// кольцо видно даже там, где слоты прилегают к кромке.
///
/// `inset` — отступ от кромки внутрь: 0 у невыбранной миниатюры (кольцо на
/// кромке, как акцентное), [`WARN_RING_INSET`] у выбранной — иначе красное
/// кольцо легло бы ровно на акцентное и съело бы выбор.
struct OverflowRing {
    id: WidgetId,
    frame: Box2D,
    inset: f64,
}

impl Widget for OverflowRing {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.frame
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.frame = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        // Вырожденная миниатюра (ужатие на узком экране) может стать уже
        // двойного отступа — рисовать кольцо негде, и незачем.
        let inner_w = (self.frame.w - 2.0 * self.inset).max(0.0);
        let inner_h = (self.frame.h - 2.0 * self.inset).max(0.0);
        if inner_w <= 0.0 || inner_h <= 0.0 {
            return;
        }
        let inner = Box2D {
            cx: self.frame.cx,
            cy: self.frame.cy,
            w: inner_w,
            h: inner_h,
            rotation: 0.0,
        };
        for edge in outline_edges(inner) {
            out.push(Primitive::Fill {
                rect: edge,
                color: theme::settings::DANGER,
                opacity: 1.0,
            });
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_core::group_layout::presets_for;
    use rst_render::PointerEvent;

    fn screen(w: f64, h: f64) -> DipRect {
        DipRect::new(0.0, 0.0, w, h)
    }

    /// Три произвольные раскладки на разное число слотов — контроль того,
    /// что лента не знает ни числа окон, ни таблицы: раскладки с 1, 2 и 3
    /// слотами должны отрисоваться как есть, каждая со своими номерами.
    fn three_layouts() -> Vec<Preset> {
        vec![
            Preset {
                slots: vec![UnitRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                }],
            },
            Preset {
                slots: vec![
                    UnitRect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.5,
                        h: 1.0,
                    },
                    UnitRect {
                        x: 0.5,
                        y: 0.0,
                        w: 0.5,
                        h: 1.0,
                    },
                ],
            },
            Preset {
                slots: (0..3)
                    .map(|i| UnitRect {
                        x: i as f64 / 3.0,
                        y: 0.0,
                        w: 1.0 / 3.0,
                        h: 1.0,
                    })
                    .collect(),
            },
        ]
    }

    fn assert_close(actual: f64, expected: f64, ctx: &str) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "{ctx}: ожидалось {expected}, получено {actual}"
        );
    }

    /// Все непустые заливки панели заданного цвета — фон слотов (BTN_BG) и
    /// подложек, полосы рамки (ACCENT) и т.п.
    fn fills(panel: &Panel, color: [u8; 3]) -> Vec<Box2D> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter_map(|p| match p {
                Primitive::Fill { rect, color: c, .. }
                    if c == color && rect.w > 0.0 && rect.h > 0.0 =>
                {
                    Some(rect)
                }
                _ => None,
            })
            .collect()
    }

    /// Площадь пересечения двух прямоугольников: 0 — не налезают.
    fn overlap(a: Box2D, b: Box2D) -> f64 {
        let ix =
            (a.cx + a.w / 2.0).min(b.cx + b.w / 2.0) - (a.cx - a.w / 2.0).max(b.cx - b.w / 2.0);
        let iy =
            (a.cy + a.h / 2.0).min(b.cy + b.h / 2.0) - (a.cy - a.h / 2.0).max(b.cy - b.h / 2.0);
        ix.max(0.0) * iy.max(0.0)
    }

    /// Любое число раскладок в срезе ужимается, а не режется: все миниатюры
    /// обязаны остаться внутри экрана на любом его размере, панель тоже, и
    /// каждая миниатюра жива (ненулевая). Срезы разной длины — от пустого до
    /// полной семёрки — проходят один и тот же путь.
    #[test]
    fn any_number_of_layouts_fit_any_screen_by_shrinking_not_truncating() {
        let cases: [&[Preset]; 4] = [presets_for(2), presets_for(4), presets_for(8), &[]];
        for presets in cases {
            for (w, h) in [
                (1920.0, 1080.0),
                (1280.0, 720.0),
                (800.0, 600.0),
                (640.0, 360.0),
                (480.0, 270.0),
            ] {
                let scr = screen(w, h);
                let built = build(presets, false, None, &scr, 5, &[]);
                assert_eq!(
                    built.thumbs.len(),
                    presets.len(),
                    "len={} {w}×{h}: по миниатюре на раскладку",
                    presets.len()
                );
                assert_eq!(
                    built.slots.len(),
                    presets.iter().map(|p| p.slots.len()).sum::<usize>(),
                    "len={} {w}×{h}: все слоты всех раскладок",
                    presets.len()
                );
                for thumb in &built.thumbs {
                    assert!(
                        thumb.cx - thumb.w / 2.0 >= scr.x - 1e-9,
                        "len={} {w}×{h}",
                        presets.len()
                    );
                    assert!(
                        thumb.cx + thumb.w / 2.0 <= scr.x + scr.w + 1e-9,
                        "len={} {w}×{h}",
                        presets.len()
                    );
                }
                let f = built.panel.frame();
                let panel_w = f.w;
                assert!(
                    f.cx - f.w / 2.0 >= scr.x - 1e-9 && f.cx + f.w / 2.0 <= scr.x + scr.w + 1e-9,
                    "len={} {w}×{h}: панель шириной {panel_w} не влезает в экран {w}",
                    presets.len()
                );
                if !presets.is_empty() {
                    assert!(
                        built.thumbs[0].w > 0.0,
                        "len={} {w}×{h}: миниатюры ужаты, но живы",
                        presets.len()
                    );
                }
            }
        }
    }

    #[test]
    fn slot_rects_stay_inside_their_thumbnail_and_never_overlap() {
        for count in 2..=8 {
            let presets = presets_for(count);
            for preset_idx in 0..presets.len() {
                let built = build(presets, false, None, &screen(1920.0, 1080.0), 5, &[]);
                let slots: Vec<&SlotTarget> = built
                    .slots
                    .iter()
                    .filter(|s| s.preset == preset_idx)
                    .collect();
                assert_eq!(slots.len(), count, "count={count} preset={preset_idx}");
                let thumb = built.thumbs[preset_idx];
                for s in &slots {
                    let r = s.rect;
                    assert!(
                        r.cx - r.w / 2.0 >= thumb.cx - thumb.w / 2.0 - 1e-9
                            && r.cx + r.w / 2.0 <= thumb.cx + thumb.w / 2.0 + 1e-9
                            && r.cy - r.h / 2.0 >= thumb.cy - thumb.h / 2.0 - 1e-9
                            && r.cy + r.h / 2.0 <= thumb.cy + thumb.h / 2.0 + 1e-9,
                        "count={count} preset={preset_idx}: слот {r:?} вылез за миниатюру {thumb:?}"
                    );
                }
                for (i, a) in slots.iter().enumerate() {
                    for b in slots.iter().skip(i + 1) {
                        assert_eq!(
                            overlap(a.rect, b.rect),
                            0.0,
                            "count={count} preset={preset_idx}: нахлёст слотов {i}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn reported_slot_rects_match_the_drawn_slot_fills() {
        // Слоты рисуются кнопками: фон слота — заливка BTN_BG ровно на его
        // прямоугольнике. Подложки миниатюр рисуют BTN_BG на весь квадрат —
        // они не совпадают ни с одним отданным слотом и в подсчёт не входят.
        let built = build(presets_for(3), false, None, &screen(1920.0, 1080.0), 5, &[]);
        let drawn = fills(&built.panel, theme::settings::BTN_BG);
        let reported: Vec<Box2D> = built.slots.iter().map(|s| s.rect).collect();
        let matched = drawn.iter().filter(|r| reported.contains(r)).count();
        assert_eq!(
            matched,
            reported.len(),
            "каждый отданный слот нарисован ровно один раз"
        );
        assert!(
            reported.iter().all(|r| drawn.contains(r)),
            "все нарисованные слоты отданы наружу"
        );
    }

    /// Витрина при нуле и одном отмеченном окне — решение КООРДИНАТОРА: это
    /// он решает, что показать (набор для двух окон, чтобы лента не
    /// выглядела сломанной — живой репорт 2026-08-25), и передаёт готовый
    /// срез. Лента из числа окон ничего не выводит: что дали, то и рисует.
    #[test]
    fn zero_and_one_picked_windows_draw_whatever_layouts_were_passed() {
        let built = build(
            presets_for(2),
            false,
            Some(2),
            &screen(1920.0, 1080.0),
            5,
            &[],
        );
        assert_eq!(
            built.thumbs.len(),
            presets_for(2).len(),
            "витрина для двух окон нарисована как есть"
        );
        assert!(!built.slots.is_empty(), "слоты нарисованы");
    }

    /// Пустой срез раскладок — лента без миниатюр, без паники и без
    /// «обрубка»: панель сжимается до блока поля зазора, поле на месте.
    /// Раньше это был случай «окон больше восьми», теперь — обычный вход:
    /// генератор или координатор вправе вернуть пусто, и лента обязана это
    /// пережить.
    #[test]
    fn an_empty_layout_slice_yields_no_thumbnails_but_keeps_the_gap_field() {
        let built = build(&[], false, Some(2), &screen(1920.0, 1080.0), 5, &[]);
        assert!(built.thumbs.is_empty(), "миниатюр нет");
        assert!(built.slots.is_empty(), "слотов нет");
        let gap_block = STRIP_GAP + LABEL_GAP + text_size(GAP_LABEL).0 + FIELD_W;
        let f = built.panel.frame();
        assert_close(
            f.w,
            2.0 * PAD + gap_block,
            "панель без обрубка: только блок зазора",
        );
        assert!(
            built.panel.widget::<NumericField>(GAP_FIELD_ID).is_some(),
            "поле зазора на месте даже без ленты"
        );
    }

    #[test]
    fn degenerate_screen_builds_without_panic() {
        let cases: [&[Preset]; 2] = [presets_for(4), &[]];
        for scr in [
            DipRect::new(0.0, 0.0, 0.0, 0.0),
            DipRect::new(5.0, 5.0, -10.0, -10.0),
            DipRect::new(0.0, 0.0, 20.0, 20.0),
            DipRect::new(0.0, 0.0, 64.0, 48.0),
        ] {
            for presets in cases {
                let built = build(presets, false, Some(2), &scr, 5, &[]);
                let f = built.panel.frame();
                assert!(f.w >= 0.0 && f.h >= 0.0, "геометрия неотрицательна: {f:?}");
                let mut out = Vec::new();
                built.panel.draw(&mut out);
                for s in &built.slots {
                    assert!(s.rect.w >= 0.0 && s.rect.h >= 0.0, "слот неотрицателен");
                }
            }
        }
    }

    #[test]
    fn strip_hangs_at_the_top_of_the_screen_centered() {
        let scr = screen(1920.0, 1080.0);
        let f = build(presets_for(2), false, None, &scr, 5, &[])
            .panel
            .frame();
        assert_close(f.cy - f.h / 2.0, scr.y + TOP_GAP, "верх панели");
        assert_close(f.cx, scr.x + scr.w / 2.0, "центр по горизонтали");
    }

    #[test]
    fn full_size_strip_keeps_thumbnails_square_and_field_to_the_right() {
        let scr = screen(1920.0, 1080.0);
        let built = build(presets_for(4), false, None, &scr, 5, &[]);
        assert_close(built.thumbs[0].w, THUMB_SIZE, "миниатюры в полном размере");
        assert_close(built.thumbs[0].h, THUMB_SIZE, "миниатюры квадратные");
        let f = built.panel.frame();
        let field = built
            .panel
            .widget::<NumericField>(GAP_FIELD_ID)
            .expect("поле зазора")
            .bounds();
        assert!(
            field.cx - field.w / 2.0 >= f.cx - f.w / 2.0
                && field.cx + field.w / 2.0 <= f.cx + f.w / 2.0,
            "поле внутри панели"
        );
        let last = built.thumbs.last().expect("миниатюры есть — срез непустой");
        assert!(
            field.cx - field.w / 2.0 > last.cx + last.w / 2.0,
            "поле справа от ленты"
        );
    }

    #[test]
    fn selected_layout_draws_an_accent_ring_and_others_do_not() {
        let built = build(
            presets_for(2),
            false,
            Some(1),
            &screen(1920.0, 1080.0),
            5,
            &[],
        );
        let accent = fills(&built.panel, theme::settings::ACCENT);
        assert_eq!(accent.len(), 4, "ровно четыре полосы рамки");
        let thumb = built.thumbs[1];
        assert_eq!(
            accent
                .iter()
                .filter(|r| (r.w - thumb.w).abs() < 1e-9)
                .count(),
            2,
            "две горизонтальные полосы"
        );
        assert_eq!(
            accent
                .iter()
                .filter(|r| (r.h - thumb.h).abs() < 1e-9)
                .count(),
            2,
            "две вертикальные полосы"
        );
        let plain = build(presets_for(2), false, None, &screen(1920.0, 1080.0), 5, &[]);
        assert!(
            fills(&plain.panel, theme::settings::ACCENT).is_empty(),
            "без выбора акцентных полос нет"
        );
    }

    #[test]
    fn every_slot_shows_its_number() {
        let mut out = Vec::new();
        build(presets_for(4), false, None, &screen(1920.0, 1080.0), 5, &[])
            .panel
            .draw(&mut out);
        for n in 1..=4 {
            let digit = n.to_string();
            assert!(
                out.iter()
                    .any(|p| matches!(p, Primitive::Text { text, .. } if *text == digit)),
                "цифра {digit} на месте"
            );
        }
    }

    /// Три раскладки в срезе — три миниатюры на ленте: лента рисует ровно
    /// то, что ей дали, и не домысливает семёрку.
    #[test]
    fn three_layouts_are_drawn_as_three_thumbnails() {
        let presets = three_layouts();
        let built = build(&presets, false, None, &screen(1920.0, 1080.0), 5, &[]);
        assert_eq!(built.thumbs.len(), 3, "по миниатюре на раскладку");
        assert_eq!(
            built.slots.len(),
            presets.iter().map(|p| p.slots.len()).sum::<usize>(),
            "слоты всех трёх раскладок"
        );
        let mut out = Vec::new();
        built.panel.draw(&mut out);
        assert!(
            out.iter()
                .any(|p| matches!(p, Primitive::Text { text, .. } if *text == "3")),
            "третья раскладка с тремя слотами нарисована"
        );
    }

    /// Номера слотов на миниатюре — из САМОЙ раскладки, а не из «числа
    /// окон»: лента вообще не получает число окон, и три раскладки на 1, 2
    /// и 3 слота рисуют цифры «1»; «1,2»; «1,2,3».
    #[test]
    fn slot_numbers_follow_the_layouts_own_slot_count() {
        let mut out = Vec::new();
        build(
            &three_layouts(),
            false,
            None,
            &screen(1920.0, 1080.0),
            5,
            &[],
        )
        .panel
        .draw(&mut out);
        let digits: Vec<&str> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            digits.iter().filter(|d| **d == "1").count(),
            3,
            "«1» на каждой из трёх миниатюр"
        );
        assert_eq!(
            digits.iter().filter(|d| **d == "2").count(),
            2,
            "«2» на второй и третьей миниатюрах"
        );
        assert_eq!(
            digits.iter().filter(|d| **d == "3").count(),
            1,
            "«3» только на третьей миниатюре"
        );
    }

    #[test]
    fn clicking_a_slot_registers_on_that_slot_button_only() {
        let mut built = build(presets_for(3), false, None, &screen(1920.0, 1080.0), 5, &[]);
        let target = built
            .slots
            .iter()
            .find(|s| s.preset == 1 && s.slot == 2)
            .expect("слот preset=1 slot=2");
        let pos = (target.rect.cx, target.rect.cy);
        let _ = built.panel.pointer_event(PointerEvent::Down { pos });
        let _ = built.panel.pointer_event(PointerEvent::Up { pos });

        let clicked_id = SLOT_BASE + (MAX_SLOTS + 2) as WidgetId;
        assert!(
            built
                .panel
                .widget_mut::<Button>(clicked_id)
                .expect("кнопка слота")
                .take_click(),
            "клик по слоту засчитан именно ему"
        );
        for (i, j) in [(0usize, 0usize), (1, 1), (2, 0)] {
            let other = SLOT_BASE + (i * MAX_SLOTS + j) as WidgetId;
            assert!(
                !built
                    .panel
                    .widget_mut::<Button>(other)
                    .unwrap()
                    .take_click(),
                "чужая кнопка {i}/{j} не должна кликнуться"
            );
        }
        assert!(
            !built
                .panel
                .widget_mut::<Button>(THUMB_BASE + 1)
                .unwrap()
                .take_click(),
            "подложка той же миниатюры чиста"
        );
    }

    #[test]
    fn clicking_an_empty_spot_of_a_thumbnail_selects_that_layout() {
        // Раскладка «главное по центру» (пять окон) оставляет свободные
        // углы: клик туда не должен теряться — подложка выбирает раскладку.
        let mut built = build(presets_for(5), false, None, &screen(1920.0, 1080.0), 5, &[]);
        let thumb = built.thumbs[6];
        let pos = (
            thumb.cx - thumb.w / 2.0 + 0.1 * thumb.w,
            thumb.cy - thumb.h / 2.0 + 0.5 * thumb.h,
        );
        assert!(
            built
                .slots
                .iter()
                .filter(|s| s.preset == 6)
                .all(|s| !rst_render::box_contains(&s.rect, pos)),
            "точка действительно в пустом месте миниатюры"
        );
        let _ = built.panel.pointer_event(PointerEvent::Down { pos });
        let _ = built.panel.pointer_event(PointerEvent::Up { pos });
        assert!(
            built
                .panel
                .widget_mut::<Button>(THUMB_BASE + 6)
                .unwrap()
                .take_click(),
            "клик по пустому месту выбирает раскладку"
        );
    }

    #[test]
    fn gap_field_reports_the_passed_percent_and_clamps() {
        for gap in [0, 5, 17, 35] {
            let built = build(
                presets_for(2),
                false,
                None,
                &screen(1920.0, 1080.0),
                gap,
                &[],
            );
            let field = built
                .panel
                .widget::<NumericField>(GAP_FIELD_ID)
                .expect("поле зазора");
            assert_eq!(field.value(), gap, "зазор {gap} подставлен в поле");
        }
        let built = build(
            presets_for(2),
            false,
            None,
            &screen(1920.0, 1080.0),
            99,
            &[],
        );
        assert_eq!(
            built
                .panel
                .widget::<NumericField>(GAP_FIELD_ID)
                .unwrap()
                .value(),
            35,
            "значение вне диапазона клампится, как у всех числовых полей"
        );
    }

    /// Невлезающая раскладка получает красное кольцо РОВНО на своей
    /// миниатюре: четыре полосы цвета DANGER на самой кромке (та же
    /// геометрия, что у акцентной рамки), и ни одной полосы на соседях.
    #[test]
    fn an_overflowing_layout_draws_a_danger_ring_only_on_that_thumbnail() {
        let presets = presets_for(4);
        let mut fit = vec![ThumbFit::Fits; presets.len()];
        fit[2] = ThumbFit::Overflows;
        let built = build(presets, false, None, &screen(1920.0, 1080.0), 5, &fit);
        let danger = fills(&built.panel, theme::settings::DANGER);
        assert_eq!(
            danger,
            outline_edges(built.thumbs[2]),
            "кольцо на кромке миниатюры с индексом 2 и нигде больше"
        );
    }

    /// Выбранная И невлезающая раскладка несёт ДВЕ рамки: акцентную на
    /// кромке (выбор) и красную с отступом внутрь (предупреждение) — на
    /// одной кромке красная легла бы поверх акцентной и съела бы выбор.
    #[test]
    fn a_selected_layout_that_overflows_draws_accent_and_danger_rings_together() {
        let presets = presets_for(4);
        let mut fit = vec![ThumbFit::Fits; presets.len()];
        fit[3] = ThumbFit::Overflows;
        let built = build(presets, false, Some(3), &screen(1920.0, 1080.0), 5, &fit);
        let accent = fills(&built.panel, theme::settings::ACCENT);
        let danger = fills(&built.panel, theme::settings::DANGER);
        let thumb = built.thumbs[3];
        assert_eq!(
            accent,
            outline_edges(thumb),
            "акцентное кольцо выбора на кромке"
        );
        let inner = Box2D {
            cx: thumb.cx,
            cy: thumb.cy,
            w: thumb.w - 2.0 * WARN_RING_INSET,
            h: thumb.h - 2.0 * WARN_RING_INSET,
            rotation: 0.0,
        };
        assert_eq!(
            danger,
            outline_edges(inner),
            "красное кольцо отступает внутрь, акцентное остаётся на кромке"
        );
    }

    /// Пустой срез вердиктов — «вердиктов нет» (минимумы неизвестны,
    /// координатор ещё не посчитал): ни одного красного кольца, а
    /// акцентное кольцо выбора работает как раньше.
    #[test]
    fn an_empty_verdict_slice_marks_nothing() {
        for presets in [presets_for(2), presets_for(4), &[][..]] {
            let built = build(presets, false, None, &screen(1920.0, 1080.0), 5, &[]);
            assert!(
                fills(&built.panel, theme::settings::DANGER).is_empty(),
                "len={}: без вердиктов красных колец нет",
                presets.len()
            );
        }
        let selected = build(
            presets_for(4),
            false,
            Some(1),
            &screen(1920.0, 1080.0),
            5,
            &[],
        );
        assert_eq!(
            fills(&selected.panel, theme::settings::ACCENT).len(),
            4,
            "выбор помечается акцентным кольцом и без вердиктов"
        );
        assert!(
            fills(&selected.panel, theme::settings::DANGER).is_empty(),
            "красных колец нет даже при выбранной раскладке"
        );
    }

    /// Вердикты не ломают ленту, когда отмечено ноль или одно окно:
    /// витрину (набор для двух окон) подставляет КООРДИНАТОР — лента рисует
    /// срез как есть, и вердикты любой длины не приводят к панике; пустой
    /// срез раскладок с пустыми вердиктами — тоже.
    #[test]
    fn verdicts_do_not_break_a_strip_with_few_or_no_windows() {
        let showcase = presets_for(2);
        let all_over = vec![ThumbFit::Overflows; showcase.len()];
        let built = build(showcase, false, None, &screen(1920.0, 1080.0), 5, &all_over);
        assert_eq!(built.thumbs.len(), showcase.len());
        assert!(!built.slots.is_empty());
        let mut out = Vec::new();
        built.panel.draw(&mut out);
        assert!(
            out.iter()
                .any(|p| matches!(p, Primitive::Text { text, .. } if *text == "1")),
            "витрина: слоты нарисованы"
        );

        let empty = build(&[], false, None, &screen(1920.0, 1080.0), 5, &[]);
        assert!(empty.thumbs.is_empty(), "пустой срез — без миниатюр");
        assert!(empty.slots.is_empty(), "пустой срез — без слотов");
    }

    /// Срез вердиктов короче списка раскладок: недостающие вердикты
    /// считаются «влезает», кольцо рисуется только на покрытых миниатюрах.
    /// Срез длиннее списка — лишние вердикты не на что вешать, они
    /// игнорируются.
    #[test]
    fn a_shorter_or_longer_verdict_slice_marks_only_what_it_covers() {
        let presets = presets_for(4);
        let short = vec![ThumbFit::Fits, ThumbFit::Overflows];
        let built = build(presets, false, None, &screen(1920.0, 1080.0), 5, &short);
        let danger = fills(&built.panel, theme::settings::DANGER);
        assert_eq!(
            danger,
            outline_edges(built.thumbs[1]),
            "кольцо только на миниатюре индекса 1"
        );

        let long = vec![ThumbFit::Overflows; 9];
        let built = build(presets, false, None, &screen(1920.0, 1080.0), 5, &long);
        assert_eq!(
            fills(&built.panel, theme::settings::DANGER).len(),
            presets.len() * 4,
            "по четыре полосы на каждую миниатюру среза"
        );
    }

    /// Красное кольцо не мешает выбору: клик по невлезающей миниатюре
    /// выбирает её, как любую другую, — вердикт совещательный, и кольцо
    /// живёт в неинтерактивном слое (hit_test ложь).
    #[test]
    fn an_overflowing_thumbnail_still_accepts_clicks() {
        let presets = presets_for(5);
        let mut fit = vec![ThumbFit::Fits; presets.len()];
        fit[6] = ThumbFit::Overflows;
        let mut built = build(presets, false, None, &screen(1920.0, 1080.0), 5, &fit);
        let thumb = built.thumbs[6];
        // Точка в свободном углу раскладки «главное по центру» — та же, что
        // в `clicking_an_empty_spot_of_a_thumbnail_selects_that_layout`.
        let pos = (
            thumb.cx - thumb.w / 2.0 + 0.1 * thumb.w,
            thumb.cy - thumb.h / 2.0 + 0.5 * thumb.h,
        );
        assert!(
            built
                .slots
                .iter()
                .filter(|s| s.preset == 6)
                .all(|s| !rst_render::box_contains(&s.rect, pos)),
            "точка действительно в пустом месте миниатюры"
        );
        let _ = built.panel.pointer_event(PointerEvent::Down { pos });
        let _ = built.panel.pointer_event(PointerEvent::Up { pos });
        assert!(
            built
                .panel
                .widget_mut::<Button>(THUMB_BASE + 6)
                .expect("кнопка миниатюры")
                .take_click(),
            "клик по пустому месту выбирает невлезающую раскладку"
        );
    }

    /// Текстовые примитивы панели — для поиска подписи `adaptive`.
    fn texts(panel: &Panel) -> Vec<String> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter_map(|p| match p {
                Primitive::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Подпись `adaptive` есть ровно один раз и только когда признак
    /// выставлен; без признака подписи нет вовсе, лента выглядит как
    /// сегодня.
    #[test]
    fn adaptive_label_appears_once_only_when_flagged() {
        let presets = presets_for(4);
        let flagged = build(presets, true, None, &screen(1920.0, 1080.0), 5, &[]);
        let flagged_texts = texts(&flagged.panel);
        assert_eq!(
            flagged_texts
                .iter()
                .filter(|t| *t == ADAPTIVE_LABEL)
                .count(),
            1,
            "подпись ровно на одной карточке"
        );
        let plain = build(presets, false, None, &screen(1920.0, 1080.0), 5, &[]);
        assert!(
            !texts(&plain.panel).iter().any(|t| t == ADAPTIVE_LABEL),
            "без признака подписи нет вовсе"
        );
    }

    /// Подпись сидит ПОД первой (adaptive) карточкой и по центру её: она
    /// не должна закрывать силуэт слотов — силуэт и есть то, что
    /// пользователь сравнивает.
    #[test]
    fn adaptive_label_sits_under_the_first_card_without_covering_slots() {
        let presets = presets_for(4);
        let built = build(presets, true, None, &screen(1920.0, 1080.0), 5, &[]);
        let mut out = Vec::new();
        built.panel.draw(&mut out);
        let label = out
            .iter()
            .find_map(|p| match p {
                Primitive::Text { rect, text, .. } if text == ADAPTIVE_LABEL => Some(*rect),
                _ => None,
            })
            .expect("подпись на месте");
        let first = built.thumbs[0];
        assert!(
            (label.cx - first.cx).abs() < 1e-9,
            "подпись по центру первой карточки"
        );
        assert!(
            label.cy > first.cy + first.h / 2.0,
            "подпись ниже карточки, а не поверх неё"
        );
        assert!(
            built
                .slots
                .iter()
                .filter(|s| s.preset == 0)
                .all(|s| label.cy > s.rect.cy + s.rect.h / 2.0),
            "подпись ниже нижних кромок всех слотов первой раскладки"
        );
    }

    /// Карточка `adaptive` ведёт себя как обычная: выбирается кликом,
    /// получает акцентное кольцо при выборе и красное при невыполнимости —
    /// флаг подписи не трогает ни индексы, ни механизмы колец.
    #[test]
    fn adaptive_card_still_selects_and_draws_both_rings() {
        // Срез из одной карточки со слотом по центру: у неё есть свободные
        // углы, и клик по углу достаётся кнопке карточки, а не слотам —
        // иначе выбор нельзя было бы отличить от клика по слоту.
        let presets = vec![Preset {
            slots: vec![UnitRect {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            }],
        }];
        let fit = [ThumbFit::Overflows];
        let mut built = build(&presets, true, Some(0), &screen(1920.0, 1080.0), 5, &fit);
        let accent = fills(&built.panel, theme::settings::ACCENT);
        let danger = fills(&built.panel, theme::settings::DANGER);
        let thumb = built.thumbs[0];
        assert_eq!(
            accent,
            outline_edges(thumb),
            "акцентное кольцо выбора на adaptive-карточке"
        );
        let inner = Box2D {
            cx: thumb.cx,
            cy: thumb.cy,
            w: thumb.w - 2.0 * WARN_RING_INSET,
            h: thumb.h - 2.0 * WARN_RING_INSET,
            rotation: 0.0,
        };
        assert_eq!(
            danger,
            outline_edges(inner),
            "красное кольцо невыполнимости на adaptive-карточке"
        );
        let pos = (
            thumb.cx - thumb.w / 2.0 + 0.1 * thumb.w,
            thumb.cy - thumb.h / 2.0 + 0.5 * thumb.h,
        );
        assert!(
            built
                .slots
                .iter()
                .filter(|s| s.preset == 0)
                .all(|s| !rst_render::box_contains(&s.rect, pos)),
            "точка в свободном углу adaptive-карточки"
        );
        let _ = built.panel.pointer_event(PointerEvent::Down { pos });
        let _ = built.panel.pointer_event(PointerEvent::Up { pos });
        assert!(
            built
                .panel
                .widget_mut::<Button>(THUMB_BASE)
                .expect("кнопка adaptive-карточки")
                .take_click(),
            "adaptive-карточку можно выбрать кликом"
        );
    }

    /// Лента с подписью `adaptive` остаётся внутри экрана и на маленьком
    /// экране: подпись растягивает панель на строку, и это не должно
    /// выталкивать ленту за нижнюю кромку.
    #[test]
    fn adaptive_caption_keeps_the_strip_inside_the_screen() {
        for (w, h) in [
            (1920.0, 1080.0),
            (1280.0, 720.0),
            (640.0, 360.0),
            (480.0, 270.0),
        ] {
            let scr = screen(w, h);
            let built = build(presets_for(4), true, None, &scr, 5, &[]);
            let f = built.panel.frame();
            assert!(
                f.cy - f.h / 2.0 >= scr.y - 1e-9 && f.cy + f.h / 2.0 <= scr.y + scr.h + 1e-9,
                "{w}×{h}: панель с подписью внутри экрана по вертикали"
            );
            assert!(
                f.cx - f.w / 2.0 >= scr.x - 1e-9 && f.cx + f.w / 2.0 <= scr.x + scr.w + 1e-9,
                "{w}×{h}: панель внутри экрана по горизонтали"
            );
        }
    }

    /// Пустой срез с выставленным признаком не роняет ленту: подписи не
    ///где — карточек нет, панель сжимается до блока поля зазора как без
    /// флага.
    #[test]
    fn an_empty_slice_with_adaptive_flag_does_not_panic_and_shows_no_label() {
        let built = build(&[], true, Some(2), &screen(1920.0, 1080.0), 5, &[]);
        assert!(built.thumbs.is_empty(), "миниатюр нет");
        assert!(
            !texts(&built.panel).iter().any(|t| t == ADAPTIVE_LABEL),
            "подписи нет — рисовать её не на чем"
        );
        assert!(
            built.panel.widget::<NumericField>(GAP_FIELD_ID).is_some(),
            "поле зазора на месте"
        );
    }
}
