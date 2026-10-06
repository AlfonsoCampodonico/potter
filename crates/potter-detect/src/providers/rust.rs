//! Rust applications (spec §6.1 "Rust"). Dependencies build first against stub sources (the
//! cargo-chef technique) so source edits don't rebuild them; binaries land in /out.
//!
//! Target discovery follows cargo: declared `[[bin]]`/`[[bench]]`/`[[example]]`/`[[test]]`,
//! auto-discovered `src/main.rs` and `src/bin/*`, the library, the build script, and local path
//! dependencies. Every file cargo needs to parse a manifest gets a stub.

use std::collections::BTreeSet;

use potter_plan::{Command, Decision, ImageConfig, Origin, Process};
use toml::Value;

use crate::catalog::{self, Line, RUST, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, at_least, parse_partial, release_prefix};
use crate::{DetectError, ProviderId};

const WEB_NAMES: &[&str] = &["server", "api", "web", "app"];
const DEP_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

pub(crate) struct RustProvider;

/// One package: a workspace member, the root package, or a local path dependency.
struct Package {
    /// "" for the root, else the package directory.
    dir: String,
    name: String,
    /// Binaries that ship (workspace members only): (name, source path).
    bins: Vec<(String, String)>,
    /// Files cargo must find to parse the manifest and build the dependency graph.
    stubs: Vec<(String, &'static str)>,
}

impl Provider for RustProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Rust
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(repo.is_file("Cargo.toml").then(|| Claim {
            provider: ProviderId::Rust,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec!["Cargo.toml".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let root = repo.read_toml("Cargo.toml")?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Rust,
            file: "Cargo.toml".into(),
            hint: "the Rust provider builds Cargo projects".into(),
        })?;
        if !repo.is_file("Cargo.lock") {
            return Err(DetectError::MissingFile {
                provider: ProviderId::Rust,
                file: "Cargo.lock".into(),
                hint: "commit Cargo.lock so builds are reproducible".into(),
            });
        }
        let mut d = Vec::new();
        let rust = rust_version(repo, opts, &root, &mut d)?;
        let packages = packages(repo, &root)?;
        let bins: Vec<&(String, String)> = packages.iter().flat_map(|p| &p.bins).collect();
        if bins.is_empty() && opts.web_cmd.is_none() {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Rust,
                hint: "no binary target (src/main.rs, src/bin/* or [[bin]]); add one, or use provider = \"none\""
                    .into(),
            });
        }

        let mut deps_inputs = vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()];
        for p in packages.iter().filter(|p| !p.dir.is_empty()) {
            deps_inputs.push(format!("{}/Cargo.toml", p.dir));
        }
        for f in ["rust-toolchain.toml", "rust-toolchain", ".cargo/config.toml"] {
            if repo.is_file(f) {
                deps_inputs.push(f.into());
            }
        }
        let env = [("CARGO_HOME", "/cargo")];
        let cache = [("cargo-registry", "/cargo/registry"), ("cargo-git", "/cargo/git")];
        let stubs = stub_script(&packages);
        let deps = if stubs.is_empty() {
            "cargo build --release --locked".to_string()
        } else {
            format!("{stubs} && cargo build --release --locked")
        };
        let clean: Vec<String> = packages.iter().map(|p| format!("-p {}", p.name)).collect();
        let mut build = format!(
            "cargo clean --release {} && cargo build --release --locked",
            clean.join(" ")
        );
        if !bins.is_empty() {
            let paths: Vec<String> = bins.iter().map(|(name, _)| format!("target/release/{name}")).collect();
            build.push_str(&format!(" && mkdir -p /out && cp {} /out/", paths.join(" ")));
        }
        let steps = vec![
            inputs("deps-inputs", deps_inputs),
            run("deps", Command::Shell(deps), &env, &cache),
            source(),
            run("build", Command::Shell(build), &env, &cache),
        ];

        let root_package = root.get("package").and_then(|p| p.get("name")).and_then(Value::as_str);
        let web = match opts.web_cmd {
            Some(_) => None,
            None => Some(pick_web(&bins, root_package, opts)?),
        };
        if let Some(web) = &web {
            d.push(Decision::detected(
                "process.web",
                format!("/out/{web}"),
                [format!("binary target {web}")],
            ));
        }
        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/out", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        let mut image = empty_image(vec![cref("os-min", None)], config);
        image.export = vec![export("/out", "/out")];
        for (name, _) in &bins {
            let process = if web.as_deref() == Some(name.as_str()) {
                "web".to_string()
            } else {
                name.clone()
            };
            if potter_plan::is_name(&process) {
                image
                    .processes
                    .insert(process, Process::new(&[&format!("/out/{name}")]));
            }
        }
        let layers = vec![cref("os-min", None), cref("rust", Some(rust.version))];
        Ok(Detection {
            target: new_target(target, ProviderId::Rust, layers, steps, image),
            decisions: d,
        })
    }
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only: false,
    }
}

