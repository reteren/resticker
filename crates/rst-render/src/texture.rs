//! GPU-текстура спрайта с мипмапами и приведение альфы к premultiplied.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

use crate::RenderError;

/// GPU-текстура спрайта (RGBA, premultiplied, полная цепочка мипмапов).
///
/// Владение: COM-указатели — умные указатели windows-rs, `Release` вызывается
/// автоматически в `Drop`. `Clone` — это COM `AddRef`, то есть дешёвый.
#[derive(Clone)]
pub struct Texture {
    srv: ID3D11ShaderResourceView,
    // Держатель самой текстуры: не читается, но обязан жить, пока жив SRV.
    _texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    /// `Some` только для текстур-масок (M4, создаются
    /// `Device::create_mask_texture`, R8, рендер-таргет + SRV) — обычные
    /// спрайтовые текстуры (`from_rgba`) в render target не рисуются, у них
    /// `None`.
    rtv: Option<ID3D11RenderTargetView>,
}

impl std::fmt::Debug for Texture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Texture({}x{})", self.width, self.height)
    }
}

impl Texture {
    /// Ширина в пикселях.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Высота в пикселях.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Обернуть ЧУЖУЮ текстуру и уже созданный на неё SRV.
    ///
    /// Нужно для живого куска окна: кадр приходит текстурой от
    /// Windows.Graphics.Capture, её нельзя ни создать здесь, ни заполнить
    /// через `UpdateSubresource` — она уже лежит на общем с рендером
    /// устройстве. Обёртка берёт COM-ссылку (`Clone` = `AddRef`), поэтому
    /// вызов на каждый кадр не создаёт ни ресурсов, ни SRV — именно это и
    /// требуется, чтобы кусок не пересобирал GPU-объекты 60 раз в секунду.
    ///
    /// `rtv: None`: чужая текстура захвата в render target не рисуется —
    /// мы из неё только читаем.
    pub(crate) fn from_external(
        srv: ID3D11ShaderResourceView,
        texture: ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            srv,
            _texture: texture,
            width,
            height,
            rtv: None,
        }
    }

    /// SRV для биндинга в пиксельный шейдер (только внутри крейта).
    pub(crate) fn srv(&self) -> &ID3D11ShaderResourceView {
        &self.srv
    }

    /// RTV для рендера В эту текстуру — только у масок
    /// (`Device::create_mask_texture`). `None` у обычных спрайтовых текстур.
    /// Возвращает клон (тот же паттерн, что `WindowTarget::rtv`) — COM
    /// `AddRef`, дёшево.
    pub(crate) fn rtv(&self) -> Option<ID3D11RenderTargetView> {
        self.rtv.clone()
    }

    /// Загрузить RGBA-пиксели (straight alpha) в GPU: premultiply на CPU,
    /// текстура с мипмапами (если формат поддерживает автогенерацию — она
    /// поддерживается всеми практическими D3D11-устройствами; иначе одна
    /// мип-степень, честно и без падения).
    pub(crate) fn from_rgba(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        Self::from_rgba_inner(device, context, data, width, height, true)
    }

    /// Как [`Self::from_rgba`], но БЕЗ мипмапов — для текстурных атласов
    /// анимации (M5a): автогенерация усреднила бы соседние кадры в нижних
    /// мипах (цвет ячейки «протёк» бы в соседнюю), поэтому атлас всегда
    /// одно-миповый (docs/M5A_ANIMATION_DESIGN.md §3).
    pub(crate) fn from_rgba_atlas(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        Self::from_rgba_inner(device, context, data, width, height, false)
    }

    fn from_rgba_inner(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        data: &[u8],
        width: u32,
        height: u32,
        with_mips: bool,
    ) -> Result<Self, RenderError> {
        validate_texture_data(width, height, data.len())?;
        let mut data = data.to_vec();
        premultiply_rgba(&mut data);

        // Проверяем поддержку автогенерации мипмапов форматом.
        // SAFETY: вызов не трогает чужую память; устройство живо.
        let support = unsafe { device.CheckFormatSupport(DXGI_FORMAT_R8G8B8A8_UNORM) }
            .map_err(RenderError::Windows)?;
        let autogen = with_mips && support & D3D11_FORMAT_SUPPORT_MIP_AUTOGEN.0 as u32 != 0;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: if autogen { 0 } else { 1 },
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: if autogen {
                (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32
            } else {
                D3D11_BIND_SHADER_RESOURCE.0 as u32
            },
            CPUAccessFlags: 0,
            MiscFlags: if autogen {
                D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32
            } else {
                0
            },
        };
        // Начальных данных нет: с MipLevels=0 рантайм ожидал бы сразу ВСЕ
        // мип-уровни и отвечал бы E_INVALIDARG на один сабресурс. Заливаем
        // нулевой мип через UpdateSubresource, остальные достраивает GPU.
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(RenderError::Windows)?;
        let texture = texture.expect("CreateTexture2D без ошибки возвращает объект");

        // SAFETY: `texture` — валидный ID3D11Resource устройства контекста;
        // `data` живёт до конца вызова; UpdateSubresource копирует синхронно.
        unsafe {
            let tex_res: ID3D11Resource = texture.cast().map_err(RenderError::Windows)?;
            context.UpdateSubresource(Some(&tex_res), 0, None, data.as_ptr().cast(), width * 4, 0);
        }

        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: `texture` — валидный ID3D11Resource; out-параметр валиден.
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut srv)) }
            .map_err(RenderError::Windows)?;
        let srv = srv.expect("CreateShaderResourceView без ошибки возвращает объект");

        if autogen {
            // SAFETY: `srv` — валидное представление текстуры с флагом
            // GENERATE_MIPS; контекст того же устройства.
            unsafe { context.GenerateMips(Some(&srv)) };
        }
        Ok(Self {
            srv,
            _texture: texture,
            width,
            height,
            rtv: None,
        })
    }

    /// Обновить содержимое одно-миповой RGBA-текстуры новым кадром (ROADMAP.md
    /// M5a, «потоковый режим для очень длинных анимаций»): тот же размер,
    /// что при создании (`from_rgba_atlas`) — переиспользование текстуры на
    /// каждый декодированный кадр вместо пересоздания, тот же паттерн, что
    /// `update_r8` у видео. Straight alpha на входе, premultiply — как у
    /// `from_rgba_inner`.
    pub(crate) fn update_rgba(
        &self,
        context: &ID3D11DeviceContext,
        data: &[u8],
    ) -> Result<(), RenderError> {
        validate_texture_data(self.width, self.height, data.len())?;
        let mut data = data.to_vec();
        premultiply_rgba(&mut data);
        // SAFETY: `self._texture` — валидный ID3D11Resource устройства
        // контекста; `data` живёт до конца вызова; UpdateSubresource
        // копирует синхронно; RowPitch = width*4 (RGBA8).
        unsafe {
            let tex_res: ID3D11Resource =
                self._texture.clone().cast().map_err(RenderError::Windows)?;
            context.UpdateSubresource(
                Some(&tex_res),
                0,
                None,
                data.as_ptr().cast(),
                self.width * 4,
                0,
            );
        }
        Ok(())
    }

    /// Создать R8_UNORM текстурy-плоскость для видеокадра (M5b,
    /// docs/M5B_VIDEO_DESIGN.md §3): один 8-битный канал на тексель, один
    /// мип, без рендер-таргета. Y/U/V-плоскости видео обновляются на каждый
    /// показанный кадр через `update_r8` (UpdateSubresource) — пересоздание
    /// текстуры на каждый кадр дороже, поэтому это отдельный путь от
    /// `from_rgba`. Мипмапы не генерируются: плоскости перезаливаются
    /// целиком каждый кадр, автогенерация добавляла бы работу GPU без
    /// видимой выгоды на видеоспрайте (тот же аргумент, что у атласов M5a).
    pub(crate) fn from_r8(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        validate_plane_data(width, height, data.len())?;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(RenderError::Windows)?;
        let texture = texture.expect("CreateTexture2D без ошибки возвращает объект");

        // SAFETY: `texture` — валидный ID3D11Resource устройства контекста;
        // `data` живёт до конца вызова; UpdateSubresource копирует синхронно.
        unsafe {
            let tex_res: ID3D11Resource = texture.cast().map_err(RenderError::Windows)?;
            context.UpdateSubresource(Some(&tex_res), 0, None, data.as_ptr().cast(), width, 0);
        }

        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: `texture` — валидный ID3D11Resource; out-параметр валиден.
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut srv)) }
            .map_err(RenderError::Windows)?;
        let srv = srv.expect("CreateShaderResourceView без ошибки возвращает объект");

        Ok(Self {
            srv,
            _texture: texture,
            width,
            height,
            rtv: None,
        })
    }

    /// Обновить содержимое R8-плоскости новыми данными (M5b): тот же
    /// размер, что при создании (`from_r8`); переиспользование текстуры
    /// вместо пересоздания — видеокадры меняются каждый кадр, создавать
    /// три текстуры на каждый показанный кадр дорого.
    pub(crate) fn update_r8(
        &self,
        context: &ID3D11DeviceContext,
        data: &[u8],
    ) -> Result<(), RenderError> {
        validate_plane_data(self.width, self.height, data.len())?;
        // SAFETY: `self._texture` — валидный ID3D11Resource устройства
        // контекста; `data` живёт до конца вызова; UpdateSubresource
        // копирует синхронно; RowPitch = width (1 байт на тексель R8).
        unsafe {
            let tex_res: ID3D11Resource =
                self._texture.clone().cast().map_err(RenderError::Windows)?;
            context.UpdateSubresource(Some(&tex_res), 0, None, data.as_ptr().cast(), self.width, 0);
        }
        Ok(())
    }

    /// Создать R8_UNORM offscreen render target для маски перекрытия (M4,
    /// docs/M4_MASK_RENDER_DESIGN.md §3): рендер-таргет + SRV на одной
    /// текстуре (`Device::draw_mask` рисует в неё, `Device::draw_masked`
    /// сэмплирует в шейдере спрайта, t1). `R8_UNORM` достаточно поддержан
    /// FL 11.0 и как рендер-таргет, и как SRV — автогенерация мипмапов не
    /// нужна (маска сэмплируется 1:1 с backbuffer, mip 0 всегда).
    pub(crate) fn create_mask_target(
        device: &ID3D11Device,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidTextureData(
                "нулевая ширина или высота маски".to_string(),
            ));
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(RenderError::Windows)?;
        let texture = texture.expect("CreateTexture2D без ошибки возвращает объект");

        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: `texture` — валидный ID3D11Resource; out-параметр валиден.
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut srv)) }
            .map_err(RenderError::Windows)?;
        let srv = srv.expect("CreateShaderResourceView без ошибки возвращает объект");

        let mut rtv: Option<ID3D11RenderTargetView> = None;
        // SAFETY: `texture` — валидный ID3D11Resource с BIND_RENDER_TARGET;
        // out-параметр валиден.
        unsafe { device.CreateRenderTargetView(&texture, None, Some(&mut rtv)) }
            .map_err(RenderError::Windows)?;
        let rtv = rtv.expect("CreateRenderTargetView без ошибки возвращает объект");

        Ok(Self {
            srv,
            _texture: texture,
            width,
            height,
            rtv: Some(rtv),
        })
    }
}

