//! `potter catalog ls | resolve | verify | affected` against a signed OCI-layout catalog.
//! None of them needs the sandbox.

mod support;

use assert_cmd::Command;
use kiln_erofs::testtar::{Opts, TarBuilder};
use potter_catalog::Kind;
use support::catalog::{Layer, TestCatalog};

fn tar(marker: &str) -> Vec<u8> {
    TarBuilder::new()
        .file("etc/marker", marker.as_bytes(), &Opts::default())
        .finish()
}

fn short(digest: &str) -> &str {
    &digest[7..19]
}

fn catalog() -> TestCatalog {
    TestCatalog::new(vec![
        Layer::new("os-min", "13", Kind::Base, tar("os")),
        Layer::new("go", "1.24", Kind::Runtime, tar("go124")),
        Layer::new("node", "22", Kind::Runtime, tar("node22")),
    ])
}

fn potter(c: &TestCatalog, args: &[&str]) -> (i32, String, String) {
    let home = c.dir.path().join("home");
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(args)
        .env("KILN_HOME", home.join("kiln"))
        .env("POTTER_HOME", home.join("potter"))
        .envs(c.env())
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn ls_lists_current_entries() {
    let c = catalog();
    let (code, out, err) = potter(&c, &["catalog", "ls"]);
    assert_eq!(code, 0, "{err}");
    let header = out.lines().next().unwrap();
    assert_eq!(
        header.split_whitespace().collect::<Vec<_>>(),
        ["NAME", "LINE", "VERSION", "ARCH", "KIND", "EXECUTOR", "DIGEST"]
    );
    let go = out.lines().find(|l| l.starts_with("go ")).unwrap();
    assert!(
        go.contains("1.24") && go.contains("runtime") && go.contains("ns"),
        "{go}"
    );
    assert!(go.contains(short(&c.current("go").digest)), "{go}");
    assert_eq!(out.lines().count(), 4, "{out}");
}

#[test]
fn ls_all_marks_superseded() {
    let mut c = catalog();
    let old = c.current("go").digest.clone();
    c.publish(
        2,
        vec![Layer::new("go", "1.24", Kind::Runtime, tar("go124b")).supersedes(&old)],
    );
    let (_, plain, _) = potter(&c, &["catalog", "ls"]);
    assert!(!plain.contains(short(&old)), "{plain}");
    let (code, all, err) = potter(&c, &["catalog", "ls", "--all"]);
    assert_eq!(code, 0, "{err}");
    let new = c.current("go").digest.clone();
    let line = all.lines().find(|l| l.contains(short(&old))).unwrap();
    assert!(line.contains(&format!("superseded by {}", short(&new))), "{line}");
    assert_eq!(all.matches("superseded by").count(), 1, "{all}");
    let (_, json, _) = potter(&c, &["catalog", "ls", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 3);
}

#[test]
fn resolve_prints_order_and_key() {
    let c = catalog();
    let (code, out, err) = potter(&c, &["catalog", "resolve", "os-min", "go@1.24"]);
    assert_eq!(code, 0, "{err}");
    let names: Vec<&str> = out
        .lines()
        .filter(|l| !l.starts_with("key") && !l.starts_with("NAME"))
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(names, ["os-min", "go"], "{out}");
    let key = out.lines().find_map(|l| l.strip_prefix("key ")).unwrap().trim();
    assert_eq!(key.len(), 64, "{out}");
    let (_, json, _) = potter(&c, &["catalog", "resolve", "os-min", "go@1.24", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["key"], key);
    assert_eq!(v["entries"].as_array().unwrap().len(), 2);
}

#[test]
fn resolve_unsatisfied_exits_2() {
    let c = catalog();
    let (code, _, err) = potter(&c, &["catalog", "resolve", "go@9.9"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("no go@9.9"), "{err}");
    let (code, _, _) = potter(&c, &["catalog", "ls", "--arch", "riscv"]);
    assert_eq!(code, 2);
}

#[test]
fn verify_ok_counts_layers() {
    let c = catalog();
    let (code, out, err) = potter(&c, &["catalog", "verify"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "ok 3 layers");
}

#[test]
fn verify_tampered_exits_3() {
    let c = catalog();
    let blob = c
        .dir
        .path()
        .join("layout/blobs/sha256")
        .join(c.current("go").digest.strip_prefix("sha256:").unwrap());
    let mut bytes = std::fs::read(&blob).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&blob, bytes).unwrap();
    let (code, _, err) = potter(&c, &["catalog", "verify"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("error: catalog is hostile"), "{err}");
}

#[test]
fn affected_by_advisory_and_by_digest() {
    let mut c = TestCatalog::new(vec![Layer::new("go", "1.24", Kind::Runtime, tar("v1"))]);
    let v1 = c.current("go").digest.clone();
    c.publish(
        2,
        vec![
            Layer::new("go", "1.24", Kind::Runtime, tar("v2"))
                .supersedes(&v1)
                .fixes("CVE-2026-1"),
        ],
    );
    let v2 = c.current("go").digest.clone();
    c.publish(
        3,
        vec![Layer::new("go", "1.24", Kind::Runtime, tar("v3")).supersedes(&v2)],
    );
    let v3 = c.current("go").digest.clone();

    let (code, out, err) = potter(&c, &["catalog", "affected", "CVE-2026-1"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(short(&v2)) && !out.contains(short(&v1)) && !out.contains(short(&v3)),
        "{out}"
    );

    let (code, out, err) = potter(&c, &["catalog", "affected", &v2]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(short(&v1)) && out.contains(short(&v3)), "{out}");
    let (_, out, _) = potter(&c, &["catalog", "affected", &v1]);
    assert!(out.contains(short(&v2)) && out.contains(short(&v3)), "{out}");
}

#[test]
fn override_env_prints_note() {
    let c = catalog();
    let (code, _, err) = potter(&c, &["catalog", "ls"]);
    assert_eq!(code, 0);
    assert!(err.contains("note: using the catalog at oci-layout:"), "{err}");
}
