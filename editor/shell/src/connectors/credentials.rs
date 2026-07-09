//! Connector credentials, kept out of the renderer. Secrets live in the OS keyring
//! (service `saffron-anima`, account = connector id), machine/user-global — never in the
//! project file.
//!
//! The backend is chosen once: when `SAFFRON_NO_KEYRING` is set or no Secret Service is
//! reachable (the toolbox / CI case), it degrades to an in-memory map so the editor and
//! the e2e host boot without a daemon. A test can also inject a secret via
//! `SAFFRON_SECRET_<ID>` (uppercased, `-`→`_`), which always takes precedence.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const SERVICE: &str = "saffron-anima";

/// A credential-layer failure.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("keyring error: {0}")]
    Keyring(String),
}

enum Backend {
    Keyring,
    Memory(Mutex<HashMap<String, String>>),
}

/// The process-wide credential store.
pub struct Credentials {
    backend: Backend,
}

static INSTANCE: OnceLock<Credentials> = OnceLock::new();

impl Credentials {
    /// The shared credential store, backend chosen on first use.
    pub fn global() -> &'static Credentials {
        INSTANCE.get_or_init(Credentials::detect)
    }

    fn detect() -> Credentials {
        if std::env::var_os("SAFFRON_NO_KEYRING").is_some() || !keyring_reachable() {
            tracing::info!(
                "store credentials: in-memory backend (no Secret Service / SAFFRON_NO_KEYRING)"
            );
            return Credentials {
                backend: Backend::Memory(Mutex::new(HashMap::new())),
            };
        }
        Credentials {
            backend: Backend::Keyring,
        }
    }

    pub fn set_secret(&self, connector_id: &str, secret: &str) -> Result<(), CredentialError> {
        match &self.backend {
            Backend::Keyring => keyring::Entry::new(SERVICE, connector_id)
                .and_then(|entry| entry.set_password(secret))
                .map_err(|e| CredentialError::Keyring(e.to_string())),
            Backend::Memory(map) => {
                lock(map).insert(connector_id.to_owned(), secret.to_owned());
                Ok(())
            }
        }
    }

    pub fn get_secret(&self, connector_id: &str) -> Option<String> {
        if let Some(injected) = env_secret(connector_id) {
            return Some(injected);
        }
        match &self.backend {
            Backend::Keyring => keyring::Entry::new(SERVICE, connector_id)
                .ok()?
                .get_password()
                .ok(),
            Backend::Memory(map) => lock(map).get(connector_id).cloned(),
        }
    }

    pub fn delete_secret(&self, connector_id: &str) -> Result<(), CredentialError> {
        match &self.backend {
            Backend::Keyring => match keyring::Entry::new(SERVICE, connector_id) {
                Ok(entry) => match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(CredentialError::Keyring(e.to_string())),
                },
                Err(e) => Err(CredentialError::Keyring(e.to_string())),
            },
            Backend::Memory(map) => {
                lock(map).remove(connector_id);
                Ok(())
            }
        }
    }

    pub fn has_secret(&self, connector_id: &str) -> bool {
        self.get_secret(connector_id).is_some()
    }
}

fn lock(
    map: &Mutex<HashMap<String, String>>,
) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
    map.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn env_secret(connector_id: &str) -> Option<String> {
    let key = format!(
        "SAFFRON_SECRET_{}",
        connector_id.to_uppercase().replace('-', "_")
    );
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Whether a real Secret Service backend answers. A missing entry counts as reachable
/// (the store works, the key is just absent); only a platform/connection failure does not.
fn keyring_reachable() -> bool {
    match keyring::Entry::new(SERVICE, "__probe__") {
        Ok(entry) => !matches!(
            entry.get_password(),
            Err(keyring::Error::PlatformFailure(_)) | Err(keyring::Error::NoStorageAccess(_))
        ),
        Err(_) => false,
    }
}
