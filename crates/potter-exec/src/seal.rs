//! Sealing a step (spec §9.4): its overlay upper as an OCI tar diff. Runs as root of the
//! sandbox's user namespace (`potter __seal`), so files of any subordinate owner are readable.
//! The upper is the step's work and untrusted: nothing is followed, and limits bound the walk.

use std::path::Path;

use kiln_store::{Digest, Store};

use crate::error::{BuildError, io};
use crate::sandboxed::{SandboxCtx, helper_error};

#[derive(Debug, Clone, Copy)]
pub struct SealLimits {
    pub max_bytes: u64,
    pub max_entries: u64,
    pub max_depth: usize,
    pub max_name: usize,
}

impl Default for SealLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 << 30,
            max_entries: 1_000_000,
            max_depth: 128,
            max_name: 255,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SealSummary {
    pub entries: u64,
    pub bytes: u64,
}

/// Seals `upper` inside the sandbox into `out_dir/diff.tar`, then stores it.
pub fn seal_in_sandbox(
    ctx: &SandboxCtx,
    upper: &Path,
    out_dir: &Path,
    store: &Store,
    skip_if_empty: &[String],
) -> Result<(Digest, u64), BuildError> {
    let out = out_dir.join("diff.tar");
    let mut args = vec![
        "__seal".to_string(),
        upper.display().to_string(),
        out.display().to_string(),
    ];
    for s in skip_if_empty {
        args.extend(["--skip".to_string(), s.clone()]);
    }
    let run = ctx.run_host(&ctx.potter, &args, &[out_dir.to_path_buf()])?;
    if !run.status.success() {
        return Err(helper_error("sealing the step", &run));
    }
    let file = std::fs::File::open(&out).map_err(io(format!("opening {}", out.display())))?;
    crate::ingest::validate_layer(std::io::BufReader::new(file), out_dir)?;
    let mut file = std::fs::File::open(&out).map_err(io(format!("opening {}", out.display())))?;
    let stored = store
        .put_reader(&mut file)
        .map_err(|e| BuildError::Infra(format!("storing the step's layer: {e}")))?;
    let _ = std::fs::remove_file(&out);
    Ok(stored)
}

