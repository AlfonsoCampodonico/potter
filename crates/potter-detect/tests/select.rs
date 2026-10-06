mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, ProviderId};

#[test]
fn no_provider_lists_manifest_dirs() {
    let d = temp_repo(&[
        ("frontend/package.json", "{}"),
        ("backend/go.mod", "module x\n"),
        ("README.md", "hi"),
    ]);
    let e = detect_at(d.path(), &DetectOptions::default()).unwrap_err();
    let msg = e.to_string();
    assert!(matches!(e, DetectError::NoProvider { .. }), "{msg}");
    assert!(msg.contains("backend/") && msg.contains("frontend/"), "{msg}");
    assert!(msg.contains("[target.backend] root = \"backend\""), "{msg}");
}

#[test]
fn empty_dir_has_no_hint() {
    let d = temp_repo(&[("README.md", "hi")]);
    let msg = detect_at(d.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert_eq!(msg, "no provider recognises the build root");
}

#[test]
fn forced_provider_must_apply() {
    let d = temp_repo(&[("README.md", "hi")]);
    let opts = DetectOptions {
        provider: Some(ProviderId::Laravel),
        ..Default::default()
    };
    assert!(matches!(
        detect_at(d.path(), &opts),
        Err(DetectError::ForcedNotApplicable(ProviderId::Laravel))
    ));
}
