//! Rust applications (spec §6.1 "Rust"). Dependencies build first against stub sources (the
//! cargo-chef technique) so source edits don't rebuild them; binaries land in /out.

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

pub(crate) struct RustProvider;

/// One workspace member (or the root package).
struct Member {
    /// "" for the root, else the member directory.
    dir: String,
    package: String,
    /// (binary name, source path relative to the repo root)
    bins: Vec<(String, String)>,
    lib: Option<String>,
    build_script: Option<String>,
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
        let members = members(repo, &root)?;
        let bins: Vec<&(String, String)> = members.iter().flat_map(|m| &m.bins).collect();
        if bins.is_empty() {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Rust,
                hint: format!(
                    "no binary target (src/main.rs or [[bin]]); set `{}`",
                    opts.key("process.web.cmd")
                ),
            });
        }

        let mut deps_inputs = vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()];
        for m in members.iter().filter(|m| !m.dir.is_empty()) {
            deps_inputs.push(format!("{}/Cargo.toml", m.dir));
        }
        for f in ["rust-toolchain.toml", "rust-toolchain", ".cargo/config.toml"] {
            if repo.is_file(f) {
                deps_inputs.push(f.into());
            }
        }
        let env = [("CARGO_HOME", "/cargo")];
        let cache = [("cargo-registry", "/cargo/registry"), ("cargo-git", "/cargo/git")];
        let stubs = stub_script(&members);
        let deps = if stubs.is_empty() {
            "cargo build --release --locked".to_string()
        } else {
            format!("{stubs} && cargo build --release --locked")
        };
        let packages: Vec<String> = members.iter().map(|m| format!("-p {}", m.package)).collect();
        let bin_paths: Vec<String> = bins.iter().map(|(name, _)| format!("target/release/{name}")).collect();
        let build = format!(
            "cargo clean --release {} && cargo build --release --locked && mkdir -p /out && cp {} /out/",
            packages.join(" "),
            bin_paths.join(" ")
        );
        let steps = vec![
            inputs("deps-inputs", deps_inputs),
            run("deps", Command::Shell(deps), &env, &cache),
            source(),
            run("build", Command::Shell(build), &env, &cache),
        ];

        let root_package = root.get("package").and_then(|p| p.get("name")).and_then(Value::as_str);
        let web = pick_web(&bins, root_package, opts)?;
        d.push(Decision::detected(
            "process.web",
            format!("/out/{web}"),
            [format!("binary target {web}")],
        ));
        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/out", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        let mut image = empty_image(vec![cref("os-min", None)], config);
        image.export = vec![export("/out", "/out")];
        for (name, _) in &bins {
            let process = if *name == web { "web".to_string() } else { name.clone() };
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

fn members(repo: &RepoView, root: &Value) -> Result<Vec<Member>, DetectError> {
    let mut dirs: Vec<String> = Vec::new();
    if root.get("package").is_some() {
        dirs.push(String::new());
    }
    if let Some(list) = root
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(Value::as_array)
    {
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
                    dirs.extend(found);
                }
                None => dirs.push(pattern.trim_end_matches('/').to_string()),
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        if !dir.is_empty() && !safe(&dir) {
            return Err(unsafe_path("Cargo.toml", &dir));
        }
        let manifest = if dir.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{dir}/Cargo.toml")
        };
        let Some(toml) = repo.read_toml(&manifest)? else {
            continue;
        };
        let Some(package) = toml.get("package").and_then(|p| p.get("name")).and_then(Value::as_str) else {
            continue;
        };
        if !package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(unsafe_path(&manifest, package));
        }
        let at = |p: &str| {
            if dir.is_empty() {
                p.to_string()
            } else {
                format!("{dir}/{p}")
            }
        };
        let mut bins = Vec::new();
        if let Some(list) = toml.get("bin").and_then(Value::as_array) {
            for b in list {
                let name = b.get("name").and_then(Value::as_str).unwrap_or(package);
                let path = b
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if name == package {
                            "src/main.rs".into()
                        } else {
                            format!("src/bin/{name}.rs")
                        }
                    });
                if !safe(name) || !safe(&path) {
                    return Err(unsafe_path(&manifest, &format!("{name} = {path}")));
                }
                bins.push((name.to_string(), at(&path)));
            }
        }
        if bins.is_empty() && repo.is_file(&at("src/main.rs")) {
            bins.push((package.to_string(), at("src/main.rs")));
        }
        let lib_path = toml
            .get("lib")
            .and_then(|l| l.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("src/lib.rs");
        let lib = (repo.is_file(&at(lib_path)) && safe(lib_path)).then(|| at(lib_path));
        let build_script = match toml.get("package").and_then(|p| p.get("build")) {
            Some(Value::String(p)) if safe(p) => Some(at(p)),
            Some(Value::Boolean(false)) => None,
            _ => repo.is_file(&at("build.rs")).then(|| at("build.rs")),
        };
        out.push(Member {
            dir,
            package: package.to_string(),
            bins,
            lib,
            build_script,
        });
    }
    Ok(out)
}

/// Shell that writes empty sources in place of every target, so `cargo build` compiles only
/// dependencies. All paths were checked by `safe`.
fn stub_script(members: &[Member]) -> String {
    let mut parts = Vec::new();
    for m in members {
        for (_, path) in &m.bins {
            parts.push(stub(path, "fn main() {}"));
        }
        if let Some(lib) = &m.lib {
            parts.push(stub(lib, ""));
        }
        if let Some(build) = &m.build_script {
            parts.push(stub(build, "fn main() {}"));
        }
    }
    parts.join(" && ")
}

fn stub(path: &str, body: &str) -> String {
    let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
    format!("mkdir -p {dir} && printf '%s\\n' '{body}' > {path}")
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
