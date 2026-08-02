//! HLSL-шейдер спрайта и его компиляция в рантайме через D3DCompile
//! (d3dcompiler_47.dll входит в Windows 10/11, отдельных зависимостей нет).

use std::ffi::c_void;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::core::PCSTR;

use crate::RenderError;

/// Вершинный и пиксельный шейдеры спрайта.
///
/// Раскладка константного буфера: три float4 (упаковка HLSL по 16 байт,
/// см. урок спайка S0 — float4 после float2 съезжает на границу).
///   tr    = (cx, cy, w, h)      центр и размер в физических пикселях
///   misc  = (cos φ, sin φ, opacity, flip_h)
///   misc2 = (flip_v, screen_w, screen_h, pad)
pub(crate) const SPRITE_HLSL: &str = r#"
cbuffer Cb : register(b0) {
    float4 tr;
    float4 misc;
    float4 misc2;
};
struct VSOut {
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
    float opacity : TEXCOORD1;
};
static const float2 corners[6] = { float2(0,0), float2(1,0), float2(0,1),
                                   float2(1,0), float2(1,1), float2(0,1) };
VSOut mainVS(uint vid : SV_VertexID) {
    float2 c = corners[vid];
    VSOut o;
    // flip через lerp: sign=+1 -> uv = c, sign=-1 -> uv = 1-c
    o.uv = float2(lerp(0.5, c.x - 0.5, misc.w) + 0.5,
                  lerp(0.5, c.y - 0.5, misc2.x) + 0.5);
    float2 local = (c - 0.5) * tr.zw;                       // размер
    float2 rot = float2(local.x * misc.x - local.y * misc.y,
                        local.x * misc.y + local.y * misc.x); // поворот
    float2 px = tr.xy + rot;                                // центр спрайта
    o.pos = float4(px.x / misc2.y * 2.0 - 1.0,
                   1.0 - px.y / misc2.z * 2.0, 0.0, 1.0);
    o.opacity = misc.z;
    return o;
}
Texture2D tex0 : register(t0);
SamplerState samp0 : register(s0);
float4 mainPS(VSOut i) : SV_Target {
    // Текстура уже premultiplied: умножение на opacity корректно и для rgb.
    return tex0.Sample(samp0, i.uv) * i.opacity;
}
"#;

/// Скомпилировать шейдер из [`SPRITE_HLSL`].
pub(crate) fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, RenderError> {
    let mut blob: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: исходник живёт в статике; out-параметры валидны; D3DCompile
    // не удерживает указатели после возврата.
    unsafe {
        D3DCompile(
            SPRITE_HLSL.as_ptr().cast::<c_void>(),
            SPRITE_HLSL.len(),
            None,
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut blob,
            Some(&mut errors),
        )
    }
    .map_err(|e| {
        let details = errors.as_ref().map_or_else(String::new, |b| {
            // SAFETY: blob валиден, буфер жив, пока жив `errors`.
            let bytes = unsafe {
                std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize())
            };
            String::from_utf8_lossy(bytes).into_owned()
        });
        RenderError::ShaderCompile(format!("{e}: {details}"))
    })?;
    let blob = blob.expect("D3DCompile без ошибки возвращает blob");
    Ok(blob)
}

/// Байткод скомпилированного шейдера.
pub(crate) fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: blob жив, пока жив заимствованный `blob`; размер от D3D.
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    }
}
