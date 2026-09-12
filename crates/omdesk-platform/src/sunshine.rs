use async_trait::async_trait;
use keyring::Entry;
use omdesk_application::ports::{
    CommandRunner, CommandSpec, HostReadiness, PortError, PortResult, StreamHost,
};
use omdesk_core::Display;
use omdesk_protocol::{SunshinePairRequest, SunshineStatusResponse};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use crate::hyprland::HyprlandAdapter;
use omdesk_application::ports::RemoteOmarchy;

pub const DEFAULT_API_BASE: &str = "https://127.0.0.1:47990";
pub const DESKTOP_APPLICATION: &str = "Desktop";
const KEYRING_SERVICE: &str = "io.omarchy.omdesk.sunshine";
const KEYRING_ACCOUNT: &str = "admin-api";

#[derive(Clone)]
pub struct SunshineCredentials {
    pub username: String,
    pub password: SecretString,
}

#[derive(Deserialize, Serialize)]
struct RawCredentials {
    username: String,
    password: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SunshineCredentialError {
    #[error("Sunshine credentials must include a username and password")]
    InvalidCredentials,
    #[error("the desktop Secret Service is unavailable or locked")]
    SecretServiceUnavailable,
    #[error("the stored Sunshine credentials are invalid")]
    InvalidStoredCredentials,
    #[error("legacy Sunshine credentials could not be migrated")]
    MigrationFailed,
}

trait CredentialBackend: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, SunshineCredentialError>;
    fn store(&self, secret: &[u8]) -> Result<(), SunshineCredentialError>;
}

struct SecretServiceCredentialBackend;

impl SecretServiceCredentialBackend {
    fn entry() -> Result<Entry, SunshineCredentialError> {
        Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .map_err(|_| SunshineCredentialError::SecretServiceUnavailable)
    }
}

impl CredentialBackend for SecretServiceCredentialBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, SunshineCredentialError> {
        match Self::entry()?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(SunshineCredentialError::SecretServiceUnavailable),
        }
    }

    fn store(&self, secret: &[u8]) -> Result<(), SunshineCredentialError> {
        Self::entry()?
            .set_secret(secret)
            .map_err(|_| SunshineCredentialError::SecretServiceUnavailable)
    }
}

#[derive(Clone)]
pub struct SunshineCredentialStore {
    backend: Arc<dyn CredentialBackend>,
    legacy_path: Option<PathBuf>,
}

impl SunshineCredentialStore {
    pub fn new(legacy_path: Option<PathBuf>) -> Self {
        Self {
            backend: Arc::new(SecretServiceCredentialBackend),
            legacy_path,
        }
    }

    #[cfg(test)]
    fn with_backend(backend: Arc<dyn CredentialBackend>, legacy_path: Option<PathBuf>) -> Self {
        Self {
            backend,
            legacy_path,
        }
    }

    pub fn load(&self) -> Result<Option<SunshineCredentials>, SunshineCredentialError> {
        if let Some(secret) = self.backend.load()? {
            return parse_credentials(&secret).map(Some);
        }

        let Some(path) = self.legacy_path.as_deref().filter(|path| path.exists()) else {
            return Ok(None);
        };
        let secret = fs::read(path).map_err(|_| SunshineCredentialError::MigrationFailed)?;
        let credentials = parse_credentials(&secret)?;

        self.store(&credentials.username, credentials.password.expose_secret())?;
        fs::remove_file(path).map_err(|_| SunshineCredentialError::MigrationFailed)?;

        Ok(Some(credentials))
    }

    pub fn store(&self, username: &str, password: &str) -> Result<(), SunshineCredentialError> {
        if username.is_empty() || password.is_empty() {
            return Err(SunshineCredentialError::InvalidCredentials);
        }

        let secret = serde_json::to_vec(&RawCredentials {
            username: username.to_owned(),
            password: password.to_owned(),
        })
        .map_err(|_| SunshineCredentialError::InvalidStoredCredentials)?;

        self.backend.store(&secret)
    }
}

fn parse_credentials(secret: &[u8]) -> Result<SunshineCredentials, SunshineCredentialError> {
    let raw: RawCredentials = serde_json::from_slice(secret)
        .map_err(|_| SunshineCredentialError::InvalidStoredCredentials)?;

    if raw.username.is_empty() || raw.password.is_empty() {
        return Err(SunshineCredentialError::InvalidStoredCredentials);
    }

    Ok(SunshineCredentials {
        username: raw.username,
        password: SecretString::from(raw.password),
    })
}

