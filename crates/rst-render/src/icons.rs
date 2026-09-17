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
        Icon::Groups => draw_groups(&mut canvas, s),
        // Глаз стикера — ТОТ ЖЕ рисунок, что у «показать/скрыть всё» на
        // панели у курсора: пользователь просил сделать кнопку тулбара
        // «такой же, как на основном тулбаре» (2026-09-06), а две похожие,
        // но разные пиктограммы одного смысла — худший вид расхождения.
        Icon::Eye => return eye_rgba(EYE_OPEN_PNG, size_px, color_of(icon)),
        Icon::EyeOff => return eye_rgba(EYE_CLOSED_PNG, size_px, color_of(icon)),
        Icon::OrderUp => draw_order_up(&mut canvas, s),
        Icon::OrderDown => draw_order_down(&mut canvas, s),
        Icon::Duplicate => draw_duplicate(&mut canvas, s),
        Icon::Delete => draw_delete(&mut canvas, s),
        Icon::FileOpen => draw_file_open(&mut canvas, s),
        Icon::PresetSave => draw_preset_save(&mut canvas, s),
        Icon::PresetLoad => draw_preset_load(&mut canvas, s),
        Icon::Settings => draw_settings(&mut canvas, s),
        Icon::Exit => draw_exit(&mut canvas, s),
        Icon::Play => draw_play(&mut canvas, s),
        Icon::Pause => draw_pause(&mut canvas, s),
        Icon::Timeline => draw_timeline(&mut canvas, s),
        Icon::TimelineOff => draw_timeline_off(&mut canvas, s),
        Icon::VolumeHigh => draw_volume(&mut canvas, s, 2),
        Icon::VolumeLow => draw_volume(&mut canvas, s, 1),
        Icon::VolumeMute => draw_volume_mute(&mut canvas, s),
        Icon::ResetScale => draw_reset_scale(&mut canvas, s),
        // Двухцветная (белая с обводкой) — рисуется целиком своим
        // генератором, как и растровая булавка ниже.
        Icon::Rotate => return rotate_arrow_rgba(size_px),
        Icon::Lock => draw_lock(&mut canvas, s),
        Icon::LockOpen => draw_lock_open(&mut canvas, s),
        Icon::Plus => draw_plus(&mut canvas, s),
        // Единственная растровая иконка — рисунок пользователя, см.
        // `pinned_badge_rgba`.
        Icon::Pinned => return pinned_badge_rgba(size_px),
    }
    canvas.into_rgba()
}

/// PNG-булавка, присланная пользователем (2026-08-21) для бейджа «окно
/// закреплено»: 64×64, straight alpha, штрих одним серым тоном
/// (`#626262`) на прозрачном фоне.
const PINNED_BADGE_PNG: &[u8] = include_bytes!("../assets/pinned_badge.png");

/// Бейдж-булавка в RGBA `size_px`×`size_px`.
///
/// Отличается от остальных иконок тем, что не рисуется примитивами:
/// пользователь дал конкретный рисунок, и повторять его вручную значило бы
/// получить похожую, но другую булавку. Из исходника берётся только АЛЬФА —
/// цвет заменяется на [`PINNED_BADGE_COLOR`]: оригинальный серый штрих
/// тонет на тёмном фоне бейджа ([`crate::widgets::theme::LOCK_INDICATOR_BG`]),
/// а форма и сглаживание сохраняются как есть.
///
/// Декодирование не кэшируется здесь намеренно: иконки растрируются один раз
/// на размер и живут в кэше текстур вызывающего слоя (`ui_textures`).
fn pinned_badge_rgba(size_px: u32) -> Vec<u8> {
    let decoded = image::load_from_memory(PINNED_BADGE_PNG);
    let mut rgba = match decoded {
        Ok(img) => img.into_rgba8(),
        Err(e) => {
            // Битый ресурс — не повод ронять оверлей: пустой бейдж честнее
            // паники, а в логе видно причину.
            tracing::warn!(error = %e, "иконка-булавка не декодировалась");
            return vec![0u8; (size_px * size_px * 4) as usize];
        }
    };
    if rgba.width() != size_px || rgba.height() != size_px {
        rgba = image::imageops::resize(
            &rgba,
            size_px,
            size_px,
            image::imageops::FilterType::Lanczos3,
        );
    }
    for px in rgba.pixels_mut() {
        let alpha = px.0[3];
        px.0 = [
            PINNED_BADGE_COLOR[0],
            PINNED_BADGE_COLOR[1],
            PINNED_BADGE_COLOR[2],
            alpha,
        ];
    }
    rgba.into_raw()
}

