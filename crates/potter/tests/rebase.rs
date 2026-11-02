//! `potter rebase` end to end (spec §16): an image built with the namespace executor gets a
//! patched base without a rebuild.
#![cfg(target_os = "linux")]

mod support;

use std::io::Read;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_registry::testregistry::TestRegistry;
use potter_catalog::Kind;
use support::Ctx;
use support::catalog::{Layer, TestCatalog};

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

/// `potter` with a test catalog's environment.
fn potter_on(ctx: &Ctx, catalog: &TestCatalog, args: &[&str]) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(args)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .envs(catalog.env())
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// An explicit Go target on the test catalog: built on `os-min` and `go@1.27`, its export on
/// `os-min` alone.
fn go_project() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let toml = "[target.app]\nprovider = \"none\"\nexport = [\"/out/app\"]\n\n[target.app.base]\n\
                catalog = [\"os-min\", \"go@1.27\"]\nrun_catalog = [\"os-min\"]\n\n\
                [target.app.steps.build]\nrun = \"go build -o /out/app .\"\n\n\
                [target.app.process.web]\ncmd = [\"/out/app\"]\n";
    for (p, c) in [
        ("potter.toml", toml),
        ("go.mod", "module example.com/hello\n\ngo 1.25\n"),
        ("main.go", "package main\n\nfunc main() {}\n"),
    ] {
        std::fs::write(repo.path().join(p), c).unwrap();
    }
    repo
}

/// A patched `os-min` (another `/etc/motd`) that supersedes the current one.
fn patched_os_min(catalog: &TestCatalog) -> Layer {
    Layer::new(
        "os-min",
        "13",
        Kind::Base,
        support::busybox_root_with_motd(b"patched\n"),
    )
    .env("PATH", "/bin")
    .supersedes(&catalog.current("os-min").digest)
}

/// The kiln image's erofs layers above its run base.
fn app_layers(ctx: &Ctx, image: &str) -> Vec<kiln_store::Digest> {
    let digest = kiln_image::resolve_name(&ctx.store, image).unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    let m = &loaded.entries[0].1;
    let base = m.config.base.as_ref().unwrap().layers;
    m.manifest
        .layers
        .iter()
        .filter(|l| l.media_type == kiln_image::types::KILN_LAYER)
        .skip(base)
        .map(|l| l.digest.clone())
        .collect()
}

#[test]
fn rebase_keeps_app_layer_digests_and_records_replaced() {
    let Some(ctx) = Ctx::new() else { return };
    let mut catalog = support::catalog::go_catalog("1.27");
    let repo = go_project();
    let (code, _, stderr) = potter_on(
        &ctx,
        &catalog,
        &["build", "--executor", "ns", repo.path().to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stderr}");
    let before = app_layers(&ctx, "potter/app:latest");
    assert_eq!(read(&ctx, "potter/app:latest", "etc/motd"), "hello\n");

    let old = catalog.current("os-min").digest.clone();
    catalog.publish(2, vec![patched_os_min(&catalog)]);
    let new = catalog.current("os-min").digest.clone();
    let rebase = ["rebase", "--executor", "ns", "--json", "potter/app:latest"];
    let (code, stdout, stderr) = potter_on(&ctx, &catalog, &rebase);
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["unchanged"], false, "{stdout}");
    assert_eq!(r["replaced"], serde_json::json!([[old, new]]), "{stdout}");
    assert!(
        !r["replaced"].to_string().contains("potter-catalog@"),
        "replaced names catalog entries: {stdout}"
    );
    assert_eq!(
        app_layers(&ctx, "potter/app:latest"),
        before,
        "the export layer is the same erofs blob"
    );
    assert_eq!(read(&ctx, "potter/app:latest", "etc/motd"), "patched\n");
    assert!(read(&ctx, "potter/app:latest", "out/app").contains("hello from go"));

    // The rebased image records the layers it is on now: nothing newer, nothing replaced.
    let (code, stdout, stderr) = potter_on(&ctx, &catalog, &rebase);
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["unchanged"], true, "{stdout}");
    assert_eq!(r["replaced"], serde_json::json!([]), "{stdout}");
}

