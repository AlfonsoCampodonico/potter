//! The VM's layer disks (spec §9.3): each layer as a read-only erofs, cached by its diff id,
//! within the VMM's device budget. When the prefix doesn't fit, its bottom layers are squashed
//! into one erofs, cached by their digests.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use kiln_erofs::{Image, LayerWriter, Limits};
use kiln_store::Store;
use potter_cache::PotterHome;
use sha2::{Digest as _, Sha256};

use super::backend::fixed_devices;
use crate::base::layer_reader;
use crate::chain::LayerRef;
use crate::error::{BuildError, io};

/// How a VM's layer disks are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskPlan {
    /// How many bottom layers merge into one disk (0: none).
    pub squash: usize,
    /// Layer disks the VM boots with.
    pub disks: usize,
    /// Slots left for layers attached mid-chain.
    pub spare: usize,
}

/// Names the conversion in cache keys: a new kiln-erofs layout or potter conversion rule gets
/// new files instead of reading old ones.
pub const CACHE_VERSION: &str = "v1-kiln-erofs-01e7a9f";

/// Lays out `prefix` layers beside `mounts` cache disks on a VMM with `max_devices` (a NIC
/// taking one when `network`), keeping up to `reserve` slots for the chain's later layers.
pub fn plan_disks(
    prefix: usize,
    mounts: usize,
    max_devices: usize,
    reserve: usize,
    network: bool,
) -> Result<DiskPlan, BuildError> {
    let fixed = fixed_devices(network);
    let budget = max_devices
        .checked_sub(fixed + mounts)
        .filter(|&b| b > 0)
        .ok_or_else(|| {
            BuildError::Infra(format!(
                "the VMM's {max_devices} devices leave no room for a layer disk beside {fixed} fixed \
                 devices and {mounts} cache mounts"
            ))
        })?;
    // The reserve gives way before the prefix shrinks below one disk.
    let layers = budget - reserve.min(budget - 1);
    if prefix <= layers {
        return Ok(DiskPlan {
            squash: 0,
            disks: prefix,
            spare: budget - prefix,
        });
    }
    Ok(DiskPlan {
        squash: prefix - layers + 1,
        disks: layers,
        spare: budget - layers,
    })
}

fn dir(home: &PotterHome) -> Result<PathBuf, BuildError> {
    let dir = home.root().join("erofs");
    std::fs::create_dir_all(&dir).map_err(io(format!("creating {}", dir.display())))?;
    Ok(dir)
}

/// Writes an erofs to `path` with `fill`, through a temporary file renamed into place.
fn cached(
    home: &PotterHome,
    path: PathBuf,
    fill: impl FnOnce(std::fs::File, &Path) -> Result<std::fs::File, BuildError>,
) -> Result<PathBuf, BuildError> {
    if path.exists() {
        return Ok(path);
    }
    let spill = home.runs();
    std::fs::create_dir_all(&spill).map_err(io("creating the runs directory"))?;
    let parent = path.parent().expect("a file in the erofs directory");
    let tmp = tempfile::NamedTempFile::new_in(parent).map_err(io("creating an erofs file"))?;
    let file = tmp.reopen().map_err(io("opening an erofs file"))?;
    let file = fill(file, &spill)?;
    file.sync_all().map_err(io("syncing an erofs file"))?;
    tmp.persist(&path)
        .map_err(|e| BuildError::Infra(format!("keeping {}: {e}", path.display())))?;
    Ok(path)
}

