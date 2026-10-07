//! Inputs layers (spec §9.2): the source files a step declares, as a deterministic layer.
//!
//! The walk holds a descriptor on each directory and opens every child relative to it with
//! `O_NOFOLLOW`, so nothing outside the target's root is ever reached, whatever a symlink or
//! a concurrent rename says. Symlinks are kept as symlinks, and refused when their target
//! leaves the root.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, fstat, openat, readlinkat, statat};

use crate::error::{BuildError, io};
use crate::tarball::{Meta, TarWriter};

/// Excluded wherever they appear as a path component.
pub const DEFAULT_EXCLUDES: [&str; 7] = [
    ".git",
    "node_modules",
    "vendor",
    "target",
    ".venv",
    "__pycache__",
    ".potter",
];

/// The largest `.potterignore` read.
const IGNORE_CAP: u64 = 1 << 20;

/// What to put in an inputs layer.
pub struct InputsSpec<'a> {
    /// The target's root on the host.
    pub root: &'a Path,
    /// Globs relative to the root; a pattern matching a directory takes its subtree.
    pub include: &'a [String],
    pub exclude: &'a [String],
    /// Absolute path the files land under.
    pub dest: &'a str,
    pub owner: (u64, u64),
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputsSummary {
    pub files: u64,
    pub bytes: u64,
}

fn glob(pattern: &str) -> Result<GlobMatcher, BuildError> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| BuildError::Plan(format!("the pattern {pattern:?} is invalid: {e}")))
}

/// `rel` and each of its ancestors (`a`, `a/b`, `a/b/c` for `a/b/c`).
fn self_and_ancestors(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/')
        .map(|(i, _)| &rel[..i])
        .chain(std::iter::once(rel))
}

fn matches(m: &GlobMatcher, rel: &str) -> bool {
    self_and_ancestors(rel).any(|p| m.is_match(p))
}

/// Include and exclude rules. `.potterignore` follows `.dockerignore`: patterns are relative to
/// the root (a leading `/` is dropped), later lines win, and `!` re-includes.
struct Rules {
    include: Vec<GlobMatcher>,
    exclude: Vec<GlobMatcher>,
    ignore: Vec<(GlobMatcher, bool)>,
}

impl Rules {
    fn new(spec: &InputsSpec, potterignore: &str) -> Result<Self, BuildError> {
        let mut ignore = Vec::new();
        for line in potterignore.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negated, pattern) = match line.strip_prefix('!') {
                Some(p) => (true, p.trim()),
                None => (false, line),
            };
            let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
            if !pattern.is_empty() {
                ignore.push((glob(pattern)?, negated));
            }
        }
        Ok(Self {
            include: spec
                .include
                .iter()
                .map(|p| glob(p.trim_start_matches('/')))
                .collect::<Result<_, _>>()?,
            exclude: spec
                .exclude
                .iter()
                .map(|p| glob(p.trim_start_matches('/')))
                .collect::<Result<_, _>>()?,
            ignore,
        })
    }

    fn included(&self, rel: &str) -> bool {
        self.include.iter().any(|m| matches(m, rel))
    }

    /// Excluded for good: nothing below can be re-included.
    fn excluded_for_good(&self, rel: &str) -> bool {
        rel.split('/').any(|c| DEFAULT_EXCLUDES.contains(&c)) || self.exclude.iter().any(|m| matches(m, rel))
    }

    fn excluded(&self, rel: &str) -> bool {
        if self.excluded_for_good(rel) {
            return true;
        }
        let mut excluded = false;
        for (m, negated) in &self.ignore {
            if matches(m, rel) {
                excluded = !negated;
            }
        }
        excluded
    }

    /// Whether a directory's subtree can be skipped.
    fn prune(&self, rel: &str) -> bool {
        self.excluded_for_good(rel) || (!self.ignore.iter().any(|(_, n)| *n) && self.excluded(rel))
    }

    fn wanted(&self, rel: &str) -> bool {
        self.included(rel) && !self.excluded(rel)
    }
}

/// An entry of the layer, keyed by its path in the layer.
enum Item {
    Dir,
    /// The path relative to the root, re-opened when written.
    File {
        rel: String,
        executable: bool,
    },
    Symlink {
        target: String,
    },
}

pub(crate) fn open_dir(dir: &OwnedFd, name: &str, rel: &str) -> Result<OwnedFd, BuildError> {
    openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {rel}"))(e.into()))
}

