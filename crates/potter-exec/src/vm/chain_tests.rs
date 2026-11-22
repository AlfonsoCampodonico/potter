//! The chain driving a (fake) build VM: sessions, trust ordering, ingest failures, the device
//! budget. No sandbox is involved, so these run everywhere.

use std::collections::BTreeMap;

use kiln_store::{Digest, Store};
use potter_cache::{ExecutorClass, Index, PotterHome, Tenant};
use potter_plan::*;

use super::backend::BuildBackend;
use super::backend::Secret;
use super::fake::{FakeBackend, Lie, Script, diff_tar};
use crate::assemble::{LayerReport, build_target};
use crate::chain::{BuildCtx, ChainState, Progress, run_chain};
use crate::error::BuildError;
use crate::sandboxed::SandboxCtx;
use crate::testing::registry_with_image;

struct World {
    _reg: kiln_registry::testregistry::TestRegistry,
    _layout: tempfile::TempDir,
    dir: tempfile::TempDir,
    store: Store,
    home: PotterHome,
    base: String,
}

fn world() -> World {
    let (reg, layout) = registry_with_image(
        "v1",
        &[
            (
                "etc/passwd",
                b"root:x:0:0::/root:/bin/sh\napp:x:1000:1000::/home/app:/bin/sh\n",
            ),
            ("etc/group", b"root:x:0:\napp:x:1000:\n"),
        ],
        &["PATH=/bin"],
        "/",
    );
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let home = PotterHome::open(dir.path().join("home")).unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("src/b.txt"), "b").unwrap();
    let base = format!("{}/library/base:v1", reg.host());
    World {
        _reg: reg,
        _layout: layout,
        dir,
        store,
        home,
        base,
    }
}

fn run(name: &str) -> Step {
    Step::Run {
        name: name.into(),
        command: Command::shell(&format!("make {name}")),
        env: BTreeMap::new(),
        workdir: "/app".into(),
        user: "0:0".into(),
        network: false,
        cache_mounts: vec![],
        secrets: vec![],
        timeout_secs: None,
    }
}

fn inputs(name: &str, file: &str) -> Step {
    Step::Inputs {
        name: name.into(),
        include: vec![file.into()],
        exclude: vec![],
        dest: "/src".into(),
        owner: "0:0".into(),
    }
}

