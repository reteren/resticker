//! Встроенный битовый шрифт 8×8 для immediate-mode UI (M2_UI_NOTES.md,
//! раздел 8, пункт 2: «либо встроенный битовый шрифт»).
//!
//! DirectWrite не используется: растеризация делается на CPU в RGBA-битмап
//! ([`rasterize`]), который вызывающий слой заливает в текстуру через
//! [`crate::Device::create_texture_from_rgba`] и кэширует по ключу
//! «строка + цвет + масштаб». Глиф-атлас с UV сейчас невозможен: шейдер
//! спрайта семплирует текстуру целиком; UV-aware шейдер — отдельная
//! аддитивная задача.
//!
//! Набор глифов — минимальный под нужды M2: цифры (числовое поле, значение
//! ползунка), `%`, `.`, `+`, `-`, пробел. Неизвестный символ рисуется
//! контурным квадратом (fallback). Расширение набора = новая строка в
//! таблице GLYPHS; для русских подписей панели 3.8 шрифт придётся
//! дополнить кириллицей либо заменить на DirectWrite.

/// Глиф 8×8: по байту на строку сверху вниз, старший бит — левый пиксель.
/// `advance` — шаг по X в DIP при масштабе 1 (ширина глифа + межбуквенный
/// зазор); шрифт не моноширинный, метрика суммируется по глифам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub rows: [u8; 8],
    pub advance: u8,
}

/// Высота строки текста в DIP при масштабе 1.
pub const LINE_HEIGHT: f64 = 8.0;

