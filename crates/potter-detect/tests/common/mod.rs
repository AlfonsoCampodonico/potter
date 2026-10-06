#![allow(dead_code)]

use std::path::{Path, PathBuf};

use potter_detect::{DetectError, DetectOptions, Detection, RepoView};
use potter_plan::{Base, Decision, Step, Target};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

pub fn detect_at(path: &Path, opts: &DetectOptions) -> Result<Detection, DetectError> {
    let repo = RepoView::open(path)?;
    potter_detect::detect(&repo, opts, "app")
}

pub fn detect(name: &str) -> Detection {
    detect_at(&fixture(name), &DetectOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

pub fn detect_err(name: &str) -> DetectError {
    match detect_at(&fixture(name), &DetectOptions::default()) {
        Ok(_) => panic!("{name}: detection should fail"),
        Err(e) => e,
    }
}

pub fn temp_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, c) in files {
        let path = d.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, c).unwrap();
    }
    d
}

pub fn layers(base: &Base) -> Vec<String> {
    match base {
        Base::Catalog { layers } => layers.iter().map(|l| l.to_string()).collect(),
        other => panic!("expected a catalog base, got {other:?}"),
    }
}

pub fn step_names(t: &Target) -> Vec<&str> {
    t.build.steps.iter().map(|s| s.name()).collect()
}

pub fn step<'a>(t: &'a Target, name: &str) -> &'a Step {
    t.build
        .steps
        .iter()
        .find(|s| s.name() == name)
        .unwrap_or_else(|| panic!("no step {name} in {:?}", step_names(t)))
}

pub fn command(t: &Target, name: &str) -> String {
    match step(t, name) {
        Step::Run { command, .. } => command.to_string(),
        other => panic!("{name} is a {} step", other.kind()),
    }
}

pub fn includes(t: &Target, name: &str) -> Vec<String> {
    match step(t, name) {
        Step::Inputs { include, .. } => include.clone(),
        other => panic!("{name} is a {} step", other.kind()),
    }
}

pub fn process(t: &Target, name: &str) -> String {
    t.image
        .processes
        .get(name)
        .unwrap_or_else(|| panic!("no process {name}"))
        .command
        .join(" ")
}

pub fn decision<'a>(d: &'a Detection, field: &str) -> &'a Decision {
    d.decisions
        .iter()
        .find(|x| x.field == field)
        .unwrap_or_else(|| panic!("no decision for {field}: {:#?}", d.decisions))
}
