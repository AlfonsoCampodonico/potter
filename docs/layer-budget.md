# The layer budget

A kiln image is a stack of erofs layers that `kiln-init` mounts with overlayfs, and every layer is
its own virtio-blk disk in the microVM. The VMM has a fixed number of virtio devices, so the number
of layers is a resource. The layer budget is the most layers potter lets an image have. When an image
stacks more, potter merges its bottom layers into one composite layer, starting with the base.

## Setting it

| Where | Form |
|---|---|
| potter.toml | `[target.<name>.image]` `max_layers = N` |
| CLI | `potter build --max-layers N` and `potter plan --max-layers N`: applies to every target and overrides potter.toml |
| Plan JSON | `targets[].image.max_layers` |

- **Default:** 10, kiln's own `--max-layers` default. A plan without a budget leaves the key out, so
  every plan written before this feature keeps its hash.
- **Valid range:** 3 to 128. Anything else is a config error (exit 2), checked in potter.toml, on the
  CLI and in plan validation.
  - 3 is the smallest budget that still keeps a composite base, a layer above it and the top layer.
  - 128 is kiln's `MAX_LAYERS`, the most layers `kiln-init` mounts.
- **Where it's recorded:**
  - It's part of the canonical plan, so it's in the target hash that every image carries
    (`dev.potter.target`).
  - It's not in step hashes or step cache keys: the budget never changes what a step produces.
  - kiln caches the composite by its contents (see below), so two budgets that merge the same layers
    share it.
- **`--affected`:** compares plans without `--max-layers`. A flag that changes nothing else
  rebuilds nothing.
- **Reporting:**
  - `potter build` prints the merge on stderr:
    `[app] layers: 11 over the budget of 10: the bottom 5 (the base's 5) merged into one composite`.
  - `--progress json` adds `layers`, `max_layers` and `composite` to `target_built`.

## The device ceiling

The budget counts app layers only. The images kiln boots also carry a kernel, which is booted
directly and isn't a disk, and the `kiln-init` layer, which is the init disk in the list below. At run
time, `kiln run` needs these devices besides the app layers:
- the init disk;
- the scratch disk;
- vsock;
- the NIC, with `--net`.

It refuses an image that doesn't fit before booting (kiln-run's `check_devices`).

| VMM | virtio devices | Most app layers, with a NIC | Without |
|---|---|---|---|
| Firecracker, x86_64 | 17 | **13** | 14 |
| Firecracker, aarch64 | 92 | 88 | 89 |
| Cloud Hypervisor | 31, one taken by its RNG | 26 | 27 |
| `kiln-init` itself | | 128 | 128 |

So the practical ceiling is the target VMM's device budget, not 128:
- Firecracker on x86_64 tops out at about 12 to 14 layers, depending on the network and on any extra
  disks a platform attaches.
- aarch64 Firecracker allows more than 80.

**The warning.** potter warns, but doesn't fail, when the budget is over **13**, the conservative x86_64
Firecracker figure:

    warning: target app: max_layers 20 is over 13, the most layers Firecracker boots on x86_64 with a
    network (17 virtio devices); an image that uses the budget needs Cloud Hypervisor (26 layers) or aarch64

potter can't know which VMM will run the image, so it doesn't refuse. Every extra disk a platform
attaches (a persistent volume, a second NIC) comes out of the same budget.

## The composite policy

potter stacks these layers, bottom first:
1. the OCI base's layers;
2. one layer per layer-producing step (the `source` inputs layer, each `run` step, `copy_from`);
3. with `export`, the run base's layers instead, then the export layer;
4. the files layer.

When the stack has `n` layers, the base has `b` of them, and the budget is `N`:

- If `n <= N`, nothing is merged.
- Otherwise, potter merges the bottom `max(n - N + 1, b)` layers into one composite:
  - **The whole base first**, even when merging fewer layers would fit the budget.
  - **Then the next-lowest step layers, one at a time, bottom-up**, only when the base alone isn't
    enough.
  - The top layers, which a customer or the platform rebuilds most, are never merged while a lower
    layer can be.

| Stack (`n`, base `b`) | Budget | Merged | Layers left |
|---|---|---|---|
| ruby:3.4-slim (5) + source + 4 steps = 10 | 10 | none | 10 |
| the same + 1 step = 11 | 10 | the 5 base layers (kiln alone would merge 2) | 7 |
| 5 + 9 steps = 14 | 10 | the 5 base layers | 10 |
| 5 + 10 steps = 15 | 10 | base + the lowest step | 10 |
| 5 + 6 steps = 11 | 20 | none | 11 |
| 1-layer base + 10 = 11 | 10 | the bottom 2 (as kiln alone) | 10 |

### How it's done

potter doesn't merge anything itself. It uses kiln's squash in `kiln_image::convert_resolved`.

