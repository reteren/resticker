//! Процесс-wide D3D11-устройство (ARCHITECTURE.md, раздел 1: «Один D3D11-девайс
//! на процесс»; M3_PREP_NOTES.md, §4.2). Владеет устройством и контекстом,
//! скомпилированными шейдерами, сэмплером, blend-состоянием и константным
//! буфером; текстуры грузятся здесь — один раз на процесс — и рисуются на
//! любом [`crate::WindowTarget`] (любом мониторе) без перезаливки на GPU.

use std::path::Path;

use rst_core::model::Placement;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIDevice, IDXGIFactory2,
};
use windows::core::{Interface, PCSTR};

use crate::sprite::Sprite;
use crate::texture::Texture;
use crate::window_target::WindowTarget;
use crate::{RenderError, shader};

/// Константный буфер шейдера спрайта: строго три float4 под HLSL-упаковку
/// по 16 байт (урок спайка S0 — float4 после float2 съезжает на границу).
#[repr(C)]
struct SpriteParams {
    /// cx, cy, w, h в физических пикселях.
    tr: [f32; 4],
    /// cos φ, sin φ, opacity, flip_h (±1).
    misc: [f32; 4],
    /// flip_v (±1), screen_w, screen_h, pad.
    misc2: [f32; 4],
}

/// Перевод Placement (DIP, координаты центра) в физические пиксели.
fn placement_to_physical(p: &Placement, scale: f32) -> [f32; 4] {
    [
        p.cx as f32 * scale,
        p.cy as f32 * scale,
        p.w as f32 * scale,
        p.h as f32 * scale,
    ]
}

/// D3D11-устройство процесса: один экземпляр на процесс, любое число
/// [`WindowTarget`] на его базе.
///
/// Владение: все COM-объекты — умные указатели windows-rs, `Release` вызывается
/// автоматически в `Drop`. Текстуры живут на устройстве: после потери D3D
/// (`DEVICE_REMOVED`) пересоздаётся устройство целиком, затем — все цели и
/// текстуры (ARCHITECTURE.md, раздел 11; M3_PREP_NOTES.md, §4.3).
pub struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// DXGI-фабрика под композиционные цепочки целей (одна на устройство).
    factory: IDXGIFactory2,
    /// DirectComposition-устройство: общее, цели создаются на конкретный HWND.
    dcomp_device: IDCompositionDevice,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
}

