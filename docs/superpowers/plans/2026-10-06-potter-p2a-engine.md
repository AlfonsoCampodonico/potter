# potter P2a: the build engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `potter build --executor ns` turns a plan whose bases are OCI images into a kiln image in `$KILN_HOME`. It runs `Run` steps rootless in vmkit's sandbox and caches every step by its input-precise key.

**Architecture:** Two new crates.
- `potter-cache` holds cache keys (spec §8.1), the local index (§8.2), tenants and `$POTTER_HOME`.
- `potter-exec` holds:
  - inputs layers (§9.2);
  - OCI bases through kiln-registry/kiln-oci;
  - layer unpacking into overlay lower dirs;
  - `NsExecutor` on `vmkit::sandbox` (§9.4);
  - sealing an overlay upper into an OCI tar diff;
  - the chain driver;
  - export by merging layer tars (§9.6);
  - image assembly through kiln-image's `convert_resolved`.

Unpacking and sealing touch files owned by subordinate ids, so they run inside the sandbox as hidden `potter __unpack` / `potter __seal` subcommands (`Root::Host`, subordinate ids). Everything that does not need Linux namespaces builds and is tested on macOS.

**Tech Stack:**
- Rust 2024;
- kiln crates (`kiln-store`, `kiln-registry`, `kiln-oci`, `kiln-image`; `kiln-erofs` for tests), git rev `01e7a9fe288160cc2eef9dd1a4a205022b8aef77`;
- vmkit, git rev of branch `sandbox-exec` (see the ledger: pinned when Task 6 starts);
- `tar`, `flate2`, `zstd`, `globset`, `rustix`, `sha2`, `serde_json`, `tempfile`.

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md`, rev 1.6:
- §4 (store layout);
- §5 (IR);
- §8.1–8.2 and §8.5 (keys, local cache, cache mounts);
- §9.1–9.2 and §9.4 (executor, inputs layers, NsExecutor);
- §9.6 (assembly);
- §11–12 (CLI, failures);
- §14.4 (P5, P6, P8 here);
- §15 (P2).

P2 is split here:
- **P2a**, this plan: OCI bases.
- **P2b**, the next plan: the stopgap catalog→upstream image mapping that lets detected targets build, and `services`.

## Global Constraints

- **Workspace conventions:** Rust 2024; workspace `resolver = "3"`; per-crate dependency versions; `thiserror` errors; rustfmt `max_width = 120`; `cargo clippy --all-targets -- -D warnings`.
- **Unsafe:** `#![forbid(unsafe_code)]` in every potter crate.
- **Pinned deps:** kiln crates from `https://github.com/AlfonsoCampodonico/kiln` at rev `01e7a9fe288160cc2eef9dd1a4a205022b8aef77`; vmkit from `https://github.com/AlfonsoCampodonico/vmkit` at the rev the ledger names.
- **Platforms:** `potter build` runs on Linux only. Everything else (and every non-namespace unit test) builds and passes on macOS.
- **Exit codes (§11):**
  - 0 on success;
  - 1 when a build step failed;
  - 2 for usage, plan or detection errors, and for features a later milestone ships;
  - 3 for a sandbox or protocol violation;
  - 4 for infrastructure errors (registry, missing subuid/newuidmap/helper, not Linux).
- **Executor choice:** never fall back to `ns` implicitly (§12). Without `--executor`, `build` exits 4: the VM executor ships in P4.
- **Output:** every repo-, registry- or guest-derived string that potter prints goes through `potter_plan::sanitize`. Step logs pass through unmodified.
- **Layers:**
  - Inputs, sealed, export and files layers are uncompressed OCI tars (`application/vnd.oci.image.layer.v1.tar`), stored in `$KILN_HOME/blobs`.
  - mtimes are normalised to `SOURCE_DATE_EPOCH` (default 0).
  - Entries are sorted bytewise by path.
  - uname/gname are empty.
- **Unsupported in P2a:** `Step::Apt`, `Step::CopyFrom`, `secrets`, `runtime_steps`, `Base::Catalog`, `Base::Stage` and `services`. Each fails at plan time with exit 2, naming the step or key and the milestone that ships it.
- **Defaults:** default tenant `local`; the executor class recorded in cache entries is `ns`.
- **Commits:** no attribution trailers or footers.

## Review Focus

1. **Undeclared inputs never reach a step (P6), and never change a key:** a source file that no `Inputs` step includes is absent from every step's filesystem, and changing it keeps every key.
2. **Repo symlinks (P8):** a symlink pointing outside the target root (`../..`, an absolute path) fails the build with exit 2 naming it. A symlinked directory is never descended into, and a file is never read through a symlink.
3. **Hostile tar input to unpack (base layers are untrusted):**
   - `..` and absolute entry paths, entries that traverse a symlink created earlier in the same layer, and hardlinks to outside the destination are refused;
   - device nodes other than whiteouts are skipped;
   - none of it can write outside `$POTTER_HOME/unpacked/<digest>`.
4. **Hostile upper to seal:** a step may leave anything in its upper (sockets, FIFOs, huge sparse files, deep trees, odd xattrs). Seal must stay within its limits, never follow a symlink, and report a violation as exit 3 without caching anything.
5. **Cache-key correctness:** keys change with the parent, the step (minus name and timeout), the inputs digest, the effective env, the platform and the executor class. They are stable across runs, hosts and `potter.toml` reformatting. A hit whose blob is missing from the store counts as a miss.

---

## File structure

| Path | Responsibility |
|---|---|
| `crates/potter-cache/src/lib.rs` | `PotterHome`, `Tenant`, `tenant_key`, re-exports |
| `crates/potter-cache/src/key.rs` | `CacheKey`, `ExecutorClass`, `base_key`, `step_key` |
| `crates/potter-cache/src/index.rs` | `Index`, `Entry` (atomic JSON files) |
| `crates/potter-exec/src/lib.rs` | `build_target`, `BuildOptions`, `BuildError`, re-exports |
| `crates/potter-exec/src/tarball.rs` | `TarWriter` (deterministic entries), `owner` parsing, path normalisation |
| `crates/potter-exec/src/inputs.rs` | inputs layer builder (§9.2), `beneath` walking |
| `crates/potter-exec/src/base.rs` | `BaseImage`, `resolve_base`, `layer_reader` (gzip/zstd/plain) |
| `crates/potter-exec/src/merge.rs` | the merged view of layer tars: `export_layer`, `read_file` |
| `crates/potter-exec/src/unpack.rs` | tar → overlay lower dir (Linux) |
| `crates/potter-exec/src/seal.rs` | overlay upper → OCI tar diff (Linux) |
| `crates/potter-exec/src/ns.rs` | `NsExecutor`: one `Run` step in vmkit's sandbox (Linux) |
| `crates/potter-exec/src/chain.rs` | the chain driver: keys, hits, misses, index |
| `crates/potter-exec/src/assemble.rs` | files layer, OCI config/manifest, kiln convert, ref |
| `crates/potter/src/main.rs` | `build`, hidden `__unpack`/`__seal` |
| `crates/potter/tests/build.rs` | end-to-end on Linux against kiln's test registry |

---

### Task 1: `potter-cache`

**Files:** Create `crates/potter-cache/{Cargo.toml, src/lib.rs, src/key.rs, src/index.rs}`.

