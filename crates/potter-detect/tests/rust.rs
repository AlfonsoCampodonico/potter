mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn axum_single_package() {
    let d = detect("rust-axum");
    let t = &d.target;
    assert_eq!(t.provider, "rust");
    assert_eq!(layers(&t.build.base), ["os-min", "rust@1.99"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(
        includes(t, "deps-inputs"),
        ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
    );
    assert_eq!(
        command(t, "deps"),
        "mkdir -p src && printf '%s\\n' 'fn main() {}' > src/main.rs && mkdir -p src && printf '%s\\n' '' > src/lib.rs && cargo build --release --locked"
    );
    assert_eq!(
        command(t, "build"),
        "cargo clean --release -p axum-hello-world && cargo build --release --locked && mkdir -p /out && cp target/release/axum-hello-world /out/"
    );
    assert_eq!(process(t, "web"), "/out/axum-hello-world");
    assert_eq!(layers(&t.image.base), ["os-min"]);
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn workspace_with_two_binaries() {
    let t = detect("rust-workspace").target;
    assert_eq!(
        includes(&t, "deps-inputs"),
        [
            "Cargo.toml",
            "Cargo.lock",
            "crates/api/Cargo.toml",
            "crates/core/Cargo.toml",
            "crates/worker/Cargo.toml"
        ]
    );
    let deps = command(&t, "deps");
    assert!(
        deps.contains("> crates/api/src/main.rs")
            && deps.contains("> crates/core/src/lib.rs")
            && deps.contains("> crates/core/build.rs"),
        "{deps}"
    );
    assert!(command(&t, "build").starts_with("cargo clean --release -p api -p core -p worker && "));
    assert_eq!(process(&t, "web"), "/out/api");
    assert_eq!(process(&t, "worker"), "/out/worker");
}

#[test]
fn toolchain_channel_and_rust_version() {
    let base = [
        ("Cargo.toml", "[package]\nname = \"x\"\nrust-version = \"1.98\"\n"),
        ("Cargo.lock", "version = 4\n"),
        ("src/main.rs", "fn main() {}\n"),
    ];
    let r = temp_repo(&base);
    assert!(
        decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.rust")
            .origin
            .to_string()
            .contains("rust-version")
    );
    let r = temp_repo(&[
        base[0],
        base[1],
        base[2],
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.97.0\"\n"),
    ]);
    assert_eq!(
        decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.rust").value,
        "1.97"
    );
    let r = temp_repo(&[base[0], base[1], base[2], ("rust-toolchain", "nightly-2026-09-01\n")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NeedsConfig { .. })
    ));
}

#[test]
fn hostile_bin_path_is_refused() {
    let r = temp_repo(&[
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\n[[bin]]\nname = \"x\"\npath = \"src/$(id).rs\"\n",
        ),
        ("Cargo.lock", "version = 4\n"),
    ]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::Parse { .. })
    ));
}

#[test]
fn missing_lock_and_library_only() {
    let r = temp_repo(&[("Cargo.toml", "[package]\nname = \"x\"\n"), ("src/lib.rs", "")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::MissingFile { .. })
    ));
    let r = temp_repo(&[
        ("Cargo.toml", "[package]\nname = \"x\"\n"),
        ("Cargo.lock", "version = 4\n"),
        ("src/lib.rs", ""),
    ]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NoProcess { .. })
    ));
}
