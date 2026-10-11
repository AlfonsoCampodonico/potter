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

use std::collections::BTreeMap;
use std::path::PathBuf;

use kiln_store::Store;
use potter_cache::PotterHome;

use crate::chain::LayerRef;
use crate::error::{BuildError, io};
use backend::{BuildBackend, BuildSpec, BuildVm, RunStep, StepOutcome, Stream};
use ingest::OutputDisk;

/// The output disk's size: the cap on everything one VM's steps produce (spec §9.3).
pub const OUTPUT_SIZE: u64 = 8 << 30;
/// spec §10: --step-mem 4 GiB and --step-cpus 2 by default.
pub const MEMORY_MIB: u32 = 4096;
pub const CPUS: u32 = 2;

/// Joins a step's output chunks into lines, per stream; potter redacts and reports whole lines.
#[derive(Default)]
pub struct Lines {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Lines {
    /// Adds `bytes` from `stream`, passing each line it completes to `line`.
    pub fn feed(&mut self, stream: Stream, bytes: &[u8], line: &mut dyn FnMut(&str)) {
        let buf = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        buf.extend_from_slice(bytes);
        while let Some(n) = buf.iter().position(|&b| b == b'\n') {
            let rest = buf.split_off(n + 1);
            line(&String::from_utf8_lossy(&buf[..n]));
            *buf = rest;
        }
    }

    /// Passes on what is left without a newline.
    pub fn flush(&mut self, line: &mut dyn FnMut(&str)) {
        for buf in [&mut self.stdout, &mut self.stderr] {
            if !buf.is_empty() {
                line(&String::from_utf8_lossy(buf));
                buf.clear();
            }
        }
    }
}

/// One VM running a chain from a miss. Every disk is attached at boot: the prefix's layers
/// (bottom ones squashed when they don't fit) and as many of the chain's later inputs and
/// copies as fit, stacked when their step is reached. Dropping it tears the VM down and
/// deletes its output disk.
pub struct Session {
    vm: Option<Box<dyn BuildVm>>,
    pub disk: OutputDisk,
    dir: PathBuf,
    network: bool,
    /// Chain step index → index in `BuildSpec.layers` of the layer it built, attached at boot.
    boot: BTreeMap<usize, usize>,
    /// Disks reached since the last step, for the next one's `lowers_added`.
    pending: Vec<usize>,
    next_id: u32,
}

impl Session {
    /// Boots a VM over `prefix` (bottom first) with `network`, attaching as many of `later`
    /// (layers the host built for the chain's later steps, by step index, in order) as fit.
    pub fn start(
        backend: &dyn BuildBackend,
        store: &Store,
        home: &PotterHome,
        prefix: &[LayerRef],
        later: &[(usize, LayerRef)],
        network: bool,
    ) -> Result<Self, BuildError> {
        let plan = disks::plan_disks(prefix.len(), 0, backend.max_devices(), later.len(), network)?;
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
        let root = layers.len();
        let mut boot = BTreeMap::new();
        for (step, l) in later.iter().take(plan.spare) {
            boot.insert(*step, layers.len());
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
            root,
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
            network,
            boot,
            pending: Vec::new(),
            next_id: 1,
        })
    }

    pub fn network(&self) -> bool {
        self.network
    }

    /// The chain reached step `step`, which built a layer: it is stacked before the next step
    /// when it was attached at boot. `false` when it wasn't, and the chain needs a new VM.
    pub fn reached(&mut self, step: usize) -> bool {
        match self.boot.get(&step) {
            Some(&disk) => {
                self.pending.push(disk);
                true
            }
            None => false,
        }
    }

    /// Runs `step`, numbered and given the disks reached since the previous one. A report
    /// naming another step is a protocol violation.
    pub fn run(&mut self, mut step: RunStep, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<StepOutcome, BuildError> {
        let id = self.next_id;
        self.next_id += 1;
        step.id = id;
        step.lowers_added = std::mem::take(&mut self.pending);
        let outcome = self.vm.as_mut().expect("a live VM").run_step(step, sink)?;
        if let Some(sealed) = &outcome.sealed
            && sealed.id != id
        {
            return Err(BuildError::Hostile(format!(
                "the guest reported step {} sealed while step {id} ran",
                sealed.id
            )));
        }
        Ok(outcome)
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
