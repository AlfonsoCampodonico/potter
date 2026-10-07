//! potter's own helpers run inside vmkit's sandbox: files owned by subordinate ids can be
//! created, read and removed only from inside the user namespace that maps them.

use std::path::{Path, PathBuf};
use std::process::Output;

use vmkit::sandbox::{self, Command, Ids, Limits, Mount, Network, Program, Root, Spec};

use crate::error::BuildError;

/// Subordinate ids mapped in every sandbox potter starts (spec §9.4).
pub const IDS: u32 = 65536;

/// How potter starts sandboxes.
#[derive(Debug, Clone)]
pub struct SandboxCtx {
    /// The `potter` binary, run as `potter __unpack` / `__seal` inside the sandbox.
    pub potter: PathBuf,
    /// Where each sandbox's run directory is made.
    pub runs: PathBuf,
}

/// Exit codes of potter's hidden helpers.
pub const HELPER_HOSTILE: i32 = 3;

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
            limits: Limits {
                open_files: 1 << 16,
                processes: 1024,
                cgroup: None,
            },
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
    if out.status.code() == Some(HELPER_HOSTILE) {
        BuildError::Hostile(format!("{what}: {stderr}"))
    } else {
        BuildError::Infra(format!("{what} ({}): {stderr}", out.status))
    }
}
