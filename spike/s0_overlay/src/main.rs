//! resticker S0 spike — throwaway proof of the core architecture hypothesis
//! (ADR-003 / ADR-004, ROADMAP "Спайк S0"):
//!   window = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TRANSPARENT
//!   content = D3D11 + DirectComposition swapchain, premultiplied alpha
//!   per-window z-order faked by cutting the occluder's rect out in the shader
//! No WS_EX_LAYERED, no UpdateLayeredWindow. Minimal error handling on purpose.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, PCSTR, PWSTR, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_DRIVER_TYPE_HARDWARE, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
// EVENT_*, OBJID_WINDOW, CHILDID_SELF, WINEVENT_* and SetProcessDPIAware
// all live in WindowsAndMessaging in windows-rs 0.62 (glob below).
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

const PNG_W: u32 = 420;
const PNG_H: u32 = 300;

// ---------------------------------------------------------------- state ----

struct Gfx {
    _device: ID3D11Device, // kept alive
    ctx: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    rtv: ID3D11RenderTargetView,
    // kept alive: dropping these tears down the composition tree
    _dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    srv: ID3D11ShaderResourceView,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    screen: (u32, u32),
    png_pos: (f32, f32),
    png_size: (f32, f32),
    mask: Option<RECT>, // Notepad rect currently cut out of the render
}

thread_local! {
    static GFX: RefCell<Option<Gfx>> = const { RefCell::new(None) };
    static LAST_LOC_PRINT: RefCell<Instant> = RefCell::new(Instant::now());
}

// layout note: float4 first so HLSL's 16-byte-alignment packing matches
// (float4 after three float2s would be pushed from offset 24 to 32)
#[repr(C)]
struct Cbuf {
    mask: [f32; 4], // left top right bottom; disabled when right <= left
    screen: [f32; 2],
    png_pos: [f32; 2],
    png_size: [f32; 2],
    _pad: [f32; 2],
}

const HLSL: &str = r#"
cbuffer Cb : register(b0) {
    float4 maskRect; float2 screenSize; float2 pngPos; float2 pngSize; float2 pad;
};
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
static const float2 corners[6] = { float2(0,0), float2(1,0), float2(0,1),
                                   float2(1,0), float2(1,1), float2(0,1) };
VSOut mainVS(uint vid : SV_VertexID) {
    float2 c = corners[vid];
    float2 px = pngPos + c * pngSize;
    VSOut o;
    o.pos = float4(px.x / screenSize.x * 2.0 - 1.0,
                   1.0 - px.y / screenSize.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}
Texture2D tex0 : register(t0);
SamplerState samp0 : register(s0);
float4 mainPS(VSOut i) : SV_Target {
    float2 px = i.pos.xy; // SV_Position in PS = pixel coords on screen
    if (maskRect.z > maskRect.x && px.x >= maskRect.x && px.x < maskRect.z
        && px.y >= maskRect.y && px.y < maskRect.w)
        discard;
    return tex0.Sample(samp0, i.uv);
}
"#;

// ------------------------------------------------------------- PNG ----

/// Generate a transparent test PNG if none exists, then decode it with the
/// `image` crate and premultiply. Returns (bgra_ready_rgba_bytes, w, h).
fn load_or_make_png() -> (Vec<u8>, u32, u32) {
    let path = Path::new("test.png");
    if !path.exists() {
        let mut img = image::RgbaImage::new(PNG_W, PNG_H);
        for y in 0..PNG_H {
            for x in 0..PNG_W {
                let r = (x * 255 / PNG_W) as u8;
                let b = (y * 255 / PNG_H) as u8;
                img.put_pixel(x, y, image::Rgba([r, 30, b, 110])); // semi gradient
            }
        }
        // opaque orange disc: solid thing to watch hiding under Notepad
        let (cx, cy, rad) = (PNG_W as f32 * 0.5, PNG_H as f32 * 0.5, 90.0f32);
        for y in 0..PNG_H {
            for x in 0..PNG_W {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                if dx * dx + dy * dy < rad * rad {
                    img.put_pixel(x, y, image::Rgba([255, 128, 0, 255]));
                }
            }
        }
        img.save(path).expect("save test.png");
    }
    let img = image::open(path).expect("open test.png").to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
    }
    (data, w, h)
}

