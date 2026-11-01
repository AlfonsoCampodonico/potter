//! Composite bases (spec §7): a resolved catalog's layers merged into one cached layer.
//!
//! The layers must already be in the store (the caller fetches them): merging is local. The
//! result is byte-reproducible, so the image's manifest digest is cached under the resolution's
//! composite key, and concurrent builds that make the same composite write the same bytes.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use kiln_oci::media::{OCI_CONFIG, OCI_LAYER_ZSTD, OCI_MANIFEST};
use kiln_oci::{ContainerConfig, Descriptor, ImageConfig, ImageManifest, Platform, RootFs, canonical_json};
use kiln_store::{Digest, Hasher, Store};
use potter_cache::PotterHome;
use potter_catalog::{Entry, Resolution};
use potter_plan::Export;

use crate::base::{BaseImage, layer_reader};
use crate::error::{BuildError, io};
use crate::merge::{ExportSpec, LayerOpen, export_layer};

/// The name composite bases go by.
pub const REPOSITORY: &str = "potter/composite";
/// The composite layer's zstd level: fixed, so the blob is too.
const ZSTD_LEVEL: i32 = 3;

/// The composite base of `r` for `platform`: its layers merged bottom first into one zstd
/// layer, with the entries' environment, working directory `/` and user `0:0`. Every layer of
/// `r` must be in `store`.
pub fn composite(
    store: &Store,
    home: &PotterHome,
    r: &Resolution,
    platform: &Platform,
) -> Result<BaseImage, BuildError> {
    if r.entries.is_empty() {
        return Err(BuildError::Plan("the catalog resolved no layers to build on".into()));
    }
    let dir = home.root().join("composite");
    std::fs::create_dir_all(&dir).map_err(io(format!("creating {}", dir.display())))?;
    let cache = dir.join(r.composite_key());
    let env = environment(&r.entries);
    // Kiln's GC must not collect the blobs between their writing and the cache naming them.
    let _lock = store
        .lock_shared()
        .map_err(|e| BuildError::Infra(format!("locking the store: {e}")))?;
    if let Some(base) = cached(store, &cache, &env, platform) {
        return Ok(base);
    }
    let layers = r
        .entries
        .iter()
        .map(|e| stored_layer(store, e))
        .collect::<Result<Vec<_>, _>>()?;
    let open: Vec<LayerOpen> = layers
        .iter()
        .map(|d| Box::new(move || layer_reader(store, d)) as LayerOpen)
        .collect();
    let (layer, diff_id) = merged_layer(store, &open, &dir)?;
    let config = ImageConfig {
        architecture: platform.architecture.clone(),
        os: platform.os.clone(),
        variant: platform.variant.clone(),
        config: Some(ContainerConfig {
            user: Some("0:0".into()),
            env: Some(env),
            working_dir: Some("/".into()),
            ..Default::default()
        }),
        rootfs: RootFs {
            fs_type: "layers".into(),
            diff_ids: vec![diff_id],
        },
    };
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("storing the composite base: {e}"));
    let config_bytes = canonical_json(&config);
    let config_digest = store.put_bytes(&config_bytes).map_err(|e| infra(&e))?;
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(OCI_CONFIG, config_digest, config_bytes.len() as u64),
        layers: vec![layer],
        annotations: None,
    };
    let manifest_digest = store.put_bytes(&canonical_json(&manifest)).map_err(|e| infra(&e))?;
    remember(&dir, &cache, &manifest_digest)?;
    BaseImage::local(store, &manifest_digest, REPOSITORY)
}

/// The entries' environment, applied bottom first: an upper entry's `PATH` is prepended to the
/// lower ones', any other variable it sets overrides.
fn environment(entries: &[Entry]) -> Vec<String> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for e in entries {
        for (k, v) in &e.env {
            match env.get_mut(k) {
                Some(lower) if k == "PATH" && !lower.is_empty() => *lower = format!("{v}:{lower}"),
                _ => {
                    env.insert(k.clone(), v.clone());
                }
            }
        }
    }
    env.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

/// The cached composite at `cache`, when its image is whole in the store and was made with the
/// same environment and platform; anything else is rebuilt.
fn cached(store: &Store, cache: &Path, env: &[String], platform: &Platform) -> Option<BaseImage> {
    let text = std::fs::read_to_string(cache).ok()?;
    let digest = Digest::parse(text.trim()).ok()?;
    let base = BaseImage::local(store, &digest, REPOSITORY).ok()?;
    let c = &base.resolved.config;
    let same = c.config.as_ref().and_then(|c| c.env.as_deref()) == Some(env)
        && c.architecture == platform.architecture
        && c.os == platform.os
        && c.variant == platform.variant;
    same.then_some(base)
}

