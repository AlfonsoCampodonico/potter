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
enum KeyCmd {
    /// Write a new P-256 signing key: <dir>/potter.key (0600) and <dir>/potter.pub
    Generate { dir: PathBuf },
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
        Cmd::Rebase {
            image,
            base,
            tag,
            executor,
            tenant,
            json,
        } => rebase(&image, base.as_deref(), tag.as_deref(), executor, &tenant, json),
        Cmd::Verify {
            image,
            key,
            expect_tenant,
            expect_builder,
        } => verify(&image, &key, expect_tenant.as_deref(), expect_builder.as_deref()),
        Cmd::Key {
            command: KeyCmd::Generate { dir },
        } => key_generate(&dir),
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

fn key_generate(dir: &Path) -> ExitCode {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let key = potter_supply::sign::SigningKey::generate();
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
    if let Err((e, code)) = write("potter.key", 0o600, &key.to_pem())
        .and_then(|()| write("potter.pub", 0o644, &key.verifying_key().to_pem()))
    {
        return fail(&e, code);
    }
    println!("{}", dir.join("potter.pub").display());
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

fn rebase(
    image: &str,
    base: Option<&str>,
    tag: Option<&str>,
    executor: Option<Executor>,
    tenant: &str,
    json: bool,
) -> ExitCode {
    use potter_exec::chain::{BuildCtx, Progress, StepState};
    if let Err(code) = check_executor(executor) {
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
    let tenant_key = match potter_cache::tenant_key(&home, &tenant) {
        Ok(k) => k,
        Err(e) => return fail(&e.to_string(), 4),
    };
    let index = match home.index(&tenant) {
        Ok(i) => i,
        Err(e) => return fail(&e.to_string(), 4),
    };
    let sandbox = potter_exec::sandboxed::SandboxCtx {
        potter,
        runs: home.runs(),
    };
    let _lock = match potter_exec::sandboxed::begin_build(&sandbox, &home) {
        Ok(l) => l,
        Err(e) => return fail(&e.to_string(), e.exit_code()),
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
        progress: &mut progress,
    };
    let req = potter_exec::rebase::Request { image, base, tag };
    let r = match potter_exec::rebase::rebase(&mut ctx, &req) {
        Ok(r) => r,
        Err(e) => return fail(&e.to_string(), e.exit_code()),
    };
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
        println!(
            "{} {} on {}@{} ({} app layers kept, {} re-derived; replaced run base {})",
            sanitize(r.tag.as_deref().unwrap_or(image)),
            r.digest,
            sanitize(&r.base_ref),
            r.base_digest,
            r.reused,
            r.changed,
            r.old_base
        );
    }
    ExitCode::SUCCESS
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