/// Hashes what passes through it.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The erofs of `layer`, converted once and kept in `$POTTER_HOME/erofs/<version>-<diff id>`.
///
/// A base's diff ids come from its config, which nothing verified: the uncompressed stream is
/// hashed during conversion and must be the diff id, or nothing is kept. A file under a diff id
/// therefore always holds that diff id's content, for every build that shares the cache.
pub fn erofs_of(store: &Store, home: &PotterHome, layer: &LayerRef) -> Result<PathBuf, BuildError> {
    let path = dir(home)?.join(format!("{CACHE_VERSION}-{}", layer.diff_id.hex()));
    cached(home, path, |out, spill| {
        let fail = |e: kiln_erofs::Error| BuildError::Infra(format!("converting layer {}: {e}", layer.diff_id));
        let mut tar = Hashing {
            inner: layer_reader(store, &layer.descriptor)?,
            hasher: Sha256::new(),
        };
        let mut w = LayerWriter::new(out, spill, Limits::default()).map_err(fail)?;
        w.append_tar(&mut tar).map_err(fail)?;
        // The writer stops at the end-of-archive marker; the diff id covers every byte.
        std::io::copy(&mut tar, &mut std::io::sink()).map_err(io(format!("reading layer {}", layer.diff_id)))?;
        let actual = hex(&tar.hasher.finalize());
        if actual != layer.diff_id.hex() {
            return Err(BuildError::Hostile(format!(
                "layer {} uncompresses to sha256:{actual}, not the diff id its image claims ({})",
                layer.descriptor.digest, layer.diff_id
            )));
        }
        Ok(w.finish(&BTreeMap::new()).map_err(fail)?.0)
    })
}

