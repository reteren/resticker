//! Аппаратное декодирование D3D11VA (M5c, ROADMAP.md): FFmpeg-контекст
//! устройства создаётся НА ТОМ ЖЕ `ID3D11Device`, что и весь рендер
//! (`rst-render::Device`) — иначе декодированные текстуры пришлось бы
//! копировать между устройствами (дорого), и zero-copy путь теряет смысл.
//!
//! FFmpeg не умеет принимать существующий `ID3D11Device` через
//! `av_hwdevice_ctx_create` (он сам создаёт устройство) — поэтому устройство
//! внедряется вручную: `av_hwdevice_ctx_alloc` + поле `device` в
//! `AVD3D11VADeviceContext` + `av_hwdevice_ctx_init` (документированный путь,
//! `libavutil/hwcontext_d3d11va.h`: «This is the only mandatory field»).
//!
//! **Гарантия**: этот модуль никогда не должен «сломать» воспроизведение.
//! Любая ошибка инициализации (нет железа/драйвера, кодек не поддерживается
//! hwaccel'ом, память) возвращается как `Err(reason)` — вызывающий код
//! обязан перейти на программный декод. ROADMAP: «корректный fallback ...
//! остаётся дефолтом при любом сомнении».
//!
//! # Почему frames-контекст создаётся в `get_format`-колбэке
//!
//! `avcodec_open2` можно вызвать с заранее выставленным `hw_frames_ctx`, но
//! `ff_get_format` (первый декодированный кадр) начинается с
//! `ff_hwaccel_uninit()`, который СНИМАЕТ ссылку `avctx->hw_frames_ctx`
//! (libavcodec/decode.c, найдено на железе: декодер молча пересоздавал
//! контекст с `BindFlags = DECODER`, и кадры теряли `SHADER_RESOURCE`).
//! Поэтому пул создаётся в самом колбэке `get_format` — это штатное место
//! для установки `hw_frames_ctx` (документация `AV_CODEC_HW_CONFIG_METHOD_
//! HW_FRAMES_CTX`: «The frames context must have been created ... inside the
//! get_format() callback»). Колбэк пересоздаёт пул при каждом перезапуске
//! декодера (смена SPS и т.п.) — `ff_hwaccel_uninit` снимает старый.
//!
//! Пул строится ВРУЧНУЮ (`av_hwframe_ctx_alloc` + поля + init), а не через
//! `avcodec_get_hw_frames_parameters`: эта функция читает `avctx->internal`,
//! который существует только ПОСЛЕ `avcodec_open2` (7.1; NULL до open2 —
//! access violation). Значения повторяют `ff_dxva2_common_frame_params`
//! (libavcodec/dxva2.c): выравнивание и число поверхностей по `codec_id`.
//! Тот же ручной паттерн использует VLC (modules/hw/d3d11va.c).
//!
//! Структуры `AVD3D11VADeviceContext`/`AVD3D11VAFramesContext` не попадают в
//! биндинги `ffmpeg-sys-next` (bindgen не находит `d3d11.h` в include-путах),
//! поэтому здесь они объявлены вручную, 1:1 с
//! `libavutil/hwcontext_d3d11va.h` (исходники FFmpeg 7.1.x на W:/ffmpeg_build
//! — источник истины для раскладки полей).

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr::null_mut;

use ffmpeg_sys_next::*;
use tracing::debug;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, ID3D11Device,
};
use windows::core::Interface;

/// Ручное зеркало `AVD3D11VADeviceContext` (hwcontext_d3d11va.h, 7 полей,
/// 56 байт на x64). Нам нужно только поле `device` — остальное FFmpeg
/// заполняет сам в `d3d11va_device_init` (immediate context, video device,
/// дефолтные lock/unlock на мьютексе).
#[repr(C)]
struct FfD3d11vaDeviceCtx {
    device: *mut std::ffi::c_void,
    device_context: *mut std::ffi::c_void,
    video_device: *mut std::ffi::c_void,
    video_context: *mut std::ffi::c_void,
    lock: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    lock_ctx: *mut std::ffi::c_void,
}

