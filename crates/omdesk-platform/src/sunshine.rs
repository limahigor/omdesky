use async_trait::async_trait;
use omdesk_application::ports::{
    CommandRunner, CommandSpec, HostReadiness, PortError, PortResult, StreamHost,
};
use omdesk_core::Display;
use omdesk_protocol::{SunshinePairRequest, SunshineStatusResponse};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use crate::hyprland::HyprlandAdapter;
use omdesk_application::ports::RemoteOmarchy;

pub const DEFAULT_API_BASE: &str = "https://127.0.0.1:47990";
pub const DESKTOP_APPLICATION: &str = "Desktop";

/// Omarchy Desk-owned credentials for Sunshine's local admin API. Stored with
/// `0600` on the controlled node and never transmitted off it.
#[derive(Clone)]
pub struct SunshineCredentials {
    pub username: String,
    pub password: SecretString,
}

#[derive(Deserialize)]
struct RawCredentials {
    username: String,
    password: String,
}

impl SunshineCredentials {
    pub fn load(path: &std::path::Path) -> Option<Self> {
        let content = fs::read_to_string(path).ok()?;
        let raw: RawCredentials = serde_json::from_str(&content).ok()?;
        if raw.username.is_empty() || raw.password.is_empty() {
            return None;
        }
        Some(Self {
            username: raw.username,
            password: SecretString::from(raw.password),
        })
    }
}

/// Persist Omarchy Desk-owned Sunshine admin credentials with `0600`. Shared by the
/// CLI (`omdesk setup`) and the TUI settings screen so both configure the same
/// file identically.
pub fn store_credentials(
    path: &std::path::Path,
    username: &str,
    password: &str,
) -> Result<(), crate::state::StateError> {
    let record = serde_json::json!({ "username": username, "password": password });
    crate::state::atomic_write_json(path, &record, true)
}

#[derive(Clone)]
pub struct SunshineAdapter {
    runner: Arc<dyn CommandRunner>,
    desktop: HyprlandAdapter,
    config_path: PathBuf,
    api_base: String,
    credentials_path: Option<PathBuf>,
    http: reqwest::Client,
}

impl SunshineAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>, config_path: PathBuf) -> Self {
        Self::with_api(runner, config_path, DEFAULT_API_BASE.to_owned(), None)
    }

    pub fn with_api(
        runner: Arc<dyn CommandRunner>,
        config_path: PathBuf,
        api_base: String,
        credentials_path: Option<PathBuf>,
    ) -> Self {
        let http = reqwest::Client::builder()
            // Sunshine serves its admin API over a self-signed certificate on
            // loopback only. We deliberately accept it because the connection
            // never leaves the host.
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            desktop: HyprlandAdapter::new(runner.clone()),
            runner,
            config_path,
            api_base,
            credentials_path,
            http,
        }
    }

    /// Load credentials on demand so that provisioning them (via `omdesk setup`)
    /// takes effect without restarting the agent.
    fn credentials(&self) -> Option<SunshineCredentials> {
        SunshineCredentials::load(self.credentials_path.as_deref()?)
    }

    async fn installed(&self) -> bool {
        self.runner
            .run(CommandSpec::new("sunshine", ["--version".to_owned()]))
            .await
            .is_ok()
    }

    async fn running(&self) -> bool {
        self.runner
            .run(CommandSpec::new(
                "systemctl",
                [
                    "--user".to_owned(),
                    "is-active".to_owned(),
                    "sunshine.service".to_owned(),
                ],
            ))
            .await
            .is_ok()
    }
}

#[async_trait]
impl StreamHost for SunshineAdapter {
    async fn readiness(&self) -> PortResult<HostReadiness> {
        let installed = self.installed().await;
        let running = self.running().await;

        let config = fs::read_to_string(&self.config_path).unwrap_or_default();
        let keyboard_disabled = config_value(&config, "keyboard") == Some("disabled");
        let mouse_disabled = config_value(&config, "mouse") == Some("disabled");

        let bind_address = config_value(&config, "bind_address");
        let wildcard = bind_address
            .is_none_or(|value| value.is_empty() || value == "0.0.0.0" || value == "::");

        Ok(HostReadiness {
            installed,
            running,
            capture_ready: installed
                && running
                && !RemoteOmarchy::displays(&self.desktop).await?.is_empty(),
            input_ready: !keyboard_disabled && !mouse_disabled,
            exposure_warning: wildcard
                .then(|| "Sunshine may be listening on non-Tailscale interfaces".to_owned()),
        })
    }

