//! `potter rebase` (spec §16): swap an image's run base for a patched one without rebuilding it.
//! The run chain the image recorded (its run base and runtime steps) is re-run on the new base,
//! and kiln's rebase stacks the image's own layers on it, or refuses with the reason. A catalog
//! image's run base is resolved again from its recorded constraints, behind the line, ABI and
//! soname gates of design §8, which add to kiln's gates and never replace them.

use std::collections::{BTreeMap, BTreeSet};

use kiln_image::{ImageError, KilnManifest, RebaseRequest};
use kiln_oci::{Descriptor, ImageManifest, Platform};
use kiln_store::{Digest, Store};
use potter_catalog::{Entry, Index, Resolution};
use potter_plan::{Base, CatalogRef, Chain};

use crate::assemble::{
    ANN_CATALOG_CONSTRAINTS, ANN_CATALOG_INDEX, ANN_CATALOG_LAYERS, ANN_CHAIN, ANN_MAX_LAYERS, ANN_RUN_CHAIN,
    ANN_SONAMES_NEEDED, RunBase, layer_plan,
};
use crate::chain::{BuildCtx, CatalogCtx, catalog_arch, needs_catalog, run_steps};
use crate::error::{BuildError, RefusedBy, io};
use crate::sonames::Caps;

/// What to rebase.
#[derive(Debug, Clone, Default)]
pub struct Request<'a> {
    /// The image: a name or a digest in the kiln store.
    pub image: &'a str,
    /// Another OCI reference for the run base. Without it, the recorded reference is resolved
    /// again, which picks up a patched build of the same tag.
    pub base: Option<&'a str>,
    /// The name to tag the result with (default: `image`, when it is a name).
    pub tag: Option<&'a str>,
    /// The catalog index the run base resolves against, when pinned: the loaded catalog must be it.
    pub catalog_index: Option<&'a str>,
    /// Check the catalog gates and report what would change, writing nothing (catalog images only).
    pub dry_run: bool,
}

/// A potter image as a rebase reads it.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub digest: Digest,
    /// The kiln image of its current run base.
    pub base: Digest,
    /// The source layers above the run base.
    pub app_layers: usize,
    /// Its run base and runtime steps.
    pub run_chain: Chain,
    pub max_layers: u32,
    /// What a catalog image recorded about its run base; `None` for an image on an OCI base, or
    /// on a catalog base lowered through the stopgap before P3.
    pub catalog: Option<RecordedCatalog>,
}

/// A catalog image's run base, as it recorded it (design §7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedCatalog {
    /// The catalog layers under its run base now, bottom first: the ones its run base records,
    /// else (an image never rebased) the ones it was built on.
    pub layers: Vec<String>,
    /// One `name@line` per layer its run base asked for: what a rebase resolves again.
    pub constraints: Vec<String>,
    /// The index it was built against.
    pub index: String,
    /// The layers above the run base a rebase keeps (`dev.potter.chain`).
    pub chain: Vec<Descriptor>,
    /// The sonames its ELF files need; `None` when the image records none.
    pub sonames_needed: Option<BTreeSet<String>>,
}

/// One catalog gate and why it refused (no reasons: it passed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub gate: &'static str,
    pub refusals: Vec<String>,
}

/// The catalog gates' verdicts on a new resolution, and the layers it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogCheck {
    /// (old, new) catalog entry digests, bottom first.
    pub replaced: Vec<(String, String)>,
    /// `line`, `abi` and `sonames`, in that order.
    pub verdicts: Vec<Verdict>,
}

impl CatalogCheck {
    /// The refusal of every gate that refused, if any did.
    pub fn refused(&self) -> Option<BuildError> {
        let reasons: Vec<String> = self.verdicts.iter().flat_map(|v| v.refusals.clone()).collect();
        (!reasons.is_empty()).then_some(BuildError::RebaseRefused {
            reasons,
            by: RefusedBy::Catalog,
        })
    }
}

