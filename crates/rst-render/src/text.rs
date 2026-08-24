//! Текст UI: растеризация встроенной гарнитуры Roboto Light на CPU.
//!
//! DirectWrite не используется: строка растрируется в RGBA-битмап
//! ([`rasterize`]), который вызывающий слой заливает в текстуру через
//! [`crate::Device::create_texture_from_rgba`] и кэширует по ключу
//! «строка + цвет + масштаб». Глиф-атлас с UV сейчас невозможен: шейдер
//! спрайта семплирует текстуру целиком; UV-aware шейдер — отдельная
//! аддитивная задача.
//!
//! Гарнитура — `assets/Roboto-Light.ttf`, вшитая в бинарь (лицензия и
//! причина, почему файл лежит в репозитории, а не берётся из системы —
//! `assets/Roboto-LICENSE.txt`). Это тот же Roboto, которым набрано окно
//! настроек (`crates/resticker/ui/fonts/*.woff2`, вес 300), поэтому оверлей
//! и настройки выглядят одним продуктом. До 2026-08-23 здесь был свой
//! битовый шрифт 8×8 на таблице глифов: он не имел ни сглаживания, ни
//! настоящих пропорций, и в тултипах/подписях выглядел скомканным (репорт
//! пользователя с картинкой).
//!
//! Кернинг сознательно не применяется: [`text_size`], [`width_up_to`] и
//! [`rasterize`] обязаны считать позиции ОДИНАКОВО (иначе разъедутся
//! центрирование, каретка поля ввода и усечение длинных строк), а у Roboto
//! кернинг-пары на кегле 12 DIP дают доли пикселя.

use std::sync::OnceLock;

use fontdue::{Font, FontSettings};

/// Встроенная гарнитура (Apache 2.0, см. `assets/Roboto-LICENSE.txt`).
///
/// Публичная: те же байты нужны платформенному слою, чтобы набрать этим же
/// шрифтом контекстное меню трея (`rst_win32::tray::register_menu_font`) —
/// GDI умеет только свои шрифты, не наш растеризатор.
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/Roboto-Light.ttf");

/// Имя семейства встроенной гарнитуры — им её ищет GDI после
/// `AddFontMemResourceEx`.
pub const FONT_FAMILY: &str = "Roboto Light";

/// Кегль текста UI в DIP — тот же 12 px, что у `.button`/`.textInput`
/// в окне настроек (`crates/resticker/ui/styles.css`).
pub const FONT_SIZE_DIP: f64 = 12.0;

/// Высота строки текста в DIP при масштабе 1 — подъём плюс спуск гарнитуры
/// на [`FONT_SIZE_DIP`], округлённые вверх до целого DIP.
///
/// Это ВЫСОТА БИТМАПА, который отдаёт [`rasterize`]: вызывающий слой рисует
/// его в прямоугольник такой же высоты, поэтому число обязано совпадать с
/// реальными метриками (проверяется тестом `line_height_matches_font`).
pub const LINE_HEIGHT: f64 = 15.0;

/// Запас над базовой линией, DIP — см. [`rasterize`].
const TOP_PAD_DIP: f64 = 0.5;

/// Разобранная гарнитура. Разбор ленивый и однократный: `Font::from_bytes`
/// парсит таблицы TTF, а строки растрируются на каждый промах кэша текстур.
fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(
            FONT_BYTES,
            FontSettings {
                // Подсказка растеризатору о рабочем кегле (влияет только на
                // внутренние буферы): берём удвоенный, под масштаб 2.
                scale: (FONT_SIZE_DIP * 2.0) as f32,
                ..FontSettings::default()
            },
        )
        .expect("встроенная гарнитура должна разбираться")
    })
}

/// Метрики строки на кегле `px`: горизонтальные шаги в порядке символов.
fn advances(text: &str, px: f32) -> impl Iterator<Item = f64> + '_ {
    let font = font();
    text.chars()
        .map(move |c| f64::from(font.metrics(c, px).advance_width))
}

/// Размер строки в DIP при масштабе 1: сумма шагов глифов × [`LINE_HEIGHT`].
pub fn text_size(text: &str) -> (f64, f64) {
    (
        advances(text, FONT_SIZE_DIP as f32).sum::<f64>(),
        LINE_HEIGHT,
    )
}

