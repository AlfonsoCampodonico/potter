//! Components named by the lockfiles a target declares as inputs (spec §17). Parsing is
//! static and bounded (P9); a lockfile that is too large, too long or malformed is refused
//! whole, so the SBOM never holds a partial or guessed list.

use std::collections::BTreeSet;

use serde_json::Value;

/// Lockfiles over this size are not parsed.
pub const MAX_LOCKFILE: usize = 16 * 1024 * 1024;
/// Lockfiles naming more components than this are not parsed.
pub const MAX_COMPONENTS: usize = 100_000;
/// `package-lock.json` v1 dependency nesting bound.
const MAX_DEPTH: usize = 64;

/// The file names potter reads as lockfile evidence. `go.sum` is read with its `go.mod`.
pub const LOCKFILES: &[&str] = &[
    "composer.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "deno.lock",
    "uv.lock",
    "poetry.lock",
    "requirements.txt",
    "Gemfile.lock",
    "go.sum",
    "Cargo.lock",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ecosystem {
    Composer,
    Npm,
    Jsr,
    PyPI,
    Gem,
    Golang,
    Cargo,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Component {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
    /// The lockfile it came from, relative to the target root.
    pub source: String,
}

impl Component {
    /// The package URL (purl) for this component.
    pub fn purl(&self) -> String {
        let v = encode(&self.version, false);
        match self.ecosystem {
            Ecosystem::Composer => format!("pkg:composer/{}@{v}", encode(&self.name, true)),
            Ecosystem::Npm => format!("pkg:npm/{}@{v}", encode(&self.name, true)),
            // jsr has no registered purl type.
            Ecosystem::Jsr => format!("pkg:generic/jsr.io/{}@{v}", encode(&self.name, true)),
            Ecosystem::PyPI => format!("pkg:pypi/{}@{v}", encode(&pep503(&self.name), true)),
            Ecosystem::Gem => format!("pkg:gem/{}@{v}", encode(&self.name, true)),
            Ecosystem::Golang => format!("pkg:golang/{}@{v}", encode(&self.name, true)),
            Ecosystem::Cargo => format!("pkg:cargo/{}@{v}", encode(&self.name, true)),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LockError {
    #[error("{path}: {size} bytes is over the {MAX_LOCKFILE}-byte lockfile limit")]
    TooLarge { path: String, size: usize },
    #[error("{path}: more than {MAX_COMPONENTS} components")]
    TooMany { path: String },
    #[error("{path}: {reason}")]
    Parse { path: String, reason: String },
    #[error("{path}: not a lockfile potter reads")]
    Unknown { path: String },
    #[error("{path}: needs its go.mod")]
    NeedsGoMod { path: String },
}

/// Components named by the lockfile at `path` (relative; its file name picks the format).
/// `go.sum` needs its `go.mod`: use [`parse_go`].
pub fn parse(path: &str, bytes: &[u8]) -> Result<Vec<Component>, LockError> {
    let text = text(path, bytes)?;
    let found = match file_name(path) {
        "composer.lock" => composer(path, &json(path, text)?),
        "package-lock.json" => package_lock(path, &json(path, text)?)?,
        "pnpm-lock.yaml" => pnpm(path, text),
        "yarn.lock" => yarn(path, text),
        "bun.lock" => bun(path, &json(path, &potter_detect::jsonc::strip_jsonc(text))?),
        "deno.lock" => deno(path, &json(path, text)?),
        "uv.lock" => toml_packages(path, text, Ecosystem::PyPI, |p| {
            p.get("source")
                .and_then(|s| s.as_table())
                .is_some_and(|s| s.contains_key("editable") || s.contains_key("virtual"))
        })?,
        "poetry.lock" => toml_packages(path, text, Ecosystem::PyPI, |_| false)?,
        "requirements.txt" => requirements(path, text),
        "Gemfile.lock" => gemfile(path, text),
        "Cargo.lock" => toml_packages(path, text, Ecosystem::Cargo, |p| !p.contains_key("source"))?,
        "go.sum" => return Err(LockError::NeedsGoMod { path: path.into() }),
        _ => return Err(LockError::Unknown { path: path.into() }),
    };
    finish(path, found)
}

/// Go modules required by `go.mod` whose version `go.sum` (at `path`) records.
pub fn parse_go(path: &str, go_mod: &[u8], go_sum: &[u8]) -> Result<Vec<Component>, LockError> {
    let sum = text(path, go_sum)?;
    let module = text(path, go_mod)?;
    let recorded: BTreeSet<(&str, &str)> = sum
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (m, v) = (f.next()?, f.next()?);
            (!v.ends_with("/go.mod")).then_some((m, v))
        })
        .collect();
    let mut found = Vec::new();
    let mut in_block = false;
    for line in module.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        let spec = if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            line
        } else if let Some(rest) = line.strip_prefix("require") {
            let rest = rest.trim();
            if rest == "(" {
                in_block = true;
                continue;
            }
            rest
        } else {
            continue;
        };
        let mut f = spec.split_whitespace();
        if let (Some(m), Some(v)) = (f.next(), f.next())
            && recorded.contains(&(m, v))
        {
            found.push(component(Ecosystem::Golang, m, v, path));
        }
    }
    finish(path, found)
}

fn finish(path: &str, mut found: Vec<Component>) -> Result<Vec<Component>, LockError> {
    found.sort();
    found.dedup();
    if found.len() > MAX_COMPONENTS {
        return Err(LockError::TooMany { path: path.into() });
    }
    Ok(found)
}

fn text<'a>(path: &str, bytes: &'a [u8]) -> Result<&'a str, LockError> {
    if bytes.len() > MAX_LOCKFILE {
        return Err(LockError::TooLarge {
            path: path.into(),
            size: bytes.len(),
        });
    }
    std::str::from_utf8(bytes).map_err(|_| parse_err(path, "not UTF-8"))
}

