//! Applying potter.toml overrides onto detector output (spec §6.2, §18).

use potter_plan::{CacheMount, Command, Decision, Export, Process, Sharing, Step, Target, is_name, is_relative_path};

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
    Step::Inputs {
        name: name.into(),
        include: include.to_vec(),
        exclude: vec![],
        dest: "/app".into(),
        owner: "0:0".into(),
    }
}

fn cache_mounts(ids: &[String], key: &str) -> Result<Vec<CacheMount>, ConfigError> {
    ids.iter()
        .map(|id| {
            if is_name(id) {
                Ok(CacheMount {
                    id: id.clone(),
                    target: format!("/cache/{id}"),
                    sharing: Sharing::Exclusive,
                })
            } else {
                Err(ConfigError::invalid(
                    format!("{key}.cache"),
                    format!("cache id `{id}` must match [a-z0-9._-]{{1,64}}"),
                ))
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
    apply_exports(target, &prefix, tc, decisions)?;
    Ok(())
}

fn apply_steps(
    target: &mut Target,
    prefix: &str,
    tc: &TargetConfig,
    decisions: &mut Vec<Decision>,
) -> Result<(), ConfigError> {
    // New steps that share an `after` anchor keep their potter.toml order.
    let mut last_inserted: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (step_name, sc) in &tc.steps {
        let key = format!("{prefix}.steps.{step_name}");
        let steps = &mut target.build.steps;
        let existing = steps
            .iter()
            .position(|s| s.name() == step_name && matches!(s, Step::Run { .. }));
        let inputs_name = format!("{step_name}-inputs");
        match existing {
            None => {
                let Some(after) = &sc.after else {
                    let available: Vec<&str> = steps
                        .iter()
                        .filter(|s| matches!(s, Step::Run { .. }))
                        .map(|s| s.name())
                        .collect();
                    return Err(ConfigError::invalid(
                        key,
                        format!(
                            "no step named `{step_name}` (steps: {}); add `after = \"…\"` to insert a new one",
                            available.join(", ")
                        ),
                    ));
                };
                let Some(run) = &sc.run else {
                    return Err(ConfigError::invalid(key, "a new step needs `run`"));
                };
                if sc.skip {
                    return Err(ConfigError::invalid(
                        format!("{key}.skip"),
                        "`skip` only applies to existing steps",
                    ));
                }
                position(steps, after)
                    .ok_or_else(|| ConfigError::invalid(format!("{key}.after"), format!("no step named `{after}`")))?;
                let anchor = last_inserted.get(after).map_or(after.as_str(), String::as_str);
                let at = position(steps, anchor).expect("the anchor or a step inserted after it exists");
                last_inserted.insert(after.clone(), step_name.clone());
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
                    return Err(ConfigError::invalid(
                        format!("{key}.after"),
                        "`after` only applies to new steps",
                    ));
                }
                if sc.skip {
                    steps.remove(i);
                    if let Some(j) = position(steps, &inputs_name) {
                        steps.remove(j);
                    }
                    decisions.push(Decision::config(
                        &format!("steps.{step_name}"),
                        "skipped",
                        format!("{key}.skip"),
                    ));
                    continue;
                }
                let mounts = match &sc.cache {
                    Some(ids) => Some(cache_mounts(ids, &key)?),
                    None => None,
                };
                if let Step::Run {
                    command,
                    network,
                    cache_mounts,
                    ..
                } = &mut steps[i]
                {
                    if let Some(run) = &sc.run {
                        *command = Command::Shell(run.clone());
                        decisions.push(Decision::config(
                            &format!("steps.{step_name}.command"),
                            run.as_str(),
                            format!("{key}.run"),
                        ));
                    }
                    if let Some(n) = sc.network {
                        *network = n;
                        decisions.push(Decision::config(
                            &format!("steps.{step_name}.network"),
                            n.to_string(),
                            format!("{key}.network"),
                        ));
                    }
                    if let Some(m) = mounts {
                        *cache_mounts = m;
                        decisions.push(Decision::config(
                            &format!("steps.{step_name}.cache"),
                            sc.cache.as_ref().map(|c| c.join(", ")).unwrap_or_default(),
                            format!("{key}.cache"),
                        ));
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
                    decisions.push(Decision::config(
                        &format!("steps.{step_name}.inputs"),
                        inc.join(" "),
                        format!("{key}.inputs"),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn apply_processes(
    target: &mut Target,
    prefix: &str,
    tc: &TargetConfig,
    decisions: &mut Vec<Decision>,
) -> Result<(), ConfigError> {
    for (pname, pc) in &tc.process {
        let key = format!("{prefix}.process.{pname}.cmd");
        if pc.cmd.first().is_none_or(|c| c.trim().is_empty()) {
            return Err(ConfigError::invalid(key, "cmd must not be empty"));
        }
        target.image.processes.insert(
            pname.clone(),
            Process {
                command: pc.cmd.clone(),
                workdir: None,
                env: Default::default(),
            },
        );
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
        decisions.push(Decision::config(
            &format!("steps.{name}"),
            c.as_str(),
            format!("{prefix}.build.commands"),
        ));
    }
    if !tc.deploy.commands.is_empty() {
        let script = tc.deploy.commands.join(" && ");
        target.image.processes.insert(
            "deploy".into(),
            Process {
                command: vec!["sh".into(), "-c".into(), script.clone()],
                workdir: None,
                env: Default::default(),
            },
        );
        decisions.push(Decision::config(
            "process.deploy",
            script,
            format!("{prefix}.deploy.commands"),
        ));
    }
}

pub(crate) fn apply_apt(target: &mut Target, apt: &AptConfig, decisions: &mut Vec<Decision>) {
    let snapshot = apt.snapshot.clone().unwrap_or_else(|| DEFAULT_APT_SNAPSHOT.to_string());
    if !apt.build.is_empty() {
        target.build.steps.insert(
            0,
            Step::Apt {
                name: "apt-build".into(),
                packages: apt.build.clone(),
                snapshot: snapshot.clone(),
            },
        );
        decisions.push(Decision::config("steps.apt-build", apt.build.join(" "), "apt.build"));
    }
    if !apt.runtime.is_empty() {
        target.image.runtime_steps.push(Step::Apt {
            name: "apt-runtime".into(),
            packages: apt.runtime.clone(),
            snapshot,
        });
        decisions.push(Decision::config(
            "runtime_steps.apt-runtime",
            apt.runtime.join(" "),
            "apt.runtime",
        ));
    }
}

/// One `export` entry: relative paths come from the build's `/app` and land under the image workdir.
pub(crate) fn export_entry(entry: &str, workdir: &str, key: &str) -> Result<Export, ConfigError> {
    if entry.starts_with('/') {
        return Ok(Export {
            path: entry.into(),
            dest: entry.into(),
            exclude: vec![],
        });
    }
    if entry == "." || !is_relative_path(entry) {
        return Err(ConfigError::invalid(
            key,
            format!("`{entry}` must be a path inside the target root or an absolute path"),
        ));
    }
    let base = workdir.trim_end_matches('/');
    Ok(Export {
        path: format!("/app/{entry}"),
        dest: format!("{base}/{entry}"),
        exclude: vec![],
    })
}

fn apply_exports(
    target: &mut Target,
    prefix: &str,
    tc: &TargetConfig,
    decisions: &mut Vec<Decision>,
) -> Result<(), ConfigError> {
    let key = format!("{prefix}.export");
    for entry in tc.export.iter().flatten() {
        let export = export_entry(entry, &target.image.config.workdir, &key)?;
        target.image.export.push(export);
        decisions.push(Decision::config("export", entry.as_str(), key.clone()));
    }
    Ok(())
}
