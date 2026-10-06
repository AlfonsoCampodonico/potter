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
        let (t, d) = plan_target(root, &name, &tc, &cfg.apt).map_err(|e| ConfigError::InTarget {
            name: name.clone(),
            source: Box::new(e),
        })?;
        targets.push(t);
        decisions.push((name, d));
    }
    let plan = Plan::new(targets);
    potter_plan::validate(&plan)?;
    Ok(PlannedRepo { plan, decisions })
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