impl Device {
    /// Создать D3D11-устройство процесса с BGRA-поддержкой (нужна
    /// композиционным цепочкам), DXGI-фабрикой, DirectComposition-устройством
    /// и шейдером спрайта. Проверено спайком S0 (ADR-003). GPU-текстур не
    /// создаёт — их грузят по требованию (`create_texture_from_rgba`).
    pub fn new() -> Result<Self, RenderError> {
        // --- D3D11-устройство (BGRA нужен композиционным цепочкам) ---
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: out-параметры валидны; возвращаемые объекты принадлежат нам.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(RenderError::Windows)?;
        let device = device.expect("D3D11CreateDevice без ошибки возвращает устройство");
        let context = context.expect("D3D11CreateDevice без ошибки возвращает контекст");

        // --- DXGI-фабрика под композиционные цепочки целей ---
        // SAFETY: вызов без параметров-указателей; возвращённая фабрика наша.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(RenderError::Windows)?;

        // --- DirectComposition: общее устройство, цели создаёт WindowTarget ---
        // Interface::cast — безопасный QueryInterface: для D3D11-устройства
        // IDXGIDevice гарантирован.
        let dxgi_dev: IDXGIDevice = device.cast().map_err(RenderError::Windows)?;
        // SAFETY: `dxgi_dev` — валидный DXGI-устройство того же адаптера.
        let dcomp_device: IDCompositionDevice =
            unsafe { DCompositionCreateDevice(&dxgi_dev) }.map_err(RenderError::Windows)?;

        // --- Шейдер спрайта ---
        // SAFETY: литералы с NUL на конце живут в статике.
        let vs_blob = shader::compile(
            PCSTR::from_raw(c"mainVS".as_ptr().cast()),
            PCSTR::from_raw(c"vs_5_0".as_ptr().cast()),
        )?;
        let ps_blob = shader::compile(
            PCSTR::from_raw(c"mainPS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        )?;
        let mut vs: Option<ID3D11VertexShader> = None;
        // SAFETY: байткод из живого blob; out-параметр валиден.
        unsafe { device.CreateVertexShader(shader::blob_bytes(&vs_blob), None, Some(&mut vs)) }
            .map_err(RenderError::Windows)?;
        let vs = vs.expect("CreateVertexShader без ошибки возвращает объект");
        let mut ps: Option<ID3D11PixelShader> = None;
        // SAFETY: байткод из живого blob; out-параметр валиден.
        unsafe { device.CreatePixelShader(shader::blob_bytes(&ps_blob), None, Some(&mut ps)) }
            .map_err(RenderError::Windows)?;
        let ps = ps.expect("CreatePixelShader без ошибки возвращает объект");

        // --- Константный буфер, сэмплер, blend-состояние ---
        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: size_of::<SpriteParams>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut cb: Option<ID3D11Buffer> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateBuffer(&cb_desc, None, Some(&mut cb)) }
            .map_err(RenderError::Windows)?;
        let cb = cb.expect("CreateBuffer без ошибки возвращает объект");

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_ANISOTROPIC,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxAnisotropy: 4,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler: Option<ID3D11SamplerState> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateSamplerState(&sampler_desc, Some(&mut sampler)) }
            .map_err(RenderError::Windows)?;
        let sampler = sampler.expect("CreateSamplerState без ошибки возвращает объект");

        // Premultiplied alpha: out = src + dst * (1 - src.a).
        let rt_blend = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend_desc = D3D11_BLEND_DESC::default();
        blend_desc.RenderTarget[0] = rt_blend;
        let mut blend: Option<ID3D11BlendState> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateBlendState(&blend_desc, Some(&mut blend)) }
            .map_err(RenderError::Windows)?;
        let blend = blend.expect("CreateBlendState без ошибки возвращает объект");

