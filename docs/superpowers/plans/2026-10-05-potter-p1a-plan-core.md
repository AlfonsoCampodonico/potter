# potter P1a (plan core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `potter plan`, `potter plan --explain` and `potter detect`. Together they turn a PHP/Laravel, Node, Bun, Deno or static-site repository, plus an optional `potter.toml`, into a validated, canonical `potter.plan/v1` build plan. Nothing executes and nothing touches the network.

**Architecture:** Four crates in a new Cargo workspace:
- **`potter-plan`**: IR types, validation, RFC 8785 canonical JSON, SHA-256 hashing, the JSON Schema and the sanitiser.
- **`potter-detect`**: version-constraint parsers, a bounded read-only `RepoView`, the provider framework and the six providers.
- **`potter-config`**: `potter.toml` parsing, overrides, explicit plans, and the `plan_repo` orchestrator.
- **`potter`**: the clap CLI.

Detectors only parse files (P9). They emit `Base::Catalog` refs that P2 maps to OCI images and P3 resolves through the real catalog.

**Tech Stack:** Rust 2024 (resolver 3) with serde, serde_json, toml (preserve_order), indexmap, thiserror, globset, schemars 1, sha2 0.11, libc and clap 4. Tests use proptest, insta, assert_cmd, predicates and tempfile.

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md` (rev 1.4). This milestone covers §4 (crates), §5 (IR), §6.1–§6.2, §6.1.1, §11 (`plan`, `detect`), §12 (detection rows), §14.1–§14.2 (P1a fixtures), §15 P1a and §18 (build/deploy command mapping).

**Where to work:** create a new worktree on a new branch `p1a-plan-core` from `docs/design-spec`. All paths below are relative to the potter repo root.

```bash
cd /Users/alfonso/Github/Personal/playground/potter
git worktree add -b p1a-plan-core .worktrees/p1a-plan-core docs/design-spec
cd .worktrees/p1a-plan-core
```

**Commits:** plain conventional messages, with **no** attribution trailers (a user preference).

## Global Constraints

- Workspace `edition = "2024"`, `resolver = "3"`, `license = "Apache-2.0"`, `rustfmt.toml` `max_width = 120` (the kiln conventions).
- Each crate pins its own dependency versions; there is no `[workspace.dependencies]`. Sibling crates are path deps.
- Library crates use one `thiserror` enum in `src/error.rs`. Messages are lowercase with no trailing period.
- CI on `ubuntu-24.04` and `macos-15` runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --all`.
- The plan schema id is exactly `potter.plan/v1`.
- Names (targets, steps, cache mounts, processes, services) match `[a-z0-9._-]{1,64}`.
- Users are a name matching `[a-z_][a-z0-9_-]{0,31}`, a numeric id ≤ 2³¹−1, or `uid:gid`. Image paths are absolute, with no `..` and no NUL.
- `RepoView` limits:
  - at most 200 000 entries and a walk depth of 64;
  - reads capped at 4 MiB, except parsed lockfiles at 64 MiB;
  - symlinks are never followed;
  - the default excludes `.git`, `node_modules`, `vendor`, `target`, `.venv`, `__pycache__` and `.potter` are never walked.
- Detection never executes repository code or tools (P9).
- Exit codes: 0 on success; 2 for usage, plan, config or detection errors (including a missing path); 4 for other I/O errors.
- Every repo-derived string printed by the human CLI output goes through `potter_plan::sanitize`.
- Catalog version lines until P3:
  - php 8.2/8.3/8.4, default 8.4;
  - node 20/22/24 (all LTS), default 22;
  - bun 1, default 1;
  - deno 2, default 2;
  - composer 2.
- Default ports: PHP and static 8080; Node, Bun and Deno 3000. `[target.x] port` overrides, and `[target.x.php] port` is an alias.
- Generated PHP config paths are `/etc/potter/nginx.conf` and `/etc/potter/php-fpm.conf`. The php-fpm socket is `/run/php-fpm.sock`.
- `DENO_DIR=/app/.deno`, kept in the layer.

## Review Focus

These five failure modes are the ones most likely to bite real users:
1. **A real Laravel repo with both `package-lock.json` and `yarn.lock`** (`laravel-dev-starter`) must plan with npm and record a decision, never refuse. Pinned in Task 11 (`laravel_with_two_lockfiles_uses_npm`).
2. **A monorepo root with no root manifest** (`nextjs-go-monorepo`) must fail with `NoProvider`, naming `backend/` and `frontend/` and the `[target.*]` fix. Pinned in Task 8 (`no_provider_lists_manifest_dirs`).
3. **Lockfiles over 4 MiB** must still plan (64 MiB lockfile cap). A file over its cap must fail with an error naming the file and the cap. Pinned in Task 7 (`lockfile_cap_allows_large_lockfiles`, `oversized_file_is_refused`).
4. **Real-world version strings** must parse or fail clearly:
   - `.nvmrc` values `v20.11.0`, `lts/*`, a trailing comment, CRLF;
   - composer `>=8.3 <8.5`, `^8.2 || ^8.3`, `8.2.*`, `>8.2`;
   - an unsatisfiable `^7.4` must produce an error listing the available lines.
   - Pinned in Tasks 5 and 6.
5. **Hostile or odd manifests** must give a clear error naming the file, never a panic: a symlinked `composer.json`, non-UTF-8 bytes, invalid JSON, or a UTF-8 BOM (which must be accepted). Pinned in Task 7 (`symlinked_manifest_is_refused`, `bom_is_accepted`, `invalid_json_names_file`).

## File Structure

```
Cargo.toml                       workspace
rustfmt.toml                     max_width = 120
LICENSE                          Apache-2.0 (copied from kiln)
.github/workflows/ci.yml         fmt + clippy + test on ubuntu/macos
docs/plan-format.md              IR, hashing, potter.toml reference, exit codes
docs/schema/potter.plan.v1.json  generated JSON Schema (golden)
fixtures/<name>/…                manifest-only fixture repos
crates/potter-plan/
  src/lib.rs                     re-exports
  src/ir.rs                      IR types (§5.1)
  src/validate.rs                validation (§5.3)
  src/canon.rs                   JCS canonical JSON + Sha256Digest + step/plan hashing (§5.2)
  src/schema.rs                  JSON Schema generation
  src/sanitize.rs                terminal-safe escaping
  src/explain.rs                 Decision / Origin for --explain
  src/error.rs                   PlanError
  src/testing.rs                 sample_plan() for unit tests (cfg(test))
  tests/schema.rs                golden schema test
crates/potter-detect/
  src/lib.rs                     detect(), claims(), re-exports
  src/error.rs                   DetectError
  src/options.rs                 DetectOptions and friends
  src/repo.rs                    RepoView (bounded, no symlinks)
  src/jsonc.rs                   JSONC → JSON
  src/scan.rs                    tiny text scanners for config files
  src/catalog.rs                 version lines + pick()
  src/select.rs                  Claim, select()
  src/version/{mod,partial,set,npm,composer,file}.rs
  src/providers/{mod,common,php,php_config,laravel,js,node,bun,deno,static_site}.rs
  tests/common/mod.rs            fixture helpers
  tests/{php,laravel,node,bun_deno,static_site,select}.rs
crates/potter-config/
  src/lib.rs                     plan_repo(), PlannedRepo
  src/error.rs                   ConfigError
  src/file.rs                    potter.toml schema + load()
  src/options.rs                 TargetConfig → DetectOptions
  src/merge.rs                   overrides onto detector output
  src/explicit.rs                provider = "none" plans
  tests/{file,merge,fixtures}.rs + tests/snapshots/
crates/potter/
  src/main.rs                    clap CLI, exit codes
  src/render.rs                  human plan / explain / detect output
  tests/cli.rs
```

---

### Task 1: Workspace, CI and the plan IR types

**Files:**
- Create: `Cargo.toml`, `rustfmt.toml`, `LICENSE`, `.github/workflows/ci.yml`
- Create: `crates/potter-plan/Cargo.toml`, `crates/potter-plan/src/lib.rs`, `crates/potter-plan/src/ir.rs`, `crates/potter-plan/src/testing.rs`

**Interfaces:**
- Produces (`potter_plan::`):
  - `SCHEMA: &str`, `Plan { schema, targets }`, `Plan::new(Vec<Target>)`.
  - `Target { name, root, platform: Option<String>, provider, build: Chain, image: Image }`.
  - `Chain { base: Base, steps: Vec<Step> }`.
  - `Base::{Catalog { layers: Vec<CatalogRef> }, Oci { reference, digest: Option<String> }, Stage { name }}`.
  - `CatalogRef { name, version: Option<String> }` and `CatalogRef::new(&str, Option<&str>)`. `Display` prints `name@version` or `name`.
  - `Step::{Inputs, Run, CopyFrom, Apt, Meta}` (struct variants, fields below), plus `Step::name(&self) -> &str` and `Step::kind(&self) -> &'static str`.
  - `Command::{Argv(Vec<String>), Shell(String)}`, `Command::argv(&[&str])` and `Command::shell(&str)`. `Display` joins argv with spaces or prints the shell string.
  - `CopySource::{Stage(String), Target(String)}`, `CacheMount { id, target, sharing: Sharing }`, `Sharing::Exclusive` and `SecretRef { id, target }`.
  - `Image { base, runtime_steps: Vec<Step>, export: Vec<Export>, files: Vec<GeneratedFile>, config: ImageConfig, processes: BTreeMap<String, Process>, services: BTreeMap<String, Vec<Service>> }`.
  - `Export { path, dest, exclude }` and `GeneratedFile { path, content, mode: u32 }`.
  - `ImageConfig { env, workdir, user, labels, port: Option<u16>, stop_signal: Option<String> }` and `ImageConfig::new(workdir, user)`.
  - `Process { command: Vec<String>, workdir: Option<String>, env }` and `Process::new(&[&str])`.
  - `Service { name, command, ready: Ready, stop_signal }` and `Ready::{Unix(String), Tcp(u16), None}`.

- [ ] **Step 1: Create the workspace files**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
license = "Apache-2.0"
```

`rustfmt.toml`:

```toml
max_width = 120
```

Copy the licence:

```bash
cp /Users/alfonso/Github/Personal/playground/kiln/LICENSE LICENSE
```

`.github/workflows/ci.yml`:

```yaml
name: ci
on: [push, pull_request]
jobs:
  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-24.04, macos-15]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test --all
```

`crates/potter-plan/Cargo.toml`:

```toml
[package]
name = "potter-plan"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "potter's build plan IR: types, validation, canonical JSON and hashing"

[dependencies]
globset = "0.4"
schemars = "1"
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
sha2 = "0.11.0"
thiserror = "2.0.21"

[dev-dependencies]
proptest = "1.11.0"
```

- [ ] **Step 2: Write the failing round-trip tests**

`crates/potter-plan/src/testing.rs`:

```rust
//! Shared fixtures for unit tests.

use std::collections::BTreeMap;

use crate::ir::*;

pub(crate) fn sample_plan() -> Plan {
    let run = Step::Run {
        name: "deps".into(),
        command: Command::shell("composer install --no-dev"),
        env: BTreeMap::from([("COMPOSER_CACHE_DIR".into(), "/cache/composer".into())]),
        workdir: "/app".into(),
        user: "root".into(),
        network: true,
        cache_mounts: vec![CacheMount { id: "composer".into(), target: "/cache/composer".into(), sharing: Sharing::Exclusive }],
        secrets: vec![],
        timeout_secs: None,
    };
    let inputs = Step::Inputs {
        name: "deps-inputs".into(),
        include: vec!["composer.json".into(), "composer.lock".into()],
        exclude: vec![],
        dest: "/app".into(),
        owner: "0:0".into(),
    };
    let mut config = ImageConfig::new("/app", "root");
    config.port = Some(8080);
    Plan::new(vec![Target {
        name: "app".into(),
        root: ".".into(),
        platform: None,
        provider: "php".into(),
        build: Chain {
            base: Base::Catalog { layers: vec![CatalogRef::new("os-min", None), CatalogRef::new("php", Some("8.4"))] },
            steps: vec![inputs, run],
        },
        image: Image {
            base: Base::Catalog { layers: vec![CatalogRef::new("os-min", None), CatalogRef::new("php-fpm", Some("8.4"))] },
            runtime_steps: vec![],
            export: vec![Export { path: "/app".into(), dest: "/app".into(), exclude: vec!["node_modules".into()] }],
            files: vec![GeneratedFile { path: "/etc/potter/nginx.conf".into(), content: "daemon off;\n".into(), mode: 0o644 }],
            config,
            processes: BTreeMap::from([("web".into(), Process::new(&["nginx", "-c", "/etc/potter/nginx.conf"]))]),
            services: BTreeMap::from([(
                "web".into(),
                vec![Service {
                    name: "php-fpm".into(),
                    command: vec!["php-fpm".into(), "--nodaemonize".into()],
                    ready: Ready::Unix("/run/php-fpm.sock".into()),
                    stop_signal: "SIGQUIT".into(),
                }],
            )]),
        },
    }])
}
```

Append to `crates/potter-plan/src/ir.rs`. Create the file containing only this test module first, so it fails to compile:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_plan;

    #[test]
    fn plan_round_trips_through_json() {
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn steps_are_internally_tagged() {
        let plan = sample_plan();
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v["targets"][0]["build"]["steps"][0]["kind"], "inputs");
        assert_eq!(v["targets"][0]["build"]["steps"][1]["kind"], "run");
        assert_eq!(v["targets"][0]["build"]["steps"][1]["command"]["shell"], "composer install --no-dev");
        assert_eq!(v["targets"][0]["build"]["base"]["kind"], "catalog");
        assert_eq!(v["targets"][0]["image"]["services"]["web"][0]["ready"]["unix"], "/run/php-fpm.sock");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut v = serde_json::to_value(sample_plan()).unwrap();
        v["targets"][0]["build"]["steps"][1]["surprise"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Plan>(v).is_err());
        let mut v = serde_json::to_value(sample_plan()).unwrap();
        v["targets"][0]["surprise"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Plan>(v).is_err());
    }

    #[test]
    fn ready_none_is_a_bare_string() {
        assert_eq!(serde_json::to_value(Ready::None).unwrap(), serde_json::json!("none"));
    }

    #[test]
    fn catalog_ref_display() {
        assert_eq!(CatalogRef::new("php", Some("8.4")).to_string(), "php@8.4");
        assert_eq!(CatalogRef::new("nginx", None).to_string(), "nginx");
    }

    #[test]
    fn command_display() {
        assert_eq!(Command::argv(&["php", "artisan", "horizon"]).to_string(), "php artisan horizon");
        assert_eq!(Command::shell("a && b").to_string(), "a && b");
    }
}
```

`crates/potter-plan/src/lib.rs`:

```rust
//! potter's build plan IR (spec §5).

mod ir;
#[cfg(test)]
mod testing;

pub use ir::*;
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-plan`
Expected: compile errors, such as "cannot find type `Plan` in this scope".

- [ ] **Step 4: Implement the IR types**

Put this above the test module in `crates/potter-plan/src/ir.rs`:

```rust
//! The plan IR (spec §5.1). Every frontend lowers onto these types.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The schema identifier every plan carries.
pub const SCHEMA: &str = "potter.plan/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub targets: Vec<Target>,
}

impl Plan {
    pub fn new(targets: Vec<Target>) -> Self {
        Plan { schema: SCHEMA.to_string(), targets }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    /// Relative to the repository root; "." is the root itself.
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The provider that produced this target, or "none" for an explicit plan.
    pub provider: String,
    pub build: Chain,
    pub image: Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub base: Base,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Base {
    Catalog {
        layers: Vec<CatalogRef>,
    },
    Oci {
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    Stage {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl CatalogRef {
    pub fn new(name: &str, version: Option<&str>) -> Self {
        CatalogRef { name: name.to_string(), version: version.map(str::to_string) }
    }
}

impl fmt::Display for CatalogRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) => write!(f, "{}@{}", self.name, v),
            None => f.write_str(&self.name),
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// A host-built layer of source files (spec §9.2).
    Inputs {
        name: String,
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
        dest: String,
        owner: String,
    },
    Run {
        name: String,
        command: Command,
        #[serde(default)]
        env: BTreeMap<String, String>,
        workdir: String,
        user: String,
        #[serde(default = "yes")]
        network: bool,
        #[serde(default)]
        cache_mounts: Vec<CacheMount>,
        #[serde(default)]
        secrets: Vec<SecretRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    CopyFrom {
        name: String,
        from: CopySource,
        paths: Vec<String>,
        dest: String,
        owner: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    Apt {
        name: String,
        packages: Vec<String>,
        snapshot: String,
    },
    Meta {
        name: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
}

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::Inputs { name, .. }
            | Step::Run { name, .. }
            | Step::CopyFrom { name, .. }
            | Step::Apt { name, .. }
            | Step::Meta { name, .. } => name,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Step::Inputs { .. } => "inputs",
            Step::Run { .. } => "run",
            Step::CopyFrom { .. } => "copy_from",
            Step::Apt { .. } => "apt",
            Step::Meta { .. } => "meta",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Argv(Vec<String>),
    Shell(String),
}

impl Command {
    pub fn argv(args: &[&str]) -> Self {
        Command::Argv(args.iter().map(|s| s.to_string()).collect())
    }

    pub fn shell(script: &str) -> Self {
        Command::Shell(script.to_string())
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Argv(args) => f.write_str(&args.join(" ")),
            Command::Shell(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CopySource {
    Stage(String),
    Target(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sharing {
    #[default]
    Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheMount {
    pub id: String,
    pub target: String,
    #[serde(default)]
    pub sharing: Sharing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub id: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub base: Base,
    /// Runtime system packages; only `Step::Apt` is allowed here (checked by `validate`).
    #[serde(default)]
    pub runtime_steps: Vec<Step>,
    #[serde(default)]
    pub export: Vec<Export>,
    #[serde(default)]
    pub files: Vec<GeneratedFile>,
    pub config: ImageConfig,
    #[serde(default)]
    pub processes: BTreeMap<String, Process>,
    #[serde(default)]
    pub services: BTreeMap<String, Vec<Service>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub path: String,
    pub dest: String,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageConfig {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub workdir: String,
    pub user: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
}

impl ImageConfig {
    pub fn new(workdir: &str, user: &str) -> Self {
        ImageConfig {
            env: BTreeMap::new(),
            workdir: workdir.to_string(),
            user: user.to_string(),
            labels: BTreeMap::new(),
            port: None,
            stop_signal: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Process {
    pub fn new(command: &[&str]) -> Self {
        Process { command: command.iter().map(|s| s.to_string()).collect(), workdir: None, env: BTreeMap::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    pub command: Vec<String>,
    pub ready: Ready,
    pub stop_signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ready {
    Unix(String),
    Tcp(u16),
    None,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-plan && cargo clippy -p potter-plan --all-targets -- -D warnings && cargo fmt --all --check`
Expected: 6 tests pass, with no clippy warnings and no fmt diff.

If `unknown_fields_are_rejected` fails for the step case, serde is not applying `deny_unknown_fields` to the internally tagged struct variant. Check with `serde_json::from_value::<Step>`, and do not remove the test.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml rustfmt.toml LICENSE .github crates/potter-plan Cargo.lock
git commit -m "feat(potter-plan): workspace, CI and plan IR types"
```

---

### Task 2: Plan validation

**Files:**
- Create: `crates/potter-plan/src/error.rs`, `crates/potter-plan/src/validate.rs`
- Modify: `crates/potter-plan/src/lib.rs`

**Interfaces:**
- Consumes: the Task 1 IR types and `testing::sample_plan()`.
- Produces:
  - `potter_plan::validate(&Plan) -> Result<(), PlanError>`.
  - `PlanError::{Invalid(Vec<Issue>), Float, Json(serde_json::Error)}` and `Issue { at: String, reason: String }`.
  - `potter_plan::is_name(&str) -> bool`, the `[a-z0-9._-]{1,64}` rule, reused by potter-config.
  - `potter_plan::is_relative_path(&str) -> bool`, reused by potter-detect and potter-config.

- [ ] **Step 1: Write the failing tests**

Create `crates/potter-plan/src/validate.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::*;
    use crate::testing::sample_plan;

    fn issues(plan: &Plan) -> Vec<Issue> {
        match validate(plan) {
            Ok(()) => vec![],
            Err(PlanError::Invalid(issues)) => issues,
            Err(other) => panic!("unexpected error {other}"),
        }
    }

    fn has(issues: &[Issue], at: &str) -> bool {
        issues.iter().any(|i| i.at == at)
    }

    #[test]
    fn sample_is_valid() {
        assert_eq!(issues(&sample_plan()), vec![]);
    }

    #[test]
    fn wrong_schema() {
        let mut p = sample_plan();
        p.schema = "potter.plan/v0".into();
        assert!(has(&issues(&p), "schema"));
    }

    #[test]
    fn bad_and_duplicate_names() {
        let mut p = sample_plan();
        p.targets.push(p.targets[0].clone());
        assert!(has(&issues(&p), "targets[1].name"));
        let mut p = sample_plan();
        p.targets[0].name = "App".into();
        assert!(has(&issues(&p), "targets[0].name"));
        let mut p = sample_plan();
        let dup = p.targets[0].build.steps[1].clone();
        p.targets[0].build.steps.push(dup);
        assert!(has(&issues(&p), "targets[0].build.steps[2].name"));
    }

    #[test]
    fn paths() {
        let mut p = sample_plan();
        p.targets[0].root = "../escape".into();
        assert!(has(&issues(&p), "targets[0].root"));
        let mut p = sample_plan();
        p.targets[0].image.export[0].dest = "relative".into();
        assert!(has(&issues(&p), "targets[0].image.export[0].dest"));
        let mut p = sample_plan();
        p.targets[0].image.export[0].path = "/app/../etc".into();
        assert!(has(&issues(&p), "targets[0].image.export[0].path"));
        let mut p = sample_plan();
        p.targets[0].image.files[0].path = "/etc/\0x".into();
        assert!(has(&issues(&p), "targets[0].image.files[0].path"));
    }

    #[test]
    fn globs() {
        let mut p = sample_plan();
        if let Step::Inputs { include, .. } = &mut p.targets[0].build.steps[0] {
            include.push("/abs/**".into());
            include.push("a/[".into());
        }
        let found = issues(&p);
        assert!(has(&found, "targets[0].build.steps[0].include[2]"));
        assert!(has(&found, "targets[0].build.steps[0].include[3]"));
    }

    #[test]
    fn users_and_owners() {
        for (user, ok) in [("root", true), ("www-data", true), ("1000", true), ("1000:1000", true), ("2147483648", false), ("Root", false), ("", false)] {
            let mut p = sample_plan();
            if let Step::Run { user: u, .. } = &mut p.targets[0].build.steps[1] {
                *u = user.into();
            }
            assert_eq!(issues(&p).is_empty(), ok, "user {user:?}");
        }
        let mut p = sample_plan();
        if let Step::Inputs { owner, .. } = &mut p.targets[0].build.steps[0] {
            *owner = "root".into();
        }
        assert!(has(&issues(&p), "targets[0].build.steps[0].owner"));
    }

    #[test]
    fn env_keys() {
        let mut p = sample_plan();
        p.targets[0].image.config.env.insert("1BAD".into(), "x".into());
        assert!(has(&issues(&p), "targets[0].image.config.env.1BAD"));
    }

    #[test]
    fn runtime_steps_only_apt() {
        let mut p = sample_plan();
        let run = p.targets[0].build.steps[1].clone();
        p.targets[0].image.runtime_steps.push(run);
        assert!(has(&issues(&p), "targets[0].image.runtime_steps[0]"));
    }

    #[test]
    fn apt_fields() {
        let mut p = sample_plan();
        p.targets[0].image.runtime_steps.push(Step::Apt {
            name: "apt-runtime".into(),
            packages: vec!["libpq5".into(), "Bad Name".into()],
            snapshot: "2026-10-01".into(),
        });
        let found = issues(&p);
        assert!(has(&found, "targets[0].image.runtime_steps[0].packages[1]"));
        assert!(has(&found, "targets[0].image.runtime_steps[0].snapshot"));
    }

    #[test]
    fn services_need_a_process() {
        let mut p = sample_plan();
        let svc = p.targets[0].image.services.remove("web").unwrap();
        p.targets[0].image.services.insert("worker".into(), svc);
        assert!(has(&issues(&p), "targets[0].image.services.worker"));
    }

    #[test]
    fn empty_commands() {
        let mut p = sample_plan();
        p.targets[0].image.processes.insert("worker".into(), Process::new(&[]));
        assert!(has(&issues(&p), "targets[0].image.processes.worker.command"));
        let mut p = sample_plan();
        if let Step::Run { command, .. } = &mut p.targets[0].build.steps[1] {
            *command = Command::Shell("  ".into());
        }
        assert!(has(&issues(&p), "targets[0].build.steps[1].command"));
    }

    #[test]
    fn oci_digest() {
        let mut p = sample_plan();
        p.targets[0].image.base = Base::Oci { reference: "php:8.4-fpm".into(), digest: Some("sha256:abc".into()) };
        assert!(has(&issues(&p), "targets[0].image.base.digest"));
    }

    #[test]
    fn file_modes() {
        let mut p = sample_plan();
        p.targets[0].image.files[0].mode = 0o17777;
        assert!(has(&issues(&p), "targets[0].image.files[0].mode"));
    }

    #[test]
    fn helper_predicates() {
        assert!(is_name("deps-inputs"));
        assert!(is_name("user-build-1"));
        assert!(!is_name(""));
        assert!(!is_name(&"a".repeat(65)));
        assert!(is_relative_path("."));
        assert!(is_relative_path("services/api"));
        assert!(!is_relative_path("/abs"));
        assert!(!is_relative_path("a/../b"));
    }
}
```

`crates/potter-plan/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Where the problem is, e.g. `targets[0].build.steps[2].name`.
    pub at: String,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("plan is invalid:\n{}", format_issues(.0))]
    Invalid(Vec<Issue>),
    #[error("floating-point numbers are not allowed in canonical JSON")]
    Float,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

fn format_issues(issues: &[Issue]) -> String {
    issues.iter().map(|i| format!("  {}: {}", i.at, i.reason)).collect::<Vec<_>>().join("\n")
}
```

Update `crates/potter-plan/src/lib.rs`:

```rust
//! potter's build plan IR (spec §5).

mod error;
mod ir;
#[cfg(test)]
mod testing;
mod validate;

pub use error::{Issue, PlanError};
pub use ir::*;
pub use validate::{is_name, is_relative_path, validate};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-plan validate`
Expected: compile error, "cannot find function `validate`".

- [ ] **Step 3: Implement validation**

Put this above the tests in `crates/potter-plan/src/validate.rs`:

```rust
//! Plan validation (spec §5.3). Collects every issue rather than stopping at the first.

use std::collections::BTreeSet;

use crate::error::{Issue, PlanError};
use crate::ir::*;

const MAX_ID: u64 = (1 << 31) - 1;

pub fn validate(plan: &Plan) -> Result<(), PlanError> {
    let mut v = Validator::default();
    if plan.schema != SCHEMA {
        v.issue("schema", format!("expected `{SCHEMA}`, found `{}`", plan.schema));
    }
    let mut names = BTreeSet::new();
    for (i, t) in plan.targets.iter().enumerate() {
        let at = format!("targets[{i}]");
        v.name(&format!("{at}.name"), &t.name);
        if !names.insert(t.name.as_str()) {
            v.issue(&format!("{at}.name"), format!("duplicate target name `{}`", t.name));
        }
        if !is_relative_path(&t.root) {
            v.issue(&format!("{at}.root"), "must be a relative path without `..`");
        }
        v.name(&format!("{at}.provider"), &t.provider);
        v.chain(&format!("{at}.build"), &t.build);
        v.image(&format!("{at}.image"), &t.image);
    }
    if v.issues.is_empty() { Ok(()) } else { Err(PlanError::Invalid(v.issues)) }
}

/// `[a-z0-9._-]{1,64}`.
pub fn is_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// Relative, no `..` components, no NUL; "." is allowed.
pub fn is_relative_path(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('/') && !s.contains('\0') && !s.split('/').any(|c| c == "..")
}

fn is_absolute_path(s: &str) -> bool {
    s.starts_with('/') && s.len() <= 4096 && !s.contains('\0') && !s.split('/').any(|c| c == "..")
}

fn is_numeric_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u64>().is_ok_and(|n| n <= MAX_ID)
}

fn is_user_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b == b'_' => {}
        _ => return false,
    }
    s.len() <= 32 && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn is_user(s: &str) -> bool {
    match s.split_once(':') {
        Some((u, g)) => is_numeric_id(u) && is_numeric_id(g),
        None => is_numeric_id(s) || is_user_name(s),
    }
}

fn is_owner(s: &str) -> bool {
    s.split_once(':').is_some_and(|(u, g)| is_numeric_id(u) && is_numeric_id(g))
}

