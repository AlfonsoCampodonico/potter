//! `potter build` end to end (spec §11, §15 P2), against kiln's in-process registry.
#![cfg(target_os = "linux")]

mod support;

use std::path::Path;

use assert_cmd::Command;
use support::Ctx;

fn project(base: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("potter.toml"),
        format!(
            r#"[target.app]
provider = "none"

[target.app.base]
oci = "{base}"

[target.app.steps.build]
run = "cat /app/greeting > /app/out"

[target.app.process.web]
cmd = ["/bin/cat", "/app/out"]
"#
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("greeting"), "hello\n").unwrap();
    dir
}

fn build(ctx: &Ctx, repo: &Path, extra: &[&str]) -> (i32, String, String) {
    build_with(ctx, repo, extra, &[])
}

/// `potter build` with `env` set too.
fn build_with(ctx: &Ctx, repo: &Path, extra: &[&str], env: &[(&str, String)]) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["build", "--executor", "ns"])
        .args(extra)
        .arg(repo)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .envs(env.iter().map(|(k, v)| (k, v)))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The OCI manifest `name` was converted from: its annotations are what a rebase reads.
fn source_manifest(ctx: &Ctx, name: &str) -> serde_json::Value {
    let digest = kiln_image::resolve_name(&ctx.store, name).unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    let source = &loaded.entries[0].1.config.source.manifest_digest;
    serde_json::from_slice(&ctx.store.read_metadata(source).unwrap()).unwrap()
}

#[test]
fn potter_build_makes_a_kiln_image_and_then_hits_the_cache() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = project(&base);
    let (code, stdout, stderr) = build(&ctx, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("potter/app:latest sha256:"), "{stdout}");
    assert!(stdout.contains("(executed 1, cached 0)"), "{stdout}");
    assert!(stderr.contains("[app] build: running"), "{stderr}");
    let digest = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    assert_eq!(loaded.entries[0].1.config.process.entrypoint, ["/bin/cat", "/app/out"]);
    // An image on an OCI base records nothing about the catalog.
    let annotations = source_manifest(&ctx, "potter/app:latest")["annotations"].clone();
    let keys: Vec<&String> = annotations.as_object().unwrap().keys().collect();
    assert!(
        !keys.iter().any(|k| k.starts_with("dev.potter.catalog.")
            || *k == "dev.potter.chain"
            || k.starts_with("dev.potter.sonames.")),
        "{keys:?}"
    );

    let (code, stdout, stderr) = build(&ctx, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 0, cached 1)"), "{stdout}");
    assert!(stderr.contains("[app] build: cached"), "{stderr}");

    let (code, stdout, _) = build(&ctx, repo.path(), &["--no-cache"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("(executed 1, cached 0)"), "{stdout}");
}

#[test]
fn a_failing_step_exits_1_with_its_output() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = project(&base);
    let toml = std::fs::read_to_string(repo.path().join("potter.toml")).unwrap();
    std::fs::write(
        repo.path().join("potter.toml"),
        toml.replace("cat /app/greeting > /app/out", "echo broken; exit 9"),
    )
    .unwrap();
    let (code, _, stderr) = build(&ctx, repo.path(), &[]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("[app/build] broken"), "{stderr}");
    assert!(stderr.contains("step build failed"), "{stderr}");
}

/// Builds that reach the network (Docker Hub for the PHP stopgap, package registries): only with
/// POTTER_TEST_NETWORK=1.
fn network() -> bool {
    std::env::var_os("POTTER_TEST_NETWORK").is_some_and(|v| v == "1")
}

fn write(dir: &Path, files: &[(&str, &str)]) {
    for (p, c) in files {
        let path = dir.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, c).unwrap();
    }
}

/// Builds a detected project and returns the image's entrypoint.
fn build_detected(ctx: &Ctx, files: &[(&str, &str)]) -> Vec<String> {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), files);
    let (code, stdout, stderr) = build(ctx, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("potter/app:latest sha256:"), "{stdout}");
    let digest = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    loaded.entries[0].1.config.process.entrypoint.clone()
}

