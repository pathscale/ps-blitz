//! The three version constants exist, and they are independent.
//!
//! Existence is easy to assert and is not the interesting half. Independence is
//! a property of how the code is written — the moment one module consults
//! another's version constant, a bump to either starts meaning something about
//! both, and the reason there are three constants rather than one evaporates.
//!
//! There is no type-level way to state "this module does not read that
//! constant", so this test reads the source. It is the same technique as the
//! manifest check in `only_serde.rs`: cheap, blunt, and it fails at the moment
//! the discipline lapses rather than at the moment somebody notices.
//!
//! **Doc comments are exempt**, and deliberately: every one of the three
//! constants is documented as being independent of the other two, which means
//! naming them. A rule that forbade the mention would forbid the explanation.

use dom_abi::host::HOST_ABI_VERSION;
use dom_abi::runtime::RUNTIME_ABI_VERSION;
use dom_abi::template::TEMPLATE_FORMAT_VERSION;

const TEMPLATE_SRC: &str = include_str!("../src/template.rs");
const HOST_SRC: &str = include_str!("../src/host.rs");
const RUNTIME_SRC: &str = include_str!("../src/runtime.rs");

const TEMPLATE_CONST: &str = "TEMPLATE_FORMAT_VERSION";
const HOST_CONST: &str = "HOST_ABI_VERSION";
const RUNTIME_CONST: &str = "RUNTIME_ABI_VERSION";

/// Lines that are code: everything that is not a comment or a doc comment.
fn code_lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
}

fn assert_code_never_names(src: &str, module: &str, forbidden: &[&str]) {
    for (number, line) in code_lines(src).enumerate() {
        for name in forbidden {
            assert!(
                !line.contains(name),
                "{module} names {name} in code:\n  line {number}: {line}\n\
                 The three version constants are independent; a module that reads \
                 another's makes a bump to either mean something about both."
            );
        }
    }
}

#[test]
fn each_module_declares_its_own_version_constant() {
    assert!(TEMPLATE_SRC.contains(&format!("pub const {TEMPLATE_CONST}: u32")));
    assert!(HOST_SRC.contains(&format!("pub const {HOST_CONST}: u32")));
    assert!(RUNTIME_SRC.contains(&format!("pub const {RUNTIME_CONST}: u32")));
}

#[test]
fn no_module_consults_another_modules_version() {
    assert_code_never_names(TEMPLATE_SRC, "template.rs", &[HOST_CONST, RUNTIME_CONST]);
    assert_code_never_names(HOST_SRC, "host.rs", &[TEMPLATE_CONST, RUNTIME_CONST]);
    assert_code_never_names(RUNTIME_SRC, "runtime.rs", &[TEMPLATE_CONST, HOST_CONST]);
}

#[test]
fn the_constants_are_readable_and_can_drift_apart() {
    // They happen to be equal today, which is exactly the state in which an
    // accidental coupling would go unnoticed. Assert against the values so that
    // the first one to move does so on purpose.
    assert_eq!(TEMPLATE_FORMAT_VERSION, 1);
    assert_eq!(HOST_ABI_VERSION, 1);
    assert_eq!(RUNTIME_ABI_VERSION, 1);
}

/// The detector, because a check that cannot fail is not a check.
#[test]
fn the_comment_filter_keeps_code_and_drops_documentation() {
    let src = "\
//! HOST_ABI_VERSION in a module doc
/// HOST_ABI_VERSION in a doc comment
// HOST_ABI_VERSION in a plain comment
let checked = HOST_ABI_VERSION;
";
    let code: Vec<&str> = code_lines(src).collect();
    assert_eq!(code, vec!["let checked = HOST_ABI_VERSION;"]);
}
