//! Текст UI: растеризация встроенной гарнитуры Commissioner на CPU.
//!
//! DirectWrite не используется: строка растрируется в RGBA-битмап
//! ([`rasterize`]), который вызывающий слой заливает в текстуру через
//! [`crate::Device::create_texture_from_rgba`] и кэширует по ключу
//! «строка + цвет + масштаб». Глиф-атлас с UV сейчас невозможен: шейдер
//! спрайта семплирует текстуру целиком; UV-aware шейдер — отдельная
//! аддитивная задача.
//!
//! Гарнитура — `assets/Commissioner-Medium.ttf`, вшитая в бинарь (лицензия —
//! `assets/Commissioner-LICENSE.txt`). Это тот же Commissioner, которым
//! набрано окно настроек (`crates/resticker/ui/fonts/commissioner-*.woff2`),
//! поэтому оверлей и настройки выглядят одним продуктом
//! (`docs/DESIGN_LIQUID_GLASS.md` §6). До 2026-08-29 здесь был Roboto Light,
//! до 2026-08-23 — свой битовый шрифт 8×8.
//!
//! Кернинг сознательно не применяется: [`text_size`], [`width_up_to`] и
//! [`rasterize`] обязаны считать позиции ОДИНАКОВО (иначе разъедутся
//! центрирование, каретка поля ввода и усечение длинных строк), а у
//! Commissioner кернинг-пары на кегле 12 DIP дают доли пикселя.
//!
//! Свечение текста (§2.3 дизайн-дока) запекается прямо в растр: глифы
//! ложатся поверх собственной размытой белой копии. Растр из-за этого шире и
//! выше строки на [`GLOW_PAD_DIP`] с каждой стороны, а [`text_size`]
//! ОСТАЁТСЯ коробкой вёрстки — раздувает прямоугольник назначения
//! единственное место сшивки (`overlay_manager::prims_to_sprites`). Так ни
//! один сборщик панели про гало не знает и своих отступов не меняет.

use std::sync::OnceLock;

use fontdue::{Font, FontSettings};

/// Встроенная гарнитура (SIL OFL, см. `assets/Commissioner-LICENSE.txt`).
///
/// Публичная: те же байты нужны платформенному слою, чтобы набрать этим же
/// шрифтом контекстное меню трея (`rst_win32::tray::register_menu_font`) —
/// GDI умеет только свои шрифты, не наш растеризатор.
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/Commissioner-Medium.ttf");

/// Имя семейства встроенной гарнитуры — им её ищет GDI после
/// `AddFontMemResourceEx`. У статического начертания Commissioner имя
/// семейства (name ID 1) включает вес: «Commissioner Medium», а не
/// «Commissioner» (замерено по таблице name 2026-08-29). GDI ищет именно по
/// нему, поэтому здесь полное имя.
pub const FONT_FAMILY: &str = "Commissioner Medium";

/// Кегль текста UI в DIP.
///
/// Остаётся 12.0 при переходе с Roboto Light на Commissioner Medium, хотя
/// дизайн-док называет 12.5: у Commissioner подъём 1.017 em против 0.928 em
/// у Roboto, то есть на одном номинальном кегле новая гарнитура и так
/// рисует примерно на 10 % крупнее. Кегль 12.5 поднял бы [`LINE_HEIGHT`]
/// с 15 до 16 DIP и сдвинул бы геометрию каждой строки в каждой панели —
/// цена, за которую взамен не видно ничего.
pub const FONT_SIZE_DIP: f64 = 12.0;

/// Запас вокруг строки под свечение, DIP с каждой стороны (§2.3: радиус
/// гало 2.0 DIP).
pub const GLOW_PAD_DIP: f64 = 2.0;

/// Пиковая непрозрачность гало под глифом (§2.3 `TEXT_GLOW`).
const GLOW_ALPHA: f64 = 0.26;

/// Цвет гало — белый: свет, а не тень.
const GLOW_COLOR: [u8; 3] = [0xff, 0xff, 0xff];

/// Запас под свечение в физических пикселях при масштабе `scale`.
///
/// Нужен вызывающему слою: растр [`rasterize`] на столько же больше коробки
/// [`text_size`] с каждой стороны, и прямоугольник назначения обязан
/// раздуться ровно на эту величину, иначе гало сожмёт глифы.
pub fn glow_pad_px(scale: u32) -> u32 {
    (GLOW_PAD_DIP * f64::from(scale.max(1))).round().max(0.0) as u32
}