fn target(w: &World, steps: Vec<Step>) -> Target {
    let base = Base::Oci {
        reference: w.base.clone(),
        digest: None,
    };
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

/// Runs `t` with `executor` (and `backend` for vm), collecting its log lines.
fn chain(
    w: &World,
    t: &Target,
    executor: ExecutorClass,
    backend: Option<&dyn BuildBackend>,
    secrets: &BTreeMap<String, Vec<u8>>,
) -> (Result<ChainState, BuildError>, Vec<String>) {
    with_ctx(w, executor, backend, secrets, |ctx| run_chain(ctx, t, w.dir.path()))
}

/// Calls `f` with a build context for `executor` (and `backend` for vm), collecting the log lines.
fn with_ctx<R>(
    w: &World,
    executor: ExecutorClass,
    backend: Option<&dyn BuildBackend>,
    secrets: &BTreeMap<String, Vec<u8>>,
    f: impl FnOnce(&mut BuildCtx) -> R,
) -> (R, Vec<String>) {
    let tenant = Tenant::local();
    let index: Index = w.home.index(&tenant).unwrap();
    let sandbox = SandboxCtx {
        potter: "/nonexistent/potter".into(),
        runs: w.home.runs(),
    };
    let mut lines = Vec::new();
    let mut progress = |p: Progress| match p {
        Progress::Log { line, .. } => lines.push(line),
        Progress::Warning(w) => lines.push(format!("warning: {w}")),
        _ => {}
    };
    let mut ctx = BuildCtx {
        store: &w.store,
        home: &w.home,
        tenant: &tenant,
        index: &index,
        sandbox: &sandbox,
        platform: kiln_oci::Platform::host(),
        no_cache: false,
        auth: kiln_registry::DockerConfig::anonymous(),
        executor,
        backend,
        secrets,
        tenant_key: [7u8; 32],
        cache_from: None,
        cache_to: None,
        deps: BTreeMap::new(),
        catalog: None,
        progress: &mut progress,
    };
    let r = f(&mut ctx);
    drop(ctx);
    (r, lines)
}

fn vm(w: &World, t: &Target, fake: &FakeBackend) -> Result<ChainState, BuildError> {
    chain(w, t, ExecutorClass::Vm, Some(fake), &BTreeMap::new()).0
}

fn diff(files: &[(&str, &str)]) -> Digest {
    let files: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
        .collect();
    Digest::of(&diff_tar(&files).0)
}

fn index_entries(w: &World) -> usize {
    std::fs::read_dir(w.home.root().join("cache/local/index")).map_or(0, |d| d.count())
}

/// What the VM tests leave behind in the runs directory: nothing.
fn runs_left(w: &World) -> usize {
    std::fs::read_dir(w.home.runs()).map_or(0, |d| d.count())
}

#[test]
fn a_chain_runs_in_one_vm_and_then_hits_its_vm_entries() {
    let w = world();
    let t = target(&w, vec![run("a"), run("b")]);
    let fake = FakeBackend::new(
        31,
        vec![Script::files(&[("app/a", "1")]), Script::files(&[("app/b", "2")])],
    );
    let s = vm(&w, &t, &fake).unwrap();
    assert_eq!((s.executed, s.cached), (2, 0));
    let layers: Vec<&Digest> = s.layers.iter().rev().take(2).map(|l| &l.diff_id).collect();
    assert_eq!(layers, [&diff(&[("app/b", "2")]), &diff(&[("app/a", "1")])]);
    assert!(s.records.iter().all(|r| r.class == Some(ExecutorClass::Vm)));
    {
        let f = fake.state();
        assert_eq!((f.specs.len(), f.steps.len(), f.finished), (1, 2, 1));
        assert_eq!(f.specs[0].layers.len(), 1, "the base's one layer");
        assert_eq!(f.steps[0].workdir, "/app");
        assert!(!f.specs[0].network);
    }
    assert_eq!(runs_left(&w), 0, "the output disk is deleted");
    let again = vm(&w, &t, &fake).unwrap();
    assert_eq!((again.executed, again.cached), (0, 2));
    assert_eq!(fake.state().specs.len(), 1, "no VM for an all-hit chain");
    assert_eq!(again.layers, s.layers);
}

#[test]
fn an_ns_build_uses_vm_entries_and_a_vm_build_never_uses_ns_ones() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    vm(&w, &t, &fake).unwrap();
    // An ns build falls back to the vm key (spec §8.4); with every step a hit, no sandbox runs.
    let (ns, _) = chain(&w, &t, ExecutorClass::Ns, None, &BTreeMap::new());
    let ns = ns.unwrap();
    assert_eq!((ns.executed, ns.cached), (0, 1));
    assert_eq!(ns.records[0].class, Some(ExecutorClass::Vm));
    // Seed an ns entry for a step a vm build then runs: it is never looked up (P3).
    let t2 = target(&w, vec![run("poisoned")]);
    let ns_key = crate::chain::tests_support::run_key(&w.store, &t2, ExecutorClass::Ns);
    let poison = w
        .store
        .put_bytes(&diff_tar(&[("app/p".into(), b"poison".to_vec())]).0)
        .unwrap();
    w.home
        .index(&Tenant::local())
        .unwrap()
        .put(
            &ns_key,
            &potter_cache::Entry {
                layer: Some(poison.to_string()),
                class: ExecutorClass::Ns,
                created: 0,
                potter: "test".into(),
            },
        )
        .unwrap();
    let fake2 = FakeBackend::new(31, vec![Script::files(&[("app/clean", "1")])]);
    let s = vm(&w, &t2, &fake2).unwrap();
    assert_eq!((s.executed, s.cached), (1, 0));
    assert_eq!(s.layers.last().unwrap().diff_id, diff(&[("app/clean", "1")]));
}

#[test]
fn a_lying_guest_fails_the_build_with_3_and_leaves_nothing() {
    for lie in [Lie::Sha, Lie::Entries, Lie::Overlap, Lie::Id] {
        let w = world();
        let t = target(&w, vec![run("a"), run("b")]);
        let fake = FakeBackend::new(
            31,
            vec![
                Script::files(&[("app/a", "1")]),
                Script::Diff {
                    files: vec![("app/b".into(), b"2".to_vec())],
                    lines: vec![],
                    lie,
                },
            ],
        );
        let before = index_entries(&w);
        let e = vm(&w, &t, &fake).unwrap_err();
        assert_eq!(e.exit_code(), 3, "{lie:?}: {e}");
        assert_eq!(
            index_entries(&w),
            before + 1,
            "{lie:?}: only the honest step is recorded"
        );
        assert_eq!(fake.state().finished, 1, "{lie:?}: the VM is torn down");
        assert_eq!(runs_left(&w), 0, "{lie:?}");
    }
}

