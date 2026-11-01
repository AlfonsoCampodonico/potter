//! The provenance statement: in-toto v1 with a SLSA v1 provenance predicate (spec §17). Every
//! field comes from the plan or the host's own records; it names the executor class behind
//! every layer, so it can't overstate how the image was built (P12).

use serde_json::{Map, Value, json};

pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
pub const SLSA_PREDICATE: &str = "https://slsa.dev/provenance/v1";
pub const SPDX_PREDICATE: &str = "https://spdx.dev/Document/v2.3";
pub const BUILD_TYPE: &str = "https://potter.dev/build/v1";

/// One layer of the image, as the host recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvStep {
    pub chain: String,
    pub step: String,
    /// The step's cache key (hex); `None` for host-built layers.
    pub key: Option<String>,
    /// `sha256:<hex>`.
    pub layer: String,
    /// `ns` or `vm`; `None` when the host built the layer.
    pub class: Option<String>,
}

/// A catalog layer the build used, from the signed index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogLayer {
    /// `sha256:<hex>` of the layer blob.
    pub digest: String,
    pub name: String,
    pub version: String,
    /// The executor class that built it: `ns` or `vm`.
    pub executor: String,
}

pub struct ProvenanceInput<'a> {
    pub subject_name: &'a str,
    /// `sha256:<hex>` of the image manifest.
    pub subject_digest: &'a str,
    pub builder_id: &'a str,
    pub potter_version: &'a str,
    pub target: &'a str,
    /// The canonical hash of what was built (hex).
    pub plan_hash: &'a str,
    pub source_revision: Option<&'a str>,
    /// Whether the tree had uncommitted changes; `None` when nobody checked (rendered `null`).
    pub source_dirty: Option<bool>,
    pub tenant: &'a str,
    pub catalog_index: Option<&'a str>,
    /// The catalog layers behind the image's catalog bases, bottom first.
    pub catalog: &'a [CatalogLayer],
    /// (reference, `sha256:<hex>`) of each OCI base image.
    pub bases: &'a [(String, String)],
    pub steps: &'a [ProvStep],
    /// RFC 3339.
    pub started: &'a str,
    pub finished: &'a str,
}

/// An in-toto v1 statement about one subject.
pub fn in_toto(subject_name: &str, subject_digest: &str, predicate_type: &str, predicate: Value) -> Value {
    json!({
        "_type": STATEMENT_TYPE,
        "subject": [{"name": subject_name, "digest": digest_set(subject_digest)}],
        "predicateType": predicate_type,
        "predicate": predicate,
    })
}

/// The executor class the image as a whole may claim (P12): `ns` when any layer, a step's or a
/// catalog layer's, is not `vm`-built; `vm` when they all are; `host` when potter built every
/// layer itself.
pub fn image_executor(steps: &[ProvStep], catalog: &[CatalogLayer]) -> &'static str {
    let classes: Vec<&str> = steps
        .iter()
        .filter_map(|s| s.class.as_deref())
        .chain(catalog.iter().map(|c| c.executor.as_str()))
        .collect();
    if classes.is_empty() {
        "host"
    } else if classes.iter().all(|&c| c == "vm") {
        "vm"
    } else {
        "ns"
    }
}

pub fn statement(input: &ProvenanceInput) -> Value {
    let dependencies: Vec<Value> = input
        .bases
        .iter()
        .map(|(reference, digest)| json!({"uri": reference, "digest": digest_set(digest)}))
        .chain(input.catalog.iter().map(|c| {
            json!({
                "name": c.name,
                "digest": digest_set(&c.digest),
                "annotations": {"version": c.version, "executor": c.executor},
            })
        }))
        .collect();
    let byproducts: Vec<Value> = input
        .steps
        .iter()
        .map(|s| {
            let mut annotations = Map::new();
            annotations.insert("executor".into(), json!(s.class.as_deref().unwrap_or("host")));
            if let Some(key) = &s.key {
                annotations.insert("cacheKey".into(), json!(key));
            }
            json!({
                "name": format!("{}/{}", s.chain, s.step),
                "digest": digest_set(&s.layer),
                "annotations": annotations,
            })
        })
        .collect();
    let predicate = json!({
        "buildDefinition": {
            "buildType": BUILD_TYPE,
            "externalParameters": {
                "target": input.target,
                "plan": input.plan_hash,
                "source": {"revision": input.source_revision, "dirty": input.source_dirty},
            },
            "internalParameters": {
                "tenant": input.tenant,
                "catalogIndex": input.catalog_index,
                "executor": image_executor(input.steps, input.catalog),
            },
            "resolvedDependencies": dependencies,
        },
        "runDetails": {
            "builder": {"id": input.builder_id, "version": {"potter": input.potter_version}},
            "metadata": {"startedOn": input.started, "finishedOn": input.finished},
            "byproducts": byproducts,
        },
    });
    in_toto(input.subject_name, input.subject_digest, SLSA_PREDICATE, predicate)
}

