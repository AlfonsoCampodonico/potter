mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn root_main_package() {
    let d = detect("go-api");
    let t = &d.target;
    assert_eq!(t.provider, "go");
    assert_eq!(layers(&t.build.base), ["os-min", "build-tools", "go@1.27"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(includes(t, "deps-inputs"), ["go.mod"]);
    assert_eq!(command(t, "deps"), "go mod download");
    assert_eq!(
        command(t, "build"),
        "go build -trimpath -ldflags=\"-s -w\" -o /out/app ."
    );
    match step(t, "build") {
        potter_plan::Step::Run { env, .. } => {
            assert_eq!(env["CGO_ENABLED"], "0");
            assert_eq!(env["GOTOOLCHAIN"], "local");
        }
        _ => unreachable!(),
    }
    assert_eq!(layers(&t.image.base), ["os-min"]);
    assert_eq!(t.image.export[0].path, "/out");
    assert_eq!(process(t, "web"), "/out/app");
    assert!(decision(&d, "runtime.go").origin.to_string().contains("go.mod go 1.26"));
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn cmd_directories() {
    let t = detect("go-cmd").target;
    assert_eq!(includes(&t, "deps-inputs"), ["go.mod", "go.sum"]);
    assert_eq!(
        command(&t, "build"),
        "go build -trimpath -ldflags=\"-s -w\" -o /out/api ./cmd/api && go build -trimpath -ldflags=\"-s -w\" -o /out/worker ./cmd/worker"
    );
    assert_eq!(process(&t, "web"), "/out/api");
    assert_eq!(process(&t, "worker"), "/out/worker");
    assert_eq!(layers(&t.build.base), ["os-min", "build-tools", "go@1.27"]);
}

#[test]
fn ambiguous_mains_need_config() {
    let r = temp_repo(&[
        ("go.mod", "module x\n\ngo 1.26\n"),
        ("cmd/alpha/main.go", "package main\n"),
        ("cmd/beta/main.go", "package main\n"),
    ]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NeedsConfig { .. })
    ));
}

#[test]
fn library_only_module_has_no_process() {
    let r = temp_repo(&[("go.mod", "module x\n\ngo 1.26\n"), ("lib.go", "package lib\n")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NoProcess { .. })
    ));
}

#[test]
fn too_new_go_is_unsatisfiable() {
    let r = temp_repo(&[("go.mod", "module x\n\ngo 1.30\n"), ("main.go", "package main\n")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "runtime.go: go.mod go 1.30 (minimum) is not in the catalog; the catalog has go 1.27"
    );
}

#[test]
fn old_toolchain_falls_back_to_the_go_minimum() {
    let r = temp_repo(&[
        ("go.mod", "module x\n\ngo 1.24.0\n\ntoolchain go1.24.4\n"),
        ("main.go", "package main\n"),
    ]);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert_eq!(decision(&d, "runtime.go").value, "1.27");
    assert!(
        decision(&d, "runtime.go")
            .origin
            .to_string()
            .contains("toolchain go1.24.4 isn't in the catalog")
    );
}

#[test]
fn go_build_chain_has_build_tools() {
    let t = detect("go-api").target;
    assert_eq!(layers(&t.build.base), ["os-min", "build-tools", "go@1.27"]);
}

#[test]
fn run_base_has_no_build_tools() {
    for name in ["go-api", "go-cmd"] {
        let t = detect(name).target;
        assert!(!layers(&t.image.base).contains(&"build-tools".to_string()), "{name}");
    }
    for name in [
        "rust-axum",
        "bun",
        "deno-serve",
        "vite-static",
        "python-flask",
        "ruby-sinatra",
    ] {
        let t = detect(name).target;
        assert!(
            layers(&t.build.base).contains(&"build-tools".to_string()),
            "{name} build"
        );
        assert!(
            !layers(&t.image.base).contains(&"build-tools".to_string()),
            "{name} run"
        );
    }
}