fn rust_version(
    repo: &RepoView,
    opts: &DetectOptions,
    root: &Value,
    d: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.rust.as_deref(), &opts.key("runtime.rust"), "rust")?;
    if src.is_none() {
        let channel = match repo.read_toml("rust-toolchain.toml")? {
            Some(t) => t
                .get("toolchain")
                .and_then(|t| t.get("channel"))
                .and_then(Value::as_str)
                .map(str::to_string),
            None => repo
                .read_string("rust-toolchain", READ_CAP)?
                .map(|s| s.trim().to_string()),
        };
        match channel.as_deref() {
            None | Some("stable") => {}
            Some(c) if c.starts_with("nightly") || c.starts_with("beta") => {
                return Err(DetectError::NeedsConfig {
                    provider: ProviderId::Rust,
                    message: format!("the `{c}` toolchain isn't in the catalog; pin a stable version"),
                    key: opts.key("runtime.rust"),
                });
            }
            Some(c) => {
                let set = parse_partial(release_prefix(c))
                    .and_then(|p| p.x_range())
                    .map_err(|e| bad_constraint("rust", c, "rust-toolchain channel", e))?;
                src = Some(detected(format!("rust-toolchain channel = {c}"), set));
            }
        }
    }
    if src.is_none() {
        let rv = root
            .get("package")
            .and_then(|p| p.get("rust-version"))
            .or_else(|| root.get("workspace")?.get("package")?.get("rust-version"))
            .and_then(Value::as_str);
        if let Some(v) = rv {
            let set = at_least(v).map_err(|e| bad_constraint("rust", v, "Cargo.toml rust-version", e))?;
            src = Some(detected(format!("Cargo.toml rust-version = {v} (minimum)"), set));
        }
    }
    catalog::pick(&RUST, "runtime.rust", src, d)
}