/// Проверка входных данных текстуры. Вынесена из `from_rgba` отдельной
/// функцией, чтобы её можно было юнит-тестировать без GPU-устройства.
/// Переиспользуется `Device::create_texture_atlas` для проверки кадров.
pub(crate) fn validate_texture_data(
    width: u32,
    height: u32,
    data_len: usize,
) -> Result<(), RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::InvalidTextureData(
            "нулевая ширина или высота".to_string(),
        ));
    }
    let expected = width as usize * height as usize * 4;
    if data_len != expected {
        return Err(RenderError::InvalidTextureData(format!(
            "ожидалось {expected} байт RGBA, получено {data_len}"
        )));
    }
    Ok(())
}

/// Проверка входных данных одноканальной (R8) текстуры-плоскости (M5b,
/// видео): ровно `width * height` байт. Юнит-тестируется без GPU.
pub(crate) fn validate_plane_data(
    width: u32,
    height: u32,
    data_len: usize,
) -> Result<(), RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::InvalidTextureData(
            "нулевая ширина или высота".to_string(),
        ));
    }
    let expected = width as usize * height as usize;
    if data_len != expected {
        return Err(RenderError::InvalidTextureData(format!(
            "ожидалось {expected} байт R8, получено {data_len}"
        )));
    }
    Ok(())
}

