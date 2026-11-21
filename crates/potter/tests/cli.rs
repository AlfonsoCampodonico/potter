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
fn max_layers_enters_the_plan_and_wins_over_potter_toml() {
    let plan = |args: &[&str], dir: &Path| -> serde_json::Value {
        let out = potter()
            .args(["plan", "--json"])
            .args(args)
            .arg(dir)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&out).unwrap()
    };
    let budget = |v: &serde_json::Value| v["targets"][0]["image"]["max_layers"].clone();
    // Unset: absent, so plans without a budget keep their hash.
    assert_eq!(budget(&plan(&[], &fixture("nestjs"))), serde_json::Value::Null);
    assert_eq!(budget(&plan(&["--max-layers", "24"], &fixture("nestjs"))), 24);

    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("potter.toml"),
        "[target.app]\nprovider = \"none\"\n[target.app.base]\noci = \"debian:13\"\n\
         [target.app.process.web]\ncmd = [\"/bin/x\"]\n[target.app.image]\nmax_layers = 16\n",
    )
    .unwrap();
    assert_eq!(budget(&plan(&[], d.path())), 16);
    assert_eq!(budget(&plan(&["--max-layers", "12"], d.path())), 12);
    potter()
        .args(["plan"])
        .arg(d.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("max layers: 16"));
}

#[test]
fn max_layers_out_of_range_is_a_usage_error() {
    for bad in ["2", "129", "0", "-1", "ten"] {
        let flag = format!("--max-layers={bad}");
        potter()
            .args(["plan", &flag])
            .arg(fixture("nestjs"))
            .assert()
            .code(2)
            .stderr(predicate::str::contains("--max-layers"));
        potter()
            .args(["build", "--executor", "ns", &flag])
            .arg(fixture("nestjs"))
            .assert()
            .code(2);
    }
}

fn bare() -> assert_cmd::Command {
    let mut c = potter();
    c.env_remove("POTTER_KERNEL")
        .env_remove("POTTER_KILN_INIT")
        .env_remove("POTTER_VMM");
    c
}

#[test]
fn build_never_picks_an_executor_by_itself() {
    bare()
        .args(["build"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("--executor vm").and(predicate::str::contains("--executor ns")));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_vm_build_off_linux_exits_4_and_says_why() {
    bare()
        .args(["build", "--executor", "vm"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("VM builds need Linux with KVM"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_vm_build_without_a_kernel_or_kiln_init_exits_4_naming_the_flag() {
    bare()
        .args(["build", "--executor", "vm"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("--kernel <path> or set POTTER_KERNEL"));
    bare()
        .args(["build", "--executor", "vm", "--kernel", "/nonexistent"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("--kiln-init <path> or set POTTER_KILN_INIT"));
    bare()
        .args([
            "build",
            "--executor",
            "vm",
            "--kernel",
            "/nonexistent",
            "--kiln-init",
            "/nonexistent",
        ])
        .arg(fixture("explicit-go"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("the kernel /nonexistent is not a file"));
}

#[test]
fn an_unknown_vmm_is_a_usage_error() {
    bare()
        .args(["build", "--executor", "vm", "--vmm", "qemu"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown VMM"));
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
fn key_generate_ed25519_writes_0600_key() {
    let dir = tempfile::tempdir().unwrap();
    potter()
        .args(["key", "generate", "--ed25519"])
        .arg(dir.path())
        .assert()
        .success();
    let private = dir.path().join("catalog.key");
    let pem = std::fs::read_to_string(&private).unwrap();
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"), "{pem}");
    assert!(
        std::fs::read_to_string(dir.path().join("catalog.pub"))
            .unwrap()
            .contains("BEGIN PUBLIC KEY")
    );
    assert!(!dir.path().join("potter.key").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o600);
    }
    potter()
        .args(["key", "generate", "--ed25519"])
        .arg(dir.path())
        .assert()
        .code(2);
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

#[test]
fn the_vmm_comes_from_the_flag_or_the_environment() {
    // Accepted values get past parsing: off Linux they stop at the platform check, on Linux at
    // the missing kernel, both exit 4 and never the usage error 2.
    for (env, flag) in [
        (Some("cloud-hypervisor"), None),
        (Some("qemu"), Some("firecracker")),
        (None, Some("cloud-hypervisor")),
        (None, None),
    ] {
        let mut c = bare();
        if let Some(e) = env {
            c.env("POTTER_VMM", e);
        }
        c.args(["build", "--executor", "vm"]);
        if let Some(f) = flag {
            c.args(["--vmm", f]);
        }
        c.arg(fixture("explicit-go")).assert().code(4);
    }
    bare()
        .env("POTTER_VMM", "qemu")
        .args(["build", "--executor", "vm"])
        .arg(fixture("explicit-go"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown VMM"));
}

#[test]
fn the_default_vmm_is_firecracker() {
    bare()
        .args(["build", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[default: firecracker]"));
}

#[test]
fn a_rebase_dry_run_needs_no_vm_setup() {
    let kiln = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let run = |extra: &[&str]| {
        bare()
            .env("KILN_HOME", kiln.path())
            .env("POTTER_HOME", home.path())
            .args(["rebase", "--dry-run"])
            .args(extra)
            .arg("potter/none:latest")
            .output()
            .unwrap()
    };
    let with_vm = run(&["--executor", "vm"]);
    let without = run(&[]);
    assert_eq!(with_vm.status.code(), without.status.code());
    assert_eq!(with_vm.stderr, without.stderr);
    assert!(!String::from_utf8_lossy(&with_vm.stderr).contains("VM builds need"));
}
