# microVM tooling roadmap

> This is the proposed replacement for `playground/ROADMAP.md` (2026-10-05). It is kept here because the session that wrote it couldn't edit outside the potter worktree. To apply it: `cp docs/superpowers/ROADMAP.md ../../ROADMAP.md`, run from the potter repo root after merging.

Three repositories, each a serious open-source project. Goals: stable formats and real docs, deep learning of the internals, and relevance to hosting platforms where customer images deploy to microVMs.

| Repo | Owns |
|---|---|
| **`vmkit`** | Firecracker and Cloud Hypervisor behind one `Vmm` trait; the unprivileged `sandbox` (generalised to run any process); `net` (per-VM netns, nftables, `pasta` egress, inbound port publishing); snapshot/restore; lazy restore underneath the VMM (codename **raku**) |
| **`kiln`** | The image format (erofs layers + init); `convert`, `pull`, `push`; `kiln-init` (PID 1, `services`, build mode); `run`; snapshot, restore and fork (the agent-sandbox core, codename **sagger**); snapshot artifacts in OCI registries with chunk dedup (codename **sherd**); `kiln-shim` (containerd shim v2, codename **slip**); `kiln-mcp` |
| **`potter`** | Source → kiln image: detectors for PHP/Laravel, Node, Bun, Deno, Python, Ruby, Go and Rust; the plan IR (codename **greenware**); the signed runtime catalog (codename **glaze**); VM and namespace executors; the Dockerfile frontend; tenant-scoped remote cache; the build daemon (codename **wheel**) |

The dependency direction is `potter` → `kiln` → `vmkit`. Nothing depends upward.

## Status (2026-10-05)

| Work | State |
|---|---|
| kiln M1a, M1b-1, M1b-2 (convert, store, registry) | Done. M1b-2 is merged locally, not yet pushed |
| vmkit M2a, M2b (VMMs, sandbox, net) | Merged. Raw-disk and CH disk-growth fixes are pending, found in M3a |
| kiln M3a (guest: proto, init, ext4, testguest) | 8 of 9 tasks done on `m3a-guest` |
| kiln M3b (`kiln run`) | Planned |
| kiln M3c (`kiln build`) | Shrinks to backend dispatch (potter, then BuildKit, then Podman/Buildah). The native RUN-free backend from spec rev 2.7 is cancelled; potter owns Dockerfiles |
| potter | Spec rev 1.3 approved (branch `docs/design-spec`). Next: the P1a implementation plan |
| kiln-shim (slip) | Draft spec; it belongs in kiln. D1–D4 are confirmed |
| KVM capacity | Decided (D1): nested virtualization on C8i first, benchmarked in a canary region. Metal only if needed; aarch64 needs metal |

## Build order

1. **kiln M3a, then M3b.** Finish the guest, then `kiln run`.
2. **potter P1–P2, in parallel with step 1.** P1 is plan and detectors; P2 is the first images via the namespace executor, with no KVM needed. P2 needs two small upstream PRs:
   - kiln-init `services` (potter spec §4);
   - vmkit generalised `sandbox` and tap-less `net` (potter spec §4).
3. **kiln-shim (slip).** Runs kiln images as a k8s RuntimeClass, so existing Kubernetes operators, namespaces, ingress and node-level log collection work unchanged. This is the deploy path.
   - First: a KVM node-group spike (S0).
   - Then the kiln PRs: exec, guest stats and OOM reporting, `FileUpdate`, and media-type alignment with containerd 2.3's erofs snapshotter.
4. **potter P3–P7.** Catalog, VM executor, Dockerfile frontend, remote cache and graph, daemon.
5. **Agent sandbox (sagger), then snapshot distribution (sherd) with lazy restore (raku).** These need specs for snapshot/fork and for lazy restore.

## Consolidation decisions (2026-10-05)

| Piece | Decision |
|---|---|
| Separate projects for the agent sandbox (#2) and the snapshot registry (#1) | Dropped. Their cores live in kiln and vmkit, under the codenames above |
| Standalone host agent | Not needed. The containerd shim makes kubelet and containerd the host agent |
| Separate ingress layer | vmkit `net` gains inbound port publishing; k8s Services and Ingress do the rest |
| Separate logs/metrics shipper | Under the shim, VM stdio goes to the CRI log path, and metrics come from containerd task stats |
| Persistent volumes | Deferred, later via CSI under the shim |
| potter's own supervisor and namespace sandbox | Replaced by kiln-init `services` and vmkit `sandbox` |

## Decisions that apply to all projects

- **Rust** everywhere.
- **Firecracker and Cloud Hypervisor** from day one, behind vmkit's `Vmm` trait. Backend differences are exposed as `Capabilities`, never by matching on backend type.
- **aarch64 and x86_64** from day one. Development is on Apple Silicon via Lima with nested virtualization; x86_64 is covered by CI.
- **Apache-2.0.**
- **Untrusted by default.** Images, repos, build steps and guests are untrusted. Guest-asserted signals (exit codes, digests, sizes) never drive security decisions.

## Carried-forward decisions for sagger, sherd and raku

- **Snapshots are not portable across VMMs.** Every snapshot bundle records the VMM name and version and the architecture. Restore refuses mismatches.
- **Lazy restore happens below the VMM**, not through Firecracker's UFFD handler socket. A lazily fetched, chunk-backed block/file layer (FUSE or ublk) serves the guest memory file, and each VMM maps it normally. That gives one mechanism for both backends. It needs a design-time comparison of FUSE vs ublk vs UFFD, and a check of Cloud Hypervisor's on-demand restore mode.
- **Snapshot distribution builds on OCI.** Memory and disk snapshots become additional kiln artifact types. sherd's value is chunk-level dedup and lazy fetch, which plain OCI registries can't provide.
- **Deterministic erofs layers** mean identical layers have identical digests across machines, which gives cross-image dedup for free. Squashed bottom layers are the exception: they don't dedupe across images.
- **Guest control channel.** kiln-proto covers config, readiness, shutdown, signals, stdio, TTY, exit status, services and build mode. Extend it; don't invent another protocol. Snapshot/restore must handle vsock connections resetting on restore.
- **Restore is a constructor.** `Vmm::restore(bundle, RestoreSpec) -> Vm`; both VMMs need a fresh process for it. Paths inside the sandbox are fixed (`/vm/disk/<n>`), so snapshots restore from any host-side location.
- **Unprivileged sandbox and default-deny egress.** These are already in vmkit. The sandbox adds per-sandbox egress policies, and later DNS-name policies, on top of them.
- **Device budget.** Firecracker x86_64 has 17 virtio devices in total. Balloon and any extra devices come out of the same budget (`Capabilities.max_virtio_devices`).
