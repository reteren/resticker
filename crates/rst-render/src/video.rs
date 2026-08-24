//! Видео-текстуры и конвертация YUV→RGB (M5b, docs/M5B_VIDEO_DESIGN.md §3).
//!
//! Видеокадр — три R8-плоскости: Y (полное разрешение) и U/V (для 4:2:0 —
//! половина по каждой оси, для 4:4:4 — полное разрешение; размеры приносит
//! координатор из кадра декодера) плюс опциональная альфа-плоскость A
//! (полное разрешение) для прозрачного видео (M5e, ROADMAP.md). Плоскости
//! переиспользуются и обновляются через `Device::update_video_textures`
//! (UpdateSubresource) на каждый показанный кадр — пересоздание текстур на
//! каждый кадр дорого, в отличие от атласа анимации (M5a), который живёт на
//! фиксированном наборе кадров.
//!
//! Рисуется тем же `Sprite`-путём: координатор строит `Sprite` с
//! `with_video` поверх `VideoTextures`, `draw`/`draw_masked` семплируют
//! плоскости и конвертируют в RGB в пиксельном шейдере (`mainVideoPS`),
//! а не на CPU (`sws_scale` из M5b-скоупа исключён — ARCHITECTURE.md,
//! «Как не убить процессор»). Альфа (M5e): шейдер умножает RGB на альфу —
//! premultiplied-выход, тот же контракт, что у `mainPS` и всего остального
//! рендера (ARCHITECTURE.md). Для непрозрачного видео (`alpha == None`)
//! в t5 биндится белая 1×1-текстура — a = 1, поведение M5b без изменений.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D,
};

use crate::texture::Texture;

/// Три (или четыре) R8-плоскости одного видеокадра: Y (width×height), U/V
/// (4:2:0 — ceil(width/2)×ceil(height/2), 4:4:4 — полное разрешение) и
/// опциональная альфа-плоскость A (полное разрешение) для прозрачного
/// видео (M5e).
///
/// Владение: COM-указатели текстур — умные указатели windows-rs, `Release`
/// автоматически в `Drop`; `Clone` — COM `AddRef`, дешёвый (спрайт держит
/// клон плоскостей, а координатор — исходник для обновления кадром).
#[derive(Debug, Clone)]
pub struct VideoTextures {
    /// Плоскость яркости, полное разрешение.
    pub y: Texture,
    /// Плоскость синей цветоразности (4:2:0 — половина разрешения,
    /// 4:4:4 — полное).
    pub u: Texture,
    /// Плоскость красной цветоразности (как `u`).
    pub v: Texture,
    /// Альфа-плоскость, полное разрешение, если видео прозрачное
    /// (YUVA420P/YUVA444P10LE/qtrle); `None` — непрозрачное видео (YUV420P):
    /// в `mainVideoPS` сэмплируется белая 1×1-текстура (a = 1, поведение
    /// M5b без изменений).
    pub alpha: Option<Texture>,
    /// Аппаратный путь (M5c, zero-copy): `Some` — спрайт рисуется через
    /// `mainVideoNv12PS` из NV12-текстуры декодера d3d11va на ОБЩЕМ
    /// D3D11-девайсе; плоскости `y/u/v` при этом остаются чёрным
    /// плейсхолдером до первого кадра. `None` — программный путь (M5b/M5e)
    /// или аппаратные кадры ещё не приходили.
    pub(crate) nv12: Option<Nv12VideoTextures>,
}

impl VideoTextures {
    /// Несёт ли кадр альфа-плоскость: если да — `mainVideoPS` умножает RGB
    /// на альфу (premultiplied-выход), если нет — альфа трактуется как 1.
    pub fn has_alpha(&self) -> bool {
        self.alpha.is_some()
    }

    /// Активен ли аппаратный NV12-путь (M5c): `true` — спрайт семплирует
    /// текстуру декодера напрямую (zero-copy), `false` — программные
    /// плоскости.
    pub fn is_nv12(&self) -> bool {
        self.nv12.is_some()
    }
}

