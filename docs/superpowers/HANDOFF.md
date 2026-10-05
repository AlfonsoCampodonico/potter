# potter: session handoff

**State (2026-10-05):**
- The design spec is at `docs/superpowers/specs/2026-10-04-potter-design.md`, rev 1.2. The user has not yet reviewed the written spec.
- `docs/superpowers/ROADMAP.md` is the proposed replacement for `playground/ROADMAP.md` (the three-repo consolidation). It still needs copying over: `cp docs/superpowers/ROADMAP.md ../ROADMAP.md`, run from the potter repo root.

**Next steps, in order:**
1. The user reviews the spec. Apply their changes and bump the revision history.
2. Once the spec is approved, run `superpowers:writing-plans` for milestone **P1a** (plan core plus the PHP/Laravel, Node, Bun, Deno and Static detectors). Each milestone in §15 gets its own plan.

**Upstream follow-ups this design creates.** Raise each in the owning repo's session. Don't edit kiln or vmkit from a potter session.

| Repo | Change | Needed by |
|---|---|---|
| kiln | kiln-proto + kiln-init `services` (spec §4) | potter P2 (PHP web images) |
| kiln | Spec: cancel the rev 2.7 native RUN-free Dockerfile backend. M3c becomes backend dispatch, with `--builder auto` trying potter, then BuildKit, then Podman/Buildah. Update the `kiln-build-backends` memory | Before M3c planning |
| kiln | kiln-proto `Config.mode = Build`, the build-mode messages, kiln-init build mode (spec §9.3) | potter P4 |
| vmkit | A generalised `sandbox` that runs any process, with nested userns; tap-less `net` for a bare netns (spec §4, §9.4) | potter P2 |
| vmkit | Inbound port publishing in `net` (`pasta -t`) | kiln-shim, the deploy path |

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
