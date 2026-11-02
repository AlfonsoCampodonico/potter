//! The real catalog end to end (`catalog/`, built by `potter catalog build`): each project under
//! `tests/fixtures/catalog-e2e/` is detected, built with the namespace executor on it, and its
//! `web` process run once until it listens. Also a real `apt` install on a catalog chain, and a
//! rebase of an image with an `apt` runtime step onto a superseding catalog layer.
//!
//! Only with POTTER_TEST_CATALOG=1, reading the catalog from POTTER_CATALOG and its key from
//! POTTER_CATALOG_KEY. The builds reach package registries and snapshot.debian.org.
#![cfg(target_os = "linux")]

mod support;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use kiln_oci::{Descriptor, ImageIndex, ImageManifest, media};
use kiln_store::Digest;
use potter_catalog::{Entry, INDEX_TAG, Index, SigningKey};
use sha2::Digest as _;
use support::Ctx;

/// The snapshot.debian.org date the catalog's Debian packages come from.
const SNAPSHOT: &str = "20261030T000000Z";

/// The catalog to build on, as (POTTER_CATALOG, POTTER_CATALOG_KEY), when the tests are on.
fn real_catalog() -> Option<(String, String)> {
    if std::env::var_os("POTTER_TEST_CATALOG").is_none_or(|v| v != "1") {
        return None;
    }
    let (Ok(catalog), Ok(key)) = (std::env::var("POTTER_CATALOG"), std::env::var("POTTER_CATALOG_KEY")) else {
        panic!("POTTER_TEST_CATALOG=1 needs POTTER_CATALOG and POTTER_CATALOG_KEY");
    };
    Some((catalog, key))
}

fn potter(ctx: &Ctx, args: &[&str], catalog: &(String, String)) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(args)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .env("POTTER_CATALOG", &catalog.0)
        .env("POTTER_CATALOG_KEY", &catalog.1)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

/// A copy of the fixture project `name`.
fn fixture(name: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/catalog-e2e")
        .join(name);
    copy_dir(&src, repo.path());
    repo
}

/// `potter build --executor ns` of `repo`, which must succeed.
fn build(ctx: &Ctx, repo: &Path, catalog: &(String, String)) {
    let (code, stdout, stderr) = potter(ctx, &["build", "--executor", "ns", repo.to_str().unwrap()], catalog);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("potter/app:latest sha256:"), "{stdout}");
}

/// The OCI manifest `image` was converted from.
fn source_manifest(ctx: &Ctx, image: &str) -> (ImageManifest, kiln_image::types::Process) {
    let digest = kiln_image::resolve_name(&ctx.store, image).unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    let config = &loaded.entries[0].1.config;
    let manifest = serde_json::from_slice(&ctx.store.read_metadata(&config.source.manifest_digest).unwrap()).unwrap();
    (manifest, config.process.clone())
}

fn annotation(manifest: &ImageManifest, key: &str) -> String {
    manifest
        .annotations
        .as_ref()
        .and_then(|a| a.get(key))
        .unwrap_or_else(|| panic!("the image has no {key}"))
        .clone()
}

/// Runs `script` as `/bin/sh -c script web <process...>` in `image`'s root, with the image's
/// environment, working directory and user, and no network. Returns its status and output.
fn run_in(ctx: &Ctx, image: &str, process: &[String], script: &str) -> (bool, Vec<String>) {
    let (manifest, config) = source_manifest(ctx, image);
    let mut lowers: Vec<PathBuf> = manifest
        .layers
        .iter()
        .map(|l| potter_exec::unpack::ensure_unpacked(&ctx.sandbox, &ctx.store, l, &ctx.home).unwrap())
        .collect();
    lowers.reverse();
    let (uid, gid) = match config.user.as_deref().unwrap_or("root") {
        "root" => (0, 0),
        ids => {
            let (u, g) = ids.split_once(':').unwrap_or((ids, ids));
            (u.parse().unwrap(), g.parse().unwrap())
        }
    };
    let env = config
        .env
        .iter()
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "web".to_string(),
    ];
    argv.extend(process.iter().cloned());
    let runs = tempfile::Builder::new()
        .prefix("serve-")
        .tempdir_in(ctx.home.runs())
        .unwrap()
        .keep();
    for d in ["upper", "work", "run"] {
        std::fs::create_dir_all(runs.join(d)).unwrap();
    }
    let mut lines = Vec::new();
    let mut log = |l: &str| lines.push(l.to_string());
    let outcome = potter_exec::ns::run_step(potter_exec::ns::StepRun {
        lowers,
        upper: runs.join("upper"),
        work: runs.join("work"),
        args: potter_exec::ns::wrap(
            &potter_plan::Command::Argv(argv),
            config.working_dir.as_deref().unwrap_or("/"),
        ),
        env,
        user: vmkit::sandbox::User {
            uid,
            gid,
            groups: vec![],
        },
        network: false,
        cache_mounts: vec![],
        secrets: vec![],
        timeout: Duration::from_secs(180),
        run_dir: runs.join("run"),
        log: &mut log,
    })
    .unwrap();
    (outcome.status.success() && !outcome.timed_out, lines)
}

