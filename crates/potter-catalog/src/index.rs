use std::collections::{BTreeMap, BTreeSet};

use potter_plan::CatalogRef;
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

pub const SCHEMA: &str = "potter.catalog/v1";
/// Largest index blob `parse` will look at.
pub const MAX_INDEX_BYTES: u64 = 8 << 20;
pub const MAX_ENTRIES: usize = 10_000;
/// The executor classes a catalog layer can be built with (P12).
pub const EXECUTORS: [&str; 2] = ["ns", "vm"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema: String,
    pub serial: u64,
    /// RFC 3339 timestamp.
    pub created: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Base,
    Runtime,
    Extension,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub line: String,
    pub version: String,
    pub arch: Arch,
    pub kind: Kind,
    /// Blob digest.
    pub digest: String,
    pub diff_id: String,
    pub size: u64,
    pub media_type: String,
    #[serde(default)]
    pub abi: BTreeMap<String, String>,
    #[serde(default)]
    pub requires: Vec<CatalogRef>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub provides_sonames: Vec<String>,
    pub executor: String,
    pub source: String,
    pub license: String,
    /// Digest of the entry this one replaces.
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub fixes: Vec<String>,
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Index, CatalogError> {
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(hostile(format!("index is larger than {MAX_INDEX_BYTES} bytes")));
        }
        let index: Index = serde_json::from_slice(bytes).map_err(|e| hostile(format!("index does not parse: {e}")))?;
        if index.schema != SCHEMA {
            return Err(hostile(format!(
                "unsupported schema {:?}, want {SCHEMA:?}",
                index.schema
            )));
        }
        if index.entries.len() > MAX_ENTRIES {
            return Err(hostile(format!("index has more than {MAX_ENTRIES} entries")));
        }
        let mut seen = BTreeSet::new();
        for e in &index.entries {
            for (field, value) in [("digest", &e.digest), ("diff_id", &e.diff_id)] {
                if !is_sha256(value) {
                    return Err(hostile(format!("{} {field} {value:?} is not sha256:<64 hex>", e.name)));
                }
            }
            if !EXECUTORS.contains(&e.executor.as_str()) {
                return Err(hostile(format!(
                    "{} executor {:?} is neither \"ns\" nor \"vm\"",
                    e.name, e.executor
                )));
            }
            if !seen.insert(e.digest.as_str()) {
                return Err(hostile(format!("duplicate digest {}", e.digest)));
            }
        }
        Ok(index)
    }

    /// The JCS bytes that get signed.
    pub fn canonical(&self) -> Vec<u8> {
        potter_plan::canonical_json(self)
            .expect("an index serializes to JSON without floats")
            .into_bytes()
    }

    /// Entries no other entry supersedes.
    pub fn current(&self) -> impl Iterator<Item = &Entry> {
        let superseded: BTreeSet<&str> = self.entries.iter().filter_map(|e| e.supersedes.as_deref()).collect();
        self.entries
            .iter()
            .filter(move |e| !superseded.contains(e.digest.as_str()))
    }

    pub fn by_digest(&self, digest: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.digest == digest)
    }
}

fn hostile(msg: String) -> CatalogError {
    CatalogError::Hostile(msg)
}

fn is_sha256(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u8) -> String {
        format!("sha256:{}", format!("{n:02x}").repeat(32))
    }

    fn entry(n: u8) -> Entry {
        Entry {
            name: "php".into(),
            line: "8.4".into(),
            version: format!("8.4.{n}"),
            arch: Arch::X86_64,
            kind: Kind::Runtime,
            digest: d(n),
            diff_id: d(n.wrapping_add(100)),
            size: 1024,
            media_type: "application/vnd.oci.image.layer.v1.tar+zstd".into(),
            abi: [("glibc".to_string(), "2.39".to_string())].into(),
            requires: vec![potter_plan::CatalogRef::new("base", Some("1"))],
            conflicts: vec![],
            env: [("PHP_INI_DIR".to_string(), "/etc/php".to_string())].into(),
            provides_sonames: vec!["libphp.so".into()],
            executor: "ns".into(),
            source: "https://example.test/php".into(),
            license: "PHP-3.01".into(),
            supersedes: None,
            fixes: vec![],
        }
    }

    fn index(entries: Vec<Entry>) -> Index {
        Index {
            schema: SCHEMA.into(),
            serial: 7,
            created: "2026-10-31T00:00:00Z".into(),
            entries,
        }
    }

    fn bytes(i: &Index) -> Vec<u8> {
        serde_json::to_vec(i).unwrap()
    }

    #[test]
    fn parse_round_trips_canonical() {
        let i = index(vec![entry(1), entry(2)]);
        let canon = i.canonical();
        let parsed = Index::parse(&canon).unwrap();
        assert_eq!(parsed, i);
        assert_eq!(parsed.canonical(), canon);
    }

    #[test]
    fn unknown_field_is_hostile() {
        let mut v = serde_json::to_value(index(vec![entry(1)])).unwrap();
        v["entries"][0]["surprise"] = true.into();
        let err = Index::parse(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert!(matches!(err, CatalogError::Hostile(_)), "{err:?}");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn oversized_blob_is_hostile() {
        let blob = vec![b' '; MAX_INDEX_BYTES as usize + 1];
        assert!(matches!(Index::parse(&blob), Err(CatalogError::Hostile(_))));
    }

    #[test]
    fn too_many_entries_is_hostile() {
        let entries = (0..MAX_ENTRIES + 1)
            .map(|n| {
                let mut e = entry(1);
                e.digest = format!("sha256:{n:064x}");
                e
            })
            .collect();
        let err = Index::parse(&bytes(&index(entries))).unwrap_err();
        assert!(matches!(err, CatalogError::Hostile(_)), "{err:?}");
    }

    #[test]
    fn duplicate_digest_is_hostile() {
        let err = Index::parse(&bytes(&index(vec![entry(1), entry(1)]))).unwrap_err();
        assert!(matches!(err, CatalogError::Hostile(m) if m.contains("duplicate")));
    }

    #[test]
    fn wrong_schema_and_bad_digest_are_hostile() {
        let mut i = index(vec![entry(1)]);
        i.schema = "potter.catalog/v2".into();
        assert!(matches!(Index::parse(&bytes(&i)), Err(CatalogError::Hostile(_))));
        let mut i = index(vec![entry(1)]);
        i.entries[0].digest = "sha256:XYZ".into();
        assert!(matches!(Index::parse(&bytes(&i)), Err(CatalogError::Hostile(_))));
    }

    #[test]
    fn an_executor_other_than_ns_or_vm_is_hostile() {
        for ok in ["ns", "vm"] {
            let mut i = index(vec![entry(1)]);
            i.entries[0].executor = ok.into();
            Index::parse(&bytes(&i)).unwrap();
        }
        for bad in ["", "host", "php", "VM"] {
            let mut i = index(vec![entry(1)]);
            i.entries[0].executor = bad.into();
            let err = Index::parse(&bytes(&i)).unwrap_err();
            assert!(
                matches!(&err, CatalogError::Hostile(m) if m.contains("executor")),
                "{bad:?}: {err:?}"
            );
        }
    }

    #[test]
    fn current_skips_superseded() {
        let old = entry(1);
        let mut new = entry(2);
        new.supersedes = Some(old.digest.clone());
        let i = index(vec![old, new.clone()]);
        let cur: Vec<_> = i.current().collect();
        assert_eq!(cur, vec![&new]);
        assert_eq!(i.by_digest(&new.digest), Some(&new));
        assert!(i.by_digest("sha256:nope").is_none());
    }
}
