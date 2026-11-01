//! Catalog recipes (design §4.2): `catalog/<name>/layer.toml` says what the layer is, and
//! `catalog/<name>/potter.toml` is the explicit plan whose target `<name>` builds it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use potter_catalog::Kind;
use potter_plan::{Base, CatalogRef, Target, is_name};
use serde::Deserialize;

use crate::error::BuildError;

/// The layer every other layer sits on. Its build chain starts from an OCI image, and the
/// driver reads every entry's `os` and `libc` ABI values from it.
pub const OS_MIN: &str = "os-min";

/// `layer.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerToml {
    pub name: String,
    pub line: String,
    pub version: String,
    pub kind: Kind,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// ABI values beyond `os` and `libc`, which the driver fills in from `os-min`.
    #[serde(default)]
    pub abi: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub source: String,
    pub license: String,
    /// A shell script that must succeed on the composite of the layer's closure.
    pub smoke: String,
    /// Extra layers for the smoke composite, e.g. `build-tools`.
    #[serde(default)]
    pub smoke_with: Vec<String>,
    /// Advisory ids the next superseding entry fixes.
    #[serde(default)]
    pub fixes: Vec<String>,
}

/// One recipe, read and checked.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub dir: PathBuf,
    pub layer: LayerToml,
    pub target: Target,
}

impl Recipe {
    pub fn name(&self) -> &str {
        &self.layer.name
    }

    /// `requires` as catalog refs.
    pub fn requires(&self) -> Vec<CatalogRef> {
        self.layer.requires.iter().map(|r| catalog_ref(r)).collect()
    }

    /// The layers the smoke test runs on: this one at its line, then `smoke_with`.
    pub fn smoke_refs(&self) -> Vec<CatalogRef> {
        std::iter::once(CatalogRef::new(&self.layer.name, Some(&self.layer.line)))
            .chain(self.layer.smoke_with.iter().map(|r| catalog_ref(r)))
            .collect()
    }

