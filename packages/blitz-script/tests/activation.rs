//! What a checkbox, a radio and a switch do when something presses them.
//!
//! Driven through the debug-control protocol against a document in another
//! process, so every assertion is something the running page reported rather
//! than something read out of the engine that produced it. A page that merely
//! parses cannot pass any of these: each one requires a press to have been
//! delivered, an activation to have run, and listeners to have seen the result.

#![cfg(feature = "debug-control")]

mod harness;

use harness::Driven;

#[test]
fn a_click_listener_sees_the_checkedness_the_press_produced() {
    // HTML flips a checkbox in the pre-click activation steps, *before* the
    // event is dispatched, so every listener sees the value the press produced.
    // Doing it afterwards hands `click` the previous value, which is the
    // ordinary way to read a checkbox and reads as a control that responds to
    // every second press.
    let page = Driven::open("checkbox-activation.html");
    assert_eq!(page.text("#log"), "quiet");

    page.click("#bare");

    assert_eq!(page.text("#log"), "click:true,input:true,change:true");
    assert!(page.checked("#bare"));
}

#[test]
fn a_label_press_reaches_the_control_it_labels() {
    // A label's activation behaviour is to fire a click at its control. Running
    // the control's default action instead toggles it and fires `input` and
    // `change` while no `click` ever arrives, which is the case every switch
    // and styled checkbox depends on.
    let page = Driven::open("switch-over-label.html");
    assert_eq!(page.text("#log"), "quiet");

    page.click("#switch");

    assert_eq!(page.text("#log"), "click:true,input:true,change:true");
    assert!(page.checked("#hidden-input"));
}

#[test]
fn a_disabled_control_hears_nothing_from_its_label() {
    // The pre-click activation steps refuse to toggle a disabled input, but
    // listeners run before that refusal is observable, so a `click` delivered
    // here runs whatever the application wired to it -- the one thing
    // `disabled` is there to prevent.
    let page = Driven::open("checkbox-activation.html");

    page.click("#disabled-label");

    assert_eq!(page.text("#disabled-log"), "quiet");
    assert!(!page.checked("#off-limits"));
}

#[test]
fn cancelling_a_click_puts_the_checkedness_back() {
    // The canceled activation steps. `preventDefault` on the click undoes the
    // pre-click flip, so a control can refuse its own activation.
    let page = Driven::open("checkbox-activation.html");
    assert!(!page.checked("#vetoed"));

    page.click("#vetoed");

    assert!(
        !page.checked("#vetoed"),
        "a cancelled click must leave the checkbox as it was"
    );
}

#[test]
fn selecting_a_radio_clears_its_set_before_listeners_run() {
    let page = Driven::open("checkbox-activation.html");
    assert!(page.checked("#one"));

    page.click("#two");

    assert_eq!(page.text("#radio-log"), "click:true,input:true,change:true");
    assert!(page.checked("#two"));
    assert!(!page.checked("#one"), "the set has one selection");
}

#[test]
fn a_control_bound_to_false_is_not_checked() {
    // `checked` is an HTML boolean attribute: present means on, whatever the
    // value reads. Reflecting a written `false` into it therefore *set* the
    // input, and every controlled checkbox, radio and switch came up already
    // selected whatever its state said.
    let page = Driven::open("controlled-checkbox.html");

    assert_eq!(page.text("#state"), "false");
    assert!(!page.checked("#controlled"));
}

#[test]
fn writing_checked_from_script_moves_the_control() {
    // After construction the checkedness lives in the element's own state, and
    // that is what the renderer, the accessibility tree and `change` read. A
    // property write that reached only the attribute left a controlled
    // component unable to drive its own input.
    let page = Driven::open("controlled-checkbox.html");
    assert!(!page.checked("#controlled"));

    page.click("#controlled");
    assert!(page.checked("#controlled"));

    // Back off again from script alone, with no press involved.
    page.click("#controlled");
    assert!(!page.checked("#controlled"));
}