/// Paths and names that end up in shell commands: a conservative character set only.
fn safe(s: &str) -> bool {
    !s.is_empty()
        && potter_plan::is_relative_path(s)
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

fn unsafe_path(file: &str, what: &str) -> DetectError {
    DetectError::Parse {
        file: file.into(),
        reason: format!("`{what}` must be a plain relative path or name"),
    }
}

fn join(dir: &str, path: &str) -> String {
    if dir.is_empty() {
        path.to_string()
    } else {
        format!("{dir}/{path}")
    }
}

/// `base/rel` with `.` and `..` resolved; `None` if it leaves the build root.
fn normalize(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// Workspace members (with globs and `exclude`), the root package, then local path dependencies.
fn packages(repo: &RepoView, root: &Value) -> Result<Vec<Package>, DetectError> {
    let mut member_dirs: Vec<String> = Vec::new();
    if root.get("package").is_some() {
        member_dirs.push(String::new());
    }
    let workspace = root.get("workspace");
    let exclude: BTreeSet<String> = workspace
        .and_then(|w| w.get("exclude"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim_end_matches('/').to_string())
                .collect()
        })
        .unwrap_or_default();
    if let Some(list) = workspace.and_then(|w| w.get("members")).and_then(Value::as_array) {
        for pattern in list.iter().filter_map(Value::as_str) {
            match pattern.strip_suffix("/*") {
                Some(prefix) => {
                    let mut found: Vec<String> = repo
                        .files()
                        .filter_map(|f| f.strip_prefix(prefix)?.strip_prefix('/')?.strip_suffix("/Cargo.toml"))
                        .filter(|name| !name.contains('/'))
                        .map(|name| format!("{prefix}/{name}"))
                        .collect();
                    found.sort();
                    member_dirs.extend(found);
                }
                None => member_dirs.push(pattern.trim_end_matches('/').to_string()),
            }
        }
    }
    member_dirs.retain(|d| !exclude.contains(d));

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    let mut queue: Vec<(String, bool)> = member_dirs.into_iter().map(|d| (d, true)).collect();
    let mut i = 0;
    while i < queue.len() {
        let (dir, member) = queue[i].clone();
        i += 1;
        if !seen.insert(dir.clone()) {
            continue;
        }
        if !dir.is_empty() && !safe(&dir) {
            return Err(unsafe_path("Cargo.toml", &dir));
        }
        let manifest = join(&dir, "Cargo.toml");
        let Some(toml) = repo.read_toml(&manifest)? else {
            continue;
        };
        let mut dep_tables: Vec<&Value> = DEP_TABLES.iter().filter_map(|t| toml.get(*t)).collect();
        if dir.is_empty()
            && let Some(ws_deps) = toml.get("workspace").and_then(|w| w.get("dependencies"))
        {
            dep_tables.push(ws_deps);
        }
        for table in dep_tables.iter().filter_map(|t| t.as_table()) {
            for spec in table.values() {
                if let Some(path) = spec.get("path").and_then(Value::as_str) {
                    let resolved = normalize(&dir, path).ok_or_else(|| unsafe_path(&manifest, path))?;
                    if !safe(&resolved) {
                        return Err(unsafe_path(&manifest, path));
                    }
                    queue.push((resolved, false));
                }
            }
        }
        let Some(name) = toml.get("package").and_then(|p| p.get("name")).and_then(Value::as_str) else {
            continue;
        };
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(unsafe_path(&manifest, name));
        }
        out.push(package(repo, &dir, name, &toml, &manifest, member)?);
    }
    Ok(out)
}

