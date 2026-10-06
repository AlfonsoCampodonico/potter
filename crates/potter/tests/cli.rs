use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

fn potter() -> Command {
    Command::cargo_bin("potter").unwrap()
}

#[test]
fn plan_human_output() {
    potter()
        .args(["plan"])
        .arg(fixture("laravel"))
        .assert()
        .success()
        .stdout(predicate::str::contains("target app (laravel, root .)"))
        .stdout(predicate::str::contains("php artisan queue:work"))
        .stdout(predicate::str::contains("(+ php-fpm)"));
}

#[test]
fn plan_json_is_canonical() {
    let out = potter()
        .args(["plan", "--json"])
        .arg(fixture("nestjs"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["schema"], "potter.plan/v1");
    assert!(!text.trim_end().contains('\n'), "canonical JSON is a single line");
}

#[test]
fn plan_explain() {
    potter()
        .args(["plan", "--explain"])
        .arg(fixture("php-composer"))
        .assert()
        .success()
        .stdout(predicate::str::contains("decisions for target app:"))
        .stdout(predicate::str::contains("composer.json require.php = ^8.3"));
}

#[test]
fn json_and_explain_conflict() {
    potter()
        .args(["plan", "--json", "--explain"])
        .arg(fixture("nestjs"))
        .assert()
        .code(2);
}

#[test]
fn ambiguous_exits_2() {
    potter()
        .args(["plan"])
        .arg(fixture("ambiguous"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("all claim the build root"));
}

#[test]
fn missing_path_exits_2() {
    potter().args(["plan", "/definitely/not/here"]).assert().code(2);
}

#[test]
fn unknown_target_exits_2() {
    potter()
        .args(["plan", "--target", "nope"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no target named `nope`"));
}

#[test]
fn detect_lists_claims() {
    potter()
        .args(["detect"])
        .arg(fixture("laravel"))
        .assert()
        .success()
        .stdout(predicate::str::contains("laravel"))
        .stdout(predicate::str::contains("chosen: laravel"));
}

#[test]
fn detect_monorepo_root_hints() {
    potter()
        .args(["detect"])
        .arg(fixture("monorepo-root"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("[target.backend]"));
}

#[test]
fn repo_text_in_errors_is_sanitised() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("composer.json"),
        "{\"require\":{\"php\":\"\\u001b[31mx\"}}",
    )
    .unwrap();
    std::fs::write(d.path().join("composer.lock"), "{\"packages\":[]}").unwrap();
    let out = potter()
        .arg("plan")
        .arg(d.path())
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let err = String::from_utf8(out).unwrap();
    assert!(!err.contains('\u{1b}'), "{err:?}");
    assert!(err.contains("\\u{1b}"), "{err:?}");
}