/// Ширина первых `n` символов строки в DIP (для позиции каретки).
pub fn width_up_to(text: &str, n: usize) -> f64 {
    advances(text, FONT_SIZE_DIP as f32).take(n).sum()
}

/// Растрировать строку в RGBA-битмап (straight alpha): цвет постоянный,
/// альфа — покрытие пикселя глифом (сглаживание). `scale` — целочисленный
/// масштаб (под `set_dpi_scale` рендерера: 1 для 100%, 2 для 200%; дробный
/// DPI округляется вверх вызывающим слоем). Premultiply делает
/// [`crate::texture::Texture::from_rgba`] при заливке.
///
/// Возвращает `(pixels, width, height)`; ширина — округлённая вверх сумма
/// шагов, высота — `LINE_HEIGHT × scale`. Для пустой строки битмап
/// `1 × высота` (нулевая ширина запрещена валидацией текстур).
pub fn rasterize(text: &str, color: [u8; 3], scale: u32) -> (Vec<u8>, u32, u32) {
    let scale = scale.max(1);
    let px = FONT_SIZE_DIP as f32 * scale as f32;
    let h = (LINE_HEIGHT * f64::from(scale)).round().max(1.0) as u32;
    let w = advances(text, px)
        .sum::<f64>()
        .ceil()
        .max(1.0)
        .min(f64::from(u32::MAX)) as u32;
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];

    // Базовая линия: подъём гарнитуры от верха битмапа. Метрики строки
    // берём у самой гарнитуры, а не из константы — при смене шрифта
    // достаточно поменять файл.
    let line = font().horizontal_line_metrics(px).unwrap_or_else(|| {
        // Гарнитура без hhea — сюда попасть нельзя, но паниковать в
        // отрисовке нечего: садим базовую линию на 3/4 высоты.
        fontdue::LineMetrics {
            ascent: h as f32 * 0.75,
            descent: -(h as f32 * 0.25),
            line_gap: 0.0,
            new_line_size: h as f32,
        }
    });
    // Полпикселя запаса сверху: у кириллических Ё/Й диакритика чуть выше
    // подъёма гарнитуры (замерено: ink top = 0.13 DIP при ascent 11.13), и
    // без запаса верхний ряд битмапа срезал бы точки.
    let baseline = f64::from(line.ascent) + TOP_PAD_DIP * f64::from(scale);

    let mut pen_x = 0.0f64;
    for c in text.chars() {
        let (metrics, coverage) = font().rasterize(c, px);
        let x0 = (pen_x + f64::from(metrics.xmin)).round() as i64;
        // `ymin` — низ глифа относительно базовой линии, вверх положительно.
        let y0 = (baseline - f64::from(metrics.height as i32 + metrics.ymin)).round() as i64;
        for (i, &a) in coverage.iter().enumerate() {
            if a == 0 {
                continue;
            }
            let x = x0 + (i % metrics.width) as i64;
            let y = y0 + (i / metrics.width) as i64;
            if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) {
                continue;
            }
            let idx = ((y as usize) * (w as usize) + x as usize) * 4;
            // Глифы в строке не перекрываются, но диакритика и наплывы
            // соседних букв возможны — берём максимум покрытия, а не
            // последнюю запись.
            if a > rgba[idx + 3] {
                rgba[idx..idx + 3].copy_from_slice(&color);
                rgba[idx + 3] = a;
            }
        }
        pen_x += f64::from(metrics.advance_width);
    }
    (rgba, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Альфа пикселя битмапа.
    fn alpha(rgba: &[u8], w: u32, x: u32, y: u32) -> u8 {
        rgba[((y * w + x) * 4 + 3) as usize]
    }

    #[test]
    fn line_height_matches_font() {
        let m = font()
            .horizontal_line_metrics(FONT_SIZE_DIP as f32)
            .expect("у Roboto есть hhea");
        let real = f64::from(m.ascent - m.descent);
        assert!(
            real <= LINE_HEIGHT && LINE_HEIGHT - real < 1.0,
            "LINE_HEIGHT={LINE_HEIGHT} должен быть потолком реальной высоты строки {real}"
        );
    }

    #[test]
    fn size_grows_with_text_and_height_is_line_height() {
        let (w0, h0) = text_size("");
        assert_eq!((w0, h0), (0.0, LINE_HEIGHT));
        let (w1, _) = text_size("1");
        let (w3, _) = text_size("100");
        assert!(w1 > 0.0);
        assert!(w3 > w1, "три цифры шире одной: {w3} <= {w1}");
        // Цифры в Roboto табличные — «100» ровно втрое шире «1».
        assert!((w3 - 3.0 * w1).abs() < 0.01, "{w3} != 3 * {w1}");
    }

    #[test]
    fn width_of_prefix_is_monotonic_and_matches_full_width() {
        assert_eq!(width_up_to("123", 0), 0.0);
        assert!(width_up_to("123", 2) > width_up_to("123", 1));
        assert_eq!(width_up_to("123", 99), text_size("123").0);
    }

    #[test]
    fn rasterize_marks_glyph_pixels_and_writes_color() {
        let (rgba, w, h) = rasterize("0", [10, 20, 30], 1);
        assert_eq!(h, LINE_HEIGHT as u32);
        assert_eq!(w, text_size("0").0.ceil() as u32);
        let lit = rgba
            .chunks_exact(4)
            .find(|p| p[3] > 0)
            .expect("глиф должен дать закрашенные пиксели");
        assert_eq!(lit[..3], [10, 20, 30]);
    }

    #[test]
    fn rasterize_is_antialiased() {
        // Признак настоящей гарнитуры против битового шрифта: есть пиксели
        // с частичным покрытием, а не только 0 и 255.
        let (rgba, _, _) = rasterize("Sticker", [255, 255, 255], 2);
        assert!(
            rgba.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255),
            "сглаживания нет — растеризатор работает не так, как ожидалось"
        );
    }

    #[test]
    fn rasterize_scales_with_dpi() {
        let (_, w1, h1) = rasterize("100", [255, 255, 255], 1);
        let (_, w2, h2) = rasterize("100", [255, 255, 255], 2);
        assert_eq!(h2, h1 * 2);
        assert!(
            (i64::from(w2) - i64::from(w1) * 2).abs() <= 2,
            "ширина при масштабе 2 должна быть вдвое больше: {w1} -> {w2}"
        );
    }

    #[test]
    fn rasterize_empty_string_minimal_bitmap() {
        let (rgba, w, h) = rasterize("", [0, 0, 0], 1);
        assert_eq!((w, h), (1, LINE_HEIGHT as u32));
        assert!(rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn glyphs_stay_inside_the_bitmap() {
        // Буквы со спуском и подъёмом (p, y, Й, Ё) обязаны помещаться в
        // LINE_HEIGHT: обрезка сверху/снизу была бы видна как срезанные
        // хвосты.
        for s in ["pygjq", "ЁЙёй", "Wg"] {
            let (rgba, w, h) = rasterize(s, [255, 255, 255], 2);
            let row_lit = |y: u32| (0..w).any(|x| alpha(&rgba, w, x, y) > 0);
            assert!(!row_lit(0), "строка {s:?}: глиф упирается в верх битмапа");
            assert!(
                !row_lit(h - 1),
                "строка {s:?}: глиф упирается в низ битмапа"
            );
        }
    }

    #[test]
    fn latin_cyrillic_and_punctuation_render_non_blank() {
        for s in [
            "Sticker",
            "chrome",
            "Наклейка",
            "ёлка",
            "0:00",
            "1:00:00",
            "8%",
            "Reset scale",
        ] {
            let (rgba, w, h) = rasterize(s, [255, 255, 255], 1);
            assert_eq!(h, LINE_HEIGHT as u32);
            assert_eq!(w, text_size(s).0.ceil() as u32);
            assert!(
                rgba.iter().any(|&b| b != 0),
                "строка {s:?} должна давать закрашенные пиксели"
            );
        }
    }

    #[test]
    fn unknown_glyph_falls_back_without_panic() {
        // Символа нет в Roboto — гарнитура отдаёт .notdef, растеризация
        // обязана пройти без паники и дать ненулевую ширину.
        let (_, w, _) = rasterize("\u{10FFFF}", [255, 255, 255], 1);
        assert!(w >= 1);
    }
}
