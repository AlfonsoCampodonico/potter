//! The image SBOM: an SPDX 2.3 JSON document assembled from data potter already holds — the
//! bases, the catalog (or stopgap) layers and the declared lockfiles — never from a scan and
//! never from anything a step reported (spec §17).

use serde_json::{Value, json};

use crate::lockfiles::Component;

pub struct SbomInput<'a> {
    pub image_name: &'a str,
    /// `sha256:<hex>` of the image manifest.
    pub image_digest: &'a str,
    /// RFC 3339.
    pub created: &'a str,
    /// (reference, `sha256:<hex>` manifest digest) of each base image.
    pub bases: &'a [(String, String)],
    /// (name, version) of each catalog or stopgap layer; `None` when nothing pins it.
    pub catalog: &'a [(String, Option<String>)],
    pub components: &'a [Component],
    pub potter_version: &'a str,
}

const NOASSERTION: &str = "NOASSERTION";

/// The SPDX document. Packages are sorted before they are numbered, so the document depends
/// only on its inputs, not their order.
pub fn spdx(input: &SbomInput) -> Value {
    let mut bases = input.bases.to_vec();
    bases.sort();
    let mut catalog = input.catalog.to_vec();
    catalog.sort();
    let mut components = input.components.to_vec();
    components.sort();

    let hex = input.image_digest.strip_prefix("sha256:").unwrap_or(input.image_digest);
    let mut image = package("SPDXRef-Image", input.image_name, input.image_digest);
    image["checksums"] = json!([{"algorithm": "SHA256", "checksumValue": hex}]);
    image["primaryPackagePurpose"] = json!("CONTAINER");
    let mut packages = vec![image];
    for (i, (reference, digest)) in bases.iter().enumerate() {
        let mut p = package(&format!("SPDXRef-Base-{}", i + 1), reference, digest);
        p["primaryPackagePurpose"] = json!("CONTAINER");
        packages.push(p);
    }
    for (i, (name, version)) in catalog.iter().enumerate() {
        let version = version.as_deref().unwrap_or(NOASSERTION);
        packages.push(package(&format!("SPDXRef-Catalog-{}", i + 1), name, version));
    }
    for (i, c) in components.iter().enumerate() {
        let mut p = package(&format!("SPDXRef-Pkg-{}", i + 1), &c.name, &c.version);
        p["sourceInfo"] = json!(format!("lockfile {}", c.source));
        p["externalRefs"] = json!([{
            "referenceCategory": "PACKAGE-MANAGER",
            "referenceType": "purl",
            "referenceLocator": c.purl(),
        }]);
        packages.push(p);
    }

    let mut relationships = vec![json!({
        "spdxElementId": "SPDXRef-DOCUMENT",
        "relationshipType": "DESCRIBES",
        "relatedSpdxElement": "SPDXRef-Image",
    })];
    relationships.extend(packages[1..].iter().map(|p| {
        json!({
            "spdxElementId": "SPDXRef-Image",
            "relationshipType": "CONTAINS",
            "relatedSpdxElement": p["SPDXID"],
        })
    }));

    json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": input.image_name,
        "documentNamespace": format!("https://potter.dev/spdx/{}", input.image_digest),
        "creationInfo": {
            "created": input.created,
            "creators": [format!("Tool: potter-{}", input.potter_version)],
        },
        "packages": packages,
        "relationships": relationships,
    })
}

