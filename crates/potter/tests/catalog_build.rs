//! `potter catalog build` and `potter catalog publish` end to end (design §6): the two-recipe
//! catalog in `crates/potter-exec/tests/fixtures/catalog/` (a busybox `os-min` stand-in and a
//! `hello` layer that requires it), built with the namespace executor. No network.
#![cfg(target_os = "linux")]

mod support;

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use kiln_registry::testregistry::{Config, TestRegistry};
use potter_catalog::{Entry, Index};
use support::Ctx;

/// The fixture catalog copied to `dir/catalog`, its `os-min` on `base`.
fn catalog(dir: &Path, base: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../potter-exec/tests/fixtures/catalog");
    let dest = dir.join("catalog");
    for recipe in std::fs::read_dir(&src).unwrap() {
        let recipe = recipe.unwrap();
        let to = dest.join(recipe.file_name());
        std::fs::create_dir_all(&to).unwrap();
        for f in std::fs::read_dir(recipe.path()).unwrap() {
            let f = f.unwrap();
            let text = std::fs::read_to_string(f.path()).unwrap().replace("@BASE@", base);
            std::fs::write(to.join(f.file_name()), text).unwrap();
        }
    }
    dest
}

/// Rewrites `file` of recipe `name`, replacing `from` with `to`.
fn edit(catalog: &Path, name: &str, file: &str, from: &str, to: &str) {
    let path = catalog.join(name).join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{from:?} is not in {}", path.display());
    std::fs::write(&path, text.replace(from, to)).unwrap();
}

/// A copy of recipe `from` named `to`.
fn copy_recipe(catalog: &Path, from: &str, to: &str) {
    let dest = catalog.join(to);
    std::fs::create_dir_all(&dest).unwrap();
    for f in ["potter.toml", "layer.toml"] {
        let text = std::fs::read_to_string(catalog.join(from).join(f))
            .unwrap()
            .replace(&format!("target.{from}"), &format!("target.{to}"))
            .replace(&format!("name = \"{from}\""), &format!("name = \"{to}\""));
        std::fs::write(dest.join(f), text).unwrap();
    }
}

/// A catalog signing key: (private PEM path, public PEM path).
fn key(dir: &Path) -> (PathBuf, PathBuf) {
    let (private, public) = potter_catalog::generate_pem();
    let (k, p) = (dir.join("catalog.key"), dir.join("catalog.pub"));
    std::fs::write(&k, private).unwrap();
    std::fs::write(&p, public).unwrap();
    (k, p)
}

fn potter(ctx: &Ctx, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(args)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .env_remove("POTTER_CATALOG")
        .env_remove("POTTER_CATALOG_KEY")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `potter catalog build CATALOG --out OUT --sign-key KEY [--previous oci-layout:PREVIOUS]`,
/// trusting `public` for the previous catalog.
fn build(
    ctx: &Ctx,
    catalog: &Path,
    out: &Path,
    keys: &(PathBuf, PathBuf),
    previous: Option<&Path>,
) -> (i32, String, String) {
    let previous = previous.map(|p| format!("oci-layout:{}", p.display()));
    let mut args = vec![
        "catalog",
        "build",
        catalog.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--sign-key",
        keys.0.to_str().unwrap(),
    ];
    if let Some(p) = &previous {
        args.extend(["--previous", p.as_str()]);
    }
    potter(ctx, &args, &[("POTTER_CATALOG_KEY", keys.1.to_str().unwrap())])
}

/// The JSON a layout's `index.json` names `name`, and the blob it points at.
fn named(layout: &Path, name: &str) -> serde_json::Value {
    let index: serde_json::Value = serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap()).unwrap();
    let m = index["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["annotations"]["org.opencontainers.image.ref.name"] == name)
        .unwrap_or_else(|| panic!("{} names no {name}", layout.display()))
        .clone();
    serde_json::from_slice(&blob(layout, m["digest"].as_str().unwrap())).unwrap()
}

fn blob(layout: &Path, digest: &str) -> Vec<u8> {
    std::fs::read(
        layout
            .join("blobs/sha256")
            .join(digest.strip_prefix("sha256:").unwrap()),
    )
    .unwrap()
}

/// The index of the layout at `layout` (its signature is checked by the CLI tests below).
fn index(layout: &Path) -> Index {
    let m = named(layout, "index");
    Index::parse(&blob(layout, m["layers"][0]["digest"].as_str().unwrap())).unwrap()
}

fn current<'a>(index: &'a Index, name: &str) -> &'a Entry {
    index.current().find(|e| e.name == name).unwrap()
}

