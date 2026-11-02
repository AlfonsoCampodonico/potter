//! The catalog driver (design §4.2, §6.2): `potter catalog build` builds every recipe under a
//! catalog directory with the namespace executor, makes each one layer, checks the layers, and
//! writes a signed OCI layout; `potter catalog publish` verifies that layout and pushes it.
//!
//! Recipes build in `requires` order, each against the layers built before it: an in-progress
//! layout signed with a key made for this run and trusted by this process alone.

pub mod abi;
pub mod assign;
pub mod overlap;
pub mod recipe;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use kiln_oci::media::OCI_LAYER_ZSTD;
use kiln_oci::{Descriptor, Platform};
use kiln_registry::{DockerConfig, Reference};
use kiln_store::{Digest, Hasher, Store};
use potter_cache::{ExecutorClass, PotterHome, Tenant};
use potter_catalog::{Entry, Index, Location, SigningKey, TrustedKey, Verified, fetch_layer};
use potter_plan::{Base, Chain, Command, Export, Step};
use sha2::Digest as _;

use crate::base::layer_reader;
use crate::chain::{BuildCtx, CatalogCtx, LayerRef, Progress, catalog_arch, run_chain, run_steps};
use crate::error::{BuildError, io};
use crate::merge::{ExportSpec, LayerOpen, export_layer, file_size, normalise, read_file};
use crate::sandboxed::SandboxCtx;
use crate::sonames::{Caps, provided};
use crate::tarball::{Meta, TarWriter};
use assign::{Outcome, assign};
use recipe::{OS_MIN, Recipe};

/// The tenant whose cache catalog builds use.
pub const TENANT: &str = "catalog";
/// The executor class every entry records (design §6.4).
pub const EXECUTOR: &str = "ns";
/// The layer blobs' zstd level: fixed, so the blob is too.
const ZSTD_LEVEL: i32 = 3;

/// What the driver builds on: the parts of a [`BuildCtx`] that stay the same for every recipe.
pub struct Parts<'a> {
    pub store: &'a Store,
    pub home: &'a PotterHome,
    pub sandbox: &'a SandboxCtx,
    pub platform: Platform,
    pub auth: DockerConfig,
    pub progress: &'a mut dyn FnMut(Progress),
}

pub struct Request<'a> {
    pub catalog_dir: &'a Path,
    /// The OCI layout to write.
    pub out: &'a Path,
    pub sign_key: &'a SigningKey,
    /// The catalog this one follows: its entries stay, and its serial goes up by one.
    pub previous: Option<&'a Location>,
    /// The index's `created` timestamp (RFC 3339).
    pub created: String,
}

/// A written catalog.
#[derive(Debug, Clone)]
pub struct Report {
    pub index: Index,
    /// The index blob's digest.
    pub digest: String,
    /// One per recipe, in build order: the entry it ended as and what became of it.
    pub layers: Vec<(Entry, Outcome)>,
}

/// One built layer.
struct Built {
    recipe: Recipe,
    entry: Entry,
    /// Its blob in the store.
    blob: PathBuf,
    /// Its regular files.
    files: BTreeSet<String>,
}

