# potter: session handoff

**State (2026-10-04):**
- The design spec is drafted at `docs/superpowers/specs/2026-10-04-potter-design.md` (rev 1). The user has not yet reviewed the written spec.
- Brainstorming is complete. Every section (§1–§4 in conversation) was approved, with these scope changes from the user:
  - all languages in v1: PHP, JS, TS (Node, Bun and Deno), Python, Ruby, Go, Rust;
  - the Dockerfile frontend is in v1;
  - the rootless namespace executor is in v1.

**Next steps, in order:**
1. The user reviews the spec. Apply their changes and bump the revision history.
2. Once the spec is approved, run `superpowers:writing-plans` for milestone **P1a** (plan core plus the PHP/Laravel, Node, Bun, Deno and Static detectors). Each milestone in §15 gets its own plan.

**Decisions taken with the user** (don't re-ask):
- Input model: a plan IR, with detectors, `potter.toml` and Dockerfiles as frontends.
- Default executor: one microVM per target chain (vmkit + kiln-init build mode). `NsExecutor` is a second backend, and the trust ordering means VM builds never consume ns cache entries.
- Turborepo-like features in v1: input-precise hashing (enforced), a remote cache in an OCI registry, monorepo/multi-target builds, persistent cache mounts.
- Runtimes: a signed catalog of composable erofs layers on Debian stable minimal.
- Process shape: a library plus a CLI in v1; the daemon is milestone P7.
- The step cache is scoped per tenant. Only signed catalog layers are shared across tenants.
- A separate repo, depending on kiln crates and vmkit pinned by git rev.
- Export and `COPY --from` are computed on the host from erofs layers. The VM's diff transport is a raw output disk.

**Related context:**
- The kiln spec is in `../kiln/docs/superpowers/specs/2026-09-30-kiln-design.md`. Rev 2.7 is on branch `docs/m3a-plan-spec-2.6`; it adds pluggable `kiln build` backends and a RUN-free native backend. potter supersedes kiln's post-v1 "RUN in microVM" idea.
- vmkit is in `../vmkit`.
- kiln M3a (kiln-init) is in progress on branch `m3a-guest`. potter P4 depends on kiln M3 and on new kiln-proto/kiln-init build-mode PRs.
- Commits carry no attribution trailers (a user preference).
