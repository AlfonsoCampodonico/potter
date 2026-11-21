//! A scripted stand-in for kiln's build VM: each step plays the next [`Script`] entry, writing
//! its diff to the output disk the way kiln-init does and reporting it, honestly or not.

use std::collections::VecDeque;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use kiln_run::VmmKind;
use kiln_run::build as kb;

use super::backend::{BuildBackend, BuildSpec, BuildVm, Exit, RunStep, StepOutcome, Stream};
use super::ingest::{ALIGN, StepSealed};
use super::kiln_map::{Mirror, VmConfig, kiln_spec, kiln_step, start_error, vm_error};
use crate::error::BuildError;

/// How a scripted report lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lie {
    None,
    Sha,
    Entries,
    /// Reports the previous diff's offset.
    Overlap,
    /// Reports another step's id.
    Id,
}

#[derive(Debug, Clone)]
pub enum Script {
    /// The step succeeds, adding `files` (path, contents), printing `lines`.
    Diff {
        files: Vec<(String, Vec<u8>)>,
        lines: Vec<String>,
        lie: Lie,
    },
    /// The step exits with `code`, printing `lines`.
    Fail { code: i32, lines: Vec<String> },
    /// The step runs past its timeout. kiln kills the VM then, so it is dead afterwards.
    Hang,
    /// kiln-init fails to set the step up: exit 125, `line` on stderr (a test passes a line
    /// starting `kiln-init: `).
    SetupFail { line: String },
    /// The VM dies under the step: `run_step` errors and the VM is dead from then on.
    Dies { why: String },
}

impl Script {
    pub fn files(files: &[(&str, &str)]) -> Self {
        Script::Diff {
            files: files
                .iter()
                .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
                .collect(),
            lines: vec![],
            lie: Lie::None,
        }
    }
}

#[derive(Default)]
pub struct FakeState {
    pub script: VecDeque<Script>,
    pub specs: Vec<BuildSpec>,
    pub steps: Vec<RunStep>,
    pub finished: usize,
    /// `finish` fails.
    pub finish_fails: bool,
}

#[derive(Clone)]
pub struct FakeBackend {
    pub max_devices: usize,
    state: Arc<Mutex<FakeState>>,
}

impl FakeBackend {
    pub fn new(max_devices: usize, script: Vec<Script>) -> Self {
        Self {
            max_devices,
            state: Arc::new(Mutex::new(FakeState {
                script: script.into(),
                ..Default::default()
            })),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }
}

impl BuildBackend for FakeBackend {
    fn max_devices(&self) -> usize {
        self.max_devices
    }

    fn start(&self, spec: BuildSpec) -> Result<Box<dyn BuildVm>, BuildError> {
        let cfg = VmConfig::new(VmmKind::Firecracker, "/fake/kernel".into(), "/fake/init".into());
        let kspec = kiln_spec(&spec, &cfg)?;
        kspec.check(self.max_devices as u32).map_err(start_error)?;
        let output = spec.output.clone();
        self.state().specs.push(spec);
        Ok(Box::new(FakeVm {
            state: self.state.clone(),
            output,
            cursor: 0,
            last: 0,
            kspec,
            mirror: Mirror::default(),
            dead: None,
        }))
    }
}

struct FakeVm {
    state: Arc<Mutex<FakeState>>,
    output: PathBuf,
    cursor: u64,
    last: u64,
    kspec: kb::BuildSpec,
    mirror: Mirror,
    /// Why the VM has ended, once it has.
    dead: Option<String>,
}

/// Sends `line` and its newline in two chunks split mid-line, as a pipe may deliver them.
fn send(sink: &mut dyn FnMut(Stream, &[u8]), stream: Stream, line: &str) {
    let bytes = format!("{line}\n").into_bytes();
    let (a, b) = bytes.split_at(bytes.len() / 2);
    sink(stream, a);
    sink(stream, b);
}

/// A diff tar of `files` with their parent directories, as kiln-init would seal it.
pub fn diff_tar(files: &[(String, Vec<u8>)]) -> (Vec<u8>, u64) {
    let mut b = tar::Builder::new(Vec::new());
    let mut dirs = std::collections::BTreeSet::new();
    let mut entries = 0;
    for (path, data) in files {
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            let dir = parts[..i].join("/");
            if dirs.insert(dir.clone()) {
                let mut h = tar::Header::new_ustar();
                h.set_entry_type(tar::EntryType::Directory);
                h.set_mode(0o755);
                h.set_size(0);
                h.set_mtime(0);
                h.set_uid(0);
                h.set_gid(0);
                b.append_data(&mut h, format!("{dir}/"), &[][..]).unwrap();
                entries += 1;
            }
        }
        let mut h = tar::Header::new_ustar();
        h.set_mode(0o644);
        h.set_size(data.len() as u64);
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        b.append_data(&mut h, path, &data[..]).unwrap();
        entries += 1;
    }
    (b.into_inner().unwrap(), entries)
}

