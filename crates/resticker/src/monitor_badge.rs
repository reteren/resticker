//! Бейдж с номером монитора (T7) — тот же смысл, что у кнопки Identify в
//! настройках Windows: при открытом меню редактирования групп в левом
//! верхнем углу КАЖДОГО монитора висит крупная цифра, и по ней видно, на
//! каком экране ты сейчас работаешь.
//!
//! Чистый билдер, как `cursor_panel`/`preset_picker`: на вход — рабочая
//! область монитора в DIP (уже без панели задач; `rcWork` со слоя rst-win32
//! в DIP переводит координатор) и номер, на выход — [`Panel`]. Никакого
//! состояния и никакого Win32.
//!
//! Цифра — растровый примитив [`Primitive::Rgba`], а не [`Primitive::Text`]:
//! конвейер текста растрирует гарнитуру на фиксированном кегле 12 DIP
//! (`rst_render::rasterize`, `FONT_SIZE_DIP` в rst-render), и сколько ни
//! растягивай прямоугольник текстового примитива — получится размытая
//! надпись, а не крупная цифра (тот же вывод, что у комментария про
//! растяжение подписи кнопки в `widgets.rs`). `Rgba` же растягивает любой
//! битмап на свой прямоугольник — на этом живут скруглённые углы панелей
//! (`settings_frame_radius`) — поэтому цифра растрируется во встроенной
//! гарнитуре на большом кегле ([`DIGIT_RASTER_SCALE`]) и рисуется с
//! сохранением пропорций битмапа.
//!
//! Оформление — стилистика окна настроек, как у всех панелей режима
//! редактирования (VGUI, скруглённые углы; запрос пользователя 2026-08-23 —
//! «весь UI поверх экрана читается как одно окно продукта»). Полупрозрачный
//! [`theme::settings::BG`] на [`theme::settings::BG_OPACITY`] поверх светлого
//! десктопа даёт примерно тот же серый, что бейдж Identify у Windows
//! (чёрный при ~60 % поверх белого — оба выходят в район 0x66–0x8F), а
//! белая цифра [`theme::settings::TEXT`] читается на нём с любого расстояния.
//! Отдельную «тёмную» заливку не вводим: скруглённый полупрозрачный фон в
//! теме ровно один, и уходить от палитры продукта ради оттенка бейджа —
//! значит выбить его из остального UI.

use rst_core::hittest::DipRect;
use rst_render::{Box2D, Panel, Primitive, Widget, WidgetId, WidgetStyle, rasterize, theme};

/// Идентификатор панели бейджа. Диапазон 900+ в оверлее свободен (баннер
/// предупреждений — 901). Бейджи разных мониторов — отдельные объекты
/// `Panel` с одним id: id используется только для опроса виджетов ВНУТРИ
/// панели, и коллизий между мониторами нет (тот же приём, что у модала
/// подтверждения с фиксированными id на любом мониторе).
pub const BADGE_PANEL_ID: WidgetId = 902;
/// Идентификатор виджета цифры внутри панели (для опроса; не интерактивен).
const DIGIT_ID: WidgetId = 903;

/// Сторона квадрата бейджа, DIP. Windows рисует Identify примерно в восьмую
/// часть высоты монитора (для 1080p это ~135 px); фиксированные 112 DIP дают
/// тот же порядок на любом экране и не привязывают размер к разрешению —
/// цифра остаётся крупным элементом, а не подписью.
pub const BADGE_SIZE: f64 = 112.0;
/// Отступ бейджа от левого верхнего угла рабочей области, DIP.
const MARGIN: f64 = 16.0;
/// Высота цифры как доля стороны бейджа (у Windows — примерно те же 70 %).
const DIGIT_FRACTION: f64 = 0.7;
/// Масштаб растра цифры: высота битмапа `LINE_HEIGHT × DIGIT_RASTER_SCALE`
/// = 300 пикселей. На мониторе цифра занимает `DIGIT_FRACTION × BADGE_SIZE`
/// ≈ 78 DIP, а физический размер прямоугольника — ещё и DPI-масштаб
/// (вплоть до ~3 на 4K-ноутбуках): 300 пикселей с запасом покрывают 78 DIP
/// до ~380 %, при меньшем DPI растр уменьшается, а уменьшение чёткое
/// (увеличение — мыльное). Выше порога цифра слегка смягчается, но не
/// ломается, а такие масштабы — экзотика.
const DIGIT_RASTER_SCALE: u32 = 20;

