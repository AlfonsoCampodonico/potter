//! Node.js applications (spec §6.1 "Node").

use potter_plan::{Command, Decision, ImageConfig, Process};

use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson};
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::Claim;
use crate::{DetectError, ProviderId};

pub(crate) const OTHER_RUNTIME_FILES: &[&str] = &["bun.lock", "bun.lockb", "deno.json", "deno.jsonc"];

pub(crate) struct NodeProvider;

impl Provider for NodeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Node
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        if OTHER_RUNTIME_FILES.iter().any(|f| repo.is_file(f)) {
            return Ok(None);
        }
        let Some(pkg) = PackageJson::read(repo)? else {
            return Ok(None);
        };
        Ok(Some(Claim {
            provider: ProviderId::Node,
            refines: None,
            kind: js::js_kind(&pkg, &[]),
            evidence: vec!["package.json".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let pkg = PackageJson::read(repo)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Node,
            file: "package.json".into(),
            hint: "the Node provider builds npm projects".into(),
        })?;
        let mut d = Vec::new();
        let pm = js::detect_pm(repo, &pkg, opts, ProviderId::Node, &mut d)?;
        let node = js::node_version(repo, &pkg, opts, &mut d)?;
        let mut steps = js::deps_steps(pm, repo, "");
        steps.push(source());
        if pkg.script("build").is_some() {
            steps.push(run("build", Command::Argv(pm.run_script("build")), &[], &[]));
        }
        if let Some(prune) = pm.prune() {
            steps.push(run("prune", prune, &[], &[]));
        }
        let port = port(opts, 3000, &mut d);
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("NODE_ENV".into(), "production".into());

        let (exports, web) = if pkg.dep("next") && next_standalone(repo)? {
            d.push(Decision::detected(
                "process.web",
                "next standalone",
                ["next.config output = standalone"],
            ));
            config.env.insert("HOSTNAME".into(), "0.0.0.0".into());
            let mut e = vec![
                export("/app/.next/standalone", "/app"),
                export("/app/.next/static", "/app/.next/static"),
            ];
            if repo.is_dir("public") {
                e.push(export("/app/public", "/app/public"));
            }
            (e, vec!["node".to_string(), "server.js".to_string()])
        } else if pkg.dep("nuxt") {
            d.push(Decision::detected(
                "process.web",
                "nuxt",
                ["package.json dependencies.nuxt"],
            ));
            config.env.insert("HOST".into(), "0.0.0.0".into());
            (
                vec![export("/app/.output", "/app/.output")],
                vec!["node".to_string(), ".output/server/index.mjs".to_string()],
            )
        } else if pkg.script("start").is_some() {
            d.push(Decision::detected(
                "process.web",
                "start script",
                ["package.json scripts.start"],
            ));
            (vec![export("/app", "/app")], pm.run_script("start"))
        } else if let Some(cmd) = &opts.web_cmd {
            (vec![export("/app", "/app")], cmd.clone())
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Node,
                hint: format!(
                    "add a `start` script to package.json, or set `{}` in potter.toml",
                    opts.key("process.web.cmd")
                ),
            });
        };

        let layers = vec![cref("os-min", None), cref("node", Some(node.version))];
        let mut image = empty_image(layers.clone(), config);
        image.export = exports;
        image.processes.insert(
            "web".into(),
            Process {
                command: web,
                workdir: None,
                env: Default::default(),
            },
        );
        Ok(Detection {
            target: new_target(target, ProviderId::Node, layers, steps, image),
            decisions: d,
        })
    }
}

fn next_standalone(repo: &RepoView) -> Result<bool, DetectError> {
    for f in [
        "next.config.js",
        "next.config.mjs",
        "next.config.cjs",
        "next.config.ts",
        "next.config.mts",
    ] {
        if let Some(text) = repo.read_string(f, READ_CAP)?
            && quoted_value_after(&text, "output", ':').as_deref() == Some("standalone")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