impl BuildVm for FakeVm {
    fn run_step(&mut self, step: RunStep, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<StepOutcome, BuildError> {
        let id = step.id;
        if let Some(why) = &self.dead {
            return Err(BuildError::Infra(format!("the build VM has ended: {why}")));
        }
        let kstep = kiln_step(&step);
        self.mirror.check(&self.kspec, &kstep)?;
        let script = {
            let mut s = self.state.lock().unwrap();
            s.steps.push(step);
            s.script.pop_front().expect("the fake VM ran out of script")
        };
        let result = self.play(id, script, sink)?;
        self.mirror.record(&kstep, &kiln_outcome(&result));
        Ok(result)
    }

    fn finish(self: Box<Self>) -> Result<(), BuildError> {
        if let Some(why) = &self.dead {
            return Err(BuildError::Infra(format!("the build VM has ended: {why}")));
        }
        let mut s = self.state.lock().unwrap();
        s.finished += 1;
        if s.finish_fails {
            return Err(BuildError::Infra("the build VM did not shut down cleanly".into()));
        }
        Ok(())
    }
}

/// kiln's view of an outcome, for the mirror.
fn kiln_outcome(o: &StepOutcome) -> kb::StepOutcome {
    kb::StepOutcome {
        exit: match o.exit {
            Exit::Code(c) => kb::Exit::Code(c),
            Exit::Signal(s) => kb::Exit::Signal(s),
            Exit::TimedOut => kb::Exit::TimedOut,
        },
        sealed: o.sealed.as_ref().map(|s| kiln_proto::StepSealed {
            id: s.id,
            offset: s.offset,
            len: s.len,
            sha256: s.sha256.clone(),
            entries: s.entries,
        }),
    }
}

impl FakeVm {
    fn play(
        &mut self,
        id: u32,
        script: Script,
        sink: &mut dyn FnMut(Stream, &[u8]),
    ) -> Result<StepOutcome, BuildError> {
        match script {
            Script::SetupFail { line } => {
                send(sink, Stream::Stderr, &line);
                Ok(StepOutcome {
                    exit: Exit::Code(125),
                    sealed: None,
                })
            }
            Script::Dies { why } => {
                let e = vm_error(kiln_run::Error::Refused(format!("step {id}: {why}")));
                self.dead = Some(why);
                Err(e)
            }
            Script::Fail { code, lines } => {
                lines.iter().for_each(|l| send(sink, Stream::Stderr, l));
                Ok(StepOutcome {
                    exit: Exit::Code(code),
                    sealed: None,
                })
            }
            Script::Hang => {
                self.dead = Some(format!("step {id} timed out and kiln killed it"));
                Ok(StepOutcome {
                    exit: Exit::TimedOut,
                    sealed: None,
                })
            }
            Script::Diff { files, lines, lie } => {
                lines.iter().for_each(|l| send(sink, Stream::Stdout, l));
                let (tar, entries) = diff_tar(&files);
                let offset = self.cursor;
                let mut f = std::fs::OpenOptions::new().write(true).open(&self.output).unwrap();
                f.seek(SeekFrom::Start(offset)).unwrap();
                f.write_all(&tar).unwrap();
                let len = tar.len() as u64;
                let mut report = StepSealed {
                    id,
                    offset,
                    len,
                    sha256: kiln_store::Digest::of(&tar).hex().to_string(),
                    entries,
                };
                match lie {
                    Lie::None => {}
                    Lie::Sha => report.sha256 = kiln_store::Digest::of(b"lie").hex().to_string(),
                    Lie::Entries => report.entries += 1,
                    Lie::Overlap => report.offset = self.last,
                    Lie::Id => report.id += 1,
                }
                self.last = offset;
                self.cursor = (offset + len).div_ceil(ALIGN) * ALIGN;
                Ok(StepOutcome {
                    exit: Exit::Code(0),
                    sealed: Some(report),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use kiln_store::Store;

    use super::*;
    use crate::vm::ingest::OutputDisk;

    fn step(id: u32) -> RunStep {
        RunStep {
            id,
            command: potter_plan::Command::shell("true"),
            env: BTreeMap::new(),
            workdir: "/".into(),
            uid: 0,
            gid: 0,
            lowers_added: vec![],
            mounts: vec![],
            secrets: vec![],
            network: false,
            timeout: Duration::from_secs(1),
        }
    }

    /// A 31-device fake, a spec with a 1 MiB output file, and the directory holding it.
    fn one_vm(script: Vec<Script>) -> (FakeBackend, BuildSpec, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("output");
        std::fs::File::create(&out).unwrap().set_len(1 << 20).unwrap();
        let spec = BuildSpec {
            layers: vec![],
            root: 0,
            cache_disks: vec![],
            output: out,
            output_size: 1 << 20,
            network: false,
            memory_mib: 4096,
            cpus: 2,
            source_date_epoch: None,
        };
        (FakeBackend::new(31, script), spec, dir)
    }

    #[test]
    fn scripted_diffs_ingest_and_lies_do_not() {
        let (fake, spec, dir) = one_vm(vec![
            Script::files(&[("app/a.txt", "a")]),
            Script::Diff {
                files: vec![("b".into(), b"b".to_vec())],
                lines: vec!["hello".into()],
                lie: Lie::Sha,
            },
            Script::Fail {
                code: 2,
                lines: vec!["boom".into()],
            },
        ]);
        let store = Store::open(dir.path().join("store")).unwrap();
        let out = spec.output.clone();
        let mut vm = fake.start(spec.clone()).unwrap();
        let mut disk = OutputDisk::new(&out, 1 << 20);
        let mut out = Vec::new();
        let mut sink = |s: Stream, b: &[u8]| out.push((s, b.to_vec()));
        let ok = vm.run_step(step(1), &mut sink).unwrap();
        assert!(ok.exit.success());
        disk.take(ok.sealed.as_ref().unwrap(), &store, dir.path(), 1 << 20)
            .unwrap();
        let lie = vm.run_step(step(2), &mut sink).unwrap();
        let e = disk
            .take(lie.sealed.as_ref().unwrap(), &store, dir.path(), 1 << 20)
            .unwrap_err();
        assert_eq!(e.exit_code(), 3);
        let fail = vm.run_step(step(3), &mut sink).unwrap();
        assert_eq!((fail.exit, fail.sealed), (Exit::Code(2), None));
        let joined = |stream| {
            out.iter()
                .filter(|(s, _)| *s == stream)
                .flat_map(|(_, b)| b.clone())
                .collect::<Vec<u8>>()
        };
        assert_eq!(
            (joined(Stream::Stdout), joined(Stream::Stderr)),
            (b"hello\n".to_vec(), b"boom\n".to_vec())
        );
        vm.finish().unwrap();
        let s = fake.state();
        assert_eq!((s.specs.as_slice(), s.steps.len(), s.finished), (&[spec][..], 3, 1));
    }

    #[test]
    fn a_timed_out_or_dead_vm_refuses_more_steps_and_finish() {
        for script in [
            Script::Hang,
            Script::Dies {
                why: "the VM ended (Killed)".into(),
            },
        ] {
            let (fake, spec, _dir) = one_vm(vec![script, Script::files(&[("a", "1")])]);
            let mut vm = fake.start(spec).unwrap();
            let _ = vm.run_step(step(1), &mut |_, _| {});
            assert_eq!(vm.run_step(step(2), &mut |_, _| {}).unwrap_err().exit_code(), 4);
            assert!(vm.finish().is_err());
        }
    }

    #[test]
    fn the_fake_refuses_what_kiln_refuses_and_stays_usable() {
        let (fake, spec, _dir) = one_vm(vec![Script::files(&[("a", "1")])]);
        let mut vm = fake.start(spec).unwrap();
        let mut bad = step(1);
        bad.env.insert("A=B".into(), "x".into());
        assert_eq!(vm.run_step(bad, &mut |_, _| {}).unwrap_err().exit_code(), 2);
        assert!(fake.state().steps.is_empty(), "nothing was sent");
        assert!(vm.run_step(step(2), &mut |_, _| {}).unwrap().exit.success());
    }

    #[test]
    fn a_spec_over_the_device_budget_does_not_boot() {
        let (fake, mut spec, _dir) = one_vm(vec![]);
        spec.layers = vec!["/l".into(); 40];
        spec.root = 40;
        assert_eq!(fake.start(spec).err().unwrap().exit_code(), 4);
    }

    #[test]
    fn setup_failure_and_finish_failure_are_scripted() {
        let (fake, spec, _dir) = one_vm(vec![Script::SetupFail {
            line: "kiln-init: no mount".into(),
        }]);
        fake.state().finish_fails = true;
        let mut vm = fake.start(spec).unwrap();
        let mut err = Vec::new();
        let o = vm.run_step(step(1), &mut |_, b| err.extend_from_slice(b)).unwrap();
        assert_eq!(o.exit, Exit::Code(125));
        assert_eq!(err, b"kiln-init: no mount\n");
        assert!(vm.finish().is_err());
        assert_eq!(fake.state().finished, 1);
    }
}