/// One rebased image.
#[derive(Debug, Clone)]
pub struct Rebased {
    pub digest: Digest,
    pub tag: Option<String>,
    /// The run base it replaced (a kiln image).
    pub old_base: Digest,
    /// The run base it is on now (a kiln image), and the OCI base under it.
    pub new_base: Digest,
    pub base_ref: String,
    pub base_digest: Digest,
    /// Already on the new base: nothing was written.
    pub unchanged: bool,
    /// The app layers kept byte for byte, and those re-derived for the new base.
    pub reused: usize,
    pub changed: usize,
    /// Runtime steps run, and taken from the cache.
    pub executed: usize,
    pub cached: usize,
    /// (old, new) catalog entry digests a catalog image's run base swapped.
    pub replaced: Vec<(String, String)>,
    /// The catalog gates' verdicts (none for an image on an OCI or stopgap base).
    pub verdicts: Vec<Verdict>,
    /// A dry run: nothing was written, and only `digest`, `replaced` and `verdicts` say anything.
    pub dry_run: bool,
}

fn image_error(e: ImageError) -> BuildError {
    match e {
        ImageError::RebaseRefused(refusals) => BuildError::RebaseRefused {
            reasons: refusals.iter().map(ToString::to_string).collect(),
            by: RefusedBy::Kiln,
        },
        e => BuildError::Infra(e.to_string()),
    }
}

/// Reads what `image` recorded at build time.
pub fn recorded(store: &Store, image: &str) -> Result<Recorded, BuildError> {
    let digest = kiln_image::resolve_name(store, image).map_err(|e| BuildError::Plan(format!("{image}: {e}")))?;
    let loaded = kiln_image::load(store, &digest).map_err(|e| BuildError::Plan(format!("{image}: {e}")))?;
    if loaded.is_index || loaded.entries.len() != 1 {
        return Err(BuildError::Plan(format!(
            "{image} is a multi-platform image; potter rebase takes the one-platform images potter builds"
        )));
    }
    let m: &KilnManifest = &loaded.entries[0].1;
    let Some(base) = &m.config.base else {
        return Err(BuildError::Plan(format!(
            "{image} records no run base: it was built before potter rebase, or nothing sits above its base; rebuild it"
        )));
    };
    let source = &m.config.source.manifest_digest;
    let not_potter = |why: &str| BuildError::Plan(format!("{image} is not a potter image ({why})"));
    let bytes = store
        .read_metadata(source)
        .map_err(|_| not_potter("its source manifest is gone"))?;
    let manifest: ImageManifest = serde_json::from_slice(&bytes).map_err(|_| not_potter("its source manifest"))?;
    let annotation = |key: &str| {
        manifest
            .annotations
            .as_ref()
            .and_then(|a| a.get(key))
            .ok_or_else(|| not_potter(&format!("no {key}")))
    };
    let run_chain: Chain =
        serde_json::from_str(annotation(ANN_RUN_CHAIN)?).map_err(|e| not_potter(&format!("{ANN_RUN_CHAIN}: {e}")))?;
    let max_layers = annotation(ANN_MAX_LAYERS)?
        .parse()
        .map_err(|e| not_potter(&format!("{ANN_MAX_LAYERS}: {e}")))?;
    let app_layers = manifest
        .layers
        .len()
        .checked_sub(base.source_layers)
        .filter(|&n| n > 0)
        .ok_or_else(|| not_potter("its base record does not match its source manifest"))?;
    let catalog = match manifest.annotations.as_ref().and_then(|a| a.get(ANN_CATALOG_LAYERS)) {
        None => None,
        Some(built_on) => {
            let list = |key: &str, value: &str| -> Result<Vec<String>, BuildError> {
                serde_json::from_str(value).map_err(|e| not_potter(&format!("{key}: {e}")))
            };
            let chain = list(ANN_CHAIN, annotation(ANN_CHAIN)?)?
                .iter()
                .map(|d| {
                    manifest
                        .layers
                        .iter()
                        .find(|l| l.digest.to_string() == *d)
                        .cloned()
                        .ok_or_else(|| not_potter(&format!("{ANN_CHAIN} names {d}, which it does not have")))
                })
                .collect::<Result<_, _>>()?;
            let sonames_needed = match manifest.annotations.as_ref().and_then(|a| a.get(ANN_SONAMES_NEEDED)) {
                Some(v) => Some(list(ANN_SONAMES_NEEDED, v)?.into_iter().collect()),
                None => None,
            };
            let layers = match run_base_layers(store, &base.manifest_digest) {
                Some(layers) => layers,
                None => list(ANN_CATALOG_LAYERS, built_on)?,
            };
            Some(RecordedCatalog {
                layers,
                constraints: list(ANN_CATALOG_CONSTRAINTS, annotation(ANN_CATALOG_CONSTRAINTS)?)?,
                index: annotation(ANN_CATALOG_INDEX)?.clone(),
                chain,
                sonames_needed,
            })
        }
    };
    Ok(Recorded {
        digest,
        base: base.manifest_digest.clone(),
        app_layers,
        run_chain,
        max_layers,
        catalog,
    })
}