/// One erofs of `layers` (bottom first) merged, kept in `$POTTER_HOME/erofs/squash-<hash>`.
pub fn squashed(store: &Store, home: &PotterHome, layers: &[LayerRef]) -> Result<PathBuf, BuildError> {
    let mut h = Sha256::new();
    for l in layers {
        h.update(l.diff_id.to_string().as_bytes());
        h.update(b"\n");
    }
    // Keyed by diff ids: each is verified when its layer converts, below or earlier.
    let path = dir(home)?.join(format!("{CACHE_VERSION}-squash-{}", hex(&h.finalize())));
    cached(home, path, |out, spill| {
        let disks = layers
            .iter()
            .map(|l| erofs_of(store, home, l))
            .collect::<Result<Vec<_>, _>>()?;
        let fail = |e: kiln_erofs::Error| BuildError::Infra(format!("squashing {} layers: {e}", layers.len()));
        let mut images = disks
            .iter()
            .map(|p| {
                let f = std::fs::File::open(p).map_err(io(format!("opening {}", p.display())))?;
                Image::open(f).map_err(fail)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(kiln_erofs::squash(&mut images, out, spill).map_err(fail)?.0)
    })
}

#[cfg(test)]
mod tests {
    use kiln_erofs::Image;
    use kiln_oci::{Descriptor, media};

    use super::*;
    use crate::vm::fake::diff_tar;

    #[test]
    fn the_budget_squashes_only_what_does_not_fit() {
        // 31 devices with a NIC: 26 for layers and mounts.
        let p = |prefix, mounts, reserve| plan_disks(prefix, mounts, 31, reserve, true).unwrap();
        assert_eq!(
            p(3, 0, 0),
            DiskPlan {
                squash: 0,
                disks: 3,
                spare: 23
            }
        );
        assert_eq!(
            p(26, 0, 0),
            DiskPlan {
                squash: 0,
                disks: 26,
                spare: 0
            }
        );
        assert_eq!(
            p(27, 0, 0),
            DiskPlan {
                squash: 2,
                disks: 26,
                spare: 0
            }
        );
        assert_eq!(
            p(30, 2, 0),
            DiskPlan {
                squash: 7,
                disks: 24,
                spare: 0
            }
        );
        // Room kept for the chain's inputs squashes more of the prefix.
        assert_eq!(
            p(26, 0, 4),
            DiskPlan {
                squash: 5,
                disks: 22,
                spare: 4
            }
        );
        // The reserve gives way before the prefix shrinks below one disk.
        assert_eq!(
            p(10, 24, 9),
            DiskPlan {
                squash: 10,
                disks: 1,
                spare: 1
            }
        );
        // Without a NIC there is one more slot.
        assert_eq!(
            plan_disks(27, 0, 31, 0, false).unwrap(),
            DiskPlan {
                squash: 0,
                disks: 27,
                spare: 0
            }
        );
        // Firecracker on x86_64: 17 devices, 12 for layers and mounts with a NIC.
        assert_eq!(
            plan_disks(20, 0, 17, 0, true).unwrap(),
            DiskPlan {
                squash: 9,
                disks: 12,
                spare: 0
            }
        );
        assert_eq!(
            plan_disks(0, 0, 17, 0, true).unwrap(),
            DiskPlan {
                squash: 0,
                disks: 0,
                spare: 12
            }
        );
        // No room for even one layer disk.
        assert_eq!(plan_disks(3, 26, 31, 0, true).unwrap_err().exit_code(), 4);
    }

    #[test]
    fn a_layer_whose_content_is_not_its_diff_id_poisons_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let home = PotterHome::open(dir.path().join("home")).unwrap();
        let real = layer(&store, &[("app/x", "evil")]);
        // A base whose config claims another layer's diff id for this blob.
        let victim = layer(&store, &[("app/x", "good")]);
        let forged = LayerRef {
            descriptor: real.descriptor.clone(),
            diff_id: victim.diff_id.clone(),
        };
        let e = erofs_of(&store, &home, &forged).unwrap_err();
        assert_eq!(e.exit_code(), 3, "{e}");
        let e = squashed(&store, &home, &[forged.clone(), real.clone()]).unwrap_err();
        assert_eq!(e.exit_code(), 3, "{e}");
        let kept: Vec<_> = std::fs::read_dir(home.root().join("erofs"))
            .map(|d| d.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(
            kept.iter().all(|n| !n.to_string_lossy().contains(victim.diff_id.hex())),
            "nothing kept under the victim's id: {kept:?}"
        );
        // The victim still converts to its own content.
        let ok = erofs_of(&store, &home, &victim).unwrap();
        assert_eq!(read(&ok, "app/x").as_deref(), Some(&b"good"[..]));
        assert!(
            ok.file_name().unwrap().to_string_lossy().starts_with(CACHE_VERSION),
            "{ok:?}"
        );
    }

    fn layer(store: &Store, files: &[(&str, &str)]) -> LayerRef {
        let files: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
            .collect();
        let (tar, _) = diff_tar(&files);
        let d = store.put_bytes(&tar).unwrap();
        LayerRef {
            descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), tar.len() as u64),
            diff_id: d,
        }
    }

    fn read(path: &Path, file: &str) -> Option<Vec<u8>> {
        let mut img = Image::open(std::fs::File::open(path).unwrap()).unwrap();
        let nid = img.lookup(file.as_bytes()).unwrap()?;
        Some(img.read_data(nid).unwrap())
    }

    #[test]
    fn layers_convert_once_and_squash_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let home = PotterHome::open(dir.path().join("home")).unwrap();
        let a = layer(&store, &[("app/x", "1"), ("app/y", "y")]);
        let b = layer(&store, &[("app/x", "2")]);
        let ea = erofs_of(&store, &home, &a).unwrap();
        assert_eq!(read(&ea, "app/x").as_deref(), Some(&b"1"[..]));
        let modified = std::fs::metadata(&ea).unwrap().modified().unwrap();
        assert_eq!(erofs_of(&store, &home, &a).unwrap(), ea);
        assert_eq!(std::fs::metadata(&ea).unwrap().modified().unwrap(), modified, "cached");
        let sq = squashed(&store, &home, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(read(&sq, "app/x").as_deref(), Some(&b"2"[..]));
        assert_eq!(read(&sq, "app/y").as_deref(), Some(&b"y"[..]));
        assert_eq!(squashed(&store, &home, &[a.clone(), b.clone()]).unwrap(), sq);
        assert_ne!(
            squashed(&store, &home, &[b, a]).unwrap(),
            sq,
            "order is part of the key"
        );
    }
}