/// Ручное зеркало `AVD3D11VAFramesContext` (hwcontext_d3d11va.h, 4 поля,
/// 24 байта на x64: texture@0, BindFlags@8, MiscFlags@12, texture_infos@16).
/// Через `BindFlags` просим пул текстур декодера с флагом
/// `D3D11_BIND_SHADER_RESOURCE` — без него на текстурах нельзя создать SRV,
/// и zero-copy рендер невозможен (frame_params от hwaccel ставит только
/// `D3D11_BIND_DECODER`).
#[repr(C)]
struct FfD3d11vaFramesCtx {
    texture: *mut std::ffi::c_void,
    bind_flags: u32,
    misc_flags: u32,
    texture_infos: *mut std::ffi::c_void,
}

/// Компиляционные инварианты ручных зеркал: размеры обязаны совпадать с
/// C-структурами (иначе FFmpeg пишет в «чужие» смещения — молчаливое
/// повреждение памяти, пойманное именно так при первом прогоне на железе).
const _: () = assert!(std::mem::size_of::<FfD3d11vaDeviceCtx>() == 56);
const _: () = assert!(std::mem::size_of::<FfD3d11vaFramesCtx>() == 24);

/// RAII-обёртка `AVBufferRef*` для путей с ранними return: единственная
/// ссылка, освобождается `av_buffer_unref`. Для передачи владения наружу
/// есть [`BufferRef::leak`].
struct BufferRef(*mut AVBufferRef);

impl BufferRef {
    fn new(ptr: *mut AVBufferRef) -> Self {
        Self(ptr)
    }

    /// Отдать сырой указатель, отключив обёртку (владелец — вызывающий код).
    fn leak(self) -> *mut AVBufferRef {
        let ptr = self.0;
        std::mem::forget(self);
        ptr
    }
}

impl Drop for BufferRef {
    fn drop(&mut self) {
        // SAFETY: обёртка владеет единственной ссылкой; см. av_buffer_unref.
        unsafe { av_buffer_unref(&mut self.0) };
    }
}

/// Состояние аппаратного декодирования одного файла (живёт на
/// декодер-потоке вместе с остальными FFmpeg-контекстами).
///
/// Владение: `device_ctx` — `AVBufferRef`, освобождается `av_buffer_unref`
/// в `Drop` (FFmpeg сам делает `Release` нашему `ID3D11Device` — мы его
/// один раз AddRef'или при передаче). `frames_ctx` создаётся
/// `get_format`-колбэком при первом кадре и подхватывается
/// [`attach_frames_ctx`]; в `Drop` он НЕ освобождается — см. док `Drop`.
pub(crate) struct HwDecode {
    device_ctx: *mut AVBufferRef,
    frames_ctx: *mut AVBufferRef,
    /// Размеры текстур пула (выровненные до 16/32/128 px): текстура
    /// шире/выше видимой области, шейдер рендера компенсирует это
    /// отношением display/tex.
    pub tex_width: u32,
    pub tex_height: u32,
}

impl Drop for HwDecode {
    fn drop(&mut self) {
        // Документированное ограничение (найдено на стенде M5c, NVIDIA GTX
        // 1070 Ti + FFmpeg 7.1): `av_buffer_unref` frames-контекста после
        // ЦИКЛИЧЕСКОГО использования пула (все 17 поверхностей побывали в
        // работе) приводит к падению/зависанию в teardown — внутри
        // `buffer_pool_flush` → `free_texture` → `ID3D11Texture2D_Release`
        // (переход в освобождённую память, ip == addr). Без циклирования
        // пула teardown чистый (10/10 прогонов). Дефект во взаимодействии
        // FFmpeg и драйвера, из Rust не лечится.
        //
        // Обход: frames-контекст (и его пул ~10 МБ для 1080p) НЕ
        // освобождается при закрытии файла — живёт до конца процесса
        // (утечка, ограниченная числом закрытых видео; TODO: починить
        // вместе с апстримом). Устройство (ID3D11Device — общий с
        // рендером) освобождается штатно.
        let _ = self.frames_ctx; // намеренно не unref — см. выше
        self.frames_ctx = std::ptr::null_mut();
        // SAFETY: контекст устройства жив; unref потокобезопасен.
        unsafe { av_buffer_unref(&mut self.device_ctx) };
    }
}

