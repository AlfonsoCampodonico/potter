use std::collections::{BTreeMap, BTreeSet};

use potter_plan::CatalogRef;
use sha2::{Digest, Sha256};

use crate::error::CatalogError;
use crate::fetch::Verified;
use crate::index::{Arch, Entry, Kind};

/// Names only the PHP stack provides; a non-PHP plan never asks for them.
pub const PHP_ONLY: &[&str] = &[
    "php",
    "php-fpm",
    "composer",
    "nginx",
    "swoole",
    "roadrunner",
    "frankenphp",
];

const PHP_EXT_PREFIX: &str = "php-ext-";

pub fn is_php_only(name: &str) -> bool {
    PHP_ONLY.contains(&name) || name.starts_with(PHP_EXT_PREFIX)
}

/// The layers a plan needs, bottom first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub entries: Vec<Entry>,
    /// One `<name>@<chosen line>` per requested ref, in request order (not dependencies).
    pub constraints: Vec<String>,
    /// The digest of the index this resolved against.
    pub index_digest: String,
}

impl Resolution {
    /// Hex SHA-256 of `composite` followed by the layer digests, newline-joined.
    pub fn composite_key(&self) -> String {
        let digests: Vec<&str> = self.entries.iter().map(|e| e.digest.as_str()).collect();
        let mut h = Sha256::new();
        h.update(b"composite");
        h.update(digests.join("\n").as_bytes());
        format!("{:x}", h.finalize())
    }
}

fn unsatisfied(msg: String) -> CatalogError {
    CatalogError::Unsatisfied(msg)
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

/// Dotted-numeric sort key: `3.9` < `3.15`.
fn dotted(s: &str) -> Dotted<'_> {
    s.split('.')
        .map(|p| {
            let at = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
            let (num, suffix) = p.split_at(at);
            (num.parse().unwrap_or(0), suffix.is_empty(), suffix)
        })
        .collect()
}

/// Per part: number, whether it is a bare number (a suffix like `rc1` sorts below), suffix.
type Dotted<'a> = Vec<(u64, bool, &'a str)>;

fn rank(e: &Entry) -> (Dotted<'_>, Dotted<'_>, &str) {
    (dotted(&e.line), dotted(&e.version), e.digest.as_str())
}

fn select<'a>(v: &'a Verified, r: &CatalogRef, arch: Arch) -> Result<&'a Entry, CatalogError> {
    let named: Vec<&Entry> = v.index.current().filter(|e| e.name == r.name).collect();
    let best = named
        .iter()
        .filter(|e| e.arch == arch && r.version.as_ref().is_none_or(|l| *l == e.line))
        .max_by(|a, b| rank(a).cmp(&rank(b)));
    if let Some(e) = best {
        return Ok(e);
    }
    let want = r.to_string();
    let on_arch: BTreeSet<&str> = named
        .iter()
        .filter(|e| e.arch == arch)
        .map(|e| e.line.as_str())
        .collect();
    let has = if !on_arch.is_empty() {
        let mut lines: Vec<&str> = on_arch.into_iter().collect();
        lines.sort_by(|a, b| dotted(a).cmp(&dotted(b)));
        format!("it has {} {}", r.name, lines.join(", "))
    } else if named.is_empty() {
        format!("it has no {} at all", r.name)
    } else {
        let archs: BTreeSet<&str> = named.iter().map(|e| arch_name(e.arch)).collect();
        format!(
            "it has {} only for {}",
            r.name,
            archs.into_iter().collect::<Vec<_>>().join(", ")
        )
    };
    Err(unsatisfied(format!(
        "the catalog has no {want} for {}; {has}",
        arch_name(arch)
    )))
}

fn add<'a>(
    v: &'a Verified,
    r: &CatalogRef,
    arch: Arch,
    chosen: &mut BTreeMap<&'a str, &'a Entry>,
) -> Result<(), CatalogError> {
    if let Some(have) = chosen.get(r.name.as_str()) {
        return match &r.version {
            Some(line) if *line != have.line => Err(unsatisfied(format!(
                "{r} for {} conflicts with {}@{} already required",
                arch_name(arch),
                have.name,
                have.line
            ))),
            _ => Ok(()),
        };
    }
    let e = select(v, r, arch)?;
    chosen.insert(e.name.as_str(), e);
    for dep in &e.requires {
        add(v, dep, arch, chosen)?;
    }
    Ok(())
}

