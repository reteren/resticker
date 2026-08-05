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
/// rawUV * uv_scale`). Доли честно по числу столбцов/строк — последний ряд
/// может быть неполным, но `uv_scale` у всех кадров одинаковый (весь ряд —
/// одна полоса в UV), поэтому смешивать ячейки шейдер не может.
pub(crate) fn frame_uvs(i: usize, layout: &GridLayout) -> ([f32; 2], [f32; 2]) {
    let col = (i % layout.columns as usize) as f32;
    let row = (i / layout.columns as usize) as f32;
    let columns = layout.columns as f32;
    let rows = layout.rows as f32;
    ([col / columns, row / rows], [1.0 / columns, 1.0 / rows])
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

    #[test]
    fn uvs_map_frame_to_its_cell() {
        let g = grid_layout(4, 8, 8); // 2×2
        assert_eq!(frame_uvs(0, &g), ([0.0, 0.0], [0.5, 0.5]));
        assert_eq!(frame_uvs(1, &g), ([0.5, 0.0], [0.5, 0.5]));
        assert_eq!(frame_uvs(2, &g), ([0.0, 0.5], [0.5, 0.5]));
        assert_eq!(frame_uvs(3, &g), ([0.5, 0.5], [0.5, 0.5]));
    }

    #[test]
    fn uvs_handles_incomplete_last_row() {
        // 3 кадра в 2×2: третий — ячейка (col 0, row 1), та же uv_scale,
        // что у всех (ряд полный по UV-полосе).
        let g = grid_layout(3, 8, 8);
        assert_eq!(frame_uvs(2, &g), ([0.0, 0.5], [0.5, 0.5]));
    }

    #[test]
    fn uvs_identity_for_single_frame() {
        let g = grid_layout(1, 8, 8);
        assert_eq!(frame_uvs(0, &g), ([0.0, 0.0], [1.0, 1.0]));
    }
}