#[test]
fn a_failing_or_hanging_step_reports_its_tail_and_tears_the_vm_down() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(
        31,
        vec![Script::Fail {
            code: 2,
            lines: vec!["compiling".into(), "boom".into()],
        }],
    );
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    match r.unwrap_err() {
        BuildError::StepFailed { step, status, tail } => {
            assert_eq!(step, "a");
            assert!(status.contains('2'), "{status}");
            assert_eq!(tail, ["compiling", "boom"]);
        }
        e => panic!("{e}"),
    }
    assert_eq!(lines, ["compiling", "boom"]);
    assert_eq!((fake.state().finished, runs_left(&w), index_entries(&w)), (1, 0, 0));
    let hang = FakeBackend::new(31, vec![Script::Hang]);
    let e = vm(&w, &t, &hang).unwrap_err();
    assert!(e.to_string().contains("timed out"), "{e}");
}

#[test]
fn secrets_travel_inline_and_are_redacted() {
    let w = world();
    let mut step = run("a");
    if let Step::Run { secrets, .. } = &mut step {
        secrets.push(SecretRef {
            id: "npm".into(),
            target: "/run/secrets/npm".into(),
        });
    }
    let t = target(&w, vec![step]);
    let fake = FakeBackend::new(
        31,
        vec![Script::Diff {
            files: vec![("app/a".into(), b"1".to_vec())],
            lines: vec!["token s3cret".into()],
            lie: Lie::None,
        }],
    );
    let secrets = BTreeMap::from([("npm".to_string(), b"s3cret".to_vec())]);
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &secrets);
    r.unwrap();
    assert_eq!(lines, ["token ***"]);
    assert_eq!(
        fake.state().steps[0].secrets,
        [("/run/secrets/npm".to_string(), Secret(b"s3cret".to_vec()))]
    );
}

#[test]
fn a_vm_build_without_a_backend_fails_with_4() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, None, &BTreeMap::new());
    let e = r.unwrap_err();
    assert_eq!(e.exit_code(), 4, "{e}");
    assert!(e.to_string().contains("--kernel"), "{e}");
}

#[test]
fn a_vm_step_gets_the_same_home_default_as_an_ns_step() {
    let w = world();
    let mut nobody = run("a");
    if let Step::Run { user, .. } = &mut nobody {
        *user = "1000:1000".into();
    }
    let t = target(&w, vec![run("root"), nobody]);
    let fake = FakeBackend::new(
        31,
        vec![Script::files(&[("app/r", "1")]), Script::files(&[("app/a", "1")])],
    );
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.steps[0].env.get("HOME").map(String::as_str), Some("/root"), "uid 0");
    assert_eq!(
        f.steps[1].env.get("HOME").map(String::as_str),
        Some("/"),
        "no passwd entry: as ns.rs"
    );
}

#[test]
fn a_full_device_budget_restarts_the_vm_and_builds_the_same_layers() {
    let steps = vec![
        run("a"),
        inputs("one", "src/a.txt"),
        inputs("two", "src/b.txt"),
        run("b"),
    ];
    let script = || vec![Script::files(&[("app/a", "1")]), Script::files(&[("app/b", "2")])];
    let roomy = world();
    let wide = FakeBackend::new(31, script());
    let expected = vm(&roomy, &target(&roomy, steps.clone()), &wide).unwrap();
    {
        let f = wide.state();
        assert_eq!(f.specs.len(), 1);
        assert_eq!(f.steps[1].lowers_added, [1, 2]);
    }
    // 6 devices, no NIC: 2 for layers. The base takes one and the first inputs layer the
    // other; the second finds no slot, so step b boots a new VM from the whole prefix.
    let tight = world();
    let narrow = FakeBackend::new(6, script());
    narrow.state().finish_fails = true;
    let (s, lines) = chain(
        &tight,
        &target(&tight, steps),
        ExecutorClass::Vm,
        Some(&narrow),
        &BTreeMap::new(),
    );
    let s = s.unwrap();
    let warnings = lines
        .iter()
        .filter(|l| l.starts_with("warning: the build VM did not shut down cleanly"))
        .count();
    assert_eq!(warnings, 2, "the replaced VM and the last one both warn: {lines:?}");
    let f = narrow.state();
    assert_eq!((f.specs.len(), f.finished), (2, 2));
    assert_eq!(f.specs[0].layers.len(), 2, "the base and the first inputs");
    assert_eq!(f.specs[1].layers.len(), 2, "four layers squashed into two disks");
    assert!(f.steps[1].lowers_added.is_empty());
    let ids = |s: &ChainState| s.layers.iter().map(|l| l.diff_id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&s), ids(&expected));
}

