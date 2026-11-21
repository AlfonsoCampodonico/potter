//! The mapping between potter's build-VM types (`backend`) and kiln's build mode
//! (`kiln_run::build`), and the classification of every kiln error into a potter exit code.
//! Nothing here needs a VM, so it builds and tests on every platform.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use kiln_run::VmmKind;
use kiln_run::build as kb;
use potter_plan::Command;

pub use kiln_proto::MAX_BUILD_LOWERS;

use super::backend::{BuildSpec, Exit, RunStep, StepOutcome, StepSealed, Stream};
use crate::error::BuildError;

/// The scratch disk holding every step's upper layer (decision D6).
pub const SCRATCH_BYTES: u64 = 8 << 30;
/// How long a VM gets to boot.
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(60);

/// What potter needs besides a [`BuildSpec`] to boot a kiln build VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfig {
    pub vmm: VmmKind,
    pub kernel: PathBuf,
    pub init: PathBuf,
    pub scratch_bytes: u64,
    pub boot_timeout: Duration,
}

impl VmConfig {
    pub fn new(vmm: VmmKind, kernel: PathBuf, init: PathBuf) -> Self {
        Self {
            vmm,
            kernel,
            init,
            scratch_bytes: SCRATCH_BYTES,
            boot_timeout: BOOT_TIMEOUT,
        }
    }
}

/// The devices potter may plan for: the VMM's available ones, capped at the layer and cache
/// disks kiln-proto allows plus the fixed devices (D14).
pub fn device_cap(available: u32) -> usize {
    (available as usize).min(kiln_proto::MAX_LAYERS as usize + kb::FIXED_DEVICES)
}

/// kiln's `BuildSpec` for `spec`. A `cpus` kiln's `u8` cannot hold is an infrastructure error.
pub fn kiln_spec(spec: &BuildSpec, cfg: &VmConfig) -> Result<kb::BuildSpec, BuildError> {
    let cpus = u8::try_from(spec.cpus)
        .map_err(|_| BuildError::Infra(format!("{} vCPUs: a build VM has at most 255", spec.cpus)))?;
    Ok(kb::BuildSpec {
        vmm: cfg.vmm,
        kernel: cfg.kernel.clone(),
        init: cfg.init.clone(),
        layers: spec.layers.clone(),
        root: spec.root,
        cache_disks: spec.cache_disks.clone(),
        output: spec.output.clone(),
        output_size: spec.output_size,
        network: spec.network,
        memory_mib: spec.memory_mib,
        cpus,
        scratch_bytes: cfg.scratch_bytes,
        boot_timeout: cfg.boot_timeout,
        source_date_epoch: spec.source_date_epoch,
    })
}

pub fn kiln_step(step: &RunStep) -> kb::RunStep {
    kb::RunStep {
        id: step.id,
        command: match &step.command {
            Command::Argv(a) => kiln_proto::Command::Argv(a.clone()),
            Command::Shell(s) => kiln_proto::Command::Shell(s.clone()),
        },
        env: step.env.clone(),
        workdir: step.workdir.clone(),
        uid: step.uid,
        gid: step.gid,
        lowers_added: step.lowers_added.clone(),
        mounts: step.mounts.clone(),
        secrets: step
            .secrets
            .iter()
            .map(|(t, s)| (t.clone(), kb::Secret(s.0.clone())))
            .collect(),
        network: step.network,
        timeout: step.timeout,
    }
}

pub fn outcome(o: kb::StepOutcome) -> StepOutcome {
    StepOutcome {
        exit: match o.exit {
            kb::Exit::Code(c) => Exit::Code(c),
            kb::Exit::Signal(s) => Exit::Signal(s),
            kb::Exit::TimedOut => Exit::TimedOut,
        },
        sealed: o.sealed.map(|s| StepSealed {
            id: s.id,
            offset: s.offset,
            len: s.len,
            sha256: s.sha256,
            entries: s.entries,
        }),
    }
}

pub fn stream(s: kb::Stream) -> Stream {
    match s {
        kb::Stream::Stdout => Stream::Stdout,
        kb::Stream::Stderr => Stream::Stderr,
    }
}

