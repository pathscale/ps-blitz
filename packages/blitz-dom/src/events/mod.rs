mod driver;
mod focus;
mod ime;
mod keyboard;
mod pointer;

use crate::util::Point;
use blitz_traits::events::{DomEvent, DomEventData, PointerCoords, UiEvent};
use blitz_traits::node_id::NodeId;
pub use driver::{EventDriver, EventHandler, NoopEventHandler};
use focus::generate_focus_events;
pub(crate) use ime::handle_ime_event;
use keyboard::{KeyboardOrTextInputEvent, handle_key_or_input_event};
use keyboard_types::Key;
pub(crate) use pointer::{DragMode, ScrollAnimationState};
use pointer::{handle_click, handle_pointerdown, handle_pointermove, handle_pointerup};

use crate::{BaseDocument, events::pointer::handle_wheel};

fn adjust_coords_for_subdocument(
    coords: &mut PointerCoords,
    offset: Point<f32>,
    viewport_scroll: Point<f64>,
) {
    coords.page_x -= offset.x - viewport_scroll.x as f32;
    coords.page_y -= offset.y - viewport_scroll.y as f32;
    coords.client_x -= offset.x;
    coords.client_y -= offset.y;
}

fn map_dom_event_to_ui_event(
    event: &mut DomEvent,
    node_offset: Point<f32>,
    viewport_scroll: Point<f64>,
) -> Option<UiEvent> {
    // TODO: eliminate clone
    match event.data.clone() {
        DomEventData::PointerMove(mut event) => {
            adjust_coords_for_subdocument(&mut event.coords, node_offset, viewport_scroll);
            Some(UiEvent::PointerMove(event))
        }
        DomEventData::PointerDown(mut event) => {
            adjust_coords_for_subdocument(&mut event.coords, node_offset, viewport_scroll);
            Some(UiEvent::PointerDown(event))
        }
        DomEventData::PointerUp(mut event) => {
            adjust_coords_for_subdocument(&mut event.coords, node_offset, viewport_scroll);
            Some(UiEvent::PointerUp(event))
        }
        DomEventData::PointerCancel(mut event) => {
            adjust_coords_for_subdocument(&mut event.coords, node_offset, viewport_scroll);
            Some(UiEvent::PointerCancel(event))
        }

        // Enter/leave events will be recreated by sub-document's event driver
        // based move events
        DomEventData::PointerEnter(_) => None,
        DomEventData::PointerLeave(_) => None,
        DomEventData::PointerOver(_) => None,
        DomEventData::PointerOut(_) => None,

        // Mouse events will be recreated by sub-document's event driver
        // based pointer events
        DomEventData::MouseMove(_) => None,
        DomEventData::MouseDown(_) => None,
        DomEventData::MouseUp(_) => None,
        DomEventData::MouseEnter(_) => None,
        DomEventData::MouseLeave(_) => None,
        DomEventData::MouseOver(_) => None,
        DomEventData::MouseOut(_) => None,

        // Touch events will be recreated by sub-document's event driver
        // based pointer events
        DomEventData::TouchStart(_) => None,
        DomEventData::TouchMove(_) => None,
        DomEventData::TouchEnd(_) => None,
        DomEventData::TouchCancel(_) => None,

        DomEventData::KeyDown(data) => Some(UiEvent::KeyDown(data)),
        DomEventData::KeyUp(data) => Some(UiEvent::KeyUp(data)),
        DomEventData::Ime(data) => Some(UiEvent::Ime(data)),
        DomEventData::AppleStandardKeybinding(data) => Some(UiEvent::AppleStandardKeybinding(data)),

        DomEventData::KeyPress(_) => None,
        DomEventData::Click(_) => None,
        DomEventData::ContextMenu(_) => None,
        DomEventData::DoubleClick(_) => None,
        DomEventData::Input(_) => None,
        DomEventData::Wheel(data) => Some(UiEvent::Wheel(data)),
        DomEventData::Scroll(_) => None,
        DomEventData::Focus(_) => None,
        DomEventData::Blur(_) => None,
        DomEventData::FocusIn(_) => None,
        DomEventData::FocusOut(_) => None,
    }
}

