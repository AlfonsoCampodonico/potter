//! What potter needs from a build VM (spec §9.3), as a trait: kiln's build mode (kiln M3e,
//! `kiln_run::build`) implements it in P4b; tests use [`super::fake`].
//!
//! potter creates the output disk and owns its ingest; the backend attaches it, boots the VM
//! over the layer disks, runs one step at a time and reports where each step's diff landed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use potter_plan::Command;

pub use super::ingest::StepSealed;
use crate::error::BuildError;

/// The disks the VM holds besides its fixed ones (init, scratch, output, vsock, net).
pub const FIXED_DEVICES: usize = 5;

/// One VM's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSpec {
    /// Read-only erofs layer disks, bottom first.
    pub layers: Vec<PathBuf>,
    /// Cache-mount disks and their targets (spec §8.5).
    pub cache_disks: Vec<(PathBuf, String)>,
    /// The output disk potter created: a sparse raw file of `output_size` bytes.
    pub output: PathBuf,
    pub output_size: u64,
    /// Whether any step of this VM has network (the egress policy of spec §9.5).
    pub network: bool,
    pub memory_mib: u32,
    pub cpus: u32,
}

/// One step to run (kiln-proto build mode `RunStep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStep {
    pub id: u32,
    pub command: Command,
    pub env: BTreeMap<String, String>,
    pub workdir: String,
    pub uid: u32,
    pub gid: u32,
    /// Layer disks attached since the VM booted (inputs and copies built mid-chain), stacked
    /// above the current root before the step runs, bottom first.
    pub lowers_added: Vec<PathBuf>,
    /// Targets of the cache disks this step mounts.
    pub mounts: Vec<String>,
    /// Secret targets and values: written by the guest to a tmpfs, never to the upper.
    pub secrets: Vec<(String, Vec<u8>)>,
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
    /// Runs `step`, streaming each line of its output to `sink`.
    fn run_step(&mut self, step: RunStep, sink: &mut dyn FnMut(&str)) -> Result<StepOutcome, BuildError>;
    /// `Shutdown`, then a kill after the grace period.
    fn finish(self: Box<Self>) -> Result<(), BuildError>;
}