/// (path, is a regular file) for every entry of `e`'s zstd layer in `layout`.
fn layer_paths(layout: &Path, e: &Entry) -> Vec<(String, bool)> {
    let mut tar = Vec::new();
    zstd::Decoder::new(blob(layout, &e.digest).as_slice())
        .unwrap()
        .read_to_end(&mut tar)
        .unwrap();
    tar::Archive::new(tar.as_slice())
        .entries()
        .unwrap()
        .map(|en| {
            let en = en.unwrap();
            let regular = en.header().entry_type() == tar::EntryType::Regular;
            (
                en.path()
                    .unwrap()
                    .display()
                    .to_string()
                    .trim_end_matches('/')
                    .to_string(),
                regular,
            )
        })
        .collect()
}

fn position(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is not in:\n{haystack}"))
}

#[test]
fn driver_builds_in_requires_order() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (catalog(dir.path(), &base), dir.path().join("out"), key(dir.path()));
    let (code, stdout, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    // hello sorts first by name but requires os-min, which builds (and is smoke-tested) first.
    assert!(
        position(&stderr, "[os-min] release: running") < position(&stderr, "[hello] install: running"),
        "{stderr}"
    );
    assert!(
        position(&stderr, "[hello] install: running") < position(&stderr, "[os-min:smoke] smoke: running"),
        "{stderr}"
    );
    position(&stderr, "[hello:smoke] smoke: running");

    let index = index(&out);
    assert_eq!(index.serial, 1);
    let names: Vec<&str> = index.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["os-min", "hello"]);
    let hello = current(&index, "hello");
    assert_eq!(hello.requires, [potter_plan::CatalogRef::new("os-min", None)]);
    assert_eq!((hello.line.as_str(), hello.version.as_str()), ("1", "1.0.0"));
    assert_eq!(hello.kind, potter_catalog::Kind::Tool);
    assert_eq!(hello.env["PATH"], "/usr/local/bin");
    assert!(
        hello.fixes.is_empty(),
        "a first entry supersedes nothing, so it fixes nothing"
    );
    for e in &index.entries {
        assert_eq!(e.abi["os"], "debian-13", "{}", e.name);
        assert_eq!(e.abi["libc"], "glibc-2.41", "{}", e.name);
        assert_eq!(e.executor, "ns");
        assert_eq!(e.media_type, "application/vnd.oci.image.layer.v1.tar+zstd");
        assert_eq!(e.supersedes, None);
    }
    for e in &index.entries {
        assert!(
            stdout.contains(&format!("{} {} {} new", e.name, e.version, e.digest)),
            "{stdout}"
        );
    }
    assert!(
        stdout.contains(&format!(
            "serial 1, index {}",
            named(&out, "index")["layers"][0]["digest"].as_str().unwrap()
        )),
        "{stdout}"
    );

    // The layout is a catalog potter reads, under the signing key.
    let layout = format!("oci-layout:{}", out.display());
    let env = [
        ("POTTER_CATALOG", layout.as_str()),
        ("POTTER_CATALOG_KEY", keys.1.to_str().unwrap()),
    ];
    let (code, stdout, stderr) = potter(&ctx, &["catalog", "verify"], &env);
    assert_eq!((code, stdout.trim()), (0, "ok 2 layers"), "{stderr}");
    let (code, stdout, stderr) = potter(&ctx, &["catalog", "resolve", "hello", "--json"], &env);
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["constraints"], serde_json::json!(["hello@1"]));
    // No one else trusts the run's in-process key: the output is signed with the given key only.
    let (code, _, stderr) = potter(&ctx, &["catalog", "verify"], &env[..1]);
    assert_eq!(code, 3, "{stderr}");
}

