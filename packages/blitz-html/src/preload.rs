//! Script discovery without native DOM nodes, text storage or resource loads.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::rc::{Rc, Weak};

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, ParseOpts, QualName};

/// Source metadata for a script in the navigation's HTML tree.
///
/// Values retain html5ever's decoded attribute tendrils. Inline script text
/// is not retained. Template contents are excluded.
#[derive(Clone, Debug)]
pub struct ScriptTag {
    pub src: Option<StrTendril>,
    pub script_type: StrTendril,
    pub nomodule: bool,
    pub mount: Option<StrTendril>,
}

impl ScriptTag {
    fn from_attributes(attributes: &[Attribute]) -> Self {
        let attribute = |name: &str| {
            attributes
                .iter()
                .find(|attribute| attribute.name.local.as_ref() == name)
                .map(|attribute| attribute.value.clone())
        };
        Self {
            src: attribute("src"),
            script_type: attribute("type").unwrap_or_default(),
            nomodule: attribute("nomodule").is_some(),
            mount: attribute("mount"),
        }
    }

    /// Match the script types supported by blitz-script's navigation loader.
    ///
    /// Import maps are inline-only and do not name a prefetchable script.
    pub fn is_javascript(&self) -> bool {
        match self.script_type.trim().to_ascii_lowercase().as_str() {
            "module" => true,
            "" | "text/javascript" | "application/javascript" => !self.nomodule,
            _ => false,
        }
    }
}

