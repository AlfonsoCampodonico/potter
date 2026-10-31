//! Images assembled as potter assembles them, from in-memory layer tars: no namespaces needed.
#![allow(dead_code)]

use std::collections::BTreeMap;

use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_oci::media::{OCI_CONFIG, OCI_LAYER_TAR, OCI_MANIFEST};
use kiln_oci::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, Platform, ResolvedImage, RootFs, canonical_json,
};
use kiln_store::{Digest, Store};
use potter_exec::assemble::{ANN_MAX_LAYERS, ANN_RUN_CHAIN, LayerReport, RunBase, convert_within_budget, image_config};
use potter_exec::base::BaseImage;
use potter_exec::chain::LayerRef;
use potter_plan::{Base, Chain, Image};

pub fn platform() -> Platform {
    Platform::parse("linux/arm64").unwrap()
}

pub fn plan_image() -> Image {
    Image {
        base: Base::Oci {
            reference: "x:1".into(),
            digest: None,
        },
        runtime_steps: vec![],
        export: vec![],
        files: vec![],
        config: potter_plan::ImageConfig::new("/app", "0:0"),
        processes: BTreeMap::new(),
        services: BTreeMap::new(),
        max_layers: None,
    }
}

fn store_tars(store: &Store, tars: &[Vec<u8>]) -> Vec<LayerRef> {
    tars.iter()
        .map(|t| {
            let digest = store.put_bytes(t).unwrap();
            LayerRef {
                descriptor: Descriptor::new(OCI_LAYER_TAR, digest.clone(), t.len() as u64),
                diff_id: digest,
            }
        })
        .collect()
}

fn commit(
    store: &Store,
    config: ImageConfig,
    layers: &[LayerRef],
    annotations: Option<BTreeMap<String, String>>,
) -> ResolvedImage {
    let config_bytes = canonical_json(&config);
    let config_digest = store.put_bytes(&config_bytes).unwrap();
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(OCI_CONFIG, config_digest.clone(), config_bytes.len() as u64),
        layers: layers.iter().map(|l| l.descriptor.clone()).collect(),
        annotations,
    };
    let manifest_digest = store.put_bytes(&canonical_json(&manifest)).unwrap();
    ResolvedImage {
        platform: platform(),
        manifest_digest,
        manifest,
        config_digest,
        config,
        ref_name: None,
    }
}

/// An OCI base of `tars` with `env`, as a registry would serve it.
pub fn oci_base(store: &Store, tars: &[Vec<u8>], env: &[&str]) -> BaseImage {
    let layers = store_tars(store, tars);
    let config = ImageConfig {
        architecture: "arm64".into(),
        os: "linux".into(),
        variant: None,
        config: Some(ContainerConfig {
            env: Some(env.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }),
        rootfs: RootFs {
            fs_type: "layers".into(),
            diff_ids: layers.iter().map(|l| l.diff_id.clone()).collect(),
        },
    };
    BaseImage {
        resolved: commit(store, config, &layers, None),
        repository: "lib/base".into(),
    }
}

/// The run base `name`: `base`'s layers, then `runtime` (runtime step layers).
pub fn run_base(store: &Store, name: &str, base: &BaseImage, runtime: &[Vec<u8>]) -> RunBase {
    let mut layers: Vec<LayerRef> = base
        .layers()
        .iter()
        .zip(base.diff_ids())
        .map(|(d, id)| LayerRef {
            descriptor: d.clone(),
            diff_id: id.clone(),
        })
        .collect();
    layers.extend(store_tars(store, runtime));
    RunBase::new(store, name, base, &base.env(), &layers).unwrap()
}

/// The image potter assembles: `run_base`'s layers, then `app`, with the annotations a rebase
/// reads, converted within `budget` and tagged `reference`.
pub fn image(
    store: &Store,
    run_base: &RunBase,
    app: &[Vec<u8>],
    budget: u32,
    reference: &str,
) -> (Digest, LayerReport) {
    let mut layers: Vec<LayerRef> = run_base
        .resolved
        .manifest
        .layers
        .iter()
        .zip(&run_base.resolved.config.rootfs.diff_ids)
        .map(|(d, id)| LayerRef {
            descriptor: d.clone(),
            diff_id: id.clone(),
        })
        .collect();
    layers.extend(store_tars(store, app));
    let config = image_config(
        &platform(),
        &potter_exec::base::env_of(&run_base.resolved.config),
        &run_base.labels(),
        &plan_image(),
        layers.iter().map(|l| l.diff_id.clone()).collect(),
    );
    let chain = Chain {
        base: Base::Oci {
            reference: run_base.name.clone(),
            digest: None,
        },
        steps: vec![],
    };
    let annotations = BTreeMap::from([
        (ANN_RUN_CHAIN.to_string(), potter_plan::canonical_json(&chain).unwrap()),
        (ANN_MAX_LAYERS.to_string(), budget.to_string()),
    ]);
    let resolved = commit(store, config, &layers, Some(annotations));
    let (digest, report) = convert_within_budget(store, resolved, reference, run_base, budget).unwrap();
    store.set_ref(reference, &digest).unwrap();
    (digest, report)
}

/// A small layer: `dir/` with one file whose content is `note`. `tmp/` comes from base layer 0 and
/// is never repeated, so step layers writing under it inherit it (an implicit parent).
pub fn layer(dir: &str, note: &str) -> Vec<u8> {
    let mut t = TarBuilder::new();
    if dir == "base0" {
        t.dir("tmp", &Opts::default().mode(0o1777));
        t.dir("etc", &Opts::default().mode(0o755));
        t.file("etc/os-release", b"ID=debian\nVERSION_ID=\"13\"\n", &Opts::default());
    }
    t.dir(dir, &Opts::default().mode(0o755))
        .file(&format!("{dir}/f"), note.as_bytes(), &Opts::default());
    if dir.starts_with("step") {
        t.file(&format!("tmp/{dir}"), b"t", &Opts::default());
    }
    t.finish()
}

pub fn base_tars(n: usize, note: &str) -> Vec<Vec<u8>> {
    (0..n).map(|i| layer(&format!("base{i}"), note)).collect()
}

pub fn steps(n: usize, note: &str) -> Vec<Vec<u8>> {
    (0..n).map(|i| layer(&format!("step{i}"), note)).collect()
}

/// The kiln image's app layers: (erofs digest, the OCI layers it was built from).
pub fn kiln_layers(store: &Store, digest: &Digest) -> Vec<(Digest, Vec<String>)> {
    let loaded = kiln_image::load(store, digest).unwrap();
    loaded.entries[0]
        .1
        .manifest
        .layers
        .iter()
        .map(|l| {
            let sources = l
                .annotation(kiln_image::types::ANN_SOURCE_DIGESTS)
                .unwrap()
                .split(',')
                .map(str::to_string)
                .collect();
            (l.digest.clone(), sources)
        })
        .collect()
}

/// The source digests of `tars`, as kiln records them.
pub fn sources(tars: &[Vec<u8>]) -> Vec<String> {
    tars.iter().map(|t| Digest::of(t).to_string()).collect()
}
