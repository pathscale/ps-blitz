use crate::HtmlParserProvider;
use blitz_traits::{
    navigation::NavigationProvider,
    net::{AbortSignal, NetProvider},
    shell::{ShellProvider, Viewport},
};
use parley::FontContext;
use std::sync::Arc;
use style::media_queries::MediaType;

/// Strategy for Stylo's style traversal during `resolve`.
///
/// Blitz permits at most one parallel traversal at a time through a
/// process-wide guard. Concurrent `Parallel` resolves of independent
/// documents fall back to sequential traversal when the guard is unavailable.
/// This avoids overlapping parallel traversals borrowing Stylo's worker-local
/// caches: <https://github.com/DioxusLabs/blitz/issues/430>.
///
/// Styling must not reenter Stylo on a thread with an active style context.
/// Other users of Stylo's global pool must coordinate separately.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StyleThreading {
    /// Use Stylo's parallel traversal via its global rayon thread pool when
    /// the traversal guard is available. Otherwise run sequentially on the
    /// calling thread without waiting for the guard.
    Parallel,
    /// Run style traversal sequentially on the calling thread, bypassing
    /// the global pool. This is the default.
    #[default]
    Sequential,
}

/// Options used when constructing a [`BaseDocument`](crate::BaseDocument)
#[derive(Default)]
pub struct DocumentConfig {
    /// The initial `Viewport`
    pub viewport: Option<Viewport>,
    /// The base url which relative URLs are resolved against
    pub base_url: Option<String>,
    /// User Agent stylesheets
    pub ua_stylesheets: Option<Vec<String>>,
    /// Net provider to handle network requests for resources
    pub net_provider: Option<Arc<dyn NetProvider>>,
    /// Navigation provider to handle link clicks and form submissions
    pub navigation_provider: Option<Arc<dyn NavigationProvider>>,
    /// Shell provider to redraw requests, clipboard, etc
    pub shell_provider: Option<Arc<dyn ShellProvider>>,
    /// HTML parser provider. Used to parse HTML for setInnerHTML
    pub html_parser_provider: Option<Arc<dyn HtmlParserProvider>>,
    /// Parley `FontContext`
    pub font_ctx: Option<FontContext>,
    /// Defer system-font discovery until this document first accesses its font context.
    ///
    /// The default is `false`, preserving eager system-font loading behavior.
    /// When no custom font context is supplied, deferred documents share a
    /// process-local system-font template while keeping document registrations isolated.
    pub defer_system_fonts: bool,
    /// The CSS media type used to evaluate `@media` rules.
    /// Defaults to [`MediaType::screen`].
    pub media_type: Option<MediaType>,
    /// Strategy for Stylo's style traversal.
    /// Defaults to [`StyleThreading::Sequential`].
    pub style_threading: StyleThreading,
    /// Whether incremental layout is enabled. Defaults to `true`.
    pub incremental: Option<bool>,
    /// If set, every sub-resource `Request` blitz-dom creates for this
    /// document will carry this signal. Aborting it cancels every in-flight
    /// fetch tied to this document.
    pub abort_signal: Option<AbortSignal>,
    /// Host receipt established before parsing or resource initiation.
    pub teardown: Option<Arc<crate::release::Teardown>>,
    /// How deeply this document is nested within other documents
    /// (0 for a root document). Used to limit `<iframe>` nesting depth.
    pub subdocument_depth: usize,
    /// Maps a CSS family name to a family the embedder registered in `font_ctx`, tried
    /// after the author name. Process-wide: the first document that sets it wins. See
    /// [`font_fallback`](crate::font_fallback).
    pub family_fallback: Option<crate::font_fallback::FamilyFallback>,
}
