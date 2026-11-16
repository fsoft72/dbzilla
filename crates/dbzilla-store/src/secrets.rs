//! Secret storage: OS keychain in production, memory in tests (NFR-05).

use crate::{StoreError, StoreResult};
use std::collections::HashMap;
use std::sync::Mutex;

/// Service name used with the OS keychain.
const SERVICE: &str = "dbzilla";

/// Key of a connection secret, `dbzilla/<connection_id>/<field>`.
pub fn secret_key(conn_id: &str, field: &str) -> String {
    format!("{SERVICE}/{conn_id}/{field}")
}

/// Storage of passwords, passphrases and tokens.
pub trait SecretStore: Send + Sync {
    /// Reads a secret; `None` when it does not exist.
    fn get(&self, key: &str) -> StoreResult<Option<String>>;
    /// Stores or replaces a secret.
    fn set(&self, key: &str, value: &str) -> StoreResult<()>;
    /// Deletes a secret; deleting a missing one is not an error.
    fn delete(&self, key: &str) -> StoreResult<()>;
}

/// The OS keychain (Secret Service, Keychain, Credential Manager).
#[derive(Debug, Default)]
pub struct KeyringStore;

impl KeyringStore {
    /// Entry for a key.
    fn entry(key: &str) -> StoreResult<keyring::Entry> {
        keyring::Entry::new(SERVICE, key).map_err(|e| StoreError::Secret(e.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> StoreResult<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(StoreError::Secret(e.to_string())),
        }
    }

    fn set(&self, key: &str, value: &str) -> StoreResult<()> {
        Self::entry(key)?.set_password(value).map_err(|e| StoreError::Secret(e.to_string()))
    }

    fn delete(&self, key: &str) -> StoreResult<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(StoreError::Secret(e.to_string())),
        }
    }
}

/// In-memory store for tests and for sessions without a keychain.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> StoreResult<Option<String>> {
        Ok(self.map.lock().map_err(|_| StoreError::Secret("lock poisoned".into()))?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> StoreResult<()> {
        self.map.lock().map_err(|_| StoreError::Secret("lock poisoned".into()))?.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> StoreResult<()> {
        self.map.lock().map_err(|_| StoreError::Secret("lock poisoned".into()))?.remove(key);
        Ok(())
    }
}
