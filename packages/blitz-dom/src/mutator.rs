use blitz_traits::node_id::NodeId;
use std::collections::HashSet;
use std::mem;
use std::ops::{Deref, DerefMut};

use crate::document::make_device;
use crate::layout::damage::ALL_DAMAGE;
use crate::net::{ImageHandler, ResourceHandler, StylesheetHandler};
use crate::node::{CanvasData, NodeFlags, SpecialElementData};
use crate::util::ImageType;
use crate::{
    Attribute, BaseDocument, Document, ElementData, Node, NodeData, QualName, local_name, qual_name,
};
use blitz_traits::shell::Viewport;
use markup5ever::ns;
use selectors::matching::ElementSelectorFlags;
use style::Atom;
use style::invalidation::element::restyle_hints::RestyleHint;
use style::stylesheets::OriginSet;
use thin_vec::ThinVec;

macro_rules! tag_and_attr {
    ($tag:tt, $attr:tt) => {
        (&local_name!($tag), &local_name!($attr))
    };
}

#[derive(Debug, Clone)]
pub enum AppendTextErr {
    /// The node is not a text node
    NotTextNode,
}

/// Operations that happen almost immediately, but are deferred within a
/// function for borrow-checker reasons.
enum SpecialOp {
    LoadImage(NodeId),
    LoadIframe(NodeId),
    LoadStylesheet(NodeId),
    UnloadStylesheet(NodeId),
    LoadCustomPaintSource(NodeId),
    ProcessButtonInput(NodeId),
    UnloadSubDocument(NodeId),
    #[cfg(feature = "custom-widget")]
    UnloadCustomWidget(NodeId),
    #[cfg(feature = "shadow-dom")]
    UpgradeCustomElement(NodeId),
    #[cfg(feature = "shadow-dom")]
    DisconnectCustomElement(NodeId),
}

pub struct DocumentMutator<'doc> {
    /// Document is public as an escape hatch, but users of this API should ideally avoid using it
    /// and prefer exposing additional functionality in DocumentMutator.
    pub doc: &'doc mut BaseDocument,

    eager_op_queue: Vec<SpecialOp>,

    // Tracked nodes for deferred processing when mutations have completed
    title_node: Option<NodeId>,
    style_nodes: HashSet<NodeId>,
    form_nodes: HashSet<NodeId>,

    /// Whether an element/attribute that affect animation status has been seen
    recompute_is_animating: bool,

    /// Whether any mutation that affects rendered output has been performed
    mutations_occurred: bool,

    /// Deferred custom-element attribute-change notifications: (host_id, attr
    /// name, old value, new value). Drained and dispatched on flush.
    #[cfg(feature = "shadow-dom")]
    custom_element_attr_changes: Vec<(NodeId, QualName, Option<String>, Option<String>)>,

    /// The (latest) node which has been mounted in and had autofocus=true, if any
    #[cfg(feature = "autofocus")]
    node_to_autofocus: Option<NodeId>,
}

impl Drop for DocumentMutator<'_> {
    fn drop(&mut self) {
        self.flush(); // Defined at bottom of file
        if self.mutations_occurred {
            self.doc.shell_provider.request_redraw();
        }
    }
}

