//! The ELF sonames a layer needs and provides (spec §16): `DT_NEEDED` and `DT_SONAME` of every
//! regular file that starts with `\x7fELF`, read with goblin. A rebase refuses to swap a run base
//! that no longer provides a soname the app needs, so a scan that could not finish must say so:
//! over a cap, [`Caps::exceeded`] names why, and the caller records nothing.

use std::collections::BTreeSet;
use std::io::Read;

use goblin::container::Ctx;
use goblin::elf::dynamic::{DT_NEEDED, DT_SONAME, DT_STRTAB};
use goblin::elf::{Dynamic, Elf, ProgramHeader};
use goblin::strtab::Strtab;
use tar::EntryType;

use crate::error::{BuildError, io};

/// The most bytes one ELF file may have.
pub const MAX_FILE: u64 = 256 << 20;
/// The most ELF bytes one scan reads, over all files and layers.
pub const MAX_TOTAL: u64 = 4 << 30;
/// The most ELF files one scan reads.
pub const MAX_FILES: usize = 100_000;

/// The limits of one scan, which may cover several layers, and what it has used of them.
#[derive(Debug, Clone)]
pub struct Caps {
    file: u64,
    total: u64,
    files: usize,
    read: u64,
    seen: usize,
    exceeded: Option<String>,
}

impl Default for Caps {
    fn default() -> Self {
        Caps::new(MAX_FILE, MAX_TOTAL, MAX_FILES)
    }
}

impl Caps {
    pub fn new(file: u64, total: u64, files: usize) -> Self {
        Caps {
            file,
            total,
            files,
            read: 0,
            seen: 0,
            exceeded: None,
        }
    }

    /// Why the scan stopped short; `None` when it read every ELF file.
    pub fn exceeded(&self) -> Option<&str> {
        self.exceeded.as_deref()
    }
}

/// The sonames the ELF files in `layer` (an uncompressed tar) need.
pub fn needed(layer: &mut dyn Read, caps: &mut Caps) -> Result<BTreeSet<String>, BuildError> {
    let mut out = BTreeSet::new();
    scan(layer, caps, |_, names| out.extend(names.needed.iter().cloned()))?;
    Ok(out)
}

/// The sonames the ELF files in `layer` (an uncompressed tar) provide: each shared object's
/// `DT_SONAME`, or its file name when it has none.
pub fn provided(layer: &mut dyn Read, caps: &mut Caps) -> Result<BTreeSet<String>, BuildError> {
    let mut out = BTreeSet::new();
    scan(layer, caps, |path, names| match &names.soname {
        Some(s) => {
            out.insert(s.clone());
        }
        None => {
            let name = path.rsplit('/').next().unwrap_or(path);
            if name.contains(".so") {
                out.insert(name.to_string());
            }
        }
    })?;
    Ok(out)
}

const MAGIC: [u8; 4] = *b"\x7fELF";

/// The names in a dynamically linked ELF file's dynamic section.
struct DynNames {
    needed: Vec<String>,
    soname: Option<String>,
}

/// `DT_NEEDED` and `DT_SONAME` of the ELF file `bytes`, reading only its header, program headers,
/// dynamic section and dynamic string table; `None` for a statically linked file.
fn dyn_names(bytes: &[u8]) -> Result<Option<DynNames>, String> {
    let header = Elf::parse_header(bytes).map_err(|e| e.to_string())?;
    let ctx = Ctx::new(
        header.container().map_err(|e| e.to_string())?,
        header.endianness().map_err(|e| e.to_string())?,
    );
    let phdrs = ProgramHeader::parse(bytes, header.e_phoff as usize, header.e_phnum as usize, ctx)
        .map_err(|e| e.to_string())?;
    let Some(dynamic) = Dynamic::parse(bytes, &phdrs, ctx).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let names: Vec<(u64, usize)> = dynamic
        .dyns
        .iter()
        .filter(|d| d.d_tag == DT_NEEDED || d.d_tag == DT_SONAME)
        .map(|d| (d.d_tag, d.d_val as usize))
        .collect();
    if names.is_empty() {
        return Ok(Some(DynNames {
            needed: Vec::new(),
            soname: None,
        }));
    }
    // goblin maps DT_STRTAB to a file offset, or to 0 when no segment holds it.
    if dynamic.info.strtab == 0 || !dynamic.dyns.iter().any(|d| d.d_tag == DT_STRTAB) {
        return Err("its dynamic string table is in no loaded segment".into());
    }
    let strtab = Strtab::parse(bytes, dynamic.info.strtab, dynamic.info.strsz, 0).map_err(|e| e.to_string())?;
    let mut out = DynNames {
        needed: Vec::new(),
        soname: None,
    };
    for (tag, at) in names {
        let name = strtab
            .get_at(at)
            .ok_or_else(|| format!("a dynamic entry names offset {at}, outside its string table"))?
            .to_string();
        if tag == DT_NEEDED {
            out.needed.push(name);
        } else {
            out.soname = Some(name);
        }
    }
    Ok(Some(out))
}