/// Высота строки текста в DIP при масштабе 1 — [`TOP_PAD_DIP`] плюс подъём
/// плюс спуск гарнитуры на [`FONT_SIZE_DIP`], округлённые вверх до целого
/// DIP.
///
/// Это ВЫСОТА КОРОБКИ, в которую вызывающий слой кладёт строку, поэтому
/// число обязано покрывать реальные метрики (проверяется тестом
/// `line_height_matches_font`). Выросла с 15 до 16 при переходе на
/// Commissioner: у него спуск 2.47 DIP против 2.93 у Roboto, но подъём
/// 12.20 против 11.13 — вместе с запасом над базовой линией строка перестала
/// помещаться в 15, и хвосты «p»/«y» срезало (поймано тестом
/// `glyphs_stay_inside_the_bitmap`).
pub const LINE_HEIGHT: f64 = 16.0;

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

/// Ширина слота цифры на кегле `px` — шаг самой широкой цифры гарнитуры.
///
/// Синтетический `tabular-nums`: у Commissioner цифры пропорциональные
/// (замерено — «1» уже «0» примерно на треть), и без выравнивания счётчик
/// закреплённых окон, проценты непрозрачности и таймер видео дёргались бы
/// по ширине на каждой смене цифры. OpenType-фича `tnum` тут недоступна:
/// fontdue не применяет фичи, только метрики глифов.
fn digit_slot(px: f32) -> f64 {
    let font = font();
    ('0'..='9')
        .map(|d| f64::from(font.metrics(d, px).advance_width))
        .fold(0.0f64, f64::max)
}

/// Метрики строки на кегле `px`: горизонтальные шаги в порядке символов.
/// Цифры получают общий слот [`digit_slot`], остальные символы — свой шаг.
fn advances(text: &str, px: f32) -> impl Iterator<Item = f64> + '_ {
    let font = font();
    let slot = digit_slot(px);
    text.chars().map(move |c| {
        if c.is_ascii_digit() {
            slot
        } else {
            f64::from(font.metrics(c, px).advance_width)
        }
    })
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
/// Возвращает `(pixels, width, height)`. Размер — коробка строки
/// (`text_size × scale`), РАЗДУТАЯ на [`glow_pad_px`] с каждой стороны под
/// свечение: гало живёт за пределами глифов, и без запаса его срезало бы.
/// Для пустой строки битмап минимальный (нулевая ширина запрещена
/// валидацией текстур).
pub fn rasterize(text: &str, color: [u8; 3], scale: u32) -> (Vec<u8>, u32, u32) {
    let scale = scale.max(1);
    let px = FONT_SIZE_DIP as f32 * scale as f32;
    let pad = glow_pad_px(scale);
    let core_h = (LINE_HEIGHT * f64::from(scale)).round().max(1.0) as u32;
    let core_w = advances(text, px)
        .sum::<f64>()
        .ceil()
        .max(1.0)
        .min(f64::from(u32::MAX)) as u32;
    let (w, h) = (core_w + 2 * pad, core_h + 2 * pad);
    // Покрытие глифов отдельным слоем: по нему считается и гало (размытая
    // копия), и сами буквы. Смешивать сразу в RGBA нельзя — размывать
    // пришлось бы уже покрашенное.
    let mut coverage = vec![0u8; (w as usize) * (h as usize)];

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

    let slot = digit_slot(px);
    let mut pen_x = 0.0f64;
    for c in text.chars() {
        let (metrics, glyph) = font().rasterize(c, px);
        // Узкая цифра ставится по центру общего слота — иначе «1» липла бы
        // к левому краю своей ячейки и колонка чисел выглядела бы рваной.
        let (step, center) = if c.is_ascii_digit() {
            let natural = f64::from(metrics.advance_width);
            (slot, (slot - natural) / 2.0)
        } else {
            (f64::from(metrics.advance_width), 0.0)
        };
        let x0 = (pen_x + center + f64::from(metrics.xmin)).round() as i64 + i64::from(pad);
        // `ymin` — низ глифа относительно базовой линии, вверх положительно.
        let y0 = (baseline - f64::from(metrics.height as i32 + metrics.ymin)).round() as i64
            + i64::from(pad);
        for (i, &a) in glyph.iter().enumerate() {
            if a == 0 {
                continue;
            }
            let x = x0 + (i % metrics.width) as i64;
            let y = y0 + (i / metrics.width) as i64;
            if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) {
                continue;
            }
            let idx = (y as usize) * (w as usize) + x as usize;
            // Глифы в строке не перекрываются, но диакритика и наплывы
            // соседних букв возможны — берём максимум покрытия, а не
            // последнюю запись.
            coverage[idx] = coverage[idx].max(a);
        }
        pen_x += step;
    }

    compose_with_glow(&coverage, w, h, color, pad)
}

