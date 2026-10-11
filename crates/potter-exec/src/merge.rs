//! The merged view of a chain's layers (spec §9.6): export, and reading one file.
//!
//! Pass 1 reads only metadata, bottom layer first: whiteouts delete, opaque directories hide
//! their lower-layer contents, a later entry replaces an earlier one. Pass 2 copies the data of
//! the files that are needed, layer by layer, into a spill file, and the result is written in
//! path order. Layer contents are untrusted: a path with `..` or a leading `/` is a violation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use potter_plan::Export;

use crate::error::{BuildError, io};
use crate::tarball::{Meta, TarWriter};

/// Opens one layer's uncompressed tar stream; it may be called more than once.
pub type LayerOpen<'a> = Box<dyn Fn() -> Result<Box<dyn Read + Send>, BuildError> + 'a>;

pub struct ExportSpec<'a> {
    pub exports: &'a [Export],
    pub epoch: u64,
    /// Where file data is staged while the layer is written.
    pub spill_dir: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportSummary {
    pub entries: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Dir,
    /// Data is the `index`-th entry of layer `layer` (a regular file entry).
    File {
        layer: usize,
        index: usize,
        size: u64,
    },
    Symlink(String),
    Fifo,
}

#[derive(Debug, Clone)]
struct Node {
    kind: Kind,
    mode: u32,
    uid: u64,
    gid: u64,
    /// The layer the entry came from (opaque directories hide only lower layers).
    from: usize,
    xattrs: Vec<(String, Vec<u8>)>,
}

/// A layer path without `./`, a leading `/` refused, `..` refused; `None` for the root itself.
pub(crate) fn normalise(raw: &[u8]) -> Result<Option<String>, BuildError> {
    let s = std::str::from_utf8(raw).map_err(|_| BuildError::Hostile("a layer path is not UTF-8".into()))?;
    if s.starts_with('/') {
        return Err(BuildError::Hostile(format!("the layer path {s:?} is absolute")));
    }
    let mut parts = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => return Err(BuildError::Hostile(format!("the layer path {s:?} leaves the root"))),
            c => parts.push(c),
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// Removes `path` and everything under it (only entries from layers below `below`, if given).
fn remove_tree(map: &mut BTreeMap<String, Node>, path: &str, below: Option<usize>) {
    let prefix = format!("{path}/");
    let doomed: Vec<String> = map
        .range(prefix.clone()..)
        .take_while(|(k, _)| k.starts_with(&prefix))
        .filter(|(_, n)| below.is_none_or(|l| n.from < l))
        .map(|(k, _)| k.clone())
        .collect();
    for k in doomed {
        map.remove(&k);
    }
    if below.is_none() {
        map.remove(path);
    }
}

fn hostile(e: std::io::Error) -> BuildError {
    BuildError::Hostile(format!("a layer is not a valid tar stream: {e}"))
}

/// Pass 1: the merged metadata of `layers`, bottom first.
fn merged(layers: &[LayerOpen]) -> Result<BTreeMap<String, Node>, BuildError> {
    let mut map: BTreeMap<String, Node> = BTreeMap::new();
    for (li, open) in layers.iter().enumerate() {
        let mut archive = tar::Archive::new(open()?);
        let mut opaque = Vec::new();
        for (index, entry) in archive.entries().map_err(hostile)?.enumerate() {
            let mut entry = entry.map_err(hostile)?;
            let Some(path) = normalise(&entry.path_bytes())? else {
                continue;
            };
            let (dir, name) = match path.rsplit_once('/') {
                Some((d, n)) => (Some(d), n),
                None => (None, path.as_str()),
            };
            if name == ".wh..wh..opq" {
                opaque.push(dir.unwrap_or_default().to_string());
                continue;
            }
            if let Some(hidden) = name.strip_prefix(".wh.") {
                let target = dir.map_or_else(|| hidden.to_string(), |d| format!("{d}/{hidden}"));
                remove_tree(&mut map, &target, None);
                continue;
            }
            let h = entry.header();
            let (mode, uid, gid) = (
                h.mode().map_err(hostile)?,
                h.uid().map_err(hostile)?,
                h.gid().map_err(hostile)?,
            );
            let mut xattrs = Vec::new();
            if let Some(pax) = entry.pax_extensions().map_err(hostile)? {
                for ext in pax {
                    let ext = ext.map_err(hostile)?;
                    if let Some(name) = ext.key().ok().and_then(|k| k.strip_prefix("SCHILY.xattr.")) {
                        xattrs.push((name.to_string(), ext.value_bytes().to_vec()));
                    }
                }
            }
            // A hardlink shares its target's inode: its metadata, not the link header's.
            let mut shared: Option<Node> = None;
            let kind = match entry.header().entry_type() {
                tar::EntryType::Directory => Kind::Dir,
                tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => Kind::File {
                    layer: li,
                    index,
                    size: entry.size(),
                },
                tar::EntryType::Symlink => {
                    let target = entry.link_name_bytes().unwrap_or_default();
                    Kind::Symlink(String::from_utf8_lossy(&target).into_owned())
                }
                tar::EntryType::Link => {
                    let target = entry.link_name_bytes().unwrap_or_default();
                    let Some(target) = normalise(&target)? else { continue };
                    // The inode as the target holds it now: a later replacement of the target
                    // does not change what this link holds.
                    match map.get(&target) {
                        Some(n) if matches!(n.kind, Kind::File { .. }) => {
                            shared = Some(n.clone());
                            n.kind.clone()
                        }
                        _ => continue,
                    }
                }
                tar::EntryType::Fifo => Kind::Fifo,
                // Devices cannot be made in a user namespace and are never exported.
                _ => continue,
            };
            if kind != Kind::Dir {
                remove_tree(&mut map, &path, None);
            }
            let node = match shared {
                Some(target) => Node { from: li, ..target },
                None => Node {
                    kind,
                    mode,
                    uid,
                    gid,
                    from: li,
                    xattrs,
                },
            };
            map.insert(path, node);
        }
        for d in opaque {
            remove_tree(&mut map, &d, Some(li));
        }
    }
    Ok(map)
}

fn glob(pattern: &str) -> Result<GlobMatcher, BuildError> {
    GlobBuilder::new(pattern.trim_matches('/'))
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| BuildError::Plan(format!("the export exclude {pattern:?} is invalid: {e}")))
}