/// Discover script tags in HTML document order without constructing a Blitz DOM.
///
/// The ordinary html5ever tree builder handles raw text, comments, foreign
/// content, ignored tags and malformed markup. Its sink retains script metadata
/// and ancestry, not a second document. Closed branches without scripts lose
/// their strong handles as the tree builder releases them.
///
/// This function is for HTML. XHTML callers retain their XML parsing path.
pub fn scan_script_tags(html: &str) -> Vec<ScriptTag> {
    let sink = PreloadSink {
        root: PreloadNode::new(None, None),
        scripts: RefCell::new(Vec::new()),
    };
    html5ever::parse_document(
        sink,
        ParseOpts {
            tree_builder: TreeBuilderOpts {
                scripting_enabled: true,
                drop_doctype: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .one(html)
}

struct PreloadNode {
    name: RefCell<Option<QualName>>,
    script: Option<ScriptTag>,
    parent: RefCell<Option<Rc<PreloadNode>>>,
    children: RefCell<Vec<Weak<PreloadNode>>>,
    template_contents: RefCell<Option<Rc<PreloadNode>>>,
}

impl PreloadNode {
    fn new(name: Option<QualName>, script: Option<ScriptTag>) -> Rc<Self> {
        Rc::new(Self {
            name: RefCell::new(name),
            script,
            parent: RefCell::new(None),
            children: RefCell::new(Vec::new()),
            template_contents: RefCell::new(None),
        })
    }

    fn detach(node: &Rc<Self>) {
        let parent = node.parent.borrow_mut().take();
        if let Some(parent) = parent {
            let pointer = Rc::as_ptr(node);
            parent
                .children
                .borrow_mut()
                .retain(|child| child.as_ptr() != pointer);
        }
    }

    fn append(parent: &Rc<Self>, node: Rc<Self>) {
        Self::detach(&node);
        *node.parent.borrow_mut() = Some(Rc::clone(parent));
        parent.children.borrow_mut().push(Rc::downgrade(&node));
    }
}

struct PreloadSink {
    root: Rc<PreloadNode>,
    // Anchor script branches until document order and template exclusion can
    // be determined. Child edges are weak, so unrelated branches are released.
    scripts: RefCell<Vec<Rc<PreloadNode>>>,
}

impl TreeSink for PreloadSink {
    type Output = Vec<ScriptTag>;
    type Handle = Rc<PreloadNode>;
    type ElemName<'a>
        = Ref<'a, QualName>
    where
        Self: 'a;

    fn finish(self) -> Self::Output {
        let mut scripts = Vec::new();
        let mut stack = vec![Rc::clone(&self.root)];
        while let Some(node) = stack.pop() {
            if let Some(script) = &node.script {
                scripts.push(script.clone());
            }
            stack.extend(
                node.children
                    .borrow()
                    .iter()
                    .rev()
                    .filter_map(Weak::upgrade),
            );
        }
        scripts
    }

    fn parse_error(&self, _: Cow<'static, str>) {}

    fn get_document(&self) -> Self::Handle {
        Rc::clone(&self.root)
    }

    fn elem_name<'a>(&'a self, target: &'a Self::Handle) -> Self::ElemName<'a> {
        Ref::map(target.name.borrow(), |name| {
            name.as_ref().expect("preload handle is not an element")
        })
    }

    fn create_element(
        &self,
        name: QualName,
        attributes: Vec<Attribute>,
        _: ElementFlags,
    ) -> Self::Handle {
        let script = (name.local.as_ref() == "script")
            .then(|| ScriptTag::from_attributes(&attributes));
        let node = PreloadNode::new(Some(name), script);
        if node.script.is_some() {
            self.scripts.borrow_mut().push(Rc::clone(&node));
        }
        node
    }

    fn create_comment(&self, _: StrTendril) -> Self::Handle {
        PreloadNode::new(None, None)
    }

    fn create_pi(&self, _: StrTendril, _: StrTendril) -> Self::Handle {
        PreloadNode::new(None, None)
    }

    fn append(&self, parent: &Self::Handle, child: NodeOrText<Self::Handle>) {
        if let NodeOrText::AppendNode(node) = child {
            PreloadNode::append(parent, node);
        }
    }

    fn append_before_sibling(&self, sibling: &Self::Handle, child: NodeOrText<Self::Handle>) {
        let NodeOrText::AppendNode(node) = child else {
            return;
        };
        if Rc::ptr_eq(sibling, &node) {
            return;
        }
        let parent = sibling.parent.borrow().clone();
        let Some(parent) = parent else {
            return;
        };
        PreloadNode::detach(&node);
        let pointer = Rc::as_ptr(sibling);
        let mut children = parent.children.borrow_mut();
        let index = children
            .iter()
            .position(|child| child.as_ptr() == pointer)
            .expect("preload sibling missing from parent");
        *node.parent.borrow_mut() = Some(Rc::clone(&parent));
        children.insert(index, Rc::downgrade(&node));
    }

    fn append_based_on_parent_node(
        &self,
        element: &Self::Handle,
        previous: &Self::Handle,
        child: NodeOrText<Self::Handle>,
    ) {
        let has_parent = element.parent.borrow().is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(previous, child);
        }
    }

    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}

    fn get_template_contents(&self, target: &Self::Handle) -> Self::Handle {
        let mut contents = target.template_contents.borrow_mut();
        Rc::clone(contents.get_or_insert_with(|| PreloadNode::new(None, None)))
    }

    fn same_node(&self, left: &Self::Handle, right: &Self::Handle) -> bool {
        Rc::ptr_eq(left, right)
    }

    fn set_quirks_mode(&self, _: QuirksMode) {}

    fn add_attrs_if_missing(&self, _: &Self::Handle, _: Vec<Attribute>) {
        // Document tree building merges attributes onto html/body, neither
        // of which carries script metadata.
    }

    fn remove_from_parent(&self, target: &Self::Handle) {
        PreloadNode::detach(target);
    }

    fn reparent_children(&self, previous: &Self::Handle, parent: &Self::Handle) {
        let children: Vec<_> = previous
            .children
            .borrow_mut()
            .drain(..)
            .filter_map(|child| child.upgrade())
            .collect();
        for child in children {
            PreloadNode::append(parent, child);
        }
    }
}

