//! Loading a verified index from a catalog location, fetching its layers, and writing and pushing
//! catalog layouts.
//!
//! The index is an OCI image manifest (`artifactType` [`INDEX_TYPE`]) whose one layer is the index
//! JSON and whose annotations carry its signature ([`SIGNATURE`], [`KEY`]). It is tagged
//! `index`, and also `index-<blob hex>` so a pinned index stays fetchable after a newer one is
//! published. Each catalog layer is kept referenced by its own manifest, tagged `layer-<hex>`.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use kiln_oci::media::{OCI_INDEX, OCI_MANIFEST};
use kiln_oci::{BlobSource, Descriptor, DirLayout, ImageIndex, ImageManifest, OciError, REF_NAME, canonical_json};
use kiln_registry::{Client, DockerConfig, MAX_MANIFEST, Reference, RegistryError};
use kiln_store::{Digest, HashingReader, Store, StoreError};
use sha2::{Digest as _, Sha256};

use crate::location::INDEX_TAG;
use crate::{CatalogError, Entry, Index, Location, MAX_INDEX_BYTES, TrustedKey, trusted_keys, verify};

/// The index artifact's `artifactType`, and its one layer's media type.
pub const INDEX_TYPE: &str = "application/vnd.potter.catalog.index.v1+json";
/// The `artifactType` of the manifests that keep each catalog layer referenced in a registry.
pub const LAYER_TYPE: &str = "application/vnd.potter.catalog.layer.v1";
/// Manifest annotation: base64 ed25519 signature over the SHA-256 of the index blob.
pub const SIGNATURE: &str = "dev.potter.catalog.signature";
/// Manifest annotation: the id of the key that made [`SIGNATURE`].
pub const KEY: &str = "dev.potter.catalog.key";
const EMPTY_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY: &[u8] = b"{}";
/// Store refs `potter-catalog/index/<blob hex>` name the manifest of every verified index, so a
/// pinned index (and its signature) is found offline and survives the store's GC.
const STORE_REF: &str = "potter-catalog/index/";

/// An index whose signature verified, and where it came from.
#[derive(Debug, Clone)]
pub struct Verified {
    pub index: Index,
    /// The index blob's digest (`sha256:<hex>`): what `--catalog-index` pins.
    pub digest: String,
    pub location: Location,
}

fn hostile(loc: &Location, e: impl Display) -> CatalogError {
    CatalogError::Hostile(format!("catalog {loc}: {e}"))
}

fn infra(loc: &Location, e: impl Display) -> CatalogError {
    CatalogError::Infra(format!("catalog {loc}: {e}"))
}

/// Names `loc` in an error that does not yet.
fn at(loc: &Location, e: CatalogError) -> CatalogError {
    match e {
        CatalogError::Hostile(m) => hostile(loc, m),
        CatalogError::Infra(m) => infra(loc, m),
        other => other,
    }
}

fn from_registry(loc: &Location, e: RegistryError) -> CatalogError {
    use RegistryError as R;
    match &e {
        R::DigestMismatch { .. }
        | R::TooLarge { .. }
        | R::Schema1
        | R::UnsupportedManifest(_)
        | R::BadResponse { .. }
        | R::Store(StoreError::DigestMismatch { .. } | StoreError::SizeMismatch { .. }) => hostile(loc, e),
        _ => infra(loc, e),
    }
}

fn from_store(loc: &Location, e: StoreError) -> CatalogError {
    match e {
        StoreError::DigestMismatch { .. } | StoreError::SizeMismatch { .. } => hostile(loc, e),
        _ => infra(loc, e),
    }
}

fn from_oci(loc: &Location, e: OciError) -> CatalogError {
    match e {
        OciError::MetadataTooLarge { .. } | OciError::BadArchive(_) | OciError::Json { .. } => hostile(loc, e),
        _ => infra(loc, e),
    }
}

fn pinned_tag(d: &Digest) -> String {
    format!("{INDEX_TAG}-{}", d.hex())
}

fn layer_tag(d: &Digest) -> String {
    format!("layer-{}", d.hex())
}

