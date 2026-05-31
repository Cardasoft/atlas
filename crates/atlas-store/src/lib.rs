//! atlas-store — stockage objet (doc 02 §3.3 / doc 23 §5).
//!
//! TDD : `storage_key` (pure) et `FsObjectStore` (FS local, testable sans service) sont
//! couverts. Le backend **SeaweedFS S3** s'implémentera derrière le même trait `ObjectStore`,
//! sans toucher les appelants.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(String),
    #[error("introuvable: {0}")]
    NotFound(String),
}

/// Contrat d'un stockage objet (mêmes opérations FS et S3).
pub trait ObjectStore: Send + Sync {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StoreError>;
    fn get(&self, key: &str) -> Result<Vec<u8>, StoreError>;
    fn delete(&self, key: &str) -> Result<(), StoreError>;
    fn exists(&self, key: &str) -> bool;
}

/// Clé de stockage déterministe, shardée par les 2 premiers octets du hash (équilibrage).
/// Forme : `tenant/<ab>/<cd>/<sha>.<ext>`. Le contenu adressé par hash = dédup naturelle.
pub fn storage_key(tenant: &str, sha256_hex: &str, ext: &str) -> String {
    let ab = &sha256_hex[0..2.min(sha256_hex.len())];
    let cd = &sha256_hex[2..4.min(sha256_hex.len())];
    if ext.is_empty() {
        format!("{tenant}/{ab}/{cd}/{sha256_hex}")
    } else {
        format!("{tenant}/{ab}/{cd}/{sha256_hex}.{ext}")
    }
}

/// Stockage sur système de fichiers (édition Solo / dev / tests).
pub struct FsObjectStore {
    root: PathBuf,
}

impl FsObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

impl ObjectStore for FsObjectStore {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let p = self.path(key);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        std::fs::write(&p, bytes).map_err(|e| StoreError::Io(e.to_string()))
    }
    fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let p = self.path(key);
        if !Path::new(&p).exists() {
            return Err(StoreError::NotFound(key.to_string()));
        }
        std::fs::read(&p).map_err(|e| StoreError::Io(e.to_string()))
    }
    fn delete(&self, key: &str) -> Result<(), StoreError> {
        let p = self.path(key);
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        Ok(())
    }
    fn exists(&self, key: &str) -> bool {
        self.path(key).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_sharded_and_deterministic() {
        let k = storage_key("t1", "abcdef0123456789", "jpg");
        assert_eq!(k, "t1/ab/cd/abcdef0123456789.jpg");
        assert_eq!(k, storage_key("t1", "abcdef0123456789", "jpg"));
    }

    #[test]
    fn key_without_extension() {
        assert_eq!(storage_key("t1", "abcdef", ""), "t1/ab/cd/abcdef");
    }

    fn tmp_root() -> PathBuf {
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "atlas-store-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(uniq);
        p
    }

    #[test]
    fn put_get_delete_roundtrip() {
        let store = FsObjectStore::new(tmp_root());
        let key = storage_key("t1", "deadbeef", "bin");
        assert!(!store.exists(&key));
        store.put(&key, b"hello atlas").unwrap();
        assert!(store.exists(&key));
        assert_eq!(store.get(&key).unwrap(), b"hello atlas");
        store.delete(&key).unwrap();
        assert!(!store.exists(&key));
        assert!(matches!(store.get(&key), Err(StoreError::NotFound(_))));
    }
}
