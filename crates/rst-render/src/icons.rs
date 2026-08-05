//! Векторная генерация иконок кнопок тулбара/панели у курсора — замена
//! placeholder-заливки (M2_WIRING_PLAN.md, §14 «Иконки кнопок»): каждый
//! [`Icon`] рисуется набором простых примитивов (прямоугольники, круги,
//! эллипсы, треугольники, линии) в квадратный RGBA-битмап.
//!
//! Соглашение об альфе — straight alpha, как у
//! [`crate::selection::checkerboard_tile`] и [`crate::text::rasterize`];
//! premultiply делает [`crate::texture::Texture::from_rgba`] при заливке
//! в текстуру. Антиалиасинг — суперсемплинг 2×2 на пиксель.

use crate::widgets::Icon;

/// Смещения суперсемплинга внутри пикселя (2×2): край примитива сглаживается
/// долей включённых сэмплов.
const SUB_SAMPLES: [(f64, f64); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];

/// Иконка `icon` в RGBA-битмап `size_px`×`size_px` (straight alpha):
/// включённые пиксели — цвет иконки с альфой 255, фон прозрачный.
/// `size_px` обязан быть больше нуля.
pub fn icon_rgba(icon: Icon, size_px: u32) -> Vec<u8> {
    assert!(size_px > 0, "размер иконки должен быть больше нуля");
    let s = f64::from(size_px);
    let mut canvas = Canvas::new(size_px, color_of(icon));
    match icon {
        Icon::Layers => draw_layers(&mut canvas, s),
        Icon::Eye => draw_eye(&mut canvas, s),
        Icon::EyeOff => draw_eye_off(&mut canvas, s),
        Icon::OrderUp => draw_order_up(&mut canvas, s),
        Icon::OrderDown => draw_order_down(&mut canvas, s),
        Icon::Duplicate => draw_duplicate(&mut canvas, s),
        Icon::Delete => draw_delete(&mut canvas, s),
        Icon::FileOpen => draw_file_open(&mut canvas, s),
        Icon::ShowAll => draw_show_all(&mut canvas, s),
        Icon::HideAll => draw_hide_all(&mut canvas, s),
        Icon::PresetSave => draw_preset_save(&mut canvas, s),
        Icon::PresetLoad => draw_preset_load(&mut canvas, s),
        Icon::Settings => draw_settings(&mut canvas, s),
        Icon::Exit => draw_exit(&mut canvas, s),
        Icon::Play => draw_play(&mut canvas, s),
        Icon::Pause => draw_pause(&mut canvas, s),
    }
    canvas.into_rgba()
}

/// Цвет иконки (иконки различаются и формой, и цветом — кнопки не монохромны).
fn color_of(icon: Icon) -> [u8; 3] {
    match icon {
        Icon::Layers => [0xcf, 0xcf, 0xd6],
        Icon::Eye => [0xf0, 0xf0, 0xf0],
        Icon::EyeOff => [0xa8, 0xa8, 0xb2],
        Icon::OrderUp => [0x8a, 0xd0, 0x9c],
        Icon::OrderDown => [0x6f, 0xa8, 0xff],
        Icon::Duplicate => [0xc2, 0xd4, 0xe6],
        Icon::Delete => [0xe8, 0x7a, 0x7a],
        Icon::FileOpen => [0xf2, 0xb8, 0x66],
        Icon::ShowAll => [0xb0, 0x8a, 0xcc],
        Icon::HideAll => [0xcf, 0x8a, 0xc8],
        Icon::PresetSave => [0x7f, 0xcf, 0xc0],
        Icon::PresetLoad => [0x9f, 0xc2, 0xe8],
        Icon::Settings => [0xe8, 0xe8, 0xee],
        Icon::Exit => [0xf2, 0x9a, 0x6a],
        Icon::Play => [0x8a, 0xd0, 0x9c],
        Icon::Pause => [0xf0, 0xf0, 0xf0],
    }
}

/// Буфер покрытия иконки: накапливает альфу по пикселям. Цвет у иконки один
/// на все примитивы, поэтому хранится отдельно и пишется в RGBA в конце.
struct Canvas {
    size: u32,
    color: [u8; 3],
    /// Покрытие 0..1 на пиксель (после наложения всех примитивов).
    cov: Vec<f32>,
}

impl Canvas {
    fn new(size: u32, color: [u8; 3]) -> Self {
        Self {
            size,
            color,
            cov: vec![0.0; (size * size) as usize],
        }
    }

