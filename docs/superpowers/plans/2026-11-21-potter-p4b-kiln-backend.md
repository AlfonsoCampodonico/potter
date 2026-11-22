# potter P4b: the kiln build backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `potter build --executor vm` builds real images in kiln build-mode microVMs: potter's `BuildBackend`/`BuildVm` traits run over `kiln_run::build::BuildVm` (kiln 72c82570), with per-step egress, ext4 cache disks, the 230-lower cap and KVM end-to-end tests.

**Architecture:** A cross-platform mapping module (`vm/kiln_map.rs`) turns potter's `BuildSpec`/`RunStep` into kiln's, maps outcomes back, and classifies every kiln error into potter's exit codes. It also mirrors kiln's own pre-send check (`kiln_run::build::BuildSpec::proto_step`) so that refusals are classified before anything reaches the VM. The fake backend uses the same mapping and the same check, so kiln's refusals are tested on macOS. A Linux-only `vm/kiln_backend.rs` wraps the real `BuildVm`. The chain learns four new things: one VM per chain segment with egress toggled per step, a lower count that restarts the VM before kiln's cap, persistent ext4 cache disks, and an explicit `finish()`.

**Tech Stack:** Rust 2024; kiln-run, kiln-proto, kiln-image (`scratch::create_scratch`), kiln-erofs, kiln-store at kiln `72c82570e6306c9ec2d60ae0473065110a8889d2`; vmkit `924ae248ee09d7969bb865085d4b6a19e1736eee`.

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md` §8.5, §9.1, §9.3, §9.5, §11, §12, §14.4 (P3, P4, P7, P11). kiln's contract: `docs/format.md` "Build mode (version 3)" and `crates/kiln-run/src/build.rs` at kiln 72c82570. The P4a plan is `docs/superpowers/plans/2026-10-11-potter-p4a-vm-executor.md`. HANDOFF bullets: "From the kiln session (2026-10-11)", P4a, P4b, "M3e prototype signatures".

## Global Constraints

- **Pins.** Every kiln crate potter uses (kiln-erofs, kiln-image, kiln-oci, kiln-registry, kiln-store, and the new kiln-run and kiln-proto) is at `rev = "72c82570e6306c9ec2d60ae0473065110a8889d2"`. Every vmkit dependency is at `rev = "924ae248ee09d7969bb865085d4b6a19e1736eee"`, the rev kiln's workspace pins. `Cargo.lock` holds exactly one `vmkit` package.
- **Don't edit kiln or vmkit.** Anything they lack is a follow-up for their own sessions; record it in the HANDOFF.
- **The host trusts nothing the guest reports (P4).** kiln checks each `StepSealed`: aligned to 4096, inside the output disk, contiguous with the previous seal. `vm/ingest.rs` (`OutputDisk::take`) keeps every one of its own checks unchanged.
- **Trust ordering (P3) is unchanged.** VM builds look up vm-class entries only; new entries are vm-class.
- **Exit codes (spec §11):** 1 a step failed (including kiln-init's 125 setup failures and timeouts), 2 the plan or usage, 3 a sandbox or protocol violation, 4 infrastructure.
- **No unwrap/expect on guest-, user- or environment-controlled values.** potter has no written rule against unwrap/expect (no CLAUDE.md, nothing in README or `rustfmt.toml`). Existing non-test code uses `expect` only for local invariants (`"a live VM"`, `"checked before the chain"`). New non-test code follows that: return a `BuildError` for anything the guest, the plan, the environment or kiln can cause. Tests may unwrap.
- **Formatting and lints:** `rustfmt.toml` has `max_width = 120`. CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` on ubuntu-24.04 and macos-15.
- **Linux-only code** is `#[cfg(target_os = "linux")]`. Everything else, the fake included, must build and test on macOS.
- **Commits:** plain conventional messages (`feat(vm): …`, `test(vm): …`, `ci: …`, `docs: …`). No `Co-Authored-By`, no `Claude-Session`, no "Generated with" lines. Don't push.
- **Lima test environment** (the KVM tests on this Mac): the Lima VM is `vmkit` (Ubuntu, nested KVM, Firecracker and Cloud Hypervisor in `~/bin`). Its ssh session predates the kvm group, so every command runs under `sg kvm -c '…'`. Use `CARGO_TARGET_DIR=/tmp/potter-p4b`. The kernel is `/Users/alfonso/Github/Personal/playground/vmkit/out/vmlinux-6.18.54-aarch64`. Once, install vmkit's AppArmor profile for the helper at `/tmp/potter-p4b/debug/vmkit-sandbox` (the script in Task 9 prints the command). A full run:
  `limactl shell vmkit -- sg kvm -c 'cd /Users/alfonso/Github/Personal/playground/potter/.claude/worktrees/p4b-kiln-build && PATH=$HOME/bin:$PATH CARGO_TARGET_DIR=/tmp/potter-p4b scripts/vm-tests.sh /Users/alfonso/Github/Personal/playground/vmkit/out/vmlinux-6.18.54-aarch64'`
- **Fixed sizes (decisions D6, D9):** output disk 8 GiB (`vm::OUTPUT_SIZE`, unchanged), scratch 8 GiB (`kiln_map::SCRATCH_BYTES = 8 << 30`), cache disk 8 GiB (`caches::CACHE_DISK_BYTES = 8 << 30`), boot timeout `kiln_map::boot_timeout(memory_mib)` = max(60 s, 45 s × ⌈memory_mib / 1024⌉), so 180 s at 4096 MiB (amended at the final review: a fixed 60 s failed 4 GiB guests under nested virtualization), computed in `kiln_spec` from the spec's memory, with `VmConfig.boot_timeout: Option<Duration>` as an explicit override, memory 4096 MiB and 2 vCPUs (unchanged).

## Decisions

Each is a choice the code could not settle. The task that implements it cites its number.