pub(crate) fn handle_dom_event<F: FnMut(DomEvent)>(
    doc: &mut BaseDocument,
    event: &mut DomEvent,
    mut dispatch_event: F,
) {
    let target_node_id = event.target;
    let node = &mut doc.nodes[target_node_id];
    let pos = node.absolute_position(0.0, 0.0);

    // Whether this event can move the caret/selection (or change the text) of a text input,
    // in which case we need to update the input's scroll offset afterwards.
    let may_move_text_input_caret = match &event.data {
        DomEventData::KeyDown(_)
        | DomEventData::AppleStandardKeybinding(_)
        | DomEventData::Ime(_)
        | DomEventData::Click(_) => true,
        DomEventData::PointerDown(event) | DomEventData::PointerMove(event) => {
            !event.buttons.is_empty()
        }
        _ => false,
    };

    // Handle event forwarding for sub-document
    if let Some(sub_doc) = node.subdoc_mut() {
        let viewport_scroll = sub_doc.inner().viewport_scroll();

        let set_focus = matches!(
            &event.data,
            DomEventData::PointerDown(_) | DomEventData::PointerUp(_)
        );
        let ui_event = map_dom_event_to_ui_event(event, pos, viewport_scroll);

        if let Some(ui_event) = ui_event {
            sub_doc.handle_ui_event(ui_event);
        }

        if set_focus {
            generate_focus_events(
                doc,
                &mut |doc| {
                    doc.set_focus_to(target_node_id);
                },
                &mut dispatch_event,
            );
        }

        return;
    }

    // Handle event forwarding for custom widget
    #[cfg(feature = "custom-widget")]
    if let Some(widget_data) = node
        .element_data_mut()
        .and_then(|el| el.custom_widget_data_mut())
    {
        let set_focus = matches!(
            &event.data,
            DomEventData::PointerDown(_) | DomEventData::PointerUp(_)
        );
        let viewport_scroll = Point { x: 0.0, y: 0.0 };
        let ui_event = map_dom_event_to_ui_event(event, pos, viewport_scroll);

        if let Some(ui_event) = ui_event {
            widget_data.widget.handle_event(&ui_event);
        }

        if set_focus {
            generate_focus_events(
                doc,
                &mut |doc| {
                    doc.set_focus_to(target_node_id);
                },
                &mut dispatch_event,
            );
        }

        return;
    }

    match &event.data {
        DomEventData::PointerMove(event) => {
            let changed = handle_pointermove(doc, target_node_id, event, dispatch_event);
            if changed {
                doc.shell_provider.request_redraw();
            }
        }
        DomEventData::MouseMove(_) => {
            // Do nothing (handled in PointerMove)
        }
        DomEventData::PointerDown(event) => {
            handle_pointerdown(
                doc,
                target_node_id,
                event.page_x(),
                event.page_y(),
                event.button,
                event.mods,
                &mut dispatch_event,
            );
        }
        DomEventData::MouseDown(_) => {
            // Do nothing (handled in PointerDown)
        }
        DomEventData::PointerUp(event) => {
            handle_pointerup(doc, target_node_id, event, dispatch_event);
        }
        DomEventData::MouseUp(_) => {
            // Do nothing (handled in PointerUp)
        }
        DomEventData::PointerCancel(_) => {
            // Do nothing (active state is reset in the event driver)
        }
        DomEventData::Click(event) => {
            handle_click(doc, target_node_id, event, &mut dispatch_event);
        }
        DomEventData::KeyDown(event) => {
            // Keyboard scrolling, before the text-input handling below claims
            // the key. Page Up, Page Down, Home, End and the arrows scroll the
            // nearest scroll container at or above the target, which is what
            // every browser does and what a keyboard-only reader needs: without
            // it a long settings page or a transcript can only be moved with a
            // pointer.
            //
            // Skipped whenever the target can take the key as text, so typing
            // in a composer still moves the caret rather than the page.
            if !scroll_key_is_claimed_by(doc, target_node_id, &event.key) {
                if let Some((dx, dy)) = scroll_delta_for_key(doc, target_node_id, &event.key) {
                    doc.scroll_nearest_container_by(target_node_id, dx, dy);
                    return;
                }
            }

            handle_key_or_input_event(
                doc,
                target_node_id,
                KeyboardOrTextInputEvent::KeyPress(event.clone()),
                dispatch_event,
            );
        }
        DomEventData::KeyPress(_) => {
            // Do nothing (no default action)
        }
        DomEventData::KeyUp(_) => {
            // Do nothing (no default action)
        }
        DomEventData::AppleStandardKeybinding(event) => {
            handle_key_or_input_event(
                doc,
                target_node_id,
                KeyboardOrTextInputEvent::AppleStandardKeyBinding(event.clone()),
                dispatch_event,
            );
        }
        DomEventData::Ime(event) => {
            handle_ime_event(doc, event.clone(), dispatch_event);
        }
        DomEventData::Input(_) => {
            // Do nothing (no default action)
        }
        DomEventData::ContextMenu(_) => {
            // TODO: Open context menu
        }
        DomEventData::DoubleClick(_) => {
            // Do nothing (no default action)
        }
        DomEventData::PointerEnter(_) => {
            // Do nothing (no default action)
        }
        DomEventData::PointerLeave(_) => {
            // Do nothing (no default action)
        }
        DomEventData::PointerOver(_) => {
            // Do nothing (no default action)
        }
        DomEventData::PointerOut(_) => {
            // Do nothing (no default action)
        }
        DomEventData::MouseEnter(_) => {
            // Do nothing (no default action)
        }
        DomEventData::MouseLeave(_) => {
            // Do nothing (no default action)
        }
        DomEventData::MouseOver(_) => {
            // Do nothing (no default action)
        }
        DomEventData::MouseOut(_) => {
            // Do nothing (no default action)
        }
        DomEventData::TouchStart(_) => {
            // Do nothing (default action handled via PointerDown)
        }
        DomEventData::TouchMove(_) => {
            // Do nothing (default action handled via PointerMove)
        }
        DomEventData::TouchEnd(_) => {
            // Do nothing (default action handled via PointerUp)
        }
        DomEventData::TouchCancel(_) => {
            // Do nothing (default action handled via PointerCancel)
        }
        DomEventData::Scroll(_) => {
            // Handled elsewhere
        }
        DomEventData::Wheel(event) => {
            handle_wheel(doc, target_node_id, event.clone(), dispatch_event);
        }
        DomEventData::Focus(_) => {
            // Do nothing (no default action)
        }
        DomEventData::Blur(_) => {
            // Do nothing (no default action)
        }
        DomEventData::FocusIn(_) => {
            // Do nothing (no default action)
        }
        DomEventData::FocusOut(_) => {
            // Do nothing (no default action)
        }
    }

    // Keep the focused text input scrolled so that its caret stays visible. Keyboard/IME events
    // target the focused input, and pointer events that hit a text input focus it, so the
    // focused node is the input whose caret may have moved.
    if may_move_text_input_caret {
        if let Some(focus_id) = doc.focus_node_id {
            doc.clamp_text_input_scroll(focus_id);
        }
    }
}

