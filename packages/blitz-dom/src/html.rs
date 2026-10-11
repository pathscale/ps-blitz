use crate::{BaseDocument, Document, DocumentConfig, DocumentMutator, PlainDocument};
use blitz_traits::node_id::NodeId;

pub trait HtmlParserProvider {
    fn parse_inner_html<'m, 'doc>(
        &self,
        mutr: &'m mut DocumentMutator<'doc>,
        element_id: NodeId,
        html: &str,
    );

    /// Parse a fragment. `allow_declarative_shadow_roots` is set for
    /// `setHTMLUnsafe` and clear for `innerHTML`.
    ///
    /// The default ignores the flag and parses the way `innerHTML` does, so a
    /// parser without declarative shadow roots keeps working.
    fn parse_fragment<'m, 'doc>(
        &self,
        mutr: &'m mut DocumentMutator<'doc>,
        element_id: NodeId,
        html: &str,
        allow_declarative_shadow_roots: bool,
    ) {
        let _ = allow_declarative_shadow_roots;
        self.parse_inner_html(mutr, element_id, html);
    }

    /// Parse a full HTML document (e.g. the contents of an `<iframe>`).
    ///
    /// The default implementation ignores the HTML and returns an empty document.
    fn parse_document(&self, html: &str, config: DocumentConfig) -> Box<dyn Document> {
        let _ = html;
        Box::new(PlainDocument(BaseDocument::new(config)))
    }
}

pub struct DummyHtmlParserProvider;
impl HtmlParserProvider for DummyHtmlParserProvider {
    fn parse_inner_html<'m, 'doc>(
        &self,
        mutr: &'m mut DocumentMutator<'doc>,
        element_id: NodeId,
        html: &str,
    ) {
        let _ = mutr;
        let _ = element_id;
        let _ = html;
        // Do nothing for now
        //
        // TODO: do something:
        // - Print warning?
        // - Parse HTML as plain text?
    }
}
