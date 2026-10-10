//! An implementation for Html5ever's sink trait, allowing us to parse HTML into a DOM.

use html5ever::ParseOpts;
use html5ever::tokenizer::TokenizerOpts;
use html5ever::tree_builder::TreeBuilderOpts;
use std::borrow::Cow;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::sync::Arc;

use blitz_dom::node::{Attribute, MarkupNode, NodeFlags};
use blitz_dom::{DocumentMutator, HtmlParserProvider, NodeId};
use html5ever::{
    QualName,
    tendril::{StrTendril, TendrilSink},
    tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink},
};

/// Convert an html5ever Attribute, which uses tendril for its value, to a blitz
/// Attribute, which interns its value.
///
/// This is the highest-volume construction site in the engine: every attribute
/// of every element of every parsed document arrives here. It is therefore
/// where interning pays off most, because a document's repeated `class` strings
/// collapse as the tree is built rather than after it.
fn html5ever_to_blitz_attr(attr: html5ever::Attribute) -> Attribute {
    Attribute {
        name: attr.name,
        value: attr.value.as_ref().into(),
    }
}

#[derive(Copy, Clone, Default, Debug)]
pub struct HtmlProvider;

impl HtmlParserProvider for HtmlProvider {
    fn parse_inner_html<'m2, 'doc2>(
        &self,
        mutr: &'m2 mut DocumentMutator<'doc2>,
        element_id: NodeId,
        html: &str,
    ) {
        DocumentHtmlParser::parse_inner_html_into_mutator(mutr, element_id, html);
    }

    fn parse_document(
        &self,
        html: &str,
        config: blitz_dom::DocumentConfig,
    ) -> Box<dyn blitz_dom::Document> {
        Box::new(crate::HtmlDocument::from_html(html, config))
    }
}

pub struct DocumentHtmlParser<'m, 'doc> {
    document_mutator: RefCell<&'m mut DocumentMutator<'doc>>,

    /// Errors that occurred during parsing.
    pub errors: RefCell<Vec<Cow<'static, str>>>,

    /// The document's quirks mode.
    pub quirks_mode: Cell<QuirksMode>,
    pub is_xml: bool,
    document_id: Option<NodeId>,
}

impl<'m, 'doc> DocumentHtmlParser<'m, 'doc> {
    #[track_caller]
    /// Get a mutable borrow of the DocumentMutator
    fn mutr(&self) -> RefMut<'_, &'m mut DocumentMutator<'doc>> {
        self.document_mutator.borrow_mut()
    }

    fn adopt_created(&self, node_id: NodeId) {
        if let Some(document_id) = self.document_id {
            self.mutr().adopt_node(node_id, document_id);
        }
    }
}

impl<'m, 'doc> DocumentHtmlParser<'m, 'doc> {
    pub fn new(mutr: &'m mut DocumentMutator<'doc>) -> DocumentHtmlParser<'m, 'doc> {
        DocumentHtmlParser {
            document_mutator: RefCell::new(mutr),
            errors: RefCell::new(Vec::new()),
            quirks_mode: Cell::new(QuirksMode::NoQuirks),
            is_xml: false,
            document_id: None,
        }
    }

