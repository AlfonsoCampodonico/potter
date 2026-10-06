//! Deno applications (spec §6.1 "Deno"). DENO_DIR lives in the layer because Deno needs its
//! module cache at runtime.

use potter_plan::{Command, Decision, ImageConfig, Process};
use serde_json::Value;

use crate::catalog::{self, DENO};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::RepoView;
use crate::select::{Claim, ClaimKind};
use crate::{DetectError, ProviderId};

const DENO_DIR: &str = "/app/.deno";
const ENTRIES: &[&str] = &["main.ts", "main.js", "mod.ts", "server.ts", "src/main.ts"];

pub(crate) struct DenoProvider;

fn config(repo: &RepoView) -> Result<Option<(&'static str, Value)>, DetectError> {
    let Some(file) = ["deno.json", "deno.jsonc"].into_iter().find(|f| repo.is_file(f)) else {
        return Ok(None);
    };
    Ok(repo.read_jsonc(file)?.map(|v| (file, v)))
}

fn has_task(cfg: &Value, name: &str) -> bool {
    cfg.get("tasks").and_then(|t| t.get(name)).is_some()
}

fn entry(repo: &RepoView) -> Option<&'static str> {
    ENTRIES.iter().copied().find(|f| repo.is_file(f))
}

impl Provider for DenoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Deno
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        let Some((file, cfg)) = config(repo)? else {
            return Ok(None);
        };
        let kind = if has_task(&cfg, "start") || entry(repo).is_some() {
            ClaimKind::JsServer
        } else {
            ClaimKind::JsAssets
        };
        Ok(Some(Claim {
            provider: ProviderId::Deno,
            refines: None,
            kind,
            evidence: vec![file.into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let (file, cfg) = config(repo)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Deno,
            file: "deno.json".into(),
            hint: "the Deno provider builds deno.json projects".into(),
        })?;
        let mut d = Vec::new();
        let mut src = version_from_config(opts.runtime.deno.as_deref(), &opts.key("runtime.deno"), "deno")?;
        if src.is_none() {
            src = version_from_file(repo, ".dvmrc", "deno")?;
        }
        let deno = catalog::pick(&DENO, "runtime.deno", src, &mut d)?;

        let locked = repo.is_file("deno.lock");
        let mut deps_inputs = vec![file.to_string()];
        if locked {
            deps_inputs.push("deno.lock".into());
        }
        if repo.is_file("package.json") {
            deps_inputs.push("package.json".into());
        }
        if let Some(map) = cfg.get("importMap").and_then(Value::as_str)
            && potter_plan::is_relative_path(map)
            && repo.is_file(map)
        {
            deps_inputs.push(map.to_string());
        }
        let env = [("DENO_DIR", DENO_DIR)];
        let install = if locked {
            Command::argv(&["deno", "install", "--frozen"])
        } else {
            Command::argv(&["deno", "install"])
        };
        let mut steps = vec![
            inputs("deps-inputs", deps_inputs),
            run("deps", install, &env, &[]),
            source(),
        ];
        if has_task(&cfg, "build") {
            steps.push(run("build", Command::argv(&["deno", "task", "build"]), &env, &[]));
        }

        let port = port(opts, 3000, &mut d);
        let web: Vec<String> = if has_task(&cfg, "start") {
            d.push(Decision::detected(
                "process.web",
                "deno task start",
                [format!("{file} tasks.start")],
            ));
            vec!["deno".into(), "task".into(), "start".into()]
        } else if let Some(e) = entry(repo) {
            d.push(Decision::detected("process.web", e, [format!("{e} exists")]));
            vec![
                "deno".into(),
                "serve".into(),
                "--port".into(),
                port.to_string(),
                e.into(),
            ]
        } else if let Some(cmd) = &opts.web_cmd {
            cmd.clone()
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Deno,
                hint: format!(
                    "add a `start` task to {file} or a main.ts, or set `{}`",
                    opts.key("process.web.cmd")
                ),
            });
        };
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("DENO_DIR".into(), DENO_DIR.into());
        let layers = vec![cref("os-min", None), cref("deno", Some(deno.version))];
        let mut image = empty_image(layers.clone(), config);
        image.export = vec![export("/app", "/app")];
        image.processes.insert(
            "web".into(),
            Process {
                command: web,
                workdir: None,
                env: Default::default(),
            },
        );
        Ok(Detection {
            target: new_target(target, ProviderId::Deno, layers, steps, image),
            decisions: d,
        })
    }
}
