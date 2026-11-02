//! The detector's version tables and the catalog's recipes agree (P3 design §7.1): every non-PHP
//! line the detector can pick has a recipe under `catalog/` with that name and line, every
//! runtime recipe is a line the detector knows, and every catalog layer a detected plan names
//! exists. A drift here would plan builds the catalog cannot serve, or ship layers no detection
//! picks. No sandbox, no network.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use potter_catalog::{Kind, is_php_only};
use potter_detect::catalog::RUNTIMES;
use potter_exec::catalog_build::recipe::{self, Recipe};
use potter_plan::{Base, CatalogRef};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn recipes() -> Vec<Recipe> {
    recipe::load(&repo().join("catalog")).expect("the catalog's recipes load")
}

fn has(recipes: &[Recipe], name: &str, line: Option<&str>) -> bool {
    recipes
        .iter()
        .any(|r| r.layer.name == name && line.is_none_or(|l| r.layer.line == l))
}

#[test]
fn every_table_line_has_a_recipe() {
    let recipes = recipes();
    let mut missing = Vec::new();
    for rt in RUNTIMES.iter().filter(|rt| !is_php_only(rt.name)) {
        for line in rt.lines {
            match recipes
                .iter()
                .find(|r| r.layer.name == rt.name && r.layer.line == line.version)
            {
                Some(r) if r.layer.kind == Kind::Runtime => {}
                Some(r) => missing.push(format!(
                    "{}@{} is a {:?} recipe, not a runtime",
                    rt.name, line.version, r.layer.kind
                )),
                None => missing.push(format!("{}@{}", rt.name, line.version)),
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the detector picks lines no recipe under catalog/ builds: {}",
        missing.join(", ")
    );
}

#[test]
fn every_runtime_recipe_is_a_table_line() {
    let recipes = recipes();
    let mut extra = Vec::new();
    for r in &recipes {
        let l = &r.layer;
        assert!(
            !is_php_only(&l.name),
            "catalog/{} is PHP-only, which the catalog does not carry",
            l.name
        );
        if l.kind != Kind::Runtime {
            continue;
        }
        let known = RUNTIMES
            .iter()
            .filter(|rt| !is_php_only(rt.name))
            .any(|rt| rt.name == l.name && rt.lines.iter().any(|x| x.version == l.line));
        if !known {
            extra.push(format!("{}@{}", l.name, l.line));
        }
    }
    assert!(
        extra.is_empty(),
        "catalog/ builds runtime lines the detector's tables do not have: {}",
        extra.join(", ")
    );
}

/// The project directories under `dir`.
fn projects(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

#[test]
fn every_layer_a_detected_plan_names_has_a_recipe() {
    let recipes = recipes();
    let mut seen = BTreeSet::new();
    let mut missing = BTreeSet::new();
    let dirs = [
        repo().join("fixtures"),
        repo().join("crates/potter/tests/fixtures/catalog-e2e"),
    ];
    for project in dirs.iter().flat_map(|d| projects(d)) {
        // Some fixtures exist to be refused (ambiguous, unsatisfiable); they name no layers.
        let Ok(planned) = potter_config::plan_repo(&project, None) else {
            continue;
        };
        // Explicit plans (provider `none`) name whatever their author chose.
        for t in planned.plan.targets.iter().filter(|t| t.provider != "none") {
            for base in [&t.build.base, &t.image.base] {
                let Base::Catalog { layers } = base else { continue };
                // A base naming anything PHP-only goes through the stopgap whole.
                if layers.iter().any(|l| is_php_only(&l.name)) {
                    continue;
                }
                for CatalogRef { name, version } in layers {
                    seen.insert(name.clone());
                    if !has(&recipes, name, version.as_deref()) {
                        let r = match version {
                            Some(v) => format!("{name}@{v}"),
                            None => name.clone(),
                        };
                        missing.insert(format!("{r} ({})", project.display()));
                    }
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "detected plans name catalog layers no recipe builds: {}",
        missing.into_iter().collect::<Vec<_>>().join(", ")
    );
    // The fixtures reach every kind of base the detectors make.
    for name in [
        "os-min",
        "build-tools",
        "static-server",
        "node",
        "bun",
        "deno",
        "python",
        "uv",
        "poetry",
        "go",
        "rust",
        "ruby",
    ] {
        assert!(seen.contains(name), "no fixture plans a base with {name}: {seen:?}");
    }
}
