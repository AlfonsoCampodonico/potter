//! `potter rebase` (spec §16) through kiln's real conversion and rebase: the run base swapped
//! under images potter assembled, with no namespaces needed.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{base_tars, image, image_with, kiln_layers, layer, oci_base, run_base, sources, steps};
use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_store::Store;
use potter_catalog::{Arch, Entry, Index, Kind, Resolution, SCHEMA};
use potter_exec::BuildError;
use potter_exec::assemble::{
    ANN_CATALOG_CONSTRAINTS, ANN_CATALOG_INDEX, ANN_CATALOG_LAYERS, ANN_CHAIN, ANN_SONAMES_NEEDED,
};
use potter_exec::rebase::{
    RecordedCatalog, check_catalog, kept_sonames, new_chain, rebase_chain, rebase_onto, recorded,
};
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
    assert!(matches!(err, BuildError::RebaseRefused { .. }), "{err}");
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

fn d(n: u8) -> String {
    format!("sha256:{}", format!("{n:02x}").repeat(32))
}

/// A catalog entry `name@line` at digest `d(n)`, on `os`, providing `sonames`.
fn entry(n: u8, name: &str, line: &str, os: &str, sonames: &[&str]) -> Entry {
    Entry {
        name: name.into(),
        line: line.into(),
        version: format!("{line}.{n}"),
        arch: Arch::X86_64,
        kind: if name == "os-min" { Kind::Base } else { Kind::Runtime },
        digest: d(n),
        diff_id: d(n),
        size: 1,
        media_type: "application/vnd.oci.image.layer.v1.tar".into(),
        abi: [("os", os), ("libc", "glibc-2.41")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        requires: vec![],
        conflicts: vec![],
        env: BTreeMap::new(),
        provides_sonames: sonames.iter().map(|s| s.to_string()).collect(),
        executor: "ns".into(),
        source: "https://example.test".into(),
        license: "MIT".into(),
        supersedes: None,
        fixes: vec![],
    }
}

/// An index holding `old` and, superseding each by name, `new`.
fn index(old: &[Entry], new: &[Entry]) -> Index {
    let mut entries = old.to_vec();
    for n in new {
        let mut n = n.clone();
        n.supersedes = old
            .iter()
            .find(|o| o.name == n.name && o.digest != n.digest)
            .map(|o| o.digest.clone());
        if !entries.contains(&n) {
            entries.push(n);
        }
    }
    Index {
        schema: SCHEMA.into(),
        serial: 2,
        created: "2026-11-01T00:00:00Z".into(),
        entries,
    }
}

fn resolution(entries: &[Entry]) -> Resolution {
    Resolution {
        entries: entries.to_vec(),
        constraints: entries.iter().map(|e| format!("{}@{}", e.name, e.line)).collect(),
        index_digest: d(0xee),
    }
}

/// What an image on `old` recorded, needing `needed`.
fn on(old: &[Entry], needed: Option<&[&str]>) -> RecordedCatalog {
    RecordedCatalog {
        layers: old.iter().map(|e| e.digest.clone()).collect(),
        constraints: old.iter().map(|e| format!("{}@{}", e.name, e.line)).collect(),
        index: d(0xdd),
        chain: vec![],
        sonames_needed: needed.map(|n| n.iter().map(|s| s.to_string()).collect()),
    }
}

fn refusals(check: &potter_exec::rebase::CatalogCheck, gate: &str) -> Vec<String> {
    check.verdicts.iter().find(|v| v.gate == gate).unwrap().refusals.clone()
}

#[test]
fn a_superseding_layer_passes_the_catalog_gates_and_is_recorded_as_replaced() {
    let old = [
        entry(1, "os-min", "13", "debian-13", &["libc.so.6"]),
        entry(2, "go", "1.27", "debian-13", &[]),
    ];
    let new = [entry(3, "os-min", "13", "debian-13", &["libc.so.6"]), old[1].clone()];
    let check = check_catalog(
        &index(&old, &new),
        &on(&old, Some(&["libc.so.6"])),
        &resolution(&new),
        &BTreeSet::new(),
    );
    assert_eq!(check.replaced, [(d(1), d(3))], "go is the same entry: not replaced");
    let gates: Vec<&str> = check.verdicts.iter().map(|v| v.gate).collect();
    assert_eq!(gates, ["line", "abi", "sonames"]);
    assert!(check.verdicts.iter().all(|v| v.refusals.is_empty()), "{check:?}");
    assert!(check.refused().is_none());
}

#[test]
fn an_abi_change_is_refused_naming_key_old_and_new() {
    let old = [entry(1, "os-min", "13", "debian-13", &[])];
    let new = [entry(3, "os-min", "13", "debian-14", &[])];
    let check = check_catalog(
        &index(&old, &new),
        &on(&old, Some(&[])),
        &resolution(&new),
        &BTreeSet::new(),
    );
    assert_eq!(
        refusals(&check, "abi"),
        ["os-min: abi os changes from debian-13 to debian-14"]
    );
    let err = check.refused().unwrap();
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.to_string().starts_with("the rebase is refused"), "{err}");
}

