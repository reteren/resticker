//! Текстурный атлас анимации (M5a, docs/M5A_ANIMATION_DESIGN.md §3): все
//! кадры анимации в одну текстуру-грид; смена кадра — сменой UV у спрайта
//! (`Sprite::with_uv`), без перезаливки текстур на GPU.
//!
//! Раскладка — грид, а не горизонтальная полоса: полоса упирается в лимит
//! текстуры D3D11 feature level 11 (16384 px) даже при скромном числе кадров
//! у крупного стикера. Создание атласа — `Device::create_texture_atlas`;
//! здесь живут типы и чистая математика раскладки (юнит-тестируется без GPU).

use std::time::Duration;

use crate::texture::Texture;

/// Один кадр анимации внутри атласа: подпрямоугольник текстуры в UV
/// (0..1, тексельные координаты: `finalUV = uv_offset + rawUV * uv_scale`)
/// и задержка до следующего кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtlasFrame {
    /// Верхний левый угол ячейки кадра в UV (доли от размеров атласа).
    pub uv_offset: [f32; 2],
    /// Размер ячейки в UV (доли от размеров атласа).
    pub uv_scale: [f32; 2],
    /// Задержка кадра (rst-media `DecodedFrame::delay`, 0 клэмпнуто к
    /// минимуму на слое декодирования).
    pub delay: Duration,
}

/// Текстурный атлас анимации: одна GPU-текстура-грид + метаданные кадров.
///
/// Владение текстуры — как у всех текстур устройства (COM `AddRef` в
/// `Clone`); атлас рисуется через `Sprite::with_uv` на любом `WindowTarget`.
#[derive(Debug, Clone)]
pub struct TextureAtlas {
    /// Текстура-грид: `columns × rows` ячеек, без мипмапов (автогенерация
    /// смекшала бы соседние кадры в нижних мипах).
    pub texture: Texture,
    /// Метаданные кадров; `frames[i]` — кадр, залитый в ячейку `i`.
    pub frames: Vec<AtlasFrame>,
}

/// Геометрия грида атласа (чистая математика, тестируется без GPU).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GridLayout {
    /// Число столбцов: `ceil(sqrt(frame_count))`.
    pub columns: u32,
    /// Число строк: `ceil(frame_count / columns)`.
    pub rows: u32,
    /// Ширина атласа в пикселях: `columns * frame_w`.
    pub atlas_w: u32,
    /// Высота атласа в пикселях: `rows * frame_h`.
    pub atlas_h: u32,
}

/// Раскладка грида по `frame_count` кадрам размера `frame_w × frame_h`
/// (docs/M5A_ANIMATION_DESIGN.md §3). Квадратный корень из числа кадров —
/// в `f64`, чтобы `ceil` был точен для любых практических размеров
/// (число кадров ограничено сверху 300 на слое декодирования, rst-media).
pub(crate) fn grid_layout(frame_count: usize, frame_w: u32, frame_h: u32) -> GridLayout {
    let columns = (frame_count as f64).sqrt().ceil().max(1.0) as u32;
    let rows = frame_count.div_ceil(columns as usize) as u32;
    GridLayout {
        columns,
        rows,
        atlas_w: columns * frame_w,
        atlas_h: rows * frame_h,
    }
}

/// UV-подпрямоугольник кадра `i` в гриде `layout` (`finalUV = uv_offset +
/// rawUV * uv_scale`), инсетнутый на пол-текселя с каждой стороны.
///
/// Ячейки в текстуре атласа заливаются впритык, без зазора
/// (`Device::create_texture_atlas`) — при билинейной фильтрации сэмпл ровно
/// у границы UV-подпрямоугольника берёт половину веса из СОСЕДНЕЙ ячейки
/// (соседнего кадра анимации), а не только из своей. У анимаций с резкой
/// кромкой (например, светлый контур персонажа на прозрачном фоне) это даёт
/// тонкую цветную/белую линию-вспышку на границе кадра — подпиксельная на
/// родном размере, но заметная при масштабировании стикера вверх (найдено
/// по репорту пользователя, «белые линии» на анимированных прозрачных
/// гифках). Инсет в пол-текселя ставит крайний сэмпл ровно в ЦЕНТР крайнего
/// текселя ячейки — билинейная интерполяция у центра texel'я берёт только
/// его самого и соседа ВНУТРИ той же ячейки, до чужой ячейки уже не
/// дотягивается. Цена — доли текселя обрезаются по краю кадра, визуально
/// неотличимо для реального контента.
pub(crate) fn frame_uvs(i: usize, layout: &GridLayout) -> ([f32; 2], [f32; 2]) {
    let col = (i % layout.columns as usize) as f32;
    let row = (i / layout.columns as usize) as f32;
    let columns = layout.columns as f32;
    let rows = layout.rows as f32;
    let cell_w = 1.0 / columns;
    let cell_h = 1.0 / rows;
    let inset_u = 0.5 / layout.atlas_w as f32;
    let inset_v = 0.5 / layout.atlas_h as f32;
    (
        [col * cell_w + inset_u, row * cell_h + inset_v],
        [cell_w - 2.0 * inset_u, cell_h - 2.0 * inset_v],
    )
}

