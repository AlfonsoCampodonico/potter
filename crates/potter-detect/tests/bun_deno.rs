mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn bun() {
    let d = detect("bun");
    let t = &d.target;
    assert_eq!(t.provider, "bun");
    assert_eq!(layers(&t.build.base), ["os-min", "bun@1"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(includes(t, "deps-inputs"), ["package.json", "bun.lock", "bunfig.toml"]);
    assert_eq!(command(t, "deps"), "bun install --frozen-lockfile");
    assert_eq!(command(t, "build"), "bun run build");
    assert_eq!(process(t, "web"), "bun run start");
    assert_eq!(layers(&t.image.base), ["os-min", "bun@1"]);
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn bun_with_a_stray_package_lock_still_uses_bun() {
    let r = temp_repo(&[
        ("package.json", r#"{"scripts":{"start":"bun index.ts"}}"#),
        ("bun.lock", "{}"),
        ("package-lock.json", "{}"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(command(&t, "deps"), "bun install --frozen-lockfile");
}

#[test]
fn bun_main_field_fallback() {
    let r = temp_repo(&[
        (
            "package.json",
            r#"{"module":"src/index.ts","dependencies":{"hono":"^4"}}"#,
        ),
        ("bun.lock", "{}"),
    ]);
    assert_eq!(
        process(&detect_at(r.path(), &DetectOptions::default()).unwrap().target, "web"),
        "bun run src/index.ts"
    );
}

#[test]
fn deno_serve() {
    let t = detect("deno-serve").target;
    assert_eq!(t.provider, "deno");
    assert_eq!(includes(&t, "deps-inputs"), ["deno.json"]);
    assert_eq!(command(&t, "deps"), "deno install");
    assert_eq!(process(&t, "web"), "deno serve --port 3000 main.ts");
    assert_eq!(t.image.config.env["DENO_DIR"], "/app/.deno");
    match step(&t, "deps") {
        potter_plan::Step::Run { env, cache_mounts, .. } => {
            assert_eq!(env["DENO_DIR"], "/app/.deno");
            assert!(cache_mounts.is_empty());
        }
        _ => unreachable!(),
    }
}

#[test]
fn deno_fresh_with_jsonc_and_lock() {
    let t = detect("deno-fresh").target;
    assert_eq!(includes(&t, "deps-inputs"), ["deno.jsonc", "deno.lock"]);
    assert_eq!(command(&t, "deps"), "deno install --frozen");
    assert_eq!(command(&t, "build"), "deno task build");
    assert_eq!(process(&t, "web"), "deno task start");
}

#[test]
fn deno_without_entry_has_no_process() {
    let r = temp_repo(&[("deno.json", "{}")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NoProcess { .. })
    ));
}

#[test]
fn deno_version_file() {
    let r = temp_repo(&[("deno.json", "{}"), ("main.ts", ""), (".dvmrc", "1.46\n")]);
    let msg = detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert!(msg.contains("no available deno version satisfies .dvmrc"), "{msg}");
}