/// Собрать RGBA (straight alpha) из покрытия глифов: сначала белое гало —
/// размытая копия покрытия силой [`GLOW_ALPHA`], затем сами буквы поверх.
///
/// Размытие — два прохода коробочного фильтра радиуса `pad`: свёртка двух
/// коробок уже неотличима от гауссианы на таких радиусах (2–4 px), а стоит
/// линейно от размера и не требует таблиц ядра.
fn compose_with_glow(
    coverage: &[u8],
    w: u32,
    h: u32,
    color: [u8; 3],
    pad: u32,
) -> (Vec<u8>, u32, u32) {
    let (wu, hu) = (w as usize, h as usize);
    let mut rgba = vec![0u8; wu * hu * 4];
    let halo = if pad == 0 {
        Vec::new()
    } else {
        let once = box_blur(coverage, wu, hu, pad as usize);
        box_blur(&once, wu, hu, pad as usize)
    };

    for i in 0..wu * hu {
        let glyph_a = f64::from(coverage[i]) / 255.0;
        let halo_a = if halo.is_empty() {
            0.0
        } else {
            f64::from(halo[i]) / 255.0 * GLOW_ALPHA
        };
        // Гало под буквой смысла не имеет — оно светит наружу; поэтому
        // вклад гало берётся с весом (1 - alpha глифа), как обычное
        // straight-alpha наложение «глиф над гало».
        let under = halo_a * (1.0 - glyph_a);
        let out_a = glyph_a + under;
        if out_a <= 0.0 {
            continue;
        }
        let idx = i * 4;
        for ch in 0..3 {
            let mixed =
                (f64::from(color[ch]) * glyph_a + f64::from(GLOW_COLOR[ch]) * under) / out_a;
            rgba[idx + ch] = mixed.round().clamp(0.0, 255.0) as u8;
        }
        rgba[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    (rgba, w, h)
}

/// Коробочное размытие одноканального растра радиусом `r` (два прохода:
/// горизонтальный, затем вертикальный).
fn box_blur(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let span = (2 * r + 1) as f64;
    let mut horiz = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            let sum: u32 = (lo..=hi).map(|i| u32::from(src[y * w + i])).sum();
            horiz[y * w + x] = (f64::from(sum) / span).round().min(255.0) as u8;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let lo = y.saturating_sub(r);
        let hi = (y + r).min(h - 1);
        for x in 0..w {
            let sum: u32 = (lo..=hi).map(|j| u32::from(horiz[j * w + x])).sum();
            out[y * w + x] = (f64::from(sum) / span).round().min(255.0) as u8;
        }
    }
    out
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
            .expect("у Commissioner есть hhea");
        // В коробку обязан помещаться не только подъём со спуском, но и
        // запас над базовой линией: без него хвосты и диакритика срезаются.
        let real = f64::from(m.ascent - m.descent) + TOP_PAD_DIP;
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
        // Цифры выровнены в общий слот (`digit_slot`) — «100» ровно втрое
        // шире «1», хотя в самой гарнитуре они пропорциональные.
        assert!((w3 - 3.0 * w1).abs() < 0.01, "{w3} != 3 * {w1}");
        // И «111» ровно той же ширины, что «000»: иначе счётчики дёргались бы.
        assert!((text_size("111").0 - text_size("000").0).abs() < 0.01);
    }

    #[test]
    fn width_of_prefix_is_monotonic_and_matches_full_width() {
        assert_eq!(width_up_to("123", 0), 0.0);
        assert!(width_up_to("123", 2) > width_up_to("123", 1));
        assert_eq!(width_up_to("123", 99), text_size("123").0);
    }

    #[test]
    fn rasterize_marks_glyph_pixels_and_writes_color() {
        let pad = glow_pad_px(1);
        let (rgba, w, h) = rasterize("0", [10, 20, 30], 1);
        assert_eq!(h, LINE_HEIGHT as u32 + 2 * pad);
        assert_eq!(w, text_size("0").0.ceil() as u32 + 2 * pad);
        // Первый непрозрачный пиксель теперь принадлежит гало (оно светит
        // наружу), поэтому цвет проверяем на самом плотном — это глиф.
        let lit = rgba
            .chunks_exact(4)
            .max_by_key(|p| p[3])
            .expect("глиф должен дать закрашенные пиксели");
        assert_eq!(lit[3], 255, "у глифа должен быть полностью плотный пиксель");
        assert_eq!(lit[..3], [10, 20, 30]);
    }

    #[test]
    fn glow_surrounds_the_glyphs() {
        // Свечение (§2.3): вокруг букв обязан быть ореол — пиксели с
        // частичной альфой ЗА пределами коробки строки, которых у
        // растеризатора без гало не было вовсе.
        let pad = glow_pad_px(1);
        assert!(pad > 0, "запас под свечение не может быть нулевым");
        let (rgba, w, h) = rasterize("W", [255, 255, 255], 1);
        let row_has = |y: u32, min: u8| (0..w).any(|x| alpha(&rgba, w, x, y) > min);
        // Верх чернил глифа: выше него не должно быть ничего, кроме гало.
        let ink_top = (0..h)
            .find(|&y| row_has(y, 200))
            .expect("глиф не нарисован");
        assert!(ink_top > 0, "глиф упёрся в самый верх растра");
        let glow_above = (0..ink_top).any(|y| row_has(y, 0));
        assert!(glow_above, "над буквой нет свечения");
        // И оно мягкое, а не заливка: над буквой нет плотных пикселей.
        assert!(
            !(0..ink_top).any(|y| row_has(y, 200)),
            "свечение должно быть мягким, а не сплошной заливкой"
        );
        let _ = pad;
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
        let pad = glow_pad_px(1);
        let (rgba, w, h) = rasterize("", [0, 0, 0], 1);
        assert_eq!((w, h), (1 + 2 * pad, LINE_HEIGHT as u32 + 2 * pad));
        // Нет глифов — нет и гало: пустой растр остаётся полностью прозрачным.
        assert!(rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn glyphs_stay_inside_the_bitmap() {
        // Буквы со спуском и подъёмом (p, y, Й, Ё) обязаны помещаться в
        // LINE_HEIGHT: обрезка сверху/снизу была бы видна как срезанные
        // хвосты.
        // Проверяются КРАЙНИЕ СТРОКИ КОРОБКИ (без полосы запаса под гало):
        // именно их срезала бы неверная базовая линия. В самой полосе запаса
        // пиксели есть всегда — это свечение.
        let pad = glow_pad_px(2);
        for s in ["pygjq", "ЁЙёй", "Wg"] {
            let (rgba, w, h) = rasterize(s, [255, 255, 255], 2);
            let ink_in_row = |y: u32| (0..w).any(|x| alpha(&rgba, w, x, y) > 128);
            assert!(
                !ink_in_row(pad),
                "строка {s:?}: глиф упирается в верх коробки"
            );
            assert!(
                !ink_in_row(h - pad - 1),
                "строка {s:?}: глиф упирается в низ коробки"
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
            let pad = glow_pad_px(1);
            let (rgba, w, h) = rasterize(s, [255, 255, 255], 1);
            assert_eq!(h, LINE_HEIGHT as u32 + 2 * pad);
            assert_eq!(w, text_size(s).0.ceil() as u32 + 2 * pad);
            assert!(
                rgba.iter().any(|&b| b != 0),
                "строка {s:?} должна давать закрашенные пиксели"
            );
        }
    }

    #[test]
    fn unknown_glyph_falls_back_without_panic() {
        // Символа нет в Commissioner — гарнитура отдаёт .notdef, растеризация
        // обязана пройти без паники и дать ненулевую ширину.
        let (_, w, _) = rasterize("\u{10FFFF}", [255, 255, 255], 1);
        assert!(w >= 1);
    }
}