impl DocumentMutator<'_> {
    pub fn new<'doc>(doc: &'doc mut BaseDocument) -> DocumentMutator<'doc> {
        DocumentMutator {
            doc,
            eager_op_queue: Vec::new(),
            title_node: None,
            style_nodes: HashSet::new(),
            form_nodes: HashSet::new(),
            recompute_is_animating: false,
            mutations_occurred: false,
            #[cfg(feature = "shadow-dom")]
            custom_element_attr_changes: Vec::new(),
            #[cfg(feature = "autofocus")]
            node_to_autofocus: None,
        }
    }

    // Query methods

    pub fn node_has_parent(&self, node_id: NodeId) -> bool {
        self.doc.nodes[node_id].parent.is_some()
    }

    pub fn previous_sibling_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].backward(1).map(|node| node.id)
    }

    pub fn next_sibling_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].forward(1).map(|node| node.id)
    }

    pub fn parent_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].parent
    }

    pub fn last_child_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].children.last().copied()
    }

    pub fn child_ids(&self, node_id: NodeId) -> ThinVec<NodeId> {
        self.doc.nodes[node_id].children.clone()
    }

    pub fn element_name(&self, node_id: NodeId) -> Option<&QualName> {
        self.doc.nodes[node_id].element_data().map(|el| &el.name)
    }

    pub fn node_at_path(&self, start_node_id: NodeId, path: &[u8]) -> NodeId {
        let mut current = &self.doc.nodes[start_node_id];
        for i in path {
            let new_id = current.children[*i as usize];
            current = &self.doc.nodes[new_id];
        }
        current.id
    }

    // Node creation methods

    pub fn create_comment_node(&mut self, contents: &str) -> NodeId {
        self.doc.create_node(NodeData::Comment {
            contents: contents.to_string(),
        })
    }

    pub fn create_text_node(&mut self, text: &str) -> NodeId {
        self.doc.create_text_node(text)
    }

    pub fn create_element(&mut self, name: QualName, attrs: Vec<Attribute>) -> NodeId {
        let mut data = ElementData::new(name, attrs);
        data.flush_style_attribute(self.doc.guard(), &self.doc.url.url_extra_data());

        let id = self.doc.create_node(NodeData::Element(Box::new(data)));
        let node = self.doc.get_node_mut(id).unwrap();

        // Initialise style data
        *node.stylo_element_data_mut().ensure_init_mut() = style::data::ElementData {
            damage: ALL_DAMAGE,
            ..Default::default()
        };

        id
    }

    pub fn deep_clone_node(&mut self, node_id: NodeId) -> NodeId {
        self.doc.deep_clone_node(node_id)
    }

    // Node mutation methods

    pub fn set_node_text(&mut self, node_id: NodeId, value: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        let node = &mut self.doc.nodes[node_id];

        // A comment is CharacterData too: `comment.data = "x"` and
        // `comment.nodeValue = "x"` both land here, and until this arm existed
        // they fell through to the `_ => return` below and vanished. The
        // contents were already on the node and simply never written.
        //
        // Deliberately not the Text arm's damage handling. A comment generates
        // no layout box, so `insert_damage(ALL_DAMAGE)` and
        // `mark_ancestors_dirty` would schedule a relayout for a change that
        // cannot affect a pixel, once per write. Nothing rendered depends on
        // this string, so setting it is the whole operation.
        if let NodeData::Comment { ref mut contents } = node.data {
            if contents != value {
                contents.clear();
                contents.push_str(value);
            }
            return;
        }

        let text = match node.data {
            NodeData::Text(ref mut text) => text,
            // TODO: otherwise this is basically element.textContent which is a bit different - need to parse as html
            _ => return,
        };

        let changed = text.content != value;
        if changed {
            self.mutations_occurred |= node_is_in_document;
            text.content.clear();
            text.content.push_str(value);
            node.insert_damage(ALL_DAMAGE);
            // Mark ancestors dirty so the style traversal visits this subtree.
            // Without this, the traversal may skip nodes with pending damage.
            node.mark_ancestors_dirty();
            let parent_id = node.parent;

            // Also insert damage on the parent element, since text content changes
            // affect the parent's layout (text may wrap differently, change size, etc.)
            if let Some(parent_id) = parent_id {
                let parent = &mut self.doc.nodes[parent_id];
                parent.insert_damage(ALL_DAMAGE);
            }

            self.maybe_record_node(parent_id);
        }
    }

    pub fn append_text_to_node(
        &mut self,
        node_id: NodeId,
        text: &str,
    ) -> Result<(), AppendTextErr> {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        let node = &mut self.doc.nodes[node_id];
        node.insert_damage(ALL_DAMAGE);
        node.mark_ancestors_dirty();
        match node.text_data_mut() {
            Some(data) => {
                data.content += text;
                self.mutations_occurred |= node_is_in_document;
                Ok(())
            }
            None => Err(AppendTextErr::NotTextNode),
        }
    }

    pub fn add_attrs_if_missing(&mut self, node_id: NodeId, attrs: Vec<Attribute>) {
        let node = &mut self.doc.nodes[node_id];
        node.insert_damage(ALL_DAMAGE);
        let element_data = node.element_data_mut().expect("Not an element");

        let existing_names = element_data
            .attrs
            .iter()
            .map(|e| e.name.clone())
            .collect::<HashSet<_>>();

        for attr in attrs
            .into_iter()
            .filter(|attr| !existing_names.contains(&attr.name))
        {
            self.set_attribute(node_id, attr.name, &attr.value);
        }
    }

    pub fn set_attribute(&mut self, node_id: NodeId, name: QualName, value: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        if node_is_in_document {
            self.doc.snapshot_node(node_id);

            // Damage is asserted only where an attribute can change what the
            // element renders without changing a computed value.
            //
            // For everything else Stylo calls `compute_layout_damage` with the
            // old and new values during the restyle the hint above asks for,
            // and that answer is the accurate one. Asserting `ALL_DAMAGE`
            // first can only OR it back up to everything, which is what made a
            // colour-only class toggle reconstruct a box: 842us against 295us,
            // and four nodes recomputed where the correct answer is none.
            //
            // The exceptions are real. `<use href>` names a sprite symbol and
            // no computed value moves when it changes, so the cached SVG has to
            // be rebuilt by damage or not at all
            // (`setting_a_use_href_later_rebuilds_the_cached_svg`). Replaced
            // elements are the same story for `src`, `width` and `height`.
            let renders_from_attributes = self.doc.nodes[node_id]
                .data
                .downcast_element()
                .is_some_and(|el| {
                    el.name.ns == ns!(svg)
                        || crate::layout::replaced::is_replaced_element(&el.name.local)
                });

            let node = &mut self.doc.nodes[node_id];
            if let Some(mut data) = node.stylo_element_data_opt_mut().and_then(|s| s.get_mut()) {
                data.hint |= RestyleHint::restyle_subtree();
                if renders_from_attributes {
                    data.damage.insert(ALL_DAMAGE);
                }
            }

            // The parent is restyled only when a selector says it depends on
            // its children.
            //
            // It used to be restyled unconditionally, which meant a class
            // toggle on one row restyled every sibling of that row: on a
            // 40-row list, a colour-only change cost 773us of style against
            // 15us for a frame that changed nothing, while layout recomputed
            // four nodes. Style was half of the whole resolve, for one
            // element's colour.
            //
            // The flags say exactly when the wide hint is needed, because
            // `apply_selector_flags` deposits them on the parent while matching:
            // `:empty` and `:only-child` on the parent, `:nth-child` and the
            // sibling combinators on the siblings, `:has()` through the
            // relative-selector directions. A parent carrying none of them has
            // no rule whose match can change because a child's attribute did.
            let parent = node.parent;
            if let Some(parent_id) = parent {
                let parent = &self.doc.nodes[parent_id];
                let flags = parent.selector_flags().get();
                let child_dependent = ElementSelectorFlags::HAS_SLOW_SELECTOR
                    | ElementSelectorFlags::HAS_SLOW_SELECTOR_LATER_SIBLINGS
                    | ElementSelectorFlags::HAS_EDGE_CHILD_SELECTOR
                    | ElementSelectorFlags::HAS_EMPTY_SELECTOR
                    | ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR
                    | ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING
                    | ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING;

                if flags.intersects(child_dependent) {
                    let parent = &mut self.doc.nodes[parent_id];
                    if let Some(mut data) = parent
                        .stylo_element_data_opt_mut()
                        .and_then(|s| s.get_mut())
                    {
                        data.hint |= RestyleHint::restyle_subtree();
                    }
                }
            }

            // Mark ancestors dirty so the style traversal visits this subtree.
            // Without this, the traversal may skip nodes with pending RestyleHint/damage
            // because it uses dirty_descendants flags to determine which subtrees to visit.
            self.doc.nodes[node_id].mark_ancestors_dirty();
        }

        if name.local == local_name!("id") && node_is_in_document {
            if let Some(old_id) = self.doc.nodes[node_id]
                .element_data()
                .map(|element| element.id.clone())
            {
                if let Some(old_id) = old_id {
                    self.doc.remove_from_id_map(&old_id, node_id);
                }
                self.doc.add_to_id_map(value, node_id);
            }
        }

        let node = &mut self.doc.nodes[node_id];

        let NodeData::Element(ref mut element) = node.data else {
            return;
        };

        self.mutations_occurred |= node_is_in_document;
        // If element is a CustomWidget, then Ccall attribute_changed on it
        #[cfg(feature = "custom-widget")]
        if let SpecialElementData::CustomWidget(widget_data) = &mut element.special_data {
            let old_value = element.attrs.get(&name).as_ref().map(|attr| &*attr.value);
            widget_data
                .widget
                .attribute_changed(&name.local, old_value, Some(value));
        }

        // If element is a CustomElement, defer an attribute_changed notification
        // (it needs mutable document access, so it can't run inline here).
        #[cfg(feature = "shadow-dom")]
        if element.custom_element_data().is_some() {
            let old_value = element
                .attrs
                .get(&name)
                .as_ref()
                .map(|attr| attr.value.to_string());
            self.custom_element_attr_changes.push((
                node_id,
                name.clone(),
                old_value,
                Some(value.to_string()),
            ));
        }

        element.attrs.set(name.clone(), value);

        // Focusability is cached on the element and comes from these
        // attributes, so it has to follow a change to one of them: a widget
        // that hands the focus around its own children - a menu, a grid -
        // sets their tabindex after creating them.
        if name.local == local_name!("tabindex")
            || name.local == local_name!("href")
            || name.local == local_name!("disabled")
        {
            element.flush_is_focussable();
        }

        let tag = &element.name.local;
        let attr = &name.local;

        if *attr == local_name!("id") {
            element.id = Some(Atom::from(value))
        }

        if *attr == local_name!("value") {
            if let Some(input_data) = element.text_input_data_mut() {
                // Update text input value
                input_data.set_text(
                    &mut self.doc.font_ctx.lock().unwrap(),
                    &mut self.doc.layout_ctx,
                    value,
                );
            }
            return;
        }

        if *attr == local_name!("style") {
            element.flush_style_attribute(&self.doc.guard, &self.doc.url.url_extra_data());
            node.mark_style_attr_updated();
            return;
        }

        if *attr == local_name!("disabled") && element.can_be_disabled() {
            node.disable();
            return;
        }

        // If node if not in the document, then don't apply any special behaviours
        // and simply set the attribute value
        if !node.flags.is_in_document() {
            return;
        }

        if (tag, attr) == tag_and_attr!("input", "checked") {
            set_input_checked_state(element, value.to_string());
        } else if (tag, attr) == tag_and_attr!("img", "src") {
            self.load_image(node_id);
        } else if (tag, attr) == tag_and_attr!("canvas", "src") {
            self.load_custom_paint_src(node_id);
        } else if (tag, attr) == tag_and_attr!("link", "href") {
            self.load_linked_stylesheet(node_id);
        } else if (tag, attr) == tag_and_attr!("iframe", "src")
            || (tag, attr) == tag_and_attr!("iframe", "srcdoc")
        {
            self.load_iframe(node_id);
        }
    }

    pub fn clear_attribute(&mut self, node_id: NodeId, name: QualName) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        if node_is_in_document {
            self.doc.snapshot_node(node_id);

            let node = &mut self.doc.nodes[node_id];

            if let Some(mut data) = node.stylo_element_data_opt_mut().and_then(|s| s.get_mut()) {
                data.hint |= RestyleHint::restyle_subtree();
                data.damage.insert(ALL_DAMAGE);
            }

            // Mark ancestors dirty so the style traversal visits this subtree.
            // Without this, the traversal may skip nodes with pending RestyleHint/damage.
            node.mark_ancestors_dirty();
        }

        if name.local == local_name!("id") && node_is_in_document {
            if let Some(old_id) = self.doc.nodes[node_id]
                .element_data()
                .and_then(|element| element.id.clone())
            {
                self.doc.remove_from_id_map(&old_id, node_id);
            }
        }

        let node = &mut self.doc.nodes[node_id];

        let Some(element) = node.element_data_mut() else {
            return;
        };

        let removed_attr = element.attrs.remove(&name);
        let had_attr = removed_attr.is_some();
        if !had_attr {
            return;
        }
        self.mutations_occurred |= node_is_in_document;

        // If element is a CustomWidget, then call attribute_changed on it
        #[cfg(feature = "custom-widget")]
        if let SpecialElementData::CustomWidget(widget_data) = &mut element.special_data {
            let old_value = removed_attr.as_ref().map(|attr| &*attr.value);
            widget_data
                .widget
                .attribute_changed(&name.local, old_value, None);
        }

        // If element is a CustomElement, defer an attribute_changed notification.
        #[cfg(feature = "shadow-dom")]
        if element.custom_element_data().is_some() {
            let old_value = removed_attr.as_ref().map(|attr| attr.value.to_string());
            self.custom_element_attr_changes
                .push((node_id, name.clone(), old_value, None));
        }

        if name.local == local_name!("id") {
            element.id = None;
        }

        // As in `set_attribute`: taking one of these away can make the element
        // unfocusable again.
        if name.local == local_name!("tabindex")
            || name.local == local_name!("href")
            || name.local == local_name!("disabled")
        {
            element.flush_is_focussable();
        }

        // Update text input value
        if name.local == local_name!("value") {
            if let Some(input_data) = element.text_input_data_mut() {
                input_data.set_text(
                    &mut self.doc.font_ctx.lock().unwrap(),
                    &mut self.doc.layout_ctx,
                    "",
                );
            }
        }

        let tag = &element.name.local;
        let attr = &name.local;

        if *attr == local_name!("disabled") && element.can_be_disabled() {
            node.enable();
            return;
        }

        if *attr == local_name!("style") {
            element.flush_style_attribute(&self.doc.guard, &self.doc.url.url_extra_data());
            node.mark_style_attr_updated();
        } else if (tag, attr) == tag_and_attr!("canvas", "src") {
            self.recompute_is_animating = true;
        } else if (tag, attr) == tag_and_attr!("link", "href") {
            self.unload_stylesheet(node_id);
        } else if (tag, attr) == tag_and_attr!("iframe", "srcdoc") && node_is_in_document {
            // Fall back to loading from the `src` attribute (if any)
            self.load_iframe(node_id);
        }
    }

    pub fn set_style_property(&mut self, node_id: NodeId, name: &str, value: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.set_style_property(node_id, name, value);
        self.mutations_occurred |= node_is_in_document;
    }

    pub fn remove_style_property(&mut self, node_id: NodeId, name: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.remove_style_property(node_id, name);
        self.mutations_occurred |= node_is_in_document;
    }

    pub fn set_sub_document(&mut self, node_id: NodeId, sub_document: Box<dyn Document>) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.set_sub_document(node_id, sub_document);
        self.mutations_occurred |= node_is_in_document;
    }

    pub fn remove_sub_document(&mut self, node_id: NodeId) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.remove_sub_document(node_id);
        self.mutations_occurred |= node_is_in_document;
    }

    #[cfg(feature = "custom-widget")]
    pub fn set_custom_widget(&mut self, node_id: NodeId, widget: Box<dyn crate::Widget>) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.set_custom_widget(node_id, widget);
        self.mutations_occurred |= node_is_in_document;
    }

    #[cfg(feature = "custom-widget")]
    pub fn remove_custom_widget(&mut self, node_id: NodeId) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.remove_custom_widget(node_id);
        self.mutations_occurred |= node_is_in_document;
    }

    /// Attach a shadow root to the given host element, returning the shadow
    /// root's node id.
    #[cfg(feature = "shadow-dom")]
    pub fn attach_shadow(&mut self, host_id: NodeId, mode: crate::node::ShadowRootMode) -> NodeId {
        self.doc.attach_shadow(host_id, mode)
    }

    /// Attach a custom element controller to the given node and run its
    /// `connected` lifecycle callback (attaching a shadow root if needed).
    #[cfg(feature = "shadow-dom")]
    pub fn set_custom_element(
        &mut self,
        node_id: NodeId,
        controller: Box<dyn crate::node::CustomElement>,
    ) {
        self.doc.set_custom_element(node_id, controller);
        self.upgrade_custom_element(node_id);
    }

    /// Remove the custom element controller from the given node, running its
    /// `disconnected` callback first.
    #[cfg(feature = "shadow-dom")]
    pub fn remove_custom_element(&mut self, node_id: NodeId) {
        self.disconnect_custom_element(node_id);
        let _ = self.doc.take_custom_element(node_id);
    }

    /// Upgrade an element into a custom element: instantiate a controller from
    /// the registry (if the node does not already have one), attach a shadow
    /// root, and run the `connected` lifecycle callback. No-op if the element is
    /// already upgraded or has no matching definition / controller.
    #[cfg(feature = "shadow-dom")]
    pub(crate) fn upgrade_custom_element(&mut self, node_id: NodeId) {
        use crate::node::{CustomElementData, ShadowRootMode, SpecialElementData};

        let Some(node) = self.doc.get_node(node_id) else {
            return;
        };
        let Some(element) = node.element_data() else {
            return;
        };

        // Determine whether a controller is already attached, and if not, look
        // up a matching registry definition to instantiate one.
        let already_has_controller =
            matches!(element.special_data, SpecialElementData::CustomElement(_));

        let mode = if already_has_controller {
            // Already attached (e.g. via set_custom_element). Default mode.
            ShadowRootMode::Open
        } else {
            let tag = element.name.local.clone();
            let Some(definition) = self.doc.custom_element_registry.get(&tag) else {
                return;
            };
            let mode = definition.mode;
            let controller = (definition.factory)();
            self.doc.nodes[node_id]
                .element_data_mut()
                .unwrap()
                .special_data =
                SpecialElementData::CustomElement(CustomElementData::new(controller));
            self.doc.custom_element_nodes.insert(node_id);
            mode
        };

        // Bail out if already upgraded.
        let is_upgraded = self.doc.nodes[node_id]
            .element_data()
            .and_then(|el| el.custom_element_data())
            .map(|data| data.upgraded)
            .unwrap_or(true);
        if is_upgraded {
            return;
        }

        // Ensure a shadow root is attached.
        let shadow_root_id = self.doc.attach_shadow(node_id, mode);

        // Take the controller out so we can pass `&mut self` (the mutator) to it.
        let Some(mut controller) = self.take_controller(node_id) else {
            return;
        };

        {
            let mut ctx = crate::node::CustomElementCtx {
                mutator: self,
                host_id: node_id,
                shadow_root_id,
            };
            controller.connected(&mut ctx);
        }

        self.restore_controller(node_id, controller, true);
    }

    /// Run the `disconnected` callback for a custom element node.
    #[cfg(feature = "shadow-dom")]
    pub(crate) fn disconnect_custom_element(&mut self, node_id: NodeId) {
        let Some(shadow_root_id) = self
            .doc
            .get_node(node_id)
            .and_then(|node| node.shadow_root_id())
        else {
            // No shadow root: still run disconnected if a controller exists.
            if let Some(mut controller) = self.take_controller(node_id) {
                // Use the host id as a stand-in shadow root id; controllers
                // should guard against missing shadow trees.
                {
                    let mut ctx = crate::node::CustomElementCtx {
                        mutator: self,
                        host_id: node_id,
                        shadow_root_id: node_id,
                    };
                    controller.disconnected(&mut ctx);
                }
                self.restore_controller(node_id, controller, false);
            }
            return;
        };

        if let Some(mut controller) = self.take_controller(node_id) {
            {
                let mut ctx = crate::node::CustomElementCtx {
                    mutator: self,
                    host_id: node_id,
                    shadow_root_id,
                };
                controller.disconnected(&mut ctx);
            }
            self.restore_controller(node_id, controller, false);
        }
    }

    /// Take the custom element controller out of a node, leaving the
    /// `CustomElementData` in place (with `controller == None`).
    #[cfg(feature = "shadow-dom")]
    fn take_controller(&mut self, node_id: NodeId) -> Option<Box<dyn crate::node::CustomElement>> {
        self.doc
            .nodes
            .get_mut(node_id)?
            .element_data_mut()?
            .custom_element_data_mut()?
            .controller
            .take()
    }

    /// Put a controller back into a node's `CustomElementData`, optionally
    /// marking it as upgraded.
    #[cfg(feature = "shadow-dom")]
    fn restore_controller(
        &mut self,
        node_id: NodeId,
        controller: Box<dyn crate::node::CustomElement>,
        upgraded: bool,
    ) {
        if let Some(data) = self
            .doc
            .nodes
            .get_mut(node_id)
            .and_then(|node| node.element_data_mut())
            .and_then(|el| el.custom_element_data_mut())
        {
            data.controller = Some(controller);
            if upgraded {
                data.upgraded = true;
            }
        }
    }

    /// Zero the layout of a node and everything under it.
    fn clear_layout_of_subtree(doc: &mut BaseDocument, node_id: NodeId) {
        let mut stack = vec![node_id];
        while let Some(id) = stack.pop() {
            let Some(node) = doc.nodes.get_mut(id) else {
                continue;
            };
            // The accessors panic on node kinds that have none, so ask the data
            // first rather than every node in the subtree: a text node has no
            // layout of its own and a removal walk hits plenty of them.
            if node.data.downcast_element().is_some() {
                *node.unrounded_layout_mut() = taffy::Layout::with_order(0);
                *node.final_layout_mut() = taffy::Layout::with_order(0);
                node.cache_mut().clear();
            }
            stack.extend(node.children.iter().copied());
        }
    }

    /// Remove the node from its parent but don't drop it.
    pub fn remove_node(&mut self, node_id: NodeId) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        // Process the subtree *before* severing the parent link so that
        // interaction state referencing removed nodes can retarget to the
        // nearest surviving ancestor.
        self.process_removed_subtree(node_id);

        // A detached node keeps its box otherwise, and a box is all layout and
        // paint need: the application's boot splash was removed by its
        // framework the moment the workspace was ready, kept a 1318x880 layout
        // for the rest of the session, and painted its own background over
        // whichever panel it landed on. The page looked blank. The node is
        // deliberately not dropped, so JS wrappers stay valid, but nothing
        // outside the document should occupy space in it.
        Self::clear_layout_of_subtree(self.doc, node_id);

        let node = &mut self.doc.nodes[node_id];

        // Update child_idx values
        if let Some(parent_id) = node.parent.take() {
            self.mutations_occurred |= node_is_in_document;
            let parent = &mut self.doc.nodes[parent_id];
            parent.insert_damage(ALL_DAMAGE);
            // Mark ancestors dirty so the style traversal visits this subtree.
            parent.mark_ancestors_dirty();
            parent.children.retain(|id| *id != node_id);
            self.maybe_record_node(parent_id);
        }
    }

    pub fn remove_and_drop_node(&mut self, node_id: NodeId) -> Option<Node> {
        self.remove_and_drop_node_with(node_id, &mut |_| {})
    }

    /// Like [`Self::remove_and_drop_node`], but calls `on_drop` with the id of
    /// every dropped node (the node itself and all of its descendants).
    pub fn remove_and_drop_node_with(
        &mut self,
        node_id: NodeId,
        on_drop: &mut dyn FnMut(NodeId),
    ) -> Option<Node> {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.process_removed_subtree(node_id);

        let node = self.doc.drop_node_ignoring_parent_with(node_id, on_drop);
        self.mutations_occurred |= node_is_in_document;

        // Update child_idx values
        if let Some(parent_id) = node.as_ref().and_then(|node| node.parent) {
            let parent = &mut self.doc.nodes[parent_id];
            parent.insert_damage(ALL_DAMAGE);
            let parent_is_in_doc = parent.flags.is_in_document();

            // TODO: make this fine grained / conditional based on ElementSelectorFlags
            if parent_is_in_doc {
                if let Some(mut data) = parent
                    .stylo_element_data_opt_mut()
                    .and_then(|s| s.get_mut())
                {
                    data.hint |= RestyleHint::restyle_subtree();
                }
                // Mark ancestors dirty so the style traversal visits this subtree.
                parent.mark_ancestors_dirty();
            }

            parent.children.retain(|id| *id != node_id);
            self.maybe_record_node(parent_id);
        }

        node
    }

    pub fn remove_and_drop_all_children(&mut self, node_id: NodeId) {
        let parent = &mut self.doc.nodes[node_id];
        let parent_is_in_doc = parent.flags.is_in_document();

        // TODO: make this fine grained / conditional based on ElementSelectorFlags
        if parent_is_in_doc {
            if let Some(mut data) = parent
                .stylo_element_data_opt_mut()
                .and_then(|s| s.get_mut())
            {
                data.hint |= RestyleHint::restyle_subtree();
            }
            // Mark ancestors dirty so the style traversal visits this subtree.
            parent.mark_ancestors_dirty();
        }

        let children = mem::take(&mut parent.children);
        self.mutations_occurred |= parent_is_in_doc && !children.is_empty();
        for child_id in children {
            self.process_removed_subtree(child_id);
            let _ = self.doc.drop_node_ignoring_parent(child_id);
        }
        self.maybe_record_node(node_id);
    }

    // Tree mutation methods
    pub fn remove_node_if_unparented(&mut self, node_id: NodeId) {
        self.remove_node_if_unparented_with(node_id, &mut |_| {});
    }

    /// Like [`Self::remove_node_if_unparented`], but calls `on_drop` with the id of
    /// every dropped node (the node itself and all of its descendants).
    pub fn remove_node_if_unparented_with(
        &mut self,
        node_id: NodeId,
        on_drop: &mut dyn FnMut(NodeId),
    ) {
        if let Some(node) = self.doc.get_node(node_id) {
            if node.parent.is_none() {
                self.remove_and_drop_node_with(node_id, on_drop);
            }
        }
    }

    /// Remove all of the children from old_parent_id and append them to new_parent_id
    pub fn append_children(&mut self, parent_id: NodeId, child_ids: &[NodeId]) {
        self.add_children_to_parent(parent_id, child_ids, &|parent, child_ids| {
            parent.children.extend_from_slice(child_ids);
        });
    }

    pub fn insert_nodes_before(&mut self, anchor_node_id: NodeId, new_node_ids: &[NodeId]) {
        let parent_id = self.doc.nodes[anchor_node_id].parent.unwrap();
        self.add_children_to_parent(parent_id, new_node_ids, &|parent, child_ids| {
            let node_child_idx = parent.index_of_child(anchor_node_id).unwrap();
            parent
                .children
                .splice(node_child_idx..node_child_idx, child_ids.iter().copied());
        });
    }

    fn add_children_to_parent(
        &mut self,
        parent_id: NodeId,
        child_ids: &[NodeId],
        insert_children_fn: &dyn Fn(&mut Node, &[NodeId]),
    ) {
        let new_parent_is_in_document = self.doc.nodes[parent_id].flags.is_in_document();
        self.mutations_occurred |= new_parent_is_in_document && !child_ids.is_empty();
        // Detach the children from their old parents *before* inserting them into
        // the new parent (matching DOM `insertBefore` semantics). If a child is
        // being moved within the same parent then detaching it after insertion
        // would remove both the old and the newly-inserted entries from the
        // parent's child list, and anchor indices would be computed against a
        // child list that still contains the moved nodes.
        for child_id in child_ids.iter().copied() {
            let child = &mut self.doc.nodes[child_id];
            let child_was_in_doc = child.flags.is_in_document();
            self.mutations_occurred |= child_was_in_doc;
            let Some(old_parent_id) = child.parent.take() else {
                continue;
            };

            let old_parent = &mut self.doc.nodes[old_parent_id];
            old_parent.insert_damage(ALL_DAMAGE);

            // TODO: make this fine grained / conditional based on ElementSelectorFlags
            if child_was_in_doc {
                if let Some(mut data) = old_parent
                    .stylo_element_data_opt_mut()
                    .and_then(|s| s.get_mut())
                {
                    data.hint |= RestyleHint::restyle_subtree();
                }
                // Mark ancestors dirty so the style traversal visits this subtree.
                old_parent.mark_ancestors_dirty();
            }

            old_parent.children.retain(|id| *id != child_id);
            self.maybe_record_node(old_parent_id);
        }

        let new_parent = &mut self.doc.nodes[parent_id];
        new_parent.insert_damage(ALL_DAMAGE);

        // TODO: make this fine grained / conditional based on ElementSelectorFlags
        if new_parent_is_in_document {
            if let Some(mut data) = new_parent
                .stylo_element_data_opt_mut()
                .and_then(|s| s.get_mut())
            {
                data.hint |= RestyleHint::restyle_subtree();
            }
            // Mark ancestors dirty so the style traversal visits this subtree.
            new_parent.mark_ancestors_dirty();
        }

        insert_children_fn(new_parent, child_ids);

        for child_id in child_ids.iter().copied() {
            let child = &mut self.doc.nodes[child_id];
            let child_was_in_doc = child.flags.is_in_document();
            child.parent = Some(parent_id);

            if new_parent_is_in_document && !child_was_in_doc {
                self.process_added_subtree(child_id);
            } else if !new_parent_is_in_document && child_was_in_doc {
                self.process_removed_subtree(child_id);
            }
        }

        self.maybe_record_node(parent_id);
    }

    // Tree mutation methods (that defer to other methods)
    pub fn insert_nodes_after(&mut self, anchor_node_id: NodeId, new_node_ids: &[NodeId]) {
        match self.next_sibling_id(anchor_node_id) {
            Some(id) => self.insert_nodes_before(id, new_node_ids),
            None => {
                let parent_id = self.parent_id(anchor_node_id).unwrap();
                self.append_children(parent_id, new_node_ids)
            }
        }
    }

    pub fn reparent_children(&mut self, old_parent_id: NodeId, new_parent_id: NodeId) {
        let child_ids = std::mem::take(&mut self.doc.nodes[old_parent_id].children);
        self.maybe_record_node(old_parent_id);
        self.append_children(new_parent_id, &child_ids);
    }

    pub fn replace_node_with(&mut self, anchor_node_id: NodeId, new_node_ids: &[NodeId]) {
        self.insert_nodes_before(anchor_node_id, new_node_ids);
        self.remove_node(anchor_node_id);
    }
}

