//! `potter rebase` (spec §16) through kiln's real conversion and rebase: the run base swapped
//! under images potter assembled, with no namespaces needed.

mod common;

use common::{base_tars, image, kiln_layers, layer, oci_base, run_base, sources, steps};
use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_store::Store;
use potter_exec::BuildError;
use potter_exec::rebase::{new_chain, rebase_onto, recorded};
use potter_plan::{Base, CatalogRef, Chain};

fn store() -> (tempfile::TempDir, Store) {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    (home, store)
}

/// A base layer that changes `etc/os-release` to `text`.
fn os_release(text: &str) -> Vec<u8> {
    TarBuilder::new()
        .file("etc/os-release", text.as_bytes(), &Opts::default())
        .finish()
}

#[test]
fn a_patched_base_replaces_the_run_base_and_keeps_the_app_layers() {
    let (_home, store) = store();
    let b = base_tars(3, "v1");
    let base = oci_base(&store, &b, &["PATH=/bin", "LANG=C"]);
    let rb = run_base(&store, "debian:trixie", &base, &[layer("runtime0", "libpq")]);
    let app = steps(3, "a");
    let (digest, _) = image(&store, &rb, &app, 10, "potter/app:latest");
    let before = kiln_layers(&store, &digest);

    let rec = recorded(&store, "potter/app:latest").unwrap();
    assert_eq!(rec.app_layers, 3);
    assert_eq!(rec.max_layers, 10);

    // The same tag, patched: one base layer changes.
    let mut patched = b.clone();
    patched[2] = layer("base2", "v2");
    let new_base = oci_base(&store, &patched, &["PATH=/bin", "LANG=C"]);
    let new_rb = run_base(&store, "debian:trixie", &new_base, &[layer("runtime0", "libpq")]);
    let r = rebase_onto(&store, &rec, &new_rb, Some("potter/app:latest")).unwrap();
    assert!(!r.unchanged);
    assert_ne!(r.digest, digest);
    assert_eq!(r.old_base, rec.base);
    assert_eq!((r.reused + r.changed, r.tag.as_deref()), (3, Some("potter/app:latest")));
    assert_eq!(store.get_ref("potter/app:latest").unwrap(), Some(r.digest.clone()));

    let after = kiln_layers(&store, &r.digest);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[2].1, sources(&patched[2..3]), "the patched base layer");
    assert_ne!(after[2].0, before[2].0);
    assert_eq!(after[..2], before[..2], "the unchanged base layers");
    assert_eq!(after[3], before[3], "the runtime step layer is the same blob");
    // Each app layer is its own source, rebuilt or reused.
    for (a, s) in after[4..].iter().zip(sources(&app)) {
        assert_eq!(a.1, [s]);
    }

    // The rebased image records its new run base, so it can be rebased again.
    let again = recorded(&store, "potter/app:latest").unwrap();
    assert_eq!(again.base, r.new_base);
    let r2 = rebase_onto(&store, &again, &new_rb, None).unwrap();
    assert!(r2.unchanged, "already on that base");
    assert_eq!(r2.digest, r.digest);
}

#[test]
fn an_image_over_its_budget_stays_within_it_on_a_bigger_base() {
    let (_home, store) = store();
    let base = oci_base(&store, &base_tars(5, "v1"), &["PATH=/bin"]);
    let rb = run_base(&store, "ruby:3.4-slim", &base, &[]);
    let (_, report) = image(&store, &rb, &steps(6, "a"), 10, "potter/app:latest");
    assert_eq!((report.composite, report.kiln), (5, 7));

    // Two more base layers: still one composite under the same budget.
    let bigger = oci_base(&store, &base_tars(7, "v2"), &["PATH=/bin"]);
    let new_rb = run_base(&store, "ruby:3.4-slim", &bigger, &[]);
    let rec = recorded(&store, "potter/app:latest").unwrap();
    let r = rebase_onto(&store, &rec, &new_rb, Some("potter/app:2")).unwrap();
    let after = kiln_layers(&store, &r.digest);
    assert_eq!(after.len(), 7);
    assert_eq!(after[0].1.len(), 7, "the new base is one composite");
}

