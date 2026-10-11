# potter P6b — Supply Chain and Build Jobs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** every image potter pushes to a registry carries an SBOM, a provenance statement and a signature as OCI referrers; `potter verify` checks them; the build CLI has everything a platform build Job needs.

**Architecture:** a new pure crate `potter-supply` holds the lockfile parsers, the SPDX 2.3 SBOM, the SLSA v1 provenance statement, ECDSA P-256 signing in the sigstore bundle v0.3 format, and verification. `potter-exec` gains `publish` (image push plus referrers through kiln-registry) and records what provenance needs (per-step keys, executor classes, base digests). The CLI wires `--output registry=`, `--sign-key`, `--tenant-key-file`, `--builder-id` and `potter verify`/`potter key generate`.

**Tech Stack:** Rust 2024, `p256` (ecdsa + pkcs8 PEM), `sha2`, `base64`, `serde_json`, `toml`, kiln-registry at the pinned rev (its `testregistry` for tests).

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md` §17 (supply chain), §18 (platform integration), §5.1 (annotations), §13 (documented formats), P12.

## Global Constraints

- Step-reported data never enters an attestation (§17): the SBOM comes from host-side lockfiles and the base/catalog refs only; provenance from the plan, the host's cache keys and executor classes.
- Components with no lockfile evidence are `NOASSERTION`, never guessed.
- Provenance can't overstate (P12): it records the executor class of every layer.
- Parsers are size- and depth-bounded (P9): lockfiles over 16 MiB or 100 000 entries are skipped with a warning, never parsed partially into the SBOM.
- Exit codes: 2 user/plan error, 3 hostile or failed verification, 4 infrastructure.
- No attribution trailers in commits.
- kiln rev stays `01e7a9fe288160cc2eef9dd1a4a205022b8aef77`; no kiln edits.

## Review Focus

1. A lockfile that is huge, deeply nested, or not valid UTF-8: skipped with a warning, the build still succeeds, the SBOM lists no guessed components.
2. A registry that already holds a fallback index on `sha256-<hex>` (an earlier push, or another tool's referrers): potter merges, never drops existing descriptors, and replaces only its own kind.
3. `potter verify` against a signature made by a different key, a bundle whose statement subject names another digest, or a referrer set missing one kind: each fails with exit 3 and says which.
4. `--sign-key` with a `kms://` reference: refused clearly (exit 2) rather than silently building unsigned.
5. Re-pushing the same image: idempotent (same referrer digests when inputs and timestamps are equal; no duplicate descriptors in the index).

---

### Task 1: Image annotations and build records

**Files:** `crates/potter-exec/src/assemble.rs`, `crates/potter-exec/src/chain.rs`, `crates/potter-exec/src/base.rs`.

**Produces:**
- Image manifest annotations: `dev.potter.port` (when the plan has a port), `dev.potter.process.<name>` (JSON argv of every process), `dev.potter.target` (canonical hash of the target, hex), `org.opencontainers.image.created` is NOT written (reproducibility).
- `Built` gains `steps: Vec<StepRecord>` and `bases: Vec<BaseRecord>`:
  - `pub struct StepRecord { pub chain: String, pub step: String, pub key: Option<String> /*hex; None for host-built layers*/, pub layer: Digest, pub class: Option<ExecutorClass> }`
  - `pub struct BaseRecord { pub reference: String, pub digest: Digest }` (resolved manifest digest of each OCI base, build and run).
- `Built.manifest_digest` is the existing `digest`.

Tests (unit, assemble.rs): annotations for a plan with `port` and two processes; `dev.potter.target` equals the target hash hex. Integration (build.rs, Linux): a built image's records list each Run step with `class == Ns` and a 64-hex key.

### Task 2: `potter-supply` lockfile parsers

**Files:** create `crates/potter-supply/{Cargo.toml,src/lib.rs,src/lockfiles.rs}`; workspace `Cargo.toml`.

**Produces:** `pub struct Component { pub ecosystem: Ecosystem, pub name: String, pub version: String, pub source: String /*lockfile path*/ }`, `Component::purl()`; `pub fn parse(path: &str, bytes: &[u8]) -> Result<Vec<Component>, LockError>`; `pub const LOCKFILES: &[&str]`; `MAX_LOCKFILE = 16 MiB`, `MAX_COMPONENTS = 100_000`.