/// Where a location's manifests and blobs come from.
enum Source {
    Registry { client: Box<Client>, repository: String },
    Layout(DirLayout),
}

impl Source {
    fn open(loc: &Location, auth: DockerConfig) -> Result<Source, CatalogError> {
        match loc {
            Location::Registry(r) => Ok(Source::Registry {
                client: Box::new(Client::new(r.registry(), auth).map_err(|e| from_registry(loc, e))?),
                repository: r.repository().to_string(),
            }),
            Location::Layout(dir) => DirLayout::open(dir).map(Source::Layout).map_err(|e| from_oci(loc, e)),
        }
    }

    /// The manifest tagged (or, in a layout, ref-named) `name`.
    fn manifest(&self, loc: &Location, name: &str) -> Result<Vec<u8>, CatalogError> {
        match self {
            Source::Registry { client, repository } => client
                .get_manifest(repository, name)
                .map(|m| m.bytes)
                .map_err(|e| from_registry(loc, e)),
            Source::Layout(layout) => {
                let bytes = layout
                    .read_small("index.json", MAX_MANIFEST)
                    .map_err(|e| from_oci(loc, e))?;
                let index: ImageIndex =
                    serde_json::from_slice(&bytes).map_err(|e| hostile(loc, format!("index.json: {e}")))?;
                let d = index
                    .manifests
                    .iter()
                    .find(|m| m.annotation(REF_NAME) == Some(name))
                    .ok_or_else(|| infra(loc, format!("the layout has no manifest named {name:?}")))?;
                if d.size > MAX_MANIFEST {
                    return Err(hostile(
                        loc,
                        format!("manifest {} is over {MAX_MANIFEST} bytes", d.digest),
                    ));
                }
                let mut bytes = Vec::new();
                layout
                    .open_blob(&d.digest)
                    .map_err(|e| from_oci(loc, e))?
                    .take(MAX_MANIFEST + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| infra(loc, e))?;
                let actual = Digest::of(&bytes);
                if actual != d.digest {
                    return Err(hostile(
                        loc,
                        format!("manifest digest mismatch: expected {}, got {actual}", d.digest),
                    ));
                }
                Ok(bytes)
            }
        }
    }

    /// Puts blob `d` in `store`; the store commits it only if it has `size` bytes and hashes to `d`.
    fn blob(&self, loc: &Location, d: &Digest, size: u64, store: &Store) -> Result<(), CatalogError> {
        match self {
            Source::Registry { client, repository } => client
                .fetch_blob(repository, d, size, store)
                .map(drop)
                .map_err(|e| from_registry(loc, e)),
            Source::Layout(layout) => {
                let mut r = layout.open_blob(d).map_err(|e| from_oci(loc, e))?;
                store
                    .put_verified(&mut r, d, Some(size))
                    .map(drop)
                    .map_err(|e| from_store(loc, e))
            }
        }
    }
}

/// The index layer, key id and signature an index manifest names.
fn check_manifest(loc: &Location, bytes: &[u8]) -> Result<(Descriptor, String, String), CatalogError> {
    let m: ImageManifest =
        serde_json::from_slice(bytes).map_err(|e| hostile(loc, format!("the index manifest does not parse: {e}")))?;
    if m.artifact_type.as_deref() != Some(INDEX_TYPE) {
        return Err(hostile(
            loc,
            format!(
                "the index manifest's artifactType is {:?}, not {INDEX_TYPE}",
                m.artifact_type
            ),
        ));
    }
    let [layer] = m.layers.as_slice() else {
        return Err(hostile(
            loc,
            format!("the index manifest has {} layers, not 1", m.layers.len()),
        ));
    };
    if layer.media_type != INDEX_TYPE {
        return Err(hostile(
            loc,
            format!("the index layer has media type {:?}", layer.media_type),
        ));
    }
    if layer.size > MAX_INDEX_BYTES {
        return Err(hostile(loc, format!("the index is over {MAX_INDEX_BYTES} bytes")));
    }
    let (Some(key), Some(sig)) = (
        m.annotations.as_ref().and_then(|a| a.get(KEY)),
        m.annotations.as_ref().and_then(|a| a.get(SIGNATURE)),
    ) else {
        return Err(hostile(loc, "the index manifest is not signed"));
    };
    Ok((layer.clone(), key.clone(), sig.clone()))
}