// ------------------------------------------------------- Notepad rect ----

/// Extended frame bounds of the first visible notepad.exe window, or None.
fn find_notepad_rect() -> Option<RECT> {
    struct Ctx {
        result: Option<RECT>,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        let mut keep_going = || unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return TRUE;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return TRUE;
            }
            if let Ok(hproc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 260];
                let mut size = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(
                    hproc,
                    PROCESS_NAME_WIN32,
                    PWSTR(buf.as_mut_ptr()),
                    &mut size,
                )
                .is_ok();
                let _ = CloseHandle(hproc);
                if ok {
                    let path = String::from_utf16_lossy(&buf[..size as usize]).to_lowercase();
                    if path.ends_with("notepad.exe") {
                        let mut rc = RECT::default();
                        let got = DwmGetWindowAttribute(
                            hwnd,
                            DWMWA_EXTENDED_FRAME_BOUNDS,
                            &mut rc as *mut _ as *mut c_void,
                            size_of::<RECT>() as u32,
                        )
                        .is_ok();
                        if got {
                            ctx.result = Some(rc);
                            return FALSE; // found it, stop enumerating
                        }
                    }
                }
            }
            TRUE
        };
        keep_going()
    }
    let mut ctx = Ctx { result: None };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.result
}

// ------------------------------------------------------------- render ----

fn render(g: &Gfx) {
    unsafe {
        // push per-frame constants
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let cb_res: ID3D11Resource = g.cb.cast().unwrap();
        g.ctx
            .Map(Some(&cb_res), 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
            .unwrap();
        let cbuf = Cbuf {
            mask: match g.mask {
                Some(r) => [r.left as f32, r.top as f32, r.right as f32, r.bottom as f32],
                None => [0.0, 0.0, 0.0, 0.0],
            },
            screen: [g.screen.0 as f32, g.screen.1 as f32],
            png_pos: [g.png_pos.0, g.png_pos.1],
            png_size: [g.png_size.0, g.png_size.1],
            _pad: [0.0; 2],
        };
        std::ptr::copy_nonoverlapping(&cbuf, mapped.pData as *mut Cbuf, 1);
        g.ctx.Unmap(Some(&cb_res), 0);

        let clear = [0.0f32, 0.0, 0.0, 0.0]; // fully transparent
        g.ctx.ClearRenderTargetView(&g.rtv, &clear);
        let vp = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: g.screen.0 as f32,
            Height: g.screen.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        g.ctx.RSSetViewports(Some(&[vp]));
        g.ctx
            .OMSetRenderTargets(Some(&[Some(g.rtv.clone())]), None);
        g.ctx.OMSetBlendState(&g.blend, None, 0xffffffff);
        g.ctx.IASetInputLayout(None);
        g.ctx
            .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        g.ctx.VSSetShader(&g.vs, None);
        g.ctx.VSSetConstantBuffers(0, Some(&[Some(g.cb.clone())]));
        g.ctx.PSSetShader(&g.ps, None);
        g.ctx.PSSetConstantBuffers(0, Some(&[Some(g.cb.clone())]));
        g.ctx.PSSetShaderResources(0, Some(&[Some(g.srv.clone())]));
        g.ctx.PSSetSamplers(0, Some(&[Some(g.sampler.clone())]));
        g.ctx.Draw(6, 0);
        g.swapchain.Present(1, DXGI_PRESENT(0)).ok().unwrap();
    }
}

/// Recompute the Notepad mask; re-render only if it actually changed.
fn refresh_mask() {
    let new_mask = find_notepad_rect();
    GFX.with(|g| {
        let mut g = g.borrow_mut();
        let g = g.as_mut().unwrap();
        if g.mask != new_mask {
            println!("mask -> {new_mask:?}");
            g.mask = new_mask;
            render(g);
        }
    });
}

// ------------------------------------------------------- WinEvent hook ----

unsafe extern "system" fn winevent_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.0.is_null() {
        return;
    }
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32)
    {
        return;
    }
    // print rects to console (step 6); LOCATIONCHANGE throttled to 10/s so
    // console I/O doesn't inflate the CPU measurement in step 8
    let mut should_print = true;
    if event == EVENT_OBJECT_LOCATIONCHANGE {
        should_print = LAST_LOC_PRINT.with(|t| {
            let mut t = t.borrow_mut();
            if t.elapsed() >= Duration::from_millis(100) {
                *t = Instant::now();
                true
            } else {
                false
            }
        });
    }
    if should_print {
        let mut rc = RECT::default();
        let _ = unsafe { GetWindowRect(hwnd, &mut rc) };
        let kind = if event == EVENT_SYSTEM_FOREGROUND {
            "FOREGROUND"
        } else {
            "LOCCHANGE"
        };
        println!(
            "[{kind}] hwnd={:p} rect=({},{})..({},{})",
            hwnd.0, rc.left, rc.top, rc.right, rc.bottom
        );
    }
    refresh_mask();
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ------------------------------------------------------------- CPU (8) ----