#[test]
fn a_vm_image_is_assembled_within_its_layer_budget_like_an_ns_one() {
    let steps = vec![run("a"), run("b"), run("c"), run("d")];
    let script = || {
        ["a", "b", "c", "d"]
            .iter()
            .map(|n| Script::files(&[(&format!("app/{n}"), "1")]))
            .collect::<Vec<_>>()
    };
    let assemble = |budget: Option<u32>| {
        let w = world();
        let mut t = target(&w, steps.clone());
        t.image.max_layers = budget;
        let fake = FakeBackend::new(31, script());
        let (built, _) = with_ctx(&w, ExecutorClass::Vm, Some(&fake), &BTreeMap::new(), |ctx| {
            build_target(ctx, &t, w.dir.path(), &[])
        });
        built.unwrap().layers
    };
    // The base's one layer and four steps: within the default budget of 10, nothing merges.
    assert_eq!(
        assemble(None),
        LayerReport {
            max_layers: 10,
            base: 1,
            run_base: 1,
            stacked: 5,
            composite: 0,
            app_composite: 0,
            kiln: 5
        }
    );
    // Over a budget of 3, the shared assembly keeps the one-layer base and merges the three
    // lowest steps above it.
    assert_eq!(
        assemble(Some(3)),
        LayerReport {
            max_layers: 3,
            base: 1,
            run_base: 1,
            stacked: 5,
            composite: 0,
            app_composite: 3,
            kiln: 3
        }
    );
}

#[test]
fn inputs_ahead_are_attached_at_boot_and_stacked_before_their_step() {
    let w = world();
    let t = target(&w, vec![run("a"), inputs("src", "src/a.txt"), run("b")]);
    let fake = FakeBackend::new(
        31,
        vec![Script::files(&[("app/a", "1")]), Script::files(&[("app/b", "2")])],
    );
    let s = vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.len(), 1);
    assert_eq!(
        (f.specs[0].root, f.specs[0].layers.len()),
        (1, 2),
        "the base, then the inputs"
    );
    assert!(f.steps[0].lowers_added.is_empty());
    assert_eq!(f.steps[1].lowers_added, [1]);
    let inputs_layer = &s.layers[s.layers.len() - 2];
    let disk = f.specs[0].layers[1].file_name().unwrap().to_string_lossy().into_owned();
    assert!(disk.ends_with(inputs_layer.diff_id.hex()), "{disk}");
}

#[test]
fn inputs_owned_by_a_name_wait_for_the_steps_before_them() {
    // An earlier step could add the user to /etc/passwd: the layer is built when reached,
    // and the chain continues in a new VM.
    let w = world();
    let mut named = inputs("src", "src/a.txt");
    if let Step::Inputs { owner, .. } = &mut named {
        *owner = "app".into();
    }
    let t = target(&w, vec![run("a"), named, run("b")]);
    let fake = FakeBackend::new(
        31,
        vec![Script::files(&[("app/a", "1")]), Script::files(&[("app/b", "2")])],
    );
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.len(), 2);
    assert_eq!(f.specs[0].layers.len(), 1, "nothing built ahead");
    assert_eq!((f.specs[1].root, f.specs[1].layers.len()), (3, 3));
}

fn net(name: &str, on: bool) -> Step {
    let mut s = run(name);
    if let Step::Run { network, .. } = &mut s {
        *network = on;
    }
    s
}

#[test]
fn steps_with_and_without_network_share_one_networked_vm() {
    let w = world();
    let t = target(
        &w,
        vec![net("a", false), net("b", true), net("c", true), net("d", false)],
    );
    let fake = FakeBackend::new(
        31,
        ["a", "b", "c", "d"]
            .iter()
            .map(|n| Script::files(&[(&format!("app/{n}"), "1")]))
            .collect(),
    );
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.iter().map(|s| s.network).collect::<Vec<_>>(), [true]);
    assert_eq!(
        f.steps.iter().map(|s| s.network).collect::<Vec<_>>(),
        [false, true, true, false]
    );
    assert_eq!(f.finished, 1);
}