/// Подхватить frames-контекст, созданный `get_format`-колбэком при первом
/// кадре: взять свою ссылку и запомнить размеры текстур пула. Вызывается
/// после первого декодированного кадра (probe). При неудаче — `Err`, но
/// кадры уже могли декодироваться: безопаснее оставить hw-путь, чем
/// переоткрывать файл (контекст жив и пригоден; readback при этом будет
/// падать с понятной ошибкой, zero-copy работает).
pub(crate) fn attach_frames_ctx(hw: &mut HwDecode, codec_ctx: *mut AVCodecContext) {
    // SAFETY: кодек-контекст жив; hw_frames_ctx выставлен колбэком.
    let fctx = unsafe { (*codec_ctx).hw_frames_ctx };
    if fctx.is_null() {
        return;
    }
    // SAFETY: ссылка валидна; берём свою (старую при этом снимаем — колбэк
    // мог пересоздать контекст после перезапуска декодера).
    let new_ref = unsafe { av_buffer_ref(fctx) };
    if new_ref.is_null() {
        return;
    }
    if !hw.frames_ctx.is_null() {
        // SAFETY: наша ссылка; см. av_buffer_unref.
        unsafe { av_buffer_unref(&mut hw.frames_ctx) };
    }
    hw.frames_ctx = new_ref;
    // SAFETY: frames-контекст инициализирован (колбэк); поля заполнены.
    let (w, h) = unsafe {
        let hwframes = &*(*fctx).data.cast::<AVHWFramesContext>();
        (hwframes.width as u32, hwframes.height as u32)
    };
    hw.tex_width = w;
    hw.tex_height = h;
    debug!("D3D11VA: frames-контекст подхвачен ({w}x{h})");
}

/// Поддержка D3D11VA в декодере `codec` (по `AVCodecHWConfig`).
fn codec_supports_d3d11va(codec: *const AVCodec) -> bool {
    // SAFETY: codec валиден (из av_find_best_stream/decoder_for); перебор
    // конфигураций завершается по NULL-указателю, индексы не выходят
    // за границы (контракт avcodec_get_hw_config).
    unsafe {
        let mut i: c_int = 0;
        loop {
            let cfg = avcodec_get_hw_config(codec, i);
            if cfg.is_null() {
                break;
            }
            let cfg = &*cfg;
            if cfg.device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA
                && (cfg.methods
                    & (AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int
                        | AV_CODEC_HW_CONFIG_METHOD_HW_FRAMES_CTX as c_int))
                    != 0
            {
                return true;
            }
            i += 1;
        }
    }
    false
}

