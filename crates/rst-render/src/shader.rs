//! HLSL-шейдер спрайта и его компиляция в рантайме через D3DCompile
//! (d3dcompiler_47.dll входит в Windows 10/11, отдельных зависимостей нет).

use std::ffi::c_void;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::core::PCSTR;

use crate::RenderError;

/// Вершинный и пиксельный шейдеры спрайта.
///
/// Раскладка константного буфера: три float4 + два float2 (упаковка HLSL
/// по 16 байт, см. урок спайка S0 — float4 после float2 съезжает на
/// границу; пара float2 укладывается ровно в четвёртый 16-байтный слот).
///   tr        = (cx, cy, w, h)      центр и размер в физических пикселях
///   misc      = (cos φ, sin φ, opacity, flip_h)
///   misc2     = (flip_v, screen_w, screen_h, pad)
///   uv_offset = (ux, uy)            верхний левый угол UV-подпрямоугольника
///   uv_scale  = (sx, sy)            размер подпрямоугольника в долях текстуры
pub(crate) const SPRITE_HLSL: &str = r#"
cbuffer Cb : register(b0) {
    float4 tr;
    float4 misc;
    float4 misc2;
    float2 uv_offset;
    float2 uv_scale;
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
Texture2D maskTex : register(t1);
SamplerState maskSamp : register(s1);
/// Три R8-плоскости видеокадра (M5b, docs/M5B_VIDEO_DESIGN.md §3):
/// Y — полное разрешение, U/V — половина по каждой оси (4:2:0).
/// Отдельные регистры от `tex0`/`maskTex` — `mainVideoPS` семплирует
/// только их; `draw`/`draw_masked` биндят их, когда спрайт несёт
/// `video: Some` (см. `Sprite::with_video`).
Texture2D yTex : register(t2);
Texture2D uTex : register(t3);
Texture2D vTex : register(t4);
SamplerState ySamp : register(s2);
SamplerState uSamp : register(s3);
SamplerState vSamp : register(s4);
float4 mainPS(VSOut i) : SV_Target {
    // Маска перекрытия (M4): сэмплится по экранной позиции пикселя, не по
    // uv стикера — вырез стабилен при движении/повороте стикера. misc2.yz
    // уже несёт размер экрана (тот же, что для NDC-конверсии в mainVS).
    // discard ДО умножения на opacity — иначе преждевременный выход ушёл бы
    // с ненулевой premultiplied-альфой (M4_PREP_NOTES §4.3). Быстрый путь
    // (нет окклюдеров/стикер Always) — маска-параметр биндит 1x1 чёрную
    // текстуру (r == 0), discard никогда не срабатывает, отдельный шейдер
    // для этого случая не нужен.
    float2 maskUv = i.pos.xy / misc2.yz;
    if (maskTex.Sample(maskSamp, maskUv).r > 0.5) discard;
    // Подпрямоугольник текстуры (M5a, атлас анимации): finalUV = uv_offset +
    // rawUV * uv_scale; при идентичных uv_offset/uv_scale — вся текстура,
    // поведение M1-M4. Применяется к обоим путям: обычная отрисовка и
    // masked (draw_masked) делят один mainPS.
    float2 finalUV = uv_offset + i.uv * uv_scale;
    // Текстура уже premultiplied: умножение на opacity корректно и для rgb.
    return tex0.Sample(samp0, finalUV) * i.opacity;
}
/// YUV→RGB матрицей BT.709 limited range (Rec. ITU-R BT.709-6):
/// Y в 16..235, Cb/Cr в 16..240; НА ВХОДЕ — сырые 8-битные значения
/// 0..255 (вызывающий код обязан домножить UNORM-сэмпл 0..1 на 255;
/// нормализация делением на 219/224 — именно на этих знаменателях
/// константы 1.5748/0.1873/0.4681/1.8556 дают точный roundtrip, проверено
/// юнит-тестами `video.rs` на CPU-зеркале той же формулы). Цветовые
/// метаданные контейнера не читаются — всегда BT.709 (docs/M5B_VIDEO_
/// DESIGN.md §3, известное упрощение: BT.601-контент может слегка «поплыть»
/// по цвету).
float3 yuv2rgb709(float y, float u, float v) {
    float yp = (y - 16.0) / 219.0;
    float up = (u - 128.0) / 224.0;
    float vp = (v - 128.0) / 224.0;
    float r = yp + 1.5748 * vp;
    float g = yp - 0.1873 * up - 0.4681 * vp;
    float b = yp + 1.8556 * up;
    return saturate(float3(r, g, b));
}
/// Пиксельный шейдер видеоспрайта (M5b): семплирует три R8-плоскости
/// (t2/t3/t4), конвертирует YUV→RGB в шейдере (не на CPU — sws_scale из
/// скоупа M5b исключён). Альфа видео всегда непрозрачна (docs/M5B_VIDEO_
/// DESIGN.md §0.3: альфа-потоки сознательно не обрабатываются): premultiplied
/// выход с alpha = opacity — тот же контракт, что у `mainPS`. Маска
/// перекрытия (t1) применяется тем же discard-паттерном, что в `mainPS`,
/// поэтому `draw_masked` работает и для видеоспрайтов без изменений.
float4 mainVideoPS(VSOut i) : SV_Target {
    float2 maskUv = i.pos.xy / misc2.yz;
    if (maskTex.Sample(maskSamp, maskUv).r > 0.5) discard;
    float2 finalUV = uv_offset + i.uv * uv_scale;
    // R8_UNORM-сэмпл даёт 0..1; константы yuv2rgb709 (16/128/219/224) — в
    // единицах 8-битного диапазона 0..255, домножаем до вызова.
    float y = yTex.Sample(ySamp, finalUV).r * 255.0;
    float u = uTex.Sample(uSamp, finalUV).r * 255.0;
    float v = vTex.Sample(vSamp, finalUV).r * 255.0;
    return float4(yuv2rgb709(y, u, v) * i.opacity, i.opacity);
}
/// Скруглённый прямоугольник в маску перекрытия (M4, ADR-004): один
/// оклюдер за вызов, `tr` несёт его центр/размер в физических px (тот же VS
/// и раскладка CB, что у спрайта — mainVS не меняется). SDF скруглённого
/// прямоугольника с 1-px антиалиасингом края; при узком/низком прямоугольнике
/// (half <= радиус) корректно деградирует к капсуле/линии.
float4 mainMaskPS(VSOut i) : SV_Target {
    float2 center = tr.xy;
    float2 halfSize = tr.zw * 0.5;
    float2 radius = float2(8.0, 8.0);
    float2 q = abs(i.pos.xy - center) - (halfSize - radius);
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius.x;
    float cov = 1.0 - smoothstep(0.0, 1.0, d);
    return float4(cov, 0.0, 0.0, cov);
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

#[cfg(test)]
mod tests {
    use super::{SPRITE_HLSL, blob_bytes, compile};
    use windows::core::PCSTR;

    /// Компиляция через D3DCompile требует d3dcompiler_47.dll — системный
    /// компонент Windows 10/11; тест грузит его, как и боевой рендерер.
    #[test]
    fn sprite_shader_compiles_for_vs_and_ps() {
        let vs = compile(
            PCSTR::from_raw(c"mainVS".as_ptr().cast()),
            PCSTR::from_raw(c"vs_5_0".as_ptr().cast()),
        )
        .expect("вершинный шейдер должен компилироваться");
        let ps = compile(
            PCSTR::from_raw(c"mainPS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        )
        .expect("пиксельный шейдер должен компилироваться");

        assert!(!blob_bytes(&vs).is_empty(), "vs_5_0 должен дать байткод");
        assert!(!blob_bytes(&ps).is_empty(), "ps_5_0 должен дать байткод");
    }

    /// Маска перекрытия (M4) — отдельная точка входа PS, тот же VS/CB.
    #[test]
    fn mask_shader_compiles() {
        let ps = compile(
            PCSTR::from_raw(c"mainMaskPS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        )
        .expect("PS маски должен компилироваться");
        assert!(!blob_bytes(&ps).is_empty(), "ps_5_0 должен дать байткод");
    }

    /// Видеоспрайт (M5b) — отдельная точка входа PS, тот же VS/CB.
    #[test]
    fn video_shader_compiles() {
        let ps = compile(
            PCSTR::from_raw(c"mainVideoPS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        )
        .expect("PS видео должен компилироваться");
        assert!(!blob_bytes(&ps).is_empty(), "ps_5_0 должен дать байткод");
    }

    /// Инварианты исходника HLSL, критичные для спрайтового рендера:
    /// обе точки входа, константный буфер, UV-remap и premultiplied-умножение
    /// на opacity.
    #[test]
    fn sprite_hlsl_contract() {
        assert!(SPRITE_HLSL.contains("mainVS"));
        assert!(SPRITE_HLSL.contains("mainPS"));
        assert!(SPRITE_HLSL.contains("cbuffer Cb"));
        // M5a: UV-подпрямоугольник (атлас анимации) считается до сэмплинга.
        assert!(SPRITE_HLSL.contains("float2 finalUV = uv_offset + i.uv * uv_scale;"));
        // Текстура уже premultiplied, поэтому rgb тоже умножается на opacity.
        assert!(SPRITE_HLSL.contains("tex0.Sample(samp0, finalUV) * i.opacity"));
    }

    /// Инварианты маски (M4, docs/M4_MASK_RENDER_DESIGN.md §4.3): второй
    /// слот текстуры/сэмплера, отдельная точка входа PS маски, и — самое
    /// важное — `discard` стоит РАНЬШЕ умножения на opacity в исходном
    /// тексте (иначе преждевременный выход ушёл бы с ненулевой
    /// premultiplied-альфой).
    #[test]
    fn mask_hlsl_contract() {
        assert!(SPRITE_HLSL.contains("register(t1)"));
        assert!(SPRITE_HLSL.contains("register(s1)"));
        assert!(SPRITE_HLSL.contains("mainMaskPS"));
        let discard_pos = SPRITE_HLSL
            .find("discard;")
            .expect("discard по маске должен присутствовать");
        let opacity_mul_pos = SPRITE_HLSL
            .find("tex0.Sample(samp0, finalUV) * i.opacity")
            .expect("умножение на opacity должно присутствовать");
        assert!(
            discard_pos < opacity_mul_pos,
            "discard обязан стоять до умножения на opacity (не после premultiply)"
        );
    }

    /// Инварианты видеоспрайта (M5b, docs/M5B_VIDEO_DESIGN.md §3): три
    /// слота текстур/сэмплеров Y/U/V (t2/t3/t4, s2/s3/s4), отдельная точка
    /// входа PS, коэффициенты BT.709 limited range и premultiplied-выход
    /// (rgb * opacity, alpha = opacity — видео всегда непрозрачно).
    #[test]
    fn video_hlsl_contract() {
        assert!(SPRITE_HLSL.contains("mainVideoPS"));
        for (tex, samp) in [("yTex", "ySamp"), ("uTex", "uSamp"), ("vTex", "vSamp")] {
            assert!(SPRITE_HLSL.contains(tex), "плоскость {tex} объявлена");
            assert!(SPRITE_HLSL.contains(samp), "сэмплер {samp} объявлен");
        }
        assert!(SPRITE_HLSL.contains("register(t2)"));
        assert!(SPRITE_HLSL.contains("register(t3)"));
        assert!(SPRITE_HLSL.contains("register(t4)"));
        // Коэффициенты BT.709 limited range — опечатка в них незаметна
        // глазами на цветном кадре, но ломает точность конверсии.
        assert!(SPRITE_HLSL.contains("1.5748"));
        assert!(SPRITE_HLSL.contains("0.1873"));
        assert!(SPRITE_HLSL.contains("0.4681"));
        assert!(SPRITE_HLSL.contains("1.8556"));
        // Нормализация limited range: /219 по яркости, /224 по цветоразности.
        assert!(SPRITE_HLSL.contains("/ 219.0"));
        assert!(SPRITE_HLSL.contains("/ 224.0"));
        // R8_UNORM-сэмпл даёт 0..1, а константы yuv2rgb709 (16/128/219/224)
        // — в единицах 8-битного диапазона: домножение на 255 обязательно,
        // без него весь кадр «съезжает» по яркости (найдено GPU-тестом).
        assert!(SPRITE_HLSL.contains(".r * 255.0"));
        // Premultiplied-выход с alpha = opacity (непрозрачное видео).
        assert!(SPRITE_HLSL.contains("yuv2rgb709(y, u, v) * i.opacity, i.opacity"));
    }
}