#[test]
fn a_chain_without_network_steps_boots_without_a_nic() {
    let w = world();
    let t = target(&w, vec![net("a", false), net("b", false)]);
    let fake = FakeBackend::new(
        31,
        ["a", "b"]
            .iter()
            .map(|n| Script::files(&[(&format!("app/{n}"), "1")]))
            .collect(),
    );
    vm(&w, &t, &fake).unwrap();
    assert_eq!(
        fake.state().specs.iter().map(|s| s.network).collect::<Vec<_>>(),
        [false]
    );
}

#[test]
fn output_chunks_are_joined_into_lines_per_stream() {
    // The fake sends every line in two chunks; failures print on stderr.
    let w = world();
    let t = target(&w, vec![run("a"), run("b")]);
    let fake = FakeBackend::new(
        31,
        vec![
            Script::Diff {
                files: vec![("app/a".into(), b"1".to_vec())],
                lines: vec!["first line".into(), "second".into()],
                lie: Lie::None,
            },
            Script::Fail {
                code: 1,
                lines: vec!["an error".into()],
            },
        ],
    );
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    assert_eq!(r.unwrap_err().exit_code(), 1);
    assert_eq!(lines, ["first line", "second", "an error"]);
}

#[test]
fn a_chain_past_the_lowers_cap_restarts_from_its_prefix() {
    let w = world();
    let names: Vec<String> = (0..235).map(|i| format!("s{i}")).collect();
    let t = target(&w, names.iter().map(|n| run(n)).collect());
    let fake = FakeBackend::new(
        31,
        names
            .iter()
            .map(|n| Script::files(&[(&format!("app/{n}"), "1")]))
            .collect(),
    );
    let state = vm(&w, &t, &fake).unwrap();
    assert_eq!(state.executed, 235);
    let f = fake.state();
    assert_eq!(f.specs.len(), 2, "one restart");
    assert_eq!(f.specs[0].root, 1);
    assert!(
        f.specs[1].root <= 27,
        "the new VM boots from the squashed prefix: {}",
        f.specs[1].root
    );
    assert_eq!(f.steps.len(), 235, "kiln refused nothing");
    // The root is 1 lower, so exactly 230 steps fit under the cap: the 231st opens a new VM.
    assert_eq!(f.steps[229].id, 230, "the first VM ran 230 steps");
    assert_eq!(f.steps[230].id, 1, "the 231st step is the second VM's first");
    assert!(
        f.specs
            .iter()
            .all(|s| s.source_date_epoch == f.specs[0].source_date_epoch)
    );
    assert_eq!(f.finished, 2);
}

#[test]
fn every_vm_clamps_mtimes_to_source_date_epoch() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    vm(&w, &t, &fake).unwrap();
    assert_eq!(fake.state().specs[0].source_date_epoch, Some(crate::tarball::epoch()));
}

#[test]
fn a_failed_finish_after_a_good_chain_is_a_warning() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    fake.state().finish_fails = true;
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    r.unwrap();
    let warning = lines
        .iter()
        .find(|l| l.starts_with("warning: the build VM did not shut down cleanly"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert_eq!(warning.matches("did not shut down cleanly").count(), 1, "{warning}");
}

#[test]
fn a_vm_that_dies_mid_step_fails_with_4_and_a_violation_with_3() {
    for (why, code) in [
        ("the VM ended (Killed)", 4),
        ("guest protocol violation: Running out of order", 3),
    ] {
        let w = world();
        let t = target(&w, vec![run("a")]);
        let fake = FakeBackend::new(31, vec![Script::Dies { why: why.into() }]);
        let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
        assert_eq!(r.unwrap_err().exit_code(), code, "{why}");
        assert_eq!(index_entries(&w), 0);
        assert_eq!(runs_left(&w), 0);
    }
}

#[test]
fn a_setup_failure_exits_1_and_says_kiln_init_could_not_set_the_step_up() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let line = "kiln-init: set up the step's mounts: /run/secrets/x: not a directory";
    let fake = FakeBackend::new(31, vec![Script::SetupFail { line: line.into() }]);
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    let e = r.unwrap_err();
    assert_eq!(e.exit_code(), 1);
    assert!(
        e.to_string()
            .contains("exit status: 125 (kiln-init could not set the step up)"),
        "{e}"
    );
    assert!(lines.iter().any(|l| l == line));
}

