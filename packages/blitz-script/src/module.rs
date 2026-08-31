//! ES module loading for `<script type="module">`.
//!
//! Modules were previously handed to the classic-script evaluator, which is why
//! a page whose entry point is `import { x } from "./x.js"` reported
//!
//! ```text
//! SyntaxError: expected token '.', got '{' in import.meta at line 1, col 7
//! ```
//!
//! Column 7 is the character after `import `. The parser was in classic-script
//! mode, where the only legal continuation is `import.meta` or `import(...)`,
//! so it read `{` as a broken `import.meta`. Nothing was wrong with the page.
//!
//! Fixing the parse alone would not have moved anything: real module entry
//! points import, so the loader has to exist for the first line to run.
//!
//! ## The identity of a module is a URL, not a path
//!
//! Boa addresses modules by [`Path`] — [`Referrer::path`], [`Module::path`],
//! [`Source::with_path`]. The web addresses them by URL. Rather than push URLs
//! through path resolution, where `https://a/b` loses a slash and `..` is
//! resolved by different rules than the URL spec uses, this loader treats the
//! path purely as an opaque key: it is the URL's own string, written in and
//! read back out with no interpretation. Resolution is done by [`Url::join`],
//! which is the spec's algorithm.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

mod import_map;

pub(crate) use import_map::ImportMap;

use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{Context, JsNativeError, JsResult, JsString, Module, Source, js_string};
use url::Url;

use crate::fetch::ScriptFetcher;

/// The fetcher shared between a [`ScriptDocument`](crate::ScriptDocument) and
/// its module loader.
///
/// The two are created at different times: the context (and therefore the
/// loader, which can only be installed at construction) is built inside
/// `ScriptDocument::from_html`, while an embedder installs its fetcher
/// afterwards via `with_fetcher`. A shared cell lets the later call reach the
/// earlier object without making the loader replaceable at runtime.
pub(crate) type SharedFetcher = Rc<RefCell<Rc<dyn ScriptFetcher>>>;

/// Loads ES modules over the document's [`ScriptFetcher`].
pub(crate) struct BlitzModuleLoader {
    fetcher: SharedFetcher,
    base_url: Option<Url>,
    /// Resolved URL -> module.
    ///
    /// Required, not an optimisation: the spec obliges the host to return the
    /// same module record every time it is asked for the same
    /// `(referrer, specifier)` pair. Without it a diamond import would
    /// instantiate the shared dependency twice and each half of the page would
    /// see its own copy of that module's state.
    cache: RefCell<HashMap<String, Module>>,
    /// The page's `<script type="importmap">`, if it has one.
    import_map: RefCell<ImportMap>,
}

impl BlitzModuleLoader {
    pub(crate) fn new(fetcher: SharedFetcher, base_url: Option<Url>) -> Self {
        Self {
            fetcher,
            base_url,
            cache: RefCell::new(HashMap::new()),
            import_map: RefCell::new(ImportMap::default()),
        }
    }

    /// Install the page's import map.
    ///
    /// Only the first one is honoured, as in a browser: a second map arriving
    /// after a module has already resolved a bare specifier could not be
    /// applied retroactively, and applying it only to later imports would give
    /// one name two meanings within a page.
    pub(crate) fn set_import_map(&self, map: ImportMap) {
        let mut installed = self.import_map.borrow_mut();
        if installed.is_empty() {
            *installed = map;
        }
    }

    /// Record a module the document parsed itself, so that an `import` of the
    /// same URL resolves to it rather than fetching and instantiating a second
    /// copy.
    pub(crate) fn register(&self, url: &Url, module: Module) {
        self.cache
            .borrow_mut()
            .insert(url.as_str().to_owned(), module);
    }

    /// The URL a module was loaded from, recovered from the opaque path.
    fn module_url(module_path: Option<&Path>) -> Option<Url> {
        Url::parse(module_path?.to_str()?).ok()
    }

