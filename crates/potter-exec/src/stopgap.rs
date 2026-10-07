//! The stopgap catalog (spec §15, P2): until P3's signed catalog exists, a catalog base is
//! lowered onto an upstream Debian trixie image and `Run` steps that install the rest.

use std::collections::BTreeMap;

use potter_plan::{CatalogRef, Command, Step};

use crate::error::BuildError;

/// A catalog base as an upstream image and the steps that complete it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lowered {
    pub reference: String,
    pub steps: Vec<Step>,
}

/// Runtimes that decide the image; at most one per base (php may add node).
const RUNTIMES: [&str; 8] = ["php", "node", "bun", "deno", "python", "ruby", "go", "rust"];

/// Extensions every official php image already has.
const PHP_BUILTIN: &[&str] = &[
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
    "random",
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

const APT_CLEAN: &str = "rm -rf /var/lib/apt/lists/*";

/// A root `Run` step with network access, in `/`.
fn install(name: &str, script: String) -> Step {
    Step::Run {
        name: name.into(),
        command: Command::Shell(script),
        env: BTreeMap::new(),
        workdir: "/".into(),
        user: "0:0".into(),
        network: true,
        cache_mounts: vec![],
        secrets: vec![],
        timeout_secs: None,
    }
}

fn apt_install(packages: &str) -> String {
    format!(
        "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends {packages} && {APT_CLEAN}"
    )
}

/// `static-server --root DIR --listen ADDR`, served by busybox's httpd.
const STATIC_SERVER: &str = r#"cat > /usr/local/bin/static-server <<'POTTER'
#!/bin/sh
root=/srv/www
listen=0.0.0.0:8080
while [ $# -gt 0 ]; do
  case "$1" in
    --root) root=$2; shift 2 ;;
    --listen) listen=$2; shift 2 ;;
    *) shift ;;
  esac
done
exec busybox httpd -f -h "$root" -p "$listen"
POTTER
chmod 755 /usr/local/bin/static-server"#;

fn unsupported(what: String) -> BuildError {
    BuildError::Unsupported {
        what,
        milestone: "P3 (the catalog)",
    }
}

