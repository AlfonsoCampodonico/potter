//! Image assembly (spec §9.6): the run base, the export layer and the generated files, an
//! OCI config from the plan's image, then kiln's conversion and the image's ref.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Seek;
use std::path::Path;

use kiln_image::{BaseImage as KilnBase, ConvertOptions, StoredLayers, convert_resolved};
use kiln_oci::media::{OCI_CONFIG, OCI_LAYER_TAR, OCI_MANIFEST};
use kiln_oci::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, Platform, ResolvedImage, RootFs, canonical_json,
};
use kiln_store::{Digest, Store};
use potter_plan::{Base, Chain, GeneratedFile, Image, Process, Target};

use crate::chain::{BuildCtx, LayerRef, Progress, StepRecord, run_chain, run_steps};
use crate::error::{BuildError, io};
use crate::merge::{ExportSpec, export_layer};
use crate::tarball::{Meta, TarWriter, epoch};

/// A built target.
#[derive(Debug, Clone)]
pub struct Built {
    pub target: String,
    /// The first ref it was tagged with.
    pub reference: String,
    pub digest: Digest,
    pub executed: usize,
    pub cached: usize,
    /// The layers the build added: the run base's chain, then the build chain.
    pub steps: Vec<StepRecord>,
    /// The OCI bases it resolved: the build base first.
    pub bases: Vec<BaseRecord>,
    /// The OCI image manifest's digest (`digest` is the kiln image's).
    pub manifest_digest: Digest,
    /// How its layers were fitted into the budget.
    pub layers: LayerReport,
}

/// The image's layers against its budget (`docs/layer-budget.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerReport {
    /// The budget it was built with.
    pub max_layers: u32,
    /// The OCI base's layers at the bottom of the stack.
    pub base: usize,
    /// The run base's layers: the OCI base's, then the runtime steps' (the part a rebase swaps).
    pub run_base: usize,
    /// The layers potter stacked: the run base's, then one per layer-producing step, export and files.
    pub stacked: usize,
    /// The bottom run base layers merged into one composite (0: none).
    pub composite: usize,
    /// The bottom layers above the run base merged into one (0: none).
    pub app_composite: usize,
    /// The kiln image's layers.
    pub kiln: usize,
}

/// How `stacked` layers fit into `budget`: the bottom `run_base` of them are the run base, and
/// the bottom `base` of those the OCI base's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPlan {
    /// The bottom run base layers to merge into one composite (0: none).
    pub composite: usize,
    /// The bottom layers above the run base to merge into one (0: none).
    pub app_composite: usize,
    /// Whether the image records its run base, so that `potter rebase` can swap it: the run base
    /// has layers and something sits above it.
    pub rebaseable: bool,
}

impl LayerPlan {
    /// The run base's layers left: the composite counts as one.
    pub fn base_layers(&self, run_base: usize) -> usize {
        merged(run_base, self.composite)
    }

    /// The layers left.
    pub fn layers(&self, stacked: usize, run_base: usize) -> usize {
        if self.rebaseable {
            self.base_layers(run_base) + merged(stacked - run_base, self.app_composite)
        } else {
            merged(stacked, self.composite)
        }
    }
}

fn merged(n: usize, composite: usize) -> usize {
    if composite == 0 { n } else { n - composite + 1 }
}

/// The composite policy: nothing is merged while the layers fit. Otherwise the whole OCI base is
/// merged first, so the composite depends on the base alone and every image on that base shares
/// it (kiln caches it by the ordered erofs digests it merges); only when that is not enough are
/// the run base's next-lowest layers merged into it. A merge never crosses the run base's top, so
/// the image stays rebaseable: past it, the lowest layers above the run base merge into one of
/// their own. The top layers, the ones rebuilt most, are always kept.
pub fn layer_plan(stacked: usize, base: usize, run_base: usize, budget: usize) -> LayerPlan {
    let budget = budget.max(1);
    let run_base = run_base.min(stacked);
    let base = base.min(run_base);
    let rebaseable = run_base > 0 && stacked > run_base;
    if stacked <= budget {
        return LayerPlan {
            composite: 0,
            app_composite: 0,
            rebaseable,
        };
    }
    let needed = stacked - budget + 1;
    if !rebaseable {
        return LayerPlan {
            composite: needed.max(base).min(stacked),
            app_composite: 0,
            rebaseable,
        };
    }
    // A single layer merges into nothing.
    let composite = Some(needed.max(base).min(run_base)).filter(|&w| w >= 2).unwrap_or(0);
    let left = merged(run_base, composite);
    let app = stacked - run_base;
    // kiln leaves the app at least one layer, even when the base fills the budget.
    let app_budget = budget.saturating_sub(left).max(1);
    LayerPlan {
        composite,
        app_composite: if app > app_budget { app - app_budget + 1 } else { 0 },
        rebaseable,
    }
}