impl<'doc> DocumentMutator<'doc> {
    pub fn flush(&mut self) {
        if self.recompute_is_animating {
            self.doc.has_canvas = self.doc.compute_has_canvas();
        }

        if let Some(id) = self.title_node {
            let title = self.doc.nodes[id].text_content();
            self.doc.shell_provider.set_window_title(title);
        }

        // Add/Update inline stylesheets (<style> elements)
        for id in self.style_nodes.drain() {
            self.doc.process_style_element(id);
        }

        for id in self.form_nodes.drain() {
            self.doc.reset_form_owner(id);
        }

        #[cfg(feature = "autofocus")]
        if let Some(node_id) = self.node_to_autofocus.take() {
            if self.doc.get_node(node_id).is_some() {
                self.doc.set_focus_to(node_id);
            }
        }

        #[cfg(feature = "shadow-dom")]
        self.dispatch_custom_element_attr_changes();
    }

    /// Dispatch all deferred custom-element `attribute_changed` callbacks.
    #[cfg(feature = "shadow-dom")]
    fn dispatch_custom_element_attr_changes(&mut self) {
        if self.custom_element_attr_changes.is_empty() {
            return;
        }
        let changes = mem::take(&mut self.custom_element_attr_changes);
        for (node_id, name, old_value, new_value) in changes {
            // Skip if the registered definition observes a restricted set that
            // excludes this attribute. Manually-attached controllers (no
            // definition) observe all attributes.
            let tag = self
                .doc
                .get_node(node_id)
                .and_then(|node| node.element_data())
                .map(|el| el.name.local.clone());
            let observed = tag
                .as_ref()
                .and_then(|tag| self.doc.custom_element_registry.get(tag))
                .map(|def| def.observes(&name.local))
                .unwrap_or(true);
            if !observed {
                continue;
            }

            let Some(shadow_root_id) = self
                .doc
                .get_node(node_id)
                .and_then(|node| node.shadow_root_id())
            else {
                continue;
            };
            let Some(mut controller) = self.take_controller(node_id) else {
                continue;
            };
            {
                let mut ctx = crate::node::CustomElementCtx {
                    mutator: self,
                    host_id: node_id,
                    shadow_root_id,
                };
                controller.attribute_changed(
                    &mut ctx,
                    &name.local,
                    old_value.as_deref(),
                    new_value.as_deref(),
                );
            }
            self.restore_controller(node_id, controller, false);
        }
    }