fn process_time_100ns() -> u64 {
    unsafe {
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        let to_u64 = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        to_u64(k) + to_u64(u)
    }
}

fn spawn_cpu_sampler() {
    thread::spawn(|| {
        let ncpu = unsafe {
            let mut si = SYSTEM_INFO::default();
            GetSystemInfo(&mut si);
            si.dwNumberOfProcessors.max(1)
        } as f64;
        let mut prev_wall = Instant::now();
        let mut prev_cpu = process_time_100ns();
        loop {
            thread::sleep(Duration::from_secs(1));
            let wall = prev_wall.elapsed().as_secs_f64();
            let cpu_now = process_time_100ns();
            let cpu_secs = (cpu_now - prev_cpu) as f64 / 10_000_000.0;
            println!("CPU: {:.2}% (of {} cores)", 100.0 * cpu_secs / wall / ncpu, ncpu);
            prev_wall = Instant::now();
            prev_cpu = cpu_now;
        }
    });
}

fn compile_shader(entry: PCSTR, target: PCSTR) -> ID3DBlob {
    unsafe {
        let mut blob: Option<ID3DBlob> = None;
        let mut err: Option<ID3DBlob> = None;
        D3DCompile(
            HLSL.as_ptr() as *const c_void,
            HLSL.len(),
            None,
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut blob,
            Some(&mut err),
        )
        .unwrap_or_else(|e| {
            let msg = err
                .as_ref()
                .map(|b| {
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        b.GetBufferPointer() as *const u8,
                        b.GetBufferSize(),
                    ))
                    .into_owned()
                })
                .unwrap_or_default();
            panic!("shader compile failed: {e}\n{msg}");
        });
        blob.unwrap()
    }
}

fn shader_bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

// ---------------------------------------------------------------- D3D11 ----

