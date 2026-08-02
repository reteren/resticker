//! Рендерер: D3D11-устройство + DirectComposition-цепочка на окно,
//! отрисовка списка спрайтов строго по требованию (ADR-006).

use std::path::Path;

use rst_core::model::Placement;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, PCSTR};

use crate::sprite::Sprite;
use crate::texture::Texture;
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

/// D3D11 + DirectComposition рендерер на одно окно.
///
/// Владение: все COM-объекты — умные указатели windows-rs, `Release` вызывается
/// автоматически в `Drop`; сырой хендл окна рендереру не принадлежит — окно
/// (из rst-win32) обязано пережить рендерер. M3 позже: сейчас один рендерер =
/// одно устройство; на процесс с несколькими мониторами устройство будет
/// общим (ARCHITECTURE.md, раздел 1).
pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    // Держатели композиционного дерева: читаются не будут, но обязаны жить,
    // пока жив рендерер — drop сносит визуальное дерево окна.
    _dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    size: (u32, u32),
    scale: f32,
}

impl Renderer {
    /// Создать рендерер на окно `hwnd` размером `width`×`height` физических
    /// пикселей: D3D11-устройство, DirectComposition-цепочка с premultiplied
    /// alpha, шейдер спрайта. Проверено спайком S0 (ADR-003).
    ///
    /// Окно должно иметь `WS_EX_NOREDIRECTIONBITMAP`: содержимое показывается
    /// только через DirectComposition, обычный Present на HWND невозможен.
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, RenderError> {
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

        // --- DirectComposition: device -> target(hwnd, topmost) -> visual ---
        // Interface::cast — безопасный QueryInterface: для D3D11-устройства
        // IDXGIDevice гарантирован.
        let dxgi_dev: IDXGIDevice = device.cast().map_err(RenderError::Windows)?;
        // SAFETY: `dxgi_dev` — валидный DXGI-устройство того же адаптера.
        let dcomp_device: IDCompositionDevice =
            unsafe { DCompositionCreateDevice(&dxgi_dev) }.map_err(RenderError::Windows)?;
        // SAFETY: `hwnd` принадлежит вызывающей стороне и жив дольше рендерера.
        let dcomp_target = unsafe { dcomp_device.CreateTargetForHwnd(hwnd, true) }
            .map_err(RenderError::Windows)?;
        // SAFETY: dcomp_device жив; visual — новый, владеем им мы.
        let dcomp_visual = unsafe { dcomp_device.CreateVisual() }.map_err(RenderError::Windows)?;
        // SAFETY: target и visual живы и принадлежат нам.
        unsafe { dcomp_target.SetRoot(Some(&dcomp_visual)) }.map_err(RenderError::Windows)?;

        // --- Композиционная цепочка с premultiplied alpha ---
        let size = (width, height);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        // SAFETY: вызов без параметров-указателей; возвращённая фабрика наша.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(RenderError::Windows)?;
        // SAFETY: `device` жив; desc валиден; цепочка для композиции, а не HWND.
        let swapchain = unsafe { factory.CreateSwapChainForComposition(&device, &desc, None) }
            .map_err(RenderError::Windows)?;
        // SAFETY: visual и swapchain живы и принадлежат нам.
        unsafe { dcomp_visual.SetContent(&swapchain) }.map_err(RenderError::Windows)?;
        // SAFETY: dcomp_device жив; Commit фиксирует дерево композиции.
        unsafe { dcomp_device.Commit() }.map_err(RenderError::Windows)?;

        let rtv = (width > 0 && height > 0)
            .then(|| create_rtv(&device, &swapchain))
            .transpose()?;

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
            _dcomp_device: dcomp_device,
            _dcomp_target: dcomp_target,
            _dcomp_visual: dcomp_visual,
            swapchain,
            rtv,
            vs,
            ps,
            cb,
            sampler,
            blend,
            size,
            scale: 1.0,
        })
    }

    /// Текущий размер цепочки в физических пикселях.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Масштаб DIP → физические пиксели (dpi/96). M1: задаётся один раз;
    /// полноценный смешанный DPI и WM_DPICHANGED — веха M3.
    pub fn set_dpi_scale(&mut self, scale: f32) {
        self.scale = if scale > 0.0 { scale } else { 1.0 };
    }

    /// Текущий масштаб DIP → физические пиксели — нужен вызывающему коду
    /// (M2: перевод координат мыши из физических пикселей в DIP перед
    /// хит-тестом, docs/M2_INTEGRATION_REVIEW.md, раздел 2).
    pub fn dpi_scale(&self) -> f32 {
        self.scale
    }

    /// Сообщить рендереру новый размер окна (физические пиксели).
    /// Нулевой размер (свёрнуто/скрыто) — не ошибка: кадры просто пропускаются.
    /// Полноценная мультимониторная пересборка — веха M3.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if (width, height) == self.size {
            return Ok(());
        }
        self.size = (width, height);
        // Все ссылки на backbuffer обязаны быть отпущены до ResizeBuffers.
        self.rtv = None;
        if width == 0 || height == 0 {
            return Ok(());
        }
        // SAFETY: ссылки на backbuffer отпущены выше; размеры ненулевые.
        unsafe {
            self.swapchain.ResizeBuffers(
                2,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }
        .map_err(RenderError::Windows)?;
        self.rtv = Some(create_rtv(&self.device, &self.swapchain)?);
        Ok(())
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
    pub fn create_texture_from_rgba(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Texture, RenderError> {
        Texture::from_rgba(&self.device, &self.context, data, width, height)
    }

    /// Отрисовать список спрайтов (порядок списка = порядок отрисовки,
    /// первый — нижний) и представить кадр. Вызывается строго по требованию
    /// (ADR-006): внутреннего цикла и таймеров нет, ноль вызовов = ноль
    /// кадров в покое.
    pub fn draw(&mut self, sprites: &[Sprite]) -> Result<(), RenderError> {
        let (w, h) = self.size;
        let Some(rtv) = self.rtv.clone() else {
            return Ok(()); // окно с нулевым размером: рисовать некуда
        };
        if w == 0 || h == 0 {
            return Ok(());
        }

        // SAFETY: все COM-объекты живы и принадлежат self; контекст
        // используется только с потока-владельца рендерера (Renderer
        // намеренно не Send/Sync). Указатели на массивы валидны на время вызова.
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
            for sprite in sprites {
                if sprite.placement.w <= 0.0 || sprite.placement.h <= 0.0 {
                    continue;
                }
                let [cx, cy, sw, sh] = placement_to_physical(&sprite.placement, self.scale);
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
        self.present()
    }

    /// Представить кадр (vsync). Потеря устройства возвращается отдельным
    /// вариантом ошибки: рендерер надо пересоздать (ARCHITECTURE.md, раздел 11).
    fn present(&self) -> Result<(), RenderError> {
        // SAFETY: swapchain жив и принадлежит self.
        let hr = unsafe { self.swapchain.Present(1, DXGI_PRESENT(0)) };
        hr.ok().map_err(|e| {
            let code = e.code();
            if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
                RenderError::DeviceLost(code)
            } else {
                RenderError::Windows(e)
            }
        })
    }
}

/// Создать RTV на backbuffer цепочки (общий код `new` и `resize`).
fn create_rtv(
    device: &ID3D11Device,
    swapchain: &IDXGISwapChain1,
) -> Result<ID3D11RenderTargetView, RenderError> {
    // SAFETY: цепочка жива; буфер 0 существует (BufferCount = 2).
    let back: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0) }.map_err(RenderError::Windows)?;
    let mut rtv: Option<ID3D11RenderTargetView> = None;
    // SAFETY: `back` — валидный ID3D11Resource и отпускается сразу после;
    // out-параметр валиден.
    unsafe { device.CreateRenderTargetView(&back, None, Some(&mut rtv)) }
        .map_err(RenderError::Windows)?;
    Ok(rtv.expect("CreateRenderTargetView без ошибки возвращает объект"))
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
