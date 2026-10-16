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
    /// Who runs the steps: decides the keys looked up and the class of new entries (spec §8.4).
    pub executor: ExecutorClass,
    /// The build VM, for `vm` builds (spec §9.3).
    pub backend: Option<&'a dyn crate::vm::backend::BuildBackend>,
    /// Build secrets by id (spec §5.4).
    pub secrets: &'a BTreeMap<String, Vec<u8>>,
    /// The tenant's key: secrets enter cache keys as its HMAC.
    pub tenant_key: [u8; 32],
    /// Where misses are looked up (spec §8.3).
    pub cache_from: Option<&'a crate::remote::RemoteCache>,
    /// Where sealed steps are published.
    pub cache_to: Option<&'a crate::remote::RemoteCache>,
    /// The final layers of the targets built so far, for `copy_from` (spec §9.6 step 2).
    pub deps: BTreeMap<String, Vec<LayerRef>>,
    pub progress: &'a mut dyn FnMut(Progress),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepState {
    Cached,
    /// A hit from the remote cache, now in the local one.
    Pulled,
    /// Pushed to the remote cache.
    Pushed,
    Running,
    Done {
        secs: f64,
    },
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

/// A layer the chain added, for provenance (spec §17, P12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    /// The chain's name: the target, or `<target>:run` for its run base.
    pub chain: String,
    pub step: String,
    /// The step's cache key (hex).
    pub key: Option<String>,
    pub layer: Digest,
    /// Who produced the layer: `None` for layers potter builds on the host (inputs, copies).
    pub class: Option<ExecutorClass>,
}

