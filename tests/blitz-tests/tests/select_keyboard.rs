//! Keyboard selection in a `<select>`.
//!
//! There was no keyboard activation for anything but a text input, so a
//! focussed picker could not be driven at all. Worse, the arrows never even
//! reached the keyboard handler: the KeyDown arm scrolls the page on
//! ArrowUp/Down/Home/End and returns, and the predicate that lets a key through
//! only knew about text inputs. A press on a select therefore scrolled the
//! document instead of choosing anything.

use blitz_test_harness::Harness;
use keyboard_types::Key;

const PICKER: &str = r#"<html><body style="margin:0; height:4000px">
    <select id="country" style="width:150px; height:30px">
        <option value="us">United States</option>
        <option value="fr">France</option>
        <option value="jp">Japan</option>
    </select>
</body></html>"#;

fn value_of(harness: &Harness<blitz_html::HtmlDocument>, selector: &str) -> String {
    let node_id = harness.node(selector);
    harness.base().select_value(node_id)
}

fn focussed_picker(html: &str) -> Harness<blitz_html::HtmlDocument> {
    let mut harness = Harness::from_html(html);
    harness.click("#country");
    assert_eq!(harness.focused(), Some(harness.node("#country")));
    harness
}

#[test]
fn arrow_down_moves_the_selection_forwards() {
    let mut harness = focussed_picker(PICKER);
    assert_eq!(value_of(&harness, "#country"), "us");

    harness.press(Key::ArrowDown);
    assert_eq!(
        value_of(&harness, "#country"),
        "fr",
        "ArrowDown must move the selection on, not scroll the page"
    );

    harness.press(Key::ArrowDown);
    assert_eq!(value_of(&harness, "#country"), "jp");
}

#[test]
fn arrow_up_moves_the_selection_backwards() {
    let mut harness = focussed_picker(PICKER);
    harness.press(Key::End);
    assert_eq!(value_of(&harness, "#country"), "jp");

    harness.press(Key::ArrowUp);
    assert_eq!(value_of(&harness, "#country"), "fr");
}

#[test]
fn the_selection_does_not_wrap_past_either_end() {
    let mut harness = focussed_picker(PICKER);
    harness.press(Key::ArrowUp);
    assert_eq!(
        value_of(&harness, "#country"),
        "us",
        "at the first option ArrowUp must do nothing rather than wrap"
    );

    harness.press(Key::End);
    harness.press(Key::ArrowDown);
    assert_eq!(value_of(&harness, "#country"), "jp");
}

#[test]
fn home_and_end_jump_to_the_first_and_last_option() {
    let mut harness = focussed_picker(PICKER);
    harness.press(Key::End);
    assert_eq!(value_of(&harness, "#country"), "jp");

    harness.press(Key::Home);
    assert_eq!(value_of(&harness, "#country"), "us");
}

#[test]
fn a_disabled_option_is_stepped_over() {
    let mut harness = focussed_picker(
        r#"<html><body style="margin:0; height:4000px">
            <select id="country" style="width:150px; height:30px">
                <option value="us">United States</option>
                <option value="fr" disabled>France</option>
                <option value="jp">Japan</option>
            </select>
        </body></html>"#,
    );
    assert_eq!(value_of(&harness, "#country"), "us");

    harness.press(Key::ArrowDown);
    assert_eq!(
        value_of(&harness, "#country"),
        "jp",
        "a disabled option cannot be chosen, so the arrow steps over it"
    );
}

#[test]
fn arrows_move_the_selection_rather_than_scrolling_the_page() {
    let mut harness = focussed_picker(PICKER);
    let before = harness.base().viewport_scroll().y;

    harness.press(Key::ArrowDown);

    assert_eq!(
        harness.base().viewport_scroll().y,
        before,
        "a focussed select claims the arrow keys; the page must not scroll under it"
    );
    assert_eq!(value_of(&harness, "#country"), "fr");
}

#[test]
fn an_unfocussed_select_does_not_claim_the_arrows() {
    let mut harness = Harness::from_html(PICKER);
    let before = harness.base().viewport_scroll().y;

    harness.press(Key::ArrowDown);

    assert_ne!(
        harness.base().viewport_scroll().y,
        before,
        "with nothing focussed the arrows must still scroll the document"
    );
    assert_eq!(value_of(&harness, "#country"), "us");
}
