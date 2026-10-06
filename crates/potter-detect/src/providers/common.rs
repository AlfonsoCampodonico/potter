//! Building blocks shared by providers.

use std::collections::BTreeMap;

use potter_plan::{
    Base, CacheMount, CatalogRef, Chain, Command, Decision, Export, Image, ImageConfig, Origin, Sharing, Step, Target,
};

use crate::catalog::VersionSource;
use crate::options::DetectOptions;
use crate::repo::{READ_CAP, RepoView};
use crate::version::{ConstraintError, parse_version_file};
use crate::{DetectError, ProviderId};

pub(crate) const APP: &str = "/app";

pub(crate) fn inputs(name: &str, include: Vec<String>) -> Step {
    Step::Inputs {
        name: name.into(),
        include,
        exclude: vec![],
        dest: APP.into(),
        owner: "0:0".into(),
    }
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
            .map(|(id, target)| CacheMount {
                id: id.to_string(),
                target: target.to_string(),
                sharing: Sharing::Exclusive,
            })
            .collect(),
        secrets: vec![],
        timeout_secs: None,
    }
}

pub(crate) fn cref(name: &str, version: Option<&str>) -> CatalogRef {
    CatalogRef::new(name, version)
}

pub(crate) fn export(path: &str, dest: &str) -> Export {
    Export {
        path: path.into(),
        dest: dest.into(),
        exclude: vec![],
    }
}

pub(crate) fn new_target(
    name: &str,
    provider: ProviderId,
    build_layers: Vec<CatalogRef>,
    steps: Vec<Step>,
    image: Image,
) -> Target {
    Target {
        name: name.into(),
        root: ".".into(),
        platform: None,
        provider: provider.as_str().into(),
        build: Chain {
            base: Base::Catalog { layers: build_layers },
            steps,
        },
        image,
    }
}

pub(crate) fn empty_image(run_layers: Vec<CatalogRef>, config: ImageConfig) -> Image {
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
    let at = steps
        .iter()
        .position(|s| s.name() == after)
        .map_or(steps.len(), |i| i + 1);
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
    DetectError::BadConstraint {
        what,
        constraint: constraint.into(),
        source_desc: source.into(),
        reason: e.0,
    }
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
    let Some(content) = repo.read_string(rel, READ_CAP)? else {
        return Ok(None);
    };
    let shown = content.trim();
    let (set, lts_only) = parse_version_file(&content).map_err(|e| bad_constraint(what, shown, rel, e))?;
    let desc = format!("{rel} = {}", shown.lines().next().unwrap_or(""));
    Ok(Some(VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only,
    }))
}

/// A repo file named in potter.toml must exist and stay inside the build root.
pub(crate) fn require_repo_file(
    repo: &RepoView,
    rel: &str,
    provider: ProviderId,
    key: &str,
) -> Result<(), DetectError> {
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
