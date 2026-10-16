//! The layer budget and the composite base (`docs/layer-budget.md`), through kiln's real
//! conversion: no namespaces needed.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_oci::media::{OCI_CONFIG, OCI_LAYER_TAR, OCI_MANIFEST};
use kiln_oci::{Descriptor, ImageManifest, Platform, ResolvedImage, canonical_json};
use kiln_store::{Digest, Store};
use potter_exec::assemble::{LayerReport, convert_within_budget, image_config, layer_plan};
use potter_plan::{Base, Image, ImageConfig};

fn plan_image() -> Image {
    Image {
        base: Base::Oci {
            reference: "x:1".into(),
            digest: None,
        },
        runtime_steps: vec![],
        export: vec![],
        files: vec![],
        config: ImageConfig::new("/app", "0:0"),
        processes: BTreeMap::new(),
        services: BTreeMap::new(),
        max_layers: None,
    }
}

/// The OCI image of `tars` (bottom first), stored as uncompressed layers, as potter assembles it.
fn resolved(store: &Store, tars: &[Vec<u8>]) -> ResolvedImage {
    let platform = Platform::parse("linux/arm64").unwrap();
    let layers: Vec<Descriptor> = tars
        .iter()
        .map(|t| Descriptor::new(OCI_LAYER_TAR, store.put_bytes(t).unwrap(), t.len() as u64))
        .collect();
    let config = image_config(
        &platform,
        &BTreeMap::new(),
        &plan_image(),
        layers.iter().map(|l| l.digest.clone()).collect(),
    );
    let config_bytes = canonical_json(&config);
    let config_digest = store.put_bytes(&config_bytes).unwrap();
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(OCI_CONFIG, config_digest.clone(), config_bytes.len() as u64),
        layers,
        annotations: None,
    };
    let manifest_digest = store.put_bytes(&canonical_json(&manifest)).unwrap();
    ResolvedImage {
        platform,
        manifest_digest,
        manifest,
        config_digest,
        config,
        ref_name: None,
    }
}

/// A small layer: `dir/` with one file whose content is `note`. `tmp/` comes from base layer 0 and
/// is never repeated, so step layers writing under it inherit it (an implicit parent).
fn layer(dir: &str, note: &str) -> Vec<u8> {
    let mut t = TarBuilder::new();
    if dir == "base0" {
        t.dir("tmp", &Opts::default().mode(0o1777));
    }
    t.dir(dir, &Opts::default().mode(0o755))
        .file(&format!("{dir}/f"), note.as_bytes(), &Opts::default());
    if dir.starts_with("step") {
        t.file(&format!("tmp/{dir}"), b"t", &Opts::default());
    }
    t.finish()
}

fn base(n: usize, note: &str) -> Vec<Vec<u8>> {
    (0..n).map(|i| layer(&format!("base{i}"), note)).collect()
}

fn steps(n: usize, note: &str) -> Vec<Vec<u8>> {
    (0..n).map(|i| layer(&format!("step{i}"), note)).collect()
}

