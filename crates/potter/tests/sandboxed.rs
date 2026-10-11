//! The engine's Linux parts, in vmkit's sandbox (spec §9.4).
#![cfg(target_os = "linux")]

mod support;

use std::os::unix::fs::{FileTypeExt, MetadataExt};

use kiln_erofs::testtar::{Opts, TarBuilder};
use std::time::Duration;

use support::{Ctx, xattr};

#[test]
fn unpacking_makes_overlay_whiteouts_and_owners_and_skips_devices() {
    let Some(ctx) = Ctx::new() else { return };
    let o = Opts::default();
    let mut t = TarBuilder::new();
    t.dir("etc", &Opts::default().mode(0o755))
        .file("etc/f", b"x", &Opts::default().uid(1000).gid(1000).mode(0o4755))
        .whiteout("etc/gone")
        .opaque("etc")
        .symlink("link", "/etc", &o)
        .file("deep/er/file", b"parents are made", &o)
        .chardev("dev/null", 1, 3, &o)
        .hardlink("etc/h", "etc/f");
    let dest = ctx.unpack(&t.finish()).unwrap();
    let gone = std::fs::symlink_metadata(dest.join("etc/gone")).unwrap();
    assert!(gone.file_type().is_char_device() && gone.rdev() == 0);
    assert_eq!(
        xattr(&dest.join("etc"), "user.overlay.opaque").as_deref(),
        Some(&b"y"[..])
    );
    let f = std::fs::metadata(dest.join("etc/f")).unwrap();
    assert_eq!(f.uid(), ctx.subuid_start + 999);
    assert_eq!(f.mode() & 0o7777, 0o4755, "set-id bits survive the chown");
    assert_eq!(std::fs::metadata(dest.join("etc/h")).unwrap().ino(), f.ino());
    assert_eq!(std::fs::read_link(dest.join("link")).unwrap().to_str(), Some("/etc"));
    assert_eq!(std::fs::read(dest.join("deep/er/file")).unwrap(), b"parents are made");
    assert!(
        std::fs::symlink_metadata(dest.join("dev/null")).is_err(),
        "devices are skipped"
    );
    // A second request reuses the directory.
    assert_eq!(ctx.unpack(&t.finish()).unwrap(), dest);
}

#[test]
fn hostile_layers_are_refused_and_leave_nothing_behind() {
    let Some(ctx) = Ctx::new() else { return };
    let o = Opts::default();
    let mut escape = TarBuilder::new();
    escape.file("../x", b"x", &o);
    let mut through_symlink = TarBuilder::new();
    through_symlink.symlink("l", "/tmp", &o).file("l/x", b"x", &o);
    let mut far_ids = TarBuilder::new();
    far_ids.file("f", b"x", &Opts::default().uid(200000));
    for (what, t) in [
        ("escape", escape),
        ("through a symlink", through_symlink),
        ("unmapped ids", far_ids),
    ] {
        let err = ctx.unpack(&t.finish()).unwrap_err();
        assert_eq!(err.exit_code(), 3, "{what}: {err}");
    }
    let left: Vec<_> = std::fs::read_dir(ctx.home.unpacked()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn a_step_runs_in_its_own_root_and_its_changes_seal_into_a_diff() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let ran = ctx.run(
        &base,
        "mkdir -p /app && echo built >/app/out && rm /etc/motd && echo log-line",
        "/",
        (0, 0),
        MINUTE,
    );
    assert!(ran.outcome.status.success(), "{:?}", ran.outcome.tail);
    assert!(ran.lines.iter().any(|l| l == "log-line"), "{:?}", ran.lines);
    let entries = ctx.seal(&ran).unwrap();
    assert!(entries.contains(&("app/out".into(), 'f')), "{entries:?}");
    assert!(entries.contains(&("etc/.wh.motd".into(), 'f')), "{entries:?}");
    assert!(
        !entries
            .iter()
            .any(|e| e.0.starts_with("proc/") || e.0.starts_with("dev/")),
        "{entries:?}"
    );
}

#[test]
fn the_workdir_is_created_and_the_user_applied() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let ran = ctx.run(&base, "pwd; id -u; echo $HOME", "/home/app/src", (1000, 1000), MINUTE);
    assert_eq!(
        ran.lines,
        ["/home/app/src", "1000", "/home/app"],
        "{:?}",
        ran.outcome.tail
    );
}