#[derive(Clone)]
pub struct SunshineAdapter {
    runner: Arc<dyn CommandRunner>,
    desktop: HyprlandAdapter,
    config_path: PathBuf,
    api_base: String,
    credentials: SunshineCredentialStore,
    http: reqwest::Client,
}

impl SunshineAdapter {
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        config_path: PathBuf,
        credentials: SunshineCredentialStore,
    ) -> Self {
        Self::with_api(
            runner,
            config_path,
            DEFAULT_API_BASE.to_owned(),
            credentials,
        )
    }

    pub fn with_api(
        runner: Arc<dyn CommandRunner>,
        config_path: PathBuf,
        api_base: String,
        credentials: SunshineCredentialStore,
    ) -> Self {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            desktop: HyprlandAdapter::new(runner.clone()),
            runner,
            config_path,
            api_base,
            credentials,
            http,
        }
    }

    async fn credentials(&self) -> PortResult<Option<SunshineCredentials>> {
        let credentials = self.credentials.clone();

        tokio::task::spawn_blocking(move || credentials.load())
            .await
            .map_err(|_| credential_port_error())?
            .map_err(|_| credential_port_error())
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
            pairing_api_available: readiness.running && self.credentials().await?.is_some(),
        })
    }

    async fn displays(&self) -> PortResult<Vec<Display>> {
        RemoteOmarchy::displays(&self.desktop).await
    }

    async fn submit_pairing_pin(&self, request: SunshinePairRequest) -> PortResult<()> {
        let credentials = self.credentials().await?.ok_or_else(|| {
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

fn credential_port_error() -> PortError {
    PortError::new(
        "SUNSHINE_API_UNAVAILABLE",
        "the desktop Secret Service is unavailable or locked",
        true,
    )
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

    #[derive(Default)]
    struct MemoryCredentialBackend {
        secret: std::sync::Mutex<Option<Vec<u8>>>,
    }

    struct FailingCredentialBackend;

    impl CredentialBackend for FailingCredentialBackend {
        fn load(&self) -> Result<Option<Vec<u8>>, SunshineCredentialError> {
            Ok(None)
        }

        fn store(&self, _secret: &[u8]) -> Result<(), SunshineCredentialError> {
            Err(SunshineCredentialError::SecretServiceUnavailable)
        }
    }

    impl CredentialBackend for MemoryCredentialBackend {
        fn load(&self) -> Result<Option<Vec<u8>>, SunshineCredentialError> {
            Ok(self.secret.lock().expect("credential lock").clone())
        }

        fn store(&self, secret: &[u8]) -> Result<(), SunshineCredentialError> {
            *self.secret.lock().expect("credential lock") = Some(secret.to_vec());
            Ok(())
        }
    }

    #[test]
    fn test_credentials_round_trip_through_secret_store() {
        let store = SunshineCredentialStore::with_backend(
            Arc::new(MemoryCredentialBackend::default()),
            None,
        );

        store.store("admin", "s3cret").expect("stored");

        let loaded = store.load().expect("keyring available").expect("loaded");
        assert_eq!(loaded.username, "admin");
        assert_eq!(loaded.password.expose_secret(), "s3cret");
    }

    #[test]
    fn test_legacy_plaintext_credentials_are_migrated_and_removed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("sunshine-credentials.json");
        let record = serde_json::json!({ "username": "admin", "password": "s3cret" });
        crate::state::atomic_write_json(&path, &record, true).expect("legacy credentials");
        let store = SunshineCredentialStore::with_backend(
            Arc::new(MemoryCredentialBackend::default()),
            Some(path.clone()),
        );

        let loaded = store.load().expect("migration succeeds").expect("loaded");

        assert_eq!(loaded.username, "admin");
        assert_eq!(loaded.password.expose_secret(), "s3cret");
        assert!(!path.exists());
    }

    #[test]
    fn test_failed_migration_keeps_legacy_plaintext_credentials() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("sunshine-credentials.json");
        let record = serde_json::json!({ "username": "admin", "password": "s3cret" });
        crate::state::atomic_write_json(&path, &record, true).expect("legacy credentials");
        let store = SunshineCredentialStore::with_backend(
            Arc::new(FailingCredentialBackend),
            Some(path.clone()),
        );

        assert!(store.load().is_err());
        assert!(path.exists());
    }

    #[test]
    fn test_empty_credentials_are_rejected() {
        let store = SunshineCredentialStore::with_backend(
            Arc::new(MemoryCredentialBackend::default()),
            None,
        );

        assert!(store.store("", "").is_err());
        assert!(store.load().expect("keyring available").is_none());
    }
}