#[test]
fn secrets_over_a_frame_fail_with_2_before_the_step_is_sent() {
    let w = world();
    let mut s = run("a");
    if let Step::Run { secrets, .. } = &mut s {
        secrets.push(SecretRef {
            id: "big".into(),
            target: "/run/secrets/big".into(),
        });
    }
    let t = target(&w, vec![s]);
    let fake = FakeBackend::new(31, vec![]);
    let secrets = BTreeMap::from([("big".to_string(), vec![b'x'; 50 << 10])]);
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &secrets);
    let e = r.unwrap_err();
    assert_eq!(e.exit_code(), 2, "{e}");
    assert!(e.to_string().contains("step a"), "{e}");
    assert!(fake.state().steps.is_empty());
}

#[test]
fn targets_are_cleaned_and_a_secret_target_ending_in_a_slash_is_refused() {
    assert_eq!(
        crate::vm::clean_target("t", "/run//secrets/./npm", true).unwrap(),
        "/run/secrets/npm"
    );
    assert_eq!(
        crate::vm::clean_target("t", "/root//.cache/", false).unwrap(),
        "/root/.cache"
    );
    for (raw, file) in [
        ("/run/secrets/x/", true),
        ("/", false),
        ("//", false),
        ("/a/../b", false),
    ] {
        assert_eq!(
            crate::vm::clean_target("t", raw, file).unwrap_err().exit_code(),
            2,
            "{raw}"
        );
    }
}

#[test]
fn a_125_without_a_kiln_init_line_is_a_plain_exit_status() {
    let w = world();
    let t = target(&w, vec![run("a")]);
    let fake = FakeBackend::new(
        31,
        vec![Script::Fail {
            code: 125,
            lines: vec!["make: *** [a] Error".into()],
        }],
    );
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    let e = r.unwrap_err();
    assert_eq!(e.exit_code(), 1);
    assert!(e.to_string().contains("exit status: 125"), "{e}");
    assert!(!e.to_string().contains("kiln-init could not"), "{e}");
}

#[test]
fn a_bad_secret_target_is_refused_before_any_vm_boots() {
    let w = world();
    let mut s = run("a");
    if let Step::Run { secrets, .. } = &mut s {
        secrets.push(SecretRef {
            id: "k".into(),
            target: "/run/secrets/k/".into(),
        });
    }
    let t = target(&w, vec![s]);
    let fake = FakeBackend::new(31, vec![]);
    let secrets = BTreeMap::from([("k".to_string(), b"v".to_vec())]);
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &secrets);
    assert_eq!(r.unwrap_err().exit_code(), 2);
    assert!(fake.state().specs.is_empty());
}

fn cached(name: &str, mounts: &[(&str, &str)]) -> Step {
    let mut s = run(name);
    if let Step::Run { cache_mounts, .. } = &mut s {
        *cache_mounts = mounts
            .iter()
            .map(|(id, target)| CacheMount {
                id: id.to_string(),
                target: target.to_string(),
                sharing: Sharing::Exclusive,
            })
            .collect();
    }
    s
}

fn scripts(names: &[&str]) -> Vec<Script> {
    names
        .iter()
        .map(|n| Script::files(&[(&format!("app/{n}"), "1")]))
        .collect()
}

#[test]
fn cache_mounts_attach_one_ext4_disk_per_id_and_steps_name_their_targets() {
    let w = world();
    let t = target(
        &w,
        vec![
            cached("a", &[("npm", "/root//.npm/")]),
            cached("b", &[("npm", "/root/.npm")]),
        ],
    );
    let fake = FakeBackend::new(31, scripts(&["a", "b"]));
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.len(), 1);
    let disk = w.home.vm_mounts(&Tenant::local()).join("npm.ext4");
    assert_eq!(f.specs[0].cache_disks, [(disk.clone(), "/root/.npm".to_string())]);
    assert_eq!(
        f.steps.iter().map(|s| s.mounts.clone()).collect::<Vec<_>>(),
        vec![vec!["/root/.npm".to_string()]; 2]
    );
    assert_eq!(
        std::fs::metadata(&disk).unwrap().len(),
        crate::vm::caches::CACHE_DISK_BYTES
    );
    assert_eq!(runs_left(&w), 0);
    assert!(
        !w.home.vm_mounts(&Tenant::local()).join("npm.dirty").exists(),
        "a clean run leaves no marker"
    );
}

