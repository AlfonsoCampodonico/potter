//! Cache mounts in VM builds (spec §8.5, decisions D7–D9): one ext4 disk per cache id, attached
//! at boot for a VM's whole segment. A persistent disk lives at `<root>/<id>.ext4`, locked by
//! `flock` on `<root>/<id>.lock` for as long as the VM holds it. A disk another build holds is
//! replaced by an empty one in the VM's directory.

use std::path::{Path, PathBuf};

use crate::error::{BuildError, io};

/// A cache disk's size. Cloud Hypervisor 53 can't grow a disk online much past this (D9).
pub const CACHE_DISK_BYTES: u64 = 8 << 30;

/// A cache id mounted at a target, as a step uses it. `target` is already cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheUse {
    pub id: String,
    pub target: String,
}

/// Adds `step`'s uses not already in `set`. `false`, with `set` unchanged, when one of them maps
/// an id to another target than `set` (or `step` itself) does, or a target to another id: such a
/// step needs a VM of its own (D9).
pub fn add_uses(set: &mut Vec<CacheUse>, step: &[CacheUse]) -> bool {
    let mut out = set.clone();
    for u in step {
        if out.contains(u) {
            continue;
        }
        if out.iter().any(|o| o.id == u.id || o.target == u.target) {
            return false;
        }
        out.push(u.clone());
    }
    *set = out;
    true
}

/// A cache disk attached to a VM: the persistent one with its lock held, or an empty stand-in.
#[derive(Debug)]
pub struct HeldCache {
    pub path: PathBuf,
    pub target: String,
    persistent: bool,
    /// Holds the `flock` on `<id>.lock` until dropped.
    lock: Option<std::fs::File>,
}

/// Whether `id` can name files under the cache root: a plan name (no `/`), and not `.` or `..`.
fn valid_id(id: &str) -> bool {
    potter_plan::is_name(id) && id != "." && id != ".."
}

