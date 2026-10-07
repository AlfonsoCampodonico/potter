# potter: session handoff

**State (2026-10-05):**
- The design spec is at `docs/superpowers/specs/2026-10-04-potter-design.md`, rev 1.3. **The user approved it on 2026-10-05.** Rev 1.3 added rebase (§16), supply chain (§17), platform integration (§18) and the catalog patch cadence.
- **kiln-shim draft:** it belongs in the **kiln** repo; don't edit kiln from here.
  - Decisions D1–D4 were confirmed by the user on 2026-10-05: nested virtualization on C8i first; one VM per pod with one container; a 1 GiB default; CNI plus a fixed deny list.
- `docs/superpowers/ROADMAP.md` is the proposed replacement for `playground/ROADMAP.md` (the three-repo consolidation). It still needs copying over: `cp docs/superpowers/ROADMAP.md ../ROADMAP.md`, run from the potter repo root.

**P1a is implemented** (2026-10-06) on branch `p1a-plan-core`:
- Plan: `docs/superpowers/plans/2026-10-05-potter-p1a-plan-core.md`. Spec rev 1.4.
- Four crates: potter-plan, potter-detect, potter-config and the potter CLI.
- 167 tests, clippy clean. A fresh whole-branch review found 1 Critical and 3 Important issues, all fixed with regression tests.

**Next steps, in order:**
1. ~~The user reviews the spec.~~ Done; approved.
2. ~~P1a plan and implementation.~~ Done. Merging `p1a-plan-core` is the user's call.
3. ~~P1b~~ Done (2026-10-06):
   - the Python, Ruby, Go and Rust detectors;
   - `export` for detected targets;
   - zero-config monorepo targets (spec rev 1.6).
   - Plan: `docs/superpowers/plans/2026-10-06-potter-p1b-detectors.md`.
4. Next: **P2**, first images via `NsExecutor`. It needs the kiln `services` PR and vmkit's generalised sandbox (see the upstream table below).

**Deferred minor findings from the P1a review** (fix in P1b/P2):
- yarn.lock flavour detection swallows read errors and over-cap files, and should read a bounded prefix.
- Some user errors exit 4 instead of 2 (a file given as PATH, an unreadable subdirectory).
- Top-level JSON that isn't an object (composer.json, package.json) should be refused, naming the file.
- Composer exact and `<=X.Y` constraints resolve to whole lines; P3 must carry the VersionSet so it picks a satisfying patch.
- The NoProvider hint can suggest invalid TOML for odd directory names.
- The Static out-dir error points at the wrong key.
- `port` vs `php.port` conflicts are silent; explicit plans ignore `php.port`, `runtime`, `php` and `node` keys.
- Invalid names in `runtime.extensions` are dropped silently.
- `--json` is not terminal-safe; a bun `module`/`main` starting with `-` becomes a flag.
- A PHP docroot of `.` exposes composer files and `vendor/`; add nginx denies.
- The composer parser accepts two operators in a row.
- JS deps inputs omit pnpm `patches/`, `.yarn/patches` and workspace manifests. **P2 must add these.**
- There is no provider-level test with a composer.lock over 4 MiB.
- Replacing Laravel's web process still ships the nginx and php-fpm layers and their configs.
- **P2 must re-check `Target.root` for symlinks** and must not trust the plan.

**Upstream follow-ups this design creates.** Raise each in the owning repo's session. Don't edit kiln or vmkit from a potter session.

| Repo | Change | Needed by |
|---|---|---|
| kiln | kiln-proto + kiln-init `services` (spec §4) | potter P2 (PHP web images) |
| kiln | Spec: cancel the rev 2.7 native RUN-free Dockerfile backend. M3c becomes backend dispatch, with `--builder auto` trying potter, then BuildKit, then Podman/Buildah. Update the `kiln-build-backends` memory | Before M3c planning |
| kiln | kiln-proto `Config.mode = Build`, the build-mode messages, kiln-init build mode (spec §9.3) | potter P4 |
| vmkit | A generalised `sandbox` that runs any process, with nested userns; tap-less `net` for a bare netns (spec §4, §9.4) | potter P2 |
| vmkit | Inbound port publishing in `net` (`pasta -t`) | Standalone `kiln run` (not used under the shim) |
| kiln | kiln-proto exec (`ExecStart`/`ExecExited`, `proc` on stdio, signal and window messages), `Stats`/`OomKill`, `FileUpdate` | kiln-shim S1 |
| kiln | Layer media type and erofs profile accepted by containerd 2.3's native EROFS layers | kiln-shim S2 |
| vmkit | `supports_memory_hotplug` capability (virtio-mem on Cloud Hypervisor), vCPU hotplug | kiln-shim S4 |

