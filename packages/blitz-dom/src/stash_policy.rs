use std::collections::BTreeSet;

use crate::{BaseDocument, NodeId, QualName};

/// Classification belongs to a document incarnation, including detached nodes.
/// Removing an attribute or changing a password's type cannot revoke it.
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
            || token.eq_ignore_ascii_case("cc-number")
            || token.eq_ignore_ascii_case("cc-csc")
            || token.eq_ignore_ascii_case("cc-exp")
            || token.eq_ignore_ascii_case("cc-exp-month")
            || token.eq_ignore_ascii_case("cc-exp-year")
    })
}

impl BaseDocument {
    pub(crate) fn note_stash_attribute(&mut self, id: NodeId, name: &QualName, value: &str) {
        if !name.ns.as_ref().is_empty() { return; }
        match name.local.as_ref() {
            "data-chuzz-no-stash" => self.stash_policy.no_stash = true,
            "data-chuzz-sensitive" => { self.stash_policy.sensitive.insert(id); }
            "autocomplete" if forbidden_autocomplete(value) => { self.stash_policy.sensitive.insert(id); }
            "type" if ["password", "file", "hidden"].iter().any(|kind| value.eq_ignore_ascii_case(kind)) => {
                self.stash_policy.sensitive.insert(id);
            }
            _ => {}
        }
    }

    /// Called at mutation boundaries, never by a periodic scan.
    pub fn classify_stash(&mut self) {
        let marked: Vec<_> = self.nodes.iter().filter_map(|(id, node)| {
            let element = node.element_data()?;
            let sensitive = element.attrs.iter().any(|attr| {
                attr.name.ns.as_ref().is_empty() && match attr.name.local.as_ref() {
                    "data-chuzz-sensitive" => true,
                    "autocomplete" => forbidden_autocomplete(&attr.value),
                    "type" => ["password", "file", "hidden"].iter().any(|kind| attr.value.eq_ignore_ascii_case(kind)),
                    _ => false,
                }
            });
            let no_stash = element.attrs.iter().any(|attr| attr.name.local.as_ref() == "data-chuzz-no-stash");
            Some((id, sensitive, no_stash))
        }).collect();
        for (id, sensitive, no_stash) in marked {
            self.stash_policy.no_stash |= no_stash;
            if sensitive { self.stash_policy.sensitive.insert(id); }
        }
        let inherited: Vec<_> = self.nodes.iter().filter_map(|(id, _)| self.stash_sensitive(id).then_some(id)).collect();
        self.stash_policy.sensitive.extend(inherited);
    }

    pub fn stash_disabled(&self) -> bool { self.stash_policy.no_stash }

    pub fn stash_sensitive(&self, id: NodeId) -> bool {
        if self.stash_policy.no_stash { return true; }
        let mut cursor = Some(id);
        while let Some(id) = cursor {
            if self.stash_policy.sensitive.contains(&id) { return true; }
            cursor = self.get_node(id).and_then(|node| node.parent);
        }
        let Some(element) = self.get_node(id).and_then(|node| node.element_data()) else { return false; };
        let form = element.attr(crate::local_name!("form"))
            .and_then(|name| self.get_element_by_id(name))
            .or_else(|| self.controls_to_form.get(&id).copied());
        form.is_some_and(|form| self.stash_policy.sensitive.contains(&form))
    }
}
