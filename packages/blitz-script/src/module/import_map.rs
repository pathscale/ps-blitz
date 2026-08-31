//! `<script type="importmap">`.
//!
//! An import map is how a page that ships unbundled modules names its
//! dependencies: `import { h } from "preact"` is a *bare specifier*, which has
//! no meaning of its own and is only resolvable through the map. Without one,
//! the specifier can only be reported as unresolvable, and the module graph
//! stops at its first dependency.
//!
//! This implements the resolution half of the [import maps specification][spec]:
//! normalisation of the map at parse time, longest-prefix matching for keys
//! ending in `/`, and scopes selected by the *importing* module's URL. It does
//! not implement `integrity`, which is a fetch-time concern.
//!
//! [spec]: https://html.spec.whatwg.org/multipage/webappapis.html#import-maps

use std::cmp::Reverse;

use url::Url;

/// One `imports` object: specifier (or specifier prefix) -> resolved URL.
///
/// Kept sorted by descending key length so a linear scan finds the longest
/// matching prefix first, which is what the spec asks for and what makes
/// `{"a/": ..., "a/b/": ...}` behave.
#[derive(Debug, Default, Clone)]
struct SpecifierMap {
    entries: Vec<(String, Url)>,
}

impl SpecifierMap {
    fn parse(value: Option<&serde_json::Value>, base_url: Option<&Url>) -> Self {
        let mut entries: Vec<(String, Url)> = value
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                if key.is_empty() {
                    return None;
                }
                let target = value.as_str()?;
                // A key ending in `/` maps a whole subtree, so its target must
                // too; the spec drops the entry rather than guessing.
                if key.ends_with('/') != target.ends_with('/') {
                    return None;
                }
                let url = resolve_map_target(target, base_url)?;
                Some((key.clone(), url))
            })
            .collect();

        entries.sort_by_key(|(key, _)| Reverse(key.len()));
        Self { entries }
    }

    fn resolve(&self, specifier: &str) -> Option<Url> {
        for (key, target) in &self.entries {
            if key == specifier {
                return Some(target.clone());
            }
            if key.ends_with('/') && specifier.starts_with(key.as_str()) {
                // The remainder is appended to the target, so `{"lib/": "/js/"}`
                // turns `lib/a/b.js` into `/js/a/b.js`.
                return target.join(&specifier[key.len()..]).ok();
            }
        }
        None
    }
}

/// A page's parsed import map.
#[derive(Debug, Default, Clone)]
pub(crate) struct ImportMap {
    imports: SpecifierMap,
    /// Scope prefix -> the map that applies to modules under it, longest
    /// prefix first. A scope lets one dependency of the page resolve a name
    /// differently from the rest of it, which is how two versions of the same
    /// library coexist.
    scopes: Vec<(String, SpecifierMap)>,
}

impl ImportMap {
    /// Parse the JSON body of a `<script type="importmap">`.
    ///
    /// Malformed maps yield an empty one rather than an error: the failure a
    /// page then sees is "this specifier does not resolve", naming the
    /// specifier, which is more use than a parse error naming a byte offset.
    pub(crate) fn parse(json: &str, base_url: Option<&Url>) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };

        let imports = SpecifierMap::parse(value.get("imports"), base_url);

        let mut scopes: Vec<(String, SpecifierMap)> = value
            .get("scopes")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(prefix, map)| {
                let prefix = resolve_map_target(prefix, base_url)?;
                Some((prefix.to_string(), SpecifierMap::parse(Some(map), base_url)))
            })
            .collect();

        scopes.sort_by_key(|(prefix, _)| Reverse(prefix.len()));

        Self { imports, scopes }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.imports.entries.is_empty() && self.scopes.is_empty()
    }

    /// Resolve `specifier` as imported by `referrer_url`.
    ///
    /// Scopes are consulted first, most specific to least, then the top-level
    /// `imports`. Returns `None` when the map says nothing about the specifier,
    /// which leaves the caller free to fall back to URL resolution.
    pub(crate) fn resolve(&self, specifier: &str, referrer_url: Option<&Url>) -> Option<Url> {
        if let Some(referrer) = referrer_url {
            let referrer = referrer.as_str();
            for (prefix, map) in &self.scopes {
                if referrer.starts_with(prefix.as_str()) {
                    if let Some(url) = map.resolve(specifier) {
                        return Some(url);
                    }
                }
            }
        }
        self.imports.resolve(specifier)
    }
}