#[test]
fn a_line_change_is_refused() {
    let old = [
        entry(1, "os-min", "13", "debian-13", &[]),
        entry(2, "libs", "1", "debian-13", &[]),
    ];
    let new = [old[0].clone(), entry(4, "libs", "2", "debian-13", &[])];
    let check = check_catalog(
        &index(&old, &new),
        &on(&old, Some(&[])),
        &resolution(&new),
        &BTreeSet::new(),
    );
    assert_eq!(refusals(&check, "line"), ["libs: the line changes from 1 to 2"]);
    // A layer the index no longer lists cannot be compared.
    let gone = on(&[entry(9, "os-min", "13", "debian-13", &[])], Some(&[]));
    let check = check_catalog(&index(&old, &new), &gone, &resolution(&new), &BTreeSet::new());
    let line = refusals(&check, "line");
    assert!(line.len() == 1 && line[0].contains(&d(9)), "{line:?}");
    assert_eq!(check.refused().unwrap().exit_code(), 2);
}

#[test]
fn missing_soname_is_refused() {
    let old = [entry(1, "os-min", "13", "debian-13", &["libc.so.6", "libssl.so.3"])];
    let new = [entry(3, "os-min", "13", "debian-13", &["libc.so.6"])];
    let rec = on(&old, Some(&["libc.so.6", "libssl.so.3"]));
    let check = check_catalog(&index(&old, &new), &rec, &resolution(&new), &BTreeSet::new());
    assert_eq!(
        refusals(&check, "sonames"),
        ["the app needs libssl.so.3, which no layer of the new run base provides"]
    );
    assert_eq!(check.refused().unwrap().exit_code(), 2);
}

#[test]
fn absent_sonames_annotation_is_refused() {
    let old = [entry(1, "os-min", "13", "debian-13", &[])];
    let new = [entry(3, "os-min", "13", "debian-13", &[])];
    let check = check_catalog(&index(&old, &new), &on(&old, None), &resolution(&new), &BTreeSet::new());
    let sonames = refusals(&check, "sonames");
    assert!(
        sonames.len() == 1 && sonames[0].contains(ANN_SONAMES_NEEDED),
        "{sonames:?}"
    );
    assert_eq!(check.refused().unwrap().exit_code(), 2);
}

