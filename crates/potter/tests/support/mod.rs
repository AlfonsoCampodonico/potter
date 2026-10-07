//! Fixtures for the tests that need Linux namespaces: skipped without `VMKIT_SANDBOX` (a built
//! `vmkit-sandbox` that may create user namespaces), `newuidmap` and a subordinate id range.
#![allow(dead_code)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use kiln_oci::{Descriptor, media};
use kiln_store::Store;
use potter_cache::PotterHome;
use potter_exec::BuildError;
use potter_exec::sandboxed::SandboxCtx;

pub struct Ctx {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub home: PotterHome,
    pub sandbox: SandboxCtx,
    pub subuid_start: u32,
}

/// The caller's first subordinate uid.
fn subuid_start() -> u32 {
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap();
    let name = passwd
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.get(2) == Some(&uid.to_string().as_str())).then(|| f[0].to_string())
        })
        .unwrap();
    std::fs::read_to_string("/etc/subuid")
        .unwrap()
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f[0] == name || f[0] == uid.to_string()).then(|| f[1].parse().unwrap())
        })
        .unwrap()
}

impl Ctx {
    pub fn new() -> Option<Self> {
        if std::env::var_os("VMKIT_SANDBOX").is_none() {
            assert!(
                std::env::var_os("POTTER_REQUIRE_SANDBOX_TESTS").is_none_or(|v| v != "1"),
                "POTTER_REQUIRE_SANDBOX_TESTS is set but VMKIT_SANDBOX is not"
            );
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("kiln")).unwrap();
        let home = PotterHome::open(dir.path().join("potter")).unwrap();
        let sandbox = SandboxCtx {
            potter: PathBuf::from(env!("CARGO_BIN_EXE_potter")),
            runs: home.runs(),
        };
        Some(Self {
            dir,
            store,
            home,
            sandbox,
            subuid_start: subuid_start(),
        })
    }

    /// Stores `tar` as an uncompressed layer blob.
    pub fn layer(&self, tar: &[u8]) -> Descriptor {
        let digest = self.store.put_bytes(tar).unwrap();
        Descriptor::new(media::OCI_LAYER_TAR, digest, tar.len() as u64)
    }

    pub fn unpack(&self, tar: &[u8]) -> Result<PathBuf, BuildError> {
        potter_exec::unpack::ensure_unpacked(&self.sandbox, &self.store, &self.layer(tar), &self.home)
    }
}

pub fn xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    let n = rustix::fs::lgetxattr(path, name, &mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}