fn json(path: &str, text: &str) -> Result<Value, LockError> {
    serde_json::from_str(text).map_err(|e| parse_err(path, &e.to_string()))
}

fn parse_err(path: &str, reason: &str) -> LockError {
    LockError::Parse {
        path: path.into(),
        reason: reason.into(),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn component(ecosystem: Ecosystem, name: &str, version: &str, source: &str) -> Component {
    Component {
        ecosystem,
        name: name.into(),
        version: version.into(),
        source: source.into(),
    }
}

/// Splits `name@version`, where the name may start with `@` (a scope).
fn split_at_version(spec: &str) -> Option<(&str, &str)> {
    let at = spec.get(1..)?.find('@')? + 1;
    let (name, version) = (&spec[..at], &spec[at + 1..]);
    (!name.is_empty() && !version.is_empty()).then_some((name, version))
}

fn composer(path: &str, lock: &Value) -> Vec<Component> {
    lock.get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?;
            let version = p.get("version")?.as_str()?;
            Some(component(Ecosystem::Composer, name, version, path))
        })
        .collect()
}

fn package_lock(path: &str, lock: &Value) -> Result<Vec<Component>, LockError> {
    if let Some(packages) = lock.get("packages").and_then(Value::as_object) {
        return Ok(packages
            .iter()
            .filter_map(|(key, p)| {
                let installed = key.rsplit_once("node_modules/")?.1;
                if p.get("dev").and_then(Value::as_bool) == Some(true)
                    || p.get("link").and_then(Value::as_bool) == Some(true)
                {
                    return None;
                }
                let name = p.get("name").and_then(Value::as_str).unwrap_or(installed);
                let version = p.get("version")?.as_str()?;
                Some(component(Ecosystem::Npm, name, version, path))
            })
            .collect());
    }
    // lockfileVersion 1: nested `dependencies`.
    fn walk(path: &str, deps: &Value, depth: usize, out: &mut Vec<Component>) -> Result<(), LockError> {
        let Some(deps) = deps.as_object() else { return Ok(()) };
        if depth > MAX_DEPTH {
            return Err(parse_err(path, "dependencies nested too deeply"));
        }
        for (name, d) in deps {
            if d.get("dev").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(v) = d.get("version").and_then(Value::as_str) {
                out.push(component(Ecosystem::Npm, name, v, path));
            }
            if let Some(nested) = d.get("dependencies") {
                walk(path, nested, depth + 1, out)?;
            }
            if out.len() > MAX_COMPONENTS {
                return Err(LockError::TooMany { path: path.into() });
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if let Some(deps) = lock.get("dependencies") {
        walk(path, deps, 0, &mut out)?;
    }
    Ok(out)
}

fn pnpm(path: &str, text: &str) -> Vec<Component> {
    let mut out = Vec::new();
    let mut in_packages = false;
    // The entry being read (v5/v6 mark dev packages with `dev: true` beneath it).
    let mut entry: Option<Component> = None;
    let mut dev = false;
    let mut flush = |entry: &mut Option<Component>, dev: bool| {
        if let Some(c) = entry.take()
            && !dev
        {
            out.push(c);
        }
    };
    for line in text.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            flush(&mut entry, dev);
            in_packages = line.trim_end() == "packages:";
            continue;
        }
        if !in_packages {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let body = line.trim();
        if indent == 2 && body.ends_with(':') {
            flush(&mut entry, dev);
            dev = false;
            let key = body.trim_end_matches(':').trim_matches(|c| c == '\'' || c == '"');
            entry = pnpm_key(key).map(|(n, v)| component(Ecosystem::Npm, n, v, path));
        } else if indent == 4 && body == "dev: true" {
            dev = true;
        }
    }
    flush(&mut entry, dev);
    out
}

/// `/name@1.0.0(peer@2)` (v6, v9 without the slash) or `/name/1.0.0_peer@2` (v5).
fn pnpm_key(key: &str) -> Option<(&str, &str)> {
    let key = key.strip_prefix('/').unwrap_or(key);
    let key = key.split('(').next()?;
    // v5: the version is the last path segment, before any `_peer` suffix.
    let unpeered = key.split('_').next()?;
    if let Some((name, version)) = unpeered.rsplit_once('/')
        && version.starts_with(|c: char| c.is_ascii_digit())
        && !name.is_empty()
    {
        return Some((name, version));
    }
    split_at_version(key)
}

fn yarn(path: &str, text: &str) -> Vec<Component> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            name = line.strip_suffix(':').and_then(|header| {
                let first = header.split(", ").next()?.trim_matches('"');
                if first.starts_with("__metadata") {
                    return None;
                }
                let (name, range) = split_at_version(first)?;
                let local = ["workspace:", "link:", "portal:", "file:"];
                (!local
                    .iter()
                    .any(|p| range.starts_with(p) || range.contains(&format!("@{p}"))))
                .then(|| name.to_string())
            });
            continue;
        }
        let body = line.trim();
        let version = body
            .strip_prefix("version: ")
            .or_else(|| body.strip_prefix("version "))
            .map(|v| v.trim_matches('"'));
        if let Some(v) = version
            && let Some(n) = name.take()
        {
            out.push(component(Ecosystem::Npm, &n, v, path));
        }
    }
    out
}