/// Builds the catalog under `req.catalog_dir` and writes it to `req.out`.
pub fn build(parts: &mut Parts, req: &Request) -> Result<Report, BuildError> {
    let recipes = recipe::load(req.catalog_dir)?;
    let arch = catalog_arch(&parts.platform)?;
    let previous = req
        .previous
        .map(|loc| potter_catalog::load(loc, None, parts.home.root(), parts.store, parts.auth.clone()))
        .transpose()?;
    let tenant = Tenant::new(TENANT).map_err(|e| BuildError::Infra(e.to_string()))?;
    let cache = parts
        .home
        .index(&tenant)
        .map_err(|e| BuildError::Infra(e.to_string()))?;
    let tenant_key = potter_cache::tenant_key(parts.home, &tenant).map_err(|e| BuildError::Infra(e.to_string()))?;
    let work = tempfile::Builder::new()
        .prefix("catalog-build-")
        .tempdir_in(parts.home.root())
        .map_err(io("creating the catalog build's work directory"))?;
    let in_progress = InProgress::new(work.path())?;

    let mut built: Vec<Built> = Vec::new();
    let mut os_abi: Option<[(String, String); 2]> = None;
    let mut catalog: Option<CatalogCtx> = None;
    for recipe in recipes {
        let state = {
            let mut ctx = BuildCtx {
                store: parts.store,
                home: parts.home,
                tenant: &tenant,
                index: &cache,
                sandbox: parts.sandbox,
                platform: parts.platform.clone(),
                no_cache: false,
                auth: parts.auth.clone(),
                executor: ExecutorClass::Ns,
                backend: None,
                secrets: &BTreeMap::new(),
                tenant_key,
                cache_from: None,
                cache_to: None,
                catalog: catalog.as_ref(),
                deps: Default::default(),
                progress: &mut *parts.progress,
            };
            run_chain(&mut ctx, &recipe.target, &recipe.dir)?
        };
        // os-min is its whole root, flattened; any other layer holds what its recipe added above
        // the layers it was built on.
        let above = match recipe.target.build.base {
            Base::Oci { .. } => 0,
            _ => state.base.layers().len(),
        };
        let (under, own) = state.layers.split_at(above);
        check_deletions(parts.store, under, own, recipe.name())?;
        if recipe.name() == OS_MIN {
            os_abi = Some(read_os_abi(parts.store, own)?);
        }
        let [os, libc] = os_abi.clone().expect("os-min builds first");
        let l = &recipe.layer;
        let mut entry = Entry {
            name: l.name.clone(),
            line: l.line.clone(),
            version: l.version.clone(),
            arch,
            kind: l.kind,
            digest: String::new(),
            diff_id: String::new(),
            size: 0,
            media_type: String::new(),
            abi: l.abi.clone().into_iter().chain([os, libc]).collect(),
            requires: recipe.requires(),
            conflicts: l.conflicts.clone(),
            env: l.env.clone(),
            provides_sonames: Vec::new(),
            executor: EXECUTOR.into(),
            source: l.source.clone(),
            license: l.license.clone(),
            supersedes: None,
            fixes: l.fixes.clone(),
        };
        let layer = extract(parts.store, own, work.path(), recipe.name(), &metadata(&entry))?;
        entry.digest = layer.descriptor.digest.to_string();
        entry.diff_id = layer.diff_id.to_string();
        entry.size = layer.descriptor.size;
        entry.media_type = layer.descriptor.media_type.clone();
        entry.provides_sonames = layer.sonames;
        if let Some(twin) = built.iter().find(|b| b.entry.digest == entry.digest) {
            return Err(BuildError::Plan(format!(
                "the recipes {} and {} build the same layer {}",
                twin.entry.name, entry.name, entry.digest
            )));
        }
        built.push(Built {
            blob: parts.store.blob_path(&layer.descriptor.digest),
            recipe,
            entry,
            files: layer.files,
        });
        catalog = Some(in_progress.publish(&built, parts)?);
    }

    let overlaps = overlap::check(
        &built
            .iter()
            .map(|b| overlap::Layer {
                name: &b.entry.name,
                conflicts: &b.entry.conflicts,
                files: &b.files,
            })
            .collect::<Vec<_>>(),
    );
    if !overlaps.is_empty() {
        return Err(BuildError::LayerCheck(format!(
            "catalog layers overlap: {}",
            overlaps.join("; ")
        )));
    }

    let catalog = catalog.expect("os-min was built");
    for b in &built {
        smoke(parts, &tenant, &cache, tenant_key, &catalog, &b.recipe)?;
    }

    let assigned = assign(
        previous.as_ref().map(|p| &p.index),
        built.iter().map(|b| b.entry.clone()).collect(),
        req.created.clone(),
    )?;
    let mut layers = Vec::with_capacity(built.len());
    for (b, (_, outcome)) in built.iter().zip(assigned.outcomes) {
        let entry = assigned
            .index
            .by_digest(&b.entry.digest)
            .cloned()
            .expect("every built layer has an entry");
        // The layer's bytes hold its metadata, so the same digest is the same entry.
        let published = |e: &Entry| {
            (
                metadata(e),
                e.diff_id.clone(),
                e.size,
                e.media_type.clone(),
                e.provides_sonames.clone(),
            )
        };
        if outcome == Outcome::Kept && published(&entry) != published(&b.entry) {
            return Err(BuildError::Infra(format!(
                "internal error: {} {} has the digest of the previous entry but not its metadata",
                b.entry.name, b.entry.digest
            )));
        }
        layers.push((entry, outcome));
    }
    let blobs = layout_blobs(parts, &assigned.index, &built, previous.as_ref())?;
    let digest = write_signed(req.out, &assigned.index, req.sign_key, &blobs)?;
    Ok(Report {
        index: assigned.index,
        digest,
        layers,
    })
}