    pub fn set_inner_html(&mut self, node_id: NodeId, html: &str) {
        self.remove_and_drop_all_children(node_id);
        self.doc
            .html_parser_provider
            .clone()
            .parse_inner_html(self, node_id, html);
    }

    fn flush_eager_ops(&mut self) {
        let mut ops = mem::take(&mut self.eager_op_queue);
        for op in ops.drain(0..) {
            match op {
                SpecialOp::LoadImage(node_id) => self.load_image(node_id),
                SpecialOp::LoadIframe(node_id) => self.load_iframe(node_id),
                SpecialOp::LoadStylesheet(node_id) => self.load_linked_stylesheet(node_id),
                SpecialOp::UnloadStylesheet(node_id) => self.unload_stylesheet(node_id),
                SpecialOp::LoadCustomPaintSource(node_id) => self.load_custom_paint_src(node_id),
                SpecialOp::ProcessButtonInput(node_id) => self.process_button_input(node_id),
                SpecialOp::UnloadSubDocument(node_id) => self.remove_sub_document(node_id),
                #[cfg(feature = "custom-widget")]
                SpecialOp::UnloadCustomWidget(node_id) => self.remove_custom_widget(node_id),
                #[cfg(feature = "shadow-dom")]
                SpecialOp::UpgradeCustomElement(node_id) => self.upgrade_custom_element(node_id),
                #[cfg(feature = "shadow-dom")]
                SpecialOp::DisconnectCustomElement(node_id) => {
                    self.disconnect_custom_element(node_id)
                }
            }
        }

        // Queue is empty, but put Vec back anyway so allocation can be reused.
        self.eager_op_queue = ops;
    }