fn is_env_key(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_digest(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn is_package(s: &str) -> bool {
    s.len() >= 2
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+.-".contains(&b))
}

fn is_snapshot(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

fn is_signal(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn is_version_label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
}

#[derive(Default)]
struct Validator {
    issues: Vec<Issue>,
}

impl Validator {
    fn issue(&mut self, at: &str, reason: impl Into<String>) {
        self.issues.push(Issue { at: at.to_string(), reason: reason.into() });
    }

    fn name(&mut self, at: &str, s: &str) {
        if !is_name(s) {
            self.issue(at, format!("`{s}` must match [a-z0-9._-]{{1,64}}"));
        }
    }

    fn abs(&mut self, at: &str, s: &str) {
        if !is_absolute_path(s) {
            self.issue(at, "must be an absolute path without `..` or NUL");
        }
    }

    fn glob(&mut self, at: &str, g: &str) {
        if g.is_empty() || g.starts_with('/') || g.contains('\0') || g.split('/').any(|c| c == "..") {
            self.issue(at, "globs are relative to the target root, without `..`");
        } else if let Err(e) = globset::Glob::new(g) {
            self.issue(at, format!("invalid glob: {e}"));
        }
    }

    fn user(&mut self, at: &str, s: &str) {
        if !is_user(s) {
            self.issue(at, "must be a user name, a numeric id up to 2^31-1, or uid:gid");
        }
    }

    fn owner(&mut self, at: &str, s: &str) {
        if !is_owner(s) {
            self.issue(at, "must be numeric uid:gid, each up to 2^31-1");
        }
    }

    fn env(&mut self, at: &str, env: &std::collections::BTreeMap<String, String>) {
        for (k, val) in env {
            if !is_env_key(k) {
                self.issue(&format!("{at}.{k}"), "environment keys match [A-Za-z_][A-Za-z0-9_]*");
            }
            if val.contains('\0') {
                self.issue(&format!("{at}.{k}"), "environment values cannot contain NUL");
            }
        }
    }

    fn command(&mut self, at: &str, c: &Command) {
        let empty = match c {
            Command::Argv(a) => a.first().is_none_or(|s| s.trim().is_empty()),
            Command::Shell(s) => s.trim().is_empty(),
        };
        if empty {
            self.issue(at, "command is empty");
        }
    }

    fn base(&mut self, at: &str, b: &Base) {
        match b {
            Base::Catalog { layers } => {
                if layers.is_empty() {
                    self.issue(&format!("{at}.layers"), "a catalog base needs at least one layer");
                }
                for (i, l) in layers.iter().enumerate() {
                    self.name(&format!("{at}.layers[{i}].name"), &l.name);
                    if let Some(v) = &l.version
                        && !is_version_label(v)
                    {
                        self.issue(&format!("{at}.layers[{i}].version"), format!("invalid version `{v}`"));
                    }
                }
            }
            Base::Oci { reference, digest } => {
                if reference.is_empty() || reference.chars().any(char::is_whitespace) {
                    self.issue(&format!("{at}.reference"), "invalid image reference");
                }
                if let Some(d) = digest
                    && !is_digest(d)
                {
                    self.issue(&format!("{at}.digest"), "digest must be sha256:<64 lowercase hex>");
                }
            }
            Base::Stage { name } => self.name(&format!("{at}.name"), name),
        }
    }

    fn step(&mut self, at: &str, s: &Step) {
        self.name(&format!("{at}.name"), s.name());
        match s {
            Step::Inputs { include, exclude, dest, owner, .. } => {
                if include.is_empty() {
                    self.issue(&format!("{at}.include"), "an inputs step needs at least one glob");
                }
                for (i, g) in include.iter().enumerate() {
                    self.glob(&format!("{at}.include[{i}]"), g);
                }
                for (i, g) in exclude.iter().enumerate() {
                    self.glob(&format!("{at}.exclude[{i}]"), g);
                }
                self.abs(&format!("{at}.dest"), dest);
                self.owner(&format!("{at}.owner"), owner);
            }
            Step::Run { command, env, workdir, user, cache_mounts, secrets, timeout_secs, .. } => {
                self.command(&format!("{at}.command"), command);
                self.env(&format!("{at}.env"), env);
                self.abs(&format!("{at}.workdir"), workdir);
                self.user(&format!("{at}.user"), user);
                for (i, m) in cache_mounts.iter().enumerate() {
                    self.name(&format!("{at}.cache_mounts[{i}].id"), &m.id);
                    self.abs(&format!("{at}.cache_mounts[{i}].target"), &m.target);
                }
                for (i, sec) in secrets.iter().enumerate() {
                    self.name(&format!("{at}.secrets[{i}].id"), &sec.id);
                    self.abs(&format!("{at}.secrets[{i}].target"), &sec.target);
                }
                if *timeout_secs == Some(0) {
                    self.issue(&format!("{at}.timeout_secs"), "timeout must be positive");
                }
            }
            Step::CopyFrom { from, paths, dest, owner, mode, .. } => {
                let (CopySource::Stage(n) | CopySource::Target(n)) = from;
                self.name(&format!("{at}.from"), n);
                for (i, p) in paths.iter().enumerate() {
                    self.abs(&format!("{at}.paths[{i}]"), p);
                }
                self.abs(&format!("{at}.dest"), dest);
                self.owner(&format!("{at}.owner"), owner);
                if mode.is_some_and(|m| m > 0o7777) {
                    self.issue(&format!("{at}.mode"), "mode must be at most 0o7777");
                }
            }
            Step::Apt { packages, snapshot, .. } => {
                if packages.is_empty() {
                    self.issue(&format!("{at}.packages"), "an apt step needs at least one package");
                }
                for (i, p) in packages.iter().enumerate() {
                    if !is_package(p) {
                        self.issue(&format!("{at}.packages[{i}]"), format!("`{p}` is not a Debian package name"));
                    }
                }
                if !is_snapshot(snapshot) {
                    self.issue(&format!("{at}.snapshot"), "snapshot must look like 20261001T000000Z");
                }
            }
            Step::Meta { env, workdir, user, .. } => {
                self.env(&format!("{at}.env"), env);
                if let Some(w) = workdir {
                    self.abs(&format!("{at}.workdir"), w);
                }
                if let Some(u) = user {
                    self.user(&format!("{at}.user"), u);
                }
            }
        }
    }

    fn chain(&mut self, at: &str, c: &Chain) {
        self.base(&format!("{at}.base"), &c.base);
        let mut names = BTreeSet::new();
        for (i, s) in c.steps.iter().enumerate() {
            let sat = format!("{at}.steps[{i}]");
            self.step(&sat, s);
            if !names.insert(s.name()) {
                self.issue(&format!("{sat}.name"), format!("duplicate step name `{}`", s.name()));
            }
        }
    }

    fn image(&mut self, at: &str, img: &Image) {
        self.base(&format!("{at}.base"), &img.base);
        for (i, s) in img.runtime_steps.iter().enumerate() {
            let sat = format!("{at}.runtime_steps[{i}]");
            if matches!(s, Step::Apt { .. }) {
                self.step(&sat, s);
            } else {
                self.issue(&sat, "runtime steps may only be apt steps");
            }
        }
        for (i, e) in img.export.iter().enumerate() {
            self.abs(&format!("{at}.export[{i}].path"), &e.path);
            self.abs(&format!("{at}.export[{i}].dest"), &e.dest);
            for (j, g) in e.exclude.iter().enumerate() {
                self.glob(&format!("{at}.export[{i}].exclude[{j}]"), g);
            }
        }
        let mut paths = BTreeSet::new();
        for (i, f) in img.files.iter().enumerate() {
            self.abs(&format!("{at}.files[{i}].path"), &f.path);
            if f.mode > 0o7777 {
                self.issue(&format!("{at}.files[{i}].mode"), "mode must be at most 0o7777");
            }
            if !paths.insert(f.path.as_str()) {
                self.issue(&format!("{at}.files[{i}].path"), "duplicate generated file");
            }
        }
        self.env(&format!("{at}.config.env"), &img.config.env);
        self.abs(&format!("{at}.config.workdir"), &img.config.workdir);
        self.user(&format!("{at}.config.user"), &img.config.user);
        if img.config.port == Some(0) {
            self.issue(&format!("{at}.config.port"), "port must be 1-65535");
        }
        if let Some(sig) = &img.config.stop_signal
            && !is_signal(sig)
        {
            self.issue(&format!("{at}.config.stop_signal"), "signals are upper-case names like SIGQUIT");
        }
        for (name, p) in &img.processes {
            self.name(&format!("{at}.processes.{name}"), name);
            if p.command.first().is_none_or(|s| s.trim().is_empty()) {
                self.issue(&format!("{at}.processes.{name}.command"), "command is empty");
            }
            self.env(&format!("{at}.processes.{name}.env"), &p.env);
            if let Some(w) = &p.workdir {
                self.abs(&format!("{at}.processes.{name}.workdir"), w);
            }
        }
        for (process, services) in &img.services {
            if !img.processes.contains_key(process) {
                self.issue(&format!("{at}.services.{process}"), format!("no process named `{process}`"));
            }
            let mut names = BTreeSet::new();
            for (i, s) in services.iter().enumerate() {
                let sat = format!("{at}.services.{process}[{i}]");
                self.name(&format!("{sat}.name"), &s.name);
                if !names.insert(s.name.as_str()) {
                    self.issue(&format!("{sat}.name"), "duplicate service name");
                }
                if s.command.first().is_none_or(|c| c.trim().is_empty()) {
                    self.issue(&format!("{sat}.command"), "command is empty");
                }
                if let Ready::Unix(p) = &s.ready {
                    self.abs(&format!("{sat}.ready"), p);
                }
                if !is_signal(&s.stop_signal) {
                    self.issue(&format!("{sat}.stop_signal"), "signals are upper-case names like SIGQUIT");
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-plan && cargo clippy -p potter-plan --all-targets -- -D warnings`
Expected: every test passes and clippy is clean.

The `if let … && …` chains need edition 2024 with Rust ≥ 1.88; the toolchain here is 1.99.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-plan
git commit -m "feat(potter-plan): plan validation"
```

---

### Task 3: Canonical JSON, hashing, JSON Schema, sanitiser and decisions

**Files:**
- Create: `crates/potter-plan/src/canon.rs`, `crates/potter-plan/src/schema.rs`, `crates/potter-plan/src/sanitize.rs`, `crates/potter-plan/src/explain.rs`, `crates/potter-plan/tests/schema.rs`, `docs/schema/potter.plan.v1.json`
- Modify: `crates/potter-plan/src/lib.rs`

**Interfaces:**
- Produces:
  - `canonical_json<T: Serialize>(&T) -> Result<String, PlanError>` (RFC 8785).
  - `Sha256Digest([u8; 32])` with `Sha256Digest::of(&[u8])` and `.hex()`. `Display` prints `sha256:<hex>`.
  - `hash_of<T: Serialize>(&T) -> Result<Sha256Digest, PlanError>`.
  - `step_hash(&Step) -> Result<Sha256Digest, PlanError>`, which ignores `name` and `timeout_secs`.
  - `plan_hash(&Plan) -> Result<Sha256Digest, PlanError>`, which ignores every `timeout_secs`.
  - `json_schema() -> String` (pretty JSON with a trailing newline).
  - `sanitize(&str) -> String`.
  - `Origin::{Detected { evidence: Vec<String> }, Config { key: String }, Default}` with `Display`.
  - `Decision { field, value, origin }` with `Decision::detected(field, value, evidence)`, `Decision::config(field, value, key)` and `Decision::default(field, value)`.

- [ ] **Step 1: Write the failing tests**

Create `crates/potter-plan/src/canon.rs` with only these tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::*;
    use crate::testing::sample_plan;
    use serde_json::json;

    #[test]
    fn keys_sorted_no_whitespace() {
        assert_eq!(canonical_json(&json!({"b": 2, "a": [1, {"d": null, "c": true}]})).unwrap(), r#"{"a":[1,{"c":true,"d":null}],"b":2}"#);
    }

    #[test]
    fn keys_sort_by_utf16_code_units() {
        // U+10000 is D800 DC00 in UTF-16, which sorts before U+E000, unlike in UTF-8.
        let v = json!({"\u{e000}": 1, "\u{10000}": 2});
        assert_eq!(canonical_json(&v).unwrap(), "{\"\u{10000}\":2,\"\u{e000}\":1}");
    }

    #[test]
    fn string_escaping_matches_jcs() {
        assert_eq!(canonical_json(&json!("a\u{1}\"\\/é\n")).unwrap(), "\"a\\u0001\\\"\\\\/é\\n\"");
    }

    #[test]
    fn floats_are_rejected() {
        assert!(matches!(canonical_json(&json!(1.5)), Err(PlanError::Float)));
    }

    #[test]
    fn step_hash_ignores_name_and_timeout() {
        let plan = sample_plan();
        let step = plan.targets[0].build.steps[1].clone();
        let mut renamed = step.clone();
        if let Step::Run { name, timeout_secs, .. } = &mut renamed {
            *name = "other".into();
            *timeout_secs = Some(60);
        }
        assert_eq!(step_hash(&step).unwrap(), step_hash(&renamed).unwrap());
        let mut changed = step.clone();
        if let Step::Run { command, .. } = &mut changed {
            *command = Command::shell("composer install");
        }
        assert_ne!(step_hash(&step).unwrap(), step_hash(&changed).unwrap());
    }

    #[test]
    fn plan_hash_ignores_timeouts_only() {
        let plan = sample_plan();
        let mut timed = plan.clone();
        if let Step::Run { timeout_secs, .. } = &mut timed.targets[0].build.steps[1] {
            *timeout_secs = Some(30);
        }
        assert_eq!(plan_hash(&plan).unwrap(), plan_hash(&timed).unwrap());
        let mut renamed = plan.clone();
        renamed.targets[0].name = "web".into();
        assert_ne!(plan_hash(&plan).unwrap(), plan_hash(&renamed).unwrap());
    }

    #[test]
    fn digest_display() {
        let d = Sha256Digest::of(b"");
        assert_eq!(d.to_string(), "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
```

`crates/potter-plan/src/sanitize.rs`:

```rust
//! Escape characters that could forge terminal output (kiln T8): C0/C1 controls except newline
//! and tab, DEL, and bidirectional overrides.

pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let bad = (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if bad {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn escapes_controls_and_bidi() {
        assert_eq!(sanitize("ok\n\tline"), "ok\n\tline");
        assert_eq!(sanitize("a\u{1b}[31mb"), "a\\u{1b}[31mb");
        assert_eq!(sanitize("x\u{202e}y\u{7f}"), "x\\u{202e}y\\u{7f}");
        assert_eq!(sanitize("\u{85}"), "\\u{85}");
    }
}
```

`crates/potter-plan/src/explain.rs`:

```rust
//! Where each plan field came from, for `potter plan --explain` (spec §6.2).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Detected { evidence: Vec<String> },
    Config { key: String },
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// A dotted field path, e.g. `runtime.php` or `steps.assets.command`.
    pub field: String,
    pub value: String,
    pub origin: Origin,
}

impl Decision {
    pub fn detected<I, S>(field: &str, value: impl Into<String>, evidence: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Decision {
            field: field.to_string(),
            value: value.into(),
            origin: Origin::Detected { evidence: evidence.into_iter().map(Into::into).collect() },
        }
    }

    pub fn config(field: &str, value: impl Into<String>, key: impl Into<String>) -> Self {
        Decision { field: field.to_string(), value: value.into(), origin: Origin::Config { key: key.into() } }
    }

    pub fn default(field: &str, value: impl Into<String>) -> Self {
        Decision { field: field.to_string(), value: value.into(), origin: Origin::Default }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Detected { evidence } => write!(f, "detected: {}", evidence.join("; ")),
            Origin::Config { key } => write!(f, "potter.toml {key}"),
            Origin::Default => f.write_str("default"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_display() {
        assert_eq!(Decision::detected("runtime.php", "8.4", ["composer.json require.php = ^8.2"]).origin.to_string(), "detected: composer.json require.php = ^8.2");
        assert_eq!(Decision::config("runtime.php", "8.3", "target.app.runtime.php").origin.to_string(), "potter.toml target.app.runtime.php");
        assert_eq!(Decision::default("runtime.node", "22").origin.to_string(), "default");
    }
}
```

`crates/potter-plan/tests/schema.rs`:

```rust
//! The committed JSON Schema must match the types. Regenerate with `UPDATE_SCHEMA=1 cargo test -p potter-plan`.

use std::path::Path;

#[test]
fn committed_schema_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema/potter.plan.v1.json");
    let generated = potter_plan::json_schema();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
    }
    let committed = std::fs::read_to_string(&path).expect("docs/schema/potter.plan.v1.json exists; run with UPDATE_SCHEMA=1");
    assert_eq!(committed, generated, "schema drifted; run UPDATE_SCHEMA=1 cargo test -p potter-plan");
}
```

Update `crates/potter-plan/src/lib.rs`:

```rust
//! potter's build plan IR (spec §5).

mod canon;
mod error;
mod explain;
mod ir;
mod sanitize;
mod schema;
#[cfg(test)]
mod testing;
mod validate;

pub use canon::{Sha256Digest, canonical_json, hash_of, plan_hash, step_hash};
pub use error::{Issue, PlanError};
pub use explain::{Decision, Origin};
pub use ir::*;
pub use sanitize::sanitize;
pub use schema::json_schema;
pub use validate::{is_name, is_relative_path, validate};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-plan`
Expected: compile errors for `canonical_json`, `json_schema` and the others.

- [ ] **Step 3: Implement `canon.rs` and `schema.rs`**

Put this above the tests in `crates/potter-plan/src/canon.rs`:

```rust
//! RFC 8785 (JCS) canonical JSON and SHA-256 hashing (spec §5.2).

use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::error::PlanError;
use crate::ir::{Plan, Step};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    pub fn of(bytes: &[u8]) -> Self {
        let out = Sha256::digest(bytes);
        let mut a = [0u8; 32];
        a.copy_from_slice(out.as_slice());
        Sha256Digest(a)
    }

    pub fn hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0xf) as usize] as char);
        }
        s
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, PlanError> {
    let v = serde_json::to_value(value)?;
    let mut out = String::new();
    write_value(&v, &mut out)?;
    Ok(out)
}

fn write_value(v: &Value, out: &mut String) -> Result<(), PlanError> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                return Err(PlanError::Float);
            }
            out.push_str(&n.to_string());
        }
        // serde_json escapes exactly what ECMAScript JSON.stringify escapes: `"`, `\`, and C0
        // controls (as \b \f \n \r \t or lower-case \u00xx). Everything else is literal UTF-8.
        Value::String(s) => out.push_str(&serde_json::to_string(s)?),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k)?);
                out.push(':');
                write_value(&map[k.as_str()], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

pub fn hash_of<T: Serialize>(value: &T) -> Result<Sha256Digest, PlanError> {
    Ok(Sha256Digest::of(canonical_json(value)?.as_bytes()))
}

/// The hash of a step for cache keys (spec §8.1): labels and timeouts are execution-only.
pub fn step_hash(step: &Step) -> Result<Sha256Digest, PlanError> {
    let mut v = serde_json::to_value(step)?;
    if let Value::Object(m) = &mut v {
        m.remove("name");
        m.remove("timeout_secs");
    }
    let mut out = String::new();
    write_value(&v, &mut out)?;
    Ok(Sha256Digest::of(out.as_bytes()))
}

/// The hash of a whole plan, used by `--affected` (spec §10). Timeouts don't count.
pub fn plan_hash(plan: &Plan) -> Result<Sha256Digest, PlanError> {
    let mut p = plan.clone();
    for t in &mut p.targets {
        for s in t.build.steps.iter_mut().chain(t.image.runtime_steps.iter_mut()) {
            if let Step::Run { timeout_secs, .. } = s {
                *timeout_secs = None;
            }
        }
    }
    hash_of(&p)
}
```

`crates/potter-plan/src/schema.rs`:

```rust
//! The published JSON Schema for `potter.plan/v1`.

use crate::ir::Plan;

pub fn json_schema() -> String {
    let schema = schemars::schema_for!(Plan);
    let mut s = serde_json::to_string_pretty(&schema).expect("a schema always serialises");
    s.push('\n');
    s
}
```

- [ ] **Step 4: Generate the schema and run the tests**

Run: `UPDATE_SCHEMA=1 cargo test -p potter-plan --test schema && cargo test -p potter-plan && cargo clippy -p potter-plan --all-targets -- -D warnings`
Expected: `docs/schema/potter.plan.v1.json` is written and every test passes.

Open the schema and check two things: its title is `Plan`, and `Step` appears as a `oneOf` with `kind` constants.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-plan docs/schema
git commit -m "feat(potter-plan): canonical JSON, hashing, schema, sanitiser and decisions"
```

---

### Task 4: `potter-detect` crate, version sets and partial versions

**Files:**
- Create: `crates/potter-detect/Cargo.toml`, `crates/potter-detect/src/lib.rs`, `crates/potter-detect/src/version/mod.rs`, `crates/potter-detect/src/version/set.rs`, `crates/potter-detect/src/version/partial.rs`

**Interfaces:**
- Produces (`potter_detect::version::`):
  - `Version { major, minor, patch: u64 }` and `Version::new(u64, u64, u64)`.
  - `Bound::{Unbounded, Inclusive(Version), Exclusive(Version)}`.
  - `Interval { lo: Bound, hi: Bound }`, with `Interval::ALL` and the methods `is_empty`, `contains(Version)` and `intersect(&Interval)`.
  - `VersionSet` (a union of intervals), with `any()`, `none()`, `interval(Bound, Bound)`, `exact(Version)`, `intersect(&Self)`, `union(Self)`, `contains(Version)`, `is_empty()` and `intersects(&Interval)`.
  - `ConstraintError(pub String)`.
- Crate-internal (`version::partial`):
  - `Partial { major, minor, patch: Option<u64>, wildcard: bool }` and `parse_partial(&str) -> Result<Partial, ConstraintError>`.
  - `Partial::floor() -> Version`.
  - `Partial::x_range() -> Result<VersionSet, ConstraintError>`.
  - `Partial::upper_exclusive() -> Result<Option<Version>, ConstraintError>`: the first version above the range the partial names, or `None` for `*`.
  - `bump(u64) -> Result<u64, ConstraintError>`.

- [ ] **Step 1: Create the crate and write the failing tests**

`crates/potter-detect/Cargo.toml`:

```toml
[package]
name = "potter-detect"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "potter's language detectors: static repository parsing into build plans"

[dependencies]
libc = "0.2"
potter-plan = { path = "../potter-plan" }
serde_json = "1.0.151"
thiserror = "2.0.21"

[dev-dependencies]
proptest = "1.11.0"
tempfile = "3.27.0"
```

`crates/potter-detect/src/lib.rs`:

```rust
//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

pub mod version;
```

`crates/potter-detect/src/version/mod.rs`:

```rust
//! Version constraints for runtime selection. Prerelease and build metadata are ignored: potter
//! selects release lines, never prereleases.

mod partial;
mod set;

pub(crate) use partial::{Partial, bump, parse_partial};
pub use set::{Bound, Interval, Version, VersionSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConstraintError(pub String);
```

`crates/potter-detect/src/version/set.rs`, starting with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v(a: u64, b: u64, c: u64) -> Version {
        Version::new(a, b, c)
    }

    #[test]
    fn interval_bounds() {
        let i = Interval { lo: Bound::Inclusive(v(1, 2, 0)), hi: Bound::Exclusive(v(2, 0, 0)) };
        assert!(i.contains(v(1, 2, 0)));
        assert!(i.contains(v(1, 99, 99)));
        assert!(!i.contains(v(2, 0, 0)));
        assert!(!i.contains(v(1, 1, 9)));
    }

    #[test]
    fn empty_intervals() {
        assert!(Interval { lo: Bound::Inclusive(v(2, 0, 0)), hi: Bound::Exclusive(v(2, 0, 0)) }.is_empty());
        assert!(!Interval { lo: Bound::Inclusive(v(2, 0, 0)), hi: Bound::Inclusive(v(2, 0, 0)) }.is_empty());
        assert!(Interval { lo: Bound::Exclusive(v(2, 0, 0)), hi: Bound::Inclusive(v(2, 0, 0)) }.is_empty());
        assert!(!Interval::ALL.is_empty());
    }

    #[test]
    fn set_operations() {
        let a = VersionSet::interval(Bound::Inclusive(v(8, 2, 0)), Bound::Unbounded);
        let b = VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v(8, 5, 0)));
        let both = a.intersect(&b);
        assert!(both.contains(v(8, 4, 1)));
        assert!(!both.contains(v(8, 5, 0)));
        assert!(!both.contains(v(8, 1, 9)));
        let either = VersionSet::exact(v(1, 0, 0)).union(VersionSet::exact(v(3, 0, 0)));
        assert!(either.contains(v(3, 0, 0)));
        assert!(!either.contains(v(2, 0, 0)));
        assert!(VersionSet::none().is_empty());
        assert!(VersionSet::interval(Bound::Inclusive(v(2, 0, 0)), Bound::Exclusive(v(1, 0, 0))).is_empty());
    }

    #[test]
    fn intersects_a_line() {
        let line = Interval { lo: Bound::Inclusive(v(8, 2, 0)), hi: Bound::Exclusive(v(8, 3, 0)) };
        assert!(VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v(8, 2, 3))).intersects(&line));
        assert!(!VersionSet::interval(Bound::Inclusive(v(8, 3, 0)), Bound::Unbounded).intersects(&line));
    }

    fn bound() -> impl Strategy<Value = Bound> {
        let ver = (0u64..4, 0u64..4, 0u64..4).prop_map(|(a, b, c)| Version::new(a, b, c));
        prop_oneof![Just(Bound::Unbounded), ver.clone().prop_map(Bound::Inclusive), ver.prop_map(Bound::Exclusive)]
    }

    proptest! {
        #[test]
        fn intersect_is_conjunction(a in bound(), b in bound(), c in bound(), d in bound(), x in 0u64..4, y in 0u64..4, z in 0u64..4) {
            let s1 = VersionSet::interval(a, b);
            let s2 = VersionSet::interval(c, d);
            let ver = Version::new(x, y, z);
            prop_assert_eq!(s1.intersect(&s2).contains(ver), s1.contains(ver) && s2.contains(ver));
            prop_assert_eq!(s1.clone().union(s2.clone()).contains(ver), s1.contains(ver) || s2.contains(ver));
        }
    }
}
```

`crates/potter-detect/src/version/partial.rs`, starting with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partials() {
        assert_eq!(parse_partial("8").unwrap(), Partial { major: Some(8), minor: None, patch: None, wildcard: false });
        assert_eq!(parse_partial("v1.2.3").unwrap(), Partial { major: Some(1), minor: Some(2), patch: Some(3), wildcard: false });
        assert_eq!(parse_partial("1.2.x").unwrap(), Partial { major: Some(1), minor: Some(2), patch: None, wildcard: true });
        assert_eq!(parse_partial("*").unwrap(), Partial { major: None, minor: None, patch: None, wildcard: true });
        assert_eq!(parse_partial("1.2.3-beta.1+build").unwrap().patch, Some(3));
        assert_eq!(parse_partial("1.2.3.4").unwrap().patch, Some(3));
        assert_eq!(parse_partial("1.0.x-dev").unwrap(), Partial { major: Some(1), minor: Some(0), patch: None, wildcard: true });
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "a.b", "1..2", "1.2.3.4.5", "-1", "1.2a", "99999999999999999999"] {
            assert!(parse_partial(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn x_ranges() {
        let r = parse_partial("1.2").unwrap().x_range().unwrap();
        assert!(r.contains(Version::new(1, 2, 9)));
        assert!(!r.contains(Version::new(1, 3, 0)));
        let r = parse_partial("1").unwrap().x_range().unwrap();
        assert!(r.contains(Version::new(1, 9, 0)));
        assert!(!r.contains(Version::new(2, 0, 0)));
        assert!(parse_partial("*").unwrap().x_range().unwrap().contains(Version::new(0, 0, 0)));
        assert!(bump(u64::MAX).is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-detect`
Expected: compile errors (missing types).

- [ ] **Step 3: Implement `set.rs` and `partial.rs`**

Put this above the tests in `crates/potter-detect/src/version/set.rs`:

```rust
use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version { major, minor, patch }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Unbounded,
    Inclusive(Version),
    Exclusive(Version),
}

impl Bound {
    fn parts(self) -> Option<(Version, bool)> {
        match self {
            Bound::Unbounded => None,
            Bound::Inclusive(v) => Some((v, false)),
            Bound::Exclusive(v) => Some((v, true)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub lo: Bound,
    pub hi: Bound,
}

impl Interval {
    pub const ALL: Interval = Interval { lo: Bound::Unbounded, hi: Bound::Unbounded };

    pub fn is_empty(&self) -> bool {
        match (self.lo.parts(), self.hi.parts()) {
            (None, _) | (_, None) => false,
            (Some((a, false)), Some((b, false))) => a > b,
            (Some((a, _)), Some((b, _))) => a >= b,
        }
    }

    pub fn contains(&self, v: Version) -> bool {
        let lo_ok = match self.lo {
            Bound::Unbounded => true,
            Bound::Inclusive(a) => v >= a,
            Bound::Exclusive(a) => v > a,
        };
        let hi_ok = match self.hi {
            Bound::Unbounded => true,
            Bound::Inclusive(b) => v <= b,
            Bound::Exclusive(b) => v < b,
        };
        lo_ok && hi_ok
    }

    pub fn intersect(&self, other: &Interval) -> Interval {
        Interval { lo: tighter_lo(self.lo, other.lo), hi: tighter_hi(self.hi, other.hi) }
    }
}

/// The larger lower bound; at equal versions an exclusive bound is tighter.
fn tighter_lo(a: Bound, b: Bound) -> Bound {
    match (a.parts(), b.parts()) {
        (None, _) => b,
        (_, None) => a,
        (Some((va, ea)), Some((vb, _))) => match va.cmp(&vb) {
            Ordering::Greater => a,
            Ordering::Less => b,
            Ordering::Equal => {
                if ea {
                    a
                } else {
                    b
                }
            }
        },
    }
}

/// The smaller upper bound; at equal versions an exclusive bound is tighter.
fn tighter_hi(a: Bound, b: Bound) -> Bound {
    match (a.parts(), b.parts()) {
        (None, _) => b,
        (_, None) => a,
        (Some((va, ea)), Some((vb, _))) => match va.cmp(&vb) {
            Ordering::Less => a,
            Ordering::Greater => b,
            Ordering::Equal => {
                if ea {
                    a
                } else {
                    b
                }
            }
        },
    }
}

/// A union of intervals. Never stores empty intervals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSet {
    intervals: Vec<Interval>,
}

impl VersionSet {
    pub fn any() -> Self {
        VersionSet { intervals: vec![Interval::ALL] }
    }

    pub fn none() -> Self {
        VersionSet { intervals: vec![] }
    }

    pub fn interval(lo: Bound, hi: Bound) -> Self {
        let i = Interval { lo, hi };
        if i.is_empty() { Self::none() } else { VersionSet { intervals: vec![i] } }
    }

    pub fn exact(v: Version) -> Self {
        Self::interval(Bound::Inclusive(v), Bound::Inclusive(v))
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let mut intervals = Vec::new();
        for a in &self.intervals {
            for b in &other.intervals {
                let i = a.intersect(b);
                if !i.is_empty() {
                    intervals.push(i);
                }
            }
        }
        VersionSet { intervals }
    }

    pub fn union(mut self, other: Self) -> Self {
        self.intervals.extend(other.intervals);
        self
    }

    pub fn contains(&self, v: Version) -> bool {
        self.intervals.iter().any(|i| i.contains(v))
    }

    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    pub fn intersects(&self, other: &Interval) -> bool {
        self.intervals.iter().any(|i| !i.intersect(other).is_empty())
    }
}
```

Put this above the tests in `crates/potter-detect/src/version/partial.rs`:

```rust
use super::{Bound, ConstraintError, Version, VersionSet};

/// A version with optional trailing components. `wildcard` records an explicit `x`, `X` or `*`,
/// which composer treats differently from an omitted component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Partial {
    pub major: Option<u64>,
    pub minor: Option<u64>,
    pub patch: Option<u64>,
    pub wildcard: bool,
}

pub(crate) fn bump(n: u64) -> Result<u64, ConstraintError> {
    n.checked_add(1).ok_or_else(|| ConstraintError("version number too large".into()))
}

pub(crate) fn parse_partial(input: &str) -> Result<Partial, ConstraintError> {
    let bad = || ConstraintError(format!("invalid version `{input}`"));
    let mut s = input.trim();
    s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
    // Build metadata, then prerelease/stability suffixes, are ignored.
    if let Some(i) = s.find('+') {
        s = &s[..i];
    }
    if let Some(i) = s.find('-') {
        s = &s[..i];
    }
    if s.is_empty() {
        return Err(bad());
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 4 {
        return Err(bad());
    }
    let mut nums: [Option<u64>; 3] = [None; 3];
    let mut wildcard = false;
    for (i, p) in parts.iter().enumerate().take(3) {
        if matches!(*p, "x" | "X" | "*") {
            wildcard = true;
            continue;
        }
        if wildcard || p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        nums[i] = Some(p.parse().map_err(|_| bad())?);
    }
    if let Some(fourth) = parts.get(3)
        && !(fourth.bytes().all(|b| b.is_ascii_digit()) || matches!(*fourth, "x" | "X" | "*"))
    {
        return Err(bad());
    }
    Ok(Partial { major: nums[0], minor: nums[1], patch: nums[2], wildcard })
}

impl Partial {
    pub fn floor(&self) -> Version {
        Version::new(self.major.unwrap_or(0), self.minor.unwrap_or(0), self.patch.unwrap_or(0))
    }

    /// The first version above everything this partial names: `1` → 2.0.0, `1.2` → 1.3.0, `1.2.3` → 1.2.4.
    pub fn upper_exclusive(&self) -> Result<Option<Version>, ConstraintError> {
        Ok(match (self.major, self.minor, self.patch) {
            (None, _, _) => None,
            (Some(m), None, _) => Some(Version::new(bump(m)?, 0, 0)),
            (Some(m), Some(n), None) => Some(Version::new(m, bump(n)?, 0)),
            (Some(m), Some(n), Some(p)) => Some(Version::new(m, n, bump(p)?)),
        })
    }

    /// npm X-range semantics: missing or wildcard components match anything.
    pub fn x_range(&self) -> Result<VersionSet, ConstraintError> {
        match self.upper_exclusive()? {
            None => Ok(VersionSet::any()),
            Some(hi) => Ok(VersionSet::interval(Bound::Inclusive(self.floor()), Bound::Exclusive(hi))),
        }
    }
}
```

Update `crates/potter-detect/src/lib.rs` so it compiles (the `pub(crate) use` warnings go away once the parsers use them in Task 5):

```rust
//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

#![allow(dead_code)] // Removed in Task 8 once every module is wired in.

pub mod version;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes, including the proptest.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-detect Cargo.lock
git commit -m "feat(potter-detect): version sets and partial versions"
```

---

### Task 5: npm and composer constraint parsers

**Files:**
- Create: `crates/potter-detect/src/version/npm.rs`, `crates/potter-detect/src/version/composer.rs`
- Modify: `crates/potter-detect/src/version/mod.rs`

**Interfaces:**
- Consumes: Task 4's `Partial`, `parse_partial`, `bump` and `VersionSet`.
- Produces:
  - `version::parse_npm_range(&str) -> Result<VersionSet, ConstraintError>`, following node-semver: `||`, hyphen ranges, X-ranges, `~`, `^` (including 0.x), comparators with partials, `v` and `=` prefixes, and a space after an operator.
  - `version::parse_composer_constraint(&str) -> Result<VersionSet, ConstraintError>`, following composer: `||`/`|`, AND by space or comma, `^`, composer `~`, wildcards, exact partials as `.0`, `!=`/`<>`, hyphen ranges and `@stability`. Branch constraints (`dev-*`) are an error.

- [ ] **Step 1: Write the failing tests**