/// The run base of an image: the bottom of its stack, as an OCI image of its own (spec §16).
#[derive(Debug, Clone)]
pub struct RunBase {
    pub resolved: ResolvedImage,
    /// How many of its layers are the OCI base's; the rest are the runtime steps'.
    pub base: usize,
    /// The name recorded in the image's base record (informational).
    pub name: String,
}

impl RunBase {
    /// `layers` (the OCI base's, then the runtime steps') under `from`'s config with `env`.
    pub fn new(
        store: &Store,
        name: &str,
        from: &crate::base::BaseImage,
        env: &BTreeMap<String, String>,
        layers: &[LayerRef],
    ) -> Result<Self, String> {
        let mut config = from.resolved.config.clone();
        let mut container = config.config.take().unwrap_or_default();
        container.env = Some(env.iter().map(|(k, v)| format!("{k}={v}")).collect());
        config.config = Some(container);
        config.rootfs = RootFs {
            fs_type: "layers".into(),
            diff_ids: layers.iter().map(|l| l.diff_id.clone()).collect(),
        };
        let config_bytes = canonical_json(&config);
        let config_digest = store.put_bytes(&config_bytes).map_err(|e| e.to_string())?;
        let manifest = ImageManifest {
            schema_version: 2,
            media_type: Some(OCI_MANIFEST.into()),
            artifact_type: None,
            config: Descriptor::new(OCI_CONFIG, config_digest.clone(), config_bytes.len() as u64),
            layers: layers.iter().map(|l| l.descriptor.clone()).collect(),
            annotations: None,
        };
        let manifest_digest = store.put_bytes(&canonical_json(&manifest)).map_err(|e| e.to_string())?;
        Ok(RunBase {
            resolved: ResolvedImage {
                platform: from.resolved.platform.clone(),
                manifest_digest,
                manifest,
                config_digest,
                config,
                ref_name: None,
            },
            base: from.layers().len(),
            name: name.to_string(),
        })
    }

    pub fn layers(&self) -> usize {
        self.resolved.manifest.layers.len()
    }

    /// The run base's labels, which the image inherits.
    pub fn labels(&self) -> BTreeMap<String, String> {
        self.resolved
            .config
            .config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .unwrap_or_default()
    }

    /// Converts the run base into a kiln image (untagged: `kiln gc` keeps it for every image that
    /// records it), its bottom `composite` layers merged into one.
    pub fn convert(&self, store: &Store, composite: usize) -> Result<Digest, String> {
        let opts = ConvertOptions {
            max_layers: merged(self.layers(), composite),
            ..ConvertOptions::default()
        };
        let output = convert_resolved(store, std::slice::from_ref(&self.resolved), None, &opts, &StoredLayers)
            .map_err(|e| e.to_string())?;
        let converted = &output.images[0];
        if converted.squashed != composite {
            return Err(format!(
                "kiln merged {} of the run base's {} layers; the layer budget expected {composite}",
                converted.squashed,
                self.layers()
            ));
        }
        Ok(output.digest)
    }
}

