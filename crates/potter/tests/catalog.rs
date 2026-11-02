//! `potter catalog ls | resolve | verify | affected` against a signed OCI-layout catalog.
//! None of them needs the sandbox.

mod support;

use std::path::{Path, PathBuf};

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

#[test]
fn verify_rehashes_a_corrupt_stored_layer() {
    let c = catalog();
    let (code, _, err) = potter(&c, &["catalog", "verify"]);
    assert_eq!(code, 0, "{err}");
    let hex = c.current("go").digest.strip_prefix("sha256:").unwrap().to_string();
    let stored = c.dir.path().join("home/kiln").join("blobs/sha256").join(&hex);
    assert!(stored.is_file(), "{}", stored.display());
    let mut bytes = std::fs::read(&stored).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&stored, bytes).unwrap();
    let (code, _, err) = potter(&c, &["catalog", "verify"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("hashes to"), "{err}");
}

#[test]
fn control_characters_in_errors_are_escaped() {
    let c = catalog();
    let (code, _, err) = potter(&c, &["catalog", "resolve", "go\u{1b}[31m@9"]);
    assert_eq!(code, 2);
    assert!(!err.contains('\u{1b}'), "{err:?}");
    assert!(err.contains("\\u{1b}[31m"), "{err:?}");
    let (code, _, err) = potter(&c, &["catalog", "affected", "sha256:\u{7}"]);
    assert_eq!(code, 2);
    assert!(!err.contains('\u{7}') && err.contains("\\u{7}"), "{err:?}");
}