/// Where a chain ended.
#[derive(Debug, Clone)]
pub struct ChainState {
    pub base: BaseImage,
    /// The reference the base was resolved from (after catalog lowering).
    pub base_ref: String,
    /// Bottom first: the base's layers, then each step's.
    pub layers: Vec<LayerRef>,
    pub env: BTreeMap<String, String>,
    pub workdir: String,
    pub user: String,
    pub executed: usize,
    pub cached: usize,
    /// Each layer added on top of the base, in order.
    pub records: Vec<StepRecord>,
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
        if let Step::CopyFrom {
            name: s,
            from: potter_plan::CopySource::Stage(_),
            ..
        } = step
        {
            return Err(unsupported(
                format!("the stage source of step {s} of {what}"),
                "P5 (the Dockerfile frontend)",
            ));
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
    let numeric = owner_is_numeric(spec);
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
    if ctx.executor == ExecutorClass::Vm {
        if ctx.backend.is_none() {
            return Err(BuildError::Infra(
                "VM builds need kiln build mode (kiln M3e), which this potter does not have yet; pass --executor ns"
                    .into(),
            ));
        }
        if let Some(Step::Run { name, .. }) = chain
            .steps
            .iter()
            .find(|s| matches!(s, Step::Run { cache_mounts, .. } if !cache_mounts.is_empty()))
        {
            return Err(unsupported(
                format!("the cache mounts of step {name} of {tname} in a VM build"),
                "P4b (kiln build mode)",
            ));
        }
    }
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
        records: Vec::new(),
        base_ref: reference.clone(),
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
    let mut live = true;
    let remote_live = &mut live;
    // The VM running the chain since its first miss (vm builds).
    let mut vm_session: Option<crate::vm::Session> = None;
    // Layers of inputs and copies built ahead of their step for a VM's boot, by step index.
    let mut prebuilt: BTreeMap<usize, LayerRef> = BTreeMap::new();
    let session = &mut vm_session;
    let result = (|| -> Result<(), BuildError> {
        for (i, step) in steps.iter().enumerate() {
            let sname = step.name().to_string();
            match step {
                Step::Meta { env, workdir, user, .. } => {
                    key = step_key(&key, &input(step, None, &state.env, &platform, &[], HOST_CLASS))
                        .map_err(cache_err)?;
                    state.env.extend(env.clone());
                    if let Some(w) = workdir {
                        state.workdir = w.clone();
                    }
                    if let Some(u) = user {
                        state.user = u.clone();
                    }
                }
                Step::Inputs { .. } | Step::CopyFrom { .. } => {
                    // Built ahead when a VM booted for the chain, else now.
                    let layer = match prebuilt.remove(&i) {
                        Some(l) => l,
                        None => host_layer(ctx, step, &state.layers, source_root)?,
                    };
                    let d = layer.diff_id.clone();
                    // The layer's digest is the step's input (spec §8.1).
                    key = step_key(
                        &key,
                        &input(step, Some(&d.to_string()), &state.env, &platform, &[], HOST_CLASS),
                    )
                    .map_err(cache_err)?;
                    state.records.push(StepRecord {
                        chain: tname.clone(),
                        step: sname.clone(),
                        key: Some(key.hex()),
                        layer: d,
                        class: None,
                    });
                    state.layers.push(layer);
                    lowers.push(None);
                    // Stacked in the running VM before its next step, if it was attached at boot.
                    if let Some(vm) = session.as_mut()
                        && !vm.reached(i)
                    {
                        *session = None;
                    }
                }
                Step::Run {
                    command,
                    env,
                    workdir,
                    user,
                    network,
                    cache_mounts,
                    secrets,
                    timeout_secs,
                    ..
                } => {
                    let mut step_env = state.env.clone();
                    step_env.extend(env.clone());
                    let mut values: Vec<(String, Vec<u8>)> = Vec::new();
                    for s in secrets {
                        let value = ctx.secrets.get(&s.id).ok_or_else(|| {
                            BuildError::Plan(format!(
                                "step {sname} needs the secret {}: pass --secret id={},src=<file>",
                                s.id, s.id
                            ))
                        })?;
                        values.push((s.id.clone(), value.clone()));
                    }
                    let secrets_key = potter_cache::secrets_digest(&ctx.tenant_key, &values);
                    let parent = key;
                    // One key per class this build may use, most preferred first (spec §8.4).
                    let mut candidates = Vec::new();
                    for &class in ctx.executor.lookup_order() {
                        let k = step_key(&parent, &input(step, None, &step_env, &platform, &secrets_key, class))
                            .map_err(cache_err)?;
                        candidates.push((class, k));
                    }
                    key = candidates[0].1;
                    let local = |ctx: &BuildCtx, class: ExecutorClass, k: &potter_cache::CacheKey| {
                        (!ctx.no_cache)
                            .then(|| ctx.index.get(k).ok().flatten())
                            .flatten()
                            // An entry under a key of another class was not written by potter.
                            .filter(|e| e.class == class)
                            .and_then(|e| Digest::parse(&e.layer?).ok())
                            .filter(|d| ctx.store.has_blob(d))
                    };
                    // After a miss the rest of the chain runs in its VM, unlooked-up (spec §9.1).
                    let mut hit = candidates
                        .iter()
                        .filter(|_| session.is_none())
                        .find_map(|&(class, k)| local(ctx, class, &k).map(|d| (d, StepState::Cached, class, k)));
                    // The longest remote prefix: asked until its first miss (spec §8.3).
                    if hit.is_none()
                        && session.is_none()
                        && !ctx.no_cache
                        && *remote_live
                        && let Some(remote) = ctx.cache_from
                    {
                        for &(class, k) in &candidates {
                            match remote.get(ctx.store, &k, &parent, &[class], &ctx.home.runs()) {
                                Ok(Some(entry)) if entry.layer.is_some() => {
                                    ctx.index.put(&k, &entry).map_err(cache_err)?;
                                    hit = local(ctx, class, &k).map(|d| (d, StepState::Pulled, class, k));
                                    break;
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    *remote_live = false;
                                    (ctx.progress)(Progress::Warning(format!(
                                        "step {sname}: the remote cache failed, building locally: {e}"
                                    )));
                                    break;
                                }
                            }
                        }
                        if hit.is_none() {
                            *remote_live = false;
                        }
                    }
                    // A local hit is published too, so a remote holds every step of a chain.
                    if let (Some((d, StepState::Cached, class, key)), Some(remote)) = (&hit, ctx.cache_to) {
                        let entry = Entry {
                            layer: Some(d.to_string()),
                            class: *class,
                            created: now(),
                            potter: env!("CARGO_PKG_VERSION").into(),
                        };
                        if let Err(e) = remote.put(ctx.store, key, &parent, &entry) {
                            (ctx.progress)(Progress::Warning(format!(
                                "step {sname}: pushing to the remote cache failed: {e}"
                            )));
                        }
                    }
                    if let Some((d, how, class, k)) = hit {
                        key = k;
                        let size = ctx.store.blob_size(&d).map_err(|e| BuildError::Infra(e.to_string()))?;
                        state.records.push(StepRecord {
                            chain: tname.clone(),
                            step: sname.clone(),
                            key: Some(key.hex()),
                            layer: d.clone(),
                            class: Some(class),
                        });
                        state.layers.push(LayerRef {
                            descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), size),
                            diff_id: d,
                        });
                        lowers.push(None);
                        state.cached += 1;
                        (ctx.progress)(Progress::Step {
                            target: tname.clone(),
                            step: sname.clone(),
                            state: how,
                        });
                        continue;
                    }
                    // A miss: the step runs under this build's own class.
                    key = candidates
                        .iter()
                        .find(|(c, _)| *c == ctx.executor)
                        .expect("a build looks up its own class")
                        .1;
                    let started = Instant::now();
                    let (d, size, upper) = if ctx.executor == ExecutorClass::Vm {
                        let (uid, gid, passwd) = ids(user, ctx.store, &state.layers, &format!("step {sname}"))?;
                        if let Some(home) = home_of(passwd.as_deref(), uid) {
                            step_env.entry("HOME".into()).or_insert(home);
                        }
                        // The namespace executor's default (ns.rs), so a step sees the same env.
                        step_env
                            .entry("HOME".into())
                            .or_insert_with(|| if uid == 0 { "/root".into() } else { "/".into() });
                        // A VM has network for all its steps or none: a step that differs gets a new
                        // VM, booted from the prefix so far.
                        if session.as_ref().is_some_and(|vm| vm.network() != *network) {
                            *session = None;
                        }
                        if session.is_none() {
                            // Every disk is attached at boot: the layers of the inputs and copies
                            // ahead, up to the next step that needs another VM, are built now.
                            let mut later = Vec::new();
                            for (j, s) in steps.iter().enumerate().skip(i + 1) {
                                match s {
                                    Step::Run { network: n, .. } if n != network => break,
                                    Step::Inputs { .. } | Step::CopyFrom { .. } if !buildable_ahead(s) => break,
                                    Step::Inputs { .. } | Step::CopyFrom { .. } => {
                                        let l = match prebuilt.get(&j) {
                                            Some(l) => l.clone(),
                                            None => host_layer(ctx, s, &state.layers, source_root)?,
                                        };
                                        prebuilt.insert(j, l.clone());
                                        later.push((j, l));
                                    }
                                    _ => {}
                                }
                            }
                            let backend = ctx.backend.expect("checked before the chain");
                            *session = Some(crate::vm::Session::start(
                                backend,
                                ctx.store,
                                ctx.home,
                                &state.layers,
                                &later,
                                *network,
                            )?);
                        }
                        (ctx.progress)(Progress::Step {
                            target: tname.clone(),
                            step: sname.clone(),
                            state: StepState::Running,
                        });
                        let redact = redactor(&values);
                        let mut tail = std::collections::VecDeque::with_capacity(crate::ns::TAIL);
                        let timeout = timeout_secs.map_or(STEP_TIMEOUT, Duration::from_secs);
                        let run_step = crate::vm::backend::RunStep {
                            id: 0,
                            command: command.clone(),
                            env: step_env,
                            workdir: workdir.clone(),
                            uid: u32::try_from(uid)
                                .map_err(|_| BuildError::Plan(format!("step {sname}: uid {uid}")))?,
                            gid: u32::try_from(gid)
                                .map_err(|_| BuildError::Plan(format!("step {sname}: gid {gid}")))?,
                            lowers_added: vec![],
                            mounts: vec![],
                            secrets: secrets
                                .iter()
                                .map(|s| s.target.clone())
                                .zip(values.iter().map(|(_, v)| crate::vm::backend::Secret(v.clone())))
                                .collect(),
                            network: *network,
                            timeout,
                        };
                        let vm = session.as_mut().expect("started above");
                        let outcome = {
                            let progress = &mut *ctx.progress;
                            // Whole lines are redacted and reported, as for the namespace executor.
                            let mut line = |line: &str| {
                                let line = redact(line);
                                if tail.len() == crate::ns::TAIL {
                                    tail.pop_front();
                                }
                                tail.push_back(line.clone());
                                progress(Progress::Log {
                                    target: tname.clone(),
                                    step: sname.clone(),
                                    line,
                                })
                            };
                            let mut lines = crate::vm::Lines::default();
                            let outcome = vm.run(run_step, &mut |stream, bytes| lines.feed(stream, bytes, &mut line));
                            lines.flush(&mut line);
                            outcome?
                        };
                        if !outcome.exit.success() {
                            (ctx.progress)(Progress::Step {
                                target: tname.clone(),
                                step: sname.clone(),
                                state: StepState::Failed,
                            });
                            let status = match outcome.exit {
                                crate::vm::backend::Exit::TimedOut => format!("timed out after {}s", timeout.as_secs()),
                                e => e.to_string(),
                            };
                            return Err(BuildError::StepFailed {
                                step: sname.clone(),
                                status,
                                tail: tail.into(),
                            });
                        }
                        let report = outcome.sealed.ok_or_else(|| {
                            BuildError::Hostile(format!("the guest ran step {sname} but reported no sealed diff"))
                        })?;
                        let (d, size) = vm
                            .disk
                            .take(&report, ctx.store, &ctx.home.runs(), crate::remote::MAX_LAYER)?;
                        (d, size, None)
                    } else {
                        // Every layer so far, unpacked, top first.
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
                        // Each secret as a 0400 file in the step's run directory, bound read-only.
                        let mut secret_mounts = Vec::new();
                        for (n, (s, (_, value))) in secrets.iter().zip(&values).enumerate() {
                            use std::os::unix::fs::OpenOptionsExt;
                            let file = run.join(format!("secret-{n}"));
                            std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .mode(0o400)
                                .open(&file)
                                .and_then(|mut f| std::io::Write::write_all(&mut f, value))
                                .map_err(io(format!("writing the secret {}", s.id)))?;
                            secret_mounts.push((file, s.target.clone()));
                        }
                        let secret_files = secret_mounts.clone();
                        let redact = redactor(&values);
                        let progress = &mut *ctx.progress;
                        let mut log = |line: &str| {
                            progress(Progress::Log {
                                target: tname.clone(),
                                step: sname.clone(),
                                line: redact(line),
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
                            secrets: secret_mounts.clone(),
                            timeout,
                            run_dir: run.join("run"),
                            log: &mut log,
                        };
                        let skip = mount_points(&step_run);
                        let outcome = run_step(step_run);
                        // A secret lives on the host only while its step runs.
                        for (file, _) in &secret_files {
                            let _ = std::fs::remove_file(file);
                        }
                        let outcome = outcome?;
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
                                tail: outcome.tail.iter().map(|l| redact(l)).collect(),
                            });
                        }
                        let (d, size) =
                            seal_in_sandbox(ctx.sandbox, &run.join("upper"), &run.join("run"), ctx.store, &skip)?;
                        (d, size, Some(run.join("upper")))
                    };
                    let entry = Entry {
                        layer: Some(d.to_string()),
                        class: ctx.executor,
                        created: now(),
                        potter: env!("CARGO_PKG_VERSION").into(),
                    };
                    ctx.index.put(&key, &entry).map_err(cache_err)?;
                    if let Some(remote) = ctx.cache_to {
                        // A push failure never fails the build (spec §12).
                        match remote.put(ctx.store, &key, &parent, &entry) {
                            Ok(()) => (ctx.progress)(Progress::Step {
                                target: tname.clone(),
                                step: sname.clone(),
                                state: StepState::Pushed,
                            }),
                            Err(e) => (ctx.progress)(Progress::Warning(format!(
                                "step {sname}: pushing to the remote cache failed: {e}"
                            ))),
                        }
                    }
                    state.records.push(StepRecord {
                        chain: tname.clone(),
                        step: sname.clone(),
                        key: Some(key.hex()),
                        layer: d.clone(),
                        class: Some(ctx.executor),
                    });
                    state.layers.push(LayerRef {
                        descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), size),
                        diff_id: d,
                    });
                    lowers.push(upper);
                    state.executed += 1;
                    (ctx.progress)(Progress::Step {
                        target: tname.clone(),
                        step: sname.clone(),
                        state: StepState::Done {
                            secs: started.elapsed().as_secs_f64(),
                        },
                    });
                }
                Step::Apt { .. } => unreachable!("lowered to a run step"),
            }
        }
        Ok(())
    })();
    drop(vm_session);
    for run in runs {
        let _ = ctx.sandbox.remove(&run);
    }
    result.map(|()| state)
}