fn check_conflicts(chosen: &BTreeMap<&str, &Entry>) -> Result<(), CatalogError> {
    for e in chosen.values() {
        if let Some(c) = e.conflicts.iter().find(|c| chosen.contains_key(c.as_str())) {
            return Err(unsatisfied(format!("{} conflicts with {c}", e.name)));
        }
    }
    Ok(())
}

fn check_abi(chosen: &BTreeMap<&str, &Entry>) -> Result<(), CatalogError> {
    for key in ["os", "libc"] {
        let mut want: Option<(&str, &str)> = None;
        for e in chosen.values() {
            let Some(val) = e.abi.get(key) else {
                return Err(unsatisfied(format!("{} carries no abi {key}", e.name)));
            };
            match want {
                None => want = Some((e.name.as_str(), val.as_str())),
                Some((first, w)) if w != val => {
                    return Err(unsatisfied(format!(
                        "abi {key} mismatch: {} is {val} but {first} is {w}",
                        e.name
                    )));
                }
                _ => {}
            }
        }
    }
    for key in ["python-abi", "ruby-api"] {
        let Some(rt) = chosen
            .values()
            .find(|e| e.kind == Kind::Runtime && e.abi.contains_key(key))
        else {
            continue;
        };
        let want = &rt.abi[key];
        if let Some(e) = chosen.values().find(|e| e.abi.get(key).is_some_and(|v| v != want)) {
            return Err(unsatisfied(format!(
                "abi {key} mismatch: {} is {} but {} is {want}",
                e.name, e.abi[key], rt.name
            )));
        }
    }
    Ok(())
}

/// Bases first, every layer after the layers it requires, ties by name.
fn order<'a>(chosen: &BTreeMap<&'a str, &'a Entry>) -> Result<Vec<&'a Entry>, CatalogError> {
    let mut out: Vec<&Entry> = Vec::with_capacity(chosen.len());
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    while out.len() < chosen.len() {
        let next = chosen
            .values()
            .filter(|e| !placed.contains(e.name.as_str()))
            .filter(|e| e.requires.iter().all(|r| placed.contains(r.name.as_str())))
            .min_by_key(|e| (e.kind != Kind::Base, e.name.as_str()));
        let Some(e) = next else {
            return Err(unsatisfied("the catalog's requires form a cycle".into()));
        };
        placed.insert(e.name.as_str());
        out.push(e);
    }
    Ok(out)
}