/// The upstream image and install steps for the catalog base `layers`.
pub fn lower(layers: &[CatalogRef]) -> Result<Lowered, BuildError> {
    let listed = layers.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let version = |r: &CatalogRef| {
        r.version
            .clone()
            .ok_or_else(|| BuildError::Plan(format!("the catalog layer {} has no version", r.name)))
    };
    let has = |name: &str| layers.iter().any(|r| r.name == name);
    let runtimes: Vec<&CatalogRef> = layers.iter().filter(|r| RUNTIMES.contains(&r.name.as_str())).collect();
    let primary = match runtimes.as_slice() {
        [] => None,
        [one] => Some(*one),
        many if has("php") && many.iter().all(|r| r.name == "php" || r.name == "node") => {
            many.iter().copied().find(|r| r.name == "php")
        }
        _ => return Err(unsupported(format!("the base [{listed}], with several runtimes"))),
    };
    let reference = match primary {
        None => "debian:trixie-slim".to_string(),
        Some(r) => {
            let v = version(r)?;
            match r.name.as_str() {
                "php" if has("php-fpm") => format!("php:{v}-fpm-trixie"),
                "php" => format!("php:{v}-cli-trixie"),
                "node" => format!("node:{v}-trixie-slim"),
                "bun" => "oven/bun:1-slim".to_string(),
                "deno" => "denoland/deno:debian".to_string(),
                "python" => format!("python:{v}-slim-trixie"),
                "ruby" => format!("ruby:{v}-slim-trixie"),
                "go" => format!("golang:{v}-trixie"),
                "rust" => format!("rust:{v}-slim-trixie"),
                _ => unreachable!("one of RUNTIMES"),
            }
        }
    };
    let php = primary.is_some_and(|r| r.name == "php");
    let mut extensions = Vec::new();
    let mut steps = Vec::new();
    for r in layers {
        let name = r.name.as_str();
        match name {
            "os-min" | "php-fpm" => {}
            _ if Some(r) == primary => {}
            _ if name.starts_with("php-ext-") && php => {
                let ext = &name["php-ext-".len()..];
                if !PHP_BUILTIN.contains(&ext) {
                    extensions.push(ext.to_string());
                }
            }
            "composer" if php => {}
            "node" if php => {}
            "nginx" | "static-server" => {}
            _ => return Err(unsupported(format!("the catalog layer {r} in [{listed}]"))),
        }
    }
    if !extensions.is_empty() {
        extensions.sort();
        extensions.dedup();
        steps.push(install(
            "catalog-php-extensions",
            format!(
                "curl -sSLf -o /usr/local/bin/install-php-extensions \
                 https://github.com/mlocati/docker-php-extension-installer/releases/latest/download/install-php-extensions \
                 && chmod 755 /usr/local/bin/install-php-extensions && install-php-extensions {}",
                extensions.join(" ")
            ),
        ));
    }
    if php && has("composer") {
        steps.push(install(
            "catalog-composer",
            "curl -sSLf -o /usr/local/bin/composer https://getcomposer.org/download/latest-2.x/composer.phar \
             && chmod 755 /usr/local/bin/composer"
                .into(),
        ));
    }
    if let Some(node) = layers.iter().find(|r| r.name == "node" && php) {
        let v = version(node)?;
        steps.push(install(
            "catalog-node",
            format!(
                "curl -fsSL https://deb.nodesource.com/setup_{v}.x | bash - && {}",
                apt_install("nodejs").trim_start_matches("apt-get update && ")
            ),
        ));
    }
    if has("nginx") {
        steps.push(install("catalog-nginx", apt_install("nginx")));
    }
    if has("static-server") {
        steps.push(install(
            "catalog-static-server",
            format!("{} && {STATIC_SERVER}", apt_install("busybox")),
        ));
    }
    Ok(Lowered { reference, steps })
}