#[test]
fn unreachable_catalog_exits_4() {
    let c = catalog();
    let missing = c.dir.path().join("nowhere");
    let home = c.dir.path().join("home");
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["catalog", "ls"])
        .env("KILN_HOME", home.join("kiln"))
        .env("POTTER_HOME", home.join("potter"))
        .env("POTTER_CATALOG", format!("oci-layout:{}", missing.display()))
        .env("POTTER_CATALOG_KEY", &c.key)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn affected_advisory_without_match_prints_header_only() {
    let c = catalog();
    let (code, out, err) = potter(&c, &["catalog", "affected", "CVE-0000-0"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.starts_with("NAME"));
}

/// One signed layout per call: the index and the tar of every entry it lists.
struct Layouts {
    dir: tempfile::TempDir,
    tars: std::collections::BTreeMap<String, Vec<u8>>,
}

impl Layouts {
    fn new() -> Self {
        Layouts {
            dir: tempfile::tempdir().unwrap(),
            tars: Default::default(),
        }
    }

    /// A key pair under `name`: (private PEM path, public PEM path).
    fn key(&self, name: &str) -> (PathBuf, PathBuf) {
        let (private, public) = potter_catalog::generate_pem();
        let (k, p) = (
            self.dir.path().join(format!("{name}.key")),
            self.dir.path().join(format!("{name}.pub")),
        );
        std::fs::write(&k, private).unwrap();
        std::fs::write(&p, public).unwrap();
        (k, p)
    }

    fn entry(&mut self, name: &str, arch: potter_catalog::Arch, marker: &str) -> potter_catalog::Entry {
        let tar = tar(marker);
        let digest = kiln_store::Digest::of(&tar).to_string();
        let e = potter_catalog::Entry {
            name: name.into(),
            line: "1".into(),
            version: "1.0".into(),
            arch,
            kind: Kind::Runtime,
            digest: digest.clone(),
            diff_id: digest.clone(),
            size: tar.len() as u64,
            media_type: kiln_oci::media::OCI_LAYER_TAR.into(),
            abi: Default::default(),
            requires: vec![],
            conflicts: vec![],
            env: Default::default(),
            provides_sonames: vec![],
            executor: "ns".into(),
            source: "https://example.test".into(),
            license: "MIT".into(),
            supersedes: None,
            fixes: vec![],
        };
        self.tars.insert(digest, tar);
        e
    }

    /// Writes `entries` at `serial` to the layout `name`, signed with `key`, holding every
    /// current entry's layer.
    fn write(&self, name: &str, serial: u64, entries: Vec<potter_catalog::Entry>, key: &Path) -> PathBuf {
        use sha2::Digest as _;
        let index = potter_catalog::Index {
            schema: potter_catalog::SCHEMA.into(),
            serial,
            created: "2026-11-01T00:00:00Z".into(),
            entries,
        };
        let blobs = self.dir.path().join("tars");
        std::fs::create_dir_all(&blobs).unwrap();
        let layers: Vec<_> = index
            .current()
            .map(|e| {
                let path = blobs.join(&e.digest[7..]);
                std::fs::write(&path, &self.tars[&e.digest]).unwrap();
                let d = kiln_store::Digest::parse(&e.digest).unwrap();
                (kiln_oci::Descriptor::new(&e.media_type, d, e.size), path)
            })
            .collect();
        let blob = index.canonical();
        let sha: [u8; 32] = sha2::Sha256::digest(&blob).into();
        let signing = potter_catalog::signing_key_from_pem(&std::fs::read_to_string(key).unwrap()).unwrap();
        let (id, sig) = potter_catalog::sign(&sha, &signing);
        let dir = self.dir.path().join(name);
        potter_catalog::write_layout(&dir, &blob, &id, &sig, &layers).unwrap();
        dir
    }
}

/// The index a layout's `index` tag names, unverified.
fn layout_index(dir: &Path) -> potter_catalog::Index {
    let blob = |d: &str| std::fs::read(dir.join("blobs/sha256").join(&d[7..])).unwrap();
    let top: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
    let m = top["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["annotations"]["org.opencontainers.image.ref.name"] == potter_catalog::INDEX_TAG)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&blob(m["digest"].as_str().unwrap())).unwrap();
    potter_catalog::Index::parse(&blob(manifest["layers"][0]["digest"].as_str().unwrap())).unwrap()
}

fn potter_env(home: &Path, args: &[&str], env: &[(&str, &Path)]) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(args)
        .env("KILN_HOME", home.join("kiln"))
        .env("POTTER_HOME", home.join("potter"))
        .env_remove("POTTER_CATALOG")
        .env_remove("POTTER_CATALOG_KEY")
        .envs(env.iter().map(|(k, v)| (*k, v.as_os_str())))
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A proxy in front of `backend` that refuses (with `status`) every read of the manifest at
/// `/v2/<repo>/manifests/<tag>` until a manifest has been put there, as ghcr answers a read of a
/// package that does not exist yet with 401/403 "denied" rather than 404. Everything else is
/// passed through. Returns the proxy's host:port.
fn deny_until_pushed(backend: String, manifest: String, status: u16) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let pushed = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut client) = client else { continue };
            let (backend, manifest, pushed) = (backend.clone(), manifest.clone(), pushed.clone());
            std::thread::spawn(move || {
                // The request line and headers; the body (if any) is piped through below.
                let mut reader = BufReader::new(client.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut words = head.split_whitespace();
                let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
                if path == manifest {
                    if matches!(method, "GET" | "HEAD") && !pushed.load(Ordering::SeqCst) {
                        let body =
                            r#"{"errors":[{"code":"DENIED","message":"requested access to the resource is denied"}]}"#;
                        let reply = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            if method == "HEAD" { "" } else { body }
                        );
                        let _ = client.write_all(reply.as_bytes());
                        return;
                    }
                    if method == "PUT" {
                        pushed.store(true, Ordering::SeqCst);
                    }
                }
                let mut upstream = TcpStream::connect(&backend).unwrap();
                upstream.write_all(head.as_bytes()).unwrap();
                upstream.write_all(reader.buffer()).unwrap();
                let mut to_upstream = upstream.try_clone().unwrap();
                let mut from_client = client.try_clone().unwrap();
                std::thread::spawn(move || std::io::copy(&mut from_client, &mut to_upstream));
                let _ = std::io::copy(&mut upstream, &mut client);
                let _ = client.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    host
}

/// `potter catalog publish --first`: a target the registry refuses to read (401/403, ghcr's
/// answer for a package that does not exist yet) counts as empty, so the very first publish
/// goes through; without `--first` that refusal is exit 4. A target that does serve an index is
/// still only replaced by a layout that follows it.
#[test]
fn publish_first_takes_a_denied_target() {
    use kiln_registry::testregistry::{Config, TestRegistry};
    use potter_catalog::Arch;
    let mut l = Layouts::new();
    let (key, public) = l.key("catalog");
    let go = l.entry("go", Arch::X86_64, "go");
    let first = l.write("first", 1, vec![go.clone()], &key);
    let trust = [("POTTER_CATALOG_KEY", public.as_path())];
    for status in [401, 403] {
        let reg = TestRegistry::start(Config::default());
        let proxy = deny_until_pushed(reg.host(), "/v2/acme/catalog/manifests/index".into(), status);
        let to = format!("{proxy}/acme/catalog:index");
        let home = l.dir.path().join(format!("home-{status}"));
        let publish = |layout: &Path, extra: &[&str]| {
            let mut args = vec!["catalog", "publish", layout.to_str().unwrap(), &to];
            args.extend_from_slice(extra);
            potter_env(&home, &args, &trust)
        };

        // Without --first the refusal is an error, and nothing is pushed.
        let (code, _, stderr) = publish(&first, &[]);
        assert_eq!(code, 4, "{status}: {stderr}");
        assert!(stderr.contains(&format!("HTTP {status}")), "{stderr}");
        let d = kiln_store::Digest::parse(&go.digest).unwrap();
        assert!(!reg.has_blob(&d), "nothing was pushed");

        // With --first it publishes, and potter reads it back from the target.
        let (code, stdout, stderr) = publish(&first, &["--first"]);
        assert_eq!(code, 0, "{status}: {stderr}");
        assert!(stdout.contains("serial 1"), "{stdout}");
        let at = PathBuf::from(&to);
        let env = [("POTTER_CATALOG", at.as_path()), trust[0]];
        let (code, stdout, stderr) = potter_env(&home.join("reader"), &["catalog", "verify"], &env);
        assert_eq!((code, stdout.trim()), (0, "ok 1 layers"), "{stderr}");

        // Now there is an index: --first still refuses to replace it with one it does not follow.
        let (code, _, stderr) = publish(&first, &["--first"]);
        assert_eq!(code, 2, "{stderr}");
        assert!(stderr.contains("would roll it back"), "{stderr}");
    }
}

/// `potter catalog build --merge`: the two architectures' layouts of one run, each built on the
/// same previous catalog, become one index one serial above it, signed once, holding both
/// architectures' new entries and every current layer.
#[test]
fn merge_makes_one_index_from_both_architectures() {
    use potter_catalog::Arch;
    let mut l = Layouts::new();
    let (catalog_key, catalog_pub) = l.key("catalog");
    let old_go = l.entry("go", Arch::X86_64, "go-x86-old");
    let arm_go = l.entry("go", Arch::Aarch64, "go-arm");
    let previous = vec![old_go.clone(), arm_go.clone()];
    let prev_dir = l.write("previous", 4, previous.clone(), &catalog_key);

    // x86_64 rebuilt go; aarch64 added node. Each build signed with a throwaway key.
    let mut new_go = l.entry("go", Arch::X86_64, "go-x86-new");
    new_go.supersedes = Some(old_go.digest.clone());
    let node = l.entry("node", Arch::Aarch64, "node-arm");
    let (x86_key, x86_pub) = l.key("x86");
    let (arm_key, arm_pub) = l.key("arm");
    let x86 = l.write("x86", 5, [previous.clone(), vec![new_go.clone()]].concat(), &x86_key);
    let arm = l.write("arm", 5, [previous.clone(), vec![node.clone()]].concat(), &arm_key);

    let home = l.dir.path().join("home");
    let out = l.dir.path().join("merged");
    let previous_ref = format!("oci-layout:{}", prev_dir.display());
    let args = |layouts: &[&Path], keys: &[&Path]| {
        let mut a: Vec<String> = ["catalog", "build", "--out", out.to_str().unwrap()]
            .into_iter()
            .map(String::from)
            .collect();
        a.extend(["--sign-key".into(), catalog_key.display().to_string()]);
        a.extend(["--previous".into(), previous_ref.clone()]);
        for d in layouts {
            a.extend(["--merge".into(), d.display().to_string()]);
        }
        for k in keys {
            a.extend(["--merge-key".into(), k.display().to_string()]);
        }
        a
    };
    let trust = [("POTTER_CATALOG_KEY", catalog_pub.as_path())];
    let run = |a: Vec<String>| potter_env(&home, &a.iter().map(String::as_str).collect::<Vec<_>>(), &trust);

    let (code, stdout, stderr) = run(args(&[&x86, &arm], &[&x86_pub, &arm_pub]));
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("serial 5"), "{stdout}");
    assert!(
        stdout.contains(&format!("go 1.0 {} supersedes {}", new_go.digest, old_go.digest)),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("node 1.0 {} new", node.digest)), "{stdout}");

    let merged = layout_index(&out);
    assert_eq!(merged.serial, 5, "one bump for both architectures");
    assert_eq!(
        merged.entries,
        [previous.clone(), vec![new_go.clone(), node.clone()]].concat()
    );
    let current: Vec<(Arch, &str)> = merged.current().map(|e| (e.arch, e.name.as_str())).collect();
    assert_eq!(
        current,
        [(Arch::Aarch64, "go"), (Arch::X86_64, "go"), (Arch::Aarch64, "node")]
    );

    // It verifies under the catalog key, and holds every current layer.
    let at = [
        ("POTTER_CATALOG", PathBuf::from(format!("oci-layout:{}", out.display()))),
        ("POTTER_CATALOG_KEY", catalog_pub.clone()),
    ];
    let at: Vec<(&str, &Path)> = at.iter().map(|(k, v)| (*k, v.as_path())).collect();
    let (code, stdout, stderr) = potter_env(&l.dir.path().join("home2"), &["catalog", "verify"], &at);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "ok 3 layers");

    // A layout whose key is not given does not verify (exit 3); two layouts of one arch, or one
    // built on another catalog, are refused (exit 2).
    let (code, _, stderr) = run(args(&[&x86, &arm], &[&x86_pub]));
    assert_eq!(code, 3, "{stderr}");
    let (code, _, stderr) = run(args(&[&x86, &x86], &[&x86_pub]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("both add x86_64"), "{stderr}");
    let stale = l.write("stale", 4, previous[..1].to_vec(), &arm_key);
    let (code, _, stderr) = run(args(&[&x86, &stale], &[&x86_pub, &arm_pub]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("serial 4, not 5"), "{stderr}");
    // --merge and a catalog directory are one or the other.
    let mut both = args(&[&x86], &[&x86_pub]);
    both.push("catalog".into());
    let (code, _, _) = run(both);
    assert_eq!(code, 2);
}

/// `potter key generate --ed25519` makes the directory it writes into.
#[test]
fn key_generate_makes_its_directory() {
    let l = Layouts::new();
    let dir = l.dir.path().join("not/there/yet");
    let (code, stdout, stderr) = potter_env(
        &l.dir.path().join("home"),
        &["key", "generate", "--ed25519", dir.to_str().unwrap()],
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), dir.join("catalog.pub").display().to_string());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.join("catalog.key")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    potter_catalog::signing_key_from_pem(&std::fs::read_to_string(dir.join("catalog.key")).unwrap()).unwrap();
    // The key files are never replaced.
    let (code, _, stderr) = potter_env(
        &l.dir.path().join("home"),
        &["key", "generate", "--ed25519", dir.to_str().unwrap()],
        &[],
    );
    assert_eq!(code, 2, "{stderr}");
}

