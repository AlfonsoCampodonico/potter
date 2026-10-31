//! `potter rebase` end to end (spec §16): an image built with the namespace executor gets a
//! patched base without a rebuild.
#![cfg(target_os = "linux")]

mod support;

use std::io::Read;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_registry::testregistry::TestRegistry;
use support::Ctx;

/// A base layout: the busybox root, then `etc/release` holding `release`, with `env`.
fn layout(dir: &Path, release: &str, env: &[&str]) -> PathBuf {
    use kiln_oci::testlayout::{LayoutBuilder, TestLayer};
    let mut layout = LayoutBuilder::new(dir);
    let config = kiln_oci::ContainerConfig {
        env: Some(env.iter().map(|s| s.to_string()).collect()),
        ..Default::default()
    };
    let release = TarBuilder::new()
        .file("etc/release", release.as_bytes(), &Opts::default())
        .finish();
    let layers = [support::busybox_root(), release].map(TestLayer::tar);
    let image = layout.image(&kiln_oci::Platform::host(), &layers, config);
    layout.add(image, Some("1"));
    layout.finish()
}

fn project(base: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("potter.toml"),
        format!(
            "[target.app]\nprovider = \"none\"\nexport = []\n\n[target.app.base]\noci = \"{base}\"\n\n\
             [target.app.process.web]\ncmd = [\"/bin/cat\", \"/etc/release\"]\n\n\
             [target.app.steps.s1]\nrun = \"echo one > /app/s1\"\n\n\
             [target.app.steps.s2]\nrun = \"echo two > /app/s2\"\n"
        ),
    )
    .unwrap();
    dir
}

fn potter(ctx: &Ctx, args: &[&str]) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(args)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A file as the image's merged layers show it.
fn read(ctx: &Ctx, image: &str, path: &str) -> String {
    let digest = kiln_image::resolve_name(&ctx.store, image).unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    let images = loaded.entries[0]
        .1
        .manifest
        .layers
        .iter()
        .filter(|l| l.media_type == kiln_image::types::KILN_LAYER)
        .map(|l| kiln_erofs::Image::open(ctx.store.open_blob(&l.digest).unwrap()).unwrap())
        .collect();
    let mut merged = kiln_erofs::Merged::new(images);
    let (layer, nid) = merged.lookup(path.as_bytes()).unwrap().unwrap();
    let mut s = String::new();
    merged
        .image(layer)
        .data_reader(nid)
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    s
}

#[test]
fn a_rebuilt_tag_is_picked_up_without_rebuilding_the_app() {
    let Some(ctx) = Ctx::new() else { return };
    let v1 = tempfile::tempdir().unwrap();
    let reg = TestRegistry::serve_layout(&layout(v1.path(), "v1", &["PATH=/bin"]), Default::default());
    let base = format!("{}/lib/base:1", reg.host());
    let repo = project(&base);
    let (code, _, stderr) = potter(&ctx, &["build", "--executor", "ns", repo.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let built = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    assert_eq!(read(&ctx, "potter/app:latest", "etc/release"), "v1");

    // Nothing new behind the tag: nothing to do.
    let (code, stdout, stderr) = potter(&ctx, &["rebase", "--executor", "ns", "--json", "potter/app:latest"]);
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["unchanged"], true, "{stdout}");

    // The same tag, rebuilt with a patch.
    let v2 = tempfile::tempdir().unwrap();
    reg.load_layout(&layout(v2.path(), "v2", &["PATH=/bin"]));
    let (code, stdout, stderr) = potter(&ctx, &["rebase", "--executor", "ns", "--json", "potter/app:latest"]);
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["unchanged"], false, "{stdout}");
    assert_eq!(r["tag"], "potter/app:latest");
    assert_eq!(r["reused"].as_u64().unwrap() + r["changed"].as_u64().unwrap(), 3);
    assert_ne!(r["digest"], built.to_string());
    assert_eq!(read(&ctx, "potter/app:latest", "etc/release"), "v2");
    assert_eq!(read(&ctx, "potter/app:latest", "app/s2"), "two\n", "the app is kept");
    assert!(!stderr.contains("running"), "no step ran again: {stderr}");
}

#[test]
fn another_base_is_taken_with_base_and_a_dropped_env_var_is_refused() {
    let Some(ctx) = Ctx::new() else { return };
    let v1 = tempfile::tempdir().unwrap();
    let reg = TestRegistry::serve_layout(
        &layout(v1.path(), "v1", &["PATH=/bin", "LANG=C.UTF-8"]),
        Default::default(),
    );
    let repo = project(&format!("{}/lib/base:1", reg.host()));
    let (code, _, stderr) = potter(&ctx, &["build", "--executor", "ns", repo.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");

    let other_dir = tempfile::tempdir().unwrap();
    let other = TestRegistry::serve_layout(
        &layout(other_dir.path(), "patched", &["PATH=/bin", "LANG=C.UTF-8"]),
        Default::default(),
    );
    let other_ref = format!("{}/lib/patched:1", other.host());
    let args = [
        "rebase",
        "--executor",
        "ns",
        "--base",
        &other_ref,
        "--tag",
        "potter/app:patched",
        "potter/app:latest",
    ];
    let (code, stdout, stderr) = potter(&ctx, &args);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("potter/app:patched sha256:"), "{stdout}");
    assert_eq!(read(&ctx, "potter/app:patched", "etc/release"), "patched");
    assert_eq!(
        read(&ctx, "potter/app:latest", "etc/release"),
        "v1",
        "--tag leaves the image"
    );

    let bare_dir = tempfile::tempdir().unwrap();
    let bare = TestRegistry::serve_layout(&layout(bare_dir.path(), "bare", &["PATH=/bin"]), Default::default());
    let bare_ref = format!("{}/lib/bare:1", bare.host());
    let (code, _, stderr) = potter(
        &ctx,
        &["rebase", "--executor", "ns", "--base", &bare_ref, "potter/app:latest"],
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("the rebase is refused") && stderr.contains("LANG"),
        "{stderr}"
    );
}

#[test]
fn rebase_needs_the_namespace_executor() {
    let Some(ctx) = Ctx::new() else { return };
    let (code, _, stderr) = potter(&ctx, &["rebase", "potter/app:latest"]);
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("choose an executor"), "{stderr}");
}
