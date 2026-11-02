# potter P3: the signed catalog (glaze), without PHP

**Date:** 2026-10-31
**Status:** Draft, awaiting approval
**Parent spec:** [2026-10-04-potter-design.md](2026-10-04-potter-design.md) rev 1.6, §7 (catalog), §8.1 (keys), §9.6 (assembly), §15 (P3), §16 (rebase), §17 (supply chain).

This document narrows and settles P3. Where it says nothing, the parent spec applies. Where the two
disagree, this document wins and the parent spec gets a revision entry (rev 1.7) when P3 merges.

Marks used below: **(user)** is a decision the user made in conversation; **(my call)** is a
decision the author made that the user can overturn while reviewing; everything else is either
inherited from the parent spec or left to the builder (said explicitly).

---

## 1. Why

Detected targets today build on upstream Docker Hub images through the stopgap
(`crates/potter-exec/src/stopgap.rs`): floating tags, unpinned `apt-get`, the full
`buildpack-deps` images as build bases, and nothing a security fix can be traced through. P3
replaces that, for every runtime except PHP, with potter's own catalog: small erofs layers built
by potter, listed in a signed index, resolved to digests at build time and merged into one
composite base. That gives:

- reproducible bases (a plan plus an index digest names the bytes exactly);
- threat property P2 (only layers from a verified index are shared across tenants; resolution
  fails closed);
- provenance that names the catalog index and the layers (§17, P12), and images that carry the
  `dev.potter.catalog.*` annotations rebase and `catalog affected` need (§16);
- fewer virtio devices and overlay lowers per build (one composite instead of an image's layers).

## 2. Goals

1. A catalog with these contents **(user)**, one version each, on x86_64 and aarch64:

   | Name | Version | Kind | Notes |
   |---|---|---|---|
   | `os-min` | Debian 13 (trixie) | base | minimal Debian plus ca-certificates and tzdata (§7) |
   | `build-tools` | Debian 13 | tool | compilers and headers for build chains only, see §4.3 **(my call)** |
   | `node` | 26 (exact patch pinned in the recipe) | runtime | |
   | `bun` | 1.4.4 | runtime | |
   | `deno` | 2.9.8 | runtime | |
   | `python` | 3.15 (exact patch pinned) | runtime | |
   | `uv`, `poetry`, `pipenv` | pinned in the recipe | tool | require `python` |
   | `ruby` | 4.0 (exact patch pinned) | runtime | includes bundler |
   | `go` | 1.27 (exact patch pinned) | runtime, build-only | **(my call: the user listed every runtime but go; 1.27 is the newest line in §7)** |
   | `rust` | 1.99 | runtime, build-only | |
   | `static-server` | potter's current shim | tool | as the stopgap's busybox `static-server` |

   Not in the catalog: PHP and everything only PHP uses (`php`, `php-fpm`, `php-ext-*`,
   `composer`, `nginx`, swoole, RoadRunner, FrankenPHP) **(user)**, node 20/22/24, and every
   older python/ruby/go/rust line **(user: latest only)**.

2. **Publishing:** recipes under `catalog/` in the potter repo, built by potter with the **ns
   executor** under the `catalog` tenant on GitHub Actions **(user: ns now, VM later)**, published
   with a signed index to `ghcr.io/alfonsocampodonico/potter-catalog` **(my call)**.
3. **Consuming:** detected Node, Bun, Deno, Python, Ruby, Go, Rust and static targets resolve
   their `Base::Catalog` refs against the verified index and build on composite bases. The
   stopgap is no longer used for them **(user)**.
4. **CLI:** `potter catalog ls`, `potter catalog resolve <constraints>`, `potter catalog verify`,
   `potter catalog affected <advisory-id | layer-digest>` (§11), and `potter rebase` for
   catalog-based images following §16 (§8 below).
5. **Refusal:** a project whose runtime constraint the catalog can't satisfy (for example
   `engines.node: "22"`, `.python-version` 3.12, `ruby "~> 3.4"`) fails at plan time with exit 2,
   naming the constraint, where it came from, and the version the catalog has **(user)**. There
   is no fallback to the stopgap for non-PHP targets.

