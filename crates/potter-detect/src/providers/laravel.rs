//! Laravel (spec §6.1 "Laravel"). Builds on the PHP provider and adds assets, package
//! discovery, Octane and worker/scheduler processes.

use std::collections::BTreeSet;

use potter_plan::{Base, Command, Decision, Process, Step};

use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider, js, php};
use crate::repo::{READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::{Claim, ClaimKind};
use crate::{DetectError, ProviderId};

const LARAVEL_EXTENSIONS: &[&str] = &["bcmath", "intl", "pcntl", "pdo_mysql", "pdo_pgsql", "redis"];
const OCTANE_SERVERS: &[&str] = &["swoole", "roadrunner", "frankenphp"];
const ROADRUNNER_PACKAGES: &[&str] = &["spiral/roadrunner", "spiral/roadrunner-http", "spiral/roadrunner-cli"];

pub(crate) struct LaravelProvider;

impl Provider for LaravelProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Laravel
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        if !repo.is_file("artisan") {
            return Ok(None);
        }
        let Some(composer) = repo.read_json("composer.json")? else {
            return Ok(None);
        };
        Ok(php::requires(&composer, "laravel/framework").then(|| Claim {
            provider: ProviderId::Laravel,
            refines: Some(ProviderId::Php),
            kind: ClaimKind::Backend,
            evidence: vec!["composer.json requires laravel/framework".into(), "artisan".into()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let mut facts = php::read_facts(
            repo,
            opts,
            ProviderId::Laravel,
            LARAVEL_EXTENSIONS,
            "Laravel default extension",
        )?;
        let server = if php::requires(&facts.composer, "laravel/octane") {
            Some(octane_server(
                repo,
                opts,
                &facts.composer,
                &facts.extensions,
                &mut facts.decisions,
            )?)
        } else {
            None
        };
        if server == Some("swoole") && facts.extensions.insert("swoole".into()) {
            facts.decisions.push(Decision::detected(
                "runtime.extensions",
                "swoole",
                ["Octane server swoole"],
            ));
        }
        let horizon = php::requires(&facts.composer, "laravel/horizon");
        let mut det = php::build_php(repo, facts, opts, ProviderId::Laravel, target)?;
        let t = &mut det.target;

        // Package discovery runs as its own step, so autoload must not run it through scripts.
        for s in &mut t.build.steps {
            if let Step::Run { name, command, .. } = s
                && name == "autoload"
            {
                *command = Command::shell("composer dump-autoload --optimize --no-dev --no-interaction --no-scripts");
            }
        }
        insert_after(
            &mut t.build.steps,
            "autoload",
            vec![run(
                "discover",
                Command::argv(&["php", "artisan", "package:discover", "--ansi"]),
                &[],
                &[],
            )],
        );

        if let Some(assets) = js::assets_for_backend(repo, opts, ProviderId::Laravel, &mut det.decisions)? {
            if let Base::Catalog { layers } = &mut t.build.base {
                layers.push(assets.runtime);
            }
            insert_after(&mut t.build.steps, "deps", assets.deps);
            insert_after(&mut t.build.steps, "source", vec![assets.assets]);
        }

        let worker: &[&str] = if horizon {
            &["php", "artisan", "horizon"]
        } else {
            &["php", "artisan", "queue:work"]
        };
        let evidence = if horizon {
            "composer.json requires laravel/horizon"
        } else {
            "Laravel queue worker"
        };
        det.decisions
            .push(Decision::detected("process.worker", worker.join(" "), [evidence]));
        t.image.processes.insert("worker".into(), Process::new(worker));
        t.image
            .processes
            .insert("scheduler".into(), Process::new(&["php", "artisan", "schedule:work"]));

        if let Some(server) = server {
            if let Base::Catalog { layers } = &mut t.image.base {
                layers.retain(|l| l.name != "php-fpm" && l.name != "nginx");
                if server != "swoole" {
                    layers.push(cref(server, None));
                }
            }
            t.image.files.clear();
            t.image.services.clear();
            t.image.config.stop_signal = None;
            t.image.config.env.remove("PHP_FPM_MAX_CHILDREN");
            let port = t.image.config.port.unwrap_or(8080);
            let server_flag = format!("--server={server}");
            let port_flag = format!("--port={port}");
            t.image.processes.insert(
                "web".into(),
                Process::new(&[
                    "php",
                    "artisan",
                    "octane:start",
                    &server_flag,
                    "--host=0.0.0.0",
                    &port_flag,
                ]),
            );
        }
        Ok(det)
    }
}

fn octane_server(
    repo: &RepoView,
    opts: &DetectOptions,
    composer: &serde_json::Value,
    extensions: &BTreeSet<String>,
    decisions: &mut Vec<Decision>,
) -> Result<&'static str, DetectError> {
    let key = opts.key("php.octane_server");
    let known = |s: &str| OCTANE_SERVERS.iter().copied().find(|x| *x == s);
    if let Some(s) = &opts.php.octane_server {
        let server = known(s).ok_or_else(|| DetectError::NeedsConfig {
            provider: ProviderId::Laravel,
            message: format!("unknown Octane server `{s}` (swoole, roadrunner or frankenphp)"),
            key: key.clone(),
        })?;
        decisions.push(Decision::config("php.octane_server", server, key));
        return Ok(server);
    }
    if let Some(text) = repo.read_string("config/octane.php", READ_CAP)?
        && let Some(s) = quoted_value_after(&text, "OCTANE_SERVER", ',')
        && let Some(server) = known(&s)
    {
        decisions.push(Decision::detected(
            "php.octane_server",
            server,
            ["config/octane.php OCTANE_SERVER default"],
        ));
        return Ok(server);
    }
    if let Some(p) = ROADRUNNER_PACKAGES.iter().find(|p| php::requires(composer, p)) {
        decisions.push(Decision::detected(
            "php.octane_server",
            "roadrunner",
            [format!("composer.json requires {p}")],
        ));
        return Ok("roadrunner");
    }
    if extensions.contains("swoole") || extensions.contains("openswoole") {
        decisions.push(Decision::detected(
            "php.octane_server",
            "swoole",
            ["the swoole extension is required"],
        ));
        return Ok("swoole");
    }
    Err(DetectError::NeedsConfig {
        provider: ProviderId::Laravel,
        message: "laravel/octane is installed but its server can't be determined".into(),
        key,
    })
}