fn package(
    repo: &RepoView,
    dir: &str,
    name: &str,
    toml: &Value,
    manifest: &str,
    member: bool,
) -> Result<Package, DetectError> {
    let at = |p: &str| join(dir, p);
    let exists = |p: &str| repo.is_file(&at(p));
    let checked = |p: String| {
        if safe(&p) {
            Ok(p)
        } else {
            Err(unsafe_path(manifest, &p))
        }
    };
    let mut bins: Vec<(String, String)> = Vec::new();
    let mut stubs: Vec<(String, &'static str)> = Vec::new();

    // Declared binaries: explicit path, or cargo's default locations in order.
    for b in toml.get("bin").and_then(Value::as_array).into_iter().flatten() {
        let bin = b.get("name").and_then(Value::as_str).unwrap_or(name);
        if !safe(bin) {
            return Err(unsafe_path(manifest, bin));
        }
        let path = match b.get("path").and_then(Value::as_str) {
            Some(p) => checked(p.to_string())?,
            None if bin == name && exists("src/main.rs") => "src/main.rs".to_string(),
            None if exists(&format!("src/bin/{bin}/main.rs")) => format!("src/bin/{bin}/main.rs"),
            None => format!("src/bin/{bin}.rs"),
        };
        bins.push((bin.to_string(), at(&path)));
    }
    // Auto-discovered binaries, unless `autobins = false`.
    let autobins = toml
        .get("package")
        .and_then(|p| p.get("autobins"))
        .and_then(Value::as_bool)
        != Some(false);
    if autobins {
        let claimed: BTreeSet<String> = bins.iter().map(|(_, p)| p.clone()).collect();
        let named: BTreeSet<String> = bins.iter().map(|(n, _)| n.clone()).collect();
        let mut auto: Vec<(String, String)> = Vec::new();
        if exists("src/main.rs") && !claimed.contains(&at("src/main.rs")) && !named.contains(name) {
            auto.push((name.to_string(), at("src/main.rs")));
        }
        let bin_dir = at("src/bin");
        for f in repo.files() {
            let Some(rest) = f.strip_prefix(&bin_dir).and_then(|r| r.strip_prefix('/')) else {
                continue;
            };
            let found = match rest.split_once('/') {
                None => rest.strip_suffix(".rs").map(|stem| (stem.to_string(), f.to_string())),
                Some((sub, "main.rs")) => Some((sub.to_string(), f.to_string())),
                Some(_) => None,
            };
            if let Some((bin, path)) = found
                && !named.contains(&bin)
                && !claimed.contains(&path)
            {
                if !safe(&bin) || !safe(&path) {
                    return Err(unsafe_path(manifest, &path));
                }
                auto.push((bin, path));
            }
        }
        bins.extend(auto);
    }
    for (_, path) in &bins {
        stubs.push((path.clone(), "fn main() {}"));
    }

    let lib_path = toml
        .get("lib")
        .and_then(|l| l.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("src/lib.rs");
    if exists(lib_path) || toml.get("lib").and_then(|l| l.get("path")).is_some() {
        stubs.push((checked(at(lib_path))?, ""));
    }
    match toml.get("package").and_then(|p| p.get("build")) {
        Some(Value::String(p)) => stubs.push((checked(at(p))?, "fn main() {}")),
        Some(Value::Boolean(false)) => {}
        _ if exists("build.rs") => stubs.push((at("build.rs"), "fn main() {}")),
        _ => {}
    }
    // Declared benches, examples and tests must exist for cargo to parse the manifest.
    for (key, folder) in [("bench", "benches"), ("example", "examples"), ("test", "tests")] {
        for t in toml.get(key).and_then(Value::as_array).into_iter().flatten() {
            let Some(target) = t.get("name").and_then(Value::as_str) else {
                continue;
            };
            let path = match t.get("path").and_then(Value::as_str) {
                Some(p) => p.to_string(),
                None if exists(&format!("{folder}/{target}/main.rs")) => format!("{folder}/{target}/main.rs"),
                None => format!("{folder}/{target}.rs"),
            };
            stubs.push((checked(at(&path))?, "fn main() {}"));
        }
    }
    Ok(Package {
        dir: dir.to_string(),
        name: name.to_string(),
        bins: if member { bins } else { Vec::new() },
        stubs,
    })
}

/// Shell that writes empty sources in place of every target, so `cargo build` compiles only
/// dependencies. All paths were checked by `safe`.
fn stub_script(packages: &[Package]) -> String {
    let mut parts = Vec::new();
    for p in packages {
        for (path, body) in &p.stubs {
            let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
            parts.push(format!("mkdir -p {dir} && printf '%s\\n' '{body}' > {path}"));
        }
    }
    parts.join(" && ")
}

fn pick_web(
    bins: &[&(String, String)],
    root_package: Option<&str>,
    opts: &DetectOptions,
) -> Result<String, DetectError> {
    if let [(only, _)] = bins {
        return Ok(only.clone());
    }
    let preferred = root_package.into_iter().chain(WEB_NAMES.iter().copied());
    for want in preferred {
        if let Some((name, _)) = bins.iter().find(|(n, _)| n == want) {
            return Ok(name.clone());
        }
    }
    Err(DetectError::NeedsConfig {
        provider: ProviderId::Rust,
        message: format!(
            "several binaries ({}) and none matches the package or is named server, api, web or app",
            bins.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
        ),
        key: opts.key("process.web.cmd"),
    })
}