#[test]
fn undeclared_host_files_are_not_visible() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let script = format!("test -e {} && echo leak || echo absent", ctx.home.root().display());
    let ran = ctx.run(&base, &script, "/", (0, 0), MINUTE);
    assert_eq!(ran.lines, ["absent"], "{:?}", ran.outcome.tail);
}

#[test]
fn a_timeout_kills_the_step() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let ran = ctx.run(&base, "echo started; sleep 30", "/", (0, 0), Duration::from_secs(1));
    assert!(ran.outcome.timed_out && !ran.outcome.status.success());
    assert_eq!(ran.outcome.tail, ["started"]);
}

#[test]
fn seal_refuses_hostile_depth() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let deep = vec!["d"; 140].join("/");
    let ran = ctx.run(&base, &format!("mkdir -p /{deep}"), "/", (0, 0), MINUTE);
    assert!(ran.outcome.status.success(), "{:?}", ran.outcome.tail);
    assert_eq!(ctx.seal(&ran).unwrap_err().exit_code(), 3);
}

fn inputs(name: &str, include: &[&str], dest: &str) -> potter_plan::Step {
    potter_plan::Step::Inputs {
        name: name.into(),
        include: include.iter().map(|s| s.to_string()).collect(),
        exclude: vec![],
        dest: dest.into(),
        owner: "0:0".into(),
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

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (p, c) in files {
        let path = dir.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, c).unwrap();
    }
    dir
}

#[test]
fn a_chain_runs_then_hits_and_undeclared_changes_keep_every_key() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[("src/a.txt", "a"), ("notes.md", "n")]);
    let target = ctx.target(
        &base,
        vec![
            inputs("src", &["src/**"], "/app"),
            run("build", "cat /app/src/a.txt > /app/out && test ! -e /app/notes.md"),
        ],
    );
    let first = ctx.chain(&target, repo.path()).unwrap();
    assert_eq!((first.executed, first.cached), (1, 0));
    std::fs::write(repo.path().join("notes.md"), "changed").unwrap();
    let second = ctx.chain(&target, repo.path()).unwrap();
    assert_eq!((second.executed, second.cached), (0, 1));
    assert_eq!(first.layers, second.layers);
    std::fs::write(repo.path().join("src/a.txt"), "b").unwrap();
    assert_eq!(ctx.chain(&target, repo.path()).unwrap().executed, 1);
}

#[test]
fn a_failing_step_reports_its_tail_and_caches_nothing() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[]);
    let target = ctx.target(&base, vec![run("bad", "echo oops; exit 3")]);
    let err = ctx.chain(&target, repo.path()).unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}");
    assert!(
        matches!(&err, potter_exec::BuildError::StepFailed { tail, .. } if tail.iter().any(|l| l == "oops")),
        "{err:?}"
    );
    assert_eq!(ctx.index_entries(), 0);
}

#[test]
fn a_cache_mount_persists_and_stays_out_of_the_layer() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[]);
    let mut step = run("warm", "echo hit >> /cache/log; wc -l < /cache/log > /count");
    if let potter_plan::Step::Run { cache_mounts, .. } = &mut step {
        cache_mounts.push(potter_plan::CacheMount {
            id: "c".into(),
            target: "/cache".into(),
            sharing: Default::default(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    let state = ctx.chain(&target, repo.path()).unwrap();
    let paths = ctx.layer_paths(state.layers.last().unwrap());
    assert!(paths.iter().any(|p| p == "count"), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with("cache")), "{paths:?}");
    assert_eq!(
        std::fs::read_to_string(ctx.home.mounts(&potter_cache::Tenant::local()).join("c/log")).unwrap(),
        "hit\n"
    );
}

#[test]
fn a_target_assembles_into_a_kiln_image() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[("src/a.txt", "a")]);
    let mut target = ctx.target(
        &base,
        vec![
            inputs("src", &["src/**"], "/app"),
            run("build", "echo built > /app/out"),
        ],
    );
    target.image.export.push(potter_plan::Export {
        path: "/app".into(),
        dest: "/app".into(),
        exclude: vec![],
    });
    target.image.files.push(potter_plan::GeneratedFile {
        path: "/etc/app.conf".into(),
        content: "x=1\n".into(),
        mode: 0o640,
    });
    target.image.config = potter_plan::ImageConfig::new("/app", "1000:1000");
    target.image.config.env.insert("B".into(), "2".into());
    let mut web = potter_plan::Process::new(&["/app/run"]);
    web.env.insert("A".into(), "1".into());
    target.image.processes.insert("web".into(), web);
    let built = ctx.build(&target, repo.path(), &[]).unwrap();
    assert_eq!(built.reference, "potter/app:latest");
    let digest = kiln_image::resolve_name(&ctx.store, "potter/app:latest").unwrap();
    assert_eq!(digest, built.digest);
    let loaded = kiln_image::load(&ctx.store, &digest).unwrap();
    let (_, m) = &loaded.entries[0];
    let p = &m.config.process;
    assert_eq!(p.entrypoint, ["/app/run"]);
    for kv in ["A=1", "B=2", "PATH=/bin"] {
        assert!(p.env.contains(&kv.to_string()), "{:?}", p.env);
    }
    assert_eq!(p.working_dir.as_deref(), Some("/app"));
    assert_eq!(p.user.as_deref(), Some("1000:1000"));
    assert_eq!(m.manifest.layers.len(), 3, "the base, the export and the files");
}