#[test]
fn a_new_distro_release_is_refused_with_exit_1() {
    let (_home, store) = store();
    let b = base_tars(2, "v1");
    let base = oci_base(&store, &b, &["PATH=/bin"]);
    let rb = run_base(&store, "debian:trixie", &base, &[]);
    image(&store, &rb, &steps(2, "a"), 10, "potter/app:latest");

    let mut next = b.clone();
    next.push(os_release("ID=debian\nVERSION_ID=\"14\"\n"));
    let new_rb = run_base(&store, "debian:trixie", &oci_base(&store, &next, &["PATH=/bin"]), &[]);
    let rec = recorded(&store, "potter/app:latest").unwrap();
    let err = rebase_onto(&store, &rec, &new_rb, Some("potter/app:latest")).unwrap_err();
    assert!(matches!(err, BuildError::RebaseRefused(_)), "{err}");
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("VERSION_ID"), "{err}");
    assert_eq!(
        store.get_ref("potter/app:latest").unwrap(),
        Some(rec.digest),
        "a refusal tags nothing"
    );
}

#[test]
fn a_base_file_the_app_overwrites_and_the_patch_changes_is_refused() {
    let (_home, store) = store();
    let b = base_tars(2, "v1");
    let base = oci_base(&store, &b, &["PATH=/bin"]);
    let rb = run_base(&store, "debian:trixie", &base, &[]);
    // The app rewrites a base file (a `sed -i` over a config).
    let edit = TarBuilder::new().file("base1/f", b"edited", &Opts::default()).finish();
    image(&store, &rb, &[edit], 10, "potter/app:latest");

    let mut patched = b.clone();
    patched[1] = layer("base1", "v2");
    let new_rb = run_base(
        &store,
        "debian:trixie",
        &oci_base(&store, &patched, &["PATH=/bin"]),
        &[],
    );
    let rec = recorded(&store, "potter/app:latest").unwrap();
    let err = rebase_onto(&store, &rec, &new_rb, None).unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}");
    assert!(err.to_string().contains("base1/f"), "{err}");
}

#[test]
fn a_base_that_drops_an_env_var_is_refused() {
    let (_home, store) = store();
    let b = base_tars(2, "v1");
    let rb = run_base(
        &store,
        "x:1",
        &oci_base(&store, &b, &["PATH=/bin", "GEM_HOME=/gems"]),
        &[],
    );
    image(&store, &rb, &steps(1, "a"), 10, "potter/app:latest");
    let new_rb = run_base(&store, "x:1", &oci_base(&store, &b, &["PATH=/bin"]), &[]);
    let rec = recorded(&store, "potter/app:latest").unwrap();
    let err = rebase_onto(&store, &rec, &new_rb, None).unwrap_err();
    assert_eq!(err.exit_code(), 1, "{err}");
    assert!(err.to_string().contains("GEM_HOME"), "{err}");
}

#[test]
fn an_image_with_no_run_base_record_is_a_plan_error() {
    let (_home, store) = store();
    let b = base_tars(2, "v1");
    let base = oci_base(&store, &b, &[]);
    // A plain kiln conversion records no base.
    let out = kiln_image::convert_resolved(
        &store,
        std::slice::from_ref(&base.resolved),
        Some("plain:1"),
        &kiln_image::ConvertOptions::default(),
        &kiln_image::StoredLayers,
    )
    .unwrap();
    store.set_ref("plain:1", &out.digest).unwrap();
    let err = recorded(&store, "plain:1").unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("records no run base"), "{err}");
    assert_eq!(recorded(&store, "missing:1").unwrap_err().exit_code(), 2);
}

#[test]
fn the_new_chain_resolves_the_tag_again_or_takes_another_base() {
    let pinned = Chain {
        base: Base::Oci {
            reference: "debian:trixie-slim".into(),
            digest: Some(format!("sha256:{}", "1".repeat(64))),
        },
        steps: vec![],
    };
    let again = new_chain(&pinned, None).unwrap();
    assert_eq!(
        again.base,
        Base::Oci {
            reference: "debian:trixie-slim".into(),
            digest: None
        },
        "the pin is dropped, so the tag finds its patched build"
    );
    let other = new_chain(&pinned, Some("registry.example/debian:trixie-patched")).unwrap();
    assert_eq!(
        other.base,
        Base::Oci {
            reference: "registry.example/debian:trixie-patched".into(),
            digest: None
        }
    );
    let catalog = Chain {
        base: Base::Catalog {
            layers: vec![CatalogRef::new("php", Some("8.4"))],
        },
        steps: vec![],
    };
    assert_eq!(new_chain(&catalog, None).unwrap(), catalog);
    assert_eq!(new_chain(&catalog, Some("x:1")).unwrap_err().exit_code(), 2);
}
