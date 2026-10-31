# potter P3 — Signed Catalog (without PHP) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** detected Node, Bun, Deno, Python, Ruby, Go, Rust and static targets build on composite bases resolved from potter's own signed catalog; PHP keeps the stopgap; `potter catalog ls|resolve|verify|affected` and catalog-aware `potter rebase` work.

**Architecture:** a new pure crate `potter-catalog` owns the index format, ed25519 verification, catalog locations (registry or OCI layout), rollback protection and resolution. `potter-exec` resolves `Base::Catalog` through it, merges the resolved layers into one cached composite layer, pins `Apt` steps to snapshot.debian.org, and records catalog annotations, provenance and SBOM entries. A hidden `potter catalog build` drives the recipes under `catalog/` with the ns executor, checks overlaps, smoke-tests, and writes a signed OCI layout that `potter catalog publish` pushes.

**Tech Stack:** Rust 2024, `ed25519-dalek` 2 (`pkcs8`, `pem`), `sha2`, `serde`/`serde_json` (JCS via the existing `potter-plan` canon), `goblin` (ELF only) for sonames, kiln-registry/kiln-oci/kiln-store at the pinned rev, GitHub Actions (`ubuntu-24.04`, `ubuntu-24.04-arm`).

**Spec:** `docs/superpowers/specs/2026-10-31-potter-p3-catalog-design.md` (this round), narrowing `docs/superpowers/specs/2026-10-04-potter-design.md` §7, §8.1, §9.6, §16, §17.

## Global Constraints

- Catalog contents, one line each, x86_64 and aarch64: `os-min` (debian-13), `build-tools`, `node` 26, `bun` 1.4.4, `deno` 2.9.8, `python` 3.15, `uv`, `poetry`, `pipenv`, `ruby` 4.0 (with bundler), `go` 1.27, `rust` 1.99, `static-server`. No PHP, php-fpm, php-ext-*, composer, nginx, swoole, RoadRunner, FrankenPHP.
- PHP-only refs (`php`, `php-fpm`, `php-ext-*`, `composer`, `nginx`, `swoole`, `roadrunner`, `frankenphp`) send the whole base through `stopgap::lower`; a ref that is neither catalog nor PHP-only is exit 2.
- A runtime constraint no catalog line satisfies is exit 2 at plan time, naming the constraint, its source, and what the catalog has ("the catalog has node 26"). No fallback to the stopgap for non-PHP targets.
- Verification fails closed (P2): signature checked over the index blob's SHA-256 before parsing; layers fetched by digest and re-hashed. Verification failure exit 3, unreachable catalog exit 4, both before any sandbox starts.
- Detection stays offline (P9); only build-time resolution reads the index.
- Anything containing an ns-built catalog layer has `ns` provenance (P12). Every entry this round has `executor = "ns"`.
- Every upstream download in a recipe is pinned by URL and SHA-256; `os-min` by a `debian:trixie-slim` digest and a `snapshot.debian.org` date. Pins move only by hand. No schedule in CI.
- Only `os-min` and `build-tools` contain Debian packages; runtime and tool layers install under `/usr/local` (or `/opt`) and hold no dpkg state.
- Default catalog location: `ghcr.io/alfonsocampodonico/potter-catalog`, tag `index`. Overrides: `POTTER_CATALOG=<registry ref | oci-layout:DIR>`, `POTTER_CATALOG_KEY=<ed25519 public key PEM path>`; each prints a `note:` line on stderr whenever it is used.
- Exit codes: 2 usage/plan/unsatisfiable, 3 verification or hostile, 4 infrastructure.
- No attribution trailers in commits. kiln rev stays `b96a9a6bd8f6b34e840cffbf2703288295342e31`, vmkit `b1baf3a0712209dbe7c7659186ad02b702524d3e`; no kiln or vmkit edits.

