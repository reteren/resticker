//! Лента раскладок тайлинга (T10) — верхняя часть меню редактирования
//! групп: горизонтальная лента миниатюр-схем, по одной на каждую раскладку.
//!
//! Каждая миниатюра — это экран, разбитый на прямоугольные слоты, и в
//! каждом слоте нарисована его цифра. Главное правило читается прямо с
//! ленты: окно, отмеченное первым в ленте карточек (`group_strip`), попадёт
//! в слот 1. Справа от ленты — поле величины зазора между окнами (проценты,
//! `NumericField::snap_gap`).
//!
//! Чистый строитель, как `preset_picker`/`group_strip`: на вход — число
//! окон в группе, индекс выбранной раскладки, прямоугольник экрана в DIP и
//! текущий зазор; на выход — [`Panel`] и геометрия для координатора
//! ([`StripBuild`]). Никакого Win32 и никакого состояния.
//!
//! Клики и перетаскивание: слоты — обычные кнопки [`Button`], клик по
//! любому месту миниатюры выбирает раскладку (кнопка-подложка ловит клики
//! по свободным углам раскладок «главное по центру»). Прямоугольники
//! слотов дополнительно отдаются наружу ([`StripBuild::slots`]) — по ним
//! координатор хитует перетаскивание окна из ленты карточек в конкретный
//! слот, это отдельный канал, не через кнопки.
//!
//! Ужатие вместо прокрутки (в отличие от ленты карточек `group_strip`):
//! раскладок ровно семь, и пользователь обязан видеть все варианты сразу —
//! выбор раскладки это сравнение силуэтов, а прокрутка скрыла бы половину
//! выбора. Миниатюры квадратные, а не в пропорции экрана: схемы читаются
//! по форме слотов, и высота квадрата использует место эффективнее, чем
//! низкая полоска 16:9.
//!
//! Модуль пока не подключён к координатору (подключение — отдельная задача
//! координатора, в `main.rs` он добавит `mod preset_strip;` сам), поэтому
//! `#![allow(dead_code)]` — тот же приём, что у `group_strip.rs`; атрибут
//! снять при подключении.

#![allow(dead_code)]

use rst_core::group_layout::{UnitRect, presets_for};
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

/// Наименьшее число окон, для которого существуют раскладки.
///
/// До второй отметки лента показывает именно этот набор — см. `build`.
const MIN_PRESET_WINDOWS: usize = 2;
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
/// Подпись поля зазора (нативный UI — английский, как весь UI оверлея).
/// Подпись поля зазора. Знак процента стоит В ПОДПИСИ, а не в самом поле:
/// поле числовое и принимает только цифры, а без единицы измерения «5»
/// рядом с раскладками читается как «пятая раскладка» (запрос пользователя
/// 2026-08-25).
const GAP_LABEL: &str = "Gap %";
/// Идентификатор подписи поля зазора (не интерактивна).
const ID_GAP_LABEL: WidgetId = 881;

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
/// Сколько раскладок у каждого числа окон (`group_layout` даёт ровно семь).
const PRESETS_PER_COUNT: f64 = 7.0;

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

