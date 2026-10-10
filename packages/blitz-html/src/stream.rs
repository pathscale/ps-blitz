//! A document tokenizer which releases its DOM borrow at script boundaries.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::rc::Rc;

use blitz_dom::{BaseDocument, NodeId, QualName};
use html5ever::TokenizerResult;
use html5ever::buffer_queue::BufferQueue;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{
    ElementFlags, NodeOrText, QuirksMode, TreeBuilder, TreeBuilderOpts, TreeSink,
};

use crate::DocumentHtmlParser;

pub struct StreamInput(BufferQueue);

impl StreamInput {
    pub fn new(text: &str) -> Self {
        let queue = BufferQueue::default();
        queue.push_back(StrTendril::from(text));
        Self(queue)
    }
}

pub struct StreamingParser {
    tokenizer: Tokenizer<TreeBuilder<NodeId, StreamSink>>,
    input: StreamInput,
}

impl StreamingParser {
    pub fn new(document: Rc<RefCell<BaseDocument>>, html: &str) -> Self {
        let builder = TreeBuilder::new(
            StreamSink { document },
            TreeBuilderOpts {
                scripting_enabled: true,
                drop_doctype: true,
                ..Default::default()
            },
        );
        Self {
            tokenizer: Tokenizer::new(builder, TokenizerOpts::default()),
            input: StreamInput::new(html),
        }
    }

    pub fn next_script(&mut self) -> Option<NodeId> {
        // The input is already decoded, so a <meta charset> indicator only
        // means: keep feeding.
        loop {
            match self.tokenizer.feed(&self.input.0) {
                TokenizerResult::Script(id) => return Some(id),
                TokenizerResult::Done => return None,
                TokenizerResult::EncodingIndicator(_) => {}
            }
        }
    }

    /// Consume inserted input without consuming the navigation's remaining input.
    pub fn feed_written(&mut self, input: &StreamInput) -> Option<NodeId> {
        // The input is already decoded, so a <meta charset> indicator only
        // means: keep feeding.
        loop {
            match self.tokenizer.feed(&input.0) {
                TokenizerResult::Script(id) => return Some(id),
                TokenizerResult::Done => return None,
                TokenizerResult::EncodingIndicator(_) => {}
            }
        }
    }

    pub fn finish(&mut self) {
        self.tokenizer.end();
    }
}

struct StreamSink {
    document: Rc<RefCell<BaseDocument>>,
}

impl StreamSink {
    fn with_sink<R>(&self, callback: impl FnOnce(&DocumentHtmlParser<'_, '_>) -> R) -> R {
        let mut document = self.document.borrow_mut();
        let mut mutator = document.mutate();
        let sink = DocumentHtmlParser::new(&mut mutator);
        callback(&sink)
    }
}

impl TreeSink for StreamSink {
    type Output = ();
    type Handle = NodeId;
    type ElemName<'a>
        = Ref<'a, QualName>
    where
        Self: 'a;

    fn finish(self) {}

    fn parse_error(&self, _: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.document.borrow().root_node().id
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Self::ElemName<'a> {
        Ref::map(self.document.borrow(), |document| {
            &document
                .get_node(*target)
                .expect("parser element disappeared")
                .element_data()
                .expect("parser handle is not an element")
                .name
        })
    }

    fn create_element(
        &self,
        name: QualName,
        attributes: Vec<html5ever::Attribute>,
        flags: ElementFlags,
    ) -> NodeId {
        self.with_sink(|sink| sink.create_element(name, attributes, flags))
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.with_sink(|sink| sink.create_comment(text))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.with_sink(|sink| sink.create_pi(target, data))
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        self.with_sink(|sink| sink.append(parent, child));
    }

    fn append_before_sibling(&self, sibling: &NodeId, child: NodeOrText<NodeId>) {
        self.with_sink(|sink| sink.append_before_sibling(sibling, child));
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        previous: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        self.with_sink(|sink| sink.append_based_on_parent_node(element, previous, child));
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        self.with_sink(|sink| sink.append_doctype_to_document(name, public_id, system_id));
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.with_sink(|sink| sink.get_template_contents(target))
    }

    fn same_node(&self, left: &NodeId, right: &NodeId) -> bool {
        left == right
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.document
            .borrow_mut()
            .set_document_quirks_mode(match mode {
                QuirksMode::NoQuirks => 0,
                QuirksMode::LimitedQuirks => 1,
                QuirksMode::Quirks => 2,
            });
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attributes: Vec<html5ever::Attribute>) {
        self.with_sink(|sink| sink.add_attrs_if_missing(target, attributes));
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.with_sink(|sink| sink.remove_from_parent(target));
    }

    fn reparent_children(&self, previous: &NodeId, parent: &NodeId) {
        self.with_sink(|sink| sink.reparent_children(previous, parent));
    }
}
