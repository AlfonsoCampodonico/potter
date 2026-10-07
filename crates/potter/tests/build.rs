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
