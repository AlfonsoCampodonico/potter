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

## Exit codes

0 success · 2 usage, config, plan or detection error (including a missing path) · 4 other I/O errors.