**What kiln does on its own** (verified in `kiln-image/src/convert.rs`, pinned by
`kiln/crates/kiln-image/tests/squash.rs` on kiln's `layer-budget` branch):
- **Which layers:** with `n > max_layers`, kiln merges exactly the bottom `n - max_layers + 1`,
  leaving `max_layers` layers. That's the minimum, so the layers it merges depend on how many layers
  the image has: one more step changes the merge.
- **Order of work:** every layer is first converted on its own against the unsquashed stack below it.
  Implicit parents are resolved there (kiln `docs/format.md`), and each layer is cached in the
  `layers` and `layers-ctx` caches.
  - Only then are the bottom layers merged.
  - So a layer above the merge is the same erofs blob with or without the merge.
- **Caching:** the merge is cached in `cache/squash` under the SHA-256 of the ordered erofs digests
  it merges. Its result is a content-addressed blob in kiln's store.
- **Determinism:** the merge is deterministic: the same inputs give the same digest in any store and
  with any job count.

**How potter steers it.** potter passes kiln a `max_layers` of `n - k + 1`, which makes kiln merge
exactly the `k` layers the policy chose. potter then checks that kiln merged `k` layers, and refuses
the image (exit 4) if a future kiln merges otherwise. The policy is `potter_exec::assemble::layer_plan`.

### What this buys

- **The composite is shared.** When the base is merged on its own, the composite depends on the base
  alone. Every image on the same base, with any number of steps, gets the same composite blob, and
  only the first conversion pays for the merge.
- **Changing a step above the composite** reconverts that step's layer, plus any layer above it whose
  inherited directory attributes changed. The composite is a cache hit, and nothing below it is
  touched.
  - Changing the potter.toml of an explicit plan changes the `source` layer, whose key the steps
    above chain on.
  - Those steps run again but seal the same bytes, so they're the same blobs.
- **Changing a layer inside the composite** (a new base digest, or a step that was merged into it)
  reconverts that layer and rebuilds the whole composite. The cost is proportional to the bytes
  merged. The layers above it are cache hits.

## Measured cost

These are timings for the conversion of an image, without running any steps, from
`cargo test --release -p potter-exec --test layer_budget -- --ignored --nocapture timings`. Each one
includes storing and hashing the OCI layer tars.

The test image is ruby-slim sized:
- a 5-layer base of 193 MiB, with 2000 small files and one 30 MiB file per layer;
- 6 step layers, 17 MiB in all.

Release builds, kiln at potter's pinned rev, on two machines:
- **Mac:** Apple M-series, macOS, APFS.
- **Linux:** the Lima VM on that Mac, aarch64 Linux, 8 vCPUs, ext4 on a virtual disk.

| Scenario | Layers (composite) | Mac | Linux |
|---|---|---|---|
| Cold: every layer converted, budget 20, no composite | 11 | 0.94 s | 1.59 s |
| Same image at budget 10: layers cached, base merged | 7 (5) | 0.44 s | 1.30 s |
| Unchanged rebuild | 7 (5) | 0.09 s | 0.13 s |
| Top step changed: only it converts; composite cached | 7 (5) | 0.19 s | 0.86 s |
| One more step (12 layers): composite still cached | 8 (5) | 0.14 s | 0.43 s |
| A base layer changed: it converts and the composite is rebuilt | 7 (5) | 0.62 s | 2.54 s |
| kiln's minimal squash alone, 11 layers | 10 (2) | 0.24 s | 1.15 s |
| kiln's minimal squash alone, 12 layers: a new merge, nothing reused | 10 (3) | 0.32 s | 1.06 s |
| 15 layers: base + lowest step merged | 10 (6) | 0.57 s | 1.54 s |
| 15 layers, the merged step changed: composite rebuilt | 10 (6) | 0.61 s | 1.48 s |
| 15 layers, the top step changed: composite cached | 10 (6) | 0.18 s | 0.30 s |

The Linux numbers are a single run on a shared VM and vary by run. Compare rows within a column.

What the numbers show:
- Merging a 190 MiB base costs about 0.35 s on the Mac and 1.2 s on the VM. It's paid once per base.
- With kiln's minimal rule, every change in the layer count paid for a new merge of part of the base.
- With the base-first rule, a top-layer change costs about what converting that layer costs.
- The price of merging a step into the composite is that changing that step rebuilds the whole
  composite: 0.61 s against 0.18 s for a step above it on the Mac, and 1.48 s against 0.30 s on
  Linux. So raise the budget before steps start being merged.

## Choosing a budget (for a platform)

1. **Start from the VMM:**
   - Firecracker on x86_64: 13 with a network, minus any extra disks you attach.
   - Cloud Hypervisor: 26.
   - aarch64 Firecracker: up to 88. Don't go past what boots on the smallest VMM you schedule on.
2. **Leave room for the steps that change.** The budget should hold the composite, every
   platform-owned layer and every customer layer without merging a step. Count the image's stack as
   1 (the composite) + steps + export + files.
3. **Prefer fewer, larger base layers to more steps.** A merged base costs nothing after its first
   build. A merged step costs a full re-merge every time it changes.
4. **Use one budget per VMM class across a fleet.** The composite is keyed by what it merges, not by
   the budget, but images on the same base built with different budgets can merge different
   step ranges, and those don't share.
5. **Measure boot.** Each disk adds a little to boot time and to overlayfs lookups. A large budget is
   a capacity, not a target.

## Deliberately not done

- **Catalog composites (spec §7).** The design spec merges the resolved catalog layers into one
  composite always, keyed by `H("composite" ‖ ordered digests)`. There's no catalog yet (catalog
  bases are lowered onto upstream OCI images, `stopgap.rs`), so here the "base" is the OCI base
  image's layers. The stopgap's install steps count as steps.
- **A composite when the image fits.** Nothing is merged under the budget: merging costs a copy, and
  separate base layers are already shared through kiln's layer cache.
- **Nested composites.** Merging steps into an existing base composite (a composite of the composite
  and the steps) would keep the base-only composite cached. But the second merge copies every byte
  again, so it costs at least as much as a flat merge and stores the base twice.
- **A per-VMM default.** potter doesn't know where an image will run, so the default stays 10 and the
  warning is advisory. `kiln run` is what refuses an image that doesn't fit.
- **The build-time VM executor (P4).** Its device budget for build steps (spec §9.3) is separate and
  unchanged.
- **kiln's pinned rev.** It's unchanged: the policy needs nothing new from kiln. kiln's `layer-budget`
  branch only adds tests that pin the squash behaviour potter relies on, and README notes.
