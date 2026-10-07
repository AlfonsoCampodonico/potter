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

/// A busybox root: the host's static busybox and its applets, `/etc/passwd` with root and `app`
/// (1000, home `/home/app`), and the usual directories.
pub fn busybox_root() -> Vec<u8> {
    use kiln_erofs::testtar::{Opts, TarBuilder};
    let dir = Opts::default().mode(0o755);
    let file = Opts::default().mode(0o644);
    let mut t = TarBuilder::new();
    for d in ["bin", "etc", "proc", "sys", "dev", "root", "home"] {
        t.dir(d, &dir);
    }
    t.dir("tmp", &Opts::default().mode(0o1777));
    t.dir("home/app", &Opts::default().mode(0o755).uid(1000).gid(1000));
    let busybox = std::fs::read("/usr/bin/busybox").expect("busybox-static at /usr/bin/busybox");
    t.file("bin/busybox", &busybox, &Opts::default().mode(0o755));
    for applet in [
        "sh", "mkdir", "echo", "rm", "id", "pwd", "test", "sleep", "cat", "ls", "touch", "chown", "wc", "env", "printf",
    ] {
        t.symlink(&format!("bin/{applet}"), "busybox", &dir);
    }
    t.file(
        "etc/passwd",
        b"root:x:0:0::/root:/bin/sh\napp:x:1000:1000::/home/app:/bin/sh\n",
        &file,
    );
    t.file("etc/group", b"root:x:0:\napp:x:1000:\n", &file);
    t.file("etc/motd", b"hello\n", &file);
    t.finish()
}

/// A step's run: where its upper and run directory are.
pub struct Ran {
    pub outcome: potter_exec::ns::StepOutcome,
    pub upper: PathBuf,
    pub run: PathBuf,
    pub lines: Vec<String>,
}

impl Ctx {
    /// The unpacked busybox root.
    pub fn busybox_lower(&self) -> PathBuf {
        self.unpack(&busybox_root()).unwrap()
    }

    /// `script` in `workdir` as `uid:gid` over `lower`, with `HOME` from the fixture's passwd.
    pub fn run(&self, lower: &Path, script: &str, workdir: &str, ids: (u32, u32), timeout: std::time::Duration) -> Ran {
        let runs = tempfile::Builder::new()
            .prefix("step-")
            .tempdir_in(self.home.runs())
            .unwrap()
            .keep();
        for d in ["upper", "work", "run"] {
            std::fs::create_dir_all(runs.join(d)).unwrap();
        }
        let home = if ids.0 == 1000 { "/home/app" } else { "/root" };
        let mut lines = Vec::new();
        let mut log = |l: &str| lines.push(l.to_string());
        let outcome = potter_exec::ns::run_step(potter_exec::ns::StepRun {
            lowers: vec![lower.to_path_buf()],
            upper: runs.join("upper"),
            work: runs.join("work"),
            args: potter_exec::ns::wrap(&potter_plan::Command::shell(script), workdir),
            env: [
                ("PATH".to_string(), "/bin".to_string()),
                ("HOME".to_string(), home.to_string()),
            ]
            .into(),
            user: vmkit::sandbox::User {
                uid: ids.0,
                gid: ids.1,
                groups: vec![],
            },
            network: false,
            cache_mounts: vec![],
            timeout,
            run_dir: runs.join("run"),
            log: &mut log,
        })
        .unwrap();
        Ran {
            outcome,
            upper: runs.join("upper"),
            run: runs.join("run"),
            lines,
        }
    }

    /// Seals a step's upper and lists the diff's (path, type).
    pub fn seal(&self, ran: &Ran) -> Result<Vec<(String, char)>, BuildError> {
        let (digest, _) = potter_exec::seal::seal_in_sandbox(&self.sandbox, &ran.upper, &ran.run, &self.store, &[])?;
        let blob = self.store.open_blob(&digest).unwrap();
        let mut a = tar::Archive::new(blob);
        Ok(a.entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let kind = match e.header().entry_type() {
                    tar::EntryType::Directory => 'd',
                    tar::EntryType::Symlink => 'l',
                    tar::EntryType::Link => 'h',
                    tar::EntryType::Regular => 'f',
                    _ => '?',
                };
                (e.path().unwrap().display().to_string(), kind)
            })
            .collect())
    }
}

pub fn xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    let n = rustix::fs::lgetxattr(path, name, &mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}