    /// Parse into a detached document. The caller's MIME type selects the
    /// parser independently of declarations in the source.
    pub fn parse_inert_into_mutator(
        mutr: &mut DocumentMutator<'_>,
        document_id: NodeId,
        source: &str,
        is_xml: bool,
    ) -> Vec<Cow<'static, str>> {
        let mut sink = DocumentHtmlParser::new(mutr);
        sink.document_id = Some(document_id);
        sink.is_xml = is_xml;
        if is_xml {
            let errors = xml5ever::driver::parse_document(sink, Default::default()).one(source);
            Self::repair_xml_prolog(mutr, document_id, source);
            errors
        } else {
            let opts = ParseOpts {
                tokenizer: TokenizerOpts::default(),
                tree_builder: TreeBuilderOpts {
                    exact_errors: false,
                    scripting_enabled: false,
                    iframe_srcdoc: false,
                    drop_doctype: false,
                    quirks_mode: QuirksMode::NoQuirks,
                },
            };
            html5ever::parse_document(sink, opts).one(source)
        }
    }

    /// Undo two xml5ever tokenizer habits that XML documents must not show:
    /// the XML declaration surfaces as a `<?xml ...?>` processing instruction,
    /// and DOCTYPE names are ASCII-lowercased (HTML's rule, not XML's). The
    /// declaration is dropped and the name restored from the source.
    fn repair_xml_prolog(mutr: &mut DocumentMutator<'_>, document_id: NodeId, source: &str) {
        let children: Vec<NodeId> = mutr
            .doc
            .get_node(document_id)
            .map(|node| node.children.to_vec())
            .unwrap_or_default();
        let name = source.find("<!DOCTYPE").map(|start| {
            source[start + "<!DOCTYPE".len()..]
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == '[' || c == '>')
                .next()
                .unwrap_or_default()
                .to_owned()
        });
        for child in children {
            let markup = mutr
                .doc
                .get_node(child)
                .and_then(|node| node.markup.clone());
            match markup.as_deref() {
                Some(MarkupNode::ProcessingInstruction { target }) if target == "xml" => {
                    mutr.remove_node(child);
                }
                Some(MarkupNode::Doctype {
                    name: parsed,
                    public_id,
                    system_id,
                }) => {
                    if let Some(original) = &name
                        && original.eq_ignore_ascii_case(parsed)
                        && original != parsed
                    {
                        let repaired = MarkupNode::Doctype {
                            name: original.clone(),
                            public_id: public_id.clone(),
                            system_id: system_id.clone(),
                        };
                        if let Some(node) = mutr.doc.get_node_mut(child) {
                            node.markup = Some(Arc::new(repaired));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Detects documents without an XML or DOCTYPE declaration whose root `<html>` element
    /// declares the XHTML namespace (e.g. `<html xmlns="http://www.w3.org/1999/xhtml">`)
    fn root_element_has_xhtml_namespace(html: &str) -> bool {
        let rest = html.trim_start_matches('\u{feff}').trim_start();
        let Some(rest) = rest.strip_prefix("<html") else {
            return false;
        };
        let Some(tag_end) = rest.find('>') else {
            return false;
        };
        rest[..tag_end].contains("xmlns=\"http://www.w3.org/1999/xhtml\"")
            || rest[..tag_end].contains("xmlns='http://www.w3.org/1999/xhtml'")
    }

    pub fn parse_into_mutator<'a, 'd>(mutr: &'a mut DocumentMutator<'d>, html: &str) {
        let mut sink = DocumentHtmlParser::new(mutr);

        let is_xhtml_doc = html.starts_with("<?xml")
            || html.starts_with("<!DOCTYPE") && {
                let first_line = html.lines().next().unwrap();
                first_line.contains("XHTML") || first_line.contains("xhtml")
            }
            || Self::root_element_has_xhtml_namespace(html);

        if is_xhtml_doc {
            // Parse as XHTML
            sink.is_xml = true;
            xml5ever::driver::parse_document(sink, Default::default())
                .from_utf8()
                .read_from(&mut html.as_bytes())
                .unwrap();
        } else {
            // Parse as HTML
            sink.is_xml = false;
            let opts = ParseOpts {
                tokenizer: TokenizerOpts::default(),
                tree_builder: TreeBuilderOpts {
                    exact_errors: false,
                    scripting_enabled: false, // Enables parsing of <noscript> tags
                    iframe_srcdoc: false,
                    drop_doctype: true,
                    quirks_mode: QuirksMode::NoQuirks,
                },
            };
            html5ever::parse_document(sink, opts)
                .from_utf8()
                .read_from(&mut html.as_bytes())
                .unwrap();
        }
    }

    pub fn parse_inner_html_into_mutator<'a, 'd>(
        mutr: &'a mut DocumentMutator<'d>,
        element_id: NodeId,
        html: &str,
    ) {
        let sink = DocumentHtmlParser::new(mutr);

        let opts = ParseOpts {
            tokenizer: TokenizerOpts::default(),
            tree_builder: TreeBuilderOpts {
                exact_errors: false,
                scripting_enabled: false, // Enables parsing of <noscript> tags
                iframe_srcdoc: false,
                drop_doctype: true,
                quirks_mode: QuirksMode::NoQuirks,
            },
        };
        html5ever::driver::parse_fragment_for_element(sink, opts, element_id, false, None)
            .from_utf8()
            .read_from(&mut html.as_bytes())
            .unwrap();

        // html5ever creates a new fragment root node under the document node and parses the nodes into that fragment root.
        // So here we move the children of the fragment root to the context's insertion target and then drop the fragment root.
        let document_id = mutr.doc.root_node().id;
        let fragment_root_id = mutr.last_child_id(document_id).unwrap();
        let child_ids = mutr.child_ids(fragment_root_id);
        let target_id = mutr
            .doc
            .get_node(element_id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.template_contents)
            .unwrap_or(element_id);
        mutr.append_children(target_id, &child_ids);
        mutr.remove_and_drop_node(fragment_root_id);
    }
}

impl<'m, 'doc> TreeSink for DocumentHtmlParser<'m, 'doc> {
    type Output = Vec<Cow<'static, str>>;

    // we use the ID of the nodes in the tree as the handle
    type Handle = NodeId;

    type ElemName<'a>
        = Ref<'a, QualName>
    where
        Self: 'a;

    fn finish(self) -> Self::Output {
        #[cfg(feature = "tracing")]
        for error in self.errors.borrow().iter() {
            tracing::error!("{error}");
        }
        self.errors.into_inner()
    }

    fn parse_error(&self, msg: Cow<'static, str>) {
        self.errors.borrow_mut().push(msg);
    }

    fn get_document(&self) -> Self::Handle {
        self.document_id
            .unwrap_or_else(|| self.document_mutator.borrow().doc.root_node().id)
    }

    fn elem_name<'a>(&'a self, target: &'a Self::Handle) -> Self::ElemName<'a> {
        Ref::map(self.document_mutator.borrow(), |docm| {
            docm.element_name(*target)
                .expect("TreeSink::elem_name called on a node which is not an element!")
        })
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<html5ever::Attribute>,
        _flags: ElementFlags,
    ) -> Self::Handle {
        let inert_script = self.document_id.is_some() && name.local.as_ref() == "script";
        let attrs = attrs.into_iter().map(html5ever_to_blitz_attr).collect();
        let id = self.mutr().create_element(name, attrs);
        self.adopt_created(id);
        if inert_script {
            self.mutr()
                .doc
                .get_node_mut(id)
                .expect("new parser element missing")
                .flags
                .insert(NodeFlags::IS_PARSER_INERT_SCRIPT);
        }
        id
    }

    fn create_comment(&self, text: StrTendril) -> Self::Handle {
        let id = self.mutr().create_comment_node(&text);
        self.adopt_created(id);
        id
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Self::Handle {
        if self.document_id.is_none() {
            return self.mutr().create_comment_node("");
        }
        let id = self.mutr().create_comment_node(&data);
        self.adopt_created(id);
        self.mutr()
            .doc
            .get_node_mut(id)
            .expect("new processing instruction missing")
            .markup = Some(Arc::new(MarkupNode::ProcessingInstruction {
            target: target.to_string(),
        }));
        id
    }

    fn append(&self, parent_id: &Self::Handle, child: NodeOrText<Self::Handle>) {
        match child {
            NodeOrText::AppendNode(id) => self.mutr().append_children(*parent_id, &[id]),
            // If content to append is text, first attempt to append it to the last child of parent.
            // Else create a new text node and append it to the parent
            NodeOrText::AppendText(text) => {
                let last_child_id = self.mutr().last_child_id(*parent_id);
                let has_appended = if let Some(id) = last_child_id {
                    self.mutr().append_text_to_node(id, &text).is_ok()
                } else {
                    false
                };
                if !has_appended {
                    let new_child_id = self.mutr().create_text_node(&text);
                    self.adopt_created(new_child_id);
                    self.mutr().append_children(*parent_id, &[new_child_id]);
                }
            }
        }
    }

    // Note: The tree builder promises we won't have a text node after the insertion point.
    // https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_positioned_layout/Stacking_contexts
    fn append_before_sibling(&self, sibling_id: &Self::Handle, new_node: NodeOrText<Self::Handle>) {
        match new_node {
            NodeOrText::AppendNode(id) => self.mutr().insert_nodes_before(*sibling_id, &[id]),
            // If content to append is text, first attempt to append it to the node before sibling_node
            // Else create a new text node and insert it before sibling_node
            NodeOrText::AppendText(text) => {
                let previous_sibling_id = self.mutr().previous_sibling_id(*sibling_id);
                let has_appended = if let Some(id) = previous_sibling_id {
                    self.mutr().append_text_to_node(id, &text).is_ok()
                } else {
                    false
                };
                if !has_appended {
                    let new_child_id = self.mutr().create_text_node(&text);
                    self.adopt_created(new_child_id);
                    self.mutr()
                        .insert_nodes_before(*sibling_id, &[new_child_id]);
                }
            }
        };
    }

    fn append_based_on_parent_node(
        &self,
        element: &Self::Handle,
        prev_element: &Self::Handle,
        child: NodeOrText<Self::Handle>,
    ) {
        if self.mutr().node_has_parent(*element) {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let Some(document_id) = self.document_id else {
            // Preserve the live-document parser's doctype policy.
            return;
        };
        let mut mutr = self.mutr();
        let id = mutr.create_comment_node("");
        mutr.adopt_node(id, document_id);
        mutr.doc
            .get_node_mut(id)
            .expect("new doctype missing")
            .markup = Some(Arc::new(MarkupNode::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        }));
        mutr.append_children(document_id, &[id]);
    }

    fn get_template_contents(&self, target: &Self::Handle) -> Self::Handle {
        self.mutr().template_contents(*target)
    }

    fn same_node(&self, x: &Self::Handle, y: &Self::Handle) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.quirks_mode.set(mode);
    }

    fn add_attrs_if_missing(&self, target: &Self::Handle, attrs: Vec<html5ever::Attribute>) {
        let attrs = attrs.into_iter().map(html5ever_to_blitz_attr).collect();
        self.mutr().add_attrs_if_missing(*target, attrs);
    }

    fn remove_from_parent(&self, target: &Self::Handle) {
        self.mutr().remove_node(*target);
    }

    fn reparent_children(&self, old_parent_id: &Self::Handle, new_parent_id: &Self::Handle) {
        self.mutr()
            .reparent_children(*old_parent_id, *new_parent_id);
    }
}

#[test]
fn parses_some_html() {
    use blitz_dom::{BaseDocument, DocumentConfig};

    let html = "<!DOCTYPE html><html><body><h1>hello world</h1></body></html>";
    let mut doc = BaseDocument::new(DocumentConfig::default());
    let mut mutr = doc.mutate();
    let sink = DocumentHtmlParser::new(&mut mutr);

    html5ever::parse_document(sink, Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap();

    drop(mutr);
    doc.print_tree()

    // Now our tree should have some nodes in it
}
