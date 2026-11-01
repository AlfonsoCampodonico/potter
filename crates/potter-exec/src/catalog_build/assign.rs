//! The new index against the previous one (design §6.2 step 4): an unchanged layer keeps its
//! entry, a changed one gets a new entry that supersedes the current one with the same name,
//! line and arch, and the serial goes up by one.

use std::collections::BTreeSet;

use potter_catalog::{Entry, Index, SCHEMA};

use crate::error::BuildError;

/// What became of one built layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No current entry has its name, line and arch: a new entry.
    New,
    /// The current entry has the same digest and is kept as it was.
    Kept,
    /// A new entry superseding this digest.
    Supersedes(String),
}

#[derive(Debug, Clone)]
pub struct Assigned {
    pub index: Index,
    /// One per built layer, in build order: its name and outcome.
    pub outcomes: Vec<(String, Outcome)>,
}

/// The index of `built` (entries with the recipe's `fixes` and no `supersedes`) over
/// `previous`. Every previous entry stays, superseded ones too; `fixes` travel only onto an
/// entry that supersedes another.
pub fn assign(previous: Option<&Index>, built: Vec<Entry>, created: String) -> Result<Assigned, BuildError> {
    let mut entries: Vec<Entry> = previous.map(|p| p.entries.clone()).unwrap_or_default();
    let mut outcomes = Vec::with_capacity(built.len());
    let mut seen = BTreeSet::new();
    for mut e in built {
        if !seen.insert(e.digest.clone()) {
            return Err(BuildError::Plan(format!(
                "{} builds the same layer {} as another recipe",
                e.name, e.digest
            )));
        }
        let current = previous.and_then(|p| {
            p.current()
                .find(|c| c.name == e.name && c.line == e.line && c.arch == e.arch)
        });
        let outcome = match current {
            Some(c) if c.digest == e.digest => Outcome::Kept,
            Some(c) => Outcome::Supersedes(c.digest.clone()),
            None => Outcome::New,
        };
        if outcome != Outcome::Kept {
            if let Some(old) = previous.and_then(|p| p.by_digest(&e.digest)) {
                return Err(BuildError::Plan(format!(
                    "{} {} built the layer {}, which the previous catalog already lists as {} {} \
                     and something supersedes",
                    e.name, e.version, e.digest, old.name, old.version
                )));
            }
            match &outcome {
                Outcome::Supersedes(old) => e.supersedes = Some(old.clone()),
                _ => e.fixes.clear(),
            }
            entries.push(e.clone());
        }
        outcomes.push((e.name, outcome));
    }
    Ok(Assigned {
        index: Index {
            schema: SCHEMA.into(),
            serial: previous.map_or(1, |p| p.serial + 1),
            created,
            entries,
        },
        outcomes,
    })
}

#[cfg(test)]
mod tests {
    use potter_catalog::{Arch, Kind};

    use super::*;

    fn d(n: u8) -> String {
        format!("sha256:{}", format!("{n:02x}").repeat(32))
    }

    fn entry(name: &str, line: &str, n: u8) -> Entry {
        Entry {
            name: name.into(),
            line: line.into(),
            version: format!("{line}.{n}"),
            arch: Arch::X86_64,
            kind: Kind::Runtime,
            digest: d(n),
            diff_id: d(n + 100),
            size: 10,
            media_type: "application/vnd.oci.image.layer.v1.tar+zstd".into(),
            abi: Default::default(),
            requires: vec![],
            conflicts: vec![],
            env: Default::default(),
            provides_sonames: vec![],
            executor: "ns".into(),
            source: "https://example.test".into(),
            license: "MIT".into(),
            supersedes: None,
            fixes: vec!["CVE-2026-1".into()],
        }
    }

    fn time() -> String {
        "2026-11-01T00:00:00Z".into()
    }

    #[test]
    fn a_first_build_is_serial_one_with_no_fixes() {
        let a = assign(None, vec![entry("os-min", "13", 1), entry("go", "1.27", 2)], time()).unwrap();
        assert_eq!(a.index.serial, 1);
        assert_eq!(a.index.schema, SCHEMA);
        assert_eq!(a.index.entries.len(), 2);
        assert!(
            a.index
                .entries
                .iter()
                .all(|e| e.fixes.is_empty() && e.supersedes.is_none())
        );
        assert_eq!(
            a.outcomes,
            [("os-min".into(), Outcome::New), ("go".into(), Outcome::New)]
        );
    }

    #[test]
    fn unchanged_keeps_changed_supersedes_and_serial_goes_up() {
        let mut old_os = entry("os-min", "13", 1);
        old_os.fixes.clear();
        old_os.version = "13.1-old".into();
        let previous = Index {
            schema: SCHEMA.into(),
            serial: 4,
            created: time(),
            entries: vec![old_os.clone(), entry("go", "1.27", 2), entry("go", "1.26", 3)],
        };
        let built = vec![entry("os-min", "13", 1), entry("go", "1.27", 4), entry("node", "24", 5)];
        let a = assign(Some(&previous), built, time()).unwrap();
        assert_eq!(a.index.serial, 5);
        assert_eq!(
            a.outcomes,
            [
                ("os-min".into(), Outcome::Kept),
                ("go".into(), Outcome::Supersedes(d(2))),
                ("node".into(), Outcome::New)
            ]
        );
        // The kept entry is the previous one, byte for byte; the old go stays listed.
        assert_eq!(a.index.entries[..3], previous.entries[..]);
        let go = a.index.by_digest(&d(4)).unwrap();
        assert_eq!(go.supersedes.as_deref(), Some(d(2).as_str()));
        assert_eq!(go.fixes, ["CVE-2026-1"]);
        let current: Vec<&str> = a.index.current().map(|e| e.version.as_str()).collect();
        assert_eq!(current, ["13.1-old", "1.26.3", "1.27.4", "24.5"]);
        // The result is an index potter accepts.
        Index::parse(&a.index.canonical()).unwrap();
    }

    #[test]
    fn a_layer_the_previous_catalog_superseded_is_refused() {
        let mut new = entry("go", "1.27", 2);
        new.supersedes = Some(d(1));
        let previous = Index {
            schema: SCHEMA.into(),
            serial: 1,
            created: time(),
            entries: vec![entry("go", "1.27", 1), new],
        };
        let err = assign(Some(&previous), vec![entry("go", "1.27", 1)], time()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("already lists")),
            "{err:?}"
        );
        let err = assign(None, vec![entry("a", "1", 1), entry("b", "1", 1)], time()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("same layer")),
            "{err:?}"
        );
    }
}