/// Pass 2: copies the data of `needed` (layer, index) entries into `spill`; returns their offsets.
fn spill(
    layers: &[LayerOpen],
    needed: &BTreeSet<(usize, usize)>,
    spill: &mut std::fs::File,
) -> Result<BTreeMap<(usize, usize), u64>, BuildError> {
    let mut offsets = BTreeMap::new();
    let mut at = 0u64;
    let by_layer: BTreeSet<usize> = needed.iter().map(|(l, _)| *l).collect();
    for li in by_layer {
        let mut archive = tar::Archive::new(layers[li]()?);
        for (index, entry) in archive.entries().map_err(hostile)?.enumerate() {
            if !needed.contains(&(li, index)) {
                continue;
            }
            let mut entry = entry.map_err(hostile)?;
            let n = std::io::copy(&mut entry, spill).map_err(io("staging file data"))?;
            offsets.insert((li, index), at);
            at += n;
        }
    }
    Ok(offsets)
}

/// One flattened layer of the `exports` of `layers` (bottom first).
pub fn export_layer<W: Write>(
    layers: &[LayerOpen],
    spec: &ExportSpec,
    out: W,
) -> Result<(W, ExportSummary), BuildError> {
    let map = merged(layers)?;
    export_from(layers, &map, spec, None, None, out)
}

/// What a `copy_from` step copies (spec §9.6 step 2).
pub struct CopySpec<'a> {
    pub paths: &'a [String],
    /// A trailing `/` makes it a directory for single files.
    pub dest: &'a str,
    pub owner: Option<(u64, u64)>,
    pub mode: Option<u32>,
    pub epoch: u64,
    pub spill_dir: &'a Path,
}