/// Whether the key should go to the target as text rather than scroll the page.
///
/// A text input owns the arrows, Home and End for caret movement, and a
/// `contenteditable` does the same. Page Up and Page Down are not claimed:
/// browsers scroll on those even inside a field.
fn scroll_key_is_claimed_by(doc: &BaseDocument, node_id: NodeId, key: &Key) -> bool {
    // Page Up and Page Down are never claimed: browsers scroll on those even
    // with the caret in a field, which is what makes a long page usable while
    // typing into a filter box at the top of it.
    if matches!(key, Key::PageUp | Key::PageDown) {
        return false;
    }
    doc.get_node(node_id)
        .and_then(|node| node.element_data())
        .is_some_and(|element| element.text_input_data().is_some())
}

/// The scroll a key asks for, in CSS pixels, or `None` if it asks for none.
///
/// A page is most of the scrollport rather than all of it: browsers overlap by
/// a couple of lines so the reader keeps their place.
fn scroll_delta_for_key(doc: &BaseDocument, node_id: NodeId, key: &Key) -> Option<(f64, f64)> {
    const LINE: f64 = 40.0;
    let viewport_height = f64::from(doc.viewport().window_size.1) / doc.viewport().scale_f64();
    let page = (viewport_height - 2.0 * LINE).max(LINE);
    let _ = node_id;
    match key {
        Key::PageDown => Some((0.0, -page)),
        Key::PageUp => Some((0.0, page)),
        Key::ArrowDown => Some((0.0, -LINE)),
        Key::ArrowUp => Some((0.0, LINE)),
        Key::Home => Some((0.0, 1.0e7)),
        Key::End => Some((0.0, -1.0e7)),
        _ => None,
    }
}