/// The kiln image's layers: (erofs digest, the OCI layers it was built from).
fn kiln_layers(store: &Store, digest: &Digest) -> Vec<(Digest, Vec<String>)> {
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

fn build(store: &Store, tars: &[Vec<u8>], base: usize, budget: u32) -> (Digest, LayerReport) {
    convert_within_budget(store, resolved(store, tars), "t:1", base, budget).unwrap()
}

fn stack(base: &[Vec<u8>], steps: &[Vec<u8>]) -> Vec<Vec<u8>> {
    base.iter().chain(steps).cloned().collect()
}

#[test]
fn the_policy_for_several_plans() {
    // (stacked, base, budget) → (composite, layers left)
    let cases = [
        ((8, 5, 10), (0, 8)),
        ((10, 5, 10), (0, 10)), // the ruby image: exactly at the default budget
        ((11, 5, 10), (5, 7)),  // one more step: the whole base, not kiln's minimal 2
        ((14, 5, 10), (5, 10)),
        ((15, 5, 10), (6, 10)), // the base is not enough: the lowest step joins it
        ((16, 5, 10), (7, 10)),
        ((11, 1, 10), (2, 10)), // a one-layer base: as kiln alone would
        ((11, 5, 20), (0, 11)),
        ((40, 5, 32), (9, 32)),
        ((4, 0, 3), (2, 3)),
        ((130, 3, 128), (3, 128)),
    ];
    for ((stacked, base, budget), (composite, left)) in cases {
        let p = layer_plan(stacked, base, budget);
        assert_eq!(
            (p.composite, p.layers(stacked)),
            (composite, left),
            "{stacked} layers, base {base}, budget {budget}"
        );
    }
}

#[test]
fn the_policy_always_fits_and_agrees_with_kilns_squash_rule() {
    for budget in 3..=40 {
        for stacked in 1..=70 {
            for base in 0..=stacked {
                let p = layer_plan(stacked, base, budget);
                let at = format!("{stacked} layers, base {base}, budget {budget}");
                assert!(p.layers(stacked) <= budget, "{at}");
                assert_eq!(p.composite == 0, stacked <= budget, "{at}");
                if p.composite > 0 {
                    assert!(p.composite >= base.min(stacked) && p.composite >= 2, "{at}");
                    // kiln squashes the bottom `n - max + 1` of `n` layers when `n > max`.
                    assert!(stacked > p.kiln_max_layers, "{at}");
                    assert_eq!(stacked - p.kiln_max_layers + 1, p.composite, "{at}");
                } else {
                    assert!(stacked <= p.kiln_max_layers, "{at}");
                }
            }
        }
    }
}

#[test]
fn over_budget_the_whole_base_becomes_one_shared_composite() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let b = base(5, "ruby");
    // 5 + 6 = 11 layers over a budget of 10: the five base layers merge into one.
    let (a, ra) = build(&store, &stack(&b, &steps(6, "a")), 5, 10);
    assert_eq!(
        ra,
        LayerReport {
            max_layers: 10,
            base: 5,
            stacked: 11,
            composite: 5,
            kiln: 7
        }
    );
    let la = kiln_layers(&store, &a);
    assert_eq!(la.len(), 7);
    let base_sources: Vec<String> = resolved(&store, &b)
        .manifest
        .layers
        .iter()
        .map(|l| l.digest.to_string())
        .collect();
    assert_eq!(la[0].1, base_sources, "the composite is exactly the base");

    // Another image on the same base, with more steps (9 + 5 = 14 layers): same composite.
    let (other, ro) = build(&store, &stack(&b, &steps(9, "other")), 5, 10);
    assert_eq!((ro.composite, ro.kiln), (5, 10));
    assert_eq!(
        kiln_layers(&store, &other)[0].0,
        la[0].0,
        "shared by every image on the base"
    );

    // At the default budget the ruby-like image (10 layers) is not merged at all.
    let (_, r) = build(&store, &stack(&b, &steps(5, "a")), 5, 10);
    assert_eq!((r.composite, r.kiln), (0, 10));
    // A raised budget keeps every layer.
    let (_, r) = build(&store, &stack(&b, &steps(6, "a")), 5, 20);
    assert_eq!((r.composite, r.kiln), (0, 11));
}

#[test]
fn the_composite_extends_upward_only_when_the_base_is_not_enough() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let b = base(5, "ruby");
    let s = steps(11, "a");
    let (d, r) = build(&store, &stack(&b, &s), 5, 10);
    assert_eq!((r.stacked, r.composite, r.kiln), (16, 7, 10));
    let layers = kiln_layers(&store, &d);
    assert_eq!(layers[0].1.len(), 7, "the base and the two lowest steps");
    let top_sources: Vec<String> = resolved(&store, &s[2..])
        .manifest
        .layers
        .iter()
        .map(|l| l.digest.to_string())
        .collect();
    let kept: Vec<String> = layers[1..].iter().flat_map(|l| l.1.clone()).collect();
    assert_eq!(kept, top_sources, "the top nine steps stay their own layers");
}

#[test]
fn a_change_above_the_composite_keeps_it_and_everything_below() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let b = base(5, "ruby");
    let mut s = steps(6, "a");
    let (a, _) = build(&store, &stack(&b, &s), 5, 10);
    s[5] = layer("step5", "changed");
    let (c, _) = build(&store, &stack(&b, &s), 5, 10);
    let (la, lc) = (kiln_layers(&store, &a), kiln_layers(&store, &c));
    assert_eq!(
        la[..6],
        lc[..6],
        "the composite and the unchanged steps are the same blobs"
    );
    assert_ne!(la[6], lc[6]);
}

#[test]
fn a_change_inside_the_composite_rebuilds_only_the_composite() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut b = base(5, "ruby");
    let s = steps(6, "a");
    let (a, _) = build(&store, &stack(&b, &s), 5, 10);
    b[2] = layer("base2", "patched");
    let (c, _) = build(&store, &stack(&b, &s), 5, 10);
    let (la, lc) = (kiln_layers(&store, &a), kiln_layers(&store, &c));
    assert_ne!(la[0].0, lc[0].0, "a new composite");
    assert_eq!(la[1..], lc[1..], "the step layers above it are unchanged blobs");
}

#[test]
fn the_same_inputs_give_the_same_image_in_any_store() {
    let tars = stack(&base(5, "ruby"), &steps(8, "a"));
    let digests: Vec<Digest> = (0..2)
        .map(|_| {
            let home = tempfile::tempdir().unwrap();
            let store = Store::open(home.path()).unwrap();
            build(&store, &tars, 5, 10).0
        })
        .collect();
    assert_eq!(digests[0], digests[1]);
}

