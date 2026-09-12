//! SRV для живого кадра Windows.Graphics.Capture и геометрия его кропа.
//!
//! Кадр захвата уже находится на том же D3D11-устройстве, что и рендерер,
//! поэтому копировать его в новую текстуру не нужно. `WindowCropTexture`
//! удерживает COM-ссылку на внешний ресурс и один SRV; пока identity ресурса
//! не поменялась, новые кадры проходят в тот же ресурс без выделений GPU.

use rst_core::model::CropRect;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11ShaderResourceView,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Interface;

use crate::RenderError;

/// UV-подпрямоугольник для `Sprite::with_uv`.
///
/// Границы сдвинуты на пол-текселя: при билинейной выборке край куска берёт
/// центр крайнего пикселя, а не смешивает его с первым пикселем за границей.
/// Это особенно важно для прозрачного кадра окна — цвет RGB у прозрачных
/// соседей не должен давать ореол после масштабирования стикера.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropUv {
    /// Верхний левый угол области в долях исходной текстуры.
    pub offset: [f32; 2],
    /// Размер области в долях исходной текстуры.
    pub scale: [f32; 2],
}

impl CropUv {
    /// Вычислить UV по долевому кропу и фактическому размеру кадра в пикселях.
    ///
    /// `CropRect::to_pixels` задаёт именно реальные пиксели окна, а не DIP,
    /// поэтому DPI монитора не меняет место выборки. `None` означает нулевой
    /// размер источника или вырожденный вход.
    pub fn from_crop(crop: CropRect, window_w: u32, window_h: u32) -> Option<Self> {
        let (x, y, width, height) = crop.to_pixels(window_w, window_h)?;
        let fw = window_w as f32;
        let fh = window_h as f32;
        // Половина текселя с каждой стороны. Для однопиксельного участка
        // scale становится нулевым, и вся выборка остаётся в его центре.
        let inset_w = width.saturating_sub(1) as f32;
        let inset_h = height.saturating_sub(1) as f32;
        Some(Self {
            offset: [(x as f32 + 0.5) / fw, (y as f32 + 0.5) / fh],
            scale: [inset_w / fw, inset_h / fh],
        })
    }
}

/// Итог обновления внешнего кадра захвата.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCropUpdate {
    /// Пришла та же COM-текстура: SRV переиспользован.
    Reused,
    /// Пришла другая COM-текстура: создан новый SRV.
    Recreated,
}

/// SRV на внешней BGRA-текстуре Windows.Graphics.Capture.
///
/// Владение ресурсом — это COM `AddRef`, не копирование пикселей. Для кадра
/// на том же устройстве вызов [`Self::update`] с тем же `ID3D11Texture2D`
/// возвращает [`WindowCropUpdate::Reused`] и не создаёт ни текстуру, ни SRV.
/// Новый SRV создаётся только при смене COM identity (resize окна, пересоздание
/// пула захвата или потеря устройства). После обновления источник можно
/// отрисовать через обычный UV-путь `mainPS`: `CropUv` передаётся в
/// `Sprite::with_uv`, а существующий шейдер сохраняет premultiplied alpha,
/// opacity, flip и rotation без отдельной ветки.
#[derive(Debug, Clone)]
pub struct WindowCropTexture {
    srv: ID3D11ShaderResourceView,
    /// COM-ссылка не даёт внешнему захвату освободить ресурс раньше SRV.
    _texture: ID3D11Texture2D,
    identity: usize,
    width: u32,
    height: u32,
}

impl WindowCropTexture {
    /// Создать держатель SRV для BGRA8-текстуры захвата.
    ///
    /// Текстура обязана быть обычной однослойной `Texture2D` с одним сэмплом
    /// и `D3D11_BIND_SHADER_RESOURCE`: именно такой ресурс может безопасно
    /// сэмплироваться текущим `Texture2D tex0` в `mainPS`.
    pub(crate) fn from_capture(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
    ) -> Result<Self, RenderError> {
        let desc = validate_capture_texture(texture)?;
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: desc уже проверен; texture принадлежит тому же D3D11
        // устройству, а out-параметр жив до конца вызова.
        unsafe { device.CreateShaderResourceView(texture, None, Some(&mut srv)) }
            .map_err(RenderError::Windows)?;
        let srv = srv.expect("CreateShaderResourceView без ошибки возвращает объект");
        Ok(Self {
            srv,
            _texture: texture.clone(),
            identity: texture.as_raw() as usize,
            width: desc.Width,
            height: desc.Height,
        })
    }

    /// Принять следующий кадр захвата.
    ///
    /// Обновление кадра в том же внешнем ресурсе не требует действий: GPU
    /// увидит новые пиксели через уже существующий SRV. Для другой текстуры
    /// создаётся новый держатель, а старый SRV освобождается после подмены.
    pub(crate) fn update(
        &mut self,
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
    ) -> Result<WindowCropUpdate, RenderError> {
        let desc = validate_capture_texture(texture)?;
        if self.holds_texture(texture) {
            debug_assert_eq!(self.width, desc.Width);
            debug_assert_eq!(self.height, desc.Height);
            return Ok(WindowCropUpdate::Reused);
        }
        let replacement = Self::from_capture(device, texture)?;
        *self = replacement;
        Ok(WindowCropUpdate::Recreated)
    }