/// Reads blob `d` from `store`, at most [`MAX_INDEX_BYTES`].
fn read_index_blob(loc: &Location, store: &Store, d: &Digest) -> Result<Vec<u8>, CatalogError> {
    let mut bytes = Vec::new();
    store
        .open_blob(d)
        .map_err(|e| from_store(loc, e))?
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| infra(loc, e))?;
    Ok(bytes)
}

/// The manifest of a verified index whose blob `d` is still in `store`.
fn stored(store: &Store, d: &Digest) -> Option<Vec<u8>> {
    let m = store.get_ref(&format!("{STORE_REF}{}", d.hex())).ok()??;
    store.has_blob(d).then(|| store.read_metadata(&m).ok()).flatten()
}

/// The verified index at `loc` (its `index` tag), or the one `pin` names (from the store when it
/// is there, else from `loc`).
pub fn load(
    loc: &Location,
    pin: Option<&str>,
    home: &Path,
    store: &Store,
    auth: DockerConfig,
) -> Result<Verified, CatalogError> {
    load_with(loc, pin, home, store, auth, &trusted_keys()?)
}

pub(crate) fn load_with(
    loc: &Location,
    pin: Option<&str>,
    home: &Path,
    store: &Store,
    auth: DockerConfig,
    keys: &[TrustedKey],
) -> Result<Verified, CatalogError> {
    let pin = pin
        .map(|p| {
            Digest::parse(p).map_err(|_| CatalogError::Plan(format!("catalog index {p:?} is not sha256:<64 hex>")))
        })
        .transpose()?;
    let manifest = match pin.as_ref().and_then(|d| stored(store, d)) {
        Some(manifest) => manifest,
        None => {
            let source = Source::open(loc, auth)?;
            let name = match (&pin, loc) {
                (Some(d), _) => pinned_tag(d),
                (None, Location::Registry(r)) => r.target(),
                (None, Location::Layout(_)) => INDEX_TAG.to_string(),
            };
            let manifest = source.manifest(loc, &name)?;
            let (layer, _, _) = check_manifest(loc, &manifest)?;
            source.blob(loc, &layer.digest, layer.size, store)?;
            manifest
        }
    };
    let (layer, key, sig) = check_manifest(loc, &manifest)?;
    if let Some(p) = &pin
        && &layer.digest != p
    {
        return Err(hostile(
            loc,
            format!("the manifest for index {p} names {}", layer.digest),
        ));
    }
    let blob = read_index_blob(loc, store, &layer.digest)?;
    // The store checked this on the way in; a blob already there is checked again here.
    let sha: [u8; 32] = Sha256::digest(&blob).into();
    if blob.len() as u64 != layer.size || hex(&sha) != layer.digest.hex() {
        return Err(hostile(
            loc,
            format!("index blob digest mismatch: expected {}", layer.digest),
        ));
    }
    verify(&sha, &key, &sig, keys).map_err(|e| at(loc, e))?;
    let index = Index::parse(&blob).map_err(|e| at(loc, e))?;

    let path = serial_path(home, loc);
    let recorded = read_serial(loc, &path)?;
    if pin.is_none()
        && let Some(seen) = recorded
        && index.serial < seen
    {
        return Err(hostile(
            loc,
            format!(
                "index serial {} is lower than serial {seen} already verified here (a rollback?); \
                 pin its digest {} to use it anyway",
                index.serial, layer.digest
            ),
        ));
    }
    {
        let _lock = store.lock_shared().map_err(|e| from_store(loc, e))?;
        let m = store.put_bytes(&manifest).map_err(|e| from_store(loc, e))?;
        store
            .set_ref(&format!("{STORE_REF}{}", layer.digest.hex()), &m)
            .map_err(|e| from_store(loc, e))?;
    }
    if recorded.is_none_or(|seen| index.serial > seen) {
        write_serial(loc, &path, index.serial)?;
    }
    Ok(Verified {
        index,
        digest: layer.digest.to_string(),
        location: loc.clone(),
    })
}