    /// Наложить примитив: покрытие пикселя — доля сэмплов внутри фигуры,
    /// комбинируется с уже накопленным (порядок примитивов не важен).
    fn draw(&mut self, inside: impl Fn(f64, f64) -> bool) {
        let n = self.size;
        for py in 0..n {
            for px in 0..n {
                let x = f64::from(px);
                let y = f64::from(py);
                let mut hits = 0u32;
                for (sx, sy) in SUB_SAMPLES {
                    if inside(x + sx, y + sy) {
                        hits += 1;
                    }
                }
                let cov = hits as f32 / SUB_SAMPLES.len() as f32;
                let i = (py * n + px) as usize;
                self.cov[i] = 1.0 - (1.0 - self.cov[i]) * (1.0 - cov);
            }
        }
    }

    /// RGBA-битмап (straight alpha): альфа = покрытие, цвет константный.
    fn into_rgba(self) -> Vec<u8> {
        let n = self.size as usize;
        let mut out = Vec::with_capacity(n * n * 4);
        for &c in &self.cov {
            let a = (c * 255.0).round() as u8;
            out.extend_from_slice(&[self.color[0], self.color[1], self.color[2], a]);
        }
        out
    }
}

/// Залитый прямоугольник `[x0, x1) × [y0, y1)`.
fn fill_rect(cv: &mut Canvas, x0: f64, y0: f64, x1: f64, y1: f64) {
    cv.draw(move |x, y| x >= x0 && x < x1 && y >= y0 && y < y1);
}

/// Контур прямоугольника толщиной `t` (полоса по краю внутрь от границы).
fn stroke_rect(cv: &mut Canvas, x0: f64, y0: f64, x1: f64, y1: f64, t: f64) {
    let h = t / 2.0;
    cv.draw(move |x, y| {
        let inside = x >= x0 && x <= x1 && y >= y0 && y <= y1;
        let near_edge = (x - x0).abs() <= h
            || (x - x1).abs() <= h
            || (y - y0).abs() <= h
            || (y - y1).abs() <= h;
        inside && near_edge
    });
}

/// Залитый круг.
fn fill_circle(cv: &mut Canvas, cx: f64, cy: f64, r: f64) {
    let r2 = r * r;
    cv.draw(move |x, y| {
        let dx = x - cx;
        let dy = y - cy;
        dx * dx + dy * dy <= r2
    });
}

/// Кольцо эллипса с полуосями `rx`, `ry` толщиной `t` (толщина масштабируется
/// меньшей полуосью — приближение, достаточное для иконок).
fn stroke_ellipse(cv: &mut Canvas, cx: f64, cy: f64, rx: f64, ry: f64, t: f64) {
    let h = t / 2.0;
    let scale = rx.min(ry);
    cv.draw(move |x, y| {
        let dx = (x - cx) / rx;
        let dy = (y - cy) / ry;
        let rn = (dx * dx + dy * dy).sqrt();
        (rn - 1.0).abs() * scale <= h
    });
}

/// Отрезок толщиной `t` (с круглыми концами).
fn line(cv: &mut Canvas, x0: f64, y0: f64, x1: f64, y1: f64, t: f64) {
    let h = t / 2.0;
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    cv.draw(move |x, y| {
        if len2 <= 0.0 {
            return (x - x0).hypot(y - y0) <= h;
        }
        let proj = ((x - x0) * dx + (y - y0) * dy) / len2;
        if proj <= 0.0 {
            (x - x0).hypot(y - y0) <= h
        } else if proj >= 1.0 {
            (x - x1).hypot(y - y1) <= h
        } else {
            let px = x0 + proj * dx;
            let py = y0 + proj * dy;
            (x - px).hypot(y - py) <= h
        }
    });
}

/// Залитый треугольник (стандартный полуплоскостной тест; ориентация любая).
fn fill_triangle(cv: &mut Canvas, a: (f64, f64), b: (f64, f64), c: (f64, f64)) {
    let (ax, ay) = a;
    let (bx, by) = b;
    let (cx, cy) = c;
    cv.draw(move |x, y| {
        let d1 = (x - bx) * (ay - by) - (ax - bx) * (y - by);
        let d2 = (x - cx) * (by - cy) - (bx - cx) * (y - cy);
        let d3 = (x - ax) * (cy - ay) - (cx - ax) * (y - ay);
        let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(has_neg && has_pos)
    });
}

