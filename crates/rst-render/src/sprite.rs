//! Отрисовываемый стикер: текстура и трансформация.

use rst_core::model::{Placement, Transform};

use crate::texture::Texture;

/// Спрайт к отрисовке: GPU-текстура плюс положение и трансформация.
/// Типы полей — общие из rst-core, чтобы биндук (rst-win32/bin) мог
/// передавать данные стикера без промежуточных конверсий.
#[derive(Debug, Clone)]
pub struct Sprite {
    /// Текстура спрайта (premultiplied, с мипмапами).
    pub texture: Texture,
    /// Положение и размер в логических пикселях (DIP) относительно левого
    /// верхнего угла монитора: `cx`/`cy` — центр (rst-core `Placement`).
    pub placement: Placement,
    /// Поворот, прозрачность, отражения (rst-core `Transform`).
    pub transform: Transform,
}

impl Sprite {
    /// Собрать спрайт из текстуры и трансформации.
    pub fn new(texture: Texture, placement: Placement, transform: Transform) -> Self {
        Self {
            texture,
            placement,
            transform,
        }
    }
}
