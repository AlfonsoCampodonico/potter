//! Publishing (spec §17, §18): an image built into the kiln store pushed to a registry, and its
//! attestations attached as OCI referrers.
//!
//! Each attestation is an OCI artifact manifest whose `subject` is the image, with the
//! `dev.potter.attestation` annotation naming its kind. Every attach also maintains the
//! referrers tag-schema fallback (tag `sha256-<hex>`, an OCI index), so registries without the
//! referrers API serve them too and `potter verify` reads one place.

use std::collections::BTreeMap;

use kiln_oci::media::{OCI_INDEX, OCI_MANIFEST};
use kiln_oci::{Descriptor, ImageIndex, ImageManifest, canonical_json};
use kiln_registry::{Client, DockerConfig, Reference, RegistryError};
use kiln_store::{Digest, Store};
use serde_json::json;

use crate::error::BuildError;

/// The annotation naming an attestation's kind.
pub const KIND: &str = "dev.potter.attestation";
/// A signed attestation: a sigstore bundle.
pub const BUNDLE_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
/// An unsigned in-toto statement.
pub const STATEMENT_TYPE: &str = "application/vnd.in-toto+json";
pub const EMPTY_TYPE: &str = "application/vnd.oci.empty.v1+json";
/// The largest attestation potter reads back.
pub const MAX_ATTESTATION: u64 = 16 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Sbom,
    Provenance,
    Signature,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Sbom, Kind::Provenance, Kind::Signature];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Sbom => "sbom",
            Kind::Provenance => "provenance",
            Kind::Signature => "signature",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One attestation to attach: its kind, its media type ([`BUNDLE_TYPE`] or
/// [`STATEMENT_TYPE`]) and its bytes.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: Kind,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// A referrer listed in the fallback index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Referrer {
    pub kind: Option<Kind>,
    pub descriptor: Descriptor,
}

pub struct Publisher {
    client: Client,
    repository: String,
}

fn infra(e: impl std::fmt::Display) -> BuildError {
    BuildError::Infra(format!("publishing: {e}"))
}

fn hostile(e: impl std::fmt::Display) -> BuildError {
    BuildError::Hostile(format!("a referrer: {e}"))
}

/// The referrers fallback tag of `digest` (OCI distribution spec, "referrers tag schema").
pub fn fallback_tag(digest: &Digest) -> String {
    digest.to_string().replace(':', "-")
}

impl Publisher {
    pub fn new(reference: &Reference, auth: DockerConfig) -> Result<Self, BuildError> {
        Ok(Self {
            client: Client::new(reference.registry(), auth).map_err(infra)?,
            repository: reference.repository().to_string(),
        })
    }

    /// Pushes the image whose OCI manifest is `manifest_digest` in `store` (its config and
    /// layers first) under `tag`; returns the descriptor attestations name as their subject.
    pub fn push_image(&self, store: &Store, tag: &str, manifest_digest: &Digest) -> Result<Descriptor, BuildError> {
        let bytes = store.read_metadata(manifest_digest).map_err(infra)?;
        let m: ImageManifest = serde_json::from_slice(&bytes).map_err(infra)?;
        for d in std::iter::once(&m.config).chain(&m.layers) {
            self.client
                .push_blob(&self.repository, &d.digest, &store.blob_path(&d.digest))
                .map_err(infra)?;
        }
        let pushed = self
            .client
            .put_manifest(&self.repository, tag, OCI_MANIFEST, &bytes)
            .map_err(infra)?;
        if &pushed != manifest_digest {
            return Err(infra(format!("the registry stored {pushed}, not {manifest_digest}")));
        }
        Ok(Descriptor::new(OCI_MANIFEST, pushed, bytes.len() as u64))
    }