/// The catalog layers the kiln image `base` (a run base) records on its source manifest.
fn run_base_layers(store: &Store, base: &Digest) -> Option<Vec<String>> {
    let loaded = kiln_image::load(store, base).ok()?;
    let source = &loaded.entries.first()?.1.config.source.manifest_digest;
    let manifest: ImageManifest = serde_json::from_slice(&store.read_metadata(source).ok()?).ok()?;
    serde_json::from_str(manifest.annotations?.get(ANN_CATALOG_LAYERS)?).ok()
}

/// The refs a recorded constraint list resolves from: `name@line` each.
fn constraint_refs(constraints: &[String]) -> Result<Vec<CatalogRef>, BuildError> {
    constraints
        .iter()
        .map(|c| match c.split_once('@') {
            Some((name, line)) if !name.is_empty() && !line.is_empty() => Ok(CatalogRef::new(name, Some(line))),
            _ => Err(BuildError::Plan(format!(
                "{ANN_CATALOG_CONSTRAINTS} holds {c:?}, which is not name@line"
            ))),
        })
        .collect()
}

/// The run chain a rebase re-runs for `recorded`:
/// - a catalog image: its recorded constraints, resolved against the catalog again;
/// - an image whose non-PHP catalog base was lowered through the stopgap before P3 (it records no
///   catalog annotations): that base lowered through the stopgap again, as design §8 keeps it;
/// - otherwise [`new_chain`].
pub fn rebase_chain(recorded: &Recorded, base: Option<&str>) -> Result<Chain, BuildError> {
    let chain = &recorded.run_chain;
    let Base::Catalog { layers } = &chain.base else {
        return new_chain(chain, base);
    };
    if base.is_some() {
        return new_chain(chain, base);
    }
    Ok(match &recorded.catalog {
        Some(c) => Chain {
            base: Base::Catalog {
                layers: constraint_refs(&c.constraints)?,
            },
            steps: chain.steps.clone(),
        },
        None if needs_catalog(&chain.base) => {
            let lowered = crate::stopgap::lower(layers, crate::stopgap::Role::Run)?;
            Chain {
                base: Base::Oci {
                    reference: lowered.reference,
                    digest: None,
                },
                steps: lowered
                    .steps
                    .into_iter()
                    .chain(chain.steps.iter().map(crate::stopgap::lower_step))
                    .collect(),
            }
        }
        None => chain.clone(),
    })
}

/// The sonames the layers a rebase keeps provide themselves (`.so` files the app bundles), which
/// the new run base need not.
pub fn kept_sonames(store: &Store, recorded: &RecordedCatalog) -> Result<BTreeSet<String>, BuildError> {
    let mut caps = Caps::default();
    let mut out = BTreeSet::new();
    for l in &recorded.chain {
        let mut reader = crate::base::layer_reader(store, l)?;
        out.extend(crate::sonames::provided(&mut reader, &mut caps)?);
        // A partial list only makes the soname gate stricter.
        if caps.exceeded().is_some() {
            break;
        }
    }
    Ok(out)
}