/// A package with every field potter has no evidence for set to `NOASSERTION`.
fn package(id: &str, name: &str, version: &str) -> Value {
    json!({
        "SPDXID": id,
        "name": name,
        "versionInfo": version,
        "downloadLocation": NOASSERTION,
        "filesAnalyzed": false,
        "licenseConcluded": NOASSERTION,
        "licenseDeclared": NOASSERTION,
        "copyrightText": NOASSERTION,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfiles::Ecosystem;

    fn comp(eco: Ecosystem, name: &str, v: &str) -> Component {
        Component {
            ecosystem: eco,
            name: name.into(),
            version: v.into(),
            source: "composer.lock".into(),
        }
    }

    fn input<'a>(
        bases: &'a [(String, String)],
        catalog: &'a [(String, Option<String>)],
        components: &'a [Component],
    ) -> SbomInput<'a> {
        SbomInput {
            image_name: "registry.example/app/web",
            image_digest: "sha256:aaaa",
            created: "2026-10-11T00:00:00Z",
            bases,
            catalog,
            components,
            potter_version: "0.1.0",
        }
    }

    fn package<'a>(doc: &'a Value, id: &str) -> &'a Value {
        doc["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["SPDXID"] == id)
            .unwrap_or_else(|| panic!("no package {id}"))
    }

    #[test]
    fn the_document_header_follows_spdx_2_3() {
        let doc = spdx(&input(&[], &[], &[]));
        assert_eq!(doc["spdxVersion"], "SPDX-2.3");
        assert_eq!(doc["dataLicense"], "CC0-1.0");
        assert_eq!(doc["SPDXID"], "SPDXRef-DOCUMENT");
        assert_eq!(doc["name"], "registry.example/app/web");
        assert_eq!(doc["documentNamespace"], "https://potter.dev/spdx/sha256:aaaa");
        assert_eq!(doc["creationInfo"]["created"], "2026-10-11T00:00:00Z");
        assert_eq!(doc["creationInfo"]["creators"], json!(["Tool: potter-0.1.0"]));
        let image = package(&doc, "SPDXRef-Image");
        assert_eq!(image["versionInfo"], "sha256:aaaa");
        assert_eq!(
            image["checksums"],
            json!([{"algorithm": "SHA256", "checksumValue": "aaaa"}])
        );
        assert_eq!(image["primaryPackagePurpose"], "CONTAINER");
        assert_eq!(
            doc["relationships"],
            json!([{"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES", "relatedSpdxElement": "SPDXRef-Image"}])
        );
    }

    #[test]
    fn every_package_is_contained_and_unproven_fields_are_noassertion() {
        let bases = [("php:8.4-fpm-trixie".to_string(), "sha256:bbbb".to_string())];
        let catalog = [
            ("nginx".to_string(), None),
            ("php".to_string(), Some("8.4".to_string())),
        ];
        let components = [comp(Ecosystem::Composer, "laravel/framework", "v11.9.2")];
        let doc = spdx(&input(&bases, &catalog, &components));
        let base = package(&doc, "SPDXRef-Base-1");
        assert_eq!(base["name"], "php:8.4-fpm-trixie");
        assert_eq!(base["versionInfo"], "sha256:bbbb");
        assert_eq!(package(&doc, "SPDXRef-Catalog-1")["versionInfo"], NOASSERTION);
        assert_eq!(package(&doc, "SPDXRef-Catalog-2")["versionInfo"], "8.4");
        let pkg = package(&doc, "SPDXRef-Pkg-1");
        assert_eq!(pkg["name"], "laravel/framework");
        assert_eq!(pkg["versionInfo"], "v11.9.2");
        assert_eq!(pkg["sourceInfo"], "lockfile composer.lock");
        assert_eq!(
            pkg["externalRefs"],
            json!([{"referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl", "referenceLocator": "pkg:composer/laravel/framework@v11.9.2"}])
        );
        for p in doc["packages"].as_array().unwrap() {
            for field in [
                "downloadLocation",
                "licenseConcluded",
                "licenseDeclared",
                "copyrightText",
            ] {
                assert_eq!(p[field], NOASSERTION, "{field} of {}", p["SPDXID"]);
            }
            assert_eq!(p["filesAnalyzed"], false);
        }
        let contains: Vec<&str> = doc["relationships"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["relationshipType"] == "CONTAINS")
            .map(|r| {
                assert_eq!(r["spdxElementId"], "SPDXRef-Image");
                r["relatedSpdxElement"].as_str().unwrap()
            })
            .collect();
        assert_eq!(
            contains,
            [
                "SPDXRef-Base-1",
                "SPDXRef-Catalog-1",
                "SPDXRef-Catalog-2",
                "SPDXRef-Pkg-1"
            ]
        );
    }

    #[test]
    fn the_document_does_not_depend_on_input_order() {
        let bases = [
            ("b".to_string(), "sha256:2".to_string()),
            ("a".to_string(), "sha256:1".to_string()),
        ];
        let catalog = [("z".to_string(), None), ("a".to_string(), None)];
        let components = [
            comp(Ecosystem::Npm, "zod", "3.0.0"),
            comp(Ecosystem::Composer, "a/b", "1.0.0"),
        ];
        let mut bases_r = bases.clone();
        bases_r.reverse();
        let mut catalog_r = catalog.clone();
        catalog_r.reverse();
        let mut components_r = components.clone();
        components_r.reverse();
        assert_eq!(
            spdx(&input(&bases, &catalog, &components)),
            spdx(&input(&bases_r, &catalog_r, &components_r))
        );
        let doc = spdx(&input(&bases, &catalog, &components));
        assert_eq!(package(&doc, "SPDXRef-Base-1")["name"], "a");
        assert_eq!(package(&doc, "SPDXRef-Pkg-1")["name"], "a/b");
    }
}