- **D1. One VM per chain segment, egress per step.** A VM boots with `network = true` when any `Run` step in its segment needs network, and kiln toggles egress per step (`Vm::set_egress`; closed means no DNS and cuts connections opened earlier). P4a's rule, a new VM whenever a step's network differs, is dropped. Why: it saves a boot per network transition (a typical `deps` (network) → `build` (none) chain booted twice). kiln kills every process when a step's command exits, so nothing from a networked step outlives it into a closed one. Cost: a NIC device slot for the whole segment.
- **D2. potter counts lowers and restarts before kiln's cap.** `Session` tracks `root + stacked + sealed`, the formula behind kiln's `BuildSpec::lowers`. Before a step whose `lowers + pending > MAX_BUILD_LOWERS` (230), potter finishes the VM and boots a new one from the current prefix (squashed to the device budget as usual). If kiln's `Error::TooManyLowers` is ever reached, it is a potter bug: exit 4.
- **D3. Pre-send refusals are classified by potter itself.** kiln's `run_step` returns `Error::Refused` both for a refused step (the VM stays usable) and for a dead VM. The adapter keeps a mirror of kiln's `stacked`/`sealed` state and calls `kiln_run::build::BuildSpec::proto_step` before `run_step`. A refusal there is exit 2, naming the step: an env name with `=` or NUL, a bad target, secrets over a frame, more than 64 mounts or secrets. The one exception is `TooManyLowers`, which is exit 4 (D2). Every `run_step` error after that means the VM is gone.
- **D4. Guest protocol violations are exit 3 by message text.** kiln has no typed violation error. A post-send error whose text contains `guest protocol violation` or `StepSealed for step` (both pinned by kiln 72c82570, `linux/build.rs` and `build.rs`) is `BuildError::Hostile` (exit 3). Any other post-send error is exit 4. A kiln follow-up asks for a typed variant.
- **D5. `finish()` after a successful chain is a warning.** Every diff was host-verified and stored before `finish`, so an unclean shutdown can't corrupt the build: the build succeeds and potter warns `the build VM did not shut down cleanly: …`. After a failed step, the session is dropped and `finish`'s result is ignored. A timed-out step already killed the VM.
- **D6. `source_date_epoch` is always `Some(tarball::epoch())`.** That is `SOURCE_DATE_EPOCH`, or 0. Cache keys depend on identical diff bytes across hosts, and without the clamp every new file would carry the guest's clock. With the default 0 this matches the ns sealer, which writes every mtime as `epoch()`. With `SOURCE_DATE_EPOCH` set, kiln clamps to at most that value, while ns sets every mtime to it; vm and ns keys differ by class anyway (P3).
- **D7. Persistent cache disks live apart from ns cache directories.** Each cache id is one ext4 image at `$POTTER_HOME/vm-mounts/<tenant>/<id>.ext4`, made from kiln's template (`kiln_image::scratch::create_scratch`, sparse, `CACHE_DISK_BYTES`). It is locked with `flock` on `<id>.lock` beside it for the VM's whole life. If another build holds it, potter uses an empty scratch ext4 in the VM directory and warns, as the ns executor does. A separate root means an ns directory can never collide with a vm file.
- **D8. A cache disk is discarded when its VM dies abnormally.** That is a `run_step` error or a `TimedOut` step, both of which kill the VM. A guest killed mid-write can leave the ext4 inconsistent, and kiln-init's failing mount would then fail every later build with exit 4. Caches are best-effort, so potter deletes the persistent image (and keeps the lock until drop). A step that merely fails keeps its caches.
- **D9. Cache disks are attached at boot, one per id, for the whole segment.** No hotplug. The segment ahead of a VM collects every `Run` step's (id, cleaned target). A step whose id is already mapped to another target, or whose target is mapped to another id, ends the segment: it runs in a new VM. Cache disks count toward the device budget (`plan_disks`'s `mounts`). The size is 8 GiB because Cloud Hypervisor 53 can't grow a disk online past about 8 GiB.
- **D10. Targets are cleaned before they reach kiln.** `clean_target` drops empty and `.` components and one trailing `/`, and refuses `..` and `/` itself (exit 2). A secret target written with a trailing `/` is refused (exit 2: a secret is a file), as kiln-proto refuses it. Cache keys keep the plan's raw strings, so cleaning changes no key.
- **D11. Kernel, kiln-init and VMM come from flags or the environment, with no auto-fetch yet.** The flags are `--kernel` / `POTTER_KERNEL`, `--kiln-init` / `POTTER_KILN_INIT`, and `--vmm firecracker|cloud-hypervisor` / `POTTER_VMM` (default `firecracker`). The VMM binary comes from vmkit's discovery (`VMKIT_FIRECRACKER` / `VMKIT_CLOUD_HYPERVISOR` or `PATH`), the sandbox helper from `VMKIT_SANDBOX`. The kernel is published as an OCI artifact (`ghcr.io/alfonsocampodonico/vmkit-kernels/base:6.18.54-<arch>`, layer `application/vnd.vmkit.kernel.v1.binary`), but kiln-init is published nowhere and kiln pins neither ("this kiln pins neither yet"), so a digest-pinned auto-fetch would cover half the problem. Missing paths are exit 4, with messages that say where to get each file. Auto-fetch comes once kiln pins both.
- **D12. Still no default executor.** The spec's default is vm, but a VM build can't run without D11's paths, so `potter build` with no `--executor` keeps exiting 4, now naming both executors. The default becomes vm when kiln pins a kernel and init.
- **D13. Default VMM: Firecracker.** The spec recommends Cloud Hypervisor for its 31 devices, but CH 53 on aarch64 under nested virtualization stalls (block and net) and can't grow disks past about 8 GiB. Firecracker is kiln's default and reliable, and works with squashing (17 devices on x86_64). CH stays available through `--vmm`.
- **D14. `max_devices()` is the VMM's available devices, capped.** The adapter returns `min(available_devices, MAX_LAYERS + FIXED_DEVICES)`: `MAX_LAYERS` is 128, the most layer and cache disks kiln-proto allows. potter subtracts `fixed_devices(network)` itself, as in P4a. kiln's `max_devices(vmm, net)` already subtracts the fixed devices, so the adapter doesn't call it (no double subtraction). `plan_disks` also caps its budget at 128 minus the cache disks.
- **D15. Exit 125 gets a hint.** A failed step whose output has a line starting `kiln-init: ` and whose exit is `Code(125)` reports `exit status: 125 (kiln-init could not set the step up)`. Otherwise 125 is the command's own code, reported as usual.
- **D16. Errors from `BuildVm::start` are exit 4.** That covers a VMM not found, a boot timeout, a version 1 or 2 kiln-init refused build mode, a bad kernel and every other failure, as `the build VM did not start: <kiln's error, with the console tail>`.

## Error mapping (complete)

| kiln | When | potter |
|---|---|---|
| any `Error` from `BuildVm::start` / `BuildSpec::check` | boot | `Infra` (4), D16 |
| `Error::TooManyLowers` from `proto_step` | pre-send | `Infra` (4), "potter counts lowers; this is a bug", D2 |
| `Error::Refused` / `Proto` / `Invalid` from `proto_step` | pre-send | `Plan` (2), `step <name>: <kiln's reason>`, D3 |
| any `Error` from `run_step` whose text has `guest protocol violation` or `StepSealed for step` | post-send | `Hostile` (3), D4 |
| any other `Error` from `run_step` (VM ended, `InitFailed`, egress switch failed, listener stopped, `Image`/`Store`/`Vmm`/`Io`) | post-send | `Infra` (4), the VM is gone |
| `Exit::Code(0)` + `sealed: Some` | step | ingest through `OutputDisk::take` (3 on any mismatch) |
| `Exit::Code(0)` + `sealed: None` | step | `Hostile` (3), unchanged P4a rule |
| `Exit::Code(c != 0)` | step | `StepFailed` (1), with the D15 hint when `c == 125` |
| `Exit::Signal(s)` | step | `StepFailed` (1), `signal: s` |
| `Exit::TimedOut` | step | `StepFailed` (1), `timed out after Ns`; the VM is gone, D8 |
| `Error` from `finish` | end | after success a warning (D5), after failure ignored |

## Review Focus

1. **Cache targets spelled the way Dockerfiles and BuildKit write them** (`target=/root//.cache/`, `./`), or the same cache id at two targets in one chain. Expected: the target is cleaned, and the chain runs, with a second VM for the conflicting step; no exit 2 and no kiln refusal. Test: Task 6 `one_id_at_two_targets_splits_the_chain_into_two_vms` and `cache_mounts_attach_one_ext4_disk_per_id_and_steps_name_their_targets`.
2. **A build step that needs user namespaces, `mknod` or a writable `/proc/sys`.** Examples: rootless tooling, bubblewrap, Chrome's sandbox in a test step, `sysctl -w`, a package that makes device nodes. Expected: the step fails with exit 1 and its own error in the tail, and the README says why. Test: Task 9 `user_namespaces_are_refused_in_a_step`.
3. **A VM killed mid-step (a timeout, the VMM dying) while a cache disk is attached.** Expected: the build fails (1 for a timeout, 4 for a dead VM), the persistent cache disk is discarded, and the next build starts with an empty cache instead of failing forever on a broken ext4. Test: Task 6 `a_vm_that_dies_or_times_out_discards_its_cache_disks`.
4. **Secrets near kiln's frame limit**, such as a CA bundle or a big `.npmrc`; about 44 KiB of secrets fit one `RunStep`. Expected: exit 2 naming the step and the limit, before the step is sent, and no VM state lost. Test: Task 4 `secrets_over_a_frame_fail_with_2_before_the_step_is_sent`.
5. **Two hosts building the same chain must seal identical bytes**, or the remote cache misses everywhere. Expected: `source_date_epoch` is `Some(0)` without `SOURCE_DATE_EPOCH`, and two `--no-cache` VM builds produce the same layer digests. Test: Task 4 `every_vm_clamps_mtimes_to_source_date_epoch` and Task 9 `two_vm_builds_seal_identical_layers`.

---

### Task 1: Pin kiln 72c82570 and vmkit 924ae24

**Files:**
- Modify: `crates/potter-exec/Cargo.toml` (kiln revs, vmkit rev, add `kiln-proto` and `kiln-run`)
- Modify: `crates/potter/Cargo.toml` (kiln revs in `[dependencies]` and `[dev-dependencies]`, vmkit rev)
- Modify: `Cargo.lock`
- Modify: any potter source that kiln's API change breaks (expected: none, or a `KilnConfig`/`ContainerConfig` literal)

**Interfaces:**
- Produces: `kiln_run` (`kiln_run::build::{BuildSpec, RunStep, Secret, Stream, Exit, StepOutcome, FIXED_DEVICES, STEP_GRACE, KILL_WAIT}`, `kiln_run::{VmmKind, Error}`; on Linux `kiln_run::{BuildVm, backend}`) and `kiln_proto` (`MAX_BUILD_LOWERS`, `MAX_LAYERS`, `check_target`, `Command`, `StepSealed`) available to `potter-exec`.

- [ ] **Step 1: Replace every kiln rev.** In both Cargo.toml files, replace `b96a9a6bd8f6b34e840cffbf2703288295342e31` with `72c82570e6306c9ec2d60ae0473065110a8889d2`, and `b1baf3a0712209dbe7c7659186ad02b702524d3e` with `924ae248ee09d7969bb865085d4b6a19e1736eee`.
- [ ] **Step 2: Add to `crates/potter-exec/Cargo.toml` `[dependencies]`**:

