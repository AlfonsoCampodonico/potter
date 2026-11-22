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

/// What replaces a kept-whole text (a secret) in a line.
const MASK: &[u8] = b"***";

/// The shortest kept-whole text [`Lines`] masks and keeps whole. A 1 or 2 byte secret line holds
/// at most 16 bits, too little to be worth guessing from a log, and masking every occurrence of
/// such a short run would mangle ordinary output. The redactor still replaces them in each line.
const MIN_KEEP: usize = 3;

/// Joins a step's output chunks into lines, per stream; potter redacts and reports whole lines.
///
/// `keep_whole` holds the secrets, raw and as the redactor sees them (`chain::keep_whole`). Every
/// raw occurrence of one in a line is masked with `***` before the line is decoded (overlapping
/// ones as one run), so the redactor's lossy matching is not the only guard. A line longer than
/// [`LINE_CAP`] is passed on in pieces of at most that size, never cut inside a kept-whole text:
/// the cut moves back to its start, and one that starts the piece and is longer than the cap is
/// passed on as `***` alone. Masking a piece is one linear pass per kept-whole text.
#[derive(Default)]
pub struct Lines {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Longest first; none shorter than [`MIN_KEEP`] or holding a newline (the lines of a
    /// multi-line secret are separate entries, and a line never holds a newline).
    keep_whole: Vec<Needle>,
    /// The longest of `keep_whole`, less one: what a cut at the cap must see past the cap.
    overlap: usize,
}

/// A kept-whole text and its Knuth-Morris-Pratt failure table, computed once.
struct Needle {
    bytes: Vec<u8>,
    fail: Vec<usize>,
}

impl Needle {
    fn new(bytes: Vec<u8>) -> Self {
        let mut fail = vec![0usize; bytes.len()];
        let mut k = 0;
        for i in 1..bytes.len() {
            while k > 0 && bytes[i] != bytes[k] {
                k = fail[k - 1];
            }
            if bytes[i] == bytes[k] {
                k += 1;
            }
            fail[i] = k;
        }
        Self { bytes, fail }
    }

    /// Calls `found` with the start of each occurrence in `hay[from..]`, in order, until it
    /// returns false. Linear in the bytes scanned.
    fn each(&self, hay: &[u8], from: usize, found: &mut dyn FnMut(usize) -> bool) {
        let n = self.bytes.len();
        let mut k = 0;
        for (i, &b) in hay.iter().enumerate().skip(from) {
            while k > 0 && b != self.bytes[k] {
                k = self.fail[k - 1];
            }
            if b == self.bytes[k] {
                k += 1;
            }
            if k == n {
                if !found(i + 1 - n) {
                    return;
                }
                k = self.fail[k - 1];
            }
        }
    }
}

impl Lines {
    /// Lines that never pass on, or cut inside, any of `keep_whole`.
    pub fn new(keep_whole: Vec<Vec<u8>>) -> Self {
        let mut keep: Vec<Vec<u8>> = keep_whole
            .into_iter()
            .filter(|k| k.len() >= MIN_KEEP && !k.contains(&b'\n'))
            .collect();
        keep.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keep.dedup();
        let overlap = keep.first().map_or(0, |k| k.len() - 1);
        Self {
            keep_whole: keep.into_iter().map(Needle::new).collect(),
            overlap,
            ..Self::default()
        }
    }

    /// Adds `bytes` from `stream`, passing each line it completes to `line`.
    pub fn feed(&mut self, stream: Stream, bytes: &[u8], line: &mut dyn FnMut(&str)) {
        let keep = &self.keep_whole;
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
                    line(&masked(&buf[start..end], keep));
                    start = end + 1;
                    next = newline(buf, start);
                }
                // Cut only once all of a kept-whole text that could span the cap is here.
                _ if buf.len() - start > LINE_CAP + self.overlap => start += piece(&buf[start..], keep, line),
                _ => break,
            }
        }
        buf.drain(..start);
    }

    /// Passes on what is left without a newline.
    pub fn flush(&mut self, line: &mut dyn FnMut(&str)) {
        let keep = &self.keep_whole;
        for buf in [&mut self.stdout, &mut self.stderr] {
            // The stream ended: what follows the cap is all there is.
            let mut start = 0;
            while buf.len() - start > LINE_CAP {
                start += piece(&buf[start..], keep, line);
            }
            if start < buf.len() {
                line(&masked(&buf[start..], keep));
            }
            buf.clear();
        }
    }
}

/// Passes on the first piece of `pending`, longer than [`LINE_CAP`], and returns its length.
fn piece(pending: &[u8], keep: &[Needle], line: &mut dyn FnMut(&str)) -> usize {
    match cut_at(pending, keep) {
        Cut::At(cut) => {
            line(&masked(&pending[..cut], keep));
            cut
        }
        Cut::Whole(len) => {
            line(&String::from_utf8_lossy(MASK));
            len
        }
    }
}

