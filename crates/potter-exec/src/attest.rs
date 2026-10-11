//! Attestations (spec §17): the SBOM and provenance of a built image, signed when a key is given.
//!
//! Only host-side data goes in. Lockfiles are read from the target's inputs layers, which potter
//! builds itself from the declared inputs; step output never reaches an attestation.

use std::io::Read;

use kiln_store::{Digest, Store};
use potter_cache::ExecutorClass;
use potter_plan::{Base, Step, Target};
use potter_supply::lockfiles::{self, Component, LOCKFILES, MAX_LOCKFILE};
use potter_supply::provenance::{self, ProvStep, ProvenanceInput};
use potter_supply::sbom::{self, SbomInput};
use potter_supply::sign::{self, SigningKey, VerifyingKey};

use crate::assemble::Built;
use crate::error::{BuildError, io};
use crate::publish::{Attachment, BUNDLE_TYPE, Kind, Publisher, STATEMENT_TYPE};

/// What the attestations describe beyond the build itself.
pub struct AttestInput<'a> {
    pub target: &'a Target,
    pub built: &'a Built,
    /// The repository the image is pushed to (`registry/repo`): the statements' subject name.
    pub subject_name: &'a str,
    pub builder_id: &'a str,
    pub tenant: &'a str,
    /// The source revision, when known.
    pub source: Option<String>,
    /// RFC 3339.
    pub started: &'a str,
    pub finished: &'a str,
}