/// `potter catalog publish` onto a registry: an empty target takes any verified layout; one that
/// already holds an index takes only a layout that follows it (a higher serial, every entry
/// still listed), and one whose index does not verify is refused.
#[test]
fn publish_never_rolls_the_target_back() {
    use kiln_registry::testregistry::{Config, TestRegistry};
    use potter_catalog::Arch;
    let mut l = Layouts::new();
    let (key, public) = l.key("catalog");
    let go = l.entry("go", Arch::X86_64, "go");
    let node = l.entry("node", Arch::X86_64, "node");
    let first = l.write("first", 1, vec![go.clone()], &key);
    let second = l.write("second", 2, vec![go.clone(), node.clone()], &key);
    let dropped = l.write("dropped", 3, vec![node.clone()], &key);
    let reg = TestRegistry::start(Config::default());
    let to = format!("{}/acme/catalog:index", reg.host());
    let home = l.dir.path().join("home");
    let trust = [("POTTER_CATALOG_KEY", public.as_path())];
    let publish = |layout: &Path, env: &[(&str, &Path)]| {
        potter_env(&home, &["catalog", "publish", layout.to_str().unwrap(), &to], env)
    };

    let (code, stdout, stderr) = publish(&first, &trust);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("serial 1"), "{stdout}");
    // The same serial again, or a lower one: refused.
    let (code, _, stderr) = publish(&first, &trust);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("at serial 1; publishing serial 1 over it would roll it back"),
        "{stderr}"
    );
    let (code, _, stderr) = publish(&second, &trust);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = publish(&first, &trust);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("would roll it back"), "{stderr}");
    // A higher serial that drops an entry: refused, and the target keeps serial 2.
    let (code, _, stderr) = publish(&dropped, &trust);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("does not list its entries"), "{stderr}");
    let at = PathBuf::from(&to);
    let env = [("POTTER_CATALOG", at.as_path()), trust[0]];
    let (code, stdout, stderr) = potter_env(&l.dir.path().join("home2"), &["catalog", "ls", "--all"], &env);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.lines().count(), 3, "{stdout}");

    // The target's index is signed by a key this process does not trust: fail closed.
    let (other_key, other_pub) = l.key("other");
    let foreign = l.write("foreign", 9, vec![go.clone(), node.clone()], &other_key);
    let (code, _, stderr) = publish(&foreign, &[("POTTER_CATALOG_KEY", other_pub.as_path())]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("not trusted"), "{stderr}");
}