/// `potter catalog build --merge`: the per-architecture layouts of one run, each built with
/// the same `previous`, as one index signed once.
pub struct MergeRequest<'a> {
    pub layouts: &'a [PathBuf],
    /// The keys the layouts are signed with (the per-architecture builds' throwaway keys). They
    /// show a layout is the one its build wrote; trust comes from `sign_key`'s signature alone.
    pub layout_keys: &'a [TrustedKey],
    pub out: &'a Path,
    pub sign_key: &'a SigningKey,
    pub previous: Option<&'a Location>,
    pub created: String,
}

/// Merges `req.layouts` into one layout at `req.out`: the previous catalog's entries, then each
/// layout's new ones, at one serial above the previous one, with every current layer's blob.
/// `home` keeps the previous catalog's serial record; `store` receives the layers, each
/// re-hashed on the way in.
pub fn merge(store: &Store, home: &Path, auth: DockerConfig, req: &MergeRequest) -> Result<Report, BuildError> {
    let previous = req
        .previous
        .map(|loc| potter_catalog::load(loc, None, home, store, auth.clone()))
        .transpose()?;
    // The layouts' serial records are of no use later: keep them out of `home`.
    let scratch = tempfile::tempdir().map_err(io("creating a scratch home"))?;
    let mut builds = Vec::with_capacity(req.layouts.len());
    for (i, dir) in req.layouts.iter().enumerate() {
        let v = potter_catalog::load_trusting(
            &Location::Layout(dir.clone()),
            None,
            &scratch.path().join(i.to_string()),
            store,
            auth.clone(),
            req.layout_keys,
        )?;
        builds.push(v);
    }
    let index = assign::merge(
        previous.as_ref().map(|p| &p.index),
        &builds.iter().map(|b| b.index.clone()).collect::<Vec<_>>(),
        req.created.clone(),
    )?;
    let mut blobs = Vec::new();
    for e in index.current() {
        let from = builds
            .iter()
            .find(|b| b.index.by_digest(&e.digest).is_some())
            .expect("every merged entry comes from a layout");
        let d = fetch_layer(from, e, store, auth.clone())?;
        blobs.push((d.clone(), store.blob_path(&d.digest)));
    }
    let digest = write_signed(req.out, &index, req.sign_key, &blobs)?;
    let before = previous.as_ref().map_or(0, |p| p.index.entries.len());
    let layers = index.entries[before..]
        .iter()
        .map(|e| {
            let outcome = match &e.supersedes {
                Some(old) => Outcome::Supersedes(old.clone()),
                None => Outcome::New,
            };
            (e.clone(), outcome)
        })
        .collect();
    Ok(Report { index, digest, layers })
}

/// Runs `recipe`'s smoke test: one `sh -c` step on the composite of its layer's closure and
/// `smoke_with`, uncached and offline.
fn smoke(
    parts: &mut Parts,
    tenant: &Tenant,
    cache: &potter_cache::Index,
    tenant_key: [u8; 32],
    catalog: &CatalogCtx,
    recipe: &Recipe,
) -> Result<(), BuildError> {
    let name = recipe.name();
    let chain = Chain {
        base: Base::Catalog {
            layers: recipe.smoke_refs(),
        },
        steps: vec![Step::Run {
            name: "smoke".into(),
            command: Command::argv(&["sh", "-c", &recipe.layer.smoke]),
            env: Default::default(),
            workdir: "/".into(),
            user: "0:0".into(),
            network: false,
            cache_mounts: vec![],
            secrets: vec![],
            timeout_secs: None,
        }],
    };
    let mut ctx = BuildCtx {
        store: parts.store,
        home: parts.home,
        tenant,
        index: cache,
        sandbox: parts.sandbox,
        platform: parts.platform.clone(),
        no_cache: true,
        auth: parts.auth.clone(),
        executor: ExecutorClass::Ns,
        backend: None,
        secrets: &BTreeMap::new(),
        tenant_key,
        cache_from: None,
        cache_to: None,
        catalog: Some(catalog),
        deps: Default::default(),
        progress: &mut *parts.progress,
    };
    match run_steps(
        &mut ctx,
        &format!("{name}:smoke"),
        &chain,
        &recipe.dir,
        crate::stopgap::Role::Build,
    ) {
        Ok(_) => Ok(()),
        Err(BuildError::StepFailed { status, tail, .. }) => {
            let tail = match tail.is_empty() {
                true => String::new(),
                false => format!(": {}", tail.join(" | ")),
            };
            Err(BuildError::LayerCheck(format!(
                "the smoke test of {name} failed ({status}){tail}"
            )))
        }
        Err(e) => Err(e),
    }
}