/// A detected Go module built on the catalog: the build base is the composite of a busybox
/// `os-min` and a fake `go`, the run base `os-min` alone. No registry and no network.
#[test]
fn detected_go_builds_on_a_test_catalog() {
    let Some(ctx) = Ctx::new() else { return };
    let repo = tempfile::tempdir().unwrap();
    write(
        repo.path(),
        &[
            ("go.mod", "module example.com/hello\n\ngo 1.25\n"),
            ("main.go", "package main\n\nfunc main() {}\n"),
        ],
    );
    // The go line the plan asks for.
    let plan = Command::cargo_bin("potter")
        .unwrap()
        .args(["plan", "--json"])
        .arg(repo.path())
        .output()
        .unwrap();
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let layers = &plan["targets"][0]["build"]["base"]["layers"];
    let line = layers
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "go")
        .and_then(|l| l["version"].as_str())
        .unwrap()
        .to_string();
    let catalog = support::catalog::go_catalog(&line);
    let (code, stdout, stderr) = build_with(&ctx, repo.path(), &[], &catalog.env());
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("potter/app:latest sha256:"), "{stdout}");
    assert!(stdout.contains("(executed 2, cached 0)"), "{stdout}");
    assert!(stderr.contains("using the catalog at oci-layout:"), "{stderr}");
    assert!(stderr.contains("[app] build: done"), "{stderr}");
    let digest = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    assert_eq!(loaded.entries[0].1.config.process.entrypoint, ["/out/app"]);
    // The composite was cached under potter's home.
    let composites = std::fs::read_dir(ctx.home.root().join("composite")).unwrap().count();
    assert_eq!(composites, 2, "the build base and the run base");
    // The image names its run base through the catalog, and the layers a rebase keeps.
    let manifest = source_manifest(&ctx, "potter/app:latest");
    let a = &manifest["annotations"];
    let json = |k: &str| serde_json::from_str::<serde_json::Value>(a[k].as_str().unwrap()).unwrap();
    let os_min = catalog.index.entries.iter().find(|e| e.name == "os-min").unwrap();
    assert_eq!(a["dev.potter.catalog.index"], catalog.digest.as_str(), "{a}");
    assert_eq!(
        json("dev.potter.catalog.layers"),
        serde_json::json!([os_min.digest]),
        "{a}"
    );
    assert_eq!(
        json("dev.potter.catalog.constraints"),
        serde_json::json!(["os-min@13"]),
        "{a}"
    );
    let export = manifest["layers"].as_array().unwrap().last().unwrap()["digest"].clone();
    assert_eq!(
        json("dev.potter.chain"),
        serde_json::json!([export]),
        "the export layer: {a}"
    );
    assert_eq!(
        json("dev.potter.sonames.needed"),
        serde_json::json!([]),
        "a shell script needs none: {a}"
    );

    // Pinned to the same index, everything is cached; a pin the catalog lacks is refused.
    let pin = catalog.digest.clone();
    let (code, stdout, stderr) = build_with(&ctx, repo.path(), &["--catalog-index", &pin], &catalog.env());
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 0, cached 2)"), "{stdout}");
    let other = format!("sha256:{}", "0".repeat(64));
    let (code, _, stderr) = build_with(&ctx, repo.path(), &["--catalog-index", &other], &catalog.env());
    assert_eq!(code, 4, "the layout has no such index: {stderr}");
    assert!(stderr.contains("catalog"), "{stderr}");

    // A rebase resolves the catalog run base again: the same index gives the same composite.
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["rebase", "potter/app:latest", "--executor", "ns", "--json"])
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .envs(catalog.env())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(summary["unchanged"], true, "{summary}");

    // `--base` on a catalog image is refused without loading the catalog (here: a layout that
    // does not exist), and a pin nothing uses is called out.
    let pin = catalog.digest.clone();
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["rebase", "potter/app:latest", "--executor", "ns", "--base", "busybox:1"])
        .args(["--catalog-index", &pin])
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .env("POTTER_CATALOG", "oci-layout:/nonexistent/catalog")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--base needs an image on an OCI base"), "{stderr}");
    assert!(stderr.contains("warning: --catalog-index is ignored"), "{stderr}");
    assert_eq!(
        summary["base_ref"],
        format!("potter-catalog@{}", catalog.digest),
        "{summary}"
    );
}

