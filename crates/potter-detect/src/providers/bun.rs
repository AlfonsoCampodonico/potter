//! Bun applications (spec §6.1 "Bun").

use potter_plan::{Command, Decision, ImageConfig, Process};

use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson, Pm};
use crate::providers::{Detection, Provider};
use crate::repo::RepoView;
use crate::select::Claim;
use crate::{DetectError, ProviderId};

/// Bun serves these frameworks natively, without a start script.
pub(crate) const BUN_SERVER_DEPS: &[&str] = &["hono", "elysia"];

pub(crate) struct BunProvider;

fn lockfile(repo: &RepoView) -> Option<&'static str> {
    ["bun.lock", "bun.lockb"].into_iter().find(|f| repo.is_file(f))
}

impl Provider for BunProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Bun
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        if repo.is_file("deno.json") || repo.is_file("deno.jsonc") {
            return Ok(None);
        }
        let Some(lock) = lockfile(repo) else { return Ok(None) };
        let Some(pkg) = PackageJson::read(repo)? else {
            return Ok(None);
        };
        Ok(Some(Claim {
            provider: ProviderId::Bun,
            refines: None,
            kind: js::js_kind(&pkg, BUN_SERVER_DEPS),
            evidence: vec!["package.json".into(), lock.into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let pkg = PackageJson::read(repo)?.ok_or_else(|| DetectError::MissingFile {
            provider: ProviderId::Bun,
            file: "package.json".into(),
            hint: "the Bun provider builds package.json projects".into(),
        })?;
        let mut d = Vec::new();
        let lock = lockfile(repo).unwrap_or("bun.lock");
        d.push(Decision::detected(
            "node.package_manager",
            "bun",
            [format!("{lock} is present")],
        ));
        let bun = js::bun_version(repo, &pkg, opts, &mut d)?;
        let mut steps = js::deps_steps(Pm::Bun, repo, "");
        steps.push(source());
        if pkg.script("build").is_some() {
            steps.push(run("build", Command::Argv(Pm::Bun.run_script("build")), &[], &[]));
        }
        let web = if pkg.script("start").is_some() {
            d.push(Decision::detected(
                "process.web",
                "start script",
                ["package.json scripts.start"],
            ));
            Pm::Bun.run_script("start")
        } else if let Some(entry) = pkg.str_field("module").or_else(|| pkg.str_field("main")) {
            d.push(Decision::detected("process.web", entry, ["package.json module/main"]));
            vec!["bun".into(), "run".into(), entry.to_string()]
        } else if let Some(cmd) = &opts.web_cmd {
            cmd.clone()
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Bun,
                hint: format!(
                    "add a `start` script or a `module` entry to package.json, or set `{}`",
                    opts.key("process.web.cmd")
                ),
            });
        };
        let port = port(opts, 3000, &mut d);
        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("NODE_ENV".into(), "production".into());
        let layers = vec![cref("os-min", None), cref("bun", Some(bun.version))];
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
            target: new_target(target, ProviderId::Bun, layers, steps, image),
            decisions: d,
        })
    }
}
