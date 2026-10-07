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