/// The catalog gates (design §8) on resolving `recorded`'s constraints to `new` against `index`:
/// - `line`: every layer keeps its name and line;
/// - `abi`: every layer keeps every ABI value;
/// - `sonames`: every soname the app needs is provided by a new layer or by a layer the rebase
///   keeps (`kept`), and the image recorded what it needs.
pub fn check_catalog(
    index: &Index,
    recorded: &RecordedCatalog,
    new: &Resolution,
    kept: &BTreeSet<String>,
) -> CatalogCheck {
    let mut line = Vec::new();
    let mut abi = Vec::new();
    let mut replaced = Vec::new();
    let by_name: BTreeMap<&str, &Entry> = new.entries.iter().map(|e| (e.name.as_str(), e)).collect();
    let mut old_names = BTreeSet::new();
    for d in &recorded.layers {
        let Some(old) = index.by_digest(d) else {
            line.push(format!(
                "the catalog index {} does not list {d}, so its line and ABI cannot be compared",
                new.index_digest
            ));
            continue;
        };
        old_names.insert(old.name.as_str());
        let Some(n) = by_name.get(old.name.as_str()) else {
            line.push(format!("{} {} is no longer in the run base", old.name, old.line));
            continue;
        };
        if n.line != old.line {
            line.push(format!(
                "{}: the line changes from {} to {}",
                old.name, old.line, n.line
            ));
        }
        let keys: BTreeSet<&String> = old.abi.keys().chain(n.abi.keys()).collect();
        for k in keys {
            let (o, w) = (old.abi.get(k), n.abi.get(k));
            if o != w {
                let show = |v: Option<&String>| v.map_or("none".to_string(), Clone::clone);
                abi.push(format!("{}: abi {k} changes from {} to {}", old.name, show(o), show(w)));
            }
        }
        if n.digest != old.digest {
            replaced.push((old.digest.clone(), n.digest.clone()));
        }
    }
    for n in &new.entries {
        if !old_names.is_empty() && !old_names.contains(n.name.as_str()) {
            line.push(format!("{} {} joins the run base", n.name, n.line));
        }
    }
    let sonames = match &recorded.sonames_needed {
        None => vec![format!(
            "the image records no {ANN_SONAMES_NEEDED} (its ELF files were not all read when it was built), so the new run base cannot be checked against them"
        )],
        Some(needed) => {
            let provided: BTreeSet<&str> = new
                .entries
                .iter()
                .flat_map(|e| e.provides_sonames.iter().map(String::as_str))
                .chain(kept.iter().map(String::as_str))
                .collect();
            let missing: Vec<&str> = needed
                .iter()
                .map(String::as_str)
                .filter(|s| !provided.contains(s))
                .collect();
            if missing.is_empty() {
                vec![]
            } else {
                vec![format!(
                    "the app needs {}, which no layer of the new run base provides",
                    missing.join(", ")
                )]
            }
        }
    };
    CatalogCheck {
        replaced,
        verdicts: vec![
            Verdict {
                gate: "line",
                refusals: line,
            },
            Verdict {
                gate: "abi",
                refusals: abi,
            },
            Verdict {
                gate: "sonames",
                refusals: sonames,
            },
        ],
    }
}

/// Resolves `recorded`'s constraints against `catalog` for `platform` and runs the catalog gates.
pub fn check(
    store: &Store,
    catalog: &CatalogCtx,
    recorded: &RecordedCatalog,
    platform: &Platform,
) -> Result<(Resolution, CatalogCheck), BuildError> {
    let refs = constraint_refs(&recorded.constraints)?;
    let new = potter_catalog::resolve(&catalog.verified, &refs, catalog_arch(platform)?)?;
    let kept = kept_sonames(store, recorded)?;
    let check = check_catalog(&catalog.verified.index, recorded, &new, &kept);
    Ok((new, check))
}

/// The run chain to re-run: the recorded one on `base` when given, else on its own reference
/// resolved again (never its pinned digest, which would find the same base).
pub fn new_chain(recorded: &Chain, base: Option<&str>) -> Result<Chain, BuildError> {
    let mut chain = recorded.clone();
    match (&mut chain.base, base) {
        (Base::Oci { reference, digest }, b) => {
            if let Some(b) = b {
                *reference = b.to_string();
            }
            *digest = None;
        }
        (_, Some(_)) => {
            return Err(BuildError::Plan(
                "--base needs an image on an OCI base; this one is on a catalog base, which is resolved again".into(),
            ));
        }
        (_, None) => {}
    }
    Ok(chain)
}