/// An explicit Go target on the test catalog, whose run base is `run_catalog`.
fn catalog_go_project(go_line: &str, run_catalog: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let toml = format!(
        "[target.app]\nprovider = \"none\"\nexport = [\"/out/app\"]\n\n[target.app.base]\n\
         catalog = [\"os-min\", \"go@{go_line}\"]\nrun_catalog = [{run_catalog}]\n\n\
         [target.app.steps.build]\nrun = \"go build -o /out/app .\"\n\n\
         [target.app.process.web]\ncmd = [\"/out/app\"]\n"
    );
    write(
        repo.path(),
        &[
            ("potter.toml", toml.as_str()),
            ("go.mod", "module example.com/hello\n\ngo 1.25\n"),
            ("main.go", "package main\n\nfunc main() {}\n"),
        ],
    );
    repo
}

/// Nothing ran: no step started and the cache index is empty.
fn assert_nothing_ran(ctx: &Ctx, stderr: &str) {
    assert!(!stderr.contains(": running"), "{stderr}");
    assert_eq!(ctx.index_entries(), 0, "{stderr}");
}

/// Every catalog base is resolved before any step runs: a run base the catalog cannot satisfy
/// fails the build before the build chain starts.
#[test]
fn an_unsatisfiable_run_base_fails_before_any_step_runs() {
    let Some(ctx) = Ctx::new() else { return };
    let catalog = support::catalog::go_catalog("1.27");
    let repo = catalog_go_project("1.27", "\"os-min\", \"zig@0.14\"");
    let (code, _, stderr) = build_with(&ctx, repo.path(), &[], &catalog.env());
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("zig"), "{stderr}");
    assert_nothing_ran(&ctx, &stderr);
    // The same target with a run base the catalog has builds.
    let repo = catalog_go_project("1.27", "\"os-min\"");
    let (code, stdout, stderr) = build_with(&ctx, repo.path(), &[], &catalog.env());
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 1, cached 0)"), "{stdout}");
}

/// A catalog layer whose bytes do not match the signed index is hostile, and is caught before
/// any step runs, even when only the run base, built after the build chain, uses it.
#[test]
fn a_tampered_catalog_layer_fails_before_any_step_runs() {
    use kiln_erofs::testtar::{Opts, TarBuilder};
    let Some(ctx) = Ctx::new() else { return };
    let mut catalog = support::catalog::go_catalog("1.27");
    let mut tz = TarBuilder::new();
    tz.dir("usr", &Opts::default().mode(0o755))
        .file("usr/zoneinfo", b"UTC", &Opts::default());
    let layer = support::catalog::Layer::new("tzdata", "2026", potter_catalog::Kind::Tool, tz.finish());
    catalog.publish(2, vec![layer]);
    let tzdata = catalog.index.entries.iter().find(|e| e.name == "tzdata").unwrap();
    let blob = catalog
        .layout
        .join("blobs/sha256")
        .join(tzdata.digest.strip_prefix("sha256:").unwrap());
    let mut bytes = std::fs::read(&blob).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&blob, bytes).unwrap();
    let repo = catalog_go_project("1.27", "\"os-min\", \"tzdata\"");
    let (code, _, stderr) = build_with(&ctx, repo.path(), &[], &catalog.env());
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("hostile"), "{stderr}");
    assert_nothing_ran(&ctx, &stderr);
    // A layer gone from the layout is an infrastructure failure, also before any step.
    std::fs::remove_file(&blob).unwrap();
    let (code, _, stderr) = build_with(&ctx, repo.path(), &[], &catalog.env());
    assert_eq!(code, 4, "{stderr}");
    assert_nothing_ran(&ctx, &stderr);
}