/// Таблица «символ → глиф».
const GLYPHS: &[(char, Glyph)] = &[
    (
        '0',
        Glyph {
            rows: [
                0b0111_0000,
                0b1000_1000,
                0b1001_1000,
                0b1010_1000,
                0b1100_1000,
                0b1000_1000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '1',
        Glyph {
            rows: [
                0b0010_0000,
                0b0110_0000,
                0b0010_0000,
                0b0010_0000,
                0b0010_0000,
                0b0010_0000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '2',
        Glyph {
            rows: [
                0b0111_0000,
                0b1000_1000,
                0b0000_1000,
                0b0011_0000,
                0b0100_0000,
                0b1000_0000,
                0b1111_1000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '3',
        Glyph {
            rows: [
                0b0111_0000,
                0b1000_1000,
                0b0000_1000,
                0b0011_0000,
                0b0000_1000,
                0b1000_1000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '4',
        Glyph {
            rows: [
                0b0001_0000,
                0b0011_0000,
                0b0101_0000,
                0b1001_0000,
                0b1111_1000,
                0b0001_0000,
                0b0001_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '5',
        Glyph {
            rows: [
                0b1111_1000,
                0b1000_0000,
                0b1111_0000,
                0b0000_1000,
                0b0000_1000,
                0b1000_1000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '6',
        Glyph {
            rows: [
                0b0111_0000,
                0b1000_0000,
                0b1111_0000,
                0b1000_1000,
                0b1000_1000,
                0b1000_1000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '7',
        Glyph {
            rows: [
                0b1111_1000,
                0b0000_1000,
                0b0001_0000,
                0b0010_0000,
                0b0010_0000,
                0b0010_0000,
                0b0010_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '8',
        Glyph {
            rows: [
                0b0111_0000,
                0b1000_1000,
                0b1000_1000,
                0b0111_0000,
                0b1000_1000,
                0b1000_1000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '9',
        Glyph {
            rows: [
                0b0111_0000,
                0b1000_1000,
                0b1000_1000,
                0b1000_1000,
                0b0111_1000,
                0b0000_1000,
                0b0111_0000,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '%',
        Glyph {
            rows: [
                0b1100_1000,
                0b1101_0000,
                0b0010_0000,
                0b0100_0000,
                0b0101_1000,
                0b1001_1000,
                0,
                0,
            ],
            advance: 6,
        },
    ),
    (
        '.',
        Glyph {
            rows: [0, 0, 0, 0, 0, 0b0010_0000, 0b0010_0000, 0],
            advance: 3,
        },
    ),
    (
        '-',
        Glyph {
            rows: [0, 0, 0, 0b1111_1000, 0, 0, 0, 0],
            advance: 5,
        },
    ),
    (
        '+',
        Glyph {
            rows: [0, 0, 0b0010_0000, 0b0111_0000, 0b0010_0000, 0, 0, 0],
            advance: 6,
        },
    ),
    (
        ' ',
        Glyph {
            rows: [0; 8],
            advance: 4,
        },
    ),
];

/// Глиф для неизвестного символа: контурный квадрат.
const FALLBACK: Glyph = Glyph {
    rows: [
        0,
        0b1111_1000,
        0b1000_1000,
        0b1000_1000,
        0b1000_1000,
        0b1111_1000,
        0,
        0,
    ],
    advance: 6,
};

/// Глиф символа (неизвестный — контурный квадрат).
pub fn glyph_for(c: char) -> Glyph {
    GLYPHS
        .iter()
        .find(|(ch, _)| *ch == c)
        .map_or(FALLBACK, |(_, g)| *g)
}

/// Размер строки в DIP при масштабе 1: сумма шагов глифов × [`LINE_HEIGHT`].
pub fn text_size(text: &str) -> (f64, f64) {
    (
        text.chars().map(|c| f64::from(glyph_for(c).advance)).sum(),
        LINE_HEIGHT,
    )
}

/// Ширина первых `n` символов строки в DIP (для позиции каретки).
pub fn width_up_to(text: &str, n: usize) -> f64 {
    text.chars()
        .take(n)
        .map(|c| f64::from(glyph_for(c).advance))
        .sum()
}

/// Растрировать строку в RGBA-битмап (straight alpha): включённые пиксели —
/// `color` с альфой 255, фон — прозрачный. `scale` — целочисленный масштаб
/// (под `set_dpi_scale` рендерера: 1 для 100%, 2 для 200%; дробный DPI
/// округляется вверх вызывающим слоем). Premultiply делает
/// [`crate::texture::Texture::from_rgba`] при заливке.
///
/// Возвращает `(pixels, width, height)`; для пустой строки — битмап 1×8
/// (нулевая ширина запрещена валидацией текстур).
pub fn rasterize(text: &str, color: [u8; 3], scale: u32) -> (Vec<u8>, u32, u32) {
    let scale = scale.max(1);
    let w = (text
        .chars()
        .map(|c| u32::from(glyph_for(c).advance))
        .sum::<u32>()
        * scale)
        .max(1);
    let h = 8 * scale;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let mut pen_x = 0u32;
    for c in text.chars() {
        let glyph = glyph_for(c);
        for (row, &bits) in glyph.rows.iter().enumerate() {
            for col in 0..8u32 {
                if bits & (0x80 >> col) == 0 {
                    continue;
                }
                // Целочисленный масштаб — блоком scale×scale, без фильтрации.
                for sy in 0..scale {
                    for sx in 0..scale {
                        let x = pen_x + col * scale + sx;
                        let y = row as u32 * scale + sy;
                        let i = ((y * w + x) * 4) as usize;
                        rgba[i..i + 3].copy_from_slice(&color);
                        rgba[i + 3] = 0xff;
                    }
                }
            }
        }
        pen_x += u32::from(glyph.advance) * scale;
    }
    (rgba, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_lookup_known_and_fallback() {
        assert_eq!(glyph_for('0').advance, 6);
        assert_eq!(glyph_for(' ').advance, 4);
        // Кириллица пока не входит в набор — контурный квадрат.
        assert_eq!(glyph_for('Ж'), FALLBACK);
        assert_eq!(glyph_for('Ж').rows[1], 0b1111_1000);
    }

    #[test]
    fn size_is_sum_of_advances() {
        assert_eq!(text_size("100"), (18.0, 8.0));
        assert_eq!(text_size("8%"), (12.0, 8.0));
        assert_eq!(text_size(""), (0.0, 8.0));
    }

    #[test]
    fn width_of_prefix() {
        assert_eq!(width_up_to("123", 0), 0.0);
        assert_eq!(width_up_to("123", 2), 12.0);
        assert_eq!(width_up_to("123", 99), 18.0);
    }

    #[test]
    fn rasterize_marks_glyph_pixels() {
        let (rgba, w, h) = rasterize("0", [255, 255, 255], 1);
        assert_eq!((w, h), (6, 8));
        let alpha = |x: u32, y: u32| rgba[((y * w + x) * 4 + 3) as usize];
        assert_eq!(alpha(1, 0), 0xff, "верхняя дуга нуля");
        assert_eq!(alpha(0, 0), 0, "угол клетки пуст");
        assert_eq!(alpha(0, 3), 0xff, "боковой штрих нуля");
    }

    #[test]
    fn rasterize_writes_color_and_alpha() {
        let (rgba, w, h) = rasterize("0", [10, 20, 30], 1);
        assert_eq!((w, h), (6, 8));
        // Пиксель (1, 0) включён: его RGBA начинается с байта 4.
        assert_eq!(rgba[4..8], [10, 20, 30, 0xff]);
    }

    #[test]
    fn rasterize_scales_by_blocks() {
        let (rgba, w, h) = rasterize("0", [255, 255, 255], 2);
        assert_eq!((w, h), (12, 16));
        let alpha = |x: u32, y: u32| rgba[((y * w + x) * 4 + 3) as usize];
        // Пиксель (1, 0) исходного глифа — блок 2×2.
        assert_eq!(alpha(2, 0), 0xff);
        assert_eq!(alpha(3, 1), 0xff);
        assert_eq!(alpha(0, 0), 0);
    }

    #[test]
    fn rasterize_empty_string_minimal_bitmap() {
        let (rgba, w, h) = rasterize("", [0, 0, 0], 1);
        assert_eq!((w, h), (1, 8));
        assert!(rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn rasterize_fallback_for_unknown_char() {
        let (_, w, _) = rasterize("Ж", [255, 255, 255], 1);
        assert_eq!(w, 6);
    }
}
