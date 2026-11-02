//! potter: build kiln microVM images from source.

mod render;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use potter_detect::{DetectOptions, RepoError, RepoView};
use potter_plan::sanitize;

#[derive(Parser)]
#[command(name = "potter", version, about = "Build kiln microVM images from source")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
// Parsed once per run: the size of the largest variant does not matter.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Print the build plan for a repository
    Plan {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only plan this target
        #[arg(long)]
        target: Option<String>,
        /// Print the canonical potter.plan/v1 JSON
        #[arg(long, conflicts_with = "explain")]
        json: bool,
        /// Show where each decision came from
        #[arg(long)]
        explain: bool,
        /// The most layers each image may have (3-128), over potter.toml's `max_layers`
        #[arg(long, value_parser = max_layers_arg)]
        max_layers: Option<u32>,
    },
    /// Build images into the kiln store ($KILN_HOME)
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only build these targets (default: all)
        #[arg(long)]
        target: Vec<String>,
        /// Where steps run; required, because the namespace executor isolates less than a VM
        #[arg(long, value_enum)]
        executor: Option<Executor>,
        /// Whose cache this build uses
        #[arg(long, default_value = "local")]
        tenant: String,
        /// Run every step, ignoring the cache
        #[arg(long)]
        no_cache: bool,
        /// Tag the image (default potter/<target>:latest); with several targets, give none
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// A build secret for steps that mount it: id=<id>,src=<file>
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Look misses up in this remote cache: <registry>[/<prefix>] (default $POTTER_REMOTE_CACHE)
        #[arg(long)]
        cache_from: Option<String>,
        /// Publish sealed steps to this remote cache (default $POTTER_REMOTE_CACHE)
        #[arg(long)]
        cache_to: Option<String>,
        /// Only build targets that changed since this git ref (spec §10)
        #[arg(long)]
        affected: Option<String>,
        /// plain: lines on stderr; json: one event per line on stdout (spec §11)
        #[arg(long, value_enum, default_value = "auto")]
        progress: ProgressMode,
        /// Also push the image, with its SBOM and provenance attached: registry=<ref>
        #[arg(long)]
        output: Option<String>,
        /// Sign the pushed image and its attestations with this PKCS#8 PEM key (spec §17)
        #[arg(long)]
        sign_key: Option<PathBuf>,
        /// The builder id recorded in provenance
        #[arg(long, default_value = "https://potter.dev/builder/local")]
        builder_id: String,
        /// The source revision recorded in provenance (default: the checkout's HEAD, read with
        /// `git rev-parse` only)
        #[arg(long)]
        source_revision: Option<String>,
        /// The tenant's cache key: 32 bytes, raw or as 64 hex characters (default: kept in $POTTER_HOME)
        #[arg(long)]
        tenant_key_file: Option<PathBuf>,
        /// The most layers each image may have (3-128; default potter.toml's `max_layers`, else 10).
        /// Above it the bottom layers merge into one composite, the base's first
        #[arg(long, value_parser = max_layers_arg)]
        max_layers: Option<u32>,
        /// Resolve catalog bases against this index (sha256:<hex>) instead of the catalog's current one
        #[arg(long)]
        catalog_index: Option<String>,
    },
    /// Swap a built image's run base for a patched one without rebuilding it (spec §16)
    Rebase {
        /// The image in the kiln store: a name or a digest
        image: String,
        /// Another OCI reference for the run base (default: the recorded one, resolved again)
        #[arg(long)]
        base: Option<String>,
        /// Tag the result (default: IMAGE, when it is a name)
        #[arg(long)]
        tag: Option<String>,
        /// Where the runtime steps run on the new base
        #[arg(long, value_enum)]
        executor: Option<Executor>,
        /// Whose cache the runtime steps use
        #[arg(long, default_value = "local")]
        tenant: String,
        /// Print a JSON summary
        #[arg(long)]
        json: bool,
        /// Resolve a catalog run base against this index (sha256:<hex>) instead of the catalog's current one
        #[arg(long)]
        catalog_index: Option<String>,
        /// Print the catalog layers a rebase would replace and every gate's verdict, writing nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Inspect the signed runtime catalog
    Catalog {
        #[command(subcommand)]
        command: CatalogCmd,
    },
    /// Check a pushed image's SBOM, provenance and signature against a public key (spec §17)
    Verify {
        /// The image: <registry>/<repository>[:tag|@digest]
        image: String,
        /// The PEM public key the attestations must be signed with
        #[arg(long)]
        key: PathBuf,
        /// The tenant the provenance must name
        #[arg(long)]
        expect_tenant: Option<String>,
        /// The builder id the provenance must name
        #[arg(long)]
        expect_builder: Option<String>,
    },
    /// Manage signing keys
    Key {
        #[command(subcommand)]
        command: KeyCmd,
    },
    /// Show which providers claim a directory
    Detect {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Unpack a layer blob into an overlay lower directory (run inside the sandbox)
    #[command(name = "__unpack", hide = true)]
    Unpack {
        blob: PathBuf,
        media_type: String,
        dest: PathBuf,
    },
    /// Seal an overlay upper into an OCI tar diff (run inside the sandbox)
    #[command(name = "__seal", hide = true)]
    Seal {
        upper: PathBuf,
        out: PathBuf,
        /// A mount point left out when empty
        #[arg(long = "skip")]
        skip: Vec<String>,
    },
}

#[derive(Subcommand)]
enum CatalogCmd {
    /// List the catalog's current layers
    Ls {
        /// x86_64 or aarch64 (default: this machine's); ignored with --all
        #[arg(long)]
        arch: Option<String>,
        /// List every architecture and the superseded entries too, ignoring --arch
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show the layers, bottom first, and the composite key that constraints resolve to
    Resolve {
        /// name or name@line
        #[arg(required = true)]
        constraints: Vec<String>,
        /// x86_64 or aarch64 (default: this machine's)
        #[arg(long)]
        arch: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Verify the index and fetch and re-hash every current layer
    Verify {
        /// Verify the index sha256:<hex> instead of the catalog's current one
        #[arg(long)]
        catalog_index: Option<String>,
    },
    /// List the entries a security advisory id fixes, or the entries around a layer digest
    Affected {
        /// An advisory id or sha256:<digest>
        what: String,
        #[arg(long)]
        json: bool,
    },
    /// Build every recipe of a catalog directory into a signed OCI layout (the catalog's CI)
    #[command(hide = true)]
    Build {
        /// One <name>/potter.toml and <name>/layer.toml per layer
        #[arg(required_unless_present = "merge")]
        catalog_dir: Option<PathBuf>,
        /// The OCI layout to write
        #[arg(long)]
        out: PathBuf,
        /// The ed25519 private key (PKCS#8 PEM) that signs the index
        #[arg(long)]
        sign_key: PathBuf,
        /// The catalog this one follows, verified first: <registry ref> or oci-layout:<dir>
        #[arg(long)]
        previous: Option<String>,
        /// Build nothing: merge these layouts, one per architecture and each built with the same
        /// --previous, into one index one serial above it (repeatable)
        #[arg(long, value_name = "LAYOUT", conflicts_with = "catalog_dir")]
        merge: Vec<PathBuf>,
        /// An ed25519 public key (PEM) trusted for every --merge layout (repeatable)
        #[arg(long, value_name = "PEM", requires = "merge")]
        merge_key: Vec<PathBuf>,
    },
    /// Verify a catalog layout with the trusted keys, then push its layers and its index
    #[command(hide = true)]
    Publish {
        layout: PathBuf,
        /// <registry>/<repository>:<tag>
        registry_ref: String,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Write a new P-256 signing key: <dir>/potter.key (0600) and <dir>/potter.pub
    Generate {
        dir: PathBuf,
        /// Write an ed25519 catalog key instead: <dir>/catalog.key (0600) and <dir>/catalog.pub
        #[arg(long)]
        ed25519: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProgressMode {
    Auto,
    Plain,
    Json,
}

/// One `--progress json` event.
fn emit(event: serde_json::Value) {
    println!("{event}");
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Executor {
    /// One microVM per target chain (P4)
    Vm,
    /// Rootless Linux namespaces (spec §9.4)
    Ns,
}

/// `--max-layers`: a budget potter-plan accepts.
fn max_layers_arg(s: &str) -> Result<u32, String> {
    let n: u32 = s.parse().map_err(|_| format!("expected a number, not {s:?}"))?;
    match potter_plan::budget::check_max_layers(n) {
        Some(why) => Err(why),
        None => Ok(n),
    }
}

/// `--max-layers` wins over every target's potter.toml value, and enters the plan (and so the
/// target hash the image records).
fn set_max_layers(plan: &mut potter_plan::Plan, max_layers: Option<u32>) {
    if let Some(n) = max_layers {
        for t in &mut plan.targets {
            t.image.max_layers = Some(n);
        }
    }
}

fn fail(message: &str, code: u8) -> ExitCode {
    eprintln!("error: {}", sanitize(message));
    ExitCode::from(code)
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::Plan {
            path,
            target,
            json,
            explain,
            max_layers,
        } => plan(&path, target.as_deref(), json, explain, max_layers),
        Cmd::Detect { path } => detect(&path),
        Cmd::Catalog { command } => catalog(command),
        Cmd::Rebase {
            image,
            base,
            tag,
            executor,
            tenant,
            json,
            catalog_index,
            dry_run,
        } => rebase(
            &image,
            base.as_deref(),
            tag.as_deref(),
            executor,
            &tenant,
            json,
            catalog_index.as_deref(),
            dry_run,
        ),
        Cmd::Verify {
            image,
            key,
            expect_tenant,
            expect_builder,
        } => verify(&image, &key, expect_tenant.as_deref(), expect_builder.as_deref()),
        Cmd::Key {
            command: KeyCmd::Generate { dir, ed25519 },
        } => key_generate(&dir, ed25519),
        Cmd::Build {
            path,
            target,
            executor,
            tenant,
            no_cache,
            tags,
            secrets,
            progress,
            cache_from,
            cache_to,
            affected,
            output,
            sign_key,
            builder_id,
            source_revision,
            tenant_key_file,
            max_layers,
            catalog_index,
        } => {
            let publish = match Publish::check(output.as_deref(), sign_key, builder_id, source_revision, &target) {
                Ok(p) => p,
                Err((e, code)) => return fail(&e, code),
            };
            let tenant_key = match tenant_key_file.as_deref().map(read_tenant_key).transpose() {
                Ok(k) => k,
                Err((e, code)) => return fail(&e, code),
            };
            let env = std::env::var("POTTER_REMOTE_CACHE").ok().filter(|v| !v.is_empty());
            let remotes = (cache_from.or_else(|| env.clone()), cache_to.or(env));
            build(
                &path,
                &target,
                executor,
                &tenant,
                no_cache,
                &tags,
                &secrets,
                progress == ProgressMode::Json,
                remotes,
                affected.as_deref(),
                publish.as_ref(),
                tenant_key,
                max_layers,
                catalog_index.as_deref(),
            )
        }
        Cmd::Unpack { blob, media_type, dest } => helper(potter_exec::unpack::unpack_blob(&blob, &media_type, &dest)),
        Cmd::Seal { upper, out, skip } => helper(potter_exec::seal::seal_to_file(&upper, &out, &skip)),
    }
}

/// A sandboxed helper's exit: 0, 3 for a violation, 5 for output over its limits, 4 otherwise.
fn helper<T>(result: Result<T, potter_exec::BuildError>) -> ExitCode {
    use potter_exec::BuildError;
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(BuildError::Hostile(e)) => fail(&e, 3),
        Err(BuildError::StepOutput(e)) => fail(&e, 5),
        Err(e) => fail(&e.to_string(), 4),
    }
}

/// The targets of `plan` (planned at `path`) that changed since `git_ref`: the ref's tree comes
/// from `git archive`, and a tree that cannot be planned makes every target changed.
fn changed_since(
    path: &Path,
    git_ref: &str,
    plan: &potter_plan::Plan,
) -> Result<std::collections::BTreeSet<String>, (String, u8)> {
    if git_ref.starts_with('-') {
        return Err((format!("--affected {git_ref:?}: a ref cannot start with `-`"), 2));
    }
    let git = |args: &[&str]| {
        // The tree is the tenant's: no system or global config, no fsmonitor or hooks (P9).
        std::process::Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-C"])
            .arg(path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| (format!("--affected needs git: {e}"), 4u8))
    };
    let top = git(&["rev-parse", "--show-toplevel"])?;
    if !top.status.success() {
        return Err((format!("--affected: {} is not in a git repository", path.display()), 2));
    }
    let top = std::path::PathBuf::from(String::from_utf8_lossy(&top.stdout).trim());
    let here = path
        .canonicalize()
        .map_err(|e| (format!("{}: {e}", path.display()), 4))?;
    let rel = here
        .strip_prefix(top.canonicalize().unwrap_or(top.clone()))
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let archive = git(&["archive", "--format=tar", git_ref, "--"])?;
    if !archive.status.success() {
        let why = String::from_utf8_lossy(&archive.stderr).trim().to_string();
        return Err((format!("--affected {git_ref}: {why}"), 2));
    }
    let tree = tempfile::tempdir().map_err(|e| (format!("--affected: {e}"), 4))?;
    tar::Archive::new(archive.stdout.as_slice())
        .unpack(tree.path())
        .map_err(|e| (format!("--affected: unpacking {git_ref}: {e}"), 4))?;
    let then_root = tree.path().join(&rel);
    let then = potter_config::plan_repo(&then_root, None)
        .map(|p| p.plan)
        .unwrap_or_else(|_| potter_plan::Plan::new(vec![]));
    potter_exec::affected::affected(plan, path, &then, &then_root).map_err(|e| (e.to_string(), e.exit_code()))
}

/// Where a build publishes (spec §17, §18).
struct Publish {
    reference: kiln_registry::Reference,
    sign_key: Option<PathBuf>,
    builder_id: String,
    source_revision: Option<String>,
}

impl Publish {
    /// Checks the publishing flags before anything runs.
    fn check(
        output: Option<&str>,
        sign_key: Option<PathBuf>,
        builder_id: String,
        source_revision: Option<String>,
        targets: &[String],
    ) -> Result<Option<Publish>, (String, u8)> {
        let Some(output) = output else {
            return match sign_key {
                Some(_) => Err(("--sign-key needs --output".into(), 2)),
                None => Ok(None),
            };
        };
        let Some(r) = output.strip_prefix("registry=") else {
            return Err((format!("--output: expected registry=<ref>, not {output:?}"), 2));
        };
        let reference = kiln_registry::Reference::parse(r).map_err(|e| (format!("--output {r:?}: {e}"), 2))?;
        if reference.digest().is_some() {
            return Err((format!("--output {r:?}: name a tag, not a digest"), 2));
        }
        if targets.len() != 1 {
            return Err(("--output needs exactly one --target".into(), 2));
        }
        if let Some(k) = &sign_key
            && k.to_string_lossy().starts_with("kms://")
        {
            return Err((
                "--sign-key: KMS keys are not supported yet; pass a PKCS#8 PEM file".into(),
                2,
            ));
        }
        Ok(Some(Publish {
            reference,
            sign_key,
            builder_id,
            source_revision,
        }))
    }
}

/// A `--tenant-key-file`: 32 raw bytes or 64 hex characters (a trailing newline allowed).
fn read_tenant_key(path: &Path) -> Result<[u8; 32], (String, u8)> {
    let bytes = std::fs::read(path).map_err(|e| (format!("--tenant-key-file {}: {e}", path.display()), 4))?;
    let bad = || {
        (
            format!(
                "--tenant-key-file {}: expected 32 bytes or 64 hex characters",
                path.display()
            ),
            2,
        )
    };
    if let Ok(k) = <[u8; 32]>::try_from(bytes.as_slice()) {
        return Ok(k);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| bad())?
        .trim_end_matches(['\n', '\r']);
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
    }
    Ok(k)
}

fn key_generate(dir: &Path, ed25519: bool) -> ExitCode {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let (private, public, names) = if ed25519 {
        let (private, public) = potter_catalog::generate_pem();
        (private, public, ("catalog.key", "catalog.pub"))
    } else {
        let key = potter_supply::sign::SigningKey::generate();
        (key.to_pem(), key.verifying_key().to_pem(), ("potter.key", "potter.pub"))
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail(&format!("{}: {e}", dir.display()), 4);
    }
    let write = |name: &str, mode: u32, text: &str| -> Result<(), (String, u8)> {
        let path = dir.join(name);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .map_err(|e| {
                let code = if e.kind() == ErrorKind::AlreadyExists { 2 } else { 4 };
                (format!("{}: {e}", path.display()), code)
            })?;
        f.write_all(text.as_bytes())
            .map_err(|e| (format!("{}: {e}", path.display()), 4))
    };
    if let Err((e, code)) = write(names.0, 0o600, &private).and_then(|()| write(names.1, 0o644, &public)) {
        return fail(&e, code);
    }
    println!("{}", dir.join(names.1).display());
    ExitCode::SUCCESS
}

/// `potter verify`: 0 when every attestation verifies, 3 when any fails, 4 on registry errors.
/// The statements must name the repository being verified, so an image copied elsewhere fails.
fn verify(image: &str, key: &Path, tenant: Option<&str>, builder: Option<&str>) -> ExitCode {
    let reference = match kiln_registry::Reference::parse(image) {
        Ok(r) => r,
        Err(e) => return fail(&format!("{image}: {e}"), 2),
    };
    let key = match std::fs::read_to_string(key) {
        Ok(text) => match potter_supply::sign::VerifyingKey::from_pem(&text) {
            Ok(k) => k,
            Err(e) => return fail(&format!("--key {}: {e}", key.display()), 2),
        },
        Err(e) => return fail(&format!("--key {}: {e}", key.display()), 4),
    };
    let repository = format!("{}/{}", reference.registry(), reference.repository());
    let expect = potter_exec::attest::Expect {
        repository: Some(&repository),
        tenant,
        builder,
    };
    let run = || -> Result<(kiln_store::Digest, potter_exec::attest::Report), potter_exec::BuildError> {
        let publisher = potter_exec::publish::Publisher::new(&reference, kiln_registry::DockerConfig::from_env())?;
        let subject = publisher.resolve(&reference.target())?;
        let dir = tempfile::tempdir().map_err(|e| potter_exec::BuildError::Infra(e.to_string()))?;
        let store = kiln_store::Store::open(dir.path()).map_err(|e| potter_exec::BuildError::Infra(e.to_string()))?;
        let report = potter_exec::attest::verify(&publisher, &store, &subject.digest, &key, &expect)?;
        Ok((subject.digest, report))
    };
    match run() {
        Err(e) => fail(&e.to_string(), e.exit_code()),
        Ok((digest, report)) => {
            let mut ok = true;
            for v in &report.verdicts {
                match &v.result {
                    Ok(()) => println!("{}: verified", v.kind.as_str()),
                    Err(why) => {
                        ok = false;
                        println!("{}: FAILED: {}", v.kind.as_str(), sanitize(why));
                    }
                }
            }
            if !ok {
                return ExitCode::from(3);
            }
            let show = |v: &Option<String>| sanitize(v.as_deref().unwrap_or("-"));
            println!("tenant {}, builder {}", show(&report.tenant), show(&report.builder));
            // Deploy this digest: a tag can move after it was verified.
            println!("{}@{digest}", sanitize(&repository));
            ExitCode::SUCCESS
        }
    }
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    potter_exec::attest::rfc3339(secs)
}

/// Pushes a built target and attaches its attestations (spec §17); returns their digests.
#[allow(clippy::too_many_arguments)]
fn publish_target(
    store: &kiln_store::Store,
    p: &Publish,
    target: &potter_plan::Target,
    built: &potter_exec::assemble::Built,
    tenant: &str,
    path: &Path,
    started: &str,
    key: Option<&potter_supply::sign::SigningKey>,
    warn: &mut dyn FnMut(String),
) -> Result<Vec<kiln_store::Digest>, potter_exec::BuildError> {
    use potter_exec::publish::Publisher;
    let publisher = Publisher::new(&p.reference, kiln_registry::DockerConfig::from_env())?;
    let tag = p.reference.tag().unwrap_or("latest");
    let subject = publisher.push_image(store, tag, &built.manifest_digest)?;
    let subject_name = format!("{}/{}", p.reference.registry(), p.reference.repository());
    let input = potter_exec::attest::AttestInput {
        target,
        built,
        subject_name: &subject_name,
        builder_id: &p.builder_id,
        tenant,
        source: p
            .source_revision
            .clone()
            .or_else(|| potter_exec::attest::source_revision(path)),
        started,
        finished: &now_rfc3339(),
    };
    let attachments = potter_exec::attest::attestations(store, &input, key, warn)?;
    publisher.attach(store, &subject, &attachments)
}

/// `id=<id>,src=<file>` flags as secret values.
fn read_secrets(flags: &[String]) -> Result<std::collections::BTreeMap<String, Vec<u8>>, (String, u8)> {
    let mut out = std::collections::BTreeMap::new();
    for flag in flags {
        let (mut id, mut src) = (None, None);
        for part in flag.split(',') {
            match part.split_once('=') {
                Some(("id", v)) => id = Some(v),
                Some(("src", v)) => src = Some(v),
                _ => return Err((format!("--secret {flag:?}: expected id=<id>,src=<file>"), 2)),
            }
        }
        let (Some(id), Some(src)) = (id, src) else {
            return Err((format!("--secret {flag:?}: expected id=<id>,src=<file>"), 2));
        };
        if !potter_plan::is_name(id) {
            return Err((format!("--secret {flag:?}: {id:?} is not a name"), 2));
        }
        let value = std::fs::read(src).map_err(|e| (format!("--secret {id}: reading {src}: {e}"), 4))?;
        out.insert(id.to_string(), value);
    }
    Ok(out)
}

const VM: &str = "VM builds need kiln build mode (kiln M3e), which this potter does not have yet";

/// Refuses every executor but `ns` on Linux, with the reason.
fn check_executor(executor: Option<Executor>) -> Result<(), ExitCode> {
    match executor {
        None => {
            return Err(fail(
                &format!(
                    "choose an executor: {VM}; pass --executor ns to build in Linux namespaces, which isolate less than a VM"
                ),
                4,
            ));
        }
        Some(Executor::Vm) => {
            return Err(fail(
                &format!("{VM}; pass --executor ns to build in Linux namespaces"),
                4,
            ));
        }
        Some(Executor::Ns) => {}
    }
    if !cfg!(target_os = "linux") {
        return Err(fail("the namespace executor needs Linux", 4));
    }
    Ok(())
}

/// The kiln store, potter's home and potter itself (the sandbox's helper).
fn open_homes() -> Result<(kiln_store::Store, potter_cache::PotterHome, PathBuf), String> {
    let store = kiln_store::Store::open(kiln_store::Store::default_root().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let home = potter_cache::PotterHome::open(potter_cache::PotterHome::default_root().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let potter = std::env::current_exe().map_err(|e| format!("finding potter itself: {e}"))?;
    Ok((store, home, potter))
}

/// The catalog, loaded only when one of `bases` is built from it: PHP and OCI bases never
/// fetch it (spec §7.2). With `prepare`, every catalog base is resolved and its layers fetched
/// now, before any step runs, so that a base the catalog cannot satisfy or a bad layer fails the
/// build early.
fn load_catalog<'b>(
    store: &kiln_store::Store,
    home: &potter_cache::PotterHome,
    pin: Option<&str>,
    bases: impl IntoIterator<Item = &'b potter_plan::Base>,
    prepare: bool,
) -> Result<Option<potter_exec::chain::CatalogCtx>, (String, u8)> {
    let bases: Vec<&potter_plan::Base> = bases.into_iter().collect();
    if !bases.iter().any(|b| potter_exec::chain::needs_catalog(b)) {
        if pin.is_some() {
            eprintln!("warning: --catalog-index is ignored: nothing here builds on the catalog");
        }
        return Ok(None);
    }
    let failed = |e: potter_exec::BuildError| (e.to_string(), e.exit_code());
    let auth = kiln_registry::DockerConfig::from_env;
    let catalog = potter_exec::chain::CatalogCtx::load(store, home, pin, auth()).map_err(failed)?;
    if prepare {
        catalog
            .prepare(store, bases, &kiln_oci::Platform::host(), auth())
            .map_err(failed)?;
    }
    Ok(Some(catalog))
}

#[allow(clippy::too_many_arguments)]
fn rebase(
    image: &str,
    base: Option<&str>,
    tag: Option<&str>,
    executor: Option<Executor>,
    tenant: &str,
    json: bool,
    catalog_index: Option<&str>,
    dry_run: bool,
) -> ExitCode {
    use potter_exec::chain::{BuildCtx, Progress, StepState};
    // A dry run runs nothing, so it needs no executor.
    if let Err(code) = check_executor(executor).or_else(|code| if dry_run { Ok(()) } else { Err(code) }) {
        return code;
    }
    let (store, home, potter) = match open_homes() {
        Ok(s) => s,
        Err(e) => return fail(&e, 4),
    };
    let tenant = match potter_cache::Tenant::new(tenant) {
        Ok(t) => t,
        Err(e) => return fail(&e.to_string(), 2),
    };
    // A dry run hashes nothing with the tenant's key, so it never makes one.
    let tenant_key = match dry_run {
        true => [0; 32],
        false => match potter_cache::tenant_key(&home, &tenant) {
            Ok(k) => k,
            Err(e) => return fail(&e.to_string(), 4),
        },
    };
    let index = match home.index(&tenant) {
        Ok(i) => i,
        Err(e) => return fail(&e.to_string(), 4),
    };
    let sandbox = potter_exec::sandboxed::SandboxCtx {
        potter,
        runs: home.runs(),
    };
    // The run chain the rebase re-runs decides whether the catalog is loaded: an image built
    // through the stopgap before P3 goes through it again, never the catalog. An image that
    // cannot be read, or a `--base` the image refuses, is reported by the rebase itself.
    let chain = potter_exec::rebase::recorded(&store, image)
        .and_then(|r| potter_exec::rebase::rebase_chain(&r, base))
        .ok();
    // No `prepare`: a rebase fetches the new layers only once the line and ABI gates pass, when
    // its run chain builds the composite, and that is its first step anyway.
    let catalog = match load_catalog(&store, &home, catalog_index, chain.iter().map(|c| &c.base), false) {
        Ok(c) => c,
        Err((e, code)) => return fail(&e, code),
    };
    let _lock = match dry_run {
        true => None,
        false => match potter_exec::sandboxed::begin_build(&sandbox, &home) {
            Ok(l) => Some(l),
            Err(e) => return fail(&e.to_string(), e.exit_code()),
        },
    };
    let secrets = Default::default();
    let mut progress = |p: Progress| match p {
        Progress::Step { target, step, state } => {
            let state = match state {
                StepState::Cached | StepState::Pulled => "cached".to_string(),
                StepState::Pushed => "pushed".to_string(),
                StepState::Running => "running".to_string(),
                StepState::Done { secs } => format!("done ({secs:.1}s)"),
                StepState::Failed => "failed".to_string(),
            };
            eprintln!("[{}] {}: {state}", sanitize(&target), sanitize(&step));
        }
        Progress::Warning(w) => eprintln!("warning: {}", sanitize(&w)),
        Progress::Log { target, step, line } => eprintln!("[{target}/{step}] {line}"),
    };
    let mut ctx = BuildCtx {
        store: &store,
        home: &home,
        tenant: &tenant,
        index: &index,
        sandbox: &sandbox,
        platform: kiln_oci::Platform::host(),
        no_cache: false,
        auth: kiln_registry::DockerConfig::from_env(),
        executor: potter_cache::ExecutorClass::Ns,
        backend: None,
        secrets: &secrets,
        tenant_key,
        cache_from: None,
        cache_to: None,
        deps: Default::default(),
        catalog: catalog.as_ref(),
        progress: &mut progress,
    };
    let req = potter_exec::rebase::Request {
        image,
        base,
        tag,
        catalog_index,
    };
    if dry_run {
        return match potter_exec::rebase::dry_run(&ctx, &req) {
            Ok(d) => print_dry_run(image, &d, json),
            Err(e) => fail(&e.to_string(), e.exit_code()),
        };
    }
    let r = match potter_exec::rebase::rebase(&mut ctx, &req) {
        Ok(r) => r,
        Err(e) => return fail(&e.to_string(), e.exit_code()),
    };
    let replaced: Vec<[&str; 2]> = r.replaced.iter().map(|(o, n)| [o.as_str(), n.as_str()]).collect();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "digest": r.digest.to_string(),
                "tag": r.tag,
                "unchanged": r.unchanged,
                "old_base": r.old_base.to_string(),
                "new_base": r.new_base.to_string(),
                "base_ref": r.base_ref,
                "base_digest": r.base_digest.to_string(),
                "reused": r.reused,
                "changed": r.changed,
                "executed": r.executed,
                "cached": r.cached,
                "replaced": replaced,
            })
        );
    } else if r.unchanged {
        println!(
            "{} is already on {}@{}",
            sanitize(image),
            sanitize(&r.base_ref),
            r.base_digest
        );
    } else {
        // A catalog image names the catalog layers it swapped, never the composite's display name.
        let replaced = match r.replaced.is_empty() {
            true => format!("replaced run base {}", r.old_base),
            false => format!(
                "replaced catalog layers {}",
                r.replaced
                    .iter()
                    .map(|(o, n)| format!("{o} -> {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        println!(
            "{} {} on {}@{} ({} app layers kept, {} re-derived; {replaced})",
            sanitize(r.tag.as_deref().unwrap_or(image)),
            r.digest,
            sanitize(&r.base_ref),
            r.base_digest,
            r.reused,
            r.changed,
        );
    }
    ExitCode::SUCCESS
}

/// `potter rebase --dry-run`: the layers a rebase would replace and every gate's verdict; exit 2
/// when a catalog gate refuses. kiln's gates need the new run base built, so a dry run cannot
/// check them.
fn print_dry_run(image: &str, r: &potter_exec::rebase::DryRun, json: bool) -> ExitCode {
    const KILN: &str = "not checked: kiln's gates need the new run base built";
    let refused = r.verdicts.iter().any(|v| !v.refusals.is_empty());
    if json {
        let mut gates: Vec<serde_json::Value> = r
            .verdicts
            .iter()
            .map(|v| {
                serde_json::json!({
                    "gate": v.gate,
                    "verdict": if v.refusals.is_empty() { "pass" } else { "refused" },
                    "reasons": v.refusals,
                    "provisional": v.provisional,
                })
            })
            .collect();
        gates.push(serde_json::json!({"gate": "kiln", "verdict": "not checked", "reasons": [KILN]}));
        let replaced: Vec<[&str; 2]> = r.replaced.iter().map(|(o, n)| [o.as_str(), n.as_str()]).collect();
        println!(
            "{}",
            serde_json::json!({
                "dry_run": true,
                "digest": r.digest.to_string(),
                "index": r.index,
                "replaced": replaced,
                "gates": gates,
            })
        );
    } else {
        if r.replaced.is_empty() {
            println!("{}: the catalog has nothing newer for its run base", sanitize(image));
        }
        for (o, n) in &r.replaced {
            println!("{}: would replace {o} with {n}", sanitize(image));
        }
        for v in &r.verdicts {
            let note = v
                .provisional
                .map(|p| format!(" (provisional: {p})"))
                .unwrap_or_default();
            match v.refusals.as_slice() {
                [] => println!("{}: pass{note}", v.gate),
                reasons => println!("{}: refused{note}: {}", v.gate, sanitize(&reasons.join("; "))),
            }
        }
        println!("kiln: {KILN}");
    }
    match refused {
        true => ExitCode::from(2),
        false => ExitCode::SUCCESS,
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    path: &Path,
    only: &[String],
    executor: Option<Executor>,
    tenant: &str,
    no_cache: bool,
    tags: &[String],
    secret_flags: &[String],
    json: bool,
    remotes: (Option<String>, Option<String>),
    affected_ref: Option<&str>,
    publish: Option<&Publish>,
    tenant_key_flag: Option<[u8; 32]>,
    max_layers: Option<u32>,
    catalog_index: Option<&str>,
) -> ExitCode {
    use potter_exec::chain::{BuildCtx, Progress, StepState};
    use serde_json::json;
    if let Err(code) = check_executor(executor) {
        return code;
    }
    if !tags.is_empty() && only.len() != 1 {
        return fail("--tag needs exactly one --target", 2);
    }
    let mut planned = match potter_config::plan_repo(path, None) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string(), if e.is_infrastructure() { 4 } else { 2 }),
    };
    // With --affected, the selection shrinks to the targets that changed since the ref. Both
    // sides are compared without --max-layers, which applies to every target alike.
    let mut selected: Vec<String> = only.to_vec();
    if let Some(r) = affected_ref {
        let changed = match changed_since(path, r, &planned.plan) {
            Ok(c) => c,
            Err((e, code)) => return fail(&e, code),
        };
        let pool: Vec<String> = if only.is_empty() {
            planned.plan.targets.iter().map(|t| t.name.clone()).collect()
        } else {
            only.to_vec()
        };
        selected = Vec::new();
        for name in pool {
            if changed.contains(&name) {
                selected.push(name);
            } else if json {
                emit(json!({"event": "unchanged", "target": name}));
            } else {
                println!("{}: unchanged", sanitize(&name));
            }
        }
        if selected.is_empty() {
            return ExitCode::SUCCESS;
        }
    }
    set_max_layers(&mut planned.plan, max_layers);
    // Every selected target, after the targets it copies from (spec §10).
    let order = match potter_exec::graph::order(&planned.plan.targets, &selected) {
        Ok(o) => o,
        Err(e) => return fail(&e.to_string(), e.exit_code()),
    };
    let targets: Vec<&potter_plan::Target> = order.iter().map(|&i| &planned.plan.targets[i]).collect();
    let (store, home, potter) = match open_homes() {
        Ok(s) => s,
        Err(e) => return fail(&e, 4),
    };
    let tenant = match potter_cache::Tenant::new(tenant) {
        Ok(t) => t,
        Err(e) => return fail(&e.to_string(), 2),
    };
    let secrets = match read_secrets(secret_flags) {
        Ok(s) => s,
        Err((e, code)) => return fail(&e, code),
    };
    let tenant_key = match tenant_key_flag
        .map(Ok)
        .unwrap_or_else(|| potter_cache::tenant_key(&home, &tenant))
    {
        Ok(k) => k,
        Err(e) => return fail(&e.to_string(), 4),
    };
    let remote = |spec: &Option<String>| -> Result<Option<potter_exec::remote::RemoteCache>, potter_exec::BuildError> {
        spec.as_deref()
            .map(|s| potter_exec::remote::RemoteCache::parse(s, &tenant, kiln_registry::DockerConfig::from_env()))
            .transpose()
    };
    let (cache_from, cache_to) = match (remote(&remotes.0), remote(&remotes.1)) {
        (Ok(f), Ok(t)) => (f, t),
        (Err(e), _) | (_, Err(e)) => return fail(&e.to_string(), e.exit_code()),
    };
    let index = match home.index(&tenant) {
        Ok(i) => i,
        Err(e) => return fail(&e.to_string(), 4),
    };
    let catalog = match load_catalog(
        &store,
        &home,
        catalog_index,
        targets.iter().flat_map(|t| [&t.build.base, &t.image.base]),
        true,
    ) {
        Ok(c) => c,
        Err((e, code)) => return fail(&e, code),
    };
    let sandbox = potter_exec::sandboxed::SandboxCtx {
        potter,
        runs: home.runs(),
    };
    let _lock = match potter_exec::sandboxed::begin_build(&sandbox, &home) {
        Ok(l) => l,
        Err(e) => return fail(&e.to_string(), e.exit_code()),
    };
    let mut progress = |p: Progress| match p {
        Progress::Step { target, step, state } if json => {
            let (event, secs) = match state {
                StepState::Cached | StepState::Pulled => ("cache_hit", None),
                StepState::Pushed => ("pushed", None),
                StepState::Running => ("step_start", None),
                StepState::Done { secs } => ("sealed", Some(secs)),
                StepState::Failed => ("failed", None),
            };
            emit(json!({"event": event, "target": target, "step": step, "secs": secs}));
        }
        Progress::Warning(w) if json => emit(json!({"event": "warning", "message": w})),
        Progress::Log { target, step, line } if json => {
            emit(json!({"event": "log", "target": target, "step": step, "line": line}))
        }
        Progress::Step { target, step, state } => {
            let state = match state {
                StepState::Cached => "cached".to_string(),
                StepState::Pulled => "cached (remote)".to_string(),
                StepState::Pushed => "pushed".to_string(),
                StepState::Running => "running".to_string(),
                StepState::Done { secs } => format!("done ({secs:.1}s)"),
                StepState::Failed => "failed".to_string(),
            };
            eprintln!("[{}] {}: {state}", sanitize(&target), sanitize(&step));
        }
        Progress::Warning(w) => eprintln!("warning: {}", sanitize(&w)),
        // Step output passes through unmodified (spec §11).
        Progress::Log { target, step, line } => eprintln!("[{target}/{step}] {line}"),
    };
    let mut ctx = BuildCtx {
        store: &store,
        home: &home,
        tenant: &tenant,
        index: &index,
        sandbox: &sandbox,
        platform: kiln_oci::Platform::host(),
        no_cache,
        auth: kiln_registry::DockerConfig::from_env(),
        executor: potter_cache::ExecutorClass::Ns,
        backend: None,
        secrets: &secrets,
        tenant_key,
        cache_from: cache_from.as_ref(),
        cache_to: cache_to.as_ref(),
        deps: Default::default(),
        catalog: catalog.as_ref(),
        progress: &mut progress,
    };
    let sign_key = match publish
        .and_then(|p| p.sign_key.as_deref())
        .map(|k| potter_supply::sign::SigningKey::load(&k.to_string_lossy()).map_err(|e| format!("--sign-key: {e}")))
    {
        Some(Err(e)) => return fail(&e, 2),
        Some(Ok(k)) => Some(k),
        None => None,
    };
    let started = now_rfc3339();
    for t in targets {
        let built =
            potter_exec::assemble::build_target(&mut ctx, t, path, if only.contains(&t.name) { tags } else { &[] });
        let built = match (built, publish) {
            (Ok(b), Some(p)) if only.contains(&t.name) => {
                let mut warn = |w: String| (ctx.progress)(Progress::Warning(w));
                match publish_target(
                    &store,
                    p,
                    t,
                    &b,
                    tenant.as_str(),
                    path,
                    &started,
                    sign_key.as_ref(),
                    &mut warn,
                ) {
                    Ok(referrers) => {
                        if json {
                            emit(json!({
                                "event": "published",
                                "target": b.target,
                                "reference": p.reference.to_string(),
                                "digest": b.manifest_digest.to_string(),
                                "referrers": referrers.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
                            }));
                        } else {
                            println!(
                                "pushed {}@{} ({} attestations)",
                                sanitize(&p.reference.to_string()),
                                b.manifest_digest,
                                referrers.len()
                            );
                        }
                        Ok(b)
                    }
                    Err(e) => Err(e),
                }
            }
            (r, _) => r,
        };
        match built {
            Ok(b) if json => emit(json!({
                "event": "target_built",
                "target": b.target,
                "reference": b.reference,
                "digest": b.digest.to_string(),
                "executed": b.executed,
                "cached": b.cached,
                "layers": b.layers.kiln,
                "max_layers": b.layers.max_layers,
                "composite": b.layers.composite,
                "app_composite": b.layers.app_composite,
            })),
            Ok(b) => {
                if let Some(m) = render::layer_merges(&b.layers) {
                    eprintln!("[{}] layers: {m}", sanitize(&b.target));
                }
                println!(
                    "{} {} (executed {}, cached {})",
                    sanitize(&b.reference),
                    b.digest,
                    b.executed,
                    b.cached
                )
            }
            Err(e) => {
                if json {
                    let class = if e.exit_code() == 3 { "hostile" } else { "error" };
                    emit(json!({"event": class, "target": t.name, "message": e.to_string(), "exit": e.exit_code()}));
                }
                return fail(&e.to_string(), e.exit_code());
            }
        }
    }
    ExitCode::SUCCESS
}

fn plan(path: &Path, target: Option<&str>, json: bool, explain: bool, max_layers: Option<u32>) -> ExitCode {
    let mut planned = match potter_config::plan_repo(path, target) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string(), if e.is_infrastructure() { 4 } else { 2 }),
    };
    set_max_layers(&mut planned.plan, max_layers);
    if json {
        return match potter_plan::canonical_json(&planned.plan) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e.to_string(), 2),
        };
    }
    print!("{}", sanitize(&render::plan(&planned)));
    if explain {
        print!("\n{}", sanitize(&render::explain(&planned)));
    }
    ExitCode::SUCCESS
}

