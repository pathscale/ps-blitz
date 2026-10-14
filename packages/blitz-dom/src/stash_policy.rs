use std::collections::BTreeSet;

use crate::{BaseDocument, NodeId, QualName};

/// Incarnation-owned policy. Revoking a marker never revokes its classification.
#[derive(Default)]
pub struct StashPolicy {
    sensitive: BTreeSet<NodeId>,
    no_stash: bool,
}

fn forbidden_autocomplete(value: &str) -> bool {
    value.split_ascii_whitespace().any(|token| {
        token.eq_ignore_ascii_case("off")
            || token.eq_ignore_ascii_case("one-time-code")
            || token.eq_ignore_ascii_case("current-password")
            || token.eq_ignore_ascii_case("new-password")
            || token.eq_ignore_ascii_case("cc-name")
            || token.eq_ignore_ascii_case("cc-given-name")
            || token.eq_ignore_ascii_case("cc-additional-name")
            || token.eq_ignore_ascii_case("cc-family-name")
            || token.eq_ignore_ascii_case("cc-type")
            || token.eq_ignore_ascii_case("cc-number")
            || token.eq_ignore_ascii_case("cc-csc")
            || token.eq_ignore_ascii_case("cc-exp")
            || token.eq_ignore_ascii_case("cc-exp-month")
            || token.eq_ignore_ascii_case("cc-exp-year")
    })
}

impl BaseDocument {
    fn mark_stash_sensitive(&mut self, id: NodeId) {
        if self.stash_policy.no_stash {
            return;
        }
        if self.stash_policy.sensitive.len() >= 4096 {
            self.stash_policy.no_stash = true;
            self.stash_policy.sensitive.clear();
        } else {
            self.stash_policy.sensitive.insert(id);
        }
    }

    pub(crate) fn inherit_stash_classification(&mut self, from: NodeId, to: NodeId) {
        if self.stash_sensitive(from) {
            self.mark_stash_sensitive(to);
        }
    }

    pub(crate) fn note_stash_attribute(&mut self, id: NodeId, name: &QualName, value: &str) {
        if !name.ns.as_ref().is_empty() {
            return;
        }
        match name.local.as_ref() {
            "data-chuzz-no-stash" => self.stash_policy.no_stash = true,
            "data-chuzz-sensitive" => self.mark_stash_sensitive(id),
            "autocomplete" if forbidden_autocomplete(value) => self.mark_stash_sensitive(id),
            "type"
                if ["password", "file", "hidden"]
                    .iter()
                    .any(|kind| value.eq_ignore_ascii_case(kind)) =>
            {
                self.mark_stash_sensitive(id)
            }
            _ => {}
        }
        self.note_stash_subtree(id);
    }

    fn note_stash_node(&mut self, id: NodeId) {
        if self.stash_policy.no_stash {
            return;
        }
        let Some(element) = self.get_node(id).and_then(|node| node.element_data()) else {
            return;
        };
        let sensitive = element.attrs.iter().any(|attr| {
            attr.name.ns.as_ref().is_empty()
                && match attr.name.local.as_ref() {
                    "data-chuzz-sensitive" => true,
                    "autocomplete" => forbidden_autocomplete(&attr.value),
                    "type" => ["password", "file", "hidden"]
                        .iter()
                        .any(|kind| str::eq_ignore_ascii_case(attr.value.as_ref(), kind)),
                    _ => false,
                }
        });
        let no_stash = element.attrs.iter().any(|attr| {
            attr.name.ns.as_ref().is_empty() && attr.name.local.as_ref() == "data-chuzz-no-stash"
        });
        self.stash_policy.no_stash |= no_stash;
        if sensitive || self.stash_sensitive(id) {
            self.mark_stash_sensitive(id);
        }
    }

    /// Mutations visit only the affected subtree and associated form controls.
    pub(crate) fn note_stash_subtree(&mut self, id: NodeId) {
        if self.stash_policy.no_stash {
            return;
        }
        let associated: Vec<_> = self
            .get_node(id)
            .filter(|node| {
                node.element_data()
                    .is_some_and(|element| element.name.local.as_ref() == "form")
            })
            .and_then(|node| node.attr(crate::local_name!("id")))
            .map_or_else(Vec::new, |name| {
                self.nodes
                    .iter()
                    .filter_map(|(control, node)| {
                        (node.attr(crate::local_name!("form")) == Some(name)
                            || self.controls_to_form.get(&control) == Some(&id))
                        .then_some(control)
                    })
                    .collect()
            });
        let mut pending = vec![id];
        pending.extend(associated);
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            self.note_stash_node(id);
            if let Some(node) = self.get_node(id) {
                pending.extend(node.children.iter().copied());
            }
        }
    }

    /// One capture checkpoint also covers native custom-element mutation paths.
    pub fn classify_stash(&mut self) {
        if self.stash_policy.no_stash {
            return;
        }
        let ids: Vec<_> = self.nodes.iter().map(|(id, _)| id).collect();
        for id in ids {
            self.note_stash_node(id);
        }
    }

    pub fn stash_disabled(&self) -> bool {
        self.stash_policy.no_stash
    }

    pub fn stash_sensitive(&self, id: NodeId) -> bool {
        if self.stash_policy.no_stash {
            return true;
        }
        let mut cursor = Some(id);
        while let Some(id) = cursor {
            if self.stash_policy.sensitive.contains(&id) {
                return true;
            }
            cursor = self.get_node(id).and_then(|node| node.parent);
        }
        let Some(element) = self.get_node(id).and_then(|node| node.element_data()) else {
            return false;
        };
        let form = element
            .attr(crate::local_name!("form"))
            .and_then(|name| self.get_element_by_id(name))
            .or_else(|| self.controls_to_form.get(&id).copied());
        form.is_some_and(|form| self.stash_policy.sensitive.contains(&form))
    }
}