/// Аппаратный NV12-путь видеоспрайта (M5c, ROADMAP.md): ДВА плоскостных
/// SRV на массив-текстуре декодера d3d11va — Y (R8) и UV (R8G8); элемент
/// кадра задаётся общим `Arc<AtomicU32>` (индекс меняется каждый показанный
/// кадр, спрайт-клоны читают свежее значение).
///
/// Почему плоскостные виды, а не один NV12-вид: на реальном железе
/// `CreateShaderResourceView` с `DXGI_FORMAT_NV12` возвращает E_INVALIDARG —
/// принимаются только виды отдельных плоскостей (R8/R8G8). Плоскостные
/// виды — штатный механизм D3D11 для планарных форматов (Y-плоскость как
/// R8, UV как R8G8).
///
/// Почему вид на ОДИН слой, а не на весь массив: пул d3d11va создаётся с
/// `D3D11_BIND_DECODER`, и такую текстуру драйвер разрешает видеть только
/// послойно — вид с `ArraySize` больше единицы отвергается тем же
/// E_INVALIDARG. Замерено пробником на этой машине (пул 2560×1440, 17
/// поверхностей, `BindFlags 0x208`): вид на весь массив — ошибка, вид на
/// один слой — успех. Первая версия делала ровно наоборот и давала
/// зелёный кадр: SRV не создавался, спрайт оставался с нулевыми
/// плоскостями, а нули в BT.709 — это зелёный (репорт 2026-08-22).
///
/// Отсюда кэш: слоёв в пуле полтора десятка, кадр приходит то с одним, то
/// с другим, и создавать вид заново на каждый кадр — лишний вызов
/// драйвера 60 раз в секунду. Виды создаются лениво и переиспользуются;
/// шейдер всегда семплирует нулевой элемент такого вида.
///
/// Владение: SRV и COM-ссылка на текстуру декодера — умные указатели
/// windows-rs; `Clone` — AddRef/Arc, дёшево.
#[derive(Debug, Clone)]
pub(crate) struct Nv12VideoTextures {
    /// Устройство рендера — на нём создаются виды по мере появления слоёв.
    device: ID3D11Device,
    /// Текстура декодера (массив NV12-поверхностей пула).
    texture: ID3D11Texture2D,
    /// Виды по слою массива: `слой -> (Y, UV)`. Общий для клонов спрайта.
    slices: Arc<Mutex<HashMap<u32, (ID3D11ShaderResourceView, ID3D11ShaderResourceView)>>>,
    /// Виды текущего показываемого слоя (общие для клонов).
    current: Arc<Mutex<(ID3D11ShaderResourceView, ID3D11ShaderResourceView)>>,
    /// Слой текущего кадра — для диагностики; шейдеру он не нужен, вид и
    /// так однослойный.
    index: Arc<AtomicU32>,
    /// Число элементов массива (валидация индекса).
    array_size: u32,
    /// Размеры текстуры (выровненные до 16/32/128 px декодером).
    tex_width: u32,
    tex_height: u32,
    /// Видимая область кадра (coded, после кропа декодером).
    pub width: u32,
    pub height: u32,
}

impl Nv12VideoTextures {
    /// Создать NV12-путь из текстуры декодера (M5c): плоскостные SRV
    /// (R8 — Y, R8G8 — UV) на ОДИН слой массив-текстуры d3d11va. Текстура
    /// обязана быть NV12 с `D3D11_BIND_SHADER_RESOURCE` (так создаёт пул
    /// `rst-video::hwaccel`). Ошибка — вызывающий код пропустит кадр,
    /// программный путь не задет.
    pub(crate) fn from_decoder_texture(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
        display_width: u32,
        display_height: u32,
    ) -> Result<Self, crate::RenderError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;