/// Takes the disk of each of `uses` (distinct ids) under `root`, creating missing ones. A disk
/// another build holds becomes an empty one under `scratch`, with a warning. Every lock taken is
/// released when the returned disks drop, including on an error here.
pub fn acquire(
    root: &Path,
    uses: &[CacheUse],
    scratch: &Path,
    warn: &mut dyn FnMut(String),
) -> Result<Vec<HeldCache>, BuildError> {
    let mut held = Vec::with_capacity(uses.len());
    if uses.is_empty() {
        return Ok(held);
    }
    std::fs::create_dir_all(root).map_err(io(format!("creating {}", root.display())))?;
    for u in uses {
        if !valid_id(&u.id) {
            return Err(BuildError::Plan(format!("the cache mount id {:?} is not a name", u.id)));
        }
        let lock_path = root.join(format!("{}.lock", u.id));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&lock_path)
            .map_err(io(format!("opening {}", lock_path.display())))?;
        let free = rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok();
        if !free {
            warn(format!(
                "cache mount {} is in use by another build; using an empty one",
                u.id
            ));
            let path = scratch.join(format!("cache-{}.ext4", u.id));
            create(&path)?;
            held.push(HeldCache {
                path,
                target: u.target.clone(),
                persistent: false,
                lock: None,
            });
            continue;
        }
        let path = root.join(format!("{}.ext4", u.id));
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => {
                return Err(BuildError::Infra(format!(
                    "the cache disk {} is not a regular file",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Made aside and renamed, so a disk at `path` is always whole.
                let tmp = tempfile::Builder::new()
                    .prefix(".new-")
                    .tempdir_in(root)
                    .map_err(io(format!("creating a directory in {}", root.display())))?;
                let fresh = tmp.path().join("disk.ext4");
                create(&fresh)?;
                std::fs::rename(&fresh, &path).map_err(io(format!("creating {}", path.display())))?;
            }
            Err(e) => return Err(io(format!("reading {}", path.display()))(e)),
        }
        held.push(HeldCache {
            path,
            target: u.target.clone(),
            persistent: true,
            lock: Some(lock),
        });
    }
    Ok(held)
}

/// An empty, sparse cache disk at `path`, from kiln's ext4 template.
fn create(path: &Path) -> Result<(), BuildError> {
    kiln_image::scratch::create_scratch(path, CACHE_DISK_BYTES)
        .map_err(|e| BuildError::Infra(format!("creating the cache disk {}: {e}", path.display())))
}

/// Deletes the persistent disks among `held`, best-effort: their VM died mid-step and may have
/// left them inconsistent (D8). Only disks whose lock this build holds are persistent, so no
/// other build's disk is touched; the locks stay held until `held` drops.
pub fn discard(held: &[HeldCache]) {
    for h in held.iter().filter(|h| h.persistent && h.lock.is_some()) {
        let _ = std::fs::remove_file(&h.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(id: &str, t: &str) -> CacheUse {
        CacheUse {
            id: id.into(),
            target: t.into(),
        }
    }

    #[test]
    fn add_uses_refuses_an_id_or_target_mapped_twice() {
        let mut set = vec![];
        assert!(add_uses(&mut set, &[u("npm", "/root/.npm")]));
        assert!(add_uses(
            &mut set,
            &[u("npm", "/root/.npm"), u("pip", "/root/.cache/pip")]
        ));
        assert_eq!(set.len(), 2);
        assert!(!add_uses(&mut set, &[u("npm", "/other")]));
        assert!(!add_uses(&mut set, &[u("yarn", "/root/.npm")]));
        assert_eq!(set.len(), 2, "unchanged after a conflict");
    }

    #[test]
    fn add_uses_refuses_a_step_that_maps_one_id_twice_itself() {
        let mut set = vec![];
        assert!(!add_uses(&mut set, &[u("c", "/x"), u("c", "/y")]));
        assert!(set.is_empty());
    }

    #[test]
    fn a_disk_is_made_once_locked_and_given_up_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let (root, scratch) = (dir.path().join("root"), dir.path().join("scratch"));
        std::fs::create_dir(&scratch).unwrap();
        let mut lines = Vec::new();
        let held = acquire(&root, &[u("npm", "/root/.npm")], &scratch, &mut |l| lines.push(l)).unwrap();
        assert_eq!(held[0].path, root.join("npm.ext4"));
        assert_eq!(std::fs::metadata(&held[0].path).unwrap().len(), CACHE_DISK_BYTES);
        std::fs::write(root.join("marker"), "").unwrap();
        // Held: a second build gets an empty disk in its own directory.
        let other = acquire(&root, &[u("npm", "/root/.npm")], &scratch, &mut |l| lines.push(l)).unwrap();
        assert_eq!(other[0].path, scratch.join("cache-npm.ext4"));
        assert_eq!(
            lines,
            ["cache mount npm is in use by another build; using an empty one"]
        );
        // Discarding the stand-in touches nothing of the holder's.
        discard(&other);
        assert!(root.join("npm.ext4").exists());
        drop(other);
        drop(held);
        // Released: the next build gets the same disk back, not a new one.
        let again = acquire(&root, &[u("npm", "/root/.npm")], &scratch, &mut |_| {}).unwrap();
        assert_eq!(again[0].path, root.join("npm.ext4"));
        assert!(again[0].lock.is_some());
        discard(&again);
        assert!(!root.join("npm.ext4").exists());
        // Only the disk and its lock: no temporary left behind.
        let mut names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["marker", "npm.lock"]);
    }

    #[test]
    fn an_error_partway_releases_the_locks_taken() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("bad.ext4")).unwrap();
        let uses = [u("good", "/a"), u("bad", "/b")];
        let e = acquire(&root, &uses, dir.path(), &mut |_| {}).unwrap_err();
        assert_eq!(e.exit_code(), 4, "{e}");
        let held = acquire(&root, &uses[..1], dir.path(), &mut |_| panic!("still locked")).unwrap();
        assert!(held[0].persistent);
    }

    #[test]
    fn ids_that_could_leave_the_root_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["..", ".", "a/b", ""] {
            let e = acquire(dir.path(), &[u(id, "/a")], dir.path(), &mut |_| {}).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{id}: {e}");
        }
    }
}