/// The in-progress layout the recipes resolve their `requires` against.
struct InProgress {
    dir: PathBuf,
    /// Keeps the layout's serial record out of `POTTER_HOME`.
    home: PathBuf,
    key: SigningKey,
    trusted: TrustedKey,
}

impl InProgress {
    fn new(work: &Path) -> Result<Self, BuildError> {
        let (private, public) = potter_catalog::generate_pem();
        let key = potter_catalog::signing_key_from_pem(&private).map_err(BuildError::Infra)?;
        let trusted = TrustedKey::from_pem(&public).map_err(BuildError::Infra)?;
        Ok(InProgress {
            dir: work.join("layout"),
            home: work.join("home"),
            key,
            trusted,
        })
    }

    /// Signs and writes the layout of `built` so far, and loads it as this process would any
    /// catalog, trusting the run's key.
    fn publish(&self, built: &[Built], parts: &Parts) -> Result<CatalogCtx, BuildError> {
        let index = Index {
            schema: potter_catalog::SCHEMA.into(),
            serial: built.len() as u64,
            created: "1970-01-01T00:00:00Z".into(),
            entries: built.iter().map(|b| b.entry.clone()).collect(),
        };
        let last = built.last().expect("a layer was built");
        let layer = Descriptor::new(
            &last.entry.media_type,
            Digest::parse(&last.entry.digest).expect("a digest the driver made"),
            last.entry.size,
        );
        write_signed(&self.dir, &index, &self.key, &[(layer, last.blob.clone())])?;
        CatalogCtx::load_at(
            parts.store,
            &self.home,
            &Location::Layout(self.dir.clone()),
            parts.auth.clone(),
            std::slice::from_ref(&self.trusted),
        )
    }
}

/// Signs `index` with `key` and writes it, with `layers`, to the layout at `dir`; returns the
/// index blob's digest.
fn write_signed(
    dir: &Path,
    index: &Index,
    key: &SigningKey,
    layers: &[(Descriptor, PathBuf)],
) -> Result<String, BuildError> {
    let blob = index.canonical();
    let sha: [u8; 32] = sha2::Sha256::digest(&blob).into();
    let (id, sig) = potter_catalog::sign(&sha, key);
    potter_catalog::write_layout(dir, &blob, &id, &sig, layers)?;
    Ok(Digest::of(&blob).to_string())
}

/// The blob of every current entry of `index`: the built ones from the store, the others
/// fetched from the previous catalog, so the layout holds every layer it resolves to.
fn layout_blobs(
    parts: &Parts,
    index: &Index,
    built: &[Built],
    previous: Option<&Verified>,
) -> Result<Vec<(Descriptor, PathBuf)>, BuildError> {
    let mut out = Vec::new();
    for e in index.current() {
        let descriptor = match (built.iter().find(|b| b.entry.digest == e.digest), previous) {
            (Some(b), _) => {
                let d = Descriptor::new(&e.media_type, Digest::parse(&e.digest).expect("built"), e.size);
                out.push((d, b.blob.clone()));
                continue;
            }
            (None, Some(p)) => fetch_layer(p, e, parts.store, parts.auth.clone())?,
            (None, None) => unreachable!("every entry of a first catalog was built"),
        };
        out.push((descriptor.clone(), parts.store.blob_path(&descriptor.digest)));
    }
    Ok(out)
}

/// Forwards writes to `inner`, hashing them.
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

/// Where each layer carries its own metadata.
pub const METADATA_DIR: &str = "usr/share/potter-catalog";

