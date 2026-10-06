//! potter.toml and the planning pipeline (spec §6.2): load config, detect or build each target,
//! apply overrides, validate.

mod error;
pub mod explicit;
pub mod file;
pub mod merge;
pub mod options;

use std::collections::BTreeSet;
use std::path::Path;

use potter_detect::{DetectOptions, RepoView};
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
    let all = discover_targets(root, &cfg)?;
    if let Some(o) = only
        && !all.iter().any(|(n, _, _)| n == o)
    {
        return Err(ConfigError::UnknownTarget {
            name: o.to_string(),
            available: all.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>().join(", "),
        });
    }
    let mut targets = Vec::new();
    let mut decisions = Vec::new();
    for (name, tc, discovered) in all {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let (t, mut d) = plan_target(root, &name, &tc, &cfg.apt).map_err(|e| match &discovered {
            Some((_, dir)) => ConfigError::InDiscoveredTarget {
                name: name.clone(),
                dir: dir.clone(),
                source: Box::new(e),
            },
            None => ConfigError::InTarget {
                name: name.clone(),
                source: Box::new(e),
            },
        })?;
        if let Some((decision, _)) = discovered {
            d.insert(0, decision);
        }
        targets.push(t);
        decisions.push((name, d));
    }
    let plan = Plan::new(targets);
    potter_plan::validate(&plan)?;
    Ok(PlannedRepo { plan, decisions })
}

/// Directories below the root that become targets when the root itself matches no provider.
const TARGET_CONTAINERS: &[&str] = &["apps", "services"];
/// Depth-1 directories that hold tooling or documentation, never deployable apps.
const AUXILIARY_DIRS: &[&str] = &[
    "bench",
    "benchmarks",
    "doc",
    "docs",
    "e2e",
    "example",
    "examples",
    "fixtures",
    "script",
    "scripts",
    "test",
    "tests",
    "tools",
];

/// (name, config, for discovered targets: the decision and the directory)
type Discovered = (String, TargetConfig, Option<(Decision, String)>);

/// Declared targets, plus (when the root matches no provider and discovery is on) one target per
/// directory below it that does (spec §10). Declared targets override discovered ones of the same
/// name and inherit their root. Without either, the implicit `app` target.
fn discover_targets(root: &Path, cfg: &file::ConfigFile) -> Result<Vec<Discovered>, ConfigError> {
    let declared: Vec<Discovered> = file::targets(cfg).into_iter().map(|(n, t)| (n, t, None)).collect();
    if cfg.discover == Some(false) {
        return Ok(declared);
    }
    // An unreadable root is reported by `plan_target` with the target's name.
    let Ok(repo) = RepoView::open(root) else {
        return Ok(declared);
    };
    if !potter_detect::claims(&repo, &DetectOptions::default())?.is_empty() {
        return Ok(declared);
    }
    let mut dirs: Vec<String> = repo
        .manifest_dirs()
        .into_iter()
        .filter(|d| !d.starts_with('.') && !AUXILIARY_DIRS.contains(&d.as_str()))
        .collect();
    for container in TARGET_CONTAINERS {
        dirs.extend(repo.manifest_dirs_under(container));
    }
    dirs.sort();
    dirs.dedup();
    if dirs.is_empty() {
        return Ok(declared);
    }
    let explicit: Vec<(String, TargetConfig)> = if cfg.target.is_empty() {
        vec![]
    } else {
        file::targets(cfg)
    };
    let declared_roots: BTreeSet<String> = explicit.iter().filter_map(|(_, t)| t.root.clone()).collect();
    let mut used = BTreeSet::new();
    let mut out: Vec<Discovered> = Vec::new();
    for dir in dirs.into_iter().filter(|d| !declared_roots.contains(d)) {
        let base = slug(&dir);
        let mut name = base.clone();
        let mut n = 2;
        while !used.insert(name.clone()) {
            name = format!("{base}-{n}");
            n += 1;
        }
        let decision = Decision::detected(
            "target",
            name.as_str(),
            [format!("{dir}/ has a manifest and the root has none")],
        );
        out.push((
            name,
            TargetConfig {
                root: Some(dir.clone()),
                ..Default::default()
            },
            Some((decision, dir)),
        ));
    }
    for (name, mut tc) in explicit {
        match out.iter_mut().find(|(n, _, _)| *n == name) {
            Some(slot) => {
                if tc.root.is_none() {
                    tc.root = slot.1.root.clone();
                }
                slot.1 = tc;
            }
            None => out.push((name, tc, None)),
        }
    }
    Ok(out)
}

/// A directory path as a target name: lower case, `[a-z0-9._-]` only, at most 60 characters.
fn slug(dir: &str) -> String {
    let s: String = dir
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s: String = s.trim_matches('-').chars().take(60).collect();
    if s.is_empty() { "target".into() } else { s }
}

fn plan_target(
    root: &Path,
    name: &str,
    tc: &TargetConfig,
    apt: &AptConfig,
) -> Result<(Target, Vec<Decision>), ConfigError> {
    let rel = tc.root.clone().unwrap_or_else(|| ".".into());
    if !is_relative_path(&rel) {
        return Err(ConfigError::invalid(
            format!("target.{name}.root"),
            "must be a relative path inside the repository",
        ));
    }
    check_root(root, name, &rel)?;
    let opts = match options::options_for(name, tc)? {
        Mode::Explicit => return explicit::explicit_target(name, &rel, tc, apt),
        Mode::Detect(o) => *o,
    };
    let dir = if rel == "." {
        root.to_path_buf()
    } else {
        root.join(&rel)
    };
    let repo = RepoView::open(&dir).map_err(potter_detect::DetectError::from)?;
    let detection = potter_detect::detect(&repo, &opts, name)?;
    let mut target = detection.target;
    target.root = rel;
    let mut decisions = detection.decisions;
    merge::apply(&mut target, name, tc, apt, &mut decisions)?;
    Ok((target, decisions))
}

/// A target root must be a real directory inside the repository: every component is checked
/// without following symlinks, so a repo-controlled `root` can't lead outside it (P8).
fn check_root(root: &Path, name: &str, rel: &str) -> Result<(), ConfigError> {
    let key = format!("target.{name}.root");
    let mut path = root.to_path_buf();
    for component in rel.split('/').filter(|c| !c.is_empty() && *c != ".") {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(ConfigError::invalid(
                    key,
                    format!("`{rel}` passes through a symlink ({component})"),
                ));
            }
            Ok(m) if !m.is_dir() => {
                return Err(ConfigError::invalid(key, format!("`{rel}` is not a directory")));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConfigError::invalid(key, format!("`{rel}` does not exist")));
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }
    Ok(())
}
