//! `potter rebase` (spec §16): swap an image's run base for a patched one without rebuilding it.
//! The run chain the image recorded (its run base and runtime steps) is re-run on the new base,
//! and kiln's rebase stacks the image's own layers on it, or refuses with the reason.

use kiln_image::{ImageError, KilnManifest, RebaseRequest};
use kiln_oci::ImageManifest;
use kiln_store::{Digest, Store};
use potter_plan::{Base, Chain};

use crate::assemble::{ANN_MAX_LAYERS, ANN_RUN_CHAIN, RunBase, layer_plan};
use crate::chain::{BuildCtx, run_steps};
use crate::error::{BuildError, io};

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
}

fn image_error(e: ImageError) -> BuildError {
    match e {
        ImageError::RebaseRefused(refusals) => {
            BuildError::RebaseRefused(refusals.iter().map(ToString::to_string).collect())
        }
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
    Ok(Recorded {
        digest,
        base: base.manifest_digest.clone(),
        app_layers,
        run_chain,
        max_layers,
    })
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
    let chain = new_chain(&recorded.run_chain, req.base)?;
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
    let run_base =
        RunBase::new(ctx.store, &run.base_ref, &run.base, &run.env, &run.layers).map_err(BuildError::Infra)?;
    let mut out = rebase_onto(ctx.store, &recorded, &run_base, tag)?;
    out.executed = run.executed;
    out.cached = run.cached;
    Ok(out)
}
