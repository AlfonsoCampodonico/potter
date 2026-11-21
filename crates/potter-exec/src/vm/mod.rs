//! `VmExecutor` (spec §9.3): build chains in microVMs. The VM itself comes from kiln's build
//! mode (kiln M3e) behind [`backend::BuildBackend`]; potter owns the disks it attaches, the
//! steps it sends, and the ingest of what comes back, which trusts nothing the guest reports.

pub mod backend;
pub mod caches;
#[cfg(test)]
mod chain_tests;
pub mod disks;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod ingest;
#[cfg(target_os = "linux")]
pub mod kiln_backend;
pub mod kiln_map;

use std::collections::BTreeMap;
use std::path::PathBuf;

use kiln_store::Store;
use potter_cache::PotterHome;

use crate::chain::LayerRef;
use crate::error::{BuildError, io};
use backend::{BuildBackend, BuildSpec, BuildVm, Exit, RunStep, StepOutcome, Stream};
use caches::{CacheUse, HeldCache};
use ingest::OutputDisk;
use kiln_map::MAX_BUILD_LOWERS;

/// The output disk's size: the cap on everything one VM's steps produce (spec §9.3).
pub const OUTPUT_SIZE: u64 = 8 << 30;
/// spec §10: --step-mem 4 GiB and --step-cpus 2 by default.
pub const MEMORY_MIB: u32 = 4096;
pub const CPUS: u32 = 2;

/// A mount or secret target as kiln-proto wants it: `/` and the non-empty, non-`.` components
/// joined by `/` (decision D10). `what` names the target in errors; `file` is a secret, which
/// can't end in a slash.
pub fn clean_target(what: &str, raw: &str, file: bool) -> Result<String, BuildError> {
    let parts: Vec<&str> = raw.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    if parts.contains(&"..") {
        return Err(BuildError::Plan(format!("{what}: the target {raw:?} has a `..`")));
    }
    if parts.is_empty() {
        return Err(BuildError::Plan(format!("{what}: the target {raw:?} is the root")));
    }
    if file && raw.ends_with('/') {
        return Err(BuildError::Plan(format!(
            "{what}: the target {raw:?} ends in `/`, but a secret is a file"
        )));
    }
    let out = format!("/{}", parts.join("/"));
    kiln_proto::check_target(what, &out).map_err(BuildError::Plan)?;
    Ok(out)
}

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

/// What a VM boots with.
pub struct Boot<'a> {
    /// The chain's layers so far, bottom first.
    pub prefix: &'a [LayerRef],
    /// Layers the host built for the chain's later steps, by step index, in order: as many as
    /// fit are attached.
    pub later: &'a [(usize, LayerRef)],
    /// The cache disks of the VM's segment, one per id (D9).
    pub caches: &'a [CacheUse],
    /// Where the persistent cache disks live: `PotterHome::vm_mounts` of the tenant.
    pub cache_root: PathBuf,
    /// A NIC, for a segment with a networked step (D1).
    pub network: bool,
}

/// One VM running a chain from a miss. Every disk is attached at boot: the prefix's layers
/// (bottom ones squashed when they don't fit) and as many of the chain's later inputs and
/// copies as fit, stacked when their step is reached, and one cache disk per cache id of its
/// segment. Dropping it tears the VM down, deletes its output disk, discards its persistent cache
/// disks if the VM died mid-step (D8) and releases their locks.
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
    /// Lowers kiln counts: the root's, those stacked and those sealed (decision D2).
    lowers: usize,
    /// A step failed in a way that ended the VM, or errored after it was sent: the VM is only
    /// dropped, and its cache disks discarded.
    failed: bool,
    /// The cache uses the VM was booted for.
    uses: Vec<CacheUse>,
    /// Their disks, locks held. Declared after everything the VM uses, so they drop last.
    caches: Vec<HeldCache>,
}