        Ok(Self {
            device,
            context,
            factory,
            dcomp_device,
            vs,
            ps,
            cb,
            sampler,
            blend,
        })
    }

    /// Загрузить изображение из файла (PNG/JPEG/WebP/BMP) в GPU-текстуру
    /// с мипмапами. Даунскейл >4096 и кэш — зона rst-media, не этого крейта.
    pub fn load_image(&self, path: &Path) -> Result<Texture, RenderError> {
        let img = image::open(path).map_err(|source| RenderError::ImageDecode {
            path: path.display().to_string(),
            source,
        })?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        self.create_texture_from_rgba(&rgba, w, h)
    }

    /// Загрузить RGBA-пиксели (straight alpha) в GPU-текстуру с мипмапами.
    /// Текстура принадлежит устройству и рисуется на любой [`WindowTarget`] —
    /// перетаскивание стикера между мониторами не перезаливает её на GPU.
    pub fn create_texture_from_rgba(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Texture, RenderError> {
        Texture::from_rgba(&self.device, &self.context, data, width, height)
    }

    /// Отрисовать список спрайтов в цель `target` (порядок списка = порядок
    /// отрисовки, первый — нижний) и представить кадр. Вызывается строго по
    /// требованию (ADR-006): внутреннего цикла и таймеров нет, ноль вызовов =
    /// ноль кадров в покое. Мутация — только состояние GPU-контекста, поэтому
    /// метод берёт `&self`: один и тот же кадр можно отдать на все цели.
    pub fn draw(&self, target: &WindowTarget, sprites: &[Sprite]) -> Result<(), RenderError> {
        let (w, h) = target.size();
        let Some(rtv) = target.rtv() else {
            return Ok(()); // окно с нулевым размером: рисовать некуда
        };
        if w == 0 || h == 0 {
            return Ok(());
        }

        // SAFETY: все COM-объекты живы и принадлежат self; контекст
        // используется только с потока-владельца (Device намеренно не
        // Send/Sync). Указатели на массивы валидны на время вызова.
        unsafe {
            let clear = [0.0f32; 4]; // полностью прозрачный фон
            self.context.ClearRenderTargetView(&rtv, &clear);
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.context.RSSetViewports(Some(&[vp]));
            self.context.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            self.context.OMSetBlendState(&self.blend, None, 0xffffffff);
            self.context.IASetInputLayout(None);
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.VSSetShader(&self.vs, None);
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.context.PSSetShader(&self.ps, None);
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        }

        // SAFETY: `cb` — валидный ID3D11Resource; mapped-память валидна
        // между Map и Unmap; SpriteParams — repr(C) и помещается в буфер.
        unsafe {
            let cb_res: ID3D11Resource = self.cb.cast().map_err(RenderError::Windows)?;
            let scale = target.dpi_scale();
            for sprite in sprites {
                if sprite.placement.w <= 0.0 || sprite.placement.h <= 0.0 {
                    continue;
                }
                let [cx, cy, sw, sh] = placement_to_physical(&sprite.placement, scale);
                let (sin, cos) = (sprite.transform.rotation as f32).sin_cos();
                let params = SpriteParams {
                    tr: [cx, cy, sw, sh],
                    misc: [
                        cos,
                        sin,
                        sprite.transform.opacity as f32,
                        if sprite.transform.flip_h { -1.0 } else { 1.0 },
                    ],
                    misc2: [
                        if sprite.transform.flip_v { -1.0 } else { 1.0 },
                        w as f32,
                        h as f32,
                        0.0,
                    ],
                };
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(
                        Some(&cb_res),
                        0,
                        D3D11_MAP_WRITE_DISCARD,
                        0,
                        Some(&mut mapped),
                    )
                    .map_err(RenderError::Windows)?;
                std::ptr::copy_nonoverlapping(&params, mapped.pData.cast::<SpriteParams>(), 1);
                self.context.Unmap(Some(&cb_res), 0);
                self.context
                    .PSSetShaderResources(0, Some(&[Some(sprite.texture.srv().clone())]));
                self.context.Draw(6, 0);
            }
        }
        target.present()
    }

    /// Доступ к D3D11-устройству для целей рендера (RTV, swapchain).
    pub(crate) fn d3d_device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Общее DirectComposition-устройство (цели создаются на конкретный HWND).
    pub(crate) fn dcomp_device(&self) -> &IDCompositionDevice {
        &self.dcomp_device
    }

    /// DXGI-фабрика под композиционные цепочки целей.
    pub(crate) fn factory(&self) -> &IDXGIFactory2 {
        &self.factory
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_core::model::MonitorId;

    #[test]
    fn sprite_params_layout_matches_hlsl_packing() {
        assert_eq!(size_of::<SpriteParams>(), 48);
        assert_eq!(std::mem::offset_of!(SpriteParams, tr), 0);
        assert_eq!(std::mem::offset_of!(SpriteParams, misc), 16);
        assert_eq!(std::mem::offset_of!(SpriteParams, misc2), 32);
    }

    #[test]
    fn placement_scales_to_physical() {
        let p = Placement {
            monitor_id: MonitorId::default(),
            cx: 100.0,
            cy: 50.0,
            w: 200.0,
            h: 80.0,
        };
        assert_eq!(placement_to_physical(&p, 1.5), [150.0, 75.0, 300.0, 120.0]);
        assert_eq!(placement_to_physical(&p, 1.0), [100.0, 50.0, 200.0, 80.0]);
    }
}