#[test]
fn abi_change_is_refused_naming_key() {
    let Some(ctx) = Ctx::new() else { return };
    let mut catalog = support::catalog::go_catalog("1.27");
    let repo = go_project();
    let (code, _, stderr) = potter_on(
        &ctx,
        &catalog,
        &["build", "--executor", "ns", repo.path().to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stderr}");
    let built = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    catalog.publish(2, vec![patched_os_min(&catalog).abi("os", "debian-14")]);

    let (code, _, stderr) = potter_on(&ctx, &catalog, &["rebase", "--executor", "ns", "potter/app:latest"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("the rebase is refused")
            && stderr.contains("os-min: abi os changes from debian-13 to debian-14"),
        "{stderr}"
    );
    assert_eq!(
        kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap(),
        built
    );

    // A dry run says the same, gate by gate.
    let (code, stdout, stderr) = potter_on(&ctx, &catalog, &["rebase", "--dry-run", "potter/app:latest"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stdout.contains("abi: refused: os-min: abi os changes"), "{stdout}");
    assert!(
        stdout.contains("line: pass") && stdout.contains("sonames: pass"),
        "{stdout}"
    );
}

/// Every blob file in the kiln store.
fn blobs(ctx: &Ctx) -> std::collections::BTreeSet<String> {
    let dir = ctx
        .store
        .blob_path(&kiln_store::Digest::of(b""))
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn dry_run_writes_nothing() {
    let Some(ctx) = Ctx::new() else { return };
    let mut catalog = support::catalog::go_catalog("1.27");
    let repo = go_project();
    let (code, _, stderr) = potter_on(
        &ctx,
        &catalog,
        &["build", "--executor", "ns", repo.path().to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stderr}");
    let old = catalog.current("os-min").digest.clone();
    catalog.publish(2, vec![patched_os_min(&catalog)]);
    let new = catalog.current("os-min").digest.clone();

    let refs = ctx.store.refs().unwrap();
    let before = blobs(&ctx);
    let composites = std::fs::read_dir(ctx.home.root().join("composite")).unwrap().count();
    let entries = ctx.index_entries();
    let (code, stdout, stderr) = potter_on(&ctx, &catalog, &["rebase", "--dry-run", "--json", "potter/app:latest"]);
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["dry_run"], true, "{stdout}");
    assert_eq!(r["index"], catalog.digest.as_str(), "{stdout}");
    assert_eq!(r["replaced"], serde_json::json!([[old, new]]), "{stdout}");
    let gates: Vec<(String, String)> = r["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["gate"].as_str().unwrap().into(),
                g["verdict"].as_str().unwrap().into(),
            )
        })
        .collect();
    let want = [
        ("line", "pass"),
        ("abi", "pass"),
        ("sonames", "pass"),
        ("kiln", "not checked"),
    ];
    assert_eq!(gates, want.map(|(g, v)| (g.to_string(), v.to_string())), "{stdout}");

    // Loading the catalog keeps its verified index (its blobs and a `potter-catalog/` ref), as
    // any read of it does; nothing else is new.
    let images = |r: std::collections::BTreeMap<String, kiln_store::Digest>| {
        r.into_iter()
            .filter(|(k, _)| !k.starts_with("potter-catalog/"))
            .collect::<Vec<_>>()
    };
    assert_eq!(images(ctx.store.refs().unwrap()), images(refs), "nothing tagged");
    let catalog_blobs: std::collections::BTreeSet<String> = std::fs::read_dir(catalog.layout.join("blobs/sha256"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let added: Vec<String> = blobs(&ctx).difference(&before).cloned().collect();
    assert!(added.iter().all(|b| catalog_blobs.contains(b)), "{added:?}");
    assert_eq!(
        std::fs::read_dir(ctx.home.root().join("composite")).unwrap().count(),
        composites,
        "no composite built"
    );
    assert_eq!(ctx.index_entries(), entries, "no step ran");
    assert_eq!(read(&ctx, "potter/app:latest", "etc/motd"), "hello\n");
}

#[test]
fn no_export_image_rebases_through_kiln_rederive() {
    let Some(ctx) = Ctx::new() else { return };
    let mut catalog = support::catalog::go_catalog("1.27");
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("potter.toml"),
        "[target.app]\nprovider = \"none\"\nexport = []\n\n[target.app.base]\ncatalog = [\"os-min\"]\n\n\
         [target.app.process.web]\ncmd = [\"/bin/cat\", \"/etc/motd\"]\n\n\
         [target.app.steps.s1]\nrun = \"echo one > /app/s1\"\n",
    )
    .unwrap();
    let (code, _, stderr) = potter_on(
        &ctx,
        &catalog,
        &["build", "--executor", "ns", repo.path().to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stderr}");
    // Every layer above the run base.
    let above = app_layers(&ctx, "potter/app:latest").len() as u64;
    let old = catalog.current("os-min").digest.clone();
    catalog.publish(2, vec![patched_os_min(&catalog)]);
    let new = catalog.current("os-min").digest.clone();

    let (code, stdout, stderr) = potter_on(
        &ctx,
        &catalog,
        &["rebase", "--executor", "ns", "--json", "potter/app:latest"],
    );
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["unchanged"], false, "{stdout}");
    assert_eq!(r["replaced"], serde_json::json!([[old, new]]), "{stdout}");
    assert_eq!(r["reused"].as_u64().unwrap() + r["changed"].as_u64().unwrap(), above);
    assert_eq!(read(&ctx, "potter/app:latest", "etc/motd"), "patched\n");
    assert_eq!(read(&ctx, "potter/app:latest", "app/s1"), "one\n", "the app is kept");
    assert!(!stderr.contains(": running"), "no build step ran again: {stderr}");
}

/// Builds that reach Docker Hub: only with POTTER_TEST_NETWORK=1.
fn network() -> bool {
    std::env::var_os("POTTER_TEST_NETWORK").is_some_and(|v| v == "1")
}

/// A Go image as potter built it before P3: its catalog run base lowered through the stopgap
/// onto `golang`'s image (here the stopgap's `debian:trixie-slim` for a bare `os-min`), with no
/// catalog annotations. The rebase lowers it the same way again and never loads the catalog.
#[test]
fn pre_p3_stopgap_image_still_rebases() {
    use kiln_oci::{Descriptor, ImageManifest, ResolvedImage, canonical_json, media};
    use potter_exec::assemble::{ANN_MAX_LAYERS, ANN_RUN_CHAIN, RunBase, convert_within_budget, image_config};
    use potter_exec::chain::LayerRef;
    use potter_plan::{Base, CatalogRef, Chain};
    let Some(ctx) = Ctx::new().filter(|_| network()) else {
        return;
    };
    let platform = kiln_oci::Platform::host();
    let reference = "debian:trixie-slim";
    let base = potter_exec::base::resolve_base(
        &ctx.store,
        reference,
        None,
        &platform,
        kiln_registry::DockerConfig::from_env(),
    )
    .unwrap();
    let mut layers: Vec<LayerRef> = base
        .layers()
        .iter()
        .zip(base.diff_ids())
        .map(|(d, id)| LayerRef {
            descriptor: d.clone(),
            diff_id: id.clone(),
        })
        .collect();
    let rb = RunBase::new(&ctx.store, reference, &base, &base.env(), &layers).unwrap();
    let app = TarBuilder::new()
        .dir("app", &Opts::default().mode(0o755))
        .file("app/s1", b"one\n", &Opts::default())
        .finish();
    let d = ctx.store.put_bytes(&app).unwrap();
    layers.push(LayerRef {
        descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), app.len() as u64),
        diff_id: d,
    });
    let plan_image = potter_plan::Image {
        base: Base::Oci {
            reference: reference.into(),
            digest: None,
        },
        runtime_steps: vec![],
        export: vec![],
        files: vec![],
        config: potter_plan::ImageConfig::new("/app", "0:0"),
        processes: Default::default(),
        services: Default::default(),
        max_layers: None,
    };
    let config = image_config(
        &platform,
        &potter_exec::base::env_of(&rb.resolved.config),
        &rb.labels(),
        &plan_image,
        layers.iter().map(|l| l.diff_id.clone()).collect(),
    );
    let run_chain = Chain {
        base: Base::Catalog {
            layers: vec![CatalogRef::new("os-min", None)],
        },
        steps: vec![],
    };
    let config_bytes = canonical_json(&config);
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(media::OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(
            media::OCI_CONFIG,
            ctx.store.put_bytes(&config_bytes).unwrap(),
            config_bytes.len() as u64,
        ),
        layers: layers.iter().map(|l| l.descriptor.clone()).collect(),
        annotations: Some(
            [
                (
                    ANN_RUN_CHAIN.to_string(),
                    potter_plan::canonical_json(&run_chain).unwrap(),
                ),
                (ANN_MAX_LAYERS.to_string(), "10".to_string()),
            ]
            .into(),
        ),
    };
    let resolved = ResolvedImage {
        platform: platform.clone(),
        manifest_digest: ctx.store.put_bytes(&canonical_json(&manifest)).unwrap(),
        config_digest: manifest.config.digest.clone(),
        manifest,
        config,
        ref_name: None,
    };
    let (digest, _) = convert_within_budget(&ctx.store, resolved, "potter/app:latest", &rb, 10).unwrap();
    ctx.store.set_ref("potter/app:latest", &digest).unwrap();

    // The catalog cannot be loaded here; the stopgap image needs no catalog.
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["rebase", "--executor", "ns", "--json", "potter/app:latest"])
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .env("POTTER_CATALOG", "oci-layout:/nonexistent/catalog")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["base_ref"], reference, "{r}");
    assert_eq!(r["unchanged"], true, "the tag still names the same build: {r}");
    assert_eq!(r["replaced"], serde_json::json!([]), "{r}");
}