fn bun(path: &str, lock: &Value) -> Vec<Component> {
    lock.get("packages")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(_, entry)| {
            let spec = entry.as_array()?.first()?.as_str()?;
            let (name, version) = split_at_version(spec)?;
            (!version.contains(':')).then(|| component(Ecosystem::Npm, name, version, path))
        })
        .collect()
}

fn deno(path: &str, lock: &Value) -> Vec<Component> {
    let section = |eco: &str| {
        lock.get(eco)
            .or_else(|| lock.get("packages").and_then(|p| p.get(eco)))
            .and_then(Value::as_object)
    };
    let mut out = Vec::new();
    for (eco, ecosystem) in [("npm", Ecosystem::Npm), ("jsr", Ecosystem::Jsr)] {
        for key in section(eco).into_iter().flatten().map(|(k, _)| k) {
            if let Some((name, version)) = split_at_version(key) {
                // npm peer-dependency suffix: `1.0.0_react@18.2.0`.
                let version = version.split('_').next().unwrap_or(version);
                out.push(component(ecosystem, name, version, path));
            }
        }
    }
    out
}

fn toml_packages(
    path: &str,
    text: &str,
    ecosystem: Ecosystem,
    skip: impl Fn(&toml::Table) -> bool,
) -> Result<Vec<Component>, LockError> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| parse_err(path, e.message()))?;
    Ok(doc
        .get("package")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let p = p.as_table()?;
            if skip(p) {
                return None;
            }
            let name = p.get("name")?.as_str()?;
            let version = p.get("version")?.as_str()?;
            Some(component(ecosystem, name, version, path))
        })
        .collect())
}

