# potter plan format (`potter.plan/v1`)

A plan is what `potter plan --json` prints and what later milestones execute. The JSON Schema is
[`docs/schema/potter.plan.v1.json`](schema/potter.plan.v1.json), generated from the Rust types and
checked in CI.

## Canonical form and hashing

- Plans are serialised as RFC 8785 (JCS) canonical JSON: object keys are sorted by UTF-16 code
  units, there is no insignificant whitespace, and strings are escaped as ECMAScript does.
  Floats are rejected.
- A node's hash is SHA-256 over its canonical bytes, written `sha256:<hex>`.
- A step's hash, used for cache keys, ignores its `name` and `timeout_secs`.
- The plan hash ignores every `timeout_secs`.

## Shape

- `targets[]`: `name`, `root` (relative to the repo), `provider`, `build` (a chain), and `image`.
- `build.base`, one of:
  - `{kind: "catalog", layers: [{name, version?}]}`;
  - `{kind: "oci", reference, digest?}`;
  - `{kind: "stage", name}`.
- `build.steps[]`, internally tagged by `kind`:
  - `inputs` `{name, include[], exclude[], dest, owner}`: a host-built layer of exactly the
    globbed source files. A step only ever sees the inputs declared by itself and its ancestors.
  - `run` `{name, command: {argv: […]} | {shell: "…"}, env, workdir, user, network, cache_mounts[], secrets[], timeout_secs?}`.
  - `copy_from`, `apt` `{name, packages[], snapshot}` and `meta`.
- `image`:
  - `base`;
  - `runtime_steps[]` (apt only);
  - `export[]` `{path, dest, exclude[]}`, read from the build chain's final state;
  - `files[]` `{path, content, mode}`;
  - `config` `{env, workdir, user, labels, port?, stop_signal?}`;
  - `processes` `{name: {command[], workdir?, env}}`;
  - `services` `{process: [{name, command[], ready: {unix: path} | {tcp: port} | "none", stop_signal}]}`.

## potter.toml (P1a)

| Key | Meaning |
|---|---|
| `[target.<name>]` `root`, `provider`, `port` | Build root (relative); force a provider (`php`, `laravel`, `node`, `bun`, `deno`, `static`, `python`, `ruby`, `go`, `rust`, or `none` for an explicit plan); listen port. |
| `[target.<name>.runtime]` `php`, `node`, `bun`, `deno`, `python`, `ruby`, `go`, `rust`, `extensions` | Version lines and extra PHP extensions. Always wins over detection. |
| `[target.<name>.php]` `nginx_conf`, `fpm_pool`, `octane_server`, `port` | Replace the generated configs (repo-relative files); pick the Octane server; port alias. |
| `[target.<name>.node]` `package_manager` | `npm`, `pnpm`, `yarn` or `bun`; its lockfile must exist. |
| `[target.<name>.python]` `package_manager`; `[target.<name>.python.django]` `collectstatic` | `uv`, `poetry`, `pipenv` or `pip` (its lockfile must exist); turn Django's `collectstatic` step off. |
| `[target.<name>.steps.<step>]` `run`, `inputs`, `skip`, `after`, `network`, `cache` | Override a detected step; with `after`, insert a new one. |
| `[target.<name>.process.<process>]` `cmd` | Replace a process (drops its services). |
| `[target.<name>.build]` `commands` | Appended build steps `user-build-<n>`. |
| `[target.<name>.deploy]` `commands` | A `deploy` process the platform runs once before rollout. |
| `[target.<name>]` `export` | Extra paths in the image: relative to the target root (copied under the image workdir, e.g. `/out/data` for Go/Rust) or absolute. Explicit plans default to `/app`. |
| `[target.<name>.base]` `catalog`/`oci`, `run_catalog`/`run_oci` | Explicit plans only. |
| `discover` (top level) | `false` turns off zero-config monorepo targets (directories with their own manifest when the root has none). Declared `[target.*]` entries otherwise merge over discovered ones. |
| `[apt]` `build`, `runtime`, `snapshot` | Debian packages from a pinned snapshot.debian.org date. |

`potter plan --explain` prints where every decision came from: what a detector found (with its
evidence), a potter.toml key, or a default.

## Cache keys (spec §8.1)

A chain's root key is `SHA-256("potter.cache/v1/base" ‖ len ‖ base manifest digest)`. Each step's key is SHA-256 over `"potter.cache/v1"`, then each of these fields as an 8-byte big-endian length followed by its bytes:

