//! Цель рендера на одно окно/монитор (M3_PREP_NOTES.md, §4.2): DirectComposition-
//! цепочка на HWND, композиционный swapchain с RTV, размер в физических
//! пикселях и DPI-масштаб. Создаётся на общем [`crate::Device`]; ресайз окна
//! и `DEVICE_REMOVED` пересоздают только цель (swapchain/RTV) — текстуры
//! устройства при этом не трогаются.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;

use crate::RenderError;
use crate::device::Device;

/// Цель рендера на окно: DirectComposition visual со swapchain, RTV на
/// backbuffer, размер в физических пикселях и масштаб DIP → физические.
///
/// Владение: COM-объекты — умные указатели windows-rs, `Release` вызывается
/// автоматически в `Drop`; сырой хендл окна цели не принадлежит — окно (из
/// rst-win32) обязано пережить цель. Одна цель на монитор; при потере
/// D3D-устройства цель создаётся заново на новом [`Device`] (ARCHITECTURE.md,
/// раздел 11).
pub struct WindowTarget {
    // Держатели композиционного дерева: читаться не будут, но обязаны жить,
    // пока жив target — drop сносит визуальное дерево окна.
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
    scale: f32,
}

impl WindowTarget {
    /// Создать цель на окно `hwnd` размером `width`×`height` физических
    /// пикселей: DirectComposition-цепочка с premultiplied alpha и swapchain
    /// на общем устройстве `device`. Проверено спайком S0 (ADR-003).
    ///
    /// Окно должно иметь `WS_EX_NOREDIRECTIONBITMAP`: содержимое показывается
    /// только через DirectComposition, обычный Present на HWND невозможен.
    pub fn new(device: &Device, hwnd: HWND, width: u32, height: u32) -> Result<Self, RenderError> {
        // --- DirectComposition: target(hwnd, topmost) -> visual ---
        // SAFETY: `hwnd` принадлежит вызывающей стороне и жив дольше цели.
        let dcomp_target = unsafe { device.dcomp_device().CreateTargetForHwnd(hwnd, true) }
            .map_err(RenderError::Windows)?;
        // SAFETY: dcomp_device жив; visual — новый, владеем им мы.
        let dcomp_visual =
            unsafe { device.dcomp_device().CreateVisual() }.map_err(RenderError::Windows)?;
        // SAFETY: target и visual живы и принадлежат нам.
        unsafe { dcomp_target.SetRoot(Some(&dcomp_visual)) }.map_err(RenderError::Windows)?;

        // --- Композиционная цепочка с premultiplied alpha ---
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
        // SAFETY: `device` жив; desc валиден; цепочка для композиции, а не HWND.
        let swapchain = unsafe {
            device
                .factory()
                .CreateSwapChainForComposition(device.d3d_device(), &desc, None)
        }
        .map_err(RenderError::Windows)?;
        // SAFETY: visual и swapchain живы и принадлежат нам.
        unsafe { dcomp_visual.SetContent(&swapchain) }.map_err(RenderError::Windows)?;
        // SAFETY: dcomp_device жив; Commit фиксирует дерево композиции.
        unsafe { device.dcomp_device().Commit() }.map_err(RenderError::Windows)?;

        let rtv = (width > 0 && height > 0)
            .then(|| create_rtv(device.d3d_device(), &swapchain))
            .transpose()?;

        Ok(Self {
            _dcomp_target: dcomp_target,
            _dcomp_visual: dcomp_visual,
            swapchain,
            rtv,
            size: (width, height),
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

    /// Сообщить цели новый размер окна (физические пиксели). Нулевой размер
    /// (свёрнуто/скрыто) — не ошибка: кадры просто пропускаются. Пересоздаёт
    /// swapchain и RTV только у этой цели; текстуры устройства не трогаются.
    pub fn resize(&mut self, device: &Device, width: u32, height: u32) -> Result<(), RenderError> {
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
        self.rtv = Some(create_rtv(device.d3d_device(), &self.swapchain)?);
        Ok(())
    }

    /// RTV на backbuffer для [`crate::Device::draw`] (None — нулевой размер).
    pub(crate) fn rtv(&self) -> Option<ID3D11RenderTargetView> {
        self.rtv.clone()
    }

    /// Представить кадр. Потеря устройства возвращается отдельным вариантом
    /// ошибки: устройство и цель надо пересоздать (ARCHITECTURE.md,
    /// раздел 11).
    ///
    /// `sync` выбирает интервал ожидания — см. [`PresentSync`].
    pub(crate) fn present(&self, sync: PresentSync) -> Result<(), RenderError> {
        // SAFETY: swapchain жив и принадлежит self.
        let hr = unsafe { self.swapchain.Present(sync.interval(), DXGI_PRESENT(0)) };
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

/// Как ждать показа кадра в [`WindowTarget::present`].
///
/// Замер (воркер-исследователь, 2026-08-21): `Present(1)` блокирует ровно
/// один период кадра — 5.6 мс на 179 Гц, около 16.7 мс на 60 Гц, — и
/// поскольку геометрию мы сэмплируем ДО этого ожидания, содержимое кадра к
/// моменту показа успевает устареть ровно на кадр. Именно это и видно как
/// «обводка отстаёт от окна» при перетаскивании. `Present(0)` возвращается
/// за 0.046 мс: если такт задаёт не он, а ожидание композиции DWM ПЕРЕД
/// сборкой кадра ([`rst_win32::dwm::wait_for_composition`] у вызывающего),
/// то сэмпл геометрии оказывается вплотную к показу и лаг падает до
/// единиц миллисекунд.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresentSync {
    /// Ждать вертикальной синхронизации внутри `Present` — режим по
    /// умолчанию для обычных кадров (анимации, видео, статика): такт задаёт
    /// сам `Present`, лишних пробуждений нет.
    #[default]
    VSync,
    /// Не ждать в `Present` вовсе. Только для кадров, такт которым задаёт
    /// вызывающий (см. доккомент типа); иначе кадры будут отправляться
    /// быстрее, чем DWM их композирует, и часть просто отбросится.
    Immediate,
}

impl PresentSync {
    fn interval(self) -> u32 {
        match self {
            Self::VSync => 1,
            Self::Immediate => 0,
        }
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
