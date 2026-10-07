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
        } => build(&path, &target, executor, &tenant, no_cache, &tags),
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

fn build(
    path: &Path,
    only: &[String],
    executor: Option<Executor>,
    tenant: &str,
    no_cache: bool,
    tags: &[String],
) -> ExitCode {
    use potter_exec::chain::{BuildCtx, Progress, StepState};
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
    let targets: Vec<&potter_plan::Target> = planned
        .plan
        .targets
        .iter()
        .filter(|t| only.is_empty() || only.contains(&t.name))
        .collect();
    if let Some(missing) = only
        .iter()
        .find(|o| !planned.plan.targets.iter().any(|t| &&t.name == o))
    {
        return fail(&format!("no target named {missing:?}"), 2);
    }
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
    let index = match home.index(&tenant) {
        Ok(i) => i,
        Err(e) => return fail(&e.to_string(), 4),
    };
    let sandbox = potter_exec::sandboxed::SandboxCtx {
        potter,
        runs: home.runs(),
    };
    let mut progress = |p: Progress| match p {
        Progress::Step { target, step, state } => {
            let state = match state {
                StepState::Cached => "cached".to_string(),
                StepState::Running => "running".to_string(),
                StepState::Done { secs } => format!("done ({secs:.1}s)"),
                StepState::Failed => "failed".to_string(),
            };
            eprintln!("[{}] {}: {state}", sanitize(&target), sanitize(&step));
        }
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
        progress: &mut progress,
    };
    for t in targets {
        match potter_exec::assemble::build_target(&mut ctx, t, path, tags) {
            Ok(b) => println!(
                "{} {} (executed {}, cached {})",
                sanitize(&b.reference),
                b.digest,
                b.executed,
                b.cached
            ),
            Err(e) => return fail(&e.to_string(), e.exit_code()),
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