`crates/potter-detect/src/version/npm.rs`, with only tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn check(range: &str, yes: &[(u64, u64, u64)], no: &[(u64, u64, u64)]) {
        let set = parse_npm_range(range).unwrap_or_else(|e| panic!("{range}: {e}"));
        for &(a, b, c) in yes {
            assert!(set.contains(Version::new(a, b, c)), "{range} should contain {a}.{b}.{c}");
        }
        for &(a, b, c) in no {
            assert!(!set.contains(Version::new(a, b, c)), "{range} should not contain {a}.{b}.{c}");
        }
    }

    #[test]
    fn node_semver_readme_examples() {
        check("1.2.3 - 2.3.4", &[(1, 2, 3), (2, 3, 4)], &[(1, 2, 2), (2, 3, 5)]);
        check("1.2 - 2.3.4", &[(1, 2, 0)], &[(1, 1, 9)]);
        check("1.2.3 - 2.3", &[(2, 3, 9)], &[(2, 4, 0)]);
        check("1.2.3 - 2", &[(2, 9, 9)], &[(3, 0, 0)]);
        check("*", &[(0, 0, 0), (99, 0, 0)], &[]);
        check("", &[(1, 0, 0)], &[]);
        check("1.x", &[(1, 0, 0), (1, 9, 9)], &[(2, 0, 0)]);
        check("1.2.x", &[(1, 2, 9)], &[(1, 3, 0)]);
        check("~1.2.3", &[(1, 2, 3), (1, 2, 9)], &[(1, 3, 0), (1, 2, 2)]);
        check("~1.2", &[(1, 2, 0)], &[(1, 3, 0)]);
        check("~1", &[(1, 9, 0)], &[(2, 0, 0)]);
        check("~0.2.3", &[(0, 2, 3)], &[(0, 3, 0)]);
        check("^1.2.3", &[(1, 2, 3), (1, 9, 9)], &[(2, 0, 0), (1, 2, 2)]);
        check("^0.2.3", &[(0, 2, 3)], &[(0, 3, 0)]);
        check("^0.0.3", &[(0, 0, 3)], &[(0, 0, 4)]);
        check("^1.2.x", &[(1, 2, 0)], &[(2, 0, 0)]);
        check("^0.0.x", &[(0, 0, 9)], &[(0, 1, 0)]);
        check("^0.0", &[(0, 0, 9)], &[(0, 1, 0)]);
        check("^1.x", &[(1, 0, 0)], &[(2, 0, 0)]);
        check("^0.x", &[(0, 9, 0)], &[(1, 0, 0)]);
    }

    #[test]
    fn comparators_with_partials() {
        check(">1", &[(2, 0, 0)], &[(1, 9, 9)]);
        check(">1.2", &[(1, 3, 0)], &[(1, 2, 9)]);
        check(">=1.2", &[(1, 2, 0)], &[(1, 1, 9)]);
        check("<1.2", &[(1, 1, 9)], &[(1, 2, 0)]);
        check("<=1.2", &[(1, 2, 9)], &[(1, 3, 0)]);
        check("<=1", &[(1, 9, 9)], &[(2, 0, 0)]);
        check(">=20.11", &[(20, 11, 0), (24, 0, 0)], &[(20, 10, 9)]);
        check("=1.2.3", &[(1, 2, 3)], &[(1, 2, 4)]);
        check("v1.2.3", &[(1, 2, 3)], &[(1, 2, 4)]);
    }

    #[test]
    fn combinations() {
        check(">=1.2.7 <1.3.0", &[(1, 2, 7), (1, 2, 99)], &[(1, 3, 0), (1, 2, 6)]);
        check("1.2.7 || >=1.2.9 <2.0.0", &[(1, 2, 7), (1, 4, 6)], &[(1, 2, 8), (2, 0, 0)]);
        check(">= 18", &[(18, 0, 0)], &[(17, 9, 9)]);
        check("^18 || ^20 || >=22", &[(20, 1, 0), (24, 0, 0)], &[(19, 0, 0), (21, 0, 0)]);
        check(">=1.0.0-beta.1", &[(1, 0, 0)], &[(0, 9, 0)]);
    }

    #[test]
    fn errors() {
        for bad in [">=", "1.2.3 -", "^", "1.2.3 - 2.0 - 3", "latest", ">=abc"] {
            assert!(parse_npm_range(bad).is_err(), "{bad:?}");
        }
    }
}
```

`crates/potter-detect/src/version/composer.rs`, with only tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn check(c: &str, yes: &[(u64, u64, u64)], no: &[(u64, u64, u64)]) {
        let set = parse_composer_constraint(c).unwrap_or_else(|e| panic!("{c}: {e}"));
        for &(a, b, p) in yes {
            assert!(set.contains(Version::new(a, b, p)), "{c} should contain {a}.{b}.{p}");
        }
        for &(a, b, p) in no {
            assert!(!set.contains(Version::new(a, b, p)), "{c} should not contain {a}.{b}.{p}");
        }
    }

    #[test]
    fn composer_docs_examples() {
        check("1.0.2", &[(1, 0, 2)], &[(1, 0, 3)]);
        check(">=1.0", &[(1, 0, 0), (9, 0, 0)], &[(0, 9, 9)]);
        check(">=1.0 <2.0", &[(1, 5, 0)], &[(2, 0, 0)]);
        check(">=1.0,<2.0", &[(1, 5, 0)], &[(2, 0, 0)]);
        check(">=1.0 <1.1 || >=1.2", &[(1, 0, 5), (1, 2, 0)], &[(1, 1, 0)]);
        check("1.0 - 2.0", &[(1, 0, 0), (2, 0, 9)], &[(2, 1, 0)]);
        check("1.0.0 - 2.1.0", &[(2, 1, 0)], &[(2, 1, 1)]);
        check("1.0.*", &[(1, 0, 9)], &[(1, 1, 0)]);
        check("~1.2", &[(1, 2, 0), (1, 9, 0)], &[(2, 0, 0)]);
        check("~1.2.3", &[(1, 2, 3), (1, 2, 9)], &[(1, 3, 0)]);
        check("~1", &[(1, 5, 0)], &[(2, 0, 0)]);
        check("^1.2.3", &[(1, 9, 0)], &[(2, 0, 0), (1, 2, 2)]);
        check("^0.3", &[(0, 3, 9)], &[(0, 4, 0)]);
        check("*", &[(0, 0, 1), (8, 4, 0)], &[]);
    }

    #[test]
    fn php_requirements_seen_in_the_wild() {
        check("^8.2", &[(8, 2, 0), (8, 4, 3)], &[(9, 0, 0), (8, 1, 9)]);
        check(">=8.3 <8.5", &[(8, 3, 0), (8, 4, 9)], &[(8, 5, 0), (8, 2, 9)]);
        check("^8.2 || ^8.3", &[(8, 3, 1)], &[(7, 4, 0)]);
        check("^8.2|^8.3", &[(8, 3, 1)], &[(7, 4, 0)]);
        check("8.2.*", &[(8, 2, 20)], &[(8, 3, 0)]);
        check(">8.2", &[(8, 2, 1)], &[(8, 2, 0)]);
        check("<=8.2", &[(8, 2, 0)], &[(8, 2, 1)]);
        check("!=8.3.0", &[(8, 3, 1), (8, 2, 0)], &[(8, 3, 0)]);
        check("8.2", &[(8, 2, 0)], &[(8, 2, 1)]);
        check(">=8.2@dev", &[(8, 4, 0)], &[(8, 1, 0)]);
        check(">= 8.1", &[(8, 1, 0)], &[(8, 0, 9)]);
    }

    #[test]
    fn errors() {
        for bad in ["", "dev-master", "||", ">=8.2 ||", "^", "abc"] {
            assert!(parse_composer_constraint(bad).is_err(), "{bad:?}");
        }
    }
}
```

Update `crates/potter-detect/src/version/mod.rs`:

```rust
//! Version constraints for runtime selection. Prerelease and build metadata are ignored: potter
//! selects release lines, never prereleases.

mod composer;
mod npm;
mod partial;
mod set;

pub use composer::parse_composer_constraint;
pub use npm::parse_npm_range;
pub(crate) use partial::{Partial, bump, parse_partial};
pub use set::{Bound, Interval, Version, VersionSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConstraintError(pub String);
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-detect version`
Expected: compile errors, "cannot find function `parse_npm_range`".

- [ ] **Step 3: Implement the parsers**

Put this above the tests in `crates/potter-detect/src/version/npm.rs`:

```rust
//! node-semver ranges (https://github.com/npm/node-semver#ranges).

use super::{Bound, ConstraintError, Partial, Version, VersionSet, bump, parse_partial};

pub fn parse_npm_range(input: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::none();
    for part in input.split("||") {
        set = set.union(parse_and(part.trim(), input)?);
    }
    Ok(set)
}

fn err(input: &str, why: &str) -> ConstraintError {
    ConstraintError(format!("`{input}`: {why}"))
}

fn parse_and(s: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    if s.is_empty() {
        return Ok(VersionSet::any());
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.contains(&"-") {
        if words.len() != 3 || words[1] != "-" {
            return Err(err(input, "a hyphen range is `A - B`"));
        }
        return hyphen(parse_partial(words[0])?, parse_partial(words[2])?);
    }
    let mut set = VersionSet::any();
    let mut pending: Option<&str> = None;
    for w in words {
        if is_operator(w) {
            if pending.is_some() {
                return Err(err(input, "two operators in a row"));
            }
            pending = Some(w);
            continue;
        }
        let comparator = match pending.take() {
            Some(op) => format!("{op}{w}"),
            None => w.to_string(),
        };
        set = set.intersect(&comparator_set(&comparator, input)?);
    }
    if pending.is_some() {
        return Err(err(input, "operator without a version"));
    }
    Ok(set)
}

fn is_operator(w: &str) -> bool {
    matches!(w, ">" | ">=" | "<" | "<=" | "=" | "~" | "~>" | "^")
}

fn split_op(c: &str) -> (&str, &str) {
    for op in [">=", "<=", "~>", ">", "<", "=", "~", "^"] {
        if let Some(rest) = c.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", c)
}

fn comparator_set(c: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    let (op, rest) = split_op(c);
    if rest.is_empty() {
        return Err(err(input, "operator without a version"));
    }
    let p = parse_partial(rest)?;
    let floor = p.floor();
    Ok(match op {
        "" | "=" => p.x_range()?,
        "^" => caret(p)?,
        "~" | "~>" => tilde(p)?,
        ">" => match p.upper_exclusive()? {
            None => VersionSet::none(),
            Some(hi) if p.patch.is_none() => VersionSet::interval(Bound::Inclusive(hi), Bound::Unbounded),
            Some(_) => VersionSet::interval(Bound::Exclusive(floor), Bound::Unbounded),
        },
        ">=" => match p.major {
            None => VersionSet::any(),
            Some(_) => VersionSet::interval(Bound::Inclusive(floor), Bound::Unbounded),
        },
        "<" => match p.major {
            None => VersionSet::none(),
            Some(_) => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(floor)),
        },
        "<=" => match p.upper_exclusive()? {
            None => VersionSet::any(),
            Some(hi) if p.patch.is_none() => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(hi)),
            Some(_) => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(floor)),
        },
        _ => unreachable!("split_op only returns listed operators"),
    })
}

/// `^`: allow changes that don't modify the left-most non-zero component.
pub(crate) fn caret(p: Partial) -> Result<VersionSet, ConstraintError> {
    let Some(major) = p.major else { return Ok(VersionSet::any()) };
    let lo = Bound::Inclusive(p.floor());
    let hi = match (major, p.minor, p.patch) {
        (m, _, _) if m > 0 => Version::new(bump(m)?, 0, 0),
        (_, None, _) => Version::new(1, 0, 0),
        (_, Some(n), _) if n > 0 => Version::new(0, bump(n)?, 0),
        (_, Some(_), None) => Version::new(0, 1, 0),
        (_, Some(_), Some(pt)) => Version::new(0, 0, bump(pt)?),
    };
    Ok(VersionSet::interval(lo, Bound::Exclusive(hi)))
}

/// npm `~`: patch-level changes if a minor is given, minor-level changes otherwise.
fn tilde(p: Partial) -> Result<VersionSet, ConstraintError> {
    let Some(major) = p.major else { return Ok(VersionSet::any()) };
    let hi = match p.minor {
        None => Version::new(bump(major)?, 0, 0),
        Some(n) => Version::new(major, bump(n)?, 0),
    };
    Ok(VersionSet::interval(Bound::Inclusive(p.floor()), Bound::Exclusive(hi)))
}

fn hyphen(lo: Partial, hi: Partial) -> Result<VersionSet, ConstraintError> {
    let lower = match lo.major {
        None => Bound::Unbounded,
        Some(_) => Bound::Inclusive(lo.floor()),
    };
    let upper = match (hi.major, hi.patch) {
        (None, _) => Bound::Unbounded,
        (Some(_), Some(_)) => Bound::Inclusive(hi.floor()),
        (Some(_), None) => match hi.upper_exclusive()? {
            Some(v) => Bound::Exclusive(v),
            None => Bound::Unbounded,
        },
    };
    Ok(VersionSet::interval(lower, upper))
}
```

Put this above the tests in `crates/potter-detect/src/version/composer.rs`:

```rust
//! Composer constraints (https://getcomposer.org/doc/articles/versions.md). Partial versions in
//! comparisons and exact constraints are padded with zeros, unlike npm.

use super::npm::caret;
use super::{Bound, ConstraintError, Version, VersionSet, bump, parse_partial};

pub fn parse_composer_constraint(input: &str) -> Result<VersionSet, ConstraintError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ConstraintError("empty constraint".into()));
    }
    let mut set = VersionSet::none();
    for alt in trimmed.split("||").flat_map(|p| p.split('|')) {
        let alt = alt.trim();
        if alt.is_empty() {
            return Err(ConstraintError(format!("`{input}`: empty alternative")));
        }
        set = set.union(parse_and(alt, input)?);
    }
    Ok(set)
}

fn parse_and(s: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    let words: Vec<&str> = s.split([' ', ',']).filter(|w| !w.is_empty()).collect();
    if words.contains(&"-") {
        if words.len() != 3 || words[1] != "-" {
            return Err(ConstraintError(format!("`{input}`: a hyphen range is `A - B`")));
        }
        return hyphen(words[0], words[2]);
    }
    let mut set = VersionSet::any();
    let mut pending: Option<&str> = None;
    for w in words {
        if matches!(w, ">" | ">=" | "<" | "<=" | "=" | "==" | "!=" | "<>" | "~" | "^") {
            pending = Some(w);
            continue;
        }
        let token = match pending.take() {
            Some(op) => format!("{op}{w}"),
            None => w.to_string(),
        };
        set = set.intersect(&token_set(&token, input)?);
    }
    if pending.is_some() {
        return Err(ConstraintError(format!("`{input}`: operator without a version")));
    }
    Ok(set)
}

fn strip_stability(t: &str) -> &str {
    t.split('@').next().unwrap_or(t)
}

fn split_op(c: &str) -> (&str, &str) {
    for op in [">=", "<=", "<>", "!=", "==", ">", "<", "=", "^", "~"] {
        if let Some(rest) = c.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", c)
}

fn token_set(token: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    let token = strip_stability(token);
    if token == "*" {
        return Ok(VersionSet::any());
    }
    let (op, rest) = split_op(token);
    if rest.is_empty() {
        return Err(ConstraintError(format!("`{input}`: operator without a version")));
    }
    if rest.starts_with("dev-") {
        return Err(ConstraintError(format!("`{input}`: branch constraints can't select a runtime version")));
    }
    let p = parse_partial(rest)?;
    let v = p.floor();
    Ok(match op {
        "" | "=" | "==" => {
            if p.wildcard {
                p.x_range()?
            } else {
                VersionSet::exact(v)
            }
        }
        "!=" | "<>" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
            .union(VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded)),
        ">" => VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded),
        ">=" => VersionSet::interval(Bound::Inclusive(v), Bound::Unbounded),
        "<" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v)),
        "<=" => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(v)),
        "^" => caret(p)?,
        "~" => {
            let Some(major) = p.major else { return Ok(VersionSet::any()) };
            let hi = match (p.minor, p.patch) {
                (Some(n), Some(_)) => Version::new(major, bump(n)?, 0),
                _ => Version::new(bump(major)?, 0, 0),
            };
            VersionSet::interval(Bound::Inclusive(v), Bound::Exclusive(hi))
        }
        _ => unreachable!("split_op only returns listed operators"),
    })
}

fn hyphen(a: &str, b: &str) -> Result<VersionSet, ConstraintError> {
    let lo = parse_partial(strip_stability(a))?;
    let hi = parse_partial(strip_stability(b))?;
    let upper = if hi.patch.is_some() {
        Bound::Inclusive(hi.floor())
    } else {
        match hi.upper_exclusive()? {
            Some(v) => Bound::Exclusive(v),
            None => Bound::Unbounded,
        }
    };
    Ok(VersionSet::interval(Bound::Inclusive(lo.floor()), upper))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-detect version && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: all version tests pass.

If `abc` or `latest` doesn't error, check that `parse_partial` rejects non-digit components.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-detect
git commit -m "feat(potter-detect): npm and composer constraint parsers"
```

---

### Task 6: Version files and catalog lines

**Files:**
- Create: `crates/potter-detect/src/version/file.rs`, `crates/potter-detect/src/catalog.rs`, `crates/potter-detect/src/error.rs`
- Modify: `crates/potter-detect/src/version/mod.rs`, `crates/potter-detect/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 4–5.
- Produces:
  - `version::parse_version_file(&str) -> Result<(VersionSet, bool /* lts_only */), ConstraintError>`, which reads `.php-version`, `.nvmrc`, `.node-version`, `.bun-version` and `.dvmrc` content.
  - `catalog::{Line, Runtime, PHP, NODE, BUN, DENO, COMPOSER_VERSION}`.
  - `catalog::VersionSource { desc: String, origin: potter_plan::Origin, set: VersionSet, lts_only: bool }`.
  - `catalog::pick(&'static Runtime, field: &str, Option<VersionSource>, &mut Vec<Decision>) -> Result<&'static Line, DetectError>`.
  - `potter_detect::DetectError` with every variant listed below. Later tasks use exactly these names.
  - `potter_detect::ProviderId` (defined here in `error.rs`, because errors carry it), with `ProviderId::ALL`, `as_str()`, `parse(&str) -> Option<ProviderId>` and `Display`.

- [ ] **Step 1: Write the failing tests**

`crates/potter-detect/src/version/file.rs`, with only tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    #[test]
    fn version_files() {
        let (s, lts) = parse_version_file("8.3\n").unwrap();
        assert!(s.contains(Version::new(8, 3, 9)) && !s.contains(Version::new(8, 4, 0)) && !lts);
        let (s, _) = parse_version_file("v20.11.0\r\n").unwrap();
        assert!(s.contains(Version::new(20, 11, 0)) && !s.contains(Version::new(20, 11, 1)));
        let (s, _) = parse_version_file("\u{feff}22 # pinned for prod\n").unwrap();
        assert!(s.contains(Version::new(22, 5, 0)));
        let (_, lts) = parse_version_file("lts/*").unwrap();
        assert!(lts);
        let (s, lts) = parse_version_file("node").unwrap();
        assert!(s.contains(Version::new(24, 0, 0)) && !lts);
        let (s, _) = parse_version_file("\n\n# comment\n20\n").unwrap();
        assert!(s.contains(Version::new(20, 0, 0)));
    }

    #[test]
    fn version_file_errors() {
        assert!(parse_version_file("lts/iron").unwrap_err().0.contains("lts/iron"));
        assert!(parse_version_file("").is_err());
        assert!(parse_version_file("banana").is_err());
    }
}
```

`crates/potter-detect/src/catalog.rs`, with only tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{parse_composer_constraint, parse_npm_range};
    use potter_plan::Origin;

    fn src(set: crate::version::VersionSet) -> Option<VersionSource> {
        Some(VersionSource { desc: "test".into(), origin: Origin::Default, set, lts_only: false })
    }

    #[test]
    fn highest_satisfying_line() {
        let mut d = vec![];
        assert_eq!(pick(&PHP, "runtime.php", src(parse_composer_constraint("^8.2").unwrap()), &mut d).unwrap().version, "8.4");
        assert_eq!(pick(&PHP, "runtime.php", src(parse_composer_constraint(">=8.2 <8.4").unwrap()), &mut d).unwrap().version, "8.3");
        assert_eq!(pick(&NODE, "runtime.node", src(parse_npm_range(">=20.11").unwrap()), &mut d).unwrap().version, "24");
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn default_without_constraint() {
        let mut d = vec![];
        assert_eq!(pick(&NODE, "runtime.node", None, &mut d).unwrap().version, "22");
        assert_eq!(d[0].origin, Origin::Default);
    }

    #[test]
    fn lts_only() {
        let mut d = vec![];
        let s = VersionSource { desc: ".nvmrc = lts/*".into(), origin: Origin::Default, set: crate::version::VersionSet::any(), lts_only: true };
        assert_eq!(pick(&NODE, "runtime.node", Some(s), &mut d).unwrap().version, "24");
    }

    #[test]
    fn unsatisfiable_lists_available() {
        let mut d = vec![];
        let e = pick(&PHP, "runtime.php", src(parse_composer_constraint("^7.4").unwrap()), &mut d).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("8.2, 8.3, 8.4"), "{msg}");
        assert!(msg.contains("test"), "{msg}");
    }

    #[test]
    fn provider_ids_round_trip() {
        for id in crate::ProviderId::ALL {
            assert_eq!(crate::ProviderId::parse(id.as_str()), Some(id));
        }
        assert_eq!(crate::ProviderId::parse("cobol"), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-detect`
Expected: compile errors.

- [ ] **Step 3: Implement `file.rs`, `error.rs` and `catalog.rs`**

Put this above the tests in `crates/potter-detect/src/version/file.rs`:

```rust
//! Single-version files: `.php-version`, `.nvmrc`, `.node-version`, `.bun-version`, `.dvmrc`.

use super::{ConstraintError, VersionSet, parse_partial};

/// Returns the version set and whether only LTS lines qualify (`lts/*`).
pub fn parse_version_file(content: &str) -> Result<(VersionSet, bool), ConstraintError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let line = content
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .find(|l| !l.is_empty())
        .ok_or_else(|| ConstraintError("the version file is empty".into()))?;
    let lower = line.to_ascii_lowercase();
    match lower.as_str() {
        "lts/*" => Ok((VersionSet::any(), true)),
        "node" | "latest" | "current" | "stable" => Ok((VersionSet::any(), false)),
        l if l.starts_with("lts/") => {
            Err(ConstraintError(format!("`{line}` names an LTS codename; use a major version such as 22 instead")))
        }
        _ => Ok((parse_partial(line)?.x_range()?, false)),
    }
}
```

`crates/potter-detect/src/error.rs`:

```rust
use std::fmt;

use thiserror::Error;

use crate::repo::RepoError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderId {
    Php,
    Laravel,
    Node,
    Bun,
    Deno,
    Static,
}

impl ProviderId {
    pub const ALL: [ProviderId; 6] =
        [ProviderId::Php, ProviderId::Laravel, ProviderId::Node, ProviderId::Bun, ProviderId::Deno, ProviderId::Static];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Php => "php",
            ProviderId::Laravel => "laravel",
            ProviderId::Node => "node",
            ProviderId::Bun => "bun",
            ProviderId::Deno => "deno",
            ProviderId::Static => "static",
        }
    }

    pub fn parse(s: &str) -> Option<ProviderId> {
        ProviderId::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error)]
pub enum DetectError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error("{}", no_provider(.candidates))]
    NoProvider { candidates: Vec<String> },
    #[error(
        "{} all claim the build root; choose one with `provider = \"…\"` under [target.<name>] in potter.toml, or split the repo into [target.*] entries",
        join(.providers)
    )]
    Ambiguous { providers: Vec<ProviderId> },
    #[error("provider `{0}` is set in potter.toml but does not apply to this directory")]
    ForcedNotApplicable(ProviderId),
    #[error("{provider}: {file} is required: {hint}")]
    MissingFile { provider: ProviderId, file: String, hint: String },
    #[error("{file}: {reason}")]
    Parse { file: String, reason: String },
    #[error("invalid {what} constraint `{constraint}` ({source_desc}): {reason}")]
    BadConstraint { what: &'static str, constraint: String, source_desc: String, reason: String },
    #[error("no available {runtime} version satisfies {source_desc} (available: {available})")]
    Unsatisfiable { runtime: &'static str, source_desc: String, available: String },
    #[error("{provider}: no web process found: {hint}")]
    NoProcess { provider: ProviderId, hint: String },
    #[error("{provider}: {message}; set `{key}` in potter.toml")]
    NeedsConfig { provider: ProviderId, message: String, key: String },
}

fn join(ids: &[ProviderId]) -> String {
    ids.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
}

fn no_provider(candidates: &[String]) -> String {
    let mut s = String::from("no provider recognises the build root");
    if !candidates.is_empty() {
        let dirs: Vec<String> = candidates.iter().map(|c| format!("{c}/")).collect();
        s.push_str(&format!(
            "; found manifests in {}. Declare each as a target in potter.toml, e.g. [target.{}] root = \"{}\"",
            dirs.join(", "),
            candidates[0].replace('/', "-"),
            candidates[0]
        ));
    }
    s
}
```

Put this above the tests in `crates/potter-detect/src/catalog.rs`:

```rust
//! Runtime version lines available until the real catalog (P3) replaces this table (spec §7).

use potter_plan::{Decision, Origin};

use crate::error::DetectError;
use crate::version::{Bound, Interval, Version, VersionSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub version: &'static str,
    pub major: u64,
    pub minor: Option<u64>,
    pub lts: bool,
}

impl Line {
    /// Every version this line can resolve to.
    pub fn interval(&self) -> Interval {
        match self.minor {
            Some(m) => Interval {
                lo: Bound::Inclusive(Version::new(self.major, m, 0)),
                hi: Bound::Exclusive(Version::new(self.major, m + 1, 0)),
            },
            None => Interval {
                lo: Bound::Inclusive(Version::new(self.major, 0, 0)),
                hi: Bound::Exclusive(Version::new(self.major + 1, 0, 0)),
            },
        }
    }
}

#[derive(Debug)]
pub struct Runtime {
    pub name: &'static str,
    pub lines: &'static [Line],
    pub default: &'static str,
}

const fn line(version: &'static str, major: u64, minor: Option<u64>, lts: bool) -> Line {
    Line { version, major, minor, lts }
}

pub static PHP: Runtime = Runtime {
    name: "php",
    lines: &[line("8.2", 8, Some(2), false), line("8.3", 8, Some(3), false), line("8.4", 8, Some(4), false)],
    default: "8.4",
};

pub static NODE: Runtime =
    Runtime { name: "node", lines: &[line("20", 20, None, true), line("22", 22, None, true), line("24", 24, None, true)], default: "22" };

pub static BUN: Runtime = Runtime { name: "bun", lines: &[line("1", 1, None, false)], default: "1" };

pub static DENO: Runtime = Runtime { name: "deno", lines: &[line("2", 2, None, false)], default: "2" };

pub const COMPOSER_VERSION: &str = "2";

/// Where a version requirement came from, for `--explain` and error messages.
#[derive(Debug, Clone)]
pub struct VersionSource {
    /// Human-readable origin, e.g. `composer.json require.php = ^8.2`.
    pub desc: String,
    pub origin: Origin,
    pub set: VersionSet,
    pub lts_only: bool,
}

/// The highest line satisfying the source, or the runtime's default when there is no source.
pub fn pick(
    rt: &'static Runtime,
    field: &str,
    source: Option<VersionSource>,
    decisions: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let Some(src) = source else {
        let line = rt.lines.iter().find(|l| l.version == rt.default).expect("default is one of the lines");
        decisions.push(Decision::default(field, line.version));
        return Ok(line);
    };
    let best = rt
        .lines
        .iter()
        .filter(|l| (!src.lts_only || l.lts) && src.set.intersects(&l.interval()))
        .max_by_key(|l| (l.major, l.minor));
    match best {
        Some(line) => {
            decisions.push(Decision { field: field.to_string(), value: line.version.to_string(), origin: src.origin });
            Ok(line)
        }
        None => Err(DetectError::Unsatisfiable {
            runtime: rt.name,
            source_desc: src.desc,
            available: rt.lines.iter().map(|l| l.version).collect::<Vec<_>>().join(", "),
        }),
    }
}
```

`error.rs` refers to `crate::repo::RepoError`, which Task 7 creates. Create a stub now so this task compiles. Task 7 replaces it.

`crates/potter-detect/src/repo.rs`:

```rust
//! Replaced in Task 7.

#[derive(Debug, thiserror::Error)]
#[error("repository error")]
pub struct RepoError;
```

Update `crates/potter-detect/src/version/mod.rs` to add:

```rust
mod file;
pub use file::parse_version_file;
```

Update `crates/potter-detect/src/lib.rs`:

```rust
//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

#![allow(dead_code)] // Removed in Task 8 once every module is wired in.

pub mod catalog;
mod error;
pub mod repo;
pub mod version;

pub use error::{DetectError, ProviderId};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-detect
git commit -m "feat(potter-detect): version files, catalog lines and detection errors"
```

---

### Task 7: `RepoView`, JSONC and text scanners

**Files:**
- Modify: `crates/potter-detect/src/repo.rs` (replace the stub)
- Create: `crates/potter-detect/src/jsonc.rs`, `crates/potter-detect/src/scan.rs`
- Modify: `crates/potter-detect/src/lib.rs`

**Interfaces:**
- Produces:
  - `RepoView::open(&Path) -> Result<RepoView, RepoError>`.
  - `RepoView` predicates: `is_file(&str) -> bool`, `is_dir(&str) -> bool` and `files() -> impl Iterator<Item = &str>`.
  - Readers:
    - `read(&str) -> Result<Option<Vec<u8>>, RepoError>` (4 MiB cap) and `read_capped(&str, u64)`;
    - `read_string(&str, cap: u64) -> Result<Option<String>, RepoError>` (strips a UTF-8 BOM);
    - `read_json(&str) -> Result<Option<Value>, RepoError>` and `read_json_capped(&str, u64)`;
    - `read_jsonc(&str) -> Result<Option<Value>, RepoError>`.
  - `manifest_dirs() -> Vec<String>`: depth-1 directories with a known manifest.
  - Constants `MAX_ENTRIES`, `MAX_DEPTH`, `READ_CAP`, `LOCKFILE_CAP` and `DEFAULT_EXCLUDES`.
  - `RepoError::{Io { path, source }, NotADirectory(String), TooManyEntries, TooLarge { path, size, cap }, Symlink(String), NotUtf8 { path }, Json { path, reason }, BadPath(String)}`.
  - `jsonc::strip_jsonc(&str) -> String`.
  - `scan::quoted_value_after(text: &str, key: &str, sep: char) -> Option<String>`: the first quoted string after `key`, any closing quote, whitespace, `sep` and more whitespace.

- [ ] **Step 1: Write the failing tests**

Write `crates/potter-detect/src/repo.rs` with only this test module (it replaces the stub):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("composer.json"), "{\"require\":{}}").unwrap();
        fs::create_dir_all(d.path().join("public")).unwrap();
        fs::write(d.path().join("public/index.php"), "<?php").unwrap();
        fs::create_dir_all(d.path().join("node_modules/x")).unwrap();
        fs::write(d.path().join("node_modules/x/package.json"), "{}").unwrap();
        fs::create_dir_all(d.path().join("frontend")).unwrap();
        fs::write(d.path().join("frontend/package.json"), "{}").unwrap();
        fs::create_dir_all(d.path().join("backend")).unwrap();
        fs::write(d.path().join("backend/go.mod"), "module x").unwrap();
        d
    }

    #[test]
    fn walks_and_excludes() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        assert!(r.is_file("composer.json"));
        assert!(r.is_file("public/index.php"));
        assert!(r.is_dir("public"));
        assert!(!r.is_file("node_modules/x/package.json"));
        assert!(!r.is_dir("node_modules"));
    }

    #[test]
    fn manifest_dirs_for_monorepo_hint() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        assert_eq!(r.manifest_dirs(), vec!["backend".to_string(), "frontend".to_string()]);
    }

    #[test]
    fn reads_json_and_missing_files() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        assert!(r.read_json("composer.json").unwrap().unwrap()["require"].is_object());
        assert!(r.read_json("package.json").unwrap().is_none());
    }

    #[test]
    fn bom_is_accepted() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), "\u{feff}{\"name\":\"x\"}").unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert_eq!(r.read_json("package.json").unwrap().unwrap()["name"], "x");
    }

    #[test]
    fn invalid_json_names_file() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), "{nope").unwrap();
        let r = RepoView::open(d.path()).unwrap();
        let e = r.read_json("package.json").unwrap_err();
        assert!(matches!(&e, RepoError::Json { path, .. } if path == "package.json"), "{e}");
    }

    #[test]
    fn non_utf8_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".nvmrc"), [0xff, 0xfe, b'2']).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert!(matches!(r.read_string(".nvmrc", READ_CAP), Err(RepoError::NotUtf8 { .. })));
    }

    #[test]
    fn oversized_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), vec![b' '; 2048]).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        let e = r.read_capped("package.json", 1024).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("package.json") && msg.contains("1024"), "{msg}");
    }

    #[test]
    fn lockfile_cap_allows_large_lockfiles() {
        let d = tempfile::tempdir().unwrap();
        let big = format!("{{\"packages\":[],\"pad\":\"{}\"}}", "x".repeat(5 * 1024 * 1024));
        fs::write(d.path().join("composer.lock"), big).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert!(r.read_json("composer.lock").is_err());
        assert!(r.read_json_capped("composer.lock", LOCKFILE_CAP).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.json"), "{}").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.json"), d.path().join("composer.json")).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert!(!r.is_file("composer.json"));
        assert!(matches!(r.read("composer.json"), Err(RepoError::Symlink(p)) if p == "composer.json"));
    }

    #[test]
    fn bad_paths_are_rejected() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        for p in ["../x", "/etc/passwd", "a//b", "./composer.json", ""] {
            assert!(matches!(r.read(p), Err(RepoError::BadPath(_))), "{p:?}");
        }
    }

    #[test]
    fn root_must_be_a_directory() {
        let d = tree();
        assert!(matches!(RepoView::open(&d.path().join("composer.json")), Err(RepoError::NotADirectory(_))));
        assert!(matches!(RepoView::open(&d.path().join("missing")), Err(RepoError::Io { .. })));
    }
}
```

`crates/potter-detect/src/jsonc.rs`, with only tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::strip_jsonc;

    fn parse(s: &str) -> serde_json::Value {
        serde_json::from_str(&strip_jsonc(s)).unwrap()
    }

    #[test]
    fn comments_and_trailing_commas() {
        let v = parse("{\n // line\n \"a\": 1, /* block */ \"b\": [1, 2,],\n \"c\": {\"d\": \"x\",},\n}");
        assert_eq!(v, serde_json::json!({"a": 1, "b": [1, 2], "c": {"d": "x"}}));
    }

    #[test]
    fn strings_are_untouched() {
        let v = parse(r#"{"url": "http://x//y", "glob": "/* not a comment */", "q": "a\"//b", "t": ",}"}"#);
        assert_eq!(v["url"], "http://x//y");
        assert_eq!(v["glob"], "/* not a comment */");
        assert_eq!(v["q"], "a\"//b");
        assert_eq!(v["t"], ",}");
    }

    #[test]
    fn comma_then_comment_then_brace() {
        assert_eq!(parse("{\"a\": 1, // last\n}"), serde_json::json!({"a": 1}));
    }
}
```

`crates/potter-detect/src/scan.rs`, with only tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::quoted_value_after;

    #[test]
    fn finds_values() {
        assert_eq!(quoted_value_after("module.exports = { output: 'standalone' }", "output", ':').as_deref(), Some("standalone"));
        assert_eq!(quoted_value_after("const c: NextConfig = {\n  output: \"standalone\",\n}", "output", ':').as_deref(), Some("standalone"));
        assert_eq!(quoted_value_after("'server' => env('OCTANE_SERVER', 'swoole'),", "OCTANE_SERVER", ',').as_deref(), Some("swoole"));
        assert_eq!(quoted_value_after("build: { outDir: 'build' }", "outDir", ':').as_deref(), Some("build"));
        assert_eq!(quoted_value_after("output: process.env.X", "output", ':'), None);
        assert_eq!(quoted_value_after("nothing here", "output", ':'), None);
    }

    #[test]
    fn skips_non_matching_occurrences() {
        let text = "// output is configured below\nexport default { output: 'standalone' }";
        assert_eq!(quoted_value_after(text, "output", ':').as_deref(), Some("standalone"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-detect`
Expected: compile errors.

- [ ] **Step 3: Implement**

Put this above the tests in `crates/potter-detect/src/repo.rs`:

```rust
//! A bounded, read-only view of a build root (spec §6.1). Symlinks are recorded but never
//! followed, default-excluded directories are never walked, and reads are size-capped.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::jsonc::strip_jsonc;

pub const MAX_ENTRIES: usize = 200_000;
pub const MAX_DEPTH: usize = 64;
pub const READ_CAP: u64 = 4 * 1024 * 1024;
pub const LOCKFILE_CAP: u64 = 64 * 1024 * 1024;
pub const DEFAULT_EXCLUDES: &[&str] = &[".git", "node_modules", "vendor", "target", ".venv", "__pycache__", ".potter"];

const MANIFESTS: &[&str] = &[
    "composer.json",
    "package.json",
    "deno.json",
    "deno.jsonc",
    "go.mod",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "Gemfile",
];

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error("more than {MAX_ENTRIES} entries under the build root; narrow it with `root` in potter.toml")]
    TooManyEntries,
    #[error("{path} is {size} bytes, over the {cap}-byte read limit")]
    TooLarge { path: String, size: u64, cap: u64 },
    #[error("{0} is a symlink; potter doesn't follow symlinks when reading build files")]
    Symlink(String),
    #[error("{path} is not valid UTF-8")]
    NotUtf8 { path: String },
    #[error("{path}: invalid JSON: {reason}")]
    Json { path: String, reason: String },
    #[error("invalid repository path `{0}`")]
    BadPath(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> RepoError + '_ {
    move |source| RepoError::Io { path: path.display().to_string(), source }
}

#[derive(Debug)]
pub struct RepoView {
    root: PathBuf,
    files: BTreeSet<String>,
    dirs: BTreeSet<String>,
    symlinks: BTreeSet<String>,
}

impl RepoView {
    pub fn open(root: &Path) -> Result<Self, RepoError> {
        let meta = fs::metadata(root).map_err(io(root))?;
        if !meta.is_dir() {
            return Err(RepoError::NotADirectory(root.display().to_string()));
        }
        let mut view =
            RepoView { root: root.to_path_buf(), files: BTreeSet::new(), dirs: BTreeSet::new(), symlinks: BTreeSet::new() };
        let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
        let mut count = 0usize;
        while let Some((dir, rel, depth)) = stack.pop() {
            for entry in fs::read_dir(&dir).map_err(io(&dir))? {
                let entry = entry.map_err(io(&dir))?;
                count += 1;
                if count > MAX_ENTRIES {
                    return Err(RepoError::TooManyEntries);
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
                let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                let path = entry.path();
                let ft = entry.file_type().map_err(io(&path))?;
                if ft.is_symlink() {
                    view.symlinks.insert(child);
                } else if ft.is_dir() {
                    if DEFAULT_EXCLUDES.contains(&name.as_str()) {
                        continue;
                    }
                    view.dirs.insert(child.clone());
                    if depth + 1 < MAX_DEPTH {
                        stack.push((path, child, depth + 1));
                    }
                } else if ft.is_file() {
                    view.files.insert(child);
                }
            }
        }
        Ok(view)
    }

    fn check(rel: &str) -> Result<(), RepoError> {
        let bad = rel.is_empty()
            || rel.starts_with('/')
            || rel.contains('\0')
            || rel.split('/').any(|c| c.is_empty() || c == "." || c == "..");
        if bad { Err(RepoError::BadPath(rel.to_string())) } else { Ok(()) }
    }

    pub fn is_file(&self, rel: &str) -> bool {
        self.files.contains(rel)
    }

    pub fn is_dir(&self, rel: &str) -> bool {
        self.dirs.contains(rel)
    }

    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(String::as_str)
    }

    pub fn read(&self, rel: &str) -> Result<Option<Vec<u8>>, RepoError> {
        self.read_capped(rel, READ_CAP)
    }

    pub fn read_capped(&self, rel: &str, cap: u64) -> Result<Option<Vec<u8>>, RepoError> {
        Self::check(rel)?;
        if self.symlinks.contains(rel) {
            return Err(RepoError::Symlink(rel.to_string()));
        }
        if !self.files.contains(rel) {
            return Ok(None);
        }
        let path = self.root.join(rel);
        let file = open_no_follow(&path).map_err(io(&path))?;
        let size = file.metadata().map_err(io(&path))?.len();
        if size > cap {
            return Err(RepoError::TooLarge { path: rel.to_string(), size, cap });
        }
        let mut buf = Vec::new();
        file.take(cap + 1).read_to_end(&mut buf).map_err(io(&path))?;
        if buf.len() as u64 > cap {
            return Err(RepoError::TooLarge { path: rel.to_string(), size: buf.len() as u64, cap });
        }
        Ok(Some(buf))
    }

    pub fn read_string(&self, rel: &str, cap: u64) -> Result<Option<String>, RepoError> {
        let Some(bytes) = self.read_capped(rel, cap)? else { return Ok(None) };
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").map(<[u8]>::to_vec).unwrap_or(bytes);
        String::from_utf8(bytes).map(Some).map_err(|_| RepoError::NotUtf8 { path: rel.to_string() })
    }

    pub fn read_json(&self, rel: &str) -> Result<Option<Value>, RepoError> {
        self.read_json_capped(rel, READ_CAP)
    }

    pub fn read_json_capped(&self, rel: &str, cap: u64) -> Result<Option<Value>, RepoError> {
        let Some(text) = self.read_string(rel, cap)? else { return Ok(None) };
        serde_json::from_str(&text).map(Some).map_err(|e| RepoError::Json { path: rel.to_string(), reason: e.to_string() })
    }

    pub fn read_jsonc(&self, rel: &str) -> Result<Option<Value>, RepoError> {
        let Some(text) = self.read_string(rel, READ_CAP)? else { return Ok(None) };
        serde_json::from_str(&strip_jsonc(&text))
            .map(Some)
            .map_err(|e| RepoError::Json { path: rel.to_string(), reason: e.to_string() })
    }

    /// Depth-1 directories containing a known manifest; used to hint at monorepo targets.
    pub fn manifest_dirs(&self) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for f in &self.files {
            if let Some((dir, name)) = f.split_once('/')
                && !name.contains('/')
                && MANIFESTS.contains(&name)
            {
                out.insert(dir.to_string());
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}
```

Put this above the tests in `crates/potter-detect/src/jsonc.rs`:

```rust
//! JSON with comments and trailing commas (deno.jsonc) → strict JSON.

pub fn strip_jsonc(input: &str) -> String {
    strip_trailing_commas(&strip_comments(input))
}

fn strip_comments(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn strip_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut in_str = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|ch| !ch.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}
```

Put this above the tests in `crates/potter-detect/src/scan.rs`:

```rust
//! Minimal text scanning for values in JS/PHP config files. These are heuristics over source
//! text, never evaluation (P9): a value that isn't a literal string is treated as unknown.

/// For each occurrence of `key`: skip a closing quote if present, whitespace, `sep`, whitespace,
/// then read a single- or double-quoted string. Returns the first occurrence that matches.
pub fn quoted_value_after(text: &str, key: &str, sep: char) -> Option<String> {
    let mut from = 0;
    while let Some(pos) = text[from..].find(key) {
        let start = from + pos + key.len();
        from = start;
        let rest = &text[start..];
        let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest).trim_start();
        let Some(rest) = rest.strip_prefix(sep) else { continue };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') else { continue };
        let body = &rest[1..];
        if let Some(end) = body.find(quote) {
            return Some(body[..end].to_string());
        }
    }
    None
}
```

Update `crates/potter-detect/src/lib.rs`:

```rust
//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

#![allow(dead_code)] // Removed in Task 8 once every module is wired in.

pub mod catalog;
mod error;
mod jsonc;
pub mod repo;
pub(crate) mod scan;
pub mod version;

pub use error::{DetectError, ProviderId};
pub use repo::{RepoError, RepoView};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes. `lockfile_cap_allows_large_lockfiles` writes 5 MiB to a tempdir, which is fine.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-detect
git commit -m "feat(potter-detect): bounded RepoView, JSONC and text scanners"
```

---

### Task 8: Provider framework, claims and selection

**Files:**
- Create: `crates/potter-detect/src/options.rs`, `crates/potter-detect/src/select.rs`, `crates/potter-detect/src/providers/mod.rs`, `crates/potter-detect/src/providers/common.rs`, `crates/potter-detect/tests/common/mod.rs`, `crates/potter-detect/tests/select.rs`
- Modify: `crates/potter-detect/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 4–7.
- Produces:
  - `DetectOptions { provider: Option<ProviderId>, port: Option<u16>, runtime: RuntimeOverrides, php: PhpOptions, node: NodeOptions, key_prefix: String }` and `DetectOptions::key(&self, rest: &str) -> String`, which builds `"{key_prefix}.{rest}"` and defaults the prefix to `target.app`.
  - `RuntimeOverrides { php, node, bun, deno: Option<String>, extensions: Vec<String> }`.
  - `PhpOptions { octane_server, nginx_conf, fpm_pool: Option<String> }` and `NodeOptions { package_manager: Option<String> }`.
  - `ClaimKind::{Backend, JsServer, JsAssets}` and `Claim { provider, refines: Option<ProviderId>, kind, evidence: Vec<String> }`.
  - `select(&[Claim], &RepoView) -> Result<ProviderId, DetectError>`.
  - `trait Provider: Sync { fn id(&self) -> ProviderId; fn claim(&self, &RepoView, &DetectOptions) -> Result<Option<Claim>, DetectError>; fn build(&self, &RepoView, &DetectOptions, target: &str) -> Result<Detection, DetectError>; }`.
  - `Detection { target: potter_plan::Target, decisions: Vec<Decision> }`.
  - `providers::all() -> Vec<&'static dyn Provider>` and `providers::get(ProviderId) -> Option<&'static dyn Provider>`.
  - `claims(&RepoView, &DetectOptions) -> Result<Vec<Claim>, DetectError>` and `detect(&RepoView, &DetectOptions, target: &str) -> Result<Detection, DetectError>`. `detect` prepends a `provider` decision.
- Helpers in `providers::common`, used by every provider:
  - `inputs(name, Vec<String>) -> Step`, `source() -> Step` and `run(name, Command, env: &[(&str, &str)], cache: &[(&str, &str)]) -> Step`;
  - `cref(name, Option<&str>) -> CatalogRef`, `export(path, dest) -> Export` and `new_target(name, ProviderId, build_layers, steps, Image) -> Target`;
  - `insert_after(&mut Vec<Step>, after: &str, Vec<Step>)` and `port(&DetectOptions, default: u16, &mut Vec<Decision>) -> u16`;
  - `version_from_config(Option<&str>, key: &str, what) -> Result<Option<VersionSource>, DetectError>` and `version_from_file(&RepoView, rel, what) -> Result<Option<VersionSource>, DetectError>`;
  - `bad_constraint(what, constraint, source, ConstraintError) -> DetectError` and `require_repo_file(&RepoView, rel, ProviderId, key) -> Result<(), DetectError>`.

- [ ] **Step 1: Write the failing tests**

`crates/potter-detect/tests/common/mod.rs`:

```rust
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use potter_detect::{DetectError, DetectOptions, Detection, RepoView};
use potter_plan::{Base, Decision, Step, Target};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

pub fn detect_at(path: &Path, opts: &DetectOptions) -> Result<Detection, DetectError> {
    let repo = RepoView::open(path)?;
    potter_detect::detect(&repo, opts, "app")
}

pub fn detect(name: &str) -> Detection {
    detect_at(&fixture(name), &DetectOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

pub fn detect_err(name: &str) -> DetectError {
    match detect_at(&fixture(name), &DetectOptions::default()) {
        Ok(_) => panic!("{name}: detection should fail"),
        Err(e) => e,
    }
}

pub fn temp_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, c) in files {
        let path = d.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, c).unwrap();
    }
    d
}