/// The layer of an `Inputs` or `CopyFrom` step, built on the host over `layers`, stored.
fn host_layer(ctx: &BuildCtx, step: &Step, layers: &[LayerRef], source_root: &Path) -> Result<LayerRef, BuildError> {
    use std::io::Seek;
    let sname = step.name();
    std::fs::create_dir_all(ctx.home.runs()).map_err(io("creating the runs directory"))?;
    let file = tempfile::tempfile_in(ctx.home.runs()).map_err(io("creating a layer file"))?;
    let mut file = match step {
        Step::Inputs {
            include,
            exclude,
            dest,
            owner: who,
            ..
        } => {
            let (uid, gid, _) = ids(who, ctx.store, layers, &format!("step {sname}"))?;
            let spec = InputsSpec {
                root: source_root,
                include,
                exclude,
                dest,
                owner: (uid, gid),
                epoch: epoch(),
            };
            build_inputs(&spec, file)?.0
        }
        Step::CopyFrom {
            from,
            paths,
            dest,
            owner: who,
            mode,
            ..
        } => {
            let potter_plan::CopySource::Target(source) = from else {
                unreachable!("check_steps refuses stages")
            };
            let source_layers = ctx.deps.get(source).ok_or_else(|| {
                BuildError::Plan(format!("step {sname} copies from {source}, which was not built first"))
            })?;
            let (uid, gid, _) = ids(who, ctx.store, layers, &format!("step {sname}"))?;
            let spill = ctx.home.runs();
            let spec = crate::merge::CopySpec {
                paths,
                dest,
                owner: Some((uid, gid)),
                mode: *mode,
                epoch: epoch(),
                spill_dir: &spill,
            };
            crate::merge::copy_layer(&openers(ctx.store, source_layers), &spec, file)?.0
        }
        _ => unreachable!("only inputs and copies are built on the host"),
    };
    file.rewind().map_err(io(format!("reading the layer of {sname}")))?;
    let (d, size) = ctx
        .store
        .put_reader(&mut file)
        .map_err(|e| BuildError::Infra(format!("storing the layer of {sname}: {e}")))?;
    Ok(LayerRef {
        descriptor: Descriptor::new(media::OCI_LAYER_TAR, d.clone(), size),
        diff_id: d,
    })
}

