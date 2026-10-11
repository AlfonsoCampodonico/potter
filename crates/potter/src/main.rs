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
        } => plan(&path, target.as_deref(), json, explain),
        Cmd::Detect { path } => detect(&path),
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
        } => {
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
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
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
) -> ExitCode {
    use potter_exec::chain::{BuildCtx, Progress, StepState};
    use serde_json::json;
    const VM: &str = "the VM executor ships in milestone P4";
    match executor {
        None => {
            return fail(
                &format!(
                    "choose an executor: {VM}; pass --executor ns to build in Linux namespaces, which isolate less than a VM"
                ),
                4,
            );
        }
        Some(Executor::Vm) => return fail(&format!("{VM}; pass --executor ns to build in Linux namespaces"), 4),
        Some(Executor::Ns) => {}
    }
    if !cfg!(target_os = "linux") {
        return fail("the namespace executor needs Linux", 4);
    }
    if !tags.is_empty() && only.len() != 1 {
        return fail("--tag needs exactly one --target", 2);
    }
    let planned = match potter_config::plan_repo(path, None) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string(), if e.is_infrastructure() { 4 } else { 2 }),
    };
    // With --affected, the selection shrinks to the targets that changed since the ref.
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
    // Every selected target, after the targets it copies from (spec §10).
    let order = match potter_exec::graph::order(&planned.plan.targets, &selected) {
        Ok(o) => o,
        Err(e) => return fail(&e.to_string(), e.exit_code()),
    };
    let targets: Vec<&potter_plan::Target> = order.iter().map(|&i| &planned.plan.targets[i]).collect();
    let setup = || -> Result<_, String> {
        let store = kiln_store::Store::open(kiln_store::Store::default_root().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let home = potter_cache::PotterHome::open(potter_cache::PotterHome::default_root().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let potter = std::env::current_exe().map_err(|e| format!("finding potter itself: {e}"))?;
        Ok((store, home, potter))
    };
    let (store, home, potter) = match setup() {
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
    let tenant_key = match potter_cache::tenant_key(&home, &tenant) {
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
    for t in targets {
        match potter_exec::assemble::build_target(&mut ctx, t, path, if only.contains(&t.name) { tags } else { &[] }) {
            Ok(b) if json => emit(json!({
                "event": "target_built",
                "target": b.target,
                "reference": b.reference,
                "digest": b.digest.to_string(),
                "executed": b.executed,
                "cached": b.cached,
            })),
            Ok(b) => println!(
                "{} {} (executed {}, cached {})",
                sanitize(&b.reference),
                b.digest,
                b.executed,
                b.cached
            ),
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

fn plan(path: &Path, target: Option<&str>, json: bool, explain: bool) -> ExitCode {
    let planned = match potter_config::plan_repo(path, target) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string(), if e.is_infrastructure() { 4 } else { 2 }),
    };
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