fn init_gfx(hwnd: HWND, pixels: &[u8], w: u32, h: u32) -> windows::core::Result<Gfx> {
    unsafe {
        // D3D11 device (BGRA support is required by composition swapchains)
        let mut device: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )?;
        let device = device.unwrap();
        let ctx = ctx.unwrap();

        // DirectComposition: device -> target(hwnd, topmost) -> visual -> swapchain
        let dxgi_dev: IDXGIDevice = device.cast()?;
        let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_dev)?;
        let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
        let visual = dcomp.CreateVisual()?;
        target.SetRoot(Some(&visual))?;

        let screen = (
            GetSystemMetrics(SM_CXSCREEN) as u32,
            GetSystemMetrics(SM_CYSCREEN) as u32,
        );
        let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: screen.0,
            Height: screen.1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: FALSE,
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
        let swapchain = factory.CreateSwapChainForComposition(&device, &desc, None)?;
        visual.SetContent(&swapchain)?;
        dcomp.Commit()?;

        let back: ID3D11Texture2D = swapchain.GetBuffer(0)?;
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
        let rtv = rtv.unwrap();

        // shaders
        let vs_blob = compile_shader(
            PCSTR::from_raw(c"mainVS".as_ptr().cast()),
            PCSTR::from_raw(c"vs_5_0".as_ptr().cast()),
        );
        let ps_blob = compile_shader(
            PCSTR::from_raw(c"mainPS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        );
        let mut vs: Option<ID3D11VertexShader> = None;
        device.CreateVertexShader(shader_bytes(&vs_blob), None, Some(&mut vs))?;
        let vs = vs.unwrap();
        let mut ps: Option<ID3D11PixelShader> = None;
        device.CreatePixelShader(shader_bytes(&ps_blob), None, Some(&mut ps))?;
        let ps = ps.unwrap();

        // constant buffer
        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: size_of::<Cbuf>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut cb: Option<ID3D11Buffer> = None;
        device.CreateBuffer(&cb_desc, None, Some(&mut cb))?;
        let cb = cb.unwrap();

        // PNG texture (premultiplied, matches DXGI_ALPHA_MODE_PREMULTIPLIED)
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM, // texture is RGBA bytes; swapchain stays BGRA
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr() as *const c_void,
            SysMemPitch: w * 4,
            SysMemSlicePitch: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&tex_desc, Some(&init), Some(&mut tex))?;
        let tex = tex.unwrap();
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
        let srv = srv.unwrap();

        let mut sampler: Option<ID3D11SamplerState> = None;
        device.CreateSamplerState(
            &D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            },
            Some(&mut sampler),
        )?;
        let sampler = sampler.unwrap();

        let mut rt_blend = D3D11_RENDER_TARGET_BLEND_DESC::default();
        rt_blend.BlendEnable = TRUE;
        rt_blend.SrcBlend = D3D11_BLEND_ONE; // premultiplied
        rt_blend.DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
        rt_blend.BlendOp = D3D11_BLEND_OP_ADD;
        rt_blend.SrcBlendAlpha = D3D11_BLEND_ONE;
        rt_blend.DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
        rt_blend.BlendOpAlpha = D3D11_BLEND_OP_ADD;
        rt_blend.RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
        let mut blend_desc = D3D11_BLEND_DESC::default();
        blend_desc.RenderTarget[0] = rt_blend;
        let mut blend: Option<ID3D11BlendState> = None;
        device.CreateBlendState(&blend_desc, Some(&mut blend))?;
        let blend = blend.unwrap();

        Ok(Gfx {
            _device: device,
            ctx,
            swapchain,
            rtv,
            _dcomp_device: dcomp,
            _dcomp_target: target,
            _dcomp_visual: visual,
            vs,
            ps,
            cb,
            srv,
            sampler,
            blend,
            screen,
            png_pos: (
                (screen.0 as f32 - w as f32) / 2.0,
                (screen.1 as f32 - h as f32) / 2.0,
            ),
            png_size: (w as f32, h as f32),
            mask: None,
        })
    }
}

// ----------------------------------------------------------------- main ----

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDPIAware(); // physical pixels everywhere

        let (pixels, w, h) = load_or_make_png();
        println!("s0_overlay: test.png {w}x{h}");

        // window class + window: exactly the three ex-styles from the spike spec
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("s0_overlay");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            panic!("RegisterClassExW failed");
        }
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("resticker s0 spike"),
            WS_POPUP,
            0,
            0,
            screen_w,
            screen_h,
            None,
            None,
            Some(hinst),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);

        let gfx = init_gfx(hwnd, &pixels, w, h)?;
        println!(
            "s0_overlay: window {:p} {}x{}, D3D11+DComp up (premultiplied alpha)",
            hwnd.0, gfx.screen.0, gfx.screen.1
        );
        GFX.with(|g| *g.borrow_mut() = Some(gfx));

        // initial mask + first frame
        GFX.with(|g| {
            let mut g = g.borrow_mut();
            let g = g.as_mut().unwrap();
            g.mask = find_notepad_rect();
            render(g);
        });
        println!("s0_overlay: first frame presented");

        // hooks: FOREGROUND + LOCATIONCHANGE, out-of-context, skip own process
        let hook_fg = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(winevent_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let hook_loc = SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(winevent_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        if hook_fg.0.is_null() || hook_loc.0.is_null() {
            panic!("SetWinEventHook failed");
        }
        println!("s0_overlay: hooks armed; open Notepad over the image");

        spawn_cpu_sampler();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWinEvent(hook_fg);
        let _ = UnhookWinEvent(hook_loc);
    }
    Ok(())
}

