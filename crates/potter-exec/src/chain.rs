//! The chain driver (spec §9.1, §8.2): keys from the base down, the longest cached prefix,
//! and the misses run in the sandbox, sealed and recorded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use kiln_oci::{Descriptor, Platform, media};
use kiln_registry::DockerConfig;
use kiln_store::{Digest, Store};
use potter_cache::{Entry, ExecutorClass, Index, PotterHome, StepInput, Tenant, base_key, step_key};
use potter_plan::{Base, Chain, Step, Target};

use crate::base::{BaseImage, layer_reader, resolve_base};
use crate::error::{BuildError, io};
use crate::inputs::{InputsSpec, build_inputs};
use crate::merge::{LayerOpen, read_file};
use crate::ns::{StepRun, mount_points, run_step, wrap};
use crate::sandboxed::SandboxCtx;
use crate::seal::seal_in_sandbox;
use crate::tarball::{epoch, owner};
use crate::unpack::ensure_unpacked;

/// The default step timeout (spec §12).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub struct BuildCtx<'a> {
    pub store: &'a Store,
    pub home: &'a PotterHome,
    pub tenant: &'a Tenant,
    pub index: &'a Index,
    pub sandbox: &'a SandboxCtx,
    pub platform: Platform,
    pub no_cache: bool,
    pub auth: DockerConfig,
    pub progress: &'a mut dyn FnMut(Progress),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepState {
    Cached,
    Running,
    Done { secs: f64 },
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Step {
        target: String,
        step: String,
        state: StepState,
    },
    Warning(String),
    Log {
        target: String,
        step: String,
        line: String,
    },
}

/// One layer of the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRef {
    pub descriptor: Descriptor,
    /// The uncompressed tar's digest.
    pub diff_id: Digest,
}

/// Where a chain ended.
#[derive(Debug, Clone)]
pub struct ChainState {
    pub base: BaseImage,
    /// Bottom first: the base's layers, then each step's.
    pub layers: Vec<LayerRef>,
    pub env: BTreeMap<String, String>,
    pub workdir: String,
    pub user: String,
    pub executed: usize,
    pub cached: usize,
}

fn unsupported(what: String, milestone: &'static str) -> BuildError {
    BuildError::Unsupported { what, milestone }
}

fn check_base(base: &Base, what: &str) -> Result<(), BuildError> {
    match base {
        // Catalog bases are lowered onto upstream images until P3 (stopgap.rs).
        Base::Oci { .. } | Base::Catalog { .. } => Ok(()),
        Base::Stage { .. } => Err(unsupported(
            format!("the stage base of {what}"),
            "P5 (the Dockerfile frontend)",
        )),
    }
}