    /// Resolve a specifier the way the web does: against the importing
    /// module's URL, falling back to the document's base URL for a module that
    /// has none (an inline `<script type="module">` in a document parsed from a
    /// string).
    fn resolve(&self, referrer: &Referrer, specifier: &JsString) -> JsResult<Url> {
        let specifier = specifier.to_std_string_escaped();
        let referrer_url = Self::module_url(referrer.path());

        // The import map is consulted first, and for every specifier rather
        // than only bare ones: a map is allowed to remap `/a.js` and `./a.js`
        // too, which is how a page pins or shims a file it does not control.
        if let Some(url) = self
            .import_map
            .borrow()
            .resolve(&specifier, referrer_url.as_ref().or(self.base_url.as_ref()))
        {
            return Ok(url);
        }

        // An absolute specifier needs no base at all, which is also the only
        // form that works when neither the referrer nor the document has a URL.
        if let Ok(url) = Url::parse(&specifier) {
            return Ok(url);
        }

        let base = referrer_url.or_else(|| self.base_url.clone());
        let Some(base) = base else {
            return Err(JsNativeError::typ()
                .with_message(format!(
                    "cannot resolve module specifier {specifier:?}: the importing script has no URL"
                ))
                .into());
        };

        // Bare specifiers ("react") have no meaning outside an import map, and
        // joining one produces a plausible-looking URL that 404s much later
        // with a message naming a path the page never wrote. Say what is
        // actually wrong instead.
        if !specifier.starts_with('/') && !specifier.starts_with('.') {
            return Err(JsNativeError::typ()
                .with_message(format!(
                    "cannot resolve bare module specifier {specifier:?}: no import map entry matches it"
                ))
                .into());
        }

        base.join(&specifier).map_err(|error| {
            JsNativeError::typ()
                .with_message(format!(
                    "cannot resolve module specifier {specifier:?} against {base}: {error}"
                ))
                .into()
        })
    }

    /// Fetch and parse one module. Separated from the async hook so the whole
    /// body is ordinary fallible code.
    fn load(
        &self,
        referrer: &Referrer,
        request: &ModuleRequest,
        context: &mut Context,
    ) -> JsResult<Module> {
        let url = self.resolve(referrer, request.specifier())?;

        if let Some(module) = self.cache.borrow().get(url.as_str()) {
            return Ok(module.clone());
        }

        // Synchronous, on the document thread, exactly as a classic
        // `<script src>` already is. A module graph is therefore as blocking as
        // the deepest import chain: embedders that cannot afford that should
        // prefetch and serve from memory through their own `ScriptFetcher`.
        let fetcher = Rc::clone(&self.fetcher.borrow());
        let source = fetcher.fetch(&url).map_err(|error| {
            JsNativeError::typ().with_message(format!("failed to fetch module {url}: {error}"))
        })?;

        let path = url.as_str().to_owned();
        let module = Module::parse(
            Source::from_bytes(source.as_bytes()).with_path(Path::new(&path)),
            None,
            context,
        )?;

        // Cached before linking, not after: a cycle re-enters this hook for a
        // module still being loaded, and only an already-recorded entry breaks
        // the recursion.
        self.cache.borrow_mut().insert(path, module.clone());
        Ok(module)
    }
}

impl ModuleLoader for BlitzModuleLoader {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl Future<Output = JsResult<Module>> {
        let result = self.load(&referrer, &request, &mut context.borrow_mut());
        async { result }
    }

    /// `import.meta.url`, which bundlers and asset helpers read to locate
    /// files next to themselves (`new URL("./icon.svg", import.meta.url)`).
    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &boa_engine::JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        let url = module
            .path()
            .and_then(Path::to_str)
            .map(str::to_owned)
            .or_else(|| self.base_url.as_ref().map(|base| base.to_string()));

        if let Some(url) = url {
            let _ = import_meta.set(
                js_string!("url"),
                JsString::from(url.as_str()),
                false,
                context,
            );
        }
    }
}