```toml
kiln-proto = { git = "https://github.com/AlfonsoCampodonico/kiln", rev = "72c82570e6306c9ec2d60ae0473065110a8889d2" }
kiln-run = { git = "https://github.com/AlfonsoCampodonico/kiln", rev = "72c82570e6306c9ec2d60ae0473065110a8889d2" }
```

- [ ] **Step 3: Update the lock and check there is one vmkit.**
Run: `cargo update -p vmkit -p kiln-store && cargo tree --target all -i vmkit -e normal | head -3 && grep -c '^name = "vmkit"' Cargo.lock`
Expected: the tree's first line is `vmkit v… (https://github.com/AlfonsoCampodonico/vmkit?rev=924ae248…#924ae248)`, and the count is `1`.
- [ ] **Step 4: Build and test everything.**
Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: PASS. kiln-erofs did not change between b96a9a6 and 72c82570 (`git diff --stat b96a9a6 72c82570 -- crates/kiln-erofs` is empty), so `disks::CACHE_VERSION` stays. If a kiln type change breaks a struct literal, add the new field with its default value.
- [ ] **Step 5: Commit.**

```bash
git add Cargo.lock crates/potter-exec/Cargo.toml crates/potter/Cargo.toml
git commit -m "build: pin kiln 72c8257 (build mode) and vmkit 924ae24, the rev kiln pins"
```

---

### Task 2: The kiln mapping, cross-platform

**Files:**
- Create: `crates/potter-exec/src/vm/kiln_map.rs`
- Modify: `crates/potter-exec/src/vm/mod.rs` (add `pub mod kiln_map;`)
- Modify: `crates/potter-exec/src/vm/backend.rs` (`BuildSpec.source_date_epoch`)
- Modify: `crates/potter-exec/src/vm/disks.rs` (`plan_disks` caps at `MAX_LAYERS`)
- Modify: every `BuildSpec { … }` literal (`vm/mod.rs`, `vm/fake.rs` tests): add `source_date_epoch: None` for now (Task 4 sets it)

**Interfaces:**
- Consumes: Task 1's crates.
- Produces, in `potter_exec::vm::kiln_map`:
  - `pub const SCRATCH_BYTES: u64 = 8 << 30;` `pub fn boot_timeout(memory_mib: u32) -> Duration` (max(60 s, 45 s per GiB rounded up); was `BOOT_TIMEOUT` = 60 s before the final review) `pub use kiln_proto::MAX_BUILD_LOWERS;`
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct VmConfig { pub vmm: kiln_run::VmmKind, pub kernel: PathBuf, pub init: PathBuf, pub scratch_bytes: u64, pub boot_timeout: Option<Duration> }` and `impl VmConfig { pub fn new(vmm: VmmKind, kernel: PathBuf, init: PathBuf) -> Self }`, which fills in `SCRATCH_BYTES` and no boot timeout override.
  - `pub fn device_cap(available: u32) -> usize`: `min(available, MAX_LAYERS + FIXED_DEVICES)` (D14).
  - `pub fn kiln_spec(spec: &BuildSpec, cfg: &VmConfig) -> Result<kiln_run::build::BuildSpec, BuildError>`: `cpus` over 255 is `Infra`.
  - `pub fn kiln_step(step: &RunStep) -> kiln_run::build::RunStep`
  - `pub fn outcome(o: kiln_run::build::StepOutcome) -> StepOutcome` and `pub fn stream(s: kiln_run::build::Stream) -> Stream`
  - `pub fn start_error(e: kiln_run::Error) -> BuildError` (D16), `pub fn refusal(step: u32, e: kiln_run::Error) -> BuildError` (D2, D3), `pub fn vm_error(e: kiln_run::Error) -> BuildError` (D4)
  - `#[derive(Debug, Default)] pub struct Mirror { stacked: BTreeSet<usize>, sealed: usize }` with `pub fn check(&self, spec: &kiln_run::build::BuildSpec, step: &kiln_run::build::RunStep) -> Result<(), BuildError>` (calls `spec.proto_step(step, &self.stacked, self.sealed)` and maps errors with `refusal`) and `pub fn record(&mut self, step: &kiln_run::build::RunStep, outcome: &kiln_run::build::StepOutcome)` (extends `stacked` by `lowers_added` whatever the exit, since kiln keeps a failed step's layers stacked, and increments `sealed` when `outcome.sealed.is_some()`).
  - `backend::BuildSpec` gains `pub source_date_epoch: Option<u64>`.

- [ ] **Step 1: Write the failing tests** in `kiln_map.rs` `#[cfg(test)] mod tests`. Use the helpers `spec() -> BuildSpec` (2 layers, root 1, one cache disk `("/c", "/root/.cache")`, network true, 4096 MiB, 2 cpus, `source_date_epoch: Some(0)`), `cfg() -> VmConfig::new(VmmKind::Firecracker, "/k".into(), "/i".into())` and `step(id) -> RunStep`.

```rust
#[test]
fn a_potter_spec_maps_field_for_field() {
    let k = kiln_spec(&spec(), &cfg()).unwrap();
    assert_eq!((k.vmm, k.kernel.as_path(), k.init.as_path()), (VmmKind::Firecracker, Path::new("/k"), Path::new("/i")));
    assert_eq!((k.layers.len(), k.root, k.cache_disks.len()), (2, 1, 1));
    assert_eq!((k.network, k.memory_mib, k.cpus, k.source_date_epoch), (true, 4096, 2u8, Some(0)));
    assert_eq!((k.scratch_bytes, k.boot_timeout), (SCRATCH_BYTES, boot_timeout(4096)));
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
    assert_eq!((k.secrets[0].0.as_str(), k.secrets[0].1.0.as_slice()), ("/run/secrets/npm", &b"tok"[..]));
    assert_eq!(kiln_step(&step(1)).command, kiln_proto::Command::Shell("true".into()));
    let sealed = kiln_proto::StepSealed { id: 3, offset: 0, len: 4096, sha256: "ab".repeat(32), entries: 2 };
    let o = outcome(kiln_run::build::StepOutcome { exit: kiln_run::build::Exit::Code(0), sealed: Some(sealed) });
    assert_eq!(o.exit, Exit::Code(0));
    assert_eq!(o.sealed.unwrap(), StepSealed { id: 3, offset: 0, len: 4096, sha256: "ab".repeat(32), entries: 2 });
    for (k, p) in [(kiln_run::build::Exit::Signal(9), Exit::Signal(9)), (kiln_run::build::Exit::TimedOut, Exit::TimedOut)] {
        assert_eq!(outcome(kiln_run::build::StepOutcome { exit: k, sealed: None }).exit, p);
    }
    assert_eq!(stream(kiln_run::build::Stream::Stderr), Stream::Stderr);
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
    let full = Mirror { stacked: BTreeSet::new(), sealed: MAX_BUILD_LOWERS };
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
    assert_eq!(start_error(kiln_run::Error::Refused("Hello 2 (build mode needs version 3)".into())).exit_code(), 4);
}

#[test]
fn the_mirror_counts_layers_a_failed_step_stacked() {
    let k = kiln_spec(&spec(), &cfg()).unwrap();
    let mut m = Mirror::default();
    let mut s = step(1);
    s.lowers_added = vec![1];
    let ks = kiln_step(&s);
    m.check(&k, &ks).unwrap();
    m.record(&ks, &kiln_run::build::StepOutcome { exit: kiln_run::build::Exit::Code(125), sealed: None });
    assert!(m.check(&k, &ks).is_err(), "layer 1 is stacked even though the step failed");
    assert_eq!(device_cap(31), 31);
    assert_eq!(device_cap(400), 132);
}
```

Add to `disks.rs`'s `the_budget_squashes_only_what_does_not_fit`:

```rust
// kiln-proto allows 128 layer and cache disks, whatever the VMM has.
assert_eq!(plan_disks(200, 0, 300, 0, false).unwrap(), DiskPlan { squash: 73, disks: 128, spare: 0 });
assert_eq!(plan_disks(200, 8, 300, 0, false).unwrap().disks, 120);
```