/// Any error from booting a VM (D16).
pub fn start_error(e: kiln_run::Error) -> BuildError {
    BuildError::Infra(format!("the build VM did not start: {e}"))
}

/// A refusal of step `step` before it was sent (D2, D3).
pub fn refusal(step: u32, e: kiln_run::Error) -> BuildError {
    match e {
        kiln_run::Error::TooManyLowers { .. } => BuildError::Infra(format!(
            "step {step}: {e} (potter counts lowers and restarts before kiln's cap: this is a potter bug)"
        )),
        e => BuildError::Plan(format!("step {step}: {e}")),
    }
}

/// An error from `run_step` after the step was sent: the VM is gone (D4).
pub fn vm_error(e: kiln_run::Error) -> BuildError {
    let text = e.to_string();
    if text.contains("guest protocol violation") || text.contains("StepSealed for step") {
        BuildError::Hostile(text)
    } else {
        BuildError::Infra(text)
    }
}

/// potter's copy of the state kiln keeps between steps, so a step's refusal can be told apart
/// from a dead VM (D3).
#[derive(Debug, Default)]
pub struct Mirror {
    stacked: BTreeSet<usize>,
    sealed: usize,
}

impl Mirror {
    /// Runs kiln's pre-send checks on `step`.
    pub fn check(&self, spec: &kb::BuildSpec, step: &kb::RunStep) -> Result<(), BuildError> {
        spec.proto_step(step, &self.stacked, self.sealed)
            .map(|_| ())
            .map_err(|e| refusal(step.id, e))
    }