#[test]
fn layer_holds_only_files_above_base() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (catalog(dir.path(), &base), dir.path().join("out"), key(dir.path()));
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    let index = index(&out);
    let hello = layer_paths(&out, current(&index, "hello"));
    let files: Vec<&str> = hello.iter().filter(|(_, f)| *f).map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        files,
        ["usr/local/bin/hello", "usr/share/potter-catalog/hello.json"],
        "{hello:?}"
    );
    // Nothing of os-min and none of the recipe's /app. The empty etc is where the sandbox
    // mounted resolv.conf for the networked step: every networked step's diff has it.
    let paths: Vec<&str> = hello.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "etc",
            "usr",
            "usr/local",
            "usr/local/bin",
            "usr/local/bin/hello",
            "usr/share",
            "usr/share/potter-catalog",
            "usr/share/potter-catalog/hello.json"
        ],
        "{hello:?}"
    );
    // os-min is its whole root, flattened: the busybox base and what its recipe added.
    let os: BTreeSet<String> = layer_paths(&out, current(&index, "os-min"))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    for p in [
        "bin/busybox",
        "bin/sh",
        "etc/passwd",
        "etc/os-release",
        "var/lib/dpkg/status",
    ] {
        assert!(os.contains(p), "{p} is not in os-min: {os:?}");
    }
    assert!(!os.iter().any(|p| p == "app" || p.starts_with("app/")), "{os:?}");
    // The layer's diff_id is its uncompressed tar's digest, and the build is deterministic.
    let e = current(&index, "hello");
    let mut tar = Vec::new();
    zstd::Decoder::new(blob(&out, &e.digest).as_slice())
        .unwrap()
        .read_to_end(&mut tar)
        .unwrap();
    assert_eq!(kiln_store::Digest::of(&tar).to_string(), e.diff_id);
    let again = dir.path().join("again");
    let (code, _, stderr) = build(&ctx, &cat, &again, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    let digests = |i: &Index| i.entries.iter().map(|e| e.digest.clone()).collect::<Vec<_>>();
    assert_eq!(digests(&self::index(&again)), digests(&index));
}

#[test]
fn overlap_is_refused_naming_layers() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (catalog(dir.path(), &base), dir.path().join("out"), key(dir.path()));
    // A second layer that installs the same /usr/local/bin/hello.
    copy_recipe(&cat, "hello", "hello2");
    edit(
        &cat,
        "hello2",
        "potter.toml",
        "755 /usr/local/bin/hello\"",
        "755 /usr/local/bin/hello && echo two > /usr/local/bin/hello2\"",
    );
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("hello and hello2 both hold 1 regular file: /usr/local/bin/hello"),
        "{stderr}"
    );
    assert!(!out.join("index.json").exists(), "nothing is written");
    // Layers that conflict are never installed together, so they may overlap.
    edit(
        &cat,
        "hello2",
        "layer.toml",
        "requires = [",
        "conflicts = [\"hello\"]\nrequires = [",
    );
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 0, "{stderr}");
}

#[test]
fn failed_smoke_fails_the_build() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (catalog(dir.path(), &base), dir.path().join("out"), key(dir.path()));
    edit(&cat, "hello", "layer.toml", "= hello\"", "= goodbye\"");
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("the smoke test of hello failed"), "{stderr}");
    assert!(!out.join("index.json").exists(), "nothing is written");
}

#[test]
fn a_recipe_error_exits_2_before_anything_builds() {
    let Some(ctx) = Ctx::new() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (
        catalog(dir.path(), "127.0.0.1:1/unused:1"),
        dir.path().join("out"),
        key(dir.path()),
    );
    edit(&cat, "hello", "layer.toml", "kind = ", "surprise = true\nkind = ");
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("surprise"), "{stderr}");
    assert!(!stderr.contains(": running"), "{stderr}");
    // A --sign-key that is not there is a usage error too.
    let gone = (dir.path().join("nope.key"), keys.1.clone());
    let (code, _, stderr) = build(&ctx, &cat, &out, &gone, None);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--sign-key"), "{stderr}");
}

#[test]
fn a_recipe_without_dpkg_status_fails_clearly() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (catalog(dir.path(), &base), dir.path().join("out"), key(dir.path()));
    edit(
        &cat,
        "os-min",
        "potter.toml",
        " > /var/lib/dpkg/status",
        " > /var/lib/dpkg/available",
    );
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("os-min has no /var/lib/dpkg/status"), "{stderr}");
}