#[test]
fn cache_ids_past_the_device_budget_split_the_chain_instead_of_failing() {
    // 4 fixed devices, a layer disk and two cache disks.
    let w = world();
    let t = target(
        &w,
        vec![
            cached("a", &[("c1", "/1")]),
            cached("b", &[("c2", "/2")]),
            cached("c", &[("c3", "/3")]),
        ],
    );
    let fake = FakeBackend::new(7, scripts(&["a", "b", "c"]));
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    let ids: Vec<Vec<String>> = f
        .specs
        .iter()
        .map(|s| s.cache_disks.iter().map(|(_, t)| t.clone()).collect())
        .collect();
    assert_eq!(ids, [vec!["/1".to_string(), "/2".to_string()], vec!["/3".to_string()]]);
}

#[test]
fn a_networked_step_whose_nic_would_crowd_out_the_layer_disk_starts_a_new_vm() {
    // Two cache disks fit beside 4 fixed devices and a layer disk; a NIC leaves room for one.
    let w = world();
    let mut b = cached("b", &[("c2", "/2")]);
    if let Step::Run { network, .. } = &mut b {
        *network = true;
    }
    let t = target(&w, vec![cached("a", &[("c1", "/1")]), b]);
    let fake = FakeBackend::new(7, scripts(&["a", "b"]));
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    assert_eq!(f.specs.iter().map(|s| s.network).collect::<Vec<_>>(), [false, true]);
}

#[test]
fn a_step_with_more_cache_disks_than_the_vmm_holds_fails_with_4() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("c1", "/1"), ("c2", "/2"), ("c3", "/3")])]);
    let fake = FakeBackend::new(7, vec![]);
    let e = vm(&w, &t, &fake).unwrap_err();
    assert_eq!(e.exit_code(), 4, "{e}");
    assert!(e.to_string().contains("step a mounts 3 cache disks"), "{e}");
    assert!(fake.state().specs.is_empty());
}

#[test]
fn one_id_at_two_targets_splits_the_chain_into_two_vms() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("c", "/x")]), cached("b", &[("c", "/y")])]);
    let fake = FakeBackend::new(31, scripts(&["a", "b"]));
    vm(&w, &t, &fake).unwrap();
    let f = fake.state();
    let targets: Vec<Vec<String>> = f
        .specs
        .iter()
        .map(|s| s.cache_disks.iter().map(|(_, t)| t.clone()).collect())
        .collect();
    assert_eq!(targets, [vec!["/x".to_string()], vec!["/y".to_string()]]);
    assert_eq!(f.finished, 2);
}

#[test]
fn two_ids_at_one_target_split_the_chain_and_a_step_that_maps_them_itself_is_refused() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("c", "/x")]), cached("b", &[("d", "/x")])]);
    let fake = FakeBackend::new(31, scripts(&["a", "b"]));
    vm(&w, &t, &fake).unwrap();
    assert_eq!(fake.state().specs.len(), 2);

    let w = world();
    let t = target(&w, vec![cached("a", &[("c", "/x"), ("c", "/y")])]);
    let fake = FakeBackend::new(31, vec![]);
    let e = vm(&w, &t, &fake).unwrap_err();
    assert_eq!(e.exit_code(), 2, "{e}");
    assert!(e.to_string().contains("step a"), "{e}");
    assert!(fake.state().specs.is_empty());
}

#[test]
fn a_cache_break_before_a_networked_step_boots_the_first_vm_without_a_nic() {
    let w = world();
    let mut b = cached("b", &[("c", "/y")]);
    if let Step::Run { network, .. } = &mut b {
        *network = true;
    }
    let t = target(&w, vec![cached("a", &[("c", "/x")]), b]);
    let fake = FakeBackend::new(31, scripts(&["a", "b"]));
    vm(&w, &t, &fake).unwrap();
    assert_eq!(
        fake.state().specs.iter().map(|s| s.network).collect::<Vec<_>>(),
        [false, true]
    );
}

