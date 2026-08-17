//! Cmd/Ctrl+C puts the selected document text on the system clipboard.
//!
//! The engine answers the copy shortcut itself: no `copy` event is dispatched
//! to script, so an embedding app cannot implement this in JS even if it wants
//! to. This pins the whole path, from the click that makes a selection to the
//! string handed to the shell, because the two halves were written a year
//! apart and only the input half had a test.

use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{
    events::{
        BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, KeyState, MouseEventButton,
        MouseEventButtons, Point, PointerCoords, PointerDetails, UiEvent,
    },
    shell::{ClipboardError, ColorScheme, ShellProvider, Viewport},
};
use keyboard_types::{Code, Key, Location, Modifiers};
use std::sync::{Arc, Mutex};

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
  body { margin: 0; font-size: 16px; }
  p { margin: 0; }
</style></head>
<body><p id="para">alpha beta gamma</p></body></html>
"#;

/// A shell that records what was put on the clipboard.
#[derive(Default)]
struct RecordingShell {
    written: Mutex<Vec<String>>,
}

impl ShellProvider for RecordingShell {
    fn set_clipboard_text(&self, text: String) -> Result<(), ClipboardError> {
        self.written.lock().unwrap().push(text);
        Ok(())
    }
}

fn make_doc(shell: Arc<RecordingShell>) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            shell_provider: Some(shell as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn pointer_event(x: f32, y: f32, buttons: MouseEventButtons) -> BlitzPointerEvent {
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons,
        mods: Default::default(),
        details: PointerDetails::default(),
        element: Point::default(),
        active_pointers: Default::default(),
    }
}

fn click_at(doc: &mut HtmlDocument, x: f32, y: f32) {
    let held = MouseEventButtons::from(MouseEventButton::Main);
    doc.handle_ui_event(UiEvent::PointerDown(pointer_event(x, y, held)));
    doc.handle_ui_event(UiEvent::PointerUp(pointer_event(
        x,
        y,
        MouseEventButtons::None,
    )));
}

/// Press the copy shortcut. `CONTROL` is accepted on every platform, so the
/// test does not have to know which modifier is the local one.
fn press_copy(doc: &mut HtmlDocument) {
    doc.handle_ui_event(UiEvent::KeyDown(BlitzKeyEvent {
        key: Key::Character("c".into()),
        code: Code::KeyC,
        modifiers: Modifiers::CONTROL,
        location: Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text: None,
    }));
}

/// A point inside "beta", resolved from the layout rather than guessed.
fn point_in_beta(doc: &HtmlDocument) -> Option<(f32, f32)> {
    let para = doc.query_selector("#para").ok()??;
    let rect = doc.get_client_bounding_rect(para)?;
    let y = (rect.y + rect.height / 2.0) as f32;
    let text = "alpha beta gamma";
    let beta = text.find("beta")?;
    let beta_end = beta + "beta".len();

    let mut x = rect.x as f32;
    let right = (rect.x + rect.width) as f32;
    while x < right {
        if let Some((_, offset)) = doc.find_text_position(x, y)
            && offset > beta
            && offset < beta_end
        {
            return Some((x, y));
        }
        x += 1.0;
    }
    None
}

#[test]
fn copy_puts_the_double_clicked_word_on_the_clipboard() {
    let shell = Arc::new(RecordingShell::default());
    let mut doc = make_doc(shell.clone());
    let Some((x, y)) = point_in_beta(&doc) else {
        eprintln!("skipping: no usable font (text measures 0x0)");
        return;
    };

    click_at(&mut doc, x, y);
    click_at(&mut doc, x, y);
    press_copy(&mut doc);

    assert_eq!(
        shell.written.lock().unwrap().as_slice(),
        &["beta".to_string()],
        "the copy shortcut should hand the selected word to the shell"
    );
}

#[test]
fn copy_with_no_selection_writes_nothing() {
    let shell = Arc::new(RecordingShell::default());
    let mut doc = make_doc(shell.clone());
    let Some((x, y)) = point_in_beta(&doc) else {
        eprintln!("skipping: no usable font (text measures 0x0)");
        return;
    };

    // A single click places a caret and selects nothing.
    click_at(&mut doc, x, y);
    press_copy(&mut doc);

    assert!(
        shell.written.lock().unwrap().is_empty(),
        "copying an empty selection should not clear the clipboard"
    );
}
