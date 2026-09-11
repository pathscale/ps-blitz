//! Live state for a `<select>` element.

/// The selectedness of a `<select>` element's list of options, plus whether its
/// drop-down is showing.
///
/// Positions, not node ids. `selectedIndex` is the spec's own handle on an
/// option, and a position cannot dangle the way a stored id does once the
/// option is removed from the tree. The list of options itself is recomputed
/// from the subtree on every read, so a stale position is at worst out of
/// range rather than pointing at some unrelated node.
///
/// A `Vec<bool>` rather than a single index because `<select multiple>` can
/// have any number of options selected at once, and a single index would make
/// the multiple case unrepresentable rather than merely unsupported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectData {
    selected: Vec<bool>,
    open: bool,
}

impl SelectData {
    /// Seed the state with one entry per option, in the select's list order.
    pub fn new(selected: Vec<bool>) -> Self {
        Self {
            selected,
            open: false,
        }
    }

    /// Grow or shrink to `len` options, keeping the selectedness of the options
    /// that are still there.
    ///
    /// Layout construction runs again on every resolve, so this is the only
    /// place a script-added `<option>` gets an entry of its own. Re-seeding
    /// from the `selected` content attributes instead would throw away whatever
    /// the user had actually chosen the moment anything else on the page
    /// changed.
    pub fn resize(&mut self, len: usize) {
        self.selected.resize(len, false);
    }

    /// The number of options the state has entries for.
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Whether the option at `index` is selected. Out of range reads `false`.
    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.get(index).copied().unwrap_or(false)
    }

    /// The first selected option, which is `selectedIndex` for a single select.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected.iter().position(|selected| *selected)
    }

    /// Set one option's selectedness without touching the others. Returns
    /// whether it changed.
    pub fn set_selected(&mut self, index: usize, selected: bool) -> bool {
        match self.selected.get_mut(index) {
            Some(entry) if *entry != selected => {
                *entry = selected;
                true
            }
            _ => false,
        }
    }

    /// Select `index` and clear every other option. Returns whether anything
    /// changed.
    pub fn select_only(&mut self, index: usize) -> bool {
        if index >= self.selected.len() {
            return false;
        }
        let mut changed = false;
        for (i, entry) in self.selected.iter_mut().enumerate() {
            let want = i == index;
            changed |= *entry != want;
            *entry = want;
        }
        changed
    }

    /// Deselect every option. Returns whether anything changed.
    pub fn clear_selection(&mut self) -> bool {
        let mut changed = false;
        for entry in self.selected.iter_mut() {
            changed |= *entry;
            *entry = false;
        }
        changed
    }

    /// Whether the drop-down list is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }
}
