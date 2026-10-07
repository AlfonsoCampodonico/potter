//! Unpacking layer tars into overlay lower directories (spec §9.4): whiteouts become 0/0
//! character devices and opaque markers `user.overlay.opaque`, as overlayfs with `userxattr`
//! reads them. Runs as root of the sandbox's user namespace (`potter __unpack`), so ownership
//! lands in the subordinate range. Layers are untrusted: a path that leaves the destination,
//! or that goes through a symlink, is a violation.

#[cfg(not(target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

use kiln_oci::Descriptor;
use kiln_store::Store;
use potter_cache::PotterHome;

use crate::error::BuildError;
use crate::sandboxed::{SandboxCtx, helper_error};

/// Bounds on one untrusted layer (P11): a decompression bomb stops here, not at a full disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackLimits {
    pub max_entries: u64,
    /// Bytes of file data written.
    pub max_bytes: u64,
}

impl Default for UnpackLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_bytes: 32 << 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnpackSummary {
    pub entries: u64,
    /// Character and block devices, which a user namespace cannot create.
    pub skipped_devices: u64,
}

/// `$POTTER_HOME/unpacked/<digest hex>`, unpacking `layer` there first if needed.
pub fn ensure_unpacked(
    ctx: &SandboxCtx,
    store: &Store,
    layer: &Descriptor,
    home: &PotterHome,
) -> Result<PathBuf, BuildError> {
    let root = home.unpacked();
    let done = root.join(layer.digest.hex());
    if done.is_dir() {
        return Ok(done);
    }
    std::fs::create_dir_all(&root).map_err(crate::error::io(format!("creating {}", root.display())))?;
    let tmp = tempfile::Builder::new()
        .prefix(&format!(".{}.", layer.digest.hex()))
        .tempdir_in(&root)
        .map_err(crate::error::io("creating an unpack directory"))?
        .keep();
    let blob = store.blob_path(&layer.digest);
    let args = vec![
        "__unpack".to_string(),
        blob.display().to_string(),
        layer.media_type.clone(),
        tmp.display().to_string(),
    ];
    let out = ctx.run_host(&ctx.potter, &args, std::slice::from_ref(&tmp))?;
    if !out.status.success() {
        let _ = ctx.remove(&tmp);
        return Err(helper_error(&format!("unpacking layer {}", layer.digest), &out));
    }
    match std::fs::rename(&tmp, &done) {
        Ok(()) => Ok(done),
        // Another build unpacked it first.
        Err(_) if done.is_dir() => {
            let _ = ctx.remove(&tmp);
            Ok(done)
        }
        Err(e) => {
            let _ = ctx.remove(&tmp);
            Err(crate::error::io(format!("renaming into {}", done.display()))(e))
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{unpack, unpack_blob};

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{
        AtFlags, CWD, Dir, FileType, Gid, Mode, OFlags, Timespec, Timestamps, Uid, XattrFlags, fchmod, fchown,
        fsetxattr, futimens, linkat, mkdirat, mknodat, openat, statat, symlinkat, unlinkat, utimensat,
    };

    use super::{UnpackLimits, UnpackSummary};
    use crate::error::{BuildError, io};
    use crate::merge::normalise;
    use crate::sandboxed::IDS;

    fn hostile(what: impl std::fmt::Display) -> BuildError {
        BuildError::Hostile(what.to_string())
    }

    fn open_dir(parent: &OwnedFd, name: &str) -> rustix::io::Result<OwnedFd> {
        openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
    }

    /// The directory holding `path`'s last component, created (0755, root) where missing.
    /// Every component is opened without following symlinks.
    fn parent_of(dest: &OwnedFd, parts: &[&str], path: &str) -> Result<OwnedFd, BuildError> {
        let mut dir = open_dir(dest, ".").map_err(|e| io("opening the destination")(e.into()))?;
        for part in parts {
            dir = match open_dir(&dir, part) {
                Ok(d) => d,
                Err(rustix::io::Errno::NOENT) => {
                    mkdirat(&dir, *part, Mode::from_raw_mode(0o755))
                        .map_err(|e| io(format!("creating a parent of {path}"))(e.into()))?;
                    open_dir(&dir, part).map_err(|e| io(format!("opening a parent of {path}"))(e.into()))?
                }
                Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                    return Err(hostile(format!(
                        "the layer path {path:?} goes through a symlink or a file"
                    )));
                }
                Err(e) => return Err(io(format!("opening a parent of {path}"))(e.into())),
            };
        }
        Ok(dir)
    }

    /// Removes `name` in `dir`, whatever it is (a directory with its whole subtree).
    fn remove(dir: &OwnedFd, name: &str) -> Result<(), BuildError> {
        let st = match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(e) => return Err(io(format!("stat {name}"))(e.into())),
        };
        if FileType::from_raw_mode(st.st_mode) == FileType::Directory {
            let sub = open_dir(dir, name).map_err(|e| io(format!("opening {name}"))(e.into()))?;
            let listing = open_dir(&sub, ".").map_err(|e| io(format!("listing {name}"))(e.into()))?;
            let names: Vec<String> = Dir::new(listing)
                .map_err(|e| io(format!("listing {name}"))(e.into()))?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "." && n != "..")
                .collect();
            for n in names {
                remove(&sub, &n)?;
            }
            unlinkat(dir, name, AtFlags::REMOVEDIR).map_err(|e| io(format!("removing {name}"))(e.into()))
        } else {
            unlinkat(dir, name, AtFlags::empty()).map_err(|e| io(format!("removing {name}"))(e.into()))
        }
    }

    fn ids(uid: u64, gid: u64, path: &str) -> Result<(Uid, Gid), BuildError> {
        let (Ok(u), Ok(g)) = (u32::try_from(uid), u32::try_from(gid)) else {
            return Err(hostile(format!("{path}: ids {uid}:{gid} are out of range")));
        };
        if u > IDS || g > IDS {
            return Err(hostile(format!(
                "{path}: ids {uid}:{gid} are beyond the {IDS} mapped ids"
            )));
        }
        Ok((Uid::from_raw(u), Gid::from_raw(g)))
    }

    fn times(mtime: u64) -> Timestamps {
        let t = Timespec {
            tv_sec: mtime as i64,
            tv_nsec: 0,
        };
        Timestamps {
            last_access: t,
            last_modification: t,
        }
    }

    /// Unpacks the tar stream `reader` into the empty directory `dest`.
    pub fn unpack(reader: impl Read, dest: &Path, limits: &UnpackLimits) -> Result<UnpackSummary, BuildError> {
        let dest = openat(
            CWD,
            dest,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io(format!("opening {}", dest.display()))(e.into()))?;
        let mut summary = UnpackSummary::default();
        let mut written = 0u64;
        let mut archive = tar::Archive::new(reader);
        for entry in archive
            .entries()
            .map_err(|e| hostile(format!("not a tar stream: {e}")))?
        {
            let mut entry = entry.map_err(|e| hostile(format!("a bad tar entry: {e}")))?;
            let Some(path) = normalise(&entry.path_bytes())? else {
                continue;
            };
            summary.entries += 1;
            if summary.entries > limits.max_entries {
                return Err(hostile(format!(
                    "the layer has more than {} entries",
                    limits.max_entries
                )));
            }
            let parts: Vec<&str> = path.split('/').collect();
            let (name, dirs) = parts.split_last().expect("a normalised path has a component");
            let parent = parent_of(&dest, dirs, &path)?;
            if *name == ".wh..wh..opq" {
                fsetxattr(&parent, "user.overlay.opaque", b"y", XattrFlags::empty())
                    .map_err(|e| io(format!("marking {path} opaque"))(e.into()))?;
                continue;
            }
            if let Some(hidden) = name.strip_prefix(".wh.") {
                remove(&parent, hidden)?;
                mknodat(&parent, hidden, FileType::CharacterDevice, Mode::empty(), 0)
                    .map_err(|e| io(format!("whiting out {path}"))(e.into()))?;
                continue;
            }
            let h = entry.header().clone();
            let bad = |e: std::io::Error| hostile(format!("{path}: {e}"));
            let (uid, gid) = ids(h.uid().map_err(bad)?, h.gid().map_err(bad)?, &path)?;
            let mode = Mode::from_raw_mode(h.mode().map_err(bad)? & 0o7777);
            let mtime = times(h.mtime().map_err(bad)?);
            let mut xattrs = Vec::new();
            if let Some(pax) = entry.pax_extensions().map_err(bad)? {
                for ext in pax {
                    let ext = ext.map_err(bad)?;
                    if let Some(name) = ext.key().ok().and_then(|k| k.strip_prefix("SCHILY.xattr."))
                        // overlayfs's own markers come only from whiteout entries: a base layer
                        // must not hide files from the overlay that potter's own reads still see.
                        && name.starts_with("user.")
                        && !name.starts_with("user.overlay.")
                    {
                        xattrs.push((name.to_string(), ext.value_bytes().to_vec()));
                    }
                }
            }
            let kind = h.entry_type();
            if kind != tar::EntryType::Directory {
                remove(&parent, name)?;
            }
            match kind {
                tar::EntryType::Directory => {
                    match statat(&parent, *name, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Directory => {}
                        Ok(_) => {
                            remove(&parent, name)?;
                            mkdirat(&parent, *name, Mode::from_raw_mode(0o700))
                                .map_err(|e| io(format!("creating {path}"))(e.into()))?;
                        }
                        Err(_) => mkdirat(&parent, *name, Mode::from_raw_mode(0o700))
                            .map_err(|e| io(format!("creating {path}"))(e.into()))?,
                    }
                    let d = open_dir(&parent, name).map_err(|e| io(format!("opening {path}"))(e.into()))?;
                    finish(&d, uid, gid, mode, mtime, &xattrs, &path)?;
                }
                tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => {
                    let f = openat(
                        &parent,
                        *name,
                        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::from_raw_mode(0o600),
                    )
                    .map_err(|e| io(format!("creating {path}"))(e.into()))?;
                    let mut file = std::fs::File::from(f);
                    let room = limits.max_bytes.saturating_sub(written);
                    let n = std::io::copy(&mut (&mut entry).take(room + 1), &mut file)
                        .map_err(|e| hostile(format!("{path}: {e}")))?;
                    written += n;
                    if written > limits.max_bytes {
                        return Err(hostile(format!("the layer holds more than {} bytes", limits.max_bytes)));
                    }
                    file.flush().map_err(io(format!("writing {path}")))?;
                    finish(
                        &file.as_fd().try_clone_to_owned().map_err(io("dup"))?,
                        uid,
                        gid,
                        mode,
                        mtime,
                        &xattrs,
                        &path,
                    )?;
                }
                tar::EntryType::Symlink => {
                    let target = entry.link_name_bytes().unwrap_or_default();
                    let target = String::from_utf8_lossy(&target).into_owned();
                    symlinkat(target.as_str(), &parent, *name).map_err(|e| io(format!("linking {path}"))(e.into()))?;
                    rustix::fs::chownat(&parent, *name, Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|e| io(format!("chown {path}"))(e.into()))?;
                    utimensat(&parent, *name, &mtime, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|e| io(format!("times of {path}"))(e.into()))?;
                }
                tar::EntryType::Link => {
                    let target = entry.link_name_bytes().unwrap_or_default();
                    let Some(target) = normalise(&target)? else {
                        return Err(hostile(format!("{path}: a hardlink to the root")));
                    };
                    let tparts: Vec<&str> = target.split('/').collect();
                    let (tname, tdirs) = tparts.split_last().expect("a normalised path has a component");
                    let tparent = parent_of(&dest, tdirs, &target)?;
                    linkat(&tparent, *tname, &parent, *name, AtFlags::empty())
                        .map_err(|e| hostile(format!("{path}: a hardlink to {target}: {e}")))?;
                }
                tar::EntryType::Fifo => {
                    mknodat(&parent, *name, FileType::Fifo, mode, 0)
                        .map_err(|e| io(format!("creating {path}"))(e.into()))?;
                    rustix::fs::chownat(&parent, *name, Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|e| io(format!("chown {path}"))(e.into()))?;
                }
                tar::EntryType::Char | tar::EntryType::Block => summary.skipped_devices += 1,
                _ => {}
            }
        }
        Ok(summary)
    }

    /// Ownership, then the mode (chown clears set-id bits), then times and user xattrs.
    fn finish(
        fd: &OwnedFd,
        uid: Uid,
        gid: Gid,
        mode: Mode,
        mtime: Timestamps,
        xattrs: &[(String, Vec<u8>)],
        path: &str,
    ) -> Result<(), BuildError> {
        fchown(fd, Some(uid), Some(gid)).map_err(|e| io(format!("chown {path}"))(e.into()))?;
        fchmod(fd, mode).map_err(|e| io(format!("chmod {path}"))(e.into()))?;
        for (k, v) in xattrs {
            fsetxattr(fd, k.as_str(), v, XattrFlags::empty())
                .map_err(|e| io(format!("xattr {k} of {path}"))(e.into()))?;
        }
        futimens(fd, &mtime).map_err(|e| io(format!("times of {path}"))(e.into()))
    }

    /// `potter __unpack`: the blob at `blob`, of `media_type`, into `dest`.
    pub fn unpack_blob(blob: &Path, media_type: &str, dest: &Path) -> Result<UnpackSummary, BuildError> {
        let compression = kiln_oci::media::layer_compression(media_type)
            .map_err(|e| BuildError::Plan(format!("layer {}: {e}", blob.display())))?;
        let file = std::fs::File::open(blob).map_err(io(format!("opening {}", blob.display())))?;
        let reader = crate::base::decompress(file, compression).map_err(io("decompressing"))?;
        unpack(reader, dest, &UnpackLimits::default())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn unpack_blob(_blob: &Path, _media_type: &str, _dest: &Path) -> Result<UnpackSummary, BuildError> {
    Err(BuildError::Infra("unpacking layers needs Linux".into()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use kiln_erofs::testtar::{Opts, TarBuilder};

    use super::*;

    /// Entries owned by the caller: unpacking them needs no namespace.
    fn mine() -> Opts {
        use std::os::unix::fs::MetadataExt;
        let me = std::fs::metadata("/proc/self").unwrap();
        Opts::default()
            .uid(u64::from(me.uid()))
            .gid(u64::from(me.gid()))
            .mode(0o644)
    }

    #[test]
    fn unpacking_stops_at_its_entry_and_byte_limits() {
        let mut t = TarBuilder::new();
        t.file("a", b"0123456789", &mine())
            .file("b", b"x", &mine())
            .file("c", b"y", &mine());
        let tar = t.finish();
        let few = UnpackLimits {
            max_entries: 2,
            max_bytes: 1 << 20,
        };
        let small = UnpackLimits {
            max_entries: 100,
            max_bytes: 5,
        };
        for limits in [few, small] {
            let dest = tempfile::tempdir().unwrap();
            let err = unpack(std::io::Cursor::new(tar.clone()), dest.path(), &limits).unwrap_err();
            assert_eq!(err.exit_code(), 3, "{err}");
        }
        let dest = tempfile::tempdir().unwrap();
        unpack(std::io::Cursor::new(tar), dest.path(), &UnpackLimits::default()).unwrap();
    }
}
