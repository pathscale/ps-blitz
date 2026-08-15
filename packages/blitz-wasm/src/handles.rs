//! The handle table: the guest's only way to name a node.
//!
//! A `NodeId` is an index into the document's arena, so a guest handed raw
//! `NodeId` values could address every node in the document by counting from
//! zero, including nodes belonging to a page it was never given. A handle is
//! an index into *this instance's* table instead, and the table only ever
//! contains nodes the host put there: the mount point it was seeded with, and
//! nodes the guest itself created.
//!
//! So a forged handle is not an escalation. Either it is out of range, which
//! is [`ERR_BAD_HANDLE`](crate::status::ERR_BAD_HANDLE), or it names a node
//! this guest already holds a handle for. There is nothing to reach that was
//! not already reachable.

use blitz_dom::NodeId;

use crate::status::{ERR_BAD_HANDLE, ERR_TOO_MANY_HANDLES};

/// A guest-facing node reference. Opaque on the guest side.
pub type Handle = u32;

/// The handle the host seeds every instance with: the node a guest appends its
/// tree to.
pub const MOUNT: Handle = 0;

/// Maps handles to node ids, one table per instance.
#[derive(Debug, Clone)]
pub struct HandleTable {
    nodes: Vec<NodeId>,
}

impl HandleTable {
    /// A table holding only the mount point, which is always [`MOUNT`].
    pub fn with_mount(mount: NodeId) -> Self {
        Self { nodes: vec![mount] }
    }

    /// Hand out a handle for a node.
    ///
    /// Handles are never reused and never freed. A node the guest detaches
    /// keeps its handle, which matches `blitz-dom-api`'s own detach-not-drop
    /// rule: the node stays addressable, so a handle to it stays meaningful.
    pub fn insert(&mut self, node: NodeId) -> Result<Handle, i32> {
        let handle = Handle::try_from(self.nodes.len()).map_err(|_| ERR_TOO_MANY_HANDLES)?;
        if handle > i32::MAX as Handle {
            return Err(ERR_TOO_MANY_HANDLES);
        }
        self.nodes.push(node);
        Ok(handle)
    }

    /// Resolve a handle, or [`ERR_BAD_HANDLE`] if this instance never issued it.
    pub fn get(&self, handle: Handle) -> Result<NodeId, i32> {
        self.nodes
            .get(handle as usize)
            .copied()
            .ok_or(ERR_BAD_HANDLE)
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
