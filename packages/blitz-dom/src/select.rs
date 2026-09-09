//! The `<select>` element's list of options and its selectedness.
//!
//! There was no notion of selectedness anywhere in the engine: an `<option>`
//! was a `display: none` node with a `selected` attribute nobody read, so a
//! select could be measured and pressed but could not say what it offered or
//! what was chosen. A QA harness driving worktables.dev's schema designer had
//! nothing to assert against and nothing to change.

use crate::node::SelectData;
use crate::traversal::TreeTraverser;
use crate::{BaseDocument, local_name};
use blitz_traits::node_id::NodeId;

impl BaseDocument {
    /// A select's *list of options*: every descendant `<option>` in tree order.
    ///
    /// A subtree walk rather than a scan of the direct children, because
    /// `<optgroup>` nests them one level down and the flattened order is what
    /// `selectedIndex` counts.
    ///
    /// Recomputed on every call rather than cached on the select. The cached
    /// form would have to be invalidated from every mutation path that can add
    /// or remove an option, and the walk is over a handful of nodes.
    pub fn select_options(&self, select_id: NodeId) -> Vec<NodeId> {
        if !self
            .get_node(select_id)
            .is_some_and(|node| node.data.is_element_with_tag_name(&local_name!("select")))
        {
            return Vec::new();
        }
        TreeTraverser::new_with_root(self, select_id)
            .filter(|id| {
                self.get_node(*id)
                    .is_some_and(|node| node.data.is_element_with_tag_name(&local_name!("option")))
            })
            .collect()
    }

    /// Whether the option is disabled, either in itself or through the
    /// `<optgroup>` containing it. A disabled option cannot be picked and is
    /// skipped by the arrow keys.
    pub fn option_is_disabled(&self, option_id: NodeId) -> bool {
        let mut current = Some(option_id);
        while let Some(id) = current {
            let Some(node) = self.get_node(id) else {
                return false;
            };
            let Some(el) = node.data.downcast_element() else {
                return false;
            };
            if el.attr(local_name!("disabled")).is_some() {
                return true;
            }
            if el.name.local == local_name!("select") {
                // The select's own `disabled` is the control's, not the
                // option's; stop before inheriting it.
                return false;
            }
            current = node.parent;
        }
        false
    }

    /// An option's label: the `label` attribute if it has one, otherwise its
    /// stripped-and-collapsed text. This is what the option shows and what the
    /// accessibility tree reports as its name.
    pub fn option_label(&self, option_id: NodeId) -> String {
        let Some(node) = self.get_node(option_id) else {
            return String::new();
        };
        if let Some(label) = node.data.downcast_element().and_then(|el| {
            el.attr(local_name!("label"))
                .filter(|label| !label.is_empty())
        }) {
            return label.to_string();
        }
        collapse_whitespace(&node.text_content())
    }

    /// An option's submission value: the `value` attribute if present,
    /// otherwise its label. An empty `value=""` is a value, not an absence,
    /// which is why this tests for the attribute rather than for a non-empty
    /// string.
    pub fn option_value(&self, option_id: NodeId) -> String {
        let Some(node) = self.get_node(option_id) else {
            return String::new();
        };
        if let Some(value) = node
            .data
            .downcast_element()
            .and_then(|el| el.attr(local_name!("value")))
        {
            return value.to_string();
        }
        self.option_label(option_id)
    }

    /// The selectedness a freshly constructed select should start with.
    ///
    /// The `selected` content attribute seeds it. A single select then has to
    /// end up with exactly one option selected if it has any enabled ones at
    /// all: HTML's *ask for a reset* step picks the last option carrying the
    /// attribute, or the first non-disabled option when none does. Without that
    /// last part a plain `<select>` with no `selected` anywhere would report an
    /// empty value, which is not what it submits or displays.
    pub fn initial_select_data(&self, select_id: NodeId) -> SelectData {
        let options = self.select_options(select_id);
        let mut selected: Vec<bool> = options
            .iter()
            .map(|id| {
                self.get_node(*id)
                    .and_then(|node| node.data.downcast_element())
                    .is_some_and(|el| el.attr(local_name!("selected")).is_some())
            })
            .collect();

        let multiple = self
            .get_node(select_id)
            .and_then(|node| node.data.downcast_element())
            .is_some_and(|el| el.attr(local_name!("multiple")).is_some());

        if !multiple {
            let last = selected.iter().rposition(|s| *s);
            let chosen = last.or_else(|| {
                options
                    .iter()
                    .position(|id| !self.option_is_disabled(*id))
                    .filter(|_| self.select_display_size(select_id) <= 1)
            });
            for (i, entry) in selected.iter_mut().enumerate() {
                *entry = Some(i) == chosen;
            }
        }

        SelectData::new(selected)
    }

    /// The number of rows the select shows: `size`, or one for a drop-down and
    /// four for a `multiple` list box, which is what browsers settled on.
    ///
    /// Only the "is this a drop-down" question is asked of it here: a list box
    /// (`size` greater than one) does *not* auto-select its first option, while
    /// a drop-down must, because a drop-down always displays something.
    pub fn select_display_size(&self, select_id: NodeId) -> u32 {
        let Some(el) = self
            .get_node(select_id)
            .and_then(|node| node.data.downcast_element())
        else {
            return 1;
        };
        el.attr(local_name!("size"))
            .and_then(|size| size.parse::<u32>().ok())
            .filter(|rows| *rows >= 1)
            .unwrap_or(if el.attr(local_name!("multiple")).is_some() {
                4
            } else {
                1
            })
    }