    fn process_added_subtree(&mut self, node_id: NodeId) {
        self.doc.iter_subtree_mut(node_id, |node_id, doc| {
            let node = &mut doc.nodes[node_id];
            node.flags.set(NodeFlags::IS_IN_DOCUMENT, true);
            node.insert_damage(ALL_DAMAGE);

            // If the node has an "id" attribute, store it in the ID map.
            if let Some(id_attr) = node.attr(local_name!("id")).map(ToString::to_string) {
                doc.add_to_id_map(&id_attr, node_id);
            }

            let node = &mut doc.nodes[node_id];
            let NodeData::Element(ref mut element) = node.data else {
                return;
            };

            // Custom post-processing by element tag name
            let tag = element.name.local.as_ref();
            match tag {
                "title" if element.name.ns == ns!(html) => self.title_node = Some(node_id),
                "link" => self.eager_op_queue.push(SpecialOp::LoadStylesheet(node_id)),
                "img" => self.eager_op_queue.push(SpecialOp::LoadImage(node_id)),
                "iframe" => self.eager_op_queue.push(SpecialOp::LoadIframe(node_id)),
                "canvas" => self
                    .eager_op_queue
                    .push(SpecialOp::LoadCustomPaintSource(node_id)),
                "style" => {
                    self.style_nodes.insert(node_id);
                }
                "button" | "fieldset" | "input" | "select" | "textarea" | "object" | "output" => {
                    self.eager_op_queue
                        .push(SpecialOp::ProcessButtonInput(node_id));
                    self.form_nodes.insert(node_id);
                }
                _ => {}
            }

            // If the element's tag name matches a registered custom element
            // definition (and it hasn't already been upgraded), queue it for
            // upgrade.
            #[cfg(feature = "shadow-dom")]
            {
                let needs_upgrade = doc.custom_element_registry.contains(&element.name.local)
                    && element.custom_element_data().is_none();
                if needs_upgrade {
                    self.eager_op_queue
                        .push(SpecialOp::UpgradeCustomElement(node_id));
                }
            }

            // `autofocus` is a boolean attribute: present is true, whatever
            // the value, and absent is the only false. Requiring the literal
            // string "true" meant the one spelling almost nothing uses, since
            // markup writes `<input autofocus>` and the parser stores that as
            // the empty string. Every framework agrees: Solid's boolean
            // attribute setter is `setAttribute(name, "")`.
            //
            // So a field marked autofocus in markup never took focus, and
            // blitz-script papered over its own path by writing "true" from
            // the property setter, which left the parsed path broken.
            #[cfg(feature = "autofocus")]
            if node.is_focussable() {
                if let NodeData::Element(ref element) = node.data {
                    if element.attr(local_name!("autofocus")).is_some() {
                        self.node_to_autofocus = Some(node_id);
                    }
                }
            }
        });

        self.flush_eager_ops();
    }

    fn process_removed_subtree(&mut self, node_id: NodeId) {
        self.doc.iter_subtree_mut(node_id, |node_id, doc| {
            doc.nodes[node_id]
                .flags
                .set(NodeFlags::IS_IN_DOCUMENT, false);

            // Clear any interaction state that references this node, running
            // the usual teardown steps (unhover/unactive the surviving
            // ancestor chain, IME disable on blur of a focused input).
            doc.clear_interaction_state_for_removed_node(node_id);

            let node = &mut doc.nodes[node_id];

            // Same for focus and for the node the last press landed on.
            //
            // These two were missed, and they are the two most likely to point
            // at a node that is being removed: dismissing a panel is a click on
            // a control *inside* it, so that control is both the focused node
            // and the mousedown node at the moment its subtree goes away.
            //
            // A stale id here is not inert. The next click calls `set_focus_to`,
            // which blurs the old node by indexing it, and indexing a dropped
            // id panics inside the event handler. The window then stops
            // responding to clicks until something forces a full rebuild.
            //
            // The upstream fix carried a second failure mode, the blur landing
            // on whatever node had taken the recycled slot. That one cannot
            // happen here: `NodeId` is versioned, so a dropped id resolves to
            // nothing rather than aliasing its successor.
            if doc.focus_node_id == Some(node_id) {
                doc.focus_node_id = None;
            }
            if doc.mousedown_node_id == Some(node_id) {
                doc.mousedown_node_id = None;
            }

            // Clear the text selection if one of its endpoints references this node.
            // This prevents stale selection endpoint references.
            if doc.text_selection.anchor.node_or_parent == Some(node_id)
                || doc.text_selection.focus.node_or_parent == Some(node_id)
            {
                doc.text_selection.clear();
            }

            // Remove any snapshot for this node to prevent stale snapshot references
            // during style invalidation.
            if node.has_snapshot() {
                let opaque_id = style::dom::TNode::opaque(&&*node);
                doc.snapshots.remove(&opaque_id);
                node.set_has_snapshot(false);
            }

            // If the node has an "id" attribute remove it from the ID map.
            if let Some(id_attr) = node.attr(local_name!("id")).map(ToString::to_string) {
                doc.remove_from_id_map(&id_attr, node_id);
            }

            let node = &mut doc.nodes[node_id];
            let NodeData::Element(ref mut element) = node.data else {
                return;
            };

            match &element.special_data {
                SpecialElementData::SubDocument(_) => {
                    self.eager_op_queue
                        .push(SpecialOp::UnloadSubDocument(node_id));
                }
                #[cfg(feature = "custom-widget")]
                SpecialElementData::CustomWidget(_) => {
                    self.eager_op_queue
                        .push(SpecialOp::UnloadCustomWidget(node_id));
                }
                #[cfg(feature = "shadow-dom")]
                SpecialElementData::CustomElement(_) => {
                    self.eager_op_queue
                        .push(SpecialOp::DisconnectCustomElement(node_id));
                }
                SpecialElementData::Stylesheet(_) => self
                    .eager_op_queue
                    .push(SpecialOp::UnloadStylesheet(node_id)),
                SpecialElementData::Image(_) => {}
                SpecialElementData::Canvas(_) => {
                    self.recompute_is_animating = true;
                }
                SpecialElementData::TableRoot(_) => {}
                SpecialElementData::TextInput(_) => {}
                SpecialElementData::CheckboxInput(_) => {}
                #[cfg(feature = "file-input")]
                SpecialElementData::FileInput(_) => {}
                SpecialElementData::None => {}
            }
        });

        self.flush_eager_ops();
    }

