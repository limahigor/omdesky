use crate::state::{atomic_write_json, read_json};
use omdesk_core::NodeId;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A stable, local DeskLink node identifier. It is used only for launcher and
/// configuration bookkeeping. The control-plane trust anchor is the Tailscale
/// identity, so this carries no cryptographic key material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeIdentity {
    pub node_id: NodeId,
}

impl NodeIdentity {
    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        if path.exists() {
            return read_json(path).map_err(IdentityError::State);
        }

        let identity = Self {
            node_id: NodeId::new(),
        };
        atomic_write_json(path, &identity, false)?;

        Ok(identity)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity state failed: {0}")]
    State(#[from] crate::state::StateError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_is_stable_across_reloads() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("identity/node.json");

        let first = NodeIdentity::load_or_create(&path).expect("identity created");
        let second = NodeIdentity::load_or_create(&path).expect("identity loaded");

        assert_eq!(first.node_id, second.node_id);
    }
}
