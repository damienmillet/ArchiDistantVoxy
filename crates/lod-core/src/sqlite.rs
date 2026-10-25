//! SQLite-backed persistent [`BlobStore`] (EF-4).
//!
//! One table per store; keys are dimension/section coordinates, values are
//! encoded LOD sections. WAL mode keeps reader and writer concurrent.
//! SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]

use std::path::Path;

use rusqlite::Connection;

use crate::cache::BlobStore;

/// SQLite blob store. The connection is internally synchronized.
pub struct SqliteStore {
    connection: std::sync::Mutex<Connection>,
}

impl SqliteStore {
    /// Opens (or creates) the store at `path` and ensures the schema.
    ///
    /// # Errors
    /// Fails when the database cannot be opened or the schema created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 CREATE TABLE IF NOT EXISTS lod_sections (
                     key   TEXT PRIMARY KEY,
                     blob  BLOB NOT NULL
                 );",
            )
            .map_err(|e| e.to_string())?;
        Ok(Self {
            connection: std::sync::Mutex::new(connection),
        })
    }

    /// Opens an in-memory store (useful for tests).
    ///
    /// # Errors
    /// Fails when the in-memory database cannot be initialized.
    pub fn in_memory() -> Result<Self, String> {
        Self::open(":memory:")
    }
}

impl BlobStore for SqliteStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        connection
            .query_row(
                "SELECT blob FROM lod_sections WHERE key = ?1",
                [key],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other.to_string()),
            })
    }

    fn put(&self, key: &str, blob: &[u8]) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        connection
            .execute(
                "INSERT INTO lod_sections (key, blob) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET blob = excluded.blob",
                rusqlite::params![key, blob],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn remove(&self, key: &str) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        connection
            .execute("DELETE FROM lod_sections WHERE key = ?1", [key])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn len(&self) -> Result<usize, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        connection
            .query_row("SELECT COUNT(*) FROM lod_sections", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .map_err(|e| e.to_string())
    }

    fn clear(&self) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        connection
            .execute("DELETE FROM lod_sections", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_remove() {
        let store = SqliteStore::in_memory().unwrap();
        assert_eq!(store.get("overworld/0/0"), Ok(None));
        assert_eq!(store.put("overworld/0/0", &[1, 2, 3]), Ok(()));
        assert_eq!(store.get("overworld/0/0"), Ok(Some(vec![1, 2, 3])));
        assert_eq!(store.len(), Ok(1));
        assert_eq!(store.remove("overworld/0/0"), Ok(()));
        assert_eq!(store.get("overworld/0/0"), Ok(None));
        assert_eq!(store.len(), Ok(0));
    }

    #[test]
    fn upsert_replaces() {
        let store = SqliteStore::in_memory().unwrap();
        store.put("overworld/1/1", &[1]).unwrap();
        store.put("overworld/1/1", &[2, 2]).unwrap();
        assert_eq!(store.get("overworld/1/1"), Ok(Some(vec![2, 2])));
        assert_eq!(store.len(), Ok(1));
    }

    #[test]
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("adv-lod-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.db");

        {
            let store = SqliteStore::open(&path).unwrap();
            store.put("overworld/5/5", &[9, 9, 9]).unwrap();
        }

        let reopened = SqliteStore::open(&path).unwrap();
        assert_eq!(reopened.get("overworld/5/5"), Ok(Some(vec![9, 9, 9])));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(dir.join("cache.db-wal"));
        let _ = std::fs::remove_file(dir.join("cache.db-shm"));
    }
}
