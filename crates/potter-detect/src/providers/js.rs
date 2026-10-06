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
    if serves {
        ClaimKind::JsServer
    } else {
        ClaimKind::JsAssets
    }
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
        [Kind::Npm, Kind::Pnpm, Kind::Yarn, Kind::Bun]
            .into_iter()
            .find(|k| k.as_str() == s)
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
        v.extend(
            self.kind()
                .lockfiles()
                .iter()
                .filter(|f| repo.is_file(f))
                .map(|f| f.to_string()),
        );
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
        return if version.starts_with("1.") {
            Pm::YarnClassic
        } else {
            Pm::YarnBerry
        };
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
        hint: format!(
            "{} is the package manager but its lockfile is missing; commit it so builds are reproducible",
            k.as_str()
        ),
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
        decisions.push(Decision::detected(
            "node.package_manager",
            k.as_str(),
            [format!("package.json packageManager = {field}")],
        ));
        return Ok(to_pm(k));
    }
    let present: Vec<(Kind, &str)> = [Kind::Pnpm, Kind::Yarn, Kind::Npm, Kind::Bun]
        .into_iter()
        .filter_map(|k| k.present(repo).map(|f| (k, f)))
        .collect();
    match present.as_slice() {
        [] => Err(DetectError::MissingFile {
            provider,
            file: "a lockfile".into(),
            hint: "commit package-lock.json, pnpm-lock.yaml, yarn.lock or bun.lock so builds are reproducible".into(),
        }),
        [(k, file)] => {
            decisions.push(Decision::detected(
                "node.package_manager",
                k.as_str(),
                [format!("{file} is present")],
            ));
            Ok(to_pm(*k))
        }
        many => {
            let chosen = if many.iter().any(|(k, _)| *k == Kind::Npm) {
                Kind::Npm
            } else {
                many[0].0
            };
            let files: Vec<&str> = many.iter().map(|(_, f)| *f).collect();
            decisions.push(Decision::detected(
                "node.package_manager",
                chosen.as_str(),
                [format!(
                    "several lockfiles ({}); chose {}; set {key} to change",
                    files.join(", "),
                    chosen.as_str()
                )],
            ));
            Ok(to_pm(chosen))
        }
    }
}

pub(crate) fn deps_steps(pm: Pm, repo: &RepoView, prefix: &str) -> Vec<Step> {
    let (id, target, env) = pm.cache();
    vec![
        inputs(&format!("{prefix}deps-inputs"), pm.lock_inputs(repo)),
        run(
            &format!("{prefix}deps"),
            pm.install(),
            &[(env, target)],
            &[(id, target)],
        ),
    ]
}

fn npm_range_source(range: &str, desc: String) -> Result<VersionSource, DetectError> {
    let set = parse_npm_range(range).map_err(|e| bad_constraint("version", range, &desc, e))?;
    Ok(VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only: false,
    })
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
        let set = parse_partial(&version)
            .and_then(|p| p.x_range())
            .map_err(|e| bad_constraint("bun", &version, &desc, e))?;
        src = Some(VersionSource {
            desc: desc.clone(),
            origin: Origin::Detected { evidence: vec![desc] },
            set,
            lts_only: false,
        });
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
    let Some(pkg) = PackageJson::read(repo)? else {
        return Ok(None);
    };
    if pkg.script("build").is_none() {
        decisions.push(Decision::detected(
            "assets",
            "none",
            ["package.json has no build script"],
        ));
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
