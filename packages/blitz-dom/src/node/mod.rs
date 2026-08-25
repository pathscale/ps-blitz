#![allow(clippy::module_inception)]

mod attributes;
#[cfg(feature = "shadow-dom")]
mod custom_element;
#[cfg(feature = "custom-widget")]
mod custom_widget;
mod element;
mod node;
pub(crate) mod scrollbar;
mod stylo_data;
mod text;

pub use attributes::{AttrAtom, Attribute, Attributes};
#[cfg(feature = "shadow-dom")]
pub use custom_element::{
    CustomElement, CustomElementCtx, CustomElementData, CustomElementDefinition,
    CustomElementFactory, CustomElementRegistry,
};
#[cfg(feature = "custom-widget")]
pub use custom_widget::{
    ComputedStyles, CustomWidgetData, CustomWidgetStatus, ProxyRenderContext, Widget,
};
#[cfg(feature = "svg")]
pub use element::SvgImageData;
pub use element::{
    CanvasData, DocumentData, ElementData, ImageData, ImageResourceData, ListItemLayout,
    ListItemLayoutPosition, Marker, RasterImageData, SpecialElementData, SpecialElementType,
    Status,
};
pub use node::*;
pub use scrollbar::{ScrollbarColor, ScrollbarRef, ScrollbarWidth};
pub use text::{GeneratedTextInputEvent, TextBrush, TextInputData, TextLayout};