**Interfaces (produces):**
```rust
pub struct PotterHome { root: PathBuf }
impl PotterHome {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CacheError>;   // creates root (0700)
    pub fn default_root() -> Result<PathBuf, CacheError>;               // $POTTER_HOME, else $XDG_DATA_HOME/potter, else ~/.local/share/potter
    pub fn root(&self) -> &Path;
    pub fn index(&self, tenant: &Tenant) -> Result<Index, CacheError>;  // cache/<tenant>/index/
    pub fn unpacked(&self) -> PathBuf;                                  // unpacked/
    pub fn mounts(&self, tenant: &Tenant) -> PathBuf;                   // mounts/<tenant>/
    pub fn runs(&self) -> PathBuf;                                      // runs/ (same filesystem as unpacked/)
}
pub struct Tenant(String);  // [a-z0-9][a-z0-9._-]{0,62}
impl Tenant { pub fn new(s: &str) -> Result<Self, CacheError>; pub fn local() -> Self; pub fn as_str(&self) -> &str }
pub fn tenant_key(home: &PotterHome, tenant: &Tenant) -> Result<[u8; 32], CacheError>; // tenants/<t>/key, 0600, from /dev/urandom

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)] #[serde(rename_all = "lowercase")]
pub enum ExecutorClass { Vm, Ns }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey(pub [u8; 32]);
impl CacheKey { pub fn hex(&self) -> String }
pub const POTTER_MAJOR: &str = "1";
/// The chain's root: the base image's manifest digest.
pub fn base_key(base_digest: &str) -> CacheKey;
pub struct StepInput<'a> {
    pub step: &'a potter_plan::Step,
    pub inputs_digest: Option<&'a str>,
    pub env: &'a BTreeMap<String, String>,   // effective env when the step runs
    pub platform: &'a str,                   // e.g. "linux/arm64"
    pub class: ExecutorClass,
}
pub fn step_key(parent: &CacheKey, input: &StepInput) -> Result<CacheKey, CacheError>;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Entry { pub layer: Option<String>, pub class: ExecutorClass, pub created: u64, pub potter: String }
pub struct Index { dir: PathBuf }
impl Index { pub fn get(&self, key: &CacheKey) -> Result<Option<Entry>, CacheError>; pub fn put(&self, key: &CacheKey, e: &Entry) -> Result<(), CacheError> }
```

`step_key` is SHA-256 over `"potter.cache/v1"`, then each field length-prefixed (u64 big-endian length, then bytes):
- `parent.0`;
- `potter_plan::step_hash(step)` bytes;
- `inputs_digest` (empty when none);
- the SHA-256 of `canonical_json(env)`;
- the secrets HMAC (empty in P2a);
- `platform`;
- `"vm"` or `"ns"`;
- `POTTER_MAJOR`.

`base_key` is SHA-256 of `"potter.cache/v1/base"` plus the length-prefixed digest.

- [ ] **Step 1: Tests** (`crates/potter-cache/src/key.rs`, `index.rs`, `lib.rs` `mod tests`):

```rust
// key.rs
fn run(script: &str) -> Step { Step::Run { name: "build".into(), command: Command::shell(script), env: BTreeMap::new(), workdir: "/app".into(), user: "0:0".into(), network: true, cache_mounts: vec![], secrets: vec![], timeout_secs: None } }
fn input<'a>(step: &'a Step, env: &'a BTreeMap<String, String>) -> StepInput<'a> { StepInput { step, inputs_digest: None, env, platform: "linux/arm64", class: ExecutorClass::Ns } }

#[test]
fn every_field_changes_the_key_and_nothing_else_does() {
    let env = BTreeMap::new();
    let parent = base_key("sha256:aa");
    let step = run("make");
    let k = step_key(&parent, &input(&step, &env)).unwrap();
    assert_eq!(k, step_key(&parent, &input(&step, &env)).unwrap(), "stable");
    // name and timeout do not count
    let mut renamed = step.clone();
    if let Step::Run { name, timeout_secs, .. } = &mut renamed { *name = "other".into(); *timeout_secs = Some(5); }
    assert_eq!(k, step_key(&parent, &input(&renamed, &env)).unwrap());
    // each of these does
    assert_ne!(k, step_key(&base_key("sha256:bb"), &input(&step, &env)).unwrap(), "parent");
    assert_ne!(k, step_key(&parent, &input(&run("make all"), &env)).unwrap(), "command");
    let env2: BTreeMap<_, _> = [("A".to_string(), "1".to_string())].into();
    assert_ne!(k, step_key(&parent, &input(&step, &env2)).unwrap(), "env");
    let mut i = input(&step, &env); i.inputs_digest = Some("sha256:cc");
    assert_ne!(k, step_key(&parent, &i).unwrap(), "inputs");
    let mut i = input(&step, &env); i.platform = "linux/amd64";
    assert_ne!(k, step_key(&parent, &i).unwrap(), "platform");
    let mut i = input(&step, &env); i.class = ExecutorClass::Vm;
    assert_ne!(k, step_key(&parent, &i).unwrap(), "class");
}

#[test]
fn length_prefixing_keeps_fields_apart() {
    let env = BTreeMap::new();
    let step = run("x");
    let mut a = input(&step, &env); a.inputs_digest = Some("ab"); a.platform = "c";
    let mut b = input(&step, &env); b.inputs_digest = Some("a"); b.platform = "bc";
    assert_ne!(step_key(&base_key("p"), &a).unwrap(), step_key(&base_key("p"), &b).unwrap());
}

// index.rs
#[test]
fn entries_round_trip_and_tenants_are_apart() {
    let dir = tempfile::tempdir().unwrap();
    let home = PotterHome::open(dir.path()).unwrap();
    let a = home.index(&Tenant::new("a").unwrap()).unwrap();
    let b = home.index(&Tenant::new("b").unwrap()).unwrap();
    let key = base_key("sha256:aa");
    let e = Entry { layer: Some("sha256:dd".into()), class: ExecutorClass::Ns, created: 1, potter: "0.1.0".into() };
    assert_eq!(a.get(&key).unwrap(), None);
    a.put(&key, &e).unwrap();
    assert_eq!(a.get(&key).unwrap(), Some(e));
    assert_eq!(b.get(&key).unwrap(), None);
    // no temporary files are left behind
    let names: Vec<_> = std::fs::read_dir(dir.path().join("cache/a/index")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, [std::ffi::OsString::from(key.hex())]);
}

#[test]
fn a_corrupt_entry_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let home = PotterHome::open(dir.path()).unwrap();
    let idx = home.index(&Tenant::local()).unwrap();
    let key = base_key("x");
    std::fs::write(dir.path().join("cache/local/index").join(key.hex()), "{not json").unwrap();
    assert_eq!(idx.get(&key).unwrap(), None);
}

// lib.rs
#[test]
fn tenant_names_are_checked_and_keys_are_private() {
    for bad in ["", "A", "../x", "a/b", ".hidden", &"x".repeat(64)] { assert!(Tenant::new(bad).is_err(), "{bad:?}"); }
    let dir = tempfile::tempdir().unwrap();
    let home = PotterHome::open(dir.path()).unwrap();
    let k1 = tenant_key(&home, &Tenant::local()).unwrap();
    assert_eq!(k1, tenant_key(&home, &Tenant::local()).unwrap());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.path().join("tenants/local/key")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
```

- [ ] **Step 2: Run; expect FAIL** (`cargo test -p potter-cache`: crate missing).
- [ ] **Step 3: Implement.**
  - `Index::put` writes `<key>.tmp-<pid>-<n>` with `create_new`, then `fsync`, then `rename`.
  - `get` maps a parse error to `None`.
  - `created` comes from `SystemTime::now()`.
  - `Entry.potter` is `env!("CARGO_PKG_VERSION")`.
  - `CacheError` covers `Io { path, source }`, `BadTenant(String)` and `Plan(PlanError)`.
- [ ] **Step 4: Run; expect PASS** (`cargo test -p potter-cache`, 5 tests).
- [ ] **Step 5: Commit** `feat(potter-cache): cache keys, the local index and tenants`.