// ---------------------------------------------------------------------------------------------
// Timings (`docs/layer-budget.md`): `cargo test --release -p potter-exec --test layer_budget
// -- --ignored --nocapture timings`.

/// Deterministic, incompressible-looking bytes.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A layer of `files` 4 KiB files and one `big_mib` MiB file under `dir/`.
fn heavy(dir: &str, seed: u64, files: usize, big_mib: usize) -> Vec<u8> {
    let mut t = TarBuilder::new();
    t.dir(dir, &Opts::default().mode(0o755));
    for i in 0..files {
        t.file(
            &format!("{dir}/f{i}"),
            &noise(seed * 100_000 + i as u64, 4096),
            &Opts::default(),
        );
    }
    t.file(&format!("{dir}/big"), &noise(seed, big_mib << 20), &Opts::default());
    t.finish()
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = f();
    (out, start.elapsed())
}

#[test]
#[ignore = "a measurement: run explicitly, in release"]
fn timings() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    // A ruby-slim-like base: five layers, about 190 MiB; steps of about 2.8 MiB each.
    let b: Vec<Vec<u8>> = (0..5).map(|i| heavy(&format!("base{i}"), i + 1, 2000, 30)).collect();
    let s: Vec<Vec<u8>> = (0..6).map(|i| heavy(&format!("step{i}"), 50 + i, 200, 2)).collect();
    let mib = |tars: &[Vec<u8>]| tars.iter().map(Vec::len).sum::<usize>() >> 20;
    println!("base {} MiB in 5 layers, steps {} MiB in 6 layers", mib(&b), mib(&s));
    let report = |what: &str, r: &LayerReport, t: Duration| {
        println!(
            "{what:<58} {:>4} layers (composite {}) {:>7.2}s",
            r.kiln,
            r.composite,
            t.as_secs_f64()
        );
    };
    let image = stack(&b, &s);
    let (_, t) = timed(|| build(&store, &image, 5, 20));
    println!(
        "{:<58} {:>7.2}s",
        "cold: every layer converted (budget 20, no composite)",
        t.as_secs_f64()
    );
    let ((_, r), t) = timed(|| build(&store, &image, 5, 10));
    report("composite made (budget 10): layers cached, base merged", &r, t);
    let ((_, r), t) = timed(|| build(&store, &image, 5, 10));
    report("unchanged rebuild", &r, t);

    let mut top = s.clone();
    top[5] = heavy("step5", 999, 200, 2);
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &top), 5, 10));
    report("top step changed: only it converts, composite cached", &r, t);

    let mut more = s.clone();
    more.push(heavy("step6", 77, 200, 2));
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &more), 5, 10));
    report("one more step (12 layers): composite still cached", &r, t);

    let mut patched = b.clone();
    patched[3] = heavy("base3", 4444, 2000, 30);
    let ((_, r), t) = timed(|| build(&store, &stack(&patched, &s), 5, 10));
    report("base layer changed: it converts, composite rebuilt", &r, t);

    // kiln's minimal squash for comparison: the merged layers depend on the layer count.
    let kiln_only = |tars: &[Vec<u8>]| {
        let opts = kiln_image::ConvertOptions {
            max_layers: 10,
            ..Default::default()
        };
        let out = kiln_image::convert_resolved(
            &store,
            &[resolved(&store, tars)],
            None,
            &opts,
            &kiln_image::StoredLayers,
        )
        .unwrap();
        (out.images[0].squashed, out.images[0].layers.len())
    };
    let ((k, n), t) = timed(|| kiln_only(&image));
    println!(
        "{:<58} {n:>4} layers (squashed {k}) {:>7.2}s",
        "kiln's minimal squash, 11 layers",
        t.as_secs_f64()
    );
    let ((k, n), t) = timed(|| kiln_only(&stack(&b, &more)));
    println!(
        "{:<58} {n:>4} layers (squashed {k}) {:>7.2}s",
        "kiln's minimal squash, 12 layers: a new merge",
        t.as_secs_f64()
    );

    // Beyond the base: the lowest step joins the composite; changing it rebuilds the composite.
    let fifteen: Vec<Vec<u8>> = (0..10).map(|i| heavy(&format!("step{i}"), 50 + i, 200, 2)).collect();
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &fifteen), 5, 10));
    report("15 layers: base + lowest step merged", &r, t);
    let mut low = fifteen.clone();
    low[0] = heavy("step0", 5555, 200, 2);
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &low), 5, 10));
    report("15 layers, merged step changed: composite rebuilt", &r, t);
    let mut high = fifteen.clone();
    high[9] = heavy("step9", 6666, 200, 2);
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &high), 5, 10));
    report("15 layers, top step changed: composite cached", &r, t);
}
