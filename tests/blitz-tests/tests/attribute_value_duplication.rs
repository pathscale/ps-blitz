//! How much of the DOM's attribute memory is the same string stored again.
//!
//! `vmmap` on a running AgencyZero 0.8.30 instance reports a 2.4G physical
//! footprint with **1.3G resident in `MALLOC_SMALL`** and only 28.6M of
//! empty-but-resident malloc regions. So the memory is live small allocations
//! rather than allocator waste or the GPU pool, and the question is which
//! small allocation there are a million of.
//!
//! `node/attributes.rs` holds `Attribute { name: QualName, value: String }` in
//! a plain `Vec<Attribute>` per element. One separately heap-allocated,
//! separately owned `String` per attribute per element, with no sharing and no
//! copy-on-write. Blink shares identical attribute sets through an
//! `ElementDataCache` (`element_data.h:172`) and says why in a comment: "very
//! common for many elements to have duplicate sets of attributes (ex. the same
//! classes)".
//!
//! Our UI is Tailwind, so a class attribute is long and every row of a list
//! carries a byte-identical copy of it. That is the hypothesis. This test
//! measures it instead of asserting it, on the application's own markup:
//!
//!   cargo test -p blitz-tests --test attribute_value_duplication -- --nocapture
//!
//! It is a **measurement, not a guard**. The assertion at the bottom is
//! deliberately loose, because the number is a property of the fixture and
//! will move whenever the fixture is redumped. What matters is the printed
//! ledger, and the reason it is a test rather than a script is that the
//! harness already knows how to build a real document from real markup.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::collections::HashMap;
use std::sync::Arc;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;

/// Enough copies of the transcript to look like a session rather than a page.
///
/// Duplication is the thing being measured, so a single pane would understate
/// it in exactly the way that matters: the interesting case is the fiftieth
/// list row carrying the same class string as the first. Six panes is what
/// `glass_depth_cost` uses and it keeps the runtime honest.
const REPEATS: usize = 6;

/// What one attribute value costs beyond its bytes.
///
/// A `String` is a 24-byte header (ptr, len, cap) inline in the `Attribute`,
/// plus a heap allocation for the bytes. macOS malloc rounds small requests up
/// to a 16-byte granule, so the heap side of a short string is never cheaper
/// than 16 bytes however few characters it holds. This is used only to turn
/// the byte counts into a plausible footprint; it is not a measurement of the
/// allocator and is marked as an estimate wherever it is printed.
const MALLOC_GRANULE: usize = 16;

fn transcript_document() -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let panes = markup.repeat(REPEATS);
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">
               {panes}
             </div>
           </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// Round a string's heap cost the way the allocator would.
fn heap_cost(value: &str) -> usize {
    value.len().div_ceil(MALLOC_GRANULE) * MALLOC_GRANULE
}

#[test]
fn attribute_values_are_mostly_duplicates() {
    let doc = transcript_document();

    // Keyed by value alone, not by (name, value): the saving from interning is
    // that one `class="flex items-center"` is stored once no matter which
    // attribute or element it appears on.
    let mut occurrences: HashMap<&str, usize> = HashMap::new();
    let mut by_name: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut total_values = 0usize;
    let mut total_bytes = 0usize;
    let mut elements = 0usize;

    for (_, node) in doc.tree().iter() {
        let Some(element) = node.data.downcast_element() else {
            continue;
        };
        elements += 1;
        for attr in element.attrs() {
            let value = attr.value.as_str();
            total_values += 1;
            total_bytes += value.len();
            *occurrences.entry(value).or_default() += 1;
            let entry = by_name.entry(attr.name.local.as_ref()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += value.len();
        }
    }

    let distinct_values = occurrences.len();
    let distinct_bytes: usize = occurrences.keys().map(|value| value.len()).sum();

    // What the current representation pays: every occurrence, header + heap.
    let current = total_values * (size_of::<String>() + 0)
        + occurrences
            .iter()
            .map(|(value, count)| heap_cost(value) * count)
            .sum::<usize>();
    // What interning would pay: one heap copy per distinct value, and a
    // refcounted handle per occurrence. `Arc<str>` is a fat pointer, 16 bytes,
    // against `String`'s 24, so the per-occurrence side shrinks too.
    let interned = total_values * size_of::<*const u8>() * 2
        + occurrences.keys().map(|value| heap_cost(value)).sum::<usize>();

    println!("\n=== attribute value census, {REPEATS} transcript panes ===");
    println!("elements                {elements:>10}");
    println!("attribute values        {total_values:>10}");
    println!("distinct values         {distinct_values:>10}");
    println!(
        "duplication             {:>9.1}x  ({} of every {} occurrences is a repeat)",
        total_values as f64 / distinct_values.max(1) as f64,
        total_values - distinct_values,
        total_values,
    );
    println!("value bytes, all        {total_bytes:>10}");
    println!("value bytes, distinct   {distinct_bytes:>10}");
    println!(
        "bytes saved by sharing  {:>10}  ({:.1}% of value bytes)",
        total_bytes - distinct_bytes,
        (total_bytes - distinct_bytes) as f64 / total_bytes.max(1) as f64 * 100.0,
    );
    println!("\n--- estimated footprint, header + 16-byte-granule heap ---");
    println!("as stored today         {current:>10} bytes");
    println!("interned (Arc<str>)     {interned:>10} bytes");
    println!(
        "estimated saving        {:>10} bytes  ({:.1}%)",
        current.saturating_sub(interned),
        current.saturating_sub(interned) as f64 / current.max(1) as f64 * 100.0,
    );

    let mut worst: Vec<_> = by_name.iter().collect();
    worst.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    println!("\n--- by attribute name, heaviest first ---");
    for (name, (count, bytes)) in worst.iter().take(8) {
        println!("{name:<24} {count:>6} values {bytes:>9} bytes");
    }

    let mut repeated: Vec<_> = occurrences
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(value, count)| (*count * value.len(), *count, *value))
        .collect();
    repeated.sort_by_key(|(wasted, _, _)| std::cmp::Reverse(*wasted));
    println!("\n--- values whose repeats cost the most ---");
    for (bytes, count, value) in repeated.iter().take(6) {
        let shown: String = value.chars().take(64).collect();
        let ellipsis = if value.chars().count() > 64 { "..." } else { "" };
        println!("{count:>5}x {bytes:>8} bytes  {shown}{ellipsis}");
    }
    println!();

    // Loose on purpose. The fixture is a dump of one build; redumping it will
    // move every number above, and a tight bound here would fail for a reason
    // that has nothing to do with the engine. What is being asserted is only
    // that the census ran against a real tree and that duplication exists at
    // all, which is the premise the interning work rests on.
    assert!(
        total_values > 100,
        "census found {total_values} attribute values, which is too few to \
         have walked a real document: check the fixture still parses"
    );
    assert!(
        distinct_values < total_values,
        "no attribute value appears twice in {total_values} values, which \
         would refute the duplication hypothesis entirely"
    );
}