/// Сторона миниатюры, при которой лента вместе с блоком поля зазора
/// влезает в экран `screen` по ширине: семь миниатюр ужимаются равномерно,
/// список никогда не режется — раскладок ровно семь, и все обязаны быть
/// видны сразу. Вырожденный экран даёт 0 — нулевые миниатюры, без паники.
fn thumb_size(screen: &DipRect) -> f64 {
    let fixed = 2.0 * PAD
        + (PRESETS_PER_COUNT - 1.0) * THUMB_GAP
        + STRIP_GAP
        + LABEL_GAP
        + text_size(GAP_LABEL).0
        + FIELD_W;
    ((screen.w - fixed) / PRESETS_PER_COUNT).clamp(0.0, THUMB_SIZE)
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
/// `window_count` — число окон в группе (2..=8; вне диапазона лента
/// пустая, панель остаётся с полем зазора); `selected` — индекс выбранной
/// раскладки (accent-рамка); `gap_pct` — текущая величина зазора в
/// процентах, подставляется в поле.
pub fn build(
    window_count: usize,
    selected: Option<usize>,
    screen: &DipRect,
    gap_pct: u32,
) -> StripBuild {
    // Раскладки заведены на 2..=8 окон. Пока пользователь отметил меньше
    // двух, показывать нечего — но пустая лента выглядит сломанной, и понять
    // по ней, что функция вообще существует, невозможно (живой репорт
    // 2026-08-25: «не вижу ни пресетов, ни окон»). Поэтому до второй отметки
    // показываем набор для двух окон: он объясняет, что здесь будет, а
    // выбранная в нём раскладка всё равно сбросится, когда состав изменится.
    let presets = presets_for(window_count.max(MIN_PRESET_WINDOWS));
    let thumb = thumb_size(screen);
    // Ширина — по ФАКТИЧЕСКОМУ числу миниатюр, а не по потолку: набор
    // короче семи (или пустой) оставлял бы за собой полосу пустой панели
    // во весь экран.
    let shown = presets.len().max(1) as f64;
    let strip_w = shown * thumb + (shown - 1.0) * THUMB_GAP;
    let gap_block = STRIP_GAP + LABEL_GAP + text_size(GAP_LABEL).0 + FIELD_W;
    let w = 2.0 * PAD + strip_w + gap_block;
    let h = 2.0 * PAD + thumb;
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
        thumbs.push(thumb_frame);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::PointerEvent;

    fn screen(w: f64, h: f64) -> DipRect {
        DipRect::new(0.0, 0.0, w, h)
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

    #[test]
    fn all_seven_layouts_fit_any_screen_by_shrinking_not_truncating() {
        for count in 2..=8 {
            for (w, h) in [
                (1920.0, 1080.0),
                (1280.0, 720.0),
                (800.0, 600.0),
                (640.0, 360.0),
                (480.0, 270.0),
            ] {
                let scr = screen(w, h);
                let built = build(count, None, &scr, 5);
                assert_eq!(
                    built.thumbs.len(),
                    7,
                    "count={count} {w}×{h}: все семь раскладок на ленте"
                );
                assert_eq!(built.slots.len(), 7 * count);
                for thumb in &built.thumbs {
                    assert!(
                        thumb.cx - thumb.w / 2.0 >= scr.x - 1e-9,
                        "count={count} {w}×{h}"
                    );
                    assert!(
                        thumb.cx + thumb.w / 2.0 <= scr.x + scr.w + 1e-9,
                        "count={count} {w}×{h}"
                    );
                }
                let f = built.panel.frame();
                let panel_w = f.w;
                assert!(
                    f.cx - f.w / 2.0 >= scr.x - 1e-9 && f.cx + f.w / 2.0 <= scr.x + scr.w + 1e-9,
                    "count={count} {w}×{h}: панель шириной {panel_w} не влезает в экран {w}"
                );
                assert!(
                    built.thumbs[0].w > 0.0,
                    "count={count} {w}×{h}: миниатюры ужаты, но живы"
                );
            }
        }
    }

    #[test]
    fn slot_rects_stay_inside_their_thumbnail_and_never_overlap() {
        for count in 2..=8 {
            for preset_idx in 0..7 {
                let built = build(count, None, &screen(1920.0, 1080.0), 5);
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
        let built = build(3, None, &screen(1920.0, 1080.0), 5);
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

    #[test]
    fn too_few_picked_windows_still_show_the_two_window_layouts() {
        // Пустая лента выглядит сломанной: по ней нельзя понять, что функция
        // вообще есть (живой репорт 2026-08-25). Пока отмечено меньше двух
        // окон, показывается набор для двух — он объясняет, что здесь будет.
        for count in [0, 1, 2] {
            let built = build(count, Some(2), &screen(1920.0, 1080.0), 5);
            assert_eq!(
                built.thumbs.len(),
                presets_for(2).len(),
                "count={count}: обязан показываться набор для двух окон"
            );
            assert!(!built.slots.is_empty(), "count={count}: слоты нарисованы");
        }
    }

    #[test]
    fn a_window_count_beyond_the_table_yields_an_empty_strip_without_panic() {
        // Девять окон в группе не бывает, но конфиг правят руками: молча
        // подсунуть им раскладку на восемь нельзя.
        for count in [9, 100] {
            let built = build(count, Some(2), &screen(1920.0, 1080.0), 5);
            assert!(built.thumbs.is_empty(), "count={count}: миниатюр нет");
            assert!(built.slots.is_empty(), "count={count}: слотов нет");
            let mut out = Vec::new();
            built.panel.draw(&mut out);
            assert!(
                built.panel.widget::<NumericField>(GAP_FIELD_ID).is_some(),
                "count={count}: поле зазора на месте даже без ленты"
            );
        }
    }

    #[test]
    fn degenerate_screen_builds_without_panic() {
        for scr in [
            DipRect::new(0.0, 0.0, 0.0, 0.0),
            DipRect::new(5.0, 5.0, -10.0, -10.0),
            DipRect::new(0.0, 0.0, 20.0, 20.0),
            DipRect::new(0.0, 0.0, 64.0, 48.0),
        ] {
            let built = build(4, Some(2), &scr, 5);
            let f = built.panel.frame();
            assert!(f.w >= 0.0 && f.h >= 0.0, "геометрия неотрицательна: {f:?}");
            let mut out = Vec::new();
            built.panel.draw(&mut out);
            for s in &built.slots {
                assert!(s.rect.w >= 0.0 && s.rect.h >= 0.0, "слот неотрицателен");
            }
        }
    }

    #[test]
    fn strip_hangs_at_the_top_of_the_screen_centered() {
        let scr = screen(1920.0, 1080.0);
        let f = build(2, None, &scr, 5).panel.frame();
        assert_close(f.cy - f.h / 2.0, scr.y + TOP_GAP, "верх панели");
        assert_close(f.cx, scr.x + scr.w / 2.0, "центр по горизонтали");
    }

    #[test]
    fn full_size_strip_keeps_thumbnails_square_and_field_to_the_right() {
        let scr = screen(1920.0, 1080.0);
        let built = build(4, None, &scr, 5);
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
        let last = built.thumbs[6];
        assert!(
            field.cx - field.w / 2.0 > last.cx + last.w / 2.0,
            "поле справа от ленты"
        );
    }

    #[test]
    fn selected_layout_draws_an_accent_ring_and_others_do_not() {
        let built = build(2, Some(1), &screen(1920.0, 1080.0), 5);
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
        let plain = build(2, None, &screen(1920.0, 1080.0), 5);
        assert!(
            fills(&plain.panel, theme::settings::ACCENT).is_empty(),
            "без выбора акцентных полос нет"
        );
    }

    #[test]
    fn every_slot_shows_its_number() {
        let mut out = Vec::new();
        build(4, None, &screen(1920.0, 1080.0), 5)
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

    #[test]
    fn clicking_a_slot_registers_on_that_slot_button_only() {
        let mut built = build(3, None, &screen(1920.0, 1080.0), 5);
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
        let mut built = build(5, None, &screen(1920.0, 1080.0), 5);
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
            let built = build(2, None, &screen(1920.0, 1080.0), gap);
            let field = built
                .panel
                .widget::<NumericField>(GAP_FIELD_ID)
                .expect("поле зазора");
            assert_eq!(field.value(), gap, "зазор {gap} подставлен в поле");
        }
        let built = build(2, None, &screen(1920.0, 1080.0), 99);
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
}