    fn maybe_record_node(&mut self, node_id: impl Into<Option<NodeId>>) {
        let Some(node_id) = node_id.into() else {
            return;
        };

        let Some(element) = self.doc.nodes[node_id].data.downcast_element() else {
            return;
        };

        match element.name.local.as_ref() {
            "title" if element.name.ns == ns!(html) => self.title_node = Some(node_id),
            "style" => {
                self.style_nodes.insert(node_id);
            }
            _ => {}
        }
    }

    fn load_linked_stylesheet(&mut self, target_id: NodeId) {
        let node = &self.doc.nodes[target_id];

        let mut is_in_head = false;
        let mut parent_id = node.parent;
        while let Some(id) = parent_id
            && !is_in_head
        {
            let parent = &self.doc.nodes[id];
            is_in_head |= parent.data.is_element_with_tag_name(&local_name!("head"));
            parent_id = parent.parent;
        }

        let rel_attr = node.attr(local_name!("rel"));
        let href_attr = node.attr(local_name!("href"));

        let (Some(rels), Some(href)) = (rel_attr, href_attr) else {
            return;
        };
        if !rels.split_ascii_whitespace().any(|rel| rel == "stylesheet") {
            return;
        }

        let url = self.doc.resolve_url(href);
        let handler = ResourceHandler::new(
            self.doc.tx.clone(),
            self.doc.id(),
            Some(node.id),
            self.doc.shell_provider.clone(),
            StylesheetHandler {
                source_url: url.clone(),
                guard: self.doc.guard.clone(),
                net_provider: self.doc.net_provider.clone(),
                abort_signal: self.doc.abort_signal.clone(),
            },
        );

        if is_in_head && !self.doc.net_provider.is_noop() {
            self.doc
                .pending_critical_resources
                .insert(handler.request_id());
        }

        self.doc.net_provider.fetch(
            self.doc.id(),
            self.doc.build_request(url),
            Box::new(handler),
        );
    }

    fn unload_stylesheet(&mut self, node_id: NodeId) {
        let node = &mut self.doc.nodes[node_id];
        let Some(element) = node.element_data_mut() else {
            unreachable!();
        };
        let SpecialElementData::Stylesheet(stylesheet) = element.special_data.take() else {
            unreachable!();
        };

        let guard = self.doc.guard.read();
        self.doc.stylist.remove_stylesheet(stylesheet, &guard);
        self.doc
            .stylist
            .force_stylesheet_origins_dirty(OriginSet::all());

        self.doc.nodes_to_stylesheet.remove(&node_id);
    }

    fn load_image(&mut self, target_id: NodeId) {
        let node = &self.doc.nodes[target_id];
        if let Some(raw_src) = node.attr(local_name!("src")) {
            if !raw_src.is_empty() {
                let src = self.doc.resolve_url(raw_src);
                let src_string = src.as_str();

                // Check cache first
                if let Some(cached_image) = self.doc.image_cache.get(src_string) {
                    #[cfg(feature = "tracing")]
                    tracing::info!("Loading image {src_string} from cache");
                    let node = &mut self.doc.nodes[target_id];
                    node.element_data_mut().unwrap().special_data =
                        SpecialElementData::Image(Box::new(cached_image.clone()));
                    node.cache_mut().clear();
                    node.insert_damage(ALL_DAMAGE);
                    return;
                }

                // Check if there's already a pending request for this URL
                if let Some(waiting_list) = self.doc.pending_images.get_mut(src_string) {
                    #[cfg(feature = "tracing")]
                    tracing::info!("Image {src_string} already pending, queueing node {target_id}");
                    waiting_list.push((target_id, ImageType::Image));
                    return;
                }

                // Start fetch and track as pending
                #[cfg(feature = "tracing")]
                tracing::info!("Fetching image {src_string}");
                self.doc
                    .pending_images
                    .insert(src_string.to_string(), vec![(target_id, ImageType::Image)]);

                self.doc.net_provider.fetch(
                    self.doc.id(),
                    self.doc.build_request(src),
                    ResourceHandler::boxed(
                        self.doc.tx.clone(),
                        self.doc.id(),
                        None, // Don't pass node_id, we'll handle it via pending_images
                        self.doc.shell_provider.clone(),
                        ImageHandler::new(ImageType::Image),
                    ),
                );
            }
        }
    }

    fn load_iframe(&mut self, target_id: NodeId) {
        if self.doc.subdocument_depth >= crate::iframe::MAX_SUBDOCUMENT_DEPTH {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Not loading iframe: max sub-document nesting depth ({}) reached",
                crate::iframe::MAX_SUBDOCUMENT_DEPTH
            );
            return;
        }

        let node = &self.doc.nodes[target_id];
        let Some(element) = node.element_data() else {
            return;
        };

        // `srcdoc` takes precedence over `src`
        if let Some(srcdoc) = element.attr(local_name!("srcdoc")) {
            let srcdoc = srcdoc.to_string();
            self.doc.load_iframe_srcdoc(target_id, &srcdoc);
            return;
        }

        let Some(raw_src) = element.attr(local_name!("src")) else {
            return;
        };
        if raw_src.is_empty() {
            return;
        }
        let Some(url) = self.doc.url.resolve_relative(raw_src) else {
            #[cfg(feature = "tracing")]
            tracing::warn!("Not loading iframe: could not resolve url {raw_src}");
            return;
        };
        self.doc.start_iframe_load(target_id, url);
    }

    fn load_custom_paint_src(&mut self, target_id: NodeId) {
        let node = &mut self.doc.nodes[target_id];
        if let Some(raw_src) = node.attr(local_name!("src")) {
            if let Ok(custom_paint_source_id) = raw_src.parse::<u64>() {
                self.recompute_is_animating = true;
                let canvas_data = SpecialElementData::Canvas(CanvasData {
                    custom_paint_source_id,
                });
                node.element_data_mut().unwrap().special_data = canvas_data;
            }
        }
    }

    fn process_button_input(&mut self, target_id: NodeId) {
        let node = &self.doc.nodes[target_id];
        let Some(data) = node.element_data() else {
            return;
        };

        let tagname = data.name.local.as_ref();
        let type_attr = data.attr(local_name!("type"));
        let value = data.attr(local_name!("value"));

        // Add content of "value" attribute as a text node child if:
        //   - Tag name is
        if let ("input", Some("button" | "submit" | "reset"), Some(value)) =
            (tagname, type_attr, value)
        {
            let value = value.to_string();
            let id = self.create_text_node(&value);
            self.append_children(target_id, &[id]);
            return;
        }
        #[cfg(feature = "file-input")]
        if let ("input", Some("file")) = (tagname, type_attr) {
            let button_id = self.create_element(
                qual_name!("button", html),
                vec![
                    Attribute {
                        name: qual_name!("type", html),
                        value: "button".to_string(),
                    },
                    Attribute {
                        name: qual_name!("tabindex", html),
                        value: "-1".to_string(),
                    },
                ],
            );
            let label_id = self.create_element(qual_name!("label", html), vec![]);
            let text_id = self.create_text_node("No File Selected");
            let button_text_id = self.create_text_node("Browse");
            self.append_children(target_id, &[button_id, label_id]);
            self.append_children(label_id, &[text_id]);
            self.append_children(button_id, &[button_text_id]);
        }
    }
}

