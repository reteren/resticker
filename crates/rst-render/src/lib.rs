//! D3D11 + DirectComposition renderer for resticker.
//!
//! One [`Renderer`] per overlay window: a D3D11 device, a DirectComposition
//! swapchain with premultiplied alpha, and a sprite shader. Event-driven by
//! design (ADR-006): nothing is rendered or presented unless
//! [`Renderer::draw`] is called — zero calls means zero GPU work, there is no
//! internal loop or timer.
//!
//! Ownership: the HWND belongs to the caller (an overlay window from
//! `rst-win32`) and must outlive the renderer. COM objects are owned by the
//! renderer and released on drop via the windows-rs smart pointers.
//!
//! Маска перекрытия (ADR-004) в этот срез не входит — она веха M4.

mod error;
mod renderer;
mod shader;
mod sprite;
mod texture;

pub use error::RenderError;
pub use renderer::Renderer;
pub use sprite::Sprite;
pub use texture::Texture;
