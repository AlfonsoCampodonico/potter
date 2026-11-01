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
use crate::merge::{ExportSpec, LayerOpen, export_layer, read_file};
use crate::sandboxed::SandboxCtx;
use crate::sonames::{Caps, provided};
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
        let layer = extract(parts.store, &state.layers[above..], work.path(), recipe.name())?;
        if recipe.name() == OS_MIN {
            os_abi = Some(read_os_abi(&layer.tar)?);
        }
        let [os, libc] = os_abi.clone().expect("os-min builds first");
        let l = &recipe.layer;
        let entry = Entry {
            name: l.name.clone(),
            line: l.line.clone(),
            version: l.version.clone(),
            arch,
            kind: l.kind,
            digest: layer.descriptor.digest.to_string(),
            diff_id: layer.diff_id.to_string(),
            size: layer.descriptor.size,
            media_type: layer.descriptor.media_type.clone(),
            abi: l.abi.clone().into_iter().chain([os, libc]).collect(),
            requires: recipe.requires(),
            conflicts: l.conflicts.clone(),
            env: l.env.clone(),
            provides_sonames: layer.sonames,
            executor: EXECUTOR.into(),
            source: l.source.clone(),
            license: l.license.clone(),
            supersedes: None,
            fixes: l.fixes.clone(),
        };
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
    let blobs = layout_blobs(parts, &assigned.index, &built, previous.as_ref())?;
    let digest = write_signed(req.out, &assigned.index, req.sign_key, &blobs)?;
    let mut layers = Vec::with_capacity(built.len());
    for (b, (_, outcome)) in built.iter().zip(assigned.outcomes) {
        let entry = assigned
            .index
            .by_digest(&b.entry.digest)
            .cloned()
            .expect("every built layer has an entry");
        // A kept entry keeps its metadata too: a layer.toml change that leaves the bytes alone
        // waits for the next change that does not.
        let unchanged = |e: &Entry| Entry {
            supersedes: None,
            fixes: Vec::new(),
            ..e.clone()
        };
        if outcome == Outcome::Kept && unchanged(&entry) != unchanged(&b.entry) {
            (parts.progress)(Progress::Warning(format!(
                "{} keeps its entry: its layer is unchanged, so what its layer.toml changes is not published",
                b.entry.name
            )));
        }
        layers.push((entry, outcome));
    }
    Ok(Report {
        index: assigned.index,
        digest,
        layers,
    })
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

/// A built layer: the flattened tar, its zstd blob in the store, and what it holds.
struct Extracted {
    /// The uncompressed tar, kept for reading `os-min`'s ABI.
    tar: tempfile::NamedTempFile,
    descriptor: Descriptor,
    diff_id: Digest,
    files: BTreeSet<String>,
    sonames: Vec<String>,
}

/// `layers` (bottom first) flattened into one deterministic layer, without the recipe's own
/// `/app`, stored zstd-compressed.
fn extract(store: &Store, layers: &[LayerRef], work: &Path, name: &str) -> Result<Extracted, BuildError> {
    if layers.is_empty() {
        return Err(BuildError::Plan(format!(
            "the recipe {name} added nothing to its layer"
        )));
    }
    let open: Vec<LayerOpen> = layers
        .iter()
        .map(|l| Box::new(move || layer_reader(store, &l.descriptor)) as LayerOpen)
        .collect();
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
        tar,
        descriptor: Descriptor::new(OCI_LAYER_ZSTD, digest, size),
        diff_id,
        files,
        sonames: sonames.into_iter().collect(),
    })
}

/// `os` and `libc` from the `os-min` layer `tar`, read on the host.
fn read_os_abi(tar: &tempfile::NamedTempFile) -> Result<[(String, String); 2], BuildError> {
    let path = tar.path().to_path_buf();
    let open: Vec<LayerOpen> = vec![Box::new(move || {
        let f = std::fs::File::open(&path).map_err(io("reading the os-min layer"))?;
        Ok(Box::new(f) as Box<dyn io::Read + Send>)
    })];
    let text = |p: &str, cap: u64| -> Result<Option<String>, BuildError> {
        Ok(read_file(&open, p, cap)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
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
/// holds every current layer, then pushes the layers and the index to `to`.
pub fn publish(layout: &Path, to: &Reference, auth: DockerConfig) -> Result<Verified, BuildError> {
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
    for e in v.index.current() {
        fetch_layer(&v, e, &store, auth.clone())?;
    }
    potter_catalog::push(layout, to, auth)?;
    Ok(v)
}
