mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn vite_static_site() {
    let d = detect("vite-static");
    let t = &d.target;
    assert_eq!(t.provider, "static");
    assert_eq!(layers(&t.build.base), ["os-min", "node@22"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(layers(&t.image.base), ["os-min", "static-server"]);
    assert_eq!(t.image.export[0].path, "/app/build");
    assert_eq!(t.image.export[0].dest, "/srv/www");
    assert_eq!(process(t, "web"), "static-server --root /srv/www --listen 0.0.0.0:8080");
    assert_eq!(decision(&d, "static.out_dir").value, "build");
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn default_out_dir_and_bun() {
    let r = temp_repo(&[
        (
            "package.json",
            r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"^6"}}"#,
        ),
        ("bun.lock", "{}"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(t.provider, "static");
    assert_eq!(command(&t, "deps"), "bun install --frozen-lockfile");
    assert_eq!(command(&t, "build"), "bun run build");
    assert_eq!(t.image.export[0].path, "/app/dist");
}

#[test]
fn a_start_script_means_node_not_static() {
    assert_eq!(detect("vue-node").target.provider, "node");
}

#[test]
fn laravel_still_wins_over_its_vite_assets() {
    assert_eq!(detect("laravel").target.provider, "laravel");
}

#[test]
fn escaping_out_dir_is_refused() {
    let r = temp_repo(&[
        (
            "package.json",
            r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"^6"}}"#,
        ),
        ("package-lock.json", "{}"),
        ("vite.config.js", "export default { build: { outDir: '../../etc' } }"),
    ]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NeedsConfig { .. })
    ));
}

#[test]
fn create_react_app() {
    let r = temp_repo(&[
        (
            "package.json",
            r#"{"scripts":{"build":"react-scripts build"},"dependencies":{"react-scripts":"5.0.1"}}"#,
        ),
        ("package-lock.json", "{}"),
    ]);
    assert_eq!(
        detect_at(r.path(), &DetectOptions::default())
            .unwrap()
            .target
            .image
            .export[0]
            .path,
        "/app/build"
    );
}
