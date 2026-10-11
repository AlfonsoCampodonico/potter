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
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["build", "--executor", "ns"])
        .args(extra)
        .arg(repo)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
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

/// Detected projects built from real upstream images (Docker Hub, PyPI, Go's proxy): only with
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

#[test]
fn a_detected_go_service_builds_from_golang_trixie() {
    let Some(ctx) = Ctx::new().filter(|_| network()) else {
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
fn a_detected_flask_app_builds_from_python_trixie() {
    let Some(ctx) = Ctx::new().filter(|_| network()) else {
        return;
    };
    let app =
        "from flask import Flask\n\napp = Flask(__name__)\n\n\n@app.get(\"/\")\ndef index():\n    return \"hello\"\n";
    let entrypoint = build_detected(&ctx, &[("requirements.txt", "flask\ngunicorn\n"), ("app.py", app)]);
    assert!(entrypoint.iter().any(|a| a.contains("gunicorn")), "{entrypoint:?}");
}

#[test]
fn a_detected_node_server_builds_from_node_trixie() {
    let Some(ctx) = Ctx::new().filter(|_| network()) else {
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
fn native_gems_compile_on_the_ruby_build_base() {
    let Some(ctx) = Ctx::new().filter(|_| network()) else {
        return;
    };
    let repo = catalog_project(&["os-min", "ruby@3.4"], "gem install nio4r --no-document", &[]);
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
