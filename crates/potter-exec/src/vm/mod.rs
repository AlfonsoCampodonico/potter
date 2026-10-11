//! `VmExecutor` (spec §9.3): build chains in microVMs. The VM itself comes from kiln's build
//! mode (kiln M3e) behind [`backend::BuildBackend`]; potter owns the disks it attaches, the
//! steps it sends, and the ingest of what comes back, which trusts nothing the guest reports.

pub mod backend;
#[cfg(test)]
mod chain_tests;
pub mod disks;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod ingest;

use std::path::PathBuf;

use kiln_store::Store;
use potter_cache::PotterHome;

use crate::chain::LayerRef;
use crate::error::{BuildError, io};
use backend::{BuildBackend, BuildSpec, BuildVm, RunStep, StepOutcome};
use ingest::OutputDisk;

/// The output disk's size: the cap on everything one VM's steps produce (spec §9.3).
pub const OUTPUT_SIZE: u64 = 8 << 30;
/// spec §10: --step-mem 4 GiB and --step-cpus 2 by default.
pub const MEMORY_MIB: u32 = 4096;
pub const CPUS: u32 = 2;

/// One VM running the rest of a chain from its first miss. Dropping it tears the VM down and
/// deletes its output disk.
pub struct Session {
    vm: Option<Box<dyn BuildVm>>,
    pub disk: OutputDisk,
    dir: PathBuf,
    /// Device slots left for layers built mid-chain.
    spare: usize,
    /// Layer disks attached since the last step, for the next one's `lowers_added`.
    pending: Vec<PathBuf>,
    next_id: u32,
}

impl Session {
    /// Boots a VM over `prefix` (bottom first), keeping up to `reserve` device slots for layers
    /// built later in the chain.
    pub fn start(
        backend: &dyn BuildBackend,
        store: &Store,
        home: &PotterHome,
        prefix: &[LayerRef],
        reserve: usize,
        network: bool,
    ) -> Result<Self, BuildError> {
        let plan = disks::plan_disks(prefix.len(), 0, backend.max_devices(), reserve)?;
        let mut layers = Vec::new();
        let rest = if plan.squash > 0 {
            layers.push(disks::squashed(store, home, &prefix[..plan.squash])?);
            &prefix[plan.squash..]
        } else {
            prefix
        };
        for l in rest {
            layers.push(disks::erofs_of(store, home, l)?);
        }
        std::fs::create_dir_all(home.runs()).map_err(io("creating the runs directory"))?;
        let dir = tempfile::Builder::new()
            .prefix("vm-")
            .tempdir_in(home.runs())
            .map_err(io("creating a VM directory"))?
            .keep();
        let output = dir.join("output.img");
        let made = std::fs::File::create(&output).and_then(|f| f.set_len(OUTPUT_SIZE));
        if let Err(e) = made {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(io("creating the output disk")(e));
        }
        let spec = BuildSpec {
            layers,
            cache_disks: vec![],
            output: output.clone(),
            output_size: OUTPUT_SIZE,
            network,
            memory_mib: MEMORY_MIB,
            cpus: CPUS,
        };
        let vm = match backend.start(spec) {
            Ok(vm) => vm,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        Ok(Self {
            vm: Some(vm),
            disk: OutputDisk::new(&output, OUTPUT_SIZE),
            dir,
            spare: plan.spare,
            pending: Vec::new(),
            next_id: 1,
        })
    }

    /// Attaches a layer built mid-chain for the next step; `false` when no slot is left and the
    /// chain must continue in a new VM.
    pub fn attach(&mut self, disk: PathBuf) -> bool {
        if self.spare == 0 {
            return false;
        }
        self.spare -= 1;
        self.pending.push(disk);
        true
    }

    /// Runs `step`, numbered and given the disks attached since the previous one.
    pub fn run(&mut self, mut step: RunStep, sink: &mut dyn FnMut(&str)) -> Result<StepOutcome, BuildError> {
        step.id = self.next_id;
        self.next_id += 1;
        step.lowers_added = std::mem::take(&mut self.pending);
        self.vm.as_mut().expect("a live VM").run_step(step, sink)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            let _ = vm.finish();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
