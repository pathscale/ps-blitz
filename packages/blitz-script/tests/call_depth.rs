//! A page can nest calls as deeply as a browser lets it.
//!
//! Boa's defaults are a sandbox's: 512 nested calls, and a value stack of
//! 10240 slots that runs out first at roughly seven slots a frame. Neither
//! number is reachable by anything a page author would recognise as recursion.
//! A framework spends the frames instead: a Solid application renders its
//! component tree as nested calls and threads each one through the reactive
//! graph's owner chain, so depth grows with how deeply the page is nested.
//!
//! Measured on support.cafe, driven headlessly: the home page renders, and
//! following the link to `/login` throws
//!
//!   RuntimeLimitError: reached the maximum number of recursive calls
//!
//! part-way through the render, leaving a fragment of a page. Nothing in the
//! route recurses; it is simply deeper than 512 frames.
//!
//! The error is also not catchable. It unwinds the whole execution rather than
//! arriving as a JavaScript exception, so a page's own error boundary cannot
//! report it and the only trace is a line in the host's log. That is why the
//! depth is measured here in two steps: the recursion runs in one evaluation
//! and the deepest frame it reached is read back in another.

use blitz_script::ScriptDocument;

/// The deepest frame a plain recursive function reaches before the engine
/// stops it.
fn reachable_depth() -> u64 {
    let mut document = ScriptDocument::from_html(
        "<html><body></body></html>",
        blitz_dom::DocumentConfig::default(),
    );
    document.eval(
        "globalThis.__reached = 0;
         function down(n) { globalThis.__reached = n; return down(n + 1); }
         down(1);",
    );
    document
        .eval_json("globalThis.__reached")
        .ok()
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
}

/// The number this exists for.
///
/// A lower bound rather than an equality, so the test says what a page needs
/// rather than restating the constant beside it. 4000 is comfortably past what
/// the fleet's deepest route was measured to want and comfortably under the
/// 8192 configured, which leaves room to tune either limit without rewriting
/// the test.
#[test]
fn a_page_can_nest_calls_as_deeply_as_a_framework_needs() {
    let depth = reachable_depth();
    assert!(
        depth > 4000,
        "a page ran out of call frames at {depth}; Boa's default is 512 and a \
         real application's render is deeper than that, so a route simply \
         stops mid-render with an error its own code cannot catch"
    );
}

/// Still a limit, and still a clean one.
///
/// The point of raising the ceiling is not to remove it. Runaway recursion has
/// to stop with an error rather than by exhausting the machine, and the value
/// stack has to be large enough that the call limit is what stops it: with only
/// the call limit raised, the stack ran out first at 1462 frames and reported
/// "reached the maximum stack size", which names the wrong thing.
#[test]
fn runaway_recursion_still_stops() {
    let depth = reachable_depth();
    assert!(
        depth < 100_000,
        "recursion reached {depth} frames, so nothing is bounding it any more"
    );
}
