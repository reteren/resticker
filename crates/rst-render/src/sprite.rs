//! Отрисовываемый стикер: текстура и трансформация.

use rst_core::model::{Placement, Transform};

use crate::texture::Texture;
use crate::video::VideoTextures;

/// Спрайт к отрисовке: GPU-текстура плюс положение и трансформация.
/// Типы полей — общие из rst-core, чтобы биндук (rst-win32/bin) мог
/// передавать данные стикера без промежуточных конверсий.
#[derive(Debug, Clone)]
pub struct Sprite {
    /// Текстура спрайта (premultiplied, с мипмапами). Для видеоспрайта
    /// (`video: Some`) — Y-плоскость кадра, но семплируется она только
    /// как фолбэк: `draw`/`draw_masked` в этом случае биндят три плоскости
    /// (`mainVideoPS`) вместо `tex0` (`mainPS`).
    pub texture: Texture,
    /// Видеокаркас (M5b, docs/M5B_VIDEO_DESIGN.md §3): три R8-плоскости
    /// Y/U/V, обновляемые `Device::update_video_textures` на каждый
    /// показанный кадр. `None` — обычный спрайт (картинка/кадр атласа),
    /// поведение M1-M5a; `Some` — рисуется через `mainVideoPS`.
    pub video: Option<VideoTextures>,
    /// Положение и размер в логических пикселях (DIP) относительно левого
    /// верхнего угла монитора: `cx`/`cy` — центр (rst-core `Placement`).
    pub placement: Placement,
    /// Поворот, прозрачность, отражения (rst-core `Transform`).
    pub transform: Transform,
    /// Смещение UV-подпрямоугольника текстуры (M5a, анимация): при
    /// `uv_offset=[0,0]`/`uv_scale=[1,1]` сэмплируется вся текстура —
    /// поведение M1-M4, значение по умолчанию.
    pub uv_offset: [f32; 2],
    /// Масштаб UV-подпрямоугольника (доли текстуры): `finalUV = uv_offset +
    /// rawUV * uv_scale` в пиксельном шейдере.
    pub uv_scale: [f32; 2],
}

impl Sprite {
    /// Собрать спрайт из текстуры и трансформации (UV по умолчанию — вся
    /// текстура; для подпрямоугольника см. [`Self::with_uv`]).
    pub fn new(texture: Texture, placement: Placement, transform: Transform) -> Self {
        Self {
            texture,
            video: None,
            placement,
            transform,
            uv_offset: [0.0, 0.0],
            uv_scale: [1.0, 1.0],
        }
    }

    /// Ограничить сэмплируемую область текстуры UV-подпрямоугольником
    /// (M5a, анимация): `offset` — верхний левый угол, `scale` — размер в
    /// долях текстуры (например, из [`crate::TextureAtlas::frames`]).
    pub fn with_uv(mut self, offset: [f32; 2], scale: [f32; 2]) -> Self {
        self.uv_offset = offset;
        self.uv_scale = scale;
        self
    }

    /// Привязать видеокаркас к спрайту (M5b, docs/M5B_VIDEO_DESIGN.md §3):
    /// `video` — три R8-плоскости кадра, которые `draw`/`draw_masked`
    /// семплируют через `mainVideoPS` (UV по умолчанию — вся текстура,
    /// как у статичных картинок, `redraw()` разницы не видит).
    pub fn with_video(mut self, video: VideoTextures) -> Self {
        self.video = Some(video);
        self
    }
}
