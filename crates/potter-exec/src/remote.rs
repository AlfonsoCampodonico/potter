//! The remote cache (spec §8.3): each entry an OCI artifact at
//! `<registry>/<prefix>/<tenant>/cache:<key-hex>`, holding the step's layer.
//!
//! Entries come from other builders and are untrusted: blobs are digest-verified on fetch, and
//! a layer is checked the way kiln ingests layers before it is used.

use std::path::Path;

use kiln_oci::media::{OCI_LAYER_TAR, OCI_MANIFEST};
use kiln_oci::{Descriptor, ImageManifest, canonical_json};
use kiln_registry::{Client, DockerConfig, RegistryError};
use kiln_store::{Digest, Store};
use potter_cache::{CacheKey, Entry, ExecutorClass, Tenant};
use serde::{Deserialize, Serialize};

use crate::error::BuildError;

pub const ARTIFACT_TYPE: &str = "application/vnd.potter.cache.v1+json";
pub const CONFIG_TYPE: &str = "application/vnd.potter.cache.config.v1+json";

/// What an entry's config blob records (spec §8.3).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheConfig {
    class: ExecutorClass,
    potter: String,
    parent: String,
}

/// One tenant's remote cache.
pub struct RemoteCache {
    client: Client,
    registry: String,
    repository: String,
}

fn infra(e: impl std::fmt::Display) -> BuildError {
    BuildError::Infra(format!("the remote cache: {e}"))
}

fn hostile(e: impl std::fmt::Display) -> BuildError {
    BuildError::Hostile(format!("a remote cache entry: {e}"))
}

impl RemoteCache {
    /// `<registry>[/<prefix>]` for `tenant`.
    pub fn parse(spec: &str, tenant: &Tenant, auth: DockerConfig) -> Result<Self, BuildError> {
        let spec = spec.trim().trim_end_matches('/');
        let (registry, prefix) = spec.split_once('/').unwrap_or((spec, ""));
        if registry.is_empty() {
            return Err(BuildError::Plan(format!("the remote cache {spec:?} names no registry")));
        }
        let repository = [prefix, tenant.as_str(), "cache"]
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/");
        let client = Client::new(registry, auth).map_err(infra)?;
        Ok(Self {
            client,
            registry: registry.to_string(),
            repository,
        })
    }

