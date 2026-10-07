//! `NsExecutor` (spec §9.4): one `Run` step in vmkit's sandbox. The step sees an overlay of
//! its lowers with a fresh upper, runs in a nested user namespace under seccomp, and holds no
//! capability over the sandbox's network namespace or its policy.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use potter_plan::Command as StepCommand;
use vmkit::sandbox::{self, Command, Ids, Mount, Network, Program, Root, Spec, User};

use crate::error::{BuildError, io};
use crate::sandboxed::IDS;

/// Lines of output kept for a failure report (spec §12).
pub const TAIL: usize = 200;

pub struct StepRun<'a> {
    /// Top first.
    pub lowers: Vec<PathBuf>,
    /// Empty, on the same filesystem as `work`.
    pub upper: PathBuf,
    pub work: PathBuf,
    /// Arguments to `/bin/sh` (see [`wrap`]).
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub user: User,
    pub network: bool,
    /// Host directory and its target inside the step.
    pub cache_mounts: Vec<(PathBuf, String)>,
    pub timeout: Duration,
    /// Holds the sandbox's own files and the step's `resolv.conf`.
    pub run_dir: PathBuf,
    /// Called with each line of the step's output.
    pub log: &'a mut dyn FnMut(&str),
}

#[derive(Debug)]
pub struct StepOutcome {
    pub status: ExitStatus,
    pub tail: Vec<String>,
    pub timed_out: bool,
}

/// `/bin/sh` arguments that create and enter `workdir`, then run `command` (`Docker`'s WORKDIR).
pub fn wrap(command: &StepCommand, workdir: &str) -> Vec<String> {
    let mut args = vec![
        "-c".to_string(),
        r#"mkdir -p -- "$0" && cd -- "$0" && exec "$@""#.to_string(),
        workdir.to_string(),
    ];
    match command {
        StepCommand::Argv(argv) => args.extend(argv.iter().cloned()),
        StepCommand::Shell(script) => args.extend(["/bin/sh".to_string(), "-c".to_string(), script.clone()]),
    }
    args
}

/// Mount points the sandbox may create in the upper: left out of the diff when still empty.
pub fn mount_points(r: &StepRun) -> Vec<String> {
    let mut points: Vec<String> = r
        .cache_mounts
        .iter()
        .map(|(_, t)| t.trim_start_matches('/').to_string())
        .collect();
    if r.network {
        points.push("etc/resolv.conf".into());
    }
    points
}

/// Runs the step to completion (or its timeout).
pub fn run_step(r: StepRun) -> Result<StepOutcome, BuildError> {
    std::fs::create_dir_all(&r.run_dir).map_err(io(format!("creating {}", r.run_dir.display())))?;
    let mut mounts: Vec<Mount> = r
        .cache_mounts
        .iter()
        .map(|(source, target)| Mount::Bind {
            source: source.clone(),
            target: target.into(),
            writable: true,
        })
        .collect();
    if r.network {
        let resolv = r.run_dir.join("resolv.conf");
        std::fs::write(&resolv, format!("nameserver {}\n", vmkit::net::NAMESERVER))
            .map_err(io("writing resolv.conf"))?;
        mounts.push(Mount::Bind {
            source: resolv,
            target: "/etc/resolv.conf".into(),
            writable: false,
        });
    }
    let mut env = r.env.clone();
    env.entry("HOME".into())
        .or_insert_with(|| if r.user.uid == 0 { "/root".into() } else { "/".into() });
    let spec = Spec {
        command: Command {
            program: Program::Path("/bin/sh".into()),
            arg0: None,
            args: r.args.clone(),
            env: Some(env.into_iter().collect()),
            cwd: "/".into(),
            user: Some(r.user.clone()),
        },
        root: Root::Overlay {
            lowers: r.lowers.clone(),
            upper: r.upper.clone(),
            work: r.work.clone(),
        },
        mounts,
        ids: Ids::Subordinate { count: IDS },
        network: if r.network {
            Network::Egress(vmkit::NetSpec::default())
        } else {
            Network::None
        },
        nest: true,
        seccomp: true,
        limits: vmkit::sandbox::Limits {
            // spec §10: --step-mem 4 GiB and --step-cpus 2 by default.
            cgroup: Some(vmkit::sandbox::Cgroup {
                memory_mib: 4096,
                cpu_percent: 200,
                tasks: 16384,
            }),
            ..crate::sandboxed::limits(1 << 20, 16384)
        },
        run_dir: r.run_dir.join("sandbox"),
    };
    let mut child = sandbox::spawn(&spec, sandbox::Stdio::piped())
        .map_err(|e| BuildError::Infra(format!("starting the step's sandbox: {e}")))?;
    let (tx, rx) = mpsc::channel::<String>();
    let readers: Vec<_> = [
        child.stdout().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child.stderr().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).split(b'\n') {
                let Ok(line) = line else { break };
                if tx.send(String::from_utf8_lossy(&line).into_owned()).is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(tx);
    let deadline = Instant::now() + r.timeout;
    let mut tail = VecDeque::with_capacity(TAIL);
    let mut timed_out = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                (r.log)(&line);
                if tail.len() == TAIL {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                let _ = child.kill();
                break;
            }
        }
    }
    let status = child.wait().map_err(io("waiting for the step"))?;
    for r in readers {
        let _ = r.join();
    }
    // Lines that arrived after a timeout.
    for line in rx.try_iter() {
        if tail.len() == TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    Ok(StepOutcome {
        status,
        tail: tail.into(),
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_creates_the_workdir_and_keeps_the_argv() {
        assert_eq!(
            wrap(&StepCommand::argv(&["npm", "ci"]), "/app"),
            [
                "-c",
                r#"mkdir -p -- "$0" && cd -- "$0" && exec "$@""#,
                "/app",
                "npm",
                "ci"
            ]
        );
        assert_eq!(
            wrap(&StepCommand::shell("a && b"), "/w")[3..],
            ["/bin/sh", "-c", "a && b"]
        );
    }
}