/// Set 'checked' state on an input based on given attributevalue
fn set_input_checked_state(element: &mut ElementData, value: String) {
    let Ok(checked) = value.parse() else {
        return;
    };
    match element.special_data {
        SpecialElementData::CheckboxInput(ref mut checked_mut) => *checked_mut = checked,
        // If we have just constructed the element, set the node attribute,
        // and NodeSpecificData will be created from that later
        // this simulates the checked attribute being set in html,
        // and the element's checked property being set from that
        SpecialElementData::None => element.attrs.push(Attribute {
            name: qual_name!("checked", html),
            value: checked.to_string(),
        }),
        _ => {}
    }
}

/// Type that allows mutable access to the viewport
/// And syncs it back to stylist on drop.
pub struct ViewportMut<'doc> {
    doc: &'doc mut BaseDocument,
    initial_viewport: Viewport,
}
impl ViewportMut<'_> {
    pub fn new(doc: &mut BaseDocument) -> ViewportMut<'_> {
        let initial_viewport = doc.viewport.clone();
        ViewportMut {
            doc,
            initial_viewport,
        }
    }
}
impl Deref for ViewportMut<'_> {
    type Target = Viewport;

    fn deref(&self) -> &Self::Target {
        &self.doc.viewport
    }
}
impl DerefMut for ViewportMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.doc.viewport
    }
}
impl Drop for ViewportMut<'_> {
    fn drop(&mut self) {
        if self.doc.viewport == self.initial_viewport {
            return;
        }

        self.doc.set_stylist_device(make_device(
            &self.doc.viewport,
            self.doc.media_type.clone(),
            self.doc.font_ctx.clone(),
        ));
        self.doc.scroll_viewport_by(0.0, 0.0); // Clamp scroll offset

        let scale_has_changed =
            self.doc.viewport().scale_f64() != self.initial_viewport.scale_f64();
        if scale_has_changed {
            self.doc.invalidate_inline_contexts();
            self.doc.shell_provider.request_redraw();
        }
    }
}