/// База ключа кэша растров цифр. Кэш вызывающего слоя ключуется по
/// `(key, width, height)` (`UiTextureCache::rgba_texture`): у разных номеров
/// разные ключ и ширина, пересечений нет; база вне диапазона ключей углов
/// скругления (`SETTINGS_CORNER_KEY` в rst-render).
const BADGE_KEY_BASE: u64 = 0xBAD6_2026_0000_0000;

/// Сторона бейджа под рабочую область: полный размер, пока помещается с
/// отступами [`MARGIN`] с обеих сторон, иначе — ужатый до области (очень
/// маленький монитор не должен ни вылезать за края, ни залезать под панель
/// задач). Вырожденная или отрицательная область даёт 0 — невидимый бейдж
/// нулевого размера, рендерер такие спрайты пропускает; паники нет.
fn badge_side(work_area: &DipRect) -> f64 {
    let max_w = (work_area.w - 2.0 * MARGIN).max(0.0);
    let max_h = (work_area.h - 2.0 * MARGIN).max(0.0);
    BADGE_SIZE.min(max_w).min(max_h)
}

/// Собрать бейдж номера `number` в рабочей области `work_area` (DIP, без
/// панели задач): скруглённый полупрозрачный квадрат в левом верхнем углу
/// с отступом [`MARGIN`] и крупная светлая цифра по центру.
pub fn build(work_area: &DipRect, number: u32) -> Panel {
    let side = badge_side(work_area);
    let cx = work_area.x + MARGIN + side / 2.0;
    let cy = work_area.y + MARGIN + side / 2.0;
    let frame = Box2D {
        cx,
        cy,
        w: side,
        h: side,
        rotation: 0.0,
    };
    let mut panel = Panel::new(BADGE_PANEL_ID, frame)
        .with_style(WidgetStyle::Settings)
        .with_corner_radius(theme::settings::CORNER_RADIUS);

    let label = number.to_string();
    let (rgba, w_px, h_px) = rasterize(&label, theme::settings::TEXT, DIGIT_RASTER_SCALE);
    // Прямоугольник цифры сохраняет соотношение сторон битмапа — иначе
    // растяжение по одной оси раздавило бы глиф.
    let digit_h = side * DIGIT_FRACTION;
    let digit_w = digit_h * w_px as f64 / h_px as f64;
    panel.add_widget(BadgeDigit {
        id: DIGIT_ID,
        rect: Box2D {
            cx,
            cy,
            w: digit_w,
            h: digit_h,
            rotation: 0.0,
        },
        key: BADGE_KEY_BASE + u64::from(number),
        width: w_px,
        height: h_px,
        rgba,
    });
    panel
}

/// Виджет цифры бейджа — неинтерактивный (клики сквозь него, как у
/// [`rst_render::Label`]): единственная работа — отдать готовый растровый
/// примитив [`Primitive::Rgba`]. Растровый, а не текстовый — см. доккомент
/// модуля: крупный текст в этом пайплайне бывает только растром.
struct BadgeDigit {
    id: WidgetId,
    rect: Box2D,
    key: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Widget for BadgeDigit {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.rect = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Rgba {
            rect: self.rect,
            key: self.key,
            width: self.width,
            height: self.height,
            rgba: self.rgba.clone(),
            opacity: 1.0,
        });
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

    fn work(w: f64, h: f64) -> DipRect {
        DipRect::new(0.0, 0.0, w, h)
    }

