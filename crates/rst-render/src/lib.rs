//! D3D11 + DirectComposition renderer for resticker.
//!
//! One [`Device`] per process: the D3D11 device and context, compiled shaders,
//! sampler, blend state and constant buffer — textures are created and owned
//! here, uploaded once and drawn on any monitor. One [`WindowTarget`] per
//! overlay window/monitor: a DirectComposition target/visual on one HWND and a
//! composition swapchain with premultiplied alpha, physical size and DPI scale.
//! Event-driven by design (ADR-006): nothing is rendered or presented unless
//! [`Device::draw`] is called — zero calls means zero GPU work, there is no
//! internal loop or timer.
//!
//! Ownership: the HWND belongs to the caller (an overlay window from
//! `rst-win32`) and must outlive its `WindowTarget`. COM objects are owned by
//! the renderer and released on drop via the windows-rs smart pointers.
//!
//! Маска перекрытия (ADR-004) в этот срез не входит — она веха M4.

mod atlas;
mod device;
mod error;
mod icons;
mod marquee;
mod selection;
mod shader;
mod sprite;
mod text;
mod texture;
mod ui_textures;
mod video;
mod widgets;
mod window_highlight;
mod window_target;

pub use atlas::{AtlasFrame, TextureAtlas};
pub use device::Device;
pub use error::RenderError;
pub use icons::icon_rgba;
pub use marquee::{
    MARQUEE_DASH_DIP, MARQUEE_FILL_OPACITY, MARQUEE_GAP_DIP, MARQUEE_STROKE_OPACITY,
    MARQUEE_THICKNESS_DIP, MarqueeVisuals, marquee_visuals,
};
pub use selection::{
    Box2D, CHECKER_BLACK, CHECKER_MAGENTA, CHECKERBOARD_HLSL, EDIT_OVERLAY_OPACITY,
    HANDLE_SIZE_DIP, HIDDEN_STICKER_CHECKERBOARD_OPACITY, HandleKind, OUTLINE_THICKNESS_DIP,
    SelectionBox, SelectionVisuals, checkerboard_tile, edit_overlay, solid_sprite,
};
pub use sprite::Sprite;
pub use text::{Glyph, LINE_HEIGHT, glyph_for, rasterize, text_size, width_up_to};
pub use texture::Texture;
pub use ui_textures::{TextureFactory, UiTextures};
pub use video::VideoTextures;
pub use widgets::{
    Button, ButtonContent, Checkbox, EventResult, Icon, Key, Label, NumericField, Panel,
    PINNED_BTN_ADD_RULE, PINNED_BTN_UNPIN, PINNED_CHECK_INTERACT_LOCK, PINNED_CHECK_MOVE_LOCK,
    PINNED_GAP, PINNED_LOCK_PANEL_HEIGHT, PINNED_PAD, PINNED_PANEL_HEIGHT, PINNED_PANEL_ID,
    PINNED_BTN_ADD_HOST, PINNED_HOST_ROW_BASE, PINNED_VISIBLE_HOSTS, PINNED_LOCK_PANEL_MIN_WIDTH,
    pinned_lock_panel_height, PINNED_PANEL_WIDTH, PINNED_ROW_BASE, PINNED_ROW_FIELD_BITS, PINNED_RULE_ROW_H,
    PINNED_SECTION_ROW_H, PINNED_VISIBLE_RULES, PinnedPanel, PinnedRowField, PointerEvent,
    Primitive, ScrollBar, Slider, TextField, Widget, WidgetId, box_contains,
    build_pinned_lock_panel, build_pinned_panel, decode_pinned_row_id, lock_indicator, pin_indicator,
    pinned_row_id, theme,
};
pub use window_highlight::{HIGHLIGHT_THICKNESS_DIP, HighlightKind, WindowHighlight};
pub use window_target::{PresentSync, WindowTarget};
