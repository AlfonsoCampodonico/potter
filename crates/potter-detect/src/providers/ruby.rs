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
const ASSET_GEMS: &[&str] = &[
    "propshaft",
    "sprockets",
    "jsbundling-rails",
    "cssbundling-rails",
    "tailwindcss-rails",
];

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
        let lock = repo
            .read_string("Gemfile.lock", LOCKFILE_CAP)?
            .ok_or_else(|| DetectError::MissingFile {
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
            d.push(Decision::detected(
                "apt.build",
                apt_build.join(" "),
                ["gems that need system libraries"],
            ));
            steps.push(Step::Apt {
                name: "apt-detected".into(),
                packages: apt_build,
                snapshot: DEFAULT_APT_SNAPSHOT.into(),
            });
        }
        let mut deps_inputs = vec!["Gemfile".to_string(), "Gemfile.lock".to_string()];
        deps_inputs.extend(
            repo.files()
                .filter(|f| !f.contains('/') && f.ends_with(".gemspec"))
                .map(str::to_string),
        );
        let mut env: Vec<(&str, &str)> = BUNDLE_ENV.to_vec();
        env.push(("BUNDLE_USER_CACHE", "/cache/bundler"));
        steps.push(inputs("deps-inputs", deps_inputs));
        steps.push(run(
            "deps",
            Command::argv(&["bundle", "install"]),
            &env,
            &[("bundler", "/cache/bundler")],
        ));

        // jsbundling/cssbundling run their JS build inside assets:precompile, so node deps come first.
        if let Some(pkg) = PackageJson::read(repo)? {
            let pm = js::detect_pm(repo, &pkg, opts, ProviderId::Ruby, &mut d)?;
            let node = js::node_version(repo, &pkg, opts, &mut d)?;
            build_layers.push(cref("node", Some(node.version)));
            steps.extend(js::deps_steps(pm, repo, "node-"));
        }
        steps.push(source());
        if gems.contains("bootsnap") {
            steps.push(run(
                "bootsnap",
                Command::argv(&["bundle", "exec", "bootsnap", "precompile", "app/", "lib/"]),
                BUNDLE_ENV,
                &[],
            ));
        }
        if rails && ASSET_GEMS.iter().any(|g| gems.contains(*g)) {
            let mut assets_env = BUNDLE_ENV.to_vec();
            assets_env.push(("RAILS_ENV", "production"));
            assets_env.push(("SECRET_KEY_BASE_DUMMY", "1"));
            steps.push(run(
                "assets",
                Command::argv(&["bin/rails", "assets:precompile"]),
                &assets_env,
                &[],
            ));
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
        image.export = vec![potter_plan::Export {
            path: "/app".into(),
            dest: "/app".into(),
            exclude: vec!["node_modules".into()],
        }];
        let web: Vec<String> = if rails {
            d.push(Decision::detected(
                "process.web",
                "bin/rails server",
                ["Gemfile.lock includes rails"],
            ));
            ["bin/rails", "server", "-b", "0.0.0.0"].map(String::from).to_vec()
        } else if repo.is_file("config.ru") {
            d.push(Decision::detected("process.web", "rackup", ["config.ru exists"]));
            vec![
                "bundle".into(),
                "exec".into(),
                "rackup".into(),
                "-o".into(),
                "0.0.0.0".into(),
                "-p".into(),
                port.to_string(),
            ]
        } else if let Some(cmd) = &opts.web_cmd {
            cmd.clone()
        } else {
            return Err(DetectError::NoProcess {
                provider: ProviderId::Ruby,
                hint: format!(
                    "add a config.ru, or set `{}` in potter.toml",
                    opts.key("process.web.cmd")
                ),
            });
        };
        image.processes.insert(
            "web".into(),
            Process {
                command: web,
                workdir: None,
                env: Default::default(),
            },
        );
        if gems.contains("solid_queue") {
            image.processes.insert("worker".into(), Process::new(&["bin/jobs"]));
            d.push(Decision::detected(
                "process.worker",
                "bin/jobs",
                ["Gemfile.lock includes solid_queue"],
            ));
        } else if gems.contains("sidekiq") {
            image
                .processes
                .insert("worker".into(), Process::new(&["bundle", "exec", "sidekiq"]));
            d.push(Decision::detected(
                "process.worker",
                "bundle exec sidekiq",
                ["Gemfile.lock includes sidekiq"],
            ));
        }
        Ok(Detection {
            target: new_target(target, ProviderId::Ruby, build_layers, steps, image),
            decisions: d,
        })
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
    VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only: false,
    }
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
        let rest = l
            .strip_prefix("ruby ")
            .or_else(|| l.strip_prefix("ruby("))?
            .trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = &rest[1..];
        body.find(quote).map(|end| body[..end].to_string())
    })
}
