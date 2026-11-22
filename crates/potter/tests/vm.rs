//! VM builds end to end (spec §9.3): kiln build-mode microVMs on KVM. Skipped without
//! `POTTER_TEST_KERNEL` and `POTTER_TEST_KILN_INIT` (and the sandbox helper); `scripts/vm-tests.sh`
//! builds kiln-init from the pinned kiln and runs them. `POTTER_TEST_VMM` picks the VMM
//! (default: Firecracker), `POTTER_TEST_VM_MEMORY_MIB` the memory of the chain tests' VMs, and
//! `POTTER_TEST_NETWORK=1` adds the egress test.
#![cfg(target_os = "linux")]

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use potter_exec::BuildError;
use potter_exec::vm::backend::{BuildBackend, BuildSpec, BuildVm};
use potter_exec::vm::kiln_backend::KilnBackend;
use potter_exec::vm::kiln_map::VmConfig;
use support::{Counting, Ctx, VmEnv};

fn vmm() -> kiln_run::VmmKind {
    std::env::var("POTTER_TEST_VMM")
        .ok()
        .map_or(kiln_run::VmmKind::Firecracker, |v| v.parse().unwrap())
}

/// The sandbox, the kernel and kiln-init, or `None` (the test skips).
fn setup() -> Option<(Ctx, &'static VmEnv)> {
    let env = support::vm_env()?;
    Some((Ctx::new()?, env))
}

/// A `KilnBackend` whose VMs get `POTTER_TEST_VM_MEMORY_MIB` instead of potter's 4096 MiB when
/// it is set: under nested virtualization (the Lima VM on Apple silicon) a guest's boot time grows
/// with its memory (the boot timeout, `kiln_map::boot_timeout`, scales with it).
struct TestBackend {
    inner: KilnBackend,
    memory_mib: Option<u32>,
}

impl BuildBackend for TestBackend {
    fn max_devices(&self) -> usize {
        self.inner.max_devices()
    }

    fn start(&self, mut spec: BuildSpec) -> Result<Box<dyn BuildVm>, BuildError> {
        if let Some(m) = self.memory_mib {
            spec.memory_mib = m;
        }
        self.inner.start(spec)
    }
}

fn backend(ctx: &Ctx, env: &VmEnv) -> TestBackend {
    let cfg = VmConfig::new(vmm(), env.kernel.clone(), env.init.clone());
    TestBackend {
        inner: KilnBackend::new(&ctx.store, cfg).unwrap(),
        memory_mib: std::env::var("POTTER_TEST_VM_MEMORY_MIB")
            .ok()
            .map(|v| v.parse().unwrap()),
    }
}

fn run(name: &str, script: &str) -> potter_plan::Step {
    potter_plan::Step::Run {
        name: name.into(),
        command: potter_plan::Command::shell(script),
        env: Default::default(),
        workdir: "/".into(),
        user: "0:0".into(),
        network: false,
        cache_mounts: vec![],
        secrets: vec![],
        timeout_secs: None,
    }
}

fn with_network(mut step: potter_plan::Step, on: bool) -> potter_plan::Step {
    if let potter_plan::Step::Run { network, .. } = &mut step {
        *network = on;
    }
    step
}

fn repo() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// `potter build --executor vm` on `repo`, with `extra` flags.
fn build(ctx: &Ctx, env: &VmEnv, repo: &Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::cargo_bin("potter")
        .unwrap()
        .args(["build", "--executor", "vm", "--vmm", vmm().name()])
        .arg("--kernel")
        .arg(&env.kernel)
        .arg("--kiln-init")
        .arg(&env.init)
        .args(extra)
        .arg(repo)
        .env("KILN_HOME", ctx.store.root())
        .env("POTTER_HOME", ctx.home.root())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The index entries' JSON texts.
fn index_entries(ctx: &Ctx) -> Vec<String> {
    std::fs::read_dir(ctx.home.root().join("cache/local/index"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect()
}

/// A step layer's entries: (path, mtime).
fn layer_entries(ctx: &Ctx, layer: &potter_exec::chain::LayerRef) -> Vec<(String, u64)> {
    let blob = ctx.store.open_blob(&layer.descriptor.digest).unwrap();
    tar::Archive::new(blob)
        .entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.path().unwrap().display().to_string(), e.header().mtime().unwrap())
        })
        .collect()
}