- [ ] **Step 2: Run the tests to check they fail.**
Run: `cargo test -p potter-exec vm::kiln_map vm::disks`
Expected: FAIL to compile (`kiln_map` missing, `source_date_epoch` unknown).
- [ ] **Step 3: Implement** `kiln_map.rs` as the Interfaces block says. In `plan_disks`, cap the budget with `.min((kiln_proto::MAX_LAYERS as usize).saturating_sub(mounts))` before the reserve split. `refusal` matches `Error::TooManyLowers { .. }` to `BuildError::Infra(format!("step {step}: {e} (potter counts lowers and restarts before kiln's cap: this is a potter bug)"))`, and maps everything else to `BuildError::Plan(format!("step {step}: {e}"))`. `vm_error` checks `e.to_string()` for the two texts of D4. `start_error` is `BuildError::Infra(format!("the build VM did not start: {e}"))`.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test -p potter-exec vm:: && cargo clippy -p potter-exec --all-targets -- -D warnings`
Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add crates/potter-exec/src/vm
git commit -m "feat(vm): map potter's build VM types onto kiln's and classify every kiln error"
```

---

### Task 3: The fake follows kiln's rules

**Files:**
- Modify: `crates/potter-exec/src/vm/fake.rs`

**Interfaces:**
- Consumes: `kiln_map::{kiln_spec, kiln_step, VmConfig, Mirror, start_error, vm_error}`.
- Produces:
  - `Script::SetupFail { line: String }`: the step exits `Code(125)` and prints `line` on stderr. A test passes a line starting `kiln-init: `.
  - `Script::Dies { why: String }`: `run_step` returns `Err(vm_error(kiln_run::Error::Refused(format!("step {id}: {why}"))))`, and the VM is dead from then on.
  - `Script::Hang` now also leaves the VM dead: kiln kills the VM on a timeout.
  - `FakeState.finish_fails: bool`: `finish` returns `Err(Infra)` when set.
  - A dead VM's `run_step` and `finish` return `Err(Infra("the build VM has ended: …"))`.
  - `FakeBackend::start` checks the spec as kiln does: `kiln_spec(&spec, &VmConfig::new(VmmKind::Firecracker, "/fake/kernel".into(), "/fake/init".into()))?`, then `.check(self.max_devices as u32).map_err(start_error)?`.
  - `FakeVm::run_step` runs `Mirror::check` before recording the step in `FakeState.steps`. A refused step is not recorded and leaves the VM usable. `Mirror::record` runs after every played step.

- [ ] **Step 1: Write the failing tests** in `fake.rs`:

```rust
#[test]
fn a_timed_out_or_dead_vm_refuses_more_steps_and_finish() {
    for script in [Script::Hang, Script::Dies { why: "the VM ended (Killed)".into() }] {
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
```

`one_vm(script) -> (FakeBackend, BuildSpec, tempfile::TempDir)` is a test helper that builds a 31-device fake and a spec with a 1 MiB output file, `source_date_epoch: None`. Refactor `scripted_diffs_ingest_and_lies_do_not` onto it.

- [ ] **Step 2: Run the tests to check they fail.**
Run: `cargo test -p potter-exec vm::fake`
Expected: FAIL (the variants don't exist; Hang doesn't kill the VM).
- [ ] **Step 3: Implement** the Interfaces block. `FakeVm` gains `kspec: kiln_run::build::BuildSpec`, `mirror: Mirror` and `dead: Option<String>`. The fake's diffs already start where the previous one ended (lengths are multiples of 512 rounded up to `ALIGN`), so kiln's contiguity rule holds. Keep `Lie::Overlap`: it is what kiln itself would refuse, and potter's ingest must refuse it too.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test -p potter-exec vm::`
Expected: PASS, the existing `chain_tests` included (they never exceed a budget the fake now checks).
- [ ] **Step 5: Commit.**

```bash
git add crates/potter-exec/src/vm/fake.rs
git commit -m "test(vm): the fake build VM checks steps and specs with kiln's own rules"
```

---

### Task 4: Session lifecycle, the lowers cap, targets and mtimes

**Files:**
- Modify: `crates/potter-exec/src/vm/mod.rs` (`Session`, `clean_target`)
- Modify: `crates/potter-exec/src/chain.rs` (the vm branch of the `Run` arm; the end of `run_steps`)
- Test: `crates/potter-exec/src/vm/chain_tests.rs`

**Interfaces:**
- Consumes: Task 2's `MAX_BUILD_LOWERS`, Task 3's scripts.
- Produces:
  - `Session` fields `lowers: usize` (starts at `spec.root`) and `failed: bool`.
  - `pub fn has_room(&self) -> bool`: `self.lowers + self.pending.len() <= MAX_BUILD_LOWERS`.
  - `pub fn finish(mut self) -> Result<(), BuildError>`: takes the VM and calls `finish`. `Drop` still finishes a VM nobody finished, ignoring the result.
  - `Session::run` adds `lowers_added.len()` to `lowers` after any `Ok`, plus 1 when `sealed.is_some()`. It sets `failed` on `Err` and on `Exit::TimedOut`.
  - `Session::start` sets `BuildSpec.source_date_epoch = Some(crate::tarball::epoch())` (D6).
  - `pub fn clean_target(what: &str, raw: &str, file: bool) -> Result<String, BuildError>` in `vm/mod.rs` (D10). It returns `/` plus the non-empty, non-`.` components joined by `/`. These are `Plan` errors: a `..` component; no component left (`/`); `file && raw.ends_with('/')`. Last, `kiln_proto::check_target(what, &out)`, whose failure is also `Plan`.
  - In `chain.rs`: `fn end_session(session: &mut Option<Session>, progress: &mut dyn FnMut(Progress))` takes the session and calls `finish`, emitting `Progress::Warning(format!("the build VM did not shut down cleanly: {e}"))` on `Err` (D5). It runs when a VM must be replaced (no room, Task 5's network, Task 6's caches) and after the loop when the chain succeeded. On failure the session is just dropped.
  - Before a miss runs in an existing session: `if session.as_ref().is_some_and(|vm| !vm.has_room()) { end_session(..) }`, and the normal boot path then starts a VM from `state.layers` (D2).
  - Secret targets pass through `clean_target(&format!("step {sname}: secret {}", s.id), &s.target, true)`.
  - The status of a `Code(125)` failure whose tail has a line starting `kiln-init: ` is `exit status: 125 (kiln-init could not set the step up)` (D15).

- [ ] **Step 1: Write the failing tests** in `chain_tests.rs`:

```rust
#[test]
fn a_chain_past_the_lowers_cap_restarts_from_its_prefix() {
    let w = world();
    let names: Vec<String> = (0..235).map(|i| format!("s{i}")).collect();
    let t = target(&w, names.iter().map(|n| run(n)).collect());
    let fake = FakeBackend::new(31, names.iter().map(|n| Script::files(&[(&format!("app/{n}"), "1")])).collect());
    let state = vm(&w, &t, &fake).unwrap();
    assert_eq!(state.executed, 235);
    let f = fake.state();
    assert_eq!(f.specs.len(), 2, "one restart");
    assert_eq!(f.specs[0].root, 1);
    assert!(f.specs[1].root <= 27, "the new VM boots from the squashed prefix: {}", f.specs[1].root);
    assert_eq!(f.steps.len(), 235, "kiln refused nothing");
    assert_eq!(f.finished, 2);
}

#[test]
fn every_vm_clamps_mtimes_to_source_date_epoch() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    vm(&w, &t, &fake).unwrap();
    // SOURCE_DATE_EPOCH is unset in the test environment: potter's default, 0.
    assert_eq!(fake.state().specs[0].source_date_epoch, Some(0));
}

#[test]
fn a_failed_finish_after_a_good_chain_is_a_warning() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    fake.state().finish_fails = true;
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    r.unwrap();
    // with_ctx collects warnings as lines prefixed "warning: " (extend it if needed).
}

#[test]
fn a_vm_that_dies_mid_step_fails_with_4_and_a_violation_with_3() {
    for (why, code) in [("the VM ended (Killed)", 4), ("guest protocol violation: Running out of order", 3)] {
        let w = world();
        let t = target(&w, vec![run("a")]);
        let fake = FakeBackend::new(31, vec![Script::Dies { why: why.into() }]);
        let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
        assert_eq!(r.unwrap_err().exit_code(), code, "{why}");
        assert_eq!(index_entries(&w), 0);
        assert_eq!(runs_left(&w), 0);
    }
}

#[test]
fn a_setup_failure_exits_1_and_says_kiln_init_could_not_set_the_step_up() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let line = "kiln-init: set up the step's mounts: /run/secrets/x: not a directory";
    let fake = FakeBackend::new(31, vec![Script::SetupFail { line: line.into() }]);
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    let e = r.unwrap_err();
    assert_eq!(e.exit_code(), 1);
    assert!(e.to_string().contains("exit status: 125 (kiln-init could not set the step up)"), "{e}");
    assert!(lines.iter().any(|l| l == line));
}

#[test]
fn secrets_over_a_frame_fail_with_2_before_the_step_is_sent() {
    let w = world();
    let mut s = run("a");
    if let Step::Run { secrets, .. } = &mut s {
        secrets.push(SecretRef { id: "big".into(), target: "/run/secrets/big".into() });
    }
    let t = target(&w, vec![s]);
    let fake = FakeBackend::new(31, vec![]);
    let secrets = BTreeMap::from([("big".to_string(), vec![b'x'; 50 << 10])]);
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &secrets);
    let e = r.unwrap_err();
    assert_eq!(e.exit_code(), 2, "{e}");
    assert!(e.to_string().contains("step a"), "{e}");
    assert!(fake.state().steps.is_empty());
}

#[test]
fn targets_are_cleaned_and_a_secret_target_ending_in_a_slash_is_refused() {
    assert_eq!(crate::vm::clean_target("t", "/run//secrets/./npm", true).unwrap(), "/run/secrets/npm");
    assert_eq!(crate::vm::clean_target("t", "/root//.cache/", false).unwrap(), "/root/.cache");
    for (raw, file) in [("/run/secrets/x/", true), ("/", false), ("//", false), ("/a/../b", false)] {
        assert_eq!(crate::vm::clean_target("t", raw, file).unwrap_err().exit_code(), 2, "{raw}");
    }
}
```