/// Where the highest serial seen at `loc` is kept.
pub(crate) fn serial_path(home: &Path, loc: &Location) -> PathBuf {
    let id = hex(&Sha256::digest(loc.to_string().as_bytes())[..8]);
    home.join("catalog").join(id).join("serial")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_serial(loc: &Location, path: &Path) -> Result<Option<u64>, CatalogError> {
    match fs::read_to_string(path) {
        Ok(s) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| infra(loc, format!("{} does not hold a serial", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(infra(loc, format!("{}: {e}", path.display()))),
    }
}

/// Replaces the serial file atomically: a temp file in the same directory, then a rename.
fn write_serial(loc: &Location, path: &Path, serial: u64) -> Result<(), CatalogError> {
    let fail = |e: io::Error| infra(loc, format!("{}: {e}", path.display()));
    let dir = path.parent().expect("the serial file has a parent");
    fs::create_dir_all(dir).map_err(fail)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(fail)?;
    tmp.write_all(serial.to_string().as_bytes()).map_err(fail)?;
    tmp.as_file().sync_all().map_err(fail)?;
    tmp.persist(path).map_err(|e| fail(e.error))?;
    Ok(())
}

/// Puts `e`'s layer in `store` (fetching it from `v`'s location unless it is already there).
pub fn fetch_layer(v: &Verified, e: &Entry, store: &Store, auth: DockerConfig) -> Result<Descriptor, CatalogError> {
    let loc = &v.location;
    let d = Digest::parse(&e.digest).map_err(|_| {
        hostile(
            loc,
            format!("{} has digest {:?}, not sha256:<64 hex>", e.name, e.digest),
        )
    })?;
    if store.has_blob(&d) {
        store
            .put_verified(&mut io::empty(), &d, Some(e.size))
            .map_err(|err| from_store(loc, err))?;
    } else {
        Source::open(loc, auth)?.blob(loc, &d, e.size, store)?;
    }
    Ok(Descriptor::new(&e.media_type, d, e.size))
}

/// Writes (or adds to) an OCI layout at `dir` holding the signed index and `layers`; returns the
/// index manifest's descriptor. Manifests already in the layout under other names are kept.
pub fn write_layout(
    dir: &Path,
    index_blob: &[u8],
    key_id: &str,
    sig: &str,
    layers: &[(Descriptor, PathBuf)],
) -> Result<Descriptor, CatalogError> {
    let fail = |e: io::Error| CatalogError::Infra(format!("writing the catalog layout {}: {e}", dir.display()));
    let blobs = dir.join("blobs/sha256");
    fs::create_dir_all(&blobs).map_err(fail)?;
    fs::write(dir.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#).map_err(fail)?;
    let put = |bytes: &[u8]| -> Result<Descriptor, CatalogError> {
        let d = Digest::of(bytes);
        let path = blobs.join(d.hex());
        if !path.is_file() {
            fs::write(&path, bytes).map_err(fail)?;
        }
        Ok(Descriptor::new(OCI_MANIFEST, d, bytes.len() as u64))
    };
    let manifest = |artifact_type: &str, layer: Descriptor, annotations| -> Result<Descriptor, CatalogError> {
        let mut config = put(EMPTY)?;
        config.media_type = EMPTY_TYPE.into();
        put(&canonical_json(&ImageManifest {
            schema_version: 2,
            media_type: Some(OCI_MANIFEST.into()),
            artifact_type: Some(artifact_type.into()),
            config,
            layers: vec![layer],
            annotations,
        }))
    };

    let mut index = put(index_blob)?;
    index.media_type = INDEX_TYPE.into();
    let signature = BTreeMap::from([
        (KEY.to_string(), key_id.to_string()),
        (SIGNATURE.to_string(), sig.to_string()),
    ]);
    let index_manifest = manifest(INDEX_TYPE, index.clone(), Some(signature))?;
    let mut named = vec![
        (pinned_tag(&index.digest), index_manifest.clone()),
        (INDEX_TAG.to_string(), index_manifest.clone()),
    ];
    for (desc, path) in layers {
        copy_blob(path, &blobs, desc)?;
        named.push((layer_tag(&desc.digest), manifest(LAYER_TYPE, desc.clone(), None)?));
    }

    let index_json = dir.join("index.json");
    let mut manifests: Vec<Descriptor> = fs::read(&index_json)
        .ok()
        .and_then(|b| serde_json::from_slice::<ImageIndex>(&b).ok())
        .map(|i| i.manifests)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| !named.iter().any(|(n, _)| m.annotation(REF_NAME) == Some(n)))
        .collect();
    manifests.extend(named.into_iter().map(|(name, mut d)| {
        d.annotations = Some(BTreeMap::from([(REF_NAME.to_string(), name)]));
        d
    }));
    let out = ImageIndex {
        schema_version: 2,
        media_type: Some(OCI_INDEX.into()),
        artifact_type: None,
        manifests,
        annotations: None,
    };
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(fail)?;
    tmp.write_all(&canonical_json(&out)).map_err(fail)?;
    tmp.persist(&index_json).map_err(|e| fail(e.error))?;
    Ok(index_manifest)
}

/// Copies the layer at `from` into `blobs`, checking it is `want`.
fn copy_blob(from: &Path, blobs: &Path, want: &Descriptor) -> Result<(), CatalogError> {
    let fail = |e: io::Error| CatalogError::Infra(format!("copying layer {}: {e}", from.display()));
    let dest = blobs.join(want.digest.hex());
    if dest.is_file() {
        return Ok(());
    }
    let mut reader = HashingReader::new(fs::File::open(from).map_err(fail)?);
    let mut tmp = tempfile::NamedTempFile::new_in(blobs).map_err(fail)?;
    io::copy(&mut reader, &mut tmp).map_err(fail)?;
    let (digest, size) = reader.finish_to_eof().map_err(fail)?;
    if digest != want.digest || size != want.size {
        return Err(CatalogError::Plan(format!(
            "layer {} is {digest} ({size} bytes), not {} ({} bytes)",
            from.display(),
            want.digest,
            want.size
        )));
    }
    tmp.persist(&dest).map_err(|e| fail(e.error))?;
    Ok(())
}

/// Pushes every manifest of the layout at `layout` (blobs first) to `to`'s repository: each
/// under its ref name, the `index` ref under `to`'s tag, and that one last.
pub fn push(layout: &Path, to: &Reference, auth: DockerConfig) -> Result<(), CatalogError> {
    let loc = Location::Registry(to.clone());
    let tag = to
        .tag()
        .ok_or_else(|| CatalogError::Plan(format!("push the catalog to a tag, not {to}")))?;
    let src = Location::Layout(layout.to_path_buf());
    let dir = DirLayout::open(layout).map_err(|e| from_oci(&src, e))?;
    let bytes = dir
        .read_small("index.json", MAX_MANIFEST)
        .map_err(|e| from_oci(&src, e))?;
    let index: ImageIndex = serde_json::from_slice(&bytes).map_err(|e| hostile(&src, format!("index.json: {e}")))?;
    let mut named: Vec<(&str, &Descriptor)> = index
        .manifests
        .iter()
        .filter_map(|m| m.annotation(REF_NAME).map(|n| (n, m)))
        .collect();
    // `false` sorts first: the current index goes up after everything it names.
    named.sort_by_key(|(n, _)| *n == INDEX_TAG);
    let client = Client::new(to.registry(), auth).map_err(|e| from_registry(&loc, e))?;
    let source = Source::Layout(dir);
    for (name, _) in named {
        let bytes = source.manifest(&src, name)?;
        let m: ImageManifest =
            serde_json::from_slice(&bytes).map_err(|e| hostile(&src, format!("manifest {name}: {e}")))?;
        for d in std::iter::once(&m.config).chain(&m.layers) {
            let path = layout.join("blobs/sha256").join(d.digest.hex());
            client
                .push_blob(to.repository(), &d.digest, &path)
                .map_err(|e| from_registry(&loc, e))?;
        }
        let target = if name == INDEX_TAG { tag } else { name };
        client
            .put_manifest(to.repository(), target, OCI_MANIFEST, &bytes)
            .map_err(|e| from_registry(&loc, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use kiln_oci::media::OCI_LAYER_TAR;
    use kiln_registry::testregistry::{Config, TestRegistry};

    use super::*;
    use crate::{Arch, INDEX_TAG, Kind, SCHEMA, sign};

    struct Fx {
        _dir: tempfile::TempDir,
        layout: PathBuf,
        home: PathBuf,
        store: Store,
        key: SigningKey,
        keys: Vec<TrustedKey>,
        layer: (Descriptor, PathBuf),
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("layer.tar");
        let bytes = b"a go layer".to_vec();
        std::fs::write(&layer_path, &bytes).unwrap();
        let key = SigningKey::generate(&mut rand_core::OsRng);
        Fx {
            layout: dir.path().join("layout"),
            home: dir.path().join("home"),
            store: Store::open(dir.path().join("store")).unwrap(),
            keys: vec![TrustedKey::new(key.verifying_key())],
            key,
            layer: (
                Descriptor::new(OCI_LAYER_TAR, Digest::of(&bytes), bytes.len() as u64),
                layer_path,
            ),
            _dir: dir,
        }
    }

    impl Fx {
        fn loc(&self) -> Location {
            Location::Layout(self.layout.clone())
        }

        fn index(&self, serial: u64) -> Vec<u8> {
            let d = &self.layer.0;
            Index {
                schema: SCHEMA.into(),
                serial,
                created: "2026-11-01T00:00:00Z".into(),
                entries: vec![Entry {
                    name: "go".into(),
                    line: "1.27".into(),
                    version: "1.27.1".into(),
                    arch: Arch::X86_64,
                    kind: Kind::Runtime,
                    digest: d.digest.to_string(),
                    diff_id: d.digest.to_string(),
                    size: d.size,
                    media_type: d.media_type.clone(),
                    abi: Default::default(),
                    requires: vec![],
                    conflicts: vec![],
                    env: Default::default(),
                    provides_sonames: vec![],
                    executor: "ns".into(),
                    source: "https://example.test/go".into(),
                    license: "BSD-3-Clause".into(),
                    supersedes: None,
                    fixes: vec![],
                }],
            }
            .canonical()
        }

        /// Writes a layout whose index has `serial`, signed with the fixture key; returns the
        /// index blob digest.
        fn publish(&self, serial: u64) -> String {
            let blob = self.index(serial);
            let (id, sig) = sign(&Sha256::digest(&blob).into(), &self.key);
            write_layout(&self.layout, &blob, &id, &sig, std::slice::from_ref(&self.layer)).unwrap();
            Digest::of(&blob).to_string()
        }

        fn load(&self, loc: &Location, pin: Option<&str>) -> Result<Verified, CatalogError> {
            load_with(loc, pin, &self.home, &self.store, DockerConfig::anonymous(), &self.keys)
        }

        fn recorded(&self, loc: &Location) -> Option<String> {
            std::fs::read_to_string(serial_path(&self.home, loc)).ok()
        }

        fn blob_path(&self, d: &str) -> PathBuf {
            self.layout
                .join("blobs/sha256")
                .join(d.strip_prefix("sha256:").unwrap())
        }
    }

    fn hostile(r: Result<impl std::fmt::Debug, CatalogError>, needle: &str) {
        let err = r.unwrap_err();
        assert!(
            matches!(&err, CatalogError::Hostile(m) if m.contains(needle)),
            "want Hostile containing {needle:?}, got {err:?}"
        );
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn layout_load_verifies_records_and_fetches() {
        let f = fx();
        let d = f.publish(7);
        let v = f.load(&f.loc(), None).unwrap();
        assert_eq!(v.index.serial, 7);
        assert_eq!(v.digest, d);
        assert_eq!(v.location, f.loc());
        assert_eq!(f.recorded(&f.loc()).as_deref(), Some("7"));
        let blob = Digest::parse(&d).unwrap();
        assert!(f.store.has_blob(&blob), "the verified index blob is kept in the store");
        let desc = fetch_layer(&v, &v.index.entries[0], &f.store, DockerConfig::anonymous()).unwrap();
        assert_eq!(desc, f.layer.0);
        assert_eq!(
            std::fs::read(f.store.blob_path(&desc.digest)).unwrap(),
            std::fs::read(&f.layer.1).unwrap()
        );
        let state = serial_path(&f.home, &f.loc());
        let name = state.parent().unwrap().file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 16);
        assert!(state.starts_with(f.home.join("catalog")));
    }

    #[test]
    fn tampered_index_blob_is_hostile() {
        let f = fx();
        let d = f.publish(7);
        let path = f.blob_path(&d);
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes.len() / 2;
        bytes[at] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        hostile(f.load(&f.loc(), None), "digest");
        assert_eq!(f.recorded(&f.loc()), None, "nothing is recorded on failure");
    }

    #[test]
    fn signature_over_other_bytes_is_hostile() {
        let f = fx();
        let (id, sig) = sign(&Sha256::digest(f.index(8)).into(), &f.key);
        write_layout(&f.layout, &f.index(7), &id, &sig, std::slice::from_ref(&f.layer)).unwrap();
        hostile(f.load(&f.loc(), None), "does not verify");
    }

    #[test]
    fn signature_from_untrusted_key_is_hostile() {
        let f = fx();
        f.publish(7);
        let other = TrustedKey::new(SigningKey::generate(&mut rand_core::OsRng).verifying_key());
        let r = load_with(&f.loc(), None, &f.home, &f.store, DockerConfig::anonymous(), &[other]);
        hostile(r, "not trusted");
    }

    #[test]
    fn malformed_index_manifest_is_hostile() {
        let f = fx();
        f.publish(7);
        // A manifest naming the right blob but with a second layer, swapped in under `index`.
        let index_json = f.layout.join("index.json");
        let mut idx: serde_json::Value = serde_json::from_slice(&std::fs::read(&index_json).unwrap()).unwrap();
        let manifests = idx["manifests"].as_array_mut().unwrap();
        let at = manifests
            .iter()
            .position(|m| m["annotations"][kiln_oci::REF_NAME] == INDEX_TAG)
            .unwrap();
        let old = Digest::parse(manifests[at]["digest"].as_str().unwrap()).unwrap();
        let mut m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.blob_path(&old.to_string())).unwrap()).unwrap();
        let layer = m["layers"][0].clone();
        m["layers"].as_array_mut().unwrap().push(layer);
        let bytes = serde_json::to_vec(&m).unwrap();
        let nd = Digest::of(&bytes);
        std::fs::write(f.blob_path(&nd.to_string()), &bytes).unwrap();
        manifests[at]["digest"] = nd.to_string().into();
        manifests[at]["size"] = bytes.len().into();
        std::fs::write(&index_json, serde_json::to_vec(&idx).unwrap()).unwrap();
        hostile(f.load(&f.loc(), None), "layers");
    }

    #[test]
    fn layer_digest_mismatch_is_hostile() {
        let f = fx();
        f.publish(7);
        let v = f.load(&f.loc(), None).unwrap();
        let e = &v.index.entries[0];
        std::fs::write(f.blob_path(&e.digest), b"a go layeR").unwrap();
        hostile(fetch_layer(&v, e, &f.store, DockerConfig::anonymous()), "digest");
        assert!(!f.store.has_blob(&Digest::parse(&e.digest).unwrap()));
    }

    #[test]
    fn lower_serial_is_refused() {
        let f = fx();
        f.publish(7);
        f.load(&f.loc(), None).unwrap();
        f.publish(6);
        hostile(f.load(&f.loc(), None), "serial 6");
        assert_eq!(f.recorded(&f.loc()).as_deref(), Some("7"));
        // The same serial again is fine; a higher one moves the record up.
        f.publish(7);
        f.load(&f.loc(), None).unwrap();
        f.publish(9);
        f.load(&f.loc(), None).unwrap();
        assert_eq!(f.recorded(&f.loc()).as_deref(), Some("9"));
    }

    #[test]
    fn lower_serial_with_pin_is_accepted() {
        let f = fx();
        f.publish(7);
        f.load(&f.loc(), None).unwrap();
        let old = f.publish(6);
        let v = f.load(&f.loc(), Some(&old)).unwrap();
        assert_eq!(v.index.serial, 6);
        assert_eq!(v.digest, old);
        assert_eq!(
            f.recorded(&f.loc()).as_deref(),
            Some("7"),
            "a pin never lowers the record"
        );
    }

    #[test]
    fn a_pin_is_a_digest() {
        let f = fx();
        f.publish(7);
        let err = f.load(&f.loc(), Some("latest")).unwrap_err();
        assert!(matches!(err, CatalogError::Plan(_)), "{err:?}");
    }

    #[test]
    fn missing_layout_is_infra() {
        let f = fx();
        let err = f.load(&f.loc(), None).unwrap_err();
        assert!(
            matches!(&err, CatalogError::Infra(m) if m.contains(&f.loc().to_string())),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 4);
    }

    fn closed_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[test]
    fn unreachable_registry_is_infra() {
        let f = fx();
        let loc = Location::parse(&format!("127.0.0.1:{}/acme/catalog", closed_port())).unwrap();
        let err = f.load(&loc, None).unwrap_err();
        assert!(
            matches!(&err, CatalogError::Infra(m) if m.contains(&loc.to_string())),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 4);
    }

    #[test]
    fn push_then_load_from_testregistry() {
        let f = fx();
        let d = f.publish(7);
        let reg = TestRegistry::start(Config::default());
        let to = Reference::parse(&format!("{}/acme/catalog:index", reg.host())).unwrap();
        push(&f.layout, &to, DockerConfig::anonymous()).unwrap();
        assert!(reg.has_blob(&f.layer.0.digest), "layers are pushed with the index");
        let loc = Location::Registry(to);
        let v = f.load(&loc, None).unwrap();
        assert_eq!((v.index.serial, v.digest.as_str()), (7, d.as_str()));
        assert_eq!(v.location, loc);
        let desc = fetch_layer(&v, &v.index.entries[0], &f.store, DockerConfig::anonymous()).unwrap();
        assert_eq!(desc.digest, f.layer.0.digest);
        assert_eq!(f.recorded(&loc).as_deref(), Some("7"));

        // A pin fetches that index by digest from the registry when the store lacks it.
        let fresh = tempfile::tempdir().unwrap();
        let store = Store::open(fresh.path()).unwrap();
        let v = load_with(&loc, Some(&d), &f.home, &store, DockerConfig::anonymous(), &f.keys).unwrap();
        assert_eq!(v.digest, d);
    }

    #[test]
    fn pinned_index_in_store_works_offline() {
        let f = fx();
        let d = f.publish(7);
        let reg = TestRegistry::start(Config::default());
        let to = Reference::parse(&format!("{}/acme/catalog:index", reg.host())).unwrap();
        push(&f.layout, &to, DockerConfig::anonymous()).unwrap();
        let loc = Location::Registry(to);
        let v = f.load(&loc, None).unwrap();
        fetch_layer(&v, &v.index.entries[0], &f.store, DockerConfig::anonymous()).unwrap();
        drop(reg);

        // Unreachable and nothing pinned: exit 4, naming the catalog.
        let dead = Location::parse(&format!("127.0.0.1:{}/acme/catalog", closed_port())).unwrap();
        let err = f.load(&dead, None).unwrap_err();
        assert!(
            matches!(&err, CatalogError::Infra(m) if m.contains(&dead.to_string())),
            "{err:?}"
        );

        // Pinned, with the index and its layers in the store: no network needed.
        let v = f.load(&dead, Some(&d)).unwrap();
        assert_eq!(v.digest, d);
        assert_eq!(v.location, dead);
        let desc = fetch_layer(&v, &v.index.entries[0], &f.store, DockerConfig::anonymous()).unwrap();
        assert_eq!(desc.digest, f.layer.0.digest);
    }
}