/// `e`'s layer, which must be in `store` already.
fn stored_layer(store: &Store, e: &Entry) -> Result<Descriptor, BuildError> {
    let digest = Digest::parse(&e.digest).map_err(|_| {
        BuildError::Hostile(format!(
            "the catalog layer {} has digest {:?}, not sha256:<64 hex>",
            e.name, e.digest
        ))
    })?;
    if !store.has_blob(&digest) {
        return Err(BuildError::Infra(format!(
            "the catalog layer {} ({digest}) is not in the store: it must be fetched first",
            e.name
        )));
    }
    Ok(Descriptor::new(&e.media_type, digest, e.size))
}

/// Forwards writes to `inner`, hashing them: the uncompressed tar's diff_id.
struct Hashing<W> {
    inner: W,
    hasher: Hasher,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// `layers` flattened into one zstd layer in `store`, with its diff_id. Data is staged in `spill`.
fn merged_layer(store: &Store, layers: &[LayerOpen], spill: &Path) -> Result<(Descriptor, Digest), BuildError> {
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("storing the composite layer: {e}"));
    let tmp = store.tmp_blob().map_err(|e| infra(&e))?;
    let file = tmp.reopen().map_err(|e| infra(&e))?;
    let encoder = zstd::Encoder::new(file, ZSTD_LEVEL).map_err(io("compressing the composite layer"))?;
    let out = Hashing {
        inner: encoder,
        hasher: Hasher::new(),
    };
    let everything = [Export {
        path: "/".into(),
        dest: "/".into(),
        exclude: Vec::new(),
    }];
    // Fixed at 0, not SOURCE_DATE_EPOCH: the cache key names the layers and nothing else.
    let spec = ExportSpec {
        exports: &everything,
        epoch: 0,
        spill_dir: spill,
    };
    let (out, _) = export_layer(layers, &spec, out)?;
    let diff_id = out.hasher.finish();
    out.inner.finish().map_err(io("compressing the composite layer"))?;
    let digest = store.commit(tmp).map_err(|e| infra(&e))?;
    let size = store.blob_size(&digest).map_err(|e| infra(&e))?;
    Ok((Descriptor::new(OCI_LAYER_ZSTD, digest, size), diff_id))
}

