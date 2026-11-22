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

/// The most bytes of one stream [`Lines`] holds without a newline: past it, they are passed on as
/// a line of their own, so a step that never writes a newline can't exhaust potter's memory.
pub const LINE_CAP: usize = 64 << 10;

/// Joins a step's output chunks into lines, per stream; potter redacts and reports whole lines.
/// A line longer than [`LINE_CAP`] is passed on in pieces of at most that size, never cut inside
/// one of the texts it was given to keep whole (the redactor's secrets).
#[derive(Default)]
pub struct Lines {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    keep_whole: Vec<Vec<u8>>,
    /// The longest of `keep_whole`, less one: what a piece cut at the cap must see past the cap.
    overlap: usize,
}

impl Lines {
    /// Lines that are never cut inside any of `keep_whole`.
    pub fn new(keep_whole: Vec<Vec<u8>>) -> Self {
        let keep_whole: Vec<Vec<u8>> = keep_whole.into_iter().filter(|k| !k.is_empty()).collect();
        let overlap = keep_whole.iter().map(|k| k.len() - 1).max().unwrap_or(0);
        Self {
            keep_whole,
            overlap,
            ..Self::default()
        }
    }

    /// Adds `bytes` from `stream`, passing each line it completes to `line`.
    pub fn feed(&mut self, stream: Stream, bytes: &[u8], line: &mut dyn FnMut(&str)) {
        let buf = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        // What was already held has no newline: only the new bytes are searched.
        let fed = buf.len();
        buf.extend_from_slice(bytes);
        let newline = |buf: &[u8], from: usize| buf[from..].iter().position(|&b| b == b'\n').map(|i| from + i);
        let mut start = 0;
        let mut next = newline(buf, fed);
        loop {
            match next {
                Some(end) if end - start <= LINE_CAP => {
                    line(&String::from_utf8_lossy(&buf[start..end]));
                    start = end + 1;
                    next = newline(buf, start);
                }
                // Cut only once the text a kept-whole one could span past the cap is here.
                _ if buf.len() - start > LINE_CAP + self.overlap => {
                    let cut = start + cut_at(&buf[start..], &self.keep_whole);
                    line(&String::from_utf8_lossy(&buf[start..cut]));
                    start = cut;
                }
                _ => break,
            }
        }
        buf.drain(..start);
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

/// Where to cut `pending`, longer than [`LINE_CAP`] plus the longest of `keep`: at the cap, off a
/// UTF-8 continuation byte, then back to the start of any of `keep` that spans the cut. A kept
/// text that starts at 0 and spans the cut is longer than the cap: it is cut anyway.
fn cut_at(pending: &[u8], keep: &[Vec<u8>]) -> usize {
    let mut cut = LINE_CAP;
    while LINE_CAP - cut < 3 && pending[cut] & 0xC0 == 0x80 {
        cut -= 1;
    }
    while let Some(p) = keep.iter().filter_map(|k| spanning(pending, k, cut)).min() {
        if p == 0 {
            break;
        }
        cut = p;
    }
    cut
}

/// The first start of `needle` in `hay` that is before `cut` and ends after it (Knuth-Morris-Pratt
/// over the 2 × len − 1 bytes around `cut`).
fn spanning(hay: &[u8], needle: &[u8], cut: usize) -> Option<usize> {
    let n = needle.len();
    if n < 2 {
        return None;
    }
    let lo = cut.saturating_sub(n - 1);
    let hi = (cut + n - 1).min(hay.len());
    let mut fail = vec![0usize; n];
    let mut k = 0;
    for i in 1..n {
        while k > 0 && needle[i] != needle[k] {
            k = fail[k - 1];
        }
        if needle[i] == needle[k] {
            k += 1;
        }
        fail[i] = k;
    }
    let mut k = 0;
    for (i, &b) in hay[lo..hi].iter().enumerate() {
        while k > 0 && b != needle[k] {
            k = fail[k - 1];
        }
        if b == needle[k] {
            k += 1;
        }
        if k == n {
            return Some(lo + i + 1 - n);
        }
    }
    None
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
/// disks if the VM died mid-step, did not shut down cleanly or the thread is panicking (D8), and
/// releases their locks.
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
    /// A failed shutdown discards the persistent cache disks: the guest may not have synced them
    /// (D8).
    pub fn finish(mut self) -> Result<(), BuildError> {
        let Some(vm) = self.vm.take() else { return Ok(()) };
        let was_failed = self.failed;
        let result = vm.finish();
        if result.is_err() {
            self.failed = true;
        }
        // A VM that failed is dead or dying: its shutdown says nothing worth reporting.
        if was_failed { Ok(()) } else { result }
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
        if let Some(vm) = self.vm.take()
            && vm.finish().is_err()
        {
            self.failed = true;
        }
        // A VM killed mid-step, or shut down uncleanly, may have left its ext4 disks
        // inconsistent (D8); so may one whose build is unwinding from a panic. The locks are
        // released after, when the fields drop.
        if self.failed || std::thread::panicking() {
            caches::discard(&self.caches);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod lines_tests {
    use super::*;

    fn collect(lines: &mut Lines, chunks: &[(Stream, &[u8])]) -> Vec<String> {
        let mut out = Vec::new();
        for (s, c) in chunks {
            lines.feed(*s, c, &mut |l| out.push(l.to_string()));
        }
        lines.flush(&mut |l| out.push(l.to_string()));
        out
    }

    #[test]
    fn ordinary_lines_are_joined_per_stream_and_split_at_newlines() {
        let mut l = Lines::default();
        let out = collect(
            &mut l,
            &[
                (Stream::Stdout, b"one\ntw"),
                (Stream::Stderr, b"err"),
                (Stream::Stdout, b"o\n\nthree"),
                (Stream::Stderr, b"or\n"),
            ],
        );
        assert_eq!(out, ["one", "two", "", "error", "three"]);
    }

    #[test]
    fn a_stream_without_newlines_stays_bounded_and_comes_out_in_capped_pieces() {
        let mut l = Lines::default();
        let chunk = vec![b'x'; 10_000];
        let mut pieces = Vec::new();
        for _ in 0..100 {
            l.feed(Stream::Stdout, &chunk, &mut |p| pieces.push(p.len()));
            assert!(l.stdout.len() <= LINE_CAP, "{}", l.stdout.len());
        }
        l.flush(&mut |p| pieces.push(p.len()));
        assert!(pieces.iter().all(|&n| n > 0 && n <= LINE_CAP), "{pieces:?}");
        assert_eq!(pieces.iter().sum::<usize>(), 1_000_000);
        assert_eq!(pieces[0], LINE_CAP);
    }

    #[test]
    fn a_long_line_before_a_newline_is_capped_too() {
        let mut l = Lines::default();
        let mut big = vec![b'y'; LINE_CAP * 2 + 5];
        big.extend_from_slice(b"\nend\n");
        let out = collect(&mut l, &[(Stream::Stdout, &big)]);
        let lens: Vec<usize> = out.iter().map(String::len).collect();
        assert_eq!(lens, [LINE_CAP, LINE_CAP, 5, 3]);
    }

    #[test]
    fn a_capped_cut_never_splits_a_secret_and_redaction_still_works() {
        let values = vec![("tok".to_string(), b"s3cr3t-token".to_vec())];
        let redact = crate::chain::redactor(&values);
        let mut l = Lines::new(crate::chain::keep_whole(&values));
        // The secret starts 4 bytes before the cap.
        let mut data = vec![b'a'; LINE_CAP - 4];
        data.extend_from_slice(b"s3cr3t-token");
        data.extend_from_slice(&vec![b'b'; 100]);
        let mut out = Vec::new();
        // Fed in small chunks, so the secret also crosses a chunk boundary.
        for c in data.chunks(7) {
            l.feed(Stream::Stdout, c, &mut |p| out.push(redact(p)));
        }
        l.flush(&mut |p| out.push(redact(p)));
        assert!(
            out.iter().all(|p| !p.contains("s3cr") && !p.contains("token")),
            "{:?}",
            out.iter().map(|p| &p[p.len().saturating_sub(20)..]).collect::<Vec<_>>()
        );
        assert!(out.iter().any(|p| p.contains("***")));
        assert_eq!(out[0].len(), LINE_CAP - 4);
    }

    #[test]
    fn redaction_works_across_a_chunk_boundary_inside_a_line() {
        let values = vec![("tok".to_string(), b"hunter2".to_vec())];
        let redact = crate::chain::redactor(&values);
        let mut l = Lines::new(crate::chain::keep_whole(&values));
        let mut out = Vec::new();
        for c in [&b"pass=hun"[..], b"ter2 ok\n"] {
            l.feed(Stream::Stdout, c, &mut |p| out.push(redact(p)));
        }
        assert_eq!(out, ["pass=*** ok"]);
    }

    #[test]
    fn a_capped_cut_moves_off_a_utf8_continuation_byte() {
        let mut l = Lines::default();
        let mut data = vec![b'a'; LINE_CAP - 1];
        data.extend_from_slice("é".as_bytes());
        data.extend_from_slice(&[b'z'; 10]);
        let out = collect(&mut l, &[(Stream::Stdout, &data)]);
        assert_eq!(out[0].len(), LINE_CAP - 1);
        assert!(out[1].starts_with('é'), "{:?}", &out[1][..4]);
    }
}
