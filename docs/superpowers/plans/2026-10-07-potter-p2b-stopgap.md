# potter P2b: building detected targets (stopgap bases) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** `potter build --executor ns` builds detected targets, whose bases are catalog refs. Until P3's catalog exists, each catalog base is lowered onto an upstream Debian trixie image plus install steps (spec §15: "P2 maps each ref to an upstream OCI image").

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md` §7 (catalog refs), §9.6 (assembly), §15 (P2's mapping).

## Global Constraints
- P2a's constraints still hold (see `2026-10-06-potter-p2a-engine.md`).
- **Image tags** (verified on Docker Hub, 2026-10-07):

  | Catalog layer | Image |
  |---|---|
  | `os-min` | `debian:trixie-slim` |
  | `php` (with `php-fpm`) | `php:<v>-fpm-trixie` |
  | `php` (without) | `php:<v>-cli-trixie` |
  | `node` | `node:<v>-trixie-slim` |
  | `bun` | `oven/bun:1-slim` |
  | `deno` | `denoland/deno:debian` (no Deno 2 tag exists) |
  | `python` | `python:<v>-slim-trixie` |
  | `ruby` | `ruby:<v>-slim-trixie` |
  | `go` | `golang:<v>-trixie` |
  | `rust` | `rust:<v>-slim-trixie` |

- **Install steps** are `Run` steps named `catalog-*`, run as root in `/`, with network:
  - `php-ext-*` use mlocati's `install-php-extensions` (built-in extensions skipped);
  - `composer` comes from getcomposer.org `latest-2.x`;
  - a secondary `node` comes from NodeSource `setup_<v>.x`;
  - `nginx` comes from apt;
  - `static-server` is a `busybox httpd` shim.
- **`apt` steps** are lowered to `Run` steps doing `apt-get install`, with `POTTER_APT_SNAPSHOT=<snapshot>` in their env so the snapshot stays in the key. They are not pinned: P3 pins them.
- **Still refused:** `services` (kiln M3d), `copy_from`, secrets, `Base::Stage`.

## Tasks
1. **`stopgap.rs`:**
   - `lower(layers: &[CatalogRef]) -> Result<Lowered { reference, steps }, BuildError>`.
   - `lower_step(step) -> Step`, which turns an `Apt` step into a `Run` step.
   - Unit tests: the table above, built-in extension skipping, unknown layers and two primary runtimes refused (exit 2), apt lowering.
2. **Wiring:**
   - `run_chain` = `check_supported` + `run_steps(ctx, name, &Chain, root)`. The base is lowered, its steps are prepended, and `apt` steps are lowered.
   - Assembly runs the run base as a chain (`image.base` + `runtime_steps`). Its layers and env make up the image's base.
   - `check_supported` stops refusing catalog bases, `apt` steps and `runtime_steps`.
   - Unit tests updated. The Linux tests stay green.
3. **Real fixtures** (Lima, `POTTER_TEST_NETWORK=1`): `potter build --executor ns` on `go-api` and `vite-static` gives a kiln image whose entrypoint is the plan's web process.