1. the parent key (the previous step's, or the root);
2. the step hash (the JCS of the step without `name` and `timeout_secs`);
3. the inputs digest (an `inputs` step's layer, `sha256:…`, else empty);
4. the SHA-256 of the JCS of the effective environment when the step runs (the base image's `Env`, then `meta` steps, then the step's own `env`);
5. the secrets HMAC (empty until secrets are supported);
6. the platform (`linux/arm64`);
7. the executor class (`ns` or `vm`);
8. the potter major version (`1`).

The local index (`$POTTER_HOME/cache/<tenant>/index/<key-hex>`) maps a key to `{"layer": "sha256:…" | null, "class": "ns" | "vm", "created": <unix seconds>, "potter": "<version>"}`. A layer is an uncompressed OCI tar blob in `$KILN_HOME`; an entry whose blob is gone is a miss.

## Secrets in keys (spec §5.4)

A step's secrets field (item 5 above) is `HMAC-SHA256(tenant_key, id ‖ 0x00 ‖ value)` for each secret it mounts, concatenated in id order. The tenant key is 32 random bytes in `$POTTER_HOME/tenants/<tenant>/key`. The values themselves never enter a key, a layer or the index.

## Remote cache entries (spec §8.3)

An entry is an OCI image manifest tagged `<key-hex>` in the repository `<prefix>/<tenant>/cache` of the cache registry.
- `artifactType` is `application/vnd.potter.cache.v1+json`.
- Its config, of type `application/vnd.potter.cache.config.v1+json`, is `{"class": "ns" | "vm", "potter": "<version>", "parent": "<parent key hex>"}`.
- Its one layer (none for steps without a layer) is the step's uncompressed tar, `application/vnd.oci.image.layer.v1.tar`.

On pull, blobs are digest-verified, and the layer must pass kiln's layer ingest before it is used.

## Progress events (`--progress json`, spec §11)

One JSON object per line on stdout, with `event` set to one of:

| `event` | Fields | Meaning |
|---|---|---|
| `step_start` | `target`, `step` | A step starts running. |
| `cache_hit` | `target`, `step` | A step was found in the local or remote cache. |
| `sealed` | `target`, `step`, `secs` | A step finished, and its diff is a layer. |
| `pushed` | `target`, `step` | A step's entry reached the remote cache. |
| `failed` | `target`, `step` | A step failed. |
| `log` | `target`, `step`, `line` | One line of step output, unmodified except secret redaction. |
| `warning` | `message` | |
| `unchanged` | `target` | `--affected` skipped the target. |
| `target_built` | `target`, `reference`, `digest`, `executed`, `cached` | |
| `error`, `hostile` | `target`, `message`, `exit` | The build failed. `hostile` is a sandbox violation (exit 3). |
| `published` | `target`, `reference`, `digest`, `referrers` | `--output` pushed the image (`digest` is its OCI manifest) and attached the attestations. |

A run base's steps report under the target name `<target>:run`.

## Image annotations (spec §5.1, §18)

Every image manifest potter writes carries:

- the plan's `labels`;
- `dev.potter.port`: the listen port, when the plan has one;
- `dev.potter.process.<name>`: each process's command as a JSON array;
- `dev.potter.target`: the canonical hash (hex) of the target that built it.

## Attestations (spec §17)

Each is an OCI image manifest with `subject` set to the image, an empty config (`application/vnd.oci.empty.v1+json`), one layer holding the attestation, and the annotation `dev.potter.attestation` = `sbom`, `provenance` or `signature`. The referrers tag schema index at `sha256-<hex>` lists them; potter replaces its own kinds there and keeps every other descriptor.

- **Signed** (`--sign-key`): `artifactType` and layer type `application/vnd.dev.sigstore.bundle.v0.3+json`.
  - `sbom`, `provenance`: a DSSE envelope (`payloadType` `application/vnd.in-toto+json`) around an in-toto v1 statement whose subject is the image digest. Predicate types `https://spdx.dev/Document/v2.3` (the SPDX 2.3 document) and `https://slsa.dev/provenance/v1`.
  - `signature`: a `messageSignature` over the image manifest bytes (ECDSA P-256, SHA-256).
  - `verificationMaterial.publicKey.hint` is the base64 SHA-256 of the public key's SPKI DER; `tlogEntries` is empty (no transparency log in v1).
- **Unsigned**: `artifactType` `application/vnd.in-toto+json`, the statement itself; no `signature` referrer.

## Exit codes

0 success · 2 usage, config, plan or detection error (including a missing path) · 4 other I/O errors.