/// Залитый выпуклый четырёхугольник (вершины по порядку обхода, любая ориентация).
fn fill_quad(cv: &mut Canvas, a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) {
    fill_triangle(cv, a, b, c);
    fill_triangle(cv, a, c, d);
}

/// «Слои»: стопка из двух смещённых залитых прямоугольников.
fn draw_layers(cv: &mut Canvas, s: f64) {
    fill_rect(cv, 0.30 * s, 0.36 * s, 0.70 * s, 0.66 * s);
    fill_rect(cv, 0.36 * s, 0.28 * s, 0.60 * s, 0.52 * s);
}

/// «Глаз»: эллипс с точкой-зрачком.
fn draw_eye(cv: &mut Canvas, s: f64) {
    stroke_ellipse(cv, 0.5 * s, 0.5 * s, 0.33 * s, 0.21 * s, 0.10 * s);
    fill_circle(cv, 0.5 * s, 0.5 * s, 0.08 * s);
}

/// «Глаз закрытый»: эллипс с диагональной чертой.
fn draw_eye_off(cv: &mut Canvas, s: f64) {
    stroke_ellipse(cv, 0.5 * s, 0.5 * s, 0.33 * s, 0.21 * s, 0.09 * s);
    fill_circle(cv, 0.5 * s, 0.5 * s, 0.07 * s);
    line(cv, 0.34 * s, 0.38 * s, 0.66 * s, 0.62 * s, 0.09 * s);
}

/// «Выше по порядку»: стрелка вверх.
fn draw_order_up(cv: &mut Canvas, s: f64) {
    fill_triangle(
        cv,
        (0.5 * s, 0.26 * s),
        (0.28 * s, 0.62 * s),
        (0.72 * s, 0.62 * s),
    );
}

/// «Ниже по порядку»: стрелка вниз.
fn draw_order_down(cv: &mut Canvas, s: f64) {
    fill_triangle(
        cv,
        (0.5 * s, 0.74 * s),
        (0.28 * s, 0.38 * s),
        (0.72 * s, 0.38 * s),
    );
}

/// «Дублировать»: два перекрывающихся квадрата (копия).
fn draw_duplicate(cv: &mut Canvas, s: f64) {
    stroke_rect(cv, 0.42 * s, 0.42 * s, 0.72 * s, 0.72 * s, 0.10 * s);
    stroke_rect(cv, 0.28 * s, 0.28 * s, 0.58 * s, 0.58 * s, 0.10 * s);
}

/// «Удалить»: крестик.
fn draw_delete(cv: &mut Canvas, s: f64) {
    line(cv, 0.34 * s, 0.34 * s, 0.66 * s, 0.66 * s, 0.11 * s);
    line(cv, 0.66 * s, 0.34 * s, 0.34 * s, 0.66 * s, 0.11 * s);
}

/// «Открыть файл»: папка с язычком.
fn draw_file_open(cv: &mut Canvas, s: f64) {
    fill_rect(cv, 0.24 * s, 0.34 * s, 0.46 * s, 0.44 * s);
    fill_quad(
        cv,
        (0.24 * s, 0.44 * s),
        (0.76 * s, 0.44 * s),
        (0.72 * s, 0.70 * s),
        (0.28 * s, 0.70 * s),
    );
}

/// «Показать все»: глаз с плюсом.
fn draw_show_all(cv: &mut Canvas, s: f64) {
    stroke_ellipse(cv, 0.5 * s, 0.5 * s, 0.33 * s, 0.21 * s, 0.09 * s);
    line(cv, 0.5 * s, 0.42 * s, 0.5 * s, 0.58 * s, 0.08 * s);
    line(cv, 0.42 * s, 0.5 * s, 0.58 * s, 0.5 * s, 0.08 * s);
}

/// «Скрыть все»: глаз с минусом.
fn draw_hide_all(cv: &mut Canvas, s: f64) {
    stroke_ellipse(cv, 0.5 * s, 0.5 * s, 0.33 * s, 0.21 * s, 0.09 * s);
    line(cv, 0.40 * s, 0.5 * s, 0.60 * s, 0.5 * s, 0.09 * s);
}

/// «Сохранить пресет»: стрелка вниз в лоток.
fn draw_preset_save(cv: &mut Canvas, s: f64) {
    fill_rect(cv, 0.30 * s, 0.66 * s, 0.70 * s, 0.74 * s);
    line(cv, 0.5 * s, 0.36 * s, 0.5 * s, 0.54 * s, 0.09 * s);
    fill_triangle(
        cv,
        (0.5 * s, 0.64 * s),
        (0.38 * s, 0.52 * s),
        (0.62 * s, 0.52 * s),
    );
}