/// Calls `f` with the dynamic names of each dynamically linked ELF file of `layer` until a cap is
/// hit. An ELF file that cannot be read, and an entry that is not a regular file, directory,
/// symlink or hard link (a sparse file, a device), stop the scan too: skipping either could hide
/// a soname.
fn scan(layer: &mut dyn Read, caps: &mut Caps, mut f: impl FnMut(&str, &DynNames)) -> Result<(), BuildError> {
    if caps.exceeded.is_some() {
        return Ok(());
    }
    let mut archive = tar::Archive::new(layer);
    for entry in archive.entries().map_err(io("reading a layer for its sonames"))? {
        let mut entry = entry.map_err(io("reading a layer for its sonames"))?;
        let path = entry.path().map_err(io("reading a layer for its sonames"))?;
        let path = path.to_string_lossy().trim_start_matches("./").to_string();
        match entry.header().entry_type() {
            EntryType::Regular => {}
            EntryType::Directory | EntryType::Symlink | EntryType::Link => continue,
            other => {
                caps.exceeded = Some(format!("/{path} is a {other:?} entry, which the scan cannot read"));
                return Ok(());
            }
        }
        if entry.size() < MAGIC.len() as u64 {
            continue;
        }
        let mut magic = [0u8; 4];
        entry.read_exact(&mut magic).map_err(io(format!("reading /{path}")))?;
        if magic != MAGIC {
            continue;
        }
        let size = entry.size();
        let over = if caps.seen == caps.files {
            Some(format!(
                "more than {} ELF files, /{path} and later ones unread",
                caps.files
            ))
        } else if size > caps.file {
            Some(format!(
                "/{path} is {size} bytes, over the {} an ELF file may have",
                caps.file
            ))
        } else if caps.read + size > caps.total {
            Some(format!(
                "more than {} bytes of ELF files, /{path} and later ones unread",
                caps.total
            ))
        } else {
            None
        };
        if over.is_some() {
            caps.exceeded = over;
            return Ok(());
        }
        caps.seen += 1;
        caps.read += size;
        let mut bytes = Vec::with_capacity(size as usize);
        bytes.extend_from_slice(&magic);
        entry.read_to_end(&mut bytes).map_err(io(format!("reading /{path}")))?;
        // A parser bug must not take the build down: a panic reads as a file it cannot read.
        let parsed = std::panic::catch_unwind(|| dyn_names(&bytes))
            .unwrap_or_else(|_| Err("the ELF parser panicked on it".into()));
        match parsed {
            Ok(Some(names)) => f(&path, &names),
            Ok(None) => {}
            Err(e) => {
                caps.exceeded = Some(format!("/{path} starts like an ELF file but does not parse: {e}"));
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Minimal ELF64 shared objects, built byte by byte: a header, a `PT_LOAD` covering the
    //! file, a `PT_DYNAMIC`, the string table and the dynamic section. No toolchain needed.
    //! The checked-in `testdata/elf/*.so` files are this generator's output for each machine
    //! (`fixture_files_match_their_generator` keeps them in step).

    pub const EM_X86_64: u16 = 62;
    pub const EM_AARCH64: u16 = 183;
    pub const NEEDED: [&str; 2] = ["libc.so.6", "libssl.so.3"];
    pub const SONAME: &str = "libpotter-fixture.so.1";

    pub fn shared_object(machine: u16, needed: &[&str], soname: Option<&str>) -> Vec<u8> {
        const EHDR: usize = 64;
        const PHDR: usize = 56;
        let str_off = EHDR + 2 * PHDR;
        let mut strtab = vec![0u8];
        let mut name = |s: &str| {
            let at = strtab.len() as u64;
            strtab.extend_from_slice(s.as_bytes());
            strtab.push(0);
            at
        };
        let mut dynamic: Vec<(u64, u64)> = needed.iter().map(|n| (1, name(n))).collect();
        if let Some(s) = soname {
            dynamic.push((14, name(s)));
        }
        let dyn_off = (str_off + strtab.len()).next_multiple_of(8);
        dynamic.push((5, str_off as u64));
        dynamic.push((10, strtab.len() as u64));
        dynamic.push((0, 0));
        let dyn_size = dynamic.len() * 16;
        let total = (dyn_off + dyn_size) as u64;

        let mut b = Vec::new();
        b.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&3u16.to_le_bytes()); // ET_DYN
        b.extend_from_slice(&machine.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        b.extend_from_slice(&(EHDR as u64).to_le_bytes()); // e_phoff
        b.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
        b.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        for v in [EHDR as u16, PHDR as u16, 2, 64, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let phdr = |b: &mut Vec<u8>, kind: u32, flags: u32, off: u64, size: u64, align: u64| {
            b.extend_from_slice(&kind.to_le_bytes());
            b.extend_from_slice(&flags.to_le_bytes());
            for v in [off, off, off, size, size, align] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        };
        phdr(&mut b, 1, 4, 0, total, 0x1000); // PT_LOAD, R
        phdr(&mut b, 2, 6, dyn_off as u64, dyn_size as u64, 8); // PT_DYNAMIC, RW
        b.extend_from_slice(&strtab);
        b.resize(dyn_off, 0);
        for (tag, val) in dynamic {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&val.to_le_bytes());
        }
        b
    }

    /// The checked-in fixture for `machine`.
    pub fn checked_in(machine: u16) -> Vec<u8> {
        shared_object(machine, &NEEDED, Some(SONAME))
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{EM_AARCH64, EM_X86_64, NEEDED, SONAME};
    use super::*;
    use crate::tarball::{Meta, TarWriter};

    const X86_64: &[u8] = include_bytes!("../testdata/elf/libpotter-fixture-x86_64.so");
    const AARCH64: &[u8] = include_bytes!("../testdata/elf/libpotter-fixture-aarch64.so");

    pub(crate) fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = TarWriter::new(Vec::new());
        for (path, body) in files {
            w.file(path, &Meta::root(0o755, 0), body.len() as u64, &mut &body[..])
                .unwrap();
        }
        w.finish().unwrap()
    }

    /// Rewrites the checked-in fixtures: `cargo test -p potter-exec write_fixture_files -- --ignored`.
    #[test]
    #[ignore]
    fn write_fixture_files() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/elf");
        std::fs::create_dir_all(&dir).unwrap();
        for (arch, machine) in [("x86_64", EM_X86_64), ("aarch64", EM_AARCH64)] {
            let path = dir.join(format!("libpotter-fixture-{arch}.so"));
            std::fs::write(path, fixture::checked_in(machine)).unwrap();
        }
    }

    #[test]
    fn fixture_files_match_their_generator() {
        assert_eq!(X86_64, fixture::checked_in(EM_X86_64));
        assert_eq!(AARCH64, fixture::checked_in(EM_AARCH64));
        assert!(X86_64.len() < 16 << 10 && AARCH64.len() < 16 << 10);
    }

    #[test]
    fn sonames_needed_reads_dt_needed() {
        for elf in [X86_64, AARCH64] {
            let layer = tar(&[
                ("usr/lib/libpotter-fixture.so.1", elf),
                ("app/readme", b"\x7fELLO, not an ELF file"),
                ("app/tiny", b"\x7f"),
            ]);
            let mut caps = Caps::default();
            let found = needed(&mut &layer[..], &mut caps).unwrap();
            assert_eq!(found, NEEDED.map(String::from).into());
            assert_eq!(caps.exceeded(), None);
        }
    }

    #[test]
    fn provided_reads_dt_soname_or_the_file_name() {
        let bare = fixture::shared_object(EM_X86_64, &[], None);
        let layer = tar(&[
            ("usr/lib/libpotter-fixture.so.1.2.3", X86_64),
            ("usr/lib/libbare.so.7", &bare),
        ]);
        let mut caps = Caps::default();
        let found = provided(&mut &layer[..], &mut caps).unwrap();
        assert_eq!(found, [SONAME, "libbare.so.7"].map(String::from).into());
    }

    #[test]
    fn every_cap_stops_the_scan_and_says_why() {
        let layer = tar(&[("a.so", X86_64), ("b.so", X86_64)]);
        let size = X86_64.len() as u64;
        for (mut caps, why) in [
            (Caps::new(size - 1, MAX_TOTAL, MAX_FILES), "a.so"),
            (Caps::new(MAX_FILE, size + 1, MAX_FILES), "b.so"),
            (Caps::new(MAX_FILE, MAX_TOTAL, 1), "b.so"),
        ] {
            needed(&mut &layer[..], &mut caps).unwrap();
            let e = caps.exceeded().expect("a cap was hit");
            assert!(e.contains(why), "{e}");
        }
        // The caps span layers: a second layer continues where the first left off.
        let mut caps = Caps::new(MAX_FILE, MAX_TOTAL, 2);
        needed(&mut &layer[..], &mut caps).unwrap();
        assert_eq!(caps.exceeded(), None);
        needed(&mut &tar(&[("c.so", X86_64)])[..], &mut caps).unwrap();
        assert!(caps.exceeded().is_some());
    }

    /// A tar of `kind` entries named `path`, after a readable ELF file.
    fn with_entry(kind: tar::EntryType, path: &str) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(X86_64.len() as u64);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, "lib/a.so", X86_64).unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_entry_type(kind);
        b.append_data(&mut h, path, &[][..]).unwrap();
        b.into_inner().unwrap()
    }

    #[test]
    fn an_entry_of_another_kind_stops_the_scan() {
        for kind in [tar::EntryType::Fifo, tar::EntryType::Char, tar::EntryType::Continuous] {
            let mut caps = Caps::default();
            needed(&mut &with_entry(kind, "odd")[..], &mut caps).unwrap();
            assert!(
                caps.exceeded().is_some_and(|e| e.contains("/odd")),
                "{kind:?}: {caps:?}"
            );
        }
        for kind in [tar::EntryType::Directory, tar::EntryType::Symlink, tar::EntryType::Link] {
            let mut caps = Caps::default();
            needed(&mut &with_entry(kind, "fine")[..], &mut caps).unwrap();
            assert_eq!(caps.exceeded(), None, "{kind:?}");
        }
    }

    #[test]
    fn a_string_table_outside_the_loaded_segments_stops_the_scan() {
        let mut elf = fixture::checked_in(EM_X86_64);
        // PT_LOAD's p_filesz and p_memsz (at 64 + 32 and 64 + 40): the segment now maps nothing.
        elf[96..112].fill(0);
        let mut caps = Caps::default();
        needed(&mut &tar(&[("lib/a.so", &elf)])[..], &mut caps).unwrap();
        assert!(caps.exceeded().is_some_and(|e| e.contains("/lib/a.so")), "{caps:?}");
    }

    #[test]
    fn a_static_elf_file_needs_and_provides_nothing() {
        let mut elf = fixture::checked_in(EM_AARCH64);
        elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum: PT_LOAD only
        let layer = tar(&[("bin/static", &elf)]);
        let mut caps = Caps::default();
        assert!(needed(&mut &layer[..], &mut caps).unwrap().is_empty());
        assert!(provided(&mut &layer[..], &mut caps).unwrap().is_empty());
        assert_eq!(caps.exceeded(), None);
    }

    #[test]
    fn an_unreadable_elf_file_stops_the_scan() {
        let layer = tar(&[("broken.so", b"\x7fELF\x02\x01\x01 and then nothing")]);
        let mut caps = Caps::default();
        needed(&mut &layer[..], &mut caps).unwrap();
        assert!(caps.exceeded().is_some_and(|e| e.contains("broken.so")), "{caps:?}");
    }
}