/// The names in `dir`, sorted bytewise.
pub(crate) fn names(dir: &OwnedFd, rel: &str) -> Result<Vec<String>, BuildError> {
    let listing = open_dir(dir, ".", rel)?;
    let mut out = Vec::new();
    for entry in Dir::new(listing).map_err(|e| io(format!("listing {rel}"))(e.into()))? {
        let entry = entry.map_err(|e| io(format!("listing {rel}"))(e.into()))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name =
            std::str::from_utf8(name).map_err(|_| BuildError::Plan(format!("{rel}: a file name is not UTF-8")))?;
        out.push(name.to_string());
    }
    out.sort();
    Ok(out)
}

/// Whether `target`, a symlink in directory `dir` (relative to the root), stays inside the root.
fn stays_inside(dir: &str, target: &str) -> bool {
    if target.starts_with('/') {
        return false;
    }
    let mut stack: Vec<&str> = dir.split('/').filter(|c| !c.is_empty()).collect();
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return false;
                }
            }
            c => stack.push(c),
        }
    }
    true
}

struct Walk<'a> {
    rules: &'a Rules,
    items: BTreeMap<String, Item>,
}

impl Walk<'_> {
    fn walk(&mut self, dir: &OwnedFd, rel_dir: &str) -> Result<(), BuildError> {
        for name in names(dir, rel_dir)? {
            let rel = if rel_dir.is_empty() {
                name.clone()
            } else {
                format!("{rel_dir}/{name}")
            };
            let st = statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io(format!("stat {rel}"))(e.into()))?;
            match FileType::from_raw_mode(st.st_mode) {
                FileType::Directory => {
                    if self.rules.prune(&rel) {
                        continue;
                    }
                    if self.rules.wanted(&rel) {
                        self.items.insert(rel.clone(), Item::Dir);
                    }
                    let child = open_dir(dir, &name, &rel)?;
                    self.walk(&child, &rel)?;
                }
                kind => {
                    if !self.rules.wanted(&rel) {
                        continue;
                    }
                    let item = match kind {
                        FileType::RegularFile => Item::File {
                            rel: rel.clone(),
                            executable: st.st_mode & 0o111 != 0,
                        },
                        FileType::Symlink => {
                            let target = readlinkat(dir, name.as_str(), Vec::new())
                                .map_err(|e| io(format!("readlink {rel}"))(e.into()))?;
                            let target = target.to_string_lossy().into_owned();
                            if !stays_inside(rel_dir, &target) {
                                return Err(BuildError::SymlinkEscape { path: rel, target });
                            }
                            Item::Symlink { target }
                        }
                        _ => return Err(BuildError::SpecialFile { path: rel }),
                    };
                    self.items.insert(rel, item);
                }
            }
        }
        Ok(())
    }
}

/// Opens `rel` below `root` one component at a time, never following a symlink.
pub(crate) fn open_beneath(root: &OwnedFd, rel: &str) -> Result<OwnedFd, BuildError> {
    let mut parts: Vec<&str> = rel.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    let mut dir: Option<OwnedFd> = None;
    for (i, part) in parts.iter().enumerate() {
        let here = parts[..=i].join("/");
        dir = Some(open_dir(dir.as_ref().unwrap_or(root), part, &here)?);
    }
    // Non-blocking: a FIFO swapped in for the file must not hang the build (the caller checks
    // the type with fstat).
    openat(
        dir.as_ref().unwrap_or(root),
        file,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {rel}"))(e.into()))
}

/// Reads the root's `.potterignore`, if it is a regular file.
fn potterignore(root: &OwnedFd) -> Result<String, BuildError> {
    let fd = match openat(
        root,
        ".potterignore",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(String::new()),
        Err(e) => return Err(BuildError::Plan(format!(".potterignore cannot be read: {e}"))),
    };
    let st = fstat(&fd).map_err(|e| io(".potterignore")(e.into()))?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Err(BuildError::Plan(".potterignore must be a regular file".into()));
    }
    let mut text = String::new();
    std::fs::File::from(fd)
        .take(IGNORE_CAP)
        .read_to_string(&mut text)
        .map_err(io(".potterignore"))?;
    Ok(text)
}