/// Refuses steps potter cannot run yet, naming them and the milestone that ships them.
fn check_steps(what: &str, steps: &[Step]) -> Result<(), BuildError> {
    for step in steps {
        match step {
            Step::CopyFrom { name: s, .. } => {
                return Err(unsupported(
                    format!("the copy_from step {s} of {what}"),
                    "P6 (the graph)",
                ));
            }
            Step::Run { name: s, secrets, .. } if !secrets.is_empty() => {
                return Err(unsupported(
                    format!("the secrets of step {s} of {what}"),
                    "P6 (secrets)",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Refuses what potter cannot build yet, naming it and the milestone that ships it.
pub fn check_supported(target: &Target) -> Result<(), BuildError> {
    let name = &target.name;
    check_base(&target.build.base, &format!("target {name}"))?;
    check_base(&target.image.base, &format!("target {name}'s image"))?;
    check_steps(&format!("target {name}"), &target.build.steps)?;
    check_steps(&format!("target {name}'s image"), &target.image.runtime_steps)?;
    if !target.image.services.is_empty() {
        return Err(unsupported(
            format!("the services of target {name} (nginx and php-fpm)"),
            "kiln milestone M3d (kiln-init services)",
        ));
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Opens each layer of `layers` (bottom first) for the merged view.
fn openers<'a>(store: &'a Store, layers: &'a [LayerRef]) -> Vec<LayerOpen<'a>> {
    layers
        .iter()
        .map(|l| Box::new(move || layer_reader(store, &l.descriptor)) as LayerOpen<'a>)
        .collect()
}

/// `/etc/passwd` and `/etc/group` of the merged view, when they exist.
fn accounts(store: &Store, layers: &[LayerRef]) -> Result<(Option<String>, Option<String>), BuildError> {
    let open = openers(store, layers);
    let read = |p: &str| -> Result<Option<String>, BuildError> {
        Ok(read_file(&open, p, 1 << 20)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
    };
    Ok((read("/etc/passwd")?, read("/etc/group")?))
}

/// Numeric ids for `spec`, looking names up in the image only when needed.
fn ids(spec: &str, store: &Store, layers: &[LayerRef], what: &str) -> Result<(u64, u64, Option<String>), BuildError> {
    let numeric = spec.split(':').all(|p| p.parse::<u64>().is_ok() || p == "root");
    let (passwd, group) = if numeric {
        (None, None)
    } else {
        accounts(store, layers)?
    };
    let (uid, gid) =
        owner(spec, passwd.as_deref(), group.as_deref()).map_err(|e| BuildError::Plan(format!("{what}: {e}")))?;
    Ok((uid, gid, passwd))
}

/// The home directory of `uid` in passwd text.
fn home_of(passwd: Option<&str>, uid: u64) -> Option<String> {
    passwd?.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 6 && f[2].parse::<u64>().ok() == Some(uid)).then(|| f[5].to_string())
    })
}

/// Runs `target`'s build chain from `repo_root`.
pub fn run_chain(ctx: &mut BuildCtx, target: &Target, repo_root: &Path) -> Result<ChainState, BuildError> {
    check_supported(target)?;
    run_steps(
        ctx,
        &target.name,
        &target.build,
        &repo_root.join(&target.root),
        crate::stopgap::Role::Build,
    )
}

/// Runs `chain` for the target `tname`, whose sources are under `source_root`. A catalog base
/// is lowered onto its stopgap image and install steps; `apt` steps become installs.
pub fn run_steps(
    ctx: &mut BuildCtx,
    tname: &str,
    chain: &Chain,
    source_root: &Path,
    role: crate::stopgap::Role,
) -> Result<ChainState, BuildError> {
    check_base(&chain.base, tname)?;
    check_steps(tname, &chain.steps)?;
    let (reference, digest, prelude) = match &chain.base {
        Base::Oci { reference, digest } => (reference.clone(), digest.clone(), Vec::new()),
        Base::Catalog { layers } => {
            let lowered = crate::stopgap::lower(layers, role)?;
            (lowered.reference, None, lowered.steps)
        }
        Base::Stage { .. } => unreachable!("check_base refuses stages"),
    };
    let steps: Vec<Step> = prelude
        .into_iter()
        .chain(chain.steps.iter().map(crate::stopgap::lower_step))
        .collect();
    let base = resolve_base(
        ctx.store,
        &reference,
        digest.as_deref(),
        &ctx.platform,
        ctx.auth.clone(),
    )?;
    let mut state = ChainState {
        layers: base
            .layers()
            .iter()
            .zip(base.diff_ids())
            .map(|(d, id)| LayerRef {
                descriptor: d.clone(),
                diff_id: id.clone(),
            })
            .collect(),
        env: base.env(),
        workdir: base.workdir().unwrap_or_else(|| "/".into()),
        user: base.user().unwrap_or_else(|| "0:0".into()),
        executed: 0,
        cached: 0,
        base,
    };
    // Unpacked directories of the layers, filled in as misses need them.
    let mut lowers: Vec<Option<PathBuf>> = vec![None; state.layers.len()];
    let platform = ctx.platform.to_string();
    let mut key = base_key(&state.base.key_digest());
    let mut runs: Vec<PathBuf> = Vec::new();
    let tname = tname.to_string();
    let mut warned = false;
    let warned_cgroups = &mut warned;
    let result = (|| -> Result<(), BuildError> {
        for step in &steps {
            let sname = step.name().to_string();
            match step {
                Step::Meta { env, workdir, user, .. } => {
                    key = step_key(&key, &input(step, None, &state.env, &platform)).map_err(cache_err)?;
                    state.env.extend(env.clone());
                    if let Some(w) = workdir {
                        state.workdir = w.clone();
                    }
                    if let Some(u) = user {
                        state.user = u.clone();
                    }
                }
                Step::Inputs {
                    include,
                    exclude,
                    dest,
                    owner: who,
                    ..
                } => {
                    let (uid, gid, _) = ids(who, ctx.store, &state.layers, &format!("step {sname}"))?;
                    std::fs::create_dir_all(ctx.home.runs()).map_err(io("creating the runs directory"))?;
                    let file = tempfile::tempfile_in(ctx.home.runs()).map_err(io("creating an inputs layer"))?;
                    let spec = InputsSpec {
                        root: source_root,
                        include,
                        exclude,
                        dest,
                        owner: (uid, gid),
                        epoch: epoch(),
                    };
                    let (mut file, _) = build_inputs(&spec, file)?;
                    use std::io::Seek;
                    file.rewind().map_err(io("reading the inputs layer"))?;
                    let (d, size) = ctx
                        .store
                        .put_reader(&mut file)
                        .map_err(|e| BuildError::Infra(format!("storing the inputs of {sname}: {e}")))?;
                    key =
                        step_key(&key, &input(step, Some(&d.to_string()), &state.env, &platform)).map_err(cache_err)?;
                    state.layers.push(LayerRef {
                        descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), size),
                        diff_id: d,
                    });
                    lowers.push(None);
                }
                Step::Run {
                    command,
                    env,
                    workdir,
                    user,
                    network,
                    cache_mounts,
                    timeout_secs,
                    ..
                } => {
                    let mut step_env = state.env.clone();
                    step_env.extend(env.clone());
                    key = step_key(&key, &input(step, None, &step_env, &platform)).map_err(cache_err)?;
                    let hit = (!ctx.no_cache)
                        .then(|| ctx.index.get(&key).ok().flatten())
                        .flatten()
                        .and_then(|e| e.layer)
                        .and_then(|l| Digest::parse(&l).ok())
                        .filter(|d| ctx.store.has_blob(d));
                    if let Some(d) = hit {
                        let size = ctx.store.blob_size(&d).map_err(|e| BuildError::Infra(e.to_string()))?;
                        state.layers.push(LayerRef {
                            descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), size),
                            diff_id: d,
                        });
                        lowers.push(None);
                        state.cached += 1;
                        (ctx.progress)(Progress::Step {
                            target: tname.clone(),
                            step: sname.clone(),
                            state: StepState::Cached,
                        });
                        continue;
                    }
                    // A miss: every layer so far, unpacked, top first.
                    for (i, layer) in state.layers.iter().enumerate() {
                        if lowers[i].is_none() {
                            lowers[i] = Some(ensure_unpacked(ctx.sandbox, ctx.store, &layer.descriptor, ctx.home)?);
                        }
                    }
                    let stack: Vec<PathBuf> = lowers
                        .iter()
                        .rev()
                        .map(|l| l.clone().expect("unpacked above"))
                        .collect();
                    let (uid, gid, passwd) = ids(user, ctx.store, &state.layers, &format!("step {sname}"))?;
                    if let Some(home) = home_of(passwd.as_deref(), uid) {
                        step_env.entry("HOME".into()).or_insert(home);
                    }
                    std::fs::create_dir_all(ctx.home.runs()).map_err(io("creating the runs directory"))?;
                    let run = tempfile::Builder::new()
                        .prefix("step-")
                        .tempdir_in(ctx.home.runs())
                        .map_err(io("creating a step directory"))?
                        .keep();
                    runs.push(run.clone());
                    let mut mounts = Vec::new();
                    // Held until the step ends: exclusive mounts (spec §8.5).
                    let mut held = Vec::new();
                    for m in cache_mounts {
                        if !potter_plan::is_name(&m.id) {
                            return Err(BuildError::Plan(format!(
                                "step {sname}: the cache mount id {:?} is not a name",
                                m.id
                            )));
                        }
                        let root = ctx.home.mounts(ctx.tenant);
                        std::fs::create_dir_all(&root).map_err(io(format!("creating {}", root.display())))?;
                        let lock_path = root.join(format!("{}.lock", m.id));
                        let lock = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&lock_path)
                            .map_err(io(format!("opening {}", lock_path.display())))?;
                        let free =
                            rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok();
                        let dir = if free {
                            held.push(lock);
                            root.join(&m.id)
                        } else {
                            // Another build holds it: an empty scratch mount, slower but correct.
                            (ctx.progress)(Progress::Warning(format!(
                                "step {sname}: the cache mount {} is in use by another build; using an empty one",
                                m.id
                            )));
                            run.join(format!("scratch-{}", m.id))
                        };
                        std::fs::create_dir_all(&dir).map_err(io(format!("creating {}", dir.display())))?;
                        mounts.push((dir, m.target.clone()));
                    }
                    if !*warned_cgroups && !vmkit::sandbox::cgroups_available() {
                        *warned_cgroups = true;
                        (ctx.progress)(Progress::Warning(
                            "no cgroup delegation (systemd user scopes): steps run without memory, CPU and task \
                             limits, bounded only by their timeout"
                                .into(),
                        ));
                    }
                    for d in ["upper", "work", "run"] {
                        std::fs::create_dir(run.join(d)).map_err(io("creating a step directory"))?;
                    }
                    (ctx.progress)(Progress::Step {
                        target: tname.clone(),
                        step: sname.clone(),
                        state: StepState::Running,
                    });
                    let started = Instant::now();
                    let progress = &mut *ctx.progress;
                    let mut log = |line: &str| {
                        progress(Progress::Log {
                            target: tname.clone(),
                            step: sname.clone(),
                            line: line.to_string(),
                        })
                    };
                    let timeout = timeout_secs.map_or(STEP_TIMEOUT, Duration::from_secs);
                    let step_run = StepRun {
                        lowers: stack,
                        upper: run.join("upper"),
                        work: run.join("work"),
                        args: wrap(command, workdir),
                        env: step_env,
                        user: vmkit::sandbox::User {
                            uid: u32::try_from(uid)
                                .map_err(|_| BuildError::Plan(format!("step {sname}: uid {uid}")))?,
                            gid: u32::try_from(gid)
                                .map_err(|_| BuildError::Plan(format!("step {sname}: gid {gid}")))?,
                            groups: vec![],
                        },
                        network: *network,
                        cache_mounts: mounts,
                        timeout,
                        run_dir: run.join("run"),
                        log: &mut log,
                    };
                    let skip = mount_points(&step_run);
                    let outcome = run_step(step_run)?;
                    drop(held);
                    if !outcome.status.success() {
                        (ctx.progress)(Progress::Step {
                            target: tname.clone(),
                            step: sname.clone(),
                            state: StepState::Failed,
                        });
                        let status = if outcome.timed_out {
                            format!("timed out after {}s", timeout.as_secs())
                        } else {
                            outcome.status.to_string()
                        };
                        return Err(BuildError::StepFailed {
                            step: sname.clone(),
                            status,
                            tail: outcome.tail,
                        });
                    }
                    let (d, size) =
                        seal_in_sandbox(ctx.sandbox, &run.join("upper"), &run.join("run"), ctx.store, &skip)?;
                    ctx.index
                        .put(
                            &key,
                            &Entry {
                                layer: Some(d.to_string()),
                                class: ExecutorClass::Ns,
                                created: now(),
                                potter: env!("CARGO_PKG_VERSION").into(),
                            },
                        )
                        .map_err(cache_err)?;
                    state.layers.push(LayerRef {
                        descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), size),
                        diff_id: d,
                    });
                    lowers.push(Some(run.join("upper")));
                    state.executed += 1;
                    (ctx.progress)(Progress::Step {
                        target: tname.clone(),
                        step: sname.clone(),
                        state: StepState::Done {
                            secs: started.elapsed().as_secs_f64(),
                        },
                    });
                }
                Step::Apt { .. } | Step::CopyFrom { .. } => unreachable!("checked above"),
            }
        }
        Ok(())
    })();
    for run in runs {
        let _ = ctx.sandbox.remove(&run);
    }
    result.map(|()| state)
}