        // SAFETY: GetDesc на живой текстуре (out-параметр).
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_NV12 {
            return Err(crate::RenderError::InvalidTextureData(format!(
                "NV12-путь получил текстуру формата {desc:?} (ожидался NV12)"
            )));
        }
        if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 == 0 {
            return Err(crate::RenderError::InvalidTextureData(
                "текстура декодера не имеет D3D11_BIND_SHADER_RESOURCE".into(),
            ));
        }
        if desc.ArraySize == 0 || desc.Width == 0 || desc.Height == 0 {
            return Err(crate::RenderError::InvalidTextureData(
                "текстура декодера с нулевыми размерами".into(),
            ));
        }
        let pair = Self::make_slice_views(device, texture, 0)?;
        Ok(Self {
            device: device.clone(),
            texture: texture.clone(),
            slices: Arc::new(Mutex::new(HashMap::from([(0u32, pair.clone())]))),
            current: Arc::new(Mutex::new(pair)),
            index: Arc::new(AtomicU32::new(0)),
            array_size: desc.ArraySize,
            tex_width: desc.Width,
            tex_height: desc.Height,
            width: display_width,
            height: display_height,
        })
    }

    /// Индекс элемента, который семплирует шейдер: всегда 0 — вид
    /// однослойный, слой выбран при его создании ([`Self::set_index`]).
    pub(crate) fn index(&self) -> u32 {
        0
    }

    /// Показывать слой `index`: берём готовые виды из кэша или создаём их
    /// один раз на слой.
    pub(crate) fn set_index(&self, index: u32) -> Result<(), crate::RenderError> {
        let index = index.min(self.array_size.saturating_sub(1));
        if self.index.load(Ordering::Relaxed) == index {
            // Тот же слой — виды уже стоят.
            return Ok(());
        }
        let pair = {
            let mut slices = self.slices.lock().expect("мьютекс видов NV12 не отравлен");
            match slices.get(&index) {
                Some(pair) => pair.clone(),
                None => {
                    let pair = Self::make_slice_views(&self.device, &self.texture, index)?;
                    slices.insert(index, pair.clone());
                    pair
                }
            }
        };
        *self
            .current
            .lock()
            .expect("мьютекс текущего вида NV12 не отравлен") = pair;
        self.index.store(index, Ordering::Relaxed);
        Ok(())
    }

    /// Пара плоскостных видов (Y: R8, UV: R8G8) на ОДИН слой массива.
    fn make_slice_views(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
        slice: u32,
    ) -> Result<(ID3D11ShaderResourceView, ID3D11ShaderResourceView), crate::RenderError> {
        use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2DARRAY;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
            D3D11_TEX2D_ARRAY_SRV,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
        };

        let make = |format| -> Result<ID3D11ShaderResourceView, crate::RenderError> {
            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: format,
                ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                        FirstArraySlice: slice,
                        // Ровно один слой: пул декодера с BIND_DECODER не
                        // позволяет вид на несколько (см. док структуры).
                        ArraySize: 1,
                    },
                },
            };
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            // SAFETY: описание валидно, текстура принадлежит устройству,
            // out-параметр жив до конца вызова.
            unsafe { device.CreateShaderResourceView(texture, Some(&srv_desc), Some(&mut srv)) }
                .map_err(crate::RenderError::Windows)?;
            Ok(srv.expect("CreateShaderResourceView без ошибки возвращает объект"))
        };
        Ok((make(DXGI_FORMAT_R8_UNORM)?, make(DXGI_FORMAT_R8G8_UNORM)?))
    }

    /// Масштаб UV видимой области: `display/tex` по каждой оси — левый
    /// верхний угол выровненной текстуры несёт видимый кадр.
    pub(crate) fn uv_scale(&self) -> [f32; 2] {
        [
            self.width as f32 / self.tex_width.max(1) as f32,
            self.height as f32 / self.tex_height.max(1) as f32,
        ]
    }

    /// SRV плоскости Y для биндинга в пиксельный шейдер (t5, R8).
    pub(crate) fn srv_y(&self) -> ID3D11ShaderResourceView {
        self.current
            .lock()
            .expect("мьютекс текущего вида NV12 не отравлен")
            .0
            .clone()
    }

    /// SRV плоскости UV для биндинга в пиксельный шейдер (t6, R8G8).
    pub(crate) fn srv_uv(&self) -> ID3D11ShaderResourceView {
        self.current
            .lock()
            .expect("мьютекс текущего вида NV12 не отравлен")
            .1
            .clone()
    }

    /// Та же ли это текстура, что мы оборачиваем (сравнение COM-указателей).
    pub(crate) fn holds_texture(&self, texture: &ID3D11Texture2D) -> bool {
        use windows::core::Interface;
        self.texture.as_raw() == texture.as_raw()
    }
}

