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
    let mut progress = |p: Progress| {
        if let Progress::Log { line, .. } = p {
            lines.push(line);
        }
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
    assert!(e.to_string().contains("M3e"), "{e}");
}

#[test]
fn cache_mounts_in_a_vm_build_are_refused_until_p4b() {
    let w = world();
    let mut step = run("a");
    if let Step::Run { cache_mounts, .. } = &mut step {
        cache_mounts.push(CacheMount {
            id: "npm".into(),
            target: "/root/.npm".into(),
            sharing: Default::default(),
        });
    }
    let fake = FakeBackend::new(31, vec![]);
    let e = vm(&w, &target(&w, vec![step]), &fake).unwrap_err();
    assert_eq!(e.exit_code(), 2, "{e}");
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
    let s = vm(&tight, &target(&tight, steps), &narrow).unwrap();
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
            stacked: 5,
            composite: 0,
            kiln: 5
        }
    );
    // Over a budget of 3, the shared assembly merges the base and the two lowest steps.
    assert_eq!(
        assemble(Some(3)),
        LayerReport {
            max_layers: 3,
            base: 1,
            stacked: 5,
            composite: 3,
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

#[test]
fn steps_with_and_without_network_never_share_a_vm() {
    let w = world();
    let net = |name: &str, on: bool| {
        let mut s = run(name);
        if let Step::Run { network, .. } = &mut s {
            *network = on;
        }
        s
    };
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
    let vms: Vec<bool> = f.specs.iter().map(|s| s.network).collect();
    assert_eq!(vms, [false, true, false]);
    let steps: Vec<bool> = f.steps.iter().map(|s| s.network).collect();
    assert_eq!(steps, [false, true, true, false]);
    assert_eq!(f.finished, 3);
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
