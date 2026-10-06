//! Generic composer projects (spec §6.1 "PHP generic", §6.1.1).

use std::collections::{BTreeMap, BTreeSet};

use potter_plan::{CatalogRef, Command, Decision, Export, GeneratedFile, ImageConfig, Origin, Process, Ready, Service};
use serde_json::Value;

use crate::catalog::{self, COMPOSER_VERSION, Line, PHP, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::php_config;
use crate::providers::{Detection, Provider};
use crate::repo::{LOCKFILE_CAP, RepoView};
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, parse_composer_constraint, parse_partial};
use crate::{DetectError, ProviderId};

/// Compiled into the catalog's php layer; never separate extension layers.
pub(crate) const BUILTIN_EXTENSIONS: &[&str] = &[
    "ctype",
    "curl",
    "date",
    "dom",
    "fileinfo",
    "filter",
    "hash",
    "iconv",
    "json",
    "libxml",
    "mbstring",
    "mysqlnd",
    "openssl",
    "pcre",
    "pdo",
    "pdo_sqlite",
    "phar",
    "posix",
    "readline",
    "reflection",
    "session",
    "simplexml",
    "sodium",
    "spl",
    "sqlite3",
    "standard",
    "tokenizer",
    "xml",
    "xmlreader",
    "xmlwriter",
    "zlib",
];

/// Packages known to need extensions they don't declare.
const PACKAGE_EXTENSIONS: &[(&str, &[&str])] = &[
    ("drupal/core", &["gd", "opcache", "pdo_mysql", "pdo_pgsql"]),
    ("drupal/core-recommended", &["gd", "opcache", "pdo_mysql", "pdo_pgsql"]),
];

pub(crate) struct PhpProvider;

impl Provider for PhpProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Php
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(repo.is_file("composer.json").then(|| Claim {
            provider: ProviderId::Php,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec!["composer.json".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let facts = read_facts(repo, opts, ProviderId::Php, &[], "")?;
        build_php(repo, facts, opts, ProviderId::Php, target)
    }
}

pub(crate) struct PhpFacts {
    #[allow(dead_code)] // Read by Laravel (Task 11).
    pub composer: Value,
    pub version: &'static Line,
    pub extensions: BTreeSet<String>,
    pub docroot: String,
    pub decisions: Vec<Decision>,
}

pub(crate) fn requires(composer: &Value, package: &str) -> bool {
    composer.get("require").and_then(|r| r.get(package)).is_some()
}

pub(crate) fn read_facts(
    repo: &RepoView,
    opts: &DetectOptions,
    provider: ProviderId,
    defaults: &[&str],
    defaults_reason: &str,
) -> Result<PhpFacts, DetectError> {
    let composer = repo
        .read_json("composer.json")?
        .ok_or_else(|| DetectError::MissingFile {
            provider,
            file: "composer.json".into(),
            hint: "the PHP providers build composer projects".into(),
        })?;
    let lock = repo
        .read_json_capped("composer.lock", LOCKFILE_CAP)?
        .ok_or_else(|| DetectError::MissingFile {
            provider,
            file: "composer.lock".into(),
            hint: "commit composer.lock (run `composer update` locally) so builds are reproducible".into(),
        })?;
    let mut decisions = Vec::new();
    let version = php_version(repo, opts, &composer, &mut decisions)?;
    let extensions = extensions(opts, &composer, &lock, defaults, defaults_reason, &mut decisions);
    let docroot = docroot(repo, &composer, &mut decisions)?;
    Ok(PhpFacts {
        composer,
        version,
        extensions,
        docroot,
        decisions,
    })
}

fn php_version(
    repo: &RepoView,
    opts: &DetectOptions,
    composer: &Value,
    decisions: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.php.as_deref(), &opts.key("runtime.php"), "php")?;
    if src.is_none() {
        src = version_from_file(repo, ".php-version", "php")?;
    }
    if src.is_none()
        && let Some(p) = composer.pointer("/config/platform/php").and_then(Value::as_str)
    {
        let set = parse_partial(p)
            .and_then(|v| v.x_range())
            .map_err(|e| bad_constraint("php", p, "composer.json config.platform.php", e))?;
        src = Some(detected_source(format!("composer.json config.platform.php = {p}"), set));
    }
    if src.is_none()
        && let Some(c) = composer.pointer("/require/php").and_then(Value::as_str)
    {
        let set = parse_composer_constraint(c).map_err(|e| bad_constraint("php", c, "composer.json require.php", e))?;
        src = Some(detected_source(format!("composer.json require.php = {c}"), set));
    }
    catalog::pick(&PHP, "runtime.php", src, decisions)
}

fn detected_source(desc: String, set: VersionSet) -> VersionSource {
    VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only: false,
    }
}