fn requirements(path: &str, text: &str) -> Vec<Component> {
    text.lines()
        .filter_map(|line| {
            let line = line.split(" #").next()?.split(';').next()?;
            let line = line.trim().trim_end_matches('\\').trim();
            if line.starts_with('-') || line.starts_with('#') {
                return None;
            }
            let (name, version) = line.split_once("==")?;
            let name = name.split('[').next()?.trim();
            let version = version.split_whitespace().next()?;
            let plain = |s: &str, extra: &str| {
                !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
            };
            // `===` (arbitrary equality) and wildcards are not pins.
            (plain(name, "._-") && plain(version, ".+!-_")).then(|| component(Ecosystem::PyPI, name, version, path))
        })
        .collect()
}

fn gemfile(path: &str, text: &str) -> Vec<Component> {
    let mut out = Vec::new();
    let (mut in_gem, mut in_specs) = (false, false);
    for line in text.lines() {
        if !line.starts_with(' ') {
            in_gem = line.trim_end() == "GEM";
            in_specs = false;
            continue;
        }
        if !in_gem {
            continue;
        }
        if line.trim_end() == "  specs:" {
            in_specs = true;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if in_specs
            && indent == 4
            && let Some((name, rest)) = line.trim().split_once(" (")
            && let Some(version) = rest.strip_suffix(')')
        {
            // `1.15.4-x86_64-linux`: the platform is not part of the version.
            let version = version.split('-').next().unwrap_or(version);
            out.push(component(Ecosystem::Gem, name, version, path));
        }
    }
    out
}

/// PEP 503 normalised name.
fn pep503(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// purl percent-encoding; `/` stays a separator in names.
fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b".-_~".contains(&b) || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(path: &str, text: &str) -> Vec<(String, String)> {
        parse(path, text.as_bytes())
            .unwrap()
            .into_iter()
            .map(|c| (c.name, c.version))
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
    }

    #[test]
    fn composer_lists_runtime_packages_only() {
        let lock = r#"{"packages":[{"name":"laravel/framework","version":"v11.9.2"}],
            "packages-dev":[{"name":"phpunit/phpunit","version":"11.0.0"}]}"#;
        assert_eq!(names("composer.lock", lock), pairs(&[("laravel/framework", "v11.9.2")]));
        let c = &parse("composer.lock", lock.as_bytes()).unwrap()[0];
        assert_eq!(c.purl(), "pkg:composer/laravel/framework@v11.9.2");
        assert_eq!(c.source, "composer.lock");
    }

    #[test]
    fn package_lock_v3_skips_root_dev_and_links() {
        let lock = r#"{"lockfileVersion":3,"packages":{
            "":{"name":"app","version":"1.0.0"},
            "node_modules/express":{"version":"4.19.2"},
            "node_modules/@types/node":{"version":"20.1.0","dev":true},
            "node_modules/a/node_modules/@scope/b":{"version":"2.0.0"},
            "node_modules/local":{"link":true,"resolved":"packages/local"},
            "node_modules/alias":{"name":"real","version":"3.0.0"}}}"#;
        assert_eq!(
            names("web/package-lock.json", lock),
            pairs(&[("@scope/b", "2.0.0"), ("express", "4.19.2"), ("real", "3.0.0")])
        );
        let c = parse("package-lock.json", lock.as_bytes()).unwrap();
        assert_eq!(c[0].purl(), "pkg:npm/%40scope/b@2.0.0");
    }

    #[test]
    fn package_lock_v1_walks_nested_dependencies() {
        let lock = r#"{"lockfileVersion":1,"dependencies":{
            "a":{"version":"1.0.0","dependencies":{"b":{"version":"2.0.0"}}},
            "c":{"version":"3.0.0","dev":true}}}"#;
        assert_eq!(
            names("package-lock.json", lock),
            pairs(&[("a", "1.0.0"), ("b", "2.0.0")])
        );
    }

    #[test]
    fn package_lock_v1_depth_is_bounded() {
        let mut lock = String::from(r#"{"version":"1.0.0"}"#);
        for _ in 0..70 {
            lock = format!(r#"{{"version":"1.0.0","dependencies":{{"x":{lock}}}}}"#);
        }
        let lock = format!(r#"{{"lockfileVersion":1,"dependencies":{{"x":{lock}}}}}"#);
        assert!(matches!(
            parse("package-lock.json", lock.as_bytes()),
            Err(LockError::Parse { .. })
        ));
    }

    #[test]
    fn pnpm_reads_v5_v6_and_v9_keys() {
        let v9 = "lockfileVersion: '9.0'\n\npackages:\n\n  '@babel/core@7.24.0':\n    resolution: {integrity: x}\n\n  react-dom@18.2.0(react@18.2.0):\n    resolution: {integrity: y}\n\nsnapshots:\n\n  other@1.0.0:\n    dependencies: {}\n";
        assert_eq!(
            names("pnpm-lock.yaml", v9),
            pairs(&[("@babel/core", "7.24.0"), ("react-dom", "18.2.0")])
        );
        let v6 = "lockfileVersion: '6.0'\npackages:\n  /lodash@4.17.21:\n    resolution: {integrity: x}\n  /vitest@1.0.0:\n    resolution: {integrity: y}\n    dev: true\n";
        assert_eq!(names("pnpm-lock.yaml", v6), pairs(&[("lodash", "4.17.21")]));
        let v5 = "lockfileVersion: 5.4\npackages:\n  /@scope/pkg/1.2.3_react@18.0.0:\n    resolution: {integrity: x}\n";
        assert_eq!(names("pnpm-lock.yaml", v5), pairs(&[("@scope/pkg", "1.2.3")]));
    }

    #[test]
    fn yarn_reads_classic_and_berry() {
        let classic = "# yarn lockfile v1\n\n\"@babel/core@^7.0.0\", \"@babel/core@^7.1.0\":\n  version \"7.24.0\"\n  resolved \"https://x\"\n\nlodash@^4.17.21:\n  resolved \"https://x\"\n  version \"4.17.21\"\n";
        assert_eq!(
            names("yarn.lock", classic),
            pairs(&[("@babel/core", "7.24.0"), ("lodash", "4.17.21")])
        );
        let berry = "__metadata:\n  version: 8\n\n\"app@workspace:.\":\n  version: 0.0.0-use.local\n\n\"lodash@npm:^4.17.21\":\n  version: 4.17.21\n  resolution: \"lodash@npm:4.17.21\"\n";
        assert_eq!(names("yarn.lock", berry), pairs(&[("lodash", "4.17.21")]));
    }

    #[test]
    fn bun_lock_is_jsonc_and_skips_local_packages() {
        let lock = "{\n  // bun\n  \"lockfileVersion\": 1,\n  \"packages\": {\n    \"hono\": [\"hono@4.5.0\", \"\", {}, \"sha512-x\"],\n    \"app\": [\"app@workspace:.\"],\n  },\n}";
        assert_eq!(names("bun.lock", lock), pairs(&[("hono", "4.5.0")]));
    }

    #[test]
    fn deno_lock_reads_v3_and_v4() {
        let v4 = r#"{"version":"4","jsr":{"@std/path@1.0.8":{}},"npm":{"chalk@5.3.0":{},"react-dom@18.2.0_react@18.2.0":{}}}"#;
        let got = parse("deno.lock", v4.as_bytes()).unwrap();
        assert_eq!(
            got.iter()
                .map(|c| (c.name.as_str(), c.version.as_str(), c.ecosystem))
                .collect::<Vec<_>>(),
            [
                ("chalk", "5.3.0", Ecosystem::Npm),
                ("react-dom", "18.2.0", Ecosystem::Npm),
                ("@std/path", "1.0.8", Ecosystem::Jsr),
            ]
        );
        assert_eq!(
            got.iter().find(|c| c.ecosystem == Ecosystem::Jsr).unwrap().purl(),
            "pkg:generic/jsr.io/%40std/path@1.0.8"
        );
        let v3 = r#"{"version":"3","packages":{"npm":{"chalk@5.3.0":{}}}}"#;
        assert_eq!(names("deno.lock", v3), pairs(&[("chalk", "5.3.0")]));
    }

    #[test]
    fn uv_skips_the_editable_project() {
        let lock = "version = 1\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n\n[[package]]\nname = \"Flask\"\nversion = \"3.0.3\"\nsource = { registry = \"https://pypi.org/simple\" }\n";
        assert_eq!(names("uv.lock", lock), pairs(&[("Flask", "3.0.3")]));
        let c = &parse("uv.lock", lock.as_bytes()).unwrap()[0];
        assert_eq!(c.purl(), "pkg:pypi/flask@3.0.3");
    }

    #[test]
    fn poetry_lists_every_package() {
        let lock = "[[package]]\nname = \"django\"\nversion = \"5.0.6\"\n\n[[package]]\nname = \"zope.interface\"\nversion = \"6.4\"\n";
        assert_eq!(
            names("poetry.lock", lock),
            pairs(&[("django", "5.0.6"), ("zope.interface", "6.4")])
        );
        let c = parse("poetry.lock", lock.as_bytes()).unwrap();
        assert_eq!(c[1].purl(), "pkg:pypi/zope-interface@6.4");
    }

    #[test]
    fn requirements_counts_only_exact_pins() {
        let reqs = "# deps\nflask==3.0.3\nrequests[socks]==2.32.3 ; python_version >= \"3.8\"\nuvicorn>=0.30\ngunicorn\nnumpy==1.*\n-r other.txt\nsix==1.16.0 \\\n    --hash=sha256:abc\nweird===1.0\n";
        assert_eq!(
            names("requirements.txt", reqs),
            pairs(&[("flask", "3.0.3"), ("requests", "2.32.3"), ("six", "1.16.0")])
        );
        assert!(parse("requirements.txt", b"uvicorn>=0.30\n").unwrap().is_empty());
    }

    #[test]
    fn gemfile_reads_gem_specs_only() {
        let lock = "GIT\n  remote: https://x\n  specs:\n    forked (1.0.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    nokogiri (1.16.5-x86_64-linux)\n      racc (~> 1.4)\n    rack (3.0.11)\n\nPLATFORMS\n  x86_64-linux\n";
        assert_eq!(
            names("Gemfile.lock", lock),
            pairs(&[("nokogiri", "1.16.5"), ("rack", "3.0.11")])
        );
    }

    #[test]
    fn go_needs_go_mod_and_keeps_recorded_requirements() {
        assert!(matches!(parse("go.sum", b""), Err(LockError::NeedsGoMod { .. })));
        let go_mod = b"module example.com/app\n\ngo 1.22\n\nrequire github.com/a/b v1.2.3\n\nrequire (\n\tgolang.org/x/net v0.25.0 // indirect\n\tgithub.com/missing/sum v0.1.0\n)\n";
        let go_sum = b"github.com/a/b v1.2.3 h1:x=\ngithub.com/a/b v1.2.3/go.mod h1:y=\ngolang.org/x/net v0.25.0 h1:z=\ngithub.com/missing/sum v0.1.0/go.mod h1:w=\n";
        let got = parse_go("go.sum", go_mod, go_sum).unwrap();
        assert_eq!(
            got.iter()
                .map(|c| (c.name.as_str(), c.version.as_str()))
                .collect::<Vec<_>>(),
            [("github.com/a/b", "v1.2.3"), ("golang.org/x/net", "v0.25.0")]
        );
        assert_eq!(got[0].purl(), "pkg:golang/github.com/a/b@v1.2.3");
    }

    #[test]
    fn cargo_skips_path_packages() {
        let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        assert_eq!(names("Cargo.lock", lock), pairs(&[("serde", "1.0.210")]));
    }

    #[test]
    fn oversized_and_non_utf8_lockfiles_are_refused_whole() {
        let big = vec![b' '; MAX_LOCKFILE + 1];
        assert!(matches!(parse("composer.lock", &big), Err(LockError::TooLarge { .. })));
        assert!(matches!(
            parse("yarn.lock", &[0xff, 0xfe, b'a']),
            Err(LockError::Parse { .. })
        ));
        assert!(matches!(parse("composer.lock", b"{"), Err(LockError::Parse { .. })));
        assert!(matches!(parse("Pipfile", b""), Err(LockError::Unknown { .. })));
    }

    #[test]
    fn too_many_components_are_refused_whole() {
        let reqs: String = (0..=MAX_COMPONENTS).map(|i| format!("p{i}==1.0\n")).collect();
        assert!(matches!(
            parse("requirements.txt", reqs.as_bytes()),
            Err(LockError::TooMany { .. })
        ));
    }

    #[test]
    fn duplicates_collapse() {
        let reqs = "flask==3.0.3\nflask==3.0.3\n";
        assert_eq!(names("requirements.txt", reqs), pairs(&[("flask", "3.0.3")]));
    }
}