#[test]
fn a_cache_disk_in_use_by_another_build_gives_an_empty_one_and_a_warning() {
    let w = world();
    let root = w.home.vm_mounts(&Tenant::local());
    std::fs::create_dir_all(&root).unwrap();
    let lock = std::fs::File::create(root.join("npm.lock")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let t = target(&w, vec![cached("a", &[("npm", "/root/.npm")])]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    r.unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cache mount npm is in use by another build")),
        "{lines:?}"
    );
    assert!(!fake.state().specs[0].cache_disks[0].0.starts_with(&root));
    assert!(!root.join("npm.ext4").exists());
    assert_eq!(runs_left(&w), 0, "the empty disk went with the VM's directory");
}

#[test]
fn a_vm_that_dies_or_times_out_discards_its_cache_disks_and_a_failed_step_keeps_them() {
    for (script, kept) in [
        (Script::Hang, false),
        (
            Script::Dies {
                why: "the VM ended (Killed)".into(),
            },
            false,
        ),
        (Script::Fail { code: 1, lines: vec![] }, true),
    ] {
        let w = world();
        let t = target(&w, vec![cached("a", &[("npm", "/root/.npm")])]);
        let fake = FakeBackend::new(31, vec![script]);
        let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
        assert!(r.is_err());
        assert_eq!(w.home.vm_mounts(&Tenant::local()).join("npm.ext4").exists(), kept);
        assert!(!w.home.vm_mounts(&Tenant::local()).join("npm.dirty").exists());
    }
}

#[test]
fn a_failed_shutdown_after_a_good_chain_discards_the_cache_disks() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("npm", "/root/.npm")])]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/a", "1")])]);
    fake.state().finish_fails = true;
    let (r, lines) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    r.unwrap();
    assert!(lines.iter().any(|l| l.starts_with("warning: ")), "{lines:?}");
    let root = w.home.vm_mounts(&Tenant::local());
    assert!(!root.join("npm.ext4").exists(), "an unsynced disk is not reused");
    assert!(!root.join("npm.dirty").exists());
    // A clean shutdown keeps it.
    let t = target(&w, vec![cached("b", &[("npm", "/root/.npm")])]);
    let fake = FakeBackend::new(31, vec![Script::files(&[("app/b", "1")])]);
    let (r, _) = chain(&w, &t, ExecutorClass::Vm, Some(&fake), &BTreeMap::new());
    r.unwrap();
    assert!(root.join("npm.ext4").exists());
}

/// A backend whose VM panics in its first step.
struct Panics;

struct PanicVm;

impl BuildBackend for Panics {
    fn max_devices(&self) -> usize {
        31
    }

    fn start(&self, _: super::backend::BuildSpec) -> Result<Box<dyn super::backend::BuildVm>, BuildError> {
        Ok(Box::new(PanicVm))
    }
}

impl super::backend::BuildVm for PanicVm {
    fn run_step(
        &mut self,
        _: super::backend::RunStep,
        _: &mut dyn FnMut(super::backend::Stream, &[u8]),
    ) -> Result<super::backend::StepOutcome, BuildError> {
        panic!("a bug mid-step");
    }

    fn finish(self: Box<Self>) -> Result<(), BuildError> {
        Ok(())
    }
}

#[test]
fn a_panic_mid_step_discards_the_cache_disks() {
    let w = world();
    let t = target(&w, vec![cached("a", &[("npm", "/root/.npm")])]);
    let root = w.home.vm_mounts(&Tenant::local());
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        chain(&w, &t, ExecutorClass::Vm, Some(&Panics), &BTreeMap::new())
    }));
    assert!(unwound.is_err());
    assert!(
        root.join("npm.lock").exists(),
        "the session got as far as the cache disks"
    );
    assert!(!root.join("npm.ext4").exists());
    assert!(!root.join("npm.dirty").exists());
}

#[test]
fn a_refused_step_keeps_the_cache_disks() {
    let w = world();
    let mut a = cached("a", &[("npm", "/root/.npm")]);
    if let Step::Run { env, .. } = &mut a {
        env.insert("A=B".into(), "x".into());
    }
    let fake = FakeBackend::new(31, vec![]);
    let e = vm(&w, &target(&w, vec![a]), &fake).unwrap_err();
    assert_eq!(e.exit_code(), 2, "{e}");
    assert!(w.home.vm_mounts(&Tenant::local()).join("npm.ext4").exists());
}

#[test]
fn a_bad_cache_mount_is_refused_before_any_vm_boots() {
    for (id, target) in [("..", "/x"), ("c", "/a/../b"), ("c", "/")] {
        let w = world();
        let t = target_of(&w, cached("a", &[(id, target)]));
        let fake = FakeBackend::new(31, vec![]);
        let e = vm(&w, &t, &fake).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{id} {target}: {e}");
        assert!(fake.state().specs.is_empty());
    }
}

fn target_of(w: &World, step: Step) -> Target {
    target(w, vec![step])
}