/// `ext-pdo_mysql` → `pdo_mysql`; `zend-opcache` → `opcache`. Names that aren't extension
/// identifiers are dropped.
fn normalize_ext(raw: &str) -> Option<String> {
    let s = raw
        .strip_prefix("ext-")
        .unwrap_or(raw)
        .to_ascii_lowercase()
        .replace('-', "_");
    let s = match s.as_str() {
        "zend_opcache" | "zendopcache" => "opcache".to_string(),
        _ => s,
    };
    (!s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    .then_some(s)
}

fn ext_keys(obj: Option<&Value>) -> Vec<String> {
    obj.and_then(Value::as_object)
        .map(|m| {
            m.keys()
                .filter(|k| k.starts_with("ext-"))
                .filter_map(|k| normalize_ext(k))
                .collect()
        })
        .unwrap_or_default()
}

fn extensions(
    opts: &DetectOptions,
    composer: &Value,
    lock: &Value,
    defaults: &[&str],
    defaults_reason: &str,
    decisions: &mut Vec<Decision>,
) -> BTreeSet<String> {
    let mut found: BTreeMap<String, Origin> = BTreeMap::new();
    let mut add = |ext: String, origin: Origin| {
        found.entry(ext).or_insert(origin);
    };
    for e in &opts.runtime.extensions {
        if let Some(n) = normalize_ext(e) {
            add(
                n,
                Origin::Config {
                    key: opts.key("runtime.extensions"),
                },
            );
        }
    }
    let ev = |s: String| Origin::Detected { evidence: vec![s] };
    for e in ext_keys(composer.get("require")) {
        add(e.clone(), ev(format!("composer.json require.ext-{e}")));
    }
    let packages: Vec<&Value> = lock
        .get("packages")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    for pkg in &packages {
        let name = pkg.get("name").and_then(Value::as_str).unwrap_or("?");
        for e in ext_keys(pkg.get("require")) {
            add(e.clone(), ev(format!("composer.lock: {name} requires ext-{e}")));
        }
    }
    for e in ext_keys(lock.get("platform")) {
        add(e.clone(), ev(format!("composer.lock platform.ext-{e}")));
    }
    for (package, exts) in PACKAGE_EXTENSIONS {
        let locked = packages
            .iter()
            .any(|p| p.get("name").and_then(Value::as_str) == Some(*package));
        if requires(composer, package) || locked {
            for e in *exts {
                add(e.to_string(), ev(format!("{package} needs ext-{e}")));
            }
        }
    }
    for e in defaults {
        add(e.to_string(), ev(defaults_reason.to_string()));
    }
    let mut out = BTreeSet::new();
    for (ext, origin) in found {
        if BUILTIN_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        decisions.push(Decision {
            field: "runtime.extensions".into(),
            value: ext.clone(),
            origin,
        });
        out.insert(ext);
    }
    out
}

fn safe_docroot(s: &str) -> bool {
    potter_plan::is_relative_path(s) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

fn docroot(repo: &RepoView, composer: &Value, decisions: &mut Vec<Decision>) -> Result<String, DetectError> {
    if let Some(root) = composer
        .pointer("/extra/drupal-scaffold/locations/web-root")
        .and_then(Value::as_str)
    {
        let trimmed = root.trim_end_matches('/');
        let r = if trimmed.is_empty() { "." } else { trimmed };
        if !safe_docroot(r) {
            return Err(DetectError::Parse {
                file: "composer.json".into(),
                reason: "extra.drupal-scaffold.locations.web-root must be a plain relative path".into(),
            });
        }
        decisions.push(Decision::detected(
            "php.docroot",
            r,
            [format!(
                "composer.json extra.drupal-scaffold.locations.web-root = {root}"
            )],
        ));
        return Ok(r.to_string());
    }
    for d in ["public", "web"] {
        if repo.is_dir(d) {
            decisions.push(Decision::detected("php.docroot", d, [format!("{d}/ exists")]));
            return Ok(d.to_string());
        }
    }
    decisions.push(Decision::default("php.docroot", "."));
    Ok(".".to_string())
}

pub(crate) fn build_php(
    repo: &RepoView,
    facts: PhpFacts,
    opts: &DetectOptions,
    provider: ProviderId,
    name: &str,
) -> Result<Detection, DetectError> {
    let PhpFacts {
        version,
        extensions,
        docroot,
        mut decisions,
        ..
    } = facts;
    let v = version.version;
    let ext_refs: Vec<CatalogRef> = extensions
        .iter()
        .map(|e| cref(&format!("php-ext-{e}"), Some(v)))
        .collect();

    let mut build_layers = vec![cref("os-min", None), cref("php", Some(v))];
    build_layers.extend(ext_refs.iter().cloned());
    build_layers.push(cref("composer", Some(COMPOSER_VERSION)));
    let steps = vec![
        inputs("deps-inputs", vec!["composer.json".into(), "composer.lock".into()]),
        run(
            "deps",
            Command::shell(
                "composer install --no-dev --no-scripts --no-autoloader --prefer-dist --no-interaction --no-progress",
            ),
            &[
                ("COMPOSER_ALLOW_SUPERUSER", "1"),
                ("COMPOSER_CACHE_DIR", "/cache/composer"),
            ],
            &[("composer", "/cache/composer")],
        ),
        source(),
        run(
            "autoload",
            Command::shell("composer dump-autoload --optimize --no-dev --no-interaction"),
            &[("COMPOSER_ALLOW_SUPERUSER", "1")],
            &[],
        ),
    ];

    let mut run_layers = vec![cref("os-min", None), cref("php", Some(v)), cref("php-fpm", Some(v))];
    run_layers.extend(ext_refs);
    run_layers.push(cref("nginx", None));

    let port = port(opts, 8080, &mut decisions);
    let docroot_abs = if docroot == "." {
        "/app".to_string()
    } else {
        format!("/app/{docroot}")
    };
    let mut files = Vec::new();
    let nginx_path = match &opts.php.nginx_conf {
        Some(p) => {
            let key = opts.key("php.nginx_conf");
            require_repo_file(repo, p, provider, &key)?;
            decisions.push(Decision::config("php.nginx_conf", p.as_str(), key));
            format!("/app/{p}")
        }
        None => {
            files.push(GeneratedFile {
                path: php_config::NGINX_PATH.into(),
                content: php_config::nginx(port, &docroot_abs),
                mode: 0o644,
            });
            php_config::NGINX_PATH.to_string()
        }
    };
    let fpm_path = match &opts.php.fpm_pool {
        Some(p) => {
            let key = opts.key("php.fpm_pool");
            require_repo_file(repo, p, provider, &key)?;
            decisions.push(Decision::config("php.fpm_pool", p.as_str(), key));
            format!("/app/{p}")
        }
        None => {
            files.push(GeneratedFile {
                path: php_config::FPM_PATH.into(),
                content: php_config::fpm(),
                mode: 0o644,
            });
            php_config::FPM_PATH.to_string()
        }
    };

    let mut config = ImageConfig::new("/app", "root");
    config.env.insert("PHP_FPM_MAX_CHILDREN".into(), "16".into());
    config.port = Some(port);
    config.stop_signal = Some("SIGQUIT".into());
    let mut image = empty_image(run_layers, config);
    image.export = vec![Export {
        path: "/app".into(),
        dest: "/app".into(),
        exclude: vec!["node_modules".into()],
    }];
    image.files = files;
    image
        .processes
        .insert("web".into(), Process::new(&["nginx", "-c", &nginx_path]));
    image.services.insert(
        "web".into(),
        vec![Service {
            name: "php-fpm".into(),
            command: vec![
                "php-fpm".into(),
                "--nodaemonize".into(),
                "--fpm-config".into(),
                fpm_path,
            ],
            ready: Ready::Unix(php_config::FPM_SOCKET.into()),
            stop_signal: "SIGQUIT".into(),
        }],
    );
    Ok(Detection {
        target: new_target(name, provider, build_layers, steps, image),
        decisions,
    })
}