## 3. Non-goals and anti-goals

**Non-goals (this round):**
- PHP in the catalog. PHP and Laravel targets keep building through the stopgap, unchanged.
- The VM executor. Catalog layers are ns-built until kiln M3e lands and P4b ships; rebuilding them
  with `VmExecutor` is a later change that publishes superseding entries (§6.4).
- Several lines per runtime. The detector's version table and the index carry one line each.
- Keyless or KMS signing of the index.
- Automated patching: no release-feed or security-tracker watchers and no scheduled rebuilds
  **(user)**. Pins (upstream versions and checksums, the `debian:trixie-slim` digest, the
  `snapshot.debian.org` date) are bumped by hand in a PR. The mechanism that carries a fix still
  ships: `supersedes`, advisory ids and `catalog affected`. The §7 24 h / 7 day targets don't
  apply this round.

**Anti-goals (would make this a failure even if it works):**
- Any path where an unverified index or a layer whose digest doesn't match the index reaches a
  sandbox, a cache key or an image. Verification fails closed, always (P2).
- Detection that touches the network. Detection stays static and offline (P9); only build-time
  resolution reads the index.
- Provenance or annotations that claim `vm` for anything built on ns-built catalog layers (P12).
- A PHP regression beyond the node/bun move. PHP targets plan and build as before, except that
  their asset-build runtime comes from the single node 26 / bun 1 line like everyone else's
  **(user)**: a Laravel project now builds assets with node 26, and one pinning
  `engines.node: "22"` is refused like any other target.
- Silent fallback: a non-PHP target never quietly lands on an upstream Docker Hub image.

## 4. The catalog layers

### 4.1 What a layer is

