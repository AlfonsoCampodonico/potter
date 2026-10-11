//! potter's cache (spec §8): keys, the local index, tenants and `$POTTER_HOME`.
#![forbid(unsafe_code)]

mod index;
mod key;

use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub use index::{Entry, Index};
pub use key::{CacheKey, ExecutorClass, POTTER_MAJOR, StepInput, base_key, secrets_digest, step_key};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid tenant {0:?}: use 1-63 of a-z, 0-9, '.', '_' and '-', starting with a letter or digit")]
    BadTenant(String),
    #[error("cannot find a home directory for $POTTER_HOME; set POTTER_HOME")]
    NoHome,
    #[error(transparent)]
    Plan(potter_plan::PlanError),
}

pub(crate) fn io_err(path: &Path, source: std::io::Error) -> CacheError {
    CacheError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A tenant: builds of different tenants share no cache entries or mounts (spec §3, P1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tenant(String);

impl Tenant {
    pub fn new(s: &str) -> Result<Self, CacheError> {
        let ok = (1..=63).contains(&s.len())
            && s.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(CacheError::BadTenant(s.to_string()))
        }
    }

    /// The tenant of local CLI use.
    pub fn local() -> Self {
        Self("local".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// potter's own state (spec §4): `$POTTER_HOME`.
#[derive(Debug, Clone)]
pub struct PotterHome {
    root: PathBuf,
}

/// Creates `dir` and its parents, private to the user.
fn private_dir(dir: &Path) -> Result<(), CacheError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io_err(dir, e))
}

impl PotterHome {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CacheError> {
        let root = root.into();
        private_dir(&root)?;
        // Unpacked layers hold set-id files owned by the caller: nobody else may reach them.
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .map_err(|e| io_err(&root, e))?;
        Ok(Self { root })
    }

    /// `$POTTER_HOME`, else `$XDG_DATA_HOME/potter`, else `~/.local/share/potter`.
    pub fn default_root() -> Result<PathBuf, CacheError> {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(p) = env("POTTER_HOME") {
            return Ok(p);
        }
        if let Some(p) = env("XDG_DATA_HOME") {
            return Ok(p.join("potter"));
        }
        env("HOME")
            .map(|h| h.join(".local/share/potter"))
            .ok_or(CacheError::NoHome)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `tenant`'s index.
    pub fn index(&self, tenant: &Tenant) -> Result<Index, CacheError> {
        Index::open(self.root.join("cache").join(tenant.as_str()).join("index"))
    }

    /// Unpacked layer directories, one per layer digest.
    pub fn unpacked(&self) -> PathBuf {
        self.root.join("unpacked")
    }

    /// `tenant`'s persistent cache mounts.
    pub fn mounts(&self, tenant: &Tenant) -> PathBuf {
        self.root.join("mounts").join(tenant.as_str())
    }

    /// Scratch for running steps, on the same filesystem as `unpacked`.
    pub fn runs(&self) -> PathBuf {
        self.root.join("runs")
    }
}

/// `tenant`'s 32-byte key (`tenants/<tenant>/key`, mode 0600), created on first use.
pub fn tenant_key(home: &PotterHome, tenant: &Tenant) -> Result<[u8; 32], CacheError> {
    let dir = home.root.join("tenants").join(tenant.as_str());
    private_dir(&dir)?;
    let path = dir.join("key");
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == 32 => return Ok(bytes.try_into().expect("32 bytes")),
        Ok(_) => {
            return Err(io_err(&path, std::io::Error::other("a tenant key is 32 bytes")));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&path, e)),
    }
    let mut key = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut key))
        .map_err(|e| io_err(Path::new("/dev/urandom"), e))?;
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, &key).and_then(|()| f.sync_all()));
    match created {
        Ok(()) => Ok(key),
        // Another build created it first: use theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => tenant_key(home, tenant),
        Err(e) => Err(io_err(&path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_home_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        PotterHome::open(&root).unwrap();
        assert_eq!(std::fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn tenant_names_are_checked_and_keys_are_private() {
        for bad in ["", "A", "../x", "a/b", ".hidden", &"x".repeat(64)] {
            assert!(Tenant::new(bad).is_err(), "{bad:?}");
        }
        assert!(Tenant::new("acme-1.prod_eu").is_ok());
        let dir = tempfile::tempdir().unwrap();
        let home = PotterHome::open(dir.path()).unwrap();
        let k1 = tenant_key(&home, &Tenant::local()).unwrap();
        assert_eq!(k1, tenant_key(&home, &Tenant::local()).unwrap());
        assert_ne!(k1, tenant_key(&home, &Tenant::new("other").unwrap()).unwrap());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("tenants/local/key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
