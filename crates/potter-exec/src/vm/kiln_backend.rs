//! potter's build VM over kiln's build mode (`kiln_run::BuildVm`, Linux only). The mapping and
//! the error classes live in [`super::kiln_map`]; this file only wires them to kiln.

use kiln_run::build as kb;
use kiln_store::Store;
use vmkit::Vmm;

use super::backend::{BuildBackend, BuildSpec, BuildVm, RunStep, StepOutcome, Stream};
use super::kiln_map::{Mirror, VmConfig, device_cap, kiln_spec, kiln_step, outcome, start_error, stream, vm_error};
use crate::error::BuildError;

/// Boots build VMs from `cfg`'s kernel, kiln-init and VMM over `store`.
pub struct KilnBackend {
    store: Store,
    cfg: VmConfig,
    devices: usize,
}

impl KilnBackend {
    /// Checks the kernel and kiln-init are files and the VMM can be found, before any VM.
    pub fn new(store: &Store, cfg: VmConfig) -> Result<Self, BuildError> {
        for (what, path) in [("the kernel", &cfg.kernel), ("kiln-init", &cfg.init)] {
            if !path.is_file() {
                return Err(BuildError::Infra(format!("{what} {} is not a file", path.display())));
            }
        }
        let vmm = kiln_run::backend(cfg.vmm)
            .discover()
            .map_err(|e| BuildError::Infra(format!("VM builds need {}: {e}", cfg.vmm.name())))?;
        let devices = device_cap(vmm.capabilities().available_devices());
        Ok(Self {
            store: store.clone(),
            cfg,
            devices,
        })
    }
}

impl BuildBackend for KilnBackend {
    fn max_devices(&self) -> usize {
        self.devices
    }

    fn start(&self, spec: BuildSpec) -> Result<Box<dyn BuildVm>, BuildError> {
        let kspec = kiln_spec(&spec, &self.cfg)?;
        let vm = kiln_run::BuildVm::start(&self.store, kspec.clone()).map_err(start_error)?;
        Ok(Box::new(KilnVm {
            vm,
            spec: kspec,
            mirror: Mirror::default(),
        }))
    }
}

struct KilnVm {
    vm: kiln_run::BuildVm,
    spec: kb::BuildSpec,
    mirror: Mirror,
}

impl BuildVm for KilnVm {
    fn run_step(&mut self, step: RunStep, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<StepOutcome, BuildError> {
        let k = kiln_step(&step);
        self.mirror.check(&self.spec, &k)?;
        let o = self
            .vm
            .run_step(k.clone(), &mut |s, b| sink(stream(s), b))
            .map_err(vm_error)?;
        self.mirror.record(&k, &o);
        Ok(outcome(o))
    }

    fn finish(self: Box<Self>) -> Result<(), BuildError> {
        self.vm
            .finish()
            .map_err(|e| BuildError::Infra(format!("the build VM did not shut down cleanly: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use kiln_run::VmmKind;

    use super::*;

    #[test]
    fn a_missing_kernel_or_kiln_init_is_refused_with_4_before_any_vmm_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        let missing = dir.path().join("missing");
        for (kernel, init, says) in [(&missing, &file, "the kernel"), (&file, &missing, "kiln-init")] {
            let cfg = VmConfig::new(VmmKind::Firecracker, kernel.clone(), init.clone());
            let e = KilnBackend::new(&store, cfg).err().unwrap();
            assert_eq!(e.exit_code(), 4);
            assert!(e.to_string().contains(says), "{e}");
        }
    }
}