/// Цвет штриха булавки поверх тёмного бейджа.
const PINNED_BADGE_COLOR: [u8; 3] = [0xf0, 0xf0, 0xf0];

/// Открытый глаз — рисунок пользователя (2026-08-31) для состояния «все
/// стикеры видны».
const EYE_OPEN_PNG: &[u8] = include_bytes!("../assets/eye_open.png");
/// Перечёркнутый глаз — тот же рисунок для состояния «все стикеры скрыты».
const EYE_CLOSED_PNG: &[u8] = include_bytes!("../assets/eye_closed.png");

/// Доля стороны иконки, которую занимает присланный рисунок.
///
/// Оба глаза нарисованы почти во всю квадратную канву (штрих доходит до
/// 0.94 стороны), а соседние по панели пиктограммы рисуются примитивами
/// внутри примерно 0.76 — вписанный «как есть» глаз читался бы заметно
/// крупнее соседей. Множитель приводит его к их размеру; общий для обоих
/// рисунков, поэтому сам глаз в паре не меняет величину — перечёркнутый
/// лишь добавляет к нему косую черту.
const RASTER_ICON_FRAC: f64 = 0.82;

/// Растровая иконка `size_px`×`size_px` из PNG: из исходника берётся только
/// АЛЬФА, цвет заменяется на `color` (как у [`pinned_badge_rgba`]),
/// рисунок вписывается в [`RASTER_ICON_FRAC`] стороны и центрируется.
fn eye_rgba(png: &[u8], size_px: u32, color: [u8; 3]) -> Vec<u8> {
    let mut out = vec![0u8; (size_px as usize) * (size_px as usize) * 4];
    let decoded = match image::load_from_memory(png) {
        Ok(img) => img.into_rgba8(),
        Err(e) => {
            // Битый ресурс — не повод ронять оверлей (тот же принцип, что у
            // булавки): пустая иконка честнее паники, причина видна в логе.
            tracing::warn!(error = %e, "иконка-глаз не декодировалась");
            return out;
        }
    };
    let inner = ((f64::from(size_px) * RASTER_ICON_FRAC).round() as u32).clamp(1, size_px);
    let scaled = image::imageops::resize(
        &decoded,
        inner,
        inner,
        image::imageops::FilterType::Lanczos3,
    );
    let off = (size_px - inner) / 2;
    for y in 0..inner {
        for x in 0..inner {
            let alpha = scaled.get_pixel(x, y).0[3];
            let i = (((y + off) * size_px + (x + off)) as usize) * 4;
            out[i] = color[0];
            out[i + 1] = color[1];
            out[i + 2] = color[2];
            out[i + 3] = alpha;
        }
    }
    out
}

