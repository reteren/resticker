//! Событийный захват содержимого окна через `Windows.Graphics.Capture`.
//!
//! В отличие от [`crate::window_thumb`] этот модуль не делает CPU-readback и
//! не вырезает прямоугольник: вызывающий слой получает целую D3D11-текстуру
//! окна и применяет свой `CropRect` при отрисовке. `FrameArrived` сохраняет
//! только последний кадр и будит координатор. По замеру W1 (2026-09-10)
//! неподвижное окно не присылает кадры 3 секунды, а 30 перерисовок прислали
//! ровно 30 кадров; это важно для обещания SPEC §13 не просыпаться в покое.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{Interface, factory};

/// Ошибка создания или настройки событийного захвата окна.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("Windows Graphics Capture: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("window handle is null")]
    NullWindow,
    #[error("Windows Graphics Capture is not supported")]
    NotSupported,
    #[error("capture source has invalid size {width}x{height}")]
    InvalidSize { width: i32, height: i32 },
}

/// Последний кадр окна, полученный без копирования через CPU.
#[derive(Debug)]
pub struct CapturedFrame {
    /// Текстура принадлежит тому же D3D11-устройству, что был передан в
    /// [`WindowCapture::new`], поэтому её можно сразу отдать рендеру.
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
}

/// Живой событийный захват окна.
pub struct WindowCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    winrt_device: IDirect3DDevice,
    frame_arrived_token: i64,
    closed_token: i64,
    frame: Arc<Mutex<Option<CapturedFrame>>>,
    size: Arc<Mutex<SizeInt32>>,
    pending_resize: Arc<Mutex<Option<SizeInt32>>>,
    closed: Arc<AtomicBool>,
}

struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: балансируем успешный CoInitializeEx на том же потоке.
            unsafe { CoUninitialize() };
        }
    }
}

impl WindowCapture {
    /// Создать живой захват `hwnd` на общем D3D11-устройстве рендера.
    ///
    /// Вызов не создаёт собственного устройства и не делает CPU-копий.
    /// Для подключения пробуждения координатора используйте
    /// [`Self::new_with_wake`].
    pub fn new(device: &ID3D11Device, hwnd: isize) -> Result<Self, CaptureError> {
        Self::new_with_wake(device, hwnd, Box::new(|| {}))
    }

    /// Вариант конструктора с колбэком пробуждения координатора.
    ///
    /// Колбэк вызывается только при новом кадре или исчезновении источника;
    /// он должен лишь разбудить координатор и быстро вернуться.
    pub fn new_with_wake(
        device: &ID3D11Device,
        hwnd: isize,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> Result<Self, CaptureError> {
        if hwnd == 0 {
            return Err(CaptureError::NullWindow);
        }
        if !GraphicsCaptureSession::IsSupported()? {
            return Err(CaptureError::NotSupported);
        }

        // WGC использует WinRT-фабрики. Если поток уже находится в другом
        // COM-апартаменте, RPC_E_CHANGED_MODE штатен: существующего COM
        // достаточно, и CoUninitialize для него вызывать нельзя.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let _com_guard = ComGuard(com.is_ok());

        let dxgi: IDXGIDevice = device.cast()?;
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let interop: IGraphicsCaptureItemInterop =
            factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForWindow(windows::Win32::Foundation::HWND(hwnd as *mut _))? };
        let size = item.Size()?;
        validate_size(size)?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Замер W1 на Windows 11 показал, что системную жёлтую рамку и
        // курсор можно отключить без ошибки. Это также не требует UI-согласия.
        session.SetIsBorderRequired(false)?;
        session.SetIsCursorCaptureEnabled(false)?;

        let frame = Arc::new(Mutex::new(None));
        let size_state = Arc::new(Mutex::new(size));
        let pending_resize = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);

