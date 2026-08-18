use crate::{BaseDocument, node::GeneratedTextInputEvent, util::has_clipboard_modifier};
use blitz_traits::node_id::NodeId;
use blitz_traits::{
    SmolStr,
    events::{BlitzInputEvent, BlitzKeyEvent, DomEvent, DomEventData},
};
use keyboard_types::{Code, Key, Modifiers};
use markup5ever::local_name;

pub(super) enum KeyboardOrTextInputEvent {
    KeyPress(BlitzKeyEvent),
    AppleStandardKeyBinding(SmolStr),
}

/// Whether this keystroke is the Copy chord.
///
/// The physical key is checked first and the character second, so a layout that
/// puts a different character on that key, or a modifier that rewrites it, still
/// copies. Reading only the character is why Cmd+C could silently do nothing.
///
/// The caller has already established that a clipboard modifier is held.
fn is_document_copy(event: &BlitzKeyEvent) -> bool {
    event.code == Code::KeyC
        || matches!(&event.key, Key::Character(c) if c.eq_ignore_ascii_case("c"))
}

pub(crate) fn handle_key_or_input_event<F: FnMut(DomEvent)>(
    doc: &mut BaseDocument,
    target: NodeId,
    event: KeyboardOrTextInputEvent,
    dispatch_event: F,
) {
    if let KeyboardOrTextInputEvent::KeyPress(event) = &event {
        if event.key == Key::Tab {
            if event.modifiers.contains(Modifiers::SHIFT) {
                doc.focus_prev_node();
            } else {
                doc.focus_next_node();
            }
            return;
        }

        /*
         * Copy a document selection: the transcript, a label, anything outside
         * a text input.
         *
         * Matched on the physical `code` as well as the character, the same way
         * `clipboard_command` does for text inputs. Reading only `key` meant a
         * layout that puts something else on that key, or a modifier that
         * rewrites the character, produced no copy at all — the keystroke
         * simply did nothing, with no way to tell it apart from an empty
         * selection.
         *
         * A focused text input no longer suppresses this. It used to: the
         * assumption was that a focused field owns the keystroke, but an app
         * whose composer holds focus permanently — which is the normal state of
         * a chat window — could then never copy from its own transcript, and
         * the selection the user could see highlighted was not what got copied.
         * What decides now is where the selection actually is: a field with a
         * selection of its own keeps the keystroke, and otherwise the document
         * selection is copied.
         */
        if event.state.is_pressed() && has_clipboard_modifier(event.modifiers) {
            if is_document_copy(event) {
                let field_owns_the_keystroke = doc.focus_node_id.is_some_and(|id| {
                    doc.get_node(id)
                        .and_then(|n| n.element_data())
                        .and_then(|e| e.text_input_data())
                        .is_some_and(|input| !input.editor.raw_selection().text_range().is_empty())
                });

                if !field_owns_the_keystroke {
                    if let Some(text) = doc.get_selected_text() {
                        if !text.is_empty() {
                            let _ = doc.shell_provider.set_clipboard_text(text);
                            return;
                        }
                    }
                }
            }
        }
    }

    if let Some(node_id) = doc.focus_node_id {
        if target != node_id {
            return;
        }

        let node = &mut doc.nodes[node_id];
        let Some(element_data) = node.element_data_mut() else {
            return;
        };

        if let Some(input_data) = element_data.text_input_data_mut() {
            let generated_event = match event {
                KeyboardOrTextInputEvent::KeyPress(blitz_key_event) => input_data
                    .apply_keypress_event(
                        &mut doc.font_ctx.lock().unwrap(),
                        &mut doc.layout_ctx,
                        &*doc.shell_provider,
                        blitz_key_event,
                    ),
                KeyboardOrTextInputEvent::AppleStandardKeyBinding(command) => input_data
                    .apply_apple_standard_keybinding(
                        &mut doc.font_ctx.lock().unwrap(),
                        &mut doc.layout_ctx,
                        &*doc.shell_provider,
                        &command,
                    ),
            };

            if let Some(generated_event) = generated_event {
                doc.apply_generated_text_input_event(node_id, generated_event, dispatch_event);
            }
        }
    }
}

