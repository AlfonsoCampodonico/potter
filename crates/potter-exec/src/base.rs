//! OCI bases (spec §15 P2): images resolved and fetched through kiln's registry client.

use std::collections::BTreeMap;
use std::io::Read;

use kiln_oci::media::{self, Compression};
use kiln_oci::{ContainerConfig, Descriptor, OciError, Platform, ResolvedImage};
use kiln_registry::{Client, DockerConfig, Reference, RegistryError};
use kiln_store::{Digest, Store};

use crate::error::{BuildError, io};

/// A base image, resolved for one platform, with every layer in the store.
#[derive(Debug, Clone)]
pub struct BaseImage {
    pub resolved: ResolvedImage,
    /// The repository it came from.
    pub repository: String,
}

impl BaseImage {
    /// The manifest digest (`sha256:...`): the chain's cache root.
    pub fn key_digest(&self) -> String {
        self.resolved.manifest_digest.to_string()
    }

    pub fn layers(&self) -> &[Descriptor] {
        &self.resolved.manifest.layers
    }

    /// The uncompressed digests of the layers, in order.
    pub fn diff_ids(&self) -> &[Digest] {
        &self.resolved.config.rootfs.diff_ids
    }

    fn container(&self) -> Option<&ContainerConfig> {
        self.resolved.config.config.as_ref()
    }

    /// The image's environment.
    pub fn env(&self) -> BTreeMap<String, String> {
        self.container()
            .and_then(|c| c.env.as_ref())
            .into_iter()
            .flatten()
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect()
    }

    pub fn workdir(&self) -> Option<String> {
        self.container()
            .and_then(|c| c.working_dir.clone())
            .filter(|w| !w.is_empty())
    }

    pub fn user(&self) -> Option<String> {
        self.container().and_then(|c| c.user.clone()).filter(|u| !u.is_empty())
    }
}

/// Resolves `reference` (pinned to `digest` when given) for `platform` and fetches its layers
/// into `store`. Plan mistakes (a bad reference, a pinned digest the registry does not have,
/// no image for the platform) exit 2; everything else about the registry exits 4.
pub fn resolve_base(
    store: &Store,
    reference: &str,
    digest: Option<&str>,
    platform: &Platform,
    auth: DockerConfig,
) -> Result<BaseImage, BuildError> {
    let plan = |e: &dyn std::fmt::Display| BuildError::Plan(format!("the base {reference}: {e}"));
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("the base {reference}: {e}"));
    let mut r = Reference::parse(reference).map_err(|e| plan(&e))?;
    if let Some(d) = digest {
        Digest::parse(d).map_err(|e| plan(&e))?;
        let pinned = format!("{}/{}@{d}", r.registry(), r.repository());
        r = Reference::parse(&pinned).map_err(|e| plan(&e))?;
    }
    let client = Client::new(r.registry(), auth).map_err(|e| infra(&e))?;
    let images =
        kiln_oci::resolve_registry(store, &client, &r, std::slice::from_ref(platform)).map_err(|e| match &e {
            OciError::MissingPlatform { .. } => plan(&e),
            OciError::Registry(RegistryError::NotFound { .. }) if digest.is_some() => {
                plan(&format!("the pinned digest is not in the registry ({e})"))
            }
            _ => infra(&e),
        })?;
    let resolved = images
        .into_iter()
        .next()
        .ok_or_else(|| plan(&format!("no image for {platform}")))?;
    let repository = r.repository().to_string();
    for layer in &resolved.manifest.layers {
        media::layer_compression(&layer.media_type).map_err(|e| plan(&e))?;
        client
            .fetch_blob(&repository, &layer.digest, layer.size, store)
            .map_err(|e| infra(&e))?;
    }
    Ok(BaseImage { resolved, repository })
}