    /// Records a step that was sent. kiln keeps a failed step's layers stacked.
    pub fn record(&mut self, step: &kb::RunStep, outcome: &kb::StepOutcome) {
        self.stacked.extend(step.lowers_added.iter().copied());
        if outcome.sealed.is_some() {
            self.sealed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::vm::backend::Secret;

    fn spec() -> BuildSpec {
        BuildSpec {
            layers: vec!["/l0".into(), "/l1".into()],
            root: 1,
            cache_disks: vec![("/c".into(), "/root/.cache".into())],
            output: "/o".into(),
            output_size: 1 << 20,
            network: true,
            memory_mib: 4096,
            cpus: 2,
            source_date_epoch: Some(0),
        }
    }

    fn cfg() -> VmConfig {
        VmConfig::new(VmmKind::Firecracker, "/k".into(), "/i".into())
    }

    fn step(id: u32) -> RunStep {
        RunStep {
            id,
            command: Command::shell("true"),
            env: Default::default(),
            workdir: "/".into(),
            uid: 0,
            gid: 0,
            lowers_added: vec![],
            mounts: vec![],
            secrets: vec![],
            network: false,
            timeout: Duration::from_secs(60),
        }
    }

    #[test]
    fn a_potter_spec_maps_field_for_field() {
        let k = kiln_spec(&spec(), &cfg()).unwrap();
        assert_eq!(
            (k.vmm, k.kernel.as_path(), k.init.as_path()),
            (VmmKind::Firecracker, Path::new("/k"), Path::new("/i"))
        );
        assert_eq!((k.layers.len(), k.root, k.cache_disks.len()), (2, 1, 1));
        assert_eq!(
            (k.network, k.memory_mib, k.cpus, k.source_date_epoch),
            (true, 4096, 2u8, Some(0))
        );
        assert_eq!((k.scratch_bytes, k.boot_timeout), (SCRATCH_BYTES, BOOT_TIMEOUT));
        let too_many = BuildSpec { cpus: 256, ..spec() };
        assert_eq!(kiln_spec(&too_many, &cfg()).unwrap_err().exit_code(), 4);
    }

    #[test]
    fn commands_secrets_and_outcomes_map_unchanged() {
        let mut s = step(3);
        s.command = Command::argv(&["make", "x"]);
        s.secrets = vec![("/run/secrets/npm".into(), Secret(b"tok".to_vec()))];
        let k = kiln_step(&s);
        assert_eq!(k.command, kiln_proto::Command::Argv(vec!["make".into(), "x".into()]));
        assert_eq!(
            (k.secrets[0].0.as_str(), k.secrets[0].1.0.as_slice()),
            ("/run/secrets/npm", &b"tok"[..])
        );
        assert_eq!(kiln_step(&step(1)).command, kiln_proto::Command::Shell("true".into()));
        let sealed = kiln_proto::StepSealed {
            id: 3,
            offset: 0,
            len: 4096,
            sha256: "ab".repeat(32),
            entries: 2,
        };
        let o = outcome(kb::StepOutcome {
            exit: kb::Exit::Code(0),
            sealed: Some(sealed),
        });
        assert_eq!(o.exit, Exit::Code(0));
        assert_eq!(
            o.sealed.unwrap(),
            StepSealed {
                id: 3,
                offset: 0,
                len: 4096,
                sha256: "ab".repeat(32),
                entries: 2
            }
        );
        for (k, p) in [
            (kb::Exit::Signal(9), Exit::Signal(9)),
            (kb::Exit::TimedOut, Exit::TimedOut),
        ] {
            assert_eq!(outcome(kb::StepOutcome { exit: k, sealed: None }).exit, p);
        }
        assert_eq!(stream(kb::Stream::Stderr), Stream::Stderr);
    }

    #[test]
    fn refusals_before_a_step_is_sent_are_plan_errors_except_the_lowers_cap() {
        let k = kiln_spec(&spec(), &cfg()).unwrap();
        let m = Mirror::default();
        let mut bad_env = step(1);
        bad_env.env.insert("A=B".into(), "1".into());
        assert_eq!(m.check(&k, &kiln_step(&bad_env)).unwrap_err().exit_code(), 2);
        let mut big = step(1);
        big.secrets = vec![("/run/secrets/big".into(), Secret(vec![b'x'; 50 << 10]))];
        let e = m.check(&k, &kiln_step(&big)).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{e}");
        let mut slash = step(1);
        slash.secrets = vec![("/run/secrets/x/".into(), Secret(b"s".to_vec()))];
        assert_eq!(m.check(&k, &kiln_step(&slash)).unwrap_err().exit_code(), 2);
        let full = Mirror {
            stacked: BTreeSet::new(),
            sealed: MAX_BUILD_LOWERS,
        };
        assert_eq!(full.check(&k, &kiln_step(&step(1))).unwrap_err().exit_code(), 4);
        m.check(&k, &kiln_step(&step(1))).unwrap();
    }

    #[test]
    fn errors_after_a_step_is_sent_are_infra_unless_the_guest_broke_the_protocol() {
        let v = kiln_run::Error::Refused("step 3: guest protocol violation: StepExited for step 9 out of order".into());
        assert_eq!(vm_error(v).exit_code(), 3);
        let seal = kiln_run::Error::Refused("step 3: StepSealed for step 3: range 8192+4096 is refused: x".into());
        assert_eq!(vm_error(seal).exit_code(), 3);
        let gone = kiln_run::Error::Refused("step 3: the VM ended (Exited)".into());
        assert_eq!(vm_error(gone).exit_code(), 4);
        assert_eq!(
            start_error(kiln_run::Error::Refused("Hello 2 (build mode needs version 3)".into())).exit_code(),
            4
        );
    }

    #[test]
    fn the_mirror_counts_layers_a_failed_step_stacked() {
        let k = kiln_spec(&spec(), &cfg()).unwrap();
        let mut m = Mirror::default();
        let mut s = step(1);
        s.lowers_added = vec![1];
        let ks = kiln_step(&s);
        m.check(&k, &ks).unwrap();
        m.record(
            &ks,
            &kb::StepOutcome {
                exit: kb::Exit::Code(125),
                sealed: None,
            },
        );
        assert!(
            m.check(&k, &ks).is_err(),
            "layer 1 is stacked even though the step failed"
        );
        assert_eq!(device_cap(31), 31);
        assert_eq!(device_cap(400), 132);
    }
}