impl BaseDocument {
    pub(crate) fn apply_generated_text_input_event<F: FnMut(DomEvent)>(
        &mut self,
        node_id: NodeId,
        event: GeneratedTextInputEvent,
        mut dispatch_event: F,
    ) {
        let node = &mut self.nodes[node_id];
        let element_data = node
            .element_data_mut()
            .expect("apply_generated_text_input_event called on a node that is not an element");
        let input_data = element_data
            .text_input_data_mut()
            .expect("apply_generated_text_input_event called on a node that is not a text input");

        match event {
            GeneratedTextInputEvent::Input => {
                let value = input_data.editor.raw_text().to_string();
                dispatch_event(DomEvent::new(
                    node_id,
                    DomEventData::Input(BlitzInputEvent { value }),
                ));
                self.shell_provider.request_redraw();
            }
            GeneratedTextInputEvent::Select | GeneratedTextInputEvent::PreEditChange => {
                self.shell_provider.request_redraw();
            }
            GeneratedTextInputEvent::Submit => {
                // TODO: Generate submit event that can be handled by script
                implicit_form_submission(self, node_id);
            }
        }
    }
}

/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#field-that-blocks-implicit-submission
fn implicit_form_submission(doc: &BaseDocument, text_target: NodeId) {
    let Some(form_owner_id) = doc.controls_to_form.get(&text_target) else {
        return;
    };
    if doc
        .controls_to_form
        .iter()
        .filter(|(_control_id, form_id)| *form_id == form_owner_id)
        .filter_map(|(control_id, _)| doc.nodes[*control_id].element_data())
        .filter(|element_data| {
            element_data.attr(local_name!("type")).is_some_and(|t| {
                matches!(
                    t,
                    "text"
                        | "search"
                        | "email"
                        | "url"
                        | "tel"
                        | "password"
                        | "date"
                        | "month"
                        | "week"
                        | "time"
                        | "datetime-local"
                        | "number"
                )
            })
        })
        .count()
        > 1
    {
        return;
    }

    doc.submit_form(*form_owner_id, *form_owner_id);
}

/// The Copy chord has to be recognised however the platform reports it.
///
/// Copying from the transcript was unreliable in a way that looked random: the
/// keystroke was matched on the character alone, so anything that changed the
/// character — a non-QWERTY layout, a modifier combination the platform folds
/// into it — produced no copy and no error either.
#[cfg(test)]
mod copy_chord_tests {
    use super::*;
    use blitz_traits::events::KeyState;
    use keyboard_types::Location;

    fn event(key: Key, code: Code) -> BlitzKeyEvent {
        BlitzKeyEvent {
            key,
            code,
            modifiers: Modifiers::CONTROL,
            location: Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }
    }

    #[test]
    fn the_plain_character_is_a_copy() {
        assert!(is_document_copy(&event(
            Key::Character("c".into()),
            Code::KeyC
        )));
    }

    /// Ctrl+C arrives as the ETX control character on some platforms, and the
    /// physical key is the only thing left that still says "C".
    #[test]
    fn a_control_character_is_a_copy_by_its_physical_key() {
        assert!(is_document_copy(&event(
            Key::Character("\u{3}".into()),
            Code::KeyC
        )));
    }

    /// A layout that puts another character on the C key still copies.
    #[test]
    fn a_remapped_character_is_a_copy_by_its_physical_key() {
        assert!(is_document_copy(&event(
            Key::Character("ç".into()),
            Code::KeyC
        )));
    }

    /// And the character still counts when the physical key is unknown, which
    /// is what a synthesised or remapped event reports.
    #[test]
    fn an_unidentified_key_is_a_copy_by_its_character() {
        assert!(is_document_copy(&event(
            Key::Character("C".into()),
            Code::Unidentified
        )));
    }

    #[test]
    fn another_key_is_not_a_copy() {
        assert!(!is_document_copy(&event(
            Key::Character("v".into()),
            Code::KeyV
        )));
    }
}