#[cfg(test)]
mod tests {
    use super::{frame_uvs, grid_layout};

    #[test]
    fn grid_two_frames_is_one_row_two_columns() {
        let g = grid_layout(2, 8, 8);
        assert_eq!(g.columns, 2);
        assert_eq!(g.rows, 1);
        assert_eq!((g.atlas_w, g.atlas_h), (16, 8));
    }

    #[test]
    fn grid_single_frame_fills_whole_texture() {
        let g = grid_layout(1, 64, 32);
        assert_eq!(g.columns, 1);
        assert_eq!(g.rows, 1);
        assert_eq!((g.atlas_w, g.atlas_h), (64, 32));
    }

    #[test]
    fn grid_three_frames_uses_square_root() {
        // 3 кадра: ceil(sqrt(3)) = 2 столбца, ceil(3/2) = 2 ряда — квадрат,
        // а не полоса 3×1, которая была бы длиннее на той же высоте.
        let g = grid_layout(3, 10, 10);
        assert_eq!((g.columns, g.rows), (2, 2));
        assert_eq!((g.atlas_w, g.atlas_h), (20, 20));
    }

    #[test]
    fn grid_300_frames_fits_16384_limit() {
        // Предельный по rst-media случай (300 кадров): грид 18×17 кадров
        // 64×64 укладывается в 16384 px, полоса 300×1 — нет.
        let g = grid_layout(300, 64, 64);
        assert_eq!(g.columns, 18);
        assert_eq!(g.rows, 17);
        assert_eq!((g.atlas_w, g.atlas_h), (1152, 1088));
        assert!(g.atlas_w <= 16384 && g.atlas_h <= 16384);
    }

    /// Инсет — пол-текселя атласа 16×16 (2×2 ячейки по 8×8): 0.5/16 = 0.03125
    /// по каждой стороне, `uv_scale` уменьшен на инсет с обеих сторон:
    /// 0.5 − 2×0.03125 = 0.4375.
    #[test]
    fn uvs_map_frame_to_its_cell_inset_by_half_texel() {
        let g = grid_layout(4, 8, 8); // 2×2
        assert_eq!(frame_uvs(0, &g), ([0.03125, 0.03125], [0.4375, 0.4375]));
        assert_eq!(frame_uvs(1, &g), ([0.53125, 0.03125], [0.4375, 0.4375]));
        assert_eq!(frame_uvs(2, &g), ([0.03125, 0.53125], [0.4375, 0.4375]));
        assert_eq!(frame_uvs(3, &g), ([0.53125, 0.53125], [0.4375, 0.4375]));
    }

    #[test]
    fn uvs_handles_incomplete_last_row() {
        // 3 кадра в 2×2: третий — ячейка (col 0, row 1), тот же инсет и
        // uv_scale, что у всех (ряд полный по UV-полосе).
        let g = grid_layout(3, 8, 8);
        assert_eq!(frame_uvs(2, &g), ([0.03125, 0.53125], [0.4375, 0.4375]));
    }