- One deterministic erofs layer (kiln's layer format), stored and pushed as a kiln/OCI blob, with
  the §7 manifest annotations: `dev.potter.catalog.name`, `.version`, `.arch`, `.abi`,
  `.requires`, `.conflicts`, `.kind`, plus `dev.potter.catalog.executor` (`ns` this round).
- `.abi` for this round: `os=debian-13 libc=glibc-<x.y>` on every layer, plus
  `python-abi=<cp315>` on `python` and its tools and `ruby-api=<4.0.0>` on `ruby`. The exact keys
  beyond `os` and `libc` are the builder's call; they must be enough for §8's rebase gate.
- A layer holds only the files its recipe adds above its `requires`, as the diff of the recipe's
  final state over the layers it was built on (the builder decides how: an "export the upper as
  the layer" output mode is the obvious route).

### 4.2 Recipes

- One explicit `potter.toml` plan per layer under `catalog/<name>/`, built with
  `potter build --executor ns --tenant catalog`, as §7 says. A small driver (an xtask or a
  `potter catalog build` subcommand hidden from normal help; builder's call) builds them in
  dependency order, emits the layers, and writes the unsigned index.
- **Upstream sources (constraint):** every download is pinned by URL and SHA-256 in the recipe,
  never a floating URL (this also closes the stopgap's deferred "floating installer URLs" item).
  Where upstream publishes official Linux binaries (node, bun, deno, go, rust), recipes use them.
  ruby is compiled from the official tarball. For python the builder picks between compiling
  CPython from python.org's tarball and python-build-standalone; **(my call: compile from
  python.org)**, since it keeps the runtime's shared libraries coming from os-min (§4.3).
- `os-min` is derived from a pinned `debian:trixie-slim` digest plus ca-certificates and tzdata,
  installed from a pinned `snapshot.debian.org` date, flattened to one layer. Builder decides the
  mechanics.

### 4.3 Debian packages and `build-tools` (my call)

The stopgap gets compilers for free from the full `buildpack-deps` images. The catalog needs that
explicitly, or native gems, wheels with C extensions, cgo and rust's linker all fail:

- **`build-tools`** (kind `tool`, requires `os-min`): gcc, g++, make, libc6-dev, pkg-config, git,
  and the -dev headers the runtimes' native ecosystems commonly need (libssl, zlib, libffi,
  libyaml, libreadline at least). Detectors add `build-tools` to every build chain of the
  runtimes above; run bases never get it.
- **Who touches dpkg:** only `os-min` and `build-tools` contain Debian packages. Runtime and tool
  layers install under `/usr/local` (or `/opt`) and contain no dpkg state. The shared libraries
  runtimes link against at run time (libssl3, zlib1g, libffi8, libyaml, libgmp, libreadline,
  libsqlite3, libbz2, liblzma, libncurses) are in `os-min`. This keeps the CI overlap check
  meaningful (§6.3) and keeps `Apt` `runtime_steps` working on top of a composite (one coherent
  dpkg database).
- **`Apt` steps** (detector `apt-detected` and `[apt]` in `potter.toml`) now run against a pinned
  `snapshot.debian.org` mirror at the step's `snapshot` date, as §5.1 always said, for catalog
  bases. PHP's stopgap path keeps its current unpinned behaviour.

## 5. The index and its signature

- **Shape:** one OCI artifact, `ghcr.io/alfonsocampodonico/potter-catalog:index`, whose payload is
  a canonical (JCS) JSON document: schema id `potter.catalog/v1`, a monotonically increasing
  `serial`, a `created` timestamp, and one entry per layer and arch with name, version, arch,
  kind, digest, size, every annotation above, `supersedes` (the digest it replaces, if any) and
  `fixes` (advisory ids, possibly empty). Superseded entries stay listed and resolvable (§7).
- **Versions (my call):** each entry has `line` (`3.15`, what detectors and constraints name) and
  `version` (the exact upstream release, `3.15.2`, for the SBOM, `catalog ls` and rebase's
  same-minor check). A rebuild supersedes the current entry with the same name, line and arch.
  Resolution picks, within a satisfying line, the entry nothing supersedes.
- **Signature:** ed25519 over the SHA-256 of the canonical index bytes, carried next to the index
  (an OCI referrer or a sibling tag; builder's call). Each signature names its key id.
- **Keys:** the public keys potter trusts are compiled into the binary as a list of
  `(key id, public key)`. Rotation adds a new key in a release, signs with it, and removes the old
  key a release later; documented in `docs/catalog.md`. The private key is a GitHub Actions secret
  that the user creates (setup step, §10).
- **Verification order (P2):** fetch the signature and the index by digest with a size cap, check
  the signature against a compiled-in key before parsing anything else, then parse with unknown
  fields refused. Layers are fetched by the digest the index names and re-hashed on pull. Any
  failure is exit 4 for infrastructure (unreachable registry) and exit 3 for a verification
  failure, before any sandbox starts.
- **Rollback protection (my call):** potter remembers the highest `serial` it has verified per
  catalog location in `$POTTER_HOME/catalog/` and refuses an index with a lower serial unless the
  user pins one explicitly with `--catalog-index <digest>`.
- **Overrides for development and tests:** `POTTER_CATALOG=<registry ref>` points at another
  catalog, and `POTTER_CATALOG_KEY=<path to an ed25519 public key>` adds a trusted key for that
  process. Both are printed on every build that uses them. Tests use a local registry and a test
  key; `potter key generate --ed25519` (or a separate subcommand; builder's call) makes one.

## 6. Building and publishing (CI)

### 6.1 Workflow

- A new workflow builds every recipe on `ubuntu-24.04` (x86_64) and `ubuntu-24.04-arm`
  (aarch64), using the same subuid/AppArmor setup the existing sandbox CI job uses.
- Triggers: pushes to `main` that touch `catalog/`, and a manual dispatch that rebuilds the
  current pins (for a lost publish, say). No schedule **(user)**.
- Advisory ids travel with the pin bump: each recipe carries an optional `fixes = [...]` list that
  the publish copies onto the entry it supersedes with, and that the next bump clears
  **(my call)**.
- Pull requests that touch `catalog/` build and test every layer but don't publish.

### 6.2 What a run does

1. Builds every layer for both arches (cached per recipe through potter's normal cache).
2. Smoke-tests each runtime on a composite of its `requires` closure: `node -e`, `bun -e`,
   `deno eval`, `python3 -c 'import ssl, sqlite3, ctypes'`, `ruby -e 'require "openssl"'`,
   `go version` plus a tiny cgo build on top of `build-tools`, `rustc` compiling and linking a
   hello world on top of `build-tools`, `uv --version` and friends, and `static-server` serving a
   file.
3. Runs the overlap check (§6.3).
4. Compares each layer's digest with the current index entry for the same name, version and arch.
   Unchanged layers keep their entry; changed ones get a new entry with `supersedes`.
5. On `main` (and on a manual dispatch), pushes new layers, then the signed index with `serial + 1`.

### 6.3 Overlap check

For every set of layers that can be co-installed (every layer with every other layer that doesn't
conflict with it, with `os-min` always present), no regular path appears in two layers, except the
paths `build-tools` is allowed to replace in `os-min` (its dpkg state and `/usr/share/doc`;
builder's call on the exact list). A failure stops the publish.

### 6.4 ns-built today, VM-built later

Every entry carries `dev.potter.catalog.executor = ns`. When P4b ships, the same recipes are rebuilt
with `--executor vm`; the new entries supersede the ns ones. Images built on ns layers keep `ns`
provenance (P12) until they're rebuilt or rebased.

## 7. Consuming the catalog

### 7.1 Detection (offline, unchanged in shape)

- `crates/potter-detect/src/catalog.rs` keeps its compiled table, now with one line per runtime:
  node `26` (default and only), bun `1`, deno `2`, python `3.15`, ruby `4.0`, go `1.27`,
  rust `1.99`. PHP keeps its three lines (8.2/8.3/8.4) and composer keeps `2`, since the stopgap
  still serves them. PHP targets pick node and bun from the same single lines **(user)**, and the
  stopgap installs node 26 / bun 1 on top of PHP; PHP snapshot tests change accordingly.
- A constraint no line satisfies is the refusal in goal 5. `pick` already errors there; the
  message must name the constraint and its source, and say what the catalog carries
  ("the catalog has node 26").
- Detectors add `build-tools` to build-chain bases (§4.3). Snapshot tests are updated.
- The table and the index must agree. A test in the catalog CI fails if the index carries a line
  the table doesn't, or the reverse.

### 7.2 Resolution (build time)

- Happens in potter-exec before any sandbox starts, in place of `stopgap::lower` for catalog
  bases: constraints → highest satisfying entry for the target's platform arch → requires and
  conflicts closure → ABI check (every layer agrees on `os` and `libc`; python tools match
  `python-abi`) → an ordered digest list (§7).
- **Which path a base takes:** a `Base::Catalog` whose refs are all catalog names goes through
  the catalog. A base that names any PHP-only ref (the list in goal 1) goes through the stopgap,
  whole. A ref that is neither is a plan error (exit 2).
- The resolved index digest, the ordered layer digests and the resolved constraints are kept for
  annotations and provenance (§7.4).
- `--catalog-index <digest>` on `build` and `rebase` pins an index (it must still verify).

### 7.3 Composite base

- The ordered layers are merged into one erofs layer with kiln's merge, byte-reproducibly, and
  cached under `H("composite" ‖ ordered digests)` (§7). Its digest is the chain's base digest in
  the cache key (§8.1), so a superseded layer changes every key above it.
- Build chains and run bases both use composites. The layer budget (`docs/layer-budget.md`)
  treats a composite as the run base's bottom, as it does an OCI base today.

### 7.4 What images record

- Annotations on every catalog-based image (§16): `dev.potter.catalog.layers` (ordered digests),
  `dev.potter.catalog.constraints`, `dev.potter.catalog.index`, `dev.potter.chain`.
- Provenance (§17) gains the index digest and, per layer, the executor class. Any image containing
  an ns-built catalog layer has `ns` provenance (P12).
- The SBOM gains one package per catalog layer (name, version, upstream source, licence), from the
  index entry, never from scanning.

### 7.5 Cache and tenancy

- Verified catalog layers and composites are shared across tenants (P2); a layer is shared only
  after it has been verified against the index in this process.
- The `catalog` tenant used by CI is an ordinary tenant name; nothing about it is privileged in
  potter itself.

## 8. Rebase for catalog-based images

The shipped `potter rebase` (docs/rebase.md) swaps an OCI run base and stays as it is for OCI-based
images **(my call: the parent spec says OCI-based images are never rebased; the user already
shipped that path, so this round leaves it alone and the spec revision records the change)**.

For catalog-based images, `potter rebase` follows §16:
1. Re-resolve `dev.potter.catalog.constraints` against the current (or `--catalog-index`) index.
   With one line per runtime, the "same minor" rule holds automatically; it's still checked.
2. Refuse ("needs rebuild", exit 2, naming the reason) when any replaced layer's `.abi` differs.
3. Refuse when the export layer's ELF files need a soname (`DT_NEEDED`) the new composite doesn't
   provide. The export's sonames are recorded at build time as an annotation; the new composite's
   provided sonames come from its layers.
4. Build the new composite, re-run `runtime_steps` (cached), stack the unchanged export and the
   current init layer, write the new image with provenance naming the replaced base. App layers
   stay byte-identical.

`potter rebase --dry-run` prints what would change and why, without writing anything.

**Images without `export` (my call):** they take the path `docs/rebase.md` already describes
(kiln re-derives the inheriting layers from their stored source tars under kiln's existing rebase
gates), with the ABI gate added on top and the soname check run over every layer above the base
instead of the export layer. kiln's gates stay in force for catalog images; §16's gates add to
them, never replace them.

**Images built before P3 on a stopgap base (my call):** a non-PHP image whose run base was lowered
through the stopgap carries no `dev.potter.catalog.*` annotations. `potter rebase` keeps rebasing
it the way it does today, by lowering its recorded run chain through the stopgap again, so
`stopgap.rs` keeps its non-PHP branches for that path only. Moving such an image onto the catalog
takes a rebuild. Builds never take this path.

## 9. CLI

| Command | Does |
|---|---|
| `potter catalog ls [--arch A] [--all] [--json]` | Verifies the index and lists current entries (`--all` includes superseded ones) |
| `potter catalog resolve <constraint>... [--arch A] [--json]` | Prints the ordered layer list and the composite key a set like `os-min python@3.15 uv` resolves to, or why it can't |
| `potter catalog verify [--catalog-index D]` | Verifies the index and every layer's digest; exit 0 or 3 |
| `potter catalog affected <advisory-id \| layer-digest> [--json]` | Lists the entries that fix that advisory or that a digest superseded, for matching against `dev.potter.catalog.layers` |
| `potter build ... --catalog-index D` | Pins the index |
| `potter rebase ... --catalog-index D --dry-run` | As §8 |

Exit codes as §11: 2 for usage and unsatisfiable constraints, 3 for verification failures, 4 for
infrastructure.

## 10. Setup only the user can do

- Create the ed25519 signing key, add the private key as a GitHub Actions secret, and commit the
  public key (the plan will provide the command).
- Make the `potter-catalog` package public on ghcr.io, so builds pull anonymously.

## 11. Testing

- The parent spec's threat tests that P3 owns: **P2** (a tampered index, a wrong key, a layer
  digest mismatch, a rolled-back serial: each fails before any sandbox starts), and the **P12**
  rebase cases (unchanged app-layer digests, a recorded replaced base, refusal on an ABI change and
  on a missing soname).
- Resolution unit tests (highest satisfying, requires/conflicts, ABI mismatch, unknown ref, PHP
  refs going to the stopgap) and property tests (resolution is deterministic; composites are
  byte-identical across runs).
- An end-to-end test per detected runtime against a local registry holding a test catalog signed
  with a test key: plan, build, run the image's `web` process once.
- Every existing PHP test keeps passing unchanged.

## 12. Left to the builder

- Crate layout (a new `potter-catalog` crate is the natural home for the index format,
  verification and resolution).
- How a recipe's diff becomes a layer, and how the driver orders and drives recipe builds.
- The exact `.abi` keys beyond `os` and `libc`, the `build-tools` package list, and the overlap
  check's allowed-replacement list.
- Where the signature lives in the registry.
- How tests stand up the local registry.
- Whether `potter catalog build` is a hidden subcommand or an xtask.
