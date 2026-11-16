//! Bearer tokens: generated once, stored only as a SHA-256 hash.

use crate::perm::Permission;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A stored token (never the secret itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenRecord {
    pub id: String,
    pub name: String,
    pub permission: Permission,
    pub hash: String,
}

/// File backed token list.
pub struct TokenStore {
    path: PathBuf,
    pub tokens: Vec<TokenRecord>,
}

/// Hex SHA-256 of a secret.
fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Random hex string of `bytes` bytes.
fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the operating system random source failed");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

impl TokenStore {
    /// Loads the file; a missing file is an empty store, a corrupt one is an error.
    pub fn load(path: &Path) -> Result<Self, String> {
        let tokens = match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| format!("corrupt token file {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(Self { path: path.to_path_buf(), tokens })
    }

    /// Writes the file atomically.
    fn save(&self) -> Result<(), String> {
        let tmp = self.path.with_extension("tmp");
        let text = serde_json::to_string_pretty(&self.tokens).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &self.path)).map_err(|e| format!("cannot save the tokens: {e}"))
    }

    /// Creates a token and returns its secret, which is shown only once.
    pub fn create(&mut self, name: &str, permission: Permission) -> Result<String, String> {
        let secret = format!("dbz_{}", random_hex(24));
        self.tokens.push(TokenRecord { id: random_hex(4), name: name.to_owned(), permission, hash: hash(&secret) });
        self.save()?;
        Ok(secret)
    }

    /// Revokes a token.
    pub fn revoke(&mut self, id: &str) -> Result<(), String> {
        self.tokens.retain(|t| t.id != id);
        self.save()
    }

    /// The token matching a presented secret.
    pub fn verify(&self, secret: &str) -> Option<&TokenRecord> {
        let h = hash(secret);
        self.tokens.iter().find(|t| t.hash == h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Secrets verify, are not stored in clear, survive a reload and can be revoked.
    #[test]
    fn lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let mut s = TokenStore::load(&path).unwrap();
        let secret = s.create("claude", Permission::ReadOnly).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains(&secret));
        assert!(s.verify("wrong").is_none());
        let id = s.verify(&secret).unwrap().id.clone();
        let again = TokenStore::load(&path).unwrap();
        assert_eq!(again.verify(&secret).unwrap().permission, Permission::ReadOnly);
        let mut s = again;
        s.revoke(&id).unwrap();
        assert!(s.verify(&secret).is_none());
    }

    /// A corrupt file is reported, never silently replaced.
    #[test]
    fn corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, "{nope").unwrap();
        assert!(TokenStore::load(&path).is_err());
    }
}
