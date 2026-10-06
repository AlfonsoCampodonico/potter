# potter P1b (remaining detectors) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Python, Ruby, Go and Rust detectors, with their constraint parsers (PEP 440, RubyGems, Go/Cargo minimums), so `potter plan` covers every language in spec §6.1.

**Architecture:** Each language is one `Provider` in `potter-detect`. All four use the P1a building blocks: `RepoView`, `common::*`, `catalog::pick` and decisions. They claim as `Backend`, so asset-only JS is absorbed, as for PHP. potter-config gains the matching `runtime.*` and `[python]` keys.

**Tech Stack:** As P1a, plus `toml = "0.9"` in potter-detect (for pyproject.toml, Cargo.toml and rust-toolchain.toml).

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md`. Task 1 bumps it to rev 1.5. This plan covers §6.1 (the Python, Ruby, Go and Rust rows), §7 (catalog lines) and §15 P1b.

**Where to work:** branch `p1b-detectors` from `main`, in a worktree. Push to `main` once the review is done and CI is green.

**Commits:** plain messages, with no attribution trailers.

## Global Constraints

- Every P1a global constraint still applies (exit codes, `RepoView` caps, sanitising, naming rules, no code execution).
- Catalog lines until P3 (spec rev 1.5 refresh):
  - python 3.12/3.13/3.14, default 3.13;
  - ruby 3.3/3.4/4.0, default 3.4;
  - go 1.25/1.26/1.27, default 1.27;
  - rust 1.97/1.98/1.99, default 1.99.
- Default ports: Python 8000; Ruby 3000; Go and Rust 8080.
- Python dependencies go into `/app/.venv`. Processes call `/app/.venv/bin/<tool>` by absolute path.
- Go steps set `GOTOOLCHAIN=local` and build with `CGO_ENABLED=0`. Binaries go to `/out/<name>`, and the root package's binary is named `app`.
- Rust builds dependencies against stub sources first, then runs `cargo clean --release -p <members>`, the real build, and a copy to `/out/`. Every repo-derived path or name used in a shell command must match `[A-Za-z0-9._/-]` and be relative.
- Steps added by a detector use the names `apt-detected` (build chain) and `apt-detected-runtime` (image), so they never collide with the user's `apt-build`/`apt-runtime`.

## Review Focus

1. **Shell injection through repo-derived Rust paths and package names** (`[[bin]] path`, workspace members): they must be refused, never interpolated. Pinned by `hostile_bin_path_is_refused` (Task 6).
2. **A requirements file whose `-r` include escapes the build root** must give a Parse error naming the file. Pinned by `escaping_include_is_refused` (Task 3).
3. **`.ruby-version` with a `ruby-` prefix** (real rails-cloud-app) must parse. Pinned by `rails_with_propshaft_and_bootsnap` (Task 4).
4. **A go.mod newer than the catalog** (`go 1.30`) must give an error listing the available lines. Pinned by `too_new_go_is_unsatisfiable` (Task 5).
5. **Ambiguous multi-binary Go and Rust projects** must ask for `process.web.cmd`, never pick arbitrarily. Pinned by `ambiguous_mains_need_config` (Task 5) and `workspace_with_two_binaries` (Task 6: chooses `api` by name).

---

### Task 1: Foundation: provider ids, options, catalog lines, TOML reading, shared apt snapshot, spec rev 1.5

**Files:**
- Modify:
  - `crates/potter-detect/Cargo.toml` (add `toml = "0.9"`);
  - `crates/potter-detect/src/error.rs` (`ProviderId::{Python, Ruby, Go, Rust}`, with `ALL` grown to 10);
  - `crates/potter-detect/src/options.rs` (`RuntimeOverrides.{python, ruby, go, rust}`, plus `PythonOptions { package_manager, collectstatic }` on `DetectOptions.python`);
  - `crates/potter-detect/src/lib.rs` (re-export `PythonOptions`);
  - `crates/potter-detect/src/catalog.rs` (`PYTHON`, `RUBY`, `GO`, `RUST`);
  - `crates/potter-detect/src/repo.rs` (`read_toml`, `RepoError::Toml`);
  - `crates/potter-plan/src/ir.rs` (`pub const DEFAULT_APT_SNAPSHOT: &str = "20261001T000000Z";`);
  - `crates/potter-config/src/file.rs` (`pub use potter_plan::DEFAULT_APT_SNAPSHOT;` replaces its own constant);
  - `docs/superpowers/specs/2026-10-04-potter-design.md` (rev 1.5).

**Interfaces:**
- Produces:
  - `RepoView::read_toml(&str) -> Result<Option<toml::Value>, RepoError>` (4 MiB cap) and `RepoError::Toml { path, reason }`;
  - `catalog::{PYTHON, RUBY, GO, RUST}`;
  - `DetectOptions.python: PythonOptions`;
  - `RuntimeOverrides.{python, ruby, go, rust}: Option<String>`;
  - `potter_plan::DEFAULT_APT_SNAPSHOT`.

- [ ] **Step 1: Write failing tests**
  - Add to the `catalog.rs` tests:
    - `assert_eq!(pick(&GO, "runtime.go", src(crate::version::parse_npm_range(">=1.26").unwrap()), &mut d).unwrap().version, "1.27");`
    - `assert_eq!(pick(&RUST, "runtime.rust", None, &mut d).unwrap().version, "1.99");`
    - `assert_eq!(pick(&PYTHON, "runtime.python", None, &mut d).unwrap().version, "3.13");`
  - Add to the `repo.rs` tests a `reads_toml` test:
    - a valid `Cargo.toml` gives `Some`;
    - `"[broken"` gives `RepoError::Toml { path, .. }` with `path == "Cargo.toml"`.
- [ ] **Step 2: Run** `cargo test -p potter-detect`. Expected: compile errors (`GO`, `read_toml` missing).
- [ ] **Step 3: Implement**
  - Catalog lines:
    - `pub static PYTHON: Runtime = Runtime { name: "python", lines: &[line("3.12", 3, Some(12), false), line("3.13", 3, Some(13), false), line("3.14", 3, Some(14), false)], default: "3.13" };`
    - `pub static RUBY: Runtime = Runtime { name: "ruby", lines: &[line("3.3", 3, Some(3), false), line("3.4", 3, Some(4), false), line("4.0", 4, Some(0), false)], default: "3.4" };`
    - `pub static GO: Runtime = Runtime { name: "go", lines: &[line("1.25", 1, Some(25), false), line("1.26", 1, Some(26), false), line("1.27", 1, Some(27), false)], default: "1.27" };`
    - `pub static RUST: Runtime = Runtime { name: "rust", lines: &[line("1.97", 1, Some(97), false), line("1.98", 1, Some(98), false), line("1.99", 1, Some(99), false)], default: "1.99" };`
  - `read_toml`: `read_string(rel, READ_CAP)`, then `text.parse::<toml::Value>()`, mapping errors to `RepoError::Toml { path: rel, reason: e.to_string() }`.
  - Spec rev 1.5 records:
    - the refreshed catalog lines;
    - default ports;
    - Python's venv and the `[target.x.python]` keys `package_manager` and `django.collectstatic`;
    - Go's `GOTOOLCHAIN=local` and multi-main rule;
    - Rust's stub step and shell-safe path rule;
    - the Ruby worker detection (solid_queue/sidekiq);
    - the `apt-detected` step names.
- [ ] **Step 4: Run** `cargo test --all && cargo clippy --all-targets -- -D warnings`. Expected: everything passes.
- [ ] **Step 5: Commit** `feat(potter-detect): P1b foundation — provider ids, catalog lines, TOML reading`.

---

### Task 2: PEP 440, RubyGems, Poetry and minimum-version parsers

**Files:**
- Create: `crates/potter-detect/src/version/extra.rs` (contents below, tests included).
- Modify:
  - `crates/potter-detect/src/version/npm.rs`: add `pub(crate) fn npm_clause(s: &str) -> Result<VersionSet, ConstraintError> { parse_and(s, s) }`.
  - `crates/potter-detect/src/version/mod.rs`: add `mod extra; pub use extra::{at_least, parse_gem_requirement, parse_pep440, parse_poetry_constraint, release_prefix};` and `pub(crate) use npm::npm_clause;`.

**Interfaces:** produces `release_prefix`, `at_least`, `parse_pep440`, `parse_gem_requirement` and `parse_poetry_constraint`, all `-> Result<VersionSet, ConstraintError>` except `release_prefix -> &str`.

- [ ] **Step 1:** Write `extra.rs` with only its test module, and register the module. **Run** `cargo test -p potter-detect version`. Expected: compile errors.
- [ ] **Step 2:** Add the implementation above the tests. **Run** the tests and clippy. Expected: they pass.
- [ ] **Step 3: Commit** `feat(potter-detect): PEP 440, RubyGems and Poetry constraint parsers`.

`crates/potter-detect/src/version/extra.rs`:

```rust
//! PEP 440 specifiers (Python), RubyGems requirements, and the leading release number shared by
//! Go, Rust and Ruby version strings.

use super::{Bound, ConstraintError, Version, VersionSet, bump, npm_clause, parse_partial};

/// The leading `[0-9.*]` run: `3.12.0rc1` → `3.12.0`, `1.26.4` → `1.26.4`, `3.3.6p108` → `3.3.6`.
pub fn release_prefix(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '*')).unwrap_or(s.len());
    s[..end].trim_end_matches('.')
}

/// The set of versions at or above `s` (Go's `go` directive, Cargo's `rust-version`).
pub fn at_least(s: &str) -> Result<VersionSet, ConstraintError> {
    let p = parse_partial(release_prefix(s))?;
    Ok(VersionSet::interval(Bound::Inclusive(p.floor()), Bound::Unbounded))
}

/// PEP 440 version specifiers, e.g. `>=3.11,<3.14` or `~=3.12`.
pub fn parse_pep440(spec: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::any();
    for clause in spec.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        set = set.intersect(&pep440_clause(clause, spec)?);
    }
    Ok(set)
}