/// The fixture shared object for the host: needs libc.so.6 and libssl.so.3, is
/// libpotter-fixture.so.1.
fn fixture_so() -> Vec<u8> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "aarch64"
    };
    std::fs::read(format!(
        "{}/testdata/elf/libpotter-fixture-{arch}.so",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A catalog image: run base `rb` on `entries`, with `app` above it, needing `needed`.
fn catalog_image(
    store: &Store,
    rb: &potter_exec::assemble::RunBase,
    entries: &[Entry],
    app: &[Vec<u8>],
    needed: &[&str],
) {
    let list = |v: Vec<String>| serde_json::to_string(&v).unwrap();
    let constraints: Vec<String> = entries.iter().map(|e| format!("{}@{}", e.name, e.line)).collect();
    let extra = BTreeMap::from([
        (
            ANN_CATALOG_LAYERS.to_string(),
            list(entries.iter().map(|e| e.digest.clone()).collect()),
        ),
        (ANN_CATALOG_CONSTRAINTS.to_string(), list(constraints)),
        (ANN_CATALOG_INDEX.to_string(), d(0xdd)),
        (ANN_CHAIN.to_string(), "{app}".to_string()),
        (
            ANN_SONAMES_NEEDED.to_string(),
            list(needed.iter().map(|s| s.to_string()).collect()),
        ),
    ]);
    let chain = Chain {
        base: Base::Catalog {
            layers: vec![CatalogRef::new("os-min", None)],
        },
        steps: vec![],
    };
    image_with(store, rb, app, 10, "potter/app:latest", &chain, extra);
}

#[test]
fn sonames_the_app_bundles_are_not_refused() {
    let (_home, store) = store();
    let old = [entry(1, "os-min", "13", "debian-13", &["libc.so.6", "libssl.so.3"])];
    let rb = run_base(
        &store,
        "potter-catalog@x",
        &oci_base(&store, &base_tars(1, "v1"), &["PATH=/bin"]),
        &[],
    );
    // The app ships the shared object itself, and a binary that needs it.
    let so = TarBuilder::new()
        .dir("app", &Opts::default().mode(0o755))
        .file(
            "app/libpotter-fixture.so.1",
            &fixture_so(),
            &Opts::default().mode(0o755),
        )
        .finish();
    catalog_image(
        &store,
        &rb,
        &old,
        std::slice::from_ref(&so),
        &["libc.so.6", "libpotter-fixture.so.1", "libssl.so.3"],
    );

    let rec = recorded(&store, "potter/app:latest").unwrap();
    let c = rec.catalog.clone().expect("a catalog image");
    assert_eq!(c.layers, [d(1)]);
    assert_eq!(c.constraints, ["os-min@13"]);
    assert_eq!(c.index, d(0xdd));
    assert_eq!(c.chain.len(), 1);
    assert_eq!(c.chain[0].digest.to_string(), sources(&[so])[0]);
    let kept = kept_sonames(&store, &c).unwrap();
    assert!(kept.contains("libpotter-fixture.so.1"), "{kept:?}");

    let new = [entry(3, "os-min", "13", "debian-13", &["libc.so.6", "libssl.so.3"])];
    let check = check_catalog(&index(&old, &new), &c, &resolution(&new), &kept);
    assert!(check.refused().is_none(), "{check:?}");
    // Without the bundled library counted, the same rebase would be refused.
    let check = check_catalog(&index(&old, &new), &c, &resolution(&new), &BTreeSet::new());
    assert!(
        refusals(&check, "sonames")[0].contains("libpotter-fixture.so.1"),
        "{check:?}"
    );
}

#[test]
fn a_catalog_image_resolves_its_constraints_again_and_records_the_layers_it_is_on() {
    let (_home, store) = store();
    let old = [entry(1, "os-min", "13", "debian-13", &[])];
    let rb = run_base(
        &store,
        "potter-catalog@x",
        &oci_base(&store, &base_tars(1, "v1"), &["PATH=/bin"]),
        &[],
    )
    .with_catalog(&store, &resolution(&old))
    .unwrap();
    catalog_image(&store, &rb, &old, &steps(1, "a"), &[]);
    let rec = recorded(&store, "potter/app:latest").unwrap();
    assert_eq!(
        rebase_chain(&rec, None).unwrap(),
        Chain {
            base: Base::Catalog {
                layers: vec![CatalogRef::new("os-min", Some("13"))]
            },
            steps: vec![],
        },
        "the recorded constraints, not the plan's refs"
    );
    assert_eq!(rebase_chain(&rec, Some("x:1")).unwrap_err().exit_code(), 2);

    // Rebased onto a superseding os-min, the image's source manifest still names the old layer,
    // and its run base the new one.
    let new = [entry(3, "os-min", "13", "debian-13", &[])];
    let new_rb = run_base(
        &store,
        "potter-catalog@y",
        &oci_base(&store, &base_tars(1, "v2"), &["PATH=/bin"]),
        &[],
    )
    .with_catalog(&store, &resolution(&new))
    .unwrap();
    rebase_onto(&store, &rec, &new_rb, Some("potter/app:latest")).unwrap();
    let again = recorded(&store, "potter/app:latest").unwrap();
    assert_eq!(again.catalog.unwrap().layers, [d(3)]);
}

#[test]
fn pre_p3_stopgap_image_still_rebases() {
    let (_home, store) = store();
    // A Go image built before P3: its catalog run base was lowered onto golang:1.27-trixie, and
    // it records no catalog annotations.
    let b = base_tars(2, "v1");
    let rb = run_base(&store, "golang:1.27-trixie", &oci_base(&store, &b, &["PATH=/bin"]), &[]);
    let chain = Chain {
        base: Base::Catalog {
            layers: vec![CatalogRef::new("os-min", None), CatalogRef::new("go", Some("1.27"))],
        },
        steps: vec![],
    };
    image_with(
        &store,
        &rb,
        &steps(2, "a"),
        10,
        "potter/app:latest",
        &chain,
        BTreeMap::new(),
    );
    let rec = recorded(&store, "potter/app:latest").unwrap();
    assert!(rec.catalog.is_none());
    let again = rebase_chain(&rec, None).unwrap();
    assert_eq!(
        again,
        Chain {
            base: Base::Oci {
                reference: "golang:1.27-trixie".into(),
                digest: None
            },
            steps: vec![],
        },
        "lowered through the stopgap again, not resolved against the catalog"
    );
    assert!(!potter_exec::chain::needs_catalog(&again.base));
    // The stopgap's patched image is taken as any OCI base's.
    let mut patched = b.clone();
    patched[1] = layer("base1", "v2");
    let new_rb = run_base(
        &store,
        "golang:1.27-trixie",
        &oci_base(&store, &patched, &["PATH=/bin"]),
        &[],
    );
    let r = rebase_onto(&store, &rec, &new_rb, Some("potter/app:latest")).unwrap();
    assert!(!r.unchanged);
    assert!(r.replaced.is_empty() && r.verdicts.is_empty());
    assert_eq!(rebase_chain(&rec, Some("x:1")).unwrap_err().exit_code(), 2);
}