#[test]
fn hardlinks_seal_target_first_with_the_inodes_metadata() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    // `foo-bar` sorts before `foo/x`, though the walk meets `foo/x` first.
    let script = "mkdir -p /w/foo && echo x > /w/foo/x && chmod 755 /w/foo/x && chown 1000:1000 /w/foo/x && busybox ln /w/foo/x /w/foo-bar";
    let ran = ctx.run(&base, script, "/", (0, 0), MINUTE);
    assert!(ran.outcome.status.success(), "{:?}", ran.outcome.tail);
    let (digest, _) = potter_exec::seal::seal_in_sandbox(&ctx.sandbox, &ran.upper, &ran.run, &ctx.store, &[]).unwrap();
    let mut a = tar::Archive::new(ctx.store.open_blob(&digest).unwrap());
    let mut seen: Vec<String> = Vec::new();
    for e in a.entries().unwrap() {
        let e = e.unwrap();
        let path = e.path().unwrap().display().to_string();
        if e.header().entry_type() == tar::EntryType::Link {
            let target = e.link_name().unwrap().unwrap().display().to_string();
            assert!(seen.contains(&target), "{path} -> {target} before its target");
            assert_eq!(
                (e.header().mode().unwrap() & 0o7777, e.header().uid().unwrap()),
                (0o755, 1000)
            );
        }
        seen.push(path);
    }
    assert!(
        seen.contains(&"w/foo-bar".to_string()) && seen.contains(&"w/foo/x".to_string()),
        "{seen:?}"
    );
}

#[test]
fn a_file_name_a_layer_cannot_hold_fails_the_step_not_the_sandbox() {
    let Some(ctx) = Ctx::new() else { return };
    let base = ctx.busybox_lower();
    let ran = ctx.run(&base, "touch \"/$(printf '\\377')\"", "/", (0, 0), MINUTE);
    assert!(ran.outcome.status.success(), "{:?}", ran.outcome.tail);
    let err = ctx.seal(&ran).unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}");
}

#[test]
fn a_base_layers_own_overlay_xattrs_are_dropped() {
    let Some(ctx) = Ctx::new() else { return };
    let dir = Opts::default().mode(0o755);
    let mut t = TarBuilder::new();
    t.dir(
        "d",
        &dir.clone().xattr("user.overlay.opaque", b"y").xattr("user.keep", b"1"),
    )
    .file("d/f", b"x", &Opts::default().mode(0o644));
    let dest = ctx.unpack(&t.finish()).unwrap();
    assert_eq!(xattr(&dest.join("d"), "user.overlay.opaque"), None);
    assert_eq!(xattr(&dest.join("d"), "user.keep").as_deref(), Some(&b"1"[..]));
}