### Task 2: Deterministic tars and inputs layers

**Files:** Create `crates/potter-exec/{Cargo.toml, src/lib.rs, src/error.rs, src/tarball.rs, src/inputs.rs}`.

**Interfaces (produces):**
```rust
// tarball.rs
pub struct Meta { pub mode: u32, pub uid: u64, pub gid: u64, pub mtime: u64 }
pub struct TarWriter<W: Write> { .. }  // GNU long names; empty uname/gname
impl<W: Write> TarWriter<W> {
    pub fn new(out: W) -> Self;
    pub fn dir(&mut self, path: &str, m: &Meta) -> io::Result<()>;
    pub fn file(&mut self, path: &str, m: &Meta, size: u64, data: &mut dyn Read) -> io::Result<()>;
    pub fn symlink(&mut self, path: &str, target: &str, m: &Meta) -> io::Result<()>;
    pub fn hardlink(&mut self, path: &str, target: &str) -> io::Result<()>;
    pub fn whiteout(&mut self, path: &str) -> io::Result<()>;        // `.wh.<name>` next to path
    pub fn opaque(&mut self, dir: &str) -> io::Result<()>;           // `<dir>/.wh..wh..opq`
    pub fn fifo(&mut self, path: &str, m: &Meta) -> io::Result<()>;
    pub fn xattr_file(&mut self, path: &str, m: &Meta, xattrs: &[(String, Vec<u8>)], size: u64, data: &mut dyn Read) -> io::Result<()>; // PAX SCHILY.xattr.*
    pub fn finish(self) -> io::Result<W>;
}
/// "0:0", "1000", "1000:1000", "root", or a user name looked up in `passwd`/`group` text.
pub fn owner(spec: &str, passwd: Option<&str>, group: Option<&str>) -> Result<(u64, u64), String>;
/// `SOURCE_DATE_EPOCH`, default 0.
pub fn epoch() -> u64;

// inputs.rs
pub const DEFAULT_EXCLUDES: [&str; 7] = [".git", "node_modules", "vendor", "target", ".venv", "__pycache__", ".potter"];
pub struct InputsSpec<'a> { pub root: &'a Path, pub include: &'a [String], pub exclude: &'a [String], pub dest: &'a str, pub owner: (u64, u64), pub epoch: u64 }
pub struct InputsSummary { pub files: u64, pub bytes: u64 }
pub fn build_inputs<W: Write>(spec: &InputsSpec, out: W) -> Result<(W, InputsSummary), BuildError>;
```

Rules (§9.2):
- **Walk:** directory descriptors, with every child opened `O_NOFOLLOW` relative to its parent (`rustix::fs::openat`, `Dir`), so nothing outside the root is ever reached.
- **Matching:** paths match relative to the root with `/` separators.
  - `include` globs: `globset`, `literal_separator(true)`; a pattern matching a directory includes its subtree.
  - `exclude` globs, plus `DEFAULT_EXCLUDES` as path components anywhere, plus `.potterignore` lines at the root: `#` comments, blank lines skipped, `!` re-includes, a leading `/` anchors, otherwise as globs.
  - Exclusion wins over inclusion unless a later `!` line re-includes.
- **Output:** entries go under `dest`, which is absolute: `dest/rel`. The dest directory and its parents are emitted as directories (0755, `owner`).
- **Metadata:** files are 0755 when any exec bit is set, else 0644. Directories are 0755. Symlinks are kept, with mode 0777.
  - A symlink whose target is absolute, or whose lexical resolution from its own directory leaves the root, is an error: `BuildError::SymlinkEscape { path, target }` (exit 2).
  - Hardlinks are stored as separate files.
  - Sockets, FIFOs and devices are an error: `BuildError::SpecialFile { path }` (exit 2).
- **Ordering:** entries are sorted bytewise; files are read once.

- [ ] **Step 1: Tests** (`inputs.rs` `mod tests`, macOS and Linux):

```rust
fn tree(files: &[(&str, &str)]) -> tempfile::TempDir { /* writes each file, creating parents */ }
fn entries(tar: &[u8]) -> Vec<(String, char, u32, u64, u64, u64)> { /* path, type ('d','f','l'), mode, uid, gid, mtime via the tar crate */ }
fn build(root: &Path, include: &[&str], exclude: &[&str]) -> Result<Vec<u8>, BuildError> { /* dest "/app", owner (0,0), epoch 0 */ }

#[test]
fn the_layer_is_byte_identical_across_runs_and_mtimes() {
    let t = tree(&[("package.json", "{}"), ("src/index.js", "x"), ("src/lib/a.js", "y")]);
    let a = build(t.path(), &["**"], &[]).unwrap();
    filetime::set_file_mtime(t.path().join("src/index.js"), filetime::FileTime::from_unix_time(12345, 0)).unwrap();
    let b = build(t.path(), &["**"], &[]).unwrap();
    assert_eq!(a, b);
    assert!(entries(&a).iter().all(|e| e.5 == 0));
}

#[test]
fn includes_excludes_and_default_excludes() {
    let t = tree(&[("package.json", "{}"), ("package-lock.json", "{}"), ("src/a.js", "x"), ("node_modules/x/i.js", "x"), ("sub/node_modules/y.js", "y"), (".git/HEAD", "r"), ("README.md", "r")]);
    let paths = |tar: &[u8]| entries(tar).into_iter().filter(|e| e.1 == 'f').map(|e| e.0).collect::<Vec<_>>();
    assert_eq!(paths(&build(t.path(), &["package.json", "package-lock.json"], &[]).unwrap()), ["app/package-lock.json", "app/package.json"]);
    assert_eq!(paths(&build(t.path(), &["**"], &["*.md"]).unwrap()), ["app/package-lock.json", "app/package.json", "app/src/a.js"]);
}

#[test]
fn potterignore_negation_and_anchoring() {
    let t = tree(&[(".potterignore", "logs\n!logs/keep.log\n/tmp\n"), ("logs/a.log", "a"), ("logs/keep.log", "k"), ("tmp/x", "x"), ("src/tmp/y", "y")]);
    let files: Vec<String> = entries(&build(t.path(), &["**"], &[]).unwrap()).into_iter().filter(|e| e.1 == 'f').map(|e| e.0).collect();
    assert!(files.contains(&"app/logs/keep.log".to_string()) && !files.contains(&"app/logs/a.log".to_string()));
    assert!(!files.contains(&"app/tmp/x".to_string()) && files.contains(&"app/src/tmp/y".to_string()));
}

#[test]
fn modes_are_normalised_and_owner_applied() {
    let t = tree(&[("run.sh", "#!/bin/sh"), ("data.txt", "d")]);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(t.path().join("run.sh"), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(t.path().join("data.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let spec = InputsSpec { root: t.path(), include: &["**".into()], exclude: &[], dest: "/app", owner: (33, 33), epoch: 7 };
    let (tar, _) = build_inputs(&spec, Vec::new()).unwrap();
    let e = entries(&tar);
    let find = |p: &str| e.iter().find(|x| x.0 == p).unwrap().clone();
    assert_eq!(find("app/run.sh").2 & 0o7777, 0o755);
    assert_eq!(find("app/data.txt").2 & 0o7777, 0o644);
    assert_eq!((find("app/data.txt").3, find("app/data.txt").4, find("app/data.txt").5), (33, 33, 7));
    assert_eq!(find("app").1, 'd');
}

#[test]
fn escaping_symlinks_are_refused_and_inner_ones_kept() {
    let t = tree(&[("a/x", "x")]);
    std::os::unix::fs::symlink("../a/x", t.path().join("a/inner")).unwrap();
    let ok = build(t.path(), &["**"], &[]).unwrap();
    assert!(entries(&ok).iter().any(|e| e.0 == "app/a/inner" && e.1 == 'l'));
    for target in ["../../etc/passwd", "/etc/passwd"] {
        let t = tree(&[("x", "x")]);
        std::os::unix::fs::symlink(target, t.path().join("link")).unwrap();
        let err = build(t.path(), &["**"], &[]).unwrap_err();
        assert!(matches!(err, BuildError::SymlinkEscape { .. }), "{target}: {err}");
    }
}

#[test]
fn a_symlinked_directory_is_not_descended() {
    let outside = tree(&[("secret", "s")]);
    let t = tree(&[("x", "x")]);
    std::os::unix::fs::symlink(outside.path(), t.path().join("dir")).unwrap();
    // absolute target: refused, never read
    assert!(matches!(build(t.path(), &["**"], &[]), Err(BuildError::SymlinkEscape { .. })));
}

#[test]
fn owners_parse_and_look_up() {
    let passwd = "root:x:0:0::/root:/bin/sh\nwww-data:x:33:33::/var/www:/usr/sbin/nologin\n";
    assert_eq!(owner("0:0", None, None), Ok((0, 0)));
    assert_eq!(owner("1000", None, None), Ok((1000, 1000)));
    assert_eq!(owner("root", None, None), Ok((0, 0)));
    assert_eq!(owner("www-data", Some(passwd), None), Ok((33, 33)));
    assert!(owner("nobody-here", Some(passwd), None).is_err());
    assert!(owner("www-data", None, None).is_err());
}
```