/// Converts the OCI image `resolved`, whose bottom layers are `run_base`'s, into a kiln image
/// within `max_layers`, following [`layer_plan`]. The image records its run base, converted on
/// its own, so that `potter rebase` can swap it.
pub fn convert_within_budget(
    store: &Store,
    resolved: ResolvedImage,
    reference: &str,
    run_base: &RunBase,
    max_layers: u32,
) -> Result<(Digest, LayerReport), String> {
    let stacked = resolved.manifest.layers.len();
    let side = run_base.layers();
    if resolved.manifest.layers.get(..side) != Some(&run_base.resolved.manifest.layers[..]) {
        return Err("the image's layers do not start with its run base's".into());
    }
    let plan = layer_plan(stacked, run_base.base, side, max_layers as usize);
    let (opts, expect) = if plan.rebaseable {
        let base = run_base.convert(store, plan.composite)?;
        let image = kiln_image::load(store, &base).map_err(|e| e.to_string())?;
        let opts = ConvertOptions {
            max_layers: max_layers as usize,
            base: Some(KilnBase {
                name: run_base.name.clone(),
                image,
            }),
            ..ConvertOptions::default()
        };
        (opts, plan.app_composite)
    } else {
        let opts = ConvertOptions {
            max_layers: merged(stacked, plan.composite),
            ..ConvertOptions::default()
        };
        (opts, plan.composite)
    };
    let output =
        convert_resolved(store, &[resolved], Some(reference), &opts, &StoredLayers).map_err(|e| e.to_string())?;
    let converted = &output.images[0];
    // A kiln whose squash rule changed would silently break the policy: refuse instead.
    if converted.squashed != expect || converted.layers.len() != plan.layers(stacked, side) {
        return Err(format!(
            "kiln merged {} of {stacked} layers into {}; the layer budget expected {expect} into {}",
            converted.squashed,
            converted.layers.len(),
            plan.layers(stacked, side)
        ));
    }
    Ok((
        output.digest,
        LayerReport {
            max_layers,
            base: run_base.base,
            run_base: side,
            stacked,
            composite: plan.composite,
            app_composite: plan.app_composite,
            kiln: converted.layers.len(),
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRecord {
    pub reference: String,
    /// The manifest digest it resolved to.
    pub digest: Digest,
}

/// The process the image runs by default: `web`, else the only one; none when several
/// processes and no `web` leave the choice to the platform.
pub fn default_process(processes: &BTreeMap<String, Process>) -> Option<&Process> {
    processes
        .get("web")
        .or_else(|| (processes.len() == 1).then(|| processes.values().next()).flatten())
}

/// The OCI config of `image` on `platform`, over a base with `base_env` and `base_labels`. The
/// labels are inherited, as an image's are, so that a rebase can tell them from the image's own.
pub fn image_config(
    platform: &Platform,
    base_env: &BTreeMap<String, String>,
    base_labels: &BTreeMap<String, String>,
    image: &Image,
    diff_ids: Vec<Digest>,
) -> ImageConfig {
    let process = default_process(&image.processes);
    let mut env = base_env.clone();
    env.extend(image.config.env.clone());
    if let Some(p) = process {
        env.extend(p.env.clone());
    }
    ImageConfig {
        architecture: platform.architecture.clone(),
        os: platform.os.clone(),
        variant: platform.variant.clone(),
        config: Some(ContainerConfig {
            user: Some(image.config.user.clone()),
            env: Some(env.into_iter().map(|(k, v)| format!("{k}={v}")).collect()),
            entrypoint: process.map(|p| p.command.clone()),
            cmd: None,
            working_dir: Some(
                process
                    .and_then(|p| p.workdir.clone())
                    .unwrap_or_else(|| image.config.workdir.clone()),
            ),
            stop_signal: image.config.stop_signal.clone(),
            labels: Some(base_labels.clone()),
        }),
        rootfs: RootFs {
            fs_type: "layers".into(),
            diff_ids,
        },
    }
}

/// Where a rebase finds the run chain to re-run on a new base: the run base and its runtime steps,
/// as canonical JSON.
pub const ANN_RUN_CHAIN: &str = "dev.potter.run_chain";
/// The layer budget the image was built with, which a rebase keeps.
pub const ANN_MAX_LAYERS: &str = "dev.potter.max_layers";

/// The image manifest's annotations (spec §5.1, §18): the plan's labels, the listen port, every
/// process as a JSON argv, and the canonical hash of the target that produced it.
pub fn manifest_annotations(image: &Image, target_hash: &str) -> BTreeMap<String, String> {
    let mut a = image.config.labels.clone();
    if let Some(port) = image.config.port {
        a.insert("dev.potter.port".into(), port.to_string());
    }
    for (name, p) in &image.processes {
        a.insert(
            format!("dev.potter.process.{name}"),
            serde_json::to_string(&p.command).expect("a string list serialises"),
        );
    }
    a.insert("dev.potter.target".into(), target_hash.to_string());
    a
}

/// Stores `file` (a tar written from its start) as an uncompressed layer.
fn store_layer(store: &Store, mut file: std::fs::File, what: &str) -> Result<LayerRef, BuildError> {
    file.rewind().map_err(io(format!("reading the {what} layer")))?;
    let (digest, size) = store
        .put_reader(&mut file)
        .map_err(|e| BuildError::Infra(format!("storing the {what} layer: {e}")))?;
    Ok(LayerRef {
        descriptor: Descriptor::new(OCI_LAYER_TAR, digest.clone(), size),
        diff_id: digest,
    })
}

/// The layer of the image's generated files: root-owned, with their parents.
fn files_layer(files: &[GeneratedFile], out: std::fs::File) -> Result<std::fs::File, BuildError> {
    let mtime = epoch();
    let mut paths: BTreeMap<String, Option<&GeneratedFile>> = BTreeMap::new();
    for f in files {
        let rel = f.path.trim_start_matches('/').to_string();
        let parents: BTreeSet<String> = rel.match_indices('/').map(|(i, _)| rel[..i].to_string()).collect();
        for p in parents {
            paths.entry(p).or_insert(None);
        }
        if paths.insert(rel, Some(f)).flatten().is_some() {
            return Err(BuildError::Plan(format!("the image generates {} twice", f.path)));
        }
    }
    let mut tar = TarWriter::new(out);
    for (path, file) in &paths {
        match file {
            None => tar.dir(path, &Meta::root(0o755, mtime)),
            Some(f) => tar.file(
                path,
                &Meta::root(f.mode & 0o7777, mtime),
                f.content.len() as u64,
                &mut f.content.as_bytes(),
            ),
        }
        .map_err(io(format!("writing /{path}")))?;
    }
    tar.finish().map_err(io("finishing the files layer"))
}

fn tmp(ctx: &BuildCtx) -> Result<std::fs::File, BuildError> {
    std::fs::create_dir_all(ctx.home.runs()).map_err(io("creating the runs directory"))?;
    tempfile::tempfile_in(ctx.home.runs()).map_err(io("creating a layer file"))
}

/// Builds `target` and commits its image to the kiln store under `tags` (default
/// `potter/<target>:latest`).
pub fn build_target(
    ctx: &mut BuildCtx,
    target: &Target,
    repo_root: &Path,
    tags: &[String],
) -> Result<Built, BuildError> {
    let chain = run_chain(ctx, target, repo_root)?;
    ctx.deps.insert(target.name.clone(), chain.layers.clone());
    let image = &target.image;
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("target {}: {e}", target.name));
    // The image's base: its own chain (the run base and its runtime steps), or with no exports
    // the build chain itself, whose run base is the OCI base it was lowered onto.
    let mut steps = Vec::new();
    let mut bases = Vec::new();
    let (run_base, run_chain, mut layers): (RunBase, Chain, Vec<LayerRef>) = if image.export.is_empty() {
        if image.base != target.build.base || !image.runtime_steps.is_empty() {
            return Err(BuildError::Plan(format!(
                "target {}: an image with its own base or runtime steps needs `export` paths",
                target.name
            )));
        }
        let n = chain.base.layers().len();
        let run_base = RunBase::new(
            ctx.store,
            &chain.base_ref,
            &chain.base,
            &chain.base.env(),
            &chain.layers[..n],
        )
        .map_err(|e| infra(&e))?;
        let run_chain = Chain {
            base: Base::Oci {
                reference: chain.base_ref.clone(),
                digest: None,
            },
            steps: Vec::new(),
        };
        (run_base, run_chain, chain.layers.clone())
    } else {
        let run_chain = Chain {
            base: image.base.clone(),
            steps: image.runtime_steps.clone(),
        };
        let run = run_steps(
            ctx,
            &format!("{}:run", target.name),
            &run_chain,
            &repo_root.join(&target.root),
            crate::stopgap::Role::Run,
        )?;
        let mut layers = run.layers.clone();
        let run_base =
            RunBase::new(ctx.store, &run.base_ref, &run.base, &run.env, &run.layers).map_err(|e| infra(&e))?;
        steps.extend(run.records.iter().cloned());
        bases.push(BaseRecord {
            reference: run.base_ref.clone(),
            digest: run.base.resolved.manifest_digest.clone(),
        });
        let store = ctx.store;
        let open: Vec<crate::merge::LayerOpen> = chain
            .layers
            .iter()
            .map(|l| Box::new(move || crate::base::layer_reader(store, &l.descriptor)) as crate::merge::LayerOpen)
            .collect();
        let spill = ctx.home.runs();
        let spec = ExportSpec {
            exports: &image.export,
            epoch: epoch(),
            spill_dir: &spill,
        };
        let (file, _) = export_layer(&open, &spec, tmp(ctx)?)?;
        let export = store_layer(ctx.store, file, "export")?;
        // Re-packed on the host from the build chain's layers.
        steps.push(StepRecord {
            chain: target.name.clone(),
            step: "potter:export".into(),
            key: None,
            layer: export.diff_id.clone(),
            class: None,
        });
        layers.push(export);
        (run_base, run_chain, layers)
    };
    if !image.files.is_empty() {
        let file = files_layer(&image.files, tmp(ctx)?)?;
        let files = store_layer(ctx.store, file, "files")?;
        steps.push(StepRecord {
            chain: target.name.clone(),
            step: "potter:files".into(),
            key: None,
            layer: files.diff_id.clone(),
            class: None,
        });
        layers.push(files);
    }

    let base_env = crate::base::env_of(&run_base.resolved.config);
    let config = image_config(
        &ctx.platform,
        &base_env,
        &run_base.labels(),
        image,
        layers.iter().map(|l| l.diff_id.clone()).collect(),
    );
    steps.extend(chain.records.iter().cloned());
    if !bases.iter().any(|b| b.digest == chain.base.resolved.manifest_digest) {
        bases.insert(
            0,
            BaseRecord {
                reference: chain.base_ref.clone(),
                digest: chain.base.resolved.manifest_digest.clone(),
            },
        );
    }
    let config_bytes = canonical_json(&config);
    let config_digest = ctx.store.put_bytes(&config_bytes).map_err(|e| infra(&e))?;
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(OCI_CONFIG, config_digest.clone(), config_bytes.len() as u64),
        layers: layers.iter().map(|l| l.descriptor.clone()).collect(),
        annotations: Some({
            let mut a = manifest_annotations(image, &potter_plan::hash_of(target).map_err(|e| infra(&e))?.hex());
            a.insert(
                ANN_RUN_CHAIN.into(),
                potter_plan::canonical_json(&run_chain).map_err(|e| infra(&e))?,
            );
            a.insert(ANN_MAX_LAYERS.into(), image.layer_budget().to_string());
            a
        }),
    };
    let manifest_digest = ctx.store.put_bytes(&canonical_json(&manifest)).map_err(|e| infra(&e))?;
    let tags: Vec<String> = if tags.is_empty() {
        vec![format!("potter/{}:latest", target.name)]
    } else {
        tags.to_vec()
    };
    for t in &tags {
        kiln_store::check_ref_name(t).map_err(|e| BuildError::Plan(format!("the tag {t}: {e}")))?;
    }
    let resolved = ResolvedImage {
        platform: ctx.platform.clone(),
        manifest_digest: manifest_digest.clone(),
        manifest,
        config_digest,
        config,
        ref_name: None,
    };
    let max_layers = image.layer_budget();
    if let Some(w) = potter_plan::budget::device_warning(max_layers) {
        (ctx.progress)(Progress::Warning(format!("target {}: {w}", target.name)));
    }
    let (digest, layer_report) =
        convert_within_budget(ctx.store, resolved, &tags[0], &run_base, max_layers).map_err(|e| infra(&e))?;
    let _lock = ctx.store.lock_shared().map_err(|e| infra(&e))?;
    for t in &tags {
        ctx.store.set_ref(t, &digest).map_err(|e| infra(&e))?;
    }
    Ok(Built {
        target: target.name.clone(),
        reference: tags[0].clone(),
        digest,
        executed: chain.executed,
        cached: chain.cached,
        steps,
        bases,
        manifest_digest,
        layers: layer_report,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kiln_oci::Platform;
    use kiln_store::Digest;
    use potter_plan::{Base, Image, ImageConfig, Process};

    use super::*;

    fn image(processes: &[(&str, &[&str])]) -> Image {
        let mut config = ImageConfig::new("/app", "1000:1000");
        config.env.insert("B".into(), "image".into());
        config.env.insert("PATH".into(), "/usr/local/bin:/bin".into());
        config.stop_signal = Some("SIGQUIT".into());
        Image {
            base: Base::Oci {
                reference: "x:1".into(),
                digest: None,
            },
            runtime_steps: vec![],
            export: vec![],
            files: vec![],
            config,
            processes: processes
                .iter()
                .map(|(n, c)| {
                    let mut p = Process::new(c);
                    p.env.insert("C".into(), (*n).into());
                    (n.to_string(), p)
                })
                .collect(),
            services: BTreeMap::new(),
            max_layers: None,
        }
    }

    #[test]
    fn the_config_merges_env_and_runs_the_default_process() {
        let base_env: BTreeMap<String, String> = [("A", "base"), ("B", "base"), ("PATH", "/bin")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
        let ids = vec![Digest::of(b"a"), Digest::of(b"b")];
        let c = image_config(
            &Platform::parse("linux/arm64").unwrap(),
            &base_env,
            &[("org.opencontainers.image.version", "13")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
            &image(&[("worker", &["w"]), ("web", &["/app/run", "--port", "8080"])]),
            ids.clone(),
        );
        assert_eq!((c.os.as_str(), c.architecture.as_str()), ("linux", "arm64"));
        assert_eq!(c.rootfs.diff_ids, ids);
        let cc = c.config.unwrap();
        assert_eq!(cc.entrypoint.unwrap(), ["/app/run", "--port", "8080"]);
        assert_eq!(
            cc.env.unwrap(),
            ["A=base", "B=image", "C=web", "PATH=/usr/local/bin:/bin"]
        );
        assert_eq!(cc.working_dir.as_deref(), Some("/app"));
        assert_eq!(cc.user.as_deref(), Some("1000:1000"));
        assert_eq!(cc.stop_signal.as_deref(), Some("SIGQUIT"));
        assert_eq!(
            cc.labels.unwrap()["org.opencontainers.image.version"],
            "13",
            "the base's labels are inherited"
        );
    }

    #[test]
    fn a_lone_process_is_the_default_and_none_means_no_entrypoint() {
        let env = BTreeMap::new();
        let p = Platform::host();
        let lone = image_config(&p, &env, &env, &image(&[("worker", &["w"])]), vec![])
            .config
            .unwrap();
        assert_eq!(lone.entrypoint.unwrap(), ["w"]);
        let none = image_config(&p, &env, &env, &image(&[]), vec![]).config.unwrap();
        assert_eq!(none.entrypoint, None);
        let two = image_config(&p, &env, &env, &image(&[("a", &["a"]), ("b", &["b"])]), vec![])
            .config
            .unwrap();
        assert_eq!(
            two.entrypoint, None,
            "no web and several processes: the platform chooses"
        );
    }

    #[test]
    fn the_manifest_records_port_processes_labels_and_the_target_hash() {
        let mut img = image(&[("web", &["/app/run", "--port", "8080"]), ("worker", &["w"])]);
        img.config.port = Some(8080);
        img.config.labels.insert("team".into(), "a".into());
        let a = manifest_annotations(&img, "ab12");
        assert_eq!(a["dev.potter.port"], "8080");
        assert_eq!(a["dev.potter.process.web"], r#"["/app/run","--port","8080"]"#);
        assert_eq!(a["dev.potter.process.worker"], r#"["w"]"#);
        assert_eq!(a["dev.potter.target"], "ab12");
        assert_eq!(a["team"], "a");
        let none = manifest_annotations(&image(&[]), "ab12");
        assert!(!none.contains_key("dev.potter.port"));
    }

    #[test]
    fn a_process_workdir_wins() {
        let mut img = image(&[("web", &["x"])]);
        img.processes.get_mut("web").unwrap().workdir = Some("/srv".into());
        let c = image_config(&Platform::host(), &BTreeMap::new(), &BTreeMap::new(), &img, vec![])
            .config
            .unwrap();
        assert_eq!(c.working_dir.as_deref(), Some("/srv"));
    }
}