**Decisions taken with the user** (don't re-ask):
- Input model: a plan IR, with detectors, `potter.toml` and Dockerfiles as frontends.
- All languages are in v1: PHP/Laravel, Node, Bun, Deno, Python, Ruby, Go, Rust. So are the Dockerfile frontend and the rootless namespace executor.
- Default executor: one microVM per target chain. `NsExecutor` is built on vmkit's sandbox, and the trust ordering means VM builds never consume ns cache entries.
- Turborepo-like features in v1: enforced input-precise hashing, a remote OCI cache scoped per tenant, monorepo/multi-target builds, persistent cache mounts.
- Runtimes: a signed catalog of composable erofs layers on Debian stable minimal.
- PHP web runtime: nginx + php-fpm as kiln-init `services`. There is no FrankenPHP default and no potter-run supervisor.
- Process shape: a library plus a CLI; the daemon is milestone P7.
- A separate repo, depending on kiln crates and vmkit pinned by git rev.
- Names stay potter, kiln and vmkit. Pottery codenames are used for internal pieces only: greenware (plan IR), glaze (catalog), wheel (daemon), sagger, sherd, raku and slip (in kiln and vmkit).
- Consolidation: three repos only. The separate sandbox, snapshot-registry, host-agent, ingress and log-shipper projects are dropped (see ROADMAP).

**Related context:**
- The kiln spec is in `../kiln/docs/superpowers/specs/2026-09-30-kiln-design.md`. Rev 2.7 is on branch `docs/m3a-plan-spec-2.6`.
- kiln M3a is in progress on branch `m3a-guest`.
- vmkit is in `../vmkit`.
- Commits carry no attribution trailers (a user preference).

**Deferred minor findings from the P1b review** (spec rev 1.6 work):
- A Go root `package main` plus `cmd/app` collide on `/out/app`. Go and Rust names starting with `.` or `-` pass `safe_name`.
- Main or binary names that fail `is_name` are dropped from processes without a decision.
- Python `-e .` and local-path requirement lines install before the source is copied. The glued form `-rbase.txt` is not followed. A `-r ../base.txt` that stays inside the root is still refused.
- Django with `manage.py` outside the target root gets an error that doesn't name `manage.py`. Pipfile `[requires] python_version` is not consulted.
- A Gemfile with a multi-argument `ruby ">= 3.2", "< 4"` reads only the first argument. `.ruby-version` `ruby-3.3.6@gemset` is an error.
- TOML parse errors echo the whole offending line; truncate `reason`.
- `export` accepts `/` and `./data`; `/app/../etc` fails later as an anonymous plan error. Normalise, and reject at the key.
- Discovery can overlap `apps` with `apps/web` when `apps/` has its own manifest.
- **P3:** exact patch pins (`ruby "3.3.6"`, PEP 440 `==3.12`, `.python-version` `3.12.4` with `UV_PYTHON_DOWNLOADS=never`) need catalog patch resolution. `rust-toolchain.toml` components and profile vs rustup in the catalog layer.

**P2a (2026-10-06):** the build engine, on branch `p2-first-images`. Plan: `docs/superpowers/plans/2026-10-06-potter-p2a-engine.md`.
- `potter build --executor ns` builds plans with OCI bases into `$KILN_HOME`.
- It needs vmkit's generalised sandbox: vmkit PR #6, branch `sandbox-exec`, pinned at dbea230. Repin to the merge commit once merged.
- Next is **P2b**: the stopgap catalog→upstream image mapping for detected targets (PHP extensions, composer, node, nginx), plus `services` once kiln-init starts them.

**From the kiln session (2026-10-06), for P2b and later:**
- **Omega minimal-API images:**
  - The app reads `SERVER_HOST`/`SERVER_PORT` (default 127.0.0.1:8000), not `PORT`. The Laravel/Omega detector should set `SERVER_HOST=0.0.0.0` and `SERVER_PORT=<the plan's port>`.
  - Omega's trusted-hosts check returns 400 unless the Host matches `APP_URL`. A hosting platform sets it; for local `kiln run`, pass `-e APP_URL=...`.
  - Omega's `/up` skips global middleware and stays 200 while real routes fail. Health checks should hit a real route.
- **kiln M3b-2:** adds `kiln run --net -p [IP:]HOST:GUEST` (default bind 127.0.0.1). Without a network, resolv.conf gets `nameserver 127.0.0.1`.
- **kiln spec rev 2.8:** the guest sends `Hello` first. potter §9.3 build mode must follow that order (fix in P4).
- **kiln M3c:** owns `kiln build --builder potter` dispatch. potter P5 supplies the Dockerfile frontend.

**P2a final review (2026-10-06):** 2 Critical and 7 Important findings, all fixed with regression tests. The Critical ones were seal hardlink order and link-header metadata. The Important ones included export hardlinks, kiln ingest before caching, exclusive cache mounts, stale-run sweeping and unpack caps. CI is green on ubuntu, macOS and the sandbox job.

Deferred:
- The 2 h build timeout.
- Bounding a step's upper while it runs.
- Cache-mount size caps.
- The merge's in-memory entry map.
- `SOURCE_DATE_EPOCH` doesn't reach `__seal`.
- A full miss and a partial hit present different lowers: the sealed upper vs the unpacked layer.
- `--tag` is checked only after the build.
- Export isn't cached (§9.6 key).
- The default tag lacks `:<git-short-sha>`.
- A concurrent `tenant_key` can be read half-written (unused).
- Named-user lookups re-read every layer.
- A step gets no supplementary groups.

**2026-10-07:** vmkit PR #6 is merged. potter pins vmkit `main` at c4363a2: #6 plus #7, which only changes the kernels workflow. P2a merges to potter main.