`filetime` is a dev-dependency.

- [ ] **Step 2: Run; expect FAIL** (`cargo test -p potter-exec inputs`).
- [ ] **Step 3: Implement** `tarball.rs` (on `tar::Header::new_gnu`, with `set_path` falling back to a GNU long name through `tar::Builder::append_data`) and `inputs.rs`. `BuildError` (in `error.rs`) gets `exit_code()` and these variants:
  - `SymlinkEscape { path, target }` and `SpecialFile { path }` (exit 2);
  - `Io { context, source }` (exit 4);
  - `Unsupported { what, milestone }` (exit 2);
  - `Plan(String)` (exit 2);
  - `StepFailed { step, status, tail: Vec<String> }` (exit 1);
  - `Hostile(String)` (exit 3);
  - `Infra(String)` (exit 4).
- [ ] **Step 4: Run; expect PASS** (7 tests).
- [ ] **Step 5: Commit** `feat(potter-exec): deterministic tars and inputs layers`.

### Task 3: OCI bases

**Files:** Create `crates/potter-exec/src/base.rs`.

**Interfaces (produces):**
```rust
pub struct BaseImage { pub resolved: kiln_oci::ResolvedImage, pub repository: String }
impl BaseImage {
    pub fn key_digest(&self) -> String;                 // manifest digest, "sha256:…"
    pub fn layers(&self) -> &[kiln_oci::Descriptor];
    pub fn env(&self) -> BTreeMap<String, String>;      // from config Env ("K=V")
    pub fn workdir(&self) -> Option<String>;
    pub fn user(&self) -> Option<String>;
}
/// Resolves `reference` (pinned to `digest` when given) for `platform` and fetches every layer into `store`.
pub fn resolve_base(store: &Store, reference: &str, digest: Option<&str>, platform: &Platform, auth: DockerConfig) -> Result<BaseImage, BuildError>;
/// A layer's uncompressed tar stream (by media type: tar, tar+gzip, tar+zstd, Docker variants).
pub fn layer_reader(store: &Store, layer: &Descriptor) -> Result<Box<dyn Read + Send>, BuildError>;
```

Resolution:
- `Reference::parse(reference)`. When `digest` is set, resolve `<name>@<digest>` and check that the manifest digest equals it.
- `Client::new(reference.registry(), auth)`, then `kiln_oci::resolve_registry(store, &client, &reference, &[platform])`, then the first image.
- For each layer: `client.fetch_blob(repository, digest, size, store)`.
- Registry and network errors are `Infra` (exit 4). A digest mismatch with the plan is `Plan` (exit 2).

- [ ] **Step 1: Tests** (`base.rs` `mod tests`; serve a layout from `kiln_registry::testregistry::TestRegistry::serve_layout` built with `kiln_oci::testlayout::LayoutBuilder` and `kiln_erofs::testtar::TarBuilder`, gzip via `flate2`):

```rust
#[test]
fn a_base_resolves_fetches_its_layers_and_reads_them_back() {
    let (reg, _dir) = registry_with_image("lib/busybox:1", &[("bin/busybox", b"elf".as_slice())], &["PATH=/bin", "A=1"], "/srv");
    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    let r = format!("{}/lib/busybox:1", reg.host());
    let base = resolve_base(&store, &r, None, &Platform::host(), DockerConfig::anonymous()).unwrap();
    assert!(base.key_digest().starts_with("sha256:"));
    assert_eq!(base.env().get("A").map(String::as_str), Some("1"));
    assert_eq!(base.workdir().as_deref(), Some("/srv"));
    let mut tar = tar::Archive::new(layer_reader(&store, &base.layers()[0]).unwrap());
    let names: Vec<String> = tar.entries().unwrap().map(|e| e.unwrap().path().unwrap().display().to_string()).collect();
    assert!(names.contains(&"bin/busybox".to_string()));
}

#[test]
fn a_pinned_digest_must_match() {
    let (reg, _dir) = registry_with_image("lib/busybox:1", &[("x", b"x".as_slice())], &[], "/");
    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    let r = format!("{}/lib/busybox:1", reg.host());
    let wrong = format!("sha256:{}", "0".repeat(64));
    let err = resolve_base(&store, &r, Some(&wrong), &Platform::host(), DockerConfig::anonymous()).unwrap_err();
    assert_eq!(err.exit_code(), 2, "{err}");
}
```

`registry_with_image(name, files, env, workdir) -> (TestRegistry, TempDir)` lives in `crates/potter-exec/src/testing.rs` (`#[cfg(test)]` plus a `pub mod testing` behind the `testing` feature for `crates/potter/tests`). It builds one gzip layer for the host platform with a `ContainerConfig` and serves it with `serve_layout`. kiln's test helpers must be enabled: check each crate's `Cargo.toml` for a feature gate on `testregistry`/`testlayout`/`testtar`, and enable it in `[dev-dependencies]`.

- [ ] **Step 2: Run; expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run; expect PASS** (2 tests).
- [ ] **Step 5: Commit** `feat(potter-exec): OCI bases through kiln's registry client`.

### Task 4: The merged view of layers, and export

**Files:** Create `crates/potter-exec/src/merge.rs`.

**Interfaces (produces):**
```rust
/// One layer of a chain, bottom first: opens its uncompressed tar stream (it may be opened twice).
pub type LayerOpen<'a> = Box<dyn Fn() -> Result<Box<dyn Read + Send>, BuildError> + 'a>;
pub struct ExportSpec<'a> { pub exports: &'a [potter_plan::Export], pub epoch: u64, pub spill_dir: &'a Path }
pub struct ExportSummary { pub entries: u64, pub bytes: u64 }
pub fn export_layer<W: Write>(layers: &[LayerOpen], spec: &ExportSpec, out: W) -> Result<(W, ExportSummary), BuildError>;
/// The content of `path` in the merged view (e.g. /etc/passwd), if it is a regular file of at most `cap` bytes.
pub fn read_file(layers: &[LayerOpen], path: &str, cap: u64) -> Result<Option<Vec<u8>>, BuildError>;
```