/// One layer of `paths` from the merged view of `layers` at `dest`, as Docker's `COPY --from`
/// places them: a directory's contents go into `dest`; a file becomes `dest`, or lands inside it
/// when `dest` ends in `/` or several paths are copied.
pub fn copy_layer<W: Write>(layers: &[LayerOpen], spec: &CopySpec, out: W) -> Result<(W, ExportSummary), BuildError> {
    let map = merged(layers)?;
    let into = spec.dest.ends_with('/') || spec.paths.len() > 1;
    let dest = spec.dest.trim_end_matches('/');
    let mut exports = Vec::new();
    for p in spec.paths {
        let rel = p.trim_matches('/');
        let node = map
            .get(rel)
            .ok_or_else(|| BuildError::Plan(format!("copy_from: {p} does not exist in the source")))?;
        let to = if node.kind == Kind::Dir || !into {
            dest.to_string()
        } else {
            let base = rel.rsplit('/').next().unwrap_or(rel);
            format!("{dest}/{base}")
        };
        exports.push(Export {
            path: p.clone(),
            dest: if to.is_empty() { "/".into() } else { to },
            exclude: vec![],
        });
    }
    let export = ExportSpec {
        exports: &exports,
        epoch: spec.epoch,
        spill_dir: spec.spill_dir,
    };
    export_from(layers, &map, &export, spec.owner, spec.mode, out)
}