/// Приведение straight alpha к premultiplied (требование
/// DXGI_ALPHA_MODE_PREMULTIPLIED у композиционной цепочки).
pub(crate) fn premultiply_rgba(data: &mut [u8]) {
    for px in data.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        px[0] = ((u32::from(px[0]) * a + 127) / 255) as u8;
        px[1] = ((u32::from(px[1]) * a + 127) / 255) as u8;
        px[2] = ((u32::from(px[2]) * a + 127) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::{premultiply_rgba, validate_plane_data, validate_texture_data};
    use crate::RenderError;

    #[test]
    fn premultiply_scales_rgb_keeps_alpha() {
        let mut data = [255u8, 100, 50, 128];
        premultiply_rgba(&mut data);
        assert_eq!(data[0], 128); // (255*128+127)/255
        assert_eq!(data[1], 50); // (100*128+127)/255
        assert_eq!(data[2], 25); // (50*128+127)/255
        assert_eq!(data[3], 128);
    }

    #[test]
    fn premultiply_zero_alpha_zeroes_rgb() {
        let mut data = [200u8, 150, 100, 0];
        premultiply_rgba(&mut data);
        assert_eq!(data, [0, 0, 0, 0]);
    }

    #[test]
    fn premultiply_opaque_is_identity() {
        let mut data = [10u8, 20, 30, 255];
        premultiply_rgba(&mut data);
        assert_eq!(data, [10, 20, 30, 255]);
    }

    #[test]
    fn premultiply_leaves_trailing_partial_pixel_untouched() {
        // chunks_exact_mut(4) обрабатывает только полные пиксели; «хвост»
        // меньше 4 байт остаётся без изменений.
        let mut data = [200u8, 150, 100, 0, 255];
        premultiply_rgba(&mut data);
        assert_eq!(data[..4], [0, 0, 0, 0]);
        assert_eq!(data[4], 255);
    }

    #[test]
    fn validate_rejects_zero_width() {
        let err = validate_texture_data(0, 8, 0).unwrap_err();
        assert!(matches!(err, RenderError::InvalidTextureData(_)));
    }

    #[test]
    fn validate_rejects_zero_height() {
        let err = validate_texture_data(8, 0, 0).unwrap_err();
        assert!(matches!(err, RenderError::InvalidTextureData(_)));
    }

    #[test]
    fn validate_rejects_wrong_data_len() {
        let err = validate_texture_data(2, 2, 15).unwrap_err();
        assert!(matches!(err, RenderError::InvalidTextureData(_)));
        assert!(validate_texture_data(2, 2, 17).is_err());
    }

    #[test]
    fn validate_accepts_exact_data_len() {
        assert!(validate_texture_data(2, 2, 16).is_ok());
    }

    #[test]
    fn plane_rejects_zero_width() {
        let err = validate_plane_data(0, 8, 0).unwrap_err();
        assert!(matches!(err, RenderError::InvalidTextureData(_)));
    }

    #[test]
    fn plane_rejects_zero_height() {
        let err = validate_plane_data(8, 0, 0).unwrap_err();
        assert!(matches!(err, RenderError::InvalidTextureData(_)));
    }

    #[test]
    fn plane_rejects_wrong_data_len() {
        let err = validate_plane_data(2, 2, 3).unwrap_err();
        assert!(matches!(err, RenderError::InvalidTextureData(_)));
        assert!(validate_plane_data(2, 2, 5).is_err());
    }

    #[test]
    fn plane_accepts_exact_data_len() {
        assert!(validate_plane_data(2, 2, 4).is_ok());
        assert!(validate_plane_data(31, 17, 31 * 17).is_ok());
    }
}