The merge:
- **Pass 1 (metadata):** layers bottom→top build `BTreeMap<String, Node>`.
  - A `.wh.<n>` entry removes `<n>` and its subtree from lower layers.
  - `<d>/.wh..wh..opq` removes `<d>`'s lower-layer subtree.
  - Entries of the same layer are never hidden by its own opaque marker.
  - A hardlink resolves to its target node at that point; if the target is not in the merged view, the entry is skipped.
  - Paths are normalised (`./` stripped, `..` or an absolute path refused as `Hostile`).
- **Selection:** for each `Export { path, dest, exclude }`, take `path` and its subtree, minus entries whose path relative to `path` equals or lies under any `exclude` glob. Rename the prefix `path` → `dest`.
  - Emit missing parents of `dest` as 0755 root directories.
  - Mode, uid and gid come from the merged entry; mtime becomes `epoch`.
  - Two exports mapping onto the same destination path are a `Plan` error.
- **Pass 2 (data):** re-read only the layers that hold selected file data, copying each needed file into one spill file under `spill_dir` (offsets recorded). Then write the output tar in bytewise path order.
  - A hardlink whose target is also exported stays a hardlink (renamed); otherwise it is written as a regular file.
  - Selected whiteouts and opaque markers are never emitted: the export is a flattened snapshot.

- [ ] **Step 1: Tests** (`merge.rs` `mod tests`; layers built with `TarBuilder`):

```rust
fn layer(t: TarBuilder) -> LayerOpen<'static> { let bytes = t.finish(); Box::new(move || Ok(Box::new(std::io::Cursor::new(bytes.clone())))) }
fn export(layers: &[LayerOpen], exports: &[(&str, &str, &[&str])]) -> Vec<(String, char, u32, u64, Vec<u8>)> { /* runs export_layer into a Vec and lists (path, type, mode, uid, data) */ }

#[test]
fn whiteouts_and_opaque_dirs_hide_lower_entries() {
    let o = Opts::default();
    let l0 = layer(TarBuilder::new().dir("app", &o).file("app/a", b"a", &o).file("app/b", b"b", &o).dir("app/d", &o).file("app/d/old", b"o", &o).clone());
    let l1 = layer(TarBuilder::new().whiteout("app/a").opaque("app/d").file("app/d/new", b"n", &o).clone());
    let out = export(&[l0, l1], &[("/app", "/app", &[])]);
    let paths: Vec<&str> = out.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(paths, ["app", "app/b", "app/d", "app/d/new"]);
}

#[test]
fn export_remaps_excludes_and_normalises_mtime_only() {
    let o = Opts::default().uid(1000).gid(1000).mode(0o750).mtime(99);
    let l0 = layer(TarBuilder::new().dir("out", &o).file("out/app", b"bin", &o).dir("out/cache", &o).file("out/cache/x", b"x", &o).clone());
    let out = export(&[l0], &[("/out", "/srv", &["cache"])]);
    let paths: Vec<&str> = out.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(paths, ["srv", "srv/app"]);
    let app = out.iter().find(|e| e.0 == "srv/app").unwrap();
    assert_eq!((app.2 & 0o7777, app.3, app.4.as_slice()), (0o750, 1000, b"bin".as_slice()));
}

#[test]
fn a_single_file_export_and_missing_parents() {
    let o = Opts::default();
    let l0 = layer(TarBuilder::new().dir("out", &o).file("out/app", b"bin", &o).clone());
    let out = export(&[l0], &[("/out/app", "/usr/local/bin/app", &[])]);
    let paths: Vec<&str> = out.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(paths, ["usr", "usr/local", "usr/local/bin", "usr/local/bin/app"]);
}

#[test]
fn hardlinks_survive_or_become_files() {
    let o = Opts::default();
    let l0 = layer(TarBuilder::new().dir("a", &o).file("a/x", b"data", &o).hardlink("a/y", "a/x").dir("b", &o).hardlink("b/z", "a/x").clone());
    let out = export(&[l0], &[("/b", "/b", &[])]);
    let z = out.iter().find(|e| e.0 == "b/z").unwrap();
    assert_eq!((z.1, z.4.as_slice()), ('f', b"data".as_slice()));
    let out = export(&[layer(TarBuilder::new().dir("a", &o).file("a/x", b"d", &o).hardlink("a/y", "a/x").clone())], &[("/a", "/a", &[])]);
    assert_eq!(out.iter().find(|e| e.0 == "a/y").unwrap().1, 'h');
}

#[test]
fn hostile_paths_are_refused() {
    let o = Opts::default();
    for bad in ["../x", "/etc/x", "a/../../x"] {
        let l = layer(TarBuilder::new().file(bad, b"x", &o).clone());
        let dir = tempfile::tempdir().unwrap();
        let spec = ExportSpec { exports: &[potter_plan::Export { path: "/a".into(), dest: "/a".into(), exclude: vec![] }], epoch: 0, spill_dir: dir.path() };
        assert!(matches!(export_layer(&[l], &spec, Vec::new()), Err(BuildError::Hostile(_))), "{bad}");
    }
}

#[test]
fn read_file_sees_the_merged_view() {
    let o = Opts::default();
    let l0 = layer(TarBuilder::new().dir("etc", &o).file("etc/passwd", b"root:x:0:0::/root:/bin/sh\n", &o).clone());
    let l1 = layer(TarBuilder::new().file("etc/passwd", b"app:x:1000:1000::/home/app:/bin/sh\n", &o).clone());
    assert_eq!(read_file(&[l0, l1], "/etc/passwd", 1 << 20).unwrap().unwrap(), b"app:x:1000:1000::/home/app:/bin/sh\n");
}
```

Builder method names follow `kiln_erofs::testtar::TarBuilder` (`dir`, `file`, `hardlink`, `whiteout`, `opaque` return `&mut Self`; `.clone()` is used in the sketch to get an owned value; adjust to the real API). `Opts` comes from the same module.

- [ ] **Step 2: Run; expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run; expect PASS** (6 tests).
- [ ] **Step 5: Commit** `feat(potter-exec): the merged view of layers and export`.

### Task 5: Unpacking layers into overlay lower directories (Linux)

**Files:** Create `crates/potter-exec/src/unpack.rs`; modify `crates/potter/src/main.rs` (hidden `__unpack`).

**Interfaces (produces):**
```rust
#[cfg(target_os = "linux")]
pub fn unpack(reader: impl Read, dest: &Path) -> Result<UnpackSummary, BuildError>;  // run as root of the sandbox's user namespace
pub struct UnpackSummary { pub entries: u64, pub skipped_devices: u64 }
/// Unpacks `layer` into `$POTTER_HOME/unpacked/<digest>` (once; atomic) by running `potter __unpack` in the sandbox.
pub fn ensure_unpacked(ctx: &SandboxCtx, store: &Store, layer: &Descriptor, home: &PotterHome) -> Result<PathBuf, BuildError>;
pub struct SandboxCtx { pub potter: PathBuf, pub ids: u32 /* 65536 */ }
```

Rules:
- `dest` must exist and be empty.
- Every entry path is normalised; `..` or an absolute path is `Hostile`.
- Parents are opened component by component from a descriptor on `dest` with `O_NOFOLLOW | O_DIRECTORY`. A symlink in the middle of a path (even one from this layer) is `Hostile`.
- **Entry kinds:**
  - files are created `O_CREAT | O_EXCL | O_NOFOLLOW`; an existing entry is replaced (tar semantics: later wins);
  - directories use `mkdirat` (an existing directory keeps its contents and gets the new metadata);
  - symlinks use `symlinkat`;
  - hardlinks use `linkat`, whose target must already exist below `dest` (opened the same way);
  - FIFOs use `mknodat(S_IFIFO)`;
  - char and block devices are skipped (counted).