/// The selection of `spec` from `map` (the merged view of `layers`), with optional owner and
/// mode overrides, as one flattened layer.
fn export_from<W: Write>(
    layers: &[LayerOpen],
    map: &BTreeMap<String, Node>,
    spec: &ExportSpec,
    owner: Option<(u64, u64)>,
    mode: Option<u32>,
    out: W,
) -> Result<(W, ExportSummary), BuildError> {
    // The output: path -> (source path, node).
    let mut chosen: BTreeMap<String, (String, Node)> = BTreeMap::new();
    for e in spec.exports {
        let src = e.path.trim_matches('/').to_string();
        let dst = e.dest.trim_matches('/').to_string();
        if !src.is_empty() && !map.contains_key(&src) {
            return Err(BuildError::Plan(format!(
                "the export path {} does not exist in the build",
                e.path
            )));
        }
        let excludes = e.exclude.iter().map(|p| glob(p)).collect::<Result<Vec<_>, _>>()?;
        let under = |k: &str| -> Option<String> {
            if src.is_empty() {
                return Some(k.to_string());
            }
            if k == src {
                return Some(String::new());
            }
            k.strip_prefix(&src)?.strip_prefix('/').map(str::to_string)
        };
        for (k, node) in map {
            let Some(rel) = under(k) else { continue };
            let excluded = !rel.is_empty()
                && excludes.iter().any(|m| {
                    rel.match_indices('/')
                        .map(|(i, _)| &rel[..i])
                        .chain(std::iter::once(rel.as_str()))
                        .any(|p| m.is_match(p))
                });
            if excluded {
                continue;
            }
            let out_path = match (dst.is_empty(), rel.is_empty()) {
                (_, true) => dst.clone(),
                (true, false) => rel.clone(),
                (false, false) => format!("{dst}/{rel}"),
            };
            if out_path.is_empty() {
                continue;
            }
            match chosen.get(&out_path) {
                // Two directories at one place merge, as COPY merges them.
                Some((_, prev)) if prev.kind == Kind::Dir && node.kind == Kind::Dir => continue,
                Some(_) => return Err(BuildError::Plan(format!("two exports write /{out_path}"))),
                None => {
                    chosen.insert(out_path.clone(), (k.clone(), node.clone()));
                }
            }
        }
    }
    // A path below an exported file or symlink cannot exist.
    for path in chosen.keys() {
        for (i, _) in path.match_indices('/') {
            if let Some((_, n)) = chosen.get(&path[..i])
                && n.kind != Kind::Dir
            {
                return Err(BuildError::Plan(format!(
                    "the exports put /{path} below /{}, which is not a directory",
                    &path[..i]
                )));
            }
        }
    }
    // Missing parents of every exported path, as root directories.
    let parents: Vec<String> = chosen
        .keys()
        .flat_map(|p| {
            p.match_indices('/')
                .map(|(i, _)| p[..i].to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    for p in parents {
        chosen.entry(p).or_insert_with(|| {
            (
                String::new(),
                Node {
                    kind: Kind::Dir,
                    mode: 0o755,
                    uid: 0,
                    gid: 0,
                    from: 0,
                    xattrs: Vec::new(),
                },
            )
        });
    }
    // Exported paths sharing one inode's data: the first in path order holds the data, the
    // others are hardlinks to it, written after it.
    let mut holder: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (path, (_, n)) in &chosen {
        if let Kind::File { layer, index, .. } = n.kind {
            holder.entry((layer, index)).or_insert_with(|| path.clone());
        }
    }
    let needed: BTreeSet<(usize, usize)> = holder.keys().copied().collect();
    let mut staged = tempfile::tempfile_in(spec.spill_dir).map_err(io("creating the export spill file"))?;
    let offsets = spill(layers, &needed, &mut staged)?;

    let mut tar = TarWriter::new(out);
    let mut summary = ExportSummary { entries: 0, bytes: 0 };
    for (path, (_, n)) in &chosen {
        let symlink = matches!(n.kind, Kind::Symlink(_));
        let (uid, gid) = owner.unwrap_or((n.uid, n.gid));
        let meta = Meta {
            mode: mode.filter(|_| !symlink).unwrap_or(n.mode & 0o7777),
            uid,
            gid,
            mtime: spec.epoch,
        };
        summary.entries += 1;
        match &n.kind {
            Kind::Dir => tar.dir(path, &meta),
            Kind::Symlink(target) => tar.symlink(path, target, &meta),
            Kind::Fifo => tar.fifo(path, &meta),
            Kind::File { layer, index, size } => {
                let first = &holder[&(*layer, *index)];
                if first != path {
                    let (_, h) = &chosen[first];
                    let (uid, gid) = owner.unwrap_or((h.uid, h.gid));
                    let meta = Meta {
                        mode: mode.unwrap_or(h.mode & 0o7777),
                        uid,
                        gid,
                        mtime: spec.epoch,
                    };
                    tar.hardlink(path, first, &meta)
                } else {
                    let at = offsets[&(*layer, *index)];
                    staged.seek(SeekFrom::Start(at)).map_err(io("reading staged data"))?;
                    summary.bytes += size;
                    tar.pax_xattrs(&n.xattrs)
                        .and_then(|()| tar.file(path, &meta, *size, &mut (&staged).take(*size)))
                }
            }
        }
        .map_err(io(format!("writing /{path}")))?;
    }
    let out = tar.finish().map_err(io("finishing the export layer"))?;
    Ok((out, summary))
}

/// The content of `path` in the merged view of `layers`, when it is a regular file of at most
/// `cap` bytes.
pub fn read_file(layers: &[LayerOpen], path: &str, cap: u64) -> Result<Option<Vec<u8>>, BuildError> {
    let map = merged(layers)?;
    let Some(Node {
        kind: Kind::File { layer, index, size },
        ..
    }) = map.get(path.trim_matches('/'))
    else {
        return Ok(None);
    };
    if *size > cap {
        return Ok(None);
    }
    let mut archive = tar::Archive::new(layers[*layer]()?);
    for (i, entry) in archive.entries().map_err(hostile)?.enumerate() {
        if i == *index {
            let mut data = Vec::new();
            entry
                .map_err(hostile)?
                .take(cap)
                .read_to_end(&mut data)
                .map_err(hostile)?;
            return Ok(Some(data));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use kiln_erofs::testtar::{Opts, TarBuilder};
    use potter_plan::Export;

    use super::*;

    fn layer(bytes: Vec<u8>) -> LayerOpen<'static> {
        Box::new(move || Ok(Box::new(std::io::Cursor::new(bytes.clone())) as Box<dyn Read + Send>))
    }

    /// (path, type, mode, uid, data)
    type Out = Vec<(String, char, u32, u64, Vec<u8>)>;

    fn run(layers: &[LayerOpen], exports: &[(&str, &str, &[&str])]) -> Result<Out, BuildError> {
        let exports: Vec<Export> = exports
            .iter()
            .map(|(p, d, x)| Export {
                path: p.to_string(),
                dest: d.to_string(),
                exclude: x.iter().map(|s| s.to_string()).collect(),
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let spec = ExportSpec {
            exports: &exports,
            epoch: 0,
            spill_dir: dir.path(),
        };
        let (bytes, _) = export_layer(layers, &spec, Vec::new())?;
        let mut a = ::tar::Archive::new(bytes.as_slice());
        Ok(a.entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let h = e.header().clone();
                let kind = match h.entry_type() {
                    ::tar::EntryType::Directory => 'd',
                    ::tar::EntryType::Symlink => 'l',
                    ::tar::EntryType::Link => 'h',
                    ::tar::EntryType::Regular => 'f',
                    other => panic!("{other:?}"),
                };
                assert_eq!(h.mtime().unwrap(), 0);
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut e, &mut data).unwrap();
                (
                    e.path().unwrap().display().to_string(),
                    kind,
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    data,
                )
            })
            .collect())
    }

    fn export(layers: &[LayerOpen], exports: &[(&str, &str, &[&str])]) -> Out {
        run(layers, exports).unwrap()
    }

    fn paths(out: &Out) -> Vec<&str> {
        out.iter().map(|e| e.0.as_str()).collect()
    }

    #[test]
    fn whiteouts_and_opaque_dirs_hide_lower_entries() {
        let o = Opts::default();
        let mut l0 = TarBuilder::new();
        l0.dir("app", &o)
            .file("app/a", b"a", &o)
            .file("app/b", b"b", &o)
            .dir("app/d", &o)
            .file("app/d/old", b"o", &o);
        let mut l1 = TarBuilder::new();
        l1.whiteout("app/a").file("app/d/new", b"n", &o).opaque("app/d");
        let out = export(&[layer(l0.finish()), layer(l1.finish())], &[("/app", "/app", &[])]);
        assert_eq!(paths(&out), ["app", "app/b", "app/d", "app/d/new"]);
    }

    #[test]
    fn export_remaps_excludes_and_normalises_mtime_only() {
        let o = Opts::default().uid(1000).gid(1000).mode(0o750).mtime(99);
        let mut l0 = TarBuilder::new();
        l0.dir("out", &o)
            .file("out/app", b"bin", &o)
            .dir("out/cache", &o)
            .file("out/cache/x", b"x", &o);
        let out = export(&[layer(l0.finish())], &[("/out", "/srv", &["cache"])]);
        assert_eq!(paths(&out), ["srv", "srv/app"]);
        let app = out.iter().find(|e| e.0 == "srv/app").unwrap();
        assert_eq!(
            (app.2 & 0o7777, app.3, app.4.as_slice()),
            (0o750, 1000, b"bin".as_slice())
        );
    }

    #[test]
    fn a_single_file_export_gets_its_parents() {
        let o = Opts::default();
        let mut l0 = TarBuilder::new();
        l0.dir("out", &o).file("out/app", b"bin", &o);
        let out = export(&[layer(l0.finish())], &[("/out/app", "/usr/local/bin/app", &[])]);
        assert_eq!(paths(&out), ["usr", "usr/local", "usr/local/bin", "usr/local/bin/app"]);
        assert_eq!(out[3].4, b"bin");
    }

    #[test]
    fn hardlinks_survive_inside_the_export_or_become_files() {
        let o = Opts::default();
        let mut l0 = TarBuilder::new();
        l0.dir("a", &o)
            .file("a/x", b"data", &o)
            .hardlink("a/y", "a/x")
            .dir("b", &o)
            .hardlink("b/z", "a/x");
        let bytes = l0.finish();
        let out = export(&[layer(bytes.clone())], &[("/b", "/b", &[])]);
        let z = out.iter().find(|e| e.0 == "b/z").unwrap();
        assert_eq!((z.1, z.4.as_slice()), ('f', b"data".as_slice()));
        let out = export(&[layer(bytes)], &[("/a", "/a", &[])]);
        assert_eq!(out.iter().find(|e| e.0 == "a/y").unwrap().1, 'h');
    }

    #[test]
    fn a_later_file_replaces_a_directory_and_its_subtree() {
        let o = Opts::default();
        let mut l0 = TarBuilder::new();
        l0.dir("app", &o).dir("app/x", &o).file("app/x/inner", b"i", &o);
        let mut l1 = TarBuilder::new();
        l1.file("app/x", b"file", &o);
        let out = export(&[layer(l0.finish()), layer(l1.finish())], &[("/app", "/app", &[])]);
        assert_eq!(paths(&out), ["app", "app/x"]);
    }

    /// (path, type, mode, uid, link target) of every entry, in stream order.
    fn headers(layers: &[LayerOpen], exports: &[(&str, &str, &[&str])]) -> Vec<(String, char, u32, u64, String)> {
        let exports: Vec<Export> = exports
            .iter()
            .map(|(p, d, x)| Export {
                path: p.to_string(),
                dest: d.to_string(),
                exclude: x.iter().map(|s| s.to_string()).collect(),
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let spec = ExportSpec {
            exports: &exports,
            epoch: 0,
            spill_dir: dir.path(),
        };
        let (bytes, _) = export_layer(layers, &spec, Vec::new()).unwrap();
        let mut a = ::tar::Archive::new(bytes.as_slice());
        a.entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let h = e.header();
                let kind = if h.entry_type() == ::tar::EntryType::Link {
                    'h'
                } else {
                    'x'
                };
                let link = e
                    .link_name()
                    .unwrap()
                    .map(|l| l.display().to_string())
                    .unwrap_or_default();
                (
                    e.path().unwrap().display().to_string(),
                    kind,
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    link,
                )
            })
            .collect()
    }

    #[test]
    fn a_hardlink_never_precedes_its_target_and_carries_its_metadata() {
        let exe = Opts::default().mode(0o755).uid(1000);
        let mut l0 = TarBuilder::new();
        // `perl` sorts before `perl5.36.0`, which holds the data in the source layer.
        l0.dir("usr", &exe)
            .file("usr/perl5.36.0", b"elf", &exe)
            .hardlink("usr/perl", "usr/perl5.36.0");
        let h = headers(&[layer(l0.finish())], &[("/usr", "/usr", &[])]);
        let mut seen = Vec::new();
        for (path, kind, mode, uid, link) in &h {
            if *kind == 'h' {
                assert!(
                    seen.contains(link),
                    "{path} links to {link} before it is written: {h:?}"
                );
            }
            if path != "usr" {
                assert_eq!((*mode & 0o7777, *uid), (0o755, 1000), "{path}: {h:?}");
            }
            seen.push(path.clone());
        }
        assert_eq!(h.iter().filter(|e| e.1 == 'h').count(), 1, "{h:?}");
    }

    #[test]
    fn a_hardlink_whose_target_was_replaced_keeps_the_old_data() {
        let o = Opts::default();
        let mut l0 = TarBuilder::new();
        l0.dir("d", &o).file("d/a", b"OLD", &o).hardlink("d/b", "d/a");
        let mut l1 = TarBuilder::new();
        l1.file("d/a", b"NEW", &o);
        let out = export(&[layer(l0.finish()), layer(l1.finish())], &[("/d", "/d", &[])]);
        let get = |p: &str| out.iter().find(|e| e.0 == p).unwrap().clone();
        assert_eq!((get("d/a").1, get("d/a").4), ('f', b"NEW".to_vec()));
        assert_eq!((get("d/b").1, get("d/b").4), ('f', b"OLD".to_vec()));
    }

    #[test]
    fn exports_nesting_under_a_file_are_plan_errors() {
        let o = Opts::default();
        let mut t = TarBuilder::new();
        t.file("f", b"x", &o).dir("d", &o).file("d/y", b"y", &o);
        let err = run(&[layer(t.finish())], &[("/f", "/x", &[]), ("/d/y", "/x/y", &[])]).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
    }

    fn copy(
        layers: &[LayerOpen],
        paths: &[&str],
        dest: &str,
        owner: Option<(u64, u64)>,
        mode: Option<u32>,
    ) -> Result<Out, BuildError> {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let spec = CopySpec {
            paths: &paths,
            dest,
            owner,
            mode,
            epoch: 0,
            spill_dir: dir.path(),
        };
        let (bytes, _) = copy_layer(layers, &spec, Vec::new())?;
        let mut a = ::tar::Archive::new(bytes.as_slice());
        Ok(a.entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let h = e.header().clone();
                let kind = if h.entry_type() == ::tar::EntryType::Directory {
                    'd'
                } else {
                    'f'
                };
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut e, &mut data).unwrap();
                (
                    e.path().unwrap().display().to_string(),
                    kind,
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    data,
                )
            })
            .collect())
    }

    fn built() -> Vec<LayerOpen<'static>> {
        let o = Opts::default().mode(0o755).uid(1000).gid(1000);
        let mut t = TarBuilder::new();
        t.dir("app", &o)
            .dir("app/dist", &o)
            .file("app/dist/index.html", b"<h1>", &o)
            .dir("app/dist/js", &o)
            .file("app/dist/js/a.js", b"a", &o)
            .file("app/bin", b"elf", &o);
        vec![layer(t.finish())]
    }

    #[test]
    fn copying_a_directory_copies_its_contents_into_dest() {
        let out = copy(&built(), &["/app/dist"], "/srv/www", None, None).unwrap();
        assert_eq!(
            paths(&out),
            ["srv", "srv/www", "srv/www/index.html", "srv/www/js", "srv/www/js/a.js"]
        );
    }

    #[test]
    fn copying_a_file_lands_at_dest_or_inside_a_dest_directory() {
        let to_name = copy(&built(), &["/app/bin"], "/usr/local/bin/app", None, None).unwrap();
        assert!(paths(&to_name).contains(&"usr/local/bin/app"));
        let into_dir = copy(&built(), &["/app/bin"], "/usr/local/bin/", None, None).unwrap();
        assert!(paths(&into_dir).contains(&"usr/local/bin/bin"));
        let several = copy(&built(), &["/app/bin", "/app/dist"], "/opt", None, None).unwrap();
        assert!(
            paths(&several).contains(&"opt/bin") && paths(&several).contains(&"opt/index.html"),
            "{:?}",
            paths(&several)
        );
    }

    #[test]
    fn owner_and_mode_override_every_copied_entry() {
        let out = copy(&built(), &["/app/dist"], "/srv/www", Some((33, 33)), Some(0o640)).unwrap();
        let f = out.iter().find(|e| e.0 == "srv/www/index.html").unwrap();
        assert_eq!((f.2 & 0o7777, f.3), (0o640, 33));
        let kept = copy(&built(), &["/app/dist"], "/srv/www", None, None).unwrap();
        let f = kept.iter().find(|e| e.0 == "srv/www/index.html").unwrap();
        assert_eq!((f.2 & 0o7777, f.3), (0o755, 1000));
    }

    #[test]
    fn copying_a_missing_path_is_a_plan_error() {
        assert_eq!(copy(&built(), &["/nope"], "/x", None, None).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn hostile_paths_are_refused() {
        let o = Opts::default();
        for bad in ["../x", "/etc/x", "a/../../x"] {
            let mut t = TarBuilder::new();
            t.file(bad, b"x", &o);
            let err = run(&[layer(t.finish())], &[("/a", "/a", &[])]).unwrap_err();
            assert!(matches!(err, BuildError::Hostile(_)), "{bad}: {err}");
        }
    }

    #[test]
    fn a_missing_export_path_and_colliding_exports_are_plan_errors() {
        let o = Opts::default();
        let mut t = TarBuilder::new();
        t.dir("a", &o).file("a/x", b"x", &o).dir("b", &o).file("b/x", b"y", &o);
        let bytes = t.finish();
        assert_eq!(
            run(&[layer(bytes.clone())], &[("/missing", "/m", &[])])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            run(&[layer(bytes)], &[("/a", "/c", &[]), ("/b", "/c", &[])])
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[test]
    fn read_file_sees_the_merged_view() {
        let o = Opts::default();
        let mut l0 = TarBuilder::new();
        l0.dir("etc", &o)
            .file("etc/passwd", b"root:x:0:0::/root:/bin/sh\n", &o)
            .file("etc/gone", b"g", &o);
        let mut l1 = TarBuilder::new();
        l1.file("etc/passwd", b"app:x:1000:1000::/home/app:/bin/sh\n", &o)
            .whiteout("etc/gone");
        let layers = [layer(l0.finish()), layer(l1.finish())];
        assert_eq!(
            read_file(&layers, "/etc/passwd", 1 << 20).unwrap().unwrap(),
            b"app:x:1000:1000::/home/app:/bin/sh\n"
        );
        assert_eq!(read_file(&layers, "/etc/gone", 1 << 20).unwrap(), None);
        assert_eq!(read_file(&layers, "/etc/passwd", 4).unwrap(), None, "over the cap");
    }
}
