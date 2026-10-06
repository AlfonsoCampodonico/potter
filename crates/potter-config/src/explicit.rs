//! `provider = "none"`: a plan written entirely in potter.toml (spec §6.2). The chain starts
//! with a `source` inputs step, then the declared steps in file order.

use std::collections::BTreeMap;

use potter_plan::{Base, CatalogRef, Chain, Command, Decision, Export, Image, ImageConfig, Process, Target};

use crate::error::ConfigError;
use crate::file::{AptConfig, TargetConfig};
use crate::merge::{apply_apt, apply_commands, export_entry, inputs_step, run_step};

fn parse_ref(s: &str) -> CatalogRef {
    match s.split_once('@') {
        Some((name, version)) => CatalogRef::new(name, Some(version)),
        None => CatalogRef::new(s, None),
    }
}

fn base_from(catalog: Option<&Vec<String>>, oci: Option<&String>, key: &str) -> Result<Option<Base>, ConfigError> {
    match (catalog, oci) {
        (Some(c), None) => Ok(Some(Base::Catalog {
            layers: c.iter().map(|s| parse_ref(s)).collect(),
        })),
        (None, Some(o)) => Ok(Some(Base::Oci {
            reference: o.clone(),
            digest: None,
        })),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(ConfigError::invalid(
            key,
            "set either a catalog list or an oci reference, not both",
        )),
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
    let run_base =
        base_from(base.run_catalog.as_ref(), base.run_oci.as_ref(), &base_key)?.unwrap_or_else(|| build_base.clone());

    let mut decisions = vec![Decision::config("provider", "none", format!("{prefix}.provider"))];
    let mut steps = vec![inputs_step("source", &["**".to_string()])];
    for (step_name, sc) in &tc.steps {
        let key = format!("{prefix}.steps.{step_name}");
        let run = sc
            .run
            .as_ref()
            .ok_or_else(|| ConfigError::invalid(&key, "explicit steps need `run`"))?;
        if sc.skip || sc.after.is_some() {
            return Err(ConfigError::invalid(
                &key,
                "`skip` and `after` don't apply to explicit plans; steps run in file order",
            ));
        }
        if let Some(inc) = &sc.inputs {
            steps.push(inputs_step(&format!("{step_name}-inputs"), inc));
        }
        steps.push(run_step(step_name, Command::Shell(run.clone()), sc, &key)?);
        decisions.push(Decision::config(&format!("steps.{step_name}"), run.as_str(), key));
    }
    if tc.process.is_empty() {
        return Err(ConfigError::invalid(
            format!("{prefix}.process"),
            "explicit plans need at least one [process.*] entry",
        ));
    }
    let mut processes = BTreeMap::new();
    for (pname, pc) in &tc.process {
        if pc.cmd.first().is_none_or(|c| c.trim().is_empty()) {
            return Err(ConfigError::invalid(
                format!("{prefix}.process.{pname}.cmd"),
                "cmd must not be empty",
            ));
        }
        processes.insert(
            pname.clone(),
            Process {
                command: pc.cmd.clone(),
                workdir: None,
                env: Default::default(),
            },
        );
    }
    let export_key = format!("{prefix}.export");
    let export = match &tc.export {
        Some(list) => list
            .iter()
            .map(|e| export_entry(e, "/app", &export_key))
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![Export {
            path: "/app".into(),
            dest: "/app".into(),
            exclude: vec![],
        }],
    };
    let mut config = ImageConfig::new("/app", "root");
    config.port = tc.port;
    let mut target = Target {
        name: name.into(),
        root: root.into(),
        platform: None,
        provider: "none".into(),
        build: Chain {
            base: build_base,
            steps,
        },
        image: Image {
            base: run_base,
            runtime_steps: vec![],
            export,
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