/// Writes the inputs layer `spec` describes to `out`.
pub fn build_inputs<W: Write>(spec: &InputsSpec, out: W) -> Result<(W, InputsSummary), BuildError> {
    let dest = spec.dest.trim_start_matches('/').trim_end_matches('/');
    if !spec.dest.starts_with('/') || dest.split('/').any(|c| c == "." || c == "..") {
        return Err(BuildError::Plan(format!(
            "the destination {:?} must be an absolute path without `.` or `..`",
            spec.dest
        )));
    }
    let root = rustix::fs::openat(
        CWD,
        spec.root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {}", spec.root.display()))(e.into()))?;
    let rules = Rules::new(spec, &potterignore(&root)?)?;
    let mut walk = Walk {
        rules: &rules,
        items: BTreeMap::new(),
    };
    walk.walk(&root, "")?;

    // Every entry under `dest`, with the directories that lead to each one.
    let place = |rel: &str| {
        if dest.is_empty() {
            rel.to_string()
        } else {
            format!("{dest}/{rel}")
        }
    };
    let mut layer: BTreeMap<String, &Item> = BTreeMap::new();
    for (rel, item) in &walk.items {
        layer.insert(place(rel), item);
    }
    let parents: Vec<String> = layer
        .keys()
        .flat_map(|p| {
            p.match_indices('/')
                .map(|(i, _)| p[..i].to_string())
                .collect::<Vec<_>>()
        })
        .chain((!dest.is_empty()).then(|| dest.to_string()))
        .collect();
    for p in parents {
        layer.entry(p).or_insert(&Item::Dir);
    }

    let (uid, gid) = spec.owner;
    let meta = |mode| Meta {
        mode,
        uid,
        gid,
        mtime: spec.epoch,
    };
    let mut tar = TarWriter::new(out);
    let mut summary = InputsSummary { files: 0, bytes: 0 };
    for (path, item) in &layer {
        match item {
            Item::Dir => tar.dir(path, &meta(0o755)),
            Item::Symlink { target } => tar.symlink(path, target, &meta(0o777)),
            Item::File { rel, executable } => {
                let fd = open_beneath(&root, rel)?;
                let st = fstat(&fd).map_err(|e| io(format!("stat {rel}"))(e.into()))?;
                if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
                    return Err(BuildError::SpecialFile { path: rel.clone() });
                }
                let size = st.st_size as u64;
                summary.files += 1;
                summary.bytes += size;
                let mode = if *executable { 0o755 } else { 0o644 };
                tar.file(path, &meta(mode), size, &mut std::fs::File::from(fd))
            }
        }
        .map_err(io(format!("writing {path}")))?;
    }
    let out = tar.finish().map_err(io("finishing the inputs layer"))?;
    Ok((out, summary))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        dir
    }

    /// (path, type, mode, uid, gid, mtime)
    fn entries(tar: &[u8]) -> Vec<(String, char, u32, u64, u64, u64)> {
        let mut a = ::tar::Archive::new(tar);
        a.entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let h = e.header();
                let kind = match h.entry_type() {
                    ::tar::EntryType::Directory => 'd',
                    ::tar::EntryType::Symlink => 'l',
                    ::tar::EntryType::Regular => 'f',
                    other => panic!("{other:?}"),
                };
                (
                    e.path().unwrap().display().to_string(),
                    kind,
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    h.gid().unwrap(),
                    h.mtime().unwrap(),
                )
            })
            .collect()
    }

    fn build(root: &Path, include: &[&str], exclude: &[&str]) -> Result<Vec<u8>, BuildError> {
        let include: Vec<String> = include.iter().map(|s| s.to_string()).collect();
        let exclude: Vec<String> = exclude.iter().map(|s| s.to_string()).collect();
        let spec = InputsSpec {
            root,
            include: &include,
            exclude: &exclude,
            dest: "/app",
            owner: (0, 0),
            epoch: 0,
        };
        build_inputs(&spec, Vec::new()).map(|(v, _)| v)
    }

    fn files(tar: &[u8]) -> Vec<String> {
        entries(tar).into_iter().filter(|e| e.1 == 'f').map(|e| e.0).collect()
    }

    #[test]
    fn the_layer_is_byte_identical_across_runs_and_mtimes() {
        let t = tree(&[("package.json", "{}"), ("src/index.js", "x"), ("src/lib/a.js", "y")]);
        let a = build(t.path(), &["**"], &[]).unwrap();
        filetime::set_file_mtime(
            t.path().join("src/index.js"),
            filetime::FileTime::from_unix_time(12345, 0),
        )
        .unwrap();
        let b = build(t.path(), &["**"], &[]).unwrap();
        assert_eq!(a, b);
        assert!(entries(&a).iter().all(|e| e.5 == 0));
        let paths: Vec<String> = entries(&a).into_iter().map(|e| e.0).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "entries are in path order");
    }

    #[test]
    fn includes_excludes_and_default_excludes() {
        let t = tree(&[
            ("package.json", "{}"),
            ("package-lock.json", "{}"),
            ("src/a.js", "x"),
            ("node_modules/x/i.js", "x"),
            ("sub/node_modules/y.js", "y"),
            (".git/HEAD", "r"),
            ("README.md", "r"),
        ]);
        assert_eq!(
            files(&build(t.path(), &["package.json", "package-lock.json"], &[]).unwrap()),
            ["app/package-lock.json", "app/package.json"]
        );
        assert_eq!(
            files(&build(t.path(), &["**"], &["*.md"]).unwrap()),
            ["app/package-lock.json", "app/package.json", "app/src/a.js"]
        );
        assert_eq!(
            files(&build(t.path(), &["src"], &[]).unwrap()),
            ["app/src/a.js"],
            "a directory includes its subtree"
        );
    }

    #[test]
    fn potterignore_negation_and_anchoring() {
        let t = tree(&[
            (".potterignore", "# comment\nlogs\n!logs/keep.log\n/tmp\n"),
            ("logs/a.log", "a"),
            ("logs/keep.log", "k"),
            ("tmp/x", "x"),
            ("src/tmp/y", "y"),
        ]);
        let f = files(&build(t.path(), &["**"], &[]).unwrap());
        assert!(
            f.contains(&"app/logs/keep.log".to_string()) && !f.contains(&"app/logs/a.log".to_string()),
            "{f:?}"
        );
        assert!(
            !f.contains(&"app/tmp/x".to_string()) && f.contains(&"app/src/tmp/y".to_string()),
            "{f:?}"
        );
    }

    #[test]
    fn modes_are_normalised_and_owner_applied() {
        use std::os::unix::fs::PermissionsExt;
        let t = tree(&[("run.sh", "#!/bin/sh"), ("data.txt", "d")]);
        std::fs::set_permissions(t.path().join("run.sh"), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(t.path().join("data.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let include = vec!["**".to_string()];
        let spec = InputsSpec {
            root: t.path(),
            include: &include,
            exclude: &[],
            dest: "/app",
            owner: (33, 33),
            epoch: 7,
        };
        let (tar, summary) = build_inputs(&spec, Vec::new()).unwrap();
        assert_eq!(summary.files, 2);
        let e = entries(&tar);
        let find = |p: &str| e.iter().find(|x| x.0 == p).unwrap().clone();
        assert_eq!(find("app/run.sh").2 & 0o7777, 0o755);
        assert_eq!(find("app/data.txt").2 & 0o7777, 0o644);
        assert_eq!(
            (find("app/data.txt").3, find("app/data.txt").4, find("app/data.txt").5),
            (33, 33, 7)
        );
        assert_eq!((find("app").1, find("app").2 & 0o7777, find("app").3), ('d', 0o755, 33));
    }

    #[test]
    fn escaping_symlinks_are_refused_and_inner_ones_kept() {
        let t = tree(&[("a/x", "x")]);
        std::os::unix::fs::symlink("../a/x", t.path().join("a/inner")).unwrap();
        let ok = build(t.path(), &["**"], &[]).unwrap();
        assert!(entries(&ok).iter().any(|e| e.0 == "app/a/inner" && e.1 == 'l'));
        for target in ["../../etc/passwd", "/etc/passwd", "../.."] {
            let t = tree(&[("x", "x")]);
            std::os::unix::fs::symlink(target, t.path().join("link")).unwrap();
            let err = build(t.path(), &["**"], &[]).unwrap_err();
            assert!(matches!(err, BuildError::SymlinkEscape { .. }), "{target}: {err}");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn a_symlinked_directory_is_not_descended_and_an_excluded_one_is_ignored() {
        let outside = tree(&[("secret", "s")]);
        let t = tree(&[("x", "x")]);
        std::os::unix::fs::symlink(outside.path(), t.path().join("dir")).unwrap();
        assert!(matches!(
            build(t.path(), &["**"], &[]),
            Err(BuildError::SymlinkEscape { .. })
        ));
        let f = files(&build(t.path(), &["x"], &[]).unwrap());
        assert_eq!(f, ["app/x"], "a symlink nobody includes is never looked at");
    }

    #[test]
    fn a_fifo_potterignore_fails_instead_of_hanging() {
        let t = tree(&[("x", "x")]);
        assert!(
            std::process::Command::new("mkfifo")
                .arg(t.path().join(".potterignore"))
                .status()
                .unwrap()
                .success()
        );
        let root = t.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(build(&root, &["**"], &[]).map(|_| ()).map_err(|e| e.exit_code()));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the build hung on a FIFO");
        assert_eq!(result, Err(2));
    }

    #[test]
    fn special_files_are_refused_when_included() {
        let t = tree(&[("x", "x")]);
        let sock = std::os::unix::net::UnixListener::bind(t.path().join("sock")).unwrap();
        let err = build(t.path(), &["**"], &[]).unwrap_err();
        assert!(matches!(err, BuildError::SpecialFile { .. }), "{err}");
        assert!(build(t.path(), &["x"], &[]).is_ok());
        drop(sock);
    }
}
