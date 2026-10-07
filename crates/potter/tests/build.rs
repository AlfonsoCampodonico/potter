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
