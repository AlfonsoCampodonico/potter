//! potter's own helpers run inside vmkit's sandbox: files owned by subordinate ids can be
//! created, read and removed only from inside the user namespace that maps them.

use std::path::{Path, PathBuf};
use std::process::Output;

use vmkit::sandbox::{self, Command, Ids, Limits, Mount, Network, Program, Root, Spec};

use crate::error::BuildError;

/// Subordinate ids mapped in every sandbox potter starts (spec §9.4).
pub const IDS: u32 = 65536;

/// `open_files` and `processes`, capped at the caller's own hard limits: a sandbox cannot
/// raise a hard limit.
pub fn limits(open_files: u64, processes: u64) -> Limits {
    use rustix::process::{Resource, getrlimit};
    let hard = |r: Resource, want: u64| getrlimit(r).maximum.map_or(want, |max| want.min(max));
    Limits {
        open_files: hard(Resource::Nofile, open_files),
        processes: hard(Resource::Nproc, processes),
        cgroup: None,
    }
}

/// How potter starts sandboxes.
#[derive(Debug, Clone)]
pub struct SandboxCtx {
    /// The `potter` binary, run as `potter __unpack` / `__seal` inside the sandbox.
    pub potter: PathBuf,
    /// Where each sandbox's run directory is made.
    pub runs: PathBuf,
}

/// Held for a whole build: other builds may run alongside, the sweep may not.
pub struct BuildLock {
    _file: std::fs::File,
}

/// Takes the build lock. When no other build runs, first removes what killed builds left
/// behind: step directories and half-unpacked layers, which hold files of subordinate owners
/// that only the sandbox can delete.
pub fn begin_build(ctx: &SandboxCtx, home: &potter_cache::PotterHome) -> Result<BuildLock, BuildError> {
    use rustix::fs::{FlockOperation, flock};
    let path = home.root().join("build.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(crate::error::io(format!("opening {}", path.display())))?;
    if flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok() {
        let stale = |dir: PathBuf, hidden_only: bool| -> Vec<PathBuf> {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| !hidden_only || e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| e.path())
                .collect()
        };
        for p in stale(home.runs(), false)
            .into_iter()
            .chain(stale(home.unpacked(), true))
        {
            ctx.remove(&p)?;
        }
    }
    // Shared for the rest of the build (a downgrade when the exclusive lock was taken).
    flock(&file, FlockOperation::LockShared).map_err(|e| crate::error::io("taking the build lock")(e.into()))?;
    Ok(BuildLock { _file: file })
}

/// Exit codes of potter's hidden helpers.
pub const HELPER_HOSTILE: i32 = 3;
pub const HELPER_TOO_LARGE: i32 = 5;

impl SandboxCtx {
    /// Runs `program args` as root of the sandbox's user namespace, in the read-only host
    /// root, with `writable` directories (which must exist) bound at their own paths.
    pub fn run_host(&self, program: &Path, args: &[String], writable: &[PathBuf]) -> Result<Output, BuildError> {
        std::fs::create_dir_all(&self.runs).map_err(crate::error::io(format!("creating {}", self.runs.display())))?;
        let run_dir = tempfile::Builder::new()
            .prefix("helper-")
            .tempdir_in(&self.runs)
            .map_err(crate::error::io("creating a run directory"))?;
        let spec = Spec {
            command: Command {
                program: Program::Path(program.to_path_buf()),
                arg0: None,
                args: args.to_vec(),
                env: Some(vec![("PATH".into(), "/usr/bin:/bin".into())]),
                cwd: "/".into(),
                user: None,
            },
            root: Root::Host,
            mounts: writable
                .iter()
                .map(|p| Mount::Bind {
                    source: p.clone(),
                    target: p.clone(),
                    writable: true,
                })
                .collect(),
            ids: Ids::Subordinate { count: IDS },
            network: Network::None,
            nest: false,
            seccomp: false,
            limits: limits(1 << 16, 1024),
            run_dir: run_dir.path().to_path_buf(),
        };
        sandbox::spawn(&spec, sandbox::Stdio::piped())
            .and_then(|s| s.wait_with_output().map_err(vmkit::Error::from))
            .map_err(|e| BuildError::Infra(format!("the sandbox: {e}")))
    }

    /// Removes `path` (which may hold files owned by subordinate ids), from inside the sandbox.
    pub fn remove(&self, path: &Path) -> Result<(), BuildError> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| BuildError::Infra(format!("cannot remove {}", path.display())))?;
        let out = self.run_host(
            Path::new("/bin/rm"),
            &["-rf".into(), "--".into(), path.display().to_string()],
            &[parent.to_path_buf()],
        )?;
        if out.status.success() {
            Ok(())
        } else {
            Err(BuildError::Infra(format!(
                "removing {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }
}

/// The error for a failed helper run: a violation when it says so, else infrastructure.
pub fn helper_error(what: &str, out: &Output) -> BuildError {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let detail = stderr.strip_prefix("error: ").unwrap_or(&stderr).to_string();
    if out.status.code() == Some(HELPER_HOSTILE) {
        BuildError::Hostile(format!("{what}: {detail}"))
    } else if out.status.code() == Some(HELPER_TOO_LARGE) {
        BuildError::StepOutput(detail)
    } else {
        BuildError::Infra(format!("{what} ({}): {stderr}", out.status))
    }
}
