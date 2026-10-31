//! The layer budget and the composite base (`docs/layer-budget.md`), through kiln's real
//! conversion: no namespaces needed.

mod common;

use std::time::{Duration, Instant};

use common::{base_tars as base, kiln_layers, layer, oci_base, run_base, sources, steps};
use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_store::{Digest, Store};
use potter_exec::assemble::{LayerReport, layer_plan};

/// An image of `tars` (bottom first) whose bottom `base` layers are its OCI base, with no runtime
/// steps, converted within `budget`.
fn build(store: &Store, tars: &[Vec<u8>], base: usize, budget: u32) -> (Digest, LayerReport) {
    let b = oci_base(store, &tars[..base], &["PATH=/bin"]);
    let rb = run_base(store, "x:1", &b, &[]);
    common::image(store, &rb, &tars[base..], budget, "t:1")
}

fn stack(base: &[Vec<u8>], steps: &[Vec<u8>]) -> Vec<Vec<u8>> {
    base.iter().chain(steps).cloned().collect()
}

#[test]
fn the_policy_for_several_plans() {
    // (stacked, base, run base, budget) → (composite, app composite, layers left)
    let cases = [
        ((8, 5, 5, 10), (0, 0, 8)),
        ((10, 5, 5, 10), (0, 0, 10)), // the ruby image: exactly at the default budget
        ((11, 5, 5, 10), (5, 0, 7)),  // one more step: the whole base, not kiln's minimal 2
        ((14, 5, 5, 10), (5, 0, 10)),
        ((15, 5, 5, 10), (5, 2, 10)), // the base is not enough: the two lowest steps merge too
        ((16, 5, 5, 10), (5, 3, 10)),
        ((11, 1, 1, 10), (0, 2, 10)), // a one-layer base: only the steps can merge
        ((11, 5, 5, 20), (0, 0, 11)),
        ((40, 5, 5, 32), (5, 5, 32)),
        ((130, 3, 3, 128), (3, 0, 128)),
        // Runtime steps: the OCI base merges first, then the runtime layers above it.
        ((12, 3, 5, 10), (3, 0, 10)),
        ((14, 3, 5, 10), (5, 0, 10)),
        ((15, 3, 5, 10), (5, 2, 10)),
        // Nothing above the run base, or no run base: not rebaseable, one composite as before.
        ((12, 5, 12, 10), (5, 0, 8)),
        ((4, 0, 0, 3), (2, 0, 3)),
    ];
    for ((stacked, b, rb, budget), (composite, app, left)) in cases {
        let p = layer_plan(stacked, b, rb, budget);
        assert_eq!(
            (p.composite, p.app_composite, p.layers(stacked, rb)),
            (composite, app, left),
            "{stacked} layers, base {b}, run base {rb}, budget {budget}"
        );
    }
}

#[test]
fn the_policy_always_fits_and_never_merges_across_the_run_base() {
    for budget in 3..=40 {
        for stacked in 1..=50 {
            for rb in 0..=stacked {
                for b in 0..=rb {
                    let p = layer_plan(stacked, b, rb, budget);
                    let at = format!("{stacked} layers, base {b}, run base {rb}, budget {budget}");
                    assert!(p.layers(stacked, rb) <= budget, "{at}");
                    assert_eq!(p.composite == 0 && p.app_composite == 0, stacked <= budget, "{at}");
                    assert_eq!(p.rebaseable, rb > 0 && stacked > rb, "{at}");
                    for w in [p.composite, p.app_composite] {
                        assert!(w == 0 || w >= 2, "{at}: a one-layer merge");
                    }
                    if p.rebaseable {
                        assert!(p.composite <= rb, "{at}");
                        assert!(p.app_composite <= stacked - rb, "{at}");
                        if p.composite > 0 {
                            assert!(p.composite >= b, "{at}: the whole OCI base first");
                        }
                        // Steps merge only once the run base is one layer, or cannot shrink.
                        if p.app_composite > 0 {
                            assert!(p.composite == rb || rb == 1, "{at}");
                        }
                    }
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
            run_base: 5,
            stacked: 11,
            composite: 5,
            app_composite: 0,
            kiln: 7
        }
    );
    let la = kiln_layers(&store, &a);
    assert_eq!(la.len(), 7);
    assert_eq!(la[0].1, sources(&b), "the composite is exactly the base");

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
fn past_the_base_the_lowest_steps_merge_on_their_own_side() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let b = base(5, "ruby");
    let s = steps(11, "a");
    let (d, r) = build(&store, &stack(&b, &s), 5, 10);
    assert_eq!((r.stacked, r.composite, r.app_composite, r.kiln), (16, 5, 3, 10));
    let layers = kiln_layers(&store, &d);
    assert_eq!(layers[0].1, sources(&b), "the composite is the base alone");
    assert_eq!(layers[1].1, sources(&s[..3]), "the three lowest steps");
    let kept: Vec<String> = layers[2..].iter().flat_map(|l| l.1.clone()).collect();
    assert_eq!(kept, sources(&s[3..]), "the top eight steps stay their own layers");
    let loaded = kiln_image::load(&store, &d).unwrap();
    let record = loaded.entries[0].1.config.base.clone().unwrap();
    assert_eq!(
        (record.layers, record.source_layers),
        (1, 5),
        "the run base is recorded"
    );
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
            "{what:<58} {:>4} layers (composite {}+{}) {:>7.2}s",
            r.kiln,
            r.composite,
            r.app_composite,
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
            &[oci_base(&store, tars, &[]).resolved],
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

    // Beyond the base: the lowest steps merge above it; changing one rebuilds only that merge.
    let fifteen: Vec<Vec<u8>> = (0..10).map(|i| heavy(&format!("step{i}"), 50 + i, 200, 2)).collect();
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &fifteen), 5, 10));
    report("15 layers: base merged, the 2 lowest steps merged above it", &r, t);
    let mut low = fifteen.clone();
    low[0] = heavy("step0", 5555, 200, 2);
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &low), 5, 10));
    report("15 layers, a merged step changed: only the step merge rebuilt", &r, t);
    let mut high = fifteen.clone();
    high[9] = heavy("step9", 6666, 200, 2);
    let ((_, r), t) = timed(|| build(&store, &stack(&b, &high), 5, 10));
    report("15 layers, top step changed: both merges cached", &r, t);
}