/// Включить аппаратный декод на `codec_ctx` (кодек-контекст ещё НЕ открыт
/// `avcodec_open2`): внедряем наш `ID3D11Device` в hw-устройство FFmpeg,
/// вешаем `get_format`-колбэк (создаст пул NV12 с `SHADER_RESOURCE` при
/// первом кадре) и привязываем устройство к контексту.
///
/// При любой ошибке — `Err(причина)`: `codec_ctx` остаётся в исходном
/// состоянии (все созданные контексты освобождены), вызывающий код просто
/// открывает кодек без hw — программный путь, состояние файла не сломано.
pub(crate) fn enable(
    codec_ctx: *mut AVCodecContext,
    codec: *const AVCodec,
    device: &ID3D11Device,
) -> Result<HwDecode, String> {
    // Декодер-поток и поток рендера будут пользоваться ОДНИМ D3D11-девайсом
    // (immediate context не потокобезопасен). FFmpeg включает
    // multithread-protection, когда сам создаёт устройство; для нашего —
    // включаем явно, это штатный способ разделять девайс между декодом и
    // рендером (тот же приём, что у Chromium/MPV). Устройство rst-render
    // создаётся БЕЗ D3D11_CREATE_DEVICE_SINGLETHREADED — включение легально.
    // SAFETY: cast — QueryInterface ID3D10Multithread, поддерживается всеми
    // D3D11-устройствами.
    let mt: ID3D10Multithread = device
        .cast()
        .map_err(|e| format!("ID3D10Multithread: {e}"))?;
    // SAFETY: SetMultithreadProtected — см. выше.
    let _ = unsafe { mt.SetMultithreadProtected(true) };

    // --- 1. Кодек вообще поддерживает D3D11VA (h264/hevc/vp9/…)? ---
    if !codec_supports_d3d11va(codec) {
        return Err(format!(
            "кодек {} не имеет конфигурации D3D11VA",
            codec_name(codec)
        ));
    }

    // --- 2. HW-устройство на НАШЕМ ID3D11Device ---
    // SAFETY: alloc возвращает ссылку или NULL; BufferRef чистит все ранние
    // выходы, утечек нет ни на одном пути.
    let device_ctx_raw = unsafe { av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA) };
    if device_ctx_raw.is_null() {
        return Err("av_hwdevice_ctx_alloc(D3D11VA): не хватило памяти".into());
    }
    let device_ctx = BufferRef::new(device_ctx_raw);
    // SAFETY: hwctx — AVD3D11VADeviceContext (поле hwctx в начале структуры
    // AVHWDeviceContext); устройство ещё не инициализировано, это штатная
    // точка для подстановки device (документация hwcontext_d3d11va.h).
    unsafe {
        let hwdev = &mut *(*device_ctx_raw).data.cast::<AVHWDeviceContext>();
        let d3d = &mut *(hwdev.hwctx as *mut FfD3d11vaDeviceCtx);
        // AddRef: FFmpeg сам сделает Release при уничтожении контекста —
        // держатель (rst-render::Device) своей ссылки не отдаёт.
        let extra = device.clone();
        d3d.device = extra.into_raw().cast();
        let ret = av_hwdevice_ctx_init(device_ctx_raw);
        if ret < 0 {
            return Err(format!("av_hwdevice_ctx_init(D3D11VA): {}", ff_err(ret)));
        }
    }
    debug!("D3D11VA: hw-устройство создано на общем ID3D11Device");

    // --- 3. Привязать устройство и get_format-колбэк ---
    // SAFETY: ссылка валидна; codec_ctx жив до avcodec_free_context; колбэк —
    // статическая функция (см. её док).
    unsafe {
        (*codec_ctx).hw_device_ctx = av_buffer_ref(device_ctx_raw);
        (*codec_ctx).get_format = Some(pick_d3d11_format);
    }

    Ok(HwDecode {
        device_ctx: device_ctx.leak(),
        frames_ctx: null_mut(),
        tex_width: 0,
        tex_height: 0,
    })
}

/// `get_format`-колбэк аппаратного пути: создаёт (при необходимости) пул
/// NV12-кадров с `D3D11_BIND_SHADER_RESOURCE` на нашем устройстве и
/// возвращает `AV_PIX_FMT_D3D11`. Вызывается FFmpeg при выборе формата
/// (первый кадр, перезапуск после смены SPS) — `ff_hwaccel_uninit` к этому
/// моменту уже снял старый `hw_frames_ctx` (см. док модуля).
///
/// # SAFETY
///
/// Вызывается FFmpeg на декодер-потоке; контракт get_format — вернуть
/// формат из списка `pix_fmts` или `AV_PIX_FMT_NONE`. Все обращения идут
/// к живому `AVCodecContext`.
unsafe extern "C" fn pick_d3d11_format(
    codec_ctx: *mut AVCodecContext,
    pix_fmts: *const AVPixelFormat,
) -> AVPixelFormat {
    // SAFETY: список терминируется AV_PIX_FMT_NONE (контракт API).
    let offers_d3d11 = {
        let mut i = 0usize;
        let mut found = false;
        loop {
            let f = unsafe { *pix_fmts.add(i) };
            if f == AVPixelFormat::AV_PIX_FMT_NONE {
                break;
            }
            if f == AVPixelFormat::AV_PIX_FMT_D3D11 {
                found = true;
                break;
            }
            i += 1;
        }
        found
    };
    if !offers_d3d11 {
        return AVPixelFormat::AV_PIX_FMT_NONE;
    }
    // SAFETY: codec_ctx жив; hw_device_ctx выставлен в `enable`.
    let has_ctx = unsafe { !(*codec_ctx).hw_frames_ctx.is_null() };
    if !has_ctx {
        if let Err(reason) = create_frames_ctx(codec_ctx) {
            tracing::warn!("D3D11VA: get_format не смог создать пул: {reason}");
            return AVPixelFormat::AV_PIX_FMT_NONE;
        }
    }
    AVPixelFormat::AV_PIX_FMT_D3D11
}