/// Detected projects built on the real catalog: only with POTTER_TEST_NETWORK=1 and
/// POTTER_TEST_CATALOG=1, reading the catalog from POTTER_CATALOG and POTTER_CATALOG_KEY.
/// Task 12's built catalog drives them.
fn catalog_network() -> bool {
    let on = network() && std::env::var_os("POTTER_TEST_CATALOG").is_some_and(|v| v == "1");
    if on {
        assert!(
            std::env::var_os("POTTER_CATALOG").is_some() && std::env::var_os("POTTER_CATALOG_KEY").is_some(),
            "POTTER_TEST_CATALOG=1 needs POTTER_CATALOG and POTTER_CATALOG_KEY"
        );
    }
    on
}

#[test]
fn a_detected_go_service_builds_on_the_catalog() {
    let Some(ctx) = Ctx::new().filter(|_| catalog_network()) else {
        return;
    };
    let main = "package main\n\nimport (\n\t\"fmt\"\n\t\"net/http\"\n\t\"os\"\n)\n\nfunc main() {\n\tport := os.Getenv(\"PORT\")\n\tif port == \"\" {\n\t\tport = \"8080\"\n\t}\n\thttp.HandleFunc(\"/\", func(w http.ResponseWriter, r *http.Request) { fmt.Fprintln(w, \"hello\") })\n\t_ = http.ListenAndServe(\":\"+port, nil)\n}\n";
    let entrypoint = build_detected(
        &ctx,
        &[("go.mod", "module example.com/hello\n\ngo 1.25\n"), ("main.go", main)],
    );
    assert!(!entrypoint.is_empty());
}

#[test]
fn a_detected_flask_app_builds_on_the_catalog() {
    let Some(ctx) = Ctx::new().filter(|_| catalog_network()) else {
        return;
    };
    let app =
        "from flask import Flask\n\napp = Flask(__name__)\n\n\n@app.get(\"/\")\ndef index():\n    return \"hello\"\n";
    let entrypoint = build_detected(&ctx, &[("requirements.txt", "flask\ngunicorn\n"), ("app.py", app)]);
    assert!(entrypoint.iter().any(|a| a.contains("gunicorn")), "{entrypoint:?}");
}

#[test]
fn a_detected_node_server_builds_on_the_catalog() {
    let Some(ctx) = Ctx::new().filter(|_| catalog_network()) else {
        return;
    };
    let entrypoint = build_detected(
        &ctx,
        &[
            (
                "package.json",
                r#"{"name":"hello","version":"1.0.0","scripts":{"start":"node server.js"}}"#,
            ),
            (
                "package-lock.json",
                r#"{"name":"hello","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"hello","version":"1.0.0"}}}"#,
            ),
            (
                "server.js",
                "require('http').createServer((q, s) => s.end('hello')).listen(process.env.PORT || 3000);\n",
            ),
        ],
    );
    assert!(!entrypoint.is_empty());
}

/// An explicit plan over a catalog base: `layers` and one build step.
fn catalog_project(layers: &[&str], run: &str, extra: &[(&str, &str)]) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let list = layers.iter().map(|l| format!("{l:?}")).collect::<Vec<_>>().join(", ");
    let toml = format!(
        "[target.app]\nprovider = \"none\"\n\n[target.app.base]\ncatalog = [{list}]\n\n[target.app.steps.deps]\nrun = {run:?}\n\n[target.app.process.web]\ncmd = [\"/bin/true\"]\n"
    );
    write(repo.path(), &[("potter.toml", toml.as_str())]);
    write(repo.path(), extra);
    repo
}

