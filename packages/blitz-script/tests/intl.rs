//! `Intl` is backed by real CLDR data, behind the `intl` feature.
//!
//! Without the feature, `Intl` is simply absent and a page that formats a date
//! throws. With it, Boa carries the locale data that makes the formatting
//! correct for a locale other than the one the test machine happens to be in.
//!
//! Every assertion here compares against a *different* locale's output, because
//! that is what a stub cannot produce. A naive implementation that ignored the
//! locale tag and formatted everything the American way would pass a test that
//! only checked the call returned a non-empty string.

#![cfg(feature = "intl")]

use blitz_script::ScriptDocument;

fn eval_string(doc: &mut ScriptDocument, code: &str) -> String {
    doc.eval(&format!("globalThis.__out = String({code});"));
    doc.eval_json("globalThis.__out")
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn page() -> ScriptDocument {
    ScriptDocument::from_html(
        "<html><body></body></html>",
        blitz_dom::DocumentConfig::default(),
    )
}

/// `Intl` exists at all.
#[test]
fn intl_is_defined() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(eval_string(&mut doc, "typeof Intl"), "object");
}

/// Number grouping and the decimal separator follow the locale.
///
/// German swaps the roles of `.` and `,` against English, so this fails on any
/// implementation that formats without consulting the data.
#[test]
fn number_format_follows_the_locale() {
    let mut doc = page();
    doc.execute_scripts();

    let en = eval_string(&mut doc, "new Intl.NumberFormat('en-US').format(1234567.89)");
    let de = eval_string(&mut doc, "new Intl.NumberFormat('de-DE').format(1234567.89)");

    assert_eq!(en, "1,234,567.89");
    assert_eq!(de, "1.234.567,89");
    assert_ne!(en, de, "the locale tag was ignored");
}

/// Month names come from the locale's own calendar data.
#[test]
fn date_format_uses_localised_month_names() {
    let mut doc = page();
    doc.execute_scripts();

    let out = eval_string(
        &mut doc,
        "new Intl.DateTimeFormat('fr-FR', { month: 'long', timeZone: 'UTC' })\
             .format(new Date(Date.UTC(2020, 0, 15)))",
    );

    assert_eq!(out, "janvier");
}

/// Collation is language-aware, not codepoint order.
///
/// In Swedish `ä` sorts after `z`; in German it sorts with `a`. Nothing but
/// real collation data gets both right.
#[test]
fn collation_is_language_aware() {
    let mut doc = page();
    doc.execute_scripts();

    let sv = eval_string(&mut doc, "new Intl.Collator('sv').compare('ä', 'z')");
    let de = eval_string(&mut doc, "new Intl.Collator('de').compare('ä', 'z')");

    assert_eq!(sv, "1", "Swedish sorts ä after z");
    assert_eq!(de, "-1", "German sorts ä before z");
}

/// Plural categories differ by language.
///
/// Russian puts 2 in `few` where English has only `one` and `other`.
#[test]
fn plural_rules_are_language_specific() {
    let mut doc = page();
    doc.execute_scripts();

    let en = eval_string(&mut doc, "new Intl.PluralRules('en-US').select(2)");
    let ru = eval_string(&mut doc, "new Intl.PluralRules('ru-RU').select(2)");

    assert_eq!(en, "other");
    assert_eq!(ru, "few");
}