/// Создать пул NV12-кадров на устройстве кодек-контекста и привязать к
/// `codec_ctx->hw_frames_ctx`. Значения пула повторяют
/// `ff_dxva2_common_frame_params` (libavcodec/dxva2.c); ручное построение —
/// см. док модуля.
fn create_frames_ctx(codec_ctx: *mut AVCodecContext) -> Result<(), String> {
    // SAFETY: codec_ctx жив; hw_device_ctx выставлен в enable.
    let device_ctx_raw = unsafe { (*codec_ctx).hw_device_ctx };
    if device_ctx_raw.is_null() {
        return Err("hw_device_ctx не выставлен".into());
    }
    // SAFETY: av_hwframe_ctx_alloc отдаёт ссылку или NULL.
    let frames_ref_raw = unsafe { av_hwframe_ctx_alloc(device_ctx_raw) };
    if frames_ref_raw.is_null() {
        return Err("av_hwframe_ctx_alloc: не хватило памяти".into());
    }
    let _frames_ref = BufferRef::new(frames_ref_raw);
    // Выравнивание и размер пула — копия ff_dxva2_common_frame_params.
    // SAFETY: codec_id — валидный enum из живого контекста.
    let (alignment, pool_size) = unsafe {
        match (*codec_ctx).codec_id {
            AVCodecID::AV_CODEC_ID_MPEG2VIDEO => (32, 3),
            AVCodecID::AV_CODEC_ID_HEVC | AVCodecID::AV_CODEC_ID_AV1 => (128, 17),
            AVCodecID::AV_CODEC_ID_H264 => (16, 17),
            AVCodecID::AV_CODEC_ID_VP9 => (16, 9),
            _ => (16, 3),
        }
    };
    let align = |v: i32| (v + alignment - 1) / alignment * alignment;
    // К моменту get_format размеры уже кодовые (coded) — из битстрима.
    let tex_width = unsafe { align((*codec_ctx).width) };
    let tex_height = unsafe { align((*codec_ctx).height) };
    if tex_width <= 0 || tex_height <= 0 {
        return Err(format!("невалидные размеры {tex_width}x{tex_height}"));
    }
    // SAFETY: frames_ref валиден; поля заполняются до av_hwframe_ctx_init —
    // контракт AVHWFramesContext (формат/размеры/пул) и
    // AVD3D11VAFramesContext (BindFlags: декодер + SRV для zero-copy).
    unsafe {
        let hwframes = &mut *(*frames_ref_raw).data.cast::<AVHWFramesContext>();
        hwframes.format = AVPixelFormat::AV_PIX_FMT_D3D11;
        hwframes.sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
        hwframes.width = tex_width;
        hwframes.height = tex_height;
        hwframes.initial_pool_size = pool_size;
        let d3d = &mut *(hwframes.hwctx as *mut FfD3d11vaFramesCtx);
        d3d.bind_flags = (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
    }
    let ret = unsafe { av_hwframe_ctx_init(frames_ref_raw) };
    if ret < 0 {
        return Err(format!("av_hwframe_ctx_init: {}", ff_err(ret)));
    }
    // SAFETY: ссылка валидна; codec_ctx жив до avcodec_free_context.
    unsafe {
        (*codec_ctx).hw_frames_ctx = av_buffer_ref(frames_ref_raw);
    }
    debug!("D3D11VA: пул {tex_width}x{tex_height}, {pool_size} поверхностей");
    Ok(())
}

fn codec_name(codec: *const AVCodec) -> String {
    // SAFETY: codec валиден; name — NUL-терминированная строка из FFmpeg.
    let ptr = unsafe { (*codec).name };
    if ptr.is_null() {
        "?".into()
    } else {
        // SAFETY: ptr — NUL-терминированная строка.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

fn ff_err(code: c_int) -> String {
    let mut buf = [0 as c_char; 128];
    // SAFETY: buf — валидный буфер с размером; см. av_strerror.
    unsafe { av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: av_strerror гарантирует NUL-терминацию при успехе.
    let text = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    format!("{code}: {text}")
}
