use blitz_dom::node::RasterImageData;
use boa_engine::Context;

use crate::state::DomCtx;

/// Read an embedder-marked raster from a native JavaScript callback.
///
/// The document remains on its owning thread. Cloning the raster shares its
/// decoded Blob; this does not copy the pixel allocation or perform layout.
pub fn raster_image_for_attribute(
    context: &Context,
    name: &str,
    value: &str,
) -> Option<RasterImageData> {
    let dom = context.get_data::<DomCtx>()?;
    let document = dom.doc.try_borrow().ok()?;
    document.tree().iter().find_map(|(_, node)| {
        let element = node.element_data()?;
        if !element.attrs().iter().any(|attribute| {
            attribute.name.local.as_ref() == name && attribute.value.as_ref() == value
        }) {
            return None;
        }
        element.raster_image_data().cloned()
    })
}