fn pep440_clause(clause: &str, spec: &str) -> Result<VersionSet, ConstraintError> {
    let err = |why: &str| ConstraintError(format!("`{spec}`: {why}"));
    let (op, rest) = ["===", "~=", "==", "!=", "<=", ">=", "<", ">"]
        .iter()
        .find_map(|op| clause.strip_prefix(op).map(|r| (*op, r.trim())))
        .ok_or_else(|| err("each clause needs an operator such as >= or =="))?;
    if rest.contains('!') {
        return Err(err("version epochs are not supported"));
    }
    let release = release_prefix(rest);
    if release.is_empty() {
        return Err(err("operator without a version"));
    }
    let p = parse_partial(release)?;
    let v = p.floor();
    Ok(match op {
        "==" | "===" if p.wildcard => p.x_range()?,
        "==" | "===" => VersionSet::exact(v),
        "!=" if p.wildcard => {
            let hi = p.upper_exclusive()?.ok_or_else(|| err("`!=*` excludes everything"))?;
            VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
                .union(VersionSet::interval(Bound::Inclusive(hi), Bound::Unbounded))
        }
        "!=" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
            .union(VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded)),
        "<=" => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(v)),
        ">=" => VersionSet::interval(Bound::Inclusive(v), Bound::Unbounded),
        "<" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v)),
        ">" => VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded),
        "~=" => compatible(release, v, &err)?,
        _ => unreachable!("listed operators only"),
    })
}

/// `~=X.Y` → `>=X.Y, ==X.*`; `~=X.Y.Z` → `>=X.Y.Z, ==X.Y.*`.
fn compatible(release: &str, v: Version, err: &dyn Fn(&str) -> ConstraintError) -> Result<VersionSet, ConstraintError> {
    let hi = match release.split('.').count() {
        0 | 1 => return Err(err("~= needs at least two release segments")),
        2 => Version::new(bump(v.major)?, 0, 0),
        _ => Version::new(v.major, bump(v.minor)?, 0),
    };
    Ok(VersionSet::interval(Bound::Inclusive(v), Bound::Exclusive(hi)))
}

/// RubyGems requirements, e.g. `~> 3.3`, `>= 3.2, < 4`.
pub fn parse_gem_requirement(req: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::any();
    let mut any = false;
    for clause in req.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        any = true;
        set = set.intersect(&gem_clause(clause, req)?);
    }
    if !any {
        return Err(ConstraintError(format!("`{req}`: empty requirement")));
    }
    Ok(set)
}

fn gem_clause(clause: &str, req: &str) -> Result<VersionSet, ConstraintError> {
    let (op, rest) = ["~>", ">=", "<=", "!=", "=", ">", "<"]
        .iter()
        .find_map(|op| clause.strip_prefix(op).map(|r| (*op, r.trim())))
        .unwrap_or(("=", clause));
    let release = release_prefix(rest);
    if release.is_empty() {
        return Err(ConstraintError(format!("`{req}`: operator without a version")));
    }
    let p = parse_partial(release)?;
    let v = p.floor();
    Ok(match op {
        "=" => VersionSet::exact(v),
        "!=" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
            .union(VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded)),
        ">=" => VersionSet::interval(Bound::Inclusive(v), Bound::Unbounded),
        "<=" => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(v)),
        ">" => VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded),
        "<" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v)),
        "~>" => {
            // Pessimistic: drop the last segment and bump the one before it (`~> 3` → `< 4`).
            let hi = match (p.minor, p.patch) {
                (None, _) | (Some(_), None) => Version::new(bump(v.major)?, 0, 0),
                (Some(_), Some(_)) => Version::new(v.major, bump(v.minor)?, 0),
            };
            VersionSet::interval(Bound::Inclusive(v), Bound::Exclusive(hi))
        }
        _ => unreachable!("listed operators only"),
    })
}

