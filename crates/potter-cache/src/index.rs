//! The local cache index (spec §8.2): one JSON file per key.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{CacheError, CacheKey, ExecutorClass, io_err};

/// What a key resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The layer blob in `$KILN_HOME` (`sha256:...`); `None` for steps without a layer (`Meta`).
    pub layer: Option<String>,
    pub class: ExecutorClass,
    /// Seconds since the Unix epoch.
    pub created: u64,
    /// The potter version that wrote it.
    pub potter: String,
}

/// One tenant's index: `$POTTER_HOME/cache/<tenant>/index/<key-hex>`.
pub struct Index {
    dir: PathBuf,
}

impl Index {
    pub(crate) fn open(dir: PathBuf) -> Result<Self, CacheError> {
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The entry for `key`. A missing or unreadable entry is a miss.
    pub fn get(&self, key: &CacheKey) -> Result<Option<Entry>, CacheError> {
        let path = self.dir.join(key.hex());
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(&path, e)),
        }
    }

    /// Records `entry` for `key` atomically: a temporary file, synced, then renamed.
    pub fn put(&self, key: &CacheKey, entry: &Entry) -> Result<(), CacheError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = self.dir.join(format!(
            ".{}.tmp-{}-{}",
            key.hex(),
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_vec(entry).map_err(|e| CacheError::Io {
            path: tmp.clone(),
            source: std::io::Error::other(e),
        })?;
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, self.dir.join(key.hex()))
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io_err(&tmp, e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutorClass, PotterHome, Tenant, base_key};

    #[test]
    fn entries_round_trip_and_tenants_are_apart() {
        let dir = tempfile::tempdir().unwrap();
        let home = PotterHome::open(dir.path()).unwrap();
        let a = home.index(&Tenant::new("a").unwrap()).unwrap();
        let b = home.index(&Tenant::new("b").unwrap()).unwrap();
        let key = base_key("sha256:aa");
        let e = Entry {
            layer: Some("sha256:dd".into()),
            class: ExecutorClass::Ns,
            created: 1,
            potter: "0.1.0".into(),
        };
        assert_eq!(a.get(&key).unwrap(), None);
        a.put(&key, &e).unwrap();
        assert_eq!(a.get(&key).unwrap(), Some(e));
        assert_eq!(b.get(&key).unwrap(), None);
        let names: Vec<_> = std::fs::read_dir(dir.path().join("cache/a/index"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(key.hex())]);
    }

    #[test]
    fn a_corrupt_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let home = PotterHome::open(dir.path()).unwrap();
        let idx = home.index(&Tenant::local()).unwrap();
        let key = base_key("x");
        std::fs::write(dir.path().join("cache/local/index").join(key.hex()), "{not json").unwrap();
        assert_eq!(idx.get(&key).unwrap(), None);
    }
}