#[cfg(test)]
mod test {
    use style::media_queries::MediaType;
    use style_dom::ElementState;

    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport};

    use crate::{
        Attribute, BaseDocument, DocumentConfig, ElementData, NodeData, NodeId, qual_name,
    };

    #[test]
    fn media_type_defaults_to_screen() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        assert_eq!(*document.media_type(), MediaType::screen());
        assert_eq!(document.stylist_device().media_type(), MediaType::screen());
    }

    #[test]
    fn media_type_honors_config() {
        let mut document = BaseDocument::new(DocumentConfig {
            media_type: Some(MediaType::print()),
            ..Default::default()
        });
        assert_eq!(*document.media_type(), MediaType::print());
        assert_eq!(document.stylist_device().media_type(), MediaType::print());
    }

    #[test]
    fn set_media_type_updates_stylist_device() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        assert_eq!(document.stylist_device().media_type(), MediaType::screen());

        document.set_media_type(MediaType::print());
        assert_eq!(*document.media_type(), MediaType::print());
        assert_eq!(document.stylist_device().media_type(), MediaType::print());
    }

    #[test]
    fn removing_a_node_forgets_it_as_focused_and_pressed() {
        // Dismissing a panel is a click on a control inside it, so at that
        // moment the control is both the focused node and the mousedown node,
        // and then its subtree goes away. Removal used to clear hover, active
        // and the selection endpoints but leave these two, and the next click
        // indexed a dropped id and panicked inside the event handler.
        let mut document = BaseDocument::new(DocumentConfig::default());
        let button = document.create_node(NodeData::Element(Box::new(ElementData::new(
            qual_name!("button"),
            Vec::new(),
        ))));
        let root = document.root_node().id;

        let mut mutator = document.mutate();
        mutator.append_children(root, &[button]);
        drop(mutator);

        document.set_focus_to(button);
        document.set_mousedown_node_id(Some(button));
        assert_eq!(document.get_focussed_node_id(), Some(button));
        assert_eq!(document.mousedown_node_id, Some(button));

        let mut mutator = document.mutate();
        mutator.remove_node(button);
        drop(mutator);

        assert_eq!(
            document.get_focussed_node_id(),
            None,
            "a removed node must not stay focused"
        );
        assert_eq!(
            document.mousedown_node_id, None,
            "a removed node must not stay the pressed node"
        );
    }

    #[test]
    fn mutator_remove_disabled() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let id = document.create_node(NodeData::Element(Box::new(ElementData::new(
            qual_name!("button"),
            vec![Attribute {
                name: qual_name!("disabled"),
                value: "".into(),
            }],
        ))));

        let node = document.get_node(id).unwrap();
        assert!(
            node.element_state().contains(ElementState::DISABLED),
            "form node is disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is not enabled yet"
        );

        let mut mutator = document.mutate();
        mutator.clear_attribute(id, qual_name!("disabled"));
        drop(mutator);

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is no longer disabled"
        );
        assert!(
            node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );
    }

    #[test]
    fn mutator_set_disabled() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let id = document.create_node(NodeData::Element(Box::new(ElementData::new(
            qual_name!("button"),
            vec![],
        ))));

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is not disabled"
        );
        assert!(
            node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );

        let mut mutator = document.mutate();
        mutator.set_attribute(id, qual_name!("disabled"), "");
        drop(mutator);

        let node = document.get_node(id).unwrap();

        assert!(
            node.element_state().contains(ElementState::DISABLED),
            "form node is disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is no longer enabled enabled"
        );
    }

    #[test]
    fn mutator_set_disabled_invalid_node() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let id = document.create_node(NodeData::Element(Box::new(ElementData::new(
            qual_name!("a"),
            vec![],
        ))));

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is not disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );

        let mut mutator = document.mutate();
        mutator.set_attribute(id, qual_name!("disabled"), "");
        drop(mutator);

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is not disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );
    }

    #[test]
    fn mutator_id_attribute_updates_id_map() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id;

        let node_id = {
            let mut mutator = document.mutate();
            let node_id = mutator.create_element(
                qual_name!("div"),
                vec![Attribute {
                    name: qual_name!("id"),
                    value: "old".into(),
                }],
            );
            mutator.append_children(root_id, &[node_id]);
            node_id
        };
        assert_eq!(document.get_element_by_id("old"), Some(node_id));

        {
            let mut mutator = document.mutate();
            mutator.set_attribute(node_id, qual_name!("id"), "new");
        }
        assert_eq!(document.get_element_by_id("new"), Some(node_id));
        assert_eq!(document.get_element_by_id("old"), None);

        {
            let mut mutator = document.mutate();
            mutator.clear_attribute(node_id, qual_name!("id"));
        }
        assert_eq!(document.get_element_by_id("new"), None);
    }

    #[test]
    fn get_element_by_id_duplicate_ids_first_in_tree_order_wins() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id;

        let (first_id, second_id) = {
            let mut mutator = document.mutate();
            let first_id = mutator.create_element(qual_name!("div"), vec![]);
            let second_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.append_children(root_id, &[first_id, second_id]);
            // Assign the id to the later node first so that insertion order
            // differs from tree order
            mutator.set_attribute(second_id, qual_name!("id"), "dup");
            mutator.set_attribute(first_id, qual_name!("id"), "dup");
            (first_id, second_id)
        };
        assert_eq!(document.get_element_by_id("dup"), Some(first_id));

        {
            let mut mutator = document.mutate();
            mutator.remove_node(first_id);
        }
        assert_eq!(document.get_element_by_id("dup"), Some(second_id));
    }

    #[derive(Default)]
    struct RedrawShell {
        redraw_requests: AtomicUsize,
    }

    impl ShellProvider for RedrawShell {
        fn request_redraw(&self) {
            self.redraw_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn mutator_requests_redraw_only_after_mutation() {
        let shell = Arc::new(RedrawShell::default());
        let mut document = BaseDocument::new(DocumentConfig {
            shell_provider: Some(shell.clone()),
            ..Default::default()
        });
        let root_id = document.root_node().id;

        {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("span"), vec![]);
            mutator.append_children(parent_id, &[child_id]);
            mutator.remove_and_drop_all_children(parent_id);
            mutator.set_attribute(parent_id, qual_name!("id"), "detached");
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 0);

        {
            let mutator = document.mutate();
            assert_eq!(mutator.child_ids(root_id).len(), 0);
        }

        {
            let mut mutator = document.mutate();
            let node_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.append_children(root_id, &[node_id]);
            mutator.set_attribute(node_id, qual_name!("id"), "in-document");
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 1);

        {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("span"), vec![]);
            mutator.append_children(root_id, &[parent_id]);
            mutator.append_children(parent_id, &[child_id]);
            mutator.remove_and_drop_all_children(parent_id);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);

        {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("span"), vec![]);
            let detached_target_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.append_children(root_id, &[parent_id]);
            mutator.append_children(parent_id, &[child_id]);
            assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);
            mutator.append_children(detached_target_id, &[child_id]);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn moving_subtree_out_of_document_clears_in_document_flag() {
        let shell = Arc::new(RedrawShell::default());
        let mut document = BaseDocument::new(DocumentConfig {
            shell_provider: Some(shell.clone()),
            ..Default::default()
        });
        let root_id = document.root_node().id;
        let (child_id, grandchild_id, detached_parent_id) = {
            let mut mutator = document.mutate();
            let in_document_parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("div"), vec![]);
            let grandchild_id = mutator.create_element(qual_name!("span"), vec![]);
            let detached_parent_id = mutator.create_element(qual_name!("section"), vec![]);
            mutator.append_children(root_id, &[in_document_parent_id]);
            mutator.append_children(in_document_parent_id, &[child_id]);
            mutator.append_children(child_id, &[grandchild_id]);
            (child_id, grandchild_id, detached_parent_id)
        };
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 1);
        assert!(document.get_node(child_id).unwrap().flags.is_in_document());
        assert!(
            document
                .get_node(grandchild_id)
                .unwrap()
                .flags
                .is_in_document()
        );

        {
            let mut mutator = document.mutate();
            mutator.append_children(detached_parent_id, &[child_id]);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);
        assert!(!document.get_node(child_id).unwrap().flags.is_in_document());
        assert!(
            !document
                .get_node(grandchild_id)
                .unwrap()
                .flags
                .is_in_document()
        );

        {
            let mut mutator = document.mutate();
            mutator.set_attribute(child_id, qual_name!("id"), "detached");
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);

        {
            let mut mutator = document.mutate();
            mutator.append_children(root_id, &[child_id]);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 3);
        assert!(document.get_node(child_id).unwrap().flags.is_in_document());
        assert!(
            document
                .get_node(grandchild_id)
                .unwrap()
                .flags
                .is_in_document()
        );
    }

    /// A `calc()` does not reach taffy as a value. `stylo_taffy` hands it over
    /// as a raw pointer into the node's `ComputedValues`, and layout
    /// dereferences that pointer on every resolve, so the cached taffy style
    /// must never outlive the arc it was built from.
    ///
    /// A restyle that lands no relayout damage still replaces those computed
    /// values. Colour is the cheapest example and it is the real one: a slow
    /// command's response restyled the project header two seconds after boot,
    /// the header's absolutely positioned chip carries
    /// `max-width: calc(100% - 24px)`, and 0.6.x experimental died there in
    /// three different ways depending on what had taken the freed allocation.
    #[test]
    fn a_paint_only_restyle_refreshes_the_calc_the_taffy_style_points_at() {
        use style::servo_arc::Arc as ServoArc;

        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = document.root_node().id;

        let (header_id, chip_id) = {
            let mut mutator = document.mutate();
            let header_id = mutator.create_element(qual_name!("div"), vec![]);
            let chip_id = mutator.create_element(qual_name!("span"), vec![]);
            mutator.set_style_property(header_id, "position", "relative");
            mutator.set_style_property(header_id, "width", "800px");
            mutator.set_style_property(header_id, "height", "60px");
            mutator.set_style_property(chip_id, "position", "absolute");
            mutator.set_style_property(chip_id, "max-width", "calc(100% - 24px)");
            mutator.set_style_property(chip_id, "color", "rgb(1, 2, 3)");
            mutator.append_children(header_id, &[chip_id]);
            mutator.append_children(root_id, &[header_id]);
            (header_id, chip_id)
        };

        document.resolve(0.0);

        // Restyled through inheritance, not directly: the chip's own mutation
        // damage would force a rebuild and hide the hazard. Recolouring the
        // parent recomputes the child's values — a new arc — while the child's
        // own damage stays repaint-only, which is exactly the gap the gate left
        // open.
        {
            let mut mutator = document.mutate();
            mutator.set_style_property(header_id, "color", "rgb(4, 5, 6)");
        }
        document.resolve(0.0);

        let node = document.get_node(chip_id).unwrap();
        let stylo_data = node.stylo_element_data_opt().and_then(|data| data.get());
        let primary = stylo_data
            .as_ref()
            .and_then(|data| data.styles.get_primary())
            .expect("the chip is styled");
        let source = node
            .style_source_opt()
            .expect("a styled node records the computed values its taffy style was built from");

        assert!(
            ServoArc::ptr_eq(primary, source),
            "the cached taffy style still points into computed values that a restyle replaced, \
             so every calc() in it is a dangling pointer",
        );
    }

    #[test]
    fn style_property_updates_nested_layout() {
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = document.root_node().id;

        let mover_id = {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let mover_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.set_style_property(parent_id, "position", "relative");
            mutator.set_style_property(parent_id, "width", "800px");
            mutator.set_style_property(parent_id, "height", "600px");
            mutator.set_style_property(mover_id, "position", "absolute");
            mutator.set_style_property(mover_id, "left", "0px");
            mutator.set_style_property(mover_id, "top", "0px");
            mutator.append_children(parent_id, &[mover_id]);
            mutator.append_children(root_id, &[parent_id]);
            mover_id
        };

        document.resolve(0.0);
        assert_eq!(
            document
                .get_node(mover_id)
                .unwrap()
                .final_layout()
                .location
                .x,
            0.0
        );

        {
            let mut mutator = document.mutate();
            mutator.set_style_property(mover_id, "left", "120px");
        }

        document.resolve(0.0);
        assert_eq!(
            document
                .get_node(mover_id)
                .unwrap()
                .final_layout()
                .location
                .x,
            120.0
        );
    }

    /// `<html><body><div>text<!--comment--></div></body></html>`, laid out
    /// once, returning the text and comment ids.
    fn doc_with_a_comment() -> (BaseDocument, NodeId, NodeId, NodeId) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = doc.root_node().id;

        let mut mutr = doc.mutate();
        let html = mutr.create_element(qual_name!("html"), vec![]);
        let body = mutr.create_element(qual_name!("body"), vec![]);
        let container = mutr.create_element(qual_name!("div"), vec![]);
        let text = mutr.create_text_node("text");
        let comment = mutr.create_comment_node("comment");
        mutr.append_children(container, &[text, comment]);
        mutr.append_children(body, &[container]);
        mutr.append_children(html, &[body]);
        mutr.append_children(root_id, &[html]);
        drop(mutr);

        doc.resolve(0.0);
        (doc, container, text, comment)
    }

    /// A comment is CharacterData: `comment.data = "x"` has to land somewhere.
    /// Before this arm existed it fell through and vanished, so a getter that
    /// returned the contents would have disagreed with every write.
    #[test]
    fn setting_a_comments_data_writes_the_contents() {
        let (mut doc, _container, _text, comment) = doc_with_a_comment();

        doc.mutate().set_node_text(comment, "rewritten");

        let NodeData::Comment { contents } = &doc.get_node(comment).unwrap().data else {
            panic!("expected a comment node");
        };
        assert_eq!(contents, "rewritten");
    }

    /// A comment generates no box, so writing its data must not schedule a
    /// relayout. Without this the obvious implementation (copy the Text arm)
    /// costs a full resolve per write, and nothing observable would say so.
    ///
    /// The text-node write at the end is the control: it proves the assertion
    /// above is capable of failing.
    #[test]
    fn setting_a_comments_data_does_not_dirty_layout() {
        let (mut doc, container, text, comment) = doc_with_a_comment();

        let container_damage_before = doc.get_node(container).unwrap().damage();
        let comment_damage_before = doc.get_node(comment).unwrap().damage();

        doc.mutate().set_node_text(comment, "rewritten");

        assert_eq!(
            doc.get_node(comment).unwrap().damage(),
            comment_damage_before,
            "writing a comment's data damaged the comment"
        );
        assert_eq!(
            doc.get_node(container).unwrap().damage(),
            container_damage_before,
            "writing a comment's data damaged its parent, scheduling a relayout \
             for a change that cannot affect a pixel"
        );

        doc.mutate().set_node_text(text, "rewritten");
        assert_ne!(
            doc.get_node(container).unwrap().damage(),
            container_damage_before,
            "a text write should damage the parent, so the assertions above can fail"
        );
    }

    /// Writing the same contents back is not a change, and must stay as inert
    /// as a write of different contents.
    #[test]
    fn rewriting_a_comment_with_its_own_contents_is_inert() {
        let (mut doc, container, _text, comment) = doc_with_a_comment();
        let container_damage_before = doc.get_node(container).unwrap().damage();

        doc.mutate().set_node_text(comment, "comment");

        let NodeData::Comment { contents } = &doc.get_node(comment).unwrap().data else {
            panic!("expected a comment node");
        };
        assert_eq!(contents, "comment");
        assert_eq!(
            doc.get_node(container).unwrap().damage(),
            container_damage_before
        );
    }
}