/// A map target is either an absolute URL or a path relative to the document.
///
/// The spec restricts relative targets to the three forms below, so that a
/// bare-looking target cannot silently become a second lookup.
fn resolve_map_target(target: &str, base_url: Option<&Url>) -> Option<Url> {
    if let Ok(url) = Url::parse(target) {
        return Some(url);
    }
    if target.starts_with('/') || target.starts_with("./") || target.starts_with("../") {
        // A document parsed from a string has no URL, so a relative target has
        // nothing to be relative to. Dropping the entry keeps the map honest;
        // an absolute-URL map still works in that document.
        return base_url?.join(target).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.invalid/app/index.html").expect("a valid base")
    }

    #[test]
    fn a_bare_specifier_resolves_through_imports() {
        let map = ImportMap::parse(
            r#"{"imports": {"preact": "/vendor/preact.js"}}"#,
            Some(&base()),
        );
        assert_eq!(
            map.resolve("preact", None).map(|url| url.to_string()),
            Some("https://example.invalid/vendor/preact.js".to_owned())
        );
    }

    /// The longest matching prefix wins, not the first one written down.
    #[test]
    fn a_trailing_slash_key_maps_a_subtree_by_longest_prefix() {
        let map = ImportMap::parse(
            r#"{"imports": {"lib/": "/js/", "lib/deep/": "/other/"}}"#,
            Some(&base()),
        );
        assert_eq!(
            map.resolve("lib/a.js", None).map(|url| url.to_string()),
            Some("https://example.invalid/js/a.js".to_owned())
        );
        assert_eq!(
            map.resolve("lib/deep/a.js", None)
                .map(|url| url.to_string()),
            Some("https://example.invalid/other/a.js".to_owned())
        );
    }

    /// The case scopes exist for: one dependency sees a different version of a
    /// shared library from the rest of the page.
    #[test]
    fn a_scope_overrides_the_top_level_map_for_modules_under_it() {
        let map = ImportMap::parse(
            r#"{
                "imports": {"dep": "/v2/dep.js"},
                "scopes": {"/legacy/": {"dep": "/v1/dep.js"}}
            }"#,
            Some(&base()),
        );

        let legacy = Url::parse("https://example.invalid/legacy/old.js").expect("a valid URL");
        let modern = Url::parse("https://example.invalid/modern/new.js").expect("a valid URL");

        assert_eq!(
            map.resolve("dep", Some(&legacy)).map(|url| url.to_string()),
            Some("https://example.invalid/v1/dep.js".to_owned())
        );
        assert_eq!(
            map.resolve("dep", Some(&modern)).map(|url| url.to_string()),
            Some("https://example.invalid/v2/dep.js".to_owned())
        );
    }

    /// A half-slashed entry would turn a subtree mapping into a single-file one
    /// and silently misroute every import under it.
    #[test]
    fn a_mismatched_trailing_slash_is_dropped() {
        let map = ImportMap::parse(r#"{"imports": {"lib/": "/js/bundle.js"}}"#, Some(&base()));
        assert!(map.is_empty());
    }

    /// Nothing is worse here than a map that half-applies, so a broken map
    /// applies not at all.
    #[test]
    fn malformed_json_yields_an_empty_map() {
        assert!(ImportMap::parse("{not json", Some(&base())).is_empty());
    }
}