#[test]
fn unchanged_layer_keeps_entry_changed_supersedes() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, keys) = (catalog(dir.path(), &base), key(dir.path()));
    let (v1, v2) = (dir.path().join("v1"), dir.path().join("v2"));
    let (code, _, stderr) = build(&ctx, &cat, &v1, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    let first = index(&v1);

    // A patched hello: new bytes, a new version; os-min is untouched.
    edit(&cat, "hello", "potter.toml", "echo hello\\\\n", "echo hello again\\\\n");
    edit(&cat, "hello", "layer.toml", "= hello\"", "= \\\"hello again\\\"\"");
    edit(&cat, "hello", "layer.toml", "1.0.0", "1.0.1");
    let (code, stdout, stderr) = build(&ctx, &cat, &v2, &keys, Some(&v1));
    assert_eq!(code, 0, "{stderr}");
    let second = index(&v2);
    assert_eq!(second.serial, 2);
    let (old_os, old_hello) = (current(&first, "os-min"), current(&first, "hello"));
    assert_eq!(
        current(&second, "os-min"),
        old_os,
        "the unchanged layer keeps its entry"
    );
    let hello = current(&second, "hello");
    assert_ne!(hello.digest, old_hello.digest);
    assert_eq!(hello.version, "1.0.1");
    assert_eq!(hello.supersedes.as_deref(), Some(old_hello.digest.as_str()));
    assert_eq!(
        hello.fixes,
        ["TEST-2026-0001"],
        "the recipe's fixes go on the superseding entry"
    );
    assert_eq!(
        second.by_digest(&old_hello.digest),
        Some(old_hello),
        "the superseded entry stays listed"
    );
    assert_eq!(second.entries.len(), 3);
    assert!(
        stdout.contains(&format!("os-min 13.0 {} kept", old_os.digest)),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("hello 1.0.1 {} supersedes {}", hello.digest, old_hello.digest)),
        "{stdout}"
    );
    // The new layout holds every current layer, the kept one too.
    let layout = format!("oci-layout:{}", v2.display());
    let env = [
        ("POTTER_CATALOG", layout.as_str()),
        ("POTTER_CATALOG_KEY", keys.1.to_str().unwrap()),
    ];
    let (code, stdout, stderr) = potter(&ctx, &["catalog", "verify"], &env);
    assert_eq!((code, stdout.trim()), (0, "ok 2 layers"), "{stderr}");

    // A previous catalog that does not verify is refused before anything builds.
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = key(&other);
    let (code, _, stderr) = build(&ctx, &cat, &dir.path().join("v3"), &other, Some(&v1));
    assert_eq!(code, 3, "{stderr}");
    assert!(!stderr.contains(": running"), "{stderr}");
}

#[test]
fn serial_increments() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, keys, out) = (catalog(dir.path(), &base), key(dir.path()), dir.path().join("out"));
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    let first = index(&out);
    // CI's flow: the previous catalog is the layout being written.
    for serial in [2, 3] {
        let (code, stdout, stderr) = build(&ctx, &cat, &out, &keys, Some(&out));
        assert_eq!(code, 0, "{stderr}");
        let now = index(&out);
        assert_eq!(now.serial, serial);
        assert_eq!(now.entries, first.entries, "nothing changed, so every entry is kept");
        assert_eq!(stdout.matches(" kept").count(), 2, "{stdout}");
    }
    // Every index stays fetchable by its digest.
    let pinned = std::fs::read_to_string(out.join("index.json")).unwrap();
    assert_eq!(pinned.matches("\"index-").count(), 3, "{pinned}");
}

#[test]
fn publish_refuses_unsigned_layout() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, keys, out) = (catalog(dir.path(), &base), key(dir.path()), dir.path().join("out"));
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    let reg = TestRegistry::start(Config::default());
    let to = format!("{}/acme/catalog:index", reg.host());
    let publish = |env: &[(&str, &str)]| potter(&ctx, &["catalog", "publish", out.to_str().unwrap(), &to], env);
    let trusted = [("POTTER_CATALOG_KEY", keys.1.to_str().unwrap())];

    // Signed by a key nobody trusts.
    let (code, _, stderr) = publish(&[]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("not trusted"), "{stderr}");

    // Trusted: pushed, and potter reads it from the registry.
    let (code, stdout, stderr) = publish(&trusted);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains(&format!("pushed {to} serial 1")), "{stdout}");
    let env = [("POTTER_CATALOG", to.as_str()), trusted[0]];
    let (code, stdout, stderr) = potter(&ctx, &["catalog", "verify"], &env);
    assert_eq!((code, stdout.trim()), (0, "ok 2 layers"), "{stderr}");

    // The index manifest's signature removed: refused, and nothing is pushed.
    let index_json = out.join("index.json");
    let mut idx: serde_json::Value = serde_json::from_slice(&std::fs::read(&index_json).unwrap()).unwrap();
    for m in idx["manifests"].as_array_mut().unwrap() {
        let name = m["annotations"]["org.opencontainers.image.ref.name"]
            .as_str()
            .unwrap()
            .to_string();
        if name != "index" {
            continue;
        }
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&blob(&out, m["digest"].as_str().unwrap())).unwrap();
        manifest.as_object_mut().unwrap().remove("annotations");
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let d = kiln_store::Digest::of(&bytes);
        std::fs::write(out.join("blobs/sha256").join(d.hex()), &bytes).unwrap();
        m["digest"] = d.to_string().into();
        m["size"] = bytes.len().into();
    }
    std::fs::write(&index_json, serde_json::to_vec(&idx).unwrap()).unwrap();
    let other = TestRegistry::start(Config::default());
    let to = format!("{}/acme/catalog:index", other.host());
    let (code, _, stderr) = potter(&ctx, &["catalog", "publish", out.to_str().unwrap(), &to], &trusted);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("not signed"), "{stderr}");
    let e = index(&out).entries[0].clone();
    assert!(
        !other.has_blob(&kiln_store::Digest::parse(&e.digest).unwrap()),
        "nothing was pushed"
    );
}

