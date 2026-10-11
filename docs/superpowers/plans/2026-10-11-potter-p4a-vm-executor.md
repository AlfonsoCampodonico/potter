# potter P4a — VM Executor (potter side) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** everything potter needs to run a build chain in a microVM (spec §9.3), coded against a local backend trait, so that swapping in kiln's `kiln_run::build` (kiln M3e, owned by the kiln session) is a small P4b.

**Architecture:** `potter-exec` gains a `vm` module: a `BuildBackend`/`BuildVm` trait pair shaped like the kiln session's sketch (`BuildVm::start(store, BuildSpec{layers, init, scratch, output_disk, cache_disks, net, limits})`, `run_step(RunStep) -> StepOutcome{exit, sealed: Option<StepSealed>}` with stdio streamed to a sink, `finish()`), host-side output-disk ingest that trusts nothing the guest reports, layer disks with device budgeting and squash, and executor-class trust ordering. The chain dispatches `Run` steps to the namespace executor or a VM session. A scripted fake backend drives every test; no real VM is needed.

**Tech Stack:** Rust 2024, kiln-erofs (layer → erofs, `squash`), kiln-store, existing `ingest::validate_layer`.

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md` §9.1, §9.3, §8.4, §12, §14.4 (P3, P4).

## Global Constraints

- The host never trusts guest-reported `sha256`, `len` or `entries` (P4): it reads `[offset, offset+len)` from the output disk itself, checks the range lies inside the disk, starts at or after the end of the previous diff and is 4 KiB-aligned, hashes it, and ingests it through the bounded path. A mismatch with the report is a protocol violation: exit 3, no cache entry, VM torn down.
- A `vm` build never consumes an `ns`-class entry, local or remote (P3). An `ns` build accepts both. VM-produced entries are `vm`-class.
- One VM per target chain, starting at the first miss; after the miss the rest of the chain runs in that VM with no further cache lookups (§9.1).
- Fixed devices: init, scratch, output, vsock, net (5). Layer + cache-mount disks must fit `max_devices - 5`; otherwise the bottom hit-prefix layers are squashed into one erofs cached by their digests. Inputs/copy layers built mid-chain are attached as `lowers_added` of the next step; when the budget runs out mid-chain, the VM finishes and a new one boots from the extended prefix.
- Secrets travel inline in `RunStep` and never touch host disk in VM mode.
- `--executor vm` without a real backend fails with exit 4: "VM builds need kiln build mode (kiln M3e)".
- No kiln edits; kiln rev stays `01e7a9fe…`. No attribution trailers in commits.

## Review Focus

1. A guest that reports a diff overlapping the previous one, or past the end of the disk, or with an unaligned offset: exit 3, nothing stored.
2. A guest whose report matches but whose tar holds `..`, absolute paths, or exceeds entry/byte limits: exit 3.
3. A chain whose inputs layers outgrow the device budget mid-chain: the VM restarts from the extended prefix and the image is identical to one built without the restart.
4. A local index holding an `ns` entry for a key a vm build asks for: a miss, rebuilt in the VM, and the new `vm` entry is stored beside the `ns` one (index entries are per class), never displacing it.
5. A step that fails or times out in the VM: the tail of its output reported as for ns, the VM torn down, scratch and output disks deleted.

---

### Task 1: Trust ordering (P3)

Files: `crates/potter-cache/src/index.rs`, `crates/potter-exec/src/chain.rs`.

- `pub fn accepts(build: ExecutorClass, entry: ExecutorClass) -> bool` (ns accepts both, vm accepts vm).
- Index stores entries per (key, class); `get(key, accept: &[ExecutorClass])` returns the most trusted acceptable entry (vm before ns).
- `BuildCtx.executor: ExecutorClass`; local and remote lookups pass `accept` for it; publishes and new entries use the entry's own class.
- Tests: an ns entry is invisible to a vm lookup; a vm entry is visible to both; writing a vm entry keeps the ns one.

### Task 2: Output-disk ingest (P4)

Files: create `crates/potter-exec/src/vm/ingest.rs`.

- `pub struct StepSealed { pub id: u32, pub offset: u64, pub len: u64, pub sha256: String, pub entries: u64 }` (wire camelCase comes from kiln; this is potter's view).
- `pub struct OutputDisk { path, size, cursor }`, `OutputDisk::take(&mut self, report: &StepSealed, store, spill_dir, max_layer) -> Result<LayerRef, BuildError>`.
- Tests (pure, macOS): good diff stored; overlapping, out-of-bounds, unaligned, oversize, lying sha, lying len, lying entries, `..` path, absolute path, malformed tar → each `exit_code() == 3` and the store has no new blob.

### Task 3: Backend trait and fake

Files: `crates/potter-exec/src/vm/backend.rs`, `crates/potter-exec/src/vm/fake.rs` (`#[cfg(any(test, feature = "testing"))]`).