#[test]
fn composer_installs_dist_packages_without_the_zip_extension() {
    let Some(ctx) = Ctx::new().filter(|_| network()) else {
        return;
    };
    let repo = catalog_project(
        &["os-min", "php@8.4", "composer@2"],
        "composer install --no-interaction --prefer-dist",
        &[("composer.json", r#"{"require":{"psr/log":"^3"}}"#)],
    );
    let (code, _, stderr) = build(&ctx, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
}

#[test]
fn native_gems_compile_on_the_catalog_ruby_base() {
    let Some(ctx) = Ctx::new().filter(|_| catalog_network()) else {
        return;
    };
    let repo = catalog_project(
        &["os-min", "build-tools", "ruby@4.0"],
        "gem install nio4r --no-document",
        &[],
    );
    let (code, _, stderr) = build(&ctx, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
}

#[test]
fn progress_json_is_one_event_per_line() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = project(&base);
    let events = |extra: &[&str]| -> Vec<serde_json::Value> {
        let mut args = vec!["--progress", "json"];
        args.extend_from_slice(extra);
        let (code, stdout, stderr) = build(&ctx, repo.path(), &args);
        assert_eq!(code, 0, "{stderr}");
        stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l:?}: {e}")))
            .collect()
    };
    let first = events(&[]);
    let kinds: Vec<&str> = first.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"step_start") && kinds.contains(&"sealed"), "{kinds:?}");
    let built = first.last().unwrap();
    assert_eq!(built["event"], "target_built");
    assert_eq!(built["target"], "app");
    assert_eq!(built["reference"], "potter/app:latest");
    assert_eq!(built["executed"], 1);
    let second = events(&[]);
    assert!(
        second.iter().any(|e| e["event"] == "cache_hit" && e["step"] == "build"),
        "{second:?}"
    );
}

#[test]
fn a_fresh_builder_gets_every_step_from_the_remote_cache_of_its_tenant() {
    let Some(a) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let cache = kiln_registry::testregistry::TestRegistry::start(Default::default());
    let remote = format!("{}/potter", cache.host());
    let repo = project(&base);
    let (code, stdout, stderr) = build(&a, repo.path(), &["--cache-to", &remote]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 1, cached 0)"), "{stdout}");
    assert!(stderr.contains("[app] build: pushed"), "{stderr}");
    let b = Ctx::new().unwrap();
    let (code, stdout, stderr) = build(&b, repo.path(), &["--cache-from", &remote]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 0, cached 1)"), "{stdout}\n{stderr}");
    assert!(stderr.contains("[app] build: cached (remote)"), "{stderr}");
    let c = Ctx::new().unwrap();
    let (code, stdout, stderr) = build(&c, repo.path(), &["--cache-from", &remote, "--tenant", "other"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("(executed 1, cached 0)"),
        "another tenant shares nothing: {stdout}"
    );
}

#[test]
fn a_target_copies_out_of_another_built_before_it() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"[target.api]
provider = "none"
[target.api.base]
oci = "{base}"
[target.api.steps.assets]
copy = {{ from = "web", paths = ["/app/dist"], dest = "/srv/www", owner = "1000:1000" }}
[target.api.steps.check]
run = "test -f /srv/www/index.html && stat -c %u /srv/www/index.html > /app/owner"
[target.api.process.web]
cmd = ["/bin/cat", "/app/owner"]

[target.web]
provider = "none"
[target.web.base]
oci = "{base}"
[target.web.steps.build]
run = "mkdir -p /app/dist && echo '<h1>' > /app/dist/index.html"
[target.web.process.web]
cmd = ["/bin/cat", "/app/dist/index.html"]
"#
    );
    write(repo.path(), &[("potter.toml", toml.as_str())]);
    let (code, stdout, stderr) = build(&ctx, repo.path(), &["--target", "api"]);
    assert_eq!(code, 0, "{stderr}");
    let order: Vec<&str> = stdout.lines().map(|l| l.split_whitespace().next().unwrap()).collect();
    assert_eq!(order, ["potter/web:latest", "potter/api:latest"], "{stdout}");
}

