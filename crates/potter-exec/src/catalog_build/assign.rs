//! The new index against the previous one (design §6.2 step 4): an unchanged layer keeps its
//! entry, a changed one gets a new entry that supersedes the current one with the same name,
//! line and arch, and the serial goes up by one.

use std::collections::{BTreeMap, BTreeSet};

use potter_catalog::{Arch, Entry, Index, SCHEMA};

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

fn arch_name(a: Arch) -> &'static str {
    match a {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

/// One index from per-architecture builds that each followed `previous`: its entries, then
/// every build's additions in build order, one serial above it. Each build must be the previous
/// entries exactly, followed by entries of one architecture no other build adds, at that serial;
/// a build with no additions (nothing changed on its architecture) is fine.
pub fn merge(previous: Option<&Index>, builds: &[Index], created: String) -> Result<Index, BuildError> {
    let base: &[Entry] = previous.map_or(&[], |p| &p.entries);
    let serial = previous.map_or(1, |p| p.serial + 1);
    if builds.is_empty() {
        return Err(BuildError::Plan("no catalog layouts to merge".into()));
    }
    let mut entries = base.to_vec();
    let mut arches: BTreeMap<Arch, usize> = BTreeMap::new();
    for (i, b) in builds.iter().enumerate() {
        let n = i + 1;
        if b.serial != serial {
            return Err(BuildError::Plan(format!(
                "layout {n} is serial {}, not {serial}: every layout must be built on the same previous catalog",
                b.serial
            )));
        }
        if b.entries.get(..base.len()) != Some(base) {
            return Err(BuildError::Plan(format!(
                "layout {n} does not start with the previous catalog's entries as they are: it was built on another catalog"
            )));
        }
        let added = &b.entries[base.len()..];
        if let Some(first) = added.first() {
            if let Some(other) = added.iter().find(|e| e.arch != first.arch) {
                return Err(BuildError::Plan(format!(
                    "layout {n} adds entries for both {} and {}: each layout is one architecture's build",
                    arch_name(first.arch),
                    arch_name(other.arch)
                )));
            }
            if let Some(m) = arches.insert(first.arch, n) {
                return Err(BuildError::Plan(format!(
                    "layouts {m} and {n} both add {} entries",
                    arch_name(first.arch)
                )));
            }
        }
        entries.extend(added.iter().cloned());
    }
    let index = Index {
        schema: SCHEMA.into(),
        serial,
        created,
        entries,
    };
    Index::parse(&index.canonical()).map_err(|e| BuildError::Plan(format!("the merged index: {e}")))?;
    Ok(index)
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

    fn on(arch: Arch, mut e: Entry) -> Entry {
        e.arch = arch;
        e
    }

    #[test]
    fn merge_takes_both_arches_at_one_serial() {
        let previous = Index {
            schema: SCHEMA.into(),
            serial: 7,
            created: time(),
            entries: vec![
                on(Arch::X86_64, entry("go", "1.27", 1)),
                on(Arch::Aarch64, entry("go", "1.27", 2)),
            ],
        };
        // Each arch's build over the same previous catalog: a new go for x86_64 superseding its
        // old one; node new on aarch64.
        let x86 = assign(Some(&previous), vec![on(Arch::X86_64, entry("go", "1.27", 3))], time())
            .unwrap()
            .index;
        let arm = assign(
            Some(&previous),
            vec![
                on(Arch::Aarch64, entry("go", "1.27", 2)),
                on(Arch::Aarch64, entry("node", "26", 4)),
            ],
            time(),
        )
        .unwrap()
        .index;
        assert_eq!((x86.serial, arm.serial), (8, 8));
        let merged = merge(
            Some(&previous),
            &[arm.clone(), x86.clone()],
            "2026-11-02T00:00:00Z".into(),
        )
        .unwrap();
        assert_eq!(merged.serial, 8, "one bump, not one per arch");
        assert_eq!(merged.created, "2026-11-02T00:00:00Z");
        assert_eq!(merged.entries[..2], previous.entries[..]);
        let digests: Vec<&str> = merged.entries.iter().map(|e| e.digest.as_str()).collect();
        assert_eq!(digests, [d(1), d(2), d(4), d(3)]);
        let current: Vec<(Arch, &str)> = merged.current().map(|e| (e.arch, e.digest.as_str())).collect();
        assert_eq!(
            current,
            [
                (Arch::Aarch64, d(2).as_str()),
                (Arch::Aarch64, d(4).as_str()),
                (Arch::X86_64, d(3).as_str())
            ]
        );
        assert_eq!(
            merged.by_digest(&d(3)).unwrap().supersedes.as_deref(),
            Some(d(1).as_str())
        );

        // A first catalog: no previous, serial 1.
        let first_x86 = assign(None, vec![on(Arch::X86_64, entry("os-min", "13", 5))], time())
            .unwrap()
            .index;
        let first_arm = assign(None, vec![on(Arch::Aarch64, entry("os-min", "13", 6))], time())
            .unwrap()
            .index;
        let merged = merge(None, &[first_x86, first_arm], time()).unwrap();
        assert_eq!((merged.serial, merged.entries.len()), (1, 2));
    }

    #[test]
    fn merge_refuses_layouts_that_do_not_fit() {
        let previous = Index {
            schema: SCHEMA.into(),
            serial: 2,
            created: time(),
            entries: vec![on(Arch::X86_64, entry("go", "1.27", 1))],
        };
        let build = |arch, n| {
            assign(Some(&previous), vec![on(arch, entry("node", "26", n))], time())
                .unwrap()
                .index
        };
        let plan = |r: Result<Index, BuildError>, want: &str| match r {
            Err(BuildError::Plan(m)) if m.contains(want) => {}
            other => panic!("want a plan error with {want:?}, got {other:?}"),
        };
        plan(merge(Some(&previous), &[], time()), "no catalog layouts");
        // Two layouts of the same arch.
        plan(
            merge(
                Some(&previous),
                &[build(Arch::X86_64, 2), build(Arch::X86_64, 3)],
                time(),
            ),
            "both add x86_64",
        );
        // Built on another previous catalog: serial and entries differ.
        let mut stale = build(Arch::Aarch64, 4);
        stale.serial = 2;
        plan(
            merge(Some(&previous), &[build(Arch::X86_64, 2), stale], time()),
            "serial 2, not 3",
        );
        let mut other = build(Arch::Aarch64, 4);
        other.entries[0].version = "1.27.9".into();
        plan(merge(Some(&previous), &[other], time()), "built on another catalog");
        // One layout with both arches.
        let mut mixed = build(Arch::X86_64, 2);
        mixed.entries.push(on(Arch::Aarch64, entry("node", "26", 5)));
        plan(merge(Some(&previous), &[mixed], time()), "both x86_64 and aarch64");
        // Both arches' builds made the same layer.
        plan(
            merge(
                Some(&previous),
                &[build(Arch::X86_64, 2), build(Arch::Aarch64, 2)],
                time(),
            ),
            "duplicate digest",
        );
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
