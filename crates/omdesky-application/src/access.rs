use crate::ports::{AccessStore, AllowedController, MeshNetwork, PortError, PortResult};
use omdesky_core::ControlCapability;
use std::{net::IpAddr, sync::Arc};

pub fn missing_callback_capabilities(
    allowlist: &[AllowedController],
    tailnet_node_id: &str,
) -> Vec<ControlCapability> {
    let entry = allowlist
        .iter()
        .find(|entry| entry.tailnet_node_id == tailnet_node_id);

    ControlCapability::CALLBACK
        .into_iter()
        .filter(|capability| !entry.is_some_and(|entry| entry.allows(*capability)))
        .collect()
}

pub fn callback_access_error(missing: &[ControlCapability]) -> PortError {
    let missing = missing
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",");

    PortError::new(
        "CALLBACK_ACCESS_MISSING",
        format!("the controller does not grant {missing} to the remote device"),
        false,
    )
}

#[derive(Clone)]
pub struct CallbackAccess {
    mesh: Arc<dyn MeshNetwork>,
    access: Arc<dyn AccessStore>,
}

impl CallbackAccess {
    pub fn new(mesh: Arc<dyn MeshNetwork>, access: Arc<dyn AccessStore>) -> Self {
        Self { mesh, access }
    }

    pub async fn verify(&self, address: IpAddr) -> PortResult<()> {
        let identity = self.mesh.identify_source(address).await?.ok_or_else(|| {
            PortError::new(
                "PEER_IDENTITY_UNKNOWN",
                "Tailscale does not know the remote address",
                false,
            )
        })?;

        let allowlist = self.access.list().await?;

        let missing = missing_callback_capabilities(&allowlist, &identity.tailnet_node_id);

        if missing.is_empty() {
            return Ok(());
        }

        Err(callback_access_error(&missing))
    }
}

#[cfg(test)]
pub(crate) fn allowed(id: &str, capabilities: &[ControlCapability]) -> AllowedController {
    AllowedController::new(
        id,
        None,
        time::OffsetDateTime::UNIX_EPOCH,
        capabilities.iter().copied(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_callback_capabilities_reports_an_unlisted_device() {
        let allowlist = vec![allowed("nOTHER", &ControlCapability::ALL)];

        assert_eq!(
            missing_callback_capabilities(&allowlist, "nREMOTE"),
            ControlCapability::CALLBACK.to_vec()
        );
    }

    #[test]
    fn test_missing_callback_capabilities_reports_each_withheld_capability() {
        let allowlist = vec![allowed("nREMOTE", &[ControlCapability::SendShortcut])];

        assert_eq!(
            missing_callback_capabilities(&allowlist, "nREMOTE"),
            vec![ControlCapability::CloseStream]
        );
    }

    #[test]
    fn test_missing_callback_capabilities_accepts_a_device_with_the_callbacks() {
        let allowlist = vec![allowed("nREMOTE", &ControlCapability::CALLBACK)];

        assert!(missing_callback_capabilities(&allowlist, "nREMOTE").is_empty());
    }

    #[test]
    fn test_an_entry_without_capabilities_grants_no_callbacks() {
        let allowlist = vec![allowed("nREMOTE", &[])];

        assert_eq!(
            missing_callback_capabilities(&allowlist, "nREMOTE"),
            ControlCapability::CALLBACK.to_vec()
        );
    }
}