/// The canonical JSON of what an entry publishes about its layer, which the layer holds at
/// `/usr/share/potter-catalog/<name>.json`: a change to any of it changes the layer's digest.
/// `fixes`, `supersedes` and `executor` describe the entry, not the layer, and stay out.
pub fn metadata(e: &Entry) -> Vec<u8> {
    let v = serde_json::json!({
        "name": e.name,
        "line": e.line,
        "version": e.version,
        "kind": e.kind,
        "requires": e.requires,
        "conflicts": e.conflicts,
        "abi": e.abi,
        "env": e.env,
        "source": e.source,
        "license": e.license,
    });
    potter_plan::canonical_json(&v)
        .expect("metadata serializes to JSON without floats")
        .into_bytes()
}

/// The base paths that `own` (the layers a recipe added) delete from `under` (the layers it
/// built on), with whiteouts or opaque directories. A layer cannot carry deletions, so they
/// would be lost: the recipe is refused instead.
fn check_deletions(store: &Store, under: &[LayerRef], own: &[LayerRef], name: &str) -> Result<(), BuildError> {
    if under.is_empty() {
        return Ok(());
    }
    let what = || format!("reading the layers of {name}");
    let paths = |l: &LayerRef| -> Result<Vec<String>, BuildError> {
        let mut out = Vec::new();
        for entry in tar::Archive::new(layer_reader(store, &l.descriptor)?)
            .entries()
            .map_err(io(what()))?
        {
            let entry = entry.map_err(io(what()))?;
            if let Some(p) = normalise(&entry.path_bytes())? {
                out.push(p);
            }
        }
        Ok(out)
    };
    let mut base = BTreeSet::new();
    for l in under {
        base.extend(
            paths(l)?
                .into_iter()
                .filter(|p| !p.rsplit('/').next().unwrap_or(p).starts_with(".wh.")),
        );
    }
    let under_dir = |dir: &str| -> Vec<String> {
        match dir.is_empty() {
            true => base.iter().cloned().collect(),
            false => {
                let prefix = format!("{dir}/");
                base.range(prefix.clone()..)
                    .take_while(|p| p.starts_with(&prefix))
                    .cloned()
                    .collect()
            }
        }
    };
    let mut hidden = BTreeSet::new();
    for l in own {
        for p in paths(l)? {
            let (dir, file) = p.rsplit_once('/').unwrap_or(("", p.as_str()));
            if file == ".wh..wh..opq" {
                hidden.extend(under_dir(dir));
            } else if let Some(gone) = file.strip_prefix(".wh.") {
                let target = if dir.is_empty() {
                    gone.to_string()
                } else {
                    format!("{dir}/{gone}")
                };
                if base.contains(&target) {
                    hidden.insert(target.clone());
                }
                hidden.extend(under_dir(&target));
            }
        }
    }
    if hidden.is_empty() {
        return Ok(());
    }
    let mut shown: Vec<String> = hidden.iter().take(overlap::SHOWN).map(|p| format!("/{p}")).collect();
    if hidden.len() > overlap::SHOWN {
        shown.push(format!("and {} more", hidden.len() - overlap::SHOWN));
    }
    Err(BuildError::Plan(format!(
        "the recipe {name} deletes {} path{} the layers it builds on hold, which its layer cannot carry: {}",
        hidden.len(),
        if hidden.len() == 1 { "" } else { "s" },
        shown.join(", ")
    )))
}

/// A built layer: its zstd blob in the store, and what it holds.
struct Extracted {
    descriptor: Descriptor,
    diff_id: Digest,
    files: BTreeSet<String>,
    sonames: Vec<String>,
}