/// `bytes` with every byte inside an occurrence of any of `keep` masked, each run of masked bytes
/// (overlapping occurrences included) becoming one `***`, decoded lossily. One linear pass per
/// needle marks the covered ranges, and one more builds the output: nothing is spliced in place.
fn masked(bytes: &[u8], keep: &[Needle]) -> String {
    if keep.is_empty() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    // +1 at an occurrence's start, -1 just past its end.
    let mut cover = vec![0i32; bytes.len() + 1];
    for k in keep {
        let n = k.bytes.len();
        k.each(bytes, 0, &mut |p| {
            cover[p] += 1;
            cover[p + n] -= 1;
            true
        });
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut depth = 0;
    let mut in_mask = false;
    for (i, &b) in bytes.iter().enumerate() {
        depth += cover[i];
        if depth > 0 {
            if !in_mask {
                out.extend_from_slice(MASK);
                in_mask = true;
            }
        } else {
            in_mask = false;
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

enum Cut {
    /// Pass on the first this many bytes.
    At(usize),
    /// The pending bytes start with a kept-whole text of this length, longer than the cap.
    Whole(usize),
}

/// Where to cut `pending`, longer than [`LINE_CAP`] and holding all of any of `keep` that spans
/// the cap (all of the stream, or the cap plus the longest of `keep`): at the cap, off a UTF-8
/// continuation byte, then back to the start of any of `keep` that spans the cut.
fn cut_at(pending: &[u8], keep: &[Needle]) -> Cut {
    let mut cut = LINE_CAP;
    while LINE_CAP - cut < 3 && pending[cut] & 0xC0 == 0x80 {
        cut -= 1;
    }
    while let Some((p, len)) = keep
        .iter()
        .filter_map(|k| spanning(pending, k, cut).map(|p| (p, k.bytes.len())))
        .min()
    {
        if p == 0 {
            return Cut::Whole(len);
        }
        cut = p;
    }
    Cut::At(cut)
}

/// The first start of `needle` in `hay` that is before `cut` and ends after it: a search of the
/// 2 × len − 1 bytes around `cut`.
fn spanning(hay: &[u8], needle: &Needle, cut: usize) -> Option<usize> {
    let n = needle.bytes.len();
    let lo = cut.saturating_sub(n - 1);
    let hi = (cut + n - 1).min(hay.len());
    let mut first = None;
    needle.each(&hay[..hi], lo, &mut |p| {
        first = Some(p);
        false
    });
    first
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
                return Err(named(e, id, name));
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

/// `e` with kiln's per-VM `step {id}: ` prefix, for the id this session gave the step, replaced
/// by the plan's step name, whatever the error's class.
fn named(e: BuildError, id: u32, name: &str) -> BuildError {
    let prefix = format!("step {id}: ");
    let rename = |m: String| match m.strip_prefix(&prefix) {
        Some(rest) => format!("step {name}: {rest}"),
        None => m,
    };
    match e {
        BuildError::Plan(m) => BuildError::Plan(rename(m)),
        BuildError::Infra(m) => BuildError::Infra(rename(m)),
        BuildError::Hostile(m) => BuildError::Hostile(rename(m)),
        e => e,
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
mod named_tests {
    use super::*;

    #[test]
    fn kilns_step_id_becomes_the_step_name_in_every_error_class() {
        type Make = fn(String) -> BuildError;
        let cases: [(Make, u8); 3] = [(BuildError::Plan, 2), (BuildError::Infra, 4), (BuildError::Hostile, 3)];
        for (make, code) in cases {
            let e = named(make("step 7: the VM ended (Killed)".into()), 7, "build");
            assert_eq!(e.exit_code(), code);
            assert!(e.to_string().contains("step build: the VM ended"), "{e}");
            assert!(!e.to_string().contains("step 7"), "{e}");
        }
    }

    #[test]
    fn only_the_exact_prefix_for_the_sessions_id_is_replaced() {
        for text in ["step 17: x", "step 7 x", "the guest said step 7: x"] {
            let e = named(BuildError::Infra(text.into()), 7, "build");
            assert_eq!(e.to_string(), text);
        }
        let other = named(BuildError::StepOutput("step 7: too big".into()), 7, "build");
        assert_eq!(other.to_string(), "step 7: too big");
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

    /// Feeds `data` in 7-byte chunks (so a secret also crosses chunk boundaries) through a
    /// `Lines` keeping `secret` whole and then the redactor, as the chain does.
    fn through(secret: &[u8], data: &[u8]) -> Vec<String> {
        let values = vec![("tok".to_string(), secret.to_vec())];
        let redact = crate::chain::redactor(&values);
        let mut l = Lines::new(crate::chain::keep_whole(&values));
        let mut out = Vec::new();
        for c in data.chunks(7) {
            l.feed(Stream::Stdout, c, &mut |p| out.push(redact(p)));
        }
        l.flush(&mut |p| out.push(redact(p)));
        assert!(out.iter().all(|p| p.len() <= LINE_CAP), "a piece over the cap");
        out
    }

    /// No 4-byte run of `secret` is in `out`. Four bytes: the filler around the secrets here is
    /// `a`, `b` and `\n`, which no 4-byte run of these secrets is made of, so any match is a leak;
    /// a shorter run carries too little of a secret to matter. Runs that aren't UTF-8 can't
    /// appear in decoded output, but their decodable neighbours are checked.
    fn assert_no_run(out: &[String], secret: &[u8]) {
        let runs: std::collections::BTreeSet<&[u8]> = secret.windows(4).collect();
        for w in runs {
            let Ok(w) = std::str::from_utf8(w) else { continue };
            for (i, p) in out.iter().enumerate() {
                assert!(!p.contains(w), "piece {i} holds {w:?} of the secret");
            }
        }
    }

    #[test]
    fn a_secret_that_starts_just_before_the_cut_is_kept_whole_and_redacted() {
        for before in [1, 4, 11] {
            let secret = b"s3cr3t-token";
            let mut data = vec![b'a'; LINE_CAP - before];
            data.extend_from_slice(secret);
            data.extend_from_slice(&[b'b'; 100]);
            let out = through(secret, &data);
            assert_no_run(&out, secret);
            assert_eq!(out[0].len(), LINE_CAP - before, "the cut moved to the secret's start");
            assert!(out[1].starts_with("***"), "{:?}", &out[1][..8]);
        }
    }

    #[test]
    fn a_secret_with_bytes_that_are_not_utf8_spanning_the_cut_never_leaks() {
        let secret = b"K9\xffQz7W\xfex8Lm\xc3Pq";
        let mut data = vec![b'a'; LINE_CAP - 5];
        data.extend_from_slice(secret);
        data.extend_from_slice(&[b'b'; 100]);
        data.push(b'\n');
        // And in an ordinary line, where only the raw match catches it.
        data.extend_from_slice(b"x=");
        data.extend_from_slice(secret);
        data.push(b'\n');
        let out = through(secret, &data);
        assert_no_run(&out, secret);
        assert_eq!(out.last().unwrap(), "x=***");
    }

    #[test]
    fn a_secret_longer_than_the_cap_comes_out_as_a_mask() {
        let secret: Vec<u8> = b"Zq7#Xw2!Kp".iter().copied().cycle().take(LINE_CAP + 100).collect();
        let mut data = vec![b'a'; 10];
        data.extend_from_slice(&secret);
        data.extend_from_slice(&vec![b'b'; LINE_CAP * 2]);
        let out = through(&secret, &data);
        assert_no_run(&out, &secret);
        assert_eq!(out[..2], ["aaaaaaaaaa", "***"]);
        assert_eq!(out[2..].iter().map(String::len).sum::<usize>(), LINE_CAP * 2);
    }

    #[test]
    fn a_piece_made_only_of_a_short_secret_is_masked_in_linear_time() {
        // 3 bytes, the shortest masked; 1 and 2 byte secret lines are left to the redactor.
        for (secret, masked_here) in [(&b"abc"[..], true), (b"ab", false), (b"a", false)] {
            let mut l = Lines::new(vec![secret.to_vec()]);
            let data: Vec<u8> = secret.iter().copied().cycle().take(4 * LINE_CAP).collect();
            let started = std::time::Instant::now();
            let mut out = Vec::new();
            for c in data.chunks(LINE_CAP / 2) {
                l.feed(Stream::Stdout, c, &mut |p| out.push(p.to_string()));
            }
            l.flush(&mut |p| out.push(p.to_string()));
            // Quadratic masking took minutes here; linear takes milliseconds, even unoptimised.
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "{:?}",
                started.elapsed()
            );
            if masked_here {
                // Every whole "abc" is masked: what is left is the stream's trailing partial one.
                assert!(
                    out.iter()
                        .all(|p| p.starts_with("***") && p.replace("***", "").len() <= 2),
                    "{:?}",
                    out.iter().map(String::len).collect::<Vec<_>>()
                );
            } else {
                assert_eq!(out.iter().map(String::len).sum::<usize>(), 4 * LINE_CAP);
            }
        }
    }

    #[test]
    fn overlapping_secrets_are_masked_as_one_run() {
        let mut l = Lines::new(vec![b"abcdef".to_vec(), b"zab".to_vec()]);
        let out = collect(&mut l, &[(Stream::Stdout, b"x zabcdef y\n")]);
        assert_eq!(out, ["x *** y"]);
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