**Plan-level decisions (settle the design's open items):**
- Catalog layers are distributed as ordinary OCI layer blobs (deterministic tar, zstd), the same form every potter chain layer has; kiln converts them to erofs when an image is converted. "erofs layer" in the design means the converted form.
- The index artifact is an OCI image manifest with `artifactType: application/vnd.potter.catalog.index.v1+json`, one layer (the index JSON, media type `application/vnd.potter.catalog.index.v1+json`), and annotations `dev.potter.catalog.signature` (base64 ed25519 over the 32-byte SHA-256 of the index blob) and `dev.potter.catalog.key` (key id). One tag fetch, then the blob by digest.
- Each entry carries `env` (applied bottom-first; `PATH` entries are prepended, others override), because composites replace image configs.

## Review Focus

1. **The catalog is unreachable and nothing pins an index:** exit 4 naming the catalog location, before any sandbox. With `--catalog-index D` and that index plus its layers already in `$POTTER_HOME`/the store, the build runs offline. Test in Task 3.
2. **An aarch64 host whose index lacks an arch entry for a ref:** exit 2 naming name, line and arch, never another arch's layer. Test in Task 4.
3. **Two targets in one build (or two concurrent builds) needing the same composite:** one composite written, both builds use it, no torn file. Test in Task 5.
4. **A Laravel target with an asset build:** still lowered through the stopgap, now with node 26, builds and plans otherwise unchanged; `engines.node: "22"` is refused with the catalog message. Tests in Task 7.
5. **A correctly signed index with an unknown field, an oversized blob (> 8 MiB) or > 10 000 entries:** exit 3, nothing cached, no serial recorded. Test in Task 1.

---

### Task 1: `potter-catalog` crate — the index format

**Files:** create `crates/potter-catalog/{Cargo.toml,src/lib.rs,src/index.rs,src/error.rs}`.

**Produces:**
- `pub const SCHEMA: &str = "potter.catalog/v1"`, `pub const MAX_INDEX_BYTES: u64 = 8 << 20`, `pub const MAX_ENTRIES: usize = 10_000`.
- `pub struct Index { pub schema: String, pub serial: u64, pub created: String /*RFC 3339*/, pub entries: Vec<Entry> }`
- `pub struct Entry { pub name: String, pub line: String, pub version: String, pub arch: Arch, pub kind: Kind, pub digest: String /*blob*/, pub diff_id: String, pub size: u64, pub media_type: String, pub abi: BTreeMap<String,String>, pub requires: Vec<CatalogRef>, pub conflicts: Vec<String>, pub env: BTreeMap<String,String>, pub provides_sonames: Vec<String>, pub executor: String, pub source: String, pub license: String, pub supersedes: Option<String>, pub fixes: Vec<String> }` — `#[serde(deny_unknown_fields)]` on both.
- `pub enum Arch { X86_64, Aarch64 }` (serde `x86_64`/`aarch64`), `pub enum Kind { Base, Runtime, Extension, Tool }` (lowercase).
- `Index::parse(bytes: &[u8]) -> Result<Index, CatalogError>` (size cap, entry cap, schema check, digest shape checks, no duplicate `digest`); `Index::canonical(&self) -> Vec<u8>` (JCS via `potter_plan::canon`); `Index::current(&self) -> impl Iterator<Item=&Entry>` (entries no other entry supersedes); `Index::by_digest(&self, &str) -> Option<&Entry>`.
- `pub enum CatalogError { Hostile(String), Infra(String), Unsatisfied(String), Plan(String) }` with `exit_code()` 3/4/2/2.

- [ ] **Step 1:** Tests in `index.rs`: `parse_round_trips_canonical`, `unknown_field_is_hostile`, `oversized_blob_is_hostile` (8 MiB + 1), `too_many_entries_is_hostile` (10 001), `duplicate_digest_is_hostile`, `current_skips_superseded`.
- [ ] **Step 2:** Run `cargo test -p potter-catalog` — FAIL (crate missing).
- [ ] **Step 3:** Implement the types and functions above; add the crate to the workspace.
- [ ] **Step 4:** Run `cargo test -p potter-catalog` — PASS; `cargo clippy -p potter-catalog -- -D warnings` clean.
- [ ] **Step 5:** Commit `feat(catalog): the potter.catalog/v1 index format`.

### Task 2: Signatures and trusted keys

**Files:** `crates/potter-catalog/src/sign.rs`, `crates/potter-catalog/keys/` (empty `.keep` until Task 14), `crates/potter/src/main.rs` (`KeyCmd`).

**Produces:**
- `pub struct TrustedKey { pub id: String /*first 16 hex of SHA-256 of the raw public key*/, pub key: ed25519_dalek::VerifyingKey }`
- `pub fn compiled_keys() -> Vec<TrustedKey>` — every `keys/*.pub` (PEM) via `include_str!` through a `build.rs` that lists the directory; an empty directory yields an empty list.
- `pub fn trusted_keys() -> Result<Vec<TrustedKey>, CatalogError>` — compiled keys plus `POTTER_CATALOG_KEY` (prints `note: trusting the extra catalog key <id> from POTTER_CATALOG_KEY`).
- `pub fn sign(blob_sha256: &[u8;32], key: &SigningKey) -> (String /*key id*/, String /*base64 sig*/)`
- `pub fn verify(blob_sha256: &[u8;32], key_id: &str, sig_b64: &str, keys: &[TrustedKey]) -> Result<(), CatalogError>` — unknown key id or bad signature is `Hostile`.
- CLI: `potter key generate --ed25519 <dir>` writes `<dir>/catalog.key` (PKCS#8 PEM, 0600) and `<dir>/catalog.pub`; the existing P-256 path is unchanged without the flag.

- [ ] **Step 1:** Tests: `sign_then_verify`, `wrong_key_is_hostile`, `unknown_key_id_is_hostile`, `flipped_bit_is_hostile`, `env_key_is_trusted_and_noted` (temp PEM), and in `crates/potter/tests/cli.rs` `key_generate_ed25519_writes_0600_key`.
- [ ] **Step 2:** Run — FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test -p potter-catalog -p potter` — PASS.
- [ ] **Step 5:** Commit `feat(catalog): ed25519 index signatures and trusted keys`.

### Task 3: Catalog locations, fetching and rollback protection

**Files:** `crates/potter-catalog/src/location.rs`, `crates/potter-catalog/src/fetch.rs`, `crates/potter-catalog/Cargo.toml` (kiln-registry, kiln-oci, kiln-store at the pinned rev).

**Interfaces:**
- Consumes: `Index::parse`, `verify`, `trusted_keys`.
- Produces:
  - `pub enum Location { Registry(kiln_oci::Reference), Layout(PathBuf) }`; `Location::from_env() -> Result<Location, CatalogError>` (default `ghcr.io/alfonsocampodonico/potter-catalog:index`; `POTTER_CATALOG` override noted); `Location::parse(&str)` (`oci-layout:DIR` or a reference).
  - `pub struct Verified { pub index: Index, pub digest: String /*index blob digest*/, pub location: Location }`
  - `pub fn load(loc: &Location, pin: Option<&str>, home: &Path, store: &Store, auth: DockerConfig) -> Result<Verified, CatalogError>` — with `pin`: the index blob by that digest (store first, then the location); without: the `index` tag. Verify, then parse. Rollback: `$POTTER_HOME/catalog/<sha256(location string) hex[..16]>/serial`; refuse a lower serial than recorded unless `pin` is set; record the new serial atomically (write + rename) after success. Verified index blobs are kept in the store.
  - `pub fn fetch_layer(v: &Verified, e: &Entry, store: &Store, auth: DockerConfig) -> Result<Descriptor, CatalogError>` — by `e.digest` with `e.size`; present-in-store skips the fetch; the store's digest check is the re-hash.
  - `pub fn write_layout(dir: &Path, index_blob: &[u8], key_id: &str, sig: &str, layers: &[(Descriptor, PathBuf)])` and `pub fn push(layout: &Path, to: &Reference, auth: DockerConfig)` — used by Task 11.
- [ ] **Step 1:** Tests (OCI layout locations in temp dirs, signed with a test key): `tampered_index_blob_is_hostile`, `signature_from_untrusted_key_is_hostile`, `layer_digest_mismatch_is_hostile`, `lower_serial_is_refused`, `lower_serial_with_pin_is_accepted`, `unreachable_registry_is_infra` (a closed localhost port), `pinned_index_in_store_works_offline` (Review Focus 1), and a registry round trip `push_then_load_from_testregistry` using kiln-registry's `testregistry`.
- [ ] **Step 2:** Run — FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test -p potter-catalog` — PASS.
- [ ] **Step 5:** Commit `feat(catalog): verified loading from registries and OCI layouts, with rollback protection`.

### Task 4: Resolution

**Files:** `crates/potter-catalog/src/resolve.rs`.

**Produces:**
- `pub const PHP_ONLY: &[&str]` (exact names above, plus the prefix `php-ext-`), `pub fn is_php_only(name: &str) -> bool`.
- `pub struct Resolution { pub entries: Vec<Entry> /*bottom first*/, pub constraints: Vec<String> /*"python@3.15"*/, pub index_digest: String }`; `Resolution::composite_key(&self) -> String` = hex SHA-256 of `"composite"` ‖ each `entries[i].digest` joined by `\n`.
- `pub fn resolve(v: &Verified, refs: &[CatalogRef], arch: Arch) -> Result<Resolution, CatalogError>`:
  - each ref: current entries with that name and arch whose `line` equals the ref's version, or any line when the ref has none; highest by `line` then `version` (dotted-numeric compare);
  - requires closure added depth-first; conflicts among the set → `Unsatisfied`;
  - ABI: all agree on `os` and `libc`; every entry carrying `python-abi` (or `ruby-api`) agrees with the runtime entry's;
  - order: `kind` Base first, then requires-before-dependents, ties by name (deterministic);
  - no candidate → `Unsatisfied("the catalog has no <name>@<line> for <arch>; it has <name> <lines…>")`.
- [ ] **Step 1:** Tests: `picks_current_not_superseded`, `requires_closure_is_ordered`, `conflict_is_unsatisfied`, `abi_mismatch_is_unsatisfied`, `missing_arch_names_name_line_arch` (Review Focus 2), `unknown_ref_names_it`, `resolution_is_deterministic` (proptest over shuffled entry order: same entries and composite key).
- [ ] **Step 2–4:** FAIL, implement, PASS.
- [ ] **Step 5:** Commit `feat(catalog): resolution with requires, conflicts and ABI checks`.

### Task 5: Composite bases

**Files:** `crates/potter-exec/src/composite.rs`, `crates/potter-exec/src/base.rs`, `crates/potter-exec/Cargo.toml` (`potter-catalog`).

**Interfaces:**
- Consumes: `Resolution`, `fetch_layer`, `merge::export_layer` (export `/`, no excludes).
- Produces:
  - `pub fn composite(store: &Store, home: &PotterHome, r: &Resolution, platform: &Platform) -> Result<BaseImage, BuildError>` — merges the layers into one deterministic tar (zstd), writes a single-layer OCI image whose config `Env` is the entries' `env` combined bottom-first (PATH prepended), `WorkingDir` `/`, `User` `0:0`; caches the manifest digest at `$POTTER_HOME/composite/<composite_key>` (write to temp + rename; a concurrent writer's identical result wins harmlessly). Byte-reproducible: mtimes from `SOURCE_DATE_EPOCH = 0`, uid/gid preserved, entries sorted.
  - `BaseImage::local(store: &Store, manifest: &Digest, repository: &str) -> Result<BaseImage, BuildError>` — reads manifest and config from the store. If `kiln_oci::ResolvedImage` has no public constructor, `BaseImage` holds manifest, config and manifest digest itself and every caller is adjusted.
- [ ] **Step 1:** Tests (unit, synthetic layers in a temp store): `composite_is_byte_identical_across_runs`, `upper_layer_wins_and_whiteouts_apply`, `env_path_is_prepended_bottom_first`, `cached_composite_is_reused`, `concurrent_composites_agree` (two threads, Review Focus 3).
- [ ] **Step 2–4:** FAIL, implement, PASS (`cargo test -p potter-exec composite`).
- [ ] **Step 5:** Commit `feat(exec): composite bases from resolved catalog layers`.

### Task 6: Building on the catalog

**Files:** `crates/potter-exec/src/chain.rs`, `crates/potter-exec/src/stopgap.rs`, `crates/potter-exec/src/apt.rs` (new), `crates/potter/src/main.rs`.

**Interfaces:**
- Consumes: `Location::from_env`, `load`, `resolve`, `composite`.
- Produces:
  - `BuildCtx` gains `pub catalog: Option<&'a CatalogCtx>`; `pub struct CatalogCtx { pub verified: Verified }` loaded once per `potter build`/`rebase` invocation, only when some target has a non-PHP catalog base.
  - `ChainState` gains `pub catalog: Option<Resolution>`.
  - In `run_steps`: `Base::Catalog` with any `is_php_only` ref → `stopgap::lower` (unchanged); otherwise `resolve` + `composite` and no prelude steps. Missing `ctx.catalog` with a non-PHP catalog base is a bug (`unreachable!` is not acceptable: return `BuildError::Infra("the catalog was not loaded")`).
  - `apt::lower(step: &Step) -> Step` for catalog chains: a `Run` with `network: true` that writes `/etc/apt/sources.list.d/debian.sources` pointing at `http://snapshot.debian.org/archive/debian/<snapshot>/` and `debian-security/<snapshot>/` for trixie, runs `apt-get -o Acquire::Check-Valid-Until=false update`, `apt-get install -y --no-install-recommends <packages>`, then removes `/var/lib/apt/lists/*`. Stopgap chains keep `stopgap::lower_step`.
  - CLI: `--catalog-index <digest>` on `build` and `rebase`.
- [ ] **Step 1:** Tests in `chain.rs` (unit, fake registry/layout): `php_refs_take_the_stopgap`, `catalog_refs_resolve_to_a_composite`, `unknown_ref_is_a_plan_error`, `apt_step_pins_the_snapshot` (the generated command contains the snapshot URL and `Check-Valid-Until=false`). In `crates/potter-exec/tests/build.rs` (Linux, sandbox job): `detected_go_builds_on_a_test_catalog` using a test catalog layout built from two synthetic layers (an os-min stand-in from `debian:trixie-slim` and a fake `go` layer), gated like the existing sandbox tests.
- [ ] **Step 2–4:** FAIL, implement, PASS.
- [ ] **Step 5:** Commit `feat(exec): non-PHP catalog bases build on verified composites`.

### Task 7: Detectors on the catalog lines

**Files:** `crates/potter-detect/src/catalog.rs`, `crates/potter-detect/src/providers/{node,bun,deno,python,ruby,go,rust,static_site,js}.rs`, snapshots under `crates/potter-config/tests/snapshots/` and `crates/potter-detect/tests/`.

**Produces:**
- Tables: `NODE` lines `["26" (lts)]` default `26`; `BUN` `["1"]`; `DENO` `["2"]`; `PYTHON` `["3.15"]` default `3.15`; `RUBY` `["4.0"]` default `4.0`; `GO` `["1.27"]`; `RUST` `["1.99"]`. `PHP` and `COMPOSER_VERSION` unchanged.
- `pick` failure message: `<field>: <constraint> from <source> is not in the catalog; the catalog has <name> <lines joined by ", ">` (exit 2 through the existing error path).
- Every non-PHP provider's build-chain base gains `cref("build-tools", None)` right after `os-min`; run bases never do. PHP providers unchanged except that their node/bun now come from the single lines.
- [ ] **Step 1:** Tests: `node_22_engine_is_refused_naming_catalog`, `python_312_version_file_is_refused`, `ruby_34_gemfile_is_refused`, `go_build_chain_has_build_tools`, `run_base_has_no_build_tools`, `laravel_asset_build_uses_node_26` (Review Focus 4), `laravel_engines_node_22_is_refused`.
- [ ] **Step 2:** Run `cargo test -p potter-detect -p potter-config` — FAIL.
- [ ] **Step 3:** Implement; review each changed snapshot by hand, then `cargo insta accept`.
- [ ] **Step 4:** Run the whole workspace test suite — PASS (PHP build tests in `potter-exec` still lower through the stopgap with node 26).
- [ ] **Step 5:** Commit `feat(detect): one catalog line per runtime, build-tools in build chains`.

### Task 8: What catalog images record

**Files:** `crates/potter-exec/src/assemble.rs`, `crates/potter-exec/src/attest.rs`, `crates/potter-exec/src/sonames.rs` (new), `crates/potter-supply/src/{sbom.rs,provenance.rs}`.

**Interfaces:**
- Consumes: `ChainState.catalog`.
- Produces:
  - `sonames::needed(layer: &mut dyn Read, caps) -> Result<BTreeSet<String>, BuildError>` and `sonames::provided(…)` — ELF `DT_NEEDED` / `DT_SONAME` over regular files whose first 4 bytes are `\x7fELF`, with goblin; per-file cap 256 MiB, total 4 GiB read, 100 000 files; over a cap → warning and the annotation is omitted (the rebase soname gate then refuses, Task 9).
  - Run-base annotations on catalog images: `dev.potter.catalog.layers` (JSON array of entry digests, bottom first), `dev.potter.catalog.constraints` (JSON array), `dev.potter.catalog.index` (index digest), `dev.potter.chain` (JSON array: runtime-step layer digests, export layer, init layer), `dev.potter.sonames.needed` (JSON array, from the export layer, or from every layer above the base when there is no export).
  - Provenance: `catalogIndex` and, per catalog layer, `{digest, name, version, executor}` in the resolved dependencies; the image's executor class is `ns` when any layer (step or catalog) is `ns`.
  - SBOM: one package per catalog entry: name, `versionInfo` = `version`, `downloadLocation` = `source`, `licenseConcluded` = `license`.
- [ ] **Step 1:** Tests: `annotations_name_catalog_layers_index_and_constraints`, `sonames_needed_reads_dt_needed` (a tiny checked-in ELF fixture per arch, under 16 KiB), `oversized_elf_omits_annotation_with_warning`, `provenance_is_ns_with_ns_catalog_layer`, `sbom_lists_catalog_packages`.
- [ ] **Step 2–4:** FAIL, implement, PASS.
- [ ] **Step 5:** Commit `feat(exec): catalog annotations, sonames, provenance and SBOM entries`.

### Task 9: Rebase for catalog images

**Files:** `crates/potter-exec/src/rebase.rs`, `crates/potter/src/main.rs`, `crates/potter-exec/tests/rebase.rs`, `crates/potter/tests/rebase.rs`.

**Produces:**
- `Request` gains `catalog_index: Option<&str>`, `dry_run: bool`. `Recorded` gains `catalog: Option<RecordedCatalog { layers, constraints, index, sonames_needed: Option<BTreeSet<String>> }>`.
- Paths:
  - recorded catalog annotations → re-resolve `run_chain`'s `Base::Catalog` against the current (or pinned) index; refuse (`BuildError::RebaseRefused`, exit 2) when a replaced entry's line differs, when any `abi` value differs (naming key, old, new), or when `sonames_needed` is absent or names a soname no new layer's `provides_sonames` and no layer above the base provides; otherwise build the new composite, re-run `runtime_steps`, and continue through the existing kiln rebase path (kiln's gates stay in force; images without export take kiln's re-derive path as today);
  - no catalog annotations and an OCI or stopgap-lowered run base → today's behaviour, unchanged.
- `Rebased` gains `replaced: Vec<(String /*old digest*/, String /*new*/)>`; provenance for a pushed rebased image is out of scope (open follow-up from PR #2).
- `--dry-run` prints the replacements and every gate's verdict and writes nothing.
- [ ] **Step 1:** Tests (Linux, test catalog layouts with a superseding entry): `rebase_keeps_app_layer_digests_and_records_replaced`, `abi_change_is_refused_naming_key`, `missing_soname_is_refused`, `absent_sonames_annotation_is_refused`, `pre_p3_stopgap_image_still_rebases`, `dry_run_writes_nothing`, `no_export_image_rebases_through_kiln_rederive`.
- [ ] **Step 2–4:** FAIL, implement, PASS.
- [ ] **Step 5:** Commit `feat(rebase): catalog images rebase onto superseding layers behind ABI and soname gates`.

### Task 10: `potter catalog` commands

**Files:** `crates/potter/src/main.rs`, `crates/potter/src/render.rs`, `crates/potter/tests/catalog.rs`.

**Produces:** `Catalog { #[command(subcommand)] command: CatalogCmd }` with
- `Ls { #[arg(long)] arch: Option<Arch>, #[arg(long)] all: bool, #[arg(long)] json: bool }` — columns `NAME LINE VERSION ARCH KIND EXECUTOR DIGEST(12)`; `--all` adds superseded entries marked `superseded by <digest12>`.
- `Resolve { constraints: Vec<String> /* name[@line] */, arch, json }` — ordered entries and the composite key, or the `Unsatisfied` message (exit 2).
- `Verify { #[arg(long)] catalog_index: Option<String> }` — verifies the index and every current entry's layer; prints `ok <n> layers` (exit 0) or the first failure (exit 3).
- `Affected { what: String /* advisory id or sha256:… */, json }` — entries whose `fixes` contains the id, or the entries superseded by / superseding the digest.
- All print strings from the index through the existing sanitiser.
- [ ] **Step 1:** CLI tests against a temp OCI layout via `POTTER_CATALOG`/`POTTER_CATALOG_KEY`: `ls_lists_current_entries`, `ls_all_marks_superseded`, `resolve_prints_order_and_key`, `resolve_unsatisfied_exits_2`, `verify_tampered_exits_3`, `affected_by_advisory_and_by_digest`, `override_env_prints_note`.
- [ ] **Step 2–4:** FAIL, implement, PASS.
- [ ] **Step 5:** Commit `feat(cli): potter catalog ls, resolve, verify and affected`.

### Task 11: The recipe driver

**Files:** `crates/potter-exec/src/catalog_build.rs` (new), `crates/potter/src/main.rs` (hidden `catalog build`, `catalog publish`).

**Produces:**
- Recipe layout: `catalog/<name>/potter.toml` (an explicit plan, one target named `<name>`, whose build chain's base is `Base::Catalog` of its `requires` or, for `os-min`, `Base::Oci`), and `catalog/<name>/layer.toml`:
  ```toml
  name = "python"
  line = "3.15"
  version = "3.15.0"
  kind = "runtime"
  requires = ["os-min"]
  conflicts = []
  abi = { python-abi = "cp315" }          # os and libc are filled in by the driver from os-min
  env = { PATH = "/usr/local/bin" }
  source = "https://www.python.org/ftp/python/3.15.0/Python-3.15.0.tar.xz"
  license = "PSF-2.0"
  smoke = "python3 -c 'import ssl, sqlite3, ctypes'"
  smoke_with = []                          # extra layers for the smoke composite, e.g. ["build-tools"]
  fixes = []
  ```
  `layer.toml` refuses unknown keys.
- `potter catalog build <catalog-dir> --out <layout-dir> --sign-key <pem> [--previous <location>]` (hidden):
  1. topological order by `requires`; each recipe built with the ns executor, tenant `catalog`, against an in-progress layout signed with an ephemeral key the process trusts;
  2. the layer = `export_layer` over the chain's layers **above its base**, flattened, deterministic;
  3. `provides_sonames` from the layer (Task 8 `sonames::provided`); the `os` and `libc` abi values for every entry come from `os-min`: `os` = `debian-<VERSION_ID>` from its `/etc/os-release`, `libc` = `glibc-<x.y>` from its `libc6` entry in `/var/lib/dpkg/status`, both read host-side without executing anything;
  4. overlap check: every pair of non-conflicting layers (with `os-min` always in) shares no regular-file path, except `build-tools` over `os-min` for `var/lib/dpkg/**`, `usr/share/doc/**`, `etc/ld.so.cache`; failure is exit 1 naming both layers and the first 20 paths;
  5. smoke: one `Run` step `sh -c '<smoke>'` on the composite of the layer's closure plus `smoke_with`, no cache, network off; failure exit 1;
  6. compare with `--previous` (a verified location): same digest → keep the old entry; changed → new entry with `supersedes` and `fixes`; serial = previous + 1 (or 1);
  7. write the layout with the real signature (Task 3 `write_layout`).
- `potter catalog publish <layout-dir> <registry-ref>` — verifies the layout with the trusted keys, pushes layers then the index (Task 3 `push`).
- [ ] **Step 1:** Tests (Linux sandbox job, synthetic two-recipe catalog under `crates/potter-exec/tests/fixtures/catalog/`): `driver_builds_in_requires_order`, `layer_holds_only_files_above_base`, `overlap_is_refused_naming_layers`, `failed_smoke_fails_the_build`, `unchanged_layer_keeps_entry_changed_supersedes`, `serial_increments`, `publish_refuses_unsigned_layout`.
- [ ] **Step 2–4:** FAIL, implement, PASS.
- [ ] **Step 5:** Commit `feat(catalog): the recipe driver and publish`.

### Task 12: The recipes

**Files:** `catalog/{os-min,build-tools,node,bun,deno,python,uv,poetry,pipenv,ruby,go,rust,static-server}/{potter.toml,layer.toml}`, `catalog/README.md`.

**Produces:** one recipe per Global-Constraints layer. Requirements per recipe:
- `os-min`: `debian:trixie-slim@<digest>` (record the multi-arch index digest), `apt` from snapshot `<date>` installing `ca-certificates tzdata libssl3t64 zlib1g libffi8 libyaml-0-2 libgmp10 libreadline8t64 libsqlite3-0 libbz2-1.0 liblzma5 libncursesw6`, then apt lists and caches removed.
- `build-tools`: requires `os-min`; `gcc g++ make libc6-dev pkg-config git python3-minimal libssl-dev zlib1g-dev libffi-dev libyaml-dev libreadline-dev`.
- `node` 26, `bun` 1.4.4, `deno` 2.9.8, `go` 1.27.x, `rust` 1.99: official upstream Linux archives for both arches, URL and SHA-256 per arch, selected with `case "$(uname -m)"`; installed under `/usr/local` (rust: `/usr/local/rustup` and `/usr/local/cargo`, `RUSTUP_HOME`/`CARGO_HOME` in `env`, minimal profile, `rustup` itself pinned by SHA-256).
- `python` 3.15.x and `ruby` 4.0.x: compiled from the official source tarballs on `build-tools` with prefix `/usr/local`, shared libraries from `os-min`; tests and `idle`/docs removed.
- `uv`, `poetry`, `pipenv`: requires `python`; uv from its official release archive (pinned); poetry and pipenv installed into `/usr/local/lib/<tool>` venvs from pinned wheel versions with `--require-hashes`, a wrapper in `/usr/local/bin`.
- `static-server`: the same busybox `httpd` shim the stopgap installs today, from a pinned busybox build.
- Every `smoke` from the design §6.2: `node -e 'process.exit(0)'`, `bun -e '0'`, `deno eval '0'`, python as above, `ruby -e 'require "openssl"'`, go: `go version` and a cgo hello world (`smoke_with = ["build-tools"]`), rust: compile and link hello world (`smoke_with = ["build-tools"]`), `uv --version`, `poetry --version`, `pipenv --version`, static-server serving a file to `wget` on 127.0.0.1.
- [ ] **Step 1:** Run `potter catalog build catalog --out /tmp/cat --sign-key <test key>` in the Lima VM (both arches via CI in Task 13) — FAIL until every recipe exists.
- [ ] **Step 2:** Write the recipes.
- [ ] **Step 3:** Run it again — every layer built, overlap check and smoke tests pass; `potter catalog ls` against `oci-layout:/tmp/cat` lists 13 entries for the host arch.
- [ ] **Step 4:** E2E (gated `POTTER_TEST_CATALOG=1`, Linux): for each fixture project (node, bun, deno, python-uv, python-poetry, ruby-rails, go, rust, static) under `fixtures/`, `potter build --executor ns` against the built layout, then run the image's `web` process once and see it listen. Add as `crates/potter/tests/catalog_e2e.rs`.
- [ ] **Step 5:** Commit `feat(catalog): recipes for every non-PHP runtime`.

### Task 13: CI, docs and the agreement test

**Files:** `.github/workflows/catalog.yml`, `.github/workflows/ci.yml` (no change unless the new crate needs it), `docs/catalog.md`, `docs/rebase.md`, `docs/plan-format.md`, `docs/README.md`, `README.md`, `docs/superpowers/specs/2026-10-04-potter-design.md` (rev 1.7 entry), `crates/potter/tests/catalog_agreement.rs`.

**Produces:**
- `catalog.yml`: matrix `ubuntu-24.04` / `ubuntu-24.04-arm`, the sandbox setup steps copied from `ci.yml`; on pull requests touching `catalog/**` or `crates/potter-catalog/**`: build with an ephemeral key, overlap check, smoke, `POTTER_TEST_CATALOG=1` e2e; on push to `main` touching `catalog/**` and on `workflow_dispatch`: build both arches with `--previous ghcr.io/alfonsocampodonico/potter-catalog:index`, merge the two arches' layouts into one index (a `potter catalog build --merge <layout>...` flag, or one job that builds aarch64 layers and hands them to the x86_64 job as an artifact; builder's call), sign with `secrets.POTTER_CATALOG_KEY`, `potter catalog publish` with `GITHUB_TOKEN` (`packages: write`). No `schedule:` trigger.
- `catalog_agreement.rs`: every non-PHP line in `potter-detect`'s tables has a current entry in `catalog/*/layer.toml` (same name and line) and the reverse.
- `docs/catalog.md`: the index format, verification order, locations and overrides, key rotation (add a key in release N, sign with it, drop the old key in N+1), bumping a pin, `fixes`, and the user setup steps.
- Spec rev 1.7: PHP stays on the stopgap, one line per runtime, ns-built catalog, OCI-based rebase kept, hand-bumped pins, the plan-level decisions above.
- [ ] **Step 1:** Write `catalog_agreement.rs`; run — PASS against the Task 12 recipes (FAIL first by temporarily removing one line to see it catch it).
- [ ] **Step 2:** Write the workflow and docs; `actionlint` clean.
- [ ] **Step 3:** Push the branch; the PR run of `catalog.yml` passes on both arches.
- [ ] **Step 4:** Commit `ci(catalog): build, test and publish the catalog; docs`.

### Task 14: First publish (needs the user)

- [ ] **Step 1 (user):** run `potter key generate --ed25519 ~/.potter-catalog-key`, add the contents of `catalog.key` as the repository secret `POTTER_CATALOG_KEY`, and give the agent `catalog.pub`.
- [ ] **Step 2:** Commit `catalog.pub` as `crates/potter-catalog/keys/<key id>.pub`; `compiled_keys()` test `the_release_key_is_compiled_in` passes. Commit `feat(catalog): the first catalog signing key`.
- [ ] **Step 3:** After merge, the `main` run publishes; **(user)** makes the `potter-catalog` package public on ghcr.io.
- [ ] **Step 4:** On a clean machine without overrides: `potter catalog verify` prints `ok 26 layers` (13 per arch), and `potter build` of the node fixture succeeds.