    /// The catalog layers the build chain starts from; none for `os-min`.
    fn base_layers(&self) -> Vec<&str> {
        match &self.target.build.base {
            Base::Catalog { layers } => layers.iter().map(|l| l.name.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

fn catalog_ref(s: &str) -> CatalogRef {
    match s.split_once('@') {
        Some((name, line)) => CatalogRef::new(name, Some(line)),
        None => CatalogRef::new(s, None),
    }
}

fn plan(msg: String) -> BuildError {
    BuildError::Plan(msg)
}

/// Parses `text`, the `layer.toml` at `path`.
pub fn parse_layer(text: &str, path: &Path) -> Result<LayerToml, BuildError> {
    let layer: LayerToml = toml::from_str(text).map_err(|e| plan(format!("{}: {e}", path.display())))?;
    let bad = |why: String| plan(format!("{}: {why}", path.display()));
    if !is_name(&layer.name) {
        return Err(bad(format!("name {:?} is not a name", layer.name)));
    }
    for key in ["os", "libc"] {
        if layer.abi.contains_key(key) {
            return Err(bad(format!(
                "abi.{key} is filled in by the driver from {OS_MIN}, not set by a recipe"
            )));
        }
    }
    for r in layer.requires.iter().chain(&layer.smoke_with) {
        let c = catalog_ref(r);
        if !is_name(&c.name) || c.version.as_deref().is_some_and(str::is_empty) {
            return Err(bad(format!("{r:?} is not name or name@line")));
        }
    }
    if layer.smoke.trim().is_empty() {
        return Err(bad("smoke must not be empty".into()));
    }
    Ok(layer)
}

/// Every recipe under `catalog_dir`, each after the layers it requires or builds on.
pub fn load(catalog_dir: &Path) -> Result<Vec<Recipe>, BuildError> {
    let read_dir = std::fs::read_dir(catalog_dir).map_err(|e| {
        let what = format!("the catalog directory {}: {e}", catalog_dir.display());
        match e.kind() {
            std::io::ErrorKind::NotFound => plan(what),
            _ => BuildError::Infra(what),
        }
    })?;
    let mut dirs = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|e| BuildError::Infra(format!("{}: {e}", catalog_dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        dirs.push((name, entry.path()));
    }
    dirs.sort();
    let mut recipes = Vec::new();
    for (name, dir) in dirs {
        let path = dir.join("layer.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => plan(format!("{} has no layer.toml", dir.display())),
            _ => BuildError::Infra(format!("{}: {e}", path.display())),
        })?;
        let layer = parse_layer(&text, &path)?;
        if layer.name != name {
            return Err(plan(format!(
                "{}: name {:?} is not the directory's name {name:?}",
                path.display(),
                layer.name
            )));
        }
        let planned = potter_config::plan_repo(&dir, Some(&name)).map_err(|e| {
            let what = format!("the recipe {name}: {e}");
            match e.is_infrastructure() {
                true => BuildError::Infra(what),
                false => plan(what),
            }
        })?;
        let target = planned
            .plan
            .targets
            .into_iter()
            .find(|t| t.name == name)
            .ok_or_else(|| plan(format!("the recipe {name}: potter.toml has no target {name}")))?;
        recipes.push(Recipe { dir, layer, target });
    }
    check(&recipes)?;
    order(recipes)
}

/// What a recipe may build on and name.
fn check(recipes: &[Recipe]) -> Result<(), BuildError> {
    let names: BTreeSet<&str> = recipes.iter().map(Recipe::name).collect();
    if !names.contains(OS_MIN) {
        return Err(plan(format!(
            "the catalog has no {OS_MIN} recipe, which every layer sits on"
        )));
    }
    for r in recipes {
        let name = r.name();
        let base = r.base_layers();
        if name == OS_MIN {
            if !matches!(r.target.build.base, Base::Oci { .. }) || !r.layer.requires.is_empty() {
                return Err(plan(format!(
                    "the recipe {OS_MIN} builds on an `oci` base and requires nothing"
                )));
            }
        } else if !matches!(r.target.build.base, Base::Catalog { .. }) {
            return Err(plan(format!(
                "the recipe {name} must build on a `catalog` base of the layers it requires"
            )));
        }
        for c in r.requires() {
            if !base.contains(&c.name.as_str()) {
                return Err(plan(format!(
                    "the recipe {name} requires {} but does not build on it",
                    c.name
                )));
            }
        }
        for (what, list) in [
            ("builds on", base.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            (
                "smoke-tests with",
                r.layer.smoke_with.iter().map(|s| catalog_ref(s).name).collect(),
            ),
        ] {
            if let Some(missing) = list.iter().find(|n| !names.contains(n.as_str())) {
                return Err(plan(format!(
                    "the recipe {name} {what} {missing}, which has no recipe here"
                )));
            }
        }
    }
    Ok(())
}

/// `recipes` with every recipe after the ones it builds on, ties by name.
pub fn order(recipes: Vec<Recipe>) -> Result<Vec<Recipe>, BuildError> {
    let deps: BTreeMap<String, BTreeSet<String>> = recipes
        .iter()
        .map(|r| {
            let d = r
                .base_layers()
                .into_iter()
                .map(str::to_string)
                .chain(r.requires().into_iter().map(|c| c.name))
                .filter(|d| d != r.name())
                .collect();
            (r.name().to_string(), d)
        })
        .collect();
    let mut left: BTreeMap<String, Recipe> = recipes.into_iter().map(|r| (r.name().to_string(), r)).collect();
    let mut out: Vec<Recipe> = Vec::with_capacity(left.len());
    while !left.is_empty() {
        let next = left
            .keys()
            .find(|n| deps[n.as_str()].iter().all(|d| out.iter().any(|r| r.name() == d)))
            .cloned();
        let Some(next) = next else {
            let stuck: Vec<&str> = left.keys().map(String::as_str).collect();
            return Err(plan(format!(
                "the recipes {} require each other in a cycle",
                stuck.join(", ")
            )));
        };
        out.push(left.remove(&next).expect("a key of left"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYTHON: &str = r#"
name = "python"
line = "3.15"
version = "3.15.0"
kind = "runtime"
requires = ["os-min"]
conflicts = []
abi = { python-abi = "cp315" }
env = { PATH = "/usr/local/bin" }
source = "https://www.python.org/ftp/python/3.15.0/Python-3.15.0.tar.xz"
license = "PSF-2.0"
smoke = "python3 -c 'import ssl, sqlite3, ctypes'"
smoke_with = []
fixes = []
"#;

    fn path() -> PathBuf {
        PathBuf::from("catalog/python/layer.toml")
    }

    #[test]
    fn the_brief_example_parses() {
        let l = parse_layer(PYTHON, &path()).unwrap();
        assert_eq!(l.name, "python");
        assert_eq!((l.line.as_str(), l.version.as_str()), ("3.15", "3.15.0"));
        assert_eq!(l.kind, Kind::Runtime);
        assert_eq!(l.requires, ["os-min"]);
        assert_eq!(l.abi["python-abi"], "cp315");
        assert_eq!(l.env["PATH"], "/usr/local/bin");
        assert_eq!(l.smoke, "python3 -c 'import ssl, sqlite3, ctypes'");
    }

    #[test]
    fn unknown_keys_are_refused() {
        let text = format!("{PYTHON}surprise = true\n");
        let err = parse_layer(&text, &path()).unwrap_err();
        assert!(matches!(&err, BuildError::Plan(m) if m.contains("surprise")), "{err:?}");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn os_and_libc_belong_to_the_driver() {
        let text = PYTHON.replace("python-abi = \"cp315\"", "os = \"debian-12\"");
        let err = parse_layer(&text, &path()).unwrap_err();
        assert!(matches!(&err, BuildError::Plan(m) if m.contains("abi.os")), "{err:?}");
    }

    #[test]
    fn missing_fields_and_bad_refs_are_refused() {
        let text = PYTHON.replace("smoke = \"python3 -c 'import ssl, sqlite3, ctypes'\"\n", "");
        assert!(matches!(parse_layer(&text, &path()), Err(BuildError::Plan(m)) if m.contains("smoke")));
        let text = PYTHON.replace("requires = [\"os-min\"]", "requires = [\"os-min@\"]");
        assert!(matches!(parse_layer(&text, &path()), Err(BuildError::Plan(m)) if m.contains("os-min@")));
        let text = PYTHON.replace("kind = \"runtime\"", "kind = \"daemon\"");
        assert!(matches!(parse_layer(&text, &path()), Err(BuildError::Plan(_))));
    }

    fn recipe(dir: &Path, name: &str, base: &str, requires: &[&str], extra: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        let requires = requires.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>().join(", ");
        std::fs::write(
            d.join("layer.toml"),
            format!(
                "name = {name:?}\nline = \"1\"\nversion = \"1.0\"\nkind = \"tool\"\nrequires = [{requires}]\n\
                 source = \"https://example.test\"\nlicense = \"MIT\"\nsmoke = \"true\"\n{extra}"
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("potter.toml"),
            format!(
                "[target.{name}]\nprovider = \"none\"\n\n[target.{name}.base]\n{base}\n\n\
                 [target.{name}.process.sh]\ncmd = [\"/bin/sh\"]\n"
            ),
        )
        .unwrap();
    }

    fn names(r: &[Recipe]) -> Vec<&str> {
        r.iter().map(Recipe::name).collect()
    }

    #[test]
    fn recipes_load_in_requires_order() {
        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "os-min", "oci = \"example.test/debian:13\"", &[], "");
        recipe(
            dir.path(),
            "aaa",
            "catalog = [\"os-min\", \"build-tools\"]",
            &["os-min"],
            "",
        );
        recipe(dir.path(), "build-tools", "catalog = [\"os-min\"]", &["os-min"], "");
        recipe(dir.path(), "zzz", "catalog = [\"os-min\"]", &["os-min"], "");
        let r = load(dir.path()).unwrap();
        assert_eq!(names(&r), ["os-min", "build-tools", "aaa", "zzz"]);
        assert_eq!(r[2].requires(), [CatalogRef::new("os-min", None)]);
        assert_eq!(
            r[2].smoke_refs(),
            [CatalogRef::new("aaa", Some("1"))],
            "the smoke composite is the layer at its line"
        );
    }

    #[test]
    fn a_cycle_or_a_missing_layer_is_a_plan_error() {
        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "os-min", "oci = \"example.test/debian:13\"", &[], "");
        recipe(dir.path(), "a", "catalog = [\"os-min\", \"b\"]", &["os-min"], "");
        recipe(dir.path(), "b", "catalog = [\"os-min\", \"a\"]", &["os-min"], "");
        let err = load(dir.path()).unwrap_err();
        assert!(matches!(&err, BuildError::Plan(m) if m.contains("cycle")), "{err:?}");

        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "os-min", "oci = \"example.test/debian:13\"", &[], "");
        recipe(
            dir.path(),
            "a",
            "catalog = [\"os-min\"]",
            &["os-min"],
            "smoke_with = [\"build-tools\"]\n",
        );
        let err = load(dir.path()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("build-tools") && m.contains("no recipe")),
            "{err:?}"
        );
    }

    #[test]
    fn bases_must_match_the_recipe_kind() {
        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "a", "catalog = [\"os-min\"]", &["os-min"], "");
        let err = load(dir.path()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("no os-min recipe")),
            "{err:?}"
        );

        recipe(dir.path(), "os-min", "catalog = [\"a\"]", &[], "");
        let err = load(dir.path()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("`oci` base")),
            "{err:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "os-min", "oci = \"example.test/debian:13\"", &[], "");
        recipe(dir.path(), "a", "oci = \"example.test/debian:13\"", &["os-min"], "");
        let err = load(dir.path()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("`catalog` base")),
            "{err:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "os-min", "oci = \"example.test/debian:13\"", &[], "");
        recipe(dir.path(), "b", "catalog = [\"os-min\"]", &["os-min"], "");
        recipe(dir.path(), "a", "catalog = [\"os-min\"]", &["os-min", "b"], "");
        let err = load(dir.path()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("requires b but does not build on it")),
            "{err:?}"
        );
    }

    #[test]
    fn the_layer_name_is_the_directory_name() {
        let dir = tempfile::tempdir().unwrap();
        recipe(dir.path(), "os-min", "oci = \"example.test/debian:13\"", &[], "");
        let toml = dir.path().join("os-min/layer.toml");
        let text = std::fs::read_to_string(&toml)
            .unwrap()
            .replace("\"os-min\"", "\"base\"");
        std::fs::write(&toml, text).unwrap();
        let err = load(dir.path()).unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("directory")),
            "{err:?}"
        );
    }

    /// The driver tests' fixture loads, os-min first, once its base is filled in.
    #[test]
    fn the_test_fixture_loads() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog");
        let dir = tempfile::tempdir().unwrap();
        for name in ["os-min", "hello"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
            for f in ["potter.toml", "layer.toml"] {
                let text = std::fs::read_to_string(src.join(name).join(f)).unwrap();
                let text = text.replace("@BASE@", "127.0.0.1:5000/lib/busybox:1");
                std::fs::write(dir.path().join(name).join(f), text).unwrap();
            }
        }
        let r = load(dir.path()).unwrap();
        assert_eq!(names(&r), ["os-min", "hello"]);
        assert_eq!(r[1].layer.fixes, ["TEST-2026-0001"]);
    }
}