/// `layers` (bottom first) flattened into one deterministic layer, without the recipe's own
/// `/app` and with `metadata` at `/usr/share/potter-catalog/<name>.json`, stored
/// zstd-compressed.
fn extract(
    store: &Store,
    layers: &[LayerRef],
    work: &Path,
    name: &str,
    metadata: &[u8],
) -> Result<Extracted, BuildError> {
    if layers.is_empty() {
        return Err(BuildError::Plan(format!(
            "the recipe {name} added nothing to its layer"
        )));
    }
    let mut top = TarWriter::new(Vec::new());
    let meta = Meta {
        mode: 0o644,
        uid: 0,
        gid: 0,
        mtime: 0,
    };
    top.file(
        &format!("{METADATA_DIR}/{name}.json"),
        &meta,
        metadata.len() as u64,
        &mut &metadata[..],
    )
    .map_err(io(format!("writing the metadata of {name}")))?;
    let top = top.finish().map_err(io(format!("writing the metadata of {name}")))?;
    let mut open: Vec<LayerOpen> = layers
        .iter()
        .map(|l| Box::new(move || layer_reader(store, &l.descriptor)) as LayerOpen)
        .collect();
    open.push(Box::new(move || {
        Ok(Box::new(io::Cursor::new(top.clone())) as Box<dyn io::Read + Send>)
    }));
    // The recipe's sources are copied to /app for its steps; they are not the layer's.
    let everything = [Export {
        path: "/".into(),
        dest: "/".into(),
        exclude: vec!["app".into()],
    }];
    let spec = ExportSpec {
        exports: &everything,
        epoch: 0,
        spill_dir: work,
    };
    let what = format!("flattening the layer {name}");
    let tar = tempfile::NamedTempFile::new_in(work).map_err(io(what.clone()))?;
    let out = Hashing {
        inner: io::BufWriter::new(tar.reopen().map_err(io(what.clone()))?),
        hasher: Hasher::new(),
    };
    let (mut out, _) = export_layer(&open, &spec, out)?;
    out.flush().map_err(io(what.clone()))?;
    let Hashing { inner, hasher } = out;
    drop(inner);
    let diff_id = hasher.finish();

    let mut files = BTreeSet::new();
    let mut file = tar.reopen().map_err(io(what.clone()))?;
    for entry in tar::Archive::new(&mut file).entries().map_err(io(what.clone()))? {
        let entry = entry.map_err(io(what.clone()))?;
        if matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Link
        ) {
            let path = entry.path().map_err(io(what.clone()))?;
            files.insert(path.to_string_lossy().trim_start_matches("./").to_string());
        }
    }
    file.seek(SeekFrom::Start(0)).map_err(io(what.clone()))?;
    let mut caps = Caps::default();
    let sonames = provided(&mut file, &mut caps)?;
    if let Some(why) = caps.exceeded() {
        return Err(BuildError::LayerCheck(format!(
            "the layer {name} could not be scanned for sonames: {why}"
        )));
    }

    file.seek(SeekFrom::Start(0)).map_err(io(what.clone()))?;
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("storing the layer {name}: {e}"));
    let blob = store.tmp_blob().map_err(|e| infra(&e))?;
    let mut encoder = zstd::Encoder::new(blob.reopen().map_err(|e| infra(&e))?, ZSTD_LEVEL)
        .map_err(io(format!("compressing the layer {name}")))?;
    io::copy(&mut file, &mut encoder).map_err(io(format!("compressing the layer {name}")))?;
    encoder.finish().map_err(io(format!("compressing the layer {name}")))?;
    let digest = store.commit(blob).map_err(|e| infra(&e))?;
    let size = store.blob_size(&digest).map_err(|e| infra(&e))?;
    Ok(Extracted {
        descriptor: Descriptor::new(OCI_LAYER_ZSTD, digest, size),
        diff_id,
        files,
        sonames: sonames.into_iter().collect(),
    })
}

/// `os` and `libc` from the `os-min` chain's `layers`, read on the host.
fn read_os_abi(store: &Store, layers: &[LayerRef]) -> Result<[(String, String); 2], BuildError> {
    let open: Vec<LayerOpen> = layers
        .iter()
        .map(|l| Box::new(move || layer_reader(store, &l.descriptor)) as LayerOpen)
        .collect();
    let text = |p: &str, cap: u64| -> Result<Option<String>, BuildError> {
        match file_size(&open, p)? {
            None => Ok(None),
            Some(n) if n > cap => Err(BuildError::Plan(format!(
                "os-min's {p} is {n} bytes, over the {cap} the driver reads"
            ))),
            Some(_) => Ok(read_file(&open, p, cap)?.map(|b| String::from_utf8_lossy(&b).into_owned())),
        }
    };
    let release = match text("/etc/os-release", 1 << 20)? {
        Some(t) => t,
        None => text("/usr/lib/os-release", 1 << 20)?.ok_or_else(|| {
            BuildError::Plan("os-min has neither /etc/os-release nor /usr/lib/os-release as a file".into())
        })?,
    };
    let status = text("/var/lib/dpkg/status", 64 << 20)?
        .ok_or_else(|| BuildError::Plan("os-min has no /var/lib/dpkg/status to read its libc6 from".into()))?;
    Ok([
        ("os".to_string(), abi::os(&release)?),
        ("libc".to_string(), abi::libc(&status)?),
    ])
}