impl Session {
    /// Boots a VM as `boot` says. `warn` gets a warning for each cache disk another build holds.
    pub fn start(
        backend: &dyn BuildBackend,
        store: &Store,
        home: &PotterHome,
        boot: Boot,
        warn: &mut dyn FnMut(String),
    ) -> Result<Self, BuildError> {
        let Boot {
            prefix,
            later,
            caches: uses,
            cache_root,
            network,
        } = boot;
        let plan = disks::plan_disks(prefix.len(), uses.len(), backend.max_devices(), later.len(), network)?;
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
        let mut attached = BTreeMap::new();
        for (step, l) in later.iter().take(plan.spare) {
            attached.insert(*step, layers.len());
            layers.push(disks::erofs_of(store, home, l)?);
        }
        std::fs::create_dir_all(home.runs()).map_err(io("creating the runs directory"))?;
        let dir = tempfile::Builder::new()
            .prefix("vm-")
            .tempdir_in(home.runs())
            .map_err(io("creating a VM directory"))?
            .keep();
        let output = dir.join("output.img");
        let made = std::fs::File::create(&output)
            .and_then(|f| f.set_len(OUTPUT_SIZE))
            .map_err(io("creating the output disk"))
            .and_then(|()| caches::acquire(&cache_root, uses, &dir, warn));
        let held = match made {
            Ok(held) => held,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let spec = BuildSpec {
            layers,
            root,
            cache_disks: held.iter().map(|h| (h.path.clone(), h.target.clone())).collect(),
            output: output.clone(),
            output_size: OUTPUT_SIZE,
            network,
            memory_mib: MEMORY_MIB,
            cpus: CPUS,
            source_date_epoch: Some(crate::tarball::epoch()),
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
            boot: attached,
            pending: Vec::new(),
            next_id: 1,
            lowers: root,
            failed: false,
            uses: uses.to_vec(),
            caches: held,
        })
    }

    /// Whether the next step fits under kiln's lowers cap with what it must stack (D2).
    pub fn has_room(&self) -> bool {
        self.lowers + self.pending.len() <= MAX_BUILD_LOWERS
    }

    /// Shuts the VM down and reports how that went. Dropping a session does the same and
    /// ignores the result.
    pub fn finish(mut self) -> Result<(), BuildError> {
        match self.vm.take() {
            // A VM that failed is dead or dying: its shutdown says nothing worth reporting.
            Some(vm) if self.failed => {
                let _ = vm.finish();
                Ok(())
            }
            Some(vm) => vm.finish(),
            None => Ok(()),
        }
    }

    pub fn network(&self) -> bool {
        self.network
    }

    /// Whether every one of `uses` is a cache disk this VM was booted with.
    pub fn serves(&self, uses: &[CacheUse]) -> bool {
        uses.iter().all(|u| self.uses.contains(u))
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
    pub fn run(
        &mut self,
        mut step: RunStep,
        name: &str,
        sink: &mut dyn FnMut(Stream, &[u8]),
    ) -> Result<StepOutcome, BuildError> {
        let id = self.next_id;
        self.next_id += 1;
        step.id = id;
        step.lowers_added = std::mem::take(&mut self.pending);
        let added = step.lowers_added.len();
        let outcome = match self.vm.as_mut().expect("a live VM").run_step(step, sink) {
            Ok(o) => o,
            Err(e) => {
                // A refusal sent nothing (D3): the VM and its caches are as they were.
                if !matches!(e, BuildError::Plan(_)) {
                    self.failed = true;
                }
                // kiln numbers a refused step by its id; the plan names it.
                return Err(match e {
                    BuildError::Plan(m) => match m.strip_prefix(&format!("step {id}: ")) {
                        Some(rest) => BuildError::Plan(format!("step {name}: {rest}")),
                        None => BuildError::Plan(m),
                    },
                    e => e,
                });
            }
        };
        self.lowers += added + usize::from(outcome.sealed.is_some());
        if outcome.exit == Exit::TimedOut {
            self.failed = true;
        }
        if let Some(sealed) = &outcome.sealed
            && sealed.id != id
        {
            self.failed = true;
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
        // A VM killed mid-step may have left its ext4 disks inconsistent (D8). The locks are
        // released after, when the fields drop.
        if self.failed {
            caches::discard(&self.caches);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
