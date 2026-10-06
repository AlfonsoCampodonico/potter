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
        let gomod = repo
            .read_string("go.mod", READ_CAP)?
            .ok_or_else(|| DetectError::MissingFile {
                provider: ProviderId::Go,
                file: "go.mod".into(),
                hint: "the Go provider builds Go modules".into(),
            })?;
        let mut d = Vec::new();
        let go = go_version(opts, &gomod, &mut d)?;
        let mains = main_packages(repo)?;
        if mains.is_empty() && opts.web_cmd.is_none() {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Go,
                hint: format!(
                    "add a main package (main.go or cmd/<name>/main.go), or set `{}` in potter.toml",
                    opts.key("process.web.cmd")
                ),
            });
        }

        let env = [
            ("GOMODCACHE", "/cache/gomod"),
            ("GOCACHE", "/cache/gobuild"),
            ("GOTOOLCHAIN", "local"),
        ];
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
        let mut steps = vec![
            inputs("deps-inputs", deps_inputs),
            run(
                "deps",
                Command::argv(&["go", "mod", "download"]),
                &env,
                &[("gomod", "/cache/gomod")],
            ),
            source(),
            run(
                "build",
                Command::Shell(build_script),
                &build_env,
                &[("gomod", "/cache/gomod"), ("gobuild", "/cache/gobuild")],
            ),
        ];

        if mains.is_empty() {
            steps.pop();
        }
        // With `process.web.cmd` set in potter.toml, every main keeps its own name.
        let web = match opts.web_cmd {
            Some(_) => None,
            None => Some(pick_web(&mains, opts)?),
        };
        if let Some(web) = &web {
            d.push(Decision::detected(
                "process.web",
                format!("/out/{web}"),
                [main_evidence(&mains, web)],
            ));
        }
        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/out", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        let mut image = empty_image(vec![cref("os-min", None)], config);
        image.export = vec![export("/out", "/out")];
        for (name, _) in &mains {
            let process = if web.as_deref() == Some(name.as_str()) {
                "web".to_string()
            } else {
                name.clone()
            };
            if potter_plan::is_name(&process) {
                image
                    .processes
                    .insert(process, Process::new(&[&format!("/out/{name}")]));
            }
        }
        let layers = vec![cref("os-min", None), cref("go", Some(go.version))];
        Ok(Detection {
            target: new_target(target, ProviderId::Go, layers, steps, image),
            decisions: d,
        })
    }
}

fn directive<'a>(gomod: &'a str, word: &str) -> Option<&'a str> {
    gomod
        .lines()
        .find_map(|l| l.trim().strip_prefix(word)?.strip_prefix(' ').map(str::trim))
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only: false,
    }
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
        // `toolchain` only suggests a version; `go` is the real minimum.
        let in_catalog = GO.lines.iter().any(|l| set.intersects(&l.interval()));
        src = match directive(gomod, "go") {
            Some(v) if !in_catalog => {
                let min = at_least(v).map_err(|e| bad_constraint("go", v, "go.mod go", e))?;
                Some(detected(
                    format!("go.mod toolchain {tc} isn't in the catalog; go {v} (minimum)"),
                    min,
                ))
            }
            _ => Some(detected(format!("go.mod toolchain {tc}"), set)),
        };
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
    Ok(repo
        .read_string(file, READ_CAP)?
        .is_some_and(|t| t.lines().any(|l| l.trim() == "package main")))
}

fn go_files_in<'a>(repo: &'a RepoView, dir: &'a str) -> impl Iterator<Item = &'a str> {
    repo.files().filter(move |f| {
        let rest = if dir.is_empty() {
            Some(*f)
        } else {
            f.strip_prefix(dir).and_then(|r| r.strip_prefix('/'))
        };
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
    let pkg = mains
        .iter()
        .find(|(n, _)| n == web)
        .map(|(_, p)| p.as_str())
        .unwrap_or(".");
    if pkg == "." {
        "main.go is package main".into()
    } else {
        format!("{pkg} is package main")
    }
}
