//! The host side of sealing in a VM (spec §9.3, P4). The guest appends each step's diff to the
//! output disk and reports where; the host reads that range itself, checks it is a fresh,
//! aligned range inside the disk, hashes and counts it, and ingests it through the bounded path.
//! The report's digest, length and entry count are compared, never trusted: a mismatch is a
//! protocol violation.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use kiln_store::{Digest, Store};
use sha2::{Digest as _, Sha256};

use crate::error::{BuildError, io};

fn violation(id: u32, what: impl std::fmt::Display) -> BuildError {
    BuildError::Hostile(format!("the guest's report of step {id}: {what}"))
}

/// Diffs start on this boundary.
pub const ALIGN: u64 = 4096;

/// What the guest reports after sealing a step: potter's view of kiln-proto's `StepSealed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepSealed {
    pub id: u32,
    pub offset: u64,
    pub len: u64,
    /// Lowercase hex SHA-256 of the range.
    pub sha256: String,
    pub entries: u64,
}

/// The VM's output disk, read only by the host, front to back.
pub struct OutputDisk {
    path: PathBuf,
    size: u64,
    /// The end of the last diff taken: the next must start at or after it.
    cursor: u64,
}

impl OutputDisk {
    /// The disk at `path`, `size` bytes as the host created it.
    pub fn new(path: &Path, size: u64) -> Self {
        Self {
            path: path.to_path_buf(),
            size,
            cursor: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Takes the diff `report` names: checked, ingested and stored. Returns its digest and size.
    pub fn take(
        &mut self,
        report: &StepSealed,
        store: &Store,
        spill_dir: &Path,
        max_layer: u64,
    ) -> Result<(Digest, u64), BuildError> {
        let r = report;
        if r.offset % ALIGN != 0 {
            return Err(violation(r.id, format!("offset {} is not {ALIGN}-aligned", r.offset)));
        }
        if r.offset < self.cursor {
            return Err(violation(
                r.id,
                format!(
                    "offset {} overlaps the previous diff, which ends at {}",
                    r.offset, self.cursor
                ),
            ));
        }
        if r.len > max_layer {
            return Err(violation(
                r.id,
                format!("{} bytes is over the layer limit of {max_layer}", r.len),
            ));
        }
        let end = r.offset.checked_add(r.len).filter(|&e| e <= self.size).ok_or_else(|| {
            violation(
                r.id,
                format!("[{}, +{}) is outside the {}-byte disk", r.offset, r.len, self.size),
            )
        })?;
        // The range, copied out and hashed by the host.
        let mut disk = std::fs::File::open(&self.path).map_err(io(format!("opening {}", self.path.display())))?;
        disk.seek(SeekFrom::Start(r.offset))
            .map_err(io("reading the output disk"))?;
        let mut tmp = tempfile::tempfile_in(spill_dir).map_err(io("creating a diff file"))?;
        let mut hasher = Sha256::new();
        let mut left = r.len;
        let mut buf = vec![0u8; 1 << 16];
        while left > 0 {
            let n = disk
                .read(&mut buf[..left.min(1 << 16) as usize])
                .map_err(io("reading the output disk"))?;
            if n == 0 {
                return Err(violation(r.id, "the range ends past the disk's data"));
            }
            hasher.update(&buf[..n]);
            tmp.write_all(&buf[..n]).map_err(io("writing a diff file"))?;
            left -= n as u64;
        }
        let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if hex != r.sha256 {
            return Err(violation(
                r.id,
                format!("sha256 {} where the range hashes to {hex}", r.sha256),
            ));
        }
        tmp.rewind().map_err(io("reading a diff file"))?;
        let mut entries = 0u64;
        for e in tar::Archive::new(&mut tmp).entries().map_err(|e| violation(r.id, e))? {
            let e = e.map_err(|e| violation(r.id, format!("not a tar: {e}")))?;
            // A diff names paths inside the root: kiln would normalise these, potter refuses them.
            let hardlink = e.header().entry_type() == tar::EntryType::Link;
            let link = if hardlink { e.link_name_bytes() } else { None };
            for name in std::iter::once(e.path_bytes()).chain(link) {
                if name.starts_with(b"/") || name.split(|&b| b == b'/').any(|c| c == b"..") {
                    return Err(violation(
                        r.id,
                        format!("the diff names {}", String::from_utf8_lossy(&name)),
                    ));
                }
            }
            entries += 1;
        }
        if entries != r.entries {
            return Err(violation(
                r.id,
                format!("{} entries where the diff holds {entries}", r.entries),
            ));
        }
        tmp.rewind().map_err(io("reading a diff file"))?;
        crate::ingest::validate_layer(std::io::BufReader::new(&mut tmp), spill_dir)?;
        tmp.rewind().map_err(io("reading a diff file"))?;
        let stored = store
            .put_reader(&mut tmp)
            .map_err(|e| BuildError::Infra(format!("storing the layer of step {}: {e}", r.id)))?;
        self.cursor = end;
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};

    use super::*;

    /// A tar of `entries` (name bytes, contents), names written raw so tests can lie.
    fn tar(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            let mut h = tar::Header::new_ustar();
            h.as_ustar_mut().unwrap().name[..name.len()].copy_from_slice(name);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mtime(0);
            h.set_cksum();
            out.extend_from_slice(h.as_bytes());
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.resize(out.len() + 1024, 0);
        out
    }

    struct Setup {
        _dir: tempfile::TempDir,
        store: Store,
        spill: PathBuf,
        disk: PathBuf,
    }

    const SIZE: u64 = 1 << 20;

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let spill = dir.path().join("spill");
        std::fs::create_dir(&spill).unwrap();
        let disk = dir.path().join("output.img");
        std::fs::File::create(&disk).unwrap().set_len(SIZE).unwrap();
        Setup {
            _dir: dir,
            store,
            spill,
            disk,
        }
    }

    fn write_at(disk: &Path, offset: u64, bytes: &[u8]) {
        let mut f = std::fs::OpenOptions::new().write(true).open(disk).unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn honest(id: u32, offset: u64, bytes: &[u8], entries: u64) -> StepSealed {
        StepSealed {
            id,
            offset,
            len: bytes.len() as u64,
            sha256: Digest::of(bytes).hex().to_string(),
            entries,
        }
    }

    fn blobs(store: &Store) -> usize {
        store.list_blobs().unwrap().len()
    }

    #[test]
    fn honest_diffs_are_taken_in_order() {
        let s = setup();
        let a = tar(&[(b"a.txt", b"a")]);
        let b = tar(&[(b"b.txt", b"bb"), (b"c.txt", b"c")]);
        write_at(&s.disk, 0, &a);
        let next = (a.len() as u64).div_ceil(ALIGN) * ALIGN;
        write_at(&s.disk, next, &b);
        let mut disk = OutputDisk::new(&s.disk, SIZE);
        let (da, sa) = disk.take(&honest(1, 0, &a, 1), &s.store, &s.spill, SIZE).unwrap();
        assert_eq!((da.clone(), sa), (Digest::of(&a), a.len() as u64));
        let (db, _) = disk.take(&honest(2, next, &b, 2), &s.store, &s.spill, SIZE).unwrap();
        assert_eq!(db, Digest::of(&b));
        assert!(s.store.has_blob(&da) && s.store.has_blob(&db));
    }

    /// Each lie or hostile diff fails with exit 3 and stores nothing.
    fn refused(s: &Setup, disk: &mut OutputDisk, report: StepSealed, why: &str) {
        let before = blobs(&s.store);
        let e = disk.take(&report, &s.store, &s.spill, 64 << 10).unwrap_err();
        assert_eq!(e.exit_code(), 3, "{why}: {e}");
        assert_eq!(blobs(&s.store), before, "{why}: stored something");
    }

    #[test]
    fn lying_or_misplaced_reports_are_protocol_violations() {
        let s = setup();
        let a = tar(&[(b"a.txt", b"a")]);
        write_at(&s.disk, 0, &a);
        let mut disk = OutputDisk::new(&s.disk, SIZE);
        let good = honest(1, 0, &a, 1);
        let mut r = good.clone();
        r.sha256 = Digest::of(b"other").hex().to_string();
        refused(&s, &mut disk, r, "a lying digest");
        let mut r = good.clone();
        r.entries = 2;
        refused(&s, &mut disk, r, "a lying entry count");
        let mut r = good.clone();
        r.len += 512;
        refused(&s, &mut disk, r, "a lying length");
        let mut r = good.clone();
        r.offset = 512;
        refused(&s, &mut disk, r, "an unaligned offset");
        let mut r = good.clone();
        r.offset = SIZE - ALIGN;
        r.len = 2 * ALIGN;
        refused(&s, &mut disk, r, "past the end of the disk");
        let mut r = good.clone();
        r.offset = u64::MAX - ALIGN + 1;
        refused(&s, &mut disk, r, "an offset that overflows");
        let mut big = good.clone();
        big.len = (64 << 10) + 1;
        refused(&s, &mut disk, big, "over the layer limit");
        // The honest report still works, then a second claim on the same range overlaps.
        disk.take(&good, &s.store, &s.spill, SIZE).unwrap();
        refused(&s, &mut disk, good.clone(), "overlapping the previous diff");
    }

    #[test]
    fn hostile_tars_are_refused_even_when_reported_honestly() {
        for (why, bytes) in [
            ("a .. path", tar(&[(b"../escape", b"x")])),
            ("an absolute path", tar(&[(b"/etc/passwd", b"x")])),
            ("garbage", vec![0x41; 4096]),
        ] {
            let s = setup();
            write_at(&s.disk, 0, &bytes);
            let mut disk = OutputDisk::new(&s.disk, SIZE);
            refused(&s, &mut disk, honest(1, 0, &bytes, 1), why);
        }
    }
}