- **Whiteouts:** `.wh.<n>` becomes `mknodat(S_IFCHR, 0)` at `<n>`; `.wh..wh..opq` sets `user.overlay.opaque=y` on its directory.
- **Metadata:** `fchownat(.., AT_SYMLINK_NOFOLLOW)` to the entry's uid and gid (an id above the mapped count is `Hostile`), then mode (not for symlinks), then mtime.
- **xattrs:** PAX `SCHILY.xattr.user.*` are set; others are skipped.
- **The subcommand:** `potter __unpack <blob-path> <media-type> <dest>` decompresses through `layer_reader`'s logic (a function taking a `File` and a media type) and calls `unpack`.
- **`ensure_unpacked`:**
  - creates `unpacked/<hex>.tmp-<pid>`;
  - runs the sandbox `Spec { Program::Path(potter), args: ["__unpack", blob, media, tmp], root: Root::Host, ids: Subordinate { count: 65536 }, mounts: [Bind(tmp writable)], network: None, nest: false, seccomp: false }`;
  - renames to `unpacked/<hex>`;
  - when the final directory exists, it is reused.

- [ ] **Step 1: Tests** (Linux-only `#[cfg(target_os = "linux")]` integration test `crates/potter/tests/sandboxed.rs`, skipped unless `VMKIT_SANDBOX` is set, as in vmkit):

```rust
#[test]
fn unpacking_makes_overlay_whiteouts_owners_and_refuses_escapes() {
    let Some(ctx) = Ctx::new() else { return };
    let o = Opts::default();
    let tar = TarBuilder::new().dir("etc", &o).file("etc/f", b"x", &o.clone().uid(1000).gid(1000)).whiteout("etc/gone").opaque("etc").symlink("link", "/etc", &o).chardev("dev/null", 1, 3, &o).finish();
    let dest = ctx.unpack(&tar).unwrap();
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let gone = std::fs::symlink_metadata(dest.join("etc/gone")).unwrap();
    assert!(gone.file_type().is_char_device() && gone.rdev() == 0);
    assert_eq!(xattr(&dest.join("etc"), "user.overlay.opaque").as_deref(), Some(&b"y"[..]));
    assert_eq!(std::fs::metadata(dest.join("etc/f")).unwrap().uid(), ctx.subuid_start + 999);
    assert!(std::fs::symlink_metadata(dest.join("dev/null")).is_err(), "devices are skipped");
    for bad in [TarBuilder::new().file("../x", b"x", &o).finish(), TarBuilder::new().symlink("l", "/tmp", &o).file("l/x", b"x", &o).finish()] {
        assert_eq!(ctx.unpack(&bad).unwrap_err().exit_code(), 3);
    }
}
```

`Ctx` (in `crates/potter/tests/support/mod.rs`) holds temp `KILN_HOME`/`POTTER_HOME`, the built `potter` binary (`env!("CARGO_BIN_EXE_potter")`), and the caller's subuid start (parsed like vmkit's tests). Its `unpack(bytes)` stores the bytes as a blob, then calls `ensure_unpacked`.

- [ ] **Step 2: Run in Lima; expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run in Lima; expect PASS.**
- [ ] **Step 5: Commit** `feat(potter-exec): unpack layers into overlay lower directories`.

### Task 6: `NsExecutor` (one `Run` step) and sealing

**Files:** Create `crates/potter-exec/src/ns.rs`, `crates/potter-exec/src/seal.rs`; modify `crates/potter/src/main.rs` (hidden `__seal`); pin vmkit.

**Interfaces (produces):**
```rust
pub struct StepRun<'a> {
    pub lowers: Vec<PathBuf>,                 // top first
    pub upper: PathBuf, pub work: PathBuf,     // empty, same filesystem
    pub argv: Vec<String>,                    // already wrapped (see below)
    pub env: BTreeMap<String, String>,
    pub user: vmkit::sandbox::User,
    pub network: bool,
    pub cache_mounts: Vec<(PathBuf, String)>, // host dir, target
    pub timeout: Duration,
    pub run_dir: PathBuf,
    pub log: &'a mut dyn FnMut(&str),         // one line of output
}
pub struct StepOutcome { pub status: ExitStatus, pub tail: Vec<String> /* last 200 lines */, pub timed_out: bool }
pub fn run_step(r: StepRun) -> Result<StepOutcome, BuildError>;
/// `sh -c 'mkdir -p -- "$0" && cd -- "$0" && exec "$@"' <workdir> <argv…>`; Shell(s) → argv ["/bin/sh", "-c", s].
pub fn wrap(command: &potter_plan::Command, workdir: &str) -> Vec<String>;
#[cfg(target_os = "linux")]
pub fn seal(upper: &Path, out: impl Write, limits: &SealLimits, skip_if_empty: &[String], epoch: u64) -> Result<SealSummary, BuildError>;
pub struct SealLimits { pub max_bytes: u64 /* 8 GiB */, pub max_entries: u64 /* 1_000_000 */, pub max_depth: usize /* 128 */, pub max_name: usize /* 255 */ }
pub fn seal_in_sandbox(ctx: &SandboxCtx, upper: &Path, store: &Store, skip: &[String]) -> Result<(Digest, u64), BuildError>; // runs `potter __seal`, writes run_dir/diff.tar, put_reader into store
```

`run_step` builds this `vmkit::sandbox::Spec`:

| Field | Value |
|---|---|
| `program` | `Path("/bin/sh")`, with the wrapped args |
| `root` | `Overlay { lowers, upper, work }` |
| `ids` | `Subordinate { count: 65536 }` |
| `nest` | `true` |
| `seccomp` | `true` |
| `network` | `Egress(NetSpec::default())` when `network`, else `None` |
| `mounts` | cache mounts as writable `Bind`s, plus `Bind(run_dir/resolv.conf → /etc/resolv.conf, read-only)` when networked. `resolv.conf` holds `nameserver <vmkit::net::NAMESERVER>`. |
| `env` | `Some(env)`, with `HOME` added from the user's passwd entry (or `/root`) when absent |
| `cwd` | `/` |
| `user` | `Some(user)` |
| `limits` | open_files 1 048 576, processes 16 384, cgroup `None` |

Execution:
- Stdout and stderr are piped and read line by line on two threads into `log` and a 200-line ring.
- On timeout the sandbox is killed and `timed_out` is set.

Seal (§9.4):
- It walks the upper with descriptors (`O_NOFOLLOW`), in sorted order.
- **Whiteouts and opaque dirs:** a 0/0 char device becomes a `.wh.` entry; `user.overlay.opaque=y` becomes an opaque marker after the directory entry. Other `user.overlay.*` are ignored; other `user.*` and `security.capability` xattrs become PAX xattrs.
- **Regular entries:**
  - regular files, directories and symlinks keep mode, uid and gid, with mtime set to `epoch`;
  - a file with `nlink > 1` seen before becomes a hardlink;
  - FIFOs are kept; sockets and devices other than 0/0 are skipped.
- **Mount points:** a path in `skip_if_empty` that is an empty file or directory is left out (mount points the sandbox created: `etc/resolv.conf`, cache-mount targets).
- **Limits:** exceeding a limit is `Hostile` (exit 3) for depth and name length, and `StepFailed`-class "build output exceeded N" (exit 1) for bytes and entries.
- **The subcommand:** `potter __seal <upper> <out> [--skip <path>]...` runs in the sandbox with `Root::Host`, subordinate ids and the out directory writable.

