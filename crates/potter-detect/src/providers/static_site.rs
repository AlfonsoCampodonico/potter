//! Static sites: a JS build with no server runtime (spec §6.1 "Static").

use potter_plan::{Command, Decision, ImageConfig, Process};

use crate::options::DetectOptions;
use crate::providers::bun::BunProvider;
use crate::providers::common::*;
use crate::providers::js::{self, PackageJson, Pm};
use crate::providers::node::NodeProvider;
use crate::providers::{Detection, Provider};
use crate::repo::{READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::{Claim, ClaimKind};
use crate::{DetectError, ProviderId};

const VITE_CONFIGS: &[&str] = &[
    "vite.config.ts",
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.mts",
    "vite.config.cjs",
];

pub(crate) struct StaticProvider;

/// The underlying JS provider's claim, if it is asset-only.
fn underlying(repo: &RepoView, opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
    let candidates: [&dyn Provider; 2] = [&NodeProvider, &BunProvider];
    for p in candidates {
        if let Some(c) = p.claim(repo, opts)? {
            return Ok((c.kind == ClaimKind::JsAssets).then_some(c));
        }
    }
    Ok(None)
}

/// (tool, output dir, evidence)
fn static_tool(repo: &RepoView, pkg: &PackageJson) -> Result<Option<(&'static str, String, String)>, DetectError> {
    if pkg.any_dep("vite") {
        for f in VITE_CONFIGS {
            if let Some(text) = repo.read_string(f, READ_CAP)?
                && let Some(out) = quoted_value_after(&text, "outDir", ':')
            {
                return Ok(Some(("vite", out, format!("{f} build.outDir"))));
            }
        }
        return Ok(Some(("vite", "dist".into(), "vite default outDir".into())));
    }
    for (tool, out) in [
        ("react-scripts", "build"),
        ("astro", "dist"),
        ("@vue/cli-service", "dist"),
    ] {
        if pkg.any_dep(tool) {
            return Ok(Some((tool, out.into(), format!("{tool} default output"))));
        }
    }
    Ok(None)
}

impl Provider for StaticProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Static
    }

    fn claim(&self, repo: &RepoView, opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        let Some(base) = underlying(repo, opts)? else {
            return Ok(None);
        };
        let Some(pkg) = PackageJson::read(repo)? else {
            return Ok(None);
        };
        if pkg.script("build").is_none() {
            return Ok(None);
        }
        let Some((tool, _, _)) = static_tool(repo, &pkg)? else {
            return Ok(None);
        };
        Ok(Some(Claim {
            provider: ProviderId::Static,
            refines: Some(base.provider),
            kind: ClaimKind::JsAssets,
            evidence: vec![format!("{tool} build with no server runtime")],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let base = underlying(repo, opts)?.ok_or(DetectError::ForcedNotApplicable(ProviderId::Static))?;
        let pkg = PackageJson::read(repo)?.ok_or(DetectError::ForcedNotApplicable(ProviderId::Static))?;
        let (_, out_dir, evidence) =
            static_tool(repo, &pkg)?.ok_or(DetectError::ForcedNotApplicable(ProviderId::Static))?;
        let out = out_dir.trim_start_matches("./").trim_end_matches('/').to_string();
        if out.is_empty() || out == "." || !potter_plan::is_relative_path(&out) {
            return Err(DetectError::NeedsConfig {
                provider: ProviderId::Static,
                message: format!("the build output directory `{out_dir}` must be a subdirectory of the project"),
                key: opts.key("process.web.cmd"),
            });
        }
        let mut d = vec![Decision::detected("static.out_dir", out.as_str(), [evidence])];
        let (pm, runtime) = if base.provider == ProviderId::Bun {
            d.push(Decision::detected(
                "node.package_manager",
                "bun",
                ["bun lockfile is present"],
            ));
            (
                Pm::Bun,
                cref("bun", Some(js::bun_version(repo, &pkg, opts, &mut d)?.version)),
            )
        } else {
            let pm = js::detect_pm(repo, &pkg, opts, ProviderId::Static, &mut d)?;
            (
                pm,
                cref("node", Some(js::node_version(repo, &pkg, opts, &mut d)?.version)),
            )
        };
        let mut steps = js::deps_steps(pm, repo, "");
        steps.push(source());
        steps.push(run("build", Command::Argv(pm.run_script("build")), &[], &[]));

        let port = port(opts, 8080, &mut d);
        let mut config = ImageConfig::new("/srv/www", "root");
        config.port = Some(port);
        let mut image = empty_image(vec![cref("os-min", None), cref("static-server", None)], config);
        image.export = vec![export(&format!("/app/{out}"), "/srv/www")];
        let listen = format!("0.0.0.0:{port}");
        image.processes.insert(
            "web".into(),
            Process::new(&["static-server", "--root", "/srv/www", "--listen", &listen]),
        );
        let layers = vec![cref("os-min", None), runtime];
        Ok(Detection {
            target: new_target(target, ProviderId::Static, layers, steps, image),
            decisions: d,
        })
    }
}