/// Коэффициенты матрицы BT.709 limited range (Rec. ITU-R BT.709-6,
/// диапазон Y 16..235, Cb/Cr 16..240) — стандарт для подавляющего
/// большинства современных H.264/VP9-исходников.
///
/// Нормализация отличается от наивной `/255`: яркость делится на 219,
/// цветоразности — на 224, именно на этих знаменателях константы
/// 1.5748/0.1873/0.4681/1.8556 дают точный roundtrip (RGB 255 → YUV →
/// RGB 255, проверено юнит-тестами ниже). В шейдере (`mainVideoPS`) —
/// та же формула в f32 с `saturate`; здесь — f64 с округлением к
/// ближайшему, CPU-зеркало для юнит-тестов и GPU-теста readback.
#[cfg(test)]
pub(crate) fn yuv_to_rgb_bt709_limited(y: u8, u: u8, v: u8) -> [u8; 3] {
    let yp = (f64::from(y) - 16.0) / 219.0;
    let up = (f64::from(u) - 128.0) / 224.0;
    let vp = (f64::from(v) - 128.0) / 224.0;
    let r = yp + 1.5748 * vp;
    let g = yp - 0.1873 * up - 0.4681 * vp;
    let b = yp + 1.8556 * up;
    [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// RGB (0..=255) → BT.709 limited-range Y/U/V (8 бит) — прямая формула для
/// построения синтетических кадров в юнит- и GPU-тестах.
#[cfg(test)]
pub(crate) fn rgb_to_yuv_bt709_limited(rgb: [u8; 3]) -> (u8, u8, u8) {
    let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
    let y = 16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b);
    let u = 128.0 + 224.0 * (-0.1146 * r - 0.3854 * g + 0.5 * b);
    let v = 128.0 + 224.0 * (0.5 * r - 0.4542 * g - 0.0458 * b);
    (y.round() as u8, u.round() as u8, v.round() as u8)
}

/// Premultiplied RGBA из Y/U/V/A (BT.709 limited, 8 бит) — CPU-зеркало
/// `mainVideoPS` с альфой (M5e): rgb = yuv2rgb709(...) × (a/255), alpha = a.
/// Тот же контракт, что у всего рендера: premultiplied везде
/// (ARCHITECTURE.md); при a = 255 результат совпадает с
/// [`yuv_to_rgb_bt709_limited`] + полная альфа.
#[cfg(test)]
pub(crate) fn yuv_to_rgba_premultiplied_bt709(y: u8, u: u8, v: u8, a: u8) -> [u8; 4] {
    let [r, g, b] = yuv_to_rgb_bt709_limited(y, u, v);
    let f = f64::from(a) / 255.0;
    let rgb = [r, g, b].map(|c| (f64::from(c) * f).round() as u8);
    [rgb[0], rgb[1], rgb[2], a]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roundtrip RGB → YUV → RGB держит ±3 на канал (потери 8-битного
    /// квантования YUV-плоскостей, не ошибка конверсии).
    fn assert_roundtrip(rgb: [u8; 3], tolerance: u8) {
        let (y, u, v) = rgb_to_yuv_bt709_limited(rgb);
        let out = yuv_to_rgb_bt709_limited(y, u, v);
        for (got, want) in out.iter().zip(rgb) {
            assert!(
                (*got as i16 - want as i16).abs() <= i16::from(tolerance),
                "roundtrip {rgb:?} → YUV ({y},{u},{v}) → {out:?}"
            );
        }
    }

    #[test]
    fn roundtrip_pure_red() {
        assert_roundtrip([255, 0, 0], 3);
    }

    #[test]
    fn roundtrip_pure_green() {
        assert_roundtrip([0, 255, 0], 3);
    }

    #[test]
    fn roundtrip_pure_blue() {
        assert_roundtrip([0, 0, 255], 3);
    }

    #[test]
    fn roundtrip_mid_gray() {
        // Серый — U=V=128, все три канала равны, roundtrip без потерь.
        let (y, u, v) = rgb_to_yuv_bt709_limited([128, 128, 128]);
        assert_eq!((u, v), (128, 128));
        assert_eq!(yuv_to_rgb_bt709_limited(y, u, v), [128, 128, 128]);
    }

    #[test]
    fn roundtrip_skin_tone() {
        assert_roundtrip([220, 180, 140], 3);
    }

    #[test]
    fn known_yuv_pair_for_red() {
        // Чистый красный в BT.709 limited: Y=62.6, Cb=102.3, Cr=240 —
        // проверяем сами плоскости, чтобы GPU-тест не опирался на
        // зеркальную функцию конверсии.
        let (y, u, v) = rgb_to_yuv_bt709_limited([255, 0, 0]);
        assert_eq!((y, u, v), (63, 102, 240));
        // Обратный проход ровно в диапазон полного красного: G и B — не
        // больше 1 (остаток квантования плоскости Y 63 вместо 62.6).
        let out = yuv_to_rgb_bt709_limited(63, 102, 240);
        assert_eq!(out[0], 255, "R — полный");
        assert!(out[1] <= 1 && out[2] <= 1, "G/B ≈ 0: {out:?}");
    }

    #[test]
    fn out_of_range_values_saturate_per_channel() {
        // Патологические значения (яркость выше 235 и ниже 16, цветоразности
        // вне 16..240) обрабатываются покомпонентным `saturate`: каналы,
        // ушедшие выше 1, режутся к 255, ниже 0 — к 0 (матрица смешивает
        // каналы, поэтому все 255 получаются только у чистых R/B-путей,
        // не у (255,255,255) — G у него остаётся серым).
        assert_eq!(yuv_to_rgb_bt709_limited(255, 255, 255)[0], 255);
        assert_eq!(yuv_to_rgb_bt709_limited(0, 0, 0)[0], 0);
    }

    #[test]
    fn gray_extremes_are_exact() {
        // U=V=128 (нейтральные цветоразности): яркость по шкале 16..235
        // даёт ровный серый, без цветового сдвига на краях диапазона.
        assert_eq!(yuv_to_rgb_bt709_limited(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb_bt709_limited(235, 128, 128), [255, 255, 255]);
    }

    // --- M5e: premultiplied-альфа (зеркало mainVideoPS с aTex) ---

    #[test]
    fn alpha_opaque_matches_no_alpha_path() {
        // a = 255: rgb идентичен непрозрачному пути (yuv_to_rgb_bt709_limited),
        // alpha полная — поведение M5b.
        for (y, u, v) in [(63u8, 102u8, 240u8), (128, 128, 128), (32, 240, 118)] {
            let want = yuv_to_rgb_bt709_limited(y, u, v);
            let out = yuv_to_rgba_premultiplied_bt709(y, u, v, 255);
            assert_eq!(out[..3], want, "YUV ({y},{u},{v})");
            assert_eq!(out[3], 255);
        }
    }

    #[test]
    fn alpha_zero_zeroes_rgb() {
        // a = 0: premultiplied-чёрный — пиксель невидим при любом цвете.
        let (y, u, v) = rgb_to_yuv_bt709_limited([255, 128, 0]);
        assert_eq!(yuv_to_rgba_premultiplied_bt709(y, u, v, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn alpha_scales_rgb_linearly() {
        // Красный с a = 128: r = 255 × 128/255 = 128; g = 1 × 128/255 → 1
        // (остаток 8-битного квантования YUV-плоскостей, не ошибка
        // конверсии — тот же эффект, что в `known_yuv_pair_for_red`).
        let (y, u, v) = rgb_to_yuv_bt709_limited([255, 0, 0]);
        assert_eq!(
            yuv_to_rgba_premultiplied_bt709(y, u, v, 128),
            [128, 1, 0, 128]
        );
        // Полубелый с a = 128: все каналы поровну.
        let white = yuv_to_rgba_premultiplied_bt709(235, 128, 128, 128);
        assert_eq!(white, [128, 128, 128, 128]);
    }

    #[test]
    fn alpha_rounds_to_nearest() {
        // Умножение с округлением к ближайшему (не усечение): проверяется
        // против явной формулы на каждом канале.
        let (y, u, v) = rgb_to_yuv_bt709_limited([100, 100, 100]);
        let opaque = yuv_to_rgb_bt709_limited(y, u, v);
        let out = yuv_to_rgba_premultiplied_bt709(y, u, v, 100);
        assert_eq!(out[3], 100);
        for (got, base) in out[..3].iter().zip(opaque) {
            let want = (f64::from(base) * 100.0 / 255.0).round() as u8;
            assert_eq!(*got, want, "канал {base} × 100/255");
        }
    }
}