    /// Attaches `attachments` to `subject`; returns their manifest digests. An attestation of a
    /// kind already attached replaces it in the fallback index; other referrers are kept.
    pub fn attach(
        &self,
        store: &Store,
        subject: &Descriptor,
        attachments: &[Attachment],
    ) -> Result<Vec<Digest>, BuildError> {
        let empty = store.put_bytes(b"{}").map_err(infra)?;
        self.client
            .push_blob(&self.repository, &empty, &store.blob_path(&empty))
            .map_err(infra)?;
        let mut added = Vec::new();
        for a in attachments {
            let blob = store.put_bytes(&a.bytes).map_err(infra)?;
            self.client
                .push_blob(&self.repository, &blob, &store.blob_path(&blob))
                .map_err(infra)?;
            // serde_json's map is ordered, so the manifest (and its digest) is deterministic.
            let manifest = json!({
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST,
                "artifactType": a.media_type,
                "config": {"mediaType": EMPTY_TYPE, "digest": empty.to_string(), "size": 2},
                "layers": [{"mediaType": a.media_type, "digest": blob.to_string(), "size": a.bytes.len()}],
                "subject": {"mediaType": subject.media_type, "digest": subject.digest.to_string(), "size": subject.size},
                "annotations": {KIND: a.kind.as_str()},
            });
            let bytes = serde_json::to_vec(&manifest).map_err(infra)?;
            let d = store.put_bytes(&bytes).map_err(infra)?;
            self.client
                .put_manifest(&self.repository, &d.to_string(), OCI_MANIFEST, &bytes)
                .map_err(infra)?;
            let mut desc = Descriptor::new(OCI_MANIFEST, d, bytes.len() as u64);
            desc.artifact_type = Some(a.media_type.clone());
            desc.annotations = Some(BTreeMap::from([(KIND.to_string(), a.kind.as_str().to_string())]));
            added.push((a.kind, desc));
        }
        let mut manifests: Vec<Descriptor> = self
            .referrers(&subject.digest)?
            .into_iter()
            .filter(|r| {
                !added
                    .iter()
                    .any(|(k, d)| r.kind == Some(*k) || r.descriptor.digest == d.digest)
            })
            .map(|r| r.descriptor)
            .collect();
        manifests.extend(added.iter().map(|(_, d)| d.clone()));
        let index = ImageIndex {
            schema_version: 2,
            media_type: Some(OCI_INDEX.into()),
            artifact_type: None,
            manifests,
            annotations: None,
        };
        self.client
            .put_manifest(
                &self.repository,
                &fallback_tag(&subject.digest),
                OCI_INDEX,
                &canonical_json(&index),
            )
            .map_err(infra)?;
        Ok(added.into_iter().map(|(_, d)| d.digest).collect())
    }

    /// The referrers of `subject` listed in its fallback index (none when it has no index).
    pub fn referrers(&self, subject: &Digest) -> Result<Vec<Referrer>, BuildError> {
        let m = match self.client.get_manifest(&self.repository, &fallback_tag(subject)) {
            Ok(m) => m,
            Err(RegistryError::NotFound { .. }) => return Ok(Vec::new()),
            Err(e) => return Err(infra(e)),
        };
        let index: ImageIndex = serde_json::from_slice(&m.bytes).map_err(hostile)?;
        Ok(index
            .manifests
            .into_iter()
            .map(|descriptor| Referrer {
                kind: descriptor
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get(KIND))
                    .and_then(|k| Kind::parse(k)),
                descriptor,
            })
            .collect())
    }

    /// The attestation a referrer carries: its media type and its one layer, at most
    /// [`MAX_ATTESTATION`] bytes.
    pub fn fetch(&self, store: &Store, referrer: &Descriptor) -> Result<(String, Vec<u8>), BuildError> {
        let m = self
            .client
            .get_manifest(&self.repository, &referrer.digest.to_string())
            .map_err(infra)?;
        let m: ImageManifest = serde_json::from_slice(&m.bytes).map_err(hostile)?;
        let [layer] = m.layers.as_slice() else {
            return Err(hostile(format!("{} has {} layers", referrer.digest, m.layers.len())));
        };
        if layer.size > MAX_ATTESTATION {
            return Err(hostile(format!("{} is over {MAX_ATTESTATION} bytes", referrer.digest)));
        }
        self.client
            .fetch_blob(&self.repository, &layer.digest, layer.size, store)
            .map_err(infra)?;
        let bytes = std::fs::read(store.blob_path(&layer.digest)).map_err(infra)?;
        Ok((layer.media_type.clone(), bytes))
    }

    /// The manifest `target` (a tag or digest) resolves to.
    pub fn resolve(&self, target: &str) -> Result<Descriptor, BuildError> {
        let m = self.client.get_manifest(&self.repository, target).map_err(infra)?;
        Ok(Descriptor::new(&m.media_type, m.digest, m.bytes.len() as u64))
    }
}

#[cfg(test)]
mod tests {
    use kiln_oci::media::{OCI_CONFIG, OCI_LAYER_TAR};
    use kiln_registry::testregistry::{Config, TestRegistry};

    use super::*;

