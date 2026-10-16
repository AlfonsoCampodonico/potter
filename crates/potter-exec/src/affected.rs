//! `--affected <git-ref>` (spec §10): which targets differ from a ref.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use potter_plan::{CopySource, Plan, Step, Target};

use crate::error::BuildError;
use crate::inputs::{InputsSpec, build_inputs};

/// SHA-256 of what is written.
struct Hashing(kiln_store::Hasher);

impl Write for Hashing {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The digest of each inputs layer of `t` built from `repo` (owners and times fixed, so only
/// the files count); `None` when the tree cannot be read (a removed root, say).
fn inputs_digests(t: &Target, repo: &Path) -> Option<Vec<String>> {
    let root = repo.join(&t.root);
    let mut out = Vec::new();
    for step in &t.build.steps {
        if let Step::Inputs {
            include, exclude, dest, ..
        } = step
        {
            let spec = InputsSpec {
                root: &root,
                include,
                exclude,
                dest,
                owner: (0, 0),
                epoch: 0,
            };
            let (h, _) = build_inputs(&spec, Hashing(kiln_store::Hasher::new())).ok()?;
            out.push(h.0.finish().to_string());
        }
    }
    Some(out)
}

/// The targets of `now` (planned from `now_root`) that differ from `then` (planned from
/// `then_root`, the ref's tree): a new target, a changed plan, a changed inputs layer, or a
/// copy from an affected target.
pub fn affected(now: &Plan, now_root: &Path, then: &Plan, then_root: &Path) -> Result<BTreeSet<String>, BuildError> {
    let old: BTreeMap<&str, &Target> = then.targets.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut out = BTreeSet::new();
    for t in &now.targets {
        let changed = match old.get(t.name.as_str()) {
            None => true,
            Some(o) => {
                let hash = |t: &Target| potter_plan::hash_of(t).map_err(|e| BuildError::Plan(e.to_string()));
                hash(t)? != hash(o)? || {
                    let now = inputs_digests(t, now_root);
                    now.is_none() || now != inputs_digests(o, then_root)
                }
            }
        };
        if changed {
            out.insert(t.name.clone());
        }
    }
    // A target that copies from an affected one is affected too.
    loop {
        let more: Vec<String> = now
            .targets
            .iter()
            .filter(|t| !out.contains(&t.name))
            .filter(|t| {
                t.build
                    .steps
                    .iter()
                    .any(|s| matches!(s, Step::CopyFrom { from: CopySource::Target(src), .. } if out.contains(src)))
            })
            .map(|t| t.name.clone())
            .collect();
        if more.is_empty() {
            return Ok(out);
        }
        out.extend(more);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use potter_plan::*;

    use super::*;

    fn target(name: &str, root: &str, extra: Vec<Step>) -> Target {
        let base = Base::Oci {
            reference: "x:1".into(),
            digest: None,
        };
        let mut steps = vec![Step::Inputs {
            name: "source".into(),
            include: vec!["**".into()],
            exclude: vec![],
            dest: "/app".into(),
            owner: "0:0".into(),
        }];
        steps.extend(extra);
        Target {
            name: name.into(),
            root: root.into(),
            platform: None,
            provider: "none".into(),
            build: Chain {
                base: base.clone(),
                steps,
            },
            image: Image {
                base,
                runtime_steps: vec![],
                export: vec![],
                files: vec![],
                config: ImageConfig::new("/", "0:0"),
                processes: BTreeMap::new(),
                services: BTreeMap::new(),
                max_layers: None,
            },
        }
    }

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let path = d.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        d
    }

    fn copy_from(source: &str) -> Step {
        Step::CopyFrom {
            name: "assets".into(),
            from: CopySource::Target(source.into()),
            paths: vec!["/app".into()],
            dest: "/srv".into(),
            owner: "0:0".into(),
            mode: None,
        }
    }

    fn plan(targets: Vec<Target>) -> Plan {
        Plan::new(targets)
    }

    fn run(now: &Path, then: &Path, now_plan: &Plan, then_plan: &Plan) -> Vec<String> {
        affected(now_plan, now, then_plan, then).unwrap().into_iter().collect()
    }

    #[test]
    fn only_targets_whose_inputs_or_plan_changed_are_affected_and_copies_follow() {
        let then = tree(&[("web/index.js", "a"), ("api/main.go", "a")]);
        let now = tree(&[("web/index.js", "a"), ("api/main.go", "b")]);
        let p = plan(vec![
            target("web", "web", vec![]),
            target("api", "api", vec![]),
            target("site", "web", vec![copy_from("api")]),
        ]);
        assert_eq!(run(now.path(), then.path(), &p, &p), ["api", "site"]);
        let same = tree(&[("web/index.js", "a"), ("api/main.go", "a")]);
        assert!(run(same.path(), then.path(), &p, &p).is_empty());
        // A plan change alone affects the target.
        let mut changed = p.clone();
        if let Step::Inputs { exclude, .. } = &mut changed.targets[0].build.steps[0] {
            exclude.push("*.md".into());
        }
        assert_eq!(run(same.path(), then.path(), &changed, &p), ["web"]);
        // A target the ref did not have is affected.
        let mut grown = p.clone();
        grown.targets.push(target("new", "web", vec![]));
        assert_eq!(run(same.path(), then.path(), &grown, &p), ["new"]);
    }
}