- `pub trait BuildBackend { fn max_devices(&self) -> usize; fn start(&self, spec: BuildSpec) -> Result<Box<dyn BuildVm>, BuildError>; }`
- `pub trait BuildVm { fn run_step(&mut self, step: RunStep, sink: &mut dyn FnMut(&str)) -> Result<StepOutcome, BuildError>; fn output_disk(&self) -> &Path; fn finish(self: Box<Self>) -> Result<(), BuildError>; }`
- `BuildSpec { layers: Vec<PathBuf> /*erofs, bottom first*/, cache_disks: Vec<(PathBuf, String)>, network: bool, output_size: u64, limits }`, `RunStep { id, args, env, workdir, user, lowers_added: Vec<PathBuf>, secrets: Vec<(String, Vec<u8>)>, timeout }`, `StepOutcome { exit: Exit, tail: Vec<String>, sealed: Option<StepSealed> }`.
- `FakeBackend`: scripted per-step results (diff tar bytes or a lie), writes diffs to its output file at the next aligned offset, records every `BuildSpec`/`RunStep` it saw.

### Task 4: Layer disks, budget and squash

Files: `crates/potter-exec/src/vm/disks.rs`.

- `pub fn erofs_of(store, home, layer: &LayerRef) -> Result<PathBuf>`: tar layer → erofs via kiln-erofs, cached under `$POTTER_HOME/erofs/<diff_id>`.
- `pub fn plan_disks(prefix: &[LayerRef], mounts: usize, max_devices: usize) -> Result<DiskPlan>`: which bottom layers squash; error when even one squashed disk plus mounts can't fit.
- `pub fn squashed(store, home, layers: &[LayerRef]) -> Result<PathBuf>` via `kiln_erofs::squash`, cached by the hash of the digest list.
- Tests: budget math table; squash of three layers equals applying them in order (read back with kiln-erofs reader).

### Task 5: VM sessions in the chain

Files: `crates/potter-exec/src/chain.rs`, `crates/potter-exec/src/vm/mod.rs`.

- `BuildCtx.backend: Option<&dyn BuildBackend>`; `executor == Vm` requires it.
- At the first miss: plan disks from the layers so far, start the VM; subsequent `Run` steps go to it; `Inputs`/`CopyFrom` layers built after the VM started become `lowers_added`; budget exhausted → finish, restart from the current prefix.
- Sealed diffs go through Task 2 and become `vm`-class entries; `StepRecord.class = Some(Vm)`.
- Failure/timeout → exit 1 with the tail; teardown always.
- Tests (fake backend): a two-step chain builds and its records are vm; a vm build ignores a seeded ns entry (P3, §14.4); a lying guest fails with 3 and leaves no index entry (P4); a budget overflow mid-chain restarts and produces the same layers.

### Task 6: CLI and docs

- `--executor vm` → exit 4 with the M3e message until a real backend exists (no backend is compiled in P4a).
- README/HANDOFF: P4a status, what P4b swaps in (kiln rev, `kiln_run::build` adapter implementing `BuildBackend`), the M3e requests sent to the kiln session (output-disk path, max devices, tmpfs secrets).