#[cfg(target_os = "linux")]
pub use linux::{seal, seal_to_file};

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::path::Path;

    use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, fgetxattr, flistxattr, openat, readlinkat, statat};

    use super::{SealLimits, SealSummary};
    use crate::error::{BuildError, io};
    use crate::inputs::{names, open_beneath, open_dir};
    use crate::tarball::{Meta, TarWriter};

    enum Item {
        Dir {
            opaque: bool,
        },
        /// `inode` is set when the file has other links.
        File {
            size: u64,
            inode: Option<(u64, u64)>,
        },
        Hardlink(String),
        Symlink(String),
        Fifo,
        Whiteout,
    }

    struct Entry {
        rel: String,
        item: Item,
        meta: Meta,
        xattrs: Vec<(String, Vec<u8>)>,
    }

    /// The xattrs worth keeping: user ones (not overlay's own) and file capabilities.
    fn xattrs(fd: &OwnedFd) -> Vec<(String, Vec<u8>)> {
        let mut names = vec![0u8; 4096];
        let Ok(n) = flistxattr(fd, &mut names) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for name in names[..n].split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let Ok(name) = std::str::from_utf8(name) else { continue };
            let keep =
                (name.starts_with("user.") && !name.starts_with("user.overlay.")) || name == "security.capability";
            if !keep {
                continue;
            }
            let mut value = vec![0u8; 65536];
            if let Ok(len) = fgetxattr(fd, name, &mut value) {
                value.truncate(len);
                out.push((name.to_string(), value));
            }
        }
        out.sort();
        out
    }

    fn is_opaque(fd: &OwnedFd) -> bool {
        let mut v = [0u8; 4];
        matches!(fgetxattr(fd, "user.overlay.opaque", &mut v), Ok(1) if v[0] == b'y')
    }

    struct Walk<'a> {
        limits: &'a SealLimits,
        skip: &'a [String],
        entries: Vec<Entry>,
        bytes: u64,
        epoch: u64,
    }

    impl Walk<'_> {
        fn walk(&mut self, dir: &OwnedFd, rel_dir: &str, depth: usize) -> Result<(), BuildError> {
            if depth > self.limits.max_depth {
                return Err(BuildError::Hostile(format!(
                    "{rel_dir}: deeper than {} directories",
                    self.limits.max_depth
                )));
            }
            let names = names(dir, rel_dir).map_err(|e| match e {
                // A step may name its files as it likes; layers need UTF-8 paths.
                BuildError::Plan(m) => BuildError::StepOutput(format!("the step's output cannot be a layer: {m}")),
                e => BuildError::Hostile(e.to_string()),
            })?;
            for name in names {
                if name.len() > self.limits.max_name {
                    return Err(BuildError::Hostile(format!(
                        "{rel_dir}: a name longer than {} bytes",
                        self.limits.max_name
                    )));
                }
                let rel = if rel_dir.is_empty() {
                    name.clone()
                } else {
                    format!("{rel_dir}/{name}")
                };
                if self.entries.len() as u64 >= self.limits.max_entries {
                    return Err(BuildError::StepOutput(format!(
                        "the build output exceeded {} entries",
                        self.limits.max_entries
                    )));
                }
                let st = statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|e| io(format!("stat {rel}"))(e.into()))?;
                let meta = Meta {
                    mode: st.st_mode & 0o7777,
                    uid: u64::from(st.st_uid),
                    gid: u64::from(st.st_gid),
                    mtime: self.epoch,
                };
                let kind = FileType::from_raw_mode(st.st_mode);
                let skippable = self.skip.iter().any(|s| s == &rel);
                match kind {
                    FileType::Directory => {
                        let child = open_dir(dir, &name, &rel)?;
                        let opaque = is_opaque(&child);
                        let mark = self.entries.len();
                        self.entries.push(Entry {
                            rel: rel.clone(),
                            item: Item::Dir { opaque },
                            meta,
                            xattrs: xattrs(&child),
                        });
                        let before = self.entries.len();
                        self.walk(&child, &rel, depth + 1)?;
                        if skippable && self.entries.len() == before && !opaque {
                            self.entries.truncate(mark);
                        }
                    }
                    FileType::RegularFile => {
                        let size = st.st_size as u64;
                        if skippable && size == 0 {
                            continue;
                        }
                        let inode = (st.st_nlink > 1).then_some((st.st_dev, st.st_ino));
                        self.bytes += size;
                        if self.bytes > self.limits.max_bytes {
                            return Err(BuildError::StepOutput(format!(
                                "the build output exceeded {} bytes",
                                self.limits.max_bytes
                            )));
                        }
                        let fd = openat(
                            dir,
                            name.as_str(),
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                        .map_err(|e| io(format!("opening {rel}"))(e.into()))?;
                        self.entries.push(Entry {
                            rel,
                            item: Item::File { size, inode },
                            meta,
                            xattrs: xattrs(&fd),
                        });
                    }
                    FileType::Symlink => {
                        let target = readlinkat(dir, name.as_str(), Vec::new())
                            .map_err(|e| io(format!("readlink {rel}"))(e.into()))?;
                        let target = target.to_string_lossy().into_owned();
                        self.entries.push(Entry {
                            rel,
                            item: Item::Symlink(target),
                            meta,
                            xattrs: Vec::new(),
                        });
                    }
                    FileType::CharacterDevice if st.st_rdev == 0 => {
                        self.entries.push(Entry {
                            rel,
                            item: Item::Whiteout,
                            meta,
                            xattrs: Vec::new(),
                        });
                    }
                    FileType::Fifo => self.entries.push(Entry {
                        rel,
                        item: Item::Fifo,
                        meta,
                        xattrs: Vec::new(),
                    }),
                    // Sockets and devices are not part of an image.
                    _ => {}
                }
            }
            Ok(())
        }
    }

    /// The OCI tar diff of `upper` to `out`.
    pub fn seal(
        upper: &Path,
        out: impl Write,
        limits: &SealLimits,
        skip_if_empty: &[String],
        epoch: u64,
    ) -> Result<SealSummary, BuildError> {
        let root = openat(
            CWD,
            upper,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io(format!("opening {}", upper.display()))(e.into()))?;
        let mut walk = Walk {
            limits,
            skip: skip_if_empty,
            entries: Vec::new(),
            bytes: 0,
            epoch,
        };
        walk.walk(&root, "", 1)?;
        walk.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
        // Of the paths of one inode, the first in stream order holds the data; the others link
        // to it, after it, with its metadata (readers apply a link's attributes to the inode).
        let mut holders: HashMap<(u64, u64), (String, Meta)> = HashMap::new();
        for e in &mut walk.entries {
            if let Item::File { inode: Some(key), .. } = e.item {
                match holders.get(&key) {
                    Some((first, meta)) => {
                        e.item = Item::Hardlink(first.clone());
                        e.meta = *meta;
                        e.xattrs.clear();
                    }
                    None => {
                        holders.insert(key, (e.rel.clone(), e.meta));
                    }
                }
            }
        }
        let mut tar = TarWriter::new(out);
        let mut summary = SealSummary::default();
        for e in &walk.entries {
            summary.entries += 1;
            let path = &e.rel;
            match &e.item {
                Item::Dir { opaque } => tar
                    .pax_xattrs(&e.xattrs)
                    .and_then(|()| tar.dir(path, &e.meta))
                    .and_then(|()| if *opaque { tar.opaque(path) } else { Ok(()) }),
                Item::File { size, .. } => {
                    let fd = open_beneath(&root, path)?;
                    summary.bytes += size;
                    tar.pax_xattrs(&e.xattrs)
                        .and_then(|()| tar.file(path, &e.meta, *size, &mut std::fs::File::from(fd)))
                }
                Item::Hardlink(target) => tar.hardlink(path, target, &e.meta),
                Item::Symlink(target) => tar.symlink(path, target, &e.meta),
                Item::Fifo => tar.fifo(path, &e.meta),
                Item::Whiteout => tar.whiteout(path),
            }
            .map_err(io(format!("writing {path}")))?;
        }
        tar.finish().map_err(io("finishing the diff"))?;
        Ok(summary)
    }

    /// `potter __seal`: `upper` into a new file `out`.
    pub fn seal_to_file(upper: &Path, out: &Path, skip_if_empty: &[String]) -> Result<SealSummary, BuildError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .map_err(io(format!("creating {}", out.display())))?;
        let mut w = std::io::BufWriter::new(file);
        let summary = seal(
            upper,
            &mut w,
            &SealLimits::default(),
            skip_if_empty,
            crate::tarball::epoch(),
        )?;
        w.flush().map_err(io("writing the diff"))?;
        Ok(summary)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn seal_to_file(_upper: &Path, _out: &Path, _skip_if_empty: &[String]) -> Result<SealSummary, BuildError> {
    Err(BuildError::Infra("sealing needs Linux".into()))
}
