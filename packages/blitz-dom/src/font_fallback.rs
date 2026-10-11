//! Embedder-supplied family fallback and helpers for registering compiled-in faces.
//!
//! blitz-dom carries no font bytes or family names here. An embedder that bundles
//! fonts (a headless build without system fonts) sets
//! [`DocumentConfig::family_fallback`](crate::DocumentConfig::family_fallback) to map
//! a CSS family name such as "Arial" to a family it registered. The author name stays
//! first in every query, so a page `@font-face` under that name still wins; the mapped
//! name is appended only when it differs.
//!
//! The mapping is process-wide: the first document created with a fallback sets it,
//! and sub-documents (iframes) created without one use it too.

use std::sync::{Arc, OnceLock};

use parley::fontique::{Blob, FamilyId, FontInfoOverride, GenericFamily, Script};

use crate::FontContext;

/// Maps a CSS family name to the family an embedder registered for it.
pub type FamilyFallback = fn(&str) -> Option<&'static str>;

static FAMILY_FALLBACK: OnceLock<FamilyFallback> = OnceLock::new();

pub(crate) fn set(fallback: FamilyFallback) {
    let _ = FAMILY_FALLBACK.set(fallback);
}

/// The fallback family for `name`, unless it is `name` itself.
pub(crate) fn lookup(name: &str) -> Option<&'static str> {
    let mapped = FAMILY_FALLBACK.get()?(name)?;
    (!mapped.eq_ignore_ascii_case(name)).then_some(mapped)
}

/// Register bytes under the family name stored in the font.
pub fn register_face(ctx: &mut FontContext, bytes: &'static [u8]) {
    ctx.collection
        .register_fonts(Blob::new(Arc::new(bytes) as _), None);
}

/// Register bytes under `family`, keeping the file's weight and style.
///
/// A second call with the same family appends another face. Register regular before
/// bold and italic so an exact weight match is the real face. `None` when the bytes
/// hold no font.
pub fn register_family(
    ctx: &mut FontContext,
    family: &str,
    bytes: &'static [u8],
) -> Option<FamilyId> {
    let registered = ctx.collection.register_fonts(
        Blob::new(Arc::new(bytes) as _),
        Some(FontInfoOverride {
            family_name: Some(family),
            ..FontInfoOverride::default()
        }),
    );
    registered.into_iter().next().map(|(id, _)| id)
}

/// Bind the CSS generics to registered families, each followed by `extra` (script
/// fallbacks such as Devanagari), and append `extra` to the fallbacks of `scripts`.
///
/// A glyph stops at the first family that has it, so `extra` does not change Latin
/// widths; it lets a run in another script resolve when the locale is untracked,
/// since fontique does not fall through from an untracked locale to the default
/// script bucket.
pub fn bind(
    ctx: &mut FontContext,
    sans: FamilyId,
    serif: FamilyId,
    mono: FamilyId,
    extra: &[FamilyId],
    scripts: &[[u8; 4]],
) {
    let with_extra = |primary: FamilyId| std::iter::once(primary).chain(extra.iter().copied());
    for generic in [
        GenericFamily::SansSerif,
        GenericFamily::SystemUi,
        GenericFamily::UiSansSerif,
    ] {
        ctx.collection
            .set_generic_families(generic, with_extra(sans));
    }
    for generic in [GenericFamily::Serif, GenericFamily::UiSerif] {
        ctx.collection
            .set_generic_families(generic, with_extra(serif));
    }
    for generic in [GenericFamily::Monospace, GenericFamily::UiMonospace] {
        ctx.collection
            .set_generic_families(generic, with_extra(mono));
    }
    for script in scripts {
        ctx.collection
            .append_fallbacks(Script::from_bytes(*script), extra.iter().copied());
    }
}
