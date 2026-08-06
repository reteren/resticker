//! HLSL-шейдер спрайта и его компиляция в рантайме через D3DCompile
//! (d3dcompiler_47.dll входит в Windows 10/11, отдельных зависимостей нет).

use std::ffi::c_void;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::core::PCSTR;

use crate::RenderError;

/// Вершинный и пиксельный шейдеры спрайта.
///
/// Раскладка константного буфера: три float4 + два float2 + float4
/// (упаковка HLSL по 16 байт, см. урок спайка S0 — float4 после float2
/// съезжает на границу; пара float2 укладывается ровно в четвёртый
/// 16-байтный слот; M5c: пятый float4 `video` — 80 байт, кратно 16).
///   tr        = (cx, cy, w, h)      центр и размер в физических пикселях
///   misc      = (cos φ, sin φ, opacity, flip_h)
///   misc2     = (flip_v, screen_w, screen_h, pad)
///   uv_offset = (ux, uy)            верхний левый угол UV-подпрямоугольника
///   uv_scale  = (sx, sy)            размер подпрямоугольника в долях текстуры
///   video     = (array_index, disp_w/tex_w, disp_h/tex_h, pad)  — M5c, только
///               `mainVideoNv12PS`
pub(crate) const SPRITE_HLSL: &str = r#"
cbuffer Cb : register(b0) {
    float4 tr;
    float4 misc;
    float4 misc2;
    float2 uv_offset;
    float2 uv_scale;
    float4 video;
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
/// Плоскости видеокадра (M5b/M5e): Y — полное разрешение, U/V — половина
/// (4:2:0) или полное (4:4:4) разрешение, A — опциональная альфа-плоскость
/// полного разрешения (прозрачное видео, M5e; для непрозрачного в t5
/// биндится белая 1×1-текстура — a = 1, поведение M5b). Отдельные регистры
/// от `tex0`/`maskTex` — `mainVideoPS` семплирует только их; `draw`/
/// `draw_masked` биндят их, когда спрайт несёт `video: Some`
/// (см. `Sprite::with_video`).
Texture2D yTex : register(t2);
Texture2D uTex : register(t3);
Texture2D vTex : register(t4);
Texture2D aTex : register(t5);
SamplerState ySamp : register(s2);
SamplerState uSamp : register(s3);
SamplerState vSamp : register(s4);
SamplerState aSamp : register(s5);
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
/// Пиксельный шейдер видеоспрайта (M5b/M5e): семплирует плоскости кадра
/// (t2/t3/t4 — Y/U/V, t5 — альфа), конвертирует YUV→RGB в шейдере (не на
/// CPU — sws_scale из скоупа исключён). Альфа (M5e): RGB умножается на
/// альфа-плоскость — premultiplied-выход с alpha = a × opacity, тот же
/// контракт, что у `mainPS` (проект везде работает с premultiplied alpha,
/// ARCHITECTURE.md). Для непрозрачного видео (`VideoTextures::alpha ==
/// None`) в t5 биндится белая 1×1-текстура — a = 1, поведение M5b без
/// изменений. Маска перекрытия (t1) применяется тем же discard-паттерном,
/// что в `mainPS`, поэтому `draw_masked` работает и для видеоспрайтов.
float4 mainVideoPS(VSOut i) : SV_Target {
    float2 maskUv = i.pos.xy / misc2.yz;
    if (maskTex.Sample(maskSamp, maskUv).r > 0.5) discard;
    float2 finalUV = uv_offset + i.uv * uv_scale;
    // R8_UNORM-сэмпл даёт 0..1; константы yuv2rgb709 (16/128/219/224) — в
    // единицах 8-битного диапазона 0..255, домножаем до вызова.
    float y = yTex.Sample(ySamp, finalUV).r * 255.0;
    float u = uTex.Sample(uSamp, finalUV).r * 255.0;
    float v = vTex.Sample(vSamp, finalUV).r * 255.0;
    float3 rgb = yuv2rgb709(y, u, v);
    // Premultiplied: rgb уже масштабирован на альфу кадра.
    float a = aTex.Sample(aSamp, finalUV).r;
    return float4(rgb * a * i.opacity, a * i.opacity);
}
/// Плоскостные виды NV12-массив-текстуры декодера d3d11va (M5c,
/// аппаратный путь): Y как R8 (t5) и UV как R8G8 (t6), элемент кадра —
/// `video.x` (индекс из константного буфера). Сэмплятся напрямую —
/// zero-copy, readback на CPU не происходит. Плоскостные виды — штатный
/// механизм D3D11 для планарных форматов (на стенде M5c драйвер NVIDIA
/// принимает только их; единый NV12-вид он отвергает E_INVALIDARG).
Texture2DArray nv12YTex : register(t5);
Texture2DArray nv12UVTex : register(t6);
SamplerState nv12Samp : register(s6);
/// Пиксельный шейдер аппаратного видеоспрайта (M5c): NV12-текстура декодера
/// на ОБЩЕМ с рендером D3D11-девайсе. Сэмпл плоскости Y даёт яркость в
/// канале R, плоскости UV — U в R и V в G (масштаб 0..1 — домножаем на 255,
/// как плоскости R8 программного пути). Выровненная текстура шире/выше
/// видимой области — UV масштабируется `video.yz` (display/tex). Маска
/// (t1) — тот же discard-паттерн.
float4 mainVideoNv12PS(VSOut i) : SV_Target {
    float2 maskUv = i.pos.xy / misc2.yz;
    if (maskTex.Sample(maskSamp, maskUv).r > 0.5) discard;
    float2 finalUV = (uv_offset + i.uv * uv_scale) * video.yz;
    float y = nv12YTex.Sample(nv12Samp, float3(finalUV, video.x)).r * 255.0;
    float2 uv = nv12UVTex.Sample(nv12Samp, float3(finalUV, video.x)).rg * 255.0;
    // Аппаратные декодеры альфа-канал не отдают — выход непрозрачный.
    return float4(yuv2rgb709(y, uv.x, uv.y) * i.opacity, i.opacity);
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

    /// Аппаратный видеоспрайт (M5c) — отдельная точка входа PS, тот же VS/CB.
    #[test]
    fn video_nv12_shader_compiles() {
        let ps = compile(
            PCSTR::from_raw(c"mainVideoNv12PS".as_ptr().cast()),
            PCSTR::from_raw(c"ps_5_0".as_ptr().cast()),
        )
        .expect("PS аппаратного видео должен компилироваться");
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

    /// Инварианты видеоспрайта (M5b/M5e): четыре слота текстур/сэмплеров
    /// Y/U/V/A (t2..t5, s2..s5), отдельная точка входа PS, коэффициенты
    /// BT.709 limited range и premultiplied-выход (rgb × альфа × opacity,
    /// alpha = альфа × opacity — прозрачное видео M5e; при a = 1 —
    /// поведение M5b).
    #[test]
    fn video_hlsl_contract() {
        assert!(SPRITE_HLSL.contains("mainVideoPS"));
        for (tex, samp) in [
            ("yTex", "ySamp"),
            ("uTex", "uSamp"),
            ("vTex", "vSamp"),
            ("aTex", "aSamp"),
        ] {
            assert!(SPRITE_HLSL.contains(tex), "плоскость {tex} объявлена");
            assert!(SPRITE_HLSL.contains(samp), "сэмплер {samp} объявлен");
        }
        assert!(SPRITE_HLSL.contains("register(t2)"));
        assert!(SPRITE_HLSL.contains("register(t3)"));
        assert!(SPRITE_HLSL.contains("register(t4)"));
        // M5e: альфа-плоскость — отдельный слот t5/s5.
        assert!(SPRITE_HLSL.contains("register(t5)"));
        assert!(SPRITE_HLSL.contains("register(s5)"));
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
        // Premultiplied-выход с альфой кадра: rgb × a × opacity, alpha = a ×
        // opacity (M5e; при a = 1 — непрозрачное видео, поведение M5b).
        assert!(SPRITE_HLSL.contains("rgb * a * i.opacity, a * i.opacity"));
        // Альфа-сэмпл берётся до умножения на opacity (premultiply — до
        // opacity, иначе бы потерялся порядок умножений).
        assert!(SPRITE_HLSL.contains("float a = aTex.Sample(aSamp, finalUV).r;"));
    }
}
