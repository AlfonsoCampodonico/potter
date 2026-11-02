//! A signed catalog in a local OCI layout, for building on catalog bases without a registry:
//! potter reads it through `POTTER_CATALOG=oci-layout:<dir>` and trusts its key through
//! `POTTER_CATALOG_KEY`.

use std::path::PathBuf;

use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::DecodePrivateKey;
use kiln_oci::{Descriptor, media};
use kiln_store::Digest;
use potter_catalog::{Arch, Entry, Index, Kind, SCHEMA};
use potter_plan::CatalogRef;
use sha2::Digest as _;

/// One layer of a test catalog: an uncompressed tar and what the index says about it.
pub struct Layer {
    pub name: String,
    pub line: String,
    pub kind: Kind,
    pub tar: Vec<u8>,
    pub env: Vec<(String, String)>,
    pub requires: Vec<CatalogRef>,
    /// ABI values over the defaults (`os` debian-13, `libc` glibc-2.41).
    pub abi: Vec<(String, String)>,
    pub provides_sonames: Vec<String>,
    /// The entry this one replaces, which then drops out of the current entries.
    pub supersedes: Option<String>,
    /// Advisory ids the layer fixes.
    pub fixes: Vec<String>,
}

impl Layer {
    pub fn new(name: &str, line: &str, kind: Kind, tar: Vec<u8>) -> Self {
        Layer {
            name: name.into(),
            line: line.into(),
            kind,
            tar,
            env: Vec::new(),
            requires: Vec::new(),
            abi: Vec::new(),
            provides_sonames: Vec::new(),
            supersedes: None,
            fixes: Vec::new(),
        }
    }

    pub fn fixes(mut self, id: &str) -> Self {
        self.fixes.push(id.into());
        self
    }

    pub fn abi(mut self, key: &str, value: &str) -> Self {
        self.abi.push((key.into(), value.into()));
        self
    }

    pub fn supersedes(mut self, digest: &str) -> Self {
        self.supersedes = Some(digest.into());
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// A catalog layout signed with a key of its own.
pub struct TestCatalog {
    pub dir: tempfile::TempDir,
    pub layout: PathBuf,
    /// The public key's PEM file.
    pub key: PathBuf,
    /// The index blob's digest: what `--catalog-index` pins.
    pub digest: String,
    pub index: Index,
    signing: SigningKey,
}

/// The catalog's name for the host's architecture.
pub fn host_arch() -> Arch {
    match kiln_oci::Platform::host().architecture.as_str() {
        "amd64" => Arch::X86_64,
        "arm64" => Arch::Aarch64,
        other => panic!("no catalog architecture for {other}"),
    }
}

impl TestCatalog {
    /// A catalog of `layers`, all for the host's architecture, at serial 1.
    pub fn new(layers: Vec<Layer>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (private, public) = potter_catalog::generate_pem();
        let key = dir.path().join("catalog.pub");
        std::fs::write(&key, public).unwrap();
        let mut catalog = TestCatalog {
            layout: dir.path().join("layout"),
            key,
            digest: String::new(),
            index: Index {
                schema: SCHEMA.into(),
                serial: 0,
                created: "2026-11-01T00:00:00Z".into(),
                entries: Vec::new(),
            },
            signing: SigningKey::from_pkcs8_pem(&private).unwrap(),
            dir,
        };
        catalog.publish(1, layers);
        catalog
    }

    /// The current entry named `name`.
    pub fn current(&self, name: &str) -> &Entry {
        self.index.current().find(|e| e.name == name).unwrap()
    }

    /// Adds `layers` and publishes the index again as `serial`.
    pub fn publish(&mut self, serial: u64, layers: Vec<Layer>) {
        let blobs = self.dir.path().join("layers");
        std::fs::create_dir_all(&blobs).unwrap();
        let mut stored = Vec::new();
        for l in layers {
            let digest = Digest::of(&l.tar);
            let path = blobs.join(digest.hex());
            std::fs::write(&path, &l.tar).unwrap();
            let size = l.tar.len() as u64;
            stored.push((Descriptor::new(media::OCI_LAYER_TAR, digest.clone(), size), path));
            self.index.entries.push(Entry {
                name: l.name.clone(),
                version: format!("{}.0", l.line),
                line: l.line,
                arch: host_arch(),
                kind: l.kind,
                digest: digest.to_string(),
                diff_id: digest.to_string(),
                size,
                media_type: media::OCI_LAYER_TAR.into(),
                abi: [("os", "debian-13"), ("libc", "glibc-2.41")]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .chain(l.abi)
                    .collect(),
                requires: l.requires,
                conflicts: vec![],
                env: l.env.into_iter().collect(),
                provides_sonames: l.provides_sonames,
                executor: "ns".into(),
                source: "https://example.test".into(),
                license: "MIT".into(),
                supersedes: l.supersedes,
                fixes: l.fixes,
            });
        }
        self.index.serial = serial;
        let blob = self.index.canonical();
        let sha: [u8; 32] = sha2::Sha256::digest(&blob).into();
        let (id, sig) = potter_catalog::sign(&sha, &self.signing);
        potter_catalog::write_layout(&self.layout, &blob, &id, &sig, &stored).unwrap();
        self.digest = Digest::of(&blob).to_string();
    }

    /// The environment that points potter at this catalog.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("POTTER_CATALOG", format!("oci-layout:{}", self.layout.display())),
            ("POTTER_CATALOG_KEY", self.key.display().to_string()),
        ]
    }
}

/// `go` on `PATH` at `/usr/local/go/bin`: `go mod download` does nothing, and `go build -o OUT`
/// writes a shell script to OUT that prints `hello from go`.
pub const FAKE_GO: &str = r#"#!/bin/sh
case "$1" in
  mod) exit 0 ;;
  build)
    out=
    while test $# -gt 0; do
      case "$1" in
        -o) out=$2; shift 2 ;;
        *) shift ;;
      esac
    done
    test -n "$out" || { echo "go build: no -o" >&2; exit 2; }
    mkdir -p "${out%/*}"
    printf '#!/bin/sh\necho hello from go\n' > "$out"
    busybox chmod 755 "$out"
    ;;
  *) echo "fake go: $*" >&2; exit 2 ;;
esac
"#;

/// The busybox root as `os-min`, a `build-tools` stand-in (detected non-PHP build chains ask for
/// one) and the fake `go` over it at `go_line`.
pub fn go_catalog(go_line: &str) -> TestCatalog {
    use kiln_erofs::testtar::{Opts, TarBuilder};
    let dir = Opts::default().mode(0o755);
    let mut go = TarBuilder::new();
    for d in ["usr", "usr/local", "usr/local/go", "usr/local/go/bin"] {
        go.dir(d, &dir);
    }
    go.file("usr/local/go/bin/go", FAKE_GO.as_bytes(), &Opts::default().mode(0o755));
    let mut tools = TarBuilder::new();
    for d in ["usr", "usr/share", "usr/share/build-tools"] {
        tools.dir(d, &dir);
    }
    tools.file(
        "usr/share/build-tools/README",
        b"stand-in\n",
        &Opts::default().mode(0o644),
    );
    TestCatalog::new(vec![
        Layer::new("os-min", "13", Kind::Base, super::busybox_root()).env("PATH", "/bin"),
        Layer::new("build-tools", "1", Kind::Tool, tools.finish()),
        Layer::new("go", go_line, Kind::Runtime, go.finish()).env("PATH", "/usr/local/go/bin"),
    ])
}
