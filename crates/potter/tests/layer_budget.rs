//! `potter build` with a layer budget (`docs/layer-budget.md`), end to end through the namespace
//! executor and kiln's conversion.
#![cfg(target_os = "linux")]

mod support;

use std::path::Path;

use assert_cmd::Command;
use kiln_erofs::testtar::{Opts, TarBuilder};
use support::Ctx;

/// A three-layer base on kiln's in-process registry: the busybox root, then two small layers.
fn serve_base() -> (kiln_registry::testregistry::TestRegistry, tempfile::TempDir, String) {
    use kiln_oci::testlayout::{LayoutBuilder, TestLayer};
    let dir = tempfile::tempdir().unwrap();
    let mut layout = LayoutBuilder::new(dir.path());
    let config = kiln_oci::ContainerConfig {
        env: Some(vec!["PATH=/bin".into()]),
        ..Default::default()
    };
    let extra = |n: &str| {
        TarBuilder::new()
            .file(&format!("etc/{n}"), n.as_bytes(), &Opts::default())
            .finish()
    };
    let layers = [support::busybox_root(), extra("one"), extra("two")].map(TestLayer::tar);
    let image = layout.image(&kiln_oci::Platform::host(), &layers, config);
    layout.add(image, Some("1"));
    let path = layout.finish();
    let reg = kiln_registry::testregistry::TestRegistry::serve_layout(&path, Default::default());
    let reference = format!("{}/lib/base3:1", reg.host());
    (reg, dir, reference)
}

/// An explicit plan whose image is its build chain (`export = []`): the base's 3 layers, the
/// `source` inputs layer, then one layer per step: 3 + 1 + `steps.len()`.
fn project(base: &str, steps: &[&str], max_layers: Option<u32>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_toml(dir.path(), base, steps, max_layers);
    dir
}

fn write_toml(dir: &Path, base: &str, steps: &[&str], max_layers: Option<u32>) {
    let mut toml = format!(
        "[target.app]\nprovider = \"none\"\nexport = []\n\n[target.app.base]\noci = \"{base}\"\n\n\
         [target.app.process.web]\ncmd = [\"/bin/cat\", \"/app/s1\"]\n"
    );
    for (i, s) in steps.iter().enumerate() {
        toml.push_str(&format!(
            "\n[target.app.steps.s{}]\nrun = \"echo {s} > /app/s{}\"\n",
            i + 1,
            i + 1
        ));
    }
    if let Some(n) = max_layers {
        toml.push_str(&format!("\n[target.app.image]\nmax_layers = {n}\n"));
    }
    std::fs::write(dir.join("potter.toml"), toml).unwrap();
}

fn build(ctx: &Ctx, repo: &Path, extra: &[&str]) -> (String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["build", "--executor", "ns"])
        .args(extra)
        .arg(repo)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    (stdout, stderr)
}

/// The kiln image's layers: (erofs digest, how many OCI layers each was built from).
fn layers(ctx: &Ctx) -> Vec<(kiln_store::Digest, usize)> {
    let digest = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    loaded.entries[0]
        .1
        .manifest
        .layers
        .iter()
        .map(|l| {
            let sources = l.annotation(kiln_image::types::ANN_SOURCE_DIGESTS).unwrap();
            (l.digest.clone(), sources.split(',').count())
        })
        .collect()
}

const SIX: [&str; 6] = ["a", "b", "c", "d", "e", "f"];

#[test]
fn the_default_budget_keeps_ten_layers() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = serve_base();
    // 3 + 1 + 6 = 10 layers: exactly the default budget, nothing merged.
    let repo = project(&base, &SIX, None);
    let (_, stderr) = build(&ctx, repo.path(), &[]);
    assert!(!stderr.contains("composite"), "{stderr}");
    let l = layers(&ctx);
    assert_eq!(l.len(), 10);
    assert!(l.iter().all(|(_, n)| *n == 1));
}

#[test]
fn over_budget_the_base_merges_first_and_is_reused() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = serve_base();
    // 10 layers over potter.toml's budget of 9: kiln alone would merge the bottom 2; potter
    // merges the whole 3-layer base, leaving 8.
    let repo = project(&base, &SIX, Some(9));
    let (stdout, stderr) = build(&ctx, repo.path(), &["--progress", "json"]);
    let built: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(built["event"], "target_built", "{stdout}\n{stderr}");
    assert_eq!(
        (
            built["layers"].clone(),
            built["max_layers"].clone(),
            built["composite"].clone()
        ),
        (8.into(), 9.into(), 3.into())
    );
    let first = layers(&ctx);
    assert_eq!(first.len(), 8);
    assert_eq!(first[0].1, 3, "the composite is the base");

    // A changed top step: the composite and every layer below the step are the same blobs.
    let mut changed = SIX;
    changed[5] = "changed";
    write_toml(repo.path(), &base, &changed, Some(9));
    let (_, stderr) = build(&ctx, repo.path(), &[]);
    assert!(stderr.contains("[app] s6: running"), "{stderr}");
    assert!(
        stderr.contains("the bottom 3 (the base's 3) merged into one composite"),
        "{stderr}"
    );
    let second = layers(&ctx);
    assert_eq!(second[0], first[0], "the composite is reused");
    // potter.toml is in the `source` inputs layer, so that layer changed too, and the steps above
    // it ran again (their keys chain on it) but sealed the same bytes: the same erofs blobs.
    assert_ne!(second[1], first[1]);
    assert_eq!(second[2..7], first[2..7]);
    assert_ne!(second[7], first[7]);
}

#[test]
fn the_cli_budget_wins_and_steps_merge_above_the_base_when_it_is_not_enough() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = serve_base();
    let repo = project(&base, &SIX, Some(9));
    // 10 layers into 4: the base merges, then the five lowest step layers (source, s1 to s4)
    // merge on their own side of it, so the image stays rebaseable.
    let (_, stderr) = build(&ctx, repo.path(), &["--max-layers", "4"]);
    assert!(
        stderr.contains(
            "the bottom 3 (the base's 3) merged into one composite, and the lowest 5 above the run base into another"
        ),
        "{stderr}"
    );
    let l = layers(&ctx);
    assert_eq!(l.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [3, 5, 1, 1]);

    // Above Firecracker's x86_64 ceiling: a warning, never a failure; nothing merged.
    let (_, stderr) = build(&ctx, repo.path(), &["--max-layers", "20"]);
    assert!(
        stderr.contains("warning: target app: max_layers 20 is over 13"),
        "{stderr}"
    );
    assert_eq!(layers(&ctx).len(), 10);
}