/// `potter catalog publish`: verifies the layout at `layout` with the trusted keys, checks it
/// holds every current layer and follows the index already at `to` (if any), then pushes the
/// layers and the index to `to`. With `first`, a registry that refuses to read `to` (401/403)
/// counts as holding no index yet, as ghcr answers for a package that does not exist; an index
/// it does serve must still be followed, so `first` never replaces a published catalog.
pub fn publish(layout: &Path, to: &Reference, auth: DockerConfig, first: bool) -> Result<Verified, BuildError> {
    // Verifying writes the index blob and a serial record: into a scratch store and home, not
    // the user's.
    let scratch = tempfile::tempdir().map_err(io("creating a scratch store"))?;
    let store = Store::open(scratch.path().join("store")).map_err(|e| BuildError::Infra(e.to_string()))?;
    let v = potter_catalog::load(
        &Location::Layout(layout.to_path_buf()),
        None,
        &scratch.path().join("home"),
        &store,
        auth.clone(),
    )?;
    // The index already published there must verify, and the new one must follow it: a higher
    // serial, and every entry it lists still listed. Otherwise a publish could roll it back.
    if potter_catalog::exists(to, auth.clone(), first)? {
        let current = potter_catalog::load(
            &Location::Registry(to.clone()),
            None,
            &scratch.path().join("target-home"),
            &store,
            auth.clone(),
        )?;
        follows(&current.index, &v.index, to)?;
    }
    for e in v.index.current() {
        fetch_layer(&v, e, &store, auth.clone())?;
    }
    potter_catalog::push(layout, to, auth)?;
    Ok(v)
}

/// Whether `new` may replace `current`, the index published at `to`.
fn follows(current: &Index, new: &Index, to: &Reference) -> Result<(), BuildError> {
    if new.serial <= current.serial {
        return Err(BuildError::Plan(format!(
            "{to} is at serial {}; publishing serial {} over it would roll it back",
            current.serial, new.serial
        )));
    }
    if new.entries.get(..current.entries.len()) != Some(&current.entries[..]) {
        return Err(BuildError::Plan(format!(
            "{to} is at serial {}, and the layout does not list its entries as they are: build it \
             with --previous {to}",
            current.serial
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use potter_catalog::{Arch, Kind};

    use super::*;

    fn entry() -> Entry {
        Entry {
            name: "hello".into(),
            line: "1".into(),
            version: "1.0.0".into(),
            arch: Arch::X86_64,
            kind: Kind::Tool,
            digest: "sha256:aa".into(),
            diff_id: "sha256:bb".into(),
            size: 1,
            media_type: OCI_LAYER_ZSTD.into(),
            abi: [("os".to_string(), "debian-13".to_string())].into(),
            requires: vec![potter_plan::CatalogRef::new("os-min", None)],
            conflicts: vec![],
            env: Default::default(),
            provides_sonames: vec![],
            executor: "ns".into(),
            source: "https://example.test".into(),
            license: "MIT".into(),
            supersedes: None,
            fixes: vec![],
        }
    }

    #[test]
    fn metadata_is_what_the_entry_publishes_about_its_layer() {
        let e = entry();
        let m = metadata(&e);
        assert_eq!(
            String::from_utf8(m.clone()).unwrap(),
            r#"{"abi":{"os":"debian-13"},"conflicts":[],"env":{},"kind":"tool","license":"MIT","line":"1","name":"hello","requires":[{"name":"os-min"}],"source":"https://example.test","version":"1.0.0"}"#
        );
        // What describes the entry rather than the layer is left out.
        let other = Entry {
            supersedes: Some("sha256:cc".into()),
            fixes: vec!["CVE-1".into()],
            executor: "vm".into(),
            digest: "sha256:dd".into(),
            ..e.clone()
        };
        assert_eq!(metadata(&other), m);
        for changed in [
            Entry {
                conflicts: vec!["x".into()],
                ..e.clone()
            },
            Entry {
                abi: [("os".to_string(), "debian-14".to_string())].into(),
                ..e.clone()
            },
            Entry {
                env: [("PATH".to_string(), "/x".to_string())].into(),
                ..e.clone()
            },
            Entry {
                license: "GPL-2.0".into(),
                ..e.clone()
            },
        ] {
            assert_ne!(metadata(&changed), m);
        }
    }
}