Extend `with_ctx` so that `Progress::Warning(w)` lands in the collected lines as `format!("warning: {w}")`, and assert in `a_failed_finish_after_a_good_chain_is_a_warning` that one line starts with `warning: the build VM did not shut down cleanly`.

- [ ] **Step 2: Run the tests to check they fail.**
Run: `cargo test -p potter-exec vm::chain_tests`
Expected: FAIL. The 235-step chain hits the fake's `TooManyLowers` (exit 4), `source_date_epoch` is `None`, there is no warning, and the 125 status has no hint.
- [ ] **Step 3: Implement** the Interfaces block in `vm/mod.rs` and `chain.rs`. The existing `drop(vm_session)` after the loop becomes: on `Ok`, `end_session(&mut vm_session, ctx.progress)`; on `Err`, drop.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test -p potter-exec && cargo clippy -p potter-exec --all-targets -- -D warnings`
Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add crates/potter-exec/src/vm crates/potter-exec/src/chain.rs
git commit -m "feat(vm): restart before kiln's lowers cap, clamp mtimes, clean targets, finish VMs explicitly"
```

---

### Task 5: Per-step egress in one VM (D1)

**Files:**
- Modify: `crates/potter-exec/src/chain.rs` (the boot-ahead loop and the "network differs" check)
- Modify: `crates/potter-exec/src/vm/backend.rs` (doc comment on `BuildSpec.network` and `RunStep.network`)
- Test: `crates/potter-exec/src/vm/chain_tests.rs`

**Interfaces:**
- Consumes: Task 4's `end_session`.
- Produces: the boot-ahead loop no longer breaks on `Step::Run { network: n, .. } if n != network`. The VM's network is `steps[i..segment_end]` having any `Run` step with `network: true`. The pre-run replacement check becomes `*network && !vm.network()`, which only fires when a segment break booted a VM without one.

- [ ] **Step 1: Replace** `steps_with_and_without_network_never_share_a_vm` with:

```rust
#[test]
fn steps_with_and_without_network_share_one_networked_vm() {
    let w = world();
    let t = target(&w, vec![net("a", false), net("b", true), net("c", true), net("d", false)]);
    let fake = FakeBackend::new(31, ["a", "b", "c", "d"].iter().map(|n| Script::files(&[(&format!("app/{n}"), "1")])).collect());
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.iter().map(|s| s.network).collect::<Vec<_>>(), [true]);
    assert_eq!(f.steps.iter().map(|s| s.network).collect::<Vec<_>>(), [false, true, true, false]);
    assert_eq!(f.finished, 1);
}

#[test]
fn a_chain_without_network_steps_boots_without_a_nic() {
    let w = world();
    let t = target(&w, vec![net("a", false), net("b", false)]);
    let fake = FakeBackend::new(31, ["a", "b"].iter().map(|n| Script::files(&[(&format!("app/{n}"), "1")])).collect());
    vm(&w, &t, &fake).unwrap();
    assert_eq!(fake.state().specs.iter().map(|s| s.network).collect::<Vec<_>>(), [false]);
}
```

Move the `net(name, on)` closure to a module-level helper `fn net(name: &str, on: bool) -> Step`.

- [ ] **Step 2: Run the tests to check they fail.**
Run: `cargo test -p potter-exec vm::chain_tests::steps_with`
Expected: FAIL: three VMs, `[false, true, false]`.
- [ ] **Step 3: Implement** the Interfaces block.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test -p potter-exec vm::`
Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add crates/potter-exec/src/chain.rs crates/potter-exec/src/vm
git commit -m "feat(vm): one VM per chain segment, egress opened and closed per step by kiln"
```

---

### Task 6: Cache mounts as ext4 cache disks (D7, D8, D9)

**Files:**
- Create: `crates/potter-exec/src/vm/caches.rs`
- Modify: `crates/potter-cache/src/lib.rs` (`PotterHome::vm_mounts`)
- Modify: `crates/potter-exec/src/vm/mod.rs` (`Session::start` takes a `Boot`; holds caches; `serves`; discard on drop)
- Modify: `crates/potter-exec/src/chain.rs` (remove the P4b refusal in `run_steps`; collect cache uses per segment; pass step mounts)
- Test: `crates/potter-exec/src/vm/chain_tests.rs`, `caches.rs` unit tests

**Interfaces:**
- Consumes: Task 4's `clean_target`, `end_session`.
- Produces:
  - `PotterHome::vm_mounts(&self, tenant: &Tenant) -> PathBuf` = `root/vm-mounts/<tenant>`.
  - In `vm::caches`:
    - `pub const CACHE_DISK_BYTES: u64 = 8 << 30;`
    - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct CacheUse { pub id: String, pub target: String }`: `target` already cleaned.
    - `pub fn add_uses(set: &mut Vec<CacheUse>, step: &[CacheUse]) -> bool`: adds the step's uses not already in `set`; `false`, with `set` unchanged, when an id is mapped to another target or a target to another id.
    - `pub struct HeldCache { pub path: PathBuf, pub target: String, persistent: bool, lock: Option<std::fs::File> }`
    - `pub fn acquire(root: &Path, uses: &[CacheUse], scratch: &Path, warn: &mut dyn FnMut(String)) -> Result<Vec<HeldCache>, BuildError>`: for each use, a nonblocking exclusive `flock` on `root/<id>.lock`. If it is free, the disk is `root/<id>.ext4`, created when missing via `create_scratch` into a temp path under `root`, then renamed. If it is busy, the disk is `scratch/cache-<id>.ext4` (`persistent: false`), with the warning `cache mount {id} is in use by another build; using an empty one`.
    - `pub fn discard(held: &[HeldCache])`: removes each persistent disk file, best-effort.
  - In `vm`: `pub struct Boot<'a> { pub prefix: &'a [LayerRef], pub later: &'a [(usize, LayerRef)], pub caches: &'a [CacheUse], pub cache_root: PathBuf, pub network: bool }`; `Session::start(backend: &dyn BuildBackend, store: &Store, home: &PotterHome, boot: Boot, warn: &mut dyn FnMut(String)) -> Result<Session, BuildError>`. `BuildSpec.cache_disks` is `held.iter().map(|h| (h.path.clone(), h.target.clone()))`, and `plan_disks` gets `mounts = caches.len()`.
  - `Session::serves(&self, uses: &[CacheUse]) -> bool`: every use is one of the session's.
  - `Session` drop: after finishing the VM, `if self.failed { caches::discard(&self.caches) }`, then the locks drop.
  - Chain: for each `Run` step, `uses` comes from its `cache_mounts`. The id must pass `potter_plan::is_name` (the ns message, moved to a shared `fn check_cache_id`), and the target goes through `clean_target(&format!("step {sname}: cache mount {}", m.id), &m.target, false)`. The boot-ahead loop runs `add_uses` for each `Run` step ahead, and a `false` ends the segment. The pre-run replacement check adds `|| !vm.serves(&uses)`. `RunStep.mounts` is the targets of `uses`.

- [ ] **Step 1: Write the failing tests.** In `caches.rs`:

```rust
#[test]
fn add_uses_refuses_an_id_or_target_mapped_twice() {
    let u = |id: &str, t: &str| CacheUse { id: id.into(), target: t.into() };
    let mut set = vec![];
    assert!(add_uses(&mut set, &[u("npm", "/root/.npm")]));
    assert!(add_uses(&mut set, &[u("npm", "/root/.npm"), u("pip", "/root/.cache/pip")]));
    assert_eq!(set.len(), 2);
    assert!(!add_uses(&mut set, &[u("npm", "/other")]));
    assert!(!add_uses(&mut set, &[u("yarn", "/root/.npm")]));
    assert_eq!(set.len(), 2, "unchanged after a conflict");
}
```

In `chain_tests.rs` (helper `fn cached(name: &str, mounts: &[(&str, &str)]) -> Step` builds a `run` step with those `CacheMount { id, target, sharing: Sharing::Exclusive }`):

```rust
#[test]
fn cache_mounts_attach_one_ext4_disk_per_id_and_steps_name_their_targets() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("npm", "/root//.npm/")]), cached("b", &[("npm", "/root/.npm")])]);
    let fake = FakeBackend::new(31, ["a", "b"].iter().map(|n| Script::files(&[(&format!("app/{n}"), "1")])).collect());
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.len(), 1);
    let disk = w.home.vm_mounts(&Tenant::local()).join("npm.ext4");
    assert_eq!(f.specs[0].cache_disks, [(disk.clone(), "/root/.npm".to_string())]);
    assert_eq!(f.steps.iter().map(|s| s.mounts.clone()).collect::<Vec<_>>(), [vec!["/root/.npm".to_string()]; 2]);
    assert_eq!(std::fs::metadata(&disk).unwrap().len(), crate::vm::caches::CACHE_DISK_BYTES);
}