- [ ] **Step 1: Pin vmkit.**
  - Add `vmkit = { git = "https://github.com/AlfonsoCampodonico/vmkit", rev = "<sandbox-exec head after its review fixes>" }`.
  - Record the rev in the ledger.
  - When vmkit's PR merges, Task 9 repins to the merge commit.
- [ ] **Step 2: Tests** (`crates/potter/tests/sandboxed.rs`, Linux, `VMKIT_SANDBOX`):

```rust
#[test]
fn a_step_runs_in_its_own_root_and_its_changes_seal_into_a_diff() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();            // unpacked busybox rootfs (host /usr/bin/busybox + applets, /etc/passwd with app:1000)
    let mut lines = Vec::new();
    let out = ctx.run(&base, "mkdir -p /app && echo built >/app/out && rm /etc/motd && echo log-line", "/", "0:0", false, &mut lines).unwrap();
    assert!(out.status.success(), "{:?}", out.tail);
    assert!(lines.iter().any(|l| l == "log-line"));
    let entries = ctx.seal_entries(&out);
    assert!(entries.contains(&("app/out".into(), 'f')));
    assert!(entries.contains(&("etc/.wh.motd".into(), 'f')));
    assert!(!entries.iter().any(|e| e.0.starts_with("etc/resolv.conf")), "no mount point in the diff");
}

#[test]
fn the_workdir_is_created_and_the_user_applied() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let mut lines = Vec::new();
    let out = ctx.run(&base, "pwd; id -u; echo $HOME", "/srv/app", "1000:1000", false, &mut lines).unwrap();
    assert_eq!(lines, ["/srv/app", "1000", "/home/app"], "{:?}", out.tail);
}

#[test]
fn undeclared_host_files_are_not_visible() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let mut lines = Vec::new();
    let out = ctx.run(&base, &format!("test -e {} && echo leak || echo absent", ctx.home().display()), "/", "0:0", false, &mut lines).unwrap();
    assert_eq!(lines, ["absent"], "{:?}", out.tail);
}

#[test]
fn a_timeout_kills_the_step() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let out = ctx.run_with_timeout(&base, "sleep 30", Duration::from_secs(1)).unwrap();
    assert!(out.timed_out && !out.status.success());
}

#[test]
fn seal_limits_report_hostile_depth() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let deep = (0..140).map(|_| "d").collect::<Vec<_>>().join("/");
    let out = ctx.run(&base, &format!("mkdir -p /{deep}"), "/", "0:0", false, &mut Vec::new()).unwrap();
    assert_eq!(ctx.seal_err(&out).exit_code(), 3);
}
```

`Ctx::busybox_lower()` builds a tar with `TarBuilder`:
- `bin/busybox` from `/usr/bin/busybox`, plus symlinks for `sh mkdir echo rm id pwd test sleep cat ls touch chown`;
- `etc/passwd` with `root:x:0:0::/root:/bin/sh` and `app:x:1000:1000::/home/app:/bin/sh`;
- `etc/group` and `etc/motd`;
- directories `proc sys dev tmp(1777) home/app(1000) root`.

It unpacks this through `ensure_unpacked` and returns the lower dir. `ctx.run(..)` creates `upper` and `work` in a fresh `runs/<n>/` and calls `run_step` with `wrap(Command::Shell(script), workdir)`.

- [ ] **Step 3: Run in Lima; expect FAIL.**
- [ ] **Step 4: Implement** `ns.rs`, `seal.rs` and `__seal`.
- [ ] **Step 5: Run in Lima; expect PASS** (5 tests).
- [ ] **Step 6: Commit** `feat(potter-exec): run steps in vmkit's sandbox and seal their diffs`.

### Task 7: The chain driver

**Files:** Create `crates/potter-exec/src/chain.rs`.

**Interfaces (produces):**
```rust
pub struct BuildCtx<'a> {
    pub store: &'a Store, pub home: &'a PotterHome, pub tenant: &'a Tenant, pub index: &'a Index,
    pub sandbox: &'a SandboxCtx, pub platform: Platform, pub no_cache: bool, pub auth: DockerConfig,
    pub progress: &'a mut dyn FnMut(Progress),
}
pub enum Progress { Step { target: String, step: String, state: StepState }, Log { target: String, step: String, line: String } }
pub enum StepState { Cached, Running, Done { secs: f64 }, Failed }
pub struct ChainState {
    pub base: BaseImage,
    pub layers: Vec<LayerRef>,     // bottom first: base layers, then each layer-producing step's blob
    pub env: BTreeMap<String, String>, pub workdir: String, pub user: String,
    pub executed: usize, pub cached: usize,
}
pub struct LayerRef { pub digest: Digest, pub media_type: String, pub size: u64, pub diff_id: Digest }
pub fn run_chain(ctx: &mut BuildCtx, target: &Target, repo_root: &Path) -> Result<ChainState, BuildError>;
```

The chain:
- **Checks:** `check_supported(target)` runs first and refuses (`Unsupported`, exit 2) `Step::Apt`, `Step::CopyFrom`, non-empty `secrets`, `Base::Catalog` ("the catalog mapping ships in P2b; set `base.oci`"), `Base::Stage`, non-empty `image.runtime_steps` and non-empty `image.services`.
- **Base:** `resolve_base`. Its env, workdir (default `/`) and user (default `0:0`) seed the state.
- **Each step**, in order, keyed from `base_key(base.key_digest())`:
  - `Inputs`:
    - Build the layer with `build_inputs` from `repo_root.join(target.root)`.
    - The owner is resolved through `tarball::owner`, with passwd and group from `merge::read_file` over the current layers when the owner is a name.
    - Store the blob (`put_reader`); `inputs_digest` = the blob digest.
    - The key is computed, then the layer is pushed (whether cached or not: building it is the hash).
  - `Meta`: the env, workdir and user update. The key is computed, and the entry is `layer: None`.
  - `Run`:
    - Its effective env is the state env plus the step env.
    - The key is computed. When `!no_cache` and the index has an entry whose layer blob exists in the store, it is a hit: push the layer, `cached += 1`.
    - Otherwise it is a miss: the lowers are every layer so far, unpacked (`ensure_unpacked`, top first). A fresh `runs/<n>/{upper,work,run}`.
    - `run_step`:
      - the user is resolved to numeric ids through passwd/group (`merge::read_file`);
      - cache mounts are `home.mounts(tenant)/<id>/`, created 0755;
      - `network` comes from the step;
      - the timeout is `timeout_secs` (default 1800).
    - On failure: `StepFailed`, with the tail.
    - Then `seal_in_sandbox`, then index `put(key, layer)`. Push the layer; `executed += 1`.
- **Afterwards:** the run directories are removed by running `/bin/rm -rf` in the sandbox (`Root::Host`; subordinate-owned files).
- **Progress:** `Cached`, `Running` and `Done` for every step; each log line goes out as `Progress::Log`.

- [ ] **Step 1: Tests.**
  - Unit (macOS): `check_supported` refusals, one per kind, each exit 2 and naming the step.
  - Linux `crates/potter/tests/sandboxed.rs`, against a busybox image served by `TestRegistry`:

