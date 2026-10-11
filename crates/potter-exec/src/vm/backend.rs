//! What potter needs from a build VM (spec §9.3), as a trait: kiln's build mode (kiln M3e,
//! `kiln_run::build`) implements it in P4b; tests use [`super::fake`].
//!
//! potter creates the output disk and owns its ingest. Every disk is attached at boot (no
//! hotplug): the backend boots the VM over them, runs one step at a time and reports where each
//! step's diff landed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use potter_plan::Command;

pub use super::ingest::StepSealed;
use crate::error::BuildError;

/// Devices every build VM holds besides its layer and cache disks: init, scratch, output, vsock.
pub const BASE_DEVICES: usize = 4;

/// The fixed devices of a VM: the NIC counts only when it has network.
pub fn fixed_devices(network: bool) -> usize {
    BASE_DEVICES + usize::from(network)
}

/// One VM's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSpec {
    /// Read-only erofs layer disks. The first `root` form the root at boot, bottom first; the
    /// rest are stacked later, when a step names them in `lowers_added`.
    pub layers: Vec<PathBuf>,
    pub root: usize,
    /// Cache-mount disks and their targets (spec §8.5).
    pub cache_disks: Vec<(PathBuf, String)>,
    /// The output disk potter created: a sparse raw file of `output_size` bytes.
    pub output: PathBuf,
    pub output_size: u64,
    /// Whether the VM has a NIC, with the egress policy of spec §9.5. Every step of a VM has
    /// the same network: steps without one run in a VM without one.
    pub network: bool,
    pub memory_mib: u32,
    pub cpus: u32,
}

/// A secret value, which no `Debug` output shows.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub Vec<u8>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// One step to run (kiln-proto build mode `RunStep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStep {
    pub id: u32,
    pub command: Command,
    pub env: BTreeMap<String, String>,
    /// Created if missing and entered before the command runs.
    pub workdir: String,
    pub uid: u32,
    pub gid: u32,
    /// Indices into `BuildSpec.layers` of boot-time disks to stack above the current root
    /// before the step runs, bottom first (inputs and copies built for later steps).
    pub lowers_added: Vec<usize>,
    /// Targets of the cache disks this step mounts.
    pub mounts: Vec<String>,
    /// Secret targets and values: written by the guest to a tmpfs, never to the upper.
    pub secrets: Vec<(String, Secret)>,
    /// Egress for this step, within a VM that has network.
    pub network: bool,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    TimedOut,
}

impl Exit {
    pub fn success(self) -> bool {
        self == Exit::Code(0)
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Exit::Code(c) => write!(f, "exit status: {c}"),
            Exit::Signal(s) => write!(f, "signal: {s}"),
            Exit::TimedOut => write!(f, "timed out"),
        }
    }
}

/// Which of the step's streams a chunk of output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// How a step ended: on success, where the guest says its diff is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub exit: Exit,
    pub sealed: Option<StepSealed>,
}

pub trait BuildBackend {
    /// `Capabilities.max_virtio_devices` of the VMM.
    fn max_devices(&self) -> usize;
    /// Boots a VM: `Config{mode: Build}`, then `Hello` and `Ready`.
    fn start(&self, spec: BuildSpec) -> Result<Box<dyn BuildVm>, BuildError>;
}

pub trait BuildVm {
    /// Runs `step`, passing its output to `sink` in chunks as it arrives. Returns within the
    /// step's timeout plus a grace period (`Exit::TimedOut`).
    fn run_step(&mut self, step: RunStep, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<StepOutcome, BuildError>;
    /// `Shutdown`, then a kill after the grace period.
    fn finish(self: Box<Self>) -> Result<(), BuildError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_bytes_never_reach_debug_output() {
        let step = RunStep {
            id: 1,
            command: Command::shell("true"),
            env: BTreeMap::new(),
            workdir: "/".into(),
            uid: 0,
            gid: 0,
            lowers_added: vec![],
            mounts: vec![],
            secrets: vec![("/run/secrets/npm".into(), Secret(b"hunter2".to_vec()))],
            network: false,
            timeout: Duration::from_secs(1),
        };
        let shown = format!("{step:?} {:?}", step.secrets[0].1);
        assert!(!shown.contains("hunter2"), "{shown}");
        // Vec<u8>'s Debug prints the bytes as numbers.
        assert!(!shown.contains("104, 117"), "{shown}");
        assert!(shown.contains("/run/secrets/npm"));
    }

    #[test]
    fn the_nic_counts_only_in_a_networked_vm() {
        assert_eq!(fixed_devices(false), 4);
        assert_eq!(fixed_devices(true), 5);
    }
}
