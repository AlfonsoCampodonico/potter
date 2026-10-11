# potter

Build [kiln](../kiln) microVM images from source. potter detects PHP/Laravel, Node, Bun, Deno, static-site,
Python, Ruby, Go and Rust projects, or reads an explicit `potter.toml`, and produces a canonical build plan.
`potter build` runs the plan and writes kiln images to `$KILN_HOME`, caching every step by its inputs.

    potter plan [PATH] [--target T] [--json | --explain]
    potter detect [PATH]
    potter build [PATH] --executor ns [--target T]... [--tenant ID] [--no-cache] [--tag REF]

Detection only parses files; it never runs your code. See `docs/plan-format.md` and the design
spec in `docs/superpowers/specs/`.

## Building

`potter build --executor ns` runs `Run` steps in rootless Linux namespaces through vmkit's sandbox. Each step gets:
- an overlay of its base and earlier layers;
- a nested user namespace with no capability over its network;
- seccomp;
- egress limited to public addresses.

Each step's diff is sealed into a layer and cached under a key of exactly what it declared (spec §8).
Source files no step includes are invisible to every step, and changing them rebuilds nothing.

The image is the run base, the `export` paths and the generated files. It is converted by kiln and tagged
`potter/<target>:latest`, so `kiln run potter/app:latest` runs it.

Requirements (Linux only):
- `newuidmap`/`newgidmap` (`uidmap`);
- a range of at least 65536 ids in `/etc/subuid` and `/etc/subgid`;
- `pasta`, `nft` and `ip`;
- vmkit's `vmkit-sandbox` helper (`$VMKIT_SANDBOX`, next to `potter`, or on `PATH`), with its AppArmor profile where Ubuntu restricts user namespaces.

There's no implicit executor, and potter never falls back to namespaces by itself. `--executor vm` (one microVM per target chain, spec §9.3) is built against a backend trait and tested with a scripted VM; it fails with exit 4 until kiln's build mode (kiln M3e) is wired in.

P2a builds plans with OCI bases, such as an explicit `potter.toml` target with `[target.x.base] oci = "..."`.
These ship later:
- detected targets with catalog bases (P2b);
- `apt` steps and `runtime_steps` (P3);
- `copy_from` and secrets (P6);
- `services` (once kiln-init starts them).

Each is refused by name with exit 2.

`$POTTER_HOME` (default `~/.local/share/potter`) holds:
- the cache index (`cache/<tenant>/index/`);
- unpacked layers (`unpacked/`);
- cache mounts (`mounts/<tenant>/`).

Layer blobs live in `$KILN_HOME`.

## Caching, monorepos and secrets

- **Remote cache:** `--cache-to <registry>[/<prefix>]` publishes every step of a chain (local hits too), and `--cache-from` looks up local misses there. `POTTER_REMOTE_CACHE` sets both.
  - Entries live under `<prefix>/<tenant>/cache`, so tenants share nothing.
  - Pulled layers are verified.
  - A cache failure is a warning, never a failed build.
- **Graph:** targets build after the targets they copy from. A `potter.toml` step can be `copy = { from = "web", paths = ["/app/dist"], dest = "/srv/www", owner = "33:33" }` instead of `run`; it follows the placement rules of `COPY --from`.
- **`--affected <git-ref>`:** builds only the targets whose plan or inputs changed since the ref, plus what they copy from. The rest are reported `unchanged`.
- **Secrets:** `--secret id=<id>,src=<file>`, used by steps that list `secrets = ["<id>"]`.
  - Each is mounted read-only at `/run/secrets/<id>`, for that step only.
  - It never reaches a layer.
  - It enters the cache key as a tenant-keyed HMAC.
  - Its value is redacted from logs (best effort).
- **`--progress json`:** one event per line on stdout; the event schema is in `docs/plan-format.md`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a build step failed, or the build output exceeded its limits |
| 2 | usage, config, plan or detection error (including a missing path, and features a later milestone ships) |
| 3 | a sandbox violation (a hostile layer or step output) |
| 4 | infrastructure: I/O, the registry, a missing prerequisite, no executor chosen |