```rust
#[test]
fn a_chain_runs_then_hits_and_undeclared_changes_keep_every_key() {
    let Some(ctx) = Ctx::new() else { return };
    let repo = ctx.repo(&[("src/a.txt", "a"), ("notes.md", "n")]);
    let target = ctx.target_with(&repo, vec![
        inputs("src", &["src/**"], "/app"),
        run("build", "cat /app/src/a.txt > /app/out && test ! -e /app/notes.md"),
    ]);
    let first = ctx.chain(&target).unwrap();
    assert_eq!((first.executed, first.cached), (1, 0));
    std::fs::write(repo.path().join("notes.md"), "changed").unwrap();
    let second = ctx.chain(&target).unwrap();
    assert_eq!((second.executed, second.cached), (0, 1));
    assert_eq!(first.layers, second.layers);
    std::fs::write(repo.path().join("src/a.txt"), "b").unwrap();
    assert_eq!(ctx.chain(&target).unwrap().executed, 1);
}

#[test]
fn a_failing_step_reports_its_tail_and_caches_nothing() {
    let Some(ctx) = Ctx::new() else { return };
    let repo = ctx.repo(&[]);
    let target = ctx.target_with(&repo, vec![run("bad", "echo oops; exit 3")]);
    let err = ctx.chain(&target).unwrap_err();
    assert_eq!(err.exit_code(), 1);
    assert!(matches!(&err, BuildError::StepFailed { tail, .. } if tail.iter().any(|l| l == "oops")));
    assert_eq!(ctx.index_entries(), 0);
}

#[test]
fn a_cache_mount_persists_and_stays_out_of_the_layer() {
    let Some(ctx) = Ctx::new() else { return };
    let repo = ctx.repo(&[]);
    let mut step = run("warm", "echo hit >> /cache/log; cat /cache/log | wc -l > /app-count");
    if let Step::Run { cache_mounts, .. } = &mut step { cache_mounts.push(CacheMount { id: "c".into(), target: "/cache".into(), sharing: Default::default() }); }
    let target = ctx.target_with(&repo, vec![step]);
    let s = ctx.chain(&target).unwrap();
    assert!(!ctx.layer_paths(s.layers.last().unwrap()).iter().any(|p| p.starts_with("cache/")));
    assert_eq!(std::fs::read_to_string(ctx.home().join("mounts/local/c/log")).unwrap(), "hit\n");
}
```

- [ ] **Step 2: Run; expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run (macOS unit + Lima integration); expect PASS.**
- [ ] **Step 5: Commit** `feat(potter-exec): the chain driver with the local cache`.

### Task 8: Image assembly

**Files:** Create `crates/potter-exec/src/assemble.rs`; modify `crates/potter-exec/src/lib.rs` (`build_target`).

**Interfaces (produces):**
```rust
pub struct BuildOptions { pub tenant: Tenant, pub no_cache: bool, pub tags: Vec<String>, pub platform: Platform, pub auth: DockerConfig }
pub struct Built { pub target: String, pub reference: String, pub digest: Digest, pub executed: usize, pub cached: usize }
pub fn build_target(ctx: &mut BuildCtx, target: &Target, repo_root: &Path, tags: &[String]) -> Result<Built, BuildError>;
```

Assembly:
- **Run base:** `image.base`, resolved again only when it differs from `build.base`.
- **Layers, bottom first:**
  1. the run base's layers;
  2. the export layer (`export_layer` over the chain's layers, opened through `layer_reader`) when `image.export` is non-empty. Otherwise `image.base` must equal `build.base`, and the chain's own layers above the base are used as they are; else `Plan` error, exit 2;
  3. a files layer from `image.files`: root-owned, the given modes, parents 0755, mtime epoch; omitted when empty.
- **Config:** a `kiln_oci::ImageConfig` with `architecture`/`os`/`variant` from the platform, and `rootfs.diff_ids` = the base's diff_ids then our blob digests (uncompressed, so diff_id = digest). `config: ContainerConfig`:
  - `Env` = the run base's env, plus `image.config.env`, plus the default process's env, as sorted `K=V`;
  - `Entrypoint` = the default process's `command` (process `web`, else the only process, else none);
  - `WorkingDir` = the process `workdir`, else `image.config.workdir`;
  - `User` = `image.config.user`;
  - `StopSignal` = `image.config.stop_signal`.
- **Commit:**
  1. Write the config and the manifest (`canonical_json`, the base layers' descriptors as they are, ours as `application/vnd.oci.image.layer.v1.tar`) with `store.put_bytes`.
  2. Build `ResolvedImage { platform, manifest_digest, manifest, config_digest, config, ref_name: None }`.
  3. `kiln_image::convert_resolved(store, &[it], Some(&reference), &ConvertOptions::default(), &StoredLayers)`.
  4. Holding `store.lock_shared()`, `set_ref` for each tag: by default `potter/<target>:latest`.
- **Ledger:** `processes` other than the default are not representable in kiln's config yet.

- [ ] **Step 1: Tests.**
  - Unit: the config builder, with env merging order, the default process choice and diff_ids.
  - Linux `sandboxed.rs`: `build_target` on a busybox chain with an export of `/app` → `/app` and one generated file. Then `kiln_image::load(store, "potter/app:latest")` succeeds. Its config's process has the entrypoint, env and workdir, and its layers are the base's plus 2.
- [ ] **Step 2: Run; expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run; expect PASS.**
- [ ] **Step 5: Commit** `feat(potter-exec): assemble kiln images`.

### Task 9: `potter build`

**Files:** Modify `crates/potter/src/main.rs`, `crates/potter/Cargo.toml`, `README.md`, `docs/plan-format.md` (cache key section), `.github/workflows/ci.yml`; create `crates/potter/tests/build.rs`.

**CLI:** `potter build [PATH] [--target T]... [--executor vm|ns] [--tenant ID] [--no-cache] [--tag REF]...`
- Without `--executor`: exit 4, "the VM executor ships in P4; pass --executor ns to build in Linux namespaces (weaker isolation)".
- `--executor vm`: exit 4, same message.
- `ns` prerequisites, each exit 4 naming the piece:
  - Linux;
  - the vmkit helper found;
  - `newuidmap`/`newgidmap`;
  - a subuid/subgid range ≥ 65536;
  - kernel ≥ 5.12.
- It plans with `potter_config::plan_repo`, then builds each selected target in plan order.
- Progress lines go to stderr: `[app] build: running`, `[app] build: done (3.2s)`, `[app] deps: cached`. Step log lines are prefixed `[app/build] `.
- Success prints `potter/app:latest sha256:<digest> (executed 2, cached 3)` per target to stdout.

Steps:
- [ ] **Step 1: Tests.**
  - `crates/potter/tests/cli.rs` (any platform): `build` without `--executor` exits 4 and its message mentions `--executor ns`.
  - `crates/potter/tests/build.rs` (Linux, `VMKIT_SANDBOX`):
    - A repo with an explicit `potter.toml` target whose `base.oci` is the busybox image on a `TestRegistry`, with an `Inputs` step and a `Run` step, and `export = ["/app"]` plus a web process.
    - `potter build --executor ns` exits 0 and prints the ref; `KILN_HOME` has it (`kiln_image::load`).
    - A second run prints `executed 0`.
    - `--no-cache` prints `executed 1`.
- [ ] **Step 2: Run; expect FAIL.**
- [ ] **Step 3: Implement.**
  - Repin vmkit to its `main` merge commit if the PR has merged; otherwise keep the branch rev and ledger it.
  - CI (`ubuntu-24.04`) runs `cargo test --test build --test sandboxed` after this setup: `apt-get install -y uidmap busybox-static passt nftables`; subuid/subgid lines for `$USER`; `cargo install --git https://github.com/AlfonsoCampodonico/vmkit --rev <rev> --bin vmkit-sandbox --root ~/.local`; vmkit's AppArmor profile for that path (run vmkit's `scripts/install-apparmor.sh` from a checkout at the rev); `VMKIT_SANDBOX` exported.
  - README gets a "Building" section.
  - `docs/plan-format.md` gets the cache key derivation (§13).
- [ ] **Step 4: Run** (macOS: fmt, clippy, all tests; Lima: `sandboxed` and `build`); expect PASS.
- [ ] **Step 5: Commit** `feat(potter): potter build with the namespace executor`.