        let pool_for_frame = pool.clone();
        let frame_for_frame = frame.clone();
        let pending_resize_for_frame = pending_resize.clone();
        let wake_for_frame = wake.clone();
        let frame_arrived_token =
            pool.FrameArrived(&TypedEventHandler::new(move |_sender, _| {
                let Ok(frame_obj) = pool_for_frame.TryGetNextFrame() else {
                    wake_for_frame();
                    return Ok(());
                };
                let Ok(surface) = frame_obj.Surface() else {
                    wake_for_frame();
                    return Ok(());
                };
                let Ok(access) = surface.cast::<IDirect3DDxgiInterfaceAccess>() else {
                    wake_for_frame();
                    return Ok(());
                };
                let Ok(texture) = (unsafe { access.GetInterface::<ID3D11Texture2D>() }) else {
                    wake_for_frame();
                    return Ok(());
                };

                let mut desc =
                    windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                // SAFETY: texture — валидный объект, полученный из WGC surface.
                unsafe { texture.GetDesc(&mut desc) };
                if desc.Width == 0 || desc.Height == 0 {
                    wake_for_frame();
                    return Ok(());
                }

                // Resize обрабатывается в take_frame на потоке координатора:
                // callback не захватывает COM-устройство и делает только
                // публикацию кадра/сигнал пробуждения.
                if let Ok(mut pending) = pending_resize_for_frame.lock() {
                    *pending = Some(SizeInt32 {
                        Width: desc.Width as i32,
                        Height: desc.Height as i32,
                    });
                }
                if let Ok(mut slot) = frame_for_frame.lock() {
                    *slot = Some(CapturedFrame {
                        texture,
                        width: desc.Width,
                        height: desc.Height,
                    });
                }
                wake_for_frame();
                Ok(())
            }))?;

        let closed_for_handler = closed.clone();
        let wake_for_closed = wake.clone();
        let closed_token = item.Closed(&TypedEventHandler::new(move |_sender, _| {
            closed_for_handler.store(true, Ordering::Release);
            wake_for_closed();
            Ok(())
        }))?;

        session.StartCapture()?;
        Ok(Self {
            pool,
            session,
            item,
            winrt_device,
            frame_arrived_token,
            closed_token,
            frame,
            size: size_state,
            pending_resize,
            closed,
        })
    }

    /// Забрать последний новый кадр; промежуточные кадры намеренно заменяются.
    pub fn take_frame(&self) -> Option<CapturedFrame> {
        let frame = self.frame.lock().ok().and_then(|mut slot| slot.take());
        if frame.is_some() {
            self.recreate_after_resize();
        }
        frame
    }

    /// Источник прислал событие закрытия и больше не будет кадров.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Текущий размер источника в физических пикселях.
    pub fn size(&self) -> SizeInt32 {
        self.size.lock().map(|size| *size).unwrap_or_default()
    }

    fn recreate_after_resize(&self) {
        let Some(requested) = self.pending_resize.lock().ok().and_then(|pending| *pending) else {
            return;
        };
        let current = self.size.lock().map(|size| *size).unwrap_or_default();
        if requested == current {
            if let Ok(mut pending) = self.pending_resize.lock() {
                *pending = None;
            }
            return;
        }
        if let Err(err) = self.pool.Recreate(
            &self.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            requested,
        ) {
            // Раньше ошибка складывалась в слот `last_error`, который никто
            // не читал: отказ пересоздания пула был не просто незаметен —
            // он даже в журнал не попадал. Слот убран, причина пишется.
            tracing::warn!(error = %err, "не удалось пересоздать пул кадров захвата после изменения размера");
            return;
        }
        if let Ok(mut size) = self.size.lock() {
            *size = requested;
        }
        if let Ok(mut pending) = self.pending_resize.lock() {
            *pending = None;
        }
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        // Удаляем handlers до закрытия COM-объектов; окно могло исчезнуть —
        // это штатный путь, поэтому ошибки удаления намеренно игнорируются.
        let _ = self.pool.RemoveFrameArrived(self.frame_arrived_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn validate_size(size: SizeInt32) -> Result<(), CaptureError> {
    if size.Width <= 0 || size.Height <= 0 {
        Err(CaptureError::InvalidSize {
            width: size.Width,
            height: size.Height,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_capture_size_is_accepted() {
        assert!(
            validate_size(SizeInt32 {
                Width: 1,
                Height: 1
            })
            .is_ok()
        );
    }

    #[test]
    fn zero_or_negative_capture_size_is_rejected() {
        assert!(
            validate_size(SizeInt32 {
                Width: 0,
                Height: 100
            })
            .is_err()
        );
        assert!(
            validate_size(SizeInt32 {
                Width: 100,
                Height: -1
            })
            .is_err()
        );
    }
}