/// Цвет иконки.
///
/// Палитра — монохром Source VGUI (2026-08-23, перевод тулбаров на
/// стилистику окна настроек): пиктограмма светлая, состояние «выключено» —
/// тёмная того же силуэта, цвет остаётся только там, где он несёт смысл,
/// который форма не передаёт (удаление). Прежние пастельные тона были
/// рассчитаны на светло-серую панель прежнего языка настроек; там мятный и
/// голубой давали контраст около 1.5:1 и читались как грязь.
///
/// В Dark Liquid Glass панель под иконкой всегда тёмное стекло (§2.1), и
/// одной палитры хватает на все панели без исключения.
fn color_of(icon: Icon) -> [u8; 3] {
    /// Обычная пиктограмма — почти белая, как текст VGUI.
    const LIGHT: [u8; 3] = [0xf2, 0xf2, 0xf2];
    /// «Недоступно» — тёмный силуэт: на сером фоне читается как
    /// «утоплено/неактивно», ровно как неактивная вкладка настроек.
    ///
    /// ВЫКЛЮЧЕННОЕ состояние переключателя этим тоном больше не рисуется
    /// (2026-09-06): «выключено» и «недоступно» на тёмном стекле
    /// оказались неотличимы — жалоба пользователя. Состояние сообщают
    /// форма рисунка и заливка кнопки; тон остался только у `LockOpen`,
    /// где пары «включено/выключено» в этом смысле нет.
    const DIM: [u8; 3] = [0x3a, 0x3a, 0x3a];
    match icon {
        // Растровая иконка красится в `pinned_badge_rgba`, сюда не попадает.
        Icon::Pinned => PINNED_BADGE_COLOR,
        Icon::LockOpen => DIM,
        // Закрытый глаз — СОСТОЯНИЕ «скрыт», а не «недоступно»: тот же
        // светлый тон, что у открытого. Пара глаз в программе одна на обе
        // кнопки видимости (2026-09-06), рисунок — присланный пользователем
        // PNG, см. `eye_rgba`.
        Icon::EyeOff => LIGHT,
        // Выключенный переключатель полосы больше НЕ красится в DIM: тон
        // на тон с фоном кнопки — ровно та жалоба, из-за которой у него
        // появилась косая черта (2026-09-06). Состояние сообщает форма
        // и заливка кнопки, а не яркость рисунка.
        Icon::TimelineOff => LIGHT,
        // Двухцветные иконки красятся своими генераторами.
        Icon::Rotate => PINNED_BADGE_COLOR,
        // Единственный цветной акцент: удаление необратимо, и форма урны
        // сама по себе этого не сообщает.
        Icon::Delete => [0xe0, 0x76, 0x76],
        // Обе иконки видимости — светлые: состояние читается формой
        // (перечёркнут или нет), а приглушать её на тёмном стекле значило
        // бы прятать саму кнопку.
        Icon::Layers
        | Icon::Groups
        | Icon::Eye
        | Icon::OrderUp
        | Icon::OrderDown
        | Icon::Duplicate
        | Icon::FileOpen
        | Icon::PresetSave
        | Icon::PresetLoad
        | Icon::Settings
        | Icon::Exit
        | Icon::Play
        | Icon::Pause
        | Icon::Timeline
        | Icon::VolumeHigh
        | Icon::VolumeLow
        | Icon::VolumeMute
        | Icon::ResetScale
        | Icon::Lock
        | Icon::Plus => LIGHT,
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

/// «Группы окон»: сетка два на два — плитки с зазором, тот же образ, что у
/// раскладок в ленте меню редактирования групп.
fn draw_groups(cv: &mut Canvas, s: f64) {
    let pad = 0.26 * s;
    let cell = 0.20 * s;
    let gap = 0.08 * s;
    for (col, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let x = pad + f64::from(col) * (cell + gap);
        let y = pad + f64::from(row) * (cell + gap);
        fill_rect(cv, x, y, x + cell, y + cell);
    }
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
    // Шестерёнка, а не три ползунка (запрос пользователя 2026-09-17): ползунки
    // в этом же тулбаре означают прозрачность и громкость, и тот же знак для
    // «настроек» читался как ещё один регулятор.
    //
    // Тело — толстое КОЛЬЦО, а не круг с дыркой: холст иконки складывает
    // покрытие и стирать уже нарисованное нечем, поэтому отверстие задаётся
    // внутренним краем кольца.
    let (cx, cy) = (0.5 * s, 0.5 * s);
    // Внутренний край кольца 0.145s — отверстие; внешний 0.315s.
    stroke_ellipse(cv, cx, cy, 0.23 * s, 0.23 * s, 0.17 * s);
    // Восемь зубцов-трапеций, сужающихся наружу: основание утоплено в кольцо
    // (0.26s), вершина на 0.45s. Половина угловой ширины — 12° у основания и
    // 8° у вершины, из-за чего зуб читается даже в 20 DIP кнопки тулбара.
    const TEETH: usize = 8;
    let (base_r, tip_r) = (0.26 * s, 0.45 * s);
    let (base_half, tip_half) = (12f64.to_radians(), 8f64.to_radians());
    for i in 0..TEETH {
        let a = std::f64::consts::TAU * i as f64 / TEETH as f64;
        let at = |r: f64, da: f64| (cx + r * (a + da).cos(), cy + r * (a + da).sin());
        fill_quad(
            cv,
            at(base_r, -base_half),
            at(tip_r, -tip_half),
            at(tip_r, tip_half),
            at(base_r, base_half),
        );
    }
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

/// «Полоса перемотки» (запрос пользователя 2026-08-22): горизонтальная
/// дорожка с круглой ручкой — узнаваемый скраббер плеера. Форма у
/// включённого и выключенного состояния одна: их различает цвет (см.
/// `icon_color`), как у пары `Lock`/`LockOpen`.
fn draw_timeline(cv: &mut Canvas, s: f64) {
    // Дорожка во всю ширину и круглая ручка ПО ЦЕНТРУ. Две другие
    // компоновки в размере кнопки (20 DIP) читались чужими знаками:
    // ручка у левого края — стрелка «влево», прямоугольный бегунок
    // поперёк дорожки — «плюс».
    fill_rect(cv, 0.10 * s, 0.47 * s, 0.90 * s, 0.53 * s);
    fill_circle(cv, 0.50 * s, 0.50 * s, 0.17 * s);
}

/// «Полоса перемотки выключена»: та же дорожка с ручкой, перечёркнутая
/// косой чертой — как `Eye`/`EyeOff`. Раньше отличие было только в тоне
/// (`DIM`), и на сером стекле кнопка читалась не «выключено», а
/// «недоступно» (репорт пользователя 2026-09-06).
fn draw_timeline_off(cv: &mut Canvas, s: f64) {
    draw_timeline(cv, s);
    line(cv, 0.18 * s, 0.80 * s, 0.82 * s, 0.20 * s, 0.10 * s);
}

/// «Динамик» с `waves` волнами справа: корпус + раструб, затем шевроны.
/// Одна волна — тихо, две — громко; сама форма динамика одинаковая, чтобы
/// уровень читался приростом, а не другим знаком.
fn draw_volume(cv: &mut Canvas, s: f64, waves: u8) {
    speaker_body(cv, s);
    if waves >= 1 {
        chevron(cv, s, 0.58, 0.14);
    }
    if waves >= 2 {
        chevron(cv, s, 0.74, 0.26);
    }
}

/// «Звук выключен»: тот же динамик, крест вместо волн. Крест, а не одна
/// косая черта: черта поверх раструба сливается с ним в размере кнопки.
fn draw_volume_mute(cv: &mut Canvas, s: f64) {
    speaker_body(cv, s);
    let t = 0.09 * s;
    line(cv, 0.60 * s, 0.34 * s, 0.88 * s, 0.66 * s, t);
    line(cv, 0.88 * s, 0.34 * s, 0.60 * s, 0.66 * s, t);
}

/// Корпус динамика: прямоугольник мембраны и трапеция раструба.
fn speaker_body(cv: &mut Canvas, s: f64) {
    fill_rect(cv, 0.08 * s, 0.38 * s, 0.26 * s, 0.62 * s);
    fill_quad(
        cv,
        (0.26 * s, 0.38 * s),
        (0.48 * s, 0.18 * s),
        (0.48 * s, 0.82 * s),
        (0.26 * s, 0.62 * s),
    );
}

/// Волна звука: шеврон «>» с вершиной на `x` и полураствором `half`
/// (в долях стороны).
fn chevron(cv: &mut Canvas, s: f64, x: f64, half: f64) {
    let t = 0.085 * s;
    let tip_x = (x + half * 0.55) * s;
    line(cv, x * s, (0.5 - half) * s, tip_x, 0.5 * s, t);
    line(cv, tip_x, 0.5 * s, x * s, (0.5 + half) * s, t);
}

/// «Поворот» — ручка на углу рамки выделения (запрос пользователя
/// 2026-08-23). Это ТА ЖЕ двусторонняя изогнутая стрелка, что показывает
/// курсор поворота (`rst_win32::input::rotate_cursor_rgba`): пользователь
/// уже знает эту форму — она всплывает, когда он входит в поворот, — и
/// просил поставить на углы именно её, а не кольцо.
///
/// Пропорции перенесены один-в-один из курсора (радиус дуги 11/32 стороны,
/// охват 100°, толщина 2.2/32, остриё 4.5/32 с раствором 55°), поэтому
/// иконка и курсор совпадают по рисунку в любом размере. Дуга смотрит
/// вдоль +X; наклон под конкретный угол делает вызывающий слой поворотом
/// прямоугольника ([`crate::SelectionBox::rotate_handle_rects`]).
///
/// Заливка белая с чёрной обводкой в один пиксель — та же конвенция, что у
/// системных курсоров: читается и на светлом, и на тёмном стикере. Поэтому
/// генератор возвращает готовый RGBA, а не одноцветное покрытие [`Canvas`].
fn rotate_arrow_rgba(size_px: u32) -> Vec<u8> {
    let n = size_px as usize;
    let s = size_px as f64;
    // Пропорции курсора поворота (32 px в оригинале).
    let (cx, cy) = (0.5 * s, 0.5 * s);
    let r = 11.0 / 32.0 * s;
    let h = (2.2 / 32.0 * s) / 2.0;
    let span = 100.0_f64.to_radians();
    let head_len = 4.5 / 32.0 * s;
    let spread = head_len * (55.0_f64.to_radians() / 2.0).tan() * 2.0;
    let start_a = -span / 2.0;
    let end_a = span / 2.0;

    let in_arc = |x: f64, y: f64| -> bool {
        let (dx, dy) = (x - cx, y - cy);
        let dist = (dx * dx + dy * dy).sqrt();
        if (dist - r).abs() > h {
            return false;
        }
        let mut angle = dy.atan2(dx);
        if angle < start_a {
            angle += 2.0 * std::f64::consts::PI;
        }
        (start_a..=end_a).contains(&angle)
    };

    // Остриё на конце дуги: у `start_a` развёрнуто назад, у `end_a` —
    // вперёд по касательной (обе стрелки смотрят «по кругу»).
    let arrowhead = |end_angle: f64, reversed: bool| {
        let (tx, ty) = (cx + r * end_angle.cos(), cy + r * end_angle.sin());
        let tangent = end_angle + std::f64::consts::FRAC_PI_2;
        let tip_dir = if reversed {
            tangent + std::f64::consts::PI
        } else {
            tangent
        };
        let tip = (tx + head_len * tip_dir.cos(), ty + head_len * tip_dir.sin());
        let perp = tip_dir + std::f64::consts::FRAC_PI_2;
        let b1 = (tx + spread * perp.cos(), ty + spread * perp.sin());
        let b2 = (tx - spread * perp.cos(), ty - spread * perp.sin());
        (tip, b1, b2)
    };
    let head_start = arrowhead(start_a, true);
    let head_end = arrowhead(end_a, false);

    let sign = |ax: f64, ay: f64, bx: f64, by: f64, px: f64, py: f64| {
        (ax - px) * (by - py) - (bx - px) * (ay - py)
    };
    let in_triangle = |x: f64, y: f64, t: ((f64, f64), (f64, f64), (f64, f64))| {
        let (p, b1, b2) = t;
        let d1 = sign(x, y, p.0, p.1, b1.0, b1.1);
        let d2 = sign(x, y, b1.0, b1.1, b2.0, b2.1);
        let d3 = sign(x, y, b2.0, b2.1, p.0, p.1);
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };

    // Покрытие с суперсемплингом 2×2 — та же техника, что у остальных
    // иконок (`SUB_SAMPLES`).
    let mut fill = vec![0.0f32; n * n];
    for py in 0..n {
        for px in 0..n {
            let mut hits = 0u32;
            for (ox, oy) in SUB_SAMPLES {
                let (x, y) = (px as f64 + ox, py as f64 + oy);
                if in_arc(x, y) || in_triangle(x, y, head_start) || in_triangle(x, y, head_end) {
                    hits += 1;
                }
            }
            fill[py * n + px] = hits as f32 / SUB_SAMPLES.len() as f32;
        }
    }

    let mut out = vec![0u8; n * n * 4];
    for py in 0..n {
        for px in 0..n {
            let f = fill[py * n + px];
            let (color, a) = if f > 0.0 {
                ([0xffu8, 0xff, 0xff], (f * 255.0).round() as u8)
            } else {
                // Обводка: дилатация заполненных пикселей на один во все
                // стороны — контур виден на любом фоне.
                let neighbour = (-1i32..=1).any(|dy| {
                    (-1i32..=1).any(|dx| {
                        if dx == 0 && dy == 0 {
                            return false;
                        }
                        let (nx, ny) = (px as i32 + dx, py as i32 + dy);
                        nx >= 0
                            && ny >= 0
                            && (nx as usize) < n
                            && (ny as usize) < n
                            && fill[ny as usize * n + nx as usize] > 0.5
                    })
                });
                if neighbour {
                    ([0u8, 0, 0], 0xff)
                } else {
                    ([0u8, 0, 0], 0)
                }
            };
            let i = (py * n + px) * 4;
            out[i..i + 3].copy_from_slice(&color);
            out[i + 3] = a;
        }
    }
    out
}

/// «Сбросить масштаб» (тулбар выделения, фидбэк пользователя 2026-08-09):
/// кольцо на 3/4 окружности (разрыв — под остриё стрелки) с треугольным
/// остриём на конце — обычная пиктограмма «вернуть исходное состояние».
fn draw_reset_scale(cv: &mut Canvas, s: f64) {
    let (cx, cy) = (0.5 * s, 0.5 * s);
    let r = 0.27 * s;
    let t = 0.09 * s;
    let h = t / 2.0;
    // Разрыв кольца (в радианах, угол от +X по часовой — экранная ось Y
    // направлена вниз): дуга идёт от gap_end до gap_start по часовой,
    // остриё стрелки садится на конец дуги у gap_end.
    let gap_start = 20.0_f64.to_radians();
    let gap_end = 95.0_f64.to_radians();
    cv.draw(move |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let dist = (dx * dx + dy * dy).sqrt();
        if (dist - r).abs() > h {
            return false;
        }
        let angle = dy.atan2(dx).rem_euclid(2.0 * std::f64::consts::PI);
        !(gap_start..gap_end).contains(&angle)
    });
    let tip_angle = gap_end;
    let (tx, ty) = (cx + r * tip_angle.cos(), cy + r * tip_angle.sin());
    let tangent = tip_angle + std::f64::consts::FRAC_PI_2;
    let spread = 0.11 * s;
    let back1 = (tx + spread * tangent.cos(), ty + spread * tangent.sin());
    let back2 = (tx - spread * tangent.cos(), ty - spread * tangent.sin());
    let point = (
        tx + 0.14 * s * tip_angle.cos(),
        ty + 0.14 * s * tip_angle.sin(),
    );
    fill_triangle(cv, point, back1, back2);
}

/// «Замок» (SPEC «закрепление окон», interact-lock): дужка-кольцо + тело
/// с узким пазом; нижняя половина кольца перекрывается телом.
fn draw_lock(cv: &mut Canvas, s: f64) {
    stroke_ellipse(cv, 0.5 * s, 0.48 * s, 0.15 * s, 0.16 * s, 0.09 * s);
    fill_rect(cv, 0.30 * s, 0.50 * s, 0.70 * s, 0.74 * s);
    fill_rect(cv, 0.47 * s, 0.58 * s, 0.53 * s, 0.66 * s);
}

/// «Замок открытый» — состояние «не заблокировано» в панели свойств
/// закреплённого окна: то же тело, но дужка разомкнута снизу (дуга-«C»,
/// разрыв внизу между углами 40° и 140°) — классическая пиктограмма
/// unlocked.
fn draw_lock_open(cv: &mut Canvas, s: f64) {
    let (cx, cy) = (0.5 * s, 0.46 * s);
    let r = 0.16 * s;
    let t = 0.09 * s;
    let h = t / 2.0;
    // Разрыв дуги внизу (в радианах, экранная ось Y вниз): дужка рисуется
    // от 140° до 400° (= 40°) по часовой — т.е. весь верх, кроме нижнего
    // сектора [40°, 140°].
    let gap_start = 40.0_f64.to_radians();
    let gap_end = 140.0_f64.to_radians();
    cv.draw(move |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let dist = (dx * dx + dy * dy).sqrt();
        if (dist - r).abs() > h {
            return false;
        }
        let angle = dy.atan2(dx).rem_euclid(2.0 * std::f64::consts::PI);
        !(gap_start..gap_end).contains(&angle)
    });
    fill_rect(cv, 0.30 * s, 0.50 * s, 0.70 * s, 0.74 * s);
    fill_rect(cv, 0.47 * s, 0.58 * s, 0.53 * s, 0.66 * s);
}

/// «Плюс» — добавление правила соседства в панели свойств закреплённого
/// окна: два пересекающихся штриха.
fn draw_plus(cv: &mut Canvas, s: f64) {
    line(cv, 0.5 * s, 0.40 * s, 0.5 * s, 0.60 * s, 0.10 * s);
    line(cv, 0.40 * s, 0.5 * s, 0.60 * s, 0.5 * s, 0.10 * s);
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
