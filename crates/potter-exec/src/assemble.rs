//! Image assembly (spec §9.6): the run base, the export layer and the generated files, an
//! OCI config from the plan's image, then kiln's conversion and the image's ref.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Seek;
use std::path::Path;

use kiln_image::{ConvertOptions, StoredLayers, convert_resolved};
use kiln_oci::media::{OCI_CONFIG, OCI_LAYER_TAR, OCI_MANIFEST};
use kiln_oci::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, Platform, ResolvedImage, RootFs, canonical_json,
};
use kiln_store::{Digest, Store};
use potter_plan::{Chain, GeneratedFile, Image, Process, Target};

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
    /// The layers potter stacked: the base's, then one per layer-producing step, export and files.
    pub stacked: usize,
    /// The bottom layers merged into one composite (0: none).
    pub composite: usize,
    /// The kiln image's layers.
    pub kiln: usize,
}

/// How `stacked` layers, the bottom `base` of them the OCI base's, fit into `budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPlan {
    /// The bottom layers to merge into one composite (0: none).
    pub composite: usize,
    /// The `max_layers` that makes kiln merge exactly those: kiln squashes the bottom
    /// `n - max_layers + 1` of `n` layers when `n > max_layers` (spec §6.4 of kiln).
    pub kiln_max_layers: usize,
}

impl LayerPlan {
    /// The layers left: the composite counts as one.
    pub fn layers(&self, stacked: usize) -> usize {
        if self.composite == 0 {
            stacked
        } else {
            stacked - self.composite + 1
        }
    }
}

/// The composite policy: nothing is merged while the layers fit. Otherwise the whole base is
/// merged first, so the composite depends on the base alone and every image on that base shares
/// it (kiln caches it by the ordered erofs digests it merges); only when that is not enough are
/// the next-lowest step layers merged into it, bottom-up. The top layers, the ones rebuilt most,
/// are always kept.
pub fn layer_plan(stacked: usize, base: usize, budget: usize) -> LayerPlan {
    let budget = budget.max(1);
    if stacked <= budget {
        return LayerPlan {
            composite: 0,
            kiln_max_layers: budget,
        };
    }
    let needed = stacked - budget + 1;
    let composite = needed.max(base).min(stacked);
    LayerPlan {
        composite,
        kiln_max_layers: stacked - composite + 1,
    }
}

/// Converts the OCI image `resolved` into a kiln image within `max_layers`, following
/// [`layer_plan`]; `base` is how many of its layers are the OCI base's.
pub fn convert_within_budget(
    store: &Store,
    resolved: ResolvedImage,
    reference: &str,
    base: usize,
    max_layers: u32,
) -> Result<(Digest, LayerReport), String> {
    let stacked = resolved.manifest.layers.len();
    let plan = layer_plan(stacked, base, max_layers as usize);
    let opts = ConvertOptions {
        max_layers: plan.kiln_max_layers,
        ..ConvertOptions::default()
    };
    let output =
        convert_resolved(store, &[resolved], Some(reference), &opts, &StoredLayers).map_err(|e| e.to_string())?;
    let converted = &output.images[0];
    // A kiln whose squash rule changed would silently break the policy: refuse instead.
    if converted.squashed != plan.composite || converted.layers.len() != plan.layers(stacked) {
        return Err(format!(
            "kiln merged {} of {stacked} layers into {}; the layer budget expected {} into {}",
            converted.squashed,
            converted.layers.len(),
            plan.composite,
            plan.layers(stacked)
        ));
    }
    Ok((
        output.digest,
        LayerReport {
            max_layers,
            base,
            stacked,
            composite: plan.composite,
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

/// The OCI config of `image` on `platform`, over a base with `base_env`.
pub fn image_config(
    platform: &Platform,
    base_env: &BTreeMap<String, String>,
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
        }),
        rootfs: RootFs {
            fs_type: "layers".into(),
            diff_ids,
        },
    }
}

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
    // The image's base: its own chain (the run base and its runtime steps), or with no exports
    // the build chain itself.
    let mut steps = Vec::new();
    let mut bases = Vec::new();
    // The OCI base's layers, at the bottom of the image: the composite merges them first.
    let mut base_layers = chain.base.layers().len();
    let (base_env, mut layers): (BTreeMap<String, String>, Vec<LayerRef>) = if image.export.is_empty() {
        if image.base != target.build.base || !image.runtime_steps.is_empty() {
            return Err(BuildError::Plan(format!(
                "target {}: an image with its own base or runtime steps needs `export` paths",
                target.name
            )));
        }
        (chain.base.env(), chain.layers.clone())
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
        base_layers = run.base.layers().len();
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
        (run.env.clone(), layers)
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

    let config = image_config(
        &ctx.platform,
        &base_env,
        image,
        layers.iter().map(|l| l.diff_id.clone()).collect(),
    );
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("target {}: {e}", target.name));
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
        annotations: Some(manifest_annotations(
            image,
            &potter_plan::hash_of(target).map_err(|e| infra(&e))?.hex(),
        )),
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
        convert_within_budget(ctx.store, resolved, &tags[0], base_layers, max_layers).map_err(|e| infra(&e))?;
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
    }

    #[test]
    fn a_lone_process_is_the_default_and_none_means_no_entrypoint() {
        let env = BTreeMap::new();
        let p = Platform::host();
        let lone = image_config(&p, &env, &image(&[("worker", &["w"])]), vec![])
            .config
            .unwrap();
        assert_eq!(lone.entrypoint.unwrap(), ["w"]);
        let none = image_config(&p, &env, &image(&[]), vec![]).config.unwrap();
        assert_eq!(none.entrypoint, None);
        let two = image_config(&p, &env, &image(&[("a", &["a"]), ("b", &["b"])]), vec![])
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
        let c = image_config(&Platform::host(), &BTreeMap::new(), &img, vec![])
            .config
            .unwrap();
        assert_eq!(c.working_dir.as_deref(), Some("/srv"));
    }
}