/// Starts the image's `web` process and waits until a socket listens on the image's port, read
/// from /proc/net/tcp{,6} (no client needed in the image); then stops it.
const LISTENS: &str = r#"
port=$(printf '%04X' "$POTTER_E2E_PORT")
"$@" > /tmp/web.log 2>&1 &
pid=$!
i=0
while [ "$i" -lt 1200 ]; do
  if grep -Eq "^ *[0-9]+: [0-9A-F]+:$port [0-9A-F]+:[0-9A-F]+ 0A " /proc/net/tcp /proc/net/tcp6 2>/dev/null; then
    echo "listening on $POTTER_E2E_PORT"
    kill "$pid"
    exit 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "the web process exited:"
    cat /tmp/web.log
    exit 1
  fi
  sleep 0.1
  i=$((i + 1))
done
echo "nothing listened on $POTTER_E2E_PORT:"
cat /tmp/web.log
kill "$pid"
exit 1
"#;

/// Builds the fixture `name` on the catalog and checks its `web` process listens.
fn serves(name: &str) {
    let Some(catalog) = real_catalog() else { return };
    let Some(ctx) = Ctx::new() else { return };
    let repo = fixture(name);
    build(&ctx, repo.path(), &catalog);
    let (manifest, _) = source_manifest(&ctx, "potter/app:latest");
    let web: Vec<String> = serde_json::from_str(&annotation(&manifest, "dev.potter.process.web")).unwrap();
    let port = annotation(&manifest, "dev.potter.port");
    let script = format!("POTTER_E2E_PORT={port}\n{LISTENS}");
    let (ok, lines) = run_in(&ctx, "potter/app:latest", &web, &script);
    assert!(ok, "{name}: {web:?} did not listen on {port}:\n{}", lines.join("\n"));
    assert!(lines.iter().any(|l| l == &format!("listening on {port}")), "{lines:?}");
}

#[test]
fn node_serves_on_the_catalog() {
    serves("node");
}

#[test]
fn bun_serves_on_the_catalog() {
    serves("bun");
}

#[test]
fn deno_serves_on_the_catalog() {
    serves("deno");
}

#[test]
fn python_uv_serves_on_the_catalog() {
    serves("python-uv");
}

#[test]
fn python_poetry_serves_on_the_catalog() {
    serves("python-poetry");
}

#[test]
fn ruby_rails_serves_on_the_catalog() {
    serves("ruby-rails");
}

#[test]
fn go_serves_on_the_catalog() {
    serves("go");
}

#[test]
fn rust_serves_on_the_catalog() {
    serves("rust");
}

#[test]
fn static_serves_on_the_catalog() {
    serves("static");
}

/// An explicit plan on the catalog that installs libpq-dev with an `apt` build step, compiles a
/// program against it, and runs it on os-min + static-server with libpq5 from an `apt` runtime
/// step: the program's `libpq.so.5` comes only from that runtime step's layer.
fn libpq_project() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"[apt]
snapshot = "{SNAPSHOT}"
build = ["libpq-dev"]
runtime = ["libpq5"]

[target.app]
provider = "none"
export = ["/out"]

[target.app.base]
catalog = ["os-min", "build-tools"]
run_catalog = ["os-min", "static-server"]

[target.app.steps.compile]
run = "mkdir -p /out && cc -o /out/pqver pqver.c $(pkg-config --cflags --libs libpq) && test -f /var/lib/dpkg/info/libpq-dev.list"