/// `sha256:<hex>` → `{"sha256": "<hex>"}`.
fn digest_set(digest: &str) -> Value {
    let (algorithm, hex) = digest.split_once(':').unwrap_or(("sha256", digest));
    json!({ algorithm: hex })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(chain: &str, name: &str, key: Option<&str>, layer: &str, class: Option<&str>) -> ProvStep {
        ProvStep {
            chain: chain.into(),
            step: name.into(),
            key: key.map(Into::into),
            layer: layer.into(),
            class: class.map(Into::into),
        }
    }

    fn input<'a>(bases: &'a [(String, String)], steps: &'a [ProvStep]) -> ProvenanceInput<'a> {
        ProvenanceInput {
            subject_name: "registry.example/app/web",
            subject_digest: "sha256:aaaa",
            builder_id: "https://potter.dev/builder/local",
            potter_version: "0.1.0",
            target: "web",
            plan_hash: "cccc",
            source_revision: Some("0123abcd"),
            source_dirty: None,
            tenant: "acme",
            catalog_index: None,
            catalog: &[],
            bases,
            steps,
            started: "2026-10-11T00:00:00Z",
            finished: "2026-10-11T00:01:00Z",
        }
    }

    #[test]
    fn statements_name_their_subject_by_digest() {
        let s = in_toto("img", "sha256:abcd", SPDX_PREDICATE, json!({"x": 1}));
        assert_eq!(s["_type"], STATEMENT_TYPE);
        assert_eq!(s["subject"], json!([{"name": "img", "digest": {"sha256": "abcd"}}]));
        assert_eq!(s["predicateType"], SPDX_PREDICATE);
        assert_eq!(s["predicate"], json!({"x": 1}));
    }

    #[test]
    fn the_build_definition_records_the_plan_source_and_bases() {
        let bases = [("php:8.4-fpm-trixie".to_string(), "sha256:bbbb".to_string())];
        let s = statement(&input(&bases, &[]));
        assert_eq!(s["predicateType"], SLSA_PREDICATE);
        assert_eq!(s["subject"][0]["digest"]["sha256"], "aaaa");
        let def = &s["predicate"]["buildDefinition"];
        assert_eq!(def["buildType"], BUILD_TYPE);
        assert_eq!(
            def["externalParameters"],
            json!({"target": "web", "plan": "cccc", "source": {"revision": "0123abcd", "dirty": null}})
        );
        assert_eq!(
            def["internalParameters"],
            json!({"tenant": "acme", "catalogIndex": null, "executor": "host"})
        );
        assert_eq!(
            def["resolvedDependencies"],
            json!([{"uri": "php:8.4-fpm-trixie", "digest": {"sha256": "bbbb"}}])
        );
        let run = &s["predicate"]["runDetails"];
        assert_eq!(
            run["builder"],
            json!({"id": "https://potter.dev/builder/local", "version": {"potter": "0.1.0"}})
        );
        assert_eq!(
            run["metadata"],
            json!({"startedOn": "2026-10-11T00:00:00Z", "finishedOn": "2026-10-11T00:01:00Z"})
        );
    }

    #[test]
    fn every_layer_names_its_executor_class() {
        let steps = [
            step("web", "inputs-app", None, "sha256:1111", None),
            step("web", "install", Some("ffff"), "sha256:2222", Some("ns")),
        ];
        let s = statement(&input(&[], &steps));
        assert_eq!(
            s["predicate"]["runDetails"]["byproducts"],
            json!([
                {"name": "web/inputs-app", "digest": {"sha256": "1111"}, "annotations": {"executor": "host"}},
                {"name": "web/install", "digest": {"sha256": "2222"}, "annotations": {"executor": "ns", "cacheKey": "ffff"}},
            ])
        );
    }

    #[test]
    fn an_unknown_source_is_null_not_guessed() {
        let mut i = input(&[], &[]);
        i.source_revision = None;
        let s = statement(&i);
        assert_eq!(
            s["predicate"]["buildDefinition"]["externalParameters"]["source"],
            json!({"revision": null, "dirty": null})
        );
    }

    fn layer(name: &str, executor: &str) -> CatalogLayer {
        CatalogLayer {
            digest: format!("sha256:{name}"),
            name: name.into(),
            version: "1.0".into(),
            executor: executor.into(),
        }
    }

    #[test]
    fn provenance_is_ns_with_ns_catalog_layer() {
        let steps = [
            step("web", "inputs-app", None, "sha256:1111", None),
            step("web", "build", Some("ffff"), "sha256:2222", Some("vm")),
        ];
        let executor = |catalog: &[CatalogLayer]| {
            let mut i = input(&[], &steps);
            i.catalog_index = Some("sha256:9999");
            i.catalog = catalog;
            statement(&i)["predicate"]["buildDefinition"]["internalParameters"].clone()
        };
        assert_eq!(executor(&[layer("os-min", "vm")])["executor"], "vm");
        let mixed = executor(&[layer("os-min", "vm"), layer("go", "ns")]);
        assert_eq!(mixed["executor"], "ns", "{mixed}");
        assert_eq!(mixed["catalogIndex"], "sha256:9999");
        // A step's ns layer is enough too, and only host layers claim no executor.
        let ns_step = [step("web", "build", Some("ffff"), "sha256:2222", Some("ns"))];
        let s = statement(&input(&[], &ns_step));
        assert_eq!(
            s["predicate"]["buildDefinition"]["internalParameters"]["executor"],
            "ns"
        );
        let host = [step("web", "inputs-app", None, "sha256:1111", None)];
        let s = statement(&input(&[], &host));
        assert_eq!(
            s["predicate"]["buildDefinition"]["internalParameters"]["executor"],
            "host"
        );
    }

    #[test]
    fn catalog_layers_are_resolved_dependencies() {
        let bases = [("busybox:1".to_string(), "sha256:bbbb".to_string())];
        let catalog = [layer("os-min", "vm"), layer("go", "ns")];
        let mut i = input(&bases, &[]);
        i.catalog = &catalog;
        let s = statement(&i);
        assert_eq!(
            s["predicate"]["buildDefinition"]["resolvedDependencies"],
            json!([
                {"uri": "busybox:1", "digest": {"sha256": "bbbb"}},
                {"name": "os-min", "digest": {"sha256": "os-min"}, "annotations": {"version": "1.0", "executor": "vm"}},
                {"name": "go", "digest": {"sha256": "go"}, "annotations": {"version": "1.0", "executor": "ns"}},
            ])
        );
    }
}