/// The lockfiles in `layers` (uncompressed tar blobs): their paths in the layer and contents.
/// `go.mod` comes along for `go.sum`. A file over [`MAX_LOCKFILE`] is read one byte past the cap,
/// so parsing it reports it as too large rather than reading a truncated file.
pub fn lockfiles(store: &Store, layers: &[Digest]) -> Result<Vec<(String, Vec<u8>)>, BuildError> {
    let mut out = Vec::new();
    for d in layers {
        let blob = store
            .open_blob(d)
            .map_err(|e| BuildError::Infra(format!("layer {d}: {e}")))?;
        let mut archive = tar::Archive::new(blob);
        for entry in archive.entries().map_err(io(format!("reading layer {d}")))? {
            let entry = entry.map_err(io(format!("reading layer {d}")))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().map_err(io(format!("reading layer {d}")))?;
            let path = path.to_string_lossy().trim_start_matches("./").to_string();
            let name = path.rsplit('/').next().unwrap_or(&path);
            if !LOCKFILES.contains(&name) && name != "go.mod" {
                continue;
            }
            let mut bytes = Vec::new();
            entry
                .take(MAX_LOCKFILE as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(io(format!("reading {path}")))?;
            out.push((path, bytes));
        }
    }
    Ok(out)
}

/// The components `files` give evidence of, in file order without repeats; a lockfile that
/// cannot be read is skipped with a warning and gives no component (never a guess).
pub fn components(files: &[(String, Vec<u8>)], warn: &mut dyn FnMut(String)) -> Vec<Component> {
    let mut out: Vec<Component> = Vec::new();
    for (path, bytes) in files {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parsed = match name {
            "go.mod" => continue,
            "go.sum" => {
                let go_mod = files
                    .iter()
                    .find(|(p, _)| p.rsplit_once('/').map_or(("", p.as_str()), |(d, n)| (d, n)) == (dir, "go.mod"));
                match go_mod {
                    Some((_, m)) => lockfiles::parse_go(path, m, bytes),
                    None => {
                        warn(format!("skipping {path}: no go.mod beside it"));
                        continue;
                    }
                }
            }
            _ => lockfiles::parse(path, bytes),
        };
        match parsed {
            Ok(found) => {
                for c in found {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
            Err(e) => warn(format!("skipping {path} in the SBOM: {e}")),
        }
    }
    out
}

/// The inputs layers of `built`: the host-built layers of the target's `Inputs` steps.
pub fn inputs_layers(target: &Target, built: &Built) -> Vec<Digest> {
    let names: Vec<&str> = target
        .build
        .steps
        .iter()
        .chain(&target.image.runtime_steps)
        .filter(|s| matches!(s, Step::Inputs { .. }))
        .map(|s| s.name())
        .collect();
    let run = format!("{}:run", target.name);
    built
        .steps
        .iter()
        .filter(|r| r.class.is_none() && (r.chain == target.name || r.chain == run) && names.contains(&r.step.as_str()))
        .map(|r| r.layer.clone())
        .collect()
}

/// The SBOM, the provenance and (with a key) the image signature, ready to attach.
pub fn attestations(
    store: &Store,
    input: &AttestInput,
    key: Option<&SigningKey>,
    warn: &mut dyn FnMut(String),
) -> Result<Vec<Attachment>, BuildError> {
    let built = input.built;
    let digest = built.manifest_digest.to_string();
    let files = lockfiles(store, &inputs_layers(input.target, built))?;
    let components = components(&files, warn);
    let catalog: Vec<(String, Option<String>)> = [&input.target.build.base, &input.target.image.base]
        .into_iter()
        .filter_map(|b| match b {
            Base::Catalog { layers } => Some(layers),
            _ => None,
        })
        .flatten()
        .map(|c| (c.name.clone(), c.version.clone()))
        .fold(Vec::new(), |mut v, c| {
            if !v.contains(&c) {
                v.push(c);
            }
            v
        });
    let bases: Vec<(String, String)> = built
        .bases
        .iter()
        .map(|b| (b.reference.clone(), b.digest.to_string()))
        .collect();
    let potter_version = env!("CARGO_PKG_VERSION");
    let sbom = sbom::spdx(&SbomInput {
        image_name: input.subject_name,
        image_digest: &digest,
        created: input.finished,
        bases: &bases,
        catalog: &catalog,
        components: &components,
        potter_version,
    });
    let sbom = provenance::in_toto(input.subject_name, &digest, provenance::SPDX_PREDICATE, sbom);
    let steps: Vec<ProvStep> = built
        .steps
        .iter()
        .map(|r| ProvStep {
            chain: r.chain.clone(),
            step: r.step.clone(),
            key: r.key.clone(),
            layer: r.layer.to_string(),
            class: r.class.map(|c| match c {
                ExecutorClass::Ns => "ns".to_string(),
                ExecutorClass::Vm => "vm".to_string(),
            }),
        })
        .collect();
    let plan_hash = potter_plan::hash_of(input.target)
        .map_err(|e| BuildError::Plan(e.to_string()))?
        .hex();
    let prov = provenance::statement(&ProvenanceInput {
        subject_name: input.subject_name,
        subject_digest: &digest,
        builder_id: input.builder_id,
        potter_version,
        target: &input.target.name,
        plan_hash: &plan_hash,
        source_revision: input.source.as_deref(),
        source_dirty: None,
        tenant: input.tenant,
        catalog_index: None,
        bases: &bases,
        steps: &steps,
        started: input.started,
        finished: input.finished,
    });
    let json = |v: &serde_json::Value| serde_json::to_vec(v).expect("JSON values serialise");
    Ok(match key {
        Some(key) => {
            let manifest = store
                .read_metadata(&built.manifest_digest)
                .map_err(|e| BuildError::Infra(format!("the image manifest: {e}")))?;
            [
                (Kind::Sbom, sign::sign_statement(key, &sbom)),
                (Kind::Provenance, sign::sign_statement(key, &prov)),
                (Kind::Signature, sign::sign_manifest(key, &manifest)),
            ]
            .into_iter()
            .map(|(kind, bundle)| Attachment {
                kind,
                media_type: BUNDLE_TYPE.into(),
                bytes: json(&bundle),
            })
            .collect()
        }
        None => [(Kind::Sbom, sbom), (Kind::Provenance, prov)]
            .into_iter()
            .map(|(kind, statement)| Attachment {
                kind,
                media_type: STATEMENT_TYPE.into(),
                bytes: json(&statement),
            })
            .collect(),
    })
}

/// What `potter verify` found for each kind of attestation.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub kind: Kind,
    /// `Err` says why it failed.
    pub result: Result<(), String>,
}

/// Verifies the attestations of `subject` with `key`: each kind must be attached, signed by the
/// key, and about `subject` (spec §17).
pub fn verify(
    publisher: &Publisher,
    store: &Store,
    subject: &Digest,
    key: &VerifyingKey,
) -> Result<Vec<Verdict>, BuildError> {
    let referrers = publisher.referrers(subject)?;
    let digest = subject.to_string();
    let mut out = Vec::new();
    for kind in Kind::ALL {
        let Some(r) = referrers.iter().find(|r| r.kind == Some(kind)) else {
            out.push(Verdict {
                kind,
                result: Err("not attached".into()),
            });
            continue;
        };
        let result = match publisher.fetch(store, &r.descriptor) {
            Err(BuildError::Infra(e)) => return Err(BuildError::Infra(e)),
            Err(e) => Err(e.to_string()),
            Ok((media_type, _)) if media_type != BUNDLE_TYPE => Err(format!("unsigned ({media_type})")),
            Ok((_, bundle)) => match kind {
                Kind::Sbom => sign::verify_statement(key, &bundle, &digest, provenance::SPDX_PREDICATE).map(|_| ()),
                Kind::Provenance => {
                    sign::verify_statement(key, &bundle, &digest, provenance::SLSA_PREDICATE).map(|_| ())
                }
                Kind::Signature => sign::verify_manifest(key, &bundle, &digest),
            }
            .map_err(|e| e.to_string()),
        };
        out.push(Verdict { kind, result });
    }
    Ok(out)
}

/// The commit `path` is checked out at, when it is in git. Only `git rev-parse` runs: it reads
/// refs and never runs the repository's hooks, filters or fsmonitor, which a tenant controls (P9).
pub fn source_revision(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-C"])
        .arg(path)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let rev = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (rev.len() >= 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())).then_some(rev)
}

/// `secs` since the Unix epoch as RFC 3339 UTC.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use potter_plan::{Chain, Image, ImageConfig};
    use potter_supply::provenance::{SLSA_PREDICATE, SPDX_PREDICATE};
    use potter_supply::sign::{verify_manifest, verify_statement};

    use super::*;
    use crate::assemble::BaseRecord;
    use crate::chain::StepRecord;
    use crate::tarball::{Meta, TarWriter};

    fn tar(files: &[(&str, &str)]) -> Vec<u8> {
        let mut w = TarWriter::new(Vec::new());
        for (path, body) in files {
            w.file(path, &Meta::root(0o644, 0), body.len() as u64, &mut body.as_bytes())
                .unwrap();
        }
        w.finish().unwrap()
    }

    const COMPOSER: &str = r#"{"packages":[{"name":"laravel/framework","version":"v11.0.0"}]}"#;

    #[test]
    fn lockfiles_come_out_of_the_layers_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = store
            .put_bytes(&tar(&[("app/composer.lock", COMPOSER), ("app/readme.md", "x")]))
            .unwrap();
        let b = store.put_bytes(&tar(&[("app/web/package-lock.json", "{}")])).unwrap();
        let found: Vec<String> = lockfiles(&store, &[a, b])
            .unwrap()
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(found, ["app/composer.lock", "app/web/package-lock.json"]);
    }

    #[test]
    fn a_bad_lockfile_is_skipped_with_a_warning_and_go_sum_reads_its_go_mod() {
        let files = vec![
            ("app/composer.lock".to_string(), COMPOSER.as_bytes().to_vec()),
            ("app/package-lock.json".to_string(), b"not json".to_vec()),
            (
                "svc/go.mod".to_string(),
                b"module x\nrequire golang.org/x/text v0.3.0\n".to_vec(),
            ),
            (
                "svc/go.sum".to_string(),
                b"golang.org/x/text v0.3.0 h1:abc=\ngolang.org/x/text v0.3.0/go.mod h1:def=\n".to_vec(),
            ),
        ];
        let mut warnings = Vec::new();
        let c = components(&files, &mut |w| warnings.push(w));
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["laravel/framework", "golang.org/x/text"]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("package-lock.json"));
    }

    #[test]
    fn the_source_revision_runs_none_of_the_repository_s_commands() {
        let repo = tempfile::tempdir().unwrap();
        let marker = repo.path().join("pwned");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        let hook = format!("touch {}", marker.display());
        git(&["config", "core.fsmonitor", &hook]);
        git(&["config", "filter.evil.clean", &hook]);
        std::fs::write(repo.path().join(".gitattributes"), "* filter=evil\n").unwrap();
        let rev = source_revision(repo.path()).unwrap();
        assert_eq!(rev.len(), 40, "{rev}");
        assert!(!marker.exists(), "repository config ran a command");
        assert_eq!(source_revision(&repo.path().join("missing")), None);
    }

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_791_715_000), "2026-10-11T10:36:40Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    fn target() -> Target {
        let base = Base::Oci {
            reference: "ruby:3.3".into(),
            digest: None,
        };
        Target {
            name: "app".into(),
            root: ".".into(),
            platform: None,
            provider: "none".into(),
            build: Chain {
                base: base.clone(),
                steps: vec![
                    Step::Inputs {
                        name: "src".into(),
                        include: vec!["Gemfile.lock".into()],
                        exclude: vec![],
                        dest: "/app".into(),
                        owner: "0:0".into(),
                    },
                    Step::Run {
                        name: "build".into(),
                        command: potter_plan::Command::shell("true"),
                        env: Default::default(),
                        workdir: "/app".into(),
                        user: "0:0".into(),
                        network: false,
                        cache_mounts: vec![],
                        secrets: vec![],
                        timeout_secs: None,
                    },
                ],
            },
            image: Image {
                base,
                runtime_steps: vec![],
                export: vec![],
                files: vec![],
                config: ImageConfig::new("/app", "0:0"),
                processes: Default::default(),
                services: Default::default(),
            },
        }
    }

    fn built(store: &Store) -> Built {
        let inputs = store.put_bytes(&tar(&[("app/composer.lock", COMPOSER)])).unwrap();
        let manifest = store.put_bytes(br#"{"schemaVersion":2}"#).unwrap();
        let step = |step: &str, layer: &Digest, class| StepRecord {
            chain: "app".into(),
            step: step.into(),
            key: Some("ab".repeat(32)),
            layer: layer.clone(),
            class,
        };
        // A run layer whose content claims a lockfile: step output never counts as evidence.
        let run = store
            .put_bytes(&tar(&[("app/Gemfile.lock", "GEM\n  specs:\n    evil (6.6.6)\n")]))
            .unwrap();
        Built {
            target: "app".into(),
            reference: "potter/app:latest".into(),
            digest: Digest::of(b"kiln"),
            executed: 1,
            cached: 0,
            steps: vec![step("src", &inputs, None), step("build", &run, Some(ExecutorClass::Ns))],
            bases: vec![BaseRecord {
                reference: "ruby:3.3".into(),
                digest: Digest::of(b"base"),
            }],
            manifest_digest: manifest,
        }
    }

    #[test]
    fn signed_attestations_verify_and_carry_only_host_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let built = built(&store);
        let target = target();
        assert_eq!(inputs_layers(&target, &built), [built.steps[0].layer.clone()]);
        let key = SigningKey::generate();
        let input = AttestInput {
            target: &target,
            built: &built,
            subject_name: "r.example/acme/app",
            builder_id: "https://potter.dev/builder/test",
            tenant: "acme",
            source: Some("c0ffee".into()),
            started: "2026-10-11T10:00:00Z",
            finished: "2026-10-11T10:01:00Z",
        };
        let atts = attestations(&store, &input, Some(&key), &mut |w| panic!("{w}")).unwrap();
        let kinds: Vec<Kind> = atts.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [Kind::Sbom, Kind::Provenance, Kind::Signature]);
        assert!(atts.iter().all(|a| a.media_type == BUNDLE_TYPE));
        let vk = key.verifying_key();
        let digest = built.manifest_digest.to_string();
        let sbom = verify_statement(&vk, &atts[0].bytes, &digest, SPDX_PREDICATE).unwrap();
        let text = sbom.to_string();
        assert!(text.contains("laravel/framework") && !text.contains("evil"), "{text}");
        let prov = verify_statement(&vk, &atts[1].bytes, &digest, SLSA_PREDICATE).unwrap();
        let byproducts = prov["predicate"]["runDetails"]["byproducts"].to_string();
        assert!(byproducts.contains(r#""executor":"ns""#) && byproducts.contains(r#""executor":"host""#));
        verify_manifest(&vk, &atts[2].bytes, &digest).unwrap();
    }

    #[test]
    fn without_a_key_the_statements_go_unsigned_and_there_is_no_signature() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let built = built(&store);
        let target = target();
        let input = AttestInput {
            target: &target,
            built: &built,
            subject_name: "r.example/acme/app",
            builder_id: "b",
            tenant: "local",
            source: None,
            started: "2026-10-11T10:00:00Z",
            finished: "2026-10-11T10:01:00Z",
        };
        let atts = attestations(&store, &input, None, &mut |_| {}).unwrap();
        assert_eq!(atts.len(), 2);
        assert!(atts.iter().all(|a| a.media_type == STATEMENT_TYPE));
    }

    mod verify {
        use kiln_oci::media::{OCI_CONFIG, OCI_MANIFEST};
        use kiln_oci::{Descriptor, ImageManifest, canonical_json};
        use kiln_registry::testregistry::{Config, TestRegistry};
        use kiln_registry::{DockerConfig, Reference};

        use super::*;

        struct World {
            _reg: TestRegistry,
            _dir: tempfile::TempDir,
            store: Store,
            publisher: Publisher,
            subject: Descriptor,
            built: Built,
            key: SigningKey,
        }

        fn world() -> World {
            let reg = TestRegistry::start(Config::default());
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path()).unwrap();
            let mut built = built(&store);
            let config = store.put_bytes(b"{}").unwrap();
            let m = ImageManifest {
                schema_version: 2,
                media_type: Some(OCI_MANIFEST.into()),
                artifact_type: None,
                config: Descriptor::new(OCI_CONFIG, config, 2),
                layers: vec![],
                annotations: None,
            };
            built.manifest_digest = store.put_bytes(&canonical_json(&m)).unwrap();
            let reference = Reference::parse(&format!("{}/acme/app:v1", reg.host())).unwrap();
            let publisher = Publisher::new(&reference, DockerConfig::anonymous()).unwrap();
            let subject = publisher.push_image(&store, "v1", &built.manifest_digest).unwrap();
            World {
                _reg: reg,
                _dir: dir,
                store,
                publisher,
                subject,
                built,
                key: SigningKey::generate(),
            }
        }

        fn attach(w: &World, key: Option<&SigningKey>, digest_of: &Built) {
            let target = target();
            let input = AttestInput {
                target: &target,
                built: digest_of,
                subject_name: "r/acme/app",
                builder_id: "b",
                tenant: "acme",
                source: None,
                started: "2026-10-11T10:00:00Z",
                finished: "2026-10-11T10:01:00Z",
            };
            let atts = attestations(&w.store, &input, key, &mut |_| {}).unwrap();
            w.publisher.attach(&w.store, &w.subject, &atts).unwrap();
        }

        fn failures(w: &World, key: &VerifyingKey) -> Vec<Kind> {
            verify(&w.publisher, &w.store, &w.subject.digest, key)
                .unwrap()
                .into_iter()
                .filter(|v| v.result.is_err())
                .map(|v| v.kind)
                .collect()
        }

        #[test]
        fn signed_attestations_verify() {
            let w = world();
            attach(&w, Some(&w.key), &w.built);
            assert_eq!(failures(&w, &w.key.verifying_key()), []);
        }

        #[test]
        fn another_key_fails_every_kind() {
            let w = world();
            attach(&w, Some(&w.key), &w.built);
            assert_eq!(failures(&w, &SigningKey::generate().verifying_key()), Kind::ALL);
        }

        #[test]
        fn missing_and_unsigned_attestations_fail() {
            let w = world();
            assert_eq!(failures(&w, &w.key.verifying_key()), Kind::ALL);
            attach(&w, None, &w.built);
            assert_eq!(failures(&w, &w.key.verifying_key()), Kind::ALL);
        }

        #[test]
        fn attestations_about_another_image_fail() {
            let w = world();
            let mut other = built(&w.store);
            other.manifest_digest = w.store.put_bytes(b"another manifest").unwrap();
            attach(&w, Some(&w.key), &other);
            assert_eq!(failures(&w, &w.key.verifying_key()), Kind::ALL);
        }
    }
}