/// Stacks `recorded`'s own layers on `run_base` with kiln's rebase, as `tag`.
pub fn rebase_onto(
    store: &Store,
    recorded: &Recorded,
    run_base: &RunBase,
    tag: Option<&str>,
) -> Result<Rebased, BuildError> {
    let side = run_base.layers();
    let plan = layer_plan(
        side + recorded.app_layers,
        run_base.base,
        side,
        recorded.max_layers as usize,
    );
    let new_base = run_base.convert(store, plan.composite).map_err(BuildError::Infra)?;
    let mut out = Rebased {
        digest: recorded.digest.clone(),
        tag: tag.map(str::to_string),
        old_base: recorded.base.clone(),
        new_base: new_base.clone(),
        base_ref: run_base.name.clone(),
        base_digest: run_base.resolved.manifest_digest.clone(),
        unchanged: new_base == recorded.base,
        reused: recorded.app_layers,
        changed: 0,
        executed: 0,
        cached: 0,
        replaced: Vec::new(),
        verdicts: Vec::new(),
        dry_run: false,
    };
    if out.unchanged {
        if let Some(t) = tag {
            store
                .set_ref(t, &recorded.digest)
                .map_err(|e| BuildError::Infra(e.to_string()))?;
        }
        return Ok(out);
    }
    let (old, new, image) = (
        recorded.base.to_string(),
        new_base.to_string(),
        recorded.digest.to_string(),
    );
    let r = kiln_image::rebase(
        store,
        &RebaseRequest {
            old: &old,
            new: &new,
            image: &image,
            tag,
            max_layers: Some(recorded.max_layers as usize),
        },
        &kiln_image::ConvertOptions::default(),
    )
    .map_err(image_error)?;
    let app = &r.images[0].app;
    out.digest = r.digest;
    out.reused = app.iter().filter(|l| l.reused).count();
    out.changed = app.len() - out.reused;
    Ok(out)
}

/// Rebases `req.image` onto a new run base: its run chain re-run (with `ctx`'s executor and
/// cache), then [`rebase_onto`].
pub fn rebase(ctx: &mut BuildCtx, req: &Request) -> Result<Rebased, BuildError> {
    let recorded = recorded(ctx.store, req.image)?;
    let chain = rebase_chain(&recorded, req.base)?;
    let catalog = match &recorded.catalog {
        Some(c) => {
            let catalog = ctx
                .catalog
                .ok_or_else(|| BuildError::Infra("the catalog was not loaded".into()))?;
            if let Some(pin) = req.catalog_index.filter(|p| *p != catalog.verified.digest) {
                return Err(BuildError::Infra(format!(
                    "the catalog loaded is the index {}, not the pinned {pin}",
                    catalog.verified.digest
                )));
            }
            Some(check(ctx.store, catalog, c, &ctx.platform)?)
        }
        None if req.dry_run => {
            return Err(BuildError::Plan(format!(
                "{}: --dry-run checks images on a catalog base, and this one records none",
                req.image
            )));
        }
        None => None,
    };
    let (replaced, verdicts) = match catalog {
        Some((new, check)) => {
            if req.dry_run {
                return Ok(Rebased {
                    digest: recorded.digest.clone(),
                    tag: None,
                    old_base: recorded.base.clone(),
                    new_base: recorded.base.clone(),
                    base_ref: format!("potter-catalog@{}", new.index_digest),
                    base_digest: recorded.base.clone(),
                    unchanged: check.replaced.is_empty(),
                    reused: recorded.app_layers,
                    changed: 0,
                    executed: 0,
                    cached: 0,
                    replaced: check.replaced,
                    verdicts: check.verdicts,
                    dry_run: true,
                });
            }
            if let Some(refused) = check.refused() {
                return Err(refused);
            }
            (check.replaced, check.verdicts)
        }
        None => (Vec::new(), Vec::new()),
    };
    let tag = match req.tag {
        Some(t) => Some(t),
        // A digest names no tag to move.
        None if Digest::parse(req.image).is_err() => Some(req.image),
        None => None,
    };
    if let Some(t) = tag {
        kiln_store::check_ref_name(t).map_err(|e| BuildError::Plan(format!("the tag {t}: {e}")))?;
    }
    // The run chain has no inputs: its steps see an empty source root.
    std::fs::create_dir_all(ctx.home.runs()).map_err(io("creating the runs directory"))?;
    let empty = tempfile::tempdir_in(ctx.home.runs()).map_err(io("creating an empty source root"))?;
    let run = run_steps(ctx, "rebase", &chain, empty.path(), crate::stopgap::Role::Run)?;
    let run_base = RunBase::of(ctx.store, &run).map_err(BuildError::Infra)?;
    let mut out = rebase_onto(ctx.store, &recorded, &run_base, tag)?;
    out.executed = run.executed;
    out.cached = run.cached;
    if !out.unchanged {
        out.replaced = replaced;
    }
    out.verdicts = verdicts;
    Ok(out)
}
