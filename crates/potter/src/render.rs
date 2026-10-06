//! Human-readable output. Callers sanitise the whole string before printing.

use std::fmt::Write;

use potter_config::PlannedRepo;
use potter_detect::{Claim, ClaimKind, ProviderId};
use potter_plan::{Base, Step};

fn base(b: &Base) -> String {
    match b {
        Base::Catalog { layers } => layers.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", "),
        Base::Oci {
            reference,
            digest: Some(d),
        } => format!("{reference}@{d}"),
        Base::Oci {
            reference,
            digest: None,
        } => reference.clone(),
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
            let exclude = if e.exclude.is_empty() {
                String::new()
            } else {
                format!(" (exclude {})", e.exclude.join(", "))
            };
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
                .map(|v| {
                    format!(
                        "  (+ {})",
                        v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ")
                    )
                })
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
        writeln!(
            s,
            "{:<10} {:<10} {:<8} {}",
            c.provider.as_str(),
            kind(c.kind),
            refines,
            c.evidence.join("; ")
        )
        .unwrap();
    }
    if let Some(id) = chosen {
        writeln!(s, "chosen: {id}").unwrap();
    }
    s
}
