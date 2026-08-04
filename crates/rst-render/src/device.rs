//! Процесс-wide D3D11-устройство (ARCHITECTURE.md, раздел 1: «Один D3D11-девайс
//! на процесс»; M3_PREP_NOTES.md, §4.2). Владеет устройством и контекстом,
//! скомпилированными шейдерами, сэмплером, blend-состоянием и константным
//! буфером; текстуры грузятся здесь — один раз на процесс — и рисуются на
//! любом [`crate::WindowTarget`] (любом мониторе) без перезаливки на GPU.

use std::path::Path;

use rst_core::model::{Placement, Rect};
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
    /// M4: PS маски перекрытия (`mainMaskPS`, SDF скруглённого прямоугольника).
    mask_ps: ID3D11PixelShader,
    /// M4: `POINT`/`CLAMP` — точное совпадение текселя маски с пикселем
    /// экрана (никакой фильтрации на краях выреза, docs/M4_MASK_RENDER_DESIGN.md §4.4).
    mask_sampler: ID3D11SamplerState,
    /// M4: аддитивный блендинг для объединения оклюдеров в одной маске
    /// (`ONE, ONE, ADD` — значения могут превысить 1, порог `> 0.5` в
    /// шейдере спрайта это переваривает; `BLEND_OP_MAX` не используем, чтобы
    /// не завязываться на FL 11.1).
    mask_blend: ID3D11BlendState,
    /// M4: 1×1 R8-текстура, очищенная в 0 — «нет маски» по умолчанию.
    /// `mainPS` теперь ВСЕГДА сэмплирует `t1` (маска — часть общего шейдера
    /// спрайта, не отдельная точка входа), поэтому `draw()` — путь M2/M3 без
    /// изменений вызова — обязан явно забиндить что-то в `t1`, а не полагаться
    /// на неявное поведение D3D11 «несвязанный SRV даёт 0» (корректно, но
    /// неявно). Тот же текстур служит фолбэком `draw_masked` для `None`.
    mask_empty: Texture,
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

        // --- PS маски перекрытия (M4) — тот же VS/CB, отдельная точка входа ---
        let mask_ps_blob = shader::compile(
            PCSTR::from_raw(c"mainMaskPS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        )?;
        let mut mask_ps: Option<ID3D11PixelShader> = None;
        // SAFETY: байткод из живого blob; out-параметр валиден.
        unsafe {
            device.CreatePixelShader(shader::blob_bytes(&mask_ps_blob), None, Some(&mut mask_ps))
        }
        .map_err(RenderError::Windows)?;
        let mask_ps = mask_ps.expect("CreatePixelShader без ошибки возвращает объект");

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

        // --- Сэмплер и blend-состояние маски перекрытия (M4) ---
        let mask_sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut mask_sampler: Option<ID3D11SamplerState> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateSamplerState(&mask_sampler_desc, Some(&mut mask_sampler)) }
            .map_err(RenderError::Windows)?;
        let mask_sampler = mask_sampler.expect("CreateSamplerState без ошибки возвращает объект");

        // Аддитивное объединение оклюдеров в одной маске (docs/M4_MASK_RENDER_DESIGN.md §4.1).
        let mask_rt_blend = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_ONE,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_ONE,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut mask_blend_desc = D3D11_BLEND_DESC::default();
        mask_blend_desc.RenderTarget[0] = mask_rt_blend;
        let mut mask_blend: Option<ID3D11BlendState> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateBlendState(&mask_blend_desc, Some(&mut mask_blend)) }
            .map_err(RenderError::Windows)?;
        let mask_blend = mask_blend.expect("CreateBlendState без ошибки возвращает объект");

        // --- «Нет маски» по умолчанию: 1x1 R8, очищена в 0 ---
        let mask_empty = Texture::create_mask_target(&device, 1, 1)?;
        // SAFETY: только что созданный RTV собственной текстуры устройства;
        // единоразовая очистка при старте, вне цикла отрисовки.
        unsafe {
            let rtv = mask_empty
                .rtv()
                .expect("create_mask_target всегда даёт RTV");
            context.ClearRenderTargetView(&rtv, &[0.0f32; 4]);
        }

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
            mask_ps,
            mask_sampler,
            mask_blend,
            mask_empty,
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
            self.context
                .PSSetSamplers(1, Some(&[Some(self.mask_sampler.clone())]));
            // Маска не варьируется по спрайту в этом пути — биндится один
            // раз, вне цикла: `mainPS` теперь всегда сэмплирует t1, но у
            // немаскированной отрисовки (M2/M3, этот метод) окклюдеров нет.
            self.context
                .PSSetShaderResources(1, Some(&[Some(self.mask_empty.srv().clone())]));
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

    /// Создать маску перекрытия (M4, docs/M4_MASK_RENDER_DESIGN.md §3):
    /// offscreen R8-рендер-таргет размером `width`×`height` физических px —
    /// должен совпадать 1:1 с размером цели монитора, для которого считается
    /// (`WindowTarget::size()`), иначе `SV_Position`-сэмплинг в `mainPS`
    /// разъедется с реальными пикселями. Пересоздавать при ресайзе/DPI-смене
    /// монитора и после `DEVICE_REMOVED`, как и сам `WindowTarget`.
    pub fn create_mask_texture(&self, width: u32, height: u32) -> Result<Texture, RenderError> {
        Texture::create_mask_target(&self.device, width, height)
    }

    /// Залить маску `mask` объединением скруглённых прямоугольников
    /// `rects` (физические px, локальные для монитора — см.
    /// `rst_core::occluders::clip_rect`). Чистит маску в 0, затем рисует
    /// каждый прямоугольник аддитивно (`mask_blend`) через SDF-заливку
    /// (`mainMaskPS`, радиус 8 px). Не вызывает `present` — маска не
    /// показывается сама по себе, только сэмплируется `draw_masked`.
    pub fn draw_mask(&self, mask: &Texture, rects: &[Rect]) -> Result<(), RenderError> {
        let Some(rtv) = mask.rtv() else {
            return Err(RenderError::InvalidTextureData(
                "draw_mask вызван на текстуре без RTV — не создана create_mask_texture".to_string(),
            ));
        };
        let (w, h) = (mask.width(), mask.height());
        if w == 0 || h == 0 {
            return Ok(());
        }

        // SAFETY: все COM-объекты живы и принадлежат self; контекст
        // используется только с потока-владельца (Device намеренно не
        // Send/Sync).
        unsafe {
            self.context.ClearRenderTargetView(&rtv, &[0.0f32; 4]);
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
            self.context
                .OMSetBlendState(&self.mask_blend, None, 0xffffffff);
            self.context.IASetInputLayout(None);
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            // Тот же VS/CB, что у спрайта (docs/M4_MASK_RENDER_DESIGN.md §4.1)
            // — только PS другой.
            self.context.VSSetShader(&self.vs, None);
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.context.PSSetShader(&self.mask_ps, None);
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
        }

        // SAFETY: `cb` — валидный ID3D11Resource; mapped-память валидна
        // между Map и Unmap; SpriteParams — repr(C) и помещается в буфер.
        unsafe {
            let cb_res: ID3D11Resource = self.cb.cast().map_err(RenderError::Windows)?;
            for rect in rects {
                if rect.w == 0 || rect.h == 0 {
                    continue;
                }
                // Rect — top-left/w/h; CB (tr) ждёт центр/размер, как Placement.
                let cx = rect.x as f32 + rect.w as f32 * 0.5;
                let cy = rect.y as f32 + rect.h as f32 * 0.5;
                let params = SpriteParams {
                    tr: [cx, cy, rect.w as f32, rect.h as f32],
                    // Оклюдер не повёрнут (cos=1, sin=0), opacity/flip не
                    // используются `mainMaskPS`, но mainVS их всё равно читает.
                    misc: [1.0, 0.0, 1.0, 1.0],
                    misc2: [1.0, w as f32, h as f32, 0.0],
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
                self.context.Draw(6, 0);
            }
        }
        Ok(())
    }

    /// Как [`Self::draw`], но каждый спрайт вырезается по своей маске
    /// (M4): `masks[i]` — маска стикера `sprites[i]` (`None` — стикер без
    /// ограничений видимости, биндится встроенная пустая маска, тот же
    /// эффект, что `draw()`). `masks.len()` должен совпадать с
    /// `sprites.len()` — лишние/недостающие элементы просто не влияют на
    /// хвост без пары (используется пустая маска).
    pub fn draw_masked(
        &self,
        target: &WindowTarget,
        sprites: &[Sprite],
        masks: &[Option<&Texture>],
    ) -> Result<(), RenderError> {
        let (w, h) = target.size();
        let Some(rtv) = target.rtv() else {
            return Ok(());
        };
        if w == 0 || h == 0 {
            return Ok(());
        }

        // SAFETY: см. draw() — тот же контекст, та же нить-владелец.
        unsafe {
            let clear = [0.0f32; 4];
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
            self.context
                .PSSetSamplers(1, Some(&[Some(self.mask_sampler.clone())]));
        }

        // SAFETY: см. draw() — тот же паттерн Map/Unmap-на-спрайт.
        unsafe {
            let cb_res: ID3D11Resource = self.cb.cast().map_err(RenderError::Windows)?;
            let scale = target.dpi_scale();
            for (i, sprite) in sprites.iter().enumerate() {
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
                let mask_srv = match masks.get(i).copied().flatten() {
                    Some(mask) => mask.srv().clone(),
                    None => self.mask_empty.srv().clone(),
                };
                self.context
                    .PSSetShaderResources(1, Some(&[Some(mask_srv)]));
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

/// Тесты маски перекрытия (M4), требующие реального GPU-устройства.
///
/// Живут ЗДЕСЬ (внутри крейта), а не в `tests/gpu_smoke.rs`: CPU-readback
/// нужен `ID3D11Device`/`ID3D11DeviceContext` (приватные поля `Device`) и
/// ресурс за RTV текстуры/цели (`Texture::rtv`/`WindowTarget::rtv` —
/// `pub(crate)`) — ничего из этого не видно из отдельного интеграционного
/// бинаря `tests/`, только из модуля-потомка `device.rs`. Запуск вручную:
/// `cargo test -p rst-render --lib -- --ignored`.
#[cfg(test)]
mod gpu_tests {
    use super::*;
    use rst_core::model::{MonitorId, Placement, Transform};
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    /// Прочитать одноканальную (R8_UNORM) текстуру устройства в CPU-буфер
    /// (staging + `CopyResource` + `Map`), построчно — GPU может паддить
    /// строки (`RowPitch` не обязан равняться `width`).
    fn read_r8_texture(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        resource: &ID3D11Resource,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
            .expect("staging-текстура создаётся");
        let staging = staging.expect("CreateTexture2D без ошибки возвращает объект");
        let staging_res: ID3D11Resource = staging.cast().expect("Texture2D -> Resource");
        // SAFETY: `resource` и `staging_res` — валидные ресурсы одного
        // устройства, совпадающих формата/размера (R8_UNORM, width×height).
        unsafe { context.CopyResource(&staging_res, resource) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging_res` создан с `CPU_ACCESS_READ`; out-параметр валиден.
        unsafe { context.Map(Some(&staging_res), 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .expect("Map staging-текстуры");
        let mut out = vec![0u8; (width * height) as usize];
        // SAFETY: `mapped.pData` валиден на `height` строк по `RowPitch`
        // байт каждая (D3D11 гарантирует это для успешного `Map`).
        unsafe {
            for y in 0..height {
                let row_ptr = mapped
                    .pData
                    .cast::<u8>()
                    .add((y * mapped.RowPitch) as usize);
                let row = std::slice::from_raw_parts(row_ptr, width as usize);
                out[(y * width) as usize..(y * width + width) as usize].copy_from_slice(row);
            }
            context.Unmap(Some(&staging_res), 0);
        }
        out
    }

    /// Как [`read_r8_texture`], но для `B8G8R8A8_UNORM` (backbuffer
    /// `WindowTarget`) — 4 байта на пиксель, тот же паттерн staging/`RowPitch`.
    fn read_bgra_texture(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        resource: &ID3D11Resource,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: desc валиден; out-параметр валиден.
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
            .expect("staging-текстура создаётся");
        let staging = staging.expect("CreateTexture2D без ошибки возвращает объект");
        let staging_res: ID3D11Resource = staging.cast().expect("Texture2D -> Resource");
        // SAFETY: `resource` и `staging_res` — валидные ресурсы одного
        // устройства, совпадающих формата/размера (BGRA, width×height).
        unsafe { context.CopyResource(&staging_res, resource) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging_res` создан с `CPU_ACCESS_READ`; out-параметр валиден.
        unsafe { context.Map(Some(&staging_res), 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .expect("Map staging-текстуры");
        let row_bytes = (width * 4) as usize;
        let mut out = vec![0u8; row_bytes * height as usize];
        // SAFETY: см. `read_r8_texture` — тот же RowPitch-паттерн, 4 байта/px.
        unsafe {
            for y in 0..height {
                let row_ptr = mapped
                    .pData
                    .cast::<u8>()
                    .add((y * mapped.RowPitch) as usize);
                let row = std::slice::from_raw_parts(row_ptr, row_bytes);
                out[y as usize * row_bytes..(y as usize + 1) * row_bytes].copy_from_slice(row);
            }
            context.Unmap(Some(&staging_res), 0);
        }
        out
    }

    #[test]
    #[ignore = "требует GPU и дисплей; запуск вручную: cargo test -p rst-render --lib -- --ignored"]
    fn draw_mask_produces_expected_coverage() {
        let device = Device::new().expect("устройство создаётся на GPU");
        let (w, h) = (64u32, 64u32);
        let mask = device.create_mask_texture(w, h).expect("маска создаётся");
        let rect = Rect {
            x: 16,
            y: 16,
            w: 32,
            h: 32,
        };
        device
            .draw_mask(&mask, &[rect])
            .expect("draw_mask не падает");

        // SAFETY: RTV только что создан `create_mask_texture`, ресурс за ним
        // жив, пока жива `mask`.
        let resource: ID3D11Resource = unsafe {
            mask.rtv()
                .expect("create_mask_texture всегда даёт RTV")
                .GetResource()
        }
        .expect("RTV даёт исходный ресурс");
        let pixels = read_r8_texture(&device.device, &device.context, &resource, w, h);
        let at = |x: u32, y: u32| pixels[(y * w + x) as usize];

        // Центр прямоугольника, вдали от скруглённых углов, — занят маской.
        assert!(
            at(32, 32) > 250,
            "центр прямоугольника должен быть занят маской: {}",
            at(32, 32)
        );
        // Далеко за пределами прямоугольника — не occluded.
        assert_eq!(
            at(4, 4),
            0,
            "точка вне прямоугольника не должна быть замаскирована"
        );
        // Радиус скругления — 8px (mainMaskPS): на строке верхнего края
        // прямоугольника (y = rect.y), между углом AABB (x = rect.x, вне
        // скруглённой формы) и началом прямого участка края (x = rect.x + 8,
        // уже внутри) должна быть зона антиалиасинга — значение строго между
        // 0 и 255 хотя бы в одной точке.
        let corner_has_partial = (0..8).any(|d| {
            let v = at(16 + d, 16);
            v > 0 && v < 255
        });
        assert!(
            corner_has_partial,
            "должна быть антиалиасинг-зона у скруглённого угла"
        );
    }

    #[test]
    #[ignore = "требует GPU и дисплей; запуск вручную: cargo test -p rst-render --lib -- --ignored"]
    fn draw_masked_cuts_out_occluded_sprite() {
        // SAFETY: окно системного класса Static, как в tests/gpu_smoke.rs —
        // регистрация своего класса не нужна, все параметры валидны.
        let hwnd = unsafe {
            let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("Static"),
                w!("rst-render mask gpu test"),
                WS_POPUP,
                0,
                0,
                64,
                64,
                None,
                None,
                Some(hinst),
                None,
            )
            .unwrap()
        };
        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };

        let device = Device::new().expect("устройство создаётся на GPU");
        let target = WindowTarget::new(&device, hwnd, 64, 64).expect("цель создаётся");

        // Непрозрачный жёлтый спрайт на весь экран (premultiplied: полная
        // альфа, цвет как есть).
        let sprite_tex = device
            .create_texture_from_rgba(&[255, 255, 0, 255], 1, 1)
            .expect("текстура спрайта создаётся");
        let sprite = Sprite::new(
            sprite_tex,
            Placement {
                monitor_id: MonitorId(String::new()),
                cx: 32.0,
                cy: 32.0,
                w: 64.0,
                h: 64.0,
            },
            Transform::default(),
        );

        // Маска покрывает ЛЕВУЮ половину экрана (x: 0..32) на всю высоту —
        // на середине высоты (y=32) это прямой край, без скругления углов
        // (те — только у верхней/нижней пары углов), так что x=32 — чёткая
        // граница occluded/visible без зоны неоднозначности.
        let mask = device.create_mask_texture(64, 64).expect("маска создаётся");
        device
            .draw_mask(
                &mask,
                &[Rect {
                    x: 0,
                    y: 0,
                    w: 32,
                    h: 64,
                }],
            )
            .expect("draw_mask не падает");

        device
            .draw_masked(&target, std::slice::from_ref(&sprite), &[Some(&mask)])
            .expect("draw_masked не падает");
        // Композиционный swapchain (FLIP_SEQUENTIAL, 2 буфера) — после
        // одного Present() тот RTV, что закэширован `WindowTarget` с момента
        // создания, может указывать не на тот буфер, что реально сейчас
        // «текущий back buffer»; второй одинаковый кадр гарантирует, что
        // именно ЭТОТ (закэшированный) буфер получил актуальную отрисовку
        // непосредственно перед чтением.
        device
            .draw_masked(&target, &[sprite], &[Some(&mask)])
            .expect("draw_masked не падает (второй кадр)");

        // SAFETY: RTV только что создан `draw_masked`/`WindowTarget::new`,
        // ресурс за ним жив, пока жива `target`.
        let resource: ID3D11Resource = unsafe {
            target
                .rtv()
                .expect("цель ненулевого размера имеет RTV")
                .GetResource()
        }
        .expect("RTV даёт исходный ресурс");
        let pixels = read_bgra_texture(&device.device, &device.context, &resource, 64, 64);
        let at = |x: u32, y: u32| {
            let i = ((y * 64 + x) * 4) as usize;
            (pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3])
        };

        // Замаскированная половина (x=8 < 32): discard сработал — пиксель
        // остался таким, каким был очищен (`draw_masked`'s clear = полностью
        // прозрачный чёрный), НЕ цветом спрайта. Discard — это не «стать
        // прозрачным», а «не записать вообще»: проверяем именно это, а не
        // произвольную альфу.
        assert_eq!(
            at(8, 32),
            (0, 0, 0, 0),
            "замаскированная половина должна остаться clear-цветом (discard сработал)"
        );
        // Незамаскированная половина (x=48 >= 32): виден жёлтый спрайт.
        // B8G8R8A8: байты в порядке B,G,R,A; premultiplied непрозрачный
        // жёлтый (R=G=255,B=0,A=255) остаётся как есть при полной альфе.
        assert_eq!(
            at(48, 32),
            (0, 255, 255, 255),
            "незамаскированная половина должна показывать спрайт"
        );

        unsafe { DestroyWindow(hwnd) }.unwrap();
    }
}