/// A step as the stopgap runs it: `apt` steps become `apt-get install` runs (unpinned until
/// P3; the snapshot stays in their key through the environment). Others are unchanged.
pub fn lower_step(step: &Step) -> Step {
    match step {
        Step::Apt {
            name,
            packages,
            snapshot,
        } => {
            let Step::Run {
                name: n,
                command,
                workdir,
                user,
                network,
                cache_mounts,
                secrets,
                timeout_secs,
                ..
            } = install(name, apt_install(&packages.join(" ")))
            else {
                unreachable!("install makes a run step")
            };
            Step::Run {
                name: n,
                command,
                env: [("POTTER_APT_SNAPSHOT".to_string(), snapshot.clone())].into(),
                workdir,
                user,
                network,
                cache_mounts,
                secrets,
                timeout_secs,
            }
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use potter_plan::{CatalogRef, Command, DEFAULT_APT_SNAPSHOT, Step};

    use super::*;

    fn refs(list: &[(&str, Option<&str>)]) -> Vec<CatalogRef> {
        list.iter().map(|(n, v)| CatalogRef::new(n, *v)).collect()
    }

    fn names(l: &Lowered) -> Vec<&str> {
        l.steps.iter().map(|s| s.name()).collect()
    }

    fn script(s: &Step) -> String {
        match s {
            Step::Run {
                command: Command::Shell(s),
                ..
            } => s.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn each_runtime_maps_to_its_upstream_image() {
        for (layers, image) in [
            (vec![("os-min", None)], "debian:trixie-slim"),
            (vec![("os-min", None), ("node", Some("22"))], "node:22-trixie-slim"),
            (vec![("os-min", None), ("bun", Some("1"))], "oven/bun:1-slim"),
            (vec![("os-min", None), ("deno", Some("2"))], "denoland/deno:debian"),
            (
                vec![("os-min", None), ("python", Some("3.13"))],
                "python:3.13-slim-trixie",
            ),
            (vec![("os-min", None), ("ruby", Some("3.4"))], "ruby:3.4-slim-trixie"),
            (vec![("os-min", None), ("go", Some("1.27"))], "golang:1.27-trixie"),
            (vec![("os-min", None), ("rust", Some("1.99"))], "rust:1.99-slim-trixie"),
            (vec![("os-min", None), ("php", Some("8.4"))], "php:8.4-cli-trixie"),
            (
                vec![("os-min", None), ("php", Some("8.3")), ("php-fpm", Some("8.3"))],
                "php:8.3-fpm-trixie",
            ),
        ] {
            let l = lower(&refs(&layers)).unwrap();
            assert_eq!(l.reference, image, "{layers:?}");
            assert!(l.steps.is_empty(), "{layers:?}: {:?}", names(&l));
        }
    }

    #[test]
    fn laravel_gets_extensions_composer_and_node_and_its_run_base_nginx() {
        let build = lower(&refs(&[
            ("os-min", None),
            ("php", Some("8.4")),
            ("php-ext-bcmath", Some("8.4")),
            ("php-ext-mbstring", Some("8.4")),
            ("php-ext-intl", Some("8.4")),
            ("php-ext-redis", Some("8.4")),
            ("composer", Some("2")),
            ("node", Some("22")),
        ]))
        .unwrap();
        assert_eq!(build.reference, "php:8.4-cli-trixie");
        assert_eq!(
            names(&build),
            ["catalog-php-extensions", "catalog-composer", "catalog-node"]
        );
        let exts = script(&build.steps[0]);
        assert!(exts.ends_with("install-php-extensions bcmath intl redis"), "{exts}");
        assert!(script(&build.steps[2]).contains("setup_22.x"));
        let run = lower(&refs(&[
            ("os-min", None),
            ("php", Some("8.4")),
            ("php-fpm", Some("8.4")),
            ("nginx", None),
        ]))
        .unwrap();
        assert_eq!(run.reference, "php:8.4-fpm-trixie");
        assert_eq!(names(&run), ["catalog-nginx"]);
    }

    #[test]
    fn the_static_server_is_a_busybox_shim() {
        let l = lower(&refs(&[("os-min", None), ("static-server", None)])).unwrap();
        assert_eq!(l.reference, "debian:trixie-slim");
        assert_eq!(names(&l), ["catalog-static-server"]);
        let s = script(&l.steps[0]);
        assert!(
            s.contains("busybox httpd -f -h") && s.contains("/usr/local/bin/static-server"),
            "{s}"
        );
    }

    #[test]
    fn what_the_stopgap_cannot_build_is_refused_by_name() {
        for layers in [
            vec![("os-min", None), ("erlang", Some("27"))],
            vec![("os-min", None), ("python", Some("3.13")), ("ruby", Some("3.4"))],
            vec![("os-min", None), ("go", None)],
        ] {
            let err = lower(&refs(&layers)).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{layers:?}: {err}");
        }
    }

    #[test]
    fn apt_steps_become_installs_that_keep_their_snapshot_in_the_key() {
        let apt = Step::Apt {
            name: "apt-build".into(),
            packages: vec!["libpq-dev".into(), "git".into()],
            snapshot: DEFAULT_APT_SNAPSHOT.into(),
        };
        let Step::Run {
            name,
            command,
            env,
            user,
            network,
            ..
        } = lower_step(&apt)
        else {
            panic!()
        };
        assert_eq!(name, "apt-build");
        assert_eq!((user.as_str(), network), ("0:0", true));
        assert_eq!(
            env.get("POTTER_APT_SNAPSHOT").map(String::as_str),
            Some(DEFAULT_APT_SNAPSHOT)
        );
        let Command::Shell(s) = command else { panic!() };
        assert!(
            s.contains("apt-get install -y --no-install-recommends libpq-dev git"),
            "{s}"
        );
        let run = Step::Run {
            name: "x".into(),
            command: Command::shell("true"),
            env: BTreeMap::new(),
            workdir: "/".into(),
            user: "0:0".into(),
            network: true,
            cache_mounts: vec![],
            secrets: vec![],
            timeout_secs: None,
        };
        assert_eq!(lower_step(&run), run);
    }
}