/// Whether the host can build `step`'s layer before the steps ahead of it run: its owner is
/// numeric, so no earlier step's `/etc/passwd` can change it.
fn buildable_ahead(step: &Step) -> bool {
    match step {
        Step::Inputs { owner, .. } | Step::CopyFrom { owner, .. } => owner_is_numeric(owner),
        _ => false,
    }
}

fn owner_is_numeric(spec: &str) -> bool {
    spec.split(':').all(|p| p.parse::<u64>().is_ok() || p == "root")
}

/// The class in the key of a step the host builds (`Meta`, `Inputs`, `CopyFrom`). It is the
/// same for every executor, so an `ns` build can follow a `vm` chain through those steps; what
/// keeps `vm` and `ns` entries apart is the class in each `Run` step's key, which every later
/// key inherits through its parent (spec §8.4).
const HOST_CLASS: ExecutorClass = ExecutorClass::Ns;

fn input<'a>(
    step: &'a Step,
    inputs_digest: Option<&'a str>,
    env: &'a BTreeMap<String, String>,
    platform: &'a str,
    secrets: &'a [u8],
    class: ExecutorClass,
) -> StepInput<'a> {
    StepInput {
        step,
        inputs_digest,
        env,
        platform,
        class,
        secrets,
    }
}

/// Replaces each secret value in a log line with `***`: the whole value and, for values that
/// span lines or end in a newline, each of their lines (best effort, spec §5.4).
pub fn redactor(values: &[(String, Vec<u8>)]) -> impl Fn(&str) -> String + '_ {
    let mut needles: Vec<String> = Vec::new();
    for (_, v) in values {
        let v = String::from_utf8_lossy(v);
        needles.push(v.to_string());
        needles.extend(v.lines().map(|l| l.trim_end_matches('\r').to_string()));
    }
    needles.retain(|n| !n.trim().is_empty());
    // Longer values first, so a line inside a longer value does not break it up.
    needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
    needles.dedup();
    move |line: &str| {
        needles
            .iter()
            .fold(line.to_string(), |l, n| l.replace(n.as_str(), "***"))
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
                max_layers: None,
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
            executor: ExecutorClass::Ns,
            backend: None,
            secrets: &BTreeMap::new(),
            tenant_key: [0u8; 32],
            cache_from: None,
            cache_to: None,
            deps: BTreeMap::new(),
            progress: &mut progress,
        };
        let copy = Step::CopyFrom {
            name: "copy".into(),
            from: CopySource::Stage("build".into()),
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
        assert!(err.to_string().contains("stage"), "{err}");
    }

    #[test]
    fn redaction_covers_trailing_newlines_and_every_line_of_a_secret() {
        let values = vec![
            ("tok".to_string(), b"abc123\n".to_vec()),
            (
                "pem".to_string(),
                b"-----BEGIN KEY-----\r\nMIIsecret\n-----END KEY-----\n".to_vec(),
            ),
        ];
        let r = redactor(&values);
        assert_eq!(r("token=abc123"), "token=***");
        assert_eq!(r("MIIsecret"), "***");
        assert_eq!(r("-----BEGIN KEY-----"), "***");
        assert_eq!(r("nothing here"), "nothing here");
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
            from: CopySource::Stage("build".into()),
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
            (target(oci(), vec![copy]), "stage"),
            (services, "services"),
        ];
        for (t, named) in cases {
            let err = check_supported(&t).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{err}");
            assert!(err.to_string().contains(named), "{err}");
        }
    }
}

/// Keys as the chain computes them, for tests that seed the index.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// The key of `t`'s first step, a `Run` straight on its OCI base, under `class`.
    pub fn run_key(store: &Store, t: &Target, class: ExecutorClass) -> potter_cache::CacheKey {
        let Base::Oci { reference, .. } = &t.build.base else {
            panic!("an OCI base")
        };
        let base = resolve_base(store, reference, None, &Platform::host(), DockerConfig::anonymous()).unwrap();
        let step = crate::stopgap::lower_step(&t.build.steps[0]);
        let Step::Run { env, .. } = &step else {
            panic!("a run step")
        };
        let mut e = base.env();
        e.extend(env.clone());
        let secrets = potter_cache::secrets_digest(&[7u8; 32], &[]);
        let platform = Platform::host().to_string();
        step_key(
            &base_key(&base.key_digest()),
            &input(&step, None, &e, &platform, &secrets, class),
        )
        .unwrap()
    }
}