/// Every published piece of an entry's metadata is in its layer's bytes, at
/// /usr/share/potter-catalog/<name>.json: a change to it is a new digest and a superseding entry.
#[test]
fn metadata_changes_supersede() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, keys) = (catalog(dir.path(), &base), key(dir.path()));
    let (v1, v2, v3) = (dir.path().join("v1"), dir.path().join("v2"), dir.path().join("v3"));
    let (code, _, stderr) = build(&ctx, &cat, &v1, &keys, None);
    assert_eq!(code, 0, "{stderr}");
    let first = index(&v1);
    let meta: serde_json::Value = {
        let e = current(&first, "hello");
        let mut tar = Vec::new();
        zstd::Decoder::new(blob(&v1, &e.digest).as_slice())
            .unwrap()
            .read_to_end(&mut tar)
            .unwrap();
        let mut a = tar::Archive::new(tar.as_slice());
        let mut f = a
            .entries()
            .unwrap()
            .map(|en| en.unwrap())
            .find(|en| en.path().unwrap().to_str() == Some("usr/share/potter-catalog/hello.json"))
            .expect("the layer holds its metadata");
        let mut text = String::new();
        f.read_to_string(&mut text).unwrap();
        serde_json::from_str(&text).unwrap()
    };
    assert_eq!(meta["name"], "hello");
    assert_eq!(meta["abi"]["os"], "debian-13");
    assert_eq!(meta["requires"], serde_json::json!([{"name": "os-min"}]));
    assert!(meta.get("fixes").is_none() && meta.get("executor").is_none(), "{meta}");

    // A conflicts-only change: hello supersedes, os-min is kept.
    edit(
        &cat,
        "hello",
        "layer.toml",
        "requires = [",
        "conflicts = [\"goodbye\"]\nrequires = [",
    );
    let (code, stdout, stderr) = build(&ctx, &cat, &v2, &keys, Some(&v1));
    assert_eq!(code, 0, "{stderr}");
    let second = index(&v2);
    let hello = current(&second, "hello");
    assert_eq!(hello.conflicts, ["goodbye"]);
    assert_eq!(
        hello.supersedes.as_deref(),
        Some(current(&first, "hello").digest.as_str())
    );
    assert_eq!(current(&second, "os-min"), current(&first, "os-min"));
    assert!(stdout.contains(" kept") && stdout.contains(" supersedes "), "{stdout}");

    // os-min's ABI changes: os-min and every layer on it supersede.
    edit(
        &cat,
        "os-min",
        "potter.toml",
        "VERSION_ID=\\\"13\\\"",
        "VERSION_ID=\\\"14\\\"",
    );
    let (code, stdout, stderr) = build(&ctx, &cat, &v3, &keys, Some(&v2));
    assert_eq!(code, 0, "{stderr}");
    let third = index(&v3);
    for name in ["os-min", "hello"] {
        let e = current(&third, name);
        assert_eq!(e.abi["os"], "debian-14", "{name}");
        assert_eq!(
            e.supersedes.as_deref(),
            Some(current(&second, name).digest.as_str()),
            "{name}"
        );
    }
    assert!(!stdout.contains(" kept"), "{stdout}");
}

/// A layer cannot delete what the layers under it hold: a step that does is refused.
#[test]
fn deleting_a_base_file_is_refused() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _base, base) = support::serve_busybox();
    let dir = tempfile::tempdir().unwrap();
    let (cat, out, keys) = (catalog(dir.path(), &base), dir.path().join("out"), key(dir.path()));
    edit(
        &cat,
        "hello",
        "potter.toml",
        "755 /usr/local/bin/hello\"",
        "755 /usr/local/bin/hello && rm /etc/motd\"",
    );
    let (code, _, stderr) = build(&ctx, &cat, &out, &keys, None);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("hello deletes 1 path the layers it builds on hold"),
        "{stderr}"
    );
    assert!(stderr.contains("/etc/motd"), "{stderr}");
    assert!(!out.join("index.json").exists());
}