pub fn layers(base: &Base) -> Vec<String> {
    match base {
        Base::Catalog { layers } => layers.iter().map(|l| l.to_string()).collect(),
        other => panic!("expected a catalog base, got {other:?}"),
    }
}

pub fn step_names(t: &Target) -> Vec<&str> {
    t.build.steps.iter().map(|s| s.name()).collect()
}

pub fn step<'a>(t: &'a Target, name: &str) -> &'a Step {
    t.build.steps.iter().find(|s| s.name() == name).unwrap_or_else(|| panic!("no step {name} in {:?}", step_names(t)))
}

pub fn command(t: &Target, name: &str) -> String {
    match step(t, name) {
        Step::Run { command, .. } => command.to_string(),
        other => panic!("{name} is a {} step", other.kind()),
    }
}

pub fn includes(t: &Target, name: &str) -> Vec<String> {
    match step(t, name) {
        Step::Inputs { include, .. } => include.clone(),
        other => panic!("{name} is a {} step", other.kind()),
    }
}

pub fn process(t: &Target, name: &str) -> String {
    t.image.processes.get(name).unwrap_or_else(|| panic!("no process {name}")).command.join(" ")
}

pub fn decision<'a>(d: &'a Detection, field: &str) -> &'a Decision {
    d.decisions.iter().find(|x| x.field == field).unwrap_or_else(|| panic!("no decision for {field}: {:#?}", d.decisions))
}
```

`crates/potter-detect/tests/select.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, ProviderId};

#[test]
fn no_provider_lists_manifest_dirs() {
    let d = temp_repo(&[("frontend/package.json", "{}"), ("backend/go.mod", "module x\n"), ("README.md", "hi")]);
    let e = detect_at(d.path(), &DetectOptions::default()).unwrap_err();
    let msg = e.to_string();
    assert!(matches!(e, DetectError::NoProvider { .. }), "{msg}");
    assert!(msg.contains("backend/") && msg.contains("frontend/"), "{msg}");
    assert!(msg.contains("[target.backend] root = \"backend\""), "{msg}");
}

#[test]
fn empty_dir_has_no_hint() {
    let d = temp_repo(&[("README.md", "hi")]);
    let msg = detect_at(d.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert_eq!(msg, "no provider recognises the build root");
}

#[test]
fn forced_provider_must_apply() {
    let d = temp_repo(&[("README.md", "hi")]);
    let opts = DetectOptions { provider: Some(ProviderId::Laravel), ..Default::default() };
    assert!(matches!(detect_at(d.path(), &opts), Err(DetectError::ForcedNotApplicable(ProviderId::Laravel))));
}
```

Create `crates/potter-detect/src/select.rs` with only these unit tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn c(provider: ProviderId, refines: Option<ProviderId>, kind: ClaimKind) -> Claim {
        Claim { provider, refines, kind, evidence: vec![] }
    }

    fn repo() -> (tempfile::TempDir, RepoView) {
        let d = tempfile::tempdir().unwrap();
        let r = RepoView::open(d.path()).unwrap();
        (d, r)
    }

    #[test]
    fn refinement_removes_the_general_claim() {
        let (_d, r) = repo();
        let claims = [c(ProviderId::Php, None, ClaimKind::Backend), c(ProviderId::Laravel, Some(ProviderId::Php), ClaimKind::Backend)];
        assert_eq!(select(&claims, &r).unwrap(), ProviderId::Laravel);
    }

    #[test]
    fn backends_absorb_asset_only_js() {
        let (_d, r) = repo();
        let claims = [
            c(ProviderId::Php, None, ClaimKind::Backend),
            c(ProviderId::Laravel, Some(ProviderId::Php), ClaimKind::Backend),
            c(ProviderId::Node, None, ClaimKind::JsAssets),
            c(ProviderId::Static, Some(ProviderId::Node), ClaimKind::JsAssets),
        ];
        assert_eq!(select(&claims, &r).unwrap(), ProviderId::Laravel);
    }

    #[test]
    fn a_js_server_competes_with_a_backend() {
        let (_d, r) = repo();
        let claims = [c(ProviderId::Laravel, None, ClaimKind::Backend), c(ProviderId::Node, None, ClaimKind::JsServer)];
        let e = select(&claims, &r).unwrap_err();
        assert!(matches!(&e, DetectError::Ambiguous { providers } if providers == &vec![ProviderId::Laravel, ProviderId::Node]));
        assert!(e.to_string().starts_with("laravel, node all claim the build root"));
    }

    #[test]
    fn a_static_site_alone_is_chosen() {
        let (_d, r) = repo();
        let claims = [c(ProviderId::Node, None, ClaimKind::JsAssets), c(ProviderId::Static, Some(ProviderId::Node), ClaimKind::JsAssets)];
        assert_eq!(select(&claims, &r).unwrap(), ProviderId::Static);
    }

    #[test]
    fn nothing_claims() {
        let (_d, r) = repo();
        assert!(matches!(select(&[], &r), Err(DetectError::NoProvider { .. })));
    }

    #[test]
    fn option_keys() {
        assert_eq!(DetectOptions::default().key("runtime.php"), "target.app.runtime.php");
        let o = DetectOptions { key_prefix: "target.api".into(), ..Default::default() };
        assert_eq!(o.key("port"), "target.api.port");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-detect`
Expected: compile errors (`select`, `Claim`, `DetectOptions` and others are missing).

- [ ] **Step 3: Implement**

`crates/potter-detect/src/options.rs`:

```rust
//! Inputs to detection that come from potter.toml (spec §6.2).

use crate::ProviderId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectOptions {
    /// `provider = "…"`: skip selection and use this provider.
    pub provider: Option<ProviderId>,
    /// `[target.x] port` (or the `[target.x.php] port` alias).
    pub port: Option<u16>,
    pub runtime: RuntimeOverrides,
    pub php: PhpOptions,
    pub node: NodeOptions,
    /// The potter.toml key prefix used in decisions and hints, e.g. `target.api`. Empty means `target.app`.
    pub key_prefix: String,
}

impl DetectOptions {
    pub fn key(&self, rest: &str) -> String {
        let prefix = if self.key_prefix.is_empty() { "target.app" } else { &self.key_prefix };
        format!("{prefix}.{rest}")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    pub php: Option<String>,
    pub extensions: Vec<String>,
    pub node: Option<String>,
    pub bun: Option<String>,
    pub deno: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhpOptions {
    pub octane_server: Option<String>,
    pub nginx_conf: Option<String>,
    pub fpm_pool: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeOptions {
    pub package_manager: Option<String>,
}
```

Put this above the tests in `crates/potter-detect/src/select.rs`:

```rust
//! Choosing one provider for a build root (spec §6.1 "Selection").

use std::collections::BTreeSet;

use crate::repo::RepoView;
use crate::{DetectError, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind {
    /// A server-side application (PHP today). It absorbs asset-only JS claims.
    Backend,
    /// A JS app that serves requests (a start script or a server framework).
    JsServer,
    /// JS that only builds assets: a supporting toolchain, or a static site when alone.
    JsAssets,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub provider: ProviderId,
    /// The more general provider this one specialises (Laravel refines PHP).
    pub refines: Option<ProviderId>,
    pub kind: ClaimKind,
    pub evidence: Vec<String>,
}

pub fn select(claims: &[Claim], repo: &RepoView) -> Result<ProviderId, DetectError> {
    let refined: BTreeSet<ProviderId> = claims.iter().filter_map(|c| c.refines).collect();
    let mut left: Vec<&Claim> = claims.iter().filter(|c| !refined.contains(&c.provider)).collect();
    if left.iter().any(|c| c.kind == ClaimKind::Backend) {
        left.retain(|c| c.kind != ClaimKind::JsAssets);
    }
    match left.as_slice() {
        [] => Err(DetectError::NoProvider { candidates: repo.manifest_dirs() }),
        [one] => Ok(one.provider),
        many => {
            let mut providers: Vec<ProviderId> = many.iter().map(|c| c.provider).collect();
            providers.sort();
            Err(DetectError::Ambiguous { providers })
        }
    }
}
```

`crates/potter-detect/src/providers/mod.rs`:

```rust
//! Language providers (spec §6.1). Each provider adds itself to `all()` in its own task.

pub(crate) mod common;

use potter_plan::{Decision, Target};

use crate::options::DetectOptions;
use crate::repo::RepoView;
use crate::select::Claim;
use crate::{DetectError, ProviderId};

#[derive(Debug, Clone)]
pub struct Detection {
    pub target: Target,
    pub decisions: Vec<Decision>,
}

pub trait Provider: Sync {
    fn id(&self) -> ProviderId;
    /// Cheap: does this provider apply here, and how? Must not fail on files only `build` needs.
    fn claim(&self, repo: &RepoView, opts: &DetectOptions) -> Result<Option<Claim>, DetectError>;
    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError>;
}

pub fn all() -> Vec<&'static dyn Provider> {
    vec![]
}

pub fn get(id: ProviderId) -> Option<&'static dyn Provider> {
    all().into_iter().find(|p| p.id() == id)
}
```

`crates/potter-detect/src/providers/common.rs`:

```rust
//! Building blocks shared by providers.

use std::collections::BTreeMap;

use potter_plan::{
    Base, CacheMount, CatalogRef, Chain, Command, Decision, Export, Image, Origin, Sharing, Step, Target,
};

use crate::catalog::VersionSource;
use crate::options::DetectOptions;
use crate::repo::{READ_CAP, RepoView};
use crate::version::{ConstraintError, parse_version_file};
use crate::{DetectError, ProviderId};

pub(crate) const APP: &str = "/app";

pub(crate) fn inputs(name: &str, include: Vec<String>) -> Step {
    Step::Inputs { name: name.into(), include, exclude: vec![], dest: APP.into(), owner: "0:0".into() }
}

/// The whole target root (default excludes and .potterignore apply when P2 builds the layer).
pub(crate) fn source() -> Step {
    inputs("source", vec!["**".into()])
}

pub(crate) fn run(name: &str, command: Command, env: &[(&str, &str)], cache: &[(&str, &str)]) -> Step {
    Step::Run {
        name: name.into(),
        command,
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        workdir: APP.into(),
        user: "root".into(),
        network: true,
        cache_mounts: cache
            .iter()
            .map(|(id, target)| CacheMount { id: id.to_string(), target: target.to_string(), sharing: Sharing::Exclusive })
            .collect(),
        secrets: vec![],
        timeout_secs: None,
    }
}

pub(crate) fn cref(name: &str, version: Option<&str>) -> CatalogRef {
    CatalogRef::new(name, version)
}

pub(crate) fn export(path: &str, dest: &str) -> Export {
    Export { path: path.into(), dest: dest.into(), exclude: vec![] }
}

pub(crate) fn new_target(name: &str, provider: ProviderId, build_layers: Vec<CatalogRef>, steps: Vec<Step>, image: Image) -> Target {
    Target {
        name: name.into(),
        root: ".".into(),
        platform: None,
        provider: provider.as_str().into(),
        build: Chain { base: Base::Catalog { layers: build_layers }, steps },
        image,
    }
}

pub(crate) fn empty_image(run_layers: Vec<CatalogRef>, config: potter_plan::ImageConfig) -> Image {
    Image {
        base: Base::Catalog { layers: run_layers },
        runtime_steps: vec![],
        export: vec![],
        files: vec![],
        config,
        processes: BTreeMap::new(),
        services: BTreeMap::new(),
    }
}

/// Insert `new` right after the step named `after`, or at the end if there is none.
pub(crate) fn insert_after(steps: &mut Vec<Step>, after: &str, new: Vec<Step>) {
    let at = steps.iter().position(|s| s.name() == after).map_or(steps.len(), |i| i + 1);
    steps.splice(at..at, new);
}

pub(crate) fn port(opts: &DetectOptions, default: u16, decisions: &mut Vec<Decision>) -> u16 {
    match opts.port {
        Some(p) => {
            decisions.push(Decision::config("port", p.to_string(), opts.key("port")));
            p
        }
        None => {
            decisions.push(Decision::default("port", default.to_string()));
            default
        }
    }
}

pub(crate) fn bad_constraint(what: &'static str, constraint: &str, source: &str, e: ConstraintError) -> DetectError {
    DetectError::BadConstraint { what, constraint: constraint.into(), source_desc: source.into(), reason: e.0 }
}

pub(crate) fn version_from_config(
    value: Option<&str>,
    key: &str,
    what: &'static str,
) -> Result<Option<VersionSource>, DetectError> {
    let Some(value) = value else { return Ok(None) };
    let (set, lts_only) = parse_version_file(value).map_err(|e| bad_constraint(what, value, key, e))?;
    Ok(Some(VersionSource {
        desc: format!("{key} = {value}"),
        origin: Origin::Config { key: key.to_string() },
        set,
        lts_only,
    }))
}

pub(crate) fn version_from_file(
    repo: &RepoView,
    rel: &str,
    what: &'static str,
) -> Result<Option<VersionSource>, DetectError> {
    let Some(content) = repo.read_string(rel, READ_CAP)? else { return Ok(None) };
    let shown = content.trim();
    let (set, lts_only) = parse_version_file(&content).map_err(|e| bad_constraint(what, shown, rel, e))?;
    let desc = format!("{rel} = {}", shown.lines().next().unwrap_or(""));
    Ok(Some(VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only }))
}

/// A repo file named in potter.toml must exist and stay inside the build root.
pub(crate) fn require_repo_file(repo: &RepoView, rel: &str, provider: ProviderId, key: &str) -> Result<(), DetectError> {
    if potter_plan::is_relative_path(rel) && repo.is_file(rel) {
        Ok(())
    } else {
        Err(DetectError::NeedsConfig {
            provider,
            message: format!("`{rel}` is not a file inside the build root"),
            key: key.to_string(),
        })
    }
}
```

Replace `crates/potter-detect/src/lib.rs`:

```rust
//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

pub mod catalog;
mod error;
mod jsonc;
mod options;
pub mod providers;
pub mod repo;
pub(crate) mod scan;
mod select;
pub mod version;

use potter_plan::Decision;

pub use error::{DetectError, ProviderId};
pub use options::{DetectOptions, NodeOptions, PhpOptions, RuntimeOverrides};
pub use providers::{Detection, Provider};
pub use repo::{RepoError, RepoView};
pub use select::{Claim, ClaimKind, select};

/// Every provider's claim on this build root, in `ProviderId` order.
pub fn claims(repo: &RepoView, opts: &DetectOptions) -> Result<Vec<Claim>, DetectError> {
    let mut out = Vec::new();
    for p in providers::all() {
        if let Some(c) = p.claim(repo, opts)? {
            out.push(c);
        }
    }
    out.sort_by_key(|c| c.provider);
    Ok(out)
}

pub fn detect(repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
    let (id, decision) = match opts.provider {
        Some(id) => {
            let p = providers::get(id).ok_or(DetectError::ForcedNotApplicable(id))?;
            if p.claim(repo, opts)?.is_none() {
                return Err(DetectError::ForcedNotApplicable(id));
            }
            (id, Decision::config("provider", id.as_str(), opts.key("provider")))
        }
        None => {
            let all = claims(repo, opts)?;
            let id = select(&all, repo)?;
            let evidence = all.iter().find(|c| c.provider == id).map(|c| c.evidence.clone()).unwrap_or_default();
            (id, Decision::detected("provider", id.as_str(), evidence))
        }
    };
    let provider = providers::get(id).expect("selected providers are registered");
    let mut detection = provider.build(repo, opts, target)?;
    detection.decisions.insert(0, decision);
    Ok(detection)
}
```

Removing `#![allow(dead_code)]` can surface unused-item warnings for helpers that the provider tasks only use later (`common::*`). Add `#[allow(dead_code)]` on `mod common;` in `providers/mod.rs` until Task 13, and remove it there.

```rust
#[allow(dead_code)] // Every helper is used once all providers land (Task 13 removes this).
pub(crate) mod common;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-detect
git commit -m "feat(potter-detect): provider framework, claims and selection"
```

---

### Task 9: The PHP provider (generic composer projects)

**Files:**
- Create: `crates/potter-detect/src/providers/php.rs`, `crates/potter-detect/src/providers/php_config.rs`, `crates/potter-detect/tests/php.rs`
- Create fixtures: `fixtures/php-composer/`, `fixtures/drupal/`
- Modify: `crates/potter-detect/src/providers/mod.rs`

**Interfaces:**
- Consumes: the Task 8 framework and helpers, `catalog::{pick, PHP, COMPOSER_VERSION}`, `parse_composer_constraint` and `parse_partial`.
- Produces (used by Laravel in Task 11):
  - `php::PhpFacts { composer: Value, version: &'static Line, extensions: BTreeSet<String>, docroot: String, decisions: Vec<Decision> }`.
  - `php::read_facts(&RepoView, &DetectOptions, ProviderId, defaults: &[&str], defaults_reason: &str) -> Result<PhpFacts, DetectError>`.
  - `php::build_php(&RepoView, PhpFacts, &DetectOptions, ProviderId, name) -> Result<Detection, DetectError>`.
  - `php::requires(&Value, package) -> bool`.
  - `php_config::{NGINX_PATH, FPM_PATH, FPM_SOCKET, nginx(port, docroot) -> String, fpm() -> String}`.
- Step names: `deps-inputs`, `deps`, `source`, `autoload`.

- [ ] **Step 1: Create the fixtures**

```bash
mkdir -p fixtures/php-composer/public fixtures/drupal/public
cp /Users/alfonso/Github/Personal/drupal-hello-world/composer.json /Users/alfonso/Github/Personal/drupal-hello-world/composer.lock fixtures/drupal/
printf '<?php\n' > fixtures/drupal/public/index.php
printf '<?php echo "ok";\n' > fixtures/php-composer/public/index.php
```

`fixtures/php-composer/composer.json`. The `post-install-cmd` exists to prove that detection never runs scripts (P9):

```json
{
    "name": "potter/php-composer",
    "require": {
        "php": "^8.3",
        "ext-intl": "*",
        "monolog/monolog": "^3.0"
    },
    "scripts": {
        "post-install-cmd": ["php -r \"touch('POTTER_EXECUTED');\""]
    }
}
```

`fixtures/php-composer/composer.lock`:

```json
{
    "packages": [
        { "name": "monolog/monolog", "version": "3.7.0", "require": { "php": ">=8.1", "ext-json": "*", "psr/log": "^2.0 || ^3.0" } },
        { "name": "psr/log", "version": "3.0.2", "require": { "php": ">=8.0.0" } }
    ],
    "packages-dev": [
        { "name": "phpunit/phpunit", "version": "11.5.0", "require": { "ext-xdebug": "*" } }
    ],
    "platform": { "php": "^8.3", "ext-intl": "*" }
}
```

- [ ] **Step 2: Write the failing tests**

`crates/potter-detect/tests/php.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, PhpOptions, RuntimeOverrides};
use potter_plan::{Origin, Ready};

#[test]
fn composer_project() {
    let d = detect("php-composer");
    let t = &d.target;
    assert_eq!(t.provider, "php");
    assert_eq!(layers(&t.build.base), ["os-min", "php@8.4", "php-ext-intl@8.4", "composer@2"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "autoload"]);
    assert_eq!(includes(t, "deps-inputs"), ["composer.json", "composer.lock"]);
    assert!(command(t, "deps").starts_with("composer install --no-dev --no-scripts --no-autoloader"));
    assert_eq!(layers(&t.image.base), ["os-min", "php@8.4", "php-fpm@8.4", "php-ext-intl@8.4", "nginx"]);
    assert_eq!(process(t, "web"), "nginx -c /etc/potter/nginx.conf");
    let fpm = &t.image.services["web"][0];
    assert_eq!(fpm.name, "php-fpm");
    assert_eq!(fpm.ready, Ready::Unix("/run/php-fpm.sock".into()));
    assert_eq!(t.image.config.port, Some(8080));
    assert_eq!(t.image.config.stop_signal.as_deref(), Some("SIGQUIT"));
    let nginx = &t.image.files.iter().find(|f| f.path == "/etc/potter/nginx.conf").unwrap().content;
    assert!(nginx.contains("listen 8080;") && nginx.contains("root /app/public;") && nginx.contains("unix:/run/php-fpm.sock"));
    let fpm_conf = &t.image.files.iter().find(|f| f.path == "/etc/potter/php-fpm.conf").unwrap().content;
    assert!(fpm_conf.contains("pm.max_children = ${PHP_FPM_MAX_CHILDREN}") && fpm_conf.contains("clear_env = no"));
    assert_eq!(t.image.config.env["PHP_FPM_MAX_CHILDREN"], "16");
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    assert!(matches!(&decision(&d, "runtime.php").origin, Origin::Detected { evidence } if evidence[0] == "composer.json require.php = ^8.3"));
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn dev_only_extensions_and_builtins_are_ignored() {
    let d = detect("php-composer");
    let l = layers(&d.target.build.base);
    assert!(!l.iter().any(|x| x.contains("xdebug")), "{l:?}");
    assert!(!l.iter().any(|x| x.contains("json")), "{l:?}");
}

#[test]
fn detection_never_runs_composer_scripts() {
    let _ = detect("php-composer");
    assert!(!fixture("php-composer").join("POTTER_EXECUTED").exists());
}

#[test]
fn drupal() {
    let d = detect("drupal");
    let t = &d.target;
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    for ext in ["php-ext-gd@8.4", "php-ext-opcache@8.4", "php-ext-pdo_mysql@8.4", "php-ext-pdo_pgsql@8.4"] {
        assert!(layers(&t.image.base).contains(&ext.to_string()), "{ext}");
    }
    assert_eq!(decision(&d, "php.docroot").value, "public");
    let nginx = &t.image.files[0].content;
    assert!(nginx.contains("root /app/public;"));
}

#[test]
fn missing_lockfile() {
    let r = temp_repo(&[("composer.json", r#"{"require":{"php":"^8.2"}}"#)]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(matches!(&e, DetectError::MissingFile { file, .. } if file == "composer.lock"), "{e}");
}

#[test]
fn unsatisfiable_php() {
    let r = temp_repo(&[("composer.json", r#"{"require":{"php":"^7.4"}}"#), ("composer.lock", r#"{"packages":[]}"#)]);
    let msg = detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert_eq!(msg, "no available php version satisfies composer.json require.php = ^7.4 (available: 8.2, 8.3, 8.4)");
}

#[test]
fn version_precedence() {
    let files = [
        ("composer.json", r#"{"require":{"php":"^8.2"},"config":{"platform":{"php":"8.3.0"}}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ];
    let r = temp_repo(&files);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.php").value, "8.3");
    let r = temp_repo(&[files[0], files[1], (".php-version", "8.2\n")]);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.php").value, "8.2");
    let opts = DetectOptions { runtime: RuntimeOverrides { php: Some("8.4".into()), ..Default::default() }, ..Default::default() };
    let d = detect_at(r.path(), &opts).unwrap();
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    assert_eq!(decision(&d, "runtime.php").origin, Origin::Config { key: "target.app.runtime.php".into() });
}

#[test]
fn bad_constraint_names_its_source() {
    let r = temp_repo(&[("composer.json", r#"{"require":{"php":"dev-master"}}"#), ("composer.lock", r#"{"packages":[]}"#)]);
    let msg = detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert!(msg.contains("composer.json require.php") && msg.contains("dev-master"), "{msg}");
}

#[test]
fn docroot_fallbacks_and_overrides() {
    let base = [("composer.json", r#"{"require":{}}"#), ("composer.lock", r#"{"packages":[]}"#)];
    let r = temp_repo(&[base[0], base[1], ("web/index.php", "<?php")]);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "php.docroot").value, "web");
    let r = temp_repo(&base);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert!(d.target.image.files[0].content.contains("root /app;"));
    let r = temp_repo(&[base[0], base[1], ("deploy/nginx.conf", "events {}")]);
    let opts = DetectOptions { php: PhpOptions { nginx_conf: Some("deploy/nginx.conf".into()), ..Default::default() }, port: Some(9000), ..Default::default() };
    let d = detect_at(r.path(), &opts).unwrap();
    assert_eq!(process(&d.target, "web"), "nginx -c /app/deploy/nginx.conf");
    assert!(d.target.image.files.iter().all(|f| f.path != "/etc/potter/nginx.conf"));
    assert_eq!(d.target.image.config.port, Some(9000));
    let opts = DetectOptions { php: PhpOptions { nginx_conf: Some("missing.conf".into()), ..Default::default() }, ..Default::default() };
    assert!(matches!(detect_at(r.path(), &opts), Err(DetectError::NeedsConfig { .. })));
}

#[test]
fn hostile_docroot_is_refused() {
    let r = temp_repo(&[
        ("composer.json", r#"{"extra":{"drupal-scaffold":{"locations":{"web-root":"public; include /etc/passwd"}}}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::Parse { .. })));
}

#[test]
fn config_extensions_are_added() {
    let r = temp_repo(&[("composer.json", r#"{"require":{}}"#), ("composer.lock", r#"{"packages":[]}"#)]);
    let opts = DetectOptions { runtime: RuntimeOverrides { extensions: vec!["imagick".into(), "ext-redis".into()], ..Default::default() }, ..Default::default() };
    let l = layers(&detect_at(r.path(), &opts).unwrap().target.build.base);
    assert!(l.contains(&"php-ext-imagick@8.4".to_string()) && l.contains(&"php-ext-redis@8.4".to_string()), "{l:?}");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-detect --test php`
Expected: fails with `NoProvider`, because no providers are registered yet.

- [ ] **Step 4: Implement**

`crates/potter-detect/src/providers/php_config.rs`:

```rust
//! Generated nginx and php-fpm configuration (spec §6.1.1). Paths are potter-owned so they don't
//! depend on how a catalog layer lays out /etc.

pub(crate) const NGINX_PATH: &str = "/etc/potter/nginx.conf";
pub(crate) const FPM_PATH: &str = "/etc/potter/php-fpm.conf";
pub(crate) const FPM_SOCKET: &str = "/run/php-fpm.sock";

const NGINX_TEMPLATE: &str = r#"worker_processes auto;
pid /run/nginx.pid;
error_log /dev/stderr warn;
daemon off;
user www-data;

events {
    worker_connections 1024;
}

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    access_log /dev/stdout;
    server_tokens off;
    sendfile on;
    client_max_body_size 100m;
    gzip on;
    gzip_types text/plain text/css text/xml application/json application/javascript application/xml image/svg+xml;

    server {
        listen @PORT@;
        root @DOCROOT@;
        index index.php index.html;

        location ~ /\.(?!well-known) {
            deny all;
        }

        location / {
            try_files $uri $uri/ /index.php?$query_string;
        }

        location ~ \.php$ {
            try_files $uri =404;
            fastcgi_pass unix:@SOCKET@;
            fastcgi_index index.php;
            include /etc/nginx/fastcgi_params;
            fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
            fastcgi_param DOCUMENT_ROOT $realpath_root;
        }
    }
}
"#;

const FPM_TEMPLATE: &str = r#"[global]
error_log = /proc/self/fd/2
daemonize = no

[www]
user = www-data
group = www-data
listen = @SOCKET@
listen.owner = www-data
listen.group = www-data
listen.mode = 0660
pm = dynamic
pm.max_children = ${PHP_FPM_MAX_CHILDREN}
pm.start_servers = 2
pm.min_spare_servers = 1
pm.max_spare_servers = 3
clear_env = no
catch_workers_output = yes
decorate_workers_output = no
php_admin_value[opcache.validate_timestamps] = 0
"#;

/// `docroot` must already be validated (absolute, [A-Za-z0-9._/-] only).
pub(crate) fn nginx(port: u16, docroot: &str) -> String {
    NGINX_TEMPLATE.replace("@PORT@", &port.to_string()).replace("@DOCROOT@", docroot).replace("@SOCKET@", FPM_SOCKET)
}

pub(crate) fn fpm() -> String {
    FPM_TEMPLATE.replace("@SOCKET@", FPM_SOCKET)
}
```

`crates/potter-detect/src/providers/php.rs`:

```rust
//! Generic composer projects (spec §6.1 "PHP generic", §6.1.1).

use std::collections::{BTreeMap, BTreeSet};

use potter_plan::{
    Base, CatalogRef, Command, Decision, Export, GeneratedFile, ImageConfig, Origin, Process, Ready, Service,
};
use serde_json::Value;

use crate::catalog::{self, COMPOSER_VERSION, Line, PHP, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::php_config;
use crate::providers::{Detection, Provider};
use crate::repo::{LOCKFILE_CAP, RepoView};
use crate::select::{Claim, ClaimKind};
use crate::version::{parse_composer_constraint, parse_partial};
use crate::{DetectError, ProviderId};

/// Compiled into the catalog's php layer; never separate extension layers.
pub(crate) const BUILTIN_EXTENSIONS: &[&str] = &[
    "ctype", "curl", "date", "dom", "fileinfo", "filter", "hash", "iconv", "json", "libxml", "mbstring", "mysqlnd",
    "openssl", "pcre", "pdo", "pdo_sqlite", "phar", "posix", "readline", "reflection", "session", "simplexml",
    "sodium", "spl", "sqlite3", "standard", "tokenizer", "xml", "xmlreader", "xmlwriter", "zlib",
];

/// Packages known to need extensions they don't declare.
const PACKAGE_EXTENSIONS: &[(&str, &[&str])] = &[
    ("drupal/core", &["gd", "opcache", "pdo_mysql", "pdo_pgsql"]),
    ("drupal/core-recommended", &["gd", "opcache", "pdo_mysql", "pdo_pgsql"]),
];

pub(crate) struct PhpProvider;

impl Provider for PhpProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Php
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(repo.is_file("composer.json").then(|| Claim {
            provider: ProviderId::Php,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec!["composer.json".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let facts = read_facts(repo, opts, ProviderId::Php, &[], "")?;
        build_php(repo, facts, opts, ProviderId::Php, target)
    }
}

pub(crate) struct PhpFacts {
    pub composer: Value,
    pub version: &'static Line,
    pub extensions: BTreeSet<String>,
    pub docroot: String,
    pub decisions: Vec<Decision>,
}

pub(crate) fn requires(composer: &Value, package: &str) -> bool {
    composer.get("require").and_then(|r| r.get(package)).is_some()
}

pub(crate) fn read_facts(
    repo: &RepoView,
    opts: &DetectOptions,
    provider: ProviderId,
    defaults: &[&str],
    defaults_reason: &str,
) -> Result<PhpFacts, DetectError> {
    let composer = repo.read_json("composer.json")?.ok_or_else(|| DetectError::MissingFile {
        provider,
        file: "composer.json".into(),
        hint: "the PHP providers build composer projects".into(),
    })?;
    let lock = repo.read_json_capped("composer.lock", LOCKFILE_CAP)?.ok_or_else(|| DetectError::MissingFile {
        provider,
        file: "composer.lock".into(),
        hint: "commit composer.lock (run `composer update` locally) so builds are reproducible".into(),
    })?;
    let mut decisions = Vec::new();
    let version = php_version(repo, opts, &composer, &mut decisions)?;
    let extensions = extensions(opts, &composer, &lock, defaults, defaults_reason, &mut decisions);
    let docroot = docroot(repo, &composer, &mut decisions)?;
    Ok(PhpFacts { composer, version, extensions, docroot, decisions })
}

fn php_version(
    repo: &RepoView,
    opts: &DetectOptions,
    composer: &Value,
    decisions: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.php.as_deref(), &opts.key("runtime.php"), "php")?;
    if src.is_none() {
        src = version_from_file(repo, ".php-version", "php")?;
    }
    if src.is_none()
        && let Some(p) = composer.pointer("/config/platform/php").and_then(Value::as_str)
    {
        let set = parse_partial(p)
            .and_then(|v| v.x_range())
            .map_err(|e| bad_constraint("php", p, "composer.json config.platform.php", e))?;
        src = Some(detected_source(format!("composer.json config.platform.php = {p}"), set));
    }
    if src.is_none()
        && let Some(c) = composer.pointer("/require/php").and_then(Value::as_str)
    {
        let set = parse_composer_constraint(c).map_err(|e| bad_constraint("php", c, "composer.json require.php", e))?;
        src = Some(detected_source(format!("composer.json require.php = {c}"), set));
    }
    catalog::pick(&PHP, "runtime.php", src, decisions)
}

fn detected_source(desc: String, set: crate::version::VersionSet) -> VersionSource {
    VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false }
}

/// `ext-pdo_mysql` → `pdo_mysql`; `zend-opcache` → `opcache`. Names that aren't extension
/// identifiers are dropped.
fn normalize_ext(raw: &str) -> Option<String> {
    let s = raw.strip_prefix("ext-").unwrap_or(raw).to_ascii_lowercase().replace('-', "_");
    let s = match s.as_str() {
        "zend_opcache" | "zendopcache" => "opcache".to_string(),
        _ => s,
    };
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')).then_some(s)
}

fn ext_keys(obj: Option<&Value>) -> Vec<String> {
    obj.and_then(Value::as_object)
        .map(|m| m.keys().filter(|k| k.starts_with("ext-")).filter_map(|k| normalize_ext(k)).collect())
        .unwrap_or_default()
}

fn extensions(
    opts: &DetectOptions,
    composer: &Value,
    lock: &Value,
    defaults: &[&str],
    defaults_reason: &str,
    decisions: &mut Vec<Decision>,
) -> BTreeSet<String> {
    let mut found: BTreeMap<String, Origin> = BTreeMap::new();
    let mut add = |ext: String, origin: Origin| {
        found.entry(ext).or_insert(origin);
    };
    for e in &opts.runtime.extensions {
        if let Some(n) = normalize_ext(e) {
            add(n, Origin::Config { key: opts.key("runtime.extensions") });
        }
    }
    let ev = |s: String| Origin::Detected { evidence: vec![s] };
    for e in ext_keys(composer.get("require")) {
        add(e.clone(), ev(format!("composer.json require.ext-{e}")));
    }
    let packages: Vec<&Value> = lock.get("packages").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    for pkg in &packages {
        let name = pkg.get("name").and_then(Value::as_str).unwrap_or("?");
        for e in ext_keys(pkg.get("require")) {
            add(e.clone(), ev(format!("composer.lock: {name} requires ext-{e}")));
        }
    }
    for e in ext_keys(lock.get("platform")) {
        add(e.clone(), ev(format!("composer.lock platform.ext-{e}")));
    }
    for (package, exts) in PACKAGE_EXTENSIONS {
        let locked = packages.iter().any(|p| p.get("name").and_then(Value::as_str) == Some(*package));
        if requires(composer, package) || locked {
            for e in *exts {
                add(e.to_string(), ev(format!("{package} needs ext-{e}")));
            }
        }
    }
    for e in defaults {
        add(e.to_string(), ev(defaults_reason.to_string()));
    }
    let mut out = BTreeSet::new();
    for (ext, origin) in found {
        if BUILTIN_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        decisions.push(Decision { field: "runtime.extensions".into(), value: ext.clone(), origin });
        out.insert(ext);
    }
    out
}

fn safe_docroot(s: &str) -> bool {
    potter_plan::is_relative_path(s) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

fn docroot(repo: &RepoView, composer: &Value, decisions: &mut Vec<Decision>) -> Result<String, DetectError> {
    if let Some(root) = composer.pointer("/extra/drupal-scaffold/locations/web-root").and_then(Value::as_str) {
        let trimmed = root.trim_end_matches('/');
        let r = if trimmed.is_empty() { "." } else { trimmed };
        if !safe_docroot(r) {
            return Err(DetectError::Parse {
                file: "composer.json".into(),
                reason: "extra.drupal-scaffold.locations.web-root must be a plain relative path".into(),
            });
        }
        decisions.push(Decision::detected(
            "php.docroot",
            r,
            [format!("composer.json extra.drupal-scaffold.locations.web-root = {root}")],
        ));
        return Ok(r.to_string());
    }
    for d in ["public", "web"] {
        if repo.is_dir(d) {
            decisions.push(Decision::detected("php.docroot", d, [format!("{d}/ exists")]));
            return Ok(d.to_string());
        }
    }
    decisions.push(Decision::default("php.docroot", "."));
    Ok(".".to_string())
}

pub(crate) fn build_php(
    repo: &RepoView,
    facts: PhpFacts,
    opts: &DetectOptions,
    provider: ProviderId,
    name: &str,
) -> Result<Detection, DetectError> {
    let PhpFacts { version, extensions, docroot, mut decisions, .. } = facts;
    let v = version.version;
    let ext_refs: Vec<CatalogRef> = extensions.iter().map(|e| cref(&format!("php-ext-{e}"), Some(v))).collect();

    let mut build_layers = vec![cref("os-min", None), cref("php", Some(v))];
    build_layers.extend(ext_refs.iter().cloned());
    build_layers.push(cref("composer", Some(COMPOSER_VERSION)));
    let steps = vec![
        inputs("deps-inputs", vec!["composer.json".into(), "composer.lock".into()]),
        run(
            "deps",
            Command::shell("composer install --no-dev --no-scripts --no-autoloader --prefer-dist --no-interaction --no-progress"),
            &[("COMPOSER_ALLOW_SUPERUSER", "1"), ("COMPOSER_CACHE_DIR", "/cache/composer")],
            &[("composer", "/cache/composer")],
        ),
        source(),
        run(
            "autoload",
            Command::shell("composer dump-autoload --optimize --no-dev --no-interaction"),
            &[("COMPOSER_ALLOW_SUPERUSER", "1")],
            &[],
        ),
    ];

    let mut run_layers = vec![cref("os-min", None), cref("php", Some(v)), cref("php-fpm", Some(v))];
    run_layers.extend(ext_refs);
    run_layers.push(cref("nginx", None));

    let port = port(opts, 8080, &mut decisions);
    let docroot_abs = if docroot == "." { "/app".to_string() } else { format!("/app/{docroot}") };
    let mut files = Vec::new();
    let nginx_path = match &opts.php.nginx_conf {
        Some(p) => {
            let key = opts.key("php.nginx_conf");
            require_repo_file(repo, p, provider, &key)?;
            decisions.push(Decision::config("php.nginx_conf", p.as_str(), key));
            format!("/app/{p}")
        }
        None => {
            files.push(GeneratedFile {
                path: php_config::NGINX_PATH.into(),
                content: php_config::nginx(port, &docroot_abs),
                mode: 0o644,
            });
            php_config::NGINX_PATH.to_string()
        }
    };
    let fpm_path = match &opts.php.fpm_pool {
        Some(p) => {
            let key = opts.key("php.fpm_pool");
            require_repo_file(repo, p, provider, &key)?;
            decisions.push(Decision::config("php.fpm_pool", p.as_str(), key));
            format!("/app/{p}")
        }
        None => {
            files.push(GeneratedFile { path: php_config::FPM_PATH.into(), content: php_config::fpm(), mode: 0o644 });
            php_config::FPM_PATH.to_string()
        }
    };

    let mut config = ImageConfig::new("/app", "root");
    config.env.insert("PHP_FPM_MAX_CHILDREN".into(), "16".into());
    config.port = Some(port);
    config.stop_signal = Some("SIGQUIT".into());
    let mut image = empty_image(run_layers, config);
    image.export = vec![Export { path: "/app".into(), dest: "/app".into(), exclude: vec!["node_modules".into()] }];
    image.files = files;
    image.processes.insert("web".into(), Process::new(&["nginx", "-c", &nginx_path]));
    image.services.insert(
        "web".into(),
        vec![Service {
            name: "php-fpm".into(),
            command: vec!["php-fpm".into(), "--nodaemonize".into(), "--fpm-config".into(), fpm_path],
            ready: Ready::Unix(php_config::FPM_SOCKET.into()),
            stop_signal: "SIGQUIT".into(),
        }],
    );
    let target = new_target(name, provider, build_layers, steps, image);
    debug_assert!(matches!(target.build.base, Base::Catalog { .. }));
    Ok(Detection { target, decisions })
}
```

Register the provider in `crates/potter-detect/src/providers/mod.rs`:

```rust
mod php;
mod php_config;
```

and change `all()`:

```rust
pub fn all() -> Vec<&'static dyn Provider> {
    vec![&php::PhpProvider]
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes.

If `drupal` fails on the version, inspect `fixtures/drupal/composer.json`. The survey recorded `php ">=8.3 <8.5"` there, which resolves to 8.4.

- [ ] **Step 6: Commit**

```bash
git add crates/potter-detect fixtures/php-composer fixtures/drupal
git commit -m "feat(potter-detect): PHP provider with nginx + php-fpm services"
```

---

### Task 10: JS toolchain helpers and the Node provider

**Files:**
- Create: `crates/potter-detect/src/providers/js.rs`, `crates/potter-detect/src/providers/node.rs`, `crates/potter-detect/tests/node.rs`
- Create fixtures: `fixtures/nestjs/`, `fixtures/next-standalone/`, `fixtures/nuxt/`, `fixtures/hono-node/`, `fixtures/vue-node/`
- Modify: `crates/potter-detect/src/providers/mod.rs`

**Interfaces:**
- Consumes: Task 8 helpers, `catalog::{NODE, BUN}`, `parse_npm_range` and `scan::quoted_value_after`.
- Produces (`providers::js`, used by Laravel, Bun and Static):
  - `PackageJson { value }` with `read(&RepoView) -> Result<Option<PackageJson>, DetectError>`, `script(&str) -> Option<&str>`, `dep(&str) -> bool` (dependencies only), `any_dep(&str) -> bool` (dependencies or devDependencies), `str_field(&str)`, `engine(&str)` and `package_manager() -> Option<(String, String)>`.
  - `SERVER_DEPS` and `js_kind(&PackageJson, extra_server_deps: &[&str]) -> ClaimKind`.
  - `Pm::{Npm, Pnpm, YarnClassic, YarnBerry, Bun}` with `name()`, `install() -> Command`, `run_script(&str) -> Vec<String>`, `cache() -> (&str id, &str target, &str env)`, `prune() -> Option<Command>` and `lock_inputs(&RepoView) -> Vec<String>`.
  - `detect_pm(&RepoView, &PackageJson, &DetectOptions, ProviderId, &mut Vec<Decision>) -> Result<Pm, DetectError>`.
  - `deps_steps(Pm, &RepoView, prefix: &str) -> Vec<Step>`: the `{prefix}deps-inputs` and `{prefix}deps` steps.
  - `node_version(...) -> Result<&'static Line, DetectError>` and `bun_version(...) -> Result<&'static Line, DetectError>`.
  - `BackendAssets { runtime: CatalogRef, deps: Vec<Step>, assets: Step }` and `assets_for_backend(&RepoView, &DetectOptions, ProviderId, &mut Vec<Decision>) -> Result<Option<BackendAssets>, DetectError>`.
- Node step names: `deps-inputs`, `deps`, `source`, `build` (if a build script exists) and `prune` (npm and pnpm only).

- [ ] **Step 1: Create the fixtures**

```bash
P=/Users/alfonso/Github/Personal
mkdir -p fixtures/nestjs fixtures/next-standalone/public fixtures/nuxt fixtures/hono-node fixtures/vue-node
cp "$P/nestjs-hello-world/package.json" "$P/nestjs-hello-world/package-lock.json" fixtures/nestjs/
cp "$P/nextjs-go-monorepo/frontend/package.json" "$P/nextjs-go-monorepo/frontend/package-lock.json" fixtures/next-standalone/
cp "$P/hono/hono/package.json" "$P/hono/hono/package-lock.json" fixtures/hono-node/
cp "$P/vue-hello-world/package.json" "$P/vue-hello-world/package-lock.json" "$P/vue-hello-world/vite.config.ts" "$P/vue-hello-world/server.mjs" fixtures/vue-node/
printf 'User-agent: *\n' > fixtures/next-standalone/public/robots.txt
```

`fixtures/next-standalone/next.config.ts` (synthetic: the original config is empty):

```ts
import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  output: "standalone",
};