/// Points `cache` at `manifest`: a temp file renamed into place, so a reader never sees a torn
/// one, and a concurrent writer of the same composite writes the same digest.
fn remember(dir: &Path, cache: &Path, manifest: &Digest) -> Result<(), BuildError> {
    let what = format!("caching the composite at {}", cache.display());
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io(what.clone()))?;
    writeln!(tmp, "{manifest}").map_err(io(what.clone()))?;
    tmp.persist(cache).map_err(|e| BuildError::Io {
        context: what,
        source: e.error,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Read;

    use kiln_erofs::testtar::{Opts, TarBuilder};
    use kiln_oci::media::OCI_LAYER_ZSTD;
    use kiln_store::Digest;
    use potter_catalog::{Arch, Entry, Kind};

    use super::*;
    use crate::base::layer_reader;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        home: PotterHome,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let home = PotterHome::open(dir.path().join("home")).unwrap();
        Fixture { _dir: dir, store, home }
    }

    /// A catalog entry for the layer `tar`, stored zstd-compressed as the catalog ships it.
    fn entry(store: &Store, name: &str, tar: &[u8], env: &[(&str, &str)]) -> Entry {
        let blob = zstd::encode_all(tar, 3).unwrap();
        let digest = store.put_bytes(&blob).unwrap();
        Entry {
            name: name.into(),
            line: "1".into(),
            version: "1.0.0".into(),
            arch: Arch::Aarch64,
            kind: Kind::Runtime,
            digest: digest.to_string(),
            diff_id: Digest::of(tar).to_string(),
            size: blob.len() as u64,
            media_type: OCI_LAYER_ZSTD.into(),
            abi: BTreeMap::new(),
            requires: vec![],
            conflicts: vec![],
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            provides_sonames: vec![],
            executor: name.into(),
            source: "https://example.test".into(),
            license: "MIT".into(),
            supersedes: None,
            fixes: vec![],
        }
    }

    fn resolution(entries: Vec<Entry>) -> Resolution {
        Resolution {
            entries,
            constraints: vec![],
            index_digest: Digest::of(b"index").to_string(),
        }
    }

    fn platform() -> Platform {
        Platform::parse("linux/arm64").unwrap()
    }

    /// Two layers: a base with `/bin`, `/etc` and `/usr/lib`, and a runtime over it.
    fn two_layers(store: &Store) -> Resolution {
        let root = Opts::default();
        let user = Opts::default().uid(1000).gid(1000).mode(0o750).mtime(1_700_000_000);
        let mut base = TarBuilder::new();
        base.dir("bin", &root)
            .file("bin/sh", b"sh", &root.clone().mode(0o755))
            .dir("etc", &root)
            .file("etc/os-release", b"debian", &root)
            .file("etc/motd", b"hello", &root)
            .dir("usr", &root)
            .dir("usr/lib", &root)
            .file("usr/lib/old.so", b"old", &root)
            .dir("home", &root)
            .dir("home/app", &user)
            .file("home/app/.profile", b"x", &user);
        let mut runtime = TarBuilder::new();
        runtime
            .dir("usr", &root)
            .dir("usr/local", &root)
            .dir("usr/local/bin", &root)
            .file("usr/local/bin/node", b"node", &root.clone().mode(0o755))
            .file("etc/os-release", b"debian+node", &root)
            .whiteout("etc/motd")
            .opaque("usr/lib")
            .file("usr/lib/new.so", b"new", &root);
        resolution(vec![
            entry(
                store,
                "base",
                &base.finish(),
                &[("PATH", "/usr/bin:/bin"), ("LANG", "C.UTF-8")],
            ),
            entry(
                store,
                "node",
                &runtime.finish(),
                &[("PATH", "/usr/local/bin"), ("NODE_ENV", "production"), ("LANG", "C")],
            ),
        ])
    }

    /// (path, type, mode, uid, gid, mtime, data) of the composite's single layer.
    type Listing = Vec<(String, char, u32, u64, u64, u64, Vec<u8>)>;

    fn listing(store: &Store, base: &BaseImage) -> Listing {
        assert_eq!(base.layers().len(), 1);
        let mut a = tar::Archive::new(layer_reader(store, &base.layers()[0]).unwrap());
        a.entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let h = e.header().clone();
                let kind = match h.entry_type() {
                    tar::EntryType::Directory => 'd',
                    tar::EntryType::Regular => 'f',
                    tar::EntryType::Symlink => 'l',
                    tar::EntryType::Link => 'h',
                    other => panic!("{other:?}"),
                };
                let mut data = Vec::new();
                e.read_to_end(&mut data).unwrap();
                (
                    e.path().unwrap().display().to_string(),
                    kind,
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    h.gid().unwrap(),
                    h.mtime().unwrap(),
                    data,
                )
            })
            .collect()
    }

    fn blob(store: &Store, d: &Digest) -> Vec<u8> {
        std::fs::read(store.blob_path(d)).unwrap()
    }

    #[test]
    fn composite_is_byte_identical_across_runs() {
        let (a, b) = (fixture(), fixture());
        let ra = two_layers(&a.store);
        let rb = two_layers(&b.store);
        let x = composite(&a.store, &a.home, &ra, &platform()).unwrap();
        let y = composite(&b.store, &b.home, &rb, &platform()).unwrap();
        assert_eq!(x.key_digest(), y.key_digest());
        assert_eq!(x.layers(), y.layers());
        assert_eq!(x.resolved.config_digest, y.resolved.config_digest);
        let layer = &x.layers()[0];
        assert_eq!(layer.media_type, OCI_LAYER_ZSTD);
        assert_eq!(blob(&a.store, &layer.digest), blob(&b.store, &layer.digest));
        assert_eq!(
            blob(&a.store, &x.resolved.manifest_digest),
            blob(&b.store, &y.resolved.manifest_digest)
        );
        // The diff_id is the uncompressed tar's digest.
        let mut raw = Vec::new();
        layer_reader(&a.store, layer).unwrap().read_to_end(&mut raw).unwrap();
        assert_eq!(x.diff_ids(), [Digest::of(&raw)]);
        // Entries sorted, mtimes 0, uid/gid preserved.
        let out = listing(&a.store, &x);
        let paths: Vec<&str> = out.iter().map(|e| e.0.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        assert!(out.iter().all(|e| e.5 == 0), "{out:?}");
        let app = out.iter().find(|e| e.0 == "home/app").unwrap();
        assert_eq!((app.2, app.3, app.4), (0o750, 1000, 1000));
        assert_eq!(x.resolved.platform, platform());
        assert_eq!(x.resolved.config.architecture, "arm64");
    }

    #[test]
    fn upper_layer_wins_and_whiteouts_apply() {
        let f = fixture();
        let r = two_layers(&f.store);
        let base = composite(&f.store, &f.home, &r, &platform()).unwrap();
        let out = listing(&f.store, &base);
        let get = |p: &str| out.iter().find(|e| e.0 == p).map(|e| e.6.clone());
        assert_eq!(get("etc/os-release").as_deref(), Some(&b"debian+node"[..]));
        assert_eq!(get("bin/sh").as_deref(), Some(&b"sh"[..]));
        assert_eq!(get("usr/local/bin/node").as_deref(), Some(&b"node"[..]));
        assert_eq!(get("usr/lib/new.so").as_deref(), Some(&b"new"[..]));
        assert_eq!(get("etc/motd"), None, "the whiteout removes it");
        assert_eq!(get("usr/lib/old.so"), None, "the opaque directory hides it");
        assert!(
            out.iter().all(|e| !e.0.contains(".wh.")),
            "no whiteout survives the merge: {out:?}"
        );
    }

    #[test]
    fn env_path_is_prepended_bottom_first() {
        let f = fixture();
        let r = two_layers(&f.store);
        let base = composite(&f.store, &f.home, &r, &platform()).unwrap();
        let env = base.env();
        assert_eq!(env["PATH"], "/usr/local/bin:/usr/bin:/bin");
        assert_eq!(env["LANG"], "C", "an upper layer's value overrides");
        assert_eq!(env["NODE_ENV"], "production");
        assert_eq!(env.len(), 3);
        assert_eq!(base.workdir().as_deref(), Some("/"));
        assert_eq!(base.user().as_deref(), Some("0:0"));
    }

    #[test]
    fn cached_composite_is_reused() {
        let f = fixture();
        let r = two_layers(&f.store);
        let first = composite(&f.store, &f.home, &r, &platform()).unwrap();
        let cache = f.home.root().join("composite").join(r.composite_key());
        assert_eq!(std::fs::read_to_string(&cache).unwrap().trim(), first.key_digest());
        // The cache answers without the catalog layers: they are not read again.
        for e in &r.entries {
            std::fs::remove_file(f.store.blob_path(&Digest::parse(&e.digest).unwrap())).unwrap();
        }
        let again = composite(&f.store, &f.home, &r, &platform()).unwrap();
        assert_eq!(again.key_digest(), first.key_digest());
        assert_eq!(again.layers(), first.layers());
        assert_eq!(again.env(), first.env());

        // A cache entry whose image is gone from the store is rebuilt, not trusted.
        let f = fixture();
        let r = two_layers(&f.store);
        let first = composite(&f.store, &f.home, &r, &platform()).unwrap();
        std::fs::remove_file(f.store.blob_path(&first.resolved.manifest_digest)).unwrap();
        let rebuilt = composite(&f.store, &f.home, &r, &platform()).unwrap();
        assert_eq!(rebuilt.key_digest(), first.key_digest());
        assert!(f.store.has_blob(&first.resolved.manifest_digest));
    }

    #[test]
    fn concurrent_composites_agree() {
        let f = fixture();
        let r = two_layers(&f.store);
        let digests: Vec<String> = std::thread::scope(|s| {
            let threads: Vec<_> = (0..2)
                .map(|_| s.spawn(|| composite(&f.store, &f.home, &r, &platform()).unwrap().key_digest()))
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).collect()
        });
        assert_eq!(digests[0], digests[1]);
        let dir = f.home.root().join("composite");
        assert_eq!(
            std::fs::read_to_string(dir.join(r.composite_key())).unwrap().trim(),
            digests[0]
        );
        // No temp file is left behind.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [r.composite_key()]);
        let base = BaseImage::local(&f.store, &Digest::parse(&digests[0]).unwrap(), "x").unwrap();
        assert_eq!(base.key_digest(), digests[0]);
    }

    #[test]
    fn a_layer_missing_from_the_store_is_infra_and_nothing_is_cached() {
        let f = fixture();
        let r = two_layers(&f.store);
        std::fs::remove_file(f.store.blob_path(&Digest::parse(&r.entries[1].digest).unwrap())).unwrap();
        let err = composite(&f.store, &f.home, &r, &platform()).unwrap_err();
        assert_eq!(err.exit_code(), 4, "{err}");
        assert!(err.to_string().contains("node"), "{err}");
        assert!(!f.home.root().join("composite").join(r.composite_key()).exists());
    }

    #[test]
    fn catalog_errors_keep_their_exit_codes() {
        use potter_catalog::CatalogError;
        for e in [
            CatalogError::Hostile("h".into()),
            CatalogError::Infra("i".into()),
            CatalogError::Unsatisfied("u".into()),
            CatalogError::Plan("p".into()),
        ] {
            let code = e.exit_code();
            assert_eq!(BuildError::from(e).exit_code(), code);
        }
    }
}
