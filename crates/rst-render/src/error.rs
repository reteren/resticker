//! Ошибки rst-render.

/// Ошибка создания устройства/цели рендера, загрузки текстуры или отрисовки кадра.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// Ошибка Win32/D3D/DXGI вызова.
    #[error("Win32/D3D error: {0}")]
    Windows(#[from] windows::core::Error),
    /// Файл изображения не удалось прочитать или декодировать.
    #[error("could not decode the image \"{path}\": {source}")]
    ImageDecode {
        /// Путь к файлу (для сообщения пользователю).
        path: String,
        /// Исходная ошибка крейта image.
        source: image::ImageError,
    },
    /// Пиксельные данные не соответствуют размерам текстуры.
    #[error("malformed texture data: {0}")]
    InvalidTextureData(String),
    /// HLSL-шейдер не скомпилировался (текст компилятора прилагается).
    #[error("shader compilation failed: {0}")]
    ShaderCompile(String),
    /// Устройство D3D потеряно (TDR, смена драйвера, выход из сна).
    /// Устройство и цели рендера нужно пересоздать (ARCHITECTURE.md, раздел 11).
    #[error("the D3D device was lost ({0:?}); device and targets must be recreated")]
    DeviceLost(windows::core::HRESULT),
}