#[test]
fn one_id_at_two_targets_splits_the_chain_into_two_vms() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("c", "/x")]), cached("b", &[("c", "/y")])]);
    let fake = FakeBackend::new(31, ["a", "b"].iter().map(|n| Script::files(&[(&format!("app/{n}"), "1")])).collect());
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    let targets: Vec<Vec<String>> = f.specs.iter().map(|s| s.cache_disks.iter().map(|(_, t)| t.clone()).collect()).collect();
    assert_eq!(targets, [vec!["/x".to_string()], vec!["/y".to_string()]]);
}

#[test]
fn a_cache_disk_in_use_by_another_build_gives_an_empty_one_and_a_warning() {
    let w = world();
    let root = w.home.vm_mounts(&Tenant::local());
    std::fs::create_dir_all(&root).unwrap();
    let lock = std::fs::File::create(root.join("npm.lock")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let t = target(&w, vec![cached("a", &[("npm", "/root/.npm")])]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    r.unwrap();
    assert!(lines.iter().any(|l| l.contains("cache mount npm is in use by another build")), "{lines:?}");
    assert!(!fake.state().specs[0].cache_disks[0].0.starts_with(&root));
}

#[test]
fn a_vm_that_dies_or_times_out_discards_its_cache_disks_and_a_failed_step_keeps_them() {
    for (script, kept) in [
        (Script::Hang, false),
        (Script::Dies { why: "the VM ended (Killed)".into() }, false),
        (Script::Fail { code: 1, lines: vec![] }, true),
    ] {
        let w = world();
        let t = target(&w, vec![cached("a", &[("npm", "/root/.npm")])]);
        let fake = FakeBackend::new(31, vec![script]);
        let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
        assert!(r.is_err());
        assert_eq!(w.home.vm_mounts(&Tenant::local()).join("npm.ext4").exists(), kept);
    }
}
```

Delete `cache_mounts_in_a_vm_build_are_refused_until_p4b`.

- [ ] **Step 2: Run the tests to check they fail.**
Run: `cargo test -p potter-exec vm::`
Expected: FAIL (the cache-mount refusal, exit 2, and missing items).
- [ ] **Step 3: Implement** the Interfaces block. Remove the `Step::Run { cache_mounts, .. } if !cache_mounts.is_empty()` refusal from `run_steps`. Keep the `ctx.backend.is_none()` check, and change its message to `VM builds need a build VM backend: pass --executor vm with --kernel and --kiln-init (potter's CLI does this), or --executor ns`.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test --all && cargo clippy --all-targets -- -D warnings`
Expected: PASS on macOS and Linux. `create_scratch` is platform-independent and the file is sparse.
- [ ] **Step 5: Commit.**

```bash
git add crates/potter-cache/src/lib.rs crates/potter-exec/src/vm crates/potter-exec/src/chain.rs
git commit -m "feat(vm): cache mounts in VM builds as persistent ext4 cache disks"
```

---

### Task 7: `KilnBackend` over `kiln_run::BuildVm` (Linux)

**Files:**
- Create: `crates/potter-exec/src/vm/kiln_backend.rs`
- Modify: `crates/potter-exec/src/vm/mod.rs` (`#[cfg(target_os = "linux")] pub mod kiln_backend;`)

**Interfaces:**
- Consumes: Task 2's mapping.
- Produces (Linux only):
  - `pub struct KilnBackend { store: kiln_store::Store, cfg: VmConfig, devices: usize }`
  - `impl KilnBackend { pub fn new(store: &Store, cfg: VmConfig) -> Result<Self, BuildError> }`. It refuses, with `Infra`, a kernel or init path that isn't a regular file: `the kernel {path} is not a file` / `kiln-init {path} is not a file`. Then it calls `kiln_run::backend(cfg.vmm).discover()`, mapping an error to `Infra(format!("VM builds need {}: {e}", cfg.vmm.name()))`, and sets `devices = device_cap(vmm.capabilities().available_devices())`.
  - `impl BuildBackend for KilnBackend`: `max_devices` returns `devices`; `start` does `kiln_spec` → `kiln_run::BuildVm::start(&self.store, kspec.clone()).map_err(start_error)` → `Box::new(KilnVm { vm, spec: kspec, mirror: Mirror::default() })`.
  - `struct KilnVm`, `impl BuildVm`:
    - `run_step` does `let k = kiln_step(&step); self.mirror.check(&self.spec, &k)?;`, then `self.vm.run_step(k.clone(), &mut |s, b| sink(stream(s), b)).map_err(vm_error)?`, then `self.mirror.record(&k, &o)` and returns `Ok(outcome(o))`.
    - `finish` maps an error to `Infra(format!("the build VM did not shut down cleanly: {e}"))`.

- [ ] **Step 1: Write the failing test** (Linux, no KVM needed):

```rust
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
```

- [ ] **Step 2: Run it to check it fails.**
Run (Linux: CI, or in Lima): `cargo test -p potter-exec vm::kiln_backend`
Expected: FAIL (the module doesn't exist).
- [ ] **Step 3: Implement** the Interfaces block.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test -p potter-exec vm::kiln_backend` (Linux), and `cargo build -p potter-exec` on macOS.
Expected: PASS; macOS still builds (the module is compiled out).
- [ ] **Step 5: Commit.**

```bash
git add crates/potter-exec/src/vm
git commit -m "feat(vm): KilnBackend runs potter's build VM over kiln_run::BuildVm"
```

---

### Task 8: `--executor vm` in the CLI, and the README

**Files:**
- Modify: `crates/potter/Cargo.toml` (`clap` features add `"env"`)
- Modify: `crates/potter/src/main.rs` (`VmArgs`, `check_executor` returns the backend, `build` and `rebase` use it)
- Modify: `crates/potter/tests/cli.rs`
- Modify: `README.md` (status table, the VM executor's requirements, differences from `docker build`)

**Interfaces:**
- Consumes: Task 7's `KilnBackend::new`, Task 2's `VmConfig::new`.
- Produces:
  - `#[derive(clap::Args, Clone)] struct VmArgs` flattened into `Cmd::Build` and `Cmd::Rebase`, with:
    - `#[arg(long, env = "POTTER_VMM", default_value = "firecracker", value_parser = parse_vmm)] vmm: kiln_run::VmmKind`, where `parse_vmm` is `str::parse::<VmmKind>`;
    - `#[arg(long, env = "POTTER_KERNEL")] kernel: Option<PathBuf>`;
    - `#[arg(long = "kiln-init", env = "POTTER_KILN_INIT")] kiln_init: Option<PathBuf>`.
  - `check_executor(executor: Option<Executor>, vm: &VmArgs) -> Result<Option<VmConfig>, ExitCode>`, with these exact messages, all exit 4:
    - `None`: `choose an executor: --executor vm builds in a microVM (Linux with KVM, --kernel and --kiln-init), --executor ns in Linux namespaces, which isolate less than a VM`
    - `Vm`, not Linux: `VM builds need Linux with KVM: run potter in a Linux VM (on macOS, Lima), where --executor ns works too`
    - `Vm`, no kernel: `VM builds need a vmkit kernel: pass --kernel <path> or set POTTER_KERNEL (oras pull ghcr.io/alfonsocampodonico/vmkit-kernels/base:6.18.54-<arch>), or pass --executor ns`
    - `Vm`, no kiln-init: `VM builds need a static kiln-init speaking protocol version 3 (kiln 72c8257 or later, built for <arch>-unknown-linux-musl): pass --kiln-init <path> or set POTTER_KILN_INIT, or pass --executor ns`
    - `Ns`, not Linux: unchanged.
  - After `open_homes`, a `Some(cfg)` becomes `KilnBackend::new(&store, cfg)` (its error goes through `fail(&e.to_string(), e.exit_code())`). `BuildCtx` gets `executor: ExecutorClass::Vm, backend: Some(&backend)`. A rebase dry run still needs no executor.

- [ ] **Step 1: Write the failing tests** in `cli.rs`, replacing `build_never_picks_the_namespace_executor_by_itself`:

```rust
fn bare() -> assert_cmd::Command {
    let mut c = potter();
    c.env_remove("POTTER_KERNEL").env_remove("POTTER_KILN_INIT").env_remove("POTTER_VMM");
    c
}

#[test]
fn build_never_picks_an_executor_by_itself() {
    bare().args(["build"]).arg(fixture("explicit-go")).assert().code(4)
        .stderr(predicate::str::contains("--executor vm").and(predicate::str::contains("--executor ns")));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_vm_build_off_linux_exits_4_and_says_why() {
    bare().args(["build", "--executor", "vm"]).arg(fixture("explicit-go")).assert().code(4)
        .stderr(predicate::str::contains("VM builds need Linux with KVM"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_vm_build_without_a_kernel_or_kiln_init_exits_4_naming_the_flag() {
    bare().args(["build", "--executor", "vm"]).arg(fixture("explicit-go")).assert().code(4)
        .stderr(predicate::str::contains("--kernel <path> or set POTTER_KERNEL"));
    bare().args(["build", "--executor", "vm", "--kernel", "/nonexistent"]).arg(fixture("explicit-go")).assert().code(4)
        .stderr(predicate::str::contains("--kiln-init <path> or set POTTER_KILN_INIT"));
    bare().args(["build", "--executor", "vm", "--kernel", "/nonexistent", "--kiln-init", "/nonexistent"])
        .arg(fixture("explicit-go")).assert().code(4)
        .stderr(predicate::str::contains("the kernel /nonexistent is not a file"));
}

#[test]
fn an_unknown_vmm_is_a_usage_error() {
    bare().args(["build", "--executor", "vm", "--vmm", "qemu"]).arg(fixture("explicit-go")).assert().code(2)
        .stderr(predicate::str::contains("unknown VMM"));
}
```

Update `rebase`'s CLI tests the same way if one asserts the old M3e message (`grep -n "M3e" crates/potter/tests/*.rs`).

- [ ] **Step 2: Run the tests to check they fail.**
Run: `cargo test -p potter --test cli`
Expected: FAIL (old messages; `--vmm` unknown).
- [ ] **Step 3: Implement** the Interfaces block. Remove the `VM` constant naming kiln M3e. Update the README:
  - The status row: `--executor vm (one microVM per target chain) | Works on Linux with KVM: --kernel, --kiln-init, Firecracker or Cloud Hypervisor`.
  - Replace the "built against a BuildBackend trait … waits for kiln's build mode" paragraph with: requirements (KVM, a VMM via `VMKIT_FIRECRACKER`/`VMKIT_CLOUD_HYPERVISOR` or `PATH`, `vmkit-sandbox` and its AppArmor profile, `pasta` and `nft` for network steps, the kernel and kiln-init per D11); one VM per chain segment with per-step egress (D1); cache mounts as 8 GiB ext4 disks under `$POTTER_HOME/vm-mounts/<tenant>/` (D7, D8).
  - A "What a VM build step can't do" list, from kiln's step isolation: no `CAP_MKNOD` (no device nodes); a read-only `/dev` with only `null`, `zero`, `full`, `random`, `urandom`, `tty`, `pts`, `shm` (no loop, fuse or block devices); `nodev` mounts; a read-only, partly masked `/proc` (`sysctl -w` fails); a read-only `/sys`; no user namespaces (`unshare -U`, rootless podman or buildah, bubblewrap and Chrome's sandbox fail: pass `--no-sandbox` to headless Chrome in build steps); only `user.*` (not `user.overlay.*`) and `security.capability` xattrs reach a layer; exit 125 with a `kiln-init:` line means the step's own setup failed (a secret, cache or `/etc` target the image makes impossible).
  - The `$POTTER_HOME` paragraph gains `vm-mounts/<tenant>/`.
- [ ] **Step 4: Run the tests to check they pass.**
Run: `cargo test -p potter --test cli && cargo clippy --all-targets -- -D warnings`
Expected: PASS (on macOS the Linux-only test is compiled out and the non-Linux one runs).
- [ ] **Step 5: Commit.**

```bash
git add crates/potter/Cargo.toml crates/potter/src/main.rs crates/potter/tests/cli.rs README.md Cargo.lock
git commit -m "feat(cli): --executor vm builds in kiln build-mode microVMs (--kernel, --kiln-init, --vmm)"
```

---

### Task 9: KVM end-to-end tests, the runner script, CI, HANDOFF and spec

**Files:**
- Create: `crates/potter/tests/vm.rs`
- Modify: `crates/potter/tests/support/mod.rs` (`VmEnv`, `vm_env()`, `Ctx::vm_chain`, a VM busybox root with the `unshare` and `wget` applets)
- Create: `scripts/vm-tests.sh`
- Modify: `.github/workflows/ci.yml` (job `vm-x86_64`)
- Modify: `docs/superpowers/HANDOFF.md` (P4b section; replace the "P4b =" bullet and mark the prototype-signature bullet superseded)
- Modify: `docs/superpowers/specs/2026-10-04-potter-design.md` (§9.3 rev; bump rev and changelog line)

**Interfaces:**
- Consumes: Task 7's `KilnBackend`, Task 8's CLI.
- Produces:
  - `support::VmEnv { pub kernel: PathBuf, pub init: PathBuf }` and `support::vm_env() -> Option<&'static VmEnv>`, read from `POTTER_TEST_KERNEL` and `POTTER_TEST_KILN_INIT`. When either is unset it returns `None`, unless `POTTER_REQUIRE_VM_TESTS=1`, which panics with `POTTER_REQUIRE_VM_TESTS is set but POTTER_TEST_KERNEL/POTTER_TEST_KILN_INIT are not`. Every test also needs `Ctx::new()` (the sandbox helper).
  - `Ctx::vm_chain(&self, target: &Target, repo: &Path, backend: &dyn BuildBackend) -> (Result<ChainState, BuildError>, Vec<String>)`: `executor: ExecutorClass::Vm`, collecting log and warning lines.
  - `struct Counting<'a> { inner: &'a dyn BuildBackend, boots: Cell<usize> }`, which implements `BuildBackend` by delegation and counts `start`.

- [ ] **Step 1: Write the tests** in `crates/potter/tests/vm.rs` (`#![cfg(target_os = "linux")]`; each returns early when `vm_env()` or `Ctx::new()` is `None`). Each uses `serve_busybox` with the VM root (busybox root plus applets `unshare`, `wget`, `nslookup`):

```rust
#[test] fn potter_build_in_a_vm_makes_an_image_and_then_hits_its_vm_entries()
// CLI: potter build --executor vm --kernel K --kiln-init I <project with two run steps>.
// The first run exits 0 and stderr has "running" for both steps. The second run exits 0
// and every step is "cached". The source manifest's records carry class "vm".

#[test] fn two_vm_builds_seal_identical_layers()
// The same project built twice with --no-cache: the two images' layer digests are equal.
// Without SOURCE_DATE_EPOCH, every entry's mtime in each step layer is 0 (read the layer
// tars from the store with tar::Archive and assert header.mtime() == 0).

#[test] fn a_cache_mount_persists_between_vm_builds()
// Step: `n=$(cat /root/.c/n 2>/dev/null || echo 0); echo $((n+1)) > /root/.c/n; echo count=$((n+1))`,
// cache mount id "c", target "/root//.c/". Two --no-cache builds: the logs say count=1, then count=2.
// The step layer holds no root/.c/n.

#[test] fn egress_opens_only_for_steps_that_ask_in_one_vm()
// Gated on POTTER_TEST_NETWORK=1 (as build.rs's network tests are). Via Ctx::vm_chain with Counting:
// step a (network false) runs `! nslookup example.com`; step b (true) runs `wget -q -O /dev/null http://example.com`;
// step c (false) runs `! wget -q -T 5 -O /dev/null http://example.com`. Ok, and boots == 1.

#[test] fn a_failing_step_and_a_setup_failure_report_their_tails()
// `echo out; echo err >&2; exit 3` fails with exit 1, and the tail has "out" and "err".
// A step whose secret target is /etc/passwd/x (a regular file's child) fails with exit 1,
// and the error contains "exit status: 125 (kiln-init could not set the step up)".

#[test] fn a_timed_out_step_fails_within_its_bound()
// timeout_secs 2, `sleep 600`: exit 1, "timed out after 2s", elapsed under 2 + 10 + 5 + 60 seconds
// (timeout + STEP_GRACE + KILL_WAIT + boot).

#[test] fn secrets_reach_the_step_and_never_the_layer()
// Secret "tok" = b"kiln-secret-p4b" at /run/secrets/tok; the step runs `test "$(cat /run/secrets/tok)" = kiln-secret-p4b && echo ok > /app/ok`.
// Ok, and no stored step layer contains the bytes "kiln-secret-p4b".

#[test] fn user_namespaces_are_refused_in_a_step()
// `unshare -U true`: exit 1, and the tail is non-empty (busybox's error). Documents Review Focus 2.
```

- [ ] **Step 2: Write `scripts/vm-tests.sh`** (executable). The kiln and vmkit checkouts are the ones cargo fetched for the pinned revs, so the guest and host always match.

```bash
#!/usr/bin/env bash
# Builds what potter's VM tests need and runs them (Linux with KVM).
# Usage: scripts/vm-tests.sh <vmkit kernel> [cargo test args...]
# Needs the VMMs on PATH, a static /usr/bin/busybox (busybox-static), uidmap and a subuid range,
# passt and nftables, the musl target for this arch, and vmkit-sandbox's AppArmor profile at
# $CARGO_TARGET_DIR/debug/vmkit-sandbox (the script prints the install command if it is missing).
set -euo pipefail
kernel=$(realpath "$1"); shift
root=$(cd "$(dirname "$0")/.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
musl=$(uname -m)-unknown-linux-musl
cd "$root"
manifest() { cargo metadata --locked --format-version 1 | python3 -c "import json,sys; print(next(p['manifest_path'] for p in json.load(sys.stdin)['packages'] if p['name'] == '$1'))"; }
kiln_ws=$(dirname "$(dirname "$(dirname "$(manifest kiln-run)")")")
vmkit_manifest=$(manifest vmkit)
cargo build --locked --release --manifest-path "$kiln_ws/Cargo.toml" --target "$musl" -p kiln-init --target-dir "$target/kiln"
cargo build --locked --manifest-path "$vmkit_manifest" --bin vmkit-sandbox --target-dir "$target"
if [ ! -e "/etc/apparmor.d/$(echo "$target/debug/vmkit-sandbox" | tr / . | sed 's/^\.//')" ] && [ -d /etc/apparmor.d ]; then
    echo "vm-tests: install the helper's AppArmor profile once: $(dirname "$vmkit_manifest")/scripts/install-apparmor.sh $target/debug/vmkit-sandbox" >&2
fi
export VMKIT_SANDBOX=$target/debug/vmkit-sandbox
export POTTER_TEST_KERNEL=$kernel
export POTTER_TEST_KILN_INIT=$target/kiln/$musl/release/kiln-init
export POTTER_REQUIRE_VM_TESTS=1 POTTER_REQUIRE_SANDBOX_TESTS=1
cargo test --locked -p potter --test vm -p potter-exec "$@"
```

Check the AppArmor profile file name `install-apparmor.sh` writes before you rely on the check above. If the name differs, print the hint unconditionally instead.

- [ ] **Step 3: Run the tests in Lima** (Global Constraints command).
Expected: every `vm.rs` test passes; `egress_opens_only_for_steps_that_ask_in_one_vm` passes too when run with `POTTER_TEST_NETWORK=1`. If a test fails, keep the run directories with `KILN_KEEP_RUN_DIR=1` and read the console log before changing anything.
- [ ] **Step 4: Add the CI job** to `.github/workflows/ci.yml`. It mirrors kiln's `boot-x86_64` job and potter's `sandbox` job:

```yaml
  # VM builds end to end: kiln build mode on Firecracker, x86_64 KVM (hosted arm64 runners have no
  # /dev/kvm; aarch64 runs in the Lima VM). kiln-init and vmkit come from the pinned revs.
  vm-x86_64:
    runs-on: ubuntu-24.04
    timeout-minutes: 60
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl
      - name: Allow KVM for the runner user
        run: |
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' | sudo tee /etc/udev/rules.d/99-kvm4all.rules
          sudo udevadm control --reload-rules
          sudo udevadm trigger --name-match=kvm
      - run: sudo apt-get update && sudo apt-get install -y uidmap busybox-static passt nftables jq flex bison bc libelf-dev libssl-dev
      - name: A subordinate id range for the runner user
        run: |
          grep -q "^$USER:" /etc/subuid || echo "$USER:200000:65536" | sudo tee -a /etc/subuid
          grep -q "^$USER:" /etc/subgid || echo "$USER:200000:65536" | sudo tee -a /etc/subgid
      - name: Find the pinned vmkit
        id: vmkit
        run: |
          vmkit=$(cargo metadata --locked --format-version 1 | jq -c '.packages[] | select(.name == "vmkit")')
          echo "dir=$(dirname "$(jq -r .manifest_path <<<"$vmkit")")" >> "$GITHUB_OUTPUT"
          echo "rev=$(jq -r .source <<<"$vmkit" | sed 's/.*#//')" >> "$GITHUB_OUTPUT"
      - run: '"${{ steps.vmkit.outputs.dir }}/scripts/install-vmms.sh" "$HOME/.local/bin"'
      - uses: actions/cache@v4
        with:
          path: out/vmlinux-*-x86_64
          key: vmkit-kernel-x86_64-${{ steps.vmkit.outputs.rev }}
      - name: Build vmkit's kernel (cached per vmkit revision)
        run: ls out/vmlinux-*-x86_64 2>/dev/null || "${{ steps.vmkit.outputs.dir }}/kernels/build.sh" x86_64 out
      - name: vmkit's sandbox helper and its AppArmor profile
        run: |
          cargo build --locked --manifest-path "${{ steps.vmkit.outputs.dir }}/Cargo.toml" --bin vmkit-sandbox --target-dir target
          "${{ steps.vmkit.outputs.dir }}/scripts/install-apparmor.sh" "$PWD/target/debug/vmkit-sandbox"
      - name: VM builds end to end
        run: |
          export PATH="$HOME/.local/bin:$PATH"
          scripts/vm-tests.sh out/vmlinux-*-x86_64 -- --test-threads=$(( $(nproc) > 1 ? $(nproc) / 2 : 1 ))
```

- [ ] **Step 5: Update the docs.**
  - HANDOFF: a new `**P4b (2026-11-21):**` section. It covers: the plan path; the pins (kiln 72c8257, vmkit 924ae24); D1–D16 one line each; the e2e suite and how to run it (Lima command, CI job); follow-ups. The follow-ups are: for kiln, a typed protocol-violation error (D4), kiln pinning a kernel and kiln-init so potter can fetch them and default to `vm` (D11, D12), `max_devices` reporting; for potter, services can now be unblocked since kiln 72c8257 includes M3d (a separate milestone), plus cache-disk gc and size flags. Replace the "P4b = pin the M3e kiln rev…" bullet with a pointer to the new section, and mark "M3e prototype signatures" as superseded by kiln 72c8257.
  - Spec §9.3: rev bump (next minor) with a changelog line. Text changes:
    - the protocol order: `Hello` (version 3), `Stage` 3–6, `Running`, then per step `StepExited` and `StepSealed`, and `Finish`;
    - the NIC is present only when a step in the VM's segment needs the network, and egress is opened and closed per step;
    - cache-mount disks are ext4, one per id, persistent per tenant;
    - the lower cap of 230 with a restart;
    - sealing details per kiln (`source_date_epoch` clamp, the xattr allowlist);
    - teardown is `Finish` (not `Shutdown`);
    - Firecracker is the default VMM (D13).
- [ ] **Step 6: Final check.**
Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all` (macOS), then the Lima command.
Expected: PASS everywhere. `vm.rs` tests are skipped on macOS, and run and pass in Lima.
- [ ] **Step 7: Commit.**

```bash
git add crates/potter/tests scripts/vm-tests.sh .github/workflows/ci.yml docs/superpowers
git commit -m "test(vm): KVM end-to-end VM builds in Lima and CI; docs for P4b"
```

---

## Not in this plan

- Fetching the kernel and kiln-init automatically, and making vm the default executor (D11, D12): after kiln pins both.
- `services` (kiln M3d is in 72c8257): its own milestone. The refusal in `check_supported` stays.
- Cache-disk size flags and gc of `vm-mounts/`.
- A typed kiln violation error (D4): a kiln follow-up.
