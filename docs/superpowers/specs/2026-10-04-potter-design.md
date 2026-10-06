# potter — Design Spec

**Date:** 2026-10-04
**Status:** Approved rev 1.6 (2026-10-06)
**Scope:** `potter`, a source-to-image builder that turns a repository (zero-config, `potter.toml` or Dockerfile) into a `kiln` microVM image.
**Related:** the kiln spec (`kiln/docs/superpowers/specs/2026-09-30-kiln-design.md`, rev 2.7 on `docs/m3a-plan-spec-2.6`), and `ROADMAP.md` in the playground root.

`potter` is a placeholder name: a potter shapes the clay, kiln fires it.

### Revision history

- **rev 1 (2026-10-04):** initial design.
- **rev 1.1 (2026-10-04):** the PHP web runtime is nginx + php-fpm under a small supervisor, `potter-run`, instead of FrankenPHP (§2, §6.1, §6.1.1, §7, §15). This was the user's decision.
- **rev 1.2 (2026-10-05):** consolidation into the existing repos (the user's decision):
  - `potter-run` is dropped. kiln-init gains `services`, and nginx + php-fpm run as kiln services (§4, §6.1.1).
  - `NsExecutor` is built on vmkit's generalised `sandbox` and `net` modules instead of its own namespace code (§4, §9.4).
  - kiln's planned native RUN-free Dockerfile backend is cancelled. potter is the only Dockerfile implementation in the project family, and `kiln build --builder potter` becomes `auto`'s first choice (§2, §6.3).
- **rev 1.6 (2026-10-06):** two additions at the user's request.
  - **Zero-config monorepo targets (§10):**
    - Applies when potter.toml declares no `[target.*]`, the root matches no provider, and directories below it do.
    - Candidates are depth-1 directories with a manifest, plus `apps/*` and `services/*`.
    - Each becomes a target named after its path, slugified to `[a-z0-9._-]`. A `target` decision records why.
    - A root that matches a provider stays a single target. Workspace-manifest discovery and `--affected` stay in P6.
    - Discovery skips hidden directories and tooling or documentation directories (docs, tools, scripts, examples, tests, e2e, fixtures, benchmarks).
    - Declared `[target.*]` entries merge over discovered ones: the same name inherits the discovered root, and a discovered directory already used as a declared root is dropped. Top-level `discover = false` turns discovery off.
    - Errors from a discovered target say it was discovered and how to opt out.
  - **`[target.x.process.web] cmd` also satisfies detection** wherever a provider would otherwise ask for it (no start script, several main packages or binaries, no framework). Go and Rust then name every binary's process after itself.
  - **`[target.x] export` for detected targets:**
    - Entries relative to the target root are copied under the image workdir (Go/Rust: `/out/<p>`); absolute entries are copied as is.
    - It closes the gap where Go/Rust images ship only the binary and miss runtime files such as templates or `data/`.
- **rev 1.5 (2026-10-06):** P1b refinements (implementation level).
  - **Catalog lines refreshed to current releases (§7):**
    - python 3.12/3.13/3.14 (default 3.13);
    - ruby 3.3/3.4/4.0 (default 3.4);
    - go 1.25/1.26/1.27 (default 1.27);
    - rust 1.97/1.98/1.99 (default 1.99).
    - Go 1.24 and Python 3.11 have aged out, and real repos already declare `go 1.26`.
  - **Default ports:** Python 8000, Ruby 3000, Go and Rust 8080.
  - **Python:**
    - Dependencies go into a `/app/.venv` virtualenv that ships in the image; processes call its binaries by absolute path.
    - `[target.x.python] package_manager` (uv, poetry, pipenv or pip) and `[target.x.python.django] collectstatic`.
    - `-r`/`-c` includes in requirements files become inputs.
  - **Ruby:**
    - Gems install with `BUNDLE_DEPLOYMENT=1` into `/app/vendor/bundle`.
    - A `ruby-` prefix in `.ruby-version` is accepted.
    - A `worker` process is added for solid_queue (`bin/jobs`) or sidekiq.
  - **Go:**
    - Steps set `GOTOOLCHAIN=local`, so the go command never downloads a toolchain.
    - The root `package main` binary is named `app`.
    - With several main packages, `web` is the one named server, api, web or app; otherwise potter asks for `process.web.cmd`. The other mains become processes named after themselves.
  - **Rust:**
    - Stub sources are written by the `deps` Run step itself, followed by `cargo clean --release -p <members>` before the real build.
    - Repo-derived paths and names used in shell commands must match `[A-Za-z0-9._/-]`.
    - Multi-binary selection follows the Go rule, preferring the root package's binary.
  - **Detector-added apt steps** are named `apt-detected` and `apt-detected-runtime`, from a small table: psycopg2/pg → libpq, mysqlclient/mysql2 → libmariadb.
- **rev 1.4 (2026-10-05):** implementation-level refinements found while planning P1a. None changes a user decision.
  - **Catalog refs from P1a.** Detectors emit `Base::Catalog` refs from P1a onwards. Until P3, P2 maps those refs to upstream OCI images, so detectors don't change when the catalog arrives (§15).
  - **IR additions.**
    - `Target.provider`.
    - `Image.files`: generated config files, host-built in P2.
    - `ImageConfig.port`: recorded as `dev.potter.port`.
    - `ImageConfig.stop_signal`: the main process's stop signal (§5.1).
  - **Lockfile read cap.** Lockfiles that detectors parse (`composer.lock`, `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `bun.lock`, `deno.lock`) have a 64 MiB read cap; every other file keeps 4 MiB. Real lockfiles exceed 4 MiB (§6.1).
  - **Several JS lockfiles, no `packageManager`.** npm is chosen when `package-lock.json` is present, and the choice is recorded as a decision. Otherwise the order is pnpm, then yarn. Override with `[target.x.node] package_manager`. Real Laravel repos commit both `package-lock.json` and `yarn.lock` (§6.1).
  - **PHP config paths.** The generated nginx and php-fpm configs live at `/etc/potter/nginx.conf` and `/etc/potter/php-fpm.conf`. The php-fpm socket moves to `/run/php-fpm.sock`, so no directory has to exist under `/run` (§6.1.1).
  - **Deno's cache is in the layer.** Deno's `DENO_DIR` is `/app/.deno` and lives in the layer, not in a cache mount, because Deno needs its module cache at runtime (§6.1).
  - **`port` at target level.** `[target.x] port` applies to every provider; `[target.x.php] port` remains as an alias.
- **rev 1.3 (2026-10-05):** gap fill.
  - Added rebase (§16), supply chain (§17), platform integration (§18) and the catalog patch cadence (§7).
  - The run image now has its own `runtime_steps` for runtime `Apt` packages (§5.1).
  - New property P12.
  - The KVM prerequisite for build nodes is stated (§1).

---

## 1. Goals

`potter` builds application images from source and emits `kiln` images. It plays the role that BuildKit and Cloud Native Buildpacks play for containers, with turborepo-style caching:

- **Zero-config detection** for PHP, JavaScript, TypeScript (Node, Bun, Deno), Python, Ruby, Go and Rust. Laravel gets first-class treatment.
- **One plan IR** that every frontend lowers onto: language detectors, `potter.toml` and Dockerfiles.
- **Input-precise caching.** A step reruns only when the files it declares change. The sandbox enforces those declarations.
- **A shared remote cache, scoped per tenant**, stored in an OCI registry. **Persistent cache mounts** hold package-manager download caches.
- **Monorepos:** one repo yields several images, identical steps dedupe across them, and only affected targets rebuild.
- **Untrusted builds run in microVMs.** A rootless namespace executor is also available, for trusted builds and hosts without KVM.

It is meant to be serious open-source software (stable plan and cache formats, real docs). It is also meant to be usable as the build system for a hosting platform that deploys microVMs.

### Success criteria

1. With no configuration, `potter build <fixture>` produces a `kiln` image for every fixture in §14.1. The image boots under `kiln run` on both VMMs and both architectures and passes the fixture's check (HTTP 200 or an exec assertion). This holds with both executors.
2. `potter build -f Dockerfile .` produces an image whose filesystem matches what `docker buildx` produces for the same Dockerfile, across the Dockerfile fixture set (§14.3).
3. Caching:
   - An unchanged rebuild where every step hits completes in **< 1 s**.
   - On the Laravel fixture, a change limited to application source re-executes only the steps after the source step.
   - Changing a file that no step declares re-executes nothing.
4. potter's overhead per build on the VM executor is **< 2 s** on top of the steps' own runtime: boot, sealing, ingest, erofs and export. A cold Laravel fixture build takes **< 90 s** on CI hardware with warm catalog layers.
5. Build nodes using the VM executor need `/dev/kvm`, from nested virtualization or bare metal, like the nodes that run kiln images.
6. The namespace executor needs **no root and no KVM**. It needs only unprivileged user namespaces, `newuidmap`/`newgidmap` and a `/etc/subuid` range, the same requirements as rootless Podman.
7. Every threat property in §3 has a test (§14.4).
8. A catalog security fix reaches every affected image by rebase (§16) without rebuilding from source, and every image carries an SBOM, a signature and provenance (§17).
9. `docs/plan-format.md` documents the plan IR, `potter.toml`, the cache artifact format and the catalog format well enough for a third party to produce plans or consume caches.

### Non-goals (v1)

- The build daemon. It comes in milestone P7 and wraps the same library.
- Sharing step cache entries across tenants. Only signed catalog layers are shared.
- A hermetic network. Builds get policy-restricted egress (§9.5), and per-tenant cache scoping covers the gap.
- Executing build steps on macOS. `plan`, `detect`, `cache ls` and `catalog` run there natively; `build` needs Linux.
- Cross-architecture builds through emulation. Builds run on a builder of the target architecture.
- Detectors for Java, .NET, Elixir and other languages. These build through an explicit `potter.toml` plan or a Dockerfile.
- Dockerfile `ADD <url>`, `ADD <git ref>` and `RUN --mount=type=ssh`. They are refused with a clear error.
- A Windows host.

---

## 2. Decisions (with rationale)

| Topic | Decision | Rationale |
|---|---|---|
| Core model | A typed plan IR (§5). Detectors, `potter.toml` and Dockerfiles are frontends that lower onto it | One executor, one cache and one test surface. New languages are additions, not rewrites. |
| Output | `kiln` images (erofs layers, init layer, kiln config). OCI export goes through `kiln` | The deploy target is a microVM. Producing erofs directly skips an OCI tar round trip. |
| Repository | Its own repo. It pins `kiln` crates and `vmkit` by git rev | Follows the ROADMAP rule: independent projects, coupling through documented formats. kiln-init build mode lands in `kiln` through its own PRs. |
| Default executor | `VmExecutor`: one microVM per target chain (§9.3) | Builds run untrusted customer code. This reuses `vmkit`, kiln-init and kiln-proto. |
| Second executor | `NsExecutor`: rootless user/mount/PID/net namespaces, built on vmkit's `sandbox` and `net` (§9.4) | Works without KVM and starts faster for trusted builds and development. Reusing vmkit means one audited sandbox and one egress policy for VMMs and build steps alike. A trust ordering keeps its outputs away from VM builds (§8.4). |
| Layer capture | Each step's overlay upper is sealed and converted to an OCI-format tar diff, which goes through kiln's untrusted tar ingest into erofs | One ingest path for both executors and for OCI bases, already hardened for untrusted layers. |
| Diff transport (VM) | A dedicated raw output disk, not vsock | High throughput. The disk size is the output cap, so the cap holds by construction. The control channel stays small. |
| Export and `COPY --from` | Computed **on the host** from the chain's erofs layers, using kiln's merge logic | Deterministic, on the trusted side, and no guest round trip. |
| Step chains | The steps in a target form a **linear chain**. Parallelism comes from building several targets at once | Matches overlay layering and keeps cache keys simple prefixes. |
| Declared inputs | Enforced: a step sees only the declared input files of itself and its ancestors (§9.2) | Makes input-precise cache keys safe instead of merely hopeful. |
| Runtimes | A catalog of composable, signed erofs runtime layers (§7). System packages come from `apt` steps pinned to a `snapshot.debian.org` date | No package manager in the hot path, heavy dedup across tenants, reproducibility. |
| Base OS | Minimal Debian stable (glibc) | Compatible with prebuilt native wheels, gems and npm binaries. |
| Cache scope | Per tenant. Only catalog layers are global | A shared step cache is a poisoning vector, because network fetches during steps are not hermetic. |
| Remote cache store | An OCI registry, through `kiln-registry` | No new server. Cache layers use kiln's layer media type, so they dedupe with images. |
| PHP web process | nginx + php-fpm, run as kiln-init `services` (§6.1.1). Octane apps run Octane's own server instead. Overridable | The conventional Laravel production stack. One PID 1 supervises everything; there's no second supervisor in the image. |
| Dockerfile parser | Our own, in `potter-dockerfile`. The only Dockerfile implementation across kiln and potter | Exercises the internals, avoids an unmaintained crate dependency, and gives exact control over the lowering. kiln delegates to it instead of keeping a second, RUN-free parser. |
| Determinism | Inputs layers, catalog composites, export layers and every host-built layer are byte-reproducible. Step layers are deterministic only when the step's command is | No claim beyond what potter controls. mtimes and ownership are normalised in the erofs writer either way. |

---

## 3. Threat model

### Assets

- The host running builds, and its network position (cloud metadata, private services).
- Other tenants' cache entries, cache mounts, secrets and images.
- The integrity of the catalog, and of every image potter emits.

### Adversaries

- **A tenant's repository**: arbitrary files, symlinks, Dockerfiles and `potter.toml`. Detectors parse it.
- **A tenant's build steps**: arbitrary code running inside the executor sandbox, including package-manager scripts.
- **Network content fetched during steps** (registries, CDNs), which may be attacker-influenced.
- **A remote registry** serving cache or base-image content.

### Trusted

The `potter`, `kiln` and `vmkit` binaries; catalog signing keys compiled into `potter`; the pinned kernel and kiln-init; the invoking user; the platform that issues per-tenant registry credentials and tenant keys.

### Out of scope

- Attackers already running as the invoking user.
- Hardware side channels.
- Hermetic reproducibility of steps that fetch from the network.
- Isolation of namespace-executor builds against kernel exploits. That is why `NsExecutor` can be disabled per deployment (§9.4) and why its outputs are trust-ordered (§8.4).
- Enforcing tenant isolation in the remote registry against a malicious *client*. The registry's per-tenant credentials enforce it. potter itself only ever addresses its own tenant's path.

### Required properties (each tested in §14.4)

| # | Property | Where enforced |
|---|---|---|
| P1 | A build in tenant A can never read or write cache entries, cache mounts or secrets of tenant B. | §8.2, §8.5 |
| P2 | Only catalog layers whose signed index verifies are shared across tenants. Catalog resolution fails closed. | §7 |
| P3 | A `VmExecutor` build never consumes a step cache entry produced by `NsExecutor`. | §8.4 |
| P4 | Diffs produced by a step are untrusted input. They are bounded in bytes, entries and path depth, path-validated, re-hashed on the host, and ingested through kiln's untrusted tar path. Guest-reported digests and sizes are never trusted. | §9.3, §9.4 |
| P5 | Secret values never appear in layers, in logs (they are redacted on a best-effort byte match), or in cache keys in plaintext (keys use an HMAC under the tenant key). | §5.4, §8.1 |
| P6 | A step can read only the declared inputs of itself and its ancestors. Undeclared source files are absent from its filesystem. | §9.2 |
| P7 | Build-time egress cannot reach link-local (including metadata), private, loopback-mapped or host addresses, or other builds, with either executor. The step cannot change the policy. | §9.5 |
| P8 | Repository content cannot make potter read host files outside the build context. Symlinks that resolve outside the context are refused. | §9.2 |
| P9 | Detection is static parsing only. potter never executes repository code, package managers or build tools on the host. Parsers are size- and depth-bounded. | §6.1 |
| P10 | Dockerfile content cannot make the host fetch URLs, clone git repos or reach SSH agents. Those forms are refused in v1. | §6.3 |
| P11 | A build cannot exhaust host disk, memory or CPU without bound. Output disk, scratch, cache mounts, step time and build time all have hard caps. | §9.3, §9.4, §12 |
| P12 | Provenance can't overstate how an image was built. It records the executor class and the catalog index digest, and an image that contains any `ns`-class layer can't claim `vm` provenance. Rebase never changes app layers and records the base it replaced. | §16, §17 |

---

## 4. Repository and components

`potter` is a Cargo workspace in its own repository. It depends on `kiln-oci`, `kiln-erofs`, `kiln-image`, `kiln-store`, `kiln-registry` and `kiln-proto` and on `vmkit`, all pinned by git rev.

| Crate | Responsibility | Depends on |
|---|---|---|
| `potter-plan` | IR types (§5), JSON schema `potter.plan/v1`, canonicalisation (RFC 8785 JCS) and hashing, validation | — |
| `potter-detect` | The `Provider` trait and one provider per language (§6.1). Static parsers for composer.json/lock, package.json and lockfiles, pyproject/uv.lock/poetry.lock/requirements, Gemfile/Gemfile.lock, go.mod, Cargo.toml/Cargo.lock, deno.json/deno.lock | plan |
| `potter-config` | Parses `potter.toml` and merges it onto detector output. Explicit config always wins, and every decision is recorded for `--explain` | plan |
| `potter-dockerfile` | Dockerfile lexer and parser, and lowering onto the plan IR (§6.3) | plan |
| `potter-graph` | Workspace and target discovery, step dedup across targets, affected-target computation, scheduling (§10) | plan |
| `potter-catalog` | Catalog index verification (ed25519), resolving constraints to layer digests, ABI checks, composite base merges (§7) | kiln-registry, kiln-erofs |
| `potter-cache` | Cache keys, the local cache index, the remote cache (OCI), tenant keys, the executor trust ordering, gc (§8) | kiln-store, kiln-registry |
| `potter-exec` | The `Executor` trait, inputs layers, diff ingest, host-side export. `VmExecutor` and `NsExecutor` live in submodules (§9) | vmkit (`Vmm`, `sandbox`, `net`), kiln-erofs, kiln-proto, kiln-image |
| `potter` | The CLI (§11) | all |

**Changes in `kiln`** (separate PRs, tracked as dependencies of P2, P4 and P5):

- **kiln-proto and kiln-init `services`** (needed by P2 for PHP web images):
  - `Config.services: [Service{name, process, ready, stop_signal}]`, where `ready` is `unix:<path> | tcp:<port> | none`.
  - Services start in order, each waiting for the previous one's readiness, and the main process starts last.
  - If any of them exits, the rest are stopped in reverse order with their `stop_signal`, within the shutdown grace period. The VM's exit status is that of the first process to exit.
  - Signals the host delivers go to the main process.
  - Every service shares the main process's stdout and stderr.
- **kiln-proto:** `Config.mode = Run | Build`, plus the build-mode messages (§9.3).
- **kiln-init build mode:** step execution, overlay sealing, conversion of the upper into an OCI tar diff, and writing to the output disk.
- **kiln `build --builder potter`:**
  - Shells out to the `potter` binary, as it does for BuildKit, which avoids a crate cycle.
  - `--builder auto` tries potter first, then BuildKit, then Podman/Buildah.
  - This replaces the native RUN-free backend in kiln spec rev 2.7, so kiln M3c shrinks to backend dispatch.

**Changes in `vmkit`** (needed by P2):

- **Generalised `sandbox`.** It can run an arbitrary process, not only a VMM, in the unprivileged user/mount/PID namespaces with seccomp/Landlock and cgroup limits. It also supports nested user namespaces, so the sandboxed process holds no capabilities over its network namespace (§9.4).
- **`net` without a tap.** It can apply the same nftables policy and `pasta` egress to a bare network namespace, so a process can join it directly.

**Store.** potter writes images and layer blobs into `$KILN_HOME`, so `kiln run`/`push`/`inspect` see them directly. potter's own state lives in `$POTTER_HOME` (default `~/.local/share/potter`):

```
$POTTER_HOME/
  cache/<tenant>/index/<key-hex>      # → {layer digest, executor class, created, potter version}
  mounts/<tenant>/<mount-id>.ext4     # persistent cache mounts (VmExecutor)
  mounts/<tenant>/<mount-id>/         # persistent cache mounts (NsExecutor)
  unpacked/<layer-digest>/            # NsExecutor lower dirs, owned within the subuid range
  tenants/<tenant>/key                # 32-byte HMAC key, 0600 (or supplied by the platform)
  catalog/index/<digest>              # verified catalog indexes
```

The default tenant for local CLI use is `local`.

---

## 5. Plan IR

### 5.1 Shape

```
Plan       { schema: "potter.plan/v1", targets: [Target] }
Target     { name, root: path, platform, provider, build: Chain, image: Image }
Chain      { base: Base, steps: [Step] }
Base       = Catalog { layers: [CatalogRef] }        # resolved to digests before execution
           | Oci { reference, digest }               # pinned; tags resolve once at plan time
           | Stage { name }                          # Dockerfile stage reference (§6.3)
Step       = Inputs   { name, include: [glob], exclude: [glob], dest: path, owner: uid:gid }  # host-built, no executor
           | Run      { name, cmd: [string] | shell: string, env: {k: v}, workdir, user, network: bool,
                        cache_mounts: [CacheMount], secrets: [SecretRef], timeout }
           | CopyFrom { name, stage | target, paths: [path], dest, owner, mode? }             # host-built
           | Apt      { name, packages: [string], snapshot: date }                            # lowers to a Run with a pinned mirror
           | Meta     { env?, workdir?, user?, labels? }                                       # config only, no layer
Image      { base: Base, runtime_steps: [Apt], export: [Export], files: [GeneratedFile], config: ImageConfig,
             processes: {name: Process}, services: {process name: [Service]} }   # services → kiln Config.services (§4)
GeneratedFile { path, content, mode }               # host-built layer, e.g. the nginx/php-fpm configs (§6.1.1)
ImageConfig   { env, workdir, user, labels, port?, stop_signal? }
Export     { path, dest, exclude: [glob] }           # read from the final state of the target's build chain
CacheMount { id, target: path, sharing: exclusive }
SecretRef  { id, target: path }                      # mounted file, never in a layer
```

- A target's `build` chain produces its build filesystem.
- The target's `image` is assembled separately: the run base, then the exported paths, then the init layer. Build-only tooling (compilers, node for asset builds, dev dependencies) never ships unless it is exported explicitly.
- `runtime_steps` holds runtime system packages (`[apt] runtime`). They run as their own short chain on top of the run base, so the libraries an app needs at runtime ship without its build toolchain. They're cached like any other step.
- `processes` is written into the kiln image config. The process named `web`, or the only process, becomes the entrypoint. The others are recorded as `dev.potter.process.<name>` annotations, for platforms that run the same image as web, worker or scheduler.

### 5.2 Canonical form and hashing

- A plan is serialised as JSON and canonicalised with RFC 8785 (JCS).
- Every node has a canonical hash: SHA-256 over the JCS bytes, with `timeout` and step `name` (labels only) excluded.
- `potter plan --json` prints the canonical plan; the schema is published in `docs/plan-format.md`.

### 5.3 Validation

- Paths are absolute in the image or relative to the target root, with no `..` and no NUL.
- Globs are gitignore-style.
- `Run.user` is a name or a numeric id ≤ 2³¹−1, matching kiln-init's rule.
- Cache mount ids and step names match `[a-z0-9._-]{1,64}`.
- `Base::Oci` must carry a digest after resolution.
- Unknown fields are rejected.

### 5.4 Secrets

- Secrets are supplied as `--secret id=<id>,src=<file>` or by the platform.
- They are mounted read-only at `SecretRef.target` for that one step only, on a tmpfs inside the sandbox, and are absent from the sealed upper.
- The cache key includes `HMAC-SHA256(tenant_key, id ‖ value)`.
- Secret values are redacted from streamed logs on a best-effort byte match. This is documented as best-effort.

---

## 6. Frontends

### 6.1 Detectors

`Provider::detect(&RepoView) -> Option<Detection>`. `RepoView` is a read-only, bounded view of the target root:

- the file list (capped at 200 000 entries);
- reads capped at 4 MiB per file, except lockfiles that detectors parse, which are capped at 64 MiB;
- no symlink traversal outside the root.

A `Detection` carries a partial plan, a specificity level and its evidence (the files and keys that drove each decision).

- **Selection:**
  - If exactly one provider claims the root, potter uses it.
  - If several do, the more specific one wins when the claims nest (for example Laravel over generic PHP, or Static over Node).
  - If two providers at the same level both claim the root (for example a Rails app with a standalone Next app at the root), potter refuses. The error lists both, with the `potter.toml` key that resolves it.
  - Supporting toolchains are not competing claims. A Rails or Laravel app's `package.json` adds Node to the *build base* of the owning provider.
- **Never execute (P9):** detectors only parse files. Version constraints are evaluated by potter's own constraint parsers: composer, npm semver, PEP 440, RubyGems, the Go toolchain directive, and Cargo's `rust-version`.
- **Version choice:** the highest catalog version that satisfies the repo's constraint. The order is the language's own version file (`.php-version`, `.nvmrc`/`.node-version`, `.python-version`, `.ruby-version`, the `go`/`toolchain` directive, `rust-toolchain.toml`), then manifest constraints, then the catalog default.

Every provider emits the same chain shape: base, then dependency inputs, then the dependency install step, then source inputs, then build steps, with export in the image. That ordering is what makes source-only changes cheap.

| Provider | Detects | Dependency step (inputs → cmd; cache mounts) | Build steps | Run base | Default process |
|---|---|---|---|---|---|
| **PHP generic** | `composer.json` | `composer.json`, `composer.lock` → `composer install --no-dev --no-scripts --no-autoloader --prefer-dist`; `composer` | `composer dump-autoload -o --no-dev` after source | php + required exts + php-fpm + nginx | `web` = nginx main process with a php-fpm service (§6.1.1) |
| **Laravel** | PHP + `laravel/framework` + `artisan` | as PHP; node deps when `package.json` exists (as Node) | asset build (`vite`/`mix`) when present; `php artisan package:discover`; *no* `config:cache`/`route:cache` (env is runtime) | as PHP; exts from `ext-*` across composer.json + composer.lock, plus Laravel defaults (`bcmath`, `intl`, `pcntl`, `pdo_mysql`, `pdo_pgsql`, `redis`); with Octane, the server's runtime (`swoole` ext, RoadRunner binary or FrankenPHP) replaces nginx + php-fpm | `web` = nginx + php-fpm (as PHP generic); with Octane, `octane:start --server=<detected> --host=0.0.0.0` (server from `config/octane.php` / `OCTANE_SERVER`, else the installed server package); `worker` = `queue:work` (or `horizon`); `scheduler` = `schedule:work` |
| **Node** | `package.json` without `bun.lock*`/`deno.json*` | manifest + lockfile → `npm ci` / `pnpm install --frozen-lockfile` / `yarn install --immutable`; picked from `[target.x.node] package_manager`, then `packageManager` (via corepack), then the lockfile. With several lockfiles, npm wins when `package-lock.json` exists (recorded as a decision); yarn classic uses `--frozen-lockfile`; `npm`/`pnpm`/`yarn` | `build` script when present (covers `tsc`); then prune dev deps (`npm prune --omit=dev`, `pnpm prune --prod`) | node | Next with `output: 'standalone'` → `node server.js`; Nuxt → `node .output/server/index.mjs`; else the `start` script |
| **Bun** | `bun.lock`/`bun.lockb` | manifest + lockfile → `bun install --frozen-lockfile`; `bun` | `build` script when present | bun | the `start` script, else `bun run <module/main>` |
| **Deno** | `deno.json`/`deno.jsonc` | `deno.json*`, `deno.lock` → `deno install --frozen` (`deno install` without a lockfile); no cache mount: `DENO_DIR=/app/.deno` stays in the layer because Deno needs it at runtime | `deno task build` when defined | deno | `deno task start`, else `deno serve` on the detected entry point. Permissions come from the task definition, never widened by potter |
| **Static** | JS project (Node/Bun/Deno) whose build outputs a static dir and that has no server runtime deps | as the underlying JS provider | as the underlying JS provider | os-min + `static-server` catalog layer | serve the build dir |
| **Python** | `pyproject.toml`, `requirements.txt`, `Pipfile` | uv: `pyproject.toml`, `uv.lock` → `uv sync --frozen --no-dev --no-install-project`; poetry: `poetry install --only main --no-root`; pip: `pip install -r requirements.txt` into `/app/.venv`; pipenv: `pipenv sync`; `uv`/`pip`/`poetry` | project install after source; Django `collectstatic --noinput` (disable with `[target.x.python.django] collectstatic = false`) | python | gunicorn/uvicorn/granian, whichever is in the deps, against the detected ASGI/WSGI app; Django → `gunicorn <proj>.wsgi` |
| **Ruby** | `Gemfile` | `Gemfile`, `Gemfile.lock` → `bundle install` (deployment, without development:test); `bundler` | Rails: `SECRET_KEY_BASE_DUMMY=1 bin/rails assets:precompile`, `bootsnap precompile` | ruby | Rails → `bin/rails server -b 0.0.0.0`; Rack → `bundle exec rackup` |
| **Go** | `go.mod` | `go.mod`, `go.sum` → `go mod download`; `gomod` (`GOMODCACHE`), `gobuild` (`GOCACHE`) | `CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/<name> <pkg>` per main package (root `main.go`, or each `cmd/*` → one target each) | os-min (ca-certs, tzdata) | `/out/<name>` |
| **Rust** | `Cargo.toml` | all workspace `Cargo.toml` + `Cargo.lock`, plus potter-generated stub `src/main.rs`/`lib.rs` per member (the cargo-chef technique) → `cargo build --release --locked`; `cargo-registry`, `cargo-git` | after source: `cargo clean --release -p <member>` for every workspace member (normalised mtimes would otherwise make cargo skip the rebuild), then `cargo build --release --locked`; one target per binary | os-min (ca-certs, tzdata) | `/out/<bin>` |

Notes:

- For Rust, the `target/` dir stays in layers, not in a cache mount. Layers are shared through the remote cache across builders; mounts are not.
- Go's `GOCACHE` is a mount because Go's own cache is content-addressed and safe to share between runs.
- For native libraries (libpq, libvips, ImageMagick and so on), providers add an `Apt` step when a known package needs one. This uses a static table: for example `psycopg2` needs `libpq-dev` in the build and `libpq5` at runtime. Users can add more with `[apt] build = [...]`, `runtime = [...]`.

### 6.1.1 PHP web runtime (nginx + php-fpm)

PHP web images run nginx and php-fpm under **kiln-init `services`** (§4), not under a supervisor of potter's own.

- **Service layout:**
  - php-fpm is a service: `php-fpm --nodaemonize`, `ready = unix:/run/php-fpm.sock`, `stop_signal = QUIT`.
  - nginx is the main process: `nginx -g 'daemon off;'`, `stop_signal = QUIT`. It starts only once php-fpm's socket accepts connections.
- **Supervision:** if either exits, kiln-init stops the other and the VM ends. Restarting is the platform's job, and there is no restart loop inside the VM.
- **Logs:** both share the main process's stdout and stderr.
  - nginx's `access_log` and `error_log` go to `/dev/stdout` and `/dev/stderr`.
  - php-fpm's `error_log` goes to `/proc/self/fd/2`, with `catch_workers_output = yes` and `decorate_workers_output = no`.
- **Default nginx config:**
  - Generated by potter at build time into a host-built layer. kiln-init does no templating.
  - It listens on `[target.x.php] port` (default 8080), with `root /app/public` and `try_files $uri $uri/ /index.php?$query_string`.
  - `fastcgi_pass unix:/run/php-fpm.sock`, `client_max_body_size 100m`, gzip for text types, `server_tokens off`, and denied dotfiles except `.well-known`.
- **Default php-fpm pool:**
  - `listen = /run/php-fpm.sock` and `pm = dynamic`.
  - `pm.max_children = ${PHP_FPM_MAX_CHILDREN}`, using php-fpm's native env expansion. The image env defaults it to 16, and the platform overrides it per VM size.
  - `clear_env = no`, so platform-injected env reaches PHP, and `opcache.validate_timestamps = 0`.
- **Overrides:**
  - `[target.x.php] nginx_conf = "deploy/nginx.conf"` and `fpm_pool = "deploy/www.conf"` replace the defaults. The files are copied in as an inputs layer.
  - `[target.x.process.web]` replaces the whole command, in which case potter emits no services.
- **Non-web processes** (`worker`, `scheduler`) run their command as the main process, with no services.

### 6.2 `potter.toml`

```toml
[target.app]            # omit [target.*] for a single-target repo at the root
root = "."
provider = "laravel"    # force a provider; or "none" for a fully explicit plan

[target.app.runtime]
php = "8.4"
extensions = ["imagick"]          # added to the detected set
node = "22"

[target.app.steps.assets]         # override a detector step by name
run = "npm run build:prod"
inputs = ["resources/**", "vite.config.ts", "package.json"]

[target.app.process.web]
cmd = ["php", "artisan", "octane:start", "--server=swoole", "--host=0.0.0.0", "--port=8080"]

[target.api]
root = "services/api"             # monorepo target
provider = "go"

[apt]
build = ["libvips-dev"]
runtime = ["libvips42"]
```

- Every detector step has a stable name (`deps`, `source`, `assets`, `autoload`, `build`, …).
- `potter.toml` can override a step, remove it (`skip = true`) or insert a new step (`after = "deps"`).
- `provider = "none"` with `[target.x.steps.*]` and `[target.x.base]` gives a fully explicit plan. That is how any language not covered by a detector builds in v1.
- `potter plan --explain` shows, for each field, whether it came from a detector (with its evidence), from `potter.toml`, or from a default.

### 6.3 Dockerfile frontend

- **Parser:** our own. It covers:
  - the `# syntax=` directive (only the default syntax is accepted);
  - the escape directive, line continuations and heredocs (`<<EOF`, `<<-EOF`);
  - JSON and shell forms;
  - ARG/ENV expansion with `${var:-x}` and `${var:+x}` semantics, global ARGs before the first FROM, and `--platform`;
  - `.dockerignore`.
- **Lowering:**
  - Each stage becomes a chain in the target. A stage's FROM becomes `Base::Oci` (resolved to a digest at plan time) or `Base::Stage`.
  - `COPY <ctx>` becomes `Inputs`, with `--chown`/`--chmod`/`--link` honoured and `--exclude` supported.
  - `COPY --from` becomes `CopyFrom`.
  - `RUN` becomes `Run`. `--mount=type=cache` maps to cache mounts, `--mount=type=secret` to secrets, and `--mount=type=bind` (context or stage, read-only) to a step-scoped read-only input. `--network=none` maps to `network: false`.
  - ENV, WORKDIR, USER, LABEL, SHELL, EXPOSE, VOLUME, ENTRYPOINT, CMD, STOPSIGNAL and HEALTHCHECK become `Meta` or image config.
  - The final stage, or `--stage`, becomes the `image`, with export = the whole stage filesystem and run base = the stage's base. Dockerfiles ship what their final stage contains.
- **Cache semantics:** Dockerfile semantics. A `RUN` sees all prior state of its stage, so its key chains through every earlier step, as in BuildKit. Input precision comes only from how the Dockerfile orders its COPYs.
- **Refused (P10):**
  - `ADD <url>`, `ADD <git>`, `RUN --mount=type=ssh` and `ONBUILD` triggers in a base image's config. Each gets an error naming the line.
  - `ADD <local archive>` is supported and extracted on the host through the same bounded tar ingest.
- **Relationship to kiln:** potter is the only Dockerfile implementation. kiln spec rev 2.7's native RUN-free backend is cancelled, and `kiln build --builder potter` (the first choice for `auto`) delegates whole Dockerfiles to potter (§4).

---

## 7. Catalog

- **Contents (v1):**
  - `os-min` (Debian stable minimal plus ca-certificates and tzdata)
  - php 8.2/8.3/8.4 (cli + fpm), with ~20 extensions each as separate layers, plus swoole, RoadRunner and FrankenPHP for Octane
  - nginx (stable). Its default config is generated by potter (§6.1.1)
  - node 20/22/24
  - bun (current)
  - deno 2.x
  - python 3.12/3.13/3.14, with uv, poetry and pipenv as tool layers
  - ruby 3.3/3.4/4.0, with bundler
  - go 1.25/1.26/1.27 (build-only)
  - rust stable (build-only; the latest three toolchain versions, pinned: 1.97/1.98/1.99 at rev 1.5)
  - `static-server`
  - all on x86_64 and aarch64
- **Layer manifest annotations:**
  - `dev.potter.catalog.name`, `.version`, `.arch`
  - `.abi`, for example `os=debian-13 libc=glibc-2.41 php-api=20240924`
  - `.requires` (other catalog names and versions)
  - `.conflicts`
  - `.kind = base|runtime|extension|tool`
- **Index:**
  - A single OCI artifact `potter-catalog:index` listing every layer by name, version, arch and digest, with its annotations.
  - The index is signed with ed25519; the potter binary compiles in the public keys, and rotation is documented.
  - Verification fails closed (P2). Layers are fetched by digest and verified on pull, as in kiln T1.
- **Resolution:** constraints → the highest satisfying versions → a requires/conflicts check → an ABI check (every layer agrees on `os` and `libc`; PHP extensions match the `php-api`) → an ordered layer list.
- **Composite base:** the resolved list is merged into one erofs layer with kiln's merge. It is cached under `H("composite" ‖ ordered digests)` and is byte-reproducible. This saves virtio devices (§9.3) and overlay lowers.
- **Building the catalog:**
  - Recipes live in `catalog/` in the potter repo, as explicit `potter.toml` plans.
  - They're built by potter itself under a dedicated `catalog` tenant, with `VmExecutor`, on trusted CI.
  - CI rejects path overlaps between layers that could be co-installed, and runs a smoke test per layer (`php -m`, `node -e`, `python -c`, …).
- **Patch cadence:**
  - **Inputs:** catalog CI watches upstream release feeds (php.net, Node, Python, Ruby, Go and Rust releases, Bun, Deno, nginx) and the Debian security tracker for every package in `os-min` and in the runtime layers.
  - **Targets:** critical or actively exploited CVEs are fixed in a rebuilt layer and a published index within **24 h**; high severity within **7 days**; everything else in the weekly rebuild.
  - **Index:** new index entries carry `supersedes: <old digest>` and the advisory ids they fix. Superseded layers stay resolvable, so old images keep verifying.
  - **Finding affected images:** `potter catalog affected <advisory-id | layer-digest>` lists the catalog layers involved. The platform matches them against the `dev.potter.catalog.layers` annotation every image carries (§16) to find the images to rebase.

---

## 8. Cache

### 8.1 Keys

```
key(step) = SHA-256( "potter.cache/v1"
                     ‖ parent            # key of the previous step, or the base digest (composite / OCI / stage-final key)
                     ‖ canon(step)       # JCS of the step, labels and timeouts removed
                     ‖ inputs_digest     # for Inputs / CopyFrom: digest of the host-built layer; else empty
                     ‖ env_digest        # effective env after Meta / ARG expansion
                     ‖ secrets_hmac      # HMAC(tenant_key, id ‖ value) per §5.4, sorted by id
                     ‖ platform ‖ executor_class ‖ potter_major )
```

- Inputs and CopyFrom layers are built on the host first; their content digest *is* the input hash. Changing a file a step did not declare cannot change any key (success criterion 3).
- `executor_class` is `vm` or `ns` (§8.4).
- `potter_major` invalidates the cache when the step-execution semantics change, and only then.

### 8.2 Local cache

- `$POTTER_HOME/cache/<tenant>/index/<key>` points to a layer digest stored in `$KILN_HOME` blobs.
- Lookup finds the **longest hit prefix** of each target chain.
- Writes are atomic: temp file, then rename. An entry is recorded only after the layer is fully ingested and verified.

### 8.3 Remote cache

- **Layout:** each entry is an OCI artifact at `<registry>/<prefix>/<tenant>/cache:<key-hex>`, with artifactType `application/vnd.potter.cache.v1+json`.
  - Its config records the executor class, the potter version and the parent key.
  - Its single layer is the kiln erofs layer blob, with kiln's media type, so it dedupes with images in the same registry.
- **Pull:** potter checks the remote only for keys that miss locally, and fetches the longest remote hit prefix. Digests are verified on pull.
- **Push:** asynchronous after each step is sealed. A push failure produces a warning and never fails the build.
- **Configuration:** `--cache-from`/`--cache-to <registry>/<prefix>`, or `POTTER_REMOTE_CACHE`. Credentials come from kiln-registry's credential chain. The platform issues per-tenant credentials.

### 8.4 Executor trust ordering (P3)

- `executor_class` is part of the key, so `vm` and `ns` entries never collide.
- A `vm` build looks up `vm` keys only.
- An `ns` build may use `vm` entries: VM outputs are at least as trustworthy. It does this by trying the `vm`-class key when the `ns` key misses.
- The reverse lookup is never performed.

### 8.5 Cache mounts

- **VmExecutor:** each `(tenant, mount id)` is an ext4 image file, attached as virtio-blk and mounted at the mount target for the steps that declare it.
- **NsExecutor:** each mount is a host directory owned within the subuid range, bind-mounted.
- **Concurrency:** `exclusive` sharing takes an `flock` on the volume. A build that can't get the lock gets an empty scratch volume for that build instead: slower but correct, and never blocking.
- **Bounds:** each mount has a size cap (default 10 GiB). VmExecutor enforces it with the ext4 image size; NsExecutor checks usage after each step and resets mounts over the cap. `potter cache gc --mounts` evicts by LRU.
- **Isolation:** mounts are never in keys, layers or the remote cache, and never cross tenants (P1).

### 8.6 GC

`potter cache gc [--keep-bytes N] [--older-than D]` evicts index entries by LRU, then calls kiln's blob gc. Blobs referenced by kiln images are never removed.

---

## 9. Execution

### 9.1 `Executor` trait

```rust
trait Executor {
    fn class(&self) -> ExecutorClass;                 // Vm | Ns
    fn capabilities(&self) -> ExecCaps;               // max lowers, max mounts, network policy support
    async fn run_chain(&self, ctx: ChainCtx, steps: &[ExecStep],
                       sink: &mut dyn StepSink) -> Result<ChainOutcome>;
}
```

- `ChainCtx` carries the lower layers (base plus hit prefix), cache mounts, secrets, limits and the egress policy.
- `ExecStep` is either a `Run` to execute or a host-built layer (`Inputs`, `CopyFrom`) to stack. Host-built layers are produced before the chain starts.
- `StepSink` receives each step's log stream and its sealed diff, as a bounded reader. It returns the ingested layer digest, so the scheduler can record cache entries while the chain continues.
- A chain stops at the first failure.
- After a miss, the rest of the chain executes in the same sandbox, even if a later key would hit. Restarting on a mid-chain hit is a deferred optimisation.

### 9.2 Inputs layers (P6, P8)

- Built on the host from the target root:
  - include/exclude globs, plus the default excludes (`.git`, `node_modules`, `vendor`, `target`, `.venv`, `__pycache__`, `.potter`), plus `.potterignore` (or `.dockerignore` for Dockerfiles);
  - files are opened with `openat2(RESOLVE_BENEATH)`.
- **Symlinks** are kept as symlinks. A symlink is refused if its target resolves outside the root. It is never followed when reading.
- **Normalisation:** mtime is fixed (`SOURCE_DATE_EPOCH`, default 0), ownership comes from `owner`, permission bits become 0644/0755 by the exec bit (unless `--chmod` sets them), and xattrs are dropped.
- **Output:** a deterministic tar written straight through kiln-erofs, giving a byte-reproducible layer.
- **Visibility:** each step sees the base, the layers of earlier steps (including their inputs layers) and its own inputs layer. Source files that no step in its ancestry declared are absent from its filesystem. That is the enforcement.
- Inputs layers in `Run` steps that a VM executes in the middle of a chain are attached the same way as hit-prefix layers. NsExecutor stacks them as unpacked lower dirs.

### 9.3 `VmExecutor`

- **VM shape:** one VM per target chain, starting at the first miss.
  - Root is an overlay of the lowers: the composite base erofs, then each hit-prefix layer and pending inputs layer as a read-only erofs virtio-blk disk, then the init layer.
  - The upper is on an ext4 scratch disk.
  - There's an **output disk**: a raw sparse file sized to the build's output cap (default 8 GiB).
  - There are cache-mount disks, a vsock and a net device.
- **Device budget:**
  - The fixed devices are init, scratch, output, vsock and net.
  - The layer and mount disks must fit in `Capabilities.max_virtio_devices` minus the fixed devices. When they don't, the bottom hit-prefix layers are squashed into one erofs with kiln's squash, cached by their digests.
  - If a chain needs more inputs layers than the budget allows, the VM ends at that point and a new one boots from the extended prefix.
  - Cloud Hypervisor (31) is the recommended builder VMM; Firecracker works with squashing (17 on x86_64).
- **Protocol (kiln-proto, build mode):**
  - The host sends `Config{mode: Build, …}` with the layer and mount table and limits. The guest replies `Hello`, then `Ready`.
  - For each step, the host sends `RunStep{id, argv|shell, env, workdir, user, lowers_added, mounts, secrets, timeout_ms}`. `lowers_added` names the pending inputs-layer disks to stack before the step runs. Secrets are sent inline and written by kiln-init to a tmpfs that is never part of the upper.
  - The guest streams stdout and stderr using the existing stdio messages, then `Exited{code|signal}`.
  - On exit 0, the guest seals the step and sends `StepSealed{id, offset, len, sha256, entries}`.
- **Sealing, in kiln-init:**
  - It kills the step's process group and any stragglers, then unmounts the root overlay.
  - It converts the upper into an OCI tar diff: overlay char-0/0 whiteouts become `.wh.<name>`, and opaque directories (`trusted.overlay.opaque`) become `.wh..wh..opq`.
  - It appends the diff to the output disk at the next 4 KiB-aligned offset.
  - Then it remounts with the sealed upper as the topmost lower dir on scratch, plus a fresh upper. Mounts use per-layer `lowerdir+` or mount(2), never one long `lowerdir` string (see the kiln M1b-1 finding).
- **Host ingest (P4):**
  - The host ignores `sha256`, `len` and `entries` for trust purposes.
  - It reads `[offset, offset + len)`, checks the range lies within the disk and after the previous diff, hashes it itself, and streams it through `kiln-erofs` tarstream ingest, applying kiln's limits (bytes, entries, path depth, name length).
  - A mismatch with the guest's report fails the build as a protocol violation.
- **Network:** a vmkit net namespace and tap, with the egress policy of §9.5.
- **Teardown:** after the last step, or on any failure, the host sends `Shutdown`, kills the VM after a grace period, and deletes the scratch and output disks.

### 9.4 `NsExecutor`

- **Requirements:** Linux ≥ 5.11 (overlayfs in user namespaces), unprivileged user namespaces, `newuidmap`/`newgidmap`, a `/etc/subuid` and `/etc/subgid` range of at least 65 536, and `pasta`.
- **Lowers:**
  - erofs cannot be mounted inside a user namespace, so `NsExecutor` uses **unpacked layer directories** in `$POTTER_HOME/unpacked/<digest>/`.
  - They're produced from the erofs layers with the kiln-erofs reader and owned within the subuid range, and are reused across builds.
- **Built on vmkit:** the namespaces, seccomp, cgroups and egress below come from vmkit's generalised `sandbox` and tap-less `net` (§4). potter adds only the overlay, mounts and sealing. Hardening fixes land once and protect VMMs and build steps alike.
- **Sandbox layout** (from the outermost layer in):
  1. potter.
  2. Helper namespace `U1`: a userns mapped to the subuid range. It owns a new net namespace, the egress nftables policy and the pasta attachment.
  3. Step namespace `U2`: a nested userns with its own mount and PID namespaces, joined to U1's netns. U2 holds no capabilities over that netns, so the step cannot alter the policy (P7).
  4. Inside U2: an overlay mounted with `userxattr` (lowers, plus a fresh upper on host disk), cache mounts as bind mounts, and secrets on a tmpfs.
  5. Hardening: `no_new_privs`, seccomp (Podman's default profile, with `mount` and `unshare` denied once setup is complete), and cgroup v2 limits when delegation is available. Without delegation, potter warns and enforces wall-clock timeouts and the upper-dir size check only.
- **Sealing:**
  - The PID namespace is torn down first, so no process can race the read.
  - The host then walks the upper with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`, converts `user.overlay.*` whiteouts and opaque markers into an OCI tar stream, and feeds it through the same bounded ingest as §9.3 (P4).
  - The sealed upper becomes the next step's top lower.
- **Deployment control:**
  - `POTTER_EXECUTORS=vm` (or the config file) disables `NsExecutor` entirely. Multi-tenant platforms set this.
  - `NsExecutor` builds always produce `ns`-class cache entries (§8.4).

### 9.5 Network policy (P7)

The policy is the same for both executors, as kiln's T5 table.

- **Allowed:** egress to public unicast addresses.
- **Denied:** link-local (including `169.254.169.254` and `fd00:ec2::254`), RFC 1918/6598, loopback-mapped, host addresses, and other builds.
- IPv6 is dropped in v1, as in kiln.
- DNS goes through pasta's resolver forwarding.
- `network: false` (from `RUN --network=none` or `[target.x.steps.y] network = false`) attaches no network at all.

### 9.6 Image assembly

Image assembly runs entirely on the host:

1. **Export:**
   - The build chain's layers are read as a merged view (kiln-erofs reader plus merge, honouring whiteouts).
   - The `Export` paths and excludes are applied.
   - The result becomes one deterministic layer, written with normalised metadata. It is cached under `H("export" ‖ chain-final key ‖ canon(exports))`.
2. **`CopyFrom`:** the same mechanism, applied to a stage's or target's final chain state.
3. **Image:** the run base (catalog composite or OCI layers), then the export layer, then the kiln init layer. The kiln config is built from `ImageConfig` and `processes`, and committed to `$KILN_HOME` through kiln-image's commit path.
4. **Output:**
   - `--output kiln` (the default; tagged `potter/<target>:<git-short-sha>` and `:latest`)
   - `--output oci-layout=<dir>`
   - `--output registry=<ref>` (push through kiln)

---

## 10. Graph and monorepos

- **Target discovery:** explicit `[target.*]` entries in `potter.toml` win. Otherwise potter uses:
  - workspace manifests: npm/pnpm/yarn/bun workspaces, Cargo workspaces (one target per binary), Go multi-module and `cmd/*`, uv workspaces;
  - deployable roots (dirs with a provider match and a process).
  - With no workspace manifest, there is a single target at the root.
- **Deduplication:** identical step keys across targets are executed once. The scheduler holds an in-flight map from key to a shared future, so a second target waits instead of recomputing.
- **Scheduling:** targets run in parallel up to `--jobs` (default: host CPUs / 2 for `vm`; CPUs for `ns`). Each VM gets `--step-cpus` (default 2) and `--step-mem` (default 4 GiB).
- **`--affected <git-ref>`:**
  - Computes each target's inputs-layer digests at HEAD and at `<ref>`, using a read-only `git` object read via `gix` with no checkout.
  - A target is affected if any inputs digest, its plan hash, or its base resolution differs. Only affected targets build.
  - The rest are reported as `unchanged`, with their last image ref if known.

---

## 11. CLI

```
potter plan    [PATH] [--target T] [--json] [--explain]
potter detect  [PATH]                                   # providers, specificity, evidence
potter build   [PATH] [--target T]... [-f Dockerfile] [--stage S] [--executor vm|ns]
               [--tenant ID] [--cache-from REF] [--cache-to REF] [--no-cache]
               [--secret id=X,src=FILE]... [--build-arg K=V]... [--affected GITREF]
               [--output kiln|oci-layout=DIR|registry=REF] [--jobs N] [--progress auto|plain|json]
potter cache   ls|gc|rm [--tenant ID] [--mounts]
potter catalog ls|resolve <constraints>|verify|affected <advisory|digest>
potter rebase  <image-ref> [--catalog-index DIGEST] [--dry-run] [--output …]
potter sbom    <image-ref>                       # print the attached SBOM
potter verify  <image-ref> [--key …]             # signature + provenance + SBOM digest check
potter bench   [PATH]
```

- **Progress:** a TTY view per target and step. `--progress json` emits newline-delimited events (step start, log, cache hit, sealed, pushed, failed, hostile), which a daemon or a CI integration can consume.
- **Exit codes:** 0 on success; 1 when a build step failed; 2 for a usage, plan or detection error; 3 for a sandbox or protocol violation (the hostile-guest class); 4 for an infrastructure error (registry, KVM, missing subuid).
- **Strings:** every string from the repo, the guest or a registry that potter prints in diagnostics is sanitised, as in kiln T8. Step logs pass through unmodified, as `docker build` does.

---

## 12. Failure handling

| Failure | Behaviour |
|---|---|
| A step exits non-zero | The build fails with exit 1, the step name, the exit code and the last 200 log lines. Earlier sealed steps keep their cache entries; nothing is recorded for the failed step. The sandbox is torn down. |
| Step timeout (default 30 min) or build timeout (default 2 h) | The sandbox is killed and the build reports a timeout. Same caching rule as above. |
| Malformed or oversized diff, digest mismatch, out-of-range offset, unexpected message, protocol violation | Exit 3, error class `hostile`. Nothing from that step is cached, and an audit event is emitted in the JSON progress stream. |
| Output disk, scratch or cache mount full | The step fails with a clear "build output exceeded N GiB" error, distinct from an app failure. |
| Remote cache pull fails or doesn't verify | Logged; the step executes locally. |
| Remote cache push fails | A warning only. |
| Ambiguous detection, unsatisfiable version constraint, or no provider match | Exit 2, with the candidates, the evidence and the `potter.toml` key that resolves it. potter never guesses silently. |
| Catalog signature, ABI or conflict failure | Exit 2 before any sandbox starts. |
| `NsExecutor` prerequisites missing | Exit 4, naming the missing piece (subuid range, `newuidmap`, kernel version, userns sysctl). |
| KVM is unavailable and `--executor` is unset | Exit 4. potter never falls back to `ns` implicitly, because the user must choose the weaker isolation. |

---

## 13. Formats documented for third parties

`docs/plan-format.md` documents:

- the plan IR JSON schema and JCS hashing;
- `potter.toml`;
- the cache key derivation;
- the cache artifact layout;
- the catalog index and its annotations;
- the progress event schema.

The `kiln` image format and kiln-proto are referenced from kiln's `docs/format.md`. potter's build-mode proto additions are documented there, in kiln's repo.

---

## 14. Testing

### 14.1 Fixtures

Fixtures are vendored into `fixtures/`, kept small, and seeded from the existing playground projects, plus new ones where a language or framework is missing:

| Language | Fixtures |
|---|---|
| PHP | laravel (from `laravel-dev-starter`), laravel-octane, laravel-inertia, drupal (`drupal-hello-world`), plain composer app |
| JS/TS (Node) | nestjs (`nestjs-hello-world`), nuxt (`nuxt-example`), next standalone, hono-node (`hono`), vite SPA (`vue-hello-world`, static) |
| Bun | `bun-test` |
| Deno | new: deno-serve, fresh |
| Python | django (`django-hello-world`), fastapi (`fastapi-hello-world`), flask (`flask-hello-world`), one each with uv, poetry and pip |
| Ruby | rails (`rails-cloud-app`), sinatra |
| Go | new: net/http, plus a `cmd/*` multi-binary repo |
| Rust | axum (`axum-hello-world`), a Cargo workspace with two bins |
| Monorepo | `nextjs-go-monorepo` |
| Ambiguous | a repo with competing root providers. Detection must refuse |
| Dockerfile | a multi-stage Go build, a Laravel Dockerfile with `--mount=type=cache`, a heredoc-heavy file, one with secrets, and the Dockerfiles already present in the fixtures above |

### 14.2 Unit and property tests

- **Detector snapshot tests** (`insta`): the canonical plan plus the evidence, per fixture.
- **Constraint parsers:** property tests against the reference semantics of each ecosystem's resolver, using upstream test vectors where they exist.
- **Cache keys:**
  - Mutating any file outside a step's declared inputs never changes its key.
  - Mutating a declared input always does.
  - Reordering keys or whitespace in `potter.toml` never does.
- **Dockerfile parser:** the upstream BuildKit dockerfile-parser test corpus (Apache-2.0), plus `cargo-fuzz` on the lexer and parser.
- **Inputs layers:** byte-identical output across runs and hosts, symlink-escape refusal, `.potterignore` semantics.

### 14.3 Integration and E2E (KVM CI, both arches)

- **Per fixture:** `potter build` with both executors, then `kiln run` on both VMMs and the fixture's check.
- **Dockerfile oracle:** for each Dockerfile fixture, build with `docker buildx --output type=oci` and with potter, then compare the exported filesystems with kiln's compare tooling. Tolerated differences: mtimes, plus the ownership normalisation documented in §9.2 for COPY layers only.
- **Cache behaviour:** the scenarios in success criterion 3, asserted on executed-step counts from `--progress json`.
- **Remote cache:** a build on builder A, then a fresh `$POTTER_HOME` on builder B, sharing a registry (`zot` in CI), gives all hits.

### 14.4 Threat-property tests

| Property | Test |
|---|---|
| P1 | Tenant B's keys and mounts are invisible from tenant A's build. Registry credentials scoped to A can't read B's path (zot with per-repo auth). |
| P2 | A tampered index, a wrong key, or a layer digest mismatch fails resolution before any sandbox starts. |
| P3 | Seed an `ns` entry with a poisoned layer; a `vm` build must not use it. |
| P4 | A kiln-testguest build-mode variant sends oversized, lying, overlapping and malformed diffs, and `..`/absolute paths in tar entries. Every case gives exit 3 and no cache entry. Diff ingest is fuzzed with `cargo-fuzz`. |
| P5 | A step that cats a secret into a file and into its logs: the layer doesn't contain it, logs are redacted, and the cache-key material doesn't contain it. |
| P6 | A step that reads an undeclared file fails with ENOENT. |
| P7 | From a step: curl the metadata IP, an RFC 1918 address and the host gateway, and try to flush nft rules. Each must fail, for both executors. |
| P8 | A repo symlink to `/etc/passwd` or `../..` is refused. |
| P9 | A repo whose `composer.json` has a `post-install-cmd` that writes a marker file: `potter plan` must not create the marker. |
| P10 | `ADD http://…`, `ADD git@…` and `--mount=type=ssh` are refused at plan time. |
| P11 | A step that fills the output disk, fork-bombs, or sleeps past its timeout is contained and reported. |
| P12 | An image that mixes `ns` and `vm` layers gets `ns` provenance. A rebase leaves the app-layer digests unchanged and records the replaced base. A rebase across an ABI change, or with a broken `DT_NEEDED` soname, is refused. `potter verify` rejects a tampered SBOM or provenance. |

### 14.5 Bench

`potter bench` measures, on the Laravel fixture:

- an all-hits rebuild;
- a source-only change;
- a cold build with warm catalog layers;
- per-step overhead (boot, seal, ingest).

It reports against the targets in success criterion 4.

---

## 15. Milestones

Each milestone gets its own implementation plan.

| # | Milestone | Content | Runs on | Depends on |
|---|---|---|---|---|
| P1a | Plan core | `potter-plan`, `potter-config`, `potter plan/explain/detect`; PHP (generic + Laravel), Node, Bun, Deno and Static providers; constraint parsers for composer and npm | macOS + Linux | — |
| P1b | Remaining detectors | Python, Ruby, Go and Rust providers and their constraint parsers | macOS + Linux | P1a |
| P2 | First images | `potter-exec` with `NsExecutor`, inputs layers, `Base::Oci` bases (no catalog yet), host-side export, cache mounts, local cache, `potter build` → kiln image | Linux, no KVM | P1a, kiln M1b (convert/store), kiln `services` PR, vmkit generalised `sandbox`/`net` |
| P3 | Catalog | Catalog format, signing, resolver, composite bases, `Apt` steps and `runtime_steps`, catalog recipes and CI for every runtime in §7, patch cadence and `catalog affected`, rebase (§16); detectors switch to catalog bases | Linux + KVM CI | P2 |
| P4 | VM executor | kiln PRs (proto build mode, kiln-init build mode), `VmExecutor`, output-disk ingest, device budgeting, trust ordering | Linux + KVM | P2, kiln M3 run |
| P5 | Dockerfile frontend | `potter-dockerfile`, lowering, the buildx oracle, `kiln build --builder potter` | Linux | P2 (P4 for VM runs) |
| P6 | Remote cache, graph and supply chain | Remote cache, tenant keys, monorepo discovery, dedup, `--affected`, `--progress json`, SBOM/provenance/signing and `potter verify` (§17), build Jobs (§18) | Linux | P2 |
| P7 | Daemon | gRPC API, build queue, warm VM pool, concurrency and tenant quotas, wrapping the library | Linux + KVM | P4, P6 |

P2 deliberately comes before the catalog and the VM executor, so real images come out early on a host without KVM. Detectors emit catalog refs from P1a. Until P3 ships the catalog, P2 maps each ref to an upstream OCI image (`php:8.4-fpm`, `node:22-slim`, …). For PHP web targets, P2's mapping turns the `nginx` ref into an `apt-get install nginx` step on top of `php:X-fpm`, and P3 replaces it with the catalog nginx layer.

---

## 16. Rebase

Catalog layers sit below the app's layers, so a runtime security fix can be applied to an existing image without rebuilding it from source, as Cloud Native Buildpacks' `rebase` does.

- **Image metadata.** Every catalog-based image records:
  - `dev.potter.catalog.layers`: the ordered catalog layer digests behind its run-base composite;
  - `dev.potter.catalog.constraints`: the resolved constraints, for example `php@8.4`, `ext-redis`, `nginx`;
  - `dev.potter.catalog.index`: the index digest;
  - `dev.potter.chain`: the digests of the `runtime_steps` layers, the export layer and the init layer.
- **`potter rebase <image>`:**
  1. Re-resolve the recorded constraints against the current (or given) index, staying on the same **minor** version: php 8.4.x → 8.4.y, never 8.5.
  2. Require **identical `.abi` annotations** on every replaced layer (os, libc, php-api and so on). Otherwise the image is refused with "needs rebuild" and the reason.
  3. Build the new composite, re-run `runtime_steps` on top of it (cached, and usually cheap), then stack the **unchanged** export layer and the current pinned init layer.
  4. Write a new image and a new provenance record (§17) naming the base it replaced. App layers are byte-identical.
- **Never rebased** (they need a rebuild):
  - images on `Base::Oci` bases, including Dockerfile builds;
  - images whose ABI changed;
  - images whose export contains native code linked against a replaced library outside the ABI contract. This is detected by recording the `DT_NEEDED` sonames of ELF files in the export layer at build time and checking them against the new layers' provided sonames.
- **Platform flow:**
  1. `potter catalog affected <advisory>` gives the layers involved.
  2. The platform queries its registry for the images whose annotations match.
  3. `potter rebase` each one, then roll out through the normal deploy path, wave by wave.

## 17. Supply chain

Every image potter outputs gets three OCI referrer artifacts, attached through the OCI referrers API, with a tag-schema fallback for registries without it.

- **SBOM:** SPDX 2.3 JSON. It is assembled from data potter already has, without scanning:
  - the catalog layer metadata (name, version, upstream source, licence);
  - `Apt` package lists (Debian package and version from the pinned snapshot);
  - the lockfiles inside the declared inputs: `composer.lock`, `package-lock.json`/`pnpm-lock.yaml`/`yarn.lock`/`bun.lock`, `deno.lock`, `uv.lock`/`poetry.lock`/`requirements.txt` (pinned only), `Gemfile.lock`, `go.sum` together with the build list, and `Cargo.lock`.
  - Components with no lockfile evidence are listed as `NOASSERTION`, never guessed.
- **Provenance:** an in-toto statement with a SLSA v1 provenance predicate. It records:
  - the builder id and potter version;
  - the canonical plan hash (§5.2) and the source revision and tree digest, when available;
  - the catalog index digest;
  - the per-step cache keys, and the executor class per layer (P12);
  - for rebased images, the replaced base.
- **Signature:** the image manifest, the SBOM and the provenance are signed in the cosign/sigstore bundle format.
  - v1 uses a **platform-held key**: a KMS reference passed with `--sign-key kms://…` or a local key for development. Keyless signing is deferred.
  - `potter verify` checks all three, and the platform's deploy step verifies before rollout.
- **Inputs to the attestation are trusted only as far as they deserve:** step-reported data never enters it. The SBOM comes from host-side inputs and the catalog only.

## 18. Platform integration

- **Build and deploy commands.** Hosting platforms commonly let an environment declare "build commands" and "deploy commands".
  - **Build commands** are delivered as `[target.x.build] commands = [...]` (from the platform or `potter.toml`). Each becomes a `Run` step named `user-build-<n>`, after the provider's build steps and before export. Their inputs are the full source layer, which is already present.
  - **Deploy commands** (migrations, cache warmups) are not build steps. potter emits them as a `deploy` process: `processes.deploy = ["sh", "-c", "<commands joined with &&>"]`. The platform runs the image once with that process, as a one-off VM, before rolling out `web`/`worker`. No exec is needed.
  - `potter plan --explain` shows where each command landed.
- **Before the daemon (P7):**
  - Builds can run as Kubernetes Jobs on KVM build nodes.
  - The Job runs `potter build --progress json --output registry=<ref> --cache-from/--cache-to <tenant cache>`. The tenant key and registry credentials come from mounted Secrets, and build secrets from `--secret` files.
  - The platform reads the newline-delimited JSON events from the Job's log stream and shows step progress and logs to the user. Build logs are build output, separate from the app's runtime logs.
- **Runtime contract:**
  - **Port:** the listen port is fixed at build time (`[target.x.php] port` or the provider default, 8080) and recorded in the `dev.potter.port` annotation. nginx can't read env vars, so the platform routes to the recorded port instead of injecting `PORT`. Non-PHP providers also honour `PORT` at runtime where the framework does.
  - **Env:** the platform injects `PHP_FPM_MAX_CHILDREN` (set per VM size) and the app's own env.
  - **Processes:** the platform starts `web`, `worker`, `scheduler` and `deploy` from the process annotations (§5.1).
