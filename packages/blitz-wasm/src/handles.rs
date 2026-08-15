//! The handle table: the guest's only way to name a node.
//!
//! [`Handle`] itself is `dom-abi`'s, along with what it means and why it is not
//! a `NodeId`. This module is the *table*: the per-instance mapping that gives
//! the type its meaning. The vocabulary is shared with the guest bindings; the
//! storage is not, and never was.

use blitz_dom::NodeId;
use dom_abi::host::{Handle, MAX_ID, Status};

/// Maps handles to node ids, one table per instance.
#[derive(Debug, Clone)]
pub struct HandleTable {
    nodes: Vec<NodeId>,
}

impl HandleTable {
    /// A table holding only the mount point, which is always [`Handle::MOUNT`].
    pub fn with_mount(mount: NodeId) -> Self {
        Self { nodes: vec![mount] }
    }

    /// Hand out a handle for a node.
    ///
    /// Handles are never reused and never freed. A node the guest detaches
    /// keeps its handle, which matches `blitz-dom-api`'s own detach-not-drop
    /// rule: the node stays addressable, so a handle to it stays meaningful.
    pub fn insert(&mut self, node: NodeId) -> Result<Handle, Status> {
        let next = u32::try_from(self.nodes.len()).map_err(|_| Status::ERR_TOO_MANY_HANDLES)?;
        if next > MAX_ID {
            return Err(Status::ERR_TOO_MANY_HANDLES);
        }
        self.nodes.push(node);
        Ok(Handle(next))
    }

    /// Resolve a handle, or [`Status::ERR_BAD_HANDLE`] if this instance never
    /// issued it.
    pub fn get(&self, handle: Handle) -> Result<NodeId, Status> {
        self.nodes
            .get(handle.0 as usize)
            .copied()
            .ok_or(Status::ERR_BAD_HANDLE)
    }

    /// How many handles have been issued, including the mount.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false: the mount handle is present from construction.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}
