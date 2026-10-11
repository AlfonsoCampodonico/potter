//! The target graph (spec §10): targets build after the targets they copy from.

use std::collections::{BTreeMap, BTreeSet};

use potter_plan::{CopySource, Step, Target};

use crate::error::BuildError;

/// The targets `target` copies from.
fn sources(target: &Target) -> Result<Vec<&str>, BuildError> {
    let mut out = Vec::new();
    for step in &target.build.steps {
        if let Step::CopyFrom { name, from, .. } = step {
            match from {
                CopySource::Target(t) => out.push(t.as_str()),
                CopySource::Stage(_) => {
                    return Err(BuildError::Unsupported {
                        what: format!("the stage source of step {name} of target {}", target.name),
                        milestone: "P5 (the Dockerfile frontend)",
                    });
                }
            }
        }
    }
    Ok(out)
}

/// The indexes of `targets` to build, in an order where every target follows the targets it
/// copies from: `only` (all when empty) and what they need. Ties keep plan order.
pub fn order(targets: &[Target], only: &[String]) -> Result<Vec<usize>, BuildError> {
    let index: BTreeMap<&str, usize> = targets.iter().enumerate().map(|(i, t)| (t.name.as_str(), i)).collect();
    let mut deps: Vec<Vec<usize>> = Vec::with_capacity(targets.len());
    for t in targets {
        let mut d = Vec::new();
        for s in sources(t)? {
            let i = *index
                .get(s)
                .ok_or_else(|| BuildError::Plan(format!("target {} copies from {s}, which is not a target", t.name)))?;
            d.push(i);
        }
        deps.push(d);
    }
    // The selection and everything it needs.
    let mut wanted = BTreeSet::new();
    let mut todo: Vec<usize> = if only.is_empty() {
        (0..targets.len()).collect()
    } else {
        only.iter()
            .map(|n| {
                index
                    .get(n.as_str())
                    .copied()
                    .ok_or_else(|| BuildError::Plan(format!("no target named {n:?}")))
            })
            .collect::<Result<_, _>>()?
    };
    while let Some(i) = todo.pop() {
        if wanted.insert(i) {
            todo.extend(&deps[i]);
        }
    }
    // Kahn's algorithm, smallest plan index first.
    let mut done: BTreeSet<usize> = BTreeSet::new();
    let mut out = Vec::new();
    while out.len() < wanted.len() {
        let next = wanted
            .iter()
            .copied()
            .find(|i| !done.contains(i) && deps[*i].iter().all(|d| done.contains(d)));
        let Some(i) = next else {
            let stuck: Vec<&str> = wanted
                .iter()
                .filter(|i| !done.contains(i))
                .map(|&i| targets[i].name.as_str())
                .collect();
            return Err(BuildError::Plan(format!(
                "copy_from steps form a cycle among {}",
                stuck.join(", ")
            )));
        };
        done.insert(i);
        out.push(i);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use potter_plan::*;

    use super::*;

    fn target(name: &str, copies: &[CopySource]) -> Target {
        let base = Base::Oci {
            reference: "x:1".into(),
            digest: None,
        };
        Target {
            name: name.into(),
            root: ".".into(),
            platform: None,
            provider: "none".into(),
            build: Chain {
                base: base.clone(),
                steps: copies
                    .iter()
                    .enumerate()
                    .map(|(i, from)| Step::CopyFrom {
                        name: format!("copy-{i}"),
                        from: from.clone(),
                        paths: vec!["/app".into()],
                        dest: "/app".into(),
                        owner: "0:0".into(),
                        mode: None,
                    })
                    .collect(),
            },
            image: Image {
                base,
                runtime_steps: vec![],
                export: vec![],
                files: vec![],
                config: ImageConfig::new("/", "0:0"),
                processes: BTreeMap::new(),
                services: BTreeMap::new(),
            },
        }
    }

    fn names(targets: &[Target], order: &[usize]) -> Vec<String> {
        order.iter().map(|&i| targets[i].name.clone()).collect()
    }

    #[test]
    fn targets_follow_the_targets_they_copy_from() {
        let t = vec![
            target("api", &[CopySource::Target("web".into())]),
            target("web", &[CopySource::Target("assets".into())]),
            target("assets", &[]),
            target("docs", &[]),
        ];
        assert_eq!(names(&t, &order(&t, &[]).unwrap()), ["assets", "web", "api", "docs"]);
        // Selecting one target brings the targets it needs.
        assert_eq!(names(&t, &order(&t, &["web".to_string()]).unwrap()), ["assets", "web"]);
    }

    #[test]
    fn cycles_unknown_targets_and_stages_are_refused() {
        let cycle = vec![
            target("a", &[CopySource::Target("b".into())]),
            target("b", &[CopySource::Target("a".into())]),
        ];
        assert_eq!(order(&cycle, &[]).unwrap_err().exit_code(), 2);
        let unknown = vec![target("a", &[CopySource::Target("ghost".into())])];
        assert!(order(&unknown, &[]).unwrap_err().to_string().contains("ghost"));
        let stage = vec![target("a", &[CopySource::Stage("build".into())])];
        assert_eq!(order(&stage, &[]).unwrap_err().exit_code(), 2);
        assert_eq!(
            order(&[target("a", &[])], &["nope".to_string()])
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