/// Goes through the CLI, so its VMs boot with potter's real 4096 MiB (`POTTER_TEST_VM_MEMORY_MIB`
/// doesn't reach them): under nested virtualization that boot takes 50 to 125 s, inside the
/// 180 s `kiln_map::boot_timeout(4096)`.
#[test]
fn potter_build_in_a_vm_makes_an_image_and_then_hits_its_vm_entries() {
    let Some((ctx, env)) = setup() else { return };
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    std::fs::write(
        repo.path().join("potter.toml"),
        format!(
            r#"[target.app]
provider = "none"

[target.app.base]
oci = "{base}"

[target.app.steps.one]
run = "mkdir -p /app && echo one > /app/one"
network = false

[target.app.steps.two]
run = "cat /app/one > /app/two"
network = false

[target.app.process.web]
cmd = ["/bin/cat", "/app/two"]
"#
        ),
    )
    .unwrap();
    let (code, stdout, stderr) = build(&ctx, env, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("potter/app:latest sha256:"), "{stdout}");
    assert!(stdout.contains("(executed 2, cached 0)"), "{stdout}");
    assert!(stderr.contains("[app] one: running"), "{stderr}");
    assert!(stderr.contains("[app] two: running"), "{stderr}");
    let digest = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    assert_eq!(loaded.entries[0].1.config.process.entrypoint, ["/bin/cat", "/app/two"]);
    // Every entry the build wrote is vm-class (P3): an ns build never reads it.
    let entries = index_entries(&ctx);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries.iter().all(|e| e.contains(r#""class":"vm""#)), "{entries:?}");

    let (code, stdout, stderr) = build(&ctx, env, repo.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("(executed 0, cached 2)"), "{stdout}");
    assert!(stderr.contains("[app] one: cached"), "{stderr}");
    assert!(stderr.contains("[app] two: cached"), "{stderr}");
}

#[test]
fn two_vm_builds_seal_identical_layers() {
    let Some((ctx, env)) = setup() else { return };
    let backend = backend(&ctx, env);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let target = ctx.target(
        &base,
        vec![
            run(
                "tree",
                "mkdir -p /app/d/e && echo a > /app/d/f && touch /app/t && ln -s f /app/d/l",
            ),
            run("more", "echo b >> /app/d/f && rm /app/t && mkdir /root/x"),
        ],
    );
    let (first, lines) = ctx.vm_chain_with(&target, repo.path(), &backend, true, &[]);
    let first = first.unwrap_or_else(|e| panic!("{e}: {lines:?}"));
    let (second, lines) = ctx.vm_chain_with(&target, repo.path(), &backend, true, &[]);
    let second = second.unwrap_or_else(|e| panic!("{e}: {lines:?}"));
    assert_eq!((first.executed, second.executed), (2, 2));
    let digests = |s: &potter_exec::chain::ChainState| -> Vec<String> {
        s.layers.iter().map(|l| l.descriptor.digest.to_string()).collect()
    };
    assert_eq!(digests(&first), digests(&second));
    // Without SOURCE_DATE_EPOCH the clamp is 0 (D6): no entry carries the guest's clock.
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .map_or(0, |v| v.parse::<u64>().unwrap());
    let step_layers = &first.layers[first.layers.len() - 2..];
    for layer in step_layers {
        let entries = layer_entries(&ctx, layer);
        assert!(!entries.is_empty());
        for (path, mtime) in &entries {
            if std::env::var_os("SOURCE_DATE_EPOCH").is_none() {
                assert_eq!(*mtime, 0, "{path}");
            } else {
                assert!(*mtime <= epoch, "{path}: {mtime}");
            }
        }
    }
}

#[test]
fn a_cache_mount_persists_between_vm_builds() {
    let Some((ctx, env)) = setup() else { return };
    let backend = backend(&ctx, env);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let mut step = run(
        "count",
        "n=$(cat /root/.c/n 2>/dev/null || echo 0); echo $((n+1)) > /root/.c/n; echo count=$((n+1))",
    );
    if let potter_plan::Step::Run { cache_mounts, .. } = &mut step {
        // Spelled as Dockerfiles write it: cleaned before it reaches kiln (D10).
        cache_mounts.push(potter_plan::CacheMount {
            id: "c".into(),
            target: "/root//.c/".into(),
            sharing: Default::default(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    for want in ["count=1", "count=2"] {
        let (state, lines) = ctx.vm_chain_with(&target, repo.path(), &backend, true, &[]);
        let state = state.unwrap_or_else(|e| panic!("{e}: {lines:?}"));
        assert!(lines.iter().any(|l| l == want), "{want}: {lines:?}");
        let paths: Vec<String> = layer_entries(&ctx, state.layers.last().unwrap())
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(
            !paths.iter().any(|p| p.trim_end_matches('/') == "root/.c/n"),
            "{paths:?}"
        );
    }
}

/// The egress test reaches example.com (or `POTTER_TEST_EGRESS_HOST`): it is opt-in, as build.rs's
/// network tests are, and passes without running anything unless POTTER_TEST_NETWORK=1.
fn network() -> bool {
    std::env::var_os("POTTER_TEST_NETWORK").is_some_and(|v| v == "1")
}

/// Opt-in: skipped silently (it returns before booting anything) without POTTER_TEST_NETWORK=1.
#[test]
fn egress_opens_only_for_steps_that_ask_in_one_vm() {
    if !network() {
        return;
    }
    let Some((ctx, env)) = setup() else { return };
    // A host that serves plain HTTP; POTTER_TEST_EGRESS_HOST overrides it where example.com
    // doesn't resolve.
    let host = std::env::var("POTTER_TEST_EGRESS_HOST").unwrap_or_else(|_| "example.com".into());
    let inner = backend(&ctx, env);
    let backend = Counting::new(&inner);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let target = ctx.target(
        &base,
        vec![
            with_network(run("a", &format!("! nslookup {host}")), false),
            with_network(run("b", &format!("wget -q -O /dev/null http://{host}")), true),
            with_network(run("c", &format!("! wget -q -T 5 -O /dev/null http://{host}")), false),
        ],
    );
    let (state, lines) = ctx.vm_chain(&target, repo.path(), &backend);
    let state = state.unwrap_or_else(|e| panic!("{e}: {lines:?}"));
    assert_eq!(state.executed, 3);
    assert_eq!(backend.boots.get(), 1, "one VM for the whole chain (D1)");
}

#[test]
fn a_failing_step_and_a_setup_failure_report_their_tails() {
    let Some((ctx, env)) = setup() else { return };
    let backend = backend(&ctx, env);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let target = ctx.target(&base, vec![run("bad", "echo out; echo err >&2; exit 3")]);
    let (state, lines) = ctx.vm_chain(&target, repo.path(), &backend);
    let err = state.unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}: {lines:?}");
    match &err {
        BuildError::StepFailed { status, tail, .. } => {
            assert_eq!(status, "exit status: 3", "{err}");
            assert!(tail.iter().any(|l| l == "out"), "{tail:?}");
            assert!(tail.iter().any(|l| l == "err"), "{tail:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(ctx.index_entries(), 0);

    // A secret under a regular file: kiln-init can't mount it and ends the step with 125 (D15).
    let mut step = run("setup", "echo must-not-run");
    if let potter_plan::Step::Run { secrets, .. } = &mut step {
        secrets.push(potter_plan::SecretRef {
            id: "tok".into(),
            target: "/etc/passwd/x".into(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    let (state, lines) = ctx.vm_chain_with(&target, repo.path(), &backend, false, &[("tok", b"x")]);
    let err = state.unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}: {lines:?}");
    assert!(
        err.to_string()
            .contains("exit status: 125 (kiln-init could not set the step up)"),
        "{err}"
    );
    assert!(!lines.iter().any(|l| l == "must-not-run"), "{lines:?}");
}

#[test]
fn a_timed_out_step_fails_within_its_bound() {
    let Some((ctx, env)) = setup() else { return };
    let backend = backend(&ctx, env);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let mut step = run("slow", "sleep 600");
    if let potter_plan::Step::Run { timeout_secs, .. } = &mut step {
        *timeout_secs = Some(2);
    }
    let target = ctx.target(&base, vec![step]);
    let started = Instant::now();
    let (state, lines) = ctx.vm_chain(&target, repo.path(), &backend);
    let elapsed = started.elapsed();
    let err = state.unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}: {lines:?}");
    assert!(err.to_string().contains("timed out after 2s"), "{err}");
    // The timeout, kiln's STEP_GRACE and KILL_WAIT, and the boot.
    let memory = backend.memory_mib.unwrap_or(potter_exec::vm::MEMORY_MIB);
    let bound = Duration::from_secs(2 + 10 + 5) + potter_exec::vm::kiln_map::boot_timeout(memory);
    assert!(elapsed < bound, "{elapsed:?}");
}

#[test]
fn secrets_reach_the_step_and_never_the_layer() {
    let Some((ctx, env)) = setup() else { return };
    let backend = backend(&ctx, env);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let mut step = run(
        "use",
        r#"mkdir -p /app && test "$(cat /run/secrets/tok)" = kiln-secret-p4b && echo ok > /app/ok"#,
    );
    if let potter_plan::Step::Run { secrets, .. } = &mut step {
        secrets.push(potter_plan::SecretRef {
            id: "tok".into(),
            target: "/run/secrets/tok".into(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    let secret: &[u8] = b"kiln-secret-p4b";
    let (state, lines) = ctx.vm_chain_with(&target, repo.path(), &backend, false, &[("tok", secret)]);
    let state = state.unwrap_or_else(|e| panic!("{e}: {lines:?}"));
    let layer = state.layers.last().unwrap();
    let paths: Vec<String> = layer_entries(&ctx, layer).into_iter().map(|(p, _)| p).collect();
    assert!(paths.iter().any(|p| p == "app/ok"), "{paths:?}");
    // No stored step layer holds the secret's bytes.
    for layer in &state.layers[1..] {
        let blob = std::fs::read(ctx.store.blob_path(&layer.descriptor.digest)).unwrap();
        assert!(!blob.windows(secret.len()).any(|w| w == secret));
    }
}

#[test]
fn user_namespaces_are_refused_in_a_step() {
    let Some((ctx, env)) = setup() else { return };
    let backend = backend(&ctx, env);
    let (_reg, _dir, base) = support::serve_vm_busybox();
    let repo = repo();
    let target = ctx.target(&base, vec![run("userns", "unshare -U true")]);
    let (state, lines) = ctx.vm_chain(&target, repo.path(), &backend);
    let err = state.unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}: {lines:?}");
    match &err {
        // busybox's own error, not a missing applet (127).
        BuildError::StepFailed { status, tail, .. } => {
            assert!(!tail.is_empty(), "{err}");
            assert_ne!(status, "exit status: 127", "{tail:?}");
        }
        other => panic!("{other:?}"),
    }
}