/// Poetry's `python = "^3.11"` style constraints: npm caret/tilde semantics, comma as AND.
pub fn parse_poetry_constraint(c: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::any();
    for clause in c.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        let alternatives: Result<Vec<VersionSet>, _> = clause.split("||").map(|a| npm_clause(a.trim())).collect();
        let union = alternatives?.into_iter().fold(VersionSet::none(), VersionSet::union);
        set = set.intersect(&union);
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(set: &VersionSet, v: (u64, u64, u64)) -> bool {
        set.contains(Version::new(v.0, v.1, v.2))
    }

    #[test]
    fn release_prefixes() {
        assert_eq!(release_prefix("3.12.0rc1"), "3.12.0");
        assert_eq!(release_prefix("3.3.6p108"), "3.3.6");
        assert_eq!(release_prefix("1.26"), "1.26");
        assert_eq!(release_prefix("v1.2."), "1.2");
        assert_eq!(release_prefix("stable"), "");
    }

    #[test]
    fn minimums() {
        let s = at_least("1.26").unwrap();
        assert!(has(&s, (1, 26, 0)) && has(&s, (1, 27, 3)) && !has(&s, (1, 25, 9)));
        assert!(at_least("nightly").is_err());
    }

    #[test]
    fn pep440() {
        let s = parse_pep440(">=3.11,<3.14").unwrap();
        assert!(has(&s, (3, 13, 9)) && !has(&s, (3, 14, 0)) && !has(&s, (3, 10, 9)));
        let s = parse_pep440("~=3.12").unwrap();
        assert!(has(&s, (3, 14, 0)) && !has(&s, (4, 0, 0)) && !has(&s, (3, 11, 9)));
        let s = parse_pep440("~=3.12.1").unwrap();
        assert!(has(&s, (3, 12, 5)) && !has(&s, (3, 13, 0)));
        let s = parse_pep440("==3.12.*").unwrap();
        assert!(has(&s, (3, 12, 9)) && !has(&s, (3, 13, 0)));
        let s = parse_pep440("!=3.13.*, >=3.11").unwrap();
        assert!(has(&s, (3, 12, 0)) && !has(&s, (3, 13, 2)) && has(&s, (3, 14, 0)));
        let s = parse_pep440(">=3.10.0rc1").unwrap();
        assert!(has(&s, (3, 10, 0)));
        assert!(parse_pep440("").unwrap().contains(Version::new(9, 9, 9)));
        for bad in ["3.12", "~=3", ">=", ">=1!2.0", ">=abc"] {
            assert!(parse_pep440(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rubygems() {
        let s = parse_gem_requirement("~> 3.3").unwrap();
        assert!(has(&s, (3, 4, 1)) && !has(&s, (4, 0, 0)) && !has(&s, (3, 2, 9)));
        let s = parse_gem_requirement("~> 3.3.6").unwrap();
        assert!(has(&s, (3, 3, 9)) && !has(&s, (3, 4, 0)));
        let s = parse_gem_requirement("~> 3").unwrap();
        assert!(has(&s, (3, 9, 0)) && !has(&s, (4, 0, 0)));
        let s = parse_gem_requirement(">= 3.2, < 4").unwrap();
        assert!(has(&s, (3, 4, 0)) && !has(&s, (4, 0, 0)));
        let s = parse_gem_requirement("3.3.6").unwrap();
        assert!(has(&s, (3, 3, 6)) && !has(&s, (3, 3, 7)));
        assert!(parse_gem_requirement("").is_err());
        assert!(parse_gem_requirement("~>").is_err());
    }

    #[test]
    fn poetry() {
        let s = parse_poetry_constraint("^3.11").unwrap();
        assert!(has(&s, (3, 13, 0)) && !has(&s, (4, 0, 0)));
        let s = parse_poetry_constraint(">=3.11,<3.13").unwrap();
        assert!(has(&s, (3, 12, 9)) && !has(&s, (3, 13, 0)));
        let s = parse_poetry_constraint("~3.12").unwrap();
        assert!(has(&s, (3, 12, 4)) && !has(&s, (3, 13, 0)));
    }
}
```

---

### Task 3: The Python provider

**Files:**
- Create: `crates/potter-detect/src/providers/python.rs` and `crates/potter-detect/tests/python.rs`.
- Modify: `crates/potter-detect/src/providers/mod.rs` (`mod python;`, add `&python::PythonProvider` to `all()`).
- Fixtures:
  - `fixtures/python-django`: copied `requirements.txt` and `manage.py` from `django-hello-world`;
  - `fixtures/python-fastapi`: `requirements.txt` and `main.py` from `fastapi-hello-world`;
  - `fixtures/python-flask`: `requirements.txt` and `app/main.py` from `flask-hello-world`;
  - `fixtures/python-uv` and `fixtures/python-poetry`: synthetic (contents below).

`fixtures/python-uv`:
- `pyproject.toml`: `[project]`, name `"uv-app"`, `requires-python = ">=3.12"`, dependencies `["fastapi>=0.115", "uvicorn>=0.34", "psycopg2>=2.9"]`;
- `uv.lock`: `version = 1\nrequires-python = ">=3.12"\n`;
- `.python-version`: `3.12`;
- `app.py`: `from fastapi import FastAPI\n\napp = FastAPI()\n`.

`fixtures/python-poetry`:
- `pyproject.toml` with `[tool.poetry]` (`name = "poetry-app"`, `version = "0.1.0"`, `package-mode = false`) and `[tool.poetry.dependencies]` (`python = "^3.11"`, `flask = "^3.1"`, `gunicorn = "^23.0"`);
- `poetry.lock`: `# minimal\n`;
- `wsgi.py`: `from flask import Flask\n\napp = Flask(__name__)\n`.

- [ ] **Step 1:** Create the fixtures, write `tests/python.rs`, and run it. Expected: fails with `NoProvider` or `Ambiguous`.
- [ ] **Step 2:** Write `python.rs` and register it. **Run** `cargo test -p potter-detect && cargo clippy -p potter-detect --all-targets -- -D warnings`. Expected: passes.
- [ ] **Step 3: Commit** `feat(potter-detect): Python provider`.

`crates/potter-detect/tests/python.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, PythonOptions};

#[test]
fn django_with_requirements() {
    let d = detect("python-django");
    let t = &d.target;
    assert_eq!(t.provider, "python");
    assert_eq!(layers(&t.build.base), ["os-min", "python@3.13"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "collectstatic"]);
    assert_eq!(includes(t, "deps-inputs"), ["requirements.txt"]);
    assert_eq!(
        command(t, "deps"),
        "python -m venv /app/.venv && /app/.venv/bin/pip install --no-input -r requirements.txt"
    );
    assert_eq!(command(t, "collectstatic"), "/app/.venv/bin/python manage.py collectstatic --noinput");
    assert_eq!(process(t, "web"), "/app/.venv/bin/gunicorn config.wsgi --bind 0.0.0.0:8000");
    assert_eq!(t.image.config.port, Some(8000));
    assert_eq!(t.image.config.env["VIRTUAL_ENV"], "/app/.venv");
    assert_eq!(layers(&t.image.base), ["os-min", "python@3.13"]);
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn collectstatic_can_be_disabled() {
    let opts = DetectOptions { python: PythonOptions { collectstatic: Some(false), ..Default::default() }, ..Default::default() };
    let t = detect_at(&fixture("python-django"), &opts).unwrap().target;
    assert!(!step_names(&t).contains(&"collectstatic"));
}

#[test]
fn fastapi_reexported_app() {
    let t = detect("python-fastapi").target;
    assert_eq!(process(&t, "web"), "/app/.venv/bin/uvicorn main:app --host 0.0.0.0 --port 8000");
}

#[test]
fn flask_in_a_package() {
    let t = detect("python-flask").target;
    assert_eq!(process(&t, "web"), "/app/.venv/bin/gunicorn app.main:app --bind 0.0.0.0:8000");
}

#[test]
fn uv_project_with_native_deps() {
    let d = detect("python-uv");
    let t = &d.target;
    assert_eq!(layers(&t.build.base), ["os-min", "python@3.12", "uv"]);
    assert_eq!(step_names(t), ["apt-detected", "deps-inputs", "deps", "source", "project"]);
    assert_eq!(includes(t, "deps-inputs"), ["pyproject.toml", "uv.lock"]);
    assert_eq!(command(t, "deps"), "uv sync --frozen --no-dev --no-install-project");
    assert_eq!(command(t, "project"), "uv sync --frozen --no-dev");
    assert!(matches!(&t.image.runtime_steps[0], potter_plan::Step::Apt { packages, .. } if packages == &["libpq5"]));
    assert_eq!(process(t, "web"), "/app/.venv/bin/uvicorn app:app --host 0.0.0.0 --port 8000");
    assert_eq!(decision(&d, "runtime.python").value, "3.12");
}

#[test]
fn poetry_project() {
    let d = detect("python-poetry");
    let t = &d.target;
    assert_eq!(layers(&t.build.base), ["os-min", "python@3.14", "poetry"]);
    assert_eq!(command(t, "deps"), "poetry install --only main --no-root --no-interaction");
    assert_eq!(process(t, "web"), "/app/.venv/bin/gunicorn wsgi:app --bind 0.0.0.0:8000");
    assert!(decision(&d, "runtime.python").origin.to_string().contains("tool.poetry.dependencies.python"));
}

#[test]
fn requirement_includes_become_inputs() {
    let r = temp_repo(&[
        ("requirements.txt", "-r requirements/base.txt\nflask==3.1\n"),
        ("requirements/base.txt", "gunicorn==23.0.0  # server\n-c constraints.txt\n"),
        ("requirements/constraints.txt", "werkzeug<4\n"),
        ("app.py", "from flask import Flask\napp = Flask(__name__)\n"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(includes(&t, "deps-inputs"), ["requirements.txt", "requirements/base.txt", "requirements/constraints.txt"]);
    assert_eq!(process(&t, "web"), "/app/.venv/bin/gunicorn app:app --bind 0.0.0.0:8000");
}

#[test]
fn escaping_include_is_refused() {
    let r = temp_repo(&[("requirements.txt", "-r ../../etc/passwd\n")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::Parse { .. })));
}

#[test]
fn missing_lock_and_missing_server() {
    let r = temp_repo(&[("pyproject.toml", "[project]\nname = \"x\"\n")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::MissingFile { .. })));
    let r = temp_repo(&[("requirements.txt", "fastapi\n"), ("main.py", "app = FastAPI()\n")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(e.to_string().contains("uvicorn or granian"), "{e}");
}

#[test]
fn version_sources() {
    let r = temp_repo(&[("requirements.txt", "flask\ngunicorn\n"), ("app.py", "app = Flask(__name__)\n"), ("runtime.txt", "python-3.12.7\n")]);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.python").value, "3.12");
    let r = temp_repo(&[("requirements.txt", "flask\ngunicorn\n"), ("app.py", "app = Flask(__name__)\n"), (".python-version", "3.10\n")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(e.to_string().contains("available: 3.12, 3.13, 3.14"), "{e}");
}
```

`crates/potter-detect/src/providers/python.rs`:

```rust
//! Python applications (spec §6.1 "Python"). Dependencies go into a virtualenv at /app/.venv,
//! which ships in the image; processes call its binaries by absolute path.

use std::collections::BTreeSet;

use potter_plan::{Command, DEFAULT_APT_SNAPSHOT, Decision, ImageConfig, Origin, Process, Step};

use crate::catalog::{self, Line, PYTHON, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::{LOCKFILE_CAP, READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, parse_pep440, parse_poetry_constraint, parse_version_file, release_prefix};
use crate::{DetectError, ProviderId};

const VENV: &str = "/app/.venv";
const MARKERS: &[&str] = &["pyproject.toml", "requirements.txt", "Pipfile", "uv.lock", "poetry.lock"];
const APP_FILES: &[&str] =
    &["main.py", "app.py", "app/main.py", "src/main.py", "application.py", "server.py", "wsgi.py", "asgi.py"];
/// Python packages that need system libraries: (package, build packages, runtime packages).
const APT_TABLE: &[(&str, &[&str], &[&str])] = &[
    ("psycopg2", &["libpq-dev"], &["libpq5"]),
    ("mysqlclient", &["default-libmysqlclient-dev", "pkg-config"], &["libmariadb3"]),
];
const MAX_INCLUDE_DEPTH: usize = 5;

pub(crate) struct PythonProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pm {
    Uv,
    Poetry,
    Pipenv,
    Pip,
}

impl Pm {
    fn name(self) -> &'static str {
        match self {
            Pm::Uv => "uv",
            Pm::Poetry => "poetry",
            Pm::Pipenv => "pipenv",
            Pm::Pip => "pip",
        }
    }

    fn lockfile(self) -> &'static str {
        match self {
            Pm::Uv => "uv.lock",
            Pm::Poetry => "poetry.lock",
            Pm::Pipenv => "Pipfile.lock",
            Pm::Pip => "requirements.txt",
        }
    }

    fn parse(s: &str) -> Option<Pm> {
        [Pm::Uv, Pm::Poetry, Pm::Pipenv, Pm::Pip].into_iter().find(|p| p.name() == s)
    }
}

impl Provider for PythonProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Python
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(MARKERS.iter().find(|m| repo.is_file(m)).map(|m| Claim {
            provider: ProviderId::Python,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec![m.to_string()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let mut d = Vec::new();
        let pm = package_manager(repo, opts, &mut d)?;
        let pyproject = repo.read_toml("pyproject.toml")?;
        let python = python_version(repo, opts, pyproject.as_ref(), &mut d)?;
        let deps = dependencies(repo, pm, pyproject.as_ref())?;

        let mut build_layers = vec![cref("os-min", None), cref("python", Some(python.version))];
        if pm != Pm::Pip {
            build_layers.push(cref(pm.name(), None));
        }
        let mut steps = Vec::new();
        let (apt_build, apt_runtime) = apt_packages(&deps.names);
        if !apt_build.is_empty() {
            d.push(Decision::detected("apt.build", apt_build.join(" "), ["Python packages that need system libraries"]));
            steps.push(Step::Apt {
                name: "apt-detected".into(),
                packages: apt_build,
                snapshot: DEFAULT_APT_SNAPSHOT.into(),
            });
        }
        let (env, cache) = pm_env(pm);
        steps.push(inputs("deps-inputs", deps.inputs.clone()));
        steps.push(run("deps", install_command(pm), &env, &cache));
        steps.push(source());
        match pm {
            Pm::Uv => steps.push(run("project", Command::argv(&["uv", "sync", "--frozen", "--no-dev"]), &env, &cache)),
            Pm::Poetry => steps.push(run(
                "project",
                Command::shell("poetry install --only main --no-interaction"),
                &env,
                &cache,
            )),
            Pm::Pipenv | Pm::Pip => {}
        }

        let port = port(opts, 8000, &mut d);
        let web = web_process(repo, &deps.names, port, opts, &mut d)?;
        if let Some(project) = &web.django_project {
            let enabled = opts.python.collectstatic.unwrap_or(true);
            if enabled {
                steps.push(run(
                    "collectstatic",
                    Command::argv(&[&format!("{VENV}/bin/python"), "manage.py", "collectstatic", "--noinput"]),
                    &[],
                    &[],
                ));
                d.push(Decision::detected("steps.collectstatic", "enabled", [format!("Django project {project}")]));
            } else {
                d.push(Decision::config(
                    "steps.collectstatic",
                    "disabled",
                    opts.key("python.django.collectstatic"),
                ));
            }
        }

        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("PYTHONUNBUFFERED".into(), "1".into());
        config.env.insert("VIRTUAL_ENV".into(), VENV.into());
        config.env.insert("PATH".into(), format!("{VENV}/bin:/usr/local/bin:/usr/bin:/bin"));
        let run_layers = vec![cref("os-min", None), cref("python", Some(python.version))];
        let mut image = empty_image(run_layers, config);
        if !apt_runtime.is_empty() {
            image.runtime_steps.push(Step::Apt {
                name: "apt-detected-runtime".into(),
                packages: apt_runtime,
                snapshot: DEFAULT_APT_SNAPSHOT.into(),
            });
        }
        image.export = vec![export("/app", "/app")];
        image.processes.insert("web".into(), Process { command: web.command, workdir: None, env: Default::default() });
        Ok(Detection { target: new_target(target, ProviderId::Python, build_layers, steps, image), decisions: d })
    }
}

fn package_manager(repo: &RepoView, opts: &DetectOptions, d: &mut Vec<Decision>) -> Result<Pm, DetectError> {
    let all = [Pm::Uv, Pm::Poetry, Pm::Pipenv, Pm::Pip];
    if let Some(name) = &opts.python.package_manager {
        let key = opts.key("python.package_manager");
        let pm = Pm::parse(name).ok_or_else(|| DetectError::NeedsConfig {
            provider: ProviderId::Python,
            message: format!("unknown package manager `{name}` (uv, poetry, pipenv or pip)"),
            key: key.clone(),
        })?;
        if !repo.is_file(pm.lockfile()) {
            return Err(DetectError::MissingFile {
                provider: ProviderId::Python,
                file: pm.lockfile().into(),
                hint: format!("{name} is the package manager but {} is missing", pm.lockfile()),
            });
        }
        d.push(Decision::config("python.package_manager", pm.name(), key));
        return Ok(pm);
    }
    let present: Vec<Pm> = all.into_iter().filter(|p| repo.is_file(p.lockfile())).collect();
    let Some(&chosen) = present.first() else {
        return Err(DetectError::MissingFile {
            provider: ProviderId::Python,
            file: "a lockfile".into(),
            hint: "commit uv.lock, poetry.lock, Pipfile.lock or requirements.txt so builds are reproducible".into(),
        });
    };
    let evidence = if present.len() > 1 {
        let files: Vec<&str> = present.iter().map(|p| p.lockfile()).collect();
        format!(
            "several lockfiles ({}); chose {}; set {} to change",
            files.join(", "),
            chosen.name(),
            opts.key("python.package_manager")
        )
    } else {
        format!("{} is present", chosen.lockfile())
    };
    d.push(Decision::detected("python.package_manager", chosen.name(), [evidence]));
    Ok(chosen)
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false }
}

fn python_version(
    repo: &RepoView,
    opts: &DetectOptions,
    pyproject: Option<&toml::Value>,
    d: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.python.as_deref(), &opts.key("runtime.python"), "python")?;
    if src.is_none() {
        src = version_from_file(repo, ".python-version", "python")?;
    }
    if src.is_none()
        && let Some(spec) = pyproject.and_then(|p| p.get("project")?.get("requires-python")?.as_str())
    {
        let desc = format!("pyproject.toml project.requires-python = {spec}");
        let set = parse_pep440(spec).map_err(|e| bad_constraint("python", spec, &desc, e))?;
        src = Some(detected(desc, set));
    }
    if src.is_none()
        && let Some(spec) =
            pyproject.and_then(|p| p.get("tool")?.get("poetry")?.get("dependencies")?.get("python")?.as_str())
    {
        let desc = format!("pyproject.toml tool.poetry.dependencies.python = {spec}");
        let set = parse_poetry_constraint(spec).map_err(|e| bad_constraint("python", spec, &desc, e))?;
        src = Some(detected(desc, set));
    }
    if src.is_none()
        && let Some(text) = repo.read_string("runtime.txt", READ_CAP)?
    {
        let raw = text.trim();
        let version = raw.strip_prefix("python-").unwrap_or(raw);
        let desc = format!("runtime.txt = {raw}");
        let (set, _) = parse_version_file(version).map_err(|e| bad_constraint("python", raw, "runtime.txt", e))?;
        src = Some(detected(desc, set));
    }
    catalog::pick(&PYTHON, "runtime.python", src, d)
}

struct Deps {
    /// Normalised distribution names (lower case, `-` separators).
    names: BTreeSet<String>,
    /// Files the dependency install reads.
    inputs: Vec<String>,
}

fn normalize(name: &str) -> String {
    name.to_ascii_lowercase().replace(['_', '.'], "-")
}

/// The distribution name at the start of a PEP 508 requirement, if it is one.
fn requirement_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['#', '-', '.', '/']) || line.contains("://") {
        return None;
    }
    let end = line.find(|c: char| !(c.is_ascii_alphanumeric() || "._-".contains(c))).unwrap_or(line.len());
    (end > 0).then(|| normalize(&line[..end]))
}

fn add_requirement(line: &str, names: &mut BTreeSet<String>) {
    if let Some(name) = requirement_name(line) {
        // `fastapi[standard]` brings uvicorn with it.
        if name == "fastapi" && line.contains("standard") {
            names.insert("uvicorn".into());
        }
        names.insert(name);
    }
}

fn dependencies(repo: &RepoView, pm: Pm, pyproject: Option<&toml::Value>) -> Result<Deps, DetectError> {
    let mut deps = Deps { names: BTreeSet::new(), inputs: Vec::new() };
    if let Some(p) = pyproject {
        if let Some(list) = p.get("project").and_then(|x| x.get("dependencies")).and_then(|x| x.as_array()) {
            for dep in list.iter().filter_map(|v| v.as_str()) {
                add_requirement(dep, &mut deps.names);
            }
        }
        if let Some(table) =
            p.get("tool").and_then(|x| x.get("poetry")).and_then(|x| x.get("dependencies")).and_then(|x| x.as_table())
        {
            deps.names.extend(table.keys().filter(|k| *k != "python").map(|k| normalize(k)));
        }
    }
    match pm {
        Pm::Uv => deps.inputs = vec!["pyproject.toml".into(), "uv.lock".into()],
        Pm::Poetry => {
            deps.inputs = vec!["pyproject.toml".into(), "poetry.lock".into()];
            if repo.is_file("poetry.toml") {
                deps.inputs.push("poetry.toml".into());
            }
        }
        Pm::Pipenv => {
            deps.inputs = vec!["Pipfile".into(), "Pipfile.lock".into()];
            if let Some(lock) = repo.read_json_capped("Pipfile.lock", LOCKFILE_CAP)?
                && let Some(default) = lock.get("default").and_then(|v| v.as_object())
            {
                deps.names.extend(default.keys().map(|k| normalize(k)));
            }
        }
        Pm::Pip => read_requirements(repo, "requirements.txt", 0, &mut deps)?,
    }
    Ok(deps)
}

/// Reads a requirements file and the files it includes with `-r`/`-c`, which the install also needs.
fn read_requirements(repo: &RepoView, rel: &str, depth: usize, deps: &mut Deps) -> Result<(), DetectError> {
    if depth > MAX_INCLUDE_DEPTH || deps.inputs.iter().any(|i| i == rel) {
        return Ok(());
    }
    let Some(text) = repo.read_string(rel, READ_CAP)? else {
        return Err(DetectError::MissingFile {
            provider: ProviderId::Python,
            file: rel.into(),
            hint: "it is referenced by another requirements file".into(),
        });
    };
    deps.inputs.push(rel.to_string());
    for raw in text.lines() {
        let line = raw.split(" #").next().unwrap_or("").trim();
        let include = ["-r", "--requirement", "-c", "--constraint"]
            .iter()
            .find_map(|flag| line.strip_prefix(flag).filter(|r| r.starts_with([' ', '='])).map(|r| r[1..].trim()));
        match include {
            Some(path) => {
                let dir = rel.rsplit_once('/').map(|(d, _)| d);
                let joined = match dir {
                    Some(d) => format!("{d}/{path}"),
                    None => path.to_string(),
                };
                if !potter_plan::is_relative_path(&joined) || joined.split('/').any(|c| c == "." || c.is_empty()) {
                    return Err(DetectError::Parse {
                        file: rel.into(),
                        reason: format!("includes `{path}`, which is not a plain path inside the build root"),
                    });
                }
                read_requirements(repo, &joined, depth + 1, deps)?;
            }
            None => add_requirement(line, &mut deps.names),
        }
    }
    Ok(())
}

fn apt_packages(names: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let mut build = Vec::new();
    let mut runtime = Vec::new();
    for (package, b, r) in APT_TABLE {
        if names.contains(*package) {
            build.extend(b.iter().map(|s| s.to_string()));
            runtime.extend(r.iter().map(|s| s.to_string()));
        }
    }
    (build, runtime)
}

fn pm_env(pm: Pm) -> (Vec<(&'static str, &'static str)>, Vec<(&'static str, &'static str)>) {
    match pm {
        Pm::Uv => (
            vec![
                ("UV_PROJECT_ENVIRONMENT", VENV),
                ("UV_CACHE_DIR", "/cache/uv"),
                ("UV_PYTHON_DOWNLOADS", "never"),
                ("UV_LINK_MODE", "copy"),
            ],
            vec![("uv", "/cache/uv")],
        ),
        Pm::Poetry => (
            vec![("POETRY_VIRTUALENVS_IN_PROJECT", "true"), ("POETRY_CACHE_DIR", "/cache/poetry")],
            vec![("poetry", "/cache/poetry")],
        ),
        Pm::Pipenv => (
            vec![("PIPENV_VENV_IN_PROJECT", "1"), ("PIP_CACHE_DIR", "/cache/pip")],
            vec![("pip", "/cache/pip")],
        ),
        Pm::Pip => (vec![("PIP_CACHE_DIR", "/cache/pip")], vec![("pip", "/cache/pip")]),
    }
}

fn install_command(pm: Pm) -> Command {
    match pm {
        Pm::Uv => Command::argv(&["uv", "sync", "--frozen", "--no-dev", "--no-install-project"]),
        Pm::Poetry => Command::shell("poetry install --only main --no-root --no-interaction"),
        Pm::Pipenv => Command::shell("pipenv sync"),
        Pm::Pip => Command::shell("python -m venv /app/.venv && /app/.venv/bin/pip install --no-input -r requirements.txt"),
    }
}

struct Web {
    command: Vec<String>,
    django_project: Option<String>,
}

/// A file defines or re-exports a module-level `app`.
fn binds_app(text: &str) -> bool {
    text.lines().map(str::trim_start).any(|l| {
        let assigns = l.strip_prefix("app").is_some_and(|r| r.trim_start().starts_with(['=', ':']));
        let imports = l.starts_with("from ")
            && l.split_once(" import ").is_some_and(|(_, names)| {
                names.split(',').any(|n| n.split_whitespace().last() == Some("app"))
            });
        assigns || imports
    })
}

fn app_module(repo: &RepoView) -> Result<Option<(String, &'static str)>, DetectError> {
    for f in APP_FILES {
        if let Some(text) = repo.read_string(f, READ_CAP)?
            && binds_app(&text)
        {
            return Ok(Some((f.trim_end_matches(".py").replace('/', "."), f)));
        }
    }
    Ok(None)
}

fn django_project(repo: &RepoView) -> Result<Option<String>, DetectError> {
    let Some(text) = repo.read_string("manage.py", READ_CAP)? else { return Ok(None) };
    let module = quoted_value_after(&text, "DJANGO_SETTINGS_MODULE", ',');
    let project = module.as_deref().and_then(|m| m.split('.').next()).filter(|p| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    });
    Ok(project.map(str::to_string))
}

fn bin(name: &str) -> String {
    format!("{VENV}/bin/{name}")
}

fn web_process(
    repo: &RepoView,
    names: &BTreeSet<String>,
    port: u16,
    opts: &DetectOptions,
    d: &mut Vec<Decision>,
) -> Result<Web, DetectError> {
    let has = |n: &str| names.contains(n);
    let p = port.to_string();
    let no_process = |why: &str| DetectError::NoProcess {
        provider: ProviderId::Python,
        hint: format!("{why}, or set `{}` in potter.toml", opts.key("process.web.cmd")),
    };
    if has("django") && repo.is_file("manage.py") {
        let project = django_project(repo)?.ok_or_else(|| no_process("manage.py doesn't name DJANGO_SETTINGS_MODULE"))?;
        let command = if has("gunicorn") {
            vec![bin("gunicorn"), format!("{project}.wsgi"), "--bind".into(), format!("0.0.0.0:{p}")]
        } else if has("uvicorn") {
            vec![bin("uvicorn"), format!("{project}.asgi:application"), "--host".into(), "0.0.0.0".into(), "--port".into(), p]
        } else {
            return Err(no_process("add gunicorn or uvicorn to the dependencies"));
        };
        d.push(Decision::detected("process.web", command.join(" "), [format!("Django project {project}")]));
        return Ok(Web { command, django_project: Some(project) });
    }
    let asgi = ["fastapi", "starlette", "litestar", "quart"].iter().any(|n| has(n));
    let wsgi = ["flask", "falcon", "bottle"].iter().any(|n| has(n));
    if !(asgi || wsgi) {
        return Err(no_process("no Django, FastAPI, Starlette or Flask dependency was found"));
    }
    let (module, file) = app_module(repo)?.ok_or_else(|| no_process("no module defining `app` was found"))?;
    let target = format!("{module}:app");
    let command = match (asgi, has("uvicorn"), has("granian"), has("gunicorn")) {
        (true, true, _, _) => {
            vec![bin("uvicorn"), target, "--host".into(), "0.0.0.0".into(), "--port".into(), p]
        }
        (_, _, true, _) => vec![
            bin("granian"),
            "--interface".into(),
            if asgi { "asgi" } else { "wsgi" }.into(),
            "--host".into(),
            "0.0.0.0".into(),
            "--port".into(),
            p,
            target,
        ],
        (false, _, _, true) => vec![bin("gunicorn"), target, "--bind".into(), format!("0.0.0.0:{p}")],
        _ => {
            let servers = if asgi { "uvicorn or granian" } else { "gunicorn or granian" };
            return Err(no_process(&format!("add {servers} to the dependencies")));
        }
    };
    d.push(Decision::detected("process.web", command.join(" "), [format!("{file} defines `app`")]));
    Ok(Web { command, django_project: None })
}
```

---

### Task 4: The Ruby provider

**Files:**
- Create: `crates/potter-detect/src/providers/ruby.rs` and `crates/potter-detect/tests/ruby.rs`.
- Modify: `providers/mod.rs` (register).
- Fixtures:
  - `fixtures/ruby-rails`: `Gemfile`, `Gemfile.lock`, `.ruby-version` and `config.ru` copied from `rails-cloud-app`;
  - `fixtures/ruby-sinatra`: synthetic (contents below).

`fixtures/ruby-sinatra`:
- `Gemfile`: `source "https://rubygems.org"\ngem "sinatra"\ngem "puma"\ngem "pg"\ngem "sidekiq"\n`;
- `Gemfile.lock`: a `GEM` / `specs:` block listing `sinatra (4.1.0)`, `puma (6.5.0)`, `pg (1.5.9)` and `sidekiq (7.3.0)`, followed by `RUBY VERSION\n   ruby 3.3.6p108\n`;
- `config.ru`: `require "sinatra"\nrun Sinatra::Application\n`.

- [ ] **Step 1:** Fixtures and tests. Run them. Expected: they fail.
- [ ] **Step 2:** Implement and register. Run the tests and clippy. Expected: they pass.
- [ ] **Step 3: Commit** `feat(potter-detect): Ruby provider`.

`crates/potter-detect/tests/ruby.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn rails_with_propshaft_and_bootsnap() {
    let d = detect("ruby-rails");
    let t = &d.target;
    assert_eq!(t.provider, "ruby");
    assert_eq!(decision(&d, "runtime.ruby").value, "3.3");
    assert_eq!(layers(&t.build.base), ["os-min", "ruby@3.3"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "bootsnap", "assets"]);
    assert_eq!(command(t, "deps"), "bundle install");
    assert_eq!(command(t, "assets"), "bin/rails assets:precompile");
    assert_eq!(process(t, "web"), "bin/rails server -b 0.0.0.0");
    assert_eq!(t.image.config.env["RAILS_ENV"], "production");
    assert_eq!(t.image.config.env["BUNDLE_PATH"], "/app/vendor/bundle");
    assert_eq!(t.image.config.port, Some(3000));
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn sinatra_with_pg() {
    let d = detect("ruby-sinatra");
    let t = &d.target;
    assert_eq!(decision(&d, "runtime.ruby").value, "3.3");
    assert_eq!(step_names(t), ["apt-detected", "deps-inputs", "deps", "source"]);
    assert_eq!(process(t, "web"), "bundle exec rackup -o 0.0.0.0 -p 3000");
    assert!(matches!(&t.image.runtime_steps[0], potter_plan::Step::Apt { packages, .. } if packages == &["libpq5"]));
    assert_eq!(process(t, "worker"), "bundle exec sidekiq");
}

#[test]
fn gemfile_ruby_requirement() {
    let lock = "GEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.0)\n";
    let r = temp_repo(&[("Gemfile", "source \"https://rubygems.org\"\nruby \"~> 3.4\"\ngem \"rack\"\n"), ("Gemfile.lock", lock), ("config.ru", "run ->(_) { [200, {}, []] }\n")]);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert_eq!(decision(&d, "runtime.ruby").value, "3.4");
}

#[test]
fn rails_with_jsbundling_installs_node_first() {
    let lock = "GEM\n  specs:\n    rails (8.1.0)\n    jsbundling-rails (1.3.0)\n";
    let r = temp_repo(&[
        ("Gemfile", "gem \"rails\"\n"),
        ("Gemfile.lock", lock),
        ("package.json", r#"{"scripts":{"build":"esbuild app/javascript/*.*"}}"#),
        ("yarn.lock", "# yarn lockfile v1\n"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(step_names(&t), ["deps-inputs", "deps", "node-deps-inputs", "node-deps", "source", "assets"]);
    assert!(layers(&t.build.base).iter().any(|l| l.starts_with("node@")));
    assert!(!layers(&t.image.base).iter().any(|l| l.starts_with("node@")));
}

#[test]
fn missing_lockfile_and_no_process() {
    let r = temp_repo(&[("Gemfile", "gem \"rake\"\n")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::MissingFile { .. })));
    let r = temp_repo(&[("Gemfile", "gem \"rake\"\n"), ("Gemfile.lock", "GEM\n  specs:\n    rake (13.0.0)\n")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NoProcess { .. })));
}
```

`crates/potter-detect/src/providers/ruby.rs`:

```rust
//! Ruby applications (spec §6.1 "Ruby"). Gems install into /app/vendor/bundle in deployment
//! mode; Rails gets asset precompilation and bootsnap.

use std::collections::BTreeSet;

use potter_plan::{Command, DEFAULT_APT_SNAPSHOT, Decision, ImageConfig, Origin, Process, Step};

use crate::catalog::{self, Line, RUBY, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson};
use crate::providers::{Detection, Provider};
use crate::repo::{LOCKFILE_CAP, READ_CAP, RepoView};
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, parse_gem_requirement, parse_partial, parse_version_file, release_prefix};
use crate::{DetectError, ProviderId};

const BUNDLE_ENV: &[(&str, &str)] = &[
    ("BUNDLE_DEPLOYMENT", "1"),
    ("BUNDLE_WITHOUT", "development:test"),
    ("BUNDLE_PATH", "/app/vendor/bundle"),
];
const APT_TABLE: &[(&str, &[&str], &[&str])] = &[
    ("pg", &["libpq-dev"], &["libpq5"]),
    ("mysql2", &["default-libmysqlclient-dev"], &["libmariadb3"]),
];
const ASSET_GEMS: &[&str] = &["propshaft", "sprockets", "jsbundling-rails", "cssbundling-rails", "tailwindcss-rails"];

pub(crate) struct RubyProvider;

impl Provider for RubyProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Ruby
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(repo.is_file("Gemfile").then(|| Claim {
            provider: ProviderId::Ruby,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec!["Gemfile".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let lock = repo.read_string("Gemfile.lock", LOCKFILE_CAP)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Ruby,
            file: "Gemfile.lock".into(),
            hint: "run `bundle lock` and commit Gemfile.lock so builds are reproducible".into(),
        })?;
        let gemfile = repo.read_string("Gemfile", READ_CAP)?.unwrap_or_default();
        let gems = locked_gems(&lock);
        let mut d = Vec::new();
        let ruby = ruby_version(repo, opts, &lock, &gemfile, &mut d)?;
        let rails = gems.contains("rails") || gems.contains("railties");

        let mut build_layers = vec![cref("os-min", None), cref("ruby", Some(ruby.version))];
        let mut steps = Vec::new();
        let (apt_build, apt_runtime) = apt_packages(&gems);
        if !apt_build.is_empty() {
            d.push(Decision::detected("apt.build", apt_build.join(" "), ["gems that need system libraries"]));
            steps.push(Step::Apt { name: "apt-detected".into(), packages: apt_build, snapshot: DEFAULT_APT_SNAPSHOT.into() });
        }
        let mut deps_inputs = vec!["Gemfile".to_string(), "Gemfile.lock".to_string()];
        deps_inputs.extend(repo.files().filter(|f| !f.contains('/') && f.ends_with(".gemspec")).map(str::to_string));
        let mut env: Vec<(&str, &str)> = BUNDLE_ENV.to_vec();
        env.push(("BUNDLE_USER_CACHE", "/cache/bundler"));
        steps.push(inputs("deps-inputs", deps_inputs));
        steps.push(run("deps", Command::argv(&["bundle", "install"]), &env, &[("bundler", "/cache/bundler")]));

        // jsbundling/cssbundling run their JS build inside assets:precompile, so node deps come first.
        if let Some(pkg) = PackageJson::read(repo)? {
            let pm = js::detect_pm(repo, &pkg, opts, ProviderId::Ruby, &mut d)?;
            let node = js::node_version(repo, &pkg, opts, &mut d)?;
            build_layers.push(cref("node", Some(node.version)));
            steps.extend(js::deps_steps(pm, repo, "node-"));
        }
        steps.push(source());
        if gems.contains("bootsnap") {
            steps.push(run("bootsnap", Command::argv(&["bundle", "exec", "bootsnap", "precompile", "app/", "lib/"]), BUNDLE_ENV, &[]));
        }
        if rails && ASSET_GEMS.iter().any(|g| gems.contains(*g)) {
            let mut assets_env = BUNDLE_ENV.to_vec();
            assets_env.push(("RAILS_ENV", "production"));
            assets_env.push(("SECRET_KEY_BASE_DUMMY", "1"));
            steps.push(run("assets", Command::argv(&["bin/rails", "assets:precompile"]), &assets_env, &[]));
        }

        let port = port(opts, 3000, &mut d);
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        for (k, v) in BUNDLE_ENV {
            config.env.insert(k.to_string(), v.to_string());
        }
        config.env.insert("RACK_ENV".into(), "production".into());
        if rails {
            config.env.insert("RAILS_ENV".into(), "production".into());
        }
        let mut image = empty_image(vec![cref("os-min", None), cref("ruby", Some(ruby.version))], config);
        if !apt_runtime.is_empty() {
            image.runtime_steps.push(Step::Apt {
                name: "apt-detected-runtime".into(),
                packages: apt_runtime,
                snapshot: DEFAULT_APT_SNAPSHOT.into(),
            });
        }
        image.export = vec![potter_plan::Export { path: "/app".into(), dest: "/app".into(), exclude: vec!["node_modules".into()] }];
        let web: Vec<String> = if rails {
            d.push(Decision::detected("process.web", "bin/rails server", ["Gemfile.lock includes rails"]));
            ["bin/rails", "server", "-b", "0.0.0.0"].map(String::from).to_vec()
        } else if repo.is_file("config.ru") {
            d.push(Decision::detected("process.web", "rackup", ["config.ru exists"]));
            vec!["bundle".into(), "exec".into(), "rackup".into(), "-o".into(), "0.0.0.0".into(), "-p".into(), port.to_string()]
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Ruby,
                hint: format!("add a config.ru, or set `{}` in potter.toml", opts.key("process.web.cmd")),
            });
        };
        image.processes.insert("web".into(), Process { command: web, workdir: None, env: Default::default() });
        if gems.contains("solid_queue") {
            image.processes.insert("worker".into(), Process::new(&["bin/jobs"]));
            d.push(Decision::detected("process.worker", "bin/jobs", ["Gemfile.lock includes solid_queue"]));
        } else if gems.contains("sidekiq") {
            image.processes.insert("worker".into(), Process::new(&["bundle", "exec", "sidekiq"]));
            d.push(Decision::detected("process.worker", "bundle exec sidekiq", ["Gemfile.lock includes sidekiq"]));
        }
        Ok(Detection { target: new_target(target, ProviderId::Ruby, build_layers, steps, image), decisions: d })
    }
}

/// Gem names from the `specs:` sections of Gemfile.lock (lines indented by exactly four spaces).
fn locked_gems(lock: &str) -> BTreeSet<String> {
    lock.lines()
        .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
        .filter_map(|l| l.trim().split(' ').next())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

fn apt_packages(gems: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let mut build = Vec::new();
    let mut runtime = Vec::new();
    for (gem, b, r) in APT_TABLE {
        if gems.contains(*gem) {
            build.extend(b.iter().map(|s| s.to_string()));
            runtime.extend(r.iter().map(|s| s.to_string()));
        }
    }
    (build, runtime)
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false }
}

fn ruby_version(
    repo: &RepoView,
    opts: &DetectOptions,
    lock: &str,
    gemfile: &str,
    d: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.ruby.as_deref(), &opts.key("runtime.ruby"), "ruby")?;
    if src.is_none()
        && let Some(text) = repo.read_string(".ruby-version", READ_CAP)?
    {
        let raw = text.trim();
        let version = raw.strip_prefix("ruby-").unwrap_or(raw);
        let (set, _) = parse_version_file(version).map_err(|e| bad_constraint("ruby", raw, ".ruby-version", e))?;
        src = Some(detected(format!(".ruby-version = {raw}"), set));
    }
    if src.is_none()
        && let Some(line) = lock.lines().skip_while(|l| l.trim() != "RUBY VERSION").nth(1)
    {
        let raw = line.trim().trim_start_matches("ruby").trim();
        let set = parse_partial(release_prefix(raw))
            .and_then(|p| p.x_range())
            .map_err(|e| bad_constraint("ruby", raw, "Gemfile.lock RUBY VERSION", e))?;
        src = Some(detected(format!("Gemfile.lock RUBY VERSION = {raw}"), set));
    }
    if src.is_none()
        && let Some(req) = gemfile_ruby(gemfile)
    {
        let set = parse_gem_requirement(&req).map_err(|e| bad_constraint("ruby", &req, "Gemfile ruby", e))?;
        src = Some(detected(format!("Gemfile ruby \"{req}\""), set));
    }
    catalog::pick(&RUBY, "runtime.ruby", src, d)
}

/// The requirement in a Gemfile `ruby "~> 3.3"` line; `ruby file: ".ruby-version"` yields nothing.
fn gemfile_ruby(gemfile: &str) -> Option<String> {
    gemfile.lines().map(str::trim).find_map(|l| {
        let rest = l.strip_prefix("ruby ").or_else(|| l.strip_prefix("ruby("))?.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = &rest[1..];
        body.find(quote).map(|end| body[..end].to_string())
    })
}
```

---

### Task 5: The Go provider

**Files:**
- Create: `crates/potter-detect/src/providers/go.rs` and `crates/potter-detect/tests/go.rs`.
- Modify: `providers/mod.rs`.
- Fixtures:
  - `fixtures/go-api`: `go.mod` and `main.go` copied from a separate Go demo app;
  - `fixtures/go-cmd`: synthetic (contents below).

`fixtures/go-cmd`:
- `go.mod`: `module example.com/multi\n\ngo 1.25\n\ntoolchain go1.25.3\n\nrequire github.com/google/uuid v1.6.0\n`;
- `go.sum`: one `github.com/google/uuid v1.6.0 h1:…` line;
- `cmd/api/main.go` and `cmd/worker/main.go`: `package main\n\nfunc main() {}\n`.

- [ ] **Step 1:** Fixtures and tests. Run them. Expected: they fail.
- [ ] **Step 2:** Implement and register. Run the tests and clippy. Expected: they pass.
- [ ] **Step 3: Commit** `feat(potter-detect): Go provider`.

`crates/potter-detect/tests/go.rs`:

```rust
mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn root_main_package() {
    let d = detect("go-api");
    let t = &d.target;
    assert_eq!(t.provider, "go");
    assert_eq!(layers(&t.build.base), ["os-min", "go@1.27"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "build"]);
    assert_eq!(includes(t, "deps-inputs"), ["go.mod"]);
    assert_eq!(command(t, "deps"), "go mod download");
    assert_eq!(command(t, "build"), "go build -trimpath -ldflags=\"-s -w\" -o /out/app .");
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
    assert_eq!(layers(&t.build.base), ["os-min", "go@1.25"]);
}

#[test]
fn ambiguous_mains_need_config() {
    let r = temp_repo(&[
        ("go.mod", "module x\n\ngo 1.26\n"),
        ("cmd/alpha/main.go", "package main\n"),
        ("cmd/beta/main.go", "package main\n"),
    ]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NeedsConfig { .. })));
}

#[test]
fn library_only_module_has_no_process() {
    let r = temp_repo(&[("go.mod", "module x\n\ngo 1.26\n"), ("lib.go", "package lib\n")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NoProcess { .. })));
}

#[test]
fn too_new_go_is_unsatisfiable() {
    let r = temp_repo(&[("go.mod", "module x\n\ngo 1.30\n"), ("main.go", "package main\n")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(e.to_string().contains("available: 1.25, 1.26, 1.27"), "{e}");
}
```

`crates/potter-detect/src/providers/go.rs`:

```rust
//! Go applications (spec §6.1 "Go"). Static binaries land in /out; the run image is os-min.
//! `GOTOOLCHAIN=local` keeps the go command from downloading a different toolchain mid-build.

use potter_plan::{Command, Decision, ImageConfig, Origin, Process};

use crate::catalog::{self, GO, Line, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, at_least, parse_partial, release_prefix};
use crate::{DetectError, ProviderId};

const WEB_NAMES: &[&str] = &["server", "api", "web", "app"];

pub(crate) struct GoProvider;

impl Provider for GoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Go
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(repo.is_file("go.mod").then(|| Claim {
            provider: ProviderId::Go,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec!["go.mod".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let gomod = repo.read_string("go.mod", READ_CAP)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Go,
            file: "go.mod".into(),
            hint: "the Go provider builds Go modules".into(),
        })?;
        let mut d = Vec::new();
        let go = go_version(opts, &gomod, &mut d)?;
        let mains = main_packages(repo)?;
        if mains.is_empty() {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Go,
                hint: format!(
                    "add a main package (main.go or cmd/<name>/main.go), or set `{}` in potter.toml",
                    opts.key("process.web.cmd")
                ),
            });
        }

        let env = [("GOMODCACHE", "/cache/gomod"), ("GOCACHE", "/cache/gobuild"), ("GOTOOLCHAIN", "local")];
        let mut deps_inputs = vec!["go.mod".to_string()];
        if repo.is_file("go.sum") {
            deps_inputs.push("go.sum".into());
        }
        let build_script = mains
            .iter()
            .map(|(name, pkg)| format!("go build -trimpath -ldflags=\"-s -w\" -o /out/{name} {pkg}"))
            .collect::<Vec<_>>()
            .join(" && ");
        let mut build_env = env.to_vec();
        build_env.push(("CGO_ENABLED", "0"));
        let steps = vec![
            inputs("deps-inputs", deps_inputs),
            run("deps", Command::argv(&["go", "mod", "download"]), &env, &[("gomod", "/cache/gomod")]),
            source(),
            run(
                "build",
                Command::Shell(build_script),
                &build_env,
                &[("gomod", "/cache/gomod"), ("gobuild", "/cache/gobuild")],
            ),
        ];

        let web = pick_web(&mains, opts)?;
        d.push(Decision::detected("process.web", format!("/out/{web}"), [main_evidence(&mains, &web)]));
        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/out", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        let mut image = empty_image(vec![cref("os-min", None)], config);
        image.export = vec![export("/out", "/out")];
        for (name, _) in &mains {
            let process = if *name == web { "web".to_string() } else { name.clone() };
            if potter_plan::is_name(&process) {
                image.processes.insert(process, Process::new(&[&format!("/out/{name}")]));
            }
        }
        let layers = vec![cref("os-min", None), cref("go", Some(go.version))];
        Ok(Detection { target: new_target(target, ProviderId::Go, layers, steps, image), decisions: d })
    }
}

fn directive<'a>(gomod: &'a str, word: &str) -> Option<&'a str> {
    gomod.lines().find_map(|l| l.trim().strip_prefix(word)?.strip_prefix(' ').map(str::trim))
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false }
}

fn go_version(opts: &DetectOptions, gomod: &str, d: &mut Vec<Decision>) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.go.as_deref(), &opts.key("runtime.go"), "go")?;
    if src.is_none()
        && let Some(tc) = directive(gomod, "toolchain")
    {
        let raw = tc.strip_prefix("go").unwrap_or(tc);
        let set = parse_partial(release_prefix(raw))
            .and_then(|p| p.x_range())
            .map_err(|e| bad_constraint("go", tc, "go.mod toolchain", e))?;
        src = Some(detected(format!("go.mod toolchain {tc}"), set));
    }
    if src.is_none()
        && let Some(v) = directive(gomod, "go")
    {
        let set = at_least(v).map_err(|e| bad_constraint("go", v, "go.mod go", e))?;
        src = Some(detected(format!("go.mod go {v} (minimum)"), set));
    }
    catalog::pick(&GO, "runtime.go", src, d)
}

fn is_main(repo: &RepoView, file: &str) -> Result<bool, DetectError> {
    Ok(repo.read_string(file, READ_CAP)?.is_some_and(|t| t.lines().any(|l| l.trim() == "package main")))
}

fn go_files_in<'a>(repo: &'a RepoView, dir: &'a str) -> impl Iterator<Item = &'a str> {
    repo.files().filter(move |f| {
        let rest = if dir.is_empty() { Some(*f) } else { f.strip_prefix(dir).and_then(|r| r.strip_prefix('/')) };
        rest.is_some_and(|r| !r.contains('/') && r.ends_with(".go") && !r.ends_with("_test.go"))
    })
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// (binary name, package) for the root package and each `cmd/<name>` that is `package main`.
fn main_packages(repo: &RepoView) -> Result<Vec<(String, String)>, DetectError> {
    let mut mains = Vec::new();
    for f in go_files_in(repo, "").collect::<Vec<_>>() {
        if is_main(repo, f)? {
            mains.push(("app".to_string(), ".".to_string()));
            break;
        }
    }
    let mut cmds: Vec<&str> = repo
        .files()
        .filter_map(|f| f.strip_prefix("cmd/")?.split_once('/').map(|(dir, _)| dir))
        .collect();
    cmds.dedup();
    for name in cmds {
        if !safe_name(name) {
            continue;
        }
        let dir = format!("cmd/{name}");
        for f in go_files_in(repo, &dir).collect::<Vec<_>>() {
            if is_main(repo, f)? {
                mains.push((name.to_string(), format!("./{dir}")));
                break;
            }
        }
    }
    Ok(mains)
}

fn pick_web(mains: &[(String, String)], opts: &DetectOptions) -> Result<String, DetectError> {
    if let [(only, _)] = mains {
        return Ok(only.clone());
    }
    WEB_NAMES
        .iter()
        .find_map(|w| mains.iter().find(|(n, _)| n == w).map(|(n, _)| n.clone()))
        .ok_or_else(|| DetectError::NeedsConfig {
            provider: ProviderId::Go,
            message: format!(
                "several main packages ({}) and none is named server, api, web or app",
                mains.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
            ),
            key: opts.key("process.web.cmd"),
        })
}

fn main_evidence(mains: &[(String, String)], web: &str) -> String {
    let pkg = mains.iter().find(|(n, _)| n == web).map(|(_, p)| p.as_str()).unwrap_or(".");
    if pkg == "." { "main.go is package main".into() } else { format!("{pkg} is package main") }
}
```

---

### Task 6: The Rust provider

**Files:**
- Create: `crates/potter-detect/src/providers/rust.rs` and `crates/potter-detect/tests/rust.rs`.
- Modify: `providers/mod.rs`.
- Fixtures:
  - `fixtures/rust-axum`: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `src/main.rs` and `src/lib.rs` copied from `axum-hello-world`;
  - `fixtures/rust-workspace`: synthetic (contents below).

`fixtures/rust-workspace`:
- the root `Cargo.toml` contains only `[workspace]\nmembers = ["crates/*"]\nresolver = "3"\n`;
- `Cargo.lock`: `version = 4\n`;
- `crates/api`: a `[package] name = "api"` manifest and `src/main.rs`;
- `crates/core`: a `name = "core"` manifest, `src/lib.rs` and `build.rs`;
- `crates/worker`: a `name = "worker"` manifest and `src/main.rs`.

- [ ] **Step 1:** Fixtures and tests. Run them. Expected: they fail.
- [ ] **Step 2:** Implement and register. Run the tests and clippy. Expected: they pass.
- [ ] **Step 3: Commit** `feat(potter-detect): Rust provider`.

`crates/potter-detect/tests/rust.rs`:

```rust
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
    assert_eq!(includes(t, "deps-inputs"), ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]);
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
        ["Cargo.toml", "Cargo.lock", "crates/api/Cargo.toml", "crates/core/Cargo.toml", "crates/worker/Cargo.toml"]
    );
    let deps = command(&t, "deps");
    assert!(deps.contains("> crates/api/src/main.rs") && deps.contains("> crates/core/src/lib.rs") && deps.contains("> crates/core/build.rs"), "{deps}");
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
    assert!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.rust").origin.to_string().contains("rust-version"));
    let r = temp_repo(&[base[0], base[1], base[2], ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.97.0\"\n")]);
    assert_eq!(decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.rust").value, "1.97");
    let r = temp_repo(&[base[0], base[1], base[2], ("rust-toolchain", "nightly-2026-09-01\n")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NeedsConfig { .. })));
}

#[test]
fn hostile_bin_path_is_refused() {
    let r = temp_repo(&[
        ("Cargo.toml", "[package]\nname = \"x\"\n[[bin]]\nname = \"x\"\npath = \"src/$(id).rs\"\n"),
        ("Cargo.lock", "version = 4\n"),
    ]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::Parse { .. })));
}

#[test]
fn missing_lock_and_library_only() {
    let r = temp_repo(&[("Cargo.toml", "[package]\nname = \"x\"\n"), ("src/lib.rs", "")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::MissingFile { .. })));
    let r = temp_repo(&[("Cargo.toml", "[package]\nname = \"x\"\n"), ("Cargo.lock", "version = 4\n"), ("src/lib.rs", "")]);
    assert!(matches!(detect_at(r.path(), &DetectOptions::default()), Err(DetectError::NoProcess { .. })));
}
```

`crates/potter-detect/src/providers/rust.rs`:

```rust
//! Rust applications (spec §6.1 "Rust"). Dependencies build first against stub sources (the
//! cargo-chef technique) so source edits don't rebuild them; binaries land in /out.

use potter_plan::{Command, Decision, ImageConfig, Origin, Process};
use toml::Value;

use crate::catalog::{self, Line, RUST, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, at_least, parse_partial, release_prefix};
use crate::{DetectError, ProviderId};

const WEB_NAMES: &[&str] = &["server", "api", "web", "app"];

pub(crate) struct RustProvider;

/// One workspace member (or the root package).
struct Member {
    /// "" for the root, else the member directory.
    dir: String,
    package: String,
    /// (binary name, source path relative to the repo root)
    bins: Vec<(String, String)>,
    lib: Option<String>,
    build_script: Option<String>,
}

impl Provider for RustProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Rust
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(repo.is_file("Cargo.toml").then(|| Claim {
            provider: ProviderId::Rust,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec!["Cargo.toml".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let root = repo.read_toml("Cargo.toml")?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Rust,
            file: "Cargo.toml".into(),
            hint: "the Rust provider builds Cargo projects".into(),
        })?;
        if !repo.is_file("Cargo.lock") {
            return Err(DetectError::MissingFile {
                provider: ProviderId::Rust,
                file: "Cargo.lock".into(),
                hint: "commit Cargo.lock so builds are reproducible".into(),
            });
        }
        let mut d = Vec::new();
        let rust = rust_version(repo, opts, &root, &mut d)?;
        let members = members(repo, &root)?;
        let bins: Vec<&(String, String)> = members.iter().flat_map(|m| &m.bins).collect();
        if bins.is_empty() {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Rust,
                hint: format!("no binary target (src/main.rs or [[bin]]); set `{}`", opts.key("process.web.cmd")),
            });
        }

        let mut deps_inputs = vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()];
        for m in members.iter().filter(|m| !m.dir.is_empty()) {
            deps_inputs.push(format!("{}/Cargo.toml", m.dir));
        }
        for f in ["rust-toolchain.toml", "rust-toolchain", ".cargo/config.toml"] {
            if repo.is_file(f) {
                deps_inputs.push(f.into());
            }
        }
        let env = [("CARGO_HOME", "/cargo")];
        let cache = [("cargo-registry", "/cargo/registry"), ("cargo-git", "/cargo/git")];
        let stubs = stub_script(&members);
        let deps = if stubs.is_empty() {
            "cargo build --release --locked".to_string()
        } else {
            format!("{stubs} && cargo build --release --locked")
        };
        let packages: Vec<String> = members.iter().map(|m| format!("-p {}", m.package)).collect();
        let bin_paths: Vec<String> = bins.iter().map(|(name, _)| format!("target/release/{name}")).collect();
        let build = format!(
            "cargo clean --release {} && cargo build --release --locked && mkdir -p /out && cp {} /out/",
            packages.join(" "),
            bin_paths.join(" ")
        );
        let steps = vec![
            inputs("deps-inputs", deps_inputs),
            run("deps", Command::Shell(deps), &env, &cache),
            source(),
            run("build", Command::Shell(build), &env, &cache),
        ];

        let root_package = root.get("package").and_then(|p| p.get("name")).and_then(Value::as_str);
        let web = pick_web(&bins, root_package, opts)?;
        d.push(Decision::detected("process.web", format!("/out/{web}"), [format!("binary target {web}")]));
        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/out", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        let mut image = empty_image(vec![cref("os-min", None)], config);
        image.export = vec![export("/out", "/out")];
        for (name, _) in &bins {
            let process = if *name == web { "web".to_string() } else { name.clone() };
            if potter_plan::is_name(&process) {
                image.processes.insert(process, Process::new(&[&format!("/out/{name}")]));
            }
        }
        let layers = vec![cref("os-min", None), cref("rust", Some(rust.version))];
        Ok(Detection { target: new_target(target, ProviderId::Rust, layers, steps, image), decisions: d })
    }
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource { desc: desc.clone(), origin: Origin::Detected { evidence: vec![desc] }, set, lts_only: false }
}

fn rust_version(
    repo: &RepoView,
    opts: &DetectOptions,
    root: &Value,
    d: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.rust.as_deref(), &opts.key("runtime.rust"), "rust")?;
    if src.is_none() {
        let channel = match repo.read_toml("rust-toolchain.toml")? {
            Some(t) => t.get("toolchain").and_then(|t| t.get("channel")).and_then(Value::as_str).map(str::to_string),
            None => repo.read_string("rust-toolchain", READ_CAP)?.map(|s| s.trim().to_string()),
        };
        match channel.as_deref() {
            None | Some("stable") => {}
            Some(c) if c.starts_with("nightly") || c.starts_with("beta") => {
                return Err(DetectError::NeedsConfig {
                    provider: ProviderId::Rust,
                    message: format!("the `{c}` toolchain isn't in the catalog; pin a stable version"),
                    key: opts.key("runtime.rust"),
                });
            }
            Some(c) => {
                let set = parse_partial(release_prefix(c))
                    .and_then(|p| p.x_range())
                    .map_err(|e| bad_constraint("rust", c, "rust-toolchain channel", e))?;
                src = Some(detected(format!("rust-toolchain channel = {c}"), set));
            }
        }
    }
    if src.is_none() {
        let rv = root
            .get("package")
            .and_then(|p| p.get("rust-version"))
            .or_else(|| root.get("workspace")?.get("package")?.get("rust-version"))
            .and_then(Value::as_str);
        if let Some(v) = rv {
            let set = at_least(v).map_err(|e| bad_constraint("rust", v, "Cargo.toml rust-version", e))?;
            src = Some(detected(format!("Cargo.toml rust-version = {v} (minimum)"), set));
        }
    }
    catalog::pick(&RUST, "runtime.rust", src, d)
}

/// Paths and names that end up in shell commands: a conservative character set only.
fn safe(s: &str) -> bool {
    !s.is_empty()
        && potter_plan::is_relative_path(s)
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

fn unsafe_path(file: &str, what: &str) -> DetectError {
    DetectError::Parse { file: file.into(), reason: format!("`{what}` must be a plain relative path or name") }
}

fn members(repo: &RepoView, root: &Value) -> Result<Vec<Member>, DetectError> {
    let mut dirs: Vec<String> = Vec::new();
    if root.get("package").is_some() {
        dirs.push(String::new());
    }
    if let Some(list) = root.get("workspace").and_then(|w| w.get("members")).and_then(Value::as_array) {
        for pattern in list.iter().filter_map(Value::as_str) {
            match pattern.strip_suffix("/*") {
                Some(prefix) => {
                    let mut found: Vec<String> = repo
                        .files()
                        .filter_map(|f| f.strip_prefix(prefix)?.strip_prefix('/')?.strip_suffix("/Cargo.toml"))
                        .filter(|name| !name.contains('/'))
                        .map(|name| format!("{prefix}/{name}"))
                        .collect();
                    found.sort();
                    dirs.extend(found);
                }
                None => dirs.push(pattern.trim_end_matches('/').to_string()),
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        if !dir.is_empty() && !safe(&dir) {
            return Err(unsafe_path("Cargo.toml", &dir));
        }
        let manifest = if dir.is_empty() { "Cargo.toml".to_string() } else { format!("{dir}/Cargo.toml") };
        let Some(toml) = repo.read_toml(&manifest)? else { continue };
        let Some(package) = toml.get("package").and_then(|p| p.get("name")).and_then(Value::as_str) else { continue };
        if !package.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(unsafe_path(&manifest, package));
        }
        let at = |p: &str| if dir.is_empty() { p.to_string() } else { format!("{dir}/{p}") };
        let mut bins = Vec::new();
        if let Some(list) = toml.get("bin").and_then(Value::as_array) {
            for b in list {
                let name = b.get("name").and_then(Value::as_str).unwrap_or(package);
                let path = b.get("path").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| {
                    if name == package { "src/main.rs".into() } else { format!("src/bin/{name}.rs") }
                });
                if !safe(name) || !safe(&path) {
                    return Err(unsafe_path(&manifest, &format!("{name} = {path}")));
                }
                bins.push((name.to_string(), at(&path)));
            }
        }
        if bins.is_empty() && repo.is_file(&at("src/main.rs")) {
            bins.push((package.to_string(), at("src/main.rs")));
        }
        let lib_path = toml.get("lib").and_then(|l| l.get("path")).and_then(Value::as_str).unwrap_or("src/lib.rs");
        let lib = (repo.is_file(&at(lib_path)) && safe(lib_path)).then(|| at(lib_path));
        let build_script = match toml.get("package").and_then(|p| p.get("build")) {
            Some(Value::String(p)) if safe(p) => Some(at(p)),
            Some(Value::Boolean(false)) => None,
            _ => repo.is_file(&at("build.rs")).then(|| at("build.rs")),
        };
        out.push(Member { dir, package: package.to_string(), bins, lib, build_script });
    }
    Ok(out)
}

/// Shell that writes empty sources in place of every target, so `cargo build` compiles only
/// dependencies. All paths were checked by `safe`.
fn stub_script(members: &[Member]) -> String {
    let mut parts = Vec::new();
    for m in members {
        for (_, path) in &m.bins {
            parts.push(stub(path, "fn main() {}"));
        }
        if let Some(lib) = &m.lib {
            parts.push(stub(lib, ""));
        }
        if let Some(build) = &m.build_script {
            parts.push(stub(build, "fn main() {}"));
        }
    }
    parts.join(" && ")
}

fn stub(path: &str, body: &str) -> String {
    let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
    format!("mkdir -p {dir} && printf '%s\\n' '{body}' > {path}")
}

fn pick_web(bins: &[&(String, String)], root_package: Option<&str>, opts: &DetectOptions) -> Result<String, DetectError> {
    if let [(only, _)] = bins {
        return Ok(only.clone());
    }
    let preferred = root_package.into_iter().chain(WEB_NAMES.iter().copied());
    for want in preferred {
        if let Some((name, _)) = bins.iter().find(|(n, _)| n == want) {
            return Ok(name.clone());
        }
    }
    Err(DetectError::NeedsConfig {
        provider: ProviderId::Rust,
        message: format!(
            "several binaries ({}) and none matches the package or is named server, api, web or app",
            bins.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
        ),
        key: opts.key("process.web.cmd"),
    })
}
```

---

### Task 7: potter.toml keys, snapshots, monorepo backend, docs

**Files:**
- Modify:
  - `crates/potter-config/src/file.rs`: `RuntimeConfig.{python, ruby, go, rust}`; `TargetConfig.python: PythonConfig { package_manager, django: DjangoConfig { collectstatic } }`.
  - `crates/potter-config/src/options.rs`: map the new keys, and update the unknown-provider message to list all ten.
  - `crates/potter-config/tests/file.rs`: a test that `[target.app.runtime] python = "3.12"` and `[target.app.python.django] collectstatic = false` map into `DetectOptions`.
  - `crates/potter-config/tests/fixtures.rs`: snapshots for the 9 new fixtures.
  - `fixtures/monorepo-targets/potter.toml`: add `[target.backend] root = "backend"`, and copy `backend/main.go` from `nextjs-go-monorepo`.
  - `docs/plan-format.md`: the new keys. `README.md`: the language list.
  - `docs/superpowers/HANDOFF.md`: P1b done.
- [ ] **Step 1:** Write the config test and new snapshot entries. Run them. Expected: the config test fails to compile.
- [ ] **Step 2:** Implement the config keys. Generate snapshots with `INSTA_UPDATE=always`, then review each new snapshot's summary against spec §6.1.
- [ ] **Step 3: Run** `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`. Expected: everything passes.
- [ ] **Step 4: Commit** `feat: P1b config keys, snapshots and docs`.