    fn assert_close(actual: f64, expected: f64, ctx: &str) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "{ctx}: ожидалось {expected}, получено {actual}"
        );
    }

    /// Растровый примитив цифры. Другие `Rgba` в выводе панели — углы
    /// скругления (`settings_frame_radius`) с ключами из rst-render, они
    /// лежат ниже [`BADGE_KEY_BASE`].
    fn digit_prim(panel: &Panel) -> Primitive {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .find(|p| matches!(p, Primitive::Rgba { key, .. } if *key >= BADGE_KEY_BASE))
            .expect("в панели есть растровая цифра")
    }

    #[test]
    fn badge_stays_inside_work_area_on_any_monitor_size() {
        for (w, h) in [
            (3840.0, 2160.0),
            (2560.0, 1440.0),
            (1920.0, 1080.0),
            (1366.0, 768.0),
            (800.0, 600.0),
            (320.0, 240.0),
            (150.0, 120.0),
        ] {
            let area = work(w, h);
            let mut out = Vec::new();
            build(&area, 1).draw(&mut out);
            for prim in &out {
                let rect = match prim {
                    Primitive::Fill { rect, .. }
                    | Primitive::Icon { rect, .. }
                    | Primitive::Rgba { rect, .. }
                    | Primitive::Text { rect, .. } => rect,
                };
                assert!(
                    rect.cx - rect.w / 2.0 >= area.x - 1e-9
                        && rect.cx + rect.w / 2.0 <= area.x + area.w + 1e-9
                        && rect.cy - rect.h / 2.0 >= area.y - 1e-9
                        && rect.cy + rect.h / 2.0 <= area.y + area.h + 1e-9,
                    "{w}×{h}: примитив {rect:?} вылез за рабочую область"
                );
            }
        }
    }

    #[test]
    fn badge_anchors_to_top_left_of_a_second_monitor_origin() {
        // Монитор со смещённым началом координат: якорь считается от его
        // собственной рабочей области, а не от нуля.
        let area = DipRect::new(-1920.0, 357.0, 1920.0, 1080.0);
        let f = build(&area, 3).frame();
        assert_close(f.cx - f.w / 2.0, area.x + MARGIN, "левый край");
        assert_close(f.cy - f.h / 2.0, area.y + MARGIN, "верх");
        assert_close(f.w, BADGE_SIZE, "полный размер");
    }

    #[test]
    fn digit_is_centered_in_the_badge_and_keeps_bitmap_proportions() {
        let panel = build(&work(1920.0, 1080.0), 9);
        let f = panel.frame();
        let Primitive::Rgba {
            rect,
            width,
            height,
            ..
        } = digit_prim(&panel)
        else {
            panic!("цифра — Rgba");
        };
        assert_close(rect.cx, f.cx, "цифра по центру по X");
        assert_close(rect.cy, f.cy, "цифра по центру по Y");
        assert_close(rect.h, f.h * DIGIT_FRACTION, "высота цифры");
        assert_close(
            rect.w / rect.h,
            f64::from(width) / f64::from(height),
            "пропорции битмапа не искажены",
        );
        assert!(rect.w <= f.w && rect.h <= f.h, "цифра внутри бейджа");
    }

    #[test]
    fn badge_number_is_reflected_in_the_digit_bitmap() {
        for n in [1u32, 2, 12] {
            let panel = build(&work(1920.0, 1080.0), n);
            let Primitive::Rgba {
                key,
                width,
                height,
                rgba,
                ..
            } = digit_prim(&panel)
            else {
                panic!("цифра — Rgba");
            };
            assert_eq!(key, BADGE_KEY_BASE + u64::from(n), "свой ключ кэша");
            let (expected, ew, eh) =
                rasterize(&n.to_string(), theme::settings::TEXT, DIGIT_RASTER_SCALE);
            assert_eq!((width, height), (ew, eh), "размеры битмапа");
            assert_eq!(&rgba, &expected, "битмап — растризация номера {n}");
        }
    }

    #[test]
    fn badge_shrinks_to_fit_small_work_areas() {
        // 150×120: по горизонтали помещается 118, по вертикали 88 — берём
        // минимум, бейдж не вылезает за края.
        assert_close(
            build(&work(150.0, 120.0), 1).frame().w,
            88.0,
            "ужатый бейдж",
        );
        // Узкий монитор (например, вертикально повёрнутый планшет) — тоже
        // ужимается по своей короткой стороне.
        assert_close(build(&work(90.0, 1600.0), 1).frame().w, 58.0, "узкий бейдж");
    }

    #[test]
    fn degenerate_work_area_builds_without_panic() {
        for area in [
            DipRect::new(0.0, 0.0, 0.0, 0.0),
            DipRect::new(5.0, 5.0, -10.0, -10.0),
            DipRect::new(0.0, 0.0, 20.0, 20.0),
            DipRect::new(0.0, 0.0, 64.0, 48.0),
        ] {
            let panel = build(&area, 5);
            let f = panel.frame();
            assert!(f.w >= 0.0 && f.h >= 0.0, "геометрия неотрицательна: {f:?}");
            let mut out = Vec::new();
            panel.draw(&mut out);
        }
    }
}