/// «Загрузить пресет»: стрелка вверх из лотка.
fn draw_preset_load(cv: &mut Canvas, s: f64) {
    fill_rect(cv, 0.30 * s, 0.66 * s, 0.70 * s, 0.74 * s);
    line(cv, 0.5 * s, 0.60 * s, 0.5 * s, 0.44 * s, 0.09 * s);
    fill_triangle(
        cv,
        (0.5 * s, 0.36 * s),
        (0.38 * s, 0.48 * s),
        (0.62 * s, 0.48 * s),
    );
}

/// «Настройки»: три ползунка с ручками.
fn draw_settings(cv: &mut Canvas, s: f64) {
    line(cv, 0.28 * s, 0.34 * s, 0.72 * s, 0.34 * s, 0.08 * s);
    fill_circle(cv, 0.46 * s, 0.34 * s, 0.07 * s);
    line(cv, 0.28 * s, 0.50 * s, 0.72 * s, 0.50 * s, 0.08 * s);
    fill_circle(cv, 0.60 * s, 0.50 * s, 0.07 * s);
    line(cv, 0.28 * s, 0.66 * s, 0.72 * s, 0.66 * s, 0.08 * s);
    fill_circle(cv, 0.38 * s, 0.66 * s, 0.07 * s);
}

/// «Выйти»: дверь со стрелкой наружу.
fn draw_exit(cv: &mut Canvas, s: f64) {
    stroke_rect(cv, 0.30 * s, 0.30 * s, 0.56 * s, 0.70 * s, 0.09 * s);
    line(cv, 0.56 * s, 0.5 * s, 0.70 * s, 0.5 * s, 0.09 * s);
    fill_triangle(
        cv,
        (0.78 * s, 0.5 * s),
        (0.68 * s, 0.42 * s),
        (0.68 * s, 0.58 * s),
    );
}

/// «Играть» (M5b): треугольник вправо — видео-стикер сейчас на паузе, клик
/// запускает воспроизведение.
fn draw_play(cv: &mut Canvas, s: f64) {
    fill_triangle(
        cv,
        (0.32 * s, 0.26 * s),
        (0.32 * s, 0.74 * s),
        (0.74 * s, 0.5 * s),
    );
}

/// «Пауза» (M5b): две вертикальные полосы — видео-стикер сейчас играет,
/// клик ставит на паузу.
fn draw_pause(cv: &mut Canvas, s: f64) {
    fill_rect(cv, 0.30 * s, 0.26 * s, 0.44 * s, 0.74 * s);
    fill_rect(cv, 0.56 * s, 0.26 * s, 0.70 * s, 0.74 * s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_size_is_square_times_four() {
        for size in [1u32, 4, 8, 16, 32] {
            for icon in Icon::ALL {
                assert_eq!(icon_rgba(icon, size).len(), (size * size * 4) as usize);
            }
        }
    }

    #[test]
    fn every_icon_has_ink() {
        for icon in Icon::ALL {
            let rgba = icon_rgba(icon, 16);
            let has_ink = rgba.chunks_exact(4).any(|px| px[3] != 0);
            assert!(has_ink, "{icon:?} рисуется полностью прозрачным");
        }
    }

    #[test]
    fn icons_are_mutually_distinct() {
        let buffers: Vec<(Icon, Vec<u8>)> = Icon::ALL
            .into_iter()
            .map(|icon| (icon, icon_rgba(icon, 16)))
            .collect();
        for (k, (a, ba)) in buffers.iter().enumerate() {
            for (b, bb) in &buffers[k + 1..] {
                assert_ne!(ba, bb, "иконки {a:?} и {b:?} дают одинаковый буфер");
            }
        }
    }

    #[test]
    fn eye_has_pupil_and_transparent_corners() {
        let rgba = icon_rgba(Icon::Eye, 16);
        let alpha = |x: usize, y: usize| rgba[(y * 16 + x) * 4 + 3];
        assert_ne!(alpha(8, 8), 0, "зрачок по центру");
        assert_eq!(alpha(0, 0), 0, "угол пуст");
    }

    #[test]
    #[should_panic(expected = "больше нуля")]
    fn icon_rgba_rejects_zero_size() {
        icon_rgba(Icon::Eye, 0);
    }
}