#[test]
fn a_busy_cache_mount_gives_the_step_a_scratch_one() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[]);
    let mounts = ctx.home.mounts(&potter_cache::Tenant::local());
    std::fs::create_dir_all(mounts.join("c")).unwrap();
    // Another build holds the mount.
    let lock = std::fs::File::create(mounts.join("c.lock")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let mut step = run("warm", "echo hit >> /cache/log; cat /cache/log > /seen");
    if let potter_plan::Step::Run { cache_mounts, .. } = &mut step {
        cache_mounts.push(potter_plan::CacheMount {
            id: "c".into(),
            target: "/cache".into(),
            sharing: Default::default(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    ctx.chain(&target, repo.path()).unwrap();
    assert!(
        !mounts.join("c/log").exists(),
        "the step wrote into a mount another build holds"
    );
    drop(lock);
}

#[test]
fn stale_run_directories_are_swept_when_no_build_runs() {
    let Some(ctx) = Ctx::new() else { return };
    let stale = ctx.home.runs().join("step-stale");
    std::fs::create_dir_all(&stale).unwrap();
    // A file owned by a subordinate id: the host user cannot delete it.
    let out = ctx
        .sandbox
        .run_host(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                format!(
                    "mkdir {0}/d && touch {0}/d/f && chown -R 1000:1000 {0}/d",
                    stale.display()
                ),
            ],
            std::slice::from_ref(&stale),
        )
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let _lock = potter_exec::sandboxed::begin_build(&ctx.sandbox, &ctx.home).unwrap();
    assert!(!stale.exists(), "the stale run directory survived");
}

#[test]
fn a_secret_reaches_its_step_only_and_never_a_layer_a_log_or_the_index() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[]);
    let mut step = run("use", "cat /run/secrets/tok; echo; wc -c < /run/secrets/tok > /len");
    if let potter_plan::Step::Run { secrets, .. } = &mut step {
        secrets.push(potter_plan::SecretRef {
            id: "tok".into(),
            target: "/run/secrets/tok".into(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    let (state, lines) = ctx.chain_with_secrets(&target, repo.path(), &[("tok", b"s3cr3t-value")]);
    let state = state.unwrap();
    assert!(lines.iter().any(|l| l.contains("***")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("s3cr3t-value")), "{lines:?}");
    let layer = state.layers.last().unwrap();
    let paths = ctx.layer_paths(layer);
    assert!(paths.iter().any(|p| p == "len"), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with("run/secrets")), "{paths:?}");
    let blob = std::fs::read(ctx.store.blob_path(&layer.descriptor.digest)).unwrap();
    assert!(!blob.windows(12).any(|w| w == b"s3cr3t-value"));
    for e in std::fs::read_dir(ctx.home.root().join("cache/local/index")).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        assert!(!bytes.windows(12).any(|w| w == b"s3cr3t-value"));
    }
    // A new value is a new key; a missing secret is a plan error.
    let (again, _) = ctx.chain_with_secrets(&target, repo.path(), &[("tok", b"other-value!")]);
    assert_eq!(again.unwrap().executed, 1);
    let (missing, _) = ctx.chain_with_secrets(&target, repo.path(), &[]);
    assert_eq!(missing.unwrap_err().exit_code(), 2);
}

#[test]
fn a_secret_file_ending_in_a_newline_is_redacted() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[]);
    let mut step = run("use", "cat /run/secrets/tok");
    if let potter_plan::Step::Run { secrets, .. } = &mut step {
        secrets.push(potter_plan::SecretRef {
            id: "tok".into(),
            target: "/run/secrets/tok".into(),
        });
    }
    let target = ctx.target(&base, vec![step]);
    let (state, lines) = ctx.chain_with_secrets(&target, repo.path(), &[("tok", b"line-one\nline-two\n")]);
    state.unwrap();
    assert!(
        !lines.iter().any(|l| l.contains("line-one") || l.contains("line-two")),
        "{lines:?}"
    );
}

#[test]
fn a_build_records_each_layer_with_its_key_and_executor_and_the_bases_it_used() {
    let Some(ctx) = Ctx::new() else { return };
    let (_reg, _dir, base) = support::serve_busybox();
    let repo = repo(&[("src/a.txt", "a")]);
    let target = ctx.target(
        &base,
        vec![
            inputs("src", &["src/**"], "/app"),
            run("build", "echo built > /app/out"),
        ],
    );
    let first = ctx.build(&target, repo.path(), &[]).unwrap();
    let steps: Vec<(&str, Option<potter_cache::ExecutorClass>)> =
        first.steps.iter().map(|s| (s.step.as_str(), s.class)).collect();
    assert_eq!(steps, [("src", None), ("build", Some(potter_cache::ExecutorClass::Ns))]);
    assert!(
        first
            .steps
            .iter()
            .all(|s| s.chain == "app" && s.key.as_ref().is_some_and(|k| k.len() == 64))
    );
    assert_eq!(first.bases.len(), 1);
    assert_eq!(first.bases[0].reference, base);
    // A cache hit keeps the class of the entry it came from.
    let again = ctx.build(&target, repo.path(), &[]).unwrap();
    assert_eq!(again.steps, first.steps);
}
