//! A bounded, read-only view of a build root (spec §6.1). Symlinks are recorded but never
//! followed, default-excluded directories are never walked, and reads are size-capped.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::jsonc::strip_jsonc;

pub const MAX_ENTRIES: usize = 200_000;
pub const MAX_DEPTH: usize = 64;
pub const READ_CAP: u64 = 4 * 1024 * 1024;
pub const LOCKFILE_CAP: u64 = 64 * 1024 * 1024;
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    "vendor",
    "target",
    ".venv",
    "__pycache__",
    ".potter",
];

const MANIFESTS: &[&str] = &[
    "composer.json",
    "package.json",
    "deno.json",
    "deno.jsonc",
    "go.mod",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "Gemfile",
];

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error("more than {MAX_ENTRIES} entries under the build root; narrow it with `root` in potter.toml")]
    TooManyEntries,
    #[error("{path} is {size} bytes, over the {cap}-byte read limit")]
    TooLarge { path: String, size: u64, cap: u64 },
    #[error("{0} is a symlink; potter doesn't follow symlinks when reading build files")]
    Symlink(String),
    #[error("{path} is not valid UTF-8")]
    NotUtf8 { path: String },
    #[error("{path}: invalid JSON: {reason}")]
    Json { path: String, reason: String },
    #[error("invalid repository path `{0}`")]
    BadPath(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> RepoError + '_ {
    move |source| RepoError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[derive(Debug)]
pub struct RepoView {
    root: PathBuf,
    files: BTreeSet<String>,
    dirs: BTreeSet<String>,
    symlinks: BTreeSet<String>,
}

impl RepoView {
    pub fn open(root: &Path) -> Result<Self, RepoError> {
        let meta = fs::metadata(root).map_err(io(root))?;
        if !meta.is_dir() {
            return Err(RepoError::NotADirectory(root.display().to_string()));
        }
        let mut view = RepoView {
            root: root.to_path_buf(),
            files: BTreeSet::new(),
            dirs: BTreeSet::new(),
            symlinks: BTreeSet::new(),
        };
        let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
        let mut count = 0usize;
        while let Some((dir, rel, depth)) = stack.pop() {
            for entry in fs::read_dir(&dir).map_err(io(&dir))? {
                let entry = entry.map_err(io(&dir))?;
                count += 1;
                if count > MAX_ENTRIES {
                    return Err(RepoError::TooManyEntries);
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let child = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{rel}/{name}")
                };
                let path = entry.path();
                let ft = entry.file_type().map_err(io(&path))?;
                if ft.is_symlink() {
                    view.symlinks.insert(child);
                } else if ft.is_dir() {
                    if DEFAULT_EXCLUDES.contains(&name.as_str()) {
                        continue;
                    }
                    view.dirs.insert(child.clone());
                    if depth + 1 < MAX_DEPTH {
                        stack.push((path, child, depth + 1));
                    }
                } else if ft.is_file() {
                    view.files.insert(child);
                }
            }
        }
        Ok(view)
    }

    fn check(rel: &str) -> Result<(), RepoError> {
        let bad = rel.is_empty()
            || rel.starts_with('/')
            || rel.contains('\0')
            || rel.split('/').any(|c| c.is_empty() || c == "." || c == "..");
        if bad {
            Err(RepoError::BadPath(rel.to_string()))
        } else {
            Ok(())
        }
    }

    pub fn is_file(&self, rel: &str) -> bool {
        self.files.contains(rel)
    }

    pub fn is_dir(&self, rel: &str) -> bool {
        self.dirs.contains(rel)
    }

    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(String::as_str)
    }

    pub fn read(&self, rel: &str) -> Result<Option<Vec<u8>>, RepoError> {
        self.read_capped(rel, READ_CAP)
    }

    pub fn read_capped(&self, rel: &str, cap: u64) -> Result<Option<Vec<u8>>, RepoError> {
        Self::check(rel)?;
        if self.symlinks.contains(rel) {
            return Err(RepoError::Symlink(rel.to_string()));
        }
        if !self.files.contains(rel) {
            return Ok(None);
        }
        let path = self.root.join(rel);
        let file = open_no_follow(&path).map_err(io(&path))?;
        let size = file.metadata().map_err(io(&path))?.len();
        if size > cap {
            return Err(RepoError::TooLarge {
                path: rel.to_string(),
                size,
                cap,
            });
        }
        let mut buf = Vec::new();
        file.take(cap + 1).read_to_end(&mut buf).map_err(io(&path))?;
        if buf.len() as u64 > cap {
            return Err(RepoError::TooLarge {
                path: rel.to_string(),
                size: buf.len() as u64,
                cap,
            });
        }
        Ok(Some(buf))
    }

    pub fn read_string(&self, rel: &str, cap: u64) -> Result<Option<String>, RepoError> {
        let Some(bytes) = self.read_capped(rel, cap)? else {
            return Ok(None);
        };
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").map(<[u8]>::to_vec).unwrap_or(bytes);
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| RepoError::NotUtf8 { path: rel.to_string() })
    }

    pub fn read_json(&self, rel: &str) -> Result<Option<Value>, RepoError> {
        self.read_json_capped(rel, READ_CAP)
    }

    pub fn read_json_capped(&self, rel: &str, cap: u64) -> Result<Option<Value>, RepoError> {
        let Some(text) = self.read_string(rel, cap)? else {
            return Ok(None);
        };
        serde_json::from_str(&text).map(Some).map_err(|e| RepoError::Json {
            path: rel.to_string(),
            reason: e.to_string(),
        })
    }

    pub fn read_jsonc(&self, rel: &str) -> Result<Option<Value>, RepoError> {
        let Some(text) = self.read_string(rel, READ_CAP)? else {
            return Ok(None);
        };
        serde_json::from_str(&strip_jsonc(&text))
            .map(Some)
            .map_err(|e| RepoError::Json {
                path: rel.to_string(),
                reason: e.to_string(),
            })
    }

    /// Depth-1 directories containing a known manifest; used to hint at monorepo targets.
    pub fn manifest_dirs(&self) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for f in &self.files {
            if let Some((dir, name)) = f.split_once('/')
                && !name.contains('/')
                && MANIFESTS.contains(&name)
            {
                out.insert(dir.to_string());
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("composer.json"), "{\"require\":{}}").unwrap();
        fs::create_dir_all(d.path().join("public")).unwrap();
        fs::write(d.path().join("public/index.php"), "<?php").unwrap();
        fs::create_dir_all(d.path().join("node_modules/x")).unwrap();
        fs::write(d.path().join("node_modules/x/package.json"), "{}").unwrap();
        fs::create_dir_all(d.path().join("frontend")).unwrap();
        fs::write(d.path().join("frontend/package.json"), "{}").unwrap();
        fs::create_dir_all(d.path().join("backend")).unwrap();
        fs::write(d.path().join("backend/go.mod"), "module x").unwrap();
        d
    }

    #[test]
    fn walks_and_excludes() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        assert!(r.is_file("composer.json"));
        assert!(r.is_file("public/index.php"));
        assert!(r.is_dir("public"));
        assert!(!r.is_file("node_modules/x/package.json"));
        assert!(!r.is_dir("node_modules"));
    }

    #[test]
    fn manifest_dirs_for_monorepo_hint() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        assert_eq!(r.manifest_dirs(), vec!["backend".to_string(), "frontend".to_string()]);
    }

    #[test]
    fn reads_json_and_missing_files() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        assert!(r.read_json("composer.json").unwrap().unwrap()["require"].is_object());
        assert!(r.read_json("package.json").unwrap().is_none());
    }

    #[test]
    fn bom_is_accepted() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), "\u{feff}{\"name\":\"x\"}").unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert_eq!(r.read_json("package.json").unwrap().unwrap()["name"], "x");
    }

    #[test]
    fn invalid_json_names_file() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), "{nope").unwrap();
        let r = RepoView::open(d.path()).unwrap();
        let e = r.read_json("package.json").unwrap_err();
        assert!(
            matches!(&e, RepoError::Json { path, .. } if path == "package.json"),
            "{e}"
        );
    }

    #[test]
    fn non_utf8_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".nvmrc"), [0xff, 0xfe, b'2']).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert!(matches!(
            r.read_string(".nvmrc", READ_CAP),
            Err(RepoError::NotUtf8 { .. })
        ));
    }

    #[test]
    fn oversized_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), vec![b' '; 2048]).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        let e = r.read_capped("package.json", 1024).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("package.json") && msg.contains("1024"), "{msg}");
    }

    #[test]
    fn lockfile_cap_allows_large_lockfiles() {
        let d = tempfile::tempdir().unwrap();
        let big = format!("{{\"packages\":[],\"pad\":\"{}\"}}", "x".repeat(5 * 1024 * 1024));
        fs::write(d.path().join("composer.lock"), big).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert!(r.read_json("composer.lock").is_err());
        assert!(r.read_json_capped("composer.lock", LOCKFILE_CAP).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.json"), "{}").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.json"), d.path().join("composer.json")).unwrap();
        let r = RepoView::open(d.path()).unwrap();
        assert!(!r.is_file("composer.json"));
        assert!(matches!(r.read("composer.json"), Err(RepoError::Symlink(p)) if p == "composer.json"));
    }

    #[test]
    fn bad_paths_are_rejected() {
        let d = tree();
        let r = RepoView::open(d.path()).unwrap();
        for p in ["../x", "/etc/passwd", "a//b", "./composer.json", ""] {
            assert!(matches!(r.read(p), Err(RepoError::BadPath(_))), "{p:?}");
        }
    }

    #[test]
    fn root_must_be_a_directory() {
        let d = tree();
        assert!(matches!(
            RepoView::open(&d.path().join("composer.json")),
            Err(RepoError::NotADirectory(_))
        ));
        assert!(matches!(
            RepoView::open(&d.path().join("missing")),
            Err(RepoError::Io { .. })
        ));
    }
}
