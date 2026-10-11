//! A scripted stand-in for kiln's build VM: each step plays the next [`Script`] entry, writing
//! its diff to the output disk the way kiln-init does and reporting it, honestly or not.

use std::collections::VecDeque;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use super::backend::{BuildBackend, BuildSpec, BuildVm, Exit, RunStep, StepOutcome};
use super::ingest::{ALIGN, StepSealed};
use crate::error::BuildError;

/// How a scripted report lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lie {
    None,
    Sha,
    Entries,
    /// Reports the previous diff's offset.
    Overlap,
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
    /// The step runs past its timeout.
    Hang,
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
        let output = spec.output.clone();
        self.state().specs.push(spec);
        Ok(Box::new(FakeVm {
            state: self.state.clone(),
            output,
            cursor: 0,
            last: 0,
        }))
    }
}

struct FakeVm {
    state: Arc<Mutex<FakeState>>,
    output: PathBuf,
    cursor: u64,
    last: u64,
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
                b.append_data(&mut h, format!("{dir}/"), &[][..]).unwrap();
                entries += 1;
            }
        }
        let mut h = tar::Header::new_ustar();
        h.set_mode(0o644);
        h.set_size(data.len() as u64);
        h.set_mtime(0);
        b.append_data(&mut h, path, &data[..]).unwrap();
        entries += 1;
    }
    (b.into_inner().unwrap(), entries)
}

impl BuildVm for FakeVm {
    fn run_step(&mut self, step: RunStep, sink: &mut dyn FnMut(&str)) -> Result<StepOutcome, BuildError> {
        let id = step.id;
        let script = {
            let mut s = self.state.lock().unwrap();
            s.steps.push(step);
            s.script.pop_front().expect("the fake VM ran out of script")
        };
        match script {
            Script::Fail { code, lines } => {
                lines.iter().for_each(|l| sink(l));
                Ok(StepOutcome {
                    exit: Exit::Code(code),
                    sealed: None,
                })
            }
            Script::Hang => Ok(StepOutcome {
                exit: Exit::TimedOut,
                sealed: None,
            }),
            Script::Diff { files, lines, lie } => {
                lines.iter().for_each(|l| sink(l));
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

    fn finish(self: Box<Self>) -> Result<(), BuildError> {
        self.state.lock().unwrap().finished += 1;
        Ok(())
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

    #[test]
    fn scripted_diffs_ingest_and_lies_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let out = dir.path().join("output");
        std::fs::File::create(&out).unwrap().set_len(1 << 20).unwrap();
        let fake = FakeBackend::new(
            31,
            vec![
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
            ],
        );
        let spec = BuildSpec {
            layers: vec![],
            cache_disks: vec![],
            output: out.clone(),
            output_size: 1 << 20,
            network: false,
            memory_mib: 4096,
            cpus: 2,
        };
        let mut vm = fake.start(spec.clone()).unwrap();
        let mut disk = OutputDisk::new(&out, 1 << 20);
        let mut lines = Vec::new();
        let ok = vm.run_step(step(1), &mut |l| lines.push(l.to_string())).unwrap();
        assert!(ok.exit.success());
        disk.take(ok.sealed.as_ref().unwrap(), &store, dir.path(), 1 << 20)
            .unwrap();
        let lie = vm.run_step(step(2), &mut |l| lines.push(l.to_string())).unwrap();
        let e = disk
            .take(lie.sealed.as_ref().unwrap(), &store, dir.path(), 1 << 20)
            .unwrap_err();
        assert_eq!(e.exit_code(), 3);
        let fail = vm.run_step(step(3), &mut |l| lines.push(l.to_string())).unwrap();
        assert_eq!((fail.exit, fail.sealed), (Exit::Code(2), None));
        assert_eq!(lines, ["hello", "boom"]);
        vm.finish().unwrap();
        let s = fake.state();
        assert_eq!((s.specs.as_slice(), s.steps.len(), s.finished), (&[spec][..], 3, 1));
    }
}