pub fn resolve(v: &Verified, refs: &[CatalogRef], arch: Arch) -> Result<Resolution, CatalogError> {
    let mut chosen = BTreeMap::new();
    for r in refs {
        add(v, r, arch, &mut chosen)?;
    }
    check_conflicts(&chosen)?;
    check_abi(&chosen)?;
    let entries: Vec<Entry> = order(&chosen)?.into_iter().cloned().collect();
    let mut constraints: Vec<String> = Vec::new();
    for r in refs {
        let c = format!("{}@{}", r.name, chosen[r.name.as_str()].line);
        if !constraints.contains(&c) {
            constraints.push(c);
        }
    }
    Ok(Resolution {
        entries,
        constraints,
        index_digest: v.digest.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proptest::prelude::*;

    use super::*;
    use crate::index::{Index, SCHEMA};
    use crate::location::Location;

    fn d(n: u8) -> String {
        format!("sha256:{}", format!("{n:02x}").repeat(32))
    }

    fn entry(n: u8, name: &str, line: &str, kind: Kind) -> Entry {
        Entry {
            name: name.into(),
            line: line.into(),
            version: format!("{line}.0"),
            arch: Arch::X86_64,
            kind,
            digest: d(n),
            diff_id: d(n.wrapping_add(100)),
            size: 1,
            media_type: "application/vnd.oci.image.layer.v1.tar+zstd".into(),
            abi: [
                ("os".to_string(), "debian12".to_string()),
                ("libc".to_string(), "glibc2.36".to_string()),
            ]
            .into(),
            requires: vec![],
            conflicts: vec![],
            env: BTreeMap::new(),
            provides_sonames: vec![],
            executor: name.into(),
            source: "https://example.test".into(),
            license: "MIT".into(),
            supersedes: None,
            fixes: vec![],
        }
    }

    fn verified(entries: Vec<Entry>) -> Verified {
        Verified {
            index: Index {
                schema: SCHEMA.into(),
                serial: 1,
                created: "2026-10-31T00:00:00Z".into(),
                entries,
            },
            digest: d(250),
            location: Location::Layout(PathBuf::from("/tmp/cat")),
        }
    }

    fn r(name: &str, line: Option<&str>) -> CatalogRef {
        CatalogRef::new(name, line)
    }

    fn names(res: &Resolution) -> Vec<&str> {
        res.entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn unsat(res: Result<Resolution, CatalogError>) -> String {
        match res {
            Err(CatalogError::Unsatisfied(m)) => m,
            other => panic!("want Unsatisfied, got {other:?}"),
        }
    }

    #[test]
    fn php_only_names() {
        for n in PHP_ONLY {
            assert!(is_php_only(n));
        }
        assert!(is_php_only("php-ext-redis"));
        assert!(!is_php_only("python"));
        assert!(!is_php_only("phpx"));
    }

    #[test]
    fn picks_current_not_superseded() {
        let old = entry(1, "python", "3.15", Kind::Runtime);
        let mut new = entry(2, "python", "3.15", Kind::Runtime);
        new.version = "3.15.1".into();
        new.supersedes = Some(old.digest.clone());
        let older_line = entry(3, "python", "3.9", Kind::Runtime);
        let v = verified(vec![old, new.clone(), older_line]);
        let res = resolve(&v, &[r("python", None)], Arch::X86_64).unwrap();
        assert_eq!(res.entries, vec![new]);
        assert_eq!(res.constraints, vec!["python@3.15"]);
        assert_eq!(res.index_digest, d(250));
        // dotted-numeric: 3.9 is below 3.15, and an explicit line picks it.
        let res = resolve(&v, &[r("python", Some("3.9"))], Arch::X86_64).unwrap();
        assert_eq!(res.entries[0].line, "3.9");
    }

    #[test]
    fn constraints_follow_requested_refs() {
        let base = entry(1, "base", "1", Kind::Base);
        let mut osmin = entry(2, "os-min", "12", Kind::Extension);
        osmin.requires = vec![r("base", None)];
        let mut py = entry(3, "python", "3.15", Kind::Runtime);
        py.requires = vec![r("libs", None)];
        let mut libs = entry(4, "libs", "1", Kind::Extension);
        libs.requires = vec![r("base", None)];
        let mut uv = entry(5, "uv", "0.9", Kind::Tool);
        uv.requires = vec![r("python", None)];
        let v = verified(vec![base, osmin, py, libs, uv]);
        let refs = [
            r("os-min", None),
            r("python", Some("3.15")),
            r("uv", None),
            r("python", None),
        ];
        let res = resolve(&v, &refs, Arch::X86_64).unwrap();
        assert_eq!(res.constraints, ["os-min@12", "python@3.15", "uv@0.9"]);
        assert!(names(&res).contains(&"libs"));
    }

    #[test]
    fn prerelease_sorts_below_release() {
        let mut rc = entry(1, "python", "3.15", Kind::Runtime);
        rc.version = "3.15.0rc1".into();
        let mut rel = entry(2, "python", "3.15", Kind::Runtime);
        rel.version = "3.15.0".into();
        let v = verified(vec![rc, rel.clone()]);
        let res = resolve(&v, &[r("python", None)], Arch::X86_64).unwrap();
        assert_eq!(res.entries, vec![rel]);
    }

    #[test]
    fn requires_closure_is_ordered() {
        let base = entry(1, "base", "1", Kind::Base);
        let mut libs = entry(2, "libs", "1", Kind::Extension);
        libs.requires = vec![r("base", Some("1"))];
        let mut py = entry(3, "python", "3.15", Kind::Runtime);
        py.requires = vec![r("libs", None)];
        let mut tool = entry(4, "aaa-tool", "1", Kind::Tool);
        tool.requires = vec![r("python", None)];
        let v = verified(vec![tool, py, libs, base]);
        let res = resolve(&v, &[r("aaa-tool", None)], Arch::X86_64).unwrap();
        assert_eq!(names(&res), ["base", "libs", "python", "aaa-tool"]);
    }

    #[test]
    fn conflict_is_unsatisfied() {
        let a = entry(1, "a", "1", Kind::Tool);
        let mut b = entry(2, "b", "1", Kind::Tool);
        b.conflicts = vec!["a".into()];
        let v = verified(vec![a, b]);
        let m = unsat(resolve(&v, &[r("a", None), r("b", None)], Arch::X86_64));
        assert!(m.contains("conflicts"), "{m}");
        // two lines of one name conflict too
        let v = verified(vec![
            entry(1, "node", "22", Kind::Runtime),
            entry(2, "node", "26", Kind::Runtime),
        ]);
        let m = unsat(resolve(
            &v,
            &[r("node", Some("22")), r("node", Some("26"))],
            Arch::X86_64,
        ));
        assert!(m.contains("node@26"), "{m}");
    }

    #[test]
    fn abi_mismatch_is_unsatisfied() {
        let mut a = entry(1, "a", "1", Kind::Tool);
        a.abi.insert("libc".into(), "musl".into());
        let b = entry(2, "b", "1", Kind::Tool);
        let v = verified(vec![a, b]);
        let m = unsat(resolve(&v, &[r("a", None), r("b", None)], Arch::X86_64));
        assert!(m.contains("libc"), "{m}");

        let mut missing = entry(3, "m", "1", Kind::Tool);
        missing.abi.remove("os");
        let m = unsat(resolve(&verified(vec![missing]), &[r("m", None)], Arch::X86_64));
        assert!(m.contains("os"), "{m}");

        let mut py = entry(4, "python", "3.15", Kind::Runtime);
        py.abi.insert("python-abi".into(), "cp315".into());
        let mut ext = entry(5, "numpy", "2", Kind::Extension);
        ext.abi.insert("python-abi".into(), "cp314".into());
        let m = unsat(resolve(
            &verified(vec![py, ext]),
            &[r("python", None), r("numpy", None)],
            Arch::X86_64,
        ));
        assert!(m.contains("python-abi"), "{m}");
    }

    #[test]
    fn missing_arch_names_name_line_arch() {
        let v = verified(vec![entry(1, "node", "26", Kind::Runtime)]);
        let m = unsat(resolve(&v, &[r("node", Some("22"))], Arch::X86_64));
        assert_eq!(m, "the catalog has no node@22 for x86_64; it has node 26");
        let mut arm = entry(2, "go", "1.25", Kind::Runtime);
        arm.arch = Arch::Aarch64;
        let m = unsat(resolve(&verified(vec![arm]), &[r("go", None)], Arch::X86_64));
        assert_eq!(m, "the catalog has no go for x86_64; it has go only for aarch64");
    }

    #[test]
    fn unknown_ref_names_it() {
        let v = verified(vec![entry(1, "node", "26", Kind::Runtime)]);
        let m = unsat(resolve(&v, &[r("zig", Some("0.14"))], Arch::X86_64));
        assert!(m.contains("zig@0.14") && m.contains("x86_64"), "{m}");
    }

    fn fixture() -> Vec<Entry> {
        let with = |mut e: Entry, reqs: &[&str]| {
            e.requires = reqs.iter().map(|n| r(n, None)).collect();
            e
        };
        vec![
            entry(1, "base", "1", Kind::Base),
            with(entry(2, "libs", "1", Kind::Extension), &["base"]),
            with(entry(3, "python", "3.15", Kind::Runtime), &["libs"]),
            with(entry(4, "python", "3.9", Kind::Runtime), &["libs"]),
            with(entry(5, "pip", "1", Kind::Tool), &["python", "base"]),
            entry(6, "curl", "8", Kind::Tool),
        ]
    }

    proptest! {
        #[test]
        fn resolution_is_deterministic(entries in Just(fixture()).prop_shuffle()) {
            let refs = [r("pip", None), r("curl", None)];
            let want = resolve(&verified(fixture()), &refs, Arch::X86_64).unwrap();
            let got = resolve(&verified(entries), &refs, Arch::X86_64).unwrap();
            prop_assert_eq!(got.composite_key(), want.composite_key());
            prop_assert_eq!(got, want);
        }
    }
}