Formats and the rule each follows:
- `composer.lock`: `packages[]` and `packages-dev[]`? — only `packages` (dev deps aren't in the image when composer runs `--no-dev`; ruling if the plan says otherwise). purl `pkg:composer/<vendor>/<name>@<v>`.
- `package-lock.json` v2/v3: `packages` keys `node_modules/...` → name from the last `node_modules/` segment, skip `""` and `dev: true`. v1 `dependencies` recursively, depth-bounded at 64.
- `pnpm-lock.yaml`: line parser over the top-level `packages:` block: keys `/name@ver`, `name@ver`, `'@scope/name@ver'` (strip quotes, peer suffix `(…)`); no YAML dependency.
- `yarn.lock` v1 and berry: entry header lines ending in `:` at column 0 give the name; the indented `version` line gives the version; skip `__metadata` and `workspace:` resolutions.
- `bun.lock` (JSONC): `packages.<key>[0]` = `"name@ver"`.
- `deno.lock`: `npm` keys `name@ver` (strip `_peer` suffixes after `_`), `jsr` keys `@scope/name@ver`.
- `uv.lock`, `poetry.lock`: TOML `[[package]] name, version`; uv skips `source = { editable | virtual }`.
- `requirements.txt`: only `name==version` lines (extras and env markers stripped); anything else is not evidence.
- `Gemfile.lock`: `specs:` blocks under `GEM`, 4-space entries `name (version[-platform])`.
- `go.sum` + `go.mod`: modules in go.mod `require` (both forms), version from go.mod, kept only if go.sum has a matching `module version h1:` line.
- `Cargo.lock`: `[[package]]` with a `source` (registry or git); path packages are the project itself and are skipped.

Tests: one fixture per format with expected components; oversize → `LockError::TooLarge`; non-UTF-8 → `LockError::Parse`; `requirements.txt` with `>=` lines yields nothing; component cap.

### Task 3: SBOM (SPDX 2.3)

**Files:** `crates/potter-supply/src/sbom.rs`.

**Produces:** `pub struct SbomInput<'a> { pub image_name: &'a str, pub image_digest: &'a str, pub created: &'a str /*RFC 3339*/, pub bases: &'a [(String, String)], pub catalog: &'a [(String, Option<String>)] /*name, version*/, pub components: &'a [Component], pub potter_version: &'a str }`; `pub fn spdx(input: &SbomInput) -> serde_json::Value`.

Rules: `spdxVersion: SPDX-2.3`, `dataLicense: CC0-1.0`, `SPDXID: SPDXRef-DOCUMENT`, `documentNamespace: https://potter.dev/spdx/<image digest>`, `creationInfo.creators: ["Tool: potter-<v>"]`. Image package `SPDXRef-Image`; each base `SPDXRef-Base-<n>` with version = digest; each catalog entry `SPDXRef-Catalog-<n>` with `versionInfo` or `NOASSERTION`; each component `SPDXRef-Pkg-<n>` with `externalRefs` purl. `downloadLocation`, `licenseConcluded`, `licenseDeclared`, `copyrightText` = `NOASSERTION`. Relationships: DOCUMENT DESCRIBES Image; Image CONTAINS each other package. Package order sorted, so the document is deterministic.

Tests: golden JSON for a small input; deterministic across input order.

### Task 4: Provenance (in-toto v1, SLSA v1)

**Files:** `crates/potter-supply/src/provenance.rs`.

**Produces:** `pub struct ProvenanceInput<'a> { pub subject_name, subject_digest_hex, builder_id, potter_version, target, plan_hash, source_revision: Option<&str>, source_dirty: bool, tenant, catalog_index: Option<&str>, bases: &[(String,String)], steps: &[StepRecord-like (chain, step, key: Option<String>, layer, class: Option<&str>)], started, finished }`; `pub fn statement(input) -> serde_json::Value`; `pub fn in_toto(subject_name, digest_hex, predicate_type, predicate) -> Value`.

Shape: `_type https://in-toto.io/Statement/v1`, `predicateType https://slsa.dev/provenance/v1`, `buildDefinition{buildType: "https://potter.dev/build/v1", externalParameters{target, plan, source{revision, dirty}}, internalParameters{tenant, catalogIndex}, resolvedDependencies[bases as {uri, digest{sha256}}]}`, `runDetails{builder{id, version{potter}}, metadata{startedOn, finishedOn}, byproducts[one per layer: {name: "<chain>/<step>", digest{sha256: layer}, annotations{cacheKey, executor}}]}`. Host-built layers carry `executor: "host"`.

Tests: P12 — a step record with class ns produces `executor: "ns"`; no field derives from step output.

### Task 5: Signing and verification (sigstore bundle v0.3)

**Files:** `crates/potter-supply/src/sign.rs`.

**Produces:**
- `pub struct SigningKey` (`from_pem(&str)`, `generate()`, `to_pem()`, `public_pem()`, `key_hint()` = base64 SHA-256 of SPKI DER); `pub struct VerifyingKey` (`from_pem`).
- `pub fn sign_statement(key, statement: &Value) -> Value` → bundle with `dsseEnvelope` (payloadType `application/vnd.in-toto+json`, PAE per DSSE v1, signature over PAE, ECDSA P-256 SHA-256, DER).
- `pub fn sign_manifest(key, manifest_bytes: &[u8]) -> Value` → bundle with `messageSignature{messageDigest{algorithm: SHA2_256, digest}, signature}`.
- `pub fn verify_statement(key, bundle: &[u8]) -> Result<Value, VerifyError>` (returns the statement), `pub fn verify_manifest(key, bundle, manifest_digest_hex) -> Result<(), VerifyError>`.
- Bundle: `mediaType application/vnd.dev.sigstore.bundle.v0.3+json`, `verificationMaterial{publicKey{hint}, tlogEntries: []}`.
- `SigningKey::parse_ref(s)`: a path to a PKCS#8 PEM; `kms://…` → `Err(KeyError::KmsUnsupported)` (ruling: KMS needs the AWS SDK and lands with the platform integration; exit 2).

Tests: round trip; wrong key fails; tampered payload fails; manifest digest mismatch fails; kms ref refused.

### Task 6: Publish image and referrers

**Files:** `crates/potter-exec/src/publish.rs`.

**Produces:** `pub struct Attachment { pub kind: Kind /*Sbom|Provenance|Signature*/, pub artifact_type: &'static str, pub bytes: Vec<u8> }`; `pub fn push_image(client, repo, tag, store, manifest_digest) -> Result<(), BuildError>`; `pub fn attach(client, repo, store, subject: &Descriptor, attachments: &[Attachment]) -> Result<Vec<Digest>, BuildError>`; `pub fn referrers(client, repo, subject_digest) -> Result<Vec<Descriptor>, BuildError>`.

Rules: each attachment is an OCI image manifest with `artifactType`, empty config (`application/vnd.oci.empty.v1+json`, `{}`), one layer, `subject`, and annotation `dev.potter.attestation: sbom|provenance|signature`; pushed by digest. Every attach also writes the referrers tag-schema fallback: tag `sha256-<hex>` holding an OCI index; an existing index there is read first, descriptors of the same `dev.potter.attestation` kind are replaced, all others kept, duplicates (same digest) collapsed. Artifact types: SBOM bundle `application/vnd.dev.sigstore.bundle.v0.3+json` with annotation kind sbom, likewise provenance and signature; unsigned builds push the raw statement with `application/vnd.in-toto+json`.

Tests (kiln `TestRegistry`): push then pull back manifest and blobs; attach twice is idempotent; a foreign descriptor in a pre-existing fallback index survives.

### Task 7: CLI

**Files:** `crates/potter/src/main.rs`, `crates/potter/tests/*.rs`.

- `build` flags: `--output registry=<ref>` (needs exactly one selected target, like `--tag`), `--sign-key <pem|kms://…>` (needs `--output`), `--builder-id <uri>` (default `https://potter.dev/builder/local`), `--tenant-key-file <file>` (32 raw bytes or 64 hex chars; overrides the home key — the Job mounts it from a Secret).
- After a target builds with `--output`: collect lockfiles from the target root that the target's steps declare as inputs (RepoView, bounded reads), build SBOM + provenance, sign if keyed, push image + attachments; JSON event `published {target, reference, digest, referrers}`.
- `potter verify <ref> --key <pub.pem>`: resolve the manifest digest, list referrers via the fallback tag, require one of each kind, verify each bundle with the key, check each statement's subject digest and the manifest signature's digest; print a line per kind; exit 0, 3 on any failure, 4 on registry errors.
- `potter key generate <dir>`: writes `potter.key` (0600) and `potter.pub`.

Tests: CLI arg errors (exit 2) on macOS; verify/key round trip against `TestRegistry` with an image pushed by Task 6 helpers.

### Task 8: Build Jobs and docs

**Files:** `docs/build-jobs.md`, `deploy/k8s/build-job.yaml` (both since removed from the repo), README, `docs/plan-format.md` (§13 formats: annotations, referrer kinds, bundle shapes), HANDOFF.

- Job manifest: KVM build node selector/tolerations, `potter build --executor vm|ns --progress json --output registry=$IMAGE --cache-from/--cache-to $CACHE --tenant $TENANT --tenant-key-file /var/run/potter/tenant.key --sign-key /var/run/potter/sign.key --secret id=…,src=/var/run/potter/secrets/…`, Secrets mounted read-only, docker config from a Secret.
- Document the event stream for the platform and that deploy commands become the `deploy` process.

Validation: `kubectl --dry-run=client` is unavailable offline; the YAML is parsed in a unit test (`serde_yaml` not added — a test reads it as text and checks the command flags match `potter build --help`).