fn detect(path: &Path) -> ExitCode {
    let repo = match RepoView::open(path) {
        Ok(r) => r,
        Err(e) => {
            let infra = matches!(&e, RepoError::Io { source, .. } if source.kind() != ErrorKind::NotFound);
            return fail(&e.to_string(), if infra { 4 } else { 2 });
        }
    };
    let claims = match potter_detect::claims(&repo, &DetectOptions::default()) {
        Ok(c) => c,
        Err(e) => return fail(&e.to_string(), 2),
    };
    let chosen = potter_detect::select(&claims, &repo);
    print!("{}", sanitize(&render::detect(&claims, chosen.as_ref().ok().copied())));
    match chosen {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(&e.to_string(), 2),
    }
}

fn catalog_arch(arch: Option<&str>) -> Result<potter_catalog::Arch, (String, u8)> {
    use potter_catalog::Arch;
    match arch {
        Some("x86_64") => Ok(Arch::X86_64),
        Some("aarch64") => Ok(Arch::Aarch64),
        Some(other) => Err((format!("--arch {other:?} is neither x86_64 nor aarch64"), 2)),
        None => potter_exec::chain::catalog_arch(&kiln_oci::Platform::host()).map_err(|e| (e.to_string(), 2)),
    }
}

fn catalog(command: CatalogCmd) -> ExitCode {
    match run_catalog(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err((e, code)) => fail(&e, code),
    }
}