[target.app.process.web]
cmd = ["/out/pqver"]
"#
    );
    std::fs::write(repo.path().join("potter.toml"), toml).unwrap();
    std::fs::write(
        repo.path().join("pqver.c"),
        "#include <stdio.h>\n#include <libpq-fe.h>\n\nint main(void) {\n    printf(\"libpq %d\\n\", PQlibVersion());\n    return 0;\n}\n",
    )
    .unwrap();
    repo
}

/// Runs the image's `web` process to completion and returns its output.
fn run_web(ctx: &Ctx, image: &str) -> Vec<String> {
    let (manifest, _) = source_manifest(ctx, image);
    let web: Vec<String> = serde_json::from_str(&annotation(&manifest, "dev.potter.process.web")).unwrap();
    let (ok, lines) = run_in(ctx, image, &web, r#""$@""#);
    assert!(ok, "{web:?} failed:\n{}", lines.join("\n"));
    lines
}

#[test]
fn apt_installs_on_a_catalog_chain() {
    let Some(catalog) = real_catalog() else { return };
    let Some(ctx) = Ctx::new() else { return };
    let repo = libpq_project();
    build(&ctx, repo.path(), &catalog);
    let lines = run_web(&ctx, "potter/app:latest");
    assert!(lines.iter().any(|l| l.starts_with("libpq ")), "{lines:?}");
    let (manifest, _) = source_manifest(&ctx, "potter/app:latest");
    let needed: Vec<String> = serde_json::from_str(&annotation(&manifest, "dev.potter.sonames.needed")).unwrap();
    assert!(needed.contains(&"libpq.so.5".to_string()), "{needed:?}");
}

/// A copy of the catalog at `layout` (an `oci-layout:` location), signed with a key of its own,
/// so a test can publish a superseding entry into it.
struct Resigned {
    dir: tempfile::TempDir,
    layout: PathBuf,
    key: PathBuf,
    index: Index,
    signing: SigningKey,
    blobs: Vec<(Descriptor, PathBuf)>,
}

fn layout_blob(layout: &Path, digest: &Digest) -> PathBuf {
    layout.join("blobs/sha256").join(digest.hex())
}

impl Resigned {
    fn new(location: &str) -> Self {
        let real = PathBuf::from(location.strip_prefix("oci-layout:").expect("an oci-layout: catalog"));
        let names: ImageIndex = serde_json::from_slice(&std::fs::read(real.join("index.json")).unwrap()).unwrap();
        let index_manifest = names
            .manifests
            .iter()
            .find(|m| m.annotation("org.opencontainers.image.ref.name") == Some(INDEX_TAG))
            .expect("the layout names its index");
        let manifest: ImageManifest =
            serde_json::from_slice(&std::fs::read(layout_blob(&real, &index_manifest.digest)).unwrap()).unwrap();
        let index = Index::parse(&std::fs::read(layout_blob(&real, &manifest.layers[0].digest)).unwrap()).unwrap();
        let blobs = index
            .current()
            .map(|e| {
                let digest = Digest::parse(&e.digest).unwrap();
                let path = layout_blob(&real, &digest);
                (Descriptor::new(&e.media_type, digest, e.size), path)
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let (private, public) = potter_catalog::generate_pem();
        let key = dir.path().join("catalog.pub");
        std::fs::write(&key, public).unwrap();
        let mut resigned = Resigned {
            layout: dir.path().join("layout"),
            key,
            index,
            signing: potter_catalog::signing_key_from_pem(&private).unwrap(),
            blobs,
            dir,
        };
        resigned.publish();
        resigned
    }

    fn publish(&mut self) {
        let blob = self.index.canonical();
        let sha: [u8; 32] = sha2::Sha256::digest(&blob).into();
        let (id, sig) = potter_catalog::sign(&sha, &self.signing);
        potter_catalog::write_layout(&self.layout, &blob, &id, &sig, &self.blobs).unwrap();
    }

    fn env(&self) -> (String, String) {
        (
            format!("oci-layout:{}", self.layout.display()),
            self.key.display().to_string(),
        )
    }

    fn current(&self, name: &str) -> Entry {
        self.index.current().find(|e| e.name == name).unwrap().clone()
    }

    /// Publishes, at the next serial, an entry that supersedes the current `name`: its layer with
    /// one file more, `extra` (path, content).
    fn supersede(&mut self, name: &str, extra: (&str, &[u8])) -> Entry {
        let old = self.current(name);
        let (desc, path) = self
            .blobs
            .iter()
            .find(|(d, _)| d.digest.to_string() == old.digest)
            .unwrap()
            .clone();
        let mut tar = Vec::new();
        let file = std::fs::File::open(&path).unwrap();
        match desc.media_type.as_str() {
            media::OCI_LAYER_TAR => std::io::BufReader::new(file).read_to_end(&mut tar).unwrap(),
            _ => zstd::Decoder::new(file).unwrap().read_to_end(&mut tar).unwrap(),
        };
        // The new file goes right after the last entry's data, before the end-of-archive blocks.
        let mut end = 0;
        for e in tar::Archive::new(&tar[..]).entries().unwrap() {
            let e = e.unwrap();
            end = end.max(e.raw_file_position() + e.size().div_ceil(512) * 512);
        }
        tar.truncate(end as usize);
        let mut extra_tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_ustar();
        header.set_size(extra.1.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        extra_tar.append_data(&mut header, extra.0, extra.1).unwrap();
        tar.extend(extra_tar.into_inner().unwrap());
        let digest = Digest::of(&tar);
        let path = self.dir.path().join(digest.hex());
        std::fs::write(&path, &tar).unwrap();
        let mut entry = old.clone();
        entry.digest = digest.to_string();
        entry.diff_id = digest.to_string();
        entry.size = tar.len() as u64;
        entry.media_type = media::OCI_LAYER_TAR.into();
        entry.supersedes = Some(old.digest.clone());
        entry.fixes = vec!["TEST-E2E-0001".into()];
        self.blobs
            .push((Descriptor::new(media::OCI_LAYER_TAR, digest, tar.len() as u64), path));
        self.index.entries.push(entry.clone());
        self.index.serial += 1;
        self.publish();
        entry
    }
}

/// The merged root of `image`'s kiln layers.
fn merged(ctx: &Ctx, image: &str) -> kiln_erofs::Merged<std::fs::File> {
    let digest = kiln_image::resolve_name(&ctx.store, image).unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    let layers = loaded.entries[0]
        .1
        .manifest
        .layers
        .iter()
        .filter(|l| l.media_type == kiln_image::types::KILN_LAYER)
        .map(|l| kiln_erofs::Image::open(ctx.store.open_blob(&l.digest).unwrap()).unwrap())
        .collect();
    kiln_erofs::Merged::new(layers)
}

/// Task 9's soname gate after the runtime steps re-run: the app's `libpq.so.5` comes from the
/// `apt` runtime step, which the rebase runs again on the superseding static-server layer.
#[test]
fn an_image_with_apt_runtime_steps_rebases_onto_a_superseding_layer() {
    let Some(real) = real_catalog() else { return };
    let Some(ctx) = Ctx::new() else { return };
    let mut catalog = Resigned::new(&real.0);
    let repo = libpq_project();
    build(&ctx, repo.path(), &catalog.env());
    let old = catalog.current("static-server");
    let new = catalog.supersede("static-server", ("etc/potter-e2e-patched", b"patched\n"));

    let rebase = ["rebase", "--executor", "ns", "--json", "potter/app:latest"];
    let (code, stdout, stderr) = potter(&ctx, &rebase, &catalog.env());
    assert_eq!(code, 0, "{stderr}");
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["unchanged"], false, "{stdout}");
    assert_eq!(r["replaced"], serde_json::json!([[old.digest, new.digest]]), "{stdout}");
    assert!(stderr.contains("apt-runtime"), "the runtime step runs again: {stderr}");

    // The rebased image is on the new layer and still holds the libpq the program needs. Its
    // kiln layers are what changed (its source manifest records the build), so read those.
    let mut root = merged(&ctx, "potter/app:latest");
    let (layer, nid) = root
        .lookup(b"etc/potter-e2e-patched")
        .unwrap()
        .expect("the new layer's file");
    let mut patched = String::new();
    root.image(layer)
        .data_reader(nid)
        .unwrap()
        .read_to_string(&mut patched)
        .unwrap();
    assert_eq!(patched, "patched\n");
    let libpq = format!("usr/lib/{}-linux-gnu/libpq.so.5", std::env::consts::ARCH);
    assert!(
        root.lookup(libpq.as_bytes()).unwrap().is_some(),
        "the rebased image has no {libpq}"
    );
}