/// The uncompressed tar stream of `layer`, a blob in `store`.
pub fn layer_reader(store: &Store, layer: &Descriptor) -> Result<Box<dyn Read + Send>, BuildError> {
    let compression = media::layer_compression(&layer.media_type)
        .map_err(|e| BuildError::Plan(format!("layer {}: {e}", layer.digest)))?;
    let file = store
        .open_blob(&layer.digest)
        .map_err(|e| BuildError::Infra(format!("layer {}: {e}", layer.digest)))?;
    decompress(file, compression).map_err(io(format!("reading layer {}", layer.digest)))
}

/// `blob` decompressed as `compression` says.
pub fn decompress(blob: impl Read + Send + 'static, compression: Compression) -> std::io::Result<Box<dyn Read + Send>> {
    let blob = std::io::BufReader::new(blob);
    Ok(match compression {
        Compression::None => Box::new(blob),
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(blob)),
        Compression::Zstd => Box::new(zstd::Decoder::with_buffer(blob)?),
    })
}

#[cfg(test)]
mod tests {
    use kiln_oci::Platform;
    use kiln_registry::DockerConfig;
    use kiln_store::Store;

    use super::*;
    use crate::testing::registry_with_image;

    #[test]
    fn a_base_resolves_fetches_its_layers_and_reads_them_back() {
        let (reg, _dir) = registry_with_image("1", &[("bin/busybox", b"elf")], &["PATH=/bin", "A=1"], "/srv");
        let store_dir = tempfile::tempdir().unwrap();
        let store = Store::open(store_dir.path()).unwrap();
        let r = format!("{}/lib/busybox:1", reg.host());
        let base = resolve_base(&store, &r, None, &Platform::host(), DockerConfig::anonymous()).unwrap();
        assert!(base.key_digest().starts_with("sha256:"));
        assert_eq!(base.env().get("A").map(String::as_str), Some("1"));
        assert_eq!(base.workdir().as_deref(), Some("/srv"));
        assert_eq!(base.layers().len(), 1);
        assert!(store.has_blob(&base.layers()[0].digest));
        let mut tar = tar::Archive::new(layer_reader(&store, &base.layers()[0]).unwrap());
        let names: Vec<String> = tar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        assert!(names.contains(&"bin/busybox".to_string()), "{names:?}");
    }

    #[test]
    fn a_pinned_digest_is_used_and_a_wrong_one_is_a_plan_error() {
        let (reg, _dir) = registry_with_image("1", &[("x", b"x")], &[], "/");
        let store_dir = tempfile::tempdir().unwrap();
        let store = Store::open(store_dir.path()).unwrap();
        let r = format!("{}/lib/busybox:1", reg.host());
        let base = resolve_base(&store, &r, None, &Platform::host(), DockerConfig::anonymous()).unwrap();
        let pinned = resolve_base(
            &store,
            &r,
            Some(&base.key_digest()),
            &Platform::host(),
            DockerConfig::anonymous(),
        )
        .unwrap();
        assert_eq!(pinned.key_digest(), base.key_digest());
        let wrong = format!("sha256:{}", "0".repeat(64));
        let err = resolve_base(&store, &r, Some(&wrong), &Platform::host(), DockerConfig::anonymous()).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
    }

    #[test]
    fn a_missing_platform_and_a_bad_reference_are_plan_errors() {
        let (reg, _dir) = registry_with_image("1", &[("x", b"x")], &[], "/");
        let store_dir = tempfile::tempdir().unwrap();
        let store = Store::open(store_dir.path()).unwrap();
        let r = format!("{}/lib/busybox:1", reg.host());
        let other = Platform::parse("linux/s390x").unwrap();
        assert_eq!(
            resolve_base(&store, &r, None, &other, DockerConfig::anonymous())
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            resolve_base(
                &store,
                "UPPER/Case::x",
                None,
                &Platform::host(),
                DockerConfig::anonymous()
            )
            .unwrap_err()
            .exit_code(),
            2
        );
    }
}