    fn image(store: &Store) -> Digest {
        let layer = store.put_bytes(b"layer bytes").unwrap();
        let config = store.put_bytes(br#"{"architecture":"amd64","os":"linux"}"#).unwrap();
        let m = ImageManifest {
            schema_version: 2,
            media_type: Some(OCI_MANIFEST.into()),
            artifact_type: None,
            config: Descriptor::new(OCI_CONFIG, config, 37),
            layers: vec![Descriptor::new(OCI_LAYER_TAR, layer, 11)],
            annotations: None,
        };
        store.put_bytes(&canonical_json(&m)).unwrap()
    }

    fn setup() -> (TestRegistry, tempfile::TempDir, Store, Publisher, Descriptor) {
        let reg = TestRegistry::start(Config::default());
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse(&format!("{}/acme/app:v1", reg.host())).unwrap();
        let p = Publisher::new(&reference, DockerConfig::anonymous()).unwrap();
        let d = image(&store);
        let subject = p.push_image(&store, "v1", &d).unwrap();
        (reg, dir, store, p, subject)
    }

    fn att(kind: Kind, body: &str) -> Attachment {
        Attachment {
            kind,
            media_type: STATEMENT_TYPE.into(),
            bytes: body.as_bytes().to_vec(),
        }
    }

    fn kinds(p: &Publisher, subject: &Descriptor) -> Vec<Option<Kind>> {
        let mut k: Vec<_> = p
            .referrers(&subject.digest)
            .unwrap()
            .into_iter()
            .map(|r| r.kind)
            .collect();
        k.sort();
        k
    }

    #[test]
    fn the_image_is_pushed_under_its_tag_with_its_blobs() {
        let (reg, _dir, store, p, subject) = setup();
        assert_eq!(subject.media_type, OCI_MANIFEST);
        assert_eq!(p.resolve("v1").unwrap().digest, subject.digest);
        let m: ImageManifest = serde_json::from_slice(&store.read_metadata(&subject.digest).unwrap()).unwrap();
        assert!(reg.has_blob(&m.config.digest) && reg.has_blob(&m.layers[0].digest));
    }

    #[test]
    fn attestations_are_listed_and_read_back_and_reattaching_is_idempotent() {
        let (_reg, _dir, store, p, subject) = setup();
        assert!(p.referrers(&subject.digest).unwrap().is_empty());
        let all: Vec<_> = Kind::ALL.iter().map(|&k| att(k, k.as_str())).collect();
        let first = p.attach(&store, &subject, &all).unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(
            kinds(&p, &subject),
            [Some(Kind::Sbom), Some(Kind::Provenance), Some(Kind::Signature)]
        );
        let again = p.attach(&store, &subject, &all).unwrap();
        assert_eq!(first, again);
        assert_eq!(p.referrers(&subject.digest).unwrap().len(), 3);
        let sbom = p
            .referrers(&subject.digest)
            .unwrap()
            .into_iter()
            .find(|r| r.kind == Some(Kind::Sbom))
            .unwrap();
        assert_eq!(
            p.fetch(&store, &sbom.descriptor).unwrap(),
            (STATEMENT_TYPE.to_string(), b"sbom".to_vec())
        );
    }

    #[test]
    fn a_new_attestation_replaces_its_kind_and_keeps_foreign_referrers() {
        let (reg, _dir, store, p, subject) = setup();
        let foreign = Descriptor::new(OCI_MANIFEST, Digest::of(b"someone else"), 12);
        let index = ImageIndex {
            schema_version: 2,
            media_type: Some(OCI_INDEX.into()),
            artifact_type: None,
            manifests: vec![foreign.clone()],
            annotations: None,
        };
        reg.put_manifest(&fallback_tag(&subject.digest), OCI_INDEX, &canonical_json(&index));
        p.attach(&store, &subject, &[att(Kind::Sbom, "old")]).unwrap();
        let new = p.attach(&store, &subject, &[att(Kind::Sbom, "new")]).unwrap();
        let refs = p.referrers(&subject.digest).unwrap();
        assert_eq!(refs.len(), 2, "{refs:?}");
        assert!(
            refs.iter()
                .any(|r| r.kind.is_none() && r.descriptor.digest == foreign.digest)
        );
        let sbom = refs.iter().find(|r| r.kind == Some(Kind::Sbom)).unwrap();
        assert_eq!(sbom.descriptor.digest, new[0]);
        assert_eq!(p.fetch(&store, &sbom.descriptor).unwrap().1, b"new");
    }

    #[test]
    fn an_attestation_manifest_names_its_subject_and_kind() {
        let (_reg, _dir, store, p, subject) = setup();
        let d = p.attach(&store, &subject, &[att(Kind::Provenance, "{}")]).unwrap();
        let m: serde_json::Value = serde_json::from_slice(&store.read_metadata(&d[0]).unwrap()).unwrap();
        assert_eq!(m["subject"]["digest"], subject.digest.to_string());
        assert_eq!(m["artifactType"], STATEMENT_TYPE);
        assert_eq!(m["annotations"][KIND], "provenance");
        assert_eq!(m["config"]["mediaType"], EMPTY_TYPE);
    }

    #[test]
    fn an_oversized_attestation_is_refused_on_read() {
        let (reg, _dir, store, p, subject) = setup();
        let d = p.attach(&store, &subject, &[att(Kind::Sbom, "x")]).unwrap();
        let m: serde_json::Value = serde_json::from_slice(&store.read_metadata(&d[0]).unwrap()).unwrap();
        let mut tampered = m.clone();
        tampered["layers"][0]["size"] = json!(MAX_ATTESTATION + 1);
        let bytes = serde_json::to_vec(&tampered).unwrap();
        let td = reg.put_manifest(&Digest::of(&bytes).to_string(), OCI_MANIFEST, &bytes);
        let e = p
            .fetch(&store, &Descriptor::new(OCI_MANIFEST, td, bytes.len() as u64))
            .unwrap_err();
        assert_eq!(e.exit_code(), 3, "{e}");
    }

    #[test]
    fn kinds_round_trip() {
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("other"), None);
    }
}