export default nextConfig;
```

`fixtures/nuxt/package.json`:

```json
{
  "name": "nuxt-fixture",
  "private": true,
  "type": "module",
  "scripts": { "build": "nuxt build", "dev": "nuxt dev" },
  "dependencies": { "nuxt": "^3.15.0", "vue": "^3.5.13" }
}
```

`fixtures/nuxt/package-lock.json`:

```json
{ "name": "nuxt-fixture", "lockfileVersion": 3, "requires": true, "packages": {} }
```

- [ ] **Step 2: Write the failing tests**

`crates/potter-detect/tests/node.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, NodeOptions, RuntimeOverrides};

#[test]
fn nestjs() {
    let d = detect("nestjs");
    let t = &d.target;
    assert_eq!(t.provider, "node");
    assert_eq!(layers(&t.build.base), ["os-min", "node@24"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build", "prune"]);
    assert_eq!(includes(t, "deps-inputs"), ["package.json", "package-lock.json"]);
    assert_eq!(command(t, "deps"), "npm ci --no-audit --no-fund");
    assert_eq!(command(t, "build"), "npm run build");
    assert_eq!(command(t, "prune"), "npm prune --omit=dev");
    assert_eq!(process(t, "web"), "npm run start");
    assert_eq!(t.image.config.port, Some(3000));
    assert_eq!(t.image.config.env["PORT"], "3000");
    assert_eq!(t.image.config.env["NODE_ENV"], "production");
    assert_eq!(decision(&d, "runtime.node").value, "24");
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn next_standalone() {
    let t = detect("next-standalone").target;
    assert_eq!(process(&t, "web"), "node server.js");
    let exports: Vec<(&str, &str)> = t.image.export.iter().map(|e| (e.path.as_str(), e.dest.as_str())).collect();
    assert_eq!(exports, [("/app/.next/standalone", "/app"), ("/app/.next/static", "/app/.next/static"), ("/app/public", "/app/public")]);
    assert_eq!(t.image.config.env["HOSTNAME"], "0.0.0.0");
}

#[test]
fn nuxt() {
    let t = detect("nuxt").target;
    assert_eq!(process(&t, "web"), "node .output/server/index.mjs");
    assert_eq!(t.image.export[0].path, "/app/.output");
}

#[test]
fn hono_and_vue_use_their_start_scripts() {
    assert_eq!(process(&detect("hono-node").target, "web"), "npm run start");
    let d = detect("vue-node");
    assert_eq!(process(&d.target, "web"), "npm run start");
    assert_eq!(decision(&d, "runtime.node").value, "24");
}

#[test]
fn package_manager_choice() {
    let pkg = r#"{"scripts":{"start":"node index.js"}}"#;
    let r = temp_repo(&[("package.json", pkg), ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n")]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(command(&t, "deps"), "corepack enable && pnpm install --frozen-lockfile");
    assert_eq!(command(&t, "prune"), "pnpm prune --prod");
    assert_eq!(process(&t, "web"), "pnpm run start");

    let r = temp_repo(&[("package.json", pkg), ("yarn.lock", "# THIS IS AN AUTOGENERATED FILE.\n# yarn lockfile v1\n")]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(command(&t, "deps"), "corepack enable && yarn install --frozen-lockfile");
    assert!(!step_names(&t).contains(&"prune"));

    let r = temp_repo(&[("package.json", pkg), ("yarn.lock", "__metadata:\n  version: 8\n"), (".yarnrc.yml", "nodeLinker: node-modules\n")]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(command(&t, "deps"), "corepack enable && yarn install --immutable");
    assert_eq!(includes(&t, "deps-inputs"), ["package.json", "yarn.lock", ".yarnrc.yml"]);
}

#[test]
fn several_lockfiles_prefer_npm_with_a_decision() {
    let r = temp_repo(&[("package.json", r#"{"scripts":{"start":"x"}}"#), ("package-lock.json", "{}"), ("yarn.lock", "# yarn lockfile v1\n")]);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert_eq!(command(&d.target, "deps"), "npm ci --no-audit --no-fund");
    let pm = decision(&d, "node.package_manager");
    assert_eq!(pm.value, "npm");
    assert!(pm.origin.to_string().contains("several lockfiles"), "{}", pm.origin);
    let opts = DetectOptions { node: NodeOptions { package_manager: Some("yarn".into()) }, ..Default::default() };
    let t = detect_at(r.path(), &opts).unwrap().target;
    assert_eq!(command(&t, "deps"), "corepack enable && yarn install --frozen-lockfile");
}

#[test]
fn package_manager_field_needs_its_lockfile() {
    let r = temp_repo(&[("package.json", r#"{"packageManager":"pnpm@9.15.0","scripts":{"start":"x"}}"#), ("package-lock.json", "{}")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(matches!(&e, DetectError::MissingFile { file, .. } if file == "pnpm-lock.yaml"), "{e}");
}

#[test]
fn no_lockfile() {
    let r = temp_repo(&[("package.json", r#"{"scripts":{"start":"x"}}"#)]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(e.to_string().contains("commit package-lock.json"), "{e}");
}

#[test]
fn no_process() {
    let r = temp_repo(&[("package.json", r#"{"dependencies":{"express":"^5"}}"#), ("package-lock.json", "{}")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(matches!(e, DetectError::NoProcess { .. }));
    assert!(e.to_string().contains("target.app.process.web"), "{e}");
}

#[test]
fn node_version_sources() {
    let base = [("package.json", r#"{"engines":{"node":">=18"},"scripts":{"start":"x"}}"#), ("package-lock.json", "{}")];
    let r = temp_repo(&[base[0], base[1], (".nvmrc", "v20.11.0 # pinned\r\n")]);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.node").value, "20");
    let r = temp_repo(&[base[0], base[1], (".nvmrc", "lts/*\n")]);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.node").value, "24");
    let r = temp_repo(&base);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.node").value, "24");
    let opts = DetectOptions { runtime: RuntimeOverrides { node: Some("22".into()), ..Default::default() }, ..Default::default() };
    assert_eq!(decision(&detect_at(r.path(), &opts).unwrap(), "runtime.node").value, "22");
    let r = temp_repo(&[base[0], base[1], (".nvmrc", "lts/iron\n")]);
    assert!(detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string().contains(".nvmrc"));
}

#[test]
fn bun_or_deno_files_hand_off() {
    let r = temp_repo(&[("package.json", r#"{"scripts":{"start":"x"}}"#), ("bun.lock", "{}")]);
    let claims = potter_detect::claims(&potter_detect::RepoView::open(r.path()).unwrap(), &DetectOptions::default()).unwrap();
    assert!(claims.iter().all(|c| c.provider != potter_detect::ProviderId::Node));
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-detect --test node`
Expected: fails, with `NoProvider` or compile errors.

- [ ] **Step 4: Implement**

`crates/potter-detect/src/providers/js.rs`:

```rust
//! package.json, package managers and JS runtimes, shared by Node, Bun, Static and Laravel's assets.

use potter_plan::{CatalogRef, Command, Decision, Origin, Step};
use serde_json::Value;

use crate::catalog::{self, BUN, Line, NODE, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::repo::{LOCKFILE_CAP, RepoView};
use crate::select::ClaimKind;
use crate::version::{parse_npm_range, parse_partial};
use crate::{DetectError, ProviderId};

pub(crate) struct PackageJson {
    pub value: Value,
}

impl PackageJson {
    pub fn read(repo: &RepoView) -> Result<Option<PackageJson>, DetectError> {
        Ok(repo.read_json("package.json")?.map(|value| PackageJson { value }))
    }

    pub fn script(&self, name: &str) -> Option<&str> {
        self.value.get("scripts")?.get(name)?.as_str()
    }

    /// Runtime dependencies only.
    pub fn dep(&self, name: &str) -> bool {
        self.value.get("dependencies").and_then(|d| d.get(name)).is_some()
    }

    pub fn any_dep(&self, name: &str) -> bool {
        self.dep(name) || self.value.get("devDependencies").and_then(|d| d.get(name)).is_some()
    }

    pub fn str_field(&self, name: &str) -> Option<&str> {
        self.value.get(name)?.as_str()
    }

    pub fn engine(&self, name: &str) -> Option<&str> {
        self.value.get("engines")?.get(name)?.as_str()
    }

    /// `"pnpm@9.15.0+sha512.…"` → `("pnpm", "9.15.0")`.
    pub fn package_manager(&self) -> Option<(String, String)> {
        let (name, rest) = self.str_field("packageManager")?.split_once('@')?;
        Some((name.to_string(), rest.split('+').next().unwrap_or(rest).to_string()))
    }
}

pub(crate) const SERVER_DEPS: &[&str] = &[
    "next",
    "nuxt",
    "@nestjs/core",
    "express",
    "fastify",
    "koa",
    "@hono/node-server",
    "@remix-run/serve",
    "@sveltejs/adapter-node",
];

pub(crate) fn js_kind(pkg: &PackageJson, extra_server_deps: &[&str]) -> ClaimKind {
    let serves = pkg.script("start").is_some() || SERVER_DEPS.iter().chain(extra_server_deps).any(|d| pkg.dep(d));
    if serves { ClaimKind::JsServer } else { ClaimKind::JsAssets }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pm {
    Npm,
    Pnpm,
    YarnClassic,
    YarnBerry,
    Bun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Npm => "npm",
            Kind::Pnpm => "pnpm",
            Kind::Yarn => "yarn",
            Kind::Bun => "bun",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        [Kind::Npm, Kind::Pnpm, Kind::Yarn, Kind::Bun].into_iter().find(|k| k.as_str() == s)
    }

    fn lockfiles(self) -> &'static [&'static str] {
        match self {
            Kind::Npm => &["package-lock.json", "npm-shrinkwrap.json"],
            Kind::Pnpm => &["pnpm-lock.yaml"],
            Kind::Yarn => &["yarn.lock"],
            Kind::Bun => &["bun.lock", "bun.lockb"],
        }
    }

    fn present(self, repo: &RepoView) -> Option<&'static str> {
        self.lockfiles().iter().copied().find(|f| repo.is_file(f))
    }
}

impl Pm {
    pub fn name(self) -> &'static str {
        match self {
            Pm::Npm => "npm",
            Pm::Pnpm => "pnpm",
            Pm::YarnClassic | Pm::YarnBerry => "yarn",
            Pm::Bun => "bun",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Pm::Npm => Kind::Npm,
            Pm::Pnpm => Kind::Pnpm,
            Pm::YarnClassic | Pm::YarnBerry => Kind::Yarn,
            Pm::Bun => Kind::Bun,
        }
    }

    pub fn install(self) -> Command {
        match self {
            Pm::Npm => Command::argv(&["npm", "ci", "--no-audit", "--no-fund"]),
            Pm::Pnpm => Command::shell("corepack enable && pnpm install --frozen-lockfile"),
            Pm::YarnClassic => Command::shell("corepack enable && yarn install --frozen-lockfile"),
            Pm::YarnBerry => Command::shell("corepack enable && yarn install --immutable"),
            Pm::Bun => Command::argv(&["bun", "install", "--frozen-lockfile"]),
        }
    }

    pub fn run_script(self, script: &str) -> Vec<String> {
        vec![self.name().to_string(), "run".to_string(), script.to_string()]
    }

    /// (cache mount id, mount target, env var pointing the tool at it)
    pub fn cache(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Pm::Npm => ("npm", "/cache/npm", "npm_config_cache"),
            Pm::Pnpm => ("pnpm", "/cache/pnpm", "npm_config_store_dir"),
            Pm::YarnClassic | Pm::YarnBerry => ("yarn", "/cache/yarn", "YARN_CACHE_FOLDER"),
            Pm::Bun => ("bun", "/cache/bun", "BUN_INSTALL_CACHE_DIR"),
        }
    }

    pub fn prune(self) -> Option<Command> {
        match self {
            Pm::Npm => Some(Command::argv(&["npm", "prune", "--omit=dev"])),
            Pm::Pnpm => Some(Command::shell("pnpm prune --prod")),
            _ => None,
        }
    }

    pub fn lock_inputs(self, repo: &RepoView) -> Vec<String> {
        let mut v = vec!["package.json".to_string()];
        v.extend(self.kind().lockfiles().iter().filter(|f| repo.is_file(f)).map(|f| f.to_string()));
        let optional_files: &[&str] = match self {
            Pm::Pnpm => &["pnpm-workspace.yaml"],
            Pm::YarnClassic | Pm::YarnBerry => &[".yarnrc.yml"],
            Pm::Bun => &["bunfig.toml"],
            Pm::Npm => &[],
        };
        v.extend(optional_files.iter().filter(|f| repo.is_file(f)).map(|f| f.to_string()));
        if matches!(self, Pm::YarnClassic | Pm::YarnBerry) {
            for dir in [".yarn/releases", ".yarn/plugins"] {
                if repo.is_dir(dir) {
                    v.push(format!("{dir}/**"));
                }
            }
        }
        if repo.is_file(".npmrc") {
            v.push(".npmrc".into());
        }
        v
    }
}

fn yarn_flavor(repo: &RepoView, pkg: &PackageJson) -> Pm {
    if let Some((name, version)) = pkg.package_manager()
        && name == "yarn"
    {
        return if version.starts_with("1.") { Pm::YarnClassic } else { Pm::YarnBerry };
    }
    match repo.read_string("yarn.lock", LOCKFILE_CAP) {
        Ok(Some(text)) if text.lines().take(5).any(|l| l.contains("yarn lockfile v1")) => Pm::YarnClassic,
        _ => Pm::YarnBerry,
    }
}

pub(crate) fn detect_pm(
    repo: &RepoView,
    pkg: &PackageJson,
    opts: &DetectOptions,
    provider: ProviderId,
    decisions: &mut Vec<Decision>,
) -> Result<Pm, DetectError> {
    let to_pm = |k: Kind| match k {
        Kind::Npm => Pm::Npm,
        Kind::Pnpm => Pm::Pnpm,
        Kind::Yarn => yarn_flavor(repo, pkg),
        Kind::Bun => Pm::Bun,
    };
    let missing = |k: Kind| DetectError::MissingFile {
        provider,
        file: k.lockfiles()[0].to_string(),
        hint: format!("{} is the package manager but its lockfile is missing; commit it so builds are reproducible", k.as_str()),
    };
    let key = opts.key("node.package_manager");
    if let Some(name) = &opts.node.package_manager {
        let k = Kind::parse(name).ok_or_else(|| DetectError::NeedsConfig {
            provider,
            message: format!("unknown package manager `{name}` (npm, pnpm, yarn or bun)"),
            key: key.clone(),
        })?;
        if k.present(repo).is_none() {
            return Err(missing(k));
        }
        decisions.push(Decision::config("node.package_manager", k.as_str(), key));
        return Ok(to_pm(k));
    }
    if let Some((name, _)) = pkg.package_manager()
        && let Some(k) = Kind::parse(&name)
    {
        if k.present(repo).is_none() {
            return Err(missing(k));
        }
        let field = pkg.str_field("packageManager").unwrap_or_default();
        decisions.push(Decision::detected("node.package_manager", k.as_str(), [format!("package.json packageManager = {field}")]));
        return Ok(to_pm(k));
    }
    let present: Vec<(Kind, &str)> =
        [Kind::Pnpm, Kind::Yarn, Kind::Npm, Kind::Bun].into_iter().filter_map(|k| k.present(repo).map(|f| (k, f))).collect();
    match present.as_slice() {
        [] => Err(DetectError::MissingFile {
            provider,
            file: "a lockfile".into(),
            hint: "commit package-lock.json, pnpm-lock.yaml, yarn.lock or bun.lock so builds are reproducible".into(),
        }),
        [(k, file)] => {
            decisions.push(Decision::detected("node.package_manager", k.as_str(), [format!("{file} is present")]));
            Ok(to_pm(*k))
        }
        many => {
            let chosen = if many.iter().any(|(k, _)| *k == Kind::Npm) { Kind::Npm } else { many[0].0 };
            let files: Vec<&str> = many.iter().map(|(_, f)| *f).collect();
            decisions.push(Decision::detected(
                "node.package_manager",
                chosen.as_str(),
                [format!("several lockfiles ({}); chose {}; set {key} to change", files.join(", "), chosen.as_str())],
            ));
            Ok(to_pm(chosen))
        }
    }
}

pub(crate) fn deps_steps(pm: Pm, repo: &RepoView, prefix: &str) -> Vec<Step> {
    let (id, target, env) = pm.cache();
    vec![
        inputs(&format!("{prefix}deps-inputs"), pm.lock_inputs(repo)),
        run(&format!("{prefix}deps"), pm.install(), &[(env, target)], &[(id, target)]),
    ]
}

fn npm_range_source(range: &str, desc: String) -> Result<VersionSource, DetectError> {
    let set = parse_npm_range(range).map_err(|e| bad_constraint("version", range, &desc, e))?;
    Ok(VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false })
}

pub(crate) fn node_version(
    repo: &RepoView,
    pkg: &PackageJson,
    opts: &DetectOptions,
    decisions: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.node.as_deref(), &opts.key("runtime.node"), "node")?;
    for f in [".nvmrc", ".node-version"] {
        if src.is_none() {
            src = version_from_file(repo, f, "node")?;
        }
    }
    if src.is_none()
        && let Some(r) = pkg.engine("node")
    {
        src = Some(npm_range_source(r, format!("package.json engines.node = {r}"))?);
    }
    catalog::pick(&NODE, "runtime.node", src, decisions)
}

pub(crate) fn bun_version(
    repo: &RepoView,
    pkg: &PackageJson,
    opts: &DetectOptions,
    decisions: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.bun.as_deref(), &opts.key("runtime.bun"), "bun")?;
    if src.is_none() {
        src = version_from_file(repo, ".bun-version", "bun")?;
    }
    if src.is_none()
        && let Some((name, version)) = pkg.package_manager()
        && name == "bun"
    {
        let desc = format!("package.json packageManager = bun@{version}");
        let set = parse_partial(&version).and_then(|p| p.x_range()).map_err(|e| bad_constraint("bun", &version, &desc, e))?;
        src = Some(VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false });
    }
    if src.is_none()
        && let Some(r) = pkg.engine("bun")
    {
        src = Some(npm_range_source(r, format!("package.json engines.bun = {r}"))?);
    }
    catalog::pick(&BUN, "runtime.bun", src, decisions)
}

pub(crate) struct BackendAssets {
    pub runtime: CatalogRef,
    pub deps: Vec<Step>,
    pub assets: Step,
}

/// Frontend assets for a backend app (Laravel): node or bun deps plus the `build` script.
pub(crate) fn assets_for_backend(
    repo: &RepoView,
    opts: &DetectOptions,
    provider: ProviderId,
    decisions: &mut Vec<Decision>,
) -> Result<Option<BackendAssets>, DetectError> {
    let Some(pkg) = PackageJson::read(repo)? else { return Ok(None) };
    if pkg.script("build").is_none() {
        decisions.push(Decision::detected("assets", "none", ["package.json has no build script"]));
        return Ok(None);
    }
    let pm = detect_pm(repo, &pkg, opts, provider, decisions)?;
    let runtime = if pm == Pm::Bun {
        cref("bun", Some(bun_version(repo, &pkg, opts, decisions)?.version))
    } else {
        cref("node", Some(node_version(repo, &pkg, opts, decisions)?.version))
    };
    Ok(Some(BackendAssets {
        runtime,
        deps: deps_steps(pm, repo, "node-"),
        assets: run("assets", Command::Argv(pm.run_script("build")), &[], &[]),
    }))
}
```

`crates/potter-detect/src/providers/node.rs`:

```rust
//! Node.js applications (spec §6.1 "Node").

use potter_plan::{Command, Decision, ImageConfig, Process};

use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson};
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::Claim;
use crate::{DetectError, ProviderId};

pub(crate) const OTHER_RUNTIME_FILES: &[&str] = &["bun.lock", "bun.lockb", "deno.json", "deno.jsonc"];

pub(crate) struct NodeProvider;

impl Provider for NodeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Node
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        if OTHER_RUNTIME_FILES.iter().any(|f| repo.is_file(f)) {
            return Ok(None);
        }
        let Some(pkg) = PackageJson::read(repo)? else { return Ok(None) };
        Ok(Some(Claim {
            provider: ProviderId::Node,
            refines: None,
            kind: js::js_kind(&pkg, &[]),
            evidence: vec!["package.json".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let pkg = PackageJson::read(repo)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Node,
            file: "package.json".into(),
            hint: "the Node provider builds npm projects".into(),
        })?;
        let mut d = Vec::new();
        let pm = js::detect_pm(repo, &pkg, opts, ProviderId::Node, &mut d)?;
        let node = js::node_version(repo, &pkg, opts, &mut d)?;
        let mut steps = js::deps_steps(pm, repo, "");
        steps.push(source());
        if pkg.script("build").is_some() {
            steps.push(run("build", Command::Argv(pm.run_script("build")), &[], &[]));
        }
        if let Some(prune) = pm.prune() {
            steps.push(run("prune", prune, &[], &[]));
        }
        let port = port(opts, 3000, &mut d);
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("NODE_ENV".into(), "production".into());

        let (exports, web) = if pkg.dep("next") && next_standalone(repo)? {
            d.push(Decision::detected("process.web", "next standalone", ["next.config output = standalone"]));
            config.env.insert("HOSTNAME".into(), "0.0.0.0".into());
            let mut e = vec![export("/app/.next/standalone", "/app"), export("/app/.next/static", "/app/.next/static")];
            if repo.is_dir("public") {
                e.push(export("/app/public", "/app/public"));
            }
            (e, vec!["node".to_string(), "server.js".to_string()])
        } else if pkg.dep("nuxt") {
            d.push(Decision::detected("process.web", "nuxt", ["package.json dependencies.nuxt"]));
            config.env.insert("HOST".into(), "0.0.0.0".into());
            (vec![export("/app/.output", "/app/.output")], vec!["node".to_string(), ".output/server/index.mjs".to_string()])
        } else if pkg.script("start").is_some() {
            d.push(Decision::detected("process.web", "start script", ["package.json scripts.start"]));
            (vec![export("/app", "/app")], pm.run_script("start"))
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Node,
                hint: format!("add a `start` script to package.json, or set `{}` in potter.toml", opts.key("process.web.cmd")),
            });
        };

        let mut image = empty_image(vec![cref("os-min", None), cref("node", Some(node.version))], config);
        image.export = exports;
        image.processes.insert("web".into(), Process { command: web, workdir: None, env: Default::default() });
        let layers = vec![cref("os-min", None), cref("node", Some(node.version))];
        Ok(Detection { target: new_target(target, ProviderId::Node, layers, steps, image), decisions: d })
    }
}

fn next_standalone(repo: &RepoView) -> Result<bool, DetectError> {
    for f in ["next.config.js", "next.config.mjs", "next.config.cjs", "next.config.ts", "next.config.mts"] {
        if let Some(text) = repo.read_string(f, READ_CAP)?
            && quoted_value_after(&text, "output", ':').as_deref() == Some("standalone")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
```

Register it in `crates/potter-detect/src/providers/mod.rs`:

```rust
pub(crate) mod js;
mod node;
```

```rust
pub fn all() -> Vec<&'static dyn Provider> {
    vec![&php::PhpProvider, &node::NodeProvider]
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes.

If `nestjs` reports a node version other than 24, check `engines.node` in `fixtures/nestjs/package.json` (the survey found `>=20.11`).

- [ ] **Step 6: Commit**

```bash
git add crates/potter-detect fixtures/nestjs fixtures/next-standalone fixtures/nuxt fixtures/hono-node fixtures/vue-node
git commit -m "feat(potter-detect): JS toolchain helpers and the Node provider"
```

---

### Task 11: The Laravel provider

**Files:**
- Create: `crates/potter-detect/src/providers/laravel.rs`, `crates/potter-detect/tests/laravel.rs`
- Create fixtures: `fixtures/laravel/` (copied), `fixtures/laravel-octane/`, `fixtures/laravel-horizon-pnpm/`
- Modify: `crates/potter-detect/src/providers/mod.rs`

**Interfaces:**
- Consumes:
  - `php::{read_facts, build_php, requires, PhpFacts}` (Task 9);
  - `js::assets_for_backend` (Task 10);
  - `common::{insert_after, run, cref}` and `scan::quoted_value_after`.
- Produces: `LaravelProvider` (refines `Php`).
- Step names: `deps-inputs`, `deps`, `node-deps-inputs`, `node-deps`, `source`, `assets`, `autoload`, `discover`.
- Processes: `web`, `worker`, `scheduler`.

- [ ] **Step 1: Create the fixtures**

```bash
L=/Users/alfonso/Github/Personal/laravel-dev-starter
mkdir -p fixtures/laravel/public fixtures/laravel-octane/public fixtures/laravel-octane/config fixtures/laravel-horizon-pnpm/public
cp "$L/composer.json" "$L/composer.lock" "$L/artisan" "$L/package.json" "$L/package-lock.json" "$L/yarn.lock" "$L/vite.config.js" fixtures/laravel/
printf '<?php\n' > fixtures/laravel/public/index.php
for d in laravel-octane laravel-horizon-pnpm; do printf '#!/usr/bin/env php\n<?php\n' > fixtures/$d/artisan; printf '<?php\n' > fixtures/$d/public/index.php; done
```

`fixtures/laravel-octane/composer.json`:

```json
{
    "name": "potter/laravel-octane",
    "require": {
        "php": "^8.3",
        "laravel/framework": "^12.0",
        "laravel/octane": "^2.6"
    }
}
```

`fixtures/laravel-octane/composer.lock`:

```json
{
    "packages": [
        { "name": "laravel/framework", "version": "v12.0.0", "require": { "php": "^8.2", "ext-mbstring": "*", "ext-openssl": "*" } },
        { "name": "laravel/octane", "version": "v2.6.0", "require": { "php": "^8.1" } }
    ]
}
```

`fixtures/laravel-octane/config/octane.php`:

```php
<?php

return [
    'server' => env('OCTANE_SERVER', 'swoole'),
];
```

`fixtures/laravel-horizon-pnpm/composer.json`:

```json
{
    "name": "potter/laravel-horizon-pnpm",
    "require": {
        "php": "^8.2",
        "laravel/framework": "^12.0",
        "laravel/horizon": "^5.30"
    }
}
```

`fixtures/laravel-horizon-pnpm/composer.lock`:

```json
{
    "packages": [
        { "name": "laravel/framework", "version": "v12.0.0", "require": { "php": "^8.2" } },
        { "name": "laravel/horizon", "version": "v5.30.0", "require": { "ext-pcntl": "*", "ext-posix": "*" } }
    ]
}
```

`fixtures/laravel-horizon-pnpm/package.json`:

```json
{
    "private": true,
    "type": "module",
    "packageManager": "pnpm@9.15.0",
    "scripts": { "build": "vite build", "dev": "vite" },
    "devDependencies": { "vite": "^6.0.0", "laravel-vite-plugin": "^1.2.0" }
}
```

`fixtures/laravel-horizon-pnpm/pnpm-lock.yaml`:

```yaml
lockfileVersion: '9.0'
```

- [ ] **Step 2: Write the failing tests**

`crates/potter-detect/tests/laravel.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, PhpOptions};

#[test]
fn laravel_with_two_lockfiles_uses_npm() {
    let d = detect("laravel");
    let t = &d.target;
    assert_eq!(t.provider, "laravel");
    assert_eq!(decision(&d, "provider").value, "laravel");
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    assert_eq!(step_names(t), ["deps-inputs", "deps", "node-deps-inputs", "node-deps", "source", "assets", "autoload", "discover"]);
    assert_eq!(command(t, "node-deps"), "npm ci --no-audit --no-fund");
    let pm = decision(&d, "node.package_manager");
    assert_eq!(pm.value, "npm");
    assert!(pm.origin.to_string().contains("several lockfiles"), "{}", pm.origin);
    assert_eq!(command(t, "assets"), "npm run build");
    assert!(command(t, "autoload").ends_with("--no-scripts"));
    assert_eq!(command(t, "discover"), "php artisan package:discover --ansi");
    let build = layers(&t.build.base);
    assert_eq!(build.last().unwrap(), "node@22");
    for ext in ["php-ext-bcmath@8.4", "php-ext-redis@8.4", "php-ext-zip@8.4", "php-ext-pcntl@8.4"] {
        assert!(build.contains(&ext.to_string()), "{ext} missing from {build:?}");
    }
    assert!(!layers(&t.image.base).iter().any(|l| l.starts_with("node")), "node must not ship");
    assert_eq!(process(t, "web"), "nginx -c /etc/potter/nginx.conf");
    assert_eq!(process(t, "worker"), "php artisan queue:work");
    assert_eq!(process(t, "scheduler"), "php artisan schedule:work");
    assert_eq!(t.image.services["web"][0].name, "php-fpm");
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn octane_swoole() {
    let t = detect("laravel-octane").target;
    let run = layers(&t.image.base);
    assert!(run.contains(&"php-ext-swoole@8.4".to_string()), "{run:?}");
    assert!(!run.iter().any(|l| l == "nginx" || l.starts_with("php-fpm")), "{run:?}");
    assert_eq!(process(&t, "web"), "php artisan octane:start --server=swoole --host=0.0.0.0 --port=8080");
    assert!(t.image.services.is_empty());
    assert!(t.image.files.is_empty());
    assert!(!t.image.config.env.contains_key("PHP_FPM_MAX_CHILDREN"));
    assert!(!step_names(&t).contains(&"assets"));
}

#[test]
fn octane_server_needs_config_when_unknown() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{"laravel/framework":"^12.0","laravel/octane":"^2.6"}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
    ]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(matches!(&e, DetectError::NeedsConfig { key, .. } if key == "target.app.php.octane_server"), "{e}");
    let opts = DetectOptions { php: PhpOptions { octane_server: Some("frankenphp".into()), ..Default::default() }, ..Default::default() };
    let t = detect_at(r.path(), &opts).unwrap().target;
    assert!(layers(&t.image.base).contains(&"frankenphp".to_string()));
    assert!(process(&t, "web").contains("--server=frankenphp"));
    let opts = DetectOptions { php: PhpOptions { octane_server: Some("apache".into()), ..Default::default() }, ..Default::default() };
    assert!(matches!(detect_at(r.path(), &opts), Err(DetectError::NeedsConfig { .. })));
}

#[test]
fn roadrunner_from_packages() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{"laravel/framework":"^12.0","laravel/octane":"^2.6","spiral/roadrunner-http":"^3.0"}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert!(layers(&t.image.base).contains(&"roadrunner".to_string()));
}

#[test]
fn horizon_and_pnpm_assets() {
    let t = detect("laravel-horizon-pnpm").target;
    assert_eq!(command(&t, "node-deps"), "corepack enable && pnpm install --frozen-lockfile");
    assert_eq!(command(&t, "assets"), "pnpm run build");
    assert_eq!(process(&t, "worker"), "php artisan horizon");
}

#[test]
fn package_json_without_build_has_no_assets() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{"laravel/framework":"^12.0"}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
        ("package.json", r#"{"scripts":{"dev":"vite"}}"#),
    ]);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert!(!step_names(&d.target).contains(&"assets"));
    assert_eq!(decision(&d, "assets").value, "none");
}

#[test]
fn artisan_alone_is_not_laravel() {
    let r = temp_repo(&[("composer.json", r#"{"require":{}}"#), ("composer.lock", r#"{"packages":[]}"#), ("artisan", "<?php")]);
    assert_eq!(detect_at(r.path(), &DetectOptions::default()).unwrap().target.provider, "php");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-detect --test laravel`
Expected: fails, because the provider is `php`, not `laravel`.

- [ ] **Step 4: Implement**

`crates/potter-detect/src/providers/laravel.rs`:

```rust
//! Laravel (spec §6.1 "Laravel"). Builds on the PHP provider and adds assets, package
//! discovery, Octane and worker/scheduler processes.

use std::collections::BTreeSet;

use potter_plan::{Base, Command, Decision, Process, Step};

use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider, js, php};
use crate::repo::{READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::{Claim, ClaimKind};
use crate::{DetectError, ProviderId};

const LARAVEL_EXTENSIONS: &[&str] = &["bcmath", "intl", "pcntl", "pdo_mysql", "pdo_pgsql", "redis"];
const OCTANE_SERVERS: &[&str] = &["swoole", "roadrunner", "frankenphp"];
const ROADRUNNER_PACKAGES: &[&str] = &["spiral/roadrunner", "spiral/roadrunner-http", "spiral/roadrunner-cli"];

pub(crate) struct LaravelProvider;

impl Provider for LaravelProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Laravel
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        if !repo.is_file("artisan") {
            return Ok(None);
        }
        let Some(composer) = repo.read_json("composer.json")? else { return Ok(None) };
        Ok(php::requires(&composer, "laravel/framework").then(|| Claim {
            provider: ProviderId::Laravel,
            refines: Some(ProviderId::Php),
            kind: ClaimKind::Backend,
            evidence: vec!["composer.json requires laravel/framework".into(), "artisan".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let mut facts = php::read_facts(repo, opts, ProviderId::Laravel, LARAVEL_EXTENSIONS, "Laravel default extension")?;
        let server = if php::requires(&facts.composer, "laravel/octane") {
            Some(octane_server(repo, opts, &facts.composer, &facts.extensions, &mut facts.decisions)?)
        } else {
            None
        };
        if server == Some("swoole") && facts.extensions.insert("swoole".into()) {
            facts.decisions.push(Decision::detected("runtime.extensions", "swoole", ["Octane server swoole"]));
        }
        let horizon = php::requires(&facts.composer, "laravel/horizon");
        let mut det = php::build_php(repo, facts, opts, ProviderId::Laravel, target)?;
        let t = &mut det.target;

        // Package discovery runs as its own step, so autoload must not run it through scripts.
        for s in &mut t.build.steps {
            if let Step::Run { name, command, .. } = s
                && name == "autoload"
            {
                *command = Command::shell("composer dump-autoload --optimize --no-dev --no-interaction --no-scripts");
            }
        }
        insert_after(
            &mut t.build.steps,
            "autoload",
            vec![run("discover", Command::argv(&["php", "artisan", "package:discover", "--ansi"]), &[], &[])],
        );

        if let Some(assets) = js::assets_for_backend(repo, opts, ProviderId::Laravel, &mut det.decisions)? {
            if let Base::Catalog { layers } = &mut t.build.base {
                layers.push(assets.runtime);
            }
            insert_after(&mut t.build.steps, "deps", assets.deps);
            insert_after(&mut t.build.steps, "source", vec![assets.assets]);
        }

        let worker: &[&str] = if horizon { &["php", "artisan", "horizon"] } else { &["php", "artisan", "queue:work"] };
        let evidence = if horizon { "composer.json requires laravel/horizon" } else { "Laravel queue worker" };
        det.decisions.push(Decision::detected("process.worker", worker.join(" "), [evidence]));
        t.image.processes.insert("worker".into(), Process::new(worker));
        t.image.processes.insert("scheduler".into(), Process::new(&["php", "artisan", "schedule:work"]));

        if let Some(server) = server {
            if let Base::Catalog { layers } = &mut t.image.base {
                layers.retain(|l| l.name != "php-fpm" && l.name != "nginx");
                if server != "swoole" {
                    layers.push(cref(server, None));
                }
            }
            t.image.files.clear();
            t.image.services.clear();
            t.image.config.stop_signal = None;
            t.image.config.env.remove("PHP_FPM_MAX_CHILDREN");
            let port = t.image.config.port.unwrap_or(8080);
            let server_flag = format!("--server={server}");
            let port_flag = format!("--port={port}");
            t.image.processes.insert(
                "web".into(),
                Process::new(&["php", "artisan", "octane:start", &server_flag, "--host=0.0.0.0", &port_flag]),
            );
        }
        Ok(det)
    }
}

fn octane_server(
    repo: &RepoView,
    opts: &DetectOptions,
    composer: &serde_json::Value,
    extensions: &BTreeSet<String>,
    decisions: &mut Vec<Decision>,
) -> Result<&'static str, DetectError> {
    let key = opts.key("php.octane_server");
    let known = |s: &str| OCTANE_SERVERS.iter().copied().find(|x| *x == s);
    if let Some(s) = &opts.php.octane_server {
        let server = known(s).ok_or_else(|| DetectError::NeedsConfig {
            provider: ProviderId::Laravel,
            message: format!("unknown Octane server `{s}` (swoole, roadrunner or frankenphp)"),
            key: key.clone(),
        })?;
        decisions.push(Decision::config("php.octane_server", server, key));
        return Ok(server);
    }
    if let Some(text) = repo.read_string("config/octane.php", READ_CAP)?
        && let Some(s) = quoted_value_after(&text, "OCTANE_SERVER", ',')
        && let Some(server) = known(&s)
    {
        decisions.push(Decision::detected("php.octane_server", server, ["config/octane.php OCTANE_SERVER default"]));
        return Ok(server);
    }
    if let Some(p) = ROADRUNNER_PACKAGES.iter().find(|p| php::requires(composer, p)) {
        decisions.push(Decision::detected("php.octane_server", "roadrunner", [format!("composer.json requires {p}")]));
        return Ok("roadrunner");
    }
    if extensions.contains("swoole") || extensions.contains("openswoole") {
        decisions.push(Decision::detected("php.octane_server", "swoole", ["the swoole extension is required"]));
        return Ok("swoole");
    }
    Err(DetectError::NeedsConfig {
        provider: ProviderId::Laravel,
        message: "laravel/octane is installed but its server can't be determined".into(),
        key,
    })
}
```

Register it in `crates/potter-detect/src/providers/mod.rs`:

```rust
mod laravel;
```

```rust
pub fn all() -> Vec<&'static dyn Provider> {
    vec![&php::PhpProvider, &laravel::LaravelProvider, &node::NodeProvider]
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes.

If `laravel_with_two_lockfiles_uses_npm` fails on an extension, check which `ext-*` keys `fixtures/laravel/composer.lock` actually has. Adjust the asserted list only to extensions present there (zip came from spatie/laravel-backup in the survey).

- [ ] **Step 6: Commit**

```bash
git add crates/potter-detect fixtures/laravel fixtures/laravel-octane fixtures/laravel-horizon-pnpm
git commit -m "feat(potter-detect): Laravel provider with assets, Octane and workers"
```

---

### Task 12: The Bun and Deno providers

**Files:**
- Create: `crates/potter-detect/src/providers/bun.rs`, `crates/potter-detect/src/providers/deno.rs`, `crates/potter-detect/tests/bun_deno.rs`
- Create fixtures: `fixtures/bun/` (copied), `fixtures/deno-serve/`, `fixtures/deno-fresh/`
- Modify: `crates/potter-detect/src/providers/mod.rs`

**Interfaces:**
- Consumes:
  - `js::{PackageJson, Pm, deps_steps, bun_version, js_kind}`;
  - `catalog::{pick, DENO}`;
  - `common::*` and `RepoView::read_jsonc`.
- Produces: `BunProvider` and `DenoProvider`. `pub(crate) BUN_SERVER_DEPS` is used by Static.
- Bun step names: `deps-inputs`, `deps`, `source`, `build`. Deno uses the same names, with `DENO_DIR=/app/.deno` on every Run step and in the image env, and no cache mount.

- [ ] **Step 1: Create the fixtures**

```bash
B=/Users/alfonso/Github/Personal/bun-test
mkdir -p fixtures/bun fixtures/deno-serve fixtures/deno-fresh
cp "$B/package.json" "$B/bun.lock" "$B/bunfig.toml" fixtures/bun/
```

`fixtures/deno-serve/deno.json`:

```json
{
  "imports": { "@std/http": "jsr:@std/http@^1.0.0" }
}
```

`fixtures/deno-serve/main.ts`:

```ts
export default {
  fetch(): Response {
    return new Response("ok");
  },
};
```

`fixtures/deno-fresh/deno.jsonc`:

```jsonc
{
  // Fresh-style tasks
  "tasks": {
    "build": "deno run -A dev.ts build",
    "start": "deno run -A main.ts", // serves on PORT
  },
  "imports": { "fresh": "jsr:@fresh/core@^2.0.0" },
}
```

`fixtures/deno-fresh/deno.lock`:

```json
{ "version": "4" }
```

`fixtures/deno-fresh/main.ts`:

```ts
console.log("fresh");
```

- [ ] **Step 2: Write the failing tests**

`crates/potter-detect/tests/bun_deno.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn bun() {
    let d = detect("bun");
    let t = &d.target;
    assert_eq!(t.provider, "bun");
    assert_eq!(layers(&t.build.base), ["os-min", "bun@1"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(includes(t, "deps-inputs"), ["package.json", "bun.lock", "bunfig.toml"]);
    assert_eq!(command(t, "deps"), "bun install --frozen-lockfile");
    assert_eq!(command(t, "build"), "bun run build");
    assert_eq!(process(t, "web"), "bun run start");
    assert_eq!(layers(&t.image.base), ["os-min", "bun@1"]);
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn bun_with_a_stray_package_lock_still_uses_bun() {
    let r = temp_repo(&[("package.json", r#"{"scripts":{"start":"bun index.ts"}}"#), ("bun.lock", "{}"), ("package-lock.json", "{}")]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(command(&t, "deps"), "bun install --frozen-lockfile");
}

#[test]
fn bun_main_field_fallback() {
    let r = temp_repo(&[("package.json", r#"{"module":"src/index.ts","dependencies":{"hono":"^4"}}"#), ("bun.lock", "{}")]);
    assert_eq!(process(&detect_at(r.path(), &DetectOptions::default()).unwrap().target, "web"), "bun run src/index.ts");
}

#[test]
fn deno_serve() {
    let t = detect("deno-serve").target;
    assert_eq!(t.provider, "deno");
    assert_eq!(includes(&t, "deps-inputs"), ["deno.json"]);
    assert_eq!(command(&t, "deps"), "deno install");
    assert_eq!(process(&t, "web"), "deno serve --port 3000 main.ts");
    assert_eq!(t.image.config.env["DENO_DIR"], "/app/.deno");
    match step(&t, "deps") {
        potter_plan::Step::Run { env, cache_mounts, .. } => {
            assert_eq!(env["DENO_DIR"], "/app/.deno");
            assert!(cache_mounts.is_empty());
        }
        _ => unreachable!(),
    }
}

#[test]
fn deno_fresh_with_jsonc_and_lock() {
    let t = detect("deno-fresh").target;
    assert_eq!(includes(&t, "deps-inputs"), ["deno.jsonc", "deno.lock"]);
    assert_eq!(command(&t, "deps"), "deno install --frozen");
    assert_eq!(command(&t, "build"), "deno task build");
    assert_eq!(process(&t, "web"), "deno task start");
}

#[test]
fn deno_without_entry_has_no_process() {
    let r = temp_repo(&[("deno.json", "{}")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NoProcess { .. })));
}

#[test]
fn deno_version_file() {
    let r = temp_repo(&[("deno.json", "{}"), ("main.ts", ""), (".dvmrc", "1.46\n")]);
    let msg = detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert!(msg.contains("no available deno version satisfies .dvmrc"), "{msg}");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-detect --test bun_deno`
Expected: fails with `NoProvider`.

- [ ] **Step 4: Implement**

`crates/potter-detect/src/providers/bun.rs`:

```rust
//! Bun applications (spec §6.1 "Bun").

use potter_plan::{Command, Decision, ImageConfig, Process};

use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson, Pm};
use crate::providers::{Detection, Provider};
use crate::repo::RepoView;
use crate::select::Claim;
use crate::{DetectError, ProviderId};

/// Bun serves these frameworks natively, without a start script.
pub(crate) const BUN_SERVER_DEPS: &[&str] = &["hono", "elysia"];

pub(crate) struct BunProvider;

fn lockfile(repo: &RepoView) -> Option<&'static str> {
    ["bun.lock", "bun.lockb"].into_iter().find(|f| repo.is_file(f))
}

impl Provider for BunProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Bun
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        if repo.is_file("deno.json") || repo.is_file("deno.jsonc") {
            return Ok(None);
        }
        let Some(lock) = lockfile(repo) else { return Ok(None) };
        let Some(pkg) = PackageJson::read(repo)? else { return Ok(None) };
        Ok(Some(Claim {
            provider: ProviderId::Bun,
            refines: None,
            kind: js::js_kind(&pkg, BUN_SERVER_DEPS),
            evidence: vec!["package.json".into(), lock.into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let pkg = PackageJson::read(repo)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Bun,
            file: "package.json".into(),
            hint: "the Bun provider builds package.json projects".into(),
        })?;
        let mut d = Vec::new();
        let lock = lockfile(repo).unwrap_or("bun.lock");
        d.push(Decision::detected("node.package_manager", "bun", [format!("{lock} is present")]));
        let bun = js::bun_version(repo, &pkg, opts, &mut d)?;
        let mut steps = js::deps_steps(Pm::Bun, repo, "");
        steps.push(source());
        if pkg.script("build").is_some() {
            steps.push(run("build", Command::Argv(Pm::Bun.run_script("build")), &[], &[]));
        }
        let web = if pkg.script("start").is_some() {
            d.push(Decision::detected("process.web", "start script", ["package.json scripts.start"]));
            Pm::Bun.run_script("start")
        } else if let Some(entry) = pkg.str_field("module").or_else(|| pkg.str_field("main")) {
            d.push(Decision::detected("process.web", entry, ["package.json module/main"]));
            vec!["bun".into(), "run".into(), entry.to_string()]
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Bun,
                hint: format!("add a `start` script or a `module` entry to package.json, or set `{}`", opts.key("process.web.cmd")),
            });
        };
        let port = port(opts, 3000, &mut d);
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("NODE_ENV".into(), "production".into());
        let layers = vec![cref("os-min", None), cref("bun", Some(bun.version))];
        let mut image = empty_image(layers.clone(), config);
        image.export = vec![export("/app", "/app")];
        image.processes.insert("web".into(), Process { command: web, workdir: None, env: Default::default() });
        Ok(Detection { target: new_target(target, ProviderId::Bun, layers, steps, image), decisions: d })
    }
}
```

`crates/potter-detect/src/providers/deno.rs`:

```rust
//! Deno applications (spec §6.1 "Deno"). DENO_DIR lives in the layer because Deno needs its
//! module cache at runtime.

use potter_plan::{Command, Decision, ImageConfig, Process};
use serde_json::Value;

use crate::catalog::{self, DENO};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::RepoView;
use crate::select::{Claim, ClaimKind};
use crate::{DetectError, ProviderId};

const DENO_DIR: &str = "/app/.deno";
const ENTRIES: &[&str] = &["main.ts", "main.js", "mod.ts", "server.ts", "src/main.ts"];

pub(crate) struct DenoProvider;

fn config(repo: &RepoView) -> Result<Option<(&'static str, Value)>, DetectError> {
    let Some(file) = ["deno.json", "deno.jsonc"].into_iter().find(|f| repo.is_file(f)) else { return Ok(None) };
    Ok(repo.read_jsonc(file)?.map(|v| (file, v)))
}

fn has_task(cfg: &Value, name: &str) -> bool {
    cfg.get("tasks").and_then(|t| t.get(name)).is_some()
}

fn entry(repo: &RepoView) -> Option<&'static str> {
    ENTRIES.iter().copied().find(|f| repo.is_file(f))
}

impl Provider for DenoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Deno
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        let Some((file, cfg)) = config(repo)? else { return Ok(None) };
        let kind = if has_task(&cfg, "start") || entry(repo).is_some() { ClaimKind::JsServer } else { ClaimKind::JsAssets };
        Ok(Some(Claim { provider: ProviderId::Deno, refines: None, kind, evidence: vec![file.into()] }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let (file, cfg) = config(repo)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Deno,
            file: "deno.json".into(),
            hint: "the Deno provider builds deno.json projects".into(),
        })?;
        let mut d = Vec::new();
        let mut src = version_from_config(opts.runtime.deno.as_deref(), &opts.key("runtime.deno"), "deno")?;
        if src.is_none() {
            src = version_from_file(repo, ".dvmrc", "deno")?;
        }
        let deno = catalog::pick(&DENO, "runtime.deno", src, &mut d)?;

        let locked = repo.is_file("deno.lock");
        let mut deps_inputs = vec![file.to_string()];
        if locked {
            deps_inputs.push("deno.lock".into());
        }
        if repo.is_file("package.json") {
            deps_inputs.push("package.json".into());
        }
        if let Some(map) = cfg.get("importMap").and_then(Value::as_str)
            && potter_plan::is_relative_path(map)
            && repo.is_file(map)
        {
            deps_inputs.push(map.to_string());
        }
        let env = [("DENO_DIR", DENO_DIR)];
        let install = if locked { Command::argv(&["deno", "install", "--frozen"]) } else { Command::argv(&["deno", "install"]) };
        let mut steps = vec![inputs("deps-inputs", deps_inputs), run("deps", install, &env, &[]), source()];
        if has_task(&cfg, "build") {
            steps.push(run("build", Command::argv(&["deno", "task", "build"]), &env, &[]));
        }

        let port = port(opts, 3000, &mut d);
        let web: Vec<String> = if has_task(&cfg, "start") {
            d.push(Decision::detected("process.web", "deno task start", [format!("{file} tasks.start")]));
            vec!["deno".into(), "task".into(), "start".into()]
        } else if let Some(e) = entry(repo) {
            d.push(Decision::detected("process.web", e, [format!("{e} exists")]));
            vec!["deno".into(), "serve".into(), "--port".into(), port.to_string(), e.into()]
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Deno,
                hint: format!("add a `start` task to {file} or a main.ts, or set `{}`", opts.key("process.web.cmd")),
            });
        };
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("DENO_DIR".into(), DENO_DIR.into());
        let layers = vec![cref("os-min", None), cref("deno", Some(deno.version))];
        let mut image = empty_image(layers.clone(), config);
        image.export = vec![export("/app", "/app")];
        image.processes.insert("web".into(), Process { command: web, workdir: None, env: Default::default() });
        Ok(Detection { target: new_target(target, ProviderId::Deno, layers, steps, image), decisions: d })
    }
}
```

Register both in `crates/potter-detect/src/providers/mod.rs`:

```rust
pub(crate) mod bun;
mod deno;
```

```rust
pub fn all() -> Vec<&'static dyn Provider> {
    vec![&php::PhpProvider, &laravel::LaravelProvider, &node::NodeProvider, &bun::BunProvider, &deno::DenoProvider]
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: everything passes.

`fixtures/bun/package.json` has `start` and `build` scripts (per the survey). If it lacks `bunfig.toml`, drop that entry from the expected `deps-inputs`.

- [ ] **Step 6: Commit**

```bash
git add crates/potter-detect fixtures/bun fixtures/deno-serve fixtures/deno-fresh
git commit -m "feat(potter-detect): Bun and Deno providers"
```

---

### Task 13: The Static provider

**Files:**
- Create: `crates/potter-detect/src/providers/static_site.rs`, `crates/potter-detect/tests/static_site.rs`
- Create fixture: `fixtures/vite-static/`
- Modify: `crates/potter-detect/src/providers/mod.rs` (register the provider, and remove the `#[allow(dead_code)]` on `common`)

**Interfaces:**
- Consumes:
  - `NodeProvider` and `BunProvider` claims;
  - `js::{PackageJson, Pm, detect_pm, deps_steps, node_version, bun_version}`;
  - `scan::quoted_value_after`.
- Produces: `StaticProvider`, which refines Node or Bun.
- Image: base `[os-min, static-server]`; export `/app/<outDir>` → `/srv/www`; web `static-server --root /srv/www --listen 0.0.0.0:<port>`, with port 8080. That is the CLI contract the P3 `static-server` catalog layer must implement.

- [ ] **Step 1: Create the fixture**

```bash
mkdir -p fixtures/vite-static
printf '<!doctype html><title>static</title>\n' > fixtures/vite-static/index.html
```

`fixtures/vite-static/package.json`:

```json
{
  "name": "vite-static",
  "private": true,
  "type": "module",
  "scripts": { "build": "vite build", "dev": "vite" },
  "devDependencies": { "vite": "^6.0.11" }
}
```

`fixtures/vite-static/package-lock.json`:

```json
{ "name": "vite-static", "lockfileVersion": 3, "requires": true, "packages": {} }
```

`fixtures/vite-static/vite.config.ts`:

```ts
import { defineConfig } from "vite";

export default defineConfig({
  build: { outDir: "build" },
});
```

- [ ] **Step 2: Write the failing tests**

`crates/potter-detect/tests/static_site.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn vite_static_site() {
    let d = detect("vite-static");
    let t = &d.target;
    assert_eq!(t.provider, "static");
    assert_eq!(layers(&t.build.base), ["os-min", "node@22"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(layers(&t.image.base), ["os-min", "static-server"]);
    assert_eq!(t.image.export[0].path, "/app/build");
    assert_eq!(t.image.export[0].dest, "/srv/www");
    assert_eq!(process(t, "web"), "static-server --root /srv/www --listen 0.0.0.0:8080");
    assert_eq!(decision(&d, "static.out_dir").value, "build");
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn default_out_dir_and_bun() {
    let r = temp_repo(&[("package.json", r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"^6"}}"#), ("bun.lock", "{}")]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(t.provider, "static");
    assert_eq!(command(&t, "deps"), "bun install --frozen-lockfile");
    assert_eq!(command(&t, "build"), "bun run build");
    assert_eq!(t.image.export[0].path, "/app/dist");
}

#[test]
fn a_start_script_means_node_not_static() {
    assert_eq!(detect("vue-node").target.provider, "node");
}

#[test]
fn laravel_still_wins_over_its_vite_assets() {
    assert_eq!(detect("laravel").target.provider, "laravel");
}

#[test]
fn escaping_out_dir_is_refused() {
    let r = temp_repo(&[
        ("package.json", r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"^6"}}"#),
        ("package-lock.json", "{}"),
        ("vite.config.js", "export default { build: { outDir: '../../etc' } }"),
    ]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NeedsConfig { .. })));
}

#[test]
fn create_react_app() {
    let r = temp_repo(&[("package.json", r#"{"scripts":{"build":"react-scripts build"},"dependencies":{"react-scripts":"5.0.1"}}"#), ("package-lock.json", "{}")]);
    assert_eq!(detect_at(r.path(), &DetectOptions::default()).unwrap().target.image.export[0].path, "/app/build");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-detect --test static_site`
Expected: `vite_static_site` fails. Without Static registered, the provider is `node` and fails with `NoProcess`.

- [ ] **Step 4: Implement**

`crates/potter-detect/src/providers/static_site.rs`:

```rust
//! Static sites: a JS build with no server runtime (spec §6.1 "Static").

use potter_plan::{Command, Decision, ImageConfig, Process};

use crate::options::DetectOptions;
use crate::providers::bun::BunProvider;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson, Pm};
use crate::providers::node::NodeProvider;
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::{Claim, ClaimKind};
use crate::{DetectError, ProviderId};

const VITE_CONFIGS: &[&str] = &["vite.config.ts", "vite.config.js", "vite.config.mjs", "vite.config.mts", "vite.config.cjs"];

pub(crate) struct StaticProvider;

/// The underlying JS provider's claim, if it is asset-only.
fn underlying(repo: &RepoView, opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
    let candidates: [&dyn Provider; 2] = [&NodeProvider, &BunProvider];
    for p in candidates {
        if let Some(c) = p.claim(repo, opts)? {
            return Ok((c.kind == ClaimKind::JsAssets).then_some(c));
        }
    }
    Ok(None)
}

/// (tool, output dir, evidence)
fn static_tool(repo: &RepoView, pkg: &PackageJson) -> Result<Option<(&'static str, String, String)>, DetectError> {
    if pkg.any_dep("vite") {
        for f in VITE_CONFIGS {
            if let Some(text) = repo.read_string(f, READ_CAP)?
                && let Some(out) = quoted_value_after(&text, "outDir", ':')
            {
                return Ok(Some(("vite", out, format!("{f} build.outDir"))));
            }
        }
        return Ok(Some(("vite", "dist".into(), "vite default outDir".into())));
    }
    for (tool, out) in [("react-scripts", "build"), ("astro", "dist"), ("@vue/cli-service", "dist")] {
        if pkg.any_dep(tool) {
            return Ok(Some((tool, out.into(), format!("{tool} default output"))));
        }
    }
    Ok(None)
}

impl Provider for StaticProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Static
    }

    fn claim(&self, repo: &RepoView, opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        let Some(base) = underlying(repo, opts)? else { return Ok(None) };
        let Some(pkg) = PackageJson::read(repo)? else { return Ok(None) };
        if pkg.script("build").is_none() {
            return Ok(None);
        }
        let Some((tool, _, _)) = static_tool(repo, &pkg)? else { return Ok(None) };
        Ok(Some(Claim {
            provider: ProviderId::Static,
            refines: Some(base.provider),
            kind: ClaimKind::JsAssets,
            evidence: vec![format!("{tool} build with no server runtime")],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let base = underlying(repo, opts)?.ok_or(DetectError::ForcedNotApplicable(ProviderId::Static))?;
        let pkg = PackageJson::read(repo)?.ok_or(DetectError::ForcedNotApplicable(ProviderId::Static))?;
        let (_, out_dir, evidence) =
            static_tool(repo, &pkg)?.ok_or(DetectError::ForcedNotApplicable(ProviderId::Static))?;
        let out = out_dir.trim_start_matches("./").trim_end_matches('/').to_string();
        if out.is_empty() || out == "." || !potter_plan::is_relative_path(&out) {
            return Err(DetectError::NeedsConfig {
                provider: ProviderId::Static,
                message: format!("the build output directory `{out_dir}` must be a subdirectory of the project"),
                key: opts.key("process.web.cmd"),
            });
        }
        let mut d = vec![Decision::detected("static.out_dir", out.as_str(), [evidence])];
        let (pm, runtime) = if base.provider == ProviderId::Bun {
            d.push(Decision::detected("node.package_manager", "bun", ["bun lockfile is present"]));
            (Pm::Bun, cref("bun", Some(js::bun_version(repo, &pkg, opts, &mut d)?.version)))
        } else {
            let pm = js::detect_pm(repo, &pkg, opts, ProviderId::Static, &mut d)?;
            (pm, cref("node", Some(js::node_version(repo, &pkg, opts, &mut d)?.version)))
        };
        let mut steps = js::deps_steps(pm, repo, "");
        steps.push(source());
        steps.push(run("build", Command::Argv(pm.run_script("build")), &[], &[]));

        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/srv/www", "root");
        config.port = Some(port);
        let mut image = empty_image(vec![cref("os-min", None), cref("static-server", None)], config);
        image.export = vec![export(&format!("/app/{out}"), "/srv/www")];
        let listen = format!("0.0.0.0:{port}");
        image.processes.insert("web".into(), Process::new(&["static-server", "--root", "/srv/www", "--listen", &listen]));
        let layers = vec![cref("os-min", None), runtime];
        Ok(Detection { target: new_target(target, ProviderId::Static, layers, steps, image), decisions: d })
    }
}
```

`crates/potter-detect/src/providers/mod.rs`, final form of the module list and registry:

```rust
pub(crate) mod bun;
pub(crate) mod common;
mod deno;
pub(crate) mod js;
mod laravel;
pub(crate) mod node;
mod php;
mod php_config;
mod static_site;
```

```rust
pub fn all() -> Vec<&'static dyn Provider> {
    vec![
        &php::PhpProvider,
        &laravel::LaravelProvider,
        &node::NodeProvider,
        &bun::BunProvider,
        &deno::DenoProvider,
        &static_site::StaticProvider,
    ]
}
```

Remove the `#[allow(dead_code)]` on `mod common`. If clippy reports anything as unused, delete the unused helper rather than re-adding the allow.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`
Expected: every potter-detect test passes, including the earlier Laravel and Node suites.

- [ ] **Step 6: Commit**

```bash
git add crates/potter-detect fixtures/vite-static
git commit -m "feat(potter-detect): Static provider"
```

---

### Task 14: `potter-config`: the `potter.toml` schema, loading and options

**Files:**
- Create: `crates/potter-config/Cargo.toml`, `crates/potter-config/src/lib.rs`, `crates/potter-config/src/error.rs`, `crates/potter-config/src/file.rs`, `crates/potter-config/src/options.rs`, `crates/potter-config/tests/file.rs`

**Interfaces:**
- Consumes: `potter_detect::{DetectOptions, RuntimeOverrides, PhpOptions, NodeOptions, ProviderId, DetectError, RepoError}` and `potter_plan::{is_name, is_relative_path, PlanError}`.
- Produces:
  - `file::{ConfigFile, TargetConfig, RuntimeConfig, PhpConfig, NodeConfig, StepConfig, ProcessConfig, CommandsConfig, BaseConfig, AptConfig}`. The fields are exactly as in Step 3.
  - `file::{FILE_NAME, MAX_BYTES, DEFAULT_APT_SNAPSHOT}`, `load(&Path) -> Result<ConfigFile, ConfigError>`, `parse(&str) -> Result<ConfigFile, ConfigError>` and `targets(&ConfigFile) -> Vec<(String, TargetConfig)>`.
  - `options::Mode::{Detect(DetectOptions), Explicit}` and `options::options_for(name: &str, &TargetConfig) -> Result<Mode, ConfigError>`.
  - `ConfigError::{Io { path, source }, Parse(String), TooLarge(u64), Invalid { key, reason }, UnknownTarget { name, available }, InTarget { name, source: Box<ConfigError> }, Detect(DetectError), Plan(PlanError)}` and `ConfigError::is_infrastructure(&self) -> bool`.

- [ ] **Step 1: Create the crate and write the failing tests**

`crates/potter-config/Cargo.toml`:

```toml
[package]
name = "potter-config"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "potter.toml parsing, overrides and the plan orchestrator"

[dependencies]
indexmap = { version = "2", features = ["serde"] }
potter-detect = { path = "../potter-detect" }
potter-plan = { path = "../potter-plan" }
serde = { version = "1.0.229", features = ["derive"] }
thiserror = "2.0.21"
toml = { version = "0.9", features = ["preserve_order"] }

[dev-dependencies]
insta = "1"
serde_json = "1.0.151"
tempfile = "3.27.0"
```

`crates/potter-config/tests/file.rs`:

```rust
use potter_config::file::{load, parse, targets};
use potter_config::options::{Mode, options_for};
use potter_config::ConfigError;
use potter_detect::ProviderId;

const SPEC_EXAMPLE: &str = r#"
[target.app]
root = "."
provider = "laravel"

[target.app.runtime]
php = "8.4"
extensions = ["imagick"]
node = "22"

[target.app.steps.assets]
run = "npm run build:prod"
inputs = ["resources/**", "vite.config.ts", "package.json"]

[target.app.process.web]
cmd = ["php", "artisan", "octane:start", "--server=frankenphp", "--port=8080"]

[target.api]
root = "services/api"
provider = "node"

[apt]
build = ["libvips-dev"]
runtime = ["libvips42"]
"#;

#[test]
fn parses_the_spec_example() {
    let cfg = parse(SPEC_EXAMPLE).unwrap();
    let t = targets(&cfg);
    assert_eq!(t.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["app", "api"]);
    let app = &cfg.target["app"];
    assert_eq!(app.runtime.extensions, ["imagick"]);
    assert_eq!(app.steps["assets"].run.as_deref(), Some("npm run build:prod"));
    assert_eq!(cfg.apt.runtime, ["libvips42"]);
}

#[test]
fn step_order_is_preserved() {
    let cfg = parse("[target.x.steps.zeta]\nrun = \"a\"\n[target.x.steps.alpha]\nrun = \"b\"\n").unwrap();
    assert_eq!(cfg.target["x"].steps.keys().collect::<Vec<_>>(), ["zeta", "alpha"]);
}

#[test]
fn unknown_keys_are_errors() {
    let e = parse("[target.app]\nprovidr = \"php\"\n").unwrap_err();
    assert!(matches!(&e, ConfigError::Parse(m) if m.contains("providr")), "{e}");
}

#[test]
fn implicit_single_target() {
    let t = targets(&parse("").unwrap());
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0, "app");
}

#[test]
fn load_missing_and_oversized() {
    let d = tempfile::tempdir().unwrap();
    assert!(load(d.path()).unwrap().target.is_empty());
    std::fs::write(d.path().join("potter.toml"), vec![b'#'; 2 * 1024 * 1024]).unwrap();
    assert!(matches!(load(d.path()), Err(ConfigError::TooLarge(_))));
}

#[test]
fn options_mapping_and_validation() {
    let cfg = parse(SPEC_EXAMPLE).unwrap();
    let Mode::Detect(o) = options_for("app", &cfg.target["app"]).unwrap() else { panic!("expected detect mode") };
    assert_eq!(o.provider, Some(ProviderId::Laravel));
    assert_eq!(o.runtime.php.as_deref(), Some("8.4"));
    assert_eq!(o.key_prefix, "target.app");

    let alias = parse("[target.app.php]\nport = 9000\n").unwrap();
    let Mode::Detect(o) = options_for("app", &alias.target["app"]).unwrap() else { panic!() };
    assert_eq!(o.port, Some(9000));

    let none = parse("[target.app]\nprovider = \"none\"\n").unwrap();
    assert!(matches!(options_for("app", &none.target["app"]).unwrap(), Mode::Explicit));

    for (toml, key) in [
        ("[target.app]\nprovider = \"cobol\"\n", "target.app.provider"),
        ("[target.app]\nport = 0\n", "target.app.port"),
        ("[target.app.base]\ncatalog = [\"os-min\"]\n", "target.app.base"),
        ("[target.app]\nexport = [\"/out\"]\n", "target.app.export"),
        ("[target.app]\nroot = \"../x\"\n", "target.app.root"),
        ("[target.App]\n", "target.App"),
        ("[target.app.steps.Bad]\nrun = \"x\"\n", "target.app.steps.Bad"),
    ] {
        let cfg = parse(toml).unwrap();
        let (name, tc) = targets(&cfg).remove(0);
        match options_for(&name, &tc) {
            Err(ConfigError::Invalid { key: k, .. }) => assert_eq!(k, key, "{toml}"),
            other => panic!("{toml}: expected Invalid({key}), got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter-config`
Expected: compile errors.

- [ ] **Step 3: Implement**

`crates/potter-config/src/error.rs`:

```rust
use std::io::ErrorKind;

use potter_detect::{DetectError, RepoError};
use potter_plan::PlanError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("potter.toml: {0}")]
    Parse(String),
    #[error("potter.toml is {0} bytes, over the 1 MiB limit")]
    TooLarge(u64),
    #[error("potter.toml: {key}: {reason}")]
    Invalid { key: String, reason: String },
    #[error("no target named `{name}` (targets: {available})")]
    UnknownTarget { name: String, available: String },
    #[error("target `{name}`: {source}")]
    InTarget { name: String, source: Box<ConfigError> },
    #[error(transparent)]
    Detect(#[from] DetectError),
    #[error(transparent)]
    Plan(#[from] PlanError),
}

impl ConfigError {
    pub(crate) fn invalid(key: impl Into<String>, reason: impl Into<String>) -> Self {
        ConfigError::Invalid { key: key.into(), reason: reason.into() }
    }

    /// I/O failures other than "not found" are infrastructure problems (exit 4), not user errors.
    pub fn is_infrastructure(&self) -> bool {
        match self {
            ConfigError::Io { source, .. } => source.kind() != ErrorKind::NotFound,
            ConfigError::InTarget { source, .. } => source.is_infrastructure(),
            ConfigError::Detect(DetectError::Repo(RepoError::Io { source, .. })) => source.kind() != ErrorKind::NotFound,
            _ => false,
        }
    }
}
```

`crates/potter-config/src/file.rs`:

```rust
//! The potter.toml schema (spec §6.2). Unknown keys are errors.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::ConfigError;

pub const FILE_NAME: &str = "potter.toml";
pub const MAX_BYTES: u64 = 1024 * 1024;
/// Debian snapshot used by `Apt` steps unless `[apt] snapshot` is set.
pub const DEFAULT_APT_SNAPSHOT: &str = "20261001T000000Z";

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub target: IndexMap<String, TargetConfig>,
    #[serde(default)]
    pub apt: AptConfig,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub root: Option<String>,
    pub provider: Option<String>,
    pub port: Option<u16>,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub php: PhpConfig,
    #[serde(default)]
    pub node: NodeConfig,
    #[serde(default)]
    pub steps: IndexMap<String, StepConfig>,
    #[serde(default)]
    pub process: IndexMap<String, ProcessConfig>,
    #[serde(default)]
    pub build: CommandsConfig,
    #[serde(default)]
    pub deploy: CommandsConfig,
    /// Explicit plans only (`provider = "none"`).
    pub base: Option<BaseConfig>,
    /// Explicit plans only: absolute paths exported into the image. Defaults to `/app`.
    pub export: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub php: Option<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub node: Option<String>,
    pub bun: Option<String>,
    pub deno: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpConfig {
    pub nginx_conf: Option<String>,
    pub fpm_pool: Option<String>,
    pub octane_server: Option<String>,
    /// Alias for `[target.x] port`.
    pub port: Option<u16>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub package_manager: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepConfig {
    pub run: Option<String>,
    pub inputs: Option<Vec<String>>,
    #[serde(default)]
    pub skip: bool,
    pub after: Option<String>,
    pub network: Option<bool>,
    /// Cache mount ids, mounted at `/cache/<id>`.
    pub cache: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    pub cmd: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandsConfig {
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseConfig {
    pub catalog: Option<Vec<String>>,
    pub oci: Option<String>,
    pub run_catalog: Option<Vec<String>>,
    pub run_oci: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptConfig {
    #[serde(default)]
    pub build: Vec<String>,
    #[serde(default)]
    pub runtime: Vec<String>,
    pub snapshot: Option<String>,
}

pub fn parse(text: &str) -> Result<ConfigFile, ConfigError> {
    toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
}

pub fn load(root: &Path) -> Result<ConfigFile, ConfigError> {
    let path = root.join(FILE_NAME);
    let io = |source| ConfigError::Io { path: path.display().to_string(), source };
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(e) => return Err(io(e)),
    };
    if meta.file_type().is_symlink() {
        return Err(ConfigError::invalid(FILE_NAME, "must be a regular file, not a symlink"));
    }
    if meta.len() > MAX_BYTES {
        return Err(ConfigError::TooLarge(meta.len()));
    }
    parse(&fs::read_to_string(&path).map_err(io)?)
}

/// Declared targets, or a single implicit `app` target at the root.
pub fn targets(cfg: &ConfigFile) -> Vec<(String, TargetConfig)> {
    if cfg.target.is_empty() {
        vec![("app".to_string(), TargetConfig::default())]
    } else {
        cfg.target.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}
```

`crates/potter-config/src/options.rs`:

```rust
//! potter.toml → detection options (spec §6.2).

use potter_detect::{DetectOptions, NodeOptions, PhpOptions, ProviderId, RuntimeOverrides};
use potter_plan::{is_name, is_relative_path};

use crate::error::ConfigError;
use crate::file::TargetConfig;

#[derive(Debug)]
pub enum Mode {
    Detect(DetectOptions),
    /// `provider = "none"`: the plan comes entirely from potter.toml.
    Explicit,
}

pub fn options_for(name: &str, tc: &TargetConfig) -> Result<Mode, ConfigError> {
    let prefix = format!("target.{name}");
    if !is_name(name) {
        return Err(ConfigError::invalid(&prefix, "target names match [a-z0-9._-]{1,64}"));
    }
    if let Some(root) = &tc.root
        && !is_relative_path(root)
    {
        return Err(ConfigError::invalid(format!("{prefix}.root"), "must be a relative path inside the repository"));
    }
    for step in tc.steps.keys() {
        if !is_name(step) {
            return Err(ConfigError::invalid(format!("{prefix}.steps.{step}"), "step names match [a-z0-9._-]{1,64}"));
        }
    }
    for process in tc.process.keys() {
        if !is_name(process) {
            return Err(ConfigError::invalid(format!("{prefix}.process.{process}"), "process names match [a-z0-9._-]{1,64}"));
        }
    }
    let port = tc.port.or(tc.php.port);
    if port == Some(0) {
        let key = if tc.port.is_some() { format!("{prefix}.port") } else { format!("{prefix}.php.port") };
        return Err(ConfigError::invalid(key, "port must be 1-65535"));
    }
    let provider = match tc.provider.as_deref() {
        Some("none") => return Ok(Mode::Explicit),
        Some(p) => Some(ProviderId::parse(p).ok_or_else(|| {
            ConfigError::invalid(
                format!("{prefix}.provider"),
                format!("unknown provider `{p}` (php, laravel, node, bun, deno, static or none)"),
            )
        })?),
        None => None,
    };
    if tc.base.is_some() {
        return Err(ConfigError::invalid(format!("{prefix}.base"), "only used with provider = \"none\""));
    }
    if tc.export.is_some() {
        return Err(ConfigError::invalid(format!("{prefix}.export"), "only used with provider = \"none\""));
    }
    Ok(Mode::Detect(DetectOptions {
        provider,
        port,
        runtime: RuntimeOverrides {
            php: tc.runtime.php.clone(),
            extensions: tc.runtime.extensions.clone(),
            node: tc.runtime.node.clone(),
            bun: tc.runtime.bun.clone(),
            deno: tc.runtime.deno.clone(),
        },
        php: PhpOptions {
            octane_server: tc.php.octane_server.clone(),
            nginx_conf: tc.php.nginx_conf.clone(),
            fpm_pool: tc.php.fpm_pool.clone(),
        },
        node: NodeOptions { package_manager: tc.node.package_manager.clone() },
        key_prefix: prefix,
    }))
}
```

`crates/potter-config/src/lib.rs`, for this task:

```rust
//! potter.toml and the planning pipeline (spec §6.2).

mod error;
pub mod file;
pub mod options;

pub use error::ConfigError;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter-config && cargo clippy -p potter-config --all-targets -- -D warnings`
Expected: everything passes.

If `step_order_is_preserved` fails, toml isn't deserializing tables in document order into `IndexMap`. Make sure the `preserve_order` feature is on, and don't sort the keys.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-config Cargo.lock
git commit -m "feat(potter-config): potter.toml schema, loading and detection options"
```

---

### Task 15: Overrides, explicit plans and `plan_repo`

**Files:**
- Create: `crates/potter-config/src/merge.rs`, `crates/potter-config/src/explicit.rs`, `crates/potter-config/tests/merge.rs`
- Modify: `crates/potter-config/src/lib.rs`
- Create fixtures: `fixtures/overrides/`, `fixtures/explicit-go/`, `fixtures/ambiguous/`, `fixtures/monorepo-root/`, `fixtures/monorepo-targets/`

**Interfaces:**
- Consumes: Task 14, plus `potter_detect::{detect, RepoView}` and `potter_plan::{validate, Plan, Step, …}`.
- Produces:
  - `merge::apply(&mut Target, name, &TargetConfig, &AptConfig, &mut Vec<Decision>) -> Result<(), ConfigError>`. It applies, in order: steps, processes, build commands, deploy commands, apt.
  - `explicit::explicit_target(name, root: &str, &TargetConfig, &AptConfig) -> Result<(Target, Vec<Decision>), ConfigError>`.
  - `PlannedRepo { plan: Plan, decisions: Vec<(String, Vec<Decision>)> }` and `plan_repo(&Path, only: Option<&str>) -> Result<PlannedRepo, ConfigError>`.
- Override semantics (spec §6.2, §18):
  - On an existing Run step: `run` replaces its command with `Shell(run)`; `inputs` replaces or creates the `<step>-inputs` step just before it; `skip` removes the step and its inputs step; `network` and `cache` replace those fields.
  - A new step needs `after` and `run`, and is inserted after the named step.
  - `[process.x] cmd` replaces the process and drops its services.
  - `build.commands` appends `user-build-<n>` steps.
  - `deploy.commands` becomes `processes.deploy = ["sh", "-c", "a && b"]`.
  - `[apt] build` inserts `apt-build` at index 0, and `runtime` adds `apt-runtime` to `image.runtime_steps`.

- [ ] **Step 1: Create the fixtures**

```bash
M=/Users/alfonso/Github/Personal/nextjs-go-monorepo
mkdir -p fixtures/overrides/public fixtures/explicit-go fixtures/ambiguous/public fixtures/monorepo-root/frontend fixtures/monorepo-root/backend fixtures/monorepo-targets/frontend fixtures/monorepo-targets/backend
cp "$M/frontend/package.json" fixtures/monorepo-root/frontend/
cp "$M/backend/go.mod" fixtures/monorepo-root/backend/
cp "$M/frontend/package.json" "$M/frontend/package-lock.json" fixtures/monorepo-targets/frontend/
cp "$M/backend/go.mod" fixtures/monorepo-targets/backend/
for d in overrides ambiguous; do printf '#!/usr/bin/env php\n<?php\n' > fixtures/$d/artisan; printf '<?php\n' > fixtures/$d/public/index.php; done
printf '{ "packages": [] }\n' > fixtures/overrides/composer.lock
printf '{ "packages": [] }\n' > fixtures/ambiguous/composer.lock
printf '{ "lockfileVersion": 3, "packages": {} }\n' > fixtures/overrides/package-lock.json
printf '{ "lockfileVersion": 3, "packages": {} }\n' > fixtures/ambiguous/package-lock.json
```

`fixtures/overrides/composer.json`:

```json
{ "require": { "php": "^8.2", "laravel/framework": "^12.0" } }
```

`fixtures/overrides/package.json`:

```json
{ "private": true, "scripts": { "build": "vite build", "build:prod": "vite build --mode production" }, "devDependencies": { "vite": "^6.0.0" } }
```

`fixtures/overrides/potter.toml`:

```toml
[target.app.runtime]
php = "8.3"
extensions = ["imagick"]

[target.app.steps.assets]
run = "npm run build:prod"
inputs = ["resources/**", "vite.config.js", "package.json"]

[target.app.steps.discover]
skip = true

[target.app.steps.warm]
after = "autoload"
run = "php artisan icons:cache"

[target.app.process.web]
cmd = ["php", "artisan", "serve", "--host=0.0.0.0", "--port=8080"]

[target.app.build]
commands = ["php artisan route:list"]

[target.app.deploy]
commands = ["php artisan migrate --force", "php artisan config:cache"]

[apt]
build = ["libvips-dev"]
runtime = ["libvips42"]
```

`fixtures/ambiguous/composer.json`:

```json
{ "require": { "php": "^8.2", "laravel/framework": "^12.0" } }
```

`fixtures/ambiguous/package.json`:

```json
{ "private": true, "scripts": { "build": "next build", "start": "next start" }, "dependencies": { "next": "16.0.0" } }
```

`fixtures/explicit-go/go.mod`:

```
module example.com/hello

go 1.25
```

`fixtures/explicit-go/main.go`:

```go
package main

import "net/http"

func main() {
	http.ListenAndServe(":8080", http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.Write([]byte("ok")) }))
}
```

`fixtures/explicit-go/potter.toml`:

```toml
[target.api]
provider = "none"
port = 8080
export = ["/out/api"]

[target.api.base]
catalog = ["os-min", "go@1.25"]
run_catalog = ["os-min"]

[target.api.steps.build]
run = "GOMODCACHE=/cache/gomod GOCACHE=/cache/gobuild CGO_ENABLED=0 go build -trimpath -o /out/api ."
cache = ["gomod", "gobuild"]

[target.api.process.web]
cmd = ["/out/api"]
```

`fixtures/monorepo-targets/potter.toml`:

```toml
[target.frontend]
root = "frontend"
```

- [ ] **Step 2: Write the failing tests**

`crates/potter-config/tests/merge.rs`:

```rust
use std::path::{Path, PathBuf};

use potter_config::{ConfigError, plan_repo};
use potter_plan::{Base, Origin, Step, Target};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

fn names(t: &Target) -> Vec<&str> {
    t.build.steps.iter().map(|s| s.name()).collect()
}

fn cmd(t: &Target, name: &str) -> String {
    match t.build.steps.iter().find(|s| s.name() == name).unwrap() {
        Step::Run { command, .. } => command.to_string(),
        other => panic!("{name} is {}", other.kind()),
    }
}

#[test]
fn overrides_apply_in_order() {
    let p = plan_repo(&fixture("overrides"), None).unwrap();
    let t = &p.plan.targets[0];
    assert_eq!(
        names(t),
        ["apt-build", "deps-inputs", "deps", "node-deps-inputs", "node-deps", "source", "assets-inputs", "assets", "autoload", "warm", "user-build-1"]
    );
    assert_eq!(cmd(t, "assets"), "npm run build:prod");
    assert_eq!(cmd(t, "warm"), "php artisan icons:cache");
    assert_eq!(cmd(t, "user-build-1"), "php artisan route:list");
    match &t.build.steps[6] {
        Step::Inputs { include, .. } => assert_eq!(include, &["resources/**", "vite.config.js", "package.json"]),
        other => panic!("{other:?}"),
    }
    let Base::Catalog { layers } = &t.build.base else { panic!() };
    assert!(layers.iter().any(|l| l.to_string() == "php@8.3"));
    assert!(layers.iter().any(|l| l.to_string() == "php-ext-imagick@8.3"));
    assert_eq!(t.image.processes["web"].command.join(" "), "php artisan serve --host=0.0.0.0 --port=8080");
    assert!(!t.image.services.contains_key("web"));
    assert_eq!(t.image.processes["deploy"].command, ["sh", "-c", "php artisan migrate --force && php artisan config:cache"]);
    assert!(matches!(&t.image.runtime_steps[0], Step::Apt { name, packages, snapshot } if name == "apt-runtime" && packages == &["libvips42"] && snapshot == "20261001T000000Z"));
    let ds = &p.decisions[0].1;
    assert!(ds.iter().any(|d| d.field == "steps.assets.command" && d.origin == Origin::Config { key: "target.app.steps.assets.run".into() }));
    assert!(ds.iter().any(|d| d.field == "steps.discover" && d.value == "skipped"));
}

#[test]
fn explicit_plan() {
    let p = plan_repo(&fixture("explicit-go"), None).unwrap();
    let t = &p.plan.targets[0];
    assert_eq!(t.name, "api");
    assert_eq!(t.provider, "none");
    assert_eq!(names(t), ["source", "build"]);
    let Base::Catalog { layers } = &t.image.base else { panic!() };
    assert_eq!(layers.iter().map(|l| l.to_string()).collect::<Vec<_>>(), ["os-min"]);
    assert_eq!(t.image.export[0].path, "/out/api");
    assert_eq!(t.image.config.port, Some(8080));
    match &t.build.steps[1] {
        Step::Run { cache_mounts, .. } => {
            assert_eq!(cache_mounts.iter().map(|m| m.target.as_str()).collect::<Vec<_>>(), ["/cache/gomod", "/cache/gobuild"]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ambiguous_root_names_the_target() {
    let msg = plan_repo(&fixture("ambiguous"), None).unwrap_err().to_string();
    assert!(msg.starts_with("target `app`: laravel, node all claim the build root"), "{msg}");
}

#[test]
fn monorepo_root_hint_and_targets() {
    let msg = plan_repo(&fixture("monorepo-root"), None).unwrap_err().to_string();
    assert!(msg.contains("frontend/") && msg.contains("backend/"), "{msg}");
    let p = plan_repo(&fixture("monorepo-targets"), None).unwrap();
    let t = &p.plan.targets[0];
    assert_eq!((t.name.as_str(), t.root.as_str(), t.provider.as_str()), ("frontend", "frontend", "node"));
}

#[test]
fn target_filter() {
    assert!(matches!(plan_repo(&fixture("explicit-go"), Some("nope")), Err(ConfigError::UnknownTarget { .. })));
    assert_eq!(plan_repo(&fixture("explicit-go"), Some("api")).unwrap().plan.targets.len(), 1);
}

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, c) in files {
        let path = d.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, c).unwrap();
    }
    d
}

#[test]
fn override_errors() {
    let php = [("composer.json", r#"{"require":{}}"#), ("composer.lock", r#"{"packages":[]}"#)];
    let cases = [
        ("[target.app.steps.nope]\nrun = \"x\"\n", "target.app.steps.nope"),
        ("[target.app.steps.new]\nafter = \"missing\"\nrun = \"x\"\n", "target.app.steps.new.after"),
        ("[target.app.steps.new]\nafter = \"deps\"\n", "target.app.steps.new"),
        ("[target.app.steps.deps]\nafter = \"source\"\n", "target.app.steps.deps.after"),
        ("[target.app.process.web]\ncmd = []\n", "target.app.process.web.cmd"),
        ("[target.app.steps.deps]\ncache = [\"Bad\"]\n", "target.app.steps.deps.cache"),
    ];
    for (toml, key) in cases {
        let d = temp(&[php[0], php[1], ("potter.toml", toml)]);
        match plan_repo(d.path(), None) {
            Err(ConfigError::InTarget { source, .. }) => match *source {
                ConfigError::Invalid { key: k, .. } => assert_eq!(k, key, "{toml}"),
                other => panic!("{toml}: {other}"),
            },
            other => panic!("{toml}: {other:?}"),
        }
    }
}

#[test]
fn invalid_plans_from_config_are_caught() {
    let d = temp(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("potter.toml", "[apt]\nbuild = [\"Not A Package\"]\n"),
    ]);
    assert!(matches!(plan_repo(d.path(), None), Err(ConfigError::Plan(_))));
}

#[test]
fn explicit_plan_needs_a_base_and_a_process() {
    let d = temp(&[("potter.toml", "[target.x]\nprovider = \"none\"\n[target.x.process.web]\ncmd = [\"/bin/x\"]\n")]);
    assert!(plan_repo(d.path(), None).unwrap_err().to_string().contains("target.x.base"));
    let d = temp(&[("potter.toml", "[target.x]\nprovider = \"none\"\n[target.x.base]\noci = \"debian:13\"\n")]);
    assert!(plan_repo(d.path(), None).unwrap_err().to_string().contains("target.x.process"));
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p potter-config --test merge`
Expected: compile errors (`plan_repo` is missing).

- [ ] **Step 4: Implement**

`crates/potter-config/src/merge.rs`:

```rust
//! Applying potter.toml overrides onto detector output (spec §6.2, §18).

use potter_plan::{CacheMount, Command, Decision, Process, Sharing, Step, Target, is_name};

use crate::error::ConfigError;
use crate::file::{AptConfig, DEFAULT_APT_SNAPSHOT, StepConfig, TargetConfig};

pub(crate) fn run_step(name: &str, command: Command, sc: &StepConfig, key: &str) -> Result<Step, ConfigError> {
    Ok(Step::Run {
        name: name.into(),
        command,
        env: Default::default(),
        workdir: "/app".into(),
        user: "root".into(),
        network: sc.network.unwrap_or(true),
        cache_mounts: cache_mounts(sc.cache.as_deref().unwrap_or(&[]), key)?,
        secrets: vec![],
        timeout_secs: None,
    })
}

pub(crate) fn inputs_step(name: &str, include: &[String]) -> Step {
    Step::Inputs { name: name.into(), include: include.to_vec(), exclude: vec![], dest: "/app".into(), owner: "0:0".into() }
}

fn cache_mounts(ids: &[String], key: &str) -> Result<Vec<CacheMount>, ConfigError> {
    ids.iter()
        .map(|id| {
            if is_name(id) {
                Ok(CacheMount { id: id.clone(), target: format!("/cache/{id}"), sharing: Sharing::Exclusive })
            } else {
                Err(ConfigError::invalid(format!("{key}.cache"), format!("cache id `{id}` must match [a-z0-9._-]{{1,64}}")))
            }
        })
        .collect()
}

fn position(steps: &[Step], name: &str) -> Option<usize> {
    steps.iter().position(|s| s.name() == name)
}

pub fn apply(
    target: &mut Target,
    name: &str,
    tc: &TargetConfig,
    apt: &AptConfig,
    decisions: &mut Vec<Decision>,
) -> Result<(), ConfigError> {
    let prefix = format!("target.{name}");
    apply_steps(target, &prefix, tc, decisions)?;
    apply_processes(target, &prefix, tc, decisions)?;
    apply_commands(target, &prefix, tc, decisions);
    apply_apt(target, apt, decisions);
    Ok(())
}

fn apply_steps(target: &mut Target, prefix: &str, tc: &TargetConfig, decisions: &mut Vec<Decision>) -> Result<(), ConfigError> {
    for (step_name, sc) in &tc.steps {
        let key = format!("{prefix}.steps.{step_name}");
        let steps = &mut target.build.steps;
        let existing = steps.iter().position(|s| s.name() == step_name && matches!(s, Step::Run { .. }));
        let inputs_name = format!("{step_name}-inputs");
        match existing {
            None => {
                let Some(after) = &sc.after else {
                    let available: Vec<&str> = steps.iter().filter(|s| matches!(s, Step::Run { .. })).map(|s| s.name()).collect();
                    return Err(ConfigError::invalid(
                        key,
                        format!("no step named `{step_name}` (steps: {}); add `after = \"…\"` to insert a new one", available.join(", ")),
                    ));
                };
                let Some(run) = &sc.run else { return Err(ConfigError::invalid(key, "a new step needs `run`")) };
                if sc.skip {
                    return Err(ConfigError::invalid(format!("{key}.skip"), "`skip` only applies to existing steps"));
                }
                let at = position(steps, after)
                    .ok_or_else(|| ConfigError::invalid(format!("{key}.after"), format!("no step named `{after}`")))?;
                let mut new = Vec::new();
                if let Some(inc) = &sc.inputs {
                    new.push(inputs_step(&inputs_name, inc));
                }
                new.push(run_step(step_name, Command::Shell(run.clone()), sc, &key)?);
                steps.splice(at + 1..at + 1, new);
                decisions.push(Decision::config(&format!("steps.{step_name}"), run.as_str(), key));
            }
            Some(i) => {
                if sc.after.is_some() {
                    return Err(ConfigError::invalid(format!("{key}.after"), "`after` only applies to new steps"));
                }
                if sc.skip {
                    steps.remove(i);
                    if let Some(j) = position(steps, &inputs_name) {
                        steps.remove(j);
                    }
                    decisions.push(Decision::config(&format!("steps.{step_name}"), "skipped", format!("{key}.skip")));
                    continue;
                }
                let mounts = match &sc.cache {
                    Some(ids) => Some(cache_mounts(ids, &key)?),
                    None => None,
                };
                if let Step::Run { command, network, cache_mounts, .. } = &mut steps[i] {
                    if let Some(run) = &sc.run {
                        *command = Command::Shell(run.clone());
                        decisions.push(Decision::config(&format!("steps.{step_name}.command"), run.as_str(), format!("{key}.run")));
                    }
                    if let Some(n) = sc.network {
                        *network = n;
                        decisions.push(Decision::config(&format!("steps.{step_name}.network"), n.to_string(), format!("{key}.network")));
                    }
                    if let Some(m) = mounts {
                        *cache_mounts = m;
                        decisions.push(Decision::config(&format!("steps.{step_name}.cache"), sc.cache.as_ref().map(|c| c.join(", ")).unwrap_or_default(), format!("{key}.cache")));
                    }
                }
                if let Some(inc) = &sc.inputs {
                    match position(steps, &inputs_name) {
                        Some(j) => {
                            if let Step::Inputs { include, .. } = &mut steps[j] {
                                *include = inc.clone();
                            }
                        }
                        None => steps.insert(i, inputs_step(&inputs_name, inc)),
                    }
                    decisions.push(Decision::config(&format!("steps.{step_name}.inputs"), inc.join(" "), format!("{key}.inputs")));
                }
            }
        }
    }
    Ok(())
}

fn apply_processes(target: &mut Target, prefix: &str, tc: &TargetConfig, decisions: &mut Vec<Decision>) -> Result<(), ConfigError> {
    for (pname, pc) in &tc.process {
        let key = format!("{prefix}.process.{pname}.cmd");
        if pc.cmd.first().is_none_or(|c| c.trim().is_empty()) {
            return Err(ConfigError::invalid(key, "cmd must not be empty"));
        }
        target.image.processes.insert(pname.clone(), Process { command: pc.cmd.clone(), workdir: None, env: Default::default() });
        target.image.services.remove(pname);
        decisions.push(Decision::config(&format!("process.{pname}"), pc.cmd.join(" "), key));
    }
    Ok(())
}

pub(crate) fn apply_commands(target: &mut Target, prefix: &str, tc: &TargetConfig, decisions: &mut Vec<Decision>) {
    let empty = StepConfig::default();
    for (i, c) in tc.build.commands.iter().enumerate() {
        let name = format!("user-build-{}", i + 1);
        let step = run_step(&name, Command::Shell(c.clone()), &empty, "").expect("no cache ids to validate");
        target.build.steps.push(step);
        decisions.push(Decision::config(&format!("steps.{name}"), c.as_str(), format!("{prefix}.build.commands")));
    }
    if !tc.deploy.commands.is_empty() {
        let script = tc.deploy.commands.join(" && ");
        target.image.processes.insert(
            "deploy".into(),
            Process { command: vec!["sh".into(), "-c".into(), script.clone()], workdir: None, env: Default::default() },
        );
        decisions.push(Decision::config("process.deploy", script, format!("{prefix}.deploy.commands")));
    }
}

pub(crate) fn apply_apt(target: &mut Target, apt: &AptConfig, decisions: &mut Vec<Decision>) {
    let snapshot = apt.snapshot.clone().unwrap_or_else(|| DEFAULT_APT_SNAPSHOT.to_string());
    if !apt.build.is_empty() {
        target.build.steps.insert(0, Step::Apt { name: "apt-build".into(), packages: apt.build.clone(), snapshot: snapshot.clone() });
        decisions.push(Decision::config("steps.apt-build", apt.build.join(" "), "apt.build"));
    }
    if !apt.runtime.is_empty() {
        target.image.runtime_steps.push(Step::Apt { name: "apt-runtime".into(), packages: apt.runtime.clone(), snapshot });
        decisions.push(Decision::config("runtime_steps.apt-runtime", apt.runtime.join(" "), "apt.runtime"));
    }
}
```

`crates/potter-config/src/explicit.rs`:

```rust
//! `provider = "none"`: a plan written entirely in potter.toml (spec §6.2). The chain starts
//! with a `source` inputs step, then the declared steps in file order.

use std::collections::BTreeMap;

use potter_plan::{Base, CatalogRef, Chain, Command, Decision, Export, Image, ImageConfig, Process, Target};

use crate::error::ConfigError;
use crate::file::{AptConfig, TargetConfig};
use crate::merge::{apply_apt, apply_commands, inputs_step, run_step};

fn parse_ref(s: &str) -> CatalogRef {
    match s.split_once('@') {
        Some((name, version)) => CatalogRef::new(name, Some(version)),
        None => CatalogRef::new(s, None),
    }
}

fn base_from(catalog: Option<&Vec<String>>, oci: Option<&String>, key: &str) -> Result<Option<Base>, ConfigError> {
    match (catalog, oci) {
        (Some(c), None) => Ok(Some(Base::Catalog { layers: c.iter().map(|s| parse_ref(s)).collect() })),
        (None, Some(o)) => Ok(Some(Base::Oci { reference: o.clone(), digest: None })),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(ConfigError::invalid(key, "set either a catalog list or an oci reference, not both")),
    }
}

pub fn explicit_target(
    name: &str,
    root: &str,
    tc: &TargetConfig,
    apt: &AptConfig,
) -> Result<(Target, Vec<Decision>), ConfigError> {
    let prefix = format!("target.{name}");
    let base_key = format!("{prefix}.base");
    let base = tc
        .base
        .as_ref()
        .ok_or_else(|| ConfigError::invalid(&base_key, "provider = \"none\" needs a base with `catalog` or `oci`"))?;
    let build_base = base_from(base.catalog.as_ref(), base.oci.as_ref(), &base_key)?
        .ok_or_else(|| ConfigError::invalid(&base_key, "set `catalog` or `oci`"))?;
    let run_base = base_from(base.run_catalog.as_ref(), base.run_oci.as_ref(), &base_key)?.unwrap_or_else(|| build_base.clone());

    let mut decisions = vec![Decision::config("provider", "none", format!("{prefix}.provider"))];
    let mut steps = vec![inputs_step("source", &["**".to_string()])];
    for (step_name, sc) in &tc.steps {
        let key = format!("{prefix}.steps.{step_name}");
        let run = sc.run.as_ref().ok_or_else(|| ConfigError::invalid(&key, "explicit steps need `run`"))?;
        if sc.skip || sc.after.is_some() {
            return Err(ConfigError::invalid(&key, "`skip` and `after` don't apply to explicit plans; steps run in file order"));
        }
        if let Some(inc) = &sc.inputs {
            steps.push(inputs_step(&format!("{step_name}-inputs"), inc));
        }
        steps.push(run_step(step_name, Command::Shell(run.clone()), sc, &key)?);
        decisions.push(Decision::config(&format!("steps.{step_name}"), run.as_str(), key));
    }
    if tc.process.is_empty() {
        return Err(ConfigError::invalid(format!("{prefix}.process"), "explicit plans need at least one [process.*] entry"));
    }
    let mut processes = BTreeMap::new();
    for (pname, pc) in &tc.process {
        if pc.cmd.first().is_none_or(|c| c.trim().is_empty()) {
            return Err(ConfigError::invalid(format!("{prefix}.process.{pname}.cmd"), "cmd must not be empty"));
        }
        processes.insert(pname.clone(), Process { command: pc.cmd.clone(), workdir: None, env: Default::default() });
    }
    let export = tc.export.clone().unwrap_or_else(|| vec!["/app".into()]);
    let mut config = ImageConfig::new("/app", "root");
    config.port = tc.port;
    let mut target = Target {
        name: name.into(),
        root: root.into(),
        platform: None,
        provider: "none".into(),
        build: Chain { base: build_base, steps },
        image: Image {
            base: run_base,
            runtime_steps: vec![],
            export: export.iter().map(|p| Export { path: p.clone(), dest: p.clone(), exclude: vec![] }).collect(),
            files: vec![],
            config,
            processes,
            services: BTreeMap::new(),
        },
    };
    apply_commands(&mut target, &prefix, tc, &mut decisions);
    apply_apt(&mut target, apt, &mut decisions);
    Ok((target, decisions))
}
```

Replace `crates/potter-config/src/lib.rs`:

```rust
//! potter.toml and the planning pipeline (spec §6.2): load config, detect or build each target,
//! apply overrides, validate.

mod error;
pub mod explicit;
pub mod file;
pub mod merge;
pub mod options;

use std::path::Path;

use potter_detect::RepoView;
use potter_plan::{Decision, Plan, Target, is_relative_path};

pub use error::ConfigError;
use file::{AptConfig, TargetConfig};
use options::Mode;

#[derive(Debug)]
pub struct PlannedRepo {
    pub plan: Plan,
    /// Per target, in plan order.
    pub decisions: Vec<(String, Vec<Decision>)>,
}

pub fn plan_repo(root: &Path, only: Option<&str>) -> Result<PlannedRepo, ConfigError> {
    let cfg = file::load(root)?;
    let all = file::targets(&cfg);
    if let Some(o) = only
        && !all.iter().any(|(n, _)| n == o)
    {
        return Err(ConfigError::UnknownTarget {
            name: o.to_string(),
            available: all.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "),
        });
    }
    let mut targets = Vec::new();
    let mut decisions = Vec::new();
    for (name, tc) in all {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let (t, d) = plan_target(root, &name, &tc, &cfg.apt)
            .map_err(|e| ConfigError::InTarget { name: name.clone(), source: Box::new(e) })?;
        targets.push(t);
        decisions.push((name, d));
    }
    let plan = Plan::new(targets);
    potter_plan::validate(&plan)?;
    Ok(PlannedRepo { plan, decisions })
}

fn plan_target(root: &Path, name: &str, tc: &TargetConfig, apt: &AptConfig) -> Result<(Target, Vec<Decision>), ConfigError> {
    let rel = tc.root.clone().unwrap_or_else(|| ".".into());
    if !is_relative_path(&rel) {
        return Err(ConfigError::invalid(format!("target.{name}.root"), "must be a relative path inside the repository"));
    }
    let opts = match options::options_for(name, tc)? {
        Mode::Explicit => return explicit::explicit_target(name, &rel, tc, apt),
        Mode::Detect(o) => o,
    };
    let dir = if rel == "." { root.to_path_buf() } else { root.join(&rel) };
    let repo = RepoView::open(&dir).map_err(potter_detect::DetectError::from)?;
    let detection = potter_detect::detect(&repo, &opts, name)?;
    let mut target = detection.target;
    target.root = rel;
    let mut decisions = detection.decisions;
    merge::apply(&mut target, name, tc, apt, &mut decisions)?;
    Ok((target, decisions))
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p potter-config && cargo clippy -p potter-config --all-targets -- -D warnings`
Expected: everything passes.

In `overrides_apply_in_order`, `assets-inputs` lands immediately before `assets`, which is index 6 once `apt-build` is at 0. If the order differs, fix `apply_steps`, not the test.

- [ ] **Step 6: Commit**

```bash
git add crates/potter-config fixtures/overrides fixtures/explicit-go fixtures/ambiguous fixtures/monorepo-root fixtures/monorepo-targets
git commit -m "feat(potter-config): overrides, explicit plans and plan_repo"
```

---

### Task 16: The `potter` CLI (`plan`, `plan --explain`, `plan --json`, `detect`)

**Files:**
- Create: `crates/potter/Cargo.toml`, `crates/potter/src/main.rs`, `crates/potter/src/render.rs`, `crates/potter/tests/cli.rs`

**Interfaces:**
- Consumes:
  - `potter_config::{plan_repo, PlannedRepo, ConfigError}`;
  - `potter_detect::{claims, select, RepoView, DetectOptions, Claim, ClaimKind, ProviderId, RepoError}`;
  - `potter_plan::{canonical_json, sanitize, Base, Step}`.
- Produces: the `potter` binary.
- Exit codes: 0 on success; 2 for user and plan errors, including a missing path; 4 for other I/O errors.

- [ ] **Step 1: Write the failing tests**

`crates/potter/Cargo.toml`:

```toml
[package]
name = "potter"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Build kiln microVM images from source"

[dependencies]
clap = { version = "4.6.7", features = ["derive"] }
potter-config = { path = "../potter-config" }
potter-detect = { path = "../potter-detect" }
potter-plan = { path = "../potter-plan" }

[dev-dependencies]
assert_cmd = "2.2.2"
predicates = "3.1.4"
serde_json = "1.0.151"
tempfile = "3.27.0"
```

`crates/potter/tests/cli.rs`:

```rust
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

fn potter() -> Command {
    Command::cargo_bin("potter").unwrap()
}

#[test]
fn plan_human_output() {
    potter()
        .args(["plan"])
        .arg(fixture("laravel"))
        .assert()
        .success()
        .stdout(predicate::str::contains("target app (laravel, root .)"))
        .stdout(predicate::str::contains("php artisan queue:work"))
        .stdout(predicate::str::contains("(+ php-fpm)"));
}

#[test]
fn plan_json_is_canonical() {
    let out = potter().args(["plan", "--json"]).arg(fixture("nestjs")).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["schema"], "potter.plan/v1");
    assert!(!text.trim_end().contains('\n'), "canonical JSON is a single line");
}

#[test]
fn plan_explain() {
    potter()
        .args(["plan", "--explain"])
        .arg(fixture("php-composer"))
        .assert()
        .success()
        .stdout(predicate::str::contains("decisions for target app:"))
        .stdout(predicate::str::contains("composer.json require.php = ^8.3"));
}

#[test]
fn json_and_explain_conflict() {
    potter().args(["plan", "--json", "--explain"]).arg(fixture("nestjs")).assert().code(2);
}

#[test]
fn ambiguous_exits_2() {
    potter()
        .args(["plan"])
        .arg(fixture("ambiguous"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("all claim the build root"));
}

#[test]
fn missing_path_exits_2() {
    potter().args(["plan", "/definitely/not/here"]).assert().code(2);
}

#[test]
fn unknown_target_exits_2() {
    potter()
        .args(["plan", "--target", "nope"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no target named `nope`"));
}

#[test]
fn detect_lists_claims() {
    potter()
        .args(["detect"])
        .arg(fixture("laravel"))
        .assert()
        .success()
        .stdout(predicate::str::contains("laravel"))
        .stdout(predicate::str::contains("chosen: laravel"));
}

#[test]
fn detect_monorepo_root_hints() {
    potter()
        .args(["detect"])
        .arg(fixture("monorepo-root"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("[target.backend]"));
}

#[test]
fn repo_text_in_errors_is_sanitised() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("composer.json"), "{\"require\":{\"php\":\"\\u001b[31mx\"}}").unwrap();
    std::fs::write(d.path().join("composer.lock"), "{\"packages\":[]}").unwrap();
    let out = potter().arg("plan").arg(d.path()).assert().code(2).get_output().stderr.clone();
    let err = String::from_utf8(out).unwrap();
    assert!(!err.contains('\u{1b}'), "{err:?}");
    assert!(err.contains("\\u{1b}"), "{err:?}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p potter`
Expected: compile errors (no `src/main.rs`).

- [ ] **Step 3: Implement**

`crates/potter/src/render.rs`:

```rust
//! Human-readable output. Callers sanitise the whole string before printing.

use std::fmt::Write;

use potter_config::PlannedRepo;
use potter_detect::{Claim, ClaimKind, ProviderId};
use potter_plan::{Base, Step};

fn base(b: &Base) -> String {
    match b {
        Base::Catalog { layers } => layers.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", "),
        Base::Oci { reference, digest: Some(d) } => format!("{reference}@{d}"),
        Base::Oci { reference, digest: None } => reference.clone(),
        Base::Stage { name } => format!("stage {name}"),
    }
}

fn summary(step: &Step) -> String {
    match step {
        Step::Inputs { include, .. } => include.join(" "),
        Step::Run { command, .. } => command.to_string(),
        Step::CopyFrom { paths, .. } => paths.join(" "),
        Step::Apt { packages, .. } => packages.join(" "),
        Step::Meta { .. } => String::new(),
    }
}

pub fn plan(p: &PlannedRepo) -> String {
    let mut s = String::new();
    for t in &p.plan.targets {
        writeln!(s, "target {} ({}, root {})", t.name, t.provider, t.root).unwrap();
        writeln!(s, "  build base: {}", base(&t.build.base)).unwrap();
        writeln!(s, "  steps:").unwrap();
        for st in &t.build.steps {
            writeln!(s, "    {:<8} {:<18} {}", st.kind(), st.name(), summary(st)).unwrap();
        }
        writeln!(s, "  image base: {}", base(&t.image.base)).unwrap();
        for st in &t.image.runtime_steps {
            writeln!(s, "  runtime: {} {}", st.name(), summary(st)).unwrap();
        }
        for e in &t.image.export {
            let exclude = if e.exclude.is_empty() { String::new() } else { format!(" (exclude {})", e.exclude.join(", ")) };
            writeln!(s, "  export: {} -> {}{exclude}", e.path, e.dest).unwrap();
        }
        for f in &t.image.files {
            writeln!(s, "  file: {}", f.path).unwrap();
        }
        if let Some(port) = t.image.config.port {
            writeln!(s, "  port: {port}").unwrap();
        }
        writeln!(s, "  processes:").unwrap();
        for (name, pr) in &t.image.processes {
            let services = t
                .image
                .services
                .get(name)
                .map(|v| format!("  (+ {})", v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ")))
                .unwrap_or_default();
            writeln!(s, "    {:<10} {}{services}", name, pr.command.join(" ")).unwrap();
        }
    }
    s
}

pub fn explain(p: &PlannedRepo) -> String {
    let mut s = String::new();
    for (target, decisions) in &p.decisions {
        writeln!(s, "decisions for target {target}:").unwrap();
        for d in decisions {
            writeln!(s, "  {:<28} {:<24} {}", d.field, d.value, d.origin).unwrap();
        }
    }
    s
}

fn kind(k: ClaimKind) -> &'static str {
    match k {
        ClaimKind::Backend => "backend",
        ClaimKind::JsServer => "js-server",
        ClaimKind::JsAssets => "js-assets",
    }
}

pub fn detect(claims: &[Claim], chosen: Option<ProviderId>) -> String {
    let mut s = String::new();
    writeln!(s, "{:<10} {:<10} {:<8} evidence", "provider", "kind", "refines").unwrap();
    for c in claims {
        let refines = c.refines.map(|r| r.as_str()).unwrap_or("-");
        writeln!(s, "{:<10} {:<10} {:<8} {}", c.provider.as_str(), kind(c.kind), refines, c.evidence.join("; ")).unwrap();
    }
    if let Some(id) = chosen {
        writeln!(s, "chosen: {id}").unwrap();
    }
    s
}
```

`crates/potter/src/main.rs`:

```rust
//! potter: build kiln microVM images from source. P1a ships planning only.

mod render;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use potter_detect::{DetectOptions, RepoError, RepoView};
use potter_plan::sanitize;

#[derive(Parser)]
#[command(name = "potter", version, about = "Build kiln microVM images from source")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the build plan for a repository
    Plan {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only plan this target
        #[arg(long)]
        target: Option<String>,
        /// Print the canonical potter.plan/v1 JSON
        #[arg(long, conflicts_with = "explain")]
        json: bool,
        /// Show where each decision came from
        #[arg(long)]
        explain: bool,
    },
    /// Show which providers claim a directory
    Detect {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn fail(message: &str, code: u8) -> ExitCode {
    eprintln!("error: {}", sanitize(message));
    ExitCode::from(code)
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::Plan { path, target, json, explain } => plan(&path, target.as_deref(), json, explain),
        Cmd::Detect { path } => detect(&path),
    }
}

fn plan(path: &Path, target: Option<&str>, json: bool, explain: bool) -> ExitCode {
    let planned = match potter_config::plan_repo(path, target) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string(), if e.is_infrastructure() { 4 } else { 2 }),
    };
    if json {
        return match potter_plan::canonical_json(&planned.plan) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e.to_string(), 2),
        };
    }
    print!("{}", sanitize(&render::plan(&planned)));
    if explain {
        print!("\n{}", sanitize(&render::explain(&planned)));
    }
    ExitCode::SUCCESS
}

fn detect(path: &Path) -> ExitCode {
    let repo = match RepoView::open(path) {
        Ok(r) => r,
        Err(e) => {
            let infra = matches!(&e, RepoError::Io { source, .. } if source.kind() != ErrorKind::NotFound);
            return fail(&e.to_string(), if infra { 4 } else { 2 });
        }
    };
    let claims = match potter_detect::claims(&repo, &DetectOptions::default()) {
        Ok(c) => c,
        Err(e) => return fail(&e.to_string(), 2),
    };
    let chosen = potter_detect::select(&claims, &repo);
    print!("{}", sanitize(&render::detect(&claims, chosen.as_ref().ok().copied())));
    match chosen {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(&e.to_string(), 2),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p potter && cargo clippy --all-targets -- -D warnings`
Expected: every CLI test passes.

Then try it by hand: `cargo run -q -p potter -- plan --explain fixtures/laravel`. Read the output: the steps and processes should match the Laravel test, and every decision should have an origin.

- [ ] **Step 5: Commit**

```bash
git add crates/potter Cargo.lock
git commit -m "feat(potter): plan, plan --explain, plan --json and detect commands"
```

---

### Task 17: Fixture snapshots, docs and the final check

**Files:**
- Create: `crates/potter-config/tests/fixtures.rs` and `crates/potter-config/tests/snapshots/*.snap` (generated)
- Create: `docs/plan-format.md`, `README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: a snapshot for every fixture (plan JSON plus decisions, or the error), and the user-facing docs.

- [ ] **Step 1: Write the snapshot test**

`crates/potter-config/tests/fixtures.rs`:

```rust
//! One snapshot per fixture: the canonical plan (pretty-printed) and every decision, or the error.
//! Review changes with `cargo insta review`, or regenerate with `INSTA_UPDATE=always`.

use std::fmt::Write;
use std::path::Path;

use potter_config::plan_repo;

fn render(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name);
    match plan_repo(&path, None) {
        Ok(p) => {
            let mut s = serde_json::to_string_pretty(&p.plan).unwrap();
            s.push_str("\n\n# decisions\n");
            for (target, ds) in &p.decisions {
                for d in ds {
                    writeln!(s, "{target} {} = {} [{}]", d.field, d.value, d.origin).unwrap();
                }
            }
            s
        }
        Err(e) => format!("error: {e}\n"),
    }
}

macro_rules! fixtures {
    ($($name:ident => $dir:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                insta::assert_snapshot!(render($dir));
            }
        )*
    };
}

fixtures! {
    php_composer => "php-composer",
    drupal => "drupal",
    laravel => "laravel",
    laravel_octane => "laravel-octane",
    laravel_horizon_pnpm => "laravel-horizon-pnpm",
    nestjs => "nestjs",
    next_standalone => "next-standalone",
    nuxt => "nuxt",
    hono_node => "hono-node",
    vue_node => "vue-node",
    bun => "bun",
    deno_serve => "deno-serve",
    deno_fresh => "deno-fresh",
    vite_static => "vite-static",
    overrides => "overrides",
    explicit_go => "explicit-go",
    ambiguous => "ambiguous",
    monorepo_root => "monorepo-root",
    monorepo_targets => "monorepo-targets",
}
```

- [ ] **Step 2: Generate the snapshots, then review every one**

Run: `INSTA_UPDATE=always cargo test -p potter-config --test fixtures`
Expected: 19 `.snap` files are created under `crates/potter-config/tests/snapshots/`.

Read each snapshot and check it against the provider table in spec §6.1:
- the provider;
- the runtime version and its origin;
- the step order;
- every command;
- the run base, with no build toolchain in it;
- the exports and processes.

Any snapshot that contradicts the spec is a bug. Fix the code, then regenerate. Never edit a snapshot to match a bug.

Run again without `INSTA_UPDATE`: `cargo test -p potter-config --test fixtures`. Expected: it passes.

- [ ] **Step 3: Write `docs/plan-format.md`**

```markdown
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
| `[target.<name>]` `root`, `provider`, `port` | Build root (relative); force a provider (`php`, `laravel`, `node`, `bun`, `deno`, `static`, or `none` for an explicit plan); listen port. |
| `[target.<name>.runtime]` `php`, `node`, `bun`, `deno`, `extensions` | Version lines and extra PHP extensions. Always wins over detection. |
| `[target.<name>.php]` `nginx_conf`, `fpm_pool`, `octane_server`, `port` | Replace the generated configs (repo-relative files); pick the Octane server; port alias. |
| `[target.<name>.node]` `package_manager` | `npm`, `pnpm`, `yarn` or `bun`; its lockfile must exist. |
| `[target.<name>.steps.<step>]` `run`, `inputs`, `skip`, `after`, `network`, `cache` | Override a detected step; with `after`, insert a new one. |
| `[target.<name>.process.<process>]` `cmd` | Replace a process (drops its services). |
| `[target.<name>.build]` `commands` | Appended build steps `user-build-<n>`. |
| `[target.<name>.deploy]` `commands` | A `deploy` process the platform runs once before rollout. |
| `[target.<name>.base]` `catalog`/`oci`, `run_catalog`/`run_oci`; `[target.<name>]` `export` | Explicit plans only. |
| `[apt]` `build`, `runtime`, `snapshot` | Debian packages from a pinned snapshot.debian.org date. |

`potter plan --explain` prints where every decision came from: what a detector found (with
its evidence), a potter.toml key, or a default.

## Exit codes

0 success · 2 usage, config, plan or detection error (including a missing path) · 4 other I/O errors.
```

`README.md`:

```markdown
# potter

Build [kiln](../kiln) microVM images from source. potter detects PHP/Laravel, Node, Bun, Deno and
static-site projects, or reads an explicit `potter.toml`, and produces a canonical build plan.
Later milestones execute plans in microVMs or rootless namespaces with input-precise caching.

    potter plan [PATH] [--target T] [--json | --explain]
    potter detect [PATH]

Detection only parses files; it never runs your code. See `docs/plan-format.md` and the design
spec in `docs/superpowers/specs/`.
```

- [ ] **Step 4: Run the full CI command set**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: every crate passes with no warnings.

Run: `git status --short`
Expected: the only new files are the snapshots and docs. Nothing has been written into `fixtures/`: there is no `POTTER_EXECUTED` marker and no `node_modules`.

- [ ] **Step 5: Commit**

```bash
git add crates/potter-config/tests docs/plan-format.md README.md
git commit -m "test: fixture snapshots; docs: plan format and README"
```