    async fn status(&self) -> PortResult<SunshineStatusResponse> {
        let readiness = self.readiness().await?;

        let ready = readiness.installed
            && readiness.running
            && readiness.capture_ready
            && readiness.input_ready;

        Ok(SunshineStatusResponse {
            installed: readiness.installed,
            running: readiness.running,
            ready,
            desktop_app: readiness.installed.then(|| DESKTOP_APPLICATION.to_owned()),
            pairing_api_available: readiness.running && self.credentials().is_some(),
        })
    }

    async fn displays(&self) -> PortResult<Vec<Display>> {
        RemoteOmarchy::displays(&self.desktop).await
    }

    async fn submit_pairing_pin(&self, request: SunshinePairRequest) -> PortResult<()> {
        let credentials = self.credentials().ok_or_else(|| {
            PortError::new(
                "SUNSHINE_API_UNAVAILABLE",
                "Sunshine admin credentials are not configured on this node; run `omdesk setup` on the host",
                false,
            )
        })?;

        let response = self
            .http
            .post(format!("{}/api/pin", self.api_base))
            .basic_auth(
                &credentials.username,
                Some(credentials.password.expose_secret()),
            )
            .json(&serde_json::json!({
                "pin": request.pin,
                "name": request.client_name,
            }))
            .send()
            .await
            .map_err(|error| PortError::new("SUNSHINE_API_UNAVAILABLE", error.to_string(), true))?;

        if !response.status().is_success() {
            return Err(PortError::new(
                "SUNSHINE_PAIRING_FAILED",
                format!(
                    "Sunshine rejected the pairing PIN (HTTP {})",
                    response.status()
                ),
                false,
            ));
        }

        let body: PinResult = response
            .json()
            .await
            .map_err(|error| PortError::new("SUNSHINE_PAIRING_FAILED", error.to_string(), false))?;

        if body.status {
            Ok(())
        } else {
            Err(PortError::new(
                "SUNSHINE_PAIRING_FAILED",
                "Sunshine did not accept the pairing PIN",
                false,
            ))
        }
    }
}

#[derive(Deserialize)]
struct PinResult {
    #[serde(default)]
    status: bool,
}

fn config_value<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    config.lines().find_map(|line| {
        let line = line.trim();
        let (candidate, value) = line.split_once('=')?;
        (candidate.trim() == key).then(|| value.trim())
    })
}

pub fn ensure_ready(readiness: &HostReadiness) -> PortResult<()> {
    if !readiness.installed {
        Err(PortError::new(
            "SUNSHINE_NOT_INSTALLED",
            "Sunshine is not installed",
            false,
        ))
    } else if !readiness.running {
        Err(PortError::new(
            "SUNSHINE_NOT_RUNNING",
            "Sunshine is not running",
            true,
        ))
    } else if !readiness.capture_ready || !readiness.input_ready {
        Err(PortError::new(
            "SUNSHINE_NOT_READY",
            "Sunshine is not ready",
            false,
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_value_reads_exact_key() {
        let config = "keyboard = enabled\nmouse = disabled\n";

        assert_eq!(config_value(config, "keyboard"), Some("enabled"));
        assert_eq!(config_value(config, "key"), None);
    }

    #[test]
    fn test_ensure_ready_maps_missing_installation() {
        let readiness = HostReadiness {
            installed: false,
            running: false,
            capture_ready: false,
            input_ready: false,
            exposure_warning: None,
        };

        assert_eq!(
            ensure_ready(&readiness).expect_err("not ready").code,
            "SUNSHINE_NOT_INSTALLED"
        );
    }

    #[test]
    fn test_credentials_round_trip_through_disk() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("sunshine-credentials.json");
        store_credentials(&path, "admin", "s3cret").expect("stored");

        let loaded = SunshineCredentials::load(&path).expect("loaded");
        assert_eq!(loaded.username, "admin");
        assert_eq!(loaded.password.expose_secret(), "s3cret");
    }

    #[test]
    fn test_empty_credentials_are_treated_as_absent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("sunshine-credentials.json");
        store_credentials(&path, "", "").expect("stored");

        assert!(SunshineCredentials::load(&path).is_none());
    }
}