#[test]
fn affected_builds_only_targets_that_changed_since_a_ref() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = tempfile::tempdir().unwrap();
    let target = |name: &str| {
        format!(
            "[target.{name}]\nprovider = \"none\"\nroot = \"{name}\"\n[target.{name}.base]\noci = \"{base}\"\n[target.{name}.steps.build]\nrun = \"cat /app/x > /app/out\"\n[target.{name}.process.web]\ncmd = [\"/bin/cat\", \"/app/out\"]\n"
        )
    };
    let toml = format!("{}\n{}", target("web"), target("api"));
    write(
        repo.path(),
        &[("potter.toml", toml.as_str()), ("web/x", "w"), ("api/x", "a")],
    );
    let git = |args: &[&str]| {
        let s = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);
    std::fs::write(repo.path().join("api/x"), "changed").unwrap();
    let (code, stdout, stderr) = build(&ctx, repo.path(), &["--affected", "HEAD"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("web: unchanged"), "{stdout}");
    assert!(stdout.contains("potter/api:latest"), "{stdout}");
    assert!(!stdout.contains("potter/web:latest"), "{stdout}");
    let (code, _, stderr) = build(&ctx, repo.path(), &["--affected", "no-such-ref"]);
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn local_hits_are_published_too() {
    let Some(a) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let cache = kiln_registry::testregistry::TestRegistry::start(Default::default());
    let remote = format!("{}/potter", cache.host());
    let repo = project(&base);
    assert_eq!(build(&a, repo.path(), &[]).0, 0);
    let (code, stdout, stderr) = build(&a, repo.path(), &["--cache-to", &remote]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 0, cached 1)"), "{stdout}");
    let b = Ctx::new().unwrap();
    let (code, stdout, stderr) = build(&b, repo.path(), &["--cache-from", &remote]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 0, cached 1)"), "{stdout}\n{stderr}");
}

#[test]
fn a_published_image_carries_signed_attestations_that_verify() {
    let Some(ctx) = Ctx::new() else { return };
    let (reg, _dir, base) = support::serve_busybox();
    let repo = project(&base);
    std::fs::write(
        repo.path().join("composer.lock"),
        r#"{"packages":[{"name":"laravel/framework","version":"v11.0.0"}]}"#,
    )
    .unwrap();
    let keys = tempfile::tempdir().unwrap();
    let potter = || Command::cargo_bin("potter").unwrap();
    assert!(
        potter()
            .args(["key", "generate"])
            .arg(keys.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    let image = format!("{}/acme/app:v1", reg.host());
    let key = keys.path().join("potter.key");
    let (code, stdout, stderr) = build(
        &ctx,
        repo.path(),
        &[
            "--target",
            "app",
            "--output",
            &format!("registry={image}"),
            "--sign-key",
            key.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains(&format!("pushed {image}@sha256:")), "{stdout}");
    assert!(stdout.contains("(3 attestations)"), "{stdout}");
    let out = potter()
        .args(["verify", &image, "--key"])
        .arg(keys.path().join("potter.pub"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(text.matches(": verified").count(), 3, "{text}");

    // The SBOM lists the lockfile's packages, read from the inputs layer.
    let publisher = potter_exec::publish::Publisher::new(
        &kiln_registry::Reference::parse(&image).unwrap(),
        kiln_registry::DockerConfig::anonymous(),
    )
    .unwrap();
    let subject = publisher.resolve("v1").unwrap();
    let sbom = publisher
        .referrers(&subject.digest)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == Some(potter_exec::publish::Kind::Sbom))
        .unwrap();
    let (_, bundle) = publisher.fetch(&ctx.store, &sbom.descriptor).unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&bundle).unwrap();
    let payload = bundle["dsseEnvelope"]["payload"].as_str().unwrap();
    use base64::Engine;
    let statement = base64::engine::general_purpose::STANDARD.decode(payload).unwrap();
    assert!(String::from_utf8_lossy(&statement).contains("laravel/framework"));
}