    /// The node id of the select's currently selected option, if any.
    pub fn select_selected_option(&self, select_id: NodeId) -> Option<NodeId> {
        let data = self
            .get_node(select_id)?
            .data
            .downcast_element()?
            .select_data()?;
        let index = data.selected_index()?;
        self.select_options(select_id).get(index).copied()
    }

    /// A select's value: the value of its first selected option, or the empty
    /// string when nothing is selected. This is `HTMLSelectElement.value`.
    pub fn select_value(&self, select_id: NodeId) -> String {
        self.select_selected_option(select_id)
            .map(|option_id| self.option_value(option_id))
            .unwrap_or_default()
    }

    /// The label of the select's selected option, which is what the control
    /// displays and what the accessibility tree reports as its value.
    pub fn select_label(&self, select_id: NodeId) -> String {
        self.select_selected_option(select_id)
            .map(|option_id| self.option_label(option_id))
            .unwrap_or_default()
    }

    /// `selectedIndex`, or `None` when nothing is selected.
    pub fn select_selected_index(&self, select_id: NodeId) -> Option<usize> {
        self.get_node(select_id)?
            .data
            .downcast_element()?
            .select_data()?
            .selected_index()
    }

    /// Whether the option is selected, according to the parent select's live
    /// state when it has any and the `selected` content attribute before
    /// construction has run.
    pub fn option_is_selected(&self, option_id: NodeId) -> bool {
        match self.option_owner_select(option_id) {
            Some(select_id) => {
                let index = self
                    .select_options(select_id)
                    .iter()
                    .position(|id| *id == option_id);
                match (index, self.select_live_data(select_id)) {
                    (Some(index), Some(data)) => data.is_selected(index),
                    _ => self.option_has_selected_attr(option_id),
                }
            }
            None => self.option_has_selected_attr(option_id),
        }
    }

    /// The `<select>` an option belongs to, walking out through any
    /// `<optgroup>`.
    pub fn option_owner_select(&self, option_id: NodeId) -> Option<NodeId> {
        let mut current = self.get_node(option_id)?.parent;
        while let Some(id) = current {
            let node = self.get_node(id)?;
            if node.data.is_element_with_tag_name(&local_name!("select")) {
                return Some(id);
            }
            current = node.parent;
        }
        None
    }

    fn select_live_data(&self, select_id: NodeId) -> Option<&SelectData> {
        self.get_node(select_id)?
            .data
            .downcast_element()?
            .select_data()
    }

    fn option_has_selected_attr(&self, option_id: NodeId) -> bool {
        self.get_node(option_id)
            .and_then(|node| node.data.downcast_element())
            .is_some_and(|el| el.attr(local_name!("selected")).is_some())
    }

    /// Select the option at `index`, clearing the others on a single select.
    /// Returns whether the selection actually changed, so a caller can decide
    /// whether an `input` event is owed.
    ///
    /// Selecting a disabled option is refused rather than ignored, because the
    /// keyboard handler steps over disabled options and a script that asks for
    /// one directly should not end up with a value the control would never
    /// submit.
    pub fn set_select_selected_index(&mut self, select_id: NodeId, index: usize) -> bool {
        let options = self.select_options(select_id);
        if options
            .get(index)
            .is_none_or(|id| self.option_is_disabled(*id))
        {
            return false;
        }
        let multiple = self
            .get_node(select_id)
            .and_then(|node| node.data.downcast_element())
            .is_some_and(|el| el.attr(local_name!("multiple")).is_some());
        let option_count = options.len();

        let Some(data) = self
            .get_node_mut(select_id)
            .and_then(|node| node.data.downcast_element_mut())
            .and_then(|el| el.select_data_mut())
        else {
            return false;
        };
        // Construction sizes this, but a script can append an <option> and set
        // it selected before the next resolve has run, at which point the entry
        // does not exist yet and the write would be silently dropped.
        data.resize(option_count);
        if multiple {
            data.set_selected(index, true)
        } else {
            data.select_only(index)
        }
    }

    /// The index the arrow keys move to from the current selection, skipping
    /// disabled options. `None` when there is nowhere to go.
    pub fn select_index_step(&self, select_id: NodeId, forwards: bool) -> Option<usize> {
        let options = self.select_options(select_id);
        if options.is_empty() {
            return None;
        }
        let current = self.select_selected_index(select_id);
        let mut index = match current {
            Some(current) => current as isize,
            // With nothing selected, the first press lands on the end the key
            // points away from rather than moving off nowhere.
            None => {
                return options
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| !self.option_is_disabled(**id))
                    .map(|(i, _)| i)
                    .next_back()
                    .filter(|_| !forwards)
                    .or_else(|| {
                        options
                            .iter()
                            .position(|id| !self.option_is_disabled(*id))
                            .filter(|_| forwards)
                    });
            }
        };
        loop {
            index += if forwards { 1 } else { -1 };
            if index < 0 || index as usize >= options.len() {
                return None;
            }
            if !self.option_is_disabled(options[index as usize]) {
                return Some(index as usize);
            }
        }
    }

    /// The first or last selectable option, for Home and End.
    pub fn select_index_edge(&self, select_id: NodeId, last: bool) -> Option<usize> {
        let options = self.select_options(select_id);
        let mut enabled = options
            .iter()
            .enumerate()
            .filter(|(_, id)| !self.option_is_disabled(**id))
            .map(|(i, _)| i);
        if last {
            enabled.next_back()
        } else {
            enabled.next()
        }
    }
}

/// Strip and collapse the way an option's label is rendered. Authors indent
/// their markup, so the raw text of `<option>\n  France\n</option>` is neither
/// what is shown nor what is submitted.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
