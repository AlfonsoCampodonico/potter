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

#[test]
fn build_never_picks_the_namespace_executor_by_itself() {
    potter()
        .args(["build"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("--executor ns"));
    potter()
        .args(["build", "--executor", "vm"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("P4"));
}

fn build_fails(args: &[&str], code: i32, message: &str) {
    potter()
        .args(["build", "--executor", "ns"])
        .args(args)
        .arg(fixture("nestjs"))
        .assert()
        .code(code)
        .stderr(predicate::str::contains(message));
}

#[test]
fn output_flags_are_checked_before_anything_runs() {
    build_fails(&["--output", "oci=x"], 2, "--output: expected registry=<ref>");
    build_fails(&["--output", "registry=Bad Ref"], 2, "--output");
    build_fails(
        &["--output", "registry=r.example/app:1"],
        2,
        "--output needs exactly one --target",
    );
    build_fails(
        &["--sign-key", "k.pem", "--target", "app"],
        2,
        "--sign-key needs --output",
    );
    build_fails(
        &[
            "--output",
            "registry=r.example/app:1",
            "--target",
            "app",
            "--sign-key",
            "kms://alias/x",
        ],
        2,
        "KMS",
    );
}

#[test]
fn a_tenant_key_file_must_hold_32_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let short = dir.path().join("short");
    std::fs::write(&short, b"abc").unwrap();
    build_fails(&["--tenant-key-file", short.to_str().unwrap()], 2, "32 bytes or 64 hex");
    build_fails(&["--tenant-key-file", "/nonexistent/key"], 4, "--tenant-key-file");
}

#[test]
fn key_generate_then_verify_a_published_image() {
    use kiln_oci::media::{OCI_CONFIG, OCI_MANIFEST};
    use kiln_oci::{Descriptor, ImageManifest, canonical_json};
    use potter_exec::publish::{Attachment, Kind, Publisher};

    let dir = tempfile::tempdir().unwrap();
    potter().args(["key", "generate"]).arg(dir.path()).assert().success();
    let private = dir.path().join("potter.key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o600);
    }
    potter().args(["key", "generate"]).arg(dir.path()).assert().code(2);
    let key = potter_supply::sign::SigningKey::load(private.to_str().unwrap()).unwrap();

    let reg = kiln_registry::testregistry::TestRegistry::start(Default::default());
    let store_dir = tempfile::tempdir().unwrap();
    let store = kiln_store::Store::open(store_dir.path()).unwrap();
    let config = store.put_bytes(b"{}").unwrap();
    let m = ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_MANIFEST.into()),
        artifact_type: None,
        config: Descriptor::new(OCI_CONFIG, config, 2),
        layers: vec![],
        annotations: None,
    };
    let manifest = canonical_json(&m);
    let digest = store.put_bytes(&manifest).unwrap();
    let image = format!("{}/acme/app:v1", reg.host());
    let publisher = Publisher::new(
        &kiln_registry::Reference::parse(&image).unwrap(),
        kiln_registry::DockerConfig::anonymous(),
    )
    .unwrap();
    let subject = publisher.push_image(&store, "v1", &digest).unwrap();
    let d = digest.to_string();
    let repository = format!("{}/acme/app", reg.host());
    let statement = |p: &str| potter_supply::provenance::in_toto(&repository, &d, p, serde_json::json!({}));
    let sign = potter_supply::sign::sign_statement;
    let atts = [
        (
            Kind::Sbom,
            sign(&key, &statement(potter_supply::provenance::SPDX_PREDICATE)),
        ),
        (
            Kind::Provenance,
            sign(&key, &statement(potter_supply::provenance::SLSA_PREDICATE)),
        ),
        (Kind::Signature, potter_supply::sign::sign_manifest(&key, &manifest)),
    ]
    .map(|(kind, b)| Attachment {
        kind,
        media_type: potter_exec::publish::BUNDLE_TYPE.into(),
        bytes: serde_json::to_vec(&b).unwrap(),
    });
    publisher.attach(&store, &subject, &atts).unwrap();

    potter()
        .args(["verify", &image, "--key"])
        .arg(dir.path().join("potter.pub"))
        .assert()
        .success()
        .stdout(predicate::str::contains("sbom: verified"))
        .stdout(predicate::str::contains("signature: verified"))
        .stdout(predicate::str::contains(format!("{repository}@{digest}")));
    potter()
        .args(["verify", &image, "--expect-tenant", "acme", "--key"])
        .arg(dir.path().join("potter.pub"))
        .assert()
        .code(3)
        .stdout(predicate::str::contains("provenance: FAILED: the tenant"));
    let other = tempfile::tempdir().unwrap();
    potter().args(["key", "generate"]).arg(other.path()).assert().success();
    potter()
        .args(["verify", &image, "--key"])
        .arg(other.path().join("potter.pub"))
        .assert()
        .code(3)
        .stdout(predicate::str::contains("provenance: FAILED"));
}