    pub fn registry(&self) -> &str {
        &self.registry
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The entry for `key`, its blobs fetched into `store` and checked; `None` when the registry
    /// has none.
    pub fn get(&self, store: &Store, key: &CacheKey, spill_dir: &Path) -> Result<Option<Entry>, BuildError> {
        let manifest = match self.client.get_manifest(&self.repository, &key.hex()) {
            Ok(m) => m,
            Err(RegistryError::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(infra(e)),
        };
        let m: ImageManifest = serde_json::from_slice(&manifest.bytes).map_err(hostile)?;
        if m.artifact_type.as_deref() != Some(ARTIFACT_TYPE) || m.config.media_type != CONFIG_TYPE {
            return Err(hostile(format!("{} is not a potter cache entry", key.hex())));
        }
        if m.layers.len() > 1 {
            return Err(hostile("more than one layer"));
        }
        self.client
            .fetch_blob(&self.repository, &m.config.digest, m.config.size, store)
            .map_err(infra)?;
        let config: CacheConfig =
            serde_json::from_slice(&store.read_metadata(&m.config.digest).map_err(hostile)?).map_err(hostile)?;
        let layer = match m.layers.first() {
            None => None,
            Some(l) => {
                if l.media_type != OCI_LAYER_TAR {
                    return Err(hostile(format!("a layer of type {}", l.media_type)));
                }
                self.client
                    .fetch_blob(&self.repository, &l.digest, l.size, store)
                    .map_err(infra)?;
                let blob = store.open_blob(&l.digest).map_err(infra)?;
                crate::ingest::validate_layer(std::io::BufReader::new(blob), spill_dir)?;
                Some(l.digest.to_string())
            }
        };
        Ok(Some(Entry {
            layer,
            class: config.class,
            created: now_secs(),
            potter: config.potter,
        }))
    }

    /// Publishes `entry` for `key` (whose parent is `parent`), pushing its blobs from `store`.
    pub fn put(&self, store: &Store, key: &CacheKey, parent: &CacheKey, entry: &Entry) -> Result<(), BuildError> {
        let config = serde_json::to_vec(&CacheConfig {
            class: entry.class,
            potter: entry.potter.clone(),
            parent: parent.hex(),
        })
        .map_err(infra)?;
        let config_digest = store.put_bytes(&config).map_err(infra)?;
        self.client
            .push_blob(&self.repository, &config_digest, &store.blob_path(&config_digest))
            .map_err(infra)?;
        let mut layers = Vec::new();
        if let Some(l) = &entry.layer {
            let d = Digest::parse(l).map_err(infra)?;
            let size = store.blob_size(&d).map_err(infra)?;
            self.client
                .push_blob(&self.repository, &d, &store.blob_path(&d))
                .map_err(infra)?;
            layers.push(Descriptor::new(OCI_LAYER_TAR, d, size));
        }
        let manifest = ImageManifest {
            schema_version: 2,
            media_type: Some(OCI_MANIFEST.into()),
            artifact_type: Some(ARTIFACT_TYPE.into()),
            config: Descriptor::new(CONFIG_TYPE, config_digest, config.len() as u64),
            layers,
            annotations: None,
        };
        self.client
            .put_manifest(&self.repository, &key.hex(), OCI_MANIFEST, &canonical_json(&manifest))
            .map_err(infra)?;
        Ok(())
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
#[cfg(test)]
mod tests {
    use kiln_erofs::testtar::{Opts, TarBuilder};
    use kiln_registry::testregistry::{Config, TestRegistry};
    use kiln_store::Store;
    use potter_cache::{Entry, ExecutorClass, Tenant, base_key};

    use super::*;

    #[test]
    fn addresses_put_the_tenant_in_the_repository() {
        let t = Tenant::new("acme").unwrap();
        let r = RemoteCache::parse(
            "127.0.0.1:5000/build/cache",
            &t,
            kiln_registry::DockerConfig::anonymous(),
        )
        .unwrap();
        assert_eq!(
            (r.registry(), r.repository()),
            ("127.0.0.1:5000", "build/cache/acme/cache")
        );
        let r = RemoteCache::parse("127.0.0.1:5000", &t, kiln_registry::DockerConfig::anonymous()).unwrap();
        assert_eq!(r.repository(), "acme/cache");
        assert!(RemoteCache::parse("", &t, kiln_registry::DockerConfig::anonymous()).is_err());
    }

    fn layer(store: &Store) -> String {
        let mut t = TarBuilder::new();
        t.dir("app", &Opts::default().mode(0o755))
            .file("app/out", b"built", &Opts::default().mode(0o644));
        store.put_bytes(&t.finish()).unwrap().to_string()
    }

    #[test]
    fn an_entry_round_trips_between_stores_and_stays_in_its_tenant() {
        let reg = TestRegistry::start(Config::default());
        let a_dir = tempfile::tempdir().unwrap();
        let a = Store::open(a_dir.path()).unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let b = Store::open(b_dir.path()).unwrap();
        let acme = Tenant::new("acme").unwrap();
        let remote = RemoteCache::parse(
            &format!("{}/c", reg.host()),
            &acme,
            kiln_registry::DockerConfig::anonymous(),
        )
        .unwrap();
        let key = base_key("sha256:aa");
        let parent = base_key("sha256:p");
        let entry = Entry {
            layer: Some(layer(&a)),
            class: ExecutorClass::Ns,
            created: 1,
            potter: "0.1.0".into(),
        };
        remote.put(&a, &key, &parent, &entry).unwrap();
        let got = remote.get(&b, &key, b_dir.path()).unwrap().unwrap();
        assert_eq!(
            (got.layer.as_deref(), got.class),
            (entry.layer.as_deref(), ExecutorClass::Ns)
        );
        let d = kiln_store::Digest::parse(got.layer.as_deref().unwrap()).unwrap();
        assert!(b.has_blob(&d));
        assert!(
            remote
                .get(&b, &base_key("sha256:other"), b_dir.path())
                .unwrap()
                .is_none()
        );
        let other = RemoteCache::parse(
            &format!("{}/c", reg.host()),
            &Tenant::new("other").unwrap(),
            kiln_registry::DockerConfig::anonymous(),
        )
        .unwrap();
        assert!(
            other.get(&b, &key, b_dir.path()).unwrap().is_none(),
            "another tenant's entry"
        );
    }

    #[test]
    fn a_layer_that_does_not_ingest_is_refused() {
        let reg = TestRegistry::start(Config::default());
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let remote = RemoteCache::parse(
            &format!("{}/c", reg.host()),
            &Tenant::local(),
            kiln_registry::DockerConfig::anonymous(),
        )
        .unwrap();
        let garbage = store
            .put_bytes(b"not a tar stream at all, just bytes that are long enough")
            .unwrap();
        let key = base_key("sha256:bb");
        let entry = Entry {
            layer: Some(garbage.to_string()),
            class: ExecutorClass::Ns,
            created: 1,
            potter: "0.1.0".into(),
        };
        remote.put(&store, &key, &base_key("p"), &entry).unwrap();
        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = Store::open(fresh_dir.path()).unwrap();
        let err = remote.get(&fresh, &key, fresh_dir.path()).unwrap_err();
        assert_eq!(err.exit_code(), 3, "{err}");
    }
}