fn input<'a>(
    step: &'a Step,
    inputs_digest: Option<&'a str>,
    env: &'a BTreeMap<String, String>,
    platform: &'a str,
) -> StepInput<'a> {
    StepInput {
        step,
        inputs_digest,
        env,
        platform,
        class: ExecutorClass::Ns,
    }
}

fn cache_err(e: potter_cache::CacheError) -> BuildError {
    BuildError::Infra(format!("the cache: {e}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use potter_plan::*;

    use super::*;

    fn target(base: Base, steps: Vec<Step>) -> Target {
        Target {
            name: "app".into(),
            root: ".".into(),
            platform: None,
            provider: "none".into(),
            build: Chain {
                base: base.clone(),
                steps,
            },
            image: Image {
                base,
                runtime_steps: vec![],
                export: vec![],
                files: vec![],
                config: ImageConfig::new("/", "0:0"),
                processes: BTreeMap::new(),
                services: BTreeMap::new(),
            },
        }
    }

    fn oci() -> Base {
        Base::Oci {
            reference: "busybox:1".into(),
            digest: None,
        }
    }

    fn run(name: &str) -> Step {
        Step::Run {
            name: name.into(),
            command: Command::shell("true"),
            env: BTreeMap::new(),
            workdir: "/".into(),
            user: "0:0".into(),
            network: true,
            cache_mounts: vec![],
            secrets: vec![],
            timeout_secs: None,
        }
    }

    #[test]
    fn run_steps_refuses_unsupported_steps_itself() {
        let dir = tempfile::tempdir().unwrap();
        let store = kiln_store::Store::open(dir.path().join("kiln")).unwrap();
        let home = PotterHome::open(dir.path().join("potter")).unwrap();
        let tenant = Tenant::local();
        let index = home.index(&tenant).unwrap();
        let sandbox = crate::sandboxed::SandboxCtx {
            potter: "/nonexistent/potter".into(),
            runs: home.runs(),
        };
        let mut progress = |_: Progress| {};
        let mut ctx = BuildCtx {
            store: &store,
            home: &home,
            tenant: &tenant,
            index: &index,
            sandbox: &sandbox,
            platform: kiln_oci::Platform::host(),
            no_cache: false,
            auth: kiln_registry::DockerConfig::anonymous(),
            progress: &mut progress,
        };
        let copy = Step::CopyFrom {
            name: "copy".into(),
            from: CopySource::Target("web".into()),
            paths: vec!["/x".into()],
            dest: "/x".into(),
            owner: "0:0".into(),
            mode: None,
        };
        let chain = Chain {
            base: oci(),
            steps: vec![copy],
        };
        let err = run_steps(&mut ctx, "app:run", &chain, dir.path(), crate::stopgap::Role::Run).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("copy"), "{err}");
    }

    #[test]
    fn catalog_bases_apt_steps_and_runtime_steps_are_accepted() {
        let catalog = Base::Catalog {
            layers: vec![CatalogRef::new("os-min", None), CatalogRef::new("node", Some("22"))],
        };
        let apt = Step::Apt {
            name: "pkgs".into(),
            packages: vec!["curl".into()],
            snapshot: DEFAULT_APT_SNAPSHOT.into(),
        };
        let mut t = target(catalog, vec![apt.clone(), run("ok")]);
        t.image.runtime_steps.push(apt);
        assert!(check_supported(&t).is_ok(), "{:?}", check_supported(&t));
    }

    #[test]
    fn what_p2b_does_not_ship_is_refused_by_name() {
        let copy = Step::CopyFrom {
            name: "copy".into(),
            from: CopySource::Target("web".into()),
            paths: vec!["/x".into()],
            dest: "/x".into(),
            owner: "0:0".into(),
            mode: None,
        };
        let mut secret = run("secret");
        if let Step::Run { secrets, .. } = &mut secret {
            secrets.push(SecretRef {
                id: "npm".into(),
                target: "/run/secrets/npm".into(),
            });
        }
        let mut services = target(oci(), vec![]);
        services.image.services.insert(
            "web".into(),
            vec![Service {
                name: "php-fpm".into(),
                command: vec!["php-fpm".into()],
                ready: Ready::None,
                stop_signal: "SIGQUIT".into(),
            }],
        );
        let stage = Base::Stage { name: "build".into() };
        let cases = [
            (target(stage, vec![]), "stage"),
            (target(oci(), vec![copy]), "copy"),
            (target(oci(), vec![secret]), "secret"),
            (services, "services"),
        ];
        for (t, named) in cases {
            let err = check_supported(&t).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{err}");
            assert!(err.to_string().contains(named), "{err}");
        }
    }
}
