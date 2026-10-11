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

use crate::chain::{BuildCtx, LayerRef, run_chain, run_steps};
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
    let image = &target.image;
    // The image's base: its own chain (the run base and its runtime steps), or with no exports
    // the build chain itself.
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
        layers.push(store_layer(ctx.store, file, "export")?);
        (run.env.clone(), layers)
    };
    if !image.files.is_empty() {
        let file = files_layer(&image.files, tmp(ctx)?)?;
        layers.push(store_layer(ctx.store, file, "files")?);
    }

    let config = image_config(
        &ctx.platform,
        &base_env,
        image,
        layers.iter().map(|l| l.diff_id.clone()).collect(),
    );
    let infra = |e: &dyn std::fmt::Display| BuildError::Infra(format!("target {}: {e}", target.name));
    let config_bytes = canonical_json(&config);
    let config_digest = ctx.store.put_bytes(&config_bytes).map_err(|e| infra(&e))?;
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(OCI_CONFIG, config_digest.clone(), config_bytes.len() as u64),
        layers: layers.iter().map(|l| l.descriptor.clone()).collect(),
        annotations: None,
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
        manifest_digest,
        manifest,
        config_digest,
        config,
        ref_name: None,
    };
    let output = convert_resolved(
        ctx.store,
        &[resolved],
        Some(&tags[0]),
        &ConvertOptions::default(),
        &StoredLayers,
    )
    .map_err(|e| infra(&e))?;
    let _lock = ctx.store.lock_shared().map_err(|e| infra(&e))?;
    for t in &tags {
        ctx.store.set_ref(t, &output.digest).map_err(|e| infra(&e))?;
    }
    Ok(Built {
        target: target.name.clone(),
        reference: tags[0].clone(),
        digest: output.digest,
        executed: chain.executed,
        cached: chain.cached,
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
    fn a_process_workdir_wins() {
        let mut img = image(&[("web", &["x"])]);
        img.processes.get_mut("web").unwrap().workdir = Some("/srv".into());
        let c = image_config(&Platform::host(), &BTreeMap::new(), &img, vec![])
            .config
            .unwrap();
        assert_eq!(c.working_dir.as_deref(), Some("/srv"));
    }
}
