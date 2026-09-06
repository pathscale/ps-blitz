//! # macOS only
//!
//! The expected 70px is 40px of padding, a 20px line box, and 10px of padding.
//! The line box exists only because the cell's `x` shapes into one: with no
//! font registered parley produces no line, the cell measures 50px, and the
//! assertion fails for a reason that has nothing to do with how table-cell
//! padding is counted.
//!
//! macOS resolves a face through Core Text, costing no system library. On Linux
//! this compiles out.
#![cfg(target_os = "macos")]

use blitz_test_harness::Harness;

#[test]
fn float_layout_counts_table_cell_padding_once() {
    let harness = Harness::from_html(
        r#"<table style="border-spacing:0"><tr><td id="cell" style="font-size:10px;line-height:20px;padding:40px 0 10px">x</td></tr></table>"#,
    );

    assert_eq!(harness.layout_rect("#cell").height, 70.0);
}