fn run_catalog(command: CatalogCmd) -> Result<(), (String, u8)> {
    use potter_catalog::{Entry, fetch_layer};
    let catalog_err = |e: potter_catalog::CatalogError| (e.to_string(), e.exit_code());
    // Building and publishing make a catalog: they never read the configured one.
    let command = match command {
        CatalogCmd::Build {
            catalog_dir,
            out,
            sign_key,
            previous,
            merge,
            merge_key,
        } => {
            return match catalog_dir {
                Some(dir) => catalog_build(&dir, &out, &sign_key, previous.as_deref()),
                None => catalog_merge(&merge, &merge_key, &out, &sign_key, previous.as_deref()),
            };
        }
        CatalogCmd::Publish { layout, registry_ref } => return catalog_publish(&layout, &registry_ref),
        other => other,
    };
    let pin = match &command {
        CatalogCmd::Verify { catalog_index } => catalog_index.as_deref(),
        _ => None,
    };
    // Reject a bad --arch before touching the catalog.
    let arch = match &command {
        CatalogCmd::Ls { arch, .. } | CatalogCmd::Resolve { arch, .. } => Some(catalog_arch(arch.as_deref())?),
        _ => None,
    };
    let (store, home, _) = open_homes().map_err(|e| (e, 4))?;
    let auth = kiln_registry::DockerConfig::from_env;
    let location = potter_catalog::Location::from_env().map_err(catalog_err)?;
    let verified = potter_catalog::load(&location, pin, home.root(), &store, auth()).map_err(catalog_err)?;
    let index = &verified.index;
    let print = |s: String| print!("{}", sanitize(&s));
    let to_json = |v: serde_json::Value| format!("{}\n", serde_json::to_string_pretty(&v).expect("json"));
    match command {
        CatalogCmd::Ls { all, json, .. } => {
            let arch = arch.expect("ls has an arch");
            let successor = |e: &Entry| {
                index
                    .entries
                    .iter()
                    .find(|n| n.supersedes.as_deref() == Some(&e.digest))
            };
            let rows: Vec<(&Entry, Option<&Entry>)> = match all {
                true => index.entries.iter().map(|e| (e, successor(e))).collect(),
                false => index.current().filter(|e| e.arch == arch).map(|e| (e, None)).collect(),
            };
            if json {
                let v: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(e, by)| {
                        let mut v = serde_json::to_value(e).expect("json");
                        v["superseded_by"] = by.map_or(serde_json::Value::Null, |n| n.digest.clone().into());
                        v
                    })
                    .collect();
                print(to_json(v.into()));
            } else {
                let rows: Vec<_> = rows
                    .into_iter()
                    .map(|(e, by)| {
                        (
                            e,
                            by.map(|n| format!("superseded by {}", render::short_digest(&n.digest))),
                        )
                    })
                    .collect();
                print(render::catalog_table(&rows));
            }
        }
        CatalogCmd::Resolve { constraints, json, .. } => {
            let refs = constraints
                .iter()
                .map(|c| match c.split_once('@') {
                    Some((n, l)) if !n.is_empty() && !l.is_empty() => Ok(potter_plan::CatalogRef::new(n, Some(l))),
                    None if !c.is_empty() => Ok(potter_plan::CatalogRef::new(c, None)),
                    _ => Err((format!("constraint {c:?} is not name or name@line"), 2)),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let r =
                potter_catalog::resolve(&verified, &refs, arch.expect("resolve has an arch")).map_err(catalog_err)?;
            if json {
                print(to_json(serde_json::json!({
                    "entries": r.entries,
                    "constraints": r.constraints,
                    "index_digest": r.index_digest,
                    "key": r.composite_key(),
                })));
            } else {
                let rows: Vec<_> = r.entries.iter().map(|e| (e, None)).collect();
                print(render::catalog_table(&rows));
                print(format!("key {}\n", r.composite_key()));
            }
        }
        CatalogCmd::Verify { .. } => {
            let mut n = 0;
            for e in index.current() {
                let descriptor = fetch_layer(&verified, e, &store, auth()).map_err(catalog_err)?;
                // fetch_layer trusts a blob already in the store: hash it again.
                let hostile = |m: String| (format!("catalog layer {} {}: {m}", e.name, e.digest), 3);
                let file = store
                    .open_blob(&descriptor.digest)
                    .map_err(|err| hostile(err.to_string()))?;
                let (actual, _) = kiln_store::HashingReader::new(file)
                    .finish_to_eof()
                    .map_err(|err| hostile(err.to_string()))?;
                if actual != descriptor.digest {
                    return Err(hostile(format!("the stored blob hashes to {actual}")));
                }
                n += 1;
            }
            println!("ok {n} layers");
        }
        CatalogCmd::Build { .. } | CatalogCmd::Publish { .. } => unreachable!("handled above"),
        CatalogCmd::Affected { what, json } => {
            let mut rows: Vec<(&Entry, Option<String>)> = Vec::new();
            if what.starts_with("sha256:") {
                let target = index
                    .by_digest(&what)
                    .ok_or_else(|| (format!("the catalog has no entry with digest {what}"), 2))?;
                let mut seen = std::collections::BTreeSet::from([target.digest.as_str()]);
                let mut at = target;
                while let Some(older) = at.supersedes.as_deref().and_then(|d| index.by_digest(d)) {
                    if !seen.insert(older.digest.as_str()) {
                        break;
                    }
                    rows.push((older, Some("superseded".into())));
                    at = older;
                }
                let mut frontier = vec![target.digest.as_str()];
                while let Some(d) = frontier.pop() {
                    for newer in index.entries.iter().filter(|e| e.supersedes.as_deref() == Some(d)) {
                        if seen.insert(newer.digest.as_str()) {
                            rows.push((newer, Some("supersedes".into())));
                            frontier.push(newer.digest.as_str());
                        }
                    }
                }
            } else {
                rows = index
                    .entries
                    .iter()
                    .filter(|e| e.fixes.contains(&what))
                    .map(|e| (e, None))
                    .collect();
            }
            if json {
                print(to_json(
                    rows.iter()
                        .map(|(e, _)| serde_json::to_value(e).expect("json"))
                        .collect(),
                ));
            } else {
                print(render::catalog_table(&rows));
            }
        }
    }
    Ok(())
}

/// `potter catalog build`: every recipe built with the namespace executor in the `catalog`
/// tenant, checked, and written to `out` signed with `sign_key`. Exit 1 when a layer overlaps
/// another or fails its smoke test, 2 for a recipe or usage error (a missing or unreadable
/// `--sign-key` too), 3 when `previous` does not verify, 4 for infrastructure.
fn catalog_build(catalog_dir: &Path, out: &Path, sign_key: &Path, previous: Option<&str>) -> Result<(), (String, u8)> {
    use potter_exec::chain::{Progress, StepState};
    let built = |e: potter_exec::BuildError| (e.to_string(), e.exit_code());
    let (key, previous) = build_args(sign_key, previous)?;
    if !cfg!(target_os = "linux") {
        return Err(("the namespace executor needs Linux".into(), 4));
    }
    let (store, home, potter) = open_homes().map_err(|e| (e, 4))?;
    let sandbox = potter_exec::sandboxed::SandboxCtx {
        potter,
        runs: home.runs(),
    };
    let _lock = potter_exec::sandboxed::begin_build(&sandbox, &home).map_err(built)?;
    let mut progress = |p: Progress| match p {
        Progress::Step { target, step, state } => {
            let state = match state {
                StepState::Cached | StepState::Pulled => "cached".to_string(),
                StepState::Pushed => "pushed".to_string(),
                StepState::Running => "running".to_string(),
                StepState::Done { secs } => format!("done ({secs:.1}s)"),
                StepState::Failed => "failed".to_string(),
            };
            eprintln!("[{}] {}: {state}", sanitize(&target), sanitize(&step));
        }
        Progress::Warning(w) => eprintln!("warning: {}", sanitize(&w)),
        Progress::Log { target, step, line } => {
            eprintln!("[{}/{}] {}", sanitize(&target), sanitize(&step), sanitize(&line))
        }
    };
    let mut parts = potter_exec::catalog_build::Parts {
        store: &store,
        home: &home,
        sandbox: &sandbox,
        platform: kiln_oci::Platform::host(),
        auth: kiln_registry::DockerConfig::from_env(),
        progress: &mut progress,
    };
    let req = potter_exec::catalog_build::Request {
        catalog_dir,
        out,
        sign_key: &key,
        previous: previous.as_ref(),
        created: now_rfc3339(),
    };
    let report = potter_exec::catalog_build::build(&mut parts, &req).map_err(built)?;
    print_report(&report, out);
    Ok(())
}

/// `--sign-key` and `--previous` of `potter catalog build`, read and parsed.
fn build_args(
    sign_key: &Path,
    previous: Option<&str>,
) -> Result<(potter_catalog::SigningKey, Option<potter_catalog::Location>), (String, u8)> {
    let pem = std::fs::read_to_string(sign_key).map_err(|e| (format!("--sign-key {}: {e}", sign_key.display()), 2))?;
    let key = potter_catalog::signing_key_from_pem(&pem)
        .map_err(|e| (format!("--sign-key {}: {e}", sign_key.display()), 2))?;
    let previous = previous
        .map(potter_catalog::Location::parse)
        .transpose()
        .map_err(|e| (format!("--previous: {e}"), e.exit_code()))?;
    Ok((key, previous))
}

/// `potter catalog build --merge`: the per-architecture layouts as one index, signed with
/// `sign_key`. Needs no sandbox. 2 when the layouts were not built on the same `previous` or
/// overlap in architecture, 3 when one does not verify, 4 for infrastructure.
fn catalog_merge(
    layouts: &[PathBuf],
    layout_keys: &[PathBuf],
    out: &Path,
    sign_key: &Path,
    previous: Option<&str>,
) -> Result<(), (String, u8)> {
    let (key, previous) = build_args(sign_key, previous)?;
    let layout_keys = layout_keys
        .iter()
        .map(|p| {
            let pem = std::fs::read_to_string(p).map_err(|e| (format!("--merge-key {}: {e}", p.display()), 2))?;
            potter_catalog::TrustedKey::from_pem(&pem).map_err(|e| (format!("--merge-key {}: {e}", p.display()), 2))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (store, home, _) = open_homes().map_err(|e| (e, 4))?;
    let req = potter_exec::catalog_build::MergeRequest {
        layouts,
        layout_keys: &layout_keys,
        out,
        sign_key: &key,
        previous: previous.as_ref(),
        created: now_rfc3339(),
    };
    let report = potter_exec::catalog_build::merge(&store, home.root(), kiln_registry::DockerConfig::from_env(), &req)
        .map_err(|e| (e.to_string(), e.exit_code()))?;
    print_report(&report, out);
    Ok(())
}

/// One line per layer of `report`, then the index written to `out`.
fn print_report(report: &potter_exec::catalog_build::Report, out: &Path) {
    for (e, outcome) in &report.layers {
        use potter_exec::catalog_build::assign::Outcome;
        let what = match outcome {
            Outcome::New => "new".to_string(),
            Outcome::Kept => "kept".to_string(),
            Outcome::Supersedes(old) => format!("supersedes {old}"),
        };
        println!("{} {} {} {what}", sanitize(&e.name), sanitize(&e.version), e.digest);
    }
    println!(
        "wrote {}: serial {}, index {}",
        sanitize(&out.display().to_string()),
        report.index.serial,
        report.digest
    );
}

/// `potter catalog publish`: 3 when the layout does not verify under the trusted keys, 4 when
/// the registry fails.
fn catalog_publish(layout: &Path, registry_ref: &str) -> Result<(), (String, u8)> {
    let to = kiln_registry::Reference::parse(registry_ref).map_err(|e| (format!("{registry_ref}: {e}"), 2))?;
    let v = potter_exec::catalog_build::publish(layout, &to, kiln_registry::DockerConfig::from_env())
        .map_err(|e| (e.to_string(), e.exit_code()))?;
    println!(
        "pushed {} serial {} index {}",
        sanitize(registry_ref),
        v.index.serial,
        v.digest
    );
    Ok(())
}