    /// Инсет — пол-текселя атласа 8×8 (1 ячейка, вся текстура): 0.5/8 =
    /// 0.0625; уже не «тождество» [0,0]-[1,1] — оно и раньше было
    /// избыточно безопасным для одного кадра (`CLAMP`-семплер не даёт
    /// протечки без соседей), но инсет применяется единообразно ко всем
    /// раскладкам, не только многокадровым.
    #[test]
    fn uvs_single_frame_still_gets_half_texel_inset() {
        let g = grid_layout(1, 8, 8);
        assert_eq!(frame_uvs(0, &g), ([0.0625, 0.0625], [0.875, 0.875]));
    }

    #[test]
    fn compute_downscale_dimensions_preserves_aspect_ratio_and_none() {
        use crate::device::compute_downscale_dimensions;

        // max_size = None сохраняет исходные размеры в точности (эквивалентность старому пути)
        assert_eq!(compute_downscale_dimensions(1920, 1080, None), (1920, 1080));
        assert_eq!(compute_downscale_dimensions(800, 600, None), (800, 600));

        // Размеры меньше лимита не увеличиваются (no upscale)
        assert_eq!(
            compute_downscale_dimensions(200, 100, Some((400, 400))),
            (200, 100)
        );

        // Пропорциональный даунскейл: ограничение по ширине
        assert_eq!(
            compute_downscale_dimensions(1920, 1080, Some((960, 960))),
            (960, 540)
        );

        // Пропорциональный даунскейл: ограничение по высоте
        assert_eq!(
            compute_downscale_dimensions(1000, 2000, Some((800, 500))),
            (250, 500)
        );

        // Граничные случаи: нулевые размеры
        assert_eq!(
            compute_downscale_dimensions(0, 100, Some((50, 50))),
            (0, 100)
        );
        assert_eq!(
            compute_downscale_dimensions(100, 100, Some((0, 50))),
            (100, 100)
        );
    }

    #[test]
    fn atlas_layout_and_uvs_after_downscale() {
        use crate::device::compute_downscale_dimensions;

        let orig_w = 1200;
        let orig_h = 800;
        let frame_count = 4;

        // Исходная раскладка без даунскейла (None)
        let (none_w, none_h) = compute_downscale_dimensions(orig_w, orig_h, None);
        let layout_orig = grid_layout(frame_count, none_w, none_h);
        assert_eq!(layout_orig.columns, 2);
        assert_eq!(layout_orig.rows, 2);
        assert_eq!(layout_orig.atlas_w, 2400);
        assert_eq!(layout_orig.atlas_h, 1600);

        // Раскладка с даунскейлом max_size = (300, 300) -> 300x200
        let (down_w, down_h) = compute_downscale_dimensions(orig_w, orig_h, Some((300, 300)));
        assert_eq!((down_w, down_h), (300, 200));

        let layout_down = grid_layout(frame_count, down_w, down_h);
        assert_eq!(layout_down.columns, 2);
        assert_eq!(layout_down.rows, 2);
        assert_eq!(layout_down.atlas_w, 600);
        assert_eq!(layout_down.atlas_h, 400);

        // Проверка UV-координат всех кадров: сетка ячеек остаётся согласованной,
        // а inset_u / inset_v корректно масштабируются под новый размер атласа
        for i in 0..frame_count {
            let (uv_off, uv_scale) = frame_uvs(i, &layout_down);
            // Ячейки в диапазоне [0, 1]
            assert!(uv_off[0] >= 0.0 && uv_off[0] < 1.0);
            assert!(uv_off[1] >= 0.0 && uv_off[1] < 1.0);
            assert!(uv_off[0] + uv_scale[0] <= 1.0);
            assert!(uv_off[1] + uv_scale[1] <= 1.0);

            // Ожидаемый инсет для 600x400: 0.5/600 и 0.5/400
            let expected_cell_w = 0.5f32; // 1 / 2 columns
            let expected_cell_h = 0.5f32; // 1 / 2 rows
            let expected_inset_u = 0.5f32 / 600.0;
            let expected_inset_v = 0.5f32 / 400.0;

            let col = (i % 2) as f32;
            let row = (i / 2) as f32;
            assert_eq!(uv_off[0], col * expected_cell_w + expected_inset_u);
            assert_eq!(uv_off[1], row * expected_cell_h + expected_inset_v);
            assert_eq!(uv_scale[0], expected_cell_w - 2.0 * expected_inset_u);
            assert_eq!(uv_scale[1], expected_cell_h - 2.0 * expected_inset_v);
        }
    }
}