    /// Текстура спрайта на этом же SRV — то, что кладётся в
    /// [`crate::Sprite::texture`].
    ///
    /// Вызывать можно на каждый кадр: внутри только COM `AddRef`, ни одного
    /// `CreateShaderResourceView`. Замер 2026-09-10 показал, что
    /// Windows.Graphics.Capture отдаёт ОДНУ И ТУ ЖЕ COM-текстуру на всех
    /// кадрах подряд (30 кадров — одна текстура), поэтому держатель живёт
    /// весь сеанс захвата, а не пересоздаётся под ротацию буферов пула.
    pub fn sprite_texture(&self) -> crate::Texture {
        crate::Texture::from_external(
            self.srv.clone(),
            self._texture.clone(),
            self.width,
            self.height,
        )
    }

    /// Источник ли это же COM-текстура (сравнение без чтения пикселей).
    pub(crate) fn holds_texture(&self, texture: &ID3D11Texture2D) -> bool {
        self.identity == texture.as_raw() as usize
    }

    /// Ширина исходного кадра в физических пикселях.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Высота исходного кадра в физических пикселях.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// SRV для специализированного пути биндинга рендера.
    pub fn srv(&self) -> &ID3D11ShaderResourceView {
        &self.srv
    }

    /// UV-подпрямоугольник для этого кадра захвата.
    pub fn crop_uv(&self, crop: CropRect) -> Option<CropUv> {
        CropUv::from_crop(crop, self.width, self.height)
    }
}

fn validate_capture_texture(
    texture: &ID3D11Texture2D,
) -> Result<D3D11_TEXTURE2D_DESC, RenderError> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: texture — живой COM-ресурс, desc — валидный out-параметр.
    unsafe { texture.GetDesc(&mut desc) };
    if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(RenderError::InvalidTextureData(format!(
            "захват окна имеет формат {:?}, ожидался BGRA8",
            desc.Format
        )));
    }
    if desc.Width == 0 || desc.Height == 0 {
        return Err(RenderError::InvalidTextureData(
            "захват окна имеет нулевой размер".to_string(),
        ));
    }
    if desc.ArraySize != 1 || desc.SampleDesc.Count != 1 {
        return Err(RenderError::InvalidTextureData(
            "захват окна должен быть однослойной Texture2D без MSAA".to_string(),
        ));
    }
    if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 == 0 {
        return Err(RenderError::InvalidTextureData(
            "текстура захвата не имеет D3D11_BIND_SHADER_RESOURCE".to_string(),
        ));
    }
    Ok(desc)
}

#[cfg(test)]
mod tests {
    use super::CropUv;
    use rst_core::model::CropRect;

    #[test]
    fn uv_uses_pixel_centres_and_real_source_size() {
        let crop = CropRect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        };
        let uv = CropUv::from_crop(crop, 800, 400).expect("valid crop");
        assert_eq!(uv.offset, [200.5 / 800.0, 200.5 / 400.0]);
        assert_eq!(uv.scale, [399.0 / 800.0, 99.0 / 400.0]);
    }

    #[test]
    fn uv_tracks_physical_source_pixels_not_monitor_dpi() {
        let crop = CropRect {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let a = CropUv::from_crop(crop, 300, 200).expect("valid crop");
        let b = CropUv::from_crop(crop, 600, 400).expect("valid crop");
        // Нормализованные UV закономерно различаются у источников разного
        // размера, но после обратного перевода обе пары указывают на центр
        // первого пикселя и последний пиксель половины кадра.
        assert_eq!(a.offset[0] * 300.0, 0.5);
        assert_eq!(a.offset[1] * 200.0, 0.5);
        assert_eq!(b.offset[0] * 600.0, 0.5);
        assert_eq!(b.offset[1] * 400.0, 0.5);
        assert_eq!(a.scale[0] * 300.0, 149.0);
        assert_eq!(a.scale[1] * 200.0, 99.0);
        assert_eq!(b.scale[0] * 600.0, 299.0);
        assert_eq!(b.scale[1] * 400.0, 199.0);
    }

    #[test]
    fn one_pixel_crop_samples_one_pixel() {
        let crop = CropRect {
            x: 0.0,
            y: 0.0,
            w: 0.005,
            h: 0.005,
        };
        let uv = CropUv::from_crop(crop, 100, 100).expect("minimum crop");
        assert_eq!(uv.offset, [0.005, 0.005]);
        assert_eq!(uv.scale, [0.0, 0.0]);
    }

    #[test]
    fn zero_source_is_rejected() {
        assert!(CropUv::from_crop(CropRect::default(), 0, 100).is_none());
        assert!(CropUv::from_crop(CropRect::default(), 100, 0).is_none());
    }
}
